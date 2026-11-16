//! Sampler: plays one sample per instance, polyphonically, pitched by key.
//!
//! Each note picks a region of the sample (`start`..`end`, optionally reversed), plays it at a
//! rate fixed at note-on (`source_rate / engine_rate * 2^(semitones / 12)`) through a cubic
//! Hermite read, and shapes it with an attack/decay/sustain/release envelope. In one-shot mode a
//! note plays to the end of its region and ignores note-off; in gate mode note-off releases it.
//! Loop mode repeats the region's tail (from `loop_start`, a fraction of the played region in
//! play order) until note-off, then releases, also in one-shot mode. Each voice runs a 12 dB/oct
//! state-variable filter (low-, high- or band-pass) whose cutoff follows its own envelope and
//! the note's velocity. Declick adds a 2 ms fade-in and a 4 ms fade-out at the region edges,
//! both counted in output frames.
//!
//! Determinism: every exp/pow/tan goes through `libm`, pan uses `sqrt`, and there is no
//! `mul_add`, so a render is bit-identical on every platform. Filter coefficients are
//! recomputed every 16 frames counted from each note's start, so the output never depends on
//! how the blocks are split. Real-time: `process` and `set_param` never allocate; `set_sample`
//! hands the previous buffer back so the caller frees it elsewhere.

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
pub const PARAM_IDS: [&str; PARAMS] = [
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
    "loop",
    "loop_start",
    "filter_type",
    "cutoff",
    "res",
    "filter_env",
    "flt_attack",
    "flt_decay",
    "flt_sustain",
    "flt_release",
    "vel_amp",
    "vel_filter",
];

/// Number of parameters.
const PARAMS: usize = 27;

