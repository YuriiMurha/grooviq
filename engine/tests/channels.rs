//! Sampler channels: mix, preview notes, the controller's checks and off-thread freeing.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    AudioProcessor, BLOCK, ChannelId, ChannelMix, ChannelSetup, Command, ControlError, Controller,
    EngineConfig, EngineState, HealthEvent, MAX_CHANNELS, NoteId, PatternData, STEP_TICKS,
    SampleBuf, SeqNote, SourceEdit, engine, engine_with, render_offline_with,
};
use std::f32::consts::SQRT_2;
use std::sync::Arc;

const SR: f32 = 48_000.0;
/// Frames in the 5 ms mix ramp.
const RAMP: usize = 240;
/// Index of the sampler's declick switch, and its output gain in dB.
const DECLICK: usize = 14;
const GAIN: u32 = 12;

fn cfg() -> EngineConfig {
    EngineConfig { sample_rate: SR }
}

/// A short click with different left and right sides.
fn click() -> Arc<SampleBuf> {
    let l: Vec<f32> = (0..200).map(|i| 0.9 * libm::powf(0.97, i as f32)).collect();
    let r: Vec<f32> = (0..200)
        .map(|i| -0.6 * libm::powf(0.95, i as f32))
        .collect();
    Arc::new(SampleBuf { rate: SR, l, r })
}

/// Two seconds of a constant `level`.
fn dc(level: f32) -> Arc<SampleBuf> {
    let n = 2 * SR as usize;
    Arc::new(SampleBuf {
        rate: SR,
        l: vec![level; n],
        r: vec![level; n],
    })
}

/// Sampler parameters at their defaults with declick off, so the output is the sample itself.
fn exact() -> Vec<f32> {
    let mut p: Vec<f32> = SPEC.iter().map(|&(_, _, d)| d).collect();
    p[DECLICK] = 0.0;
    p
}

fn channel(id: u32, sample: &Arc<SampleBuf>, mix: ChannelMix) -> ChannelSetup {
    ChannelSetup::sampler(ChannelId(id), Some(Arc::clone(sample)), exact(), mix)
}

fn state(channels: Vec<ChannelSetup>) -> EngineState {
    EngineState {
        channels,
        ..EngineState::default()
    }
}

fn note(id: u32, on: bool) -> Command {
    Command::ChannelNote {
        channel: ChannelId(id),
        key: 60,
        velocity: 1.0,
        on,
        id: NoteId(60),
    }
}

