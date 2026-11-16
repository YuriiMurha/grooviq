//! Basic Synth.
//!
//! - Two morphing oscillators (sine -> triangle -> saw -> square) read from mip-mapped,
//!   band-limited wavetables (1/3-octave levels, crossfaded so glides stay smooth), each with
//!   up to 7 unison voices (detune + stereo spread), plus a sub (sine/square) and white noise.
//! - Filter: zero-delay-feedback state-variable filter (LP12 / HP / BP) or a 4-pole TPT ladder
//!   (LP24) with a saturating feedback path; pre-filter drive; key tracking; envelope and LFO
//!   modulation of the cutoff, all per sample.
//! - Exponential (analog-style) ADSRs for amp and filter, velocity-sensitive.
//! - One global LFO (sine/triangle/saw/square/random) -> cutoff, pitch, shape or volume.
//! - 16-voice poly with click-free voice stealing (3 ms fade), mono and legato modes with a
//!   last-note-priority stack, linear-in-pitch glide and FL-style slide notes.
//! - Click-free playing and tweaking: a retrigger with fast attacks starts the note from scratch
//!   like a note from silence (the old one rings out, or in mono fades out over 3 ms); with slow
//!   attacks the envelopes go on from their level and the new velocity ramps in; a release never
//!   falls faster than a 3 ms line; filter type, unison layout, LFO destination and tuning
//!   changes crossfade or glide over 5 ms; envelope amount and key tracking are smoothed.
//! - Meters: peak levels, voice count, LFO, envelopes of the most recent voice, 256-sample scope.

use crate::api::{Device, MAX_BLOCK, NoteEvent, db_to_gain};
use crate::util::*;
use crate::wavetable::{self, LEVELS, TABLE_LEN, Wavetables};

pub const NUM_PARAMS: usize = 40;
const NUM_VOICES: usize = 16;
const MAX_UNI: usize = 7;
const SCOPE_LEN: usize = 256;
const STEAL_FADE_MS: f32 = 3.0;
const DEFAULT_SLIDE_MS: f32 = 60.0;
/// Declick floor: a release never falls faster than a straight line from the level it started
/// at to zero over this time. Releases longer than about 21 ms never reach the floor.
const MIN_RELEASE_MS: f64 = 3.0;
/// A new velocity on a voice restarted in place ramps in over this time.
const VEL_RAMP_MS: f32 = 3.0;
/// A retrigger (a re-struck key in poly mode, a new note on the sounding voice in mono and
/// legato modes) restarts the sounding voice's envelopes from their current level (no dip) only
/// when every attack that moves is at least this long: its steepest rise, from zero, is then no
/// faster than full scale in 3 ms (3 ms x ln(1.3 / 0.3) x 1.3 = 5.7 ms), which is click-free from
/// any level. With faster attacks the new note starts from scratch instead, like a note from
/// silence, while the old one rings out (poly) or fades out over STEAL_FADE_MS (mono, legato).
const IN_PLACE_ATTACK_MS: f32 = 6.0;
/// Crossfade / glide time (linear) for knob moves that would otherwise step the sound: filter
/// type, unison layout, LFO destination and tuning.
const XFADE_MS: f32 = 5.0;
/// Smoothing time (one-pole, like the cutoff knob's own) of the filter envelope amount and key
/// tracking. Both move the cutoff; a linear ramp's constant speed and abrupt stop excite a
/// resonant filter far more than a one-pole's decelerating approach (on a resonant sine patch a
/// 5 ms ramp reads +5.9 dB on the click detector, a 5 ms one-pole -9.4 dB).
const CUTOFF_MOD_MS: f32 = 5.0;
/// Smoothing time (one-pole, in log frequency) of the cutoff knob or automation value only: the
/// filter envelope, key tracking, velocity and LFO move the cutoff unsmoothed. A jump of a
/// resonant filter's cutoff sweeps its peak through the harmonics (a "zap"); 6 ms read +6.9 dB
/// on the click detector for a jump on a resonant saw, 8 ms reads +5.1 dB.
const CUTOFF_SMOOTH_MS: f32 = 8.0;
/// An attack whose time is shortened while it runs rises no faster than full scale in this time.
const SOFT_RISE_MS: f64 = 3.0;
/// `Bufs::ftype` while a filter type switch crossfades.
const FILTER_XF: u8 = u8::MAX;

const OSC1_SHAPE: usize = 0;
const OSC1_OCT: usize = 1;
const OSC1_SEMI: usize = 2;
const OSC1_FINE: usize = 3;
const OSC1_LEVEL: usize = 4;
const OSC1_UNI: usize = 5;
const OSC1_DETUNE: usize = 6;
const OSC1_SPREAD: usize = 7;
const OSC2_SHAPE: usize = 8;
const OSC2_OCT: usize = 9;
const OSC2_SEMI: usize = 10;
const OSC2_FINE: usize = 11;
const OSC2_LEVEL: usize = 12;
const OSC2_UNI: usize = 13;
const OSC2_DETUNE: usize = 14;
const OSC2_SPREAD: usize = 15;
const SUB_LEVEL: usize = 16;
const SUB_SHAPE: usize = 17;
const NOISE_LEVEL: usize = 18;
const FILTER_TYPE: usize = 19;
const CUTOFF: usize = 20;
const RES: usize = 21;
const DRIVE: usize = 22;
const ENV_AMT: usize = 23;
const KEYTRACK: usize = 24;
const AMP_A: usize = 25;
const AMP_D: usize = 26;
const AMP_S: usize = 27;
const AMP_R: usize = 28;
const FLT_A: usize = 29;
const FLT_D: usize = 30;
const FLT_S: usize = 31;
const FLT_R: usize = 32;
const LFO_SHAPE: usize = 33;
const LFO_RATE: usize = 34;
const LFO_AMOUNT: usize = 35;
const LFO_TARGET: usize = 36;
const GLIDE: usize = 37;
const VOICE_MODE: usize = 38;
const MASTER: usize = 39;

/// (min, max, default) for every parameter, in `SPEC` index order.
pub const SPEC: [(f32, f32, f32); NUM_PARAMS] = [
    (0.0, 3.0, 2.0),         // 0 osc1_shape
    (-3.0, 3.0, 0.0),        // 1 osc1_octave
    (-12.0, 12.0, 0.0),      // 2 osc1_semi
    (-100.0, 100.0, 0.0),    // 3 osc1_fine
    (0.0, 1.0, 0.8),         // 4 osc1_level
    (1.0, 7.0, 1.0),         // 5 osc1_unison
    (0.0, 1.0, 0.25),        // 6 osc1_detune
    (0.0, 1.0, 0.6),         // 7 osc1_spread
    (0.0, 3.0, 3.0),         // 8 osc2_shape
    (-3.0, 3.0, -1.0),       // 9 osc2_octave
    (-12.0, 12.0, 0.0),      // 10 osc2_semi
    (-100.0, 100.0, 7.0),    // 11 osc2_fine
    (0.0, 1.0, 0.4),         // 12 osc2_level
    (1.0, 7.0, 1.0),         // 13 osc2_unison
    (0.0, 1.0, 0.2),         // 14 osc2_detune
    (0.0, 1.0, 0.5),         // 15 osc2_spread
    (0.0, 1.0, 0.0),         // 16 sub_level
    (0.0, 1.0, 0.0),         // 17 sub_shape
    (0.0, 1.0, 0.0),         // 18 noise_level
    (0.0, 3.0, 0.0),         // 19 filter_type
    (20.0, 20000.0, 4000.0), // 20 filter_cutoff
    (0.0, 1.0, 0.2),         // 21 filter_res
    (0.0, 1.0, 0.0),         // 22 filter_drive
    (-1.0, 1.0, 0.3),        // 23 filter_env_amt
    (0.0, 1.0, 0.5),         // 24 filter_keytrack
    (0.5, 5000.0, 5.0),      // 25 amp_attack
    (1.0, 5000.0, 300.0),    // 26 amp_decay
    (0.0, 1.0, 0.7),         // 27 amp_sustain
    (1.0, 8000.0, 250.0),    // 28 amp_release
    (0.5, 5000.0, 2.0),      // 29 flt_attack
    (1.0, 5000.0, 400.0),    // 30 flt_decay
    (0.0, 1.0, 0.2),         // 31 flt_sustain
    (1.0, 8000.0, 300.0),    // 32 flt_release
    (0.0, 4.0, 0.0),         // 33 lfo_shape
    (0.05, 20.0, 2.0),       // 34 lfo_rate
    (0.0, 1.0, 0.0),         // 35 lfo_amount
    (0.0, 3.0, 0.0),         // 36 lfo_target
    (0.0, 1000.0, 0.0),      // 37 glide
    (0.0, 2.0, 0.0),         // 38 voice_mode
    (-36.0, 6.0, -6.0),      // 39 master
];

/// Parameters that are integers or enums (rounded on set).
fn is_stepped(i: usize) -> bool {
    matches!(
        i,
        OSC1_OCT
            | OSC1_SEMI
            | OSC1_UNI
            | OSC2_OCT
            | OSC2_SEMI
            | OSC2_UNI
            | SUB_SHAPE
            | FILTER_TYPE
            | LFO_SHAPE
            | LFO_TARGET
            | VOICE_MODE
    )
}

/// Preset names, in `SPEC` index order.
pub const PRESETS: [&str; 7] = [
    "808 Glide",
    "Sub Bass",
    "Reese",
    "Bell Keys",
    "Dark Pad",
    "Pluck",
    "Trap Lead",
];

