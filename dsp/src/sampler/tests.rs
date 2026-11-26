use super::*;

struct RenderPair {
    actual: Sampler,
    eager: Sampler,
}

impl RenderPair {
    fn new(frames: usize) -> Self {
        let sample = Arc::new(SampleBuf {
            rate: 44_100.0,
            l: (0..frames).map(|i| (i % 61) as f32 / 64.0 - 0.25).collect(),
            r: (0..frames).map(|i| 0.5 - (i % 47) as f32 / 64.0).collect(),
        });
        let mut actual = Sampler::new(48_000.0);
        let mut eager = Sampler::new(48_000.0);
        actual.set_sample(Some(Arc::clone(&sample)));
        eager.set_sample(Some(sample));
        Self { actual, eager }
    }

    fn set(&mut self, params: &[(usize, f32)]) {
        for &(index, value) in params {
            self.actual.set_param(index as u32, value);
            self.eager.set_param(index as u32, value);
        }
    }

    fn block(&mut self, frames: usize, events: &[NoteEvent], context: &str) -> bool {
        self.compare_block(frames, events, context, Sampler::render::<true>)
    }

    fn exact_block(&mut self, frames: usize, events: &[NoteEvent], context: &str) -> bool {
        let audible =
            self.compare_block(frames, events, context, Sampler::render_reference::<false>);
        let actual = &self.actual.voices;
        let expected = &self.eager.voices;
        assert_eq!(actual.counter, expected.counter, "{context}");
        assert_eq!(actual.stack, expected.stack, "{context}");
        assert_eq!(actual.stack_len, expected.stack_len, "{context}");
        assert_eq!(
            actual.last_pitch.map(f32::to_bits),
            expected.last_pitch.map(f32::to_bits),
            "{context}"
        );
        assert_eq!(actual.mono, expected.mono, "{context}");
        assert_eq!(actual.fade, expected.fade, "{context}");
        assert_eq!(
            self.actual.out.map(f32::to_bits),
            self.eager.out.map(f32::to_bits),
            "{context}"
        );
        assert_eq!(
            self.actual.filter_bypassed, self.eager.filter_bypassed,
            "{context}"
        );
        for (slot, (actual, expected)) in actual.v.iter().zip(&expected.v).enumerate() {
            macro_rules! same {
                ($($field:ident),+ $(,)?) => {$(
                    assert_eq!(actual.$field, expected.$field, "{context}, slot {slot}, {}", stringify!($field));
                )+};
            }
            macro_rules! same_bits {
                ($($field:ident),+ $(,)?) => {$(
                    assert_eq!(actual.$field.to_bits(), expected.$field.to_bits(), "{context}, slot {slot}, {}", stringify!($field));
                )+};
            }
            same!(
                stage, key, track, gctl, fade, age, order, id, down, looping, wrapped, fstage, ctl
            );
            same_bits!(
                pos, step, base, root, tune, fine, pitch, target, glide_step, start, end, env, vel,
                lo, hi, len, fenv, vel_oct, k,
            );
            let pending_bits = |(octaves, damp): (f32, f32)| (octaves.to_bits(), damp.to_bits());
            assert_eq!(
                actual.pending_filter.map(pending_bits),
                expected.pending_filter.map(pending_bits),
                "{context}, slot {slot}, pending_filter"
            );
            assert_eq!(
                actual.a.map(f32::to_bits),
                expected.a.map(f32::to_bits),
                "{context}, slot {slot}, a"
            );
            let integrator_bits = |ic: [f32; 2]| ic.map(f32::to_bits);
            assert_eq!(
                actual.ic.map(integrator_bits),
                expected.ic.map(integrator_bits),
                "{context}, slot {slot}, ic"
            );
        }
        audible
    }