fn mix(id: u32, mix: ChannelMix) -> Command {
    Command::SetChannelMix {
        channel: ChannelId(id),
        mix,
    }
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

fn right(inter: &[f32]) -> Vec<f32> {
    inter.iter().skip(1).step_by(2).copied().collect()
}

fn render(state: EngineState, events: &[(u64, Command)], frames: usize) -> Vec<f32> {
    render_offline_with(cfg(), state, events, frames, 64)
}

/// The largest step between neighbouring samples.
fn max_step(x: &[f32]) -> f32 {
    x.windows(2).fold(0f32, |m, w| m.max((w[1] - w[0]).abs()))
}

#[test]
fn a_channel_plays_its_sample_on_a_note() {
    let s = click();
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    let out = render(st, &[(1_000, note(1, true))], 2_000);
    let (l, r) = (left(&out), right(&out));
    assert!(l[..1_000].iter().chain(&r[..1_000]).all(|x| *x == 0.0));
    assert_eq!(&l[1_000..1_200], &s.l[..], "left, bit for bit");
    assert_eq!(&r[1_000..1_200], &s.r[..], "right, bit for bit");
    assert!(l[1_200..].iter().chain(&r[1_200..]).all(|x| *x == 0.0));
}

#[test]
fn volume_pan_mute_and_solo_shape_the_mix() {
    let a = dc(0.25);
    let b = dc(0.5);
    // Both channels sound from frame 0; the change applies at frame 4800, a block start.
    let after = |chans: Vec<ChannelSetup>, change: &[Command]| {
        let mut ev: Vec<(u64, Command)> = chans.iter().map(|c| (0, note(c.id.0, true))).collect();
        ev.extend(change.iter().map(|c| (4_800, *c)));
        let out = render(state(chans), &ev, 9_600);
        let start = 4_800 + RAMP;
        (left(&out)[start..].to_vec(), right(&out)[start..].to_vec())
    };
    let all = |x: &[f32], v: f32| x.iter().all(|s| *s == v);
    let one = || vec![channel(1, &b, ChannelMix::default())];
    let two = || {
        vec![
            channel(1, &a, ChannelMix::default()),
            channel(2, &b, ChannelMix::default()),
        ]
    };
    let m = ChannelMix::default();

    let (l, r) = after(one(), &[mix(1, ChannelMix { volume: 0.5, ..m })]);
    assert!(all(&l, 0.25) && all(&r, 0.25), "volume 0.5 halves");

    let (l, r) = after(one(), &[mix(1, ChannelMix { pan: 1.0, ..m })]);
    assert!(
        all(&l, 0.0) && all(&r, 0.5 * SQRT_2),
        "pan right: {} {}",
        l[0],
        r[0]
    );

    let (l, r) = after(one(), &[mix(1, ChannelMix { mute: true, ..m })]);
    assert!(all(&l, 0.0) && all(&r, 0.0), "mute silences");

    let (l, _) = after(two(), &[]);
    assert!(all(&l, 0.75), "both sound");
    let (l, r) = after(two(), &[mix(2, ChannelMix { solo: true, ..m })]);
    assert!(all(&l, 0.5) && all(&r, 0.5), "solo B silences A");

    let muted_solo = ChannelMix {
        mute: true,
        solo: true,
        ..m
    };
    let (l, r) = after(one(), &[mix(1, muted_solo)]);
    assert!(all(&l, 0.0) && all(&r, 0.0), "mute and solo on one channel");
    let (l, r) = after(two(), &[mix(2, muted_solo)]);
    assert!(
        all(&l, 0.0) && all(&r, 0.0),
        "and it still silences the others"
    );

    // Unsoloing brings A back.
    let (l, _) = after(
        two(),
        &[
            mix(2, ChannelMix { solo: true, ..m }),
            mix(2, ChannelMix { solo: false, ..m }),
        ],
    );
    assert!(all(&l, 0.75), "unsolo");
}

#[test]
fn adding_or_removing_a_soloed_channel_silences_or_restores_the_others() {
    let (a, b) = (dc(0.25), dc(0.5));
    let soloed = ChannelMix {
        solo: true,
        ..ChannelMix::default()
    };
    let (mut c, mut p) = engine_with(cfg(), state(vec![channel(1, &a, ChannelMix::default())]));
    c.send(note(1, true)).unwrap();
    let mut buf = vec![0f32; 640 * 2];
    p.process(&mut buf, 2);
    c.add_channel(channel(2, &b, soloed)).unwrap();
    c.send(note(2, true)).unwrap();
    p.process(&mut buf, 2);
    // The soloed newcomer sounds at once; channel 1 ramps out over 5 ms.
    assert_eq!(left(&buf)[0], 0.5 + 0.25 * (1.0 - 1.0 / RAMP as f32));
    assert!(left(&buf)[RAMP..].iter().all(|x| *x == 0.5), "A ramps out");
    c.remove_channel(ChannelId(2)).unwrap();
    p.process(&mut buf, 2);
    assert!(
        left(&buf)[RAMP..].iter().all(|x| *x == 0.25),
        "A comes back"
    );
    assert!(max_step(&left(&buf)) <= 0.25 / RAMP as f32 + 1e-6);
}

#[test]
fn channels_sum_in_insertion_order() {
    // Values where f32 addition is not associative: the order shows in the result.
    let (a, b, c) = (dc(1.0), dc(4e-8), dc(-1.0));
    let in_order = (1.0f32 + 4e-8) + -1.0;
    let reversed = (-1.0f32 + 4e-8) + 1.0;
    assert_ne!(in_order, reversed);
    let st = state(vec![
        channel(1, &a, ChannelMix::default()),
        channel(2, &b, ChannelMix::default()),
        channel(3, &c, ChannelMix::default()),
    ]);
    let ev: Vec<_> = (1..=3).map(|id| (0, note(id, true))).collect();
    let out = render(st, &ev, 128);
    assert!(
        out.iter().all(|x| *x == in_order),
        "{} {}",
        out[0],
        in_order
    );

    // Removing a channel keeps the others in their order (a swap would move the last one up).
    let gone = dc(0.5);
    let st = state(vec![
        channel(1, &a, ChannelMix::default()),
        channel(9, &gone, ChannelMix::default()),
        channel(2, &b, ChannelMix::default()),
        channel(3, &c, ChannelMix::default()),
    ]);
    let (mut ctl, mut p) = engine_with(cfg(), st);
    ctl.remove_channel(ChannelId(9)).unwrap();
    for id in 1..=3 {
        ctl.send(note(id, true)).unwrap();
    }
    let mut out = vec![0f32; 128 * 2];
    p.process(&mut out, 2);
    let swapped = (1.0f32 + -1.0) + 4e-8;
    assert_ne!(in_order, swapped);
    assert!(
        out.iter().all(|x| *x == in_order),
        "after a removal: {}",
        out[0]
    );
}

#[test]
fn a_mix_change_while_silent_is_complete_by_the_next_note() {
    let s = dc(1.0);
    let half = ChannelMix {
        volume: 0.5,
        ..ChannelMix::default()
    };
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    let out = render(st, &[(0, mix(1, half)), (640, note(1, true))], 1_280);
    assert!(left(&out)[640..].iter().all(|x| *x == 0.5));
}

#[test]
fn a_channel_starts_at_its_mix_and_values_are_clamped() {
    let b = dc(0.5);
    let loud = ChannelMix {
        volume: 9.0,
        pan: -4.0,
        ..ChannelMix::default()
    };
    let out = render(
        state(vec![channel(1, &b, loud)]),
        &[(0, note(1, true))],
        640,
    );
    // Volume 2, hard left from the first frame: no fade-in on creation.
    assert!(left(&out).iter().all(|x| *x == 2.0 * 0.5 * SQRT_2));
    assert!(right(&out).iter().all(|x| *x == 0.0));
}

#[test]
fn mix_changes_ramp_without_clicks() {
    let s = dc(1.0);
    let quiet = ChannelMix {
        volume: 0.0,
        ..ChannelMix::default()
    };
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    let out = render(st, &[(0, note(1, true)), (4_800, mix(1, quiet))], 9_600);
    let l = left(&out);
    let limit = 1.0 / (0.005 * SR) + 1e-6;
    assert!(max_step(&l) <= limit, "step {}", max_step(&l));
    assert_eq!(l[4_799], 1.0);
    assert!(
        l[4_800 + RAMP..].iter().all(|x| *x == 0.0),
        "reaches silence"
    );
}

#[test]
fn all_sound_off_chokes_channels_and_later_notes_play() {
    let s = dc(1.0);
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    let ev = [
        (0, note(1, true)),
        (4_800, Command::AllSoundOff),
        (9_600, note(1, true)),
    ];
    let l = left(&render(st.clone(), &ev, 12_000));
    // Each voice's 3 ms choke fade.
    let limit = 1.0 / (0.003 * SR) + 1e-6;
    assert!(max_step(&l[..9_600]) <= limit, "step {}", max_step(&l));
    assert!(l[4_800 + 144..9_600].iter().all(|x| *x == 0.0));
    assert!(l[9_600..].iter().all(|x| *x == 1.0), "a later note plays");

    // AllSoundOff while nothing sounds leaves no fade behind for the next note.
    let ev = [(0, Command::AllSoundOff), (4_800, note(1, true))];
    let l = left(&render(st.clone(), &ev, 6_000));
    assert!(l[4_800..].iter().all(|x| *x == 1.0));

    // A note just before AllSoundOff, in its block (which it applies at the start of), never
    // starts.
    let ev = [(4_801, note(1, true)), (4_802, Command::AllSoundOff)];
    let l = left(&render(st, &ev, 6_000));
    assert!(l.iter().all(|x| *x == 0.0));
}

#[test]
fn a_note_right_after_all_sound_off_plays_at_full_level() {
    let s = dc(1.0);
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    // AllSoundOff applies at its block's start (4 800); the note lands 1 ms later, while the
    // choked voice still fades.
    let ev = [
        (0, note(1, true)),
        (4_800, Command::AllSoundOff),
        (4_848, note(1, true)),
    ];
    let l = left(&render(st, &ev, 6_000));
    assert!(
        l[4_848..].iter().all(|x| *x >= 1.0),
        "full level, plus the fading voice"
    );
    assert!(
        l[4_800 + 144..].iter().all(|x| *x == 1.0),
        "the new note alone"
    );
}

#[test]
fn a_fade_that_outlives_its_voice_ends_before_the_next_note() {
    // A one-shot that ends by itself 72 frames into the AllSoundOff fade: the channel goes
    // silent with the fade half done, and the next note must still play in full.
    let s = click();
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    let ev = [
        (0, note(1, true)),
        (128, Command::AllSoundOff),
        (4_800, note(1, true)),
    ];
    let out = render(st, &ev, 6_000);
    let (l, r) = (left(&out), right(&out));
    assert_eq!(&l[4_800..5_000], &s.l[..], "left, bit for bit");
    assert_eq!(&r[4_800..5_000], &s.r[..], "right, bit for bit");
}

#[test]
fn sampler_params_are_clamped_mirrored_and_audible() {
    let s = dc(0.5);
    let (mut c, _p) = engine_with(cfg(), state(vec![channel(1, &s, ChannelMix::default())]));
    let set = |value| Command::SetSamplerParam {
        channel: ChannelId(1),
        index: GAIN,
        value,
    };
    c.send(set(100.0)).unwrap();
    assert_eq!(
        c.state().channels[0].instrument.params()[GAIN as usize],
        12.0
    );
    assert!(c.send(set(f32::NAN)).is_err());
    let bad_index = Command::SetSamplerParam {
        channel: ChannelId(1),
        index: SPEC.len() as u32,
        value: 0.0,
    };
    assert!(c.send(bad_index).is_err());

    // -60 dB set before the note: the channel plays a thousand times quieter.
    let st = state(vec![channel(1, &s, ChannelMix::default())]);
    let out = render(st, &[(0, set(-60.0)), (0, note(1, true))], 640);
    let p = left(&out).iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!((p - 0.0005).abs() < 1e-6, "peak {p}");
}

/// The test's sample, a channel holding it, and a hand-driven engine with that channel.
fn held() -> (Arc<SampleBuf>, Controller, AudioProcessor) {
    let s = dc(0.5);
    let (c, p) = engine_with(cfg(), state(vec![channel(1, &s, ChannelMix::default())]));
    (s, c, p)
}

#[test]
fn removing_a_sounding_channel_frees_it_off_the_audio_thread() {
    let (s, mut c, mut p) = held();
    let mut buf = vec![0f32; 256 * 2];
    c.send(note(1, true)).unwrap();
    p.process(&mut buf, 2);
    assert!(buf.iter().any(|x| *x != 0.0), "it sounds");
    // The test, the controller's state and the graph's node.
    assert_eq!(Arc::strong_count(&s), 3);
    c.remove_channel(ChannelId(1)).unwrap();
    assert_eq!(Arc::strong_count(&s), 2, "the state let go");
    p.process(&mut buf, 2);
    assert!(
        buf[RAMP * 2..].iter().all(|x| *x == 0.0),
        "faded out in 5 ms"
    );
    assert_eq!(
        Arc::strong_count(&s),
        2,
        "the node waits in the trash queue"
    );
    c.check_health();
    assert_eq!(Arc::strong_count(&s), 1, "freed by the controller");
    assert!(c.state().channels.is_empty());
    assert!(c.send(note(1, true)).is_err(), "the channel is gone");
}

#[test]
fn an_extra_sounding_channel_continues_when_fade_capacity_is_full() {
    use grooviq_engine::{PlaybackMode, TimelineUpdate};

    fn timeline(remove_channel: Option<ChannelId>) -> TimelineUpdate {
        TimelineUpdate {
            pattern: Box::new(PatternData {
                length: 480,
                notes: vec![],
            }),
            song: Box::default(),
            bpm: 110.0,
            mode: PlaybackMode::Song,
            region: None,
            remove_channel,
        }
    }

    let sample = dc(0.5);
    let sounding = MAX_CHANNELS as u32;
    let id = ChannelId(sounding);
    let setup = channel(sounding, &sample, ChannelMix::default());
    let expected = render(
        state(vec![setup.clone()]),
        &[(0, note(sounding, true))],
        8 * BLOCK,
    );
    assert!(expected.iter().all(|x| *x > 0.4));
    for remove in [
        Controller::remove_channel,
        |c: &mut Controller, id| c.set_timeline(timeline(Some(id))),
        |c: &mut Controller, id| {
            c.set_sources_and_mix(SourceEdit {
                remove: vec![id],
                add: vec![],
                timeline: Some(timeline(None)),
                mix: Default::default(),
            })
            .map(|_| ())
        },
    ] {
        let (mut c, mut p) = engine_with(
            cfg(),
            state(
                (0..sounding)
                    .map(|id| channel(id, &sample, ChannelMix::default()))
                    .collect(),
            ),
        );
        let rev = c
            .set_sources_and_mix(SourceEdit {
                remove: (0..sounding).map(ChannelId).collect(),
                add: vec![setup.clone()],
                timeline: None,
                mix: Default::default(),
            })
            .unwrap();
        c.send(note(sounding, true)).unwrap();
        let mut block = [0.0; BLOCK * 2];
        p.process(&mut block, 2);
        assert_eq!(
            block,
            expected[..BLOCK * 2],
            "the extra channel already sounds"
        );
        let old_state = c.state().clone();
        let instance = c.channel_instance(id);

        let refused = remove(&mut c, id);
        let mut continued = [0.0; 3 * BLOCK * 2];
        p.process(&mut continued, 2);
        assert_eq!(
            continued[0],
            expected[BLOCK * 2],
            "the first sample at full fade capacity must continue"
        );
        assert_eq!(continued, expected[BLOCK * 2..4 * BLOCK * 2]);
        assert_eq!(refused, Err(ControlError::Busy));
        assert_eq!(c.state(), &old_state);
        assert_eq!(c.channel_instance(id), instance);
        assert_eq!(c.mix_revision(), rev);

        // The next admission collects returned channels itself. Compare every sample with
        // the uninterrupted render times a 5 ms linear ramp, including the first silent frame.
        remove(&mut c, id).unwrap();
        let mut faded = [0.0; 4 * BLOCK * 2];
        p.process(&mut faded, 2);
        for (frame, (actual, dry)) in faded
            .as_chunks::<2>()
            .0
            .iter()
            .zip(expected[4 * BLOCK * 2..].as_chunks::<2>().0)
            .enumerate()
        {
            let gain = RAMP.saturating_sub(frame + 1) as f32 / RAMP as f32;
            if gain == 0.0 {
                assert_eq!(*actual, [0.0; 2], "frame {frame}");
            } else {
                for (&actual, &dry) in actual.iter().zip(dry) {
                    assert!(
                        (actual - dry * gain).abs() < 2e-6,
                        "frame {frame}: {actual}"
                    );
                }
            }
        }
        assert!(c.channel_instance(id).is_none());
    }
}

#[test]
fn replacing_a_sample_hands_the_old_one_back() {
    let (old, mut c, mut p) = held();
    let new = click();
    let mut buf = vec![0f32; 256 * 2];
    c.send(note(1, true)).unwrap();
    p.process(&mut buf, 2);
    c.set_channel_sample(ChannelId(1), Some(Arc::clone(&new)))
        .unwrap();
    assert_eq!(Arc::strong_count(&old), 2);
    p.process(&mut buf, 2);
    assert_eq!(Arc::strong_count(&old), 2, "not freed inside process");
    assert!(
        buf.iter().all(|x| *x == 0.0),
        "the new sample silences the old voices"
    );
    c.check_health();
    assert_eq!(Arc::strong_count(&old), 1);
    assert_eq!(Arc::strong_count(&new), 3);
    // The new sample plays.
    c.send(note(1, true)).unwrap();
    p.process(&mut buf, 2);
    assert_eq!(left(&buf)[..200], new.l[..]);

    // Clearing the sample hands the new one back too, and the channel stays silent.
    c.set_channel_sample(ChannelId(1), None).unwrap();
    p.process(&mut buf, 2);
    c.check_health();
    assert_eq!(Arc::strong_count(&new), 1);
    c.send(note(1, true)).unwrap();
    p.process(&mut buf, 2);
    assert!(buf.iter().all(|x| *x == 0.0));
}

#[test]
fn unknown_channels_and_the_257th_channel_are_refused() {
    let s = click();
    let (mut c, mut p) = engine(cfg());
    let missing = ChannelId(9);
    for cmd in [
        note(9, true),
        mix(9, ChannelMix::default()),
        Command::SetSamplerParam {
            channel: missing,
            index: 0,
            value: 60.0,
        },
    ] {
        assert_eq!(c.send(cmd), Err(cmd));
    }
    assert_eq!(c.remove_channel(missing), Err(ControlError::UnknownChannel));
    assert_eq!(
        c.set_channel_sample(missing, Some(Arc::clone(&s))),
        Err(ControlError::UnknownChannel)
    );
    assert_eq!(Arc::strong_count(&s), 1, "nothing kept the sample");

    for id in 0..MAX_CHANNELS as u32 {
        c.add_channel(channel(id, &s, ChannelMix::default()))
            .unwrap();
        // The queue only holds so many: let the processor take them.
        p.process(&mut [0f32; 128], 2);
    }
    assert_eq!(
        Arc::strong_count(&s),
        1 + 2 * MAX_CHANNELS,
        "state + graph each"
    );
    let before = c.state().clone();
    assert_eq!(
        c.add_channel(channel(3, &s, ChannelMix::default())),
        Err(ControlError::ChannelExists)
    );
    assert_eq!(
        c.add_channel(channel(MAX_CHANNELS as u32, &s, ChannelMix::default())),
        Err(ControlError::TooManyChannels)
    );
    assert_eq!(*c.state(), before, "the state is unchanged");
    drop(before);
    assert_eq!(
        Arc::strong_count(&s),
        1 + 2 * MAX_CHANNELS,
        "nothing new holds the sample"
    );

    // A non-finite value is refused too.
    c.remove_channel(ChannelId(0)).unwrap();
    let nan = ChannelSetup {
        mix: ChannelMix {
            volume: f32::NAN,
            ..ChannelMix::default()
        },
        ..channel(0, &s, ChannelMix::default())
    };
    assert_eq!(c.add_channel(nan), Err(ControlError::Rejected));
}

#[test]
fn nan_in_one_channel_silences_only_that_channel() {
    let mut bad = (*dc(0.25)).clone();
    bad.l[0] = f32::NAN;
    let bad = Arc::new(bad);
    let b = dc(0.5);
    let chans = vec![
        channel(1, &bad, ChannelMix::default()),
        channel(2, &b, ChannelMix::default()),
    ];
    let (mut c, mut p) = engine_with(cfg(), state(chans));
    c.send(note(1, true)).unwrap();
    c.send(note(2, true)).unwrap();
    let mut buf = vec![0f32; 1_024 * 2];
    p.process(&mut buf, 2);
    assert!(buf.iter().all(|x| *x == 0.5), "only B is heard");
    assert_eq!(c.live().guard_hits, 1);
    assert!(
        c.check_health()
            .contains(&HealthEvent::DeviceSilenced { total: 1 })
    );
    // The guard reset A's sampler but kept its sample: its next note trips the guard again.
    c.send(note(1, true)).unwrap();
    p.process(&mut buf, 2);
    assert_eq!(c.live().guard_hits, 2);
    assert_eq!(Arc::strong_count(&bad), 3);
}

/// Three channels and a busy minute of commands for them.
fn busy() -> (EngineState, Vec<(u64, Command)>) {
    let (a, b, k) = (dc(0.3), click(), click());
    let st = state(vec![
        channel(1, &a, ChannelMix::default()),
        // Defaults, declick on.
        ChannelSetup::sampler(
            ChannelId(2),
            Some(Arc::clone(&b)),
            Vec::new(),
            ChannelMix {
                pan: -0.3,
                ..ChannelMix::default()
            },
        ),
        channel(3, &k, ChannelMix::default()),
    ]);
    let mut ev = Vec::new();
    for i in 0..40u64 {
        let frame = i * 997 + 13;
        let id = (i % 3) as u32 + 1;
        ev.push((
            frame,
            Command::ChannelNote {
                channel: ChannelId(id),
                key: 48 + (i % 24) as u8,
                velocity: 0.5 + 0.01 * i as f32,
                on: true,
                id: NoteId(u64::from(48 + (i % 24) as u8)),
            },
        ));
        ev.push((frame + 400, note(id, false)));
        if i % 5 == 0 {
            let m = ChannelMix {
                volume: 0.2 + 0.04 * i as f32,
                pan: libm::sinf(i as f32 * 0.37),
                mute: i % 15 == 5,
                solo: i % 20 == 10,
            };
            ev.push((frame + 100, mix(id, m)));
            ev.push((
                frame + 200,
                Command::SetSamplerParam {
                    channel: ChannelId(id),
                    index: 1,
                    value: (i % 7) as f32 - 3.0,
                },
            ));
        }
    }
    (st, ev)
}

#[test]
fn output_is_identical_for_any_callback_size_with_channels() {
    let (st, ev) = busy();
    let frames = 41_000;
    let reference = render_offline_with(cfg(), st.clone(), &ev, frames, 1);
    assert!(reference.iter().any(|x| *x != 0.0));
    for callback in [37, 64, 441, 4_096] {
        let out = render_offline_with(cfg(), st.clone(), &ev, frames, callback);
        assert!(out == reference, "callback {callback}");
    }
}

fn pattern() -> PatternData {
    let n = |tick, channel, key| SeqNote {
        tick,
        channel: ChannelId(channel),
        key,
        velocity: 0.8,
        length: STEP_TICKS,
        slide: false,
    };
    PatternData {
        length: 8 * STEP_TICKS,
        notes: vec![n(0, 2, 60), n(STEP_TICKS, 3, 62), n(STEP_TICKS, 2, 61)],
    }
}

fn sorted(mut p: PatternData) -> PatternData {
    p.notes.sort_by_key(|n| (n.tick, n.channel));
    p
}

#[test]
fn load_state_replaces_everything() {
    let (old, mut c, mut p) = held();
    let mut buf = vec![0f32; 256 * 2];
    c.send(Command::Play).unwrap();
    c.send(note(1, true)).unwrap();
    p.process(&mut buf, 2);
    assert!(c.live().playing);

    let (two, three) = (click(), dc(0.1));
    let new = EngineState {
        bpm: 97.0,
        channels: vec![
            channel(2, &two, ChannelMix::default()),
            channel(
                3,
                &three,
                ChannelMix {
                    volume: 0.5,
                    ..ChannelMix::default()
                },
            ),
        ],
        pattern: sorted(pattern()),
        ..EngineState::default()
    };
    c.load_state(new.clone()).unwrap();
    assert_eq!(*c.state(), new);
    p.process(&mut buf, 2);
    assert!(!c.live().playing, "load_state stops the transport");
    assert!(buf.iter().all(|x| *x == 0.0), "the old channel is cut");
    c.check_health();
    assert_eq!(Arc::strong_count(&old), 1, "the old graph was freed");
    assert!(c.send(note(1, true)).is_err(), "the old channel is gone");
    c.send(note(2, true)).unwrap();
    p.process(&mut buf, 2);
    assert_eq!(left(&buf)[..200], two.l[..], "the new one plays");

    // A refused state changes nothing.
    let dup = EngineState {
        channels: vec![
            channel(5, &two, ChannelMix::default()),
            channel(5, &two, ChannelMix::default()),
        ],
        ..EngineState::default()
    };
    assert_eq!(c.load_state(dup), Err(ControlError::ChannelExists));
    let many = state(
        (0..=MAX_CHANNELS as u32)
            .map(|id| channel(id, &two, ChannelMix::default()))
            .collect(),
    );
    assert_eq!(c.load_state(many), Err(ControlError::TooManyChannels));
    assert_eq!(*c.state(), new);
}

#[test]
fn set_pattern_is_checked_sorted_and_mirrored() {
    let (_s, mut c, _p) = held();
    c.set_pattern(pattern()).unwrap();
    let ticks: Vec<(u32, u32)> = c
        .state()
        .pattern
        .notes
        .iter()
        .map(|n| (n.tick, n.channel.0))
        .collect();
    assert_eq!(ticks, [(0, 2), (STEP_TICKS, 2), (STEP_TICKS, 3)]);
    assert_eq!(c.state().pattern.length, 8 * STEP_TICKS);

    let before = c.state().pattern.clone();
    let empty = PatternData {
        length: 0,
        notes: Vec::new(),
    };
    assert_eq!(c.set_pattern(empty), Err(ControlError::Rejected));
    let mut bad = pattern();
    bad.notes[0].velocity = f32::NAN;
    assert_eq!(c.set_pattern(bad), Err(ControlError::Rejected));
    assert_eq!(c.state().pattern, before);

    let d = EngineState::default();
    assert_eq!((d.bpm, d.pattern.length), (140.0, 16 * STEP_TICKS));
    assert!(d.channels.is_empty() && d.pattern.notes.is_empty());
}

#[test]
fn render_offline_with_matches_a_hand_driven_engine_with() {
    let (st, _) = busy();
    let at = [
        (0u64, note(1, true)),
        (0, note(2, true)),
        (
            640,
            mix(
                1,
                ChannelMix {
                    pan: 0.5,
                    ..ChannelMix::default()
                },
            ),
        ),
        (1_280, note(3, true)),
        (1_280, note(1, false)),
    ];
    let frames = 4_096;
    let offline = render_offline_with(cfg(), st.clone(), &at, frames, 441);

    let (mut c, mut p) = engine_with(cfg(), st);
    let mut out = vec![0f32; frames * 2];
    let mut pos = 0;
    for (frame, cmd) in at {
        let frame = frame as usize;
        p.process(&mut out[pos * 2..frame * 2], 2);
        pos = frame;
        c.send(cmd).unwrap();
    }
    p.process(&mut out[pos * 2..], 2);
    assert!(out == offline);
}

/// 4 800 frames (0.1 s) of 0.25 at 48 kHz.
fn short() -> Arc<SampleBuf> {
    Arc::new(SampleBuf {
        rate: SR,
        l: vec![0.25; 4_800],
        r: vec![0.25; 4_800],
    })
}

/// Frames from the start of `l` until it first stops being 0.25, and whether only silence
/// follows.
fn played(l: &[f32]) -> (usize, bool) {
    let n = l.iter().position(|&x| x != 0.25).unwrap_or(l.len());
    (n, l[n..].iter().all(|&x| x == 0.0))
}

#[test]
fn a_remove_and_add_queued_before_a_rate_change_are_dropped() {
    // Channel 1 plays `old` at 44.1 kHz. Removing it and adding a channel 1 with `new` (its
    // node built for 44.1 kHz) are still queued when the engine moves to 48 kHz. The graph
    // rebuilt for 48 kHz already has the new channel 1: the queued pair must not replace it
    // with the 44.1 kHz node, which would play `new` 1.5 semitones sharp.
    let (old, new) = (dc(0.5), short());
    let rate = |sample_rate| EngineConfig { sample_rate };
    let (mut c, mut p) = engine_with(
        rate(44_100.0),
        state(vec![channel(1, &old, ChannelMix::default())]),
    );
    c.remove_channel(ChannelId(1)).unwrap();
    c.add_channel(channel(1, &new, ChannelMix::default()))
        .unwrap();
    // The test, the controller's state and the queued node.
    assert_eq!(Arc::strong_count(&new), 3);
    c.reconfigure(&mut p, rate(SR));
    // The test, the state and the rebuilt node: the queued node was freed by reconfigure, on
    // this thread, as was the old graph.
    assert_eq!(Arc::strong_count(&new), 3);
    assert_eq!(Arc::strong_count(&old), 1);
    c.send(note(1, true)).unwrap();
    let mut out = vec![0f32; 6_000 * 2];
    p.process(&mut out, 2);
    assert_eq!(played(&left(&out)), (4_800, true), "new, at its own pitch");
    c.check_health();
    assert_eq!(Arc::strong_count(&new), 3);
}

#[test]
fn edits_queued_before_a_new_project_and_a_rate_change_do_not_reach_it() {
    // For channel 1: another sample, tune +12, mute and a pattern that plays it; then a new
    // project whose channel 1 plays `s` untuned with an empty pattern. All still queued when
    // the engine restarts (at the same rate: only the queue's age tells them stale). On Play
    // and a note, only the note sounds: `s`, untuned and unmuted.
    let s = short();
    let (mut c, mut p) = engine_with(cfg(), state(vec![channel(1, &s, ChannelMix::default())]));
    c.set_channel_sample(ChannelId(1), Some(dc(0.5))).unwrap();
    let tune = Command::SetSamplerParam {
        channel: ChannelId(1),
        index: 1,
        value: 12.0,
    };
    c.send(tune).unwrap();
    let muted = ChannelMix {
        mute: true,
        ..ChannelMix::default()
    };
    c.send(mix(1, muted)).unwrap();
    c.set_pattern(PatternData {
        length: 16 * STEP_TICKS,
        notes: vec![SeqNote {
            tick: 2 * STEP_TICKS,
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            length: STEP_TICKS,
            slide: false,
        }],
    })
    .unwrap();
    c.load_state(state(vec![channel(1, &s, ChannelMix::default())]))
        .unwrap();
    c.reconfigure(&mut p, cfg());
    c.send(Command::Play).unwrap();
    c.send(note(1, true)).unwrap();
    // Step 2 lands before frame 16 000 at any tempo above 90 BPM.
    let mut out = vec![0f32; 16_000 * 2];
    p.process(&mut out, 2);
    assert!(c.live().playing);
    assert_eq!(
        played(&left(&out)),
        (4_800, true),
        "an octave up plays 2 400"
    );
    assert_eq!(c.state().channels[0].instrument.params()[1], 0.0);
}

fn synth_note(on: bool) -> Command {
    Command::Note {
        key: 60,
        velocity: 1.0,
        on,
        id: NoteId(60),
    }
}

#[test]
fn notes_queued_before_a_new_project_and_a_rate_change_do_not_sound() {
    // A preview note on channel 1 and a synth note, then a new project whose channel 1 plays
    // `new`; all still queued at a rate change. Without the rate change the new project
    // replaces the graph the notes went to, so nothing plays: the same holds with it.
    let (old, new) = (dc(0.5), short());
    let (mut c, mut p) = engine_with(cfg(), state(vec![channel(1, &old, ChannelMix::default())]));
    c.send(note(1, true)).unwrap();
    c.send(synth_note(true)).unwrap();
    c.load_state(state(vec![channel(1, &new, ChannelMix::default())]))
        .unwrap();
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44_100.0,
        },
    );
    let mut out = vec![0f32; 4_800 * 2];
    p.process(&mut out, 2);
    assert!(out.iter().all(|x| *x == 0.0), "nothing anyone asked for");
    c.send(note(1, true)).unwrap();
    p.process(&mut out, 2);
    assert_eq!(left(&out)[100], 0.25, "a new note plays the new channel 1");
}

