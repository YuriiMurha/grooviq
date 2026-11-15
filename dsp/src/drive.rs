//! Drive: saturator / clipper with five curves and 1x/2x/4x oversampling.
//!
//! Oversampling uses linear-phase halfband FIRs (Kaiser windowed sinc) in polyphase form:
//! 87 taps for the 1x<->2x stage (~80 dB stopband, passband to ~0.43 Fs), 23 taps for the
//! 2x<->4x stage (its transition band can be very wide). Because the filters are linear phase
//! the dry signal is simply delayed by the same integer latency (2x: 43, 4x: 49 samples), so the
//! dry/wet mix never comb-filters.
//!
//! Curves: hard clip, soft clip (tanh), tape (biased tanh = asymmetric, plus drive-dependent HF
//! loss), tube (tanh plus a bounded even-order term, strong 2nd harmonic), fold (sine wavefolder).
//! A DC blocker follows (asymmetric curves create DC), then a tilt EQ around 700 Hz.

use crate::api::{Device, NoteEvent, db_to_gain};
use crate::util::*;

/// `(min, max, default)` per parameter, devices.json order.
pub const SPEC: [(f32, f32, f32); 6] = [
    (0.0, 4.0, 1.0),   // 0 mode
    (0.0, 36.0, 6.0),  // 1 drive dB
    (-1.0, 1.0, 0.0),  // 2 tone
    (0.0, 1.0, 1.0),   // 3 mix
    (-24.0, 6.0, 0.0), // 4 output dB
    (0.0, 2.0, 1.0),   // 5 oversampling
];

const KA: usize = 22; // stage A: 4K-1 = 87 taps
const KB: usize = 6; // stage B: 23 taps
const LAT_2X: usize = 2 * KA - 1; // 43
const LAT_4X: usize = 2 * KA - 1 + KB; // 43 + 5.5 + 0.5 (one 2x-rate sample) = 49

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Causal halfband impulse response of length 4K-1 (centre tap 0.5, even offsets zero).
fn halfband(k: usize, beta: f64) -> Vec<f64> {
    let taps = 4 * k - 1;
    let m = (taps - 1) / 2;
    let mut h = vec![0.0; taps];
    let i0b = bessel_i0(beta);
    for (i, hv) in h.iter_mut().enumerate() {
        let n = i as i64 - m as i64;
        *hv = if n == 0 {
            0.5
        } else if n % 2 == 0 {
            0.0
        } else {
            let x = n as f64 / m as f64;
            let w = bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / i0b;
            (std::f64::consts::FRAC_PI_2 * n as f64).sin() / (std::f64::consts::PI * n as f64) * w
        };
    }
    // make the polyphase branch sum exactly 0.5 so DC gain is exactly 1
    let odd: f64 = h
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, v)| v)
        .sum();
    for (i, hv) in h.iter_mut().enumerate() {
        if i % 2 == 0 {
            *hv *= 0.5 / odd;
        }
    }
    h
}

/// History buffer with doubled storage so the newest `len` samples are one contiguous slice.
#[derive(Clone)]
struct Hist {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
}

impl Hist {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; 2 * len],
            len,
            pos: 0,
        }
    }
    #[inline(always)]
    fn push(&mut self, x: f32) {
        self.pos = if self.pos == 0 {
            self.len - 1
        } else {
            self.pos - 1
        };
        self.buf[self.pos] = x;
        self.buf[self.pos + self.len] = x;
    }
    /// s[0] = newest, s[j] = j samples ago.
    #[inline(always)]
    fn slice(&self) -> &[f32] {
        &self.buf[self.pos..self.pos + self.len]
    }
    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

#[inline(always)]
fn dot(c: &[f32], s: &[f32]) -> f32 {
    c.iter().zip(s).map(|(a, b)| a * b).sum()
}

/// Polyphase 2x upsampler: one input -> two outputs.
#[derive(Clone)]
struct Up {
    h: Hist,
    c: Vec<f32>,
    k: usize,
}

