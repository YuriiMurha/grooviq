//! Sampler: plays one sample per instance, polyphonically, pitched by key.
//!
//! Each note picks a region of the sample (`start`..`end`, optionally reversed), plays it at a
//! rate fixed at note-on (`source_rate / engine_rate * 2^(semitones / 12)`) through a cubic
//! Hermite read, and shapes it with an attack/decay/sustain/release envelope. In one-shot mode a
//! note plays to the end of its region and ignores note-off; in gate mode note-off releases it.
//! Declick adds a 2 ms fade-in and a 4 ms fade-out at the region edges.
//!
//! Determinism: every exp/pow goes through `libm`, pan uses `sqrt`, and there is no `mul_add`,
//! so a render is bit-identical on every platform. Real-time: `process` and `set_param` never
//! allocate; `set_sample` hands the previous buffer back so the caller frees it elsewhere.

use crate::api::{Device, NoteEvent};
use crate::util::clamp_param;
use std::sync::Arc;

/// Decoded audio, planar stereo (a mono file is copied to both channels).
#[derive(Clone, Debug, PartialEq)]
pub struct SampleBuf {
    /// The sample's own rate in Hz.
    pub rate: f32,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl SampleBuf {
    /// Frames (per channel).
    pub fn len(&self) -> usize {
        self.l.len()
    }

    pub fn is_empty(&self) -> bool {
        self.l.is_empty()
    }
}

/// Notes that can sound at once; the next one steals the oldest.
pub const VOICES: usize = 16;

/// Parameter ids, in `SPEC` (and `set_param`) order. Append-only.
pub const PARAM_IDS: [&str; 15] = [
    "root",
    "tune",
    "fine",
    "start",
    "end",
    "reverse",
    "pitch_track",
    "one_shot",
    "attack",
    "decay",
    "sustain",
    "release",
    "gain",
    "pan",
    "declick",
];

/// `(min, max, default)` per parameter, indexed like [`PARAM_IDS`]. Switches are on at >= 0.5.
pub const SPEC: [(f32, f32, f32); 15] = [
    (0.0, 127.0, 60.0),    // root: MIDI key that plays the sample at its own pitch
    (-24.0, 24.0, 0.0),    // tune: semitones
    (-100.0, 100.0, 0.0),  // fine: cents
    (0.0, 1.0, 0.0),       // start: region start, fraction of the sample
    (0.0, 1.0, 1.0),       // end: region end, fraction of the sample
    (0.0, 1.0, 0.0),       // reverse
    (0.0, 1.0, 1.0),       // pitch_track: the key changes the pitch
    (0.0, 1.0, 1.0),       // one_shot: play to the end, ignore note-off
    (0.0, 5000.0, 0.0),    // attack: ms
    (1.0, 10000.0, 500.0), // decay: ms
    (0.0, 1.0, 1.0),       // sustain: level
    (1.0, 10000.0, 40.0),  // release: ms
    (-60.0, 12.0, 0.0),    // gain: dB
    (-1.0, 1.0, 0.0),      // pan
    (0.0, 1.0, 1.0),       // declick
];

const ROOT: usize = 0;
const TUNE: usize = 1;
const FINE: usize = 2;
const START: usize = 3;
const END: usize = 4;
const REVERSE: usize = 5;
const PITCH_TRACK: usize = 6;
const ONE_SHOT: usize = 7;
const ATTACK: usize = 8;
const DECAY: usize = 9;
const SUSTAIN: usize = 10;
const RELEASE: usize = 11;
const GAIN: usize = 12;
const PAN: usize = 13;
const DECLICK: usize = 14;

/// Declick fade lengths in seconds.
const FADE_IN_S: f32 = 0.002;
const FADE_OUT_S: f32 = 0.004;
/// Output gain/pan smoothing time constant in seconds.
const GAIN_SMOOTH_S: f32 = 0.01;
/// A releasing voice stops below this level (-80 dB).
const SILENT: f32 = 1e-4;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    stage: Stage,
    key: u8,
    /// Read position in sample frames.
    pos: f64,
    /// Frames advanced per output frame (negative when reversed); fixed at note-on.
    step: f64,
    /// Region of the sample this voice plays, in frames (`start` inclusive, `end` exclusive).
    start: f64,
    end: f64,
    env: f32,
    vel: f32,
    /// Frames played so far (declick fade-in).
    age: u32,
    /// Note-on order, for stealing the oldest voice.
    order: u64,
    /// Gate mode: this note's note-off has not arrived yet. It stays set when the voice ends
    /// by itself first, so that late note-off finds this note, not a newer one on its key.
    held: bool,
}

