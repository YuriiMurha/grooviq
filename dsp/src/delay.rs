//! Stereo delay with tempo sync, ping-pong and filtered feedback.
//!
//! Time changes never click or pitch-bend: when the target time moves, a second read head is
//! started at the new time and further changes queue behind the 50 ms crossfade.
//! The fade uses constant-sum gains so correlated reads cannot boost feedback.
//! Reads use Hermite interpolation, so synced times land on
//! fractional samples exactly. The feedback path (and the wet output) runs through 12 dB/oct
//! low/high cuts and a soft safety limiter. Ping-pong feeds the mono input into the left line
//! and crosses the feedback, so echoes alternate L, R, L...
//!
//! Scalar meter 3 reports the active wet-path high cut in Hz, including log2 smoothing and the
//! sample-rate cap from [`crate::effective_high_cut`].

use crate::api::{Device, NoteEvent};
use crate::util::*;

/// `(min, max, default)` per parameter, `SPEC` index order.
pub const SPEC: [(f32, f32, f32); 9] = [
    (0.0, 1.0, 1.0),           // 0 sync
    (0.0, 8.0, 4.0),           // 1 division
    (1.0, 2000.0, 250.0),      // 2 time ms
    (0.0, 0.95, 0.35),         // 3 feedback
    (0.0, 1.0, 1.0),           // 4 ping-pong
    (20.0, 2000.0, 200.0),     // 5 low cut Hz
    (1000.0, 20000.0, 7000.0), // 6 high cut Hz
    (0.0, 1.0, 0.25),          // 7 mix
    (20.0, 400.0, 140.0),      // 8 bpm
];

/// Stable parameter ids in `SPEC` order.
pub const PARAM_IDS: [&str; 9] = [
    "sync",
    "division",
    "time_ms",
    "feedback",
    "ping_pong",
    "low_cut",
    "high_cut",
    "mix",
    "bpm",
];

/// Division lengths in quarter-note beats, `SPEC` index order.
pub const DIVISION_BEATS: [f32; 9] = [
    0.125,
    0.25,
    0.375,
    1.0 / 3.0,
    0.5,
    0.75,
    2.0 / 3.0,
    1.0,
    2.0,
];
const CTRL: u32 = 16;
const XFADE_MS: f32 = 50.0;
/// Longest delay: a half note at 20 BPM. The constructor adds the interpolation margin.
const MAX_MS: f32 = 6000.0;

#[inline]
fn flush_output_subnormal(sample: f32) -> f32 {
    let bits = sample.to_bits();
    if bits & 0x7f80_0000 == 0 {
        // Exponent zero covers subnormals and signed zero. Keep the sign bit.
        f32::from_bits(bits & 0x8000_0000)
    } else {
        sample
    }
}

pub struct Delay {
    sr: f32,
    buf: [DelayLine; 2],
    sync: bool,
    div: usize,
    time_ms: f32,
    bpm: f32,
    target: f32,
    ta: f32,
    tb: f32,
    x: f32,
    xfading: bool,
    xf_step: f32,
    fb: Smooth,
    pingpong: bool,
    lo: Smooth, // log2 Hz
    hi: Smooth,
    c_lo: Coefs,
    c_hi: Coefs,
    high_cut_hz: f32,
    st_lo: [Biquad; 2],
    st_hi: [Biquad; 2],
    dry_g: Smooth,
    wet_g: Smooth,
    ctr: u32,
    fresh: bool,
    in_m: PeakMeter,
    out_m: PeakMeter,
}

/// Ring length at the (clamped) rate `sr`: the longest delay plus the read margin.
fn buffer_delay(sr: f32) -> usize {
    (MAX_MS * 0.001 * sr) as usize + 8
}

pub(crate) fn memory_payload(sr: f32) -> crate::insert_memory::Payload {
    let mut payload = crate::insert_memory::Payload::boxed::<Delay>();
    payload.delay_lines(buffer_delay(sr.clamp(8000.0, 384_000.0)), 2);
    payload
}

