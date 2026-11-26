//! The synth voices' SIMD kernels (the `wide` crate).
//!
//! Every lane performs exactly the sequence of IEEE operations the scalar code performed (see
//! `scalar_reference`, which the tests compare against bit for bit), so the output is the same on
//! every platform: only add, subtract, multiply, divide, compare and select on lanes; no fused
//! multiply-add, no reciprocal estimates, no `min`/`max` (their zero and NaN rules differ between
//! NEON and SSE; a compare and a select do not). `libm` calls stay scalar. Sums over unison
//! copies and over voices add the lanes one by one in their scalar order, never as a tree.
//!
//! Two kernels:
//! - the unison oscillator: the copies of one oscillator side by side (four per chunk);
//! - the filter stage: four voices side by side (`Group`), after each voice has produced its
//!   pre-filter samples, tuning coefficients and amplitude gains (`Voice::prepare`).

use super::*;
use wide::{f32x4, f64x4};

/// Lanes per vector.
pub(super) const LANES: usize = 4;

// `wide`'s own `splat` repeats the value through an array loop, which LLVM turns into a
// `memset_pattern16` library call on macOS: build the lanes in straight line instead.
#[inline(always)]
fn splat(x: f32) -> f32x4 {
    f32x4::new([x, x, x, x])
}

#[inline(always)]
fn splat64(x: f64) -> f64x4 {
    f64x4::new([x, x, x, x])
}

/// Four voices' per-sample values between `Voice::prepare` and the filter stage: lane `j` of
/// sample `i` is voice `j` of the group.
pub(super) struct Group {
    pitch_cache: pitch_cache::Cache,
    /// Filter input (before the drive), left and right.
    xl: [[f32; LANES]; MAX_BLOCK],
    xr: [[f32; LANES]; MAX_BLOCK],
    /// Filter tuning coefficient `tan(pi fc / sr)`.
    g: [[f32; LANES]; MAX_BLOCK],
    /// Output gain (envelope, velocity, tremolo, fade).
    amp: [[f32; LANES]; MAX_BLOCK],
    /// Samples each voice rendered (less than the block when it ended in it).
    valid: [usize; LANES],
}