impl Voice {
    const OFF: Self = Self {
        stage: Stage::Off,
        key: 0,
        pos: 0.0,
        step: 1.0,
        start: 0.0,
        end: 0.0,
        env: 0.0,
        vel: 0.0,
        age: 0,
        order: 0,
        held: false,
    };
}

/// Per-sample coefficients derived from the parameters (recomputed in `set_param`).
#[derive(Clone, Copy, Debug)]
struct Coefs {
    /// Envelope increment per frame during attack.
    attack: f32,
    /// One-pole multipliers per frame toward sustain and toward zero.
    decay: f32,
    release: f32,
    /// Output gain (gain x pan) per channel.
    out: [f32; 2],
}

/// The voice pool: separate from the buffer so `process` can borrow both at once.
struct Voices {
    v: [Voice; VOICES],
    /// Note-ons so far, for stealing the oldest voice.
    counter: u64,
}

impl Voices {
    fn note_on(&mut self, p: &[f32; 15], sr: f32, buf: &SampleBuf, key: u8, velocity: f32) {
        let len = buf.len() as f64;
        // A buffer without a usable rate would never advance: it stays silent.
        if len < 1.0 || !(buf.rate.is_finite() && buf.rate > 0.0) {
            return;
        }
        // The region, in either order of start and end, at least one frame long.
        let a = p[START] as f64 * len;
        let b = p[END] as f64 * len;
        let mut start = a.min(b);
        let mut end = a.max(b);
        if end - start < 1.0 {
            end = (start + 1.0).min(len);
            start = end - 1.0;
        }
        let track = if p[PITCH_TRACK] >= 0.5 {
            key as f64 - p[ROOT] as f64
        } else {
            0.0
        };
        let semis = track + p[TUNE] as f64 + p[FINE] as f64 / 100.0;
        let rate = buf.rate as f64 / sr as f64 * libm::pow(2.0, semis / 12.0);
        let reverse = p[REVERSE] >= 0.5;
        self.counter += 1;
        // A free voice; else the oldest silent one still owed a note-off; else the one
        // started first.
        let i = match self.v.iter().position(|v| v.stage == Stage::Off && !v.held) {
            Some(i) => i,
            None => {
                let silent = self.v.iter().any(|v| v.stage == Stage::Off);
                let mut oldest: Option<usize> = None;
                for (i, v) in self.v.iter().enumerate() {
                    let candidate = !silent || v.stage == Stage::Off;
                    if candidate && oldest.is_none_or(|o| v.order < self.v[o].order) {
                        oldest = Some(i);
                    }
                }
                oldest.unwrap_or(0)
            }
        };
        let attack = p[ATTACK] > 0.0;
        self.v[i] = Voice {
            stage: if attack { Stage::Attack } else { Stage::Decay },
            key,
            pos: if reverse { end - 1.0 } else { start },
            step: if reverse { -rate } else { rate },
            start,
            end,
            env: if attack { 0.0 } else { 1.0 },
            vel: if velocity.is_finite() {
                velocity.clamp(0.0, 1.0)
            } else {
                0.0
            },
            age: 0,
            order: self.counter,
            held: p[ONE_SHOT] < 0.5,
        };
    }