/// `(min, max, default)` per parameter, indexed like [`PARAM_IDS`]. Switches are on at >= 0.5.
pub const SPEC: [(f32, f32, f32); PARAMS] = [
    (0.0, 127.0, 60.0),       // root: MIDI key that plays the sample at its own pitch
    (-24.0, 24.0, 0.0),       // tune: semitones
    (-100.0, 100.0, 0.0),     // fine: cents
    (0.0, 1.0, 0.0),          // start: region start, fraction of the sample
    (0.0, 1.0, 1.0),          // end: region end, fraction of the sample
    (0.0, 1.0, 0.0),          // reverse
    (0.0, 1.0, 1.0),          // pitch_track: the key changes the pitch
    (0.0, 1.0, 1.0),          // one_shot: play to the end, ignore note-off
    (0.0, 5000.0, 0.0),       // attack: ms
    (1.0, 10000.0, 500.0),    // decay: ms
    (0.0, 1.0, 1.0),          // sustain: level
    (1.0, 10000.0, 40.0),     // release: ms
    (-60.0, 12.0, 0.0),       // gain: dB
    (-1.0, 1.0, 0.0),         // pan
    (0.0, 1.0, 1.0),          // declick
    (0.0, 1.0, 0.0),          // loop: repeat the region's tail until note-off (overrides one-shot)
    (0.0, 1.0, 0.0),          // loop_start: where the loop begins, fraction of the played region
    (0.0, 3.0, 0.0),          // filter_type: 0 off, 1 low-pass, 2 high-pass, 3 band-pass
    (20.0, 20000.0, 20000.0), // cutoff: Hz
    (0.0, 1.0, 0.0),          // res: resonance
    (-1.0, 1.0, 0.0),         // filter_env: cutoff x 2^(6 x filter_env x envelope)
    (0.0, 5000.0, 0.0),       // flt_attack: ms
    (1.0, 10000.0, 300.0),    // flt_decay: ms
    (0.0, 1.0, 0.0),          // flt_sustain: level
    (1.0, 10000.0, 40.0),     // flt_release: ms
    (0.0, 1.0, 1.0),          // vel_amp: level x (1 - vel_amp + vel_amp x velocity)
    (0.0, 1.0, 0.0),          // vel_filter: cutoff x 2^(4 x vel_filter x (velocity - 1))
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
const LOOP: usize = 15;
const LOOP_START: usize = 16;
const FILTER_TYPE: usize = 17;
const CUTOFF: usize = 18;
const RES: usize = 19;
const FILTER_ENV: usize = 20;
const FLT_ATTACK: usize = 21;
const FLT_DECAY: usize = 22;
const FLT_SUSTAIN: usize = 23;
const FLT_RELEASE: usize = 24;
const VEL_AMP: usize = 25;
const VEL_FILTER: usize = 26;

/// Declick fade lengths in seconds.
const FADE_IN_S: f32 = 0.002;
const FADE_OUT_S: f32 = 0.004;
/// Output gain/pan smoothing time constant in seconds.
const GAIN_SMOOTH_S: f32 = 0.01;
/// A releasing voice stops below this level (-80 dB).
const SILENT: f32 = 1e-4;
/// A voice that has decayed to a sustain below this level (-100 dB) frees its slot.
const FREE: f32 = 1e-5;
/// Sustain level and cutoff changes ramp over this time in seconds.
const RAMP_S: f32 = 0.005;
/// Filter coefficients are recomputed every this many frames of a voice.
const CONTROL_FRAMES: u8 = 16;
/// The filter's lowest cutoff in Hz, and its highest as a fraction of the engine rate.
const MIN_CUTOFF: f32 = 20.0;
const MAX_CUTOFF_RATIO: f32 = 0.49;
/// Octaves the filter envelope (at full level) and velocity (from 1 down to 0) move the cutoff.
const ENV_OCTAVES: f32 = 6.0;
const VEL_OCTAVES: f32 = 4.0;
/// Filter damping (1 / Q) at no resonance (Butterworth) and at full resonance (Q 20).
const DAMP_MAX: f32 = std::f32::consts::SQRT_2;
const DAMP_MIN: f32 = 0.05;

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
    /// Gate or loop mode: this note's note-off has not arrived yet. It stays set when the voice
    /// ends by itself first, so that late note-off finds this note, not a newer one on its key.
    held: bool,
    /// Loop mode: the voice repeats `lo..hi` (`len` frames, a whole number) instead of ending.
    looping: bool,
    lo: f64,
    hi: f64,
    len: f64,
    /// The voice has wrapped around its loop at least once.
    wrapped: bool,
    /// Filter envelope and its stage.
    fstage: Stage,
    fenv: f32,
    /// Cutoff offset in octaves from the note's velocity.
    vel_oct: f32,
    /// Frames until the filter coefficients are recomputed (0: this frame).
    ctl: u8,
    /// Filter coefficients (TPT state-variable: `a1 a2 a3`, damping `k`) and per-channel state.
    a: [f32; 3],
    k: f32,
    ic: [[f32; 2]; 2],
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
        looping: false,
        lo: 0.0,
        hi: 0.0,
        len: 1.0,
        wrapped: false,
        fstage: Stage::Off,
        fenv: 0.0,
        vel_oct: 0.0,
        ctl: 0,
        a: [0.0; 3],
        k: DAMP_MAX,
        ic: [[0.0; 2]; 2],
    };

    /// This voice, silent, but keeping its key, order and owed note-off (as a voice that ends
    /// by itself does), so a late note-off still finds it.
    fn quieted(&self) -> Self {
        Self {
            key: self.key,
            order: self.order,
            held: self.held,
            ..Self::OFF
        }
    }

    /// Recomputes the filter coefficients for `log2_cutoff` (Hz), this voice's envelope and
    /// velocity.
    fn filter_coefs(&mut self, c: &Coefs, log2_cutoff: f32, env_amount: f32, sr: f32) {
        let octaves = log2_cutoff + ENV_OCTAVES * env_amount * self.fenv + self.vel_oct;
        // min then max, not clamp: never panics, whatever the rate.
        let fc = libm::exp2f(octaves)
            .min(MAX_CUTOFF_RATIO * sr)
            .max(MIN_CUTOFF);
        let g = libm::tanf(std::f32::consts::PI * fc / sr);
        let a1 = 1.0 / (1.0 + g * (g + c.damp));
        let a2 = g * a1;
        self.a = [a1, a2, g * a2];
        self.k = c.damp;
    }

    /// One frame of the state-variable filter on channel `ch`; `kind` 1 low-, 2 high-,
    /// 3 band-pass (unity gain at the peak).
    #[inline]
    fn filter(&mut self, ch: usize, kind: u8, v0: f32) -> f32 {
        let [a1, a2, a3] = self.a;
        let [ic1, ic2] = &mut self.ic[ch];
        let v3 = v0 - *ic2;
        let v1 = a1 * *ic1 + a2 * v3;
        let v2 = *ic2 + a2 * *ic1 + a3 * v3;
        *ic1 = 2.0 * v1 - *ic1;
        *ic2 = 2.0 * v2 - *ic2;
        match kind {
            1 => v2,
            2 => v0 - self.k * v1 - v2,
            _ => self.k * v1,
        }
    }

    /// Maps a read index into the loop once the voice has reached its far end (and, after
    /// the first wrap, its near end too), so every read across the seam sees the loop as one
    /// continuous signal.
    #[inline]
    fn loop_index(&self, k: i64) -> i64 {
        let kf = k as f64;
        let wrap = if self.step > 0.0 {
            kf >= self.hi || (self.wrapped && kf < self.lo)
        } else {
            kf < self.lo || (self.wrapped && kf >= self.hi)
        };
        if wrap {
            k - (self.len * ((kf - self.lo) / self.len).floor()) as i64
        } else {
            k
        }
    }
}