impl Delay {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.clamp(8000.0, 384_000.0);
        let max = buffer_delay(sr);
        let ctrl_rate = sr / CTRL as f32;
        let mut d = Self {
            sr,
            buf: [DelayLine::new(max), DelayLine::new(max)],
            sync: true,
            div: 4,
            time_ms: 250.0,
            bpm: 140.0,
            target: 1.0,
            ta: 1.0,
            tb: 1.0,
            x: 0.0,
            xfading: false,
            xf_step: 1.0 / (XFADE_MS * 0.001 * sr),
            fb: Smooth::new(0.0, 15.0, sr),
            pingpong: true,
            lo: Smooth::new(7.6, 15.0, ctrl_rate),
            hi: Smooth::new(12.8, 15.0, ctrl_rate),
            c_lo: Coefs::IDENTITY,
            c_hi: Coefs::IDENTITY,
            high_cut_hz: 0.0,
            st_lo: [Biquad::default(); 2],
            st_hi: [Biquad::default(); 2],
            dry_g: Smooth::new(1.0, 15.0, sr),
            wet_g: Smooth::new(0.0, 15.0, sr),
            ctr: 0,
            fresh: true,
            in_m: PeakMeter::default(),
            out_m: PeakMeter::default(),
        };
        for (i, s) in SPEC.iter().enumerate() {
            d.set_param(i as u32, s.2);
        }
        d
    }

    /// Effective delay time in ms.
    pub fn effective_ms(&self) -> f32 {
        if self.sync {
            DIVISION_BEATS[self.div] * 60_000.0 / self.bpm
        } else {
            self.time_ms
        }
    }

    fn update_target(&mut self) {
        let max = self.buf[0].max_delay() as f32 - 4.0;
        self.target = (self.effective_ms() * 0.001 * self.sr).clamp(1.0, max);
        if self.fresh {
            self.ta = self.target;
            self.tb = self.target;
            self.xfading = false;
        }
    }

    fn update_filters(&mut self) {
        let sr = self.sr as f64;
        let q = std::f64::consts::FRAC_1_SQRT_2;
        self.c_lo = Coefs::highpass(libm::exp2f(self.lo.next()) as f64, q, sr);
        let high_cut_hz = effective_high_cut(libm::exp2f(self.hi.next()) as f64, sr);
        self.c_hi = Coefs::lowpass(high_cut_hz, q, sr);
        self.high_cut_hz = high_cut_hz as f32;
    }

    fn snap_all(&mut self) {
        self.fb.snap();
        self.lo.snap();
        self.hi.snap();
        self.dry_g.snap();
        self.wet_g.snap();
        self.update_filters();
    }
}