    /// Ends one note: the oldest on `key` whose note-off has not arrived, which releases it
    /// if it still sounds. A note-off belongs to one note-on, so a newer hit on the same key
    /// keeps sounding (a swung note that overlaps the next one, or a note-off booked before
    /// a tempo change), also when the older voice already ended by itself.
    fn note_off(&mut self, p: &[f32; 15], key: u8) {
        if p[ONE_SHOT] >= 0.5 {
            return;
        }
        let mut oldest: Option<usize> = None;
        for (i, v) in self.v.iter().enumerate() {
            if v.key == key && v.held && oldest.is_none_or(|o| v.order < self.v[o].order) {
                oldest = Some(i);
            }
        }
        if let Some(i) = oldest {
            let v = &mut self.v[i];
            v.held = false;
            if matches!(v.stage, Stage::Attack | Stage::Decay | Stage::Sustain) {
                v.stage = Stage::Release;
            }
        }
    }

    fn event(&mut self, p: &[f32; 15], sr: f32, buf: &SampleBuf, e: &NoteEvent) {
        if e.on {
            self.note_on(p, sr, buf, e.key, e.velocity);
        } else {
            self.note_off(p, e.key);
        }
    }

    fn silence(&mut self) {
        self.v = [Voice::OFF; VOICES];
    }

    /// Silences every voice but keeps its key, order and owed note-off, as a voice that ends
    /// by itself does.
    fn quiet(&mut self) {
        for v in &mut self.v {
            *v = Voice {
                key: v.key,
                order: v.order,
                held: v.held,
                ..Voice::OFF
            };
        }
    }

    fn idle(&self) -> bool {
        self.v.iter().all(|v| v.stage == Stage::Off)
    }
}

/// The sampler device.
pub struct Sampler {
    sr: f32,
    sample: Option<Arc<SampleBuf>>,
    p: [f32; 15],
    c: Coefs,
    voices: Voices,
    /// Smoothed output gain per channel, moving toward `c.out`.
    out: [f32; 2],
    smooth: f32,
    fade_in: u32,
    fade_out: f64,
}

impl Sampler {
    pub fn new(sample_rate: f32) -> Self {
        let sr = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };
        let p = SPEC.map(|(_, _, d)| d);
        let c = coefs(&p, sr);
        Self {
            sr,
            sample: None,
            p,
            c,
            voices: Voices {
                v: [Voice::OFF; VOICES],
                counter: 0,
            },
            out: c.out,
            smooth: 1.0 - libm::expf(-1.0 / (GAIN_SMOOTH_S * sr)),
            fade_in: ((FADE_IN_S * sr) as u32).max(1),
            fade_out: ((FADE_OUT_S * sr) as f64).max(1.0),
        }
    }

    /// Replaces the sample. Silences every voice and returns the previous buffer, so the caller
    /// decides where it is freed (never on the audio thread).
    pub fn set_sample(&mut self, sample: Option<Arc<SampleBuf>>) -> Option<Arc<SampleBuf>> {
        self.voices.silence();
        self.out = self.c.out;
        std::mem::replace(&mut self.sample, sample)
    }

    /// Comes to rest once nothing sounds: every voice is silent and the output smoothing sits
    /// at its target, as after [`Device::reset`]. Unlike a reset, a note that ended before its
    /// note-off still waits for it, so that late note-off finds it rather than a newer note on
    /// its key. Never allocates.
    pub fn settle(&mut self) {
        self.voices.quiet();
        self.out = self.c.out;
    }

    /// The loaded sample, if any.
    pub fn sample(&self) -> Option<&Arc<SampleBuf>> {
        self.sample.as_ref()
    }

    /// Voices currently sounding (including releasing ones).
    pub fn active_voices(&self) -> usize {
        self.voices
            .v
            .iter()
            .filter(|v| v.stage != Stage::Off)
            .count()
    }
}

/// Coefficients for the parameter values `p` at rate `sr`.
fn coefs(p: &[f32; 15], sr: f32) -> Coefs {
    // Decay and release fall by e every quarter of their time (-35 dB at the set time).
    let fall = |ms: f32| libm::expf(-1.0 / (ms.max(1.0) * 0.001 * sr * 0.25));
    let gain = libm::powf(10.0, p[GAIN] / 20.0);
    // sqrt(1 -/+ pan) = sqrt((1 -/+ pan) / 2) * SQRT_2, exact at the centre and the edges.
    let pan = p[PAN];
    Coefs {
        attack: 1.0 / (p[ATTACK].max(0.01) * 0.001 * sr),
        decay: fall(p[DECAY]),
        release: fall(p[RELEASE]),
        out: [gain * (1.0 - pan).sqrt(), gain * (1.0 + pan).sqrt()],
    }
}

