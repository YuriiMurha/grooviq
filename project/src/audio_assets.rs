//! Bounded control-worker asset preparation, independent of Session and persisted schemas.
use crate::SampleFile;
use grooviq_engine::{MAX_SAMPLE_SECONDS, decode_sample, waveform::PeakPyramid};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use std::thread::{self, JoinHandle};
const SOURCE_LIMIT: usize = 512 * 1024 * 1024;
const QUEUE_BYTES: usize = 2 * 1024 * 1024 * 1024;
const QUEUE: usize = 4;
const CACHE: usize = 32;
struct SourceReservation {
    bytes: Arc<Mutex<usize>>,
    len: usize,
}
impl Drop for SourceReservation {
    fn drop(&mut self) {
        *self.bytes.lock().unwrap() -= self.len;
    }
}

pub struct PreparedAudio {
    _reservation: Arc<SourceReservation>,
    pub original: SampleFile,
    pub peaks: PeakPyramid,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    Busy,
    TooLarge,
    Cancelled,
    Decode(String),
}
struct Job {
    reservation: Arc<SourceReservation>,
    file: SampleFile,
    epoch: u64,
    cancelled: Arc<AtomicBool>,
    reply: SyncSender<Result<Arc<PreparedAudio>, AssetError>>,
}
pub struct AssetRequest {
    pub generation: u64,
    pub request: u64,
    pub asset: u32,
    epoch: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
    reply: Receiver<Result<Arc<PreparedAudio>, AssetError>>,
}
impl AssetRequest {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    /// Blocking control-thread receive; rechecks identity after a held worker completes.
    pub fn receive(&self) -> Result<Arc<PreparedAudio>, AssetError> {
        let result = self.reply.recv().unwrap_or(Err(AssetError::Cancelled));
        if self.cancelled.load(Ordering::SeqCst)
            || self.epoch.load(Ordering::SeqCst) != self.generation
        {
            Err(AssetError::Cancelled)
        } else {
            result
        }
    }
}
impl Drop for AssetRequest {
    fn drop(&mut self) {
        self.cancel();
    }
}
pub struct AudioAssets {
    sender: Option<SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
    epoch: Arc<AtomicU64>,
    queued_bytes: Arc<Mutex<usize>>,
    latest: Mutex<std::collections::HashMap<u32, Weak<AtomicBool>>>,
}
impl Default for AudioAssets {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioAssets {
    pub fn new() -> Self {
        Self::with_gate(|| {})
    }
    fn with_gate(mut gate: impl FnMut() + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE);
        let epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = epoch.clone();
        let queued_bytes = Arc::new(Mutex::new(0usize));
        let worker = thread::spawn(move || {
            let mut cache: Vec<Weak<PreparedAudio>> = Vec::new();
            while let Ok(job) = receiver.recv() {
                gate();
                let live = || {
                    worker_epoch.load(Ordering::SeqCst) == job.epoch
                        && !job.cancelled.load(Ordering::SeqCst)
                };
                let result = if !live() {
                    Err(AssetError::Cancelled)
                } else {
                    cache.retain(|entry| entry.strong_count() > 0);
                    let found = cache.iter().filter_map(Weak::upgrade).find(|entry| {
                        Arc::ptr_eq(&entry.original.bytes, &job.file.bytes)
                            || entry.original.bytes == job.file.bytes
                    });
                    if let Some(found) = found {
                        Ok(found)
                    } else {
                        let ext = std::path::Path::new(&job.file.name)
                            .extension()
                            .and_then(|s| s.to_str());
                        decode_sample(&job.file.bytes, ext, MAX_SAMPLE_SECONDS)
                            .map_err(|e| AssetError::Decode(e.to_string()))
                            .and_then(|sample| {
                                if !live() {
                                    return Err(AssetError::Cancelled);
                                }
                                let peaks = PeakPyramid::new(Arc::new(sample)).map_err(|_| {
                                    AssetError::Decode("Invalid decoded waveform".into())
                                })?;
                                let prepared = Arc::new(PreparedAudio {
                                    _reservation: job.reservation.clone(),
                                    original: job.file.clone(),
                                    peaks,
                                });
                                if cache.len() == CACHE {
                                    cache.remove(0);
                                }
                                cache.push(Arc::downgrade(&prepared));
                                Ok(prepared)
                            })
                    }
                };
                let _ = job.reply.try_send(if live() {
                    result
                } else {
                    Err(AssetError::Cancelled)
                });
            }
        });
        Self {
            sender: Some(sender),
            worker: Some(worker),
            epoch,
            queued_bytes,
            latest: Mutex::new(std::collections::HashMap::new()),
        }
    }
    /// T7 uses this monotonically advancing token as the project generation. Never reuse it.
    pub fn replace_project(&mut self) -> u64 {
        self.epoch
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_add(1))
            .expect("project generation exhausted")
            + 1
    }
    pub fn submit(
        &self,
        generation: u64,
        asset: u32,
        request: u64,
        file: SampleFile,
    ) -> Result<AssetRequest, AssetError> {
        if generation != self.epoch.load(Ordering::SeqCst) {
            return Err(AssetError::Cancelled);
        }
        let len = file.bytes.len();
        if len > SOURCE_LIMIT {
            return Err(AssetError::TooLarge);
        }
        let mut latest = self.latest.lock().unwrap();
        latest.retain(|_, token| token.strong_count() > 0);
        if latest.len() >= 4096 && !latest.contains_key(&asset) {
            return Err(AssetError::Busy);
        }
        let mut bytes = self.queued_bytes.lock().unwrap();
        if len > QUEUE_BYTES - *bytes {
            return Err(AssetError::Busy);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, receiver) = mpsc::sync_channel(1);
        let job = Job {
            reservation: Arc::new(SourceReservation {
                bytes: self.queued_bytes.clone(),
                len,
            }),
            file,
            epoch: generation,
            cancelled: cancelled.clone(),
            reply,
        };
        *bytes += len;
        drop(bytes);
        if let Err(error) = self.sender.as_ref().unwrap().try_send(job) {
            return Err(match error {
                TrySendError::Full(_) => AssetError::Busy,
                TrySendError::Disconnected(_) => AssetError::Cancelled,
            });
        }
        if let Some(old) = latest
            .insert(asset, Arc::downgrade(&cancelled))
            .and_then(|old| old.upgrade())
        {
            old.store(true, Ordering::SeqCst);
        }
        Ok(AssetRequest {
            generation,
            request,
            asset,
            epoch: self.epoch.clone(),
            cancelled,
            reply: receiver,
        })
    }
}
impl Drop for AudioAssets {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file() -> SampleFile {
        SampleFile {
            name: "wrong.extension".into(),
            bytes: Arc::new(include_bytes!("../../engine/tests/data/blip.flac").to_vec()),
        }
    }
    #[test]
    fn shared_original_decodes_once_and_preserves_bytes() {
        let worker = AudioAssets::new();
        let original = file();
        let a = worker
            .submit(0, 1, 1, original.clone())
            .unwrap()
            .receive()
            .unwrap();
        let b = worker
            .submit(0, 2, 2, original.clone())
            .unwrap()
            .receive()
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&original.bytes, &a.original.bytes));
        assert_eq!(a.peaks.sample().rate, 44100.0);
        assert_eq!(a.peaks.sample().len(), 4410);
    }
    #[test]
    fn held_generation_cancel_queue_and_shutdown() {
        let (entered, wait) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let mut first = true;
        let mut worker = AudioAssets::with_gate(move || {
            if first {
                first = false;
                entered.send(()).unwrap();
                gate.recv().unwrap();
            }
        });
        let old = worker.submit(0, 7, 1, file()).unwrap();
        wait.recv().unwrap();
        let queued: Vec<_> = (0..QUEUE)
            .map(|i| worker.submit(0, i as u32, 2, file()).unwrap())
            .collect();
        assert!(matches!(
            worker.submit(0, 8, 3, file()),
            Err(AssetError::Busy)
        ));
        worker.replace_project();
        assert!(matches!(
            worker.submit(0, 7, 99, file()),
            Err(AssetError::Cancelled)
        ));
        release.send(()).unwrap();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        for request in queued {
            assert!(matches!(request.receive(), Err(AssetError::Cancelled)));
        }
        let fresh = worker.submit(1, 7, 4, file()).unwrap();
        fresh.cancel();
        assert!(matches!(fresh.receive(), Err(AssetError::Cancelled)));
        let pending = worker.submit(1, 7, 5, file()).unwrap();
        drop(worker);
        assert!(matches!(pending.receive(), Err(AssetError::Cancelled)));
    }
    #[test]
    fn completed_reply_cannot_cross_generation() {
        let mut worker = AudioAssets::new();
        let old = worker.submit(0, 1, 1, file()).unwrap();
        let barrier = worker.submit(0, 2, 2, file()).unwrap().receive().unwrap();
        worker.replace_project();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        drop(barrier);
    }
    fn wav(frames: u32) -> SampleFile {
        let size = frames * 4;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + size).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&32000u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.resize(44 + size as usize, 0);
        SampleFile {
            name: "wrong.mp3".into(),
            bytes: Arc::new(bytes),
        }
    }
    #[test]
    fn wav_provenance_empty_and_too_long() {
        let worker = AudioAssets::new();
        let decoded = worker.submit(0, 1, 1, wav(17)).unwrap().receive().unwrap();
        assert_eq!(decoded.peaks.sample().len(), 17);
        assert_eq!(decoded.peaks.sample().rate, 8000.0);
        for frames in [0, 600 * 8000 + 1] {
            assert!(matches!(
                worker.submit(0, 1, 2, wav(frames)).unwrap().receive(),
                Err(AssetError::Decode(_))
            ));
        }
    }
    #[test]
    fn same_asset_supersedes_request_even_when_id_is_reused() {
        let (entered, wait) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let mut first = true;
        let worker = AudioAssets::with_gate(move || {
            if first {
                first = false;
                entered.send(()).unwrap();
                gate.recv().unwrap();
            }
        });
        let old = worker.submit(0, 1, 1, file()).unwrap();
        wait.recv().unwrap();
        let fresh = worker.submit(0, 1, 1, file()).unwrap();
        release.send(()).unwrap();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        assert!(fresh.receive().is_ok());
    }
    #[test]
    fn source_budget_follows_live_results_and_releases_on_drop() {
        let worker = AudioAssets::new();
        let bytes = worker.queued_bytes.clone();
        let file = file();
        let len = file.bytes.len();
        *bytes.lock().unwrap() = QUEUE_BYTES - len + 1;
        assert!(matches!(
            worker.submit(0, 1, 1, file.clone()),
            Err(AssetError::Busy)
        ));
        *bytes.lock().unwrap() = 0;
        let result = worker.submit(0, 1, 1, file).unwrap().receive().unwrap();
        drop(worker);
        let retained = *bytes.lock().unwrap();
        assert_eq!(retained, len);
        drop(result);
        let retained = *bytes.lock().unwrap();
        assert_eq!(retained, 0);
    }
    #[test]
    fn formats_and_corruption_use_existing_decoder() {
        let worker = AudioAssets::new();
        for (name, bytes) in [
            (
                "aif",
                include_bytes!("../../engine/tests/data/blip.aif").as_slice(),
            ),
            (
                "flac",
                include_bytes!("../../engine/tests/data/blip.flac").as_slice(),
            ),
            (
                "mp3",
                include_bytes!("../../engine/tests/data/blip.mp3").as_slice(),
            ),
            (
                "ogg",
                include_bytes!("../../engine/tests/data/blip.ogg").as_slice(),
            ),
        ] {
            let result = worker
                .submit(
                    0,
                    1,
                    1,
                    SampleFile {
                        name: name.into(),
                        bytes: Arc::new(bytes.to_vec()),
                    },
                )
                .unwrap()
                .receive()
                .unwrap();
            assert_eq!(result.peaks.sample().rate, 44100.0);
            assert!(!result.peaks.sample().is_empty());
        }
        assert!(matches!(
            worker
                .submit(
                    0,
                    1,
                    1,
                    SampleFile {
                        name: "bad.wav".into(),
                        bytes: Arc::new(vec![0; 90])
                    }
                )
                .unwrap()
                .receive(),
            Err(AssetError::Decode(_))
        ));
    }
}