#[test]
fn a_note_queued_before_its_channel_changes_does_not_survive_a_rate_change() {
    // Notes on channels 1, 2 and 3; then channel 1 gets a new sample and channel 3 is removed
    // and added again, which would cut their notes; all still queued when the engine
    // restarts. Only channel 2's note plays.
    let (half, quarter) = (dc(0.5), short());
    let (mut c, mut p) = engine_with(
        cfg(),
        state(vec![
            channel(1, &quarter, ChannelMix::default()),
            channel(2, &half, ChannelMix::default()),
            channel(3, &quarter, ChannelMix::default()),
        ]),
    );
    for id in 1..=3 {
        c.send(note(id, true)).unwrap();
    }
    c.set_channel_sample(ChannelId(1), Some(dc(0.5))).unwrap();
    c.remove_channel(ChannelId(3)).unwrap();
    c.add_channel(channel(3, &quarter, ChannelMix::default()))
        .unwrap();
    c.reconfigure(&mut p, cfg());
    let mut out = vec![0f32; 1_000 * 2];
    p.process(&mut out, 2);
    assert_eq!(left(&out)[500], 0.5, "channel 2 alone");
}

#[test]
fn a_synth_note_queued_before_a_rate_change_still_plays() {
    let (mut c, mut p) = engine(cfg());
    c.send(synth_note(true)).unwrap();
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44_100.0,
        },
    );
    let mut out = vec![0f32; 4_410 * 2];
    p.process(&mut out, 2);
    assert!(out.iter().any(|x| x.abs() > 0.01), "the synth plays it");
}