impl Device for Delay {
    fn set_param(&mut self, index: u32, value: f32) {
        let Some(v) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 => self.sync = v >= 0.5,
            1 => self.div = (v.round() as usize).min(DIVISION_BEATS.len() - 1),
            2 => self.time_ms = v,
            3 => self.fb.set(v),
            4 => self.pingpong = v >= 0.5,
            5 => self.lo.set(libm::log2f(v)),
            6 => self.hi.set(libm::log2f(v)),
            7 => {
                let a = v * std::f32::consts::FRAC_PI_2;
                self.dry_g.set(libm::cosf(a));
                self.wet_g.set(libm::sinf(a));
            }
            _ => self.bpm = v,
        }
        if matches!(index, 0 | 1 | 2 | 8) {
            self.update_target();
        }
        if self.fresh {
            self.snap_all();
        }
    }

    fn set_tempo(&mut self, bpm: f32) {
        self.set_param(8, bpm);
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
        for i in 0..n {
            if self.ctr == 0 && (!self.lo.settled() || !self.hi.settled()) {
                self.update_filters();
            }
            self.ctr = (self.ctr + 1) % CTRL;

            let xl = sane(l[i]);
            let xr = sane(r[i]);
            pin = pin.max(xl.abs()).max(xr.abs());

            if !self.xfading && self.target != self.ta {
                self.tb = self.target;
                self.x = 0.0;
                self.xfading = true;
            }
            // the line is read before this sample is pushed, so "d - 1" gives a delay of d
            let (mut dl, mut dr) = (
                self.buf[0].read_frac(self.ta - 1.0),
                self.buf[1].read_frac(self.ta - 1.0),
            );
            if self.xfading {
                self.x = (self.x + self.xf_step).min(1.0);
                let (ga, gb) = (1.0 - self.x, self.x);
                let bl = self.buf[0].read_frac(self.tb - 1.0);
                let br = self.buf[1].read_frac(self.tb - 1.0);
                dl = dl * ga + bl * gb;
                dr = dr * ga + br * gb;
                if self.x >= 1.0 {
                    self.ta = self.tb;
                    self.xfading = false;
                }
            }
            let fl = self.st_hi[0].run(&self.c_hi, self.st_lo[0].run(&self.c_lo, dl as f64)) as f32;
            let fr = self.st_hi[1].run(&self.c_hi, self.st_lo[1].run(&self.c_lo, dr as f64)) as f32;
            let fb = self.fb.next();
            let (wl, wr) = if self.pingpong {
                (mid(xl, xr) + fb * fr, fb * fl)
            } else {
                (xl + fb * fl, xr + fb * fr)
            };
            self.buf[0].push(flush(soft_limit(wl)));
            self.buf[1].push(flush(soft_limit(wr)));
            let dg = self.dry_g.next();
            let wg = self.wet_g.next();
            // The f64 filter tail can narrow to subnormal f32, and the mix can create more.
            l[i] = flush_output_subnormal(xl * dg + fl * wg);
            r[i] = flush_output_subnormal(xr * dg + fr * wg);
        }
        self.in_m.update(pin, n, self.sr);
        self.out_m
            .update(peak_of(&l[..n]).max(peak_of(&r[..n])), n, self.sr);
    }

    fn reset(&mut self) {
        for b in self.buf.iter_mut() {
            b.clear();
        }
        self.st_lo = [Biquad::default(); 2];
        self.st_hi = [Biquad::default(); 2];
        self.ta = self.target;
        self.tb = self.target;
        self.xfading = false;
        self.ctr = 0;
        self.snap_all();
        self.in_m = PeakMeter::default();
        self.out_m = PeakMeter::default();
        self.fresh = true;
    }

    /// 0–1: input/output peaks in dB. 2: effective delay time in ms.
    /// 3: active wet-path high cut in Hz.
    fn meter(&self, index: u32) -> f32 {
        match index {
            0 => self.in_m.db(),
            1 => self.out_m.db(),
            2 => self.effective_ms(),
            3 => self.high_cut_hz,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The pre-flush static sample path. Fixed parameters remove smoothing and crossfades.
    fn static_reference(delay: &mut Delay, xl: f32, xr: f32) -> (f32, f32) {
        let dl = delay.buf[0].read_frac(delay.ta - 1.0);
        let dr = delay.buf[1].read_frac(delay.ta - 1.0);
        let fl = delay.st_hi[0].run(&delay.c_hi, delay.st_lo[0].run(&delay.c_lo, dl as f64)) as f32;
        let fr = delay.st_hi[1].run(&delay.c_hi, delay.st_lo[1].run(&delay.c_lo, dr as f64)) as f32;
        let fb = delay.fb.cur;
        let (wl, wr) = if delay.pingpong {
            (mid(xl, xr) + fb * fr, fb * fl)
        } else {
            (xl + fb * fl, xr + fb * fr)
        };
        delay.buf[0].push(flush(soft_limit(wl)));
        delay.buf[1].push(flush(soft_limit(wr)));
        (
            xl * delay.dry_g.cur + fl * delay.wet_g.cur,
            xr * delay.dry_g.cur + fr * delay.wet_g.cur,
        )
    }

    #[test]
    fn static_output_preserves_every_non_subnormal_bit() {
        for (low_cut, high_cut, mix) in [(20.0, 20_000.0, 0.25), (2000.0, 1000.0, 1.0)] {
            for pingpong in [false, true] {
                let configured = || {
                    let mut delay = Delay::new(48_000.0);
                    for (param, value) in [
                        (0, 0.0),
                        (2, 20.05),
                        (3, 0.95),
                        (4, f32::from(pingpong)),
                        (5, low_cut),
                        (6, high_cut),
                        (7, mix),
                    ] {
                        delay.set_param(param, value);
                    }
                    delay
                };
                let mut reference = configured();
                let mut delay = configured();

                for start in (0..8192).step_by(128) {
                    let mut left = [0.0; 128];
                    let mut right = [0.0; 128];
                    if start == 0 {
                        left[0] = 0.8;
                        // At full wet, the tiny dry gain can create a subnormal before any echo.
                        left[1] = 2.0 * f32::MIN_POSITIVE;
                        right[3] = -0.3;
                    }
                    let expected: [(f32, f32); 128] = std::array::from_fn(|i| {
                        static_reference(&mut reference, left[i], right[i])
                    });
                    delay.process(&mut left, &mut right, None, &[]);
                    for (i, ((l, r), (old_l, old_r))) in
                        left.into_iter().zip(right).zip(expected).enumerate()
                    {
                        for (channel, (actual, prior)) in
                            [(l, old_l), (r, old_r)].into_iter().enumerate()
                        {
                            let matches = if prior.is_subnormal() {
                                actual == 0.0
                            } else {
                                actual.to_bits() == prior.to_bits()
                            };
                            assert!(
                                matches,
                                "cuts={low_cut}/{high_cut}, mix={mix}, ping-pong={pingpong}, frame={}, channel={channel}, actual={:#010x}, reference={:#010x}",
                                start + i,
                                actual.to_bits(),
                                prior.to_bits()
                            );
                        }
                    }
                }
            }
        }
    }
}
