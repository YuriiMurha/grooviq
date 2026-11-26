//! Lookahead brickwall limiter (~1.5 ms).
//!
//! For each input sample the gain that would put it exactly at the ceiling is computed; a
//! sliding-window minimum over the lookahead window (monotonic deque, O(1)) holds it, an
//! exponential release lets it recover, and two cascaded box filters whose combined length fits
//! the window turn the hold into a smooth S-shaped attack ramp. Because every averaged value is
//! at most the gain required by the sample leaving the delay line, the output provably never
//! exceeds the ceiling (a final clamp only guards against float rounding).
//! Latency: lookahead - 1 samples.

use crate::api::{Device, NoteEvent, db_to_gain, gain_to_db};
use crate::util::*;

/// `(min, max, default)` per parameter, `SPEC` index order.
pub const SPEC: [(f32, f32, f32); 3] = [
    (0.0, 24.0, 0.0),    // 0 input gain dB
    (-12.0, 0.0, -0.3),  // 1 ceiling dB
    (1.0, 1000.0, 60.0), // 2 release ms
];

/// Stable parameter ids in `SPEC` order.
pub const PARAM_IDS: [&str; 3] = ["input_gain", "ceiling", "release"];

fn lookahead_frames(sr: f32) -> usize {
    (libm::roundf(0.0015 * sr) as usize).max(2)
}

pub(crate) fn max_latency_frames(sample_rate: f32) -> u32 {
    (lookahead_frames(sample_rate.clamp(8000.0, 384_000.0)) - 1) as u32
}

struct BoxFilter {
    buf: Vec<f32>,
    pos: usize,
    sum: f64,
}

impl BoxFilter {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![1.0; len.max(1)],
            pos: 0,
            sum: len.max(1) as f64,
        }
    }
    #[inline(always)]
    fn run(&mut self, x: f32) -> f32 {
        self.sum += x as f64 - self.buf[self.pos] as f64;
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
            // re-sum once per lap so float drift can never accumulate
            self.sum = self.buf.iter().map(|&v| v as f64).sum();
        }
        (self.sum / self.buf.len() as f64) as f32
    }
    fn reset(&mut self) {
        self.buf.fill(1.0);
        self.pos = 0;
        self.sum = self.buf.len() as f64;
    }
}

pub struct Limiter {
    sr: f32,
    look: usize,
    gain: Smooth,
    ceil: f32,
    rel: f32,
    // audio delay (look - 1 samples)
    dl: Vec<f32>,
    dr: Vec<f32>,
    dpos: usize,
    // sliding minimum
    q_val: Vec<f32>,
    q_idx: Vec<u64>,
    q_head: usize,
    q_len: usize,
    n: u64,
    env: f32,
    box1: BoxFilter,
    box2: BoxFilter,
    fresh: bool,
    in_m: PeakMeter,
    out_m: PeakMeter,
    gr_m: f32,
}

fn filter_lengths(look: usize) -> (usize, usize) {
    let l1 = (look + 2) / 2;
    (l1, look + 1 - l1)
}

pub(crate) fn memory_payload(sr: f32) -> crate::insert_memory::Payload {
    let mut payload = crate::insert_memory::Payload::boxed::<Limiter>();
    let look = lookahead_frames(sr);
    let (l1, l2) = filter_lengths(look);
    payload.array::<f32>(look - 1, 2); // Stereo lookahead.
    payload.array::<f32>(look + 1, 1); // Sliding minimum values.
    payload.array::<u64>(look + 1, 1); // Sliding minimum indices.
    for filter in [l1, l2] {
        payload.array::<f32>(filter.max(1), 1);
    }
    payload
}