/// Full parameter list `(index, value)` for a factory preset (all 40 parameters), or None.
/// "Init" returns the default patch.
pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let overrides: &[(usize, f32)] = match name {
        "Init" | "Default" => &[],
        // Trap 808: sine through a driven ladder whose envelope opens on the attack (the "knock"),
        // keytracked so the saturation harmonics follow the note, long exponential decay,
        // legato so overlapping notes slide without retriggering.
        "808 Glide" => &[
            (OSC1_SHAPE, 0.0),
            (OSC1_OCT, -1.0),
            (OSC1_LEVEL, 0.9),
            (OSC1_UNI, 1.0),
            (OSC2_LEVEL, 0.0),
            (SUB_LEVEL, 0.0),
            (NOISE_LEVEL, 0.0),
            (FILTER_TYPE, 1.0),
            (CUTOFF, 650.0),
            (RES, 0.05),
            (DRIVE, 0.42),
            (ENV_AMT, 0.35),
            (KEYTRACK, 1.0),
            (AMP_A, 1.0),
            (AMP_D, 4500.0),
            (AMP_S, 0.12),
            (AMP_R, 220.0),
            (FLT_A, 0.5),
            (FLT_D, 160.0),
            (FLT_S, 0.0),
            (FLT_R, 200.0),
            (LFO_AMOUNT, 0.0),
            (GLIDE, 90.0),
            (VOICE_MODE, 2.0),
            (MASTER, 0.0),
        ],
        "Sub Bass" => &[
            (OSC1_SHAPE, 0.0),
            (OSC1_OCT, -1.0),
            (OSC1_LEVEL, 0.85),
            (OSC1_UNI, 1.0),
            (OSC2_SHAPE, 1.0),
            (OSC2_OCT, 0.0),
            (OSC2_FINE, 0.0),
            (OSC2_LEVEL, 0.12),
            (OSC2_UNI, 1.0),
            (FILTER_TYPE, 0.0),
            (CUTOFF, 900.0),
            (RES, 0.0),
            (DRIVE, 0.1),
            (ENV_AMT, 0.0),
            (KEYTRACK, 0.5),
            (AMP_A, 2.0),
            (AMP_D, 400.0),
            (AMP_S, 1.0),
            (AMP_R, 90.0),
            (GLIDE, 0.0),
            (VOICE_MODE, 1.0),
            (MASTER, -4.0),
        ],
        "Reese" => &[
            (OSC1_SHAPE, 2.0),
            (OSC1_OCT, -1.0),
            (OSC1_LEVEL, 0.7),
            (OSC1_UNI, 5.0),
            (OSC1_DETUNE, 0.42),
            (OSC1_SPREAD, 0.25),
            (OSC2_SHAPE, 2.0),
            (OSC2_OCT, -1.0),
            (OSC2_FINE, -9.0),
            (OSC2_LEVEL, 0.6),
            (OSC2_UNI, 3.0),
            (OSC2_DETUNE, 0.3),
            (OSC2_SPREAD, 0.35),
            (SUB_LEVEL, 0.45),
            (SUB_SHAPE, 0.0),
            (FILTER_TYPE, 1.0),
            (CUTOFF, 700.0),
            (RES, 0.25),
            (DRIVE, 0.35),
            (ENV_AMT, 0.12),
            (KEYTRACK, 0.3),
            (AMP_A, 3.0),
            (AMP_D, 800.0),
            (AMP_S, 0.9),
            (AMP_R, 160.0),
            (FLT_A, 5.0),
            (FLT_D, 600.0),
            (FLT_S, 0.3),
            (FLT_R, 300.0),
            (LFO_SHAPE, 0.0),
            (LFO_RATE, 0.25),
            (LFO_AMOUNT, 0.25),
            (LFO_TARGET, 0.0),
            (GLIDE, 50.0),
            (VOICE_MODE, 2.0),
            (MASTER, -6.0),
        ],
        // Inharmonic partial: osc 2 at 2.67x (+1 oct +5 st +50 ct) gives a struck-bell overtone.
        "Bell Keys" => &[
            (OSC1_SHAPE, 0.35),
            (OSC1_OCT, 0.0),
            (OSC1_LEVEL, 0.75),
            (OSC1_UNI, 1.0),
            (OSC2_SHAPE, 0.0),
            (OSC2_OCT, 1.0),
            (OSC2_SEMI, 5.0),
            (OSC2_FINE, 50.0),
            (OSC2_LEVEL, 0.45),
            (OSC2_UNI, 1.0),
            (FILTER_TYPE, 0.0),
            (CUTOFF, 7000.0),
            (RES, 0.1),
            (DRIVE, 0.0),
            (ENV_AMT, 0.15),
            (KEYTRACK, 0.8),
            (AMP_A, 0.5),
            (AMP_D, 1600.0),
            (AMP_S, 0.0),
            (AMP_R, 1400.0),
            (FLT_A, 0.5),
            (FLT_D, 800.0),
            (FLT_S, 0.3),
            (FLT_R, 1000.0),
            (VOICE_MODE, 0.0),
            (MASTER, -11.0),
        ],
        "Dark Pad" => &[
            (OSC1_SHAPE, 2.0),
            (OSC1_LEVEL, 0.6),
            (OSC1_UNI, 7.0),
            (OSC1_DETUNE, 0.35),
            (OSC1_SPREAD, 0.9),
            (OSC2_SHAPE, 1.4),
            (OSC2_OCT, -1.0),
            (OSC2_FINE, -5.0),
            (OSC2_LEVEL, 0.45),
            (OSC2_UNI, 5.0),
            (OSC2_DETUNE, 0.3),
            (OSC2_SPREAD, 0.8),
            (NOISE_LEVEL, 0.03),
            (FILTER_TYPE, 1.0),
            (CUTOFF, 650.0),
            (RES, 0.2),
            (DRIVE, 0.1),
            (ENV_AMT, 0.22),
            (KEYTRACK, 0.3),
            (AMP_A, 900.0),
            (AMP_D, 2500.0),
            (AMP_S, 0.85),
            (AMP_R, 2800.0),
            (FLT_A, 1800.0),
            (FLT_D, 3000.0),
            (FLT_S, 0.4),
            (FLT_R, 2500.0),
            (LFO_SHAPE, 1.0),
            (LFO_RATE, 0.15),
            (LFO_AMOUNT, 0.18),
            (LFO_TARGET, 0.0),
            (VOICE_MODE, 0.0),
            (MASTER, -9.0),
        ],
        "Pluck" => &[
            (OSC1_SHAPE, 2.0),
            (OSC1_LEVEL, 0.75),
            (OSC1_UNI, 3.0),
            (OSC1_DETUNE, 0.18),
            (OSC1_SPREAD, 0.5),
            (OSC2_SHAPE, 3.0),
            (OSC2_OCT, 1.0),
            (OSC2_FINE, 0.0),
            (OSC2_LEVEL, 0.25),
            (OSC2_UNI, 1.0),
            (FILTER_TYPE, 1.0),
            (CUTOFF, 600.0),
            (RES, 0.3),
            (DRIVE, 0.15),
            (ENV_AMT, 0.6),
            (KEYTRACK, 0.6),
            (AMP_A, 0.5),
            (AMP_D, 450.0),
            (AMP_S, 0.0),
            (AMP_R, 280.0),
            (FLT_A, 0.5),
            (FLT_D, 220.0),
            (FLT_S, 0.0),
            (FLT_R, 200.0),
            (VOICE_MODE, 0.0),
            (MASTER, 0.0),
        ],
        "Trap Lead" => &[
            (OSC1_SHAPE, 2.55),
            (OSC1_LEVEL, 0.7),
            (OSC1_UNI, 3.0),
            (OSC1_DETUNE, 0.22),
            (OSC1_SPREAD, 0.45),
            (OSC2_SHAPE, 2.0),
            (OSC2_OCT, 1.0),
            (OSC2_FINE, 5.0),
            (OSC2_LEVEL, 0.25),
            (OSC2_UNI, 1.0),
            (FILTER_TYPE, 0.0),
            (CUTOFF, 3800.0),
            (RES, 0.25),
            (DRIVE, 0.2),
            (ENV_AMT, 0.25),
            (KEYTRACK, 0.5),
            (AMP_A, 4.0),
            (AMP_D, 500.0),
            (AMP_S, 0.75),
            (AMP_R, 260.0),
            (FLT_A, 2.0),
            (FLT_D, 450.0),
            (FLT_S, 0.4),
            (FLT_R, 300.0),
            (LFO_SHAPE, 0.0),
            (LFO_RATE, 5.5),
            (LFO_AMOUNT, 0.13),
            (LFO_TARGET, 1.0),
            (GLIDE, 55.0),
            (VOICE_MODE, 2.0),
            (MASTER, -6.0),
        ],
        _ => return None,
    };
    let mut p: Vec<f32> = SPEC.iter().map(|s| s.2).collect();
    for &(i, v) in overrides {
        p[i] = v;
    }
    Some(
        p.into_iter()
            .enumerate()
            .map(|(i, v)| (i as u32, v))
            .collect(),
    )
}

