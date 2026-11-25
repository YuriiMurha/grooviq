//! Request authority is minted on the server and checked again under the commit borrow.
use super::*;
use crate::{arrangement_history::MediaVersion, audio_assets::PreparedAudio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const MAX_MEDIA_TICKET: u64 = (1 << 53) - 1;
const CANCELLED_MEDIA_TICKET_LIMIT: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MediaTarget {
    Import {
        track: u32,
        start_tick: u64,
        name: String,
    },
    Relink {
        asset: u32,
    },
}
#[derive(Clone)]
pub struct MediaIntent {
    generation: u64,
    revision: u64,
    request: u64,
    target: MediaTarget,
    cancelled: Arc<AtomicBool>,
}
impl MediaIntent {
    pub fn request(&self) -> u64 {
        self.request
    }
    pub fn key(&self) -> u32 {
        match self.target {
            MediaTarget::Relink { asset } => asset,
            _ => u32::MAX,
        }
    }
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
}
impl Session {
    pub fn media_warnings(&self) -> Vec<String> {
        self.project.arrangement.audio_assets.iter().filter_map(|a|match self.decoded.get(&a.id){
        Some(sample) if sample.rate!=a.source_rate as f32||sample.len() as u64!=a.source_frames=>Some(format!("Audio asset {}: decoded geometry differs from saved provenance; trim duration is preserved and unavailable decoded frames are silent.",a.id)),
        None=>Some(format!("Audio asset {} is missing; relink it to hear its clips.",a.id)),_=>None}).collect()
    }
    pub fn pending_media_requests(&self) -> Vec<u64> {
        self.media_requests.values().map(|t| t.request).collect()
    }
    pub fn set_play_mode_in(
        &mut self,
        generation: u64,
        revision: u64,
        mode: crate::PlayMode,
        now: Instant,
    ) -> Result<(), String> {
        self.arrangement_stamp(generation, revision)?;
        self.set_play_mode(mode, now).map_err(|e| e.to_string())
    }
    pub fn media_intent(
        &self,
        generation: u64,
        revision: u64,
        target: MediaTarget,
    ) -> Result<MediaIntent, String> {
        self.arrangement_stamp(generation, revision)?;
        match &target {
            MediaTarget::Import {
                track, start_tick, ..
            } => {
                if !self
                    .project
                    .arrangement
                    .tracks
                    .iter()
                    .any(|t| t.id == *track)
                    || *start_tick >= crate::MAX_SONG_TICKS
                {
                    return Err("Invalid import destination".into());
                }
            }
            MediaTarget::Relink { asset } => {
                if !self
                    .project
                    .arrangement
                    .audio_assets
                    .iter()
                    .any(|a| a.id == *asset)
                {
                    return Err("Audio asset removed".into());
                }
            }
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let request = NEXT
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 9_007_199_254_740_991).then_some(n + 1)
            })
            .map_err(|_| "Request identities exhausted")?;
        Ok(MediaIntent {
            generation,
            revision,
            request,
            target,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn check_media_intent(&self, intent: &MediaIntent) -> Result<(), String> {
        self.arrangement_stamp(intent.generation, intent.revision)?;
        if intent.cancelled.load(Ordering::SeqCst)
            || self
                .latest_media_requests
                .get(&intent.key())
                .is_some_and(|request| *request > intent.request)
        {
            return Err("Older or cancelled media request".into());
        }
        Ok(())
    }
    /// Called only after worker queue admission succeeds. Busy must not supersede prior intent.
    pub fn accept_media_request(&mut self, intent: &MediaIntent) -> Result<(), String> {
        self.check_media_intent(intent)?;
        self.latest_media_requests
            .insert(intent.key(), intent.request);
        if let Some(old) = self.media_requests.insert(intent.key(), intent.clone()) {
            old.cancelled.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
    pub fn cancel_media_request(&mut self, request: u64) -> bool {
        let key = self
            .media_requests
            .iter()
            .find(|(_, t)| t.request == request)
            .map(|(k, _)| *k);
        if let Some(key) = key {
            if let Some(t) = self.media_requests.remove(&key) {
                t.cancelled.store(true, Ordering::SeqCst);
            }
            true
        } else {
            false
        }
    }
    pub fn register_media_ticket(
        &mut self,
        ticket: u64,
        intent: &MediaIntent,
    ) -> Result<(), String> {
        if !(1..=MAX_MEDIA_TICKET).contains(&ticket) {
            return Err("Invalid media ticket".into());
        }
        if self.cancelled_media_tickets.contains(&ticket) {
            return Err("Media ticket cancelled".into());
        }
        let std::collections::hash_map::Entry::Vacant(entry) = self.media_tickets.entry(ticket)
        else {
            return Err("Media ticket already in use".into());
        };
        entry.insert(intent.cancellation());
        Ok(())
    }
    pub fn cancel_media_ticket(&mut self, ticket: u64) -> bool {
        if let Some(cancelled) = self.media_tickets.get(&ticket) {
            cancelled.store(true, Ordering::SeqCst);
            return true;
        }
        // Cancel can arrive before the import command registers its ticket.
        if self.cancelled_media_tickets.len() == CANCELLED_MEDIA_TICKET_LIMIT {
            self.cancelled_media_tickets.pop_front();
        }
        self.cancelled_media_tickets.push_back(ticket);
        false
    }
    pub fn release_media_ticket(&mut self, ticket: u64) {
        self.media_tickets.remove(&ticket);
    }
    pub(super) fn media_version(&self, id: u32) -> MediaVersion {
        MediaVersion {
            id,
            file: self.files.get(&id).cloned(),
            decoded: self.decoded.get(&id).cloned(),
            peaks: self.media_peaks.get(&id).cloned(),
        }
    }
    pub(super) fn publish_media_candidate(
        &mut self,
        project: &Project,
        files: &SampleFiles,
        decoded: &HashMap<u32, Arc<SampleBuf>>,
    ) -> Result<(), SessionError> {
        crate::validate_save(project, files).map_err(SessionError::Save)?;
        self.admit_media(files, decoded)?;
        if let Some(validate) = &self.precommit {
            validate(project).map_err(SessionError::InvalidValue)?;
        }
        let song = crate::compile_song::compile_song(project, decoded, None, self.song_note_limit)
            .map_err(SessionError::InvalidValue)?;
        lock(&self.controller).set_timeline(Self::timeline(project, song.clone()))?;
        self.compiled_song = song;
        self.song_error = None;
        Ok(())
    }
    pub fn publish_media(
        &mut self,
        intent: &MediaIntent,
        prepared: Arc<PreparedAudio>,
        now: Instant,
    ) -> Result<crate::arrangement_edit::ArrangementAck, String> {
        // This is the final authority check, AFTER receive, immediately before candidate work.
        self.arrangement_stamp(intent.generation, intent.revision)?;
        if intent.cancelled.load(Ordering::SeqCst)
            || !self
                .media_requests
                .get(&intent.key())
                .is_some_and(|t| t.request == intent.request)
        {
            return Err("Media request cancelled or superseded".into());
        }
        self.media_budget
            .original(&prepared.original.bytes)
            .map_err(|e| e.to_string())?;
        self.media_budget
            .peaks(&prepared.peaks)
            .map_err(|e| e.to_string())?;
        let mut candidate = self.project.clone();
        let mut ids = vec![];
        let asset = match &intent.target {
            MediaTarget::Import {
                track,
                start_tick,
                name,
            } => {
                if !candidate.arrangement.tracks.iter().any(|t| t.id == *track) {
                    return Err("Import track removed".into());
                }
                let asset = candidate.take_id().ok_or("Project ids exhausted")?;
                let clip = candidate.take_id().ok_or("Project ids exhausted")?;
                let sample = prepared.peaks.sample();
                candidate.arrangement.audio_assets.push(crate::AudioAsset {
                    id: asset,
                    sample: SampleRef {
                        path: prepared.original.name.clone().into(),
                        file: None,
                    },
                    source_rate: sample.rate as u32,
                    source_frames: sample.len() as u64,
                    original_tempo: None,
                    original_key: None,
                });
                candidate.arrangement.clips.push(crate::Clip {
                    id: clip,
                    track: *track,
                    name: name.clone(),
                    start_tick: *start_tick,
                    muted: false,
                    content: crate::ClipContent::Audio {
                        asset,
                        source_start: 0,
                        source_end: sample.len() as u64,
                        gain: 1.0,
                        reverse: false,
                        fade_in: 0,
                        fade_out: 0,
                    },
                });
                ids.extend([asset, clip]);
                asset
            }
            MediaTarget::Relink { asset } => {
                let a = candidate
                    .arrangement
                    .audio_assets
                    .iter_mut()
                    .find(|a| a.id == *asset)
                    .ok_or("Relink asset removed")?;
                a.sample = SampleRef {
                    path: prepared.original.name.clone().into(),
                    file: None,
                };
                *asset
            }
        };
        let mut files = self.files.clone();
        files.insert(asset, prepared.original.clone());
        let mut decoded = self.decoded.clone();
        decoded.insert(asset, prepared.peaks.sample().clone());
        let mut histories = self.staged_histories();
        histories.clear_redo();
        let mut change = crate::arrangement_history::Change::between(&self.project, &candidate);
        change.media.push((
            self.media_version(asset),
            MediaVersion {
                id: asset,
                file: Some(prepared.original.clone()),
                decoded: Some(prepared.peaks.sample().clone()),
                peaks: Some(prepared.peaks.clone()),
            },
        ));
        histories.protected.insert(change.order);
        histories.arrangement.undo.push(change);
        self.admit_histories(&mut histories)
            .map_err(|e| e.to_string())?;
        self.publish_media_candidate(&candidate, &files, &decoded)
            .map_err(|e| e.to_string())?;
        self.project = candidate;
        self.files = files;
        self.decoded = decoded;
        self.media_peaks.insert(asset, prepared.peaks.clone());
        self.install_histories(histories);
        self.media_requests.remove(&intent.key());
        self.changed(true, now);
        Ok(crate::arrangement_edit::ArrangementAck {
            state: self.arrangement_view(),
            created_ids: ids,
            changed: true,
        })
    }
    pub fn waveform_source(
        &self,
        generation: u64,
        asset: u32,
    ) -> Result<
        (
            Arc<SampleBuf>,
            Option<Arc<grooviq_engine::waveform::PeakPyramid>>,
        ),
        String,
    > {
        if generation != self.generation {
            return Err("Project replaced".into());
        }
        Ok((
            self.decoded
                .get(&asset)
                .cloned()
                .ok_or("Audio is missing; relink it")?,
            self.media_peaks.get(&asset).cloned(),
        ))
    }
    pub fn seek_song(&self, generation: u64, revision: u64, tick: u64) -> Result<(), String> {
        self.arrangement_stamp(generation, revision)?;
        if self.song_error.is_some() {
            return Err("Repair Song before seeking".into());
        }
        self.send(Command::Seek { tick }).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{arrangement_edit::ArrangementEdit, audio_assets::AudioAssets};
    #[test]
    fn final_generation_is_checked_independently_of_request_identity() {
        for cause in 0..4 {
            let (c, _p) = grooviq_engine::engine(grooviq_engine::EngineConfig {
                sample_rate: 48000.0,
            });
            let mut s = Session::new(Arc::new(Mutex::new(c)));
            let v = s.arrangement_view();
            let track = s
                .edit_arrangement(
                    v.generation,
                    v.revision,
                    vec![ArrangementEdit::CreateTrack {
                        name: "T".into(),
                        color: 0,
                    }],
                    Instant::now(),
                )
                .unwrap()
                .created_ids[0];
            let mut bytes = Vec::new();
            let n = 16u32;
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + n * 2).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&48000u32.to_le_bytes());
            bytes.extend_from_slice(&96000u32.to_le_bytes());
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&16u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&(n * 2).to_le_bytes());
            bytes.extend_from_slice(&[0u8; 32]);
            let v = s.arrangement_view();
            let mut intent = s
                .media_intent(
                    v.generation,
                    v.revision,
                    MediaTarget::Import {
                        track,
                        start_tick: 0,
                        name: "A".into(),
                    },
                )
                .unwrap();
            let assets = AudioAssets::with_budget(s.media_budget());
            let prepared = assets
                .submit(
                    0,
                    intent.key(),
                    intent.request(),
                    crate::SampleFile {
                        name: "generated.wav".into(),
                        bytes: Arc::new(bytes),
                    },
                )
                .unwrap()
                .receive()
                .unwrap();
            // Isolate each fence with the other server-owned facts valid. Real lifecycle
            // races remain covered through New/Open/recovery in integration tests.
            match cause {
                0 => intent.generation = s.generation + 1,
                1 => intent.revision = s.arrangement_revision + 1,
                2 => intent.cancelled.store(true, Ordering::SeqCst),
                _ => {
                    intent.target = MediaTarget::Import {
                        track: 999,
                        start_tick: 0,
                        name: "Deleted target".into(),
                    }
                }
            }
            s.media_requests.insert(intent.key(), intent.clone());
            let before = s.project.clone();
            assert!(s.publish_media(&intent, prepared, Instant::now()).is_err());
            assert_eq!(s.project, before);
        }
    }
}