/// A value that moves linearly to a new target over a fixed number of frames.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Ramp {
    fn at(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            left: 0,
        }
    }

    /// Starts moving to `target`, arriving in `frames` frames.
    fn to(&mut self, target: f32, frames: u32) {
        self.target = target;
        if frames == 0 || target == self.value {
            self.snap();
        } else {
            self.step = (target - self.value) / frames as f32;
            self.left = frames;
        }
    }

    /// Jumps to the target.
    fn snap(&mut self) {
        self.value = self.target;
        self.left = 0;
    }

    /// The value for the next frame (exactly the target once the ramp ends).
    #[inline]
    fn next(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }
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
    /// The filter envelope's attack increment and decay/release multipliers per frame.
    fattack: f32,
    fdecay: f32,
    frelease: f32,
    /// Filter damping (1 / Q) from the resonance.
    damp: f32,
}

/// The voice pool: separate from the buffer so `process` can borrow both at once.
struct Voices {
    v: [Voice; VOICES],
    /// Note-ons so far, for stealing the oldest voice.
    counter: u64,
}

impl Voices {
    fn note_on(&mut self, p: &[f32; PARAMS], sr: f32, buf: &SampleBuf, key: u8, velocity: f32) {
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
        // The loop: the played region's tail from `loop_start` (in play order), a whole number
        // of frames long so reads across the seam line up with the samples.
        let looping = p[LOOP] >= 0.5;
        let region = end - start;
        let len_loop = (region * (1.0 - p[LOOP_START] as f64))
            .round()
            .clamp(1.0, region.floor().max(1.0));
        let (lo, hi) = if reverse {
            (start, start + len_loop)
        } else {
            (end - len_loop, end)
        };
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
        let flt_attack = p[FLT_ATTACK] > 0.0;
        let vel = if velocity.is_finite() {
            velocity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let vel_amp = p[VEL_AMP];
        self.v[i] = Voice {
            stage: if attack { Stage::Attack } else { Stage::Decay },
            key,
            pos: if reverse { end - 1.0 } else { start },
            step: if reverse { -rate } else { rate },
            start,
            end,
            env: if attack { 0.0 } else { 1.0 },
            vel: 1.0 - vel_amp + vel_amp * vel,
            age: 0,
            order: self.counter,
            held: p[ONE_SHOT] < 0.5 || looping,
            looping,
            lo,
            hi,
            len: len_loop,
            wrapped: false,
            fstage: if flt_attack {
                Stage::Attack
            } else {
                Stage::Decay
            },
            fenv: if flt_attack { 0.0 } else { 1.0 },
            vel_oct: VEL_OCTAVES * p[VEL_FILTER] * (vel - 1.0),
            ..Voice::OFF
        };
    }

    /// Ends one note: the oldest on `key` whose note-off has not arrived, which releases it
    /// if it still sounds. A note-off belongs to one note-on, so a newer hit on the same key
    /// keeps sounding (a swung note that overlaps the next one, or a note-off booked before
    /// a tempo change), also when the older voice already ended by itself. In one-shot mode the
    /// note plays on, but the note-off is still its own: a gate note that is owed it when
    /// one-shot is switched on takes it, so it never ends a later note. A looping note releases
    /// on its note-off in either mode.
    fn note_off(&mut self, p: &[f32; PARAMS], key: u8) {
        let mut oldest: Option<usize> = None;
        for (i, v) in self.v.iter().enumerate() {
            if v.key == key && v.held && oldest.is_none_or(|o| v.order < self.v[o].order) {
                oldest = Some(i);
            }
        }
        if let Some(i) = oldest {
            let v = &mut self.v[i];
            v.held = false;
            let gate = p[ONE_SHOT] < 0.5 || v.looping;
            if gate && matches!(v.stage, Stage::Attack | Stage::Decay | Stage::Sustain) {
                v.stage = Stage::Release;
                if v.fstage != Stage::Off {
                    v.fstage = Stage::Release;
                }
            }
        }
    }

    fn event(&mut self, p: &[f32; PARAMS], sr: f32, buf: &SampleBuf, e: &NoteEvent) {
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
            *v = v.quieted();
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
    p: [f32; PARAMS],
    c: Coefs,
    voices: Voices,
    /// Smoothed output gain per channel, moving toward `c.out`.
    out: [f32; 2],
    /// The sustain level, ramping to `p[SUSTAIN]`.
    sustain: Ramp,
    /// log2 of the cutoff in Hz, ramping to that of `p[CUTOFF]`.
    cutoff: Ramp,
    /// Frames a sustain or cutoff change takes.
    ramp: u32,
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
            sustain: Ramp::at(p[SUSTAIN]),
            cutoff: Ramp::at(libm::log2f(p[CUTOFF])),
            ramp: ((RAMP_S * sr) as u32).max(1),
            smooth: 1.0 - libm::expf(-1.0 / (GAIN_SMOOTH_S * sr)),
            fade_in: ((FADE_IN_S * sr) as u32).max(1),
            fade_out: ((FADE_OUT_S * sr) as f64).max(1.0),
        }
    }