impl Up {
    fn new(k: usize, beta: f64) -> Self {
        let h = halfband(k, beta);
        Self {
            h: Hist::new(2 * k),
            c: (0..2 * k).map(|j| (2.0 * h[2 * j]) as f32).collect(),
            k,
        }
    }
    #[inline(always)]
    fn run(&mut self, x: f32) -> (f32, f32) {
        self.h.push(x);
        let s = self.h.slice();
        (dot(&self.c, s), s[self.k - 1])
    }
}

/// Polyphase 2x downsampler: two inputs -> one output.
#[derive(Clone)]
struct Down {
    e: Hist,
    o: Hist,
    c: Vec<f32>,
    k: usize,
}

impl Down {
    fn new(k: usize, beta: f64) -> Self {
        let h = halfband(k, beta);
        Self {
            e: Hist::new(2 * k),
            o: Hist::new(k + 1),
            c: (0..2 * k).map(|j| h[2 * j] as f32).collect(),
            k,
        }
    }
    #[inline(always)]
    fn run(&mut self, a: f32, b: f32) -> f32 {
        self.e.push(a);
        self.o.push(b);
        dot(&self.c, self.e.slice()) + 0.5 * self.o.slice()[self.k]
    }
}

const TAPE_BIAS: f32 = 0.15;

#[inline(always)]
fn shape(mode: u8, x: f32) -> f32 {
    match mode {
        0 => x.clamp(-1.0, 1.0),
        1 => fast_tanh(x),
        2 => {
            // tanh(0.15) and 1 / sech^2(0.15): unity small-signal gain, asymmetric clipping
            const TB: f32 = 0.148_885_16;
            const NORM: f32 = 1.022_664_5;
            (fast_tanh(x + TAPE_BIAS) - TB) * NORM
        }
        3 => {
            let t = fast_tanh(x);
            t + 0.22 * t * t
        }
        _ => (x * std::f32::consts::FRAC_PI_2).sin(),
    }
}

struct Chan {
    up_a: Up,
    down_a: Down,
    up_b: Up,
    down_b: Down,
    z2x: f32,
    dry: DelayLine,
    dc: DcBlock,
    tape: OnePole,
    tilt: OnePole,
}

impl Chan {
    fn new() -> Self {
        Self {
            up_a: Up::new(KA, 8.6),
            down_a: Down::new(KA, 8.6),
            up_b: Up::new(KB, 9.0),
            down_b: Down::new(KB, 9.0),
            z2x: 0.0,
            dry: DelayLine::new(LAT_4X + 1),
            dc: DcBlock::default(),
            tape: OnePole::default(),
            tilt: OnePole::default(),
        }
    }
    fn clear(&mut self) {
        for h in [
            &mut self.up_a.h,
            &mut self.up_b.h,
            &mut self.down_a.e,
            &mut self.down_a.o,
            &mut self.down_b.e,
            &mut self.down_b.o,
        ] {
            h.clear();
        }
        self.z2x = 0.0;
        self.dry.clear();
        self.dc.reset();
        self.tape.reset();
        self.tilt.reset();
    }

    /// Non-linear stage at the selected oversampling factor (input already gained).
    #[inline(always)]
    fn nonlinear(&mut self, mode: u8, os: u8, x: f32) -> f32 {
        match os {
            0 => shape(mode, x),
            1 => {
                let (a, b) = self.up_a.run(x);
                self.down_a.run(shape(mode, a), shape(mode, b))
            }
            _ => {
                let (a, b) = self.up_a.run(x);
                let (a1, a2) = self.up_b.run(a);
                let (b1, b2) = self.up_b.run(b);
                let ya = self.down_b.run(shape(mode, a1), shape(mode, a2));
                let yb = self.down_b.run(shape(mode, b1), shape(mode, b2));
                // one 2x-rate sample of delay makes the total latency an integer (49)
                let prev = self.z2x;
                self.z2x = yb;
                self.down_a.run(prev, ya)
            }
        }
    }
}