#[test]
fn sampler_trim_is_one_admitted_callback_update_and_rejects_stale_instances() {
    let sample = click();
    let setup = channel(1, &sample, ChannelMix::default());
    let (mut c, mut p) = engine_with(cfg(), state(vec![setup.clone()]));
    let mut params: [f32; grooviq_dsp::sampler::SPEC.len()] = exact().try_into().unwrap();
    params[3] = 0.25;
    params[4] = 0.75;
    params[12] = -6.0;
    let expected = ChannelSetup::sampler(
        ChannelId(1),
        Some(sample),
        params.to_vec(),
        ChannelMix::default(),
    );
    let (mut reference, mut reference_processor) = engine_with(cfg(), state(vec![expected]));
    let instance = c.channel_instance(ChannelId(1)).unwrap();
    assert!(
        c.send_at(
            0,
            Command::SetSamplerTrim {
                channel: ChannelId(1),
                instance: instance + 1,
                params
            }
        )
        .is_err()
    );
    assert_eq!(c.state().channels[0].instrument.params()[3], 0.0);
    c.send_at(
        0,
        Command::SetSamplerTrim {
            channel: ChannelId(1),
            instance,
            params,
        },
    )
    .unwrap();
    assert_eq!(c.state().channels[0].instrument.params(), params);
    c.send(note(1, true)).unwrap();
    reference.send(note(1, true)).unwrap();
    let mut out = [0.0; 512];
    let mut expected = [0.0; 512];
    p.process(&mut out, 2);
    reference_processor.process(&mut expected, 2);
    assert_eq!(
        out, expected,
        "callback must hear the entire final parameter bank before the note"
    );
    assert!(out.iter().any(|v| v.abs() > 0.001));
    // Invalid final values and a full queue leave the whole mirror intact.
    let before = c.state().channels[0].instrument.params().to_vec();
    let mut invalid = params;
    invalid[4] = f32::NAN;
    assert!(
        c.send_at(
            0,
            Command::SetSamplerTrim {
                channel: ChannelId(1),
                instance,
                params: invalid
            }
        )
        .is_err()
    );
    while c.send(Command::Stop).is_ok() {}
    params[3] = 0.0;
    params[4] = 1.0;
    assert!(
        c.send_at(
            0,
            Command::SetSamplerTrim {
                channel: ChannelId(1),
                instance,
                params
            }
        )
        .is_err()
    );
    assert_eq!(c.state().channels[0].instrument.params(), before);
}

