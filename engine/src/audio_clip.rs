//! Fixed-second audio. Voices borrow the immutable bank by index; only SongData owns buffers.
use crate::{ControlError, MAX_SONG_TICKS, SampleBuf, SongLoop};
use std::sync::Arc;
pub const MAX_AUDIO_VOICES: usize = 256;
#[derive(Clone, Debug, PartialEq)]
pub struct AudioAsset {
    pub id: u32,
    pub source_rate: u32,
    pub source_frames: u64,
    pub decoded: Option<Arc<SampleBuf>>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioClip {
    pub id: u32,
    pub asset: u32,
    pub start_tick: u64,
    pub source_start: u64,
    pub source_end: u64,
    pub gain: f32,
    pub reverse: bool,
    pub fade_in: u64,
    pub fade_out: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDiagnostic {
    pub asset: u32,
    pub message: &'static str,
}
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    clip: AudioClip,
    asset: usize,
}
#[derive(Clone, Debug, Default, PartialEq)]
struct Bank {
    assets: Vec<AudioAsset>,
    entries: Vec<Entry>,
    ends: Vec<f64>,
    leaves: usize,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioSchedule(Arc<Bank>);
impl AudioSchedule {
    /// Whether clones retain the same immutable runtime bank.
    pub fn shares_bank(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Future onsets are excluded from a region export; existing sources retain fixed seconds.
    pub(crate) fn before_tick(&self, end: u64) -> Result<Self, ControlError> {
        Self::build(
            self.0.assets.clone(),
            self.0
                .entries
                .iter()
                .filter(|e| e.clip.start_tick < end)
                .map(|e| e.clip)
                .collect(),
        )
        .map(|(schedule, _)| schedule)
    }
    pub fn is_empty(&self) -> bool {
        self.0.entries.is_empty()
    }
    pub fn build(
        mut assets: Vec<AudioAsset>,
        mut clips: Vec<AudioClip>,
    ) -> Result<(Self, Vec<AudioDiagnostic>), ControlError> {
        if assets.len() > 4096 || clips.len() > 100000 {
            return Err(ControlError::Rejected);
        }
        assets.sort_by_key(|a| a.id);
        let mut warnings = Vec::new();
        let mut decoded_seen = std::collections::BTreeSet::new();
        let mut decoded_bytes = 0usize;
        for (i, a) in assets.iter().enumerate() {
            if i > 0 && assets[i - 1].id == a.id
                || !(8000..=384000).contains(&a.source_rate)
                || a.source_frames == 0
                || a.source_frames > u64::from(a.source_rate) * 600
            {
                return Err(ControlError::Rejected);
            }
            match &a.decoded {
                None => warnings.push(AudioDiagnostic {
                    asset: a.id,
                    message: "Audio unavailable; relink the original asset",
                }),
                Some(b) => {
                    if !b.rate.is_finite()
                        || !(8000.0..=384000.0).contains(&b.rate)
                        || b.l.len() != b.r.len()
                        || b.len() as f64 > f64::from(b.rate) * 600.0
                    {
                        return Err(ControlError::Rejected);
                    }
                    if decoded_seen.insert(Arc::as_ptr(b) as usize) {
                        decoded_bytes = decoded_bytes
                            .checked_add(b.len().checked_mul(8).ok_or(ControlError::Rejected)?)
                            .ok_or(ControlError::Rejected)?;
                        if decoded_bytes > 2 * 1024 * 1024 * 1024
                            || b.l.iter().chain(&b.r).any(|x| !x.is_finite())
                        {
                            return Err(ControlError::Rejected);
                        }
                    }
                    if b.rate != a.source_rate as f32 || b.len() as u64 != a.source_frames {
                        warnings.push(AudioDiagnostic{asset:a.id,message:"Decoded geometry differs; saved source time retained, unavailable frames silent"});
                    }
                }
            }
        }
        clips.sort_by_key(|c| c.id);
        if clips.windows(2).any(|w| w[0].id == w[1].id) {
            return Err(ControlError::Rejected);
        }
        clips.sort_by_key(|c| (c.start_tick, c.id));
        let mut bank = Bank {
            assets,
            entries: Vec::with_capacity(clips.len()),
            ends: Vec::new(),
            leaves: 0,
        };
        for c in clips {
            let a = bank
                .assets
                .binary_search_by_key(&c.asset, |a| a.id)
                .map_err(|_| ControlError::Rejected)?;
            let asset = &bank.assets[a];
            if c.start_tick > MAX_SONG_TICKS
                || c.source_start >= c.source_end
                || c.source_end > asset.source_frames
                || !c.gain.is_finite()
                || !(0.0..=2.0).contains(&c.gain)
                || c.fade_in > c.source_end - c.source_start
                || c.fade_out > c.source_end - c.source_start
            {
                return Err(ControlError::Rejected);
            }
            let seconds = (c.source_end - c.source_start) as f64 / f64::from(asset.source_rate);
            // The bank has no tempo: refuse only placements impossible even at 20 BPM.
            // The project compiler admits its actual current-tempo horizon separately.
            let minimum_ticks = (seconds * 20.0 * 16.0).ceil() as u64;
            if c.start_tick
                .checked_add(minimum_ticks)
                .is_none_or(|end| end > MAX_SONG_TICKS)
            {
                return Err(ControlError::Rejected);
            }

            bank.entries.push(Entry { clip: c, asset: a });
        }
        bank.leaves = bank.entries.len().max(1).next_power_of_two();
        bank.ends = vec![0.0; bank.leaves * 2];
        for (i, e) in bank.entries.iter().enumerate() {
            bank.ends[bank.leaves + i] = e.clip.start_tick as f64
                + (e.clip.source_end - e.clip.source_start) as f64
                    / f64::from(bank.assets[e.asset].source_rate)
                    * 6400.0
                + 0.8;
        }
        for i in (1..bank.leaves).rev() {
            bank.ends[i] = bank.ends[i * 2].max(bank.ends[i * 2 + 1]);
        }
        let schedule = Self(Arc::new(bank));
        schedule.validate_loop(None)?;
        Ok((schedule, warnings))
    }
    /// Conservative circular overlap bound: one extra instance per clip plus complete loop periods.
    /// Includes carry from before a loop change. Refuse rather than steal any voice.
    pub fn validate_loop(&self, region: Option<SongLoop>) -> Result<(), ControlError> {
        if region.is_some_and(|r| r.start_tick >= r.end_tick || r.end_tick > MAX_SONG_TICKS) {
            return Err(ControlError::Rejected);
        }
        let mut events = Vec::with_capacity(self.0.entries.len() * 2);
        let mut carry = 0usize;
        for e in &self.0.entries {
            let c = e.clip;
            let duration = (c.source_end - c.source_start) as f64
                / f64::from(self.0.assets[e.asset].source_rate);
            events.push((c.start_tick as f64, 1i32));
            // One frame at the lowest admitted rate covers onset/end quantization.
            events.push((c.start_tick as f64 + (duration + 1.0 / 8000.0) * 6400.0, -1));
            if let Some(r) = region
                && c.start_tick < r.end_tick
                && c.start_tick as f64 + (duration + 1.0 / 8000.0) * 6400.0 > r.start_tick as f64
            {
                carry = carry.saturating_add(
                    ((duration + 1.0 / 8000.0) * 6400.0 / (r.end_tick - r.start_tick) as f64).ceil()
                        as usize
                        + 1,
                );
            }
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut active = 0i32;
        let mut peak = 0usize;
        for (_, d) in events {
            active += d;
            peak = peak.max(active as usize);
        }
        if peak.saturating_add(carry) > MAX_AUDIO_VOICES {
            Err(ControlError::AudioVoiceLimit)
        } else {
            Ok(())
        }
    }
    pub(crate) fn valid_horizon(&self, end_tick: u64) -> bool {
        self.0
            .entries
            .last()
            .is_none_or(|e| e.clip.start_tick < end_tick)
    }
    pub fn end_frame(&self, bpm: f32, rate: f32) -> u64 {
        self.0
            .entries
            .iter()
            .map(|e| {
                let c = e.clip;
                crate::ticks_to_frames(c.start_tick as f64, bpm, rate)
                    + ((c.source_end - c.source_start) as f64 * f64::from(rate)
                        / f64::from(self.0.assets[e.asset].source_rate))
                    .ceil() as u64
            })
            .max()
            .unwrap_or(0)
    }
}
#[derive(Clone, Copy, Default)]
struct Voice {
    entry: usize,
    seconds: f64,
    frames: u64,
    rate: f32,
    active: bool,
    serial: u64,
}
#[derive(Clone)]
pub(crate) struct AudioRuntime {
    voices: [Voice; MAX_AUDIO_VOICES],
    next: usize,
    cycle: u64,
    seek: bool,
    serial: u64,
}
impl Default for AudioRuntime {
    fn default() -> Self {
        Self {
            voices: [Voice::default(); MAX_AUDIO_VOICES],
            next: 0,
            cycle: 0,
            seek: true,
            serial: 0,
        }
    }
}
impl AudioRuntime {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn active(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }
    pub(crate) fn compatible(a: &AudioSchedule, b: &AudioSchedule) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }
    pub(crate) fn render(
        &mut self,
        s: &AudioSchedule,
        region: Option<SongLoop>,
        clock: &crate::transport::TransportClock,
        f0: u64,
        l: &mut [f32; crate::BLOCK],
        r: &mut [f32; crate::BLOCK],
    ) {
        if !clock.playing() {
            return;
        }
        let entries = &s.0.entries;
        if entries.is_empty() {
            return;
        }
        if self.seek {
            self.seek = false;
            let tick = clock.tick_at(f0);
            self.cycle = region.map_or(0, |r| {
                if tick >= r.end_tick as f64 {
                    (tick as u64 - r.start_tick) / (r.end_tick - r.start_tick)
                } else {
                    0
                }
            });
            let shift = region.map_or(0, |r| self.cycle * (r.end_tick - r.start_tick));
            let within = tick - shift as f64;
            self.next = entries.partition_point(|e| (e.clip.start_tick as f64) < within);
            self.resume(s, within, clock.bpm());
        }
        for offset in 0..crate::BLOCK {
            let mut started = offset == 0;
            let frame = f0 + offset as u64;
            // Retire half-open intervals before admitting same-frame onsets.
            for v in &mut self.voices {
                if v.active {
                    let e = &entries[v.entry];
                    let a = &s.0.assets[e.asset];
                    let elapsed = v.seconds
                        + if v.rate > 0.0 {
                            v.frames as f64 / f64::from(v.rate)
                        } else {
                            0.0
                        };
                    if elapsed * f64::from(a.source_rate)
                        >= (e.clip.source_end - e.clip.source_start) as f64
                    {
                        v.active = false;
                    }
                }
            }
            if let Some(region) = region {
                while clock.frame_of(
                    (region.end_tick + self.cycle * (region.end_tick - region.start_tick)) as f64,
                ) <= frame
                {
                    self.cycle += 1;
                    self.next = entries.partition_point(|e| e.clip.start_tick < region.start_tick);
                    self.resume(s, region.start_tick as f64, clock.bpm());
                    started = true;
                }
            }
            loop {
                let hi = region.map_or(entries.len(), |r| {
                    entries.partition_point(|e| e.clip.start_tick < r.end_tick)
                });
                if self.next >= hi {
                    break;
                }
                let shift = region.map_or(0, |r| self.cycle * (r.end_tick - r.start_tick));
                if clock.frame_of((entries[self.next].clip.start_tick + shift) as f64) > frame {
                    break;
                }
                self.start(self.next, 0.0);
                self.next += 1;
                started = true;
            }
            if started {
                self.voices.sort_unstable_by_key(|v| {
                    (
                        !v.active,
                        if v.active {
                            entries[v.entry].clip.id
                        } else {
                            0
                        },
                        v.serial,
                    )
                });
            }
            // Stable clip-id summation followed by repeated-instance creation order.
            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                let e = &entries[v.entry];
                let c = e.clip;
                let a = &s.0.assets[e.asset];
                if v.rate != clock.sample_rate() {
                    if v.rate > 0.0 {
                        v.seconds += v.frames as f64 / f64::from(v.rate);
                    }
                    v.frames = 0;
                    v.rate = clock.sample_rate();
                }
                let progress =
                    (v.seconds + v.frames as f64 / f64::from(v.rate)) * f64::from(a.source_rate);
                let length = (c.source_end - c.source_start) as f64;
                if progress >= length {
                    v.active = false;
                    continue;
                }
                let source = if c.reverse {
                    c.source_end as f64 - 1.0 - progress
                } else {
                    c.source_start as f64 + progress
                };
                let fade_in = if c.fade_in == 0 {
                    1.0
                } else {
                    (progress / c.fade_in as f64).min(1.0)
                };
                let fade_out = if c.fade_out == 0 {
                    1.0
                } else {
                    ((length - progress) / c.fade_out as f64).min(1.0)
                };
                let gain = c.gain * (fade_in * fade_out) as f32;
                if let Some(b) = &a.decoded {
                    let pos = source * f64::from(b.rate) / f64::from(a.source_rate);
                    let lo = c.source_start as f64 * f64::from(b.rate) / f64::from(a.source_rate);
                    let hi = c.source_end as f64 * f64::from(b.rate) / f64::from(a.source_rate);
                    l[offset] += read(&b.l, pos, lo, hi) * gain;
                    r[offset] += read(&b.r, pos, lo, hi) * gain;
                }
                v.frames += 1;
            }
        }
    }
    fn resume(&mut self, s: &AudioSchedule, tick: f64, bpm: f32) {
        fn visit(
            runtime: &mut AudioRuntime,
            bank: &Bank,
            node: usize,
            range: (usize, usize),
            tick: f64,
            bpm: f32,
        ) {
            let (lo, hi) = range;
            if lo >= bank.entries.len()
                || bank.ends[node] <= tick
                || bank.entries[lo].clip.start_tick as f64 >= tick
            {
                return;
            }
            if hi - lo == 1 {
                let e = &bank.entries[lo];
                let a = &bank.assets[e.asset];
                let seconds = (tick - e.clip.start_tick as f64) * 60.0 / (f64::from(bpm) * 960.0);
                if seconds
                    < (e.clip.source_end - e.clip.source_start) as f64 / f64::from(a.source_rate)
                {
                    runtime.start(lo, seconds);
                }
            } else {
                let mid = (lo + hi) / 2;
                visit(runtime, bank, node * 2, (lo, mid), tick, bpm);
                visit(runtime, bank, node * 2 + 1, (mid, hi), tick, bpm);
            }
        }
        if !s.is_empty() {
            visit(self, &s.0, 1, (0, s.0.leaves), tick, bpm);
        }
    }
    fn start(&mut self, entry: usize, seconds: f64) {
        if let Some(v) = self.voices.iter_mut().find(|v| !v.active) {
            *v = Voice {
                entry,
                seconds,
                frames: 0,
                rate: 0.0,
                active: true,
                serial: self.serial,
            };
            self.serial = self.serial.wrapping_add(1);
        }
    }
}
fn read(data: &[f32], pos: f64, lo: f64, hi: f64) -> f32 {
    if pos <= -1.0 || pos >= data.len() as f64 {
        return 0.0;
    }
    let i = libm::floor(pos) as i64;
    let sample = |i: i64| {
        if (i as f64) >= lo && (i as f64) < hi {
            usize::try_from(i)
                .ok()
                .and_then(|i| data.get(i))
                .copied()
                .unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let a = sample(i);
    let b = sample(i + 1);
    a + (b - a) * (pos - i as f64) as f32
}
