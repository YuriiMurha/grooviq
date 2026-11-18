//! Bounded control-worker asset preparation, independent of Session and persisted schemas.
use crate::SampleFile;
use grooviq_engine::{MAX_SAMPLE_SECONDS, decode_sample, waveform::PeakPyramid};
use std::collections::HashMap;
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
#[derive(Clone, Copy)]
struct Limits {
    source: usize,
    bytes: usize,
    tokens: usize,
    cache: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            source: SOURCE_LIMIT,
            bytes: QUEUE_BYTES,
            tokens: 4096,
            cache: CACHE,
        }
    }
}
#[derive(Default)]
struct SourceBudget {
    used: usize,
    sources: HashMap<usize, Weak<SourceReservation>>,
}
struct SourceReservation {
    budget: Arc<Mutex<SourceBudget>>,
    source: Arc<Vec<u8>>,
}
impl Drop for SourceReservation {
    fn drop(&mut self) {
        let mut budget = self.budget.lock().unwrap();
        budget.used -= self.source.len();
        let key = Arc::as_ptr(&self.source) as usize;
        // A new lease may replace the expired Weak while this Drop waits for the lock.
        if budget
            .sources
            .get(&key)
            .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
        {
            budget.sources.remove(&key);
        }
    }
}

pub struct PreparedAudio {
    _reservation: Arc<SourceReservation>,
    /// This request's name and exact original byte allocation, including on cache hits.
    pub original: SampleFile,
    /// Shared immutable decoded samples and peaks, independent of original file metadata.
    pub peaks: Arc<PeakPyramid>,
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
    generation: u64,
    pub request: u64,
    pub asset: u32,
    epoch: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
    reply: Receiver<Result<Arc<PreparedAudio>, AssetError>>,
}
impl AssetRequest {
    pub fn generation(&self) -> u64 {
        self.generation
    }
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Queued,
    Decode,
    Decoded,
    Built,
    Cached(usize),
}
struct CacheEntry {
    source: Weak<Vec<u8>>,
    peaks: Weak<PeakPyramid>,
}
pub struct AudioAssets {
    sender: Option<SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
    epoch: Arc<AtomicU64>,
    queued_bytes: Arc<Mutex<SourceBudget>>,
    latest: Mutex<HashMap<u32, Weak<AtomicBool>>>,
    limits: Limits,
}
impl Default for AudioAssets {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioAssets {
    pub fn new() -> Self {
        Self::with_hooks(Limits::default(), |_| {})
    }
    #[cfg(test)]
    fn with_gate(mut gate: impl FnMut() + Send + 'static) -> Self {
        Self::with_hooks(Limits::default(), move |stage| {
            if stage == Stage::Queued {
                gate()
            }
        })
    }
    fn with_hooks(limits: Limits, mut hook: impl FnMut(Stage) + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE);
        let epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = epoch.clone();
        let queued_bytes = Arc::new(Mutex::new(SourceBudget::default()));
        let worker = thread::spawn(move || {
            let mut cache: Vec<CacheEntry> = Vec::new();
            while let Ok(job) = receiver.recv() {
                hook(Stage::Queued);
                let live = || {
                    worker_epoch.load(Ordering::SeqCst) == job.epoch
                        && !job.cancelled.load(Ordering::SeqCst)
                };
                let result = if !live() {
                    Err(AssetError::Cancelled)
                } else {
                    cache.retain(|entry| {
                        entry.source.strong_count() > 0 && entry.peaks.strong_count() > 0
                    });
                    let found = cache.iter().find_map(|entry| {
                        let source = entry.source.upgrade()?;
                        if Arc::ptr_eq(&source, &job.file.bytes) || source == job.file.bytes {
                            entry.peaks.upgrade()
                        } else {
                            None
                        }
                    });
                    let peaks = if let Some(found) = found {
                        Ok(found)
                    } else {
                        let ext = std::path::Path::new(&job.file.name)
                            .extension()
                            .and_then(|s| s.to_str());
                        hook(Stage::Decode);
                        decode_sample(&job.file.bytes, ext, MAX_SAMPLE_SECONDS)
                            .map_err(|e| AssetError::Decode(e.to_string()))
                            .and_then(|sample| {
                                hook(Stage::Decoded);
                                if !live() {
                                    return Err(AssetError::Cancelled);
                                }
                                let peaks =
                                    Arc::new(PeakPyramid::new(Arc::new(sample)).map_err(|_| {
                                        AssetError::Decode("Invalid decoded waveform".into())
                                    })?);
                                hook(Stage::Built);
                                Ok(peaks)
                            })
                    };
                    peaks.map(|peaks| {
                        if !cache
                            .iter()
                            .any(|entry| entry.source.ptr_eq(&Arc::downgrade(&job.file.bytes)))
                        {
                            if cache.len() == limits.cache {
                                cache.remove(0);
                            }
                            cache.push(CacheEntry {
                                source: Arc::downgrade(&job.file.bytes),
                                peaks: Arc::downgrade(&peaks),
                            });
                        }
                        hook(Stage::Cached(cache.len()));
                        Arc::new(PreparedAudio {
                            _reservation: job.reservation.clone(),
                            original: job.file.clone(),
                            peaks,
                        })
                    })
                };
                // receive() is the publication fence, including cancellation after this send.
                let _ = job.reply.try_send(result);
            }
        });
        Self {
            sender: Some(sender),
            worker: Some(worker),
            epoch,
            queued_bytes,
            latest: Mutex::new(HashMap::new()),
            limits,
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
        if len > self.limits.source {
            return Err(AssetError::TooLarge);
        }
        let mut latest = self.latest.lock().unwrap();
        latest.retain(|_, token| token.strong_count() > 0);
        if latest.len() >= self.limits.tokens && !latest.contains_key(&asset) {
            return Err(AssetError::Busy);
        }
        let mut budget = self.queued_bytes.lock().unwrap();
        let key = Arc::as_ptr(&file.bytes) as usize;
        let reservation = if let Some(existing) = budget.sources.get(&key).and_then(Weak::upgrade) {
            existing
        } else {
            if len > self.limits.bytes - budget.used {
                return Err(AssetError::Busy);
            }
            budget.used += len;
            let reservation = Arc::new(SourceReservation {
                budget: self.queued_bytes.clone(),
                source: file.bytes.clone(),
            });
            budget.sources.insert(key, Arc::downgrade(&reservation));
            reservation
        };
        drop(budget);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (reply, receiver) = mpsc::sync_channel(1);
        let job = Job {
            reservation,
            file,
            epoch: generation,
            cancelled: cancelled.clone(),
            reply,
        };
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
    struct Release(Option<SyncSender<()>>);
    impl Release {
        fn open(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.try_send(());
            }
        }
    }
    impl Drop for Release {
        fn drop(&mut self) {
            self.open();
        }
    }
    fn held(
        limits: Limits,
        target: Stage,
    ) -> (AudioAssets, Release, Receiver<()>, Arc<Mutex<Vec<Stage>>>) {
        let (entered, wait) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let stages = Arc::new(Mutex::new(Vec::new()));
        let observed = stages.clone();
        let mut first = true;
        let worker = AudioAssets::with_hooks(limits, move |stage| {
            observed.lock().unwrap().push(stage);
            if first && stage == target {
                first = false;
                let _ = entered.send(());
                let _ = gate.recv();
            }
        });
        (worker, Release(Some(release)), wait, stages)
    }
    #[test]
    fn busy_does_not_supersede_the_current_request() {
        let (worker, mut release, wait, _) = held(Limits::default(), Stage::Queued);
        let old = worker.submit(0, 7, 1, file()).unwrap();
        wait.recv().unwrap();
        let queued: Vec<_> = (0..QUEUE)
            .map(|id| worker.submit(0, id as u32, 1, file()).unwrap())
            .collect();
        assert!(matches!(
            worker.submit(0, 7, 2, file()),
            Err(AssetError::Busy)
        ));
        release.open();
        assert!(old.receive().is_ok());
        for request in queued {
            assert!(request.receive().is_ok());
        }
    }
    #[test]
    fn completed_reply_cannot_survive_same_asset_supersession() {
        let worker = AudioAssets::new();
        let old = worker.submit(0, 7, 1, file()).unwrap();
        let barrier = worker.submit(0, 8, 1, file()).unwrap().receive().unwrap();
        let new = worker.submit(0, 7, 2, file()).unwrap();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        assert!(new.receive().is_ok());
        drop(barrier);
    }
    #[test]
    fn private_source_and_identity_limits_are_independent() {
        let source = file();
        let len = source.bytes.len();
        let worker = AudioAssets::with_hooks(
            Limits {
                source: len - 1,
                ..Limits::default()
            },
            |_| {},
        );
        assert!(matches!(
            worker.submit(0, 1, 1, source.clone()),
            Err(AssetError::TooLarge)
        ));
        let worker = AudioAssets::with_hooks(
            Limits {
                source: len,
                tokens: 2,
                ..Limits::default()
            },
            |_| {},
        );
        let a = worker.submit(0, 1, 1, source.clone()).unwrap();
        let pa = a.receive().unwrap();
        let b = worker.submit(0, 2, 1, source.clone()).unwrap();
        let pb = b.receive().unwrap();
        assert!(matches!(
            worker.submit(0, 3, 1, source.clone()),
            Err(AssetError::Busy)
        ));
        let replacement = worker.submit(0, 2, 2, source.clone()).unwrap();
        assert!(replacement.receive().is_ok());
        drop(a);
        assert!(worker.submit(0, 3, 1, source).unwrap().receive().is_ok());
        drop((pa, pb));
    }
    #[test]
    fn cache_is_bounded_and_only_weak() {
        let counts = Arc::new(Mutex::new(Vec::new()));
        let observed = counts.clone();
        let worker = AudioAssets::with_hooks(
            Limits {
                cache: 2,
                ..Limits::default()
            },
            move |stage| {
                if let Stage::Cached(len) = stage {
                    observed.lock().unwrap().push(len)
                }
            },
        );
        let mut retained = Vec::new();
        for frames in 1..=5 {
            retained.push(
                worker
                    .submit(0, frames, 1, wav(frames))
                    .unwrap()
                    .receive()
                    .unwrap(),
            );
        }
        let last = Arc::downgrade(&retained[4].peaks);
        drop(retained);
        let barrier = worker.submit(0, 9, 1, wav(6)).unwrap().receive().unwrap();
        let counts = counts.lock().unwrap();
        assert!(counts.iter().all(|count| *count <= 2));
        assert!(counts.contains(&2));
        assert!(last.upgrade().is_none());
        drop(barrier);
    }
    #[test]
    fn source_budget_charges_equal_allocations_but_not_repeated_handles() {
        let source = file();
        let len = source.bytes.len();
        let worker = AudioAssets::with_hooks(
            Limits {
                bytes: len,
                ..Limits::default()
            },
            |_| {},
        );
        let a = worker
            .submit(0, 1, 1, source.clone())
            .unwrap()
            .receive()
            .unwrap();
        let b = worker.submit(0, 2, 1, source).unwrap().receive().unwrap();
        assert!(Arc::ptr_eq(&a.peaks, &b.peaks));
        assert!(matches!(
            worker.submit(0, 3, 1, file()),
            Err(AssetError::Busy)
        ));
    }
    #[test]
    fn reservations_track_distinct_allocations_independent_of_payload_lifetime() {
        let worker = AudioAssets::new();
        let bytes = worker.queued_bytes.clone();
        let first = file();
        let second = file();
        let len = first.bytes.len();
        let a = worker
            .submit(0, 1, 1, first.clone())
            .unwrap()
            .receive()
            .unwrap();
        let b = worker
            .submit(0, 2, 1, second.clone())
            .unwrap()
            .receive()
            .unwrap();
        let c = worker
            .submit(0, 3, 1, first.clone())
            .unwrap()
            .receive()
            .unwrap();
        assert!(Arc::ptr_eq(&a.peaks, &b.peaks));
        assert!(Arc::ptr_eq(&a.peaks, &c.peaks));
        assert!(Arc::ptr_eq(&b.original.bytes, &second.bytes));
        drop(worker);
        let used = bytes.lock().unwrap().used;
        assert_eq!(used, len * 2);
        drop(a);
        drop(c);
        let used = bytes.lock().unwrap().used;
        assert_eq!(used, len);
        drop(b);
        let budget = bytes.lock().unwrap();
        assert_eq!(budget.used, 0);
        assert!(budget.sources.is_empty());
    }
    #[test]
    fn cancelled_queued_jobs_never_decode() {
        let (mut worker, mut release, wait, stages) = held(Limits::default(), Stage::Queued);
        let old = worker.submit(0, 1, 1, file()).unwrap();
        wait.recv().unwrap();
        let queued = worker.submit(0, 2, 1, file()).unwrap();
        worker.replace_project();
        release.open();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        assert!(matches!(queued.receive(), Err(AssetError::Cancelled)));
        drop(worker);
        assert!(!stages.lock().unwrap().contains(&Stage::Decode));
    }
    #[test]
    fn cancelled_decode_never_builds_peaks() {
        let (worker, mut release, wait, stages) = held(Limits::default(), Stage::Decoded);
        let old = worker.submit(0, 1, 1, file()).unwrap();
        wait.recv().unwrap();
        old.cancel();
        release.open();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        drop(worker);
        assert!(!stages.lock().unwrap().contains(&Stage::Built));
    }
    #[test]
    fn held_gate_releases_during_assertion_unwind() {
        let failure = std::panic::catch_unwind(|| {
            let (worker, _release, wait, _) = held(Limits::default(), Stage::Queued);
            let _request = worker.submit(0, 1, 1, file()).unwrap();
            wait.recv().unwrap();
            panic!("intentional gate-unwind regression");
        });
        assert!(failure.is_err());
    }
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
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&a.peaks, &b.peaks));
        assert!(Arc::ptr_eq(&original.bytes, &a.original.bytes));
        assert_eq!(a.peaks.sample().rate, 44100.0);
        assert_eq!(a.peaks.sample().len(), 4410);
    }
    #[test]
    fn equal_content_preserves_each_requests_original_identity() {
        let worker = AudioAssets::new();
        let first = file();
        let mut second = file();
        second.name = "second.wav".into();
        assert!(!Arc::ptr_eq(&first.bytes, &second.bytes));
        let a = worker
            .submit(0, 1, 1, first.clone())
            .unwrap()
            .receive()
            .unwrap();
        let b = worker
            .submit(0, 2, 2, second.clone())
            .unwrap()
            .receive()
            .unwrap();
        assert_eq!(b.original.name, second.name);
        assert!(Arc::ptr_eq(&a.original.bytes, &first.bytes));
        assert!(Arc::ptr_eq(&b.original.bytes, &second.bytes));
        assert!(Arc::ptr_eq(a.peaks.sample(), b.peaks.sample()));
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
        let mut release = Release(Some(release));
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
        release.open();
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
        assert_eq!(old.generation(), 0);
        let barrier = worker.submit(0, 2, 2, file()).unwrap().receive().unwrap();
        worker.replace_project();
        assert_eq!(old.generation(), 0);
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
        let mut release = Release(Some(release));
        let old = worker.submit(0, 1, 1, file()).unwrap();
        wait.recv().unwrap();
        let fresh = worker.submit(0, 1, 1, file()).unwrap();
        release.open();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        assert!(fresh.receive().is_ok());
    }
    #[test]
    fn source_budget_follows_live_results_and_releases_on_drop() {
        let worker = AudioAssets::new();
        let bytes = worker.queued_bytes.clone();
        let file = file();
        let len = file.bytes.len();
        bytes.lock().unwrap().used = QUEUE_BYTES - len + 1;
        assert!(matches!(
            worker.submit(0, 1, 1, file.clone()),
            Err(AssetError::Busy)
        ));
        bytes.lock().unwrap().used = 0;
        let result = worker.submit(0, 1, 1, file).unwrap().receive().unwrap();
        drop(worker);
        let retained = bytes.lock().unwrap().used;
        assert_eq!(retained, len);
        drop(result);
        let retained = bytes.lock().unwrap().used;
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
    #[test]
    fn old_reservation_drop_preserves_replacement_lease() {
        let worker = AudioAssets::new();
        let budget = worker.queued_bytes.clone();
        let f = file();
        let len = f.bytes.len();
        let key = Arc::as_ptr(&f.bytes) as usize;
        let r = worker
            .submit(0, 1, 1, f.clone())
            .unwrap()
            .receive()
            .unwrap();
        // A later completed job proves the worker released the first job's lease.
        assert!(matches!(
            worker
                .submit(
                    0,
                    2,
                    0,
                    SampleFile {
                        name: "empty.wav".into(),
                        bytes: Arc::new(Vec::new()),
                    },
                )
                .unwrap()
                .receive(),
            Err(AssetError::Decode(_))
        ));
        let weak_old = budget.lock().unwrap().sources.get(&key).unwrap().clone();
        let mut guard = budget.lock().unwrap();
        let t = std::thread::spawn(move || drop(r));
        while weak_old.strong_count() > 0 {
            std::thread::yield_now();
        }
        // Old Drop is now parked on the lock; replace the expired Weak exactly as submit does.
        guard.used += len;
        let newer = Arc::new(SourceReservation {
            budget: budget.clone(),
            source: f.bytes.clone(),
        });
        guard.sources.insert(key, Arc::downgrade(&newer));
        drop(guard);
        t.join().unwrap();
        let (kept_newer, used) = {
            let b = budget.lock().unwrap();
            (
                b.sources
                    .get(&key)
                    .and_then(Weak::upgrade)
                    .is_some_and(|e| Arc::ptr_eq(&e, &newer)),
                b.used,
            )
        };
        assert!(kept_newer, "old Drop removed the newer lease");
        assert_eq!(used, len);
        let again = worker
            .submit(0, 2, 1, f.clone())
            .unwrap()
            .receive()
            .unwrap();
        let used = budget.lock().unwrap().used;
        assert_eq!(used, len, "same Arc charged twice");
        drop((again, newer));
        drop(worker);
        assert_eq!(budget.lock().unwrap().used, 0);
    }
    #[test]
    fn cancelled_queued_job_never_decodes() {
        let (worker, mut release, wait, stages) = held(Limits::default(), Stage::Queued);
        let first = worker.submit(0, 1, 1, file()).unwrap();
        wait.recv().unwrap();
        let stale = worker.submit(0, 2, 1, wav(5)).unwrap(); // distinct bytes: no cache hit
        let superseded = worker.submit(0, 3, 1, wav(6)).unwrap();
        let fresh = worker.submit(0, 3, 2, file()).unwrap();
        stale.cancel();
        release.open();
        let first_result = first.receive().unwrap(); // keep the cache entry alive
        assert!(matches!(stale.receive(), Err(AssetError::Cancelled)));
        assert!(matches!(superseded.receive(), Err(AssetError::Cancelled)));
        assert!(fresh.receive().is_ok());
        drop(worker);
        drop(first_result);
        let decodes = stages
            .lock()
            .unwrap()
            .iter()
            .filter(|s| **s == Stage::Decode)
            .count();
        // first decodes; fresh hits the cache (same bytes); cancelled/superseded never decode.
        assert_eq!(decodes, 1, "cancelled or superseded queued job decoded");
    }
    #[test]
    fn replaced_project_decode_never_builds_peaks() {
        let (mut worker, mut release, wait, stages) = held(Limits::default(), Stage::Decoded);
        let old = worker.submit(0, 1, 1, file()).unwrap();
        wait.recv().unwrap();
        worker.replace_project();
        release.open();
        assert!(matches!(old.receive(), Err(AssetError::Cancelled)));
        drop(worker);
        assert!(!stages.lock().unwrap().contains(&Stage::Built));
    }
}
