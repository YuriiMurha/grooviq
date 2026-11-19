use grooviq_engine::*;
fn channel() -> ChannelSetup {
    ChannelSetup::sampler(ChannelId(1), None, vec![], ChannelMix::default())
}
fn clip() -> AutomationClip {
    AutomationClip {
        target: AutomationTarget::ChannelVolume(ChannelId(1)),
        priority: 3,
        start_tick: 0,
        offset_tick: 0,
        length_ticks: 960,
        points: vec![
            AutomationPoint {
                tick: 0,
                value: 0.0,
                segment: AutomationSegment::Linear,
            },
            AutomationPoint {
                tick: 960,
                value: 2.0,
                segment: AutomationSegment::Hold,
            },
        ],
    }
}
#[test]
fn native_linear_hold_and_duplicates() {
    let mut c = clip();
    c.points.insert(
        0,
        AutomationPoint {
            tick: 0,
            value: 1.0,
            segment: AutomationSegment::Hold,
        },
    );
    let s = AutomationSchedule::build(vec![c.clone()], &[channel()]).unwrap();
    assert_eq!(s.value_at(c.target, 0.0), Some(0.0));
    assert_eq!(s.value_at(c.target, 480.0), Some(1.0));
    assert_eq!(s.value_at(c.target, 960.0), None);
    c.points[2].segment = AutomationSegment::Hold;
    c.points[2].tick = 400;
    let s = AutomationSchedule::build(vec![c.clone()], &[channel()]).unwrap();
    assert_eq!(s.value_at(c.target, 800.0), Some(2.0));
}
#[test]
fn priority_slices_and_rejection() {
    let a = clip();
    let mut b = a.clone();
    b.start_tick = 100;
    b.length_ticks = 100;
    b.priority = 4;
    b.points[0].value = 1.5;
    b.points[0].segment = AutomationSegment::Hold;
    for clips in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
        let s = AutomationSchedule::build(clips, &[channel()]).unwrap();
        assert_eq!(s.value_at(a.target, 100.0), Some(1.5));
        assert_eq!(s.value_at(a.target, 200.0), Some(2.0 * 200.0 / 960.0));
    }
    b.priority = 3;
    assert!(AutomationSchedule::build(vec![a.clone(), b], &[channel()]).is_err());
    let mut left = a.clone();
    left.length_ticks = 400;
    let mut right = a.clone();
    right.start_tick = 400;
    right.offset_tick = 400;
    right.length_ticks = 560;
    let whole = AutomationSchedule::build(vec![a.clone()], &[channel()]).unwrap();
    let split = AutomationSchedule::build(vec![right, left], &[channel()]).unwrap();
    for tick in 0..960 {
        assert_eq!(
            whole.value_at(a.target, tick as f64),
            split.value_at(a.target, tick as f64)
        );
    }
}
#[test]
fn validation_is_independent() {
    for value in [f32::NAN, f32::INFINITY, -0.1, 2.1] {
        let mut c = clip();
        c.points[0].value = value;
        assert!(AutomationSchedule::build(vec![c], &[channel()]).is_err());
    }
    for value in [0.0, 2.0] {
        let mut c = clip();
        c.points[0].value = value;
        assert!(AutomationSchedule::build(vec![c], &[channel()]).is_ok());
    }
    let mut c = clip();
    c.length_ticks = 0;
    assert!(AutomationSchedule::build(vec![c], &[channel()]).is_err());
    assert!(AutomationSchedule::build(vec![clip()], &[]).is_err());
}
#[test]
fn fixed_grid_smoothing_is_callback_partition_invariant() {
    use std::sync::Arc;
    fn render(size: usize) -> Vec<f32> {
        let mut ch = channel();
        ch.instrument = Instrument::Sampler {
            sample: Some(Arc::new(SampleBuf {
                rate: 48000.0,
                l: vec![0.3; 48000],
                r: vec![0.3; 48000],
            })),
            params: vec![],
        };
        let schedule = AutomationSchedule::build(vec![clip()], std::slice::from_ref(&ch)).unwrap();
        let state = EngineState {
            bpm: 120.0,
            channels: vec![ch],
            playback_mode: PlaybackMode::Song,
            song: SongData {
                automation: schedule,
                end_tick: 1000,
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut control, mut processor) = engine_with(
            EngineConfig {
                sample_rate: 48000.0,
            },
            state,
        );
        control.send(Command::Play).unwrap();
        control
            .send(Command::ChannelNote {
                channel: ChannelId(1),
                key: 60,
                velocity: 1.0,
                on: true,
                id: NoteId::live(),
            })
            .unwrap();
        let mut out = vec![0.0; 30000 * 2];
        for chunk in out.chunks_mut(size * 2) {
            processor.process(chunk, 2);
        }
        out
    }
    let reference = render(64);
    assert!(reference.iter().any(|s| *s != 0.0));
    for size in [1, 63, 127, 128, 255, 256, 511] {
        assert_eq!(reference, render(size));
    }
}
#[test]
fn accepted_dense_and_sparse_admission_profile() {
    let channel = channel();
    for dense in [false, true] {
        let clips = (0..100_000)
            .map(|i| {
                let mut c = clip();
                c.priority = i;
                c.start_tick = if dense { 0 } else { u64::from(i) * 2 };
                c.length_ticks = 1;
                c.points.truncate(1);
                c
            })
            .collect();
        let start = std::time::Instant::now();
        let s = AutomationSchedule::build(clips, std::slice::from_ref(&channel)).unwrap();
        eprintln!("100000 clips dense={dense} build={:?}", start.elapsed());
        let start = std::time::Instant::now();
        let mut sum = 0.0;
        let target = clip().target;
        for i in 0..100_000 {
            sum += s.value_at(target, f64::from(i)).unwrap_or(0.0);
        }
        eprintln!("100000 indexed queries={:?} sum={sum}", start.elapsed());
    }
}
#[test]
fn automation_replacement_and_render_never_allocate_or_free() {
    let ch = channel();
    let s = AutomationSchedule::build(vec![clip()], std::slice::from_ref(&ch)).unwrap();
    let state = EngineState {
        channels: vec![ch],
        song: SongData {
            automation: s,
            end_tick: 1000,
            ..Default::default()
        },
        playback_mode: PlaybackMode::Song,
        ..Default::default()
    };
    let (mut controller, mut processor) = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        state,
    );
    controller.send(Command::Play).unwrap();
    let mut output = [0.0; 128];
    for _ in 0..80 {
        let song = controller.state().song.clone();
        controller.set_song(song).unwrap();
        assert_no_alloc::assert_no_alloc(|| processor.process(&mut output, 2));
    }
    controller.send(Command::Stop).unwrap();
    assert_no_alloc::assert_no_alloc(|| processor.process(&mut output, 2));
}
#[test]
fn referenced_channel_removal_and_wrong_instrument_are_atomic() {
    let ch = channel();
    let schedule = AutomationSchedule::build(vec![clip()], std::slice::from_ref(&ch)).unwrap();
    let (mut c, _) = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        EngineState {
            channels: vec![ch.clone()],
            song: SongData {
                automation: schedule,
                end_tick: 1000,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(c.remove_channel(ch.id).is_err());
    assert_eq!(c.state().channels.len(), 1);
    c.set_song(SongData::default()).unwrap();
    c.remove_channel(ch.id).unwrap();
    assert!(c.state().channels.is_empty());
    let mut bad = clip();
    bad.target = AutomationTarget::InstrumentParam {
        channel: ch.id,
        instrument: InstrumentKind::Synth,
        param: 0,
    };
    assert!(AutomationSchedule::build(vec![bad], &[ch]).is_err());
}
#[test]
fn subquantum_clip_is_visible_only_at_contained_control_starts() {
    let mut c = clip();
    c.start_tick = 1;
    c.length_ticks = 1;
    c.points[0].value = 0.25;
    c.points.truncate(1);
    let schedule = AutomationSchedule::build(vec![c.clone()], &[channel()]).unwrap();
    // 120BPM/48kHz: 64-frame starts are 0,2.56,5.12 ticks.
    for frame in [0, 64, 128] {
        let tick = frame as f64 / (48000.0 * 60.0 / (120.0 * 960.0));
        assert_eq!(schedule.value_at(c.target, tick), None);
    }
    c.start_tick = 0;
    let schedule = AutomationSchedule::build(vec![c.clone()], &[channel()]).unwrap();
    assert_eq!(schedule.value_at(c.target, 0.0), Some(0.25));
    assert_eq!(schedule.value_at(c.target, 2.56), None);
}
#[test]
fn empty_curve_is_inert_and_source_bounds_match_project_contract() {
    let mut empty = clip();
    empty.priority = 10;
    empty.points.clear();
    let base = clip();
    let schedule = AutomationSchedule::build(vec![base.clone(), empty], &[channel()]).unwrap();
    assert_eq!(schedule.value_at(base.target, 480.0), Some(1.0));
    let mut c = clip();
    c.offset_tick = MAX_SONG_TICKS as u32;
    c.length_ticks = 1;
    assert!(AutomationSchedule::build(vec![c], &[channel()]).is_err());
    let mut c = clip();
    c.points[0].tick = MAX_SONG_TICKS as u32 + 1;
    assert!(AutomationSchedule::build(vec![c], &[channel()]).is_err());
}