impl Group {
    pub(super) fn new() -> Self {
        let z = [[0.0; LANES]; MAX_BLOCK];
        Self {
            pitch_cache: pitch_cache::Cache::new(),
            xl: z,
            xr: z,
            g: z,
            amp: z,
            valid: [0; LANES],
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rendering

/// Renders every sounding voice for frames `start..end` into `out_l` / `out_r`, four voices at a
/// time through the filter stage. Voices add into the output in slot order, as before.
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    voices: &mut [Voice; SLOTS],
    cx: &Ctx,
    b: &Bufs,
    t: &Wavetables,
    grp: &mut Group,
    start: usize,
    end: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
) {
    let plan = pitch_cache::Plan::new(voices, b, start);
    grp.pitch_cache.clear(plan.rows, end - start);
    let mut slots = [0usize; LANES];
    let mut lanes = 0;
    for slot in 0..SLOTS {
        if !voices[slot].active {
            continue;
        }
        voices[slot].prepare(cx, b, t, start, end, grp, lanes, plan.tracks[slot]);
        slots[lanes] = slot;
        lanes += 1;
        if lanes == LANES {
            filter_group(voices, &slots, lanes, cx, b, grp, start, end, out_l, out_r);
            lanes = 0;
        }
    }
    if lanes > 0 {
        // lanes without a voice stay silent
        for lane in lanes..LANES {
            grp.valid[lane] = 0;
            for i in 0..end - start {
                grp.xl[i][lane] = 0.0;
                grp.xr[i][lane] = 0.0;
                grp.g[i][lane] = 0.0;
                grp.amp[i][lane] = 0.0;
            }
        }
        filter_group(voices, &slots, lanes, cx, b, grp, start, end, out_l, out_r);
    }
}

impl Voice {
    /// Everything of a voice up to the filter, for frames `start..end`, stored in lane `lane` of
    /// `grp`: envelopes, glide, tuning, oscillators, sub and noise, and the filter's tuning
    /// coefficient and the output gain. A voice that ends inside the block stops there.
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &mut self,
        cx: &Ctx,
        b: &Bufs,
        t: &Wavetables,
        start: usize,
        end: usize,
        grp: &mut Group,
        lane: usize,
        pitch_rows: [u8; 3],
    ) {
        let n = end - start;
        let mut done = n;
        for i in start..end {
            let rel = i - start;
            let mut fade = 1.0f32;
            if self.fading {
                self.fade -= cx.fade_step;
                if self.fade <= 0.0 {
                    self.active = false;
                    self.fading = false;
                    done = rel;
                    break;
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
                // The note has ended.
                self.active = false;
                done = rel;
                break;
            }
            let fenv = self.flt.next(&cx.flt) as f32;

            let pm = self.pitch + b.pitch[i];
            let p1 = pm + b.off1[i];
            if p1 != self.last_p1 {
                self.last_p1 = p1;
                let hz = grp.pitch_cache.hz(pitch_rows[0], rel, p1);
                (self.inc1, self.lv1) = tune_from_hz(p1, cx.uni1.to.max_semi, cx, hz);
            }
            let p2 = pm + b.off2[i];
            if p2 != self.last_p2 {
                self.last_p2 = p2;
                let hz = grp.pitch_cache.hz(pitch_rows[1], rel, p2);
                (self.inc2, self.lv2) = tune_from_hz(p2, cx.uni2.to.max_semi, cx, hz);
            }
            if pm != self.last_pm {
                self.last_pm = pm;
                let p = pm - 12.0;
                let hz = grp.pitch_cache.hz(pitch_rows[2], rel, p);
                (self.inc_sub, self.lv_sub) = tune_from_hz(p, 0.0, cx, hz);
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
                let s = wt_read1(t, m, self.lv_sub, self.ph_sub) * sub;
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

            // The filter's tuning: the same octave value as the last sample gives the same
            // coefficient, so the two `libm` calls run only while the cutoff moves.
            let oct = b.cut[i]
                + b.keytrack[i] * (self.pitch - 60.0) * (1.0 / 12.0)
                + fenv * b.env_oct[i] * self.vel_flt;
            if !self.g_ok || oct.to_bits() != self.oct_bits {
                let fc = libm::exp2f(oct).clamp(8.0, cx.max_fc);
                self.g = libm::tanf(fc * cx.pi_sr);
                self.oct_bits = oct.to_bits();
                self.g_ok = true;
            }
            grp.xl[rel][lane] = sl;
            grp.xr[rel][lane] = sr;
            grp.g[rel][lane] = self.g;
            grp.amp[rel][lane] = aenv * self.vel_amp * b.trem[i] * fade;
        }
        grp.valid[lane] = done;
        // (a voice that ended early leaves silence behind it)
        for rel in done..n {
            grp.xl[rel][lane] = 0.0;
            grp.xr[rel][lane] = 0.0;
            grp.g[rel][lane] = 0.0;
            grp.amp[rel][lane] = 0.0;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Unison oscillator: the copies side by side

type Quad = [[f32; 4]; TABLE_LEN + 1];

/// The four interpolated table values `[v00, v01, v10, v11]` of one phase (`i`, `fr`):
/// `a + (b - a) * fr` for the tables (k, l), (k, l + 1), (k + 1, l), (k + 1, l + 1) at once.
#[inline(always)]
fn quad_lerp(q: &Quad, i: usize, fr: f32) -> f32x4 {
    let (a, b) = (f32x4::new(q[i]), f32x4::new(q[i + 1]));
    a + (b - a) * splat(fr)
}

/// The blend of `wt_read`, per lane or per phase: the two morph tables each blended across the
/// two mip levels (only where the scalar code does), then across the morph. `v00`..`v11` as in
/// [`quad_lerp`].
#[inline(always)]
fn blend(v: [f32x4; 4], m: Morph, lt: f32) -> f32x4 {
    let mut acc = splat(0.0);
    let w0 = 1.0 - m.t;
    if w0 > 0.0 {
        let mut x = v[0];
        if lt > 0.0 {
            x += (v[1] - x) * splat(lt);
        }
        acc += x * splat(w0);
    }
    if m.t > 0.0 {
        let mut x = v[2];
        if lt > 0.0 {
            x += (v[3] - x) * splat(lt);
        }
        acc += x * splat(m.t);
    }
    acc
}

/// One band-limited sample at `phase` for a morph position and mip level pair (the scalar
/// `wt_read`, with the four tables' samples fetched from the interleaved copy).
#[inline(always)]
pub(super) fn wt_read1(t: &Wavetables, m: Morph, lv: (usize, f32), phase: f64) -> f32 {
    let pos = phase * TABLE_LEN as f64;
    let idx = (pos as usize).min(TABLE_LEN - 1);
    let fr = (pos - idx as f64) as f32;
    let (l, lt) = lv;
    let v = quad_lerp(t.quad(m.k, l), idx, fr).to_array();
    // (the scalar blend, on the four values)
    let mut acc = 0.0;
    let w0 = 1.0 - m.t;
    if w0 > 0.0 {
        let mut x = v[0];
        if lt > 0.0 {
            x += (v[1] - x) * lt;
        }
        acc += x * w0;
    }
    if m.t > 0.0 {
        let mut x = v[2];
        if lt > 0.0 {
            x += (v[3] - x) * lt;
        }
        acc += x * m.t;
    }
    acc
}

/// Sums the unison voices of one oscillator; `x` is the layout crossfade position (1 = settled).
#[inline(always)]
pub(super) fn osc_unison(
    t: &Wavetables,
    m: Morph,
    lv: (usize, f32),
    ph: &mut [f64; UNI_LANES],
    inc: f64,
    u: &UniState,
    x: f32,
) -> (f32, f32) {
    let settled = x >= 1.0;
    let copies = if settled {
        u.to.n
    } else {
        u.to.n.max(u.from_n)
    };
    let (mut l, mut r) = (0.0f32, 0.0f32);
    if copies == 1 {
        // one copy: the scalar sequence
        let p = ph[0];
        let s = wt_read1(t, m, lv, p);
        let mut np = p + inc * u.ratio[0];
        if np >= 1.0 {
            np -= 1.0;
        }
        ph[0] = np;
        if settled {
            l += s * u.to.pl[0];
            r += s * u.to.pr[0];
        } else {
            l += s * (u.from_l[0] + (u.to.pl[0] - u.from_l[0]) * x);
            r += s * (u.from_r[0] + (u.to.pr[0] - u.from_r[0]) * x);
        }
        return (l, r);
    }
    let quad = t.quad(m.k, lv.0);
    let inc = splat64(inc);
    for chunk in 0..copies.div_ceil(LANES) {
        let base = chunk * LANES;
        let live = (copies - base).min(LANES);
        let p: [f64; LANES] = ph[base..base + LANES].try_into().expect("four phases");
        let mut v = [splat(0.0); LANES];
        for j in 0..LANES {
            let pos = p[j] * TABLE_LEN as f64;
            let i = (pos as usize).min(TABLE_LEN - 1);
            v[j] = quad_lerp(quad, i, (pos - i as f64) as f32);
        }
        // a row per table, a lane per copy
        let s = blend(f32x4::transpose(v), m, lv.1);

        // phases: np = p + inc * ratio, wrapped at 1; copies beyond `live` stay where they are
        let p = f64x4::new(p);
        let ratio = f64x4::new(u.ratio[base..base + LANES].try_into().expect("four ratios"));
        let np = p + inc * ratio;
        let np = np.simd_ge(splat64(1.0)).select(np - splat64(1.0), np);
        let alive = f64x4::new([0.0, 1.0, 2.0, 3.0]).simd_lt(splat64(live as f64));
        ph[base..base + LANES].copy_from_slice(&alive.select(np, p).to_array());

        // gains
        let at = |a: &[f32; UNI_LANES]| {
            f32x4::new(a[base..base + LANES].try_into().expect("four gains"))
        };
        let (gl, gr) = if settled {
            (at(&u.to.pl), at(&u.to.pr))
        } else {
            let (fl, fr_) = (at(&u.from_l), at(&u.from_r));
            let xx = splat(x);
            (
                fl + (at(&u.to.pl) - fl) * xx,
                fr_ + (at(&u.to.pr) - fr_) * xx,
            )
        };
        // Copies beyond `live` have the gain 0 (the layouts' padding), so adding their products
        // changes nothing: a sum never holds -0, and x + (+-0) = x for every other x.
        let (pl, pr) = ((s * gl).to_array(), (s * gr).to_array());
        for j in 0..LANES {
            l += pl[j];
            r += pr[j];
        }
    }
    (l, r)
}

// ---------------------------------------------------------------------------------------------
// Filter stage: four voices side by side

/// `util::flush` per lane.
#[inline(always)]
fn flush4(x: f32x4) -> f32x4 {
    (x + splat(1.0e-18)) - splat(1.0e-18)
}

/// `util::fast_tanh` per lane (the scalar clamp: compare and select, so NaN and zeros agree).
#[inline(always)]
fn fast_tanh4(x: f32x4) -> f32x4 {
    let (lo, hi) = (splat(-4.97), splat(4.97));
    let x = x.simd_lt(lo).select(lo, x);
    let x = x.simd_gt(hi).select(hi, x);
    let x2 = x * x;
    let num = x * (splat(135135.0) + x2 * (splat(17325.0) + x2 * (splat(378.0) + x2)));
    let den = splat(135135.0) + x2 * (splat(62370.0) + x2 * (splat(3150.0) + splat(28.0) * x2));
    num / den
}

/// One ladder channel: the four stage states, a lane per voice.
#[inline(always)]
fn ladder_run(s: &mut [f32x4; 4], x: f32x4, gg: f32x4, k: f32x4) -> f32x4 {
    let one = splat(1.0);
    let b = one - gg;
    let g2 = gg * gg;
    let sigma = g2 * gg * b * s[0] + g2 * b * s[1] + gg * b * s[2] + b * s[3];
    let g4 = g2 * g2;
    let y_est = (g4 * x + sigma) / (one + k * g4);
    let mut u = splat(1.6) * fast_tanh4((x - k * y_est) * splat(0.625));
    for st in s.iter_mut() {
        let v = (u - *st) * gg;
        let y = v + *st;
        *st = flush4(y + v);
        u = y;
    }
    u * (one + splat(0.7) * k)
}

/// One state-variable filter channel (`ic1`, `ic2`): returns (low, band, high).
#[inline(always)]
fn svf_run(
    ic: &mut (f32x4, f32x4),
    v0: f32x4,
    k: f32x4,
    a1: f32x4,
    a2: f32x4,
    a3: f32x4,
) -> (f32x4, f32x4, f32x4) {
    let (ic1, ic2) = *ic;
    let v3 = v0 - ic2;
    let v1 = a1 * ic1 + a2 * v3;
    let v2 = ic2 + a2 * ic1 + a3 * v3;
    ic.0 = flush4(splat(2.0) * v1 - ic1);
    ic.1 = flush4(splat(2.0) * v2 - ic2);
    (v2, v1, v0 - k * v1 - v2)
}

fn set_lane(v: &mut f32x4, lane: usize, x: f32) {
    let mut a = v.to_array();
    a[lane] = x;
    *v = f32x4::new(a);
}

/// Drive, filter and output gain of four voices (`slots`, the first `lanes` of them real),
/// added into the output in lane order, which is slot order.
#[allow(clippy::too_many_arguments)]
fn filter_group(
    voices: &mut [Voice; SLOTS],
    slots: &[usize; LANES],
    lanes: usize,
    cx: &Ctx,
    b: &Bufs,
    grp: &Group,
    start: usize,
    end: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
) {
    let _ = (cx, end);
    // the voices' filter states, a lane per voice
    let mut svf = [(splat(0.0), splat(0.0)); 2];
    let mut lad = [[splat(0.0); 4]; 2];
    let (mut svf_live, mut lad_live) = ([true; LANES], [true; LANES]);
    for (j, &slot) in slots.iter().enumerate().take(lanes) {
        let v = &voices[slot];
        for ch in 0..2 {
            set_lane(&mut svf[ch].0, j, v.svf[ch].ic1);
            set_lane(&mut svf[ch].1, j, v.svf[ch].ic2);
            for st in 0..4 {
                set_lane(&mut lad[ch][st], j, v.lad[ch].s[st]);
            }
        }
        svf_live[j] = v.svf_live;
        lad_live[j] = v.lad_live;
    }

    let longest = grp.valid.iter().copied().max().unwrap_or(0);
    let shortest = grp.valid.iter().copied().min().unwrap_or(0);
    let one = splat(1.0);
    for rel in 0..longest {
        let i = start + rel;
        let mut xl = f32x4::new(grp.xl[rel]);
        let mut xr = f32x4::new(grp.xr[rel]);
        let g = f32x4::new(grp.g[rel]);
        let amp = f32x4::new(grp.amp[rel]);

        let dg = b.drive_g[i];
        if dg > 1.0e-3 {
            let n = splat(b.drive_n[i]);
            let dg = splat(dg);
            xl = fast_tanh4(xl * dg) * n;
            xr = fast_tanh4(xr * dg) * n;
        }

        let ft = b.ftype[i];
        let w = &b.fw[i];
        let svf_k = b.svf_k[i];
        let lad_k = b.lad_k[i];
        let use_lad = ft == 1 || (ft == FILTER_XF && w[1] > 0.0);
        let use_svf = ft != 1 && (ft != FILTER_XF || w[0] > 0.0 || w[2] > 0.0 || w[3] > 0.0);
        // A filter that was idle has a stale state: seed it with the state a long-settled filter
        // has for a constant input equal to this sample (see `scalar_reference`).
        if use_lad && lad_live.iter().any(|live| !live) {
            let (xla, xra) = (xl.to_array(), xr.to_array());
            let k1 = 1.0 + lad_k;
            for j in 0..LANES {
                if !lad_live[j] {
                    for st in 0..4 {
                        set_lane(&mut lad[0][st], j, xla[j] / k1);
                        set_lane(&mut lad[1][st], j, xra[j] / k1);
                    }
                }
            }
        }
        if use_svf && svf_live.iter().any(|live| !live) {
            let (xla, xra) = (xl.to_array(), xr.to_array());
            for j in 0..LANES {
                if !svf_live[j] {
                    set_lane(&mut svf[0].0, j, 0.0);
                    set_lane(&mut svf[0].1, j, xla[j]);
                    set_lane(&mut svf[1].0, j, 0.0);
                    set_lane(&mut svf[1].1, j, xra[j]);
                }
            }
        }
        lad_live = [use_lad; LANES];
        svf_live = [use_svf; LANES];

        let zero = splat(0.0);
        let (mut yl, mut yr) = (zero, zero);
        let mut ladder = (zero, zero);
        if use_lad {
            let gg = g / (one + g);
            let k = splat(lad_k);
            ladder = (
                ladder_run(&mut lad[0], xl, gg, k),
                ladder_run(&mut lad[1], xr, gg, k),
            );
        }
        let (out_a, out_c);
        if ft == 1 {
            (out_a, out_c) = ladder;
        } else {
            if use_svf {
                let k = splat(svf_k);
                let a1 = one / (one + g * (g + k));
                let a2 = g * a1;
                let a3 = g * a2;
                let a = svf_run(&mut svf[0], xl, k, a1, a2, a3);
                let c = svf_run(&mut svf[1], xr, k, a1, a2, a3);
                match ft {
                    0 => {
                        (yl, yr) = (a.0, c.0);
                    }
                    2 => {
                        (yl, yr) = (a.2, c.2);
                    }
                    3 => {
                        (yl, yr) = (a.1 * k, c.1 * k);
                    }
                    _ => {
                        let (w0, w2, w3) = (splat(w[0]), splat(w[2]), splat(w[3]));
                        yl = w0 * a.0 + w2 * a.2 + w3 * a.1 * k;
                        yr = w0 * c.0 + w2 * c.2 + w3 * c.1 * k;
                    }
                }
            }
            if ft == FILTER_XF {
                let w1 = splat(w[1]);
                yl += w1 * ladder.0;
                yr += w1 * ladder.1;
            }
            (out_a, out_c) = (yl, yr);
        }

        let (ol, or) = ((out_a * amp).to_array(), (out_c * amp).to_array());
        if rel < shortest {
            for j in 0..lanes {
                out_l[i] += ol[j];
                out_r[i] += or[j];
            }
        } else {
            for j in 0..lanes {
                if rel < grp.valid[j] {
                    out_l[i] += ol[j];
                    out_r[i] += or[j];
                }
            }
        }
    }

    for (j, &slot) in slots.iter().enumerate().take(lanes) {
        let v = &mut voices[slot];
        for ch in 0..2 {
            v.svf[ch].ic1 = svf[ch].0.to_array()[j];
            v.svf[ch].ic2 = svf[ch].1.to_array()[j];
            for st in 0..4 {
                v.lad[ch].s[st] = lad[ch][st].to_array()[j];
            }
        }
        v.svf_live = svf_live[j];
        v.lad_live = lad_live[j];
    }
}