    fn compare_block(
        &mut self,
        frames: usize,
        events: &[NoteEvent],
        context: &str,
        reference: fn(&mut Sampler, &mut [f32], &mut [f32], &[NoteEvent]),
    ) -> bool {
        let (mut l, mut r, mut eager_l, mut eager_r) =
            ([0.0; 127], [0.0; 127], [0.0; 127], [0.0; 127]);
        self.actual
            .process(&mut l[..frames], &mut r[..frames], None, events);
        reference(
            &mut self.eager,
            &mut eager_l[..frames],
            &mut eager_r[..frames],
            events,
        );
        for frame in 0..frames {
            assert_eq!(
                [l[frame].to_bits(), r[frame].to_bits()],
                [eager_l[frame].to_bits(), eager_r[frame].to_bits()],
                "{context}, frame {frame}",
            );
        }
        l[..frames]
            .iter()
            .chain(&r[..frames])
            .any(|x| x.abs() > 1e-6)
    }
}

#[test]
fn render_matches_main_through_fade_pool_overload() {
    let mut pair = RenderPair::new(4096);
    let chord = |first_id, offset| {
        std::array::from_fn::<_, VOICES, _>(|i| NoteEvent {
            id: first_id + i as u64,
            offset,
            ..note(52 + i as u8, 0.3 + i as f32 / 32.0)
        })
    };
    pair.exact_block(1, &chord(1, 0), "first chord");
    assert_eq!(pair.actual.active_voices(), VOICES);
    for group in 1..=4 {
        let start = 1 + group * VOICES as u64;
        assert!(pair.exact_block(17, &chord(start, 8), "staggered steals"));
        assert_eq!(pair.actual.active_voices(), VOICES * (1 + group as usize));
    }
    let oldest_tail = pair.actual.voices.v[VOICES].id;
    assert!(pair.exact_block(1, &chord(81, 0)[..1], "overloaded tail pool"));
    assert_eq!(pair.actual.active_voices(), SLOTS);
    assert_ne!(pair.actual.voices.v[VOICES].id, oldest_tail);
    assert_eq!(pair.actual.voices.v[VOICES].id, 65);
    assert!(pair.exact_block(17, &[], "full note and fade pools without events"));
    pair.actual.choke();
    pair.eager.choke();
    assert!(pair.exact_block(63, &[], "choked voices with no events"));
    assert!(pair.exact_block(127, &[], "tails end"));
    assert_eq!(pair.actual.active_voices(), 0);
    pair.actual.reset();
    pair.eager.reset();
    assert!(!pair.exact_block(1, &[], "reset"));
    assert!(pair.exact_block(64, &[note(57, 0.7)], "restart"));
}

#[test]
fn render_matches_main_for_glide_events_at_every_offset() {
    for mode in [0.0, 1.0, 2.0] {
        for offset in 0..=127 {
            let mut pair = RenderPair::new(192);
            pair.set(&[
                (VOICE_MODE, mode),
                (GLIDE, 3.7),
                (REVERSE, 1.0),
                (LOOP, 1.0),
                (LOOP_START, 0.7),
                (ONE_SHOT, 0.0),
                (FILTER_ENV, 0.6),
                (FLT_ATTACK, 0.5),
                (FLT_SUSTAIN, 0.3),
            ]);
            let context = format!("mode {mode}, offset {offset}");
            pair.exact_block(17, &[note(60, 0.7)], &context);
            let slide = NoteEvent {
                offset,
                slide: true,
                ..note(67, 0.4)
            };
            let off = |key| NoteEvent {
                on: false,
                ..note(key, 0.0)
            };
            pair.exact_block(127, &[slide], &context);
            pair.set(&[(GAIN, -12.0), (PAN, 0.85), (FILTER_TYPE, 1.0)]);
            pair.exact_block(1, &[], &context);
            pair.exact_block(63, &[note(62, 0.75), off(67)], &context);
            pair.exact_block(64, &[off(62), off(60)], &context);
            pair.exact_block(17, &[], &context);
        }
    }
}