impl Limiter {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let look = lookahead_frames(sr);
        let (l1, l2) = filter_lengths(look);
        let mut s = Self {
            sr,
            look,
            gain: Smooth::new(1.0, 10.0, sr),
            ceil: db_to_gain(-0.3),
            rel: 0.0,
            dl: vec![0.0; look - 1],
            dr: vec![0.0; look - 1],
            dpos: 0,
            q_val: vec![1.0; look + 1],
            q_idx: vec![0; look + 1],
            q_head: 0,
            q_len: 0,
            n: 0,
            env: 1.0,
            box1: BoxFilter::new(l1),
            box2: BoxFilter::new(l2),
            fresh: true,
            in_m: PeakMeter::default(),
            out_m: PeakMeter::default(),
            gr_m: 0.0,
        };
        for (i, p) in SPEC.iter().enumerate() {
            s.set_param(i as u32, p.2);
        }
        s
    }

    /// Latency in samples.
    pub fn latency(&self) -> usize {
        self.look - 1
    }

    #[inline(always)]
    fn window_min(&mut self, v: f32) -> f32 {
        let cap = self.q_val.len();
        while self.q_len > 0 {
            let back = (self.q_head + self.q_len - 1) % cap;
            if self.q_val[back] >= v {
                self.q_len -= 1;
            } else {
                break;
            }
        }
        let pos = (self.q_head + self.q_len) % cap;
        self.q_val[pos] = v;
        self.q_idx[pos] = self.n;
        self.q_len += 1;
        while self.q_idx[self.q_head] + self.look as u64 <= self.n {
            self.q_head = (self.q_head + 1) % cap;
            self.q_len -= 1;
        }
        self.n += 1;
        self.q_val[self.q_head]
    }
}

impl Device for Limiter {
    fn latency_frames(&self) -> u32 {
        self.latency() as u32
    }

    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.gain.set(db_to_gain(v)),
            1 => self.ceil = db_to_gain(v),
            _ => self.rel = libm::expf(-1.0 / (v * 0.001 * self.sr)),
        }
        if self.fresh {
            self.gain.snap();
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
        let mut pin = 0.0f32;
        let mut gmin = 1.0f32;
        let dlen = self.dl.len();
        for i in 0..n {
            let g_in = self.gain.next();
            let xl = sane(l[i]) * g_in;
            let xr = sane(r[i]) * g_in;
            let pk = xl.abs().max(xr.abs());
            pin = pin.max(pk);
            let req = if pk > self.ceil { self.ceil / pk } else { 1.0 };
            let m = self.window_min(req);
            self.env = if m < self.env {
                m
            } else {
                m + (self.env - m) * self.rel
            };
            let g = self.box2.run(self.box1.run(self.env));
            gmin = gmin.min(g);
            let (yl, yr) = if dlen > 0 {
                let a = self.dl[self.dpos];
                let b = self.dr[self.dpos];
                self.dl[self.dpos] = xl;
                self.dr[self.dpos] = xr;
                self.dpos = if self.dpos + 1 == dlen {
                    0
                } else {
                    self.dpos + 1
                };
                (a, b)
            } else {
                (xl, xr)
            };
            let c = self.ceil;
            l[i] = (yl * g).clamp(-c, c);
            r[i] = (yr * g).clamp(-c, c);
        }
        self.in_m.update(pin, n, self.sr);
        self.out_m
            .update(peak_of(&l[..n]).max(peak_of(&r[..n])), n, self.sr);
        self.gr_m = gain_to_db(gmin).min(0.0);
    }

    fn reset(&mut self) {
        self.dl.fill(0.0);
        self.dr.fill(0.0);
        self.dpos = 0;
        self.q_head = 0;
        self.q_len = 0;
        self.n = 0;
        self.env = 1.0;
        self.box1.reset();
        self.box2.reset();
        self.gain.snap();
        self.in_m = PeakMeter::default();
        self.out_m = PeakMeter::default();
        self.gr_m = 0.0;
        self.fresh = true;
    }

    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.in_m.db(),
            1 => self.out_m.db(),
            2 => self.gr_m,
            _ => 0.0,
        }
    }
}