// ---------------------------------------------------------------------------------------------
// Envelope

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Exponential ADSR coefficients (targets overshoot so each stage ends in its stated time:
/// attack is gently convex, decay/release fall ~60 dB over their time, like an analog RC).
#[derive(Clone, Copy, Debug, Default)]
struct EnvCfg {
    a_coef: f64,
    a_base: f64,
    d_coef: f64,
    d_base: f64,
    sus: f64,
    r_coef: f64,
    r_base: f64,
    /// Largest per-sample fall of a release, as a fraction of the level it started from.
    rel_k: f64,
    /// Largest per-sample rise of a softened attack.
    rise: f64,
}

impl EnvCfg {
    fn new(a_ms: f32, d_ms: f32, sus: f32, r_ms: f32, sr: f32) -> Self {
        const RA: f64 = 0.3;
        const RDR: f64 = 0.001;
        let rate = |ms: f64| (ms * 0.001 * sr as f64).max(1.0);
        let a_coef = (-((1.0 + RA) / RA).ln() / rate(a_ms as f64)).exp();
        let d_coef = (-((1.0 + RDR) / RDR).ln() / rate(d_ms as f64)).exp();
        let r_coef = (-((1.0 + RDR) / RDR).ln() / rate(r_ms as f64)).exp();
        let sus = sus as f64;
        Self {
            a_coef,
            a_base: (1.0 + RA) * (1.0 - a_coef),
            d_coef,
            d_base: (sus - RDR) * (1.0 - d_coef),
            sus,
            r_coef,
            r_base: -RDR * (1.0 - r_coef),
            rel_k: 1.0 / rate(MIN_RELEASE_MS),
            rise: 1.0 / rate(SOFT_RISE_MS),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Env {
    stage: Stage,
    v: f64,
    /// Level the release started from (scales the declick floor).
    rel_from: f64,
    /// The attack's time changed while it ran: it rises no faster than SOFT_RISE_MS.
    soft: bool,
}

impl Env {
    const IDLE: Self = Self {
        stage: Stage::Idle,
        v: 0.0,
        rel_from: 0.0,
        soft: false,
    };

    fn soften(&mut self) {
        if self.stage == Stage::Attack {
            self.soft = true;
        }
    }

    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
            self.rel_from = self.v;
        }
    }

    #[inline(always)]
    fn next(&mut self, c: &EnvCfg) -> f64 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                let v = c.a_base + self.v * c.a_coef;
                self.v = if self.soft { v.min(self.v + c.rise) } else { v };
                if self.v >= 1.0 {
                    self.v = 1.0;
                    self.stage = Stage::Decay;
                    self.soft = false;
                }
            }
            Stage::Decay => {
                self.v = c.d_base + self.v * c.d_coef;
                if self.v <= c.sus {
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {
                // follows sustain changes without a jump (and settles exactly: no denormals)
                self.v += (c.sus - self.v) * 0.002;
                if (self.v - c.sus).abs() < 1.0e-9 {
                    self.v = c.sus;
                }
            }
            Stage::Release => {
                // exponential, but never steeper than the declick floor: a release of a few ms
                // starts as steeply as a fade of a fraction of a ms otherwise (a click)
                let floor = self.v - self.rel_from * c.rel_k;
                self.v = (c.r_base + self.v * c.r_coef).max(floor);
                if self.v <= 0.0 {
                    self.v = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.v
    }
}

// ---------------------------------------------------------------------------------------------
// Filters

/// Cytomic/Simper linear trapezoidal SVF state.
#[derive(Clone, Copy, Debug, Default)]
struct Svf {
    ic1: f32,
    ic2: f32,
}

#[derive(Clone, Copy)]
struct SvfCoefs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoefs {
    #[inline(always)]
    fn new(g: f32, k: f32) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            k,
            a1,
            a2,
            a3: g * a2,
        }
    }
}

impl Svf {
    /// Returns (low, band, high).
    #[inline(always)]
    fn run(&mut self, v0: f32, c: &SvfCoefs) -> (f32, f32, f32) {
        let v3 = v0 - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);
        (v2, v1, v0 - c.k * v1 - v2)
    }
}

/// 4-pole TPT ladder. The feedback loop is solved linearly (zero delay), then the input to the
/// ladder is soft-saturated, which bounds self-oscillation and gives the classic ladder growl.
#[derive(Clone, Copy, Debug, Default)]
struct Ladder {
    s: [f32; 4],
}

impl Ladder {
    #[inline(always)]
    fn run(&mut self, x: f32, gg: f32, k: f32) -> f32 {
        let b = 1.0 - gg;
        let g2 = gg * gg;
        let sigma =
            g2 * gg * b * self.s[0] + g2 * b * self.s[1] + gg * b * self.s[2] + b * self.s[3];
        let g4 = g2 * g2;
        let y_est = (g4 * x + sigma) / (1.0 + k * g4);
        let mut u = 1.6 * fast_tanh((x - k * y_est) * 0.625);
        for s in self.s.iter_mut() {
            let v = (u - *s) * gg;
            let y = v + *s;
            *s = flush(y + v);
            u = y;
        }
        u * (1.0 + 0.7 * k)
    }
}

// ---------------------------------------------------------------------------------------------
// Declick ramps

/// A value that moves to a new target in a straight line over a fixed number of samples (a
/// target change during a ramp restarts it from where it is). Settles exactly on the target.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    cur: f32,
    target: f32,
    step: f32,
    left: u32,
    len: u32,
}

impl Ramp {
    fn new(v: f32, len: u32) -> Self {
        Self {
            cur: v,
            target: v,
            step: 0.0,
            left: 0,
            len: len.max(1),
        }
    }

    fn set(&mut self, v: f32) {
        if v != self.target {
            self.restart(self.cur, v);
        }
    }

    /// Jumps to `from` (only where nothing is heard yet, e.g. a crossfade position), then ramps.
    fn restart(&mut self, from: f32, to: f32) {
        self.cur = from;
        self.target = to;
        self.left = self.len;
        self.step = (to - from) / self.len as f32;
    }

    fn snap(&mut self) {
        self.cur = self.target;
        self.left = 0;
    }

    fn settled(&self) -> bool {
        self.left == 0
    }

    #[inline(always)]
    fn next(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            self.cur = if self.left == 0 {
                self.target
            } else {
                self.cur + self.step
            };
        }
        self.cur
    }
}

// ---------------------------------------------------------------------------------------------
// Oscillator helpers

#[derive(Clone, Copy, Debug, PartialEq)]
struct Unison {
    n: usize,
    ratio: [f64; MAX_UNI],
    pl: [f32; MAX_UNI],
    pr: [f32; MAX_UNI],
    /// Highest detune in semitones (picks the mip level for the brightest voice).
    max_semi: f32,
}

impl Unison {
    fn new(n: usize, detune: f32, spread: f32) -> Self {
        let n = n.clamp(1, MAX_UNI);
        let cents = 100.0 * detune * detune;
        let norm = 1.0 / (n as f32).sqrt();
        let mut u = Self {
            n,
            ratio: [1.0; MAX_UNI],
            pl: [0.0; MAX_UNI],
            pr: [0.0; MAX_UNI],
            max_semi: 0.0,
        };
        for v in 0..n {
            let pos = if n == 1 {
                0.0
            } else {
                -1.0 + 2.0 * v as f32 / (n - 1) as f32
            };
            u.ratio[v] = 2f64.powf((pos * cents) as f64 / 1200.0);
            let pan = pos * spread;
            u.pl[v] = (1.0 - pan).min(1.0) * norm;
            u.pr[v] = (1.0 + pan).min(1.0) * norm;
        }
        u.max_semi = if n > 1 { cents / 100.0 } else { 0.0 };
        u
    }
}

/// A unison layout plus a crossfade from the gains heard before it: a voice count or spread
/// change moves each unison voice's gain over XFADE_MS instead of stepping it (added voices fade
/// in, removed ones fade out at their old detune). Detune changes apply at once: a small,
/// phase-continuous pitch change.
#[derive(Clone, Copy, Debug)]
struct UniState {
    /// The layout the knobs ask for (the gains once the crossfade is over).
    to: Unison,
    /// Gains when the crossfade started, and how many of them are non-zero.
    from_l: [f32; MAX_UNI],
    from_r: [f32; MAX_UNI],
    from_n: usize,
    /// Detune ratios: the layout's, and the old ones of voices fading out.
    ratio: [f64; MAX_UNI],
}

impl UniState {
    fn new(u: Unison) -> Self {
        Self {
            to: u,
            from_l: u.pl,
            from_r: u.pr,
            from_n: u.n,
            ratio: u.ratio,
        }
    }

    /// Starts a crossfade to `u` from the gains heard at crossfade position `x` (1 = settled).
    fn retarget(&mut self, u: Unison, x: f32) {
        let mut from_n = 0;
        for v in 0..MAX_UNI {
            if x >= 1.0 {
                self.from_l[v] = self.to.pl[v];
                self.from_r[v] = self.to.pr[v];
            } else {
                self.from_l[v] += (self.to.pl[v] - self.from_l[v]) * x;
                self.from_r[v] += (self.to.pr[v] - self.from_r[v]) * x;
            }
            if self.from_l[v] != 0.0 || self.from_r[v] != 0.0 {
                from_n = v + 1;
            }
            if v < u.n {
                self.ratio[v] = u.ratio[v];
            }
        }
        self.from_n = from_n;
        self.to = u;
    }
}

/// Start phases for unison voices (fixed so renders are repeatable; voice 0 of a single-voice
/// oscillator always starts at phase 0 for consistent, click-free attacks). Voices beyond the
/// current count get their phases too, so raising the count later fades in decorrelated voices.
const UNI_PHASE: [f64; MAX_UNI] = [0.0, 0.613, 0.287, 0.871, 0.142, 0.459, 0.734];

/// Shape morph position: table pair (k, k + 1) and blend.
#[derive(Clone, Copy)]
struct Morph {
    k: usize,
    t: f32,
}

impl Morph {
    #[inline(always)]
    fn new(shape: f32) -> Self {
        let s = shape.clamp(0.0, 3.0);
        let k = (s as usize).min(2);
        Self { k, t: s - k as f32 }
    }
}

#[inline(always)]
fn lerp_table(tb: &[f32], idx: usize, fr: f32) -> f32 {
    let a = tb[idx];
    a + (tb[idx + 1] - a) * fr
}

/// One band-limited sample at `phase` for a morph position and mip level pair.
#[inline(always)]
fn wt_read(t: &Wavetables, m: Morph, lv: (usize, f32), phase: f64) -> f32 {
    let pos = phase * TABLE_LEN as f64;
    let idx = (pos as usize).min(TABLE_LEN - 1);
    let fr = (pos - idx as f64) as f32;
    let (l, lt) = lv;
    let mut acc = 0.0;
    let w0 = 1.0 - m.t;
    if w0 > 0.0 {
        let mut v = lerp_table(t.table(m.k, l), idx, fr);
        if lt > 0.0 {
            v += (lerp_table(t.table(m.k, l + 1), idx, fr) - v) * lt;
        }
        acc += v * w0;
    }
    if m.t > 0.0 {
        let mut v = lerp_table(t.table(m.k + 1, l), idx, fr);
        if lt > 0.0 {
            v += (lerp_table(t.table(m.k + 1, l + 1), idx, fr) - v) * lt;
        }
        acc += v * m.t;
    }
    acc
}