#[test]
fn queued_trim_cannot_leak_into_reused_channel_after_load_or_rate_rebuild() {
    for (replace, rebuild) in [(false, false), (true, false), (false, true), (true, true)] {
        let sample = click();
        let original = channel(1, &sample, ChannelMix::default());
        let (mut c, mut p) = engine_with(cfg(), state(vec![original]));
        let old = c.channel_instance(ChannelId(1)).unwrap();
        let mut params: [f32; grooviq_dsp::sampler::SPEC.len()] = exact().try_into().unwrap();
        params[3] = 0.8;
        params[4] = 0.9;
        c.send_at(
            0,
            Command::SetSamplerTrim {
                channel: ChannelId(1),
                instance: old,
                params,
            },
        )
        .unwrap();
        let replacement = channel(1, &sample, ChannelMix::default());
        if replace {
            c.load_state(state(vec![replacement.clone()])).unwrap();
        } else {
            c.remove_channel(ChannelId(1)).unwrap();
            c.add_channel(replacement.clone()).unwrap();
        }
        assert_ne!(c.channel_instance(ChannelId(1)).unwrap(), old);
        assert!(
            c.send_at(
                0,
                Command::SetSamplerTrim {
                    channel: ChannelId(1),
                    instance: old,
                    params
                }
            )
            .is_err()
        );
        let rate = if rebuild { 44100.0 } else { SR };
        if rebuild {
            c.reconfigure(&mut p, EngineConfig { sample_rate: rate });
        }
        let (mut reference, mut processor) =
            engine_with(EngineConfig { sample_rate: rate }, state(vec![replacement]));
        c.send(note(1, true)).unwrap();
        reference.send(note(1, true)).unwrap();
        let mut actual = [0.0; 512];
        let mut expected = [0.0; 512];
        p.process(&mut actual, 2);
        processor.process(&mut expected, 2);
        assert_eq!(actual, expected, "replacement={replace} rebuild={rebuild}");
        assert!(actual.iter().any(|v| v.abs() > 0.01));
    }
}

