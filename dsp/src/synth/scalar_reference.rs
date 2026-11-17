//! The synth's scalar voice code, as it was before the SIMD kernels: the reference the tests
//! compare them against bit for bit (`Synth::scalar_reference`). Kept verbatim; never optimise it.

use super::*;

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

/// 4-pole TPT ladder step (see `Ladder`).
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

/// Sums the unison voices of one oscillator; `x` is the layout crossfade position (1 = settled).
#[inline(always)]
fn osc_unison(
    t: &Wavetables,
    m: Morph,
    lv: (usize, f32),
    ph: &mut [f64; UNI_LANES],
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

impl Voice {
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
    pub(super) fn render_scalar(
        &mut self,
        cx: &Ctx,
        b: &Bufs,
        t: &Wavetables,
        start: usize,
        end: usize,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        for i in start..end {
            let mut fade = 1.0f32;
            if self.fading {
                self.fade -= cx.fade_step;
                if self.fade <= 0.0 {
                    self.active = false;
                    self.fading = false;
                    return;
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
            let fc = libm::exp2f(oct).clamp(8.0, cx.max_fc);
            let g = libm::tanf(fc * cx.pi_sr);
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
