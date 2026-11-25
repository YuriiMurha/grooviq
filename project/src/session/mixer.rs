use super::*;
use crate::compile::channel_setups;
use crate::mixer_compile::compile_mix;
use grooviq_engine::{SongData, SourceEdit};

impl Session {
    pub(super) fn publish_prepared_candidate(
        &mut self,
        candidate: &Project,
        song: SongData,
        decoded: Option<&HashMap<u32, Arc<SampleBuf>>>,
    ) -> Result<(), SessionError> {
        let mix = compile_mix(candidate).map_err(mixer_error)?;
        let before: HashSet<_> = self.project.channels.iter().map(|c| c.id).collect();
        let after: HashSet<_> = candidate.channels.iter().map(|c| c.id).collect();
        let remove = self
            .project
            .channels
            .iter()
            .filter(|c| !after.contains(&c.id))
            .map(|c| ChannelId(c.id))
            .collect();
        let add = if after.is_subset(&before) {
            Vec::new()
        } else {
            channel_setups(candidate, decoded.unwrap_or(&self.decoded))
                .into_iter()
                .filter(|c| !before.contains(&c.id.0))
                .collect()
        };
        lock(&self.controller).set_sources_and_mix(SourceEdit {
            remove,
            add,
            timeline: Some(Self::timeline(candidate, song.clone())),
            mix,
        })?;
        self.compiled_song = song;
        self.song_error = None;
        Ok(())
    }
}

use crate::mixer_edit::{MixerAck, MixerEdit, MixerView};

impl Session {
    pub fn mixer_view(&self) -> MixerView {
        MixerView {
            generation: self.generation,
            revision: self.arrangement_revision,
            mixer: self.project.mixer.clone(),
        }
    }

    pub fn edit_mixer(
        &mut self,
        generation: u64,
        revision: u64,
        ops: Vec<MixerEdit>,
        now: Instant,
    ) -> Result<MixerAck, SessionError> {
        self.arrangement_stamp(generation, revision)
            .map_err(SessionError::InvalidValue)?;
        let mut candidate = self.project.clone();
        let created_ids =
            crate::mixer_edit::apply(&mut candidate, ops).map_err(SessionError::InvalidValue)?;
        let change = crate::arrangement_history::Change::between(&self.project, &candidate);
        if change.mixer.is_none() {
            return Ok(MixerAck {
                state: self.mixer_view(),
                created_ids: Vec::new(),
                changed: false,
            });
        }
        let mut histories = self.staged_histories();
        histories.clear_redo();
        histories.protected.insert(change.order);
        histories.arrangement.undo.push(change);
        self.admit_histories(&mut histories)?;

        self.publish_candidate(&candidate)?;
        self.project = candidate;
        self.install_histories(histories);
        self.changed(true, now);
        Ok(MixerAck {
            state: self.mixer_view(),
            created_ids,
            changed: true,
        })
    }
}
