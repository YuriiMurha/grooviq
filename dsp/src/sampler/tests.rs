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
        let (mut l, mut r, mut eager_l, mut eager_r) =
            ([0.0; 127], [0.0; 127], [0.0; 127], [0.0; 127]);
        self.actual
            .process(&mut l[..frames], &mut r[..frames], None, events);
        self.eager
            .render::<true>(&mut eager_l[..frames], &mut eager_r[..frames], events);
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