/// Sums the unison voices of one oscillator; `x` is the layout crossfade position (1 = settled).
#[inline(always)]
fn osc_unison(
    t: &Wavetables,
    m: Morph,
    lv: (usize, f32),
    ph: &mut [f64; MAX_UNI],
    inc: f64,
    u: &UniState,
    x: f32,
) -> (f32, f32) {
    let (mut l, mut r) = (0.0, 0.0);
    if x >= 1.0 {
        for v in 0..u.to.n {
            let p = ph[v];
            let s = wt_read(t, m, lv, p);
            let mut np = p + inc * u.ratio[v];
            if np >= 1.0 {
                np -= 1.0;
            }
            ph[v] = np;
            l += s * u.to.pl[v];
            r += s * u.to.pr[v];
        }
    } else {
        for v in 0..u.to.n.max(u.from_n) {
            let p = ph[v];
            let s = wt_read(t, m, lv, p);
            let mut np = p + inc * u.ratio[v];
            if np >= 1.0 {
                np -= 1.0;
            }
            ph[v] = np;
            l += s * (u.from_l[v] + (u.to.pl[v] - u.from_l[v]) * x);
            r += s * (u.from_r[v] + (u.to.pr[v] - u.from_r[v]) * x);
        }
    }
    (l, r)
}

/// Phase increment and mip level for an oscillator at pitch `p` (semitones).
#[inline(always)]
fn tune(p: f32, max_semi: f32, cx: &Ctx) -> (f64, (usize, f32)) {
    (
        (pitch_hz(p) as f64 * cx.inv_sr).min(0.45),
        level_for(p + max_semi, cx.lam_const),
    )
}

#[inline(always)]
fn pitch_hz(p: f32) -> f32 {
    440.0 * ((p - 69.0) * (1.0 / 12.0)).exp2()
}

/// Mip level (and crossfade to the next level) for a pitch in semitones.
#[inline(always)]
fn level_for(p: f32, lam_const: f32) -> (usize, f32) {
    let lam = ((p - 69.0) * 0.25 + lam_const).clamp(0.0, (LEVELS - 2) as f32 + 0.999);
    let l = lam as usize;
    (l, lam - l as f32)
}

// ---------------------------------------------------------------------------------------------
// LFO

#[derive(Clone, Copy, Debug)]
struct Lfo {
    phase: f64,
    inc: f64,
    shape: u8,
    held: f32,
    rng: Rng,
    out: f32,
    coef: f32,
}

impl Lfo {
    #[inline(always)]
    fn next(&mut self) -> f32 {
        let ph = self.phase as f32;
        let raw = match self.shape {
            0 => (ph * std::f32::consts::TAU).sin(),
            1 => {
                if ph < 0.25 {
                    4.0 * ph
                } else if ph < 0.75 {
                    2.0 - 4.0 * ph
                } else {
                    4.0 * ph - 4.0
                }
            }
            2 => 1.0 - 2.0 * ph,
            3 => {
                if ph < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            _ => self.held,
        };
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            self.held = self.rng.bipolar();
        }
        // ~2 ms smoothing: removes clicks from square/saw/random edges
        self.out = flush(self.out + (raw - self.out) * self.coef);
        self.out
    }
}

// ---------------------------------------------------------------------------------------------
// Voice

#[derive(Clone, Copy, Debug)]
struct Pending {
    key: u8,
    vel: f32,
    gate: bool,
    from: Option<f32>,
    glide_ms: f32,
}

/// Per-block, per-sample values shared by every voice (smoothed params and LFO routing).
struct Bufs {
    shape1: [f32; MAX_BLOCK],
    shape2: [f32; MAX_BLOCK],
    lvl1: [f32; MAX_BLOCK],
    lvl2: [f32; MAX_BLOCK],
    sub: [f32; MAX_BLOCK],
    noise: [f32; MAX_BLOCK],
    cut: [f32; MAX_BLOCK],
    svf_k: [f32; MAX_BLOCK],
    lad_k: [f32; MAX_BLOCK],
    drive_g: [f32; MAX_BLOCK],
    drive_n: [f32; MAX_BLOCK],
    pitch: [f32; MAX_BLOCK],
    trem: [f32; MAX_BLOCK],
    master: [f32; MAX_BLOCK],
    /// Filter envelope depth in octaves.
    env_oct: [f32; MAX_BLOCK],
    keytrack: [f32; MAX_BLOCK],
    /// Oscillator tuning offsets in semitones.
    off1: [f32; MAX_BLOCK],
    off2: [f32; MAX_BLOCK],
    /// Unison layout crossfade positions (1 = settled).
    uxf1: [f32; MAX_BLOCK],
    uxf2: [f32; MAX_BLOCK],
    /// Filter type (`FILTER_XF` while a type switch crossfades, with the weights in `fw`).
    ftype: [u8; MAX_BLOCK],
    fw: [[f32; 4]; MAX_BLOCK],
}

impl Bufs {
    fn new() -> Self {
        let z = [0.0; MAX_BLOCK];
        Self {
            shape1: z,
            shape2: z,
            lvl1: z,
            lvl2: z,
            sub: z,
            noise: z,
            cut: z,
            svf_k: z,
            lad_k: z,
            drive_g: z,
            drive_n: z,
            pitch: z,
            trem: z,
            master: z,
            env_oct: z,
            keytrack: z,
            off1: z,
            off2: z,
            uxf1: z,
            uxf2: z,
            ftype: [0; MAX_BLOCK],
            fw: [[0.0; 4]; MAX_BLOCK],
        }
    }
}

/// Scalar settings every voice reads.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    sr: f32,
    inv_sr: f64,
    pi_sr: f32,
    max_fc: f32,
    amp: EnvCfg,
    flt: EnvCfg,
    uni1: UniState,
    uni2: UniState,
    sub_square: bool,
    lam_const: f32,
    fade_step: f32,
    /// VEL_RAMP_MS in samples.
    vel_ramp: u32,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    gate: bool,
    key: u8,
    vel_amp: f32,
    vel_flt: f32,
    /// After an in-place retrigger the velocity gains ramp to `vel_to` by `vel_step` per sample
    /// for `vel_left` more samples (0 = settled).
    vel_to: (f32, f32),
    vel_step: (f32, f32),
    vel_left: u32,
    age: u64,
    pitch: f32,
    target: f32,
    glide_step: f32,
    amp: Env,
    flt: Env,
    ph1: [f64; MAX_UNI],
    ph2: [f64; MAX_UNI],
    ph_sub: f64,
    svf: [Svf; 2],
    lad: [Ladder; 2],
    /// Whether each filter ran on the last sample (its state is current). One that comes back
    /// in during a filter type crossfade is first seeded from its input.
    svf_live: bool,
    lad_live: bool,
    fade: f32,
    fading: bool,
    pending: Option<Pending>,
    rng: Rng,
    /// Pitches the cached increments and mip levels were computed for (NaN = stale).
    last_p1: f32,
    last_p2: f32,
    last_pm: f32,
    inc1: f64,
    inc2: f64,
    inc_sub: f64,
    lv1: (usize, f32),
    lv2: (usize, f32),
    lv_sub: (usize, f32),
}

