//! Frozen render loop from main at 038c058a, including event and span handling.

use super::*;

impl Sampler {
    pub(super) fn render_reference<const EAGER: bool>(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
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
            filter_bypassed,
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
        let filter_on = kind != 0;
        let (env_amount, flt_sustain) = (p[FILTER_ENV], p[FLT_SUSTAIN]);
        let mut ev = events.iter().peekable();
        // Only events (and a choke, before the block) put voices in fade slots.
        let mut span = voices.span();
        if !EAGER {
            if filter_on && *filter_bypassed {
                // A parameter change between ticks must use the previous tick's inputs until
                // the next one. New notes and reset slots carry no pending inputs.
                for v in &mut voices.v[..span] {
                    if let Some((octaves, damp)) = v.pending_filter.take() {
                        v.filter_coefs(octaves, damp, *sr);
                    }
                }
            }
            *filter_bypassed = !filter_on;
        }
        for f in 0..n {
            let mut evented = false;
            while let Some(e) = ev.next_if(|e| e.offset as usize <= f) {
                voices.event(p, *sr, buf, e);
                evented = true;
            }
            if evented {
                span = voices.span();
            }
            let fade_frames = voices.fade;
            let sustain = sustain.next();
            let log2_cutoff = cutoff.next();
            let (mut sl, mut sr_) = (0.0f32, 0.0f32);
            for v in voices.v[..span].iter_mut() {
                if v.stage == Stage::Off {
                    continue;
                }
                // Fading out (stolen, cut or choked): a straight line to silence, then the slot
                // is free.
                let mut fade = None;
                if v.fade > 0 {
                    v.fade -= 1;
                    if v.fade == 0 {
                        *v = Voice::OFF;
                        continue;
                    }
                    fade = Some(v.fade as f32 / fade_frames as f32);
                }
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
                        // Decayed to silence: the slot is free.
                        if v.env < FREE {
                            *v = Voice::OFF;
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
                    let octaves = v.filter_octaves(log2_cutoff, env_amount);
                    if EAGER || filter_on {
                        v.filter_coefs(octaves, c.damp, *sr);
                    } else {
                        v.pending_filter = Some((octaves, c.damp));
                    }
                    v.ctl = CONTROL_FRAMES;
                }
                v.ctl -= 1;
                if v.pitch != v.target {
                    v.glide();
                }
                let mut amp = v.env * v.vel;
                if let Some(fade) = fade {
                    amp *= fade;
                }
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
                let (mut xl, mut xr) = resample::read(buf, v);
                if filter_on {
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
                    // Ended by itself: the slot is free.
                    *v = Voice::OFF;
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
}