#[test]
fn render_matches_main_through_both_sample_edges_and_smoothed_pan() {
    for reverse in [0.0, 1.0] {
        let mut pair = RenderPair::new(377);
        pair.set(&[
            (REVERSE, reverse),
            (START, 0.081),
            (END, 0.942),
            (PAN, -1.0),
        ]);
        assert!(pair.exact_block(17, &[note(70, 0.8)], "fractional attack"));
        pair.set(&[(GAIN, -7.0), (PAN, 1.0), (SUSTAIN, 0.4)]);
        for frames in [63, 1, 64, 127, 17] {
            pair.exact_block(frames, &[], "event-free edge and gain ramp");
        }
        assert_eq!(pair.actual.active_voices(), 0);
    }
}

#[test]
fn render_matches_main_for_one_ulp_pan_change_with_three_voices() {
    let chord = [note(59, 0.7), note(61, 0.8), note(64, 0.9)];
    // At unity the quieter channel crosses an exponent boundary. One ULP above
    // unity, its change instead shares the same bits after discarding the low bit.
    for (gain, differs_after_shift) in [(0.0, true), (6e-7, false)] {
        let mut pair = RenderPair::new(4096);
        pair.set(&[(GAIN, gain)]);
        assert!(pair.exact_block(17, &chord, "start three notes"));
        assert!(pair.exact_block(64, &[], "settled output"));
        assert_eq!(pair.actual.active_voices(), 3);
        for (pan, distances) in [(1e-7, [1, 0]), (-1e-7, [0, 1])] {
            pair.set(&[(PAN, pan)]);
            let before = pair.actual.out.map(f32::to_bits);
            let target = pair.actual.c.out.map(f32::to_bits);
            let steps = std::array::from_fn(|ch| before[ch].abs_diff(target[ch]));
            assert_eq!(steps, distances);
            assert_eq!(
                before.map(|bits| bits >> 1) != target.map(|bits| bits >> 1),
                differs_after_shift
            );
            assert!(pair.exact_block(1, &[], "snap a one-ulp output change"));
            assert_eq!(pair.actual.out.map(f32::to_bits), target);
            pair.set(&[(PAN, 0.0)]);
            assert!(pair.exact_block(1, &[], "restore centered pan"));
        }
        assert!(pair.exact_block(63, &[], "settled at the new gain"));
        pair.set(&[(FILTER_TYPE, 1.0)]);
        assert!(pair.exact_block(17, &[], "enable filter between control ticks"));
    }
}

#[test]
fn render_matches_main_with_a_hole_before_the_last_note() {
    let mut pair = RenderPair::new(4096);
    pair.set(&[(END, 0.01)]);
    pair.exact_block(17, &[note(60, 0.7)], "short note in slot zero");
    pair.set(&[(END, 1.0)]);
    pair.exact_block(64, &[note(62, 0.8)], "long note in slot one");
    assert_eq!(pair.actual.voices.v[0].stage, Stage::Off);
    assert_eq!(pair.actual.active_voices(), 1);
    assert!(pair.exact_block(63, &[], "sparse render with slot zero off"));
}

fn note(key: u8, velocity: f32) -> NoteEvent {
    NoteEvent {
        offset: 0,
        key,
        velocity,
        on: true,
        slide: false,
        id: u64::from(key),
    }
}