fn vel_curves(vel: f32) -> (f32, f32) {
    let v = vel.clamp(0.0, 1.0);
    (0.25 + 0.75 * v.powf(1.2), 0.6 + 0.4 * v)
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            gate: false,
            key: 60,
            vel_amp: 1.0,
            vel_flt: 1.0,
            vel_to: (1.0, 1.0),
            vel_step: (0.0, 0.0),
            vel_left: 0,
            age: 0,
            pitch: 60.0,
            target: 60.0,
            glide_step: 0.0,
            amp: Env::IDLE,
            flt: Env::IDLE,
            ph1: [0.0; MAX_UNI],
            ph2: [0.0; MAX_UNI],
            ph_sub: 0.0,
            svf: [Svf::default(); 2],
            lad: [Ladder::default(); 2],
            svf_live: true,
            lad_live: true,
            fade: 1.0,
            fading: false,
            pending: None,
            rng: Rng(seed),
            last_p1: f32::NAN,
            last_p2: f32::NAN,
            last_pm: f32::NAN,
            inc1: 0.0,
            inc2: 0.0,
            inc_sub: 0.0,
            lv1: (0, 0.0),
            lv2: (0, 0.0),
            lv_sub: (0, 0.0),
        }
    }

    fn glide_to(&mut self, key: f32, ms: f32, sr: f32) {
        self.target = key;
        if ms <= 0.0 {
            self.pitch = key;
            self.glide_step = 0.0;
        } else {
            self.glide_step = (key - self.pitch).abs() / (ms * 0.001 * sr).max(1.0);
        }
    }

    /// Fresh start: phases, filters and envelopes from zero.
    fn start(&mut self, p: Pending, cx: &Ctx, age: u64) {
        self.active = true;
        self.gate = p.gate;
        self.key = p.key;
        (self.vel_amp, self.vel_flt) = vel_curves(p.vel);
        self.vel_left = 0;
        self.age = age;
        self.pitch = p.from.unwrap_or(p.key as f32);
        self.glide_to(
            p.key as f32,
            if p.from.is_some() { p.glide_ms } else { 0.0 },
            cx.sr,
        );
        self.amp = Env {
            stage: Stage::Attack,
            ..Env::IDLE
        };
        self.flt = self.amp;
        if !p.gate {
            self.amp.release();
            self.flt.release();
        }
        for v in 0..MAX_UNI {
            self.ph1[v] = UNI_PHASE[v];
            self.ph2[v] = if v == 0 && cx.uni2.to.n == 1 {
                0.0
            } else {
                UNI_PHASE[(v + 3) % MAX_UNI]
            };
        }
        self.ph_sub = 0.0;
        self.svf = [Svf::default(); 2];
        self.lad = [Ladder::default(); 2];
        self.svf_live = true;
        self.lad_live = true;
        self.fade = 1.0;
        self.fading = false;
        self.pending = None;
        self.invalidate_pitch();
    }

    fn invalidate_pitch(&mut self) {
        self.last_p1 = f32::NAN;
        self.last_p2 = f32::NAN;
        self.last_pm = f32::NAN;
    }

    /// Restarts the envelopes from their current level without touching phases or filters. Only
    /// for attacks slow enough to rise from any level without a click (IN_PLACE_ATTACK_MS). The
    /// new velocity ramps in over VEL_RAMP_MS: a velocity step on a sounding voice is a click.
    fn retrigger(&mut self, key: u8, vel: f32, age: u64, cx: &Ctx) {
        self.gate = true;
        self.key = key;
        self.age = age;
        let to = vel_curves(vel);
        let n = cx.vel_ramp.max(1);
        self.vel_to = to;
        self.vel_step = (
            (to.0 - self.vel_amp) / n as f32,
            (to.1 - self.vel_flt) / n as f32,
        );
        self.vel_left = n;
        self.amp.stage = Stage::Attack;
        self.flt.stage = Stage::Attack;
    }

    fn release(&mut self) {
        self.gate = false;
        self.amp.release();
        self.flt.release();
    }

    /// Both channels through the filter (`ft` from `Bufs::ftype`): the type's own output, or
    /// while a type switch crossfades, the outputs mixed by the weights `w` (LP12, LP24, HP, BP).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn filter(
        &mut self,
        ft: u8,
        w: &[f32; 4],
        xl: f32,
        xr: f32,
        g: f32,
        svf_k: f32,
        lad_k: f32,
    ) -> (f32, f32) {
        let lad = ft == 1 || (ft == FILTER_XF && w[1] > 0.0);
        let svf = ft != 1 && (ft != FILTER_XF || w[0] > 0.0 || w[2] > 0.0 || w[3] > 0.0);
        // A filter that was idle has a stale state: seed it with the state a long-settled filter
        // has for a constant input equal to this sample (it then rings out nothing): SVF band 0,
        // low = x; every ladder stage at the loop's DC level x / (1 + k).
        if lad && !self.lad_live {
            let k1 = 1.0 + lad_k;
            self.lad = [Ladder { s: [xl / k1; 4] }, Ladder { s: [xr / k1; 4] }];
        }
        if svf && !self.svf_live {
            self.svf = [Svf { ic1: 0.0, ic2: xl }, Svf { ic1: 0.0, ic2: xr }];
        }
        self.lad_live = lad;
        self.svf_live = svf;
        let (mut yl, mut yr) = (0.0, 0.0);
        let mut ladder = (0.0, 0.0);
        if lad {
            let gg = g / (1.0 + g);
            ladder = (
                self.lad[0].run(xl, gg, lad_k),
                self.lad[1].run(xr, gg, lad_k),
            );
            if ft == 1 {
                return ladder;
            }
        }
        if svf {
            let co = SvfCoefs::new(g, svf_k);
            let a = self.svf[0].run(xl, &co);
            let c = self.svf[1].run(xr, &co);
            match ft {
                0 => return (a.0, c.0),
                2 => return (a.2, c.2),
                3 => return (a.1 * co.k, c.1 * co.k),
                _ => {
                    yl = w[0] * a.0 + w[2] * a.2 + w[3] * a.1 * co.k;
                    yr = w[0] * c.0 + w[2] * c.2 + w[3] * c.1 * co.k;
                }
            }
        }
        (yl + w[1] * ladder.0, yr + w[1] * ladder.1)
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        cx: &Ctx,
        b: &Bufs,
        t: &Wavetables,
        start: usize,
        end: usize,
        out_l: &mut [f32],
        out_r: &mut [f32],
        age: u64,
    ) {
        for i in start..end {
            let mut fade = 1.0f32;
            if self.fading {
                self.fade -= cx.fade_step;
                if self.fade <= 0.0 {
                    match self.pending.take() {
                        Some(p) => self.start(p, cx, age),
                        None => {
                            self.active = false;
                            self.fading = false;
                            return;
                        }
                    }
                } else {
                    fade = self.fade;
                }
            }

            if self.vel_left > 0 {
                self.vel_left -= 1;
                if self.vel_left == 0 {
                    (self.vel_amp, self.vel_flt) = self.vel_to;
                } else {
                    self.vel_amp += self.vel_step.0;
                    self.vel_flt += self.vel_step.1;
                }
            }

            if self.pitch != self.target {
                let d = self.target - self.pitch;
                if self.glide_step <= 0.0 || d.abs() <= self.glide_step {
                    self.pitch = self.target;
                } else {
                    self.pitch += self.glide_step.copysign(d);
                }
            }

            let aenv = self.amp.next(&cx.amp) as f32;
            if self.amp.stage == Stage::Idle {
                self.active = false;
                return;
            }
            let fenv = self.flt.next(&cx.flt) as f32;

            let pm = self.pitch + b.pitch[i];
            let p1 = pm + b.off1[i];
            if p1 != self.last_p1 {
                self.last_p1 = p1;
                (self.inc1, self.lv1) = tune(p1, cx.uni1.to.max_semi, cx);
            }
            let p2 = pm + b.off2[i];
            if p2 != self.last_p2 {
                self.last_p2 = p2;
                (self.inc2, self.lv2) = tune(p2, cx.uni2.to.max_semi, cx);
            }
            if pm != self.last_pm {
                self.last_pm = pm;
                (self.inc_sub, self.lv_sub) = tune(pm - 12.0, 0.0, cx);
            }

            let (mut sl, mut sr) = (0.0f32, 0.0f32);
            let lv1 = b.lvl1[i];
            if lv1 > 0.0 {
                let (a, c) = osc_unison(
                    t,
                    Morph::new(b.shape1[i]),
                    self.lv1,
                    &mut self.ph1,
                    self.inc1,
                    &cx.uni1,
                    b.uxf1[i],
                );
                sl += a * lv1;
                sr += c * lv1;
            }
            let lv2 = b.lvl2[i];
            if lv2 > 0.0 {
                let (a, c) = osc_unison(
                    t,
                    Morph::new(b.shape2[i]),
                    self.lv2,
                    &mut self.ph2,
                    self.inc2,
                    &cx.uni2,
                    b.uxf2[i],
                );
                sl += a * lv2;
                sr += c * lv2;
            }
            let sub = b.sub[i];
            if sub > 0.0 {
                let m = if cx.sub_square {
                    Morph { k: 2, t: 1.0 }
                } else {
                    Morph { k: 0, t: 0.0 }
                };
                let s = wt_read(t, m, self.lv_sub, self.ph_sub) * sub;
                sl += s;
                sr += s;
            }
            self.ph_sub += self.inc_sub;
            if self.ph_sub >= 1.0 {
                self.ph_sub -= 1.0;
            }
            let nz = b.noise[i];
            if nz > 0.0 {
                let s = self.rng.bipolar() * nz;
                sl += s;
                sr += s;
            }

            // filter
            let oct = b.cut[i]
                + b.keytrack[i] * (self.pitch - 60.0) * (1.0 / 12.0)
                + fenv * b.env_oct[i] * self.vel_flt;
            let fc = oct.exp2().clamp(8.0, cx.max_fc);
            let g = (fc * cx.pi_sr).tan();
            let dg = b.drive_g[i];
            let (xl, xr) = if dg > 1.0e-3 {
                let n = b.drive_n[i];
                (fast_tanh(sl * dg) * n, fast_tanh(sr * dg) * n)
            } else {
                (sl, sr)
            };
            let (yl, yr) = self.filter(b.ftype[i], &b.fw[i], xl, xr, g, b.svf_k[i], b.lad_k[i]);
            let amp = aenv * self.vel_amp * b.trem[i] * fade;
            out_l[i] += yl * amp;
            out_r[i] += yr * amp;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Synth

pub struct Synth {
    cx: Ctx,
    tables: &'static Wavetables,
    p: [f32; NUM_PARAMS],
    voices: [Voice; NUM_VOICES],
    b: Bufs,
    lfo: Lfo,
    /// LFO routing weights (cutoff, pitch, shape, volume): the target's ramps to 1, the others
    /// to 0, so changing the destination crossfades instead of jumping.
    route: [Ramp; 4],
    /// Filter envelope depth (octaves) and key tracking, smoothed like the cutoff.
    s_env: Smooth,
    s_kt: Smooth,
    /// Tuning offsets (semitones), ramped.
    r_off: [Ramp; 2],
    /// Unison layout crossfade positions (0 -> 1).
    uxf: [Ramp; 2],
    /// Filter type and the output weights (LP12, LP24, HP, BP) that crossfade a type switch.
    ftype: u8,
    fw: [Ramp; 4],
    s_shape1: Smooth,
    s_shape2: Smooth,
    s_lvl1: Smooth,
    s_lvl2: Smooth,
    s_sub: Smooth,
    s_noise: Smooth,
    s_cut: Smooth,
    s_res: Smooth,
    s_drive: Smooth,
    s_lfo_amt: Smooth,
    s_master: Smooth,
    res_cache: (f32, f32, f32),
    drive_cache: (f32, f32, f32),
    mode: u8,
    age: u64,
    stack: [u8; 16],
    stack_len: usize,
    last_pitch: Option<f32>,
    last_voice: usize,
    fresh: bool,
    level_l: PeakMeter,
    level_r: PeakMeter,
    lfo_meter: f32,
    scope: [f32; SCOPE_LEN],
    scope_pos: usize,
}

impl Synth {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let tables = wavetable::shared();
        // Highest harmonic allowed: aliases (if any) must land above ~18 kHz.
        let limit = (22_000.0f32).min(((sr - 18_000.0) / 1.26).max(0.5 * sr / 1.26));
        let lam_const = 3.0 * (1024.0 * 440.0 / limit).log2();
        let cx = Ctx {
            sr,
            inv_sr: 1.0 / sr as f64,
            pi_sr: std::f32::consts::PI / sr,
            max_fc: (0.45 * sr).min(22_000.0),
            amp: EnvCfg::default(),
            flt: EnvCfg::default(),
            uni1: UniState::new(Unison::new(1, 0.0, 0.0)),
            uni2: UniState::new(Unison::new(1, 0.0, 0.0)),
            sub_square: false,
            lam_const,
            fade_step: 1.0 / (STEAL_FADE_MS * 0.001 * sr),
            vel_ramp: (VEL_RAMP_MS * 0.001 * sr).round() as u32,
        };
        let sm = |v: f32, ms: f32| Smooth::new(v, ms, sr);
        let xf = |v: f32| Ramp::new(v, (XFADE_MS * 0.001 * sr).round() as u32);
        let mut s = Self {
            cx,
            tables,
            p: [0.0; NUM_PARAMS],
            voices: core::array::from_fn(|i| {
                Voice::new(0x9E37_79B9 ^ (i as u32).wrapping_mul(0x85EB_CA6B).wrapping_add(1))
            }),
            b: Bufs::new(),
            lfo: Lfo {
                phase: 0.0,
                inc: 0.0,
                shape: 0,
                held: 0.0,
                rng: Rng(0x1234_5678),
                out: 0.0,
                coef: 1.0 - (-1.0 / (0.002 * sr)).exp(),
            },
            route: [xf(1.0), xf(0.0), xf(0.0), xf(0.0)],
            s_env: sm(0.0, CUTOFF_MOD_MS),
            s_kt: sm(0.0, CUTOFF_MOD_MS),
            r_off: [xf(0.0); 2],
            uxf: [xf(1.0); 2],
            ftype: 0,
            fw: [xf(1.0), xf(0.0), xf(0.0), xf(0.0)],
            s_shape1: sm(0.0, 8.0),
            s_shape2: sm(0.0, 8.0),
            s_lvl1: sm(0.0, 8.0),
            s_lvl2: sm(0.0, 8.0),
            s_sub: sm(0.0, 8.0),
            s_noise: sm(0.0, 8.0),
            s_cut: sm(0.0, CUTOFF_SMOOTH_MS),
            s_res: sm(0.0, 8.0),
            s_drive: sm(0.0, 8.0),
            s_lfo_amt: sm(0.0, 8.0),
            s_master: sm(0.0, 10.0),
            res_cache: (f32::NAN, 0.0, 0.0),
            drive_cache: (f32::NAN, 0.0, 0.0),
            mode: 0,
            age: 0,
            stack: [0; 16],
            stack_len: 0,
            last_pitch: None,
            last_voice: 0,
            fresh: true,
            level_l: PeakMeter::default(),
            level_r: PeakMeter::default(),
            lfo_meter: 0.0,
            scope: [0.0; SCOPE_LEN],
            scope_pos: 0,
        };
        for (i, spec) in SPEC.iter().enumerate() {
            s.set_param(i as u32, spec.2);
        }
        s.snap_all();
        s
    }

    fn snap_all(&mut self) {
        for sm in [
            &mut self.s_shape1,
            &mut self.s_shape2,
            &mut self.s_lvl1,
            &mut self.s_lvl2,
            &mut self.s_sub,
            &mut self.s_noise,
            &mut self.s_cut,
            &mut self.s_res,
            &mut self.s_drive,
            &mut self.s_lfo_amt,
            &mut self.s_master,
            &mut self.s_env,
            &mut self.s_kt,
        ] {
            sm.snap();
        }
        let ramps = self
            .route
            .iter_mut()
            .chain(&mut self.r_off)
            .chain(&mut self.fw);
        for r in ramps.chain(&mut self.uxf) {
            r.snap();
        }
        self.cx.uni1 = UniState::new(self.cx.uni1.to);
        self.cx.uni2 = UniState::new(self.cx.uni2.to);
    }

    /// Crossfades unison layout `k` (0 = osc 1) to `u`.
    fn set_unison(&mut self, k: usize, u: Unison) {
        let uni = if k == 0 {
            &mut self.cx.uni1
        } else {
            &mut self.cx.uni2
        };
        if u != uni.to {
            uni.retarget(u, self.uxf[k].cur);
            self.uxf[k].restart(0.0, 1.0);
        }
    }

    fn apply(&mut self, i: usize) {
        let v = self.p[i];
        let sr = self.cx.sr;
        match i {
            OSC1_SHAPE => self.s_shape1.set(v),
            OSC2_SHAPE => self.s_shape2.set(v),
            OSC1_LEVEL => self.s_lvl1.set(v),
            OSC2_LEVEL => self.s_lvl2.set(v),
            SUB_LEVEL => self.s_sub.set(v),
            NOISE_LEVEL => self.s_noise.set(v),
            OSC1_OCT | OSC1_SEMI | OSC1_FINE => {
                self.r_off[0]
                    .set(self.p[OSC1_OCT] * 12.0 + self.p[OSC1_SEMI] + self.p[OSC1_FINE] * 0.01);
            }
            OSC2_OCT | OSC2_SEMI | OSC2_FINE => {
                self.r_off[1]
                    .set(self.p[OSC2_OCT] * 12.0 + self.p[OSC2_SEMI] + self.p[OSC2_FINE] * 0.01);
            }
            OSC1_UNI | OSC1_DETUNE | OSC1_SPREAD => {
                let u = Unison::new(
                    self.p[OSC1_UNI] as usize,
                    self.p[OSC1_DETUNE],
                    self.p[OSC1_SPREAD],
                );
                self.set_unison(0, u);
            }
            OSC2_UNI | OSC2_DETUNE | OSC2_SPREAD => {
                let u = Unison::new(
                    self.p[OSC2_UNI] as usize,
                    self.p[OSC2_DETUNE],
                    self.p[OSC2_SPREAD],
                );
                self.set_unison(1, u);
            }
            SUB_SHAPE => self.cx.sub_square = v >= 0.5,
            FILTER_TYPE => {
                self.ftype = v as u8;
                for (k, w) in self.fw.iter_mut().enumerate() {
                    w.set(if k == self.ftype as usize { 1.0 } else { 0.0 });
                }
            }
            CUTOFF => self.s_cut.set(v.log2()),
            RES => self.s_res.set(v),
            DRIVE => self.s_drive.set(v),
            ENV_AMT => self.s_env.set(v * 6.0),
            KEYTRACK => self.s_kt.set(v),
            AMP_A | AMP_D | AMP_S | AMP_R => {
                self.cx.amp = EnvCfg::new(
                    self.p[AMP_A],
                    self.p[AMP_D],
                    self.p[AMP_S],
                    self.p[AMP_R],
                    sr,
                );
                // a shorter attack must not snap a running attack to the top
                if i == AMP_A {
                    for vc in self.voices.iter_mut() {
                        vc.amp.soften();
                    }
                }
            }
            FLT_A | FLT_D | FLT_S | FLT_R => {
                self.cx.flt = EnvCfg::new(
                    self.p[FLT_A],
                    self.p[FLT_D],
                    self.p[FLT_S],
                    self.p[FLT_R],
                    sr,
                );
                if i == FLT_A {
                    for vc in self.voices.iter_mut() {
                        vc.flt.soften();
                    }
                }
            }
            LFO_SHAPE => self.lfo.shape = v as u8,
            LFO_RATE => self.lfo.inc = v as f64 / sr as f64,
            LFO_AMOUNT => self.s_lfo_amt.set(v),
            LFO_TARGET => {
                for (k, r) in self.route.iter_mut().enumerate() {
                    r.set(if k == v as usize { 1.0 } else { 0.0 });
                }
            }
            GLIDE => {}
            VOICE_MODE => {
                let m = v as u8;
                if m != self.mode {
                    self.mode = m;
                    for vc in self.voices.iter_mut() {
                        if vc.active {
                            vc.release();
                        }
                        vc.pending = None;
                    }
                    self.stack_len = 0;
                }
            }
            MASTER => self.s_master.set(db_to_gain(v)),
            _ => {}
        }
        // the mip level depends on the widest unison detune
        if matches!(i, OSC1_UNI | OSC1_DETUNE | OSC2_UNI | OSC2_DETUNE) {
            for vc in self.voices.iter_mut() {
                vc.invalidate_pitch();
            }
        }
    }

    fn fill_bufs(&mut self, n: usize) {
        let b = &mut self.b;
        for i in 0..n {
            let lfo = self.lfo.next();
            let amt = self.s_lfo_amt.next();
            let [r_cut, r_pitch, r_shape, r_vol] = self.route.each_mut().map(|r| r.next());
            let mod_shape = lfo * amt * 1.5 * r_shape;
            b.shape1[i] = (self.s_shape1.next() + mod_shape).clamp(0.0, 3.0);
            b.shape2[i] = (self.s_shape2.next() + mod_shape).clamp(0.0, 3.0);
            b.lvl1[i] = self.s_lvl1.next();
            b.lvl2[i] = self.s_lvl2.next();
            b.sub[i] = self.s_sub.next();
            b.noise[i] = self.s_noise.next();
            b.cut[i] = self.s_cut.next() + lfo * amt * 4.0 * r_cut;
            b.pitch[i] = lfo * 12.0 * amt * amt * r_pitch;
            b.trem[i] = 1.0 - amt * r_vol * (0.5 - 0.5 * lfo);
            b.env_oct[i] = self.s_env.next();
            b.keytrack[i] = self.s_kt.next();
            b.off1[i] = self.r_off[0].next();
            b.off2[i] = self.r_off[1].next();
            b.uxf1[i] = self.uxf[0].next();
            b.uxf2[i] = self.uxf[1].next();
            b.fw[i] = self.fw.each_mut().map(|w| w.next());
            b.ftype[i] = if self.fw.iter().all(Ramp::settled) {
                self.ftype
            } else {
                FILTER_XF
            };
            let res = self.s_res.next();
            if res != self.res_cache.0 {
                let q = 0.707 * 35f32.powf(res);
                self.res_cache = (res, 1.0 / q, 4.1 * res);
            }
            b.svf_k[i] = self.res_cache.1;
            b.lad_k[i] = self.res_cache.2;
            let drive = self.s_drive.next();
            if drive != self.drive_cache.0 {
                let g = 10.0 * drive * drive;
                let n = if g > 1.0e-3 { 1.0 / fast_tanh(g) } else { 1.0 };
                self.drive_cache = (drive, g, n);
            }
            b.drive_g[i] = self.drive_cache.1;
            b.drive_n[i] = self.drive_cache.2;
            b.master[i] = self.s_master.next();
            self.lfo_meter = lfo;
        }
    }

    fn render(&mut self, start: usize, end: usize, l: &mut [f32], r: &mut [f32]) {
        let Synth {
            cx,
            tables,
            voices,
            b,
            age,
            ..
        } = self;
        for v in voices.iter_mut() {
            if v.active {
                v.render(cx, b, tables, start, end, l, r, *age);
            }
        }
    }

    fn next_age(&mut self) -> u64 {
        self.age += 1;
        self.age
    }

    fn glide_ms(&self) -> f32 {
        self.p[GLIDE]
    }

    fn slide_ms(&self) -> f32 {
        if self.p[GLIDE] > 0.0 {
            self.p[GLIDE]
        } else {
            DEFAULT_SLIDE_MS
        }
    }

    fn handle(&mut self, e: NoteEvent) {
        let key = e.key.min(127);
        if e.on && e.velocity > 0.0 {
            if self.mode == 0 {
                self.poly_on(key, e.velocity, e.slide);
            } else {
                self.mono_on(key, e.velocity, e.slide);
            }
            self.last_pitch = Some(key as f32);
        } else if self.mode == 0 {
            self.poly_off(key);
        } else {
            self.mono_off(key);
        }
    }

    fn poly_on(&mut self, key: u8, vel: f32, slide: bool) {
        let age = self.next_age();
        let sr = self.cx.sr;
        if slide {
            // FL-style: the most recent held note glides to the new key and becomes it.
            let newest = (0..NUM_VOICES)
                .filter(|&i| self.voices[i].active && self.voices[i].gate && !self.voices[i].fading)
                .max_by_key(|&i| self.voices[i].age);
            if let Some(i) = newest {
                let ms = self.slide_ms();
                let v = &mut self.voices[i];
                v.glide_to(key as f32, ms, sr);
                v.key = key;
                v.age = age;
                self.last_voice = i;
                return;
            }
        }
        let from = if slide { self.last_pitch } else { None };
        let pend = Pending {
            key,
            vel,
            gate: true,
            from,
            glide_ms: self.slide_ms(),
        };
        // Re-striking a key that still sounds: with slow attacks its envelopes go on from where
        // they are (as before; the new velocity ramps in). With fast attacks, restarting it in
        // place would jump its level mid-cycle, so the old note rings out (released if still
        // held, cancelled if it is still waiting for a stolen voice) and the new one gets a voice
        // of its own, with the same attack as any note.
        if !slide
            && self.retrigger_in_place()
            && let Some(i) = (0..NUM_VOICES)
                .filter(|&i| {
                    let v = &self.voices[i];
                    v.active && !v.fading && v.key == key
                })
                .max_by_key(|&i| self.voices[i].age)
        {
            let cx = self.cx;
            self.voices[i].retrigger(key, vel, age, &cx);
            self.last_voice = i;
            return;
        }
        for v in self.voices.iter_mut() {
            if v.active && v.gate && !v.fading && v.key == key {
                v.release();
            }
            if let Some(p) = v.pending.as_mut().filter(|p| p.key == key) {
                p.gate = false;
            }
        }
        if let Some(i) = (0..NUM_VOICES).find(|&i| !self.voices[i].active) {
            let cx = self.cx;
            self.voices[i].start(pend, &cx, age);
            self.last_voice = i;
            return;
        }
        // steal: a voice already fading (replace its pending note), else the quietest released
        // voice, else the oldest held one
        let i = (0..NUM_VOICES)
            .find(|&i| self.voices[i].fading)
            .or_else(|| {
                (0..NUM_VOICES)
                    .filter(|&i| !self.voices[i].gate)
                    .min_by(|&a, &b| self.voices[a].amp.v.total_cmp(&self.voices[b].amp.v))
            })
            .unwrap_or_else(|| {
                (0..NUM_VOICES)
                    .min_by_key(|&i| self.voices[i].age)
                    .unwrap_or(0)
            });
        let v = &mut self.voices[i];
        if !v.fading {
            v.fading = true;
            v.fade = 1.0;
        }
        v.gate = false;
        v.pending = Some(pend);
        v.age = age;
        self.last_voice = i;
    }

    fn poly_off(&mut self, key: u8) {
        for v in self.voices.iter_mut() {
            if let Some(p) = v.pending.as_mut().filter(|p| p.key == key) {
                p.gate = false;
            }
            if v.active && v.gate && !v.fading && v.key == key {
                v.release();
            }
        }
    }

    fn stack_remove(&mut self, key: u8) {
        if let Some(pos) = self.stack[..self.stack_len].iter().position(|&k| k == key) {
            self.stack.copy_within(pos + 1..self.stack_len, pos);
            self.stack_len -= 1;
        }
    }

    fn mono_on(&mut self, key: u8, vel: f32, slide: bool) {
        self.stack_remove(key);
        if self.stack_len == self.stack.len() {
            self.stack.copy_within(1.., 0);
            self.stack_len -= 1;
        }
        self.stack[self.stack_len] = key;
        self.stack_len += 1;

        let age = self.next_age();
        let legato = self.mode == 2;
        let glide = self.glide_ms();
        let slide_ms = self.slide_ms();
        let last = self.last_pitch;
        let in_place = self.retrigger_in_place();
        let cx = self.cx;
        self.last_voice = 0;
        let v = &self.voices[0];
        let sounding = v.active && !v.fading;
        let held = sounding && v.gate;
        let ms = if slide { slide_ms } else { glide };
        if held && (legato || slide) {
            // legato / slide: the held note moves to the new key without a new attack
            let v = &mut self.voices[0];
            v.glide_to(key as f32, ms, cx.sr);
            v.key = key;
            v.age = age;
            return;
        }
        // A retrigger glides from a held note; after a release, legato mode only glides on
        // slide notes.
        let glide_now = slide || (!legato && glide > 0.0);
        let ms = if held || glide_now { ms } else { 0.0 };
        if sounding && in_place {
            // slow attacks: the envelopes go on from where they are (no dip, no click)
            let v = &mut self.voices[0];
            v.retrigger(key, vel, age, &cx);
            v.glide_to(key as f32, ms, cx.sr);
            return;
        }
        let from = if sounding {
            (ms > 0.0).then_some(v.pitch)
        } else if glide_now {
            last
        } else {
            None
        };
        let pend = Pending {
            key,
            vel,
            gate: true,
            from,
            glide_ms: ms,
        };
        // Fast attacks: the note starts from scratch, with the same attack as a note from
        // silence (restarting a sounding voice's attack mid-cycle is a click), while the old note
        // fades out in a slot of its own. With no free slot it fades out first, like a steal.
        if sounding && !self.hand_off_mono() {
            let v = &mut self.voices[0];
            v.fading = true;
            v.fade = 1.0;
            v.gate = false;
            v.pending = Some(pend);
            return;
        }
        let v = &mut self.voices[0];
        if v.active && v.fading {
            v.pending = Some(pend);
        } else {
            v.start(pend, &cx, age);
        }
    }

    /// Whether a retrigger may restart the sounding voice's envelopes in place: the amp attack,
    /// and the filter attack when it moves the cutoff, are slow enough (IN_PLACE_ATTACK_MS).
    fn retrigger_in_place(&self) -> bool {
        self.p[AMP_A] >= IN_PLACE_ATTACK_MS
            && (self.p[FLT_A] >= IN_PLACE_ATTACK_MS || self.p[ENV_AMT] == 0.0)
    }

    /// Moves the sounding mono voice to a free slot, where it fades out over STEAL_FADE_MS, so
    /// the mono voice can start the next note at once. False if no slot is free.
    fn hand_off_mono(&mut self) -> bool {
        let Some(j) = (1..NUM_VOICES).find(|&j| !self.voices[j].active) else {
            return false;
        };
        // (the copy keeps its own noise generator, so the two notes' noise is not correlated)
        let rng = self.voices[j].rng;
        self.voices[j] = Voice {
            gate: false,
            fading: true,
            fade: 1.0,
            pending: None,
            rng,
            ..self.voices[0]
        };
        true
    }

    fn mono_off(&mut self, key: u8) {
        self.stack_remove(key);
        let glide = self.glide_ms();
        let sr = self.cx.sr;
        let top = if self.stack_len > 0 {
            Some(self.stack[self.stack_len - 1])
        } else {
            None
        };
        let v = &mut self.voices[0];
        if let Some(p) = v.pending.as_mut().filter(|p| p.key == key) {
            p.gate = false;
        }
        if v.active && v.gate && v.key == key {
            match top {
                Some(k) => {
                    v.glide_to(k as f32, glide, sr);
                    v.key = k;
                }
                None => v.release(),
            }
        }
    }

    fn post(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        for i in 0..n {
            let g = self.b.master[i];
            let a = sane(l[i] * g);
            let c = sane(r[i] * g);
            l[i] = a;
            r[i] = c;
            self.scope[self.scope_pos] = mid(a, c);
            self.scope_pos = (self.scope_pos + 1) % SCOPE_LEN;
        }
        self.level_l.update(peak_of(l), n, self.cx.sr);
        self.level_r.update(peak_of(r), n, self.cx.sr);
    }
}

impl Device for Synth {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(mut v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        let i = index as usize;
        if is_stepped(i) {
            v = v.round();
        }
        self.p[i] = v;
        self.apply(i);
        // Before the first block, or while no voice sounds, there is nothing to declick: the
        // change takes effect at once (a preset loaded between notes must not ramp in).
        if self.fresh || self.voices.iter().all(|v| !v.active) {
            self.snap_all();
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sidechain: Option<(&[f32], &[f32])>,
        events: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        self.fresh = false;
        let mut base = 0;
        let mut ei = 0;
        while base < n {
            let len = (n - base).min(MAX_BLOCK);
            let last_chunk = base + len >= n;
            let cl = &mut l[base..base + len];
            let cr = &mut r[base..base + len];
            cl.fill(0.0);
            cr.fill(0.0);
            self.fill_bufs(len);
            let mut pos = 0;
            while ei < events.len() {
                let off = events[ei].offset as usize;
                if off >= base + len && !last_chunk {
                    break;
                }
                let local = off.saturating_sub(base).min(len);
                if local > pos {
                    self.render(pos, local, cl, cr);
                    pos = local;
                }
                self.handle(events[ei]);
                ei += 1;
            }
            if pos < len {
                self.render(pos, len, cl, cr);
            }
            self.post(cl, cr);
            base += len;
        }
        while ei < events.len() {
            self.handle(events[ei]);
            ei += 1;
        }
    }

    fn reset(&mut self) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            *v = Voice::new(0x9E37_79B9 ^ (i as u32).wrapping_mul(0x85EB_CA6B).wrapping_add(1));
        }
        self.stack_len = 0;
        self.last_pitch = None;
        self.lfo.phase = 0.0;
        self.lfo.out = 0.0;
        self.snap_all();
        self.level_l = PeakMeter::default();
        self.level_r = PeakMeter::default();
        self.scope = [0.0; SCOPE_LEN];
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        let v = &self.voices[self.last_voice];
        match index {
            0 => self.level_l.value,
            1 => self.level_r.value,
            2 => self.voices.iter().filter(|v| v.active).count() as f32,
            3 => self.lfo_meter,
            4 if v.active => v.amp.v as f32,
            5 if v.active => v.flt.v as f32,
            _ => 0.0,
        }
    }

    fn meter_array(&self, index: u32, out: &mut [f32]) -> usize {
        if index != 6 {
            return 0;
        }
        let n = out.len().min(SCOPE_LEN);
        for (j, o) in out.iter_mut().take(n).enumerate() {
            *o = self.scope[(self.scope_pos + SCOPE_LEN - n + j) % SCOPE_LEN];
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Ev {
        On(u8, f32, bool),
        Off(u8),
        Param(usize, f32),
    }

    /// A busy 0.8 s: poly re-strikes and steals, mono hand-offs and in-place retriggers, legato
    /// slides, and knob moves (some during a running crossfade) at frames that line up with no
    /// block size.
    fn script() -> Vec<(usize, Ev)> {
        let mut ev = vec![(0, Ev::Param(VOICE_MODE, 0.0))];
        for k in 0..24 {
            let t = 101 + k * 503;
            ev.push((
                t,
                Ev::On(40 + (k % 9) as u8 * 3, 0.3 + 0.03 * k as f32, false),
            ));
            ev.push((t + 1777, Ev::Off(40 + (k % 9) as u8 * 3)));
        }
        for (t, i, v) in [
            (2003, FILTER_TYPE, 1.0),
            (2111, FILTER_TYPE, 2.0),
            (2190, FILTER_TYPE, 3.0),
            (3001, OSC1_UNI, 7.0),
            (3077, OSC1_UNI, 2.0),
            (3333, LFO_AMOUNT, 0.7),
            (3500, LFO_TARGET, 3.0),
            (3561, LFO_TARGET, 1.0),
            (4003, ENV_AMT, -0.8),
            (4400, KEYTRACK, 1.0),
            (4999, OSC2_OCT, 2.0),
            (5013, OSC1_SEMI, -7.0),
            (6007, AMP_A, 900.0),
            (6050, AMP_A, 0.5),
            (7001, FILTER_TYPE, 0.0),
            (7003, OSC2_UNI, 5.0),
            (9001, OSC1_UNI, 6.0),
        ] {
            ev.push((t, Ev::Param(i, v)));
        }
        ev.push((14_001, Ev::Param(VOICE_MODE, 1.0)));
        for k in 0..10 {
            let t = 14_100 + k * 1301;
            ev.push((
                t,
                Ev::On(50 + (k % 4) as u8 * 5, 0.25 + 0.07 * k as f32, false),
            ));
            ev.push((t + 1500 + (k % 3) * 400, Ev::Off(50 + (k % 4) as u8 * 5)));
        }
        // slow attacks: mono retriggers in place
        ev.push((22_000, Ev::Param(AMP_A, 30.0)));
        ev.push((22_000, Ev::Param(FLT_A, 30.0)));
        for k in 0..5 {
            let t = 22_101 + k * 997;
            ev.push((t, Ev::On(60 + k as u8, 1.0 - 0.15 * k as f32, false)));
            ev.push((t + 1200, Ev::Off(60 + k as u8)));
        }
        ev.push((28_003, Ev::Param(VOICE_MODE, 2.0)));
        ev.push((28_003, Ev::Param(GLIDE, 40.0)));
        ev.push((28_100, Ev::On(45, 0.9, false)));
        ev.push((29_377, Ev::On(52, 0.6, true)));
        ev.push((30_001, Ev::Off(45)));
        ev.push((31_555, Ev::On(57, 0.8, false)));
        ev.push((32_900, Ev::Off(52)));
        ev.push((33_000, Ev::Off(57)));
        ev.push((33_700, Ev::On(57, 0.4, false)));
        ev.push((36_000, Ev::Off(57)));
        ev.sort_by_key(|e| e.0);
        ev
    }

    fn render(blocks: &mut dyn FnMut() -> usize) -> (Vec<f32>, Vec<f32>) {
        let mut s = Synth::new(48_000.0);
        for (i, v) in preset("Pluck").unwrap() {
            s.set_param(i, v);
        }
        s.set_param(NOISE_LEVEL as u32, 0.05);
        let ev = script();
        let params: Vec<(usize, usize, f32)> = ev
            .iter()
            .filter_map(|&(t, x)| match x {
                Ev::Param(p, v) => Some((t, p, v)),
                _ => None,
            })
            .collect();
        let notes: Vec<(usize, NoteEvent)> = ev
            .iter()
            .filter_map(|&(t, x)| {
                let (key, velocity, on, slide) = match x {
                    Ev::On(key, vel, slide) => (key, vel, true, slide),
                    Ev::Off(key) => (key, 0.0, false, false),
                    Ev::Param(..) => return None,
                };
                Some((
                    t,
                    NoteEvent {
                        offset: 0,
                        key,
                        velocity,
                        on,
                        slide,
                    },
                ))
            })
            .collect();
        let n = 40_000;
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        let (mut i, mut pi) = (0, 0);
        while i < n {
            while pi < params.len() && params[pi].0 <= i {
                s.set_param(params[pi].1 as u32, params[pi].2);
                pi += 1;
            }
            // blocks split where a parameter moves (it applies between blocks)
            let mut e = (i + blocks().clamp(1, MAX_BLOCK)).min(n);
            if pi < params.len() {
                e = e.min(params[pi].0);
            }
            let block: Vec<NoteEvent> = notes
                .iter()
                .filter(|(t, _)| *t >= i && *t < e)
                .map(|&(t, x)| NoteEvent {
                    offset: (t - i) as u32,
                    ..x
                })
                .collect();
            s.process(&mut l[i..e], &mut r[i..e], None, &block);
            i = e;
        }
        (l, r)
    }

    /// A preset loaded while the engine runs but no note sounds takes effect at once: the first
    /// note after it sounds exactly as on a synth that had the preset from the start. (These
    /// presets leave the LFO rate and shape at their defaults, so its phase matches too.)
    #[test]
    fn a_preset_loaded_while_silent_does_not_ramp_in() {
        for name in ["Pluck", "Bell Keys", "808 Glide"] {
            let silent = 4_800;
            let render = |load_first: bool| {
                let mut s = Synth::new(48_000.0);
                let load = |s: &mut Synth| {
                    for (i, v) in preset(name).unwrap() {
                        s.set_param(i, v);
                    }
                };
                if load_first {
                    load(&mut s);
                }
                let (mut l, mut r) = (vec![0.0f32; silent + 24_000], vec![0.0f32; silent + 24_000]);
                let mut c = 0;
                while c < l.len() {
                    if c == silent && !load_first {
                        load(&mut s);
                    }
                    let e = (c + MAX_BLOCK).min(l.len()).min(if c < silent {
                        silent
                    } else {
                        usize::MAX
                    });
                    let on = NoteEvent {
                        offset: 0,
                        key: 48,
                        velocity: 0.9,
                        on: true,
                        slide: false,
                    };
                    let ev: &[NoteEvent] = if c == silent { &[on] } else { &[] };
                    s.process(&mut l[c..e], &mut r[c..e], None, ev);
                    c = e;
                }
                (l, r)
            };
            let (a, b) = (render(true), render(false));
            assert!(
                a.0[..4_800].iter().all(|v| *v == 0.0),
                "{name}: not silent before the note"
            );
            let diff = (0..a.0.len())
                .map(|i| (a.0[i] - b.0[i]).abs().max((a.1[i] - b.1[i]).abs()))
                .fold(0.0f32, f32::max);
            assert!(diff == 0.0, "{name}: loaded while silent differs by {diff}");
        }
    }

    #[test]
    fn output_is_independent_of_block_size() {
        let reference = render(&mut || MAX_BLOCK);
        assert!(reference.0.iter().any(|v| v.abs() > 0.01), "silent render");
        let mut seed = 0x2545_F491u32;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            1 + seed as usize % MAX_BLOCK
        };
        for (label, out) in [
            ("37", render(&mut || 37)),
            ("1", render(&mut || 1)),
            ("random", render(&mut random)),
        ] {
            let diff = (0..reference.0.len())
                .map(|i| {
                    (out.0[i] - reference.0[i])
                        .abs()
                        .max((out.1[i] - reference.1[i]).abs())
                })
                .fold(0.0f32, f32::max);
            assert!(
                diff == 0.0,
                "blocks of {label} frames differ from 128 by {diff}"
            );
        }
    }
}