    /// Replaces the sample. Silences every voice and returns the previous buffer, so the caller
    /// decides where it is freed (never on the audio thread).
    pub fn set_sample(&mut self, sample: Option<Arc<SampleBuf>>) -> Option<Arc<SampleBuf>> {
        self.voices.silence();
        self.rest();
        std::mem::replace(&mut self.sample, sample)
    }

    /// Comes to rest once nothing sounds: every voice is silent and the output smoothing sits
    /// at its target, as after [`Device::reset`]. Unlike a reset, a note that ended before its
    /// note-off still waits for it, so that late note-off finds it rather than a newer note on
    /// its key. Never allocates.
    pub fn settle(&mut self) {
        self.voices.quiet();
        self.rest();
    }

    /// Puts every smoothed value at its target (nothing sounds, so nothing needs smoothing).
    fn rest(&mut self) {
        self.out = self.c.out;
        self.sustain.snap();
        self.cutoff.snap();
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
fn coefs(p: &[f32; PARAMS], sr: f32) -> Coefs {
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
        fattack: 1.0 / (p[FLT_ATTACK].max(0.01) * 0.001 * sr),
        fdecay: fall(p[FLT_DECAY]),
        frelease: fall(p[FLT_RELEASE]),
        damp: DAMP_MAX - (DAMP_MAX - DAMP_MIN) * p[RES],
    }
}

/// Cubic Hermite read of one channel at `pos`, sample index `k` read from `index(k)` (frames
/// outside the sample read as zero). At an integer position it returns that sample itself.
#[inline]
fn hermite(x: &[f32], pos: f64, index: impl Fn(i64) -> i64) -> f32 {
    let n = x.len() as i64;
    let i = pos.floor() as i64;
    let t = (pos - i as f64) as f32;
    let at = |k: i64| {
        let k = index(k);
        if k < 0 || k >= n { 0.0 } else { x[k as usize] }
    };
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
        if i == ROOT || i == FILTER_TYPE {
            v = v.round();
        }
        self.p[i] = v;
        self.c = coefs(&self.p, self.sr);
        match i {
            SUSTAIN => self.sustain.to(v, self.ramp),
            CUTOFF => self.cutoff.to(libm::log2f(v), self.ramp),
            _ => {}
        }
        // Nothing sounds: there is nothing to smooth, so the new values apply at once.
        if self.voices.idle() {
            self.rest();
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
            sustain,
            cutoff,
            ramp: _,
            smooth,
            fade_in,
            fade_out,
        } = self;
        // Borrowed for the block, never cloned: no refcount traffic, nothing to drop here.
        let Some(buf) = sample.as_deref() else {
            return;
        };
        let declick = p[DECLICK] >= 0.5;
        let kind = p[FILTER_TYPE] as u8;
        let (env_amount, flt_sustain) = (p[FILTER_ENV], p[FLT_SUSTAIN]);
        let mut ev = events.iter().peekable();
        for f in 0..n {
            while let Some(e) = ev.next_if(|e| e.offset as usize <= f) {
                voices.event(p, *sr, buf, e);
            }
            let sustain = sustain.next();
            let log2_cutoff = cutoff.next();
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
                    Stage::Sustain => {
                        v.env = sustain;
                        // Decayed to silence: the slot is free (its note-off still finds it).
                        if v.env < FREE {
                            *v = v.quieted();
                            continue;
                        }
                    }
                    Stage::Release => {
                        v.env *= c.release;
                        if v.env < SILENT {
                            *v = Voice::OFF;
                            continue;
                        }
                    }
                }
                match v.fstage {
                    Stage::Off => {}
                    Stage::Attack => {
                        v.fenv += c.fattack;
                        if v.fenv >= 1.0 {
                            v.fenv = 1.0;
                            v.fstage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        v.fenv = flt_sustain + (v.fenv - flt_sustain) * c.fdecay;
                        if (v.fenv - flt_sustain).abs() < SILENT {
                            v.fenv = flt_sustain;
                            v.fstage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => v.fenv = flt_sustain,
                    Stage::Release => {
                        v.fenv *= c.frelease;
                        if v.fenv < SILENT {
                            v.fenv = 0.0;
                            v.fstage = Stage::Off;
                        }
                    }
                }
                // Control rate, counted from the note's start: block-split independent.
                if v.ctl == 0 {
                    v.filter_coefs(c, log2_cutoff, env_amount, *sr);
                    v.ctl = CONTROL_FRAMES;
                }
                v.ctl -= 1;
                let mut amp = v.env * v.vel;
                if declick {
                    if v.age < *fade_in {
                        amp *= v.age as f32 / *fade_in as f32;
                    }
                    // A loop never reaches its edge. Elsewhere the fade lasts `fade_out` output
                    // frames at any pitch: the distance to the edge over the frames per step.
                    if !v.looping {
                        let to_edge = if v.step > 0.0 {
                            v.end - 1.0 - v.pos
                        } else {
                            v.pos - v.start
                        };
                        let frames_left = to_edge / v.step.abs();
                        if frames_left < *fade_out {
                            amp *= (frames_left / *fade_out).max(0.0) as f32;
                        }
                    }
                }
                let (mut xl, mut xr) = if v.looping {
                    let at = |k| v.loop_index(k);
                    (hermite(&buf.l, v.pos, at), hermite(&buf.r, v.pos, at))
                } else {
                    (hermite(&buf.l, v.pos, |k| k), hermite(&buf.r, v.pos, |k| k))
                };
                if kind != 0 {
                    xl = v.filter(0, kind, xl);
                    xr = v.filter(1, kind, xr);
                }
                sl += xl * amp;
                sr_ += xr * amp;
                v.age = v.age.saturating_add(1);
                v.pos += v.step;
                if v.looping {
                    let past = if v.step > 0.0 {
                        v.pos >= v.hi
                    } else {
                        v.pos < v.lo
                    };
                    if past {
                        let mut r = libm::fmod(v.pos - v.lo, v.len);
                        if r < 0.0 {
                            r += v.len;
                        }
                        v.pos = v.lo + if r < v.len { r } else { 0.0 };
                        v.wrapped = true;
                    }
                } else if v.pos >= v.end || v.pos < v.start {
                    // Ended by itself: a note-off it is still owed must find it.
                    *v = v.quieted();
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
        self.rest();
    }
}