pub struct Drive {
    sr: f32,
    mode: u8,
    os: u8,
    drive: Smooth,
    lo_g: Smooth,
    hi_g: Smooth,
    mix: Smooth,
    out: Smooth,
    tape_g: f32,
    tilt_g: f32,
    dc_r: f32,
    ch: [Chan; 2],
    fresh: bool,
    in_m: PeakMeter,
    out_m: PeakMeter,
}

impl Drive {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let sm = |v: f32| Smooth::new(v, 10.0, sr);
        let mut d = Self {
            sr,
            mode: 1,
            os: 1,
            drive: sm(1.0),
            lo_g: sm(1.0),
            hi_g: sm(1.0),
            mix: sm(1.0),
            out: sm(1.0),
            tape_g: onepole_gain(14_000.0f32.min(0.45 * sr), sr),
            tilt_g: onepole_gain(700.0, sr),
            dc_r: dc_coef(5.0, sr),
            ch: [Chan::new(), Chan::new()],
            fresh: true,
            in_m: PeakMeter::default(),
            out_m: PeakMeter::default(),
        };
        for (i, s) in SPEC.iter().enumerate() {
            d.set_param(i as u32, s.2);
        }
        d
    }

    /// Latency of the wet path in samples for the current oversampling setting.
    pub fn latency(&self) -> usize {
        match self.os {
            0 => 0,
            1 => LAT_2X,
            _ => LAT_4X,
        }
    }

    fn snap_all(&mut self) {
        for s in [
            &mut self.drive,
            &mut self.lo_g,
            &mut self.hi_g,
            &mut self.mix,
            &mut self.out,
        ] {
            s.snap();
        }
    }
}

impl Device for Drive {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.mode = v.round() as u8,
            1 => {
                self.drive.set(db_to_gain(v));
                // tape: more drive, more high-frequency loss (16 kHz -> 8 kHz)
                let fc = (16_000.0 - 8_000.0 * v / 36.0).min(0.45 * self.sr);
                self.tape_g = onepole_gain(fc, self.sr);
            }
            2 => {
                self.lo_g.set(db_to_gain(-6.0 * v));
                self.hi_g.set(db_to_gain(6.0 * v));
            }
            3 => self.mix.set(v),
            4 => self.out.set(db_to_gain(v)),
            _ => {
                let os = v.round() as u8;
                if os != self.os {
                    self.os = os;
                    for c in self.ch.iter_mut() {
                        c.clear();
                    }
                }
            }
        }
        if self.fresh {
            self.snap_all();
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _sc: Option<(&[f32], &[f32])>,
        _ev: &[NoteEvent],
    ) {
        let n = l.len().min(r.len());
        self.fresh = false;
        let lat = self.latency();
        let mut pin = 0.0f32;
        for i in 0..n {
            let g = self.drive.next();
            let lo = self.lo_g.next();
            let hi = self.hi_g.next();
            let mix = self.mix.next();
            let og = self.out.next();
            for (c, x) in [(0usize, &mut l[i]), (1usize, &mut r[i])] {
                let xin = sane(*x);
                pin = pin.max(xin.abs());
                let ch = &mut self.ch[c];
                let mut y = ch.nonlinear(self.mode, self.os, xin * g);
                if self.mode == 2 {
                    y = ch.tape.lp(y, self.tape_g);
                }
                y = ch.dc.run(y, self.dc_r);
                let low = ch.tilt.lp(y, self.tilt_g);
                y = low * lo + (y - low) * hi;
                ch.dry.push(xin);
                let dry = ch.dry.read_int(lat);
                *x = (dry + (y - dry) * mix) * og;
            }
        }
        self.in_m.update(pin, n, self.sr);
        self.out_m
            .update(peak_of(&l[..n]).max(peak_of(&r[..n])), n, self.sr);
    }

    fn reset(&mut self) {
        for c in self.ch.iter_mut() {
            c.clear();
        }
        self.snap_all();
        self.in_m = PeakMeter::default();
        self.out_m = PeakMeter::default();
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.in_m.db(),
            1 => self.out_m.db(),
            _ => 0.0,
        }
    }
}
