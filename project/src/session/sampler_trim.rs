//! Sampler-only pair transactions. The inverse retains marker bits, never a stale parameter bank.
use super::*;

#[derive(Clone, Debug)]
pub(crate) struct TrimChange {
    pub channel: u32,
    pub order: u64,
    stamp: String,
    before: [Option<f32>; 2],
    after: [Option<f32>; 2],
}
#[derive(Clone, Debug, Default)]
pub(crate) struct TrimHistory {
    pub undo: Vec<TrimChange>,
    pub redo: Vec<TrimChange>,
}
impl TrimHistory {
    pub fn bytes(&self) -> usize {
        (self.undo.capacity() + self.redo.capacity()) * std::mem::size_of::<TrimChange>()
            + self
                .undo
                .iter()
                .chain(&self.redo)
                .map(|c| c.stamp.capacity())
                .sum::<usize>()
    }
}
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "ipc-contract", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TrimView {
    pub stamp: String,
    pub start: f32,
    pub end: f32,
    pub frames: Option<usize>,
    pub undo: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TrimAck {
    pub state: TrimView,
    pub changed: bool,
}
impl Session {
    pub(super) fn next_trim_epoch(&self) -> Result<u64, SessionError> {
        self.trim_epoch.checked_add(1).ok_or_else(|| {
            SessionError::InvalidValue("Trim identities exhausted; reopen project".into())
        })
    }
    pub(super) fn invalidate_trim(&mut self, channel: u32, sample: bool) {
        // Every caller checked exhaustion before irreversible engine admission.
        self.trim_epoch += 1;
        let entry = self.trim_identities.entry(channel).or_default();
        if sample {
            entry.0 = self.trim_epoch;
        }
        entry.1 = self.trim_epoch;
        self.trim_history.undo.retain(|c| c.channel != channel);
        self.trim_history.redo.retain(|c| c.channel != channel);
        self.trim_history.undo.shrink_to_fit();
        self.trim_history.redo.shrink_to_fit();
    }
    fn trim_stamp(&self, channel: u32) -> Result<String, SessionError> {
        let i = self.channel_index(channel)?;
        if self.project.channels[i].instrument != InstrumentKind::Sampler {
            return Err(SessionError::InvalidValue("Not a sampler channel".into()));
        }
        let instance = lock(&self.controller)
            .channel_instance(ChannelId(channel))
            .ok_or(SessionError::InvalidValue("Channel removed".into()))?;
        let (sample, edit) = self
            .trim_identities
            .get(&channel)
            .copied()
            .unwrap_or_default();
        Ok(format!("{}:{instance}:{sample}:{edit}", self.generation))
    }
    pub fn sampler_trim_view(&self, channel: u32) -> Result<TrimView, SessionError> {
        let stamp = self.trim_stamp(channel)?;
        let c = &self.project.channels[self.channel_index(channel)?];
        let values = sampler_values(c);
        Ok(TrimView {
            undo: self
                .trim_history
                .undo
                .iter()
                .rev()
                .find(|c| c.channel == channel && c.stamp == stamp)
                .map(|c| c.order.to_string()),
            stamp,
            start: values[3],
            end: values[4],
            frames: self.decoded.get(&channel).map(|s| s.len()),
        })
    }
    fn check_trim_stamp(&self, channel: u32, stamp: &str) -> Result<(), SessionError> {
        if self.trim_stamp(channel)? != stamp {
            return Err(SessionError::InvalidValue(
                "Sample or trim changed; reload trim".into(),
            ));
        }
        Ok(())
    }
    pub fn sampler_trim_reset(
        &mut self,
        channel: u32,
        stamp: &str,
        now: Instant,
    ) -> Result<TrimAck, SessionError> {
        self.check_trim_stamp(channel, stamp)?;
        let view = self.sampler_trim_view(channel)?;
        if view.frames.is_none_or(|n| n == 0) {
            return Err(SessionError::InvalidValue(
                "No decoded sample to repair".into(),
            ));
        }
        if view.start == 0.0 && view.end == 1.0 {
            return Ok(TrimAck {
                state: view,
                changed: false,
            });
        }
        let index = self.channel_index(channel)?;
        let map = &self.project.channels[index].sampler;
        let before = [map.get("start").copied(), map.get("end").copied()];
        let epoch = self.next_trim_epoch()?;
        let mut h = self.staged_histories();
        h.clear_redo();
        h.trim.undo.retain(|c| c.channel != channel);
        let order = crate::undo::next_order();
        h.protected.insert(order);
        h.trim.undo.push(TrimChange {
            channel,
            order,
            stamp: format!("{}:{epoch}", stamp.rsplit_once(':').unwrap().0),
            before,
            after: [Some(0.0), Some(1.0)],
        });
        self.commit_trim(channel, [Some(0.0), Some(1.0)], epoch, h, now)?;
        Ok(TrimAck {
            state: self.sampler_trim_view(channel)?,
            changed: true,
        })
    }
    pub fn sampler_trim_undo(
        &mut self,
        channel: u32,
        stamp: &str,
        token: &str,
        now: Instant,
    ) -> Result<TrimAck, SessionError> {
        self.check_trim_stamp(channel, stamp)?;
        let order = self
            .trim_history
            .undo
            .iter()
            .find(|c| c.channel == channel && c.stamp == stamp && c.order.to_string() == token)
            .map(|c| c.order)
            .ok_or_else(|| {
                SessionError::InvalidValue("Undo reset is no longer available".into())
            })?;
        self.step_trim_history(channel, order, true, now)?;
        Ok(TrimAck {
            state: self.sampler_trim_view(channel)?,
            changed: true,
        })
    }
    pub(super) fn step_trim_history(
        &mut self,
        channel: u32,
        order: u64,
        undo: bool,
        now: Instant,
    ) -> Result<(), SessionError> {
        let mut h = self.staged_histories();
        let source = if undo {
            &mut h.trim.undo
        } else {
            &mut h.trim.redo
        };
        let at = source
            .iter()
            .position(|c| c.channel == channel && c.order == order)
            .ok_or_else(|| SessionError::InvalidValue("Trim history evicted".into()))?;
        let mut change = source.remove(at);
        self.check_trim_stamp(channel, &change.stamp)?;
        let values = if undo { change.before } else { change.after };
        let epoch = self.next_trim_epoch()?;
        change.stamp = format!("{}:{epoch}", change.stamp.rsplit_once(':').unwrap().0);
        if undo {
            h.trim.redo.push(change);
        } else {
            h.trim.undo.push(change);
        }
        h.protected.insert(order);
        h.trim.undo.sort_by_key(|c| c.order);
        h.trim.redo.sort_by_key(|c| std::cmp::Reverse(c.order));
        self.commit_trim(channel, values, epoch, h, now)?;
        Ok(())
    }
    fn commit_trim(
        &mut self,
        channel: u32,
        values: [Option<f32>; 2],
        epoch: u64,
        mut histories: crate::arrangement_history::Histories,
        now: Instant,
    ) -> Result<(), SessionError> {
        self.arrangement_stamp(self.generation, self.arrangement_revision)
            .map_err(SessionError::InvalidValue)?;
        let index = self.channel_index(channel)?;
        let mut candidate = self.project.clone();
        for (name, value) in ["start", "end"].into_iter().zip(values) {
            if let Some(value) = value {
                candidate.channels[index].sampler.insert(name.into(), value);
            } else {
                candidate.channels[index].sampler.remove(name);
            }
        }
        self.admit_histories(&mut histories)?;
        self.admit(&candidate)?;
        let params: [f32; grooviq_dsp::sampler::SPEC.len()] =
            sampler_values(&candidate.channels[index])
                .try_into()
                .expect("sampler SPEC length");
        let mut controller = lock(&self.controller);
        let instance = controller
            .channel_instance(ChannelId(channel))
            .ok_or(SessionError::InvalidValue("Channel removed".into()))?;
        controller.send_at(
            0,
            Command::SetSamplerTrim {
                channel: ChannelId(channel),
                instance,
                params,
            },
        )?;
        drop(controller);
        self.project = candidate;
        self.install_histories(histories);
        self.trim_epoch = epoch;
        self.trim_identities.entry(channel).or_default().1 = epoch;
        self.changed(false, now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rig() -> (Session, u32) {
        let (c, _) = grooviq_engine::engine(grooviq_engine::EngineConfig {
            sample_rate: 48000.0,
        });
        let mut s = Session::new(Arc::new(Mutex::new(c)));
        let id = s
            .add_channel(
                Path::new("tiny.wav"),
                crate::SampleFile {
                    name: "tiny.wav".into(),
                    bytes: Arc::new(vec![]),
                },
                Arc::new(SampleBuf {
                    rate: 48000.0,
                    l: vec![0.5],
                    r: vec![0.5],
                }),
                Instant::now(),
            )
            .unwrap()
            .channels[0]
            .id;
        (s, id)
    }
    #[test]
    fn exact_absent_default_inverse_unknown_sample_and_counter_exhaustion() {
        let (mut s, id) = rig();
        s.project.channels[0].sampler.remove("start");
        s.project.channels[0].sampler.insert("end".into(), 0.7);
        let before = s.project.channels[0].sampler.clone();
        let v = s.sampler_trim_view(id).unwrap();
        let ack = s.sampler_trim_reset(id, &v.stamp, Instant::now()).unwrap();
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now(),
        )
        .unwrap();
        assert_eq!(s.project.channels[0].sampler, before);
        let v = s.sampler_trim_view(id).unwrap();
        s.trim_epoch = u64::MAX;
        let old = s.project.clone();
        let usage = s.history_usage();
        assert!(s.sampler_trim_reset(id, &v.stamp, Instant::now()).is_err());
        assert_eq!(s.project, old);
        assert_eq!(s.history_usage(), usage);
        s.trim_epoch = 1;
        s.decoded.remove(&id);
        assert!(s.sampler_trim_reset(id, &v.stamp, Instant::now()).is_err());
        assert_eq!(s.project, old);
    }
}