/// Cubic Hermite read of one channel at `pos` (frames outside the sample read as zero). At an
/// integer position it returns the sample itself.
#[inline]
fn hermite(x: &[f32], pos: f64) -> f32 {
    let n = x.len() as i64;
    let i = pos.floor() as i64;
    let t = (pos - i as f64) as f32;
    let at = |k: i64| if k < 0 || k >= n { 0.0 } else { x[k as usize] };
    let (y0, y1, y2, y3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

impl Device for Sampler {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(mut v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        let i = index as usize;
        if i == ROOT {
            v = v.round();
        }
        self.p[i] = v;
        self.c = coefs(&self.p, self.sr);
        // Nothing sounds: there is nothing to smooth, so the new gain applies at once.
        if self.voices.idle() {
            self.out = self.c.out;
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sidechain: Option<(&[f32], &[f32])>,
        events: &[NoteEvent],
    ) {
        l.fill(0.0);
        r.fill(0.0);
        let n = l.len().min(r.len());
        let Self {
            sr,
            sample,
            p,
            c,
            voices,
            out,
            smooth,
            fade_in,
            fade_out,
        } = self;
        // Borrowed for the block, never cloned: no refcount traffic, nothing to drop here.
        let Some(buf) = sample.as_deref() else {
            return;
        };
        let declick = p[DECLICK] >= 0.5;
        let sustain = p[SUSTAIN];
        let mut ev = events.iter().peekable();
        for f in 0..n {
            while let Some(e) = ev.next_if(|e| e.offset as usize <= f) {
                voices.event(p, *sr, buf, e);
            }
            let (mut sl, mut sr_) = (0.0f32, 0.0f32);
            for v in voices.v.iter_mut() {
                match v.stage {
                    Stage::Off => continue,
                    Stage::Attack => {
                        v.env += c.attack;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        v.env = sustain + (v.env - sustain) * c.decay;
                        if (v.env - sustain).abs() < SILENT {
                            v.env = sustain;
                            v.stage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => v.env = sustain,
                    Stage::Release => {
                        v.env *= c.release;
                        if v.env < SILENT {
                            *v = Voice::OFF;
                            continue;
                        }
                    }
                }
                let mut amp = v.env * v.vel;
                if declick {
                    if v.age < *fade_in {
                        amp *= v.age as f32 / *fade_in as f32;
                    }
                    let to_edge = if v.step > 0.0 {
                        v.end - 1.0 - v.pos
                    } else {
                        v.pos - v.start
                    };
                    if to_edge < *fade_out {
                        amp *= (to_edge / *fade_out).max(0.0) as f32;
                    }
                }
                sl += hermite(&buf.l, v.pos) * amp;
                sr_ += hermite(&buf.r, v.pos) * amp;
                v.age = v.age.saturating_add(1);
                v.pos += v.step;
                if v.pos >= v.end || v.pos < v.start {
                    // Ended by itself: a note-off it is still owed must find it.
                    *v = Voice {
                        key: v.key,
                        order: v.order,
                        held: v.held,
                        ..Voice::OFF
                    };
                }
            }
            for ch in 0..2 {
                let d = c.out[ch] - out[ch];
                out[ch] = if d.abs() < 1e-6 {
                    c.out[ch]
                } else {
                    out[ch] + d * *smooth
                };
            }
            l[f] = sl * out[0];
            r[f] = sr_ * out[1];
        }
        // Events past the block's end still apply (at its end).
        for e in ev {
            voices.event(p, *sr, buf, e);
        }
    }

    fn reset(&mut self) {
        self.voices.silence();
        self.out = self.c.out;
    }
}
