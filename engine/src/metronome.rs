//! The metronome: a short sine burst on every beat while the transport plays, higher on the
//! first beat of a bar. The processor owns it (like the transport), so it outlives graph swaps,
//! and mixes it into the master after the graph. Off by default; an export never switches it
//! on.
//!
//! Beats are counted in pattern ticks from tick 0 of play, and each lands on the frame the
//! transport gives its tick (as a note would), so the clicks never depend on how blocks are
//! split.

use crate::BLOCK;
use crate::graph::Ramp;
use crate::transport::TransportClock;
use grooviq_dsp::db_to_gain;
use std::f64::consts::TAU;

/// Bar-start and other-beat pitches, in Hz.
const ACCENT_HZ: f64 = 1_500.0;
const BEAT_HZ: f64 = 1_000.0;
/// Peak level, in dBFS.
const PEAK_DB: f32 = -12.0;
/// Linear attack, then an exponential decay with this time constant.
const ATTACK_SECONDS: f32 = 0.001;
const DECAY_SECONDS: f32 = 0.030;
/// The burst stops once its envelope falls below this (-80 dB under the peak).
const END_LEVEL: f32 = 1e-4;
/// Ticks in a whole note.
const WHOLE_TICKS: u64 = 3_840;

pub(crate) struct Metronome {
    on: bool,
    beats: u8,
    unit: u8,
    gain: f32,
    sample_rate: f64,
    attack_len: u32,
    decay: f32,
    /// The burst sounding, if any: frames since its onset, its envelope, phase (cycles) and
    /// cycles per frame.
    sounding: bool,
    age: u32,
    env: f32,
    phase: f64,
    step: f64,
    /// Fading the burst out over 1 ms (the transport stopped or the metronome went off).
    release: Option<Ramp>,
}

impl Metronome {
    pub(crate) fn new(sample_rate: f32, time_signature: (u8, u8)) -> Self {
        let mut m = Self {
            on: false,
            beats: time_signature.0,
            unit: time_signature.1,
            gain: db_to_gain(PEAK_DB),
            sample_rate: 0.0,
            attack_len: 1,
            decay: 0.0,
            sounding: false,
            age: 0,
            env: 0.0,
            phase: 0.0,
            step: 0.0,
            release: None,
        };
        m.set_sample_rate(sample_rate);
        m
    }

    pub(crate) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = f64::from(sample_rate);
        self.attack_len = (ATTACK_SECONDS * sample_rate).round().max(1.0) as u32;
        self.decay = libm::expf(-1.0 / (DECAY_SECONDS * sample_rate));
        // A burst in flight ends rather than change pitch.
        self.sounding = false;
        self.release = None;
    }

    /// Switches the clicks; switching off fades a burst that is sounding over 1 ms.
    pub(crate) fn set_on(&mut self, on: bool) {
        self.on = on;
        if !on {
            self.release();
        }
    }

    /// The transport stopped, or the metronome went off: a burst sounding fades out over 1 ms
    /// (a cut would click).
    pub(crate) fn release(&mut self) {
        if self.sounding && self.release.is_none() {
            let mut ramp = Ramp::at(1.0);
            ramp.start(0.0, self.attack_len);
            self.release = Some(ramp);
        }
    }

    /// Takes a checked time signature (see [`crate::state::time_signature_ok`]).
    pub(crate) fn set_time_signature(&mut self, beats: u8, unit: u8) {
        self.beats = beats;
        self.unit = unit;
    }

    /// Adds the clicks of the block that starts at frame `start` to `l`/`r`. Real-time safe.
    pub(crate) fn render(
        &mut self,
        l: &mut [f32; BLOCK],
        r: &mut [f32; BLOCK],
        transport: &TransportClock,
        start: u64,
    ) {
        let clicking = self.on && transport.playing();
        if !clicking && !self.sounding {
            return;
        }
        let beat_ticks = WHOLE_TICKS / u64::from(self.unit);
        let end = start + BLOCK as u64;
        // The first beat that may land in the block: a beat is at least 300 frames (240 ticks
        // at 400 BPM and 8 kHz), so the one before it landed before `start`.
        let mut beat = (transport.tick_at(start).max(0.0) as u64) / beat_ticks;
        let mut next = if clicking {
            self.onset(transport, &mut beat, beat_ticks, start, end)
        } else {
            None
        };
        for (i, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            if let Some((frame, accent)) = next
                && frame == start + i as u64
            {
                self.trigger(accent);
                beat += 1;
                next = self.onset(transport, &mut beat, beat_ticks, start, end);
            }
            if !self.sounding {
                continue;
            }
            let mut s = libm::sin(self.phase * TAU) as f32 * self.env * self.gain;
            if let Some(release) = &mut self.release {
                s *= release.tick();
                if release.done() {
                    self.sounding = false;
                    self.release = None;
                }
            }
            *a += s;
            *b += s;
            self.advance();
        }
    }

    /// The next beat from `beat` on that lands in `start..end`, as `(frame, bar start)`;
    /// `beat` moves to it.
    fn onset(
        &self,
        transport: &TransportClock,
        beat: &mut u64,
        beat_ticks: u64,
        start: u64,
        end: u64,
    ) -> Option<(u64, bool)> {
        // Bounded: at most two beats are skipped (one before the block, one rounding).
        for _ in 0..4 {
            let frame = transport.frame_of((*beat * beat_ticks) as f64);
            if frame >= end {
                return None;
            }
            if frame >= start {
                return Some((frame, beat.is_multiple_of(u64::from(self.beats))));
            }
            *beat += 1;
        }
        None
    }

    /// Starts a burst at phase 0 from silence (one already sounding is cut).
    fn trigger(&mut self, accent: bool) {
        let hz = if accent { ACCENT_HZ } else { BEAT_HZ };
        self.sounding = true;
        self.age = 0;
        self.env = 0.0;
        self.phase = 0.0;
        self.step = hz / self.sample_rate;
        self.release = None;
    }

    /// Moves the burst on by one frame.
    fn advance(&mut self) {
        self.phase = (self.phase + self.step).fract();
        self.age = self.age.saturating_add(1);
        if self.age <= self.attack_len {
            self.env = self.age as f32 / self.attack_len as f32;
        } else {
            self.env *= self.decay;
            if self.env < END_LEVEL {
                self.sounding = false;
            }
        }
    }
}