#[test]
fn trim_pair_and_mode_stop_keep_their_effects_through_rebuild() {
    use grooviq_engine::{PlaybackMode, RecordEvent, SongData, TimelineUpdate};
    for timeline in [false, true] {
        let sample = click();
        let mut initial = state(vec![channel(1, &sample, ChannelMix::default())]);
        initial.song = SongData {
            end_tick: 96000,
            ..Default::default()
        };
        let (mut c, mut p) = engine_with(cfg(), initial);
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        c.send(note(1, true)).unwrap();
        p.process(&mut [0.0; 128], 2);
        assert!(c.live().playing);
        let mut params: [f32; SPEC.len()] = exact().try_into().unwrap();
        params[3] = 0.2;
        params[4] = 0.7;
        c.send(Command::SetSamplerTrim {
            channel: ChannelId(1),
            instance: c.channel_instance(ChannelId(1)).unwrap(),
            params,
        })
        .unwrap();
        let barrier = if timeline {
            let snapshot = c.state().clone();
            c.set_timeline(TimelineUpdate {
                pattern: Box::new(snapshot.pattern),
                song: Box::new(snapshot.song),
                bpm: snapshot.bpm,
                mode: PlaybackMode::Song,
                region: snapshot.song_loop,
                remove_channel: None,
            })
            .unwrap();
            c.send_marked(Command::SetTempo { bpm: 120.0 }).unwrap()
        } else {
            c.send_marked(Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            })
            .unwrap()
        };
        for rate in [44100.0, 96000.0] {
            c.reconfigure(&mut p, EngineConfig { sample_rate: rate });
        }
        p.process(&mut [0.0; 128], 2);
        assert!(!c.live().playing, "timeline={timeline}");
        assert!(!c.recording_armed());
        assert_eq!(c.state().channels[0].instrument.params(), params);
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        let end = events
            .iter()
            .position(|e| matches!(e, RecordEvent::End { .. }))
            .unwrap();
        let mark = events
            .iter()
            .position(|e| matches!(e, RecordEvent::Mark { id } if *id == barrier))
            .unwrap();
        assert!(end < mark);
        let (mut reference, mut rp) = engine_with(
            EngineConfig {
                sample_rate: 96000.0,
            },
            c.state().clone(),
        );
        c.send(note(1, true)).unwrap();
        reference.send(note(1, true)).unwrap();
        let mut actual = [0.0; 512];
        let mut expected = [0.0; 512];
        p.process(&mut actual, 2);
        rp.process(&mut expected, 2);
        assert_eq!(
            actual, expected,
            "the rebuilt callback retains the whole trim vector"
        );
        assert!(actual.iter().any(|v| v.abs() > 0.001));
    }
}