#[test]
fn reenabled_filter_matches_eager_at_every_control_offset() {
    for offset in 0..usize::from(CONTROL_FRAMES) {
        let mut pair = RenderPair::new(2048);
        let context = format!("control offset {offset}");
        pair.set(&[
            (FILTER_TYPE, 1.0),
            (CUTOFF, 1_400.0),
            (RES, 0.25),
            (FILTER_ENV, 0.55),
            (FLT_ATTACK, 12.0),
            (VEL_FILTER, 0.3),
        ]);
        pair.block(17, &[note(59, 0.6)], &context);
        pair.set(&[(FILTER_TYPE, 0.0)]);
        pair.block(1, &[], &context);
        pair.set(&[
            (CUTOFF, 3_200.0),
            (RES, 0.72),
            (FILTER_ENV, -0.33),
            (FLT_ATTACK, 1.0),
            (FLT_DECAY, 2.0),
            (FLT_SUSTAIN, 0.19),
            (FLT_RELEASE, 8.0),
            (VEL_FILTER, 0.61),
        ]);
        let later_note = NoteEvent {
            offset: 3,
            ..note(62, 0.31)
        };
        pair.block(14 + offset, &[later_note], &context);

        // Between ticks, both the newly requested damping and the moving cutoff differ from
        // the values the eager renderer used. Keep the first filtered frame in its own block.
        let kind = (offset % 3 + 1) as f32;
        pair.set(&[
            (CUTOFF, 660.0),
            (RES, 0.12),
            (FILTER_ENV, 0.91),
            (FILTER_TYPE, kind),
        ]);
        pair.block(0, &[], &context);
        assert!(pair.block(1, &[], &context));
        for frames in [63, 64, 127] {
            assert!(pair.block(frames, &[], &context));
        }
    }
}

#[test]
fn second_switch_on_without_a_bypassed_tick_matches_eager() {
    let mut pair = RenderPair::new(4096);
    pair.set(&[
        (FILTER_TYPE, 1.0),
        (CUTOFF, 900.0),
        (RES, 0.6),
        (FILTER_ENV, 0.8),
        (FLT_ATTACK, 30.0),
    ]);
    pair.block(17, &[note(60, 0.9)], "initial filtered note");
    pair.set(&[(FILTER_TYPE, 0.0)]);
    pair.block(16, &[], "bypass across a tick");
    pair.set(&[(FILTER_TYPE, 1.0)]);
    pair.block(16, &[], "apply pending inputs, then tick with filtering on");
    pair.set(&[(FILTER_TYPE, 0.0)]);
    pair.block(1, &[], "bypass between ticks");
    pair.set(&[(FILTER_TYPE, 1.0)]);
    assert!(pair.block(64, &[], "second switch-on"));
}

#[test]
fn bypass_matches_eager_across_loop_glide_choke_and_reset() {
    for mode in [1.0, 2.0] {
        let mut pair = RenderPair::new(192);
        pair.set(&[
            (VOICE_MODE, mode),
            (GLIDE, 2.5),
            (REVERSE, 1.0),
            (LOOP, 1.0),
            (LOOP_START, 0.7),
            (FILTER_ENV, 0.66),
            (CUTOFF, 2_400.0),
            (FLT_ATTACK, 0.5),
            (FLT_DECAY, 1.3),
            (FLT_SUSTAIN, 0.35),
            (FLT_RELEASE, 10.0),
            (VEL_FILTER, 0.67),
        ]);
        pair.block(63, &[note(60, 0.4)], "bypassed reverse note");
        pair.block(64, &[note(67, 0.8)], "glide to higher note");
        pair.set(&[(RES, 0.73), (FILTER_TYPE, 3.0)]);
        assert!(pair.block(127, &[], "filter during glide and wrap"));
        pair.set(&[(FILTER_TYPE, 0.0)]);
        let off = NoteEvent {
            on: false,
            ..note(67, 0.0)
        };
        pair.block(17, &[off], "glide back to held note during bypass");
        let off = NoteEvent {
            on: false,
            ..note(60, 0.0)
        };
        pair.block(17, &[off], "both envelopes release during bypass");
        pair.actual.choke();
        pair.eager.choke();
        pair.set(&[(FILTER_TYPE, 2.0)]);
        assert!(pair.block(17, &[], "filter the parked voice"));
        pair.set(&[(FILTER_TYPE, 0.0)]);
        pair.block(17, &[], "bypassed parked voice");
        pair.actual.reset();
        pair.eager.reset();
        pair.set(&[(FILTER_TYPE, 1.0)]);
        assert!(!pair.block(1, &[], "reset clears parked voices"));
        assert!(pair.block(127, &[note(61, 0.77)], "new voice after reset"));
    }
}
