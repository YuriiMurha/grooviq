use grooviq_engine::mixer::*;
use grooviq_engine::{
    AudioProcessor, BLOCK, Command, Controller, EngineConfig, EngineState, engine_with,
};

const RATE: f32 = 48_000.0;
const TONE: SourceId = SourceId::System(SystemSource::Tone);
const A: MixerTrackId = MixerTrackId(1);
const B: MixerTrackId = MixerTrackId(2);
const RETURN: MixerTrackId = MixerTrackId(3);

fn track(id: MixerTrackId, output: Option<MixerTrackId>) -> Track {
    Track {
        id,
        output,
        inserts: vec![],
    }
}

fn route(destination: MixerTrackId) -> Source {
    Source {
        id: TONE,
        destination,
    }
}

fn routed(mixer: Mixer) -> RoutedMix {
    RoutedMix {
        mixer,
        gains: vec![],
        devices: vec![],
    }
}

fn tone(mix: MixSetup) -> (Controller, AudioProcessor) {
    let (mut c, p) = engine_with(
        EngineConfig { sample_rate: RATE },
        EngineState {
            mix,
            ..EngineState::default()
        },
    );
    c.send(Command::TestTone {
        on: true,
        hz: 437.0,
        gain_db: -12.0,
    })
    .unwrap();
    (c, p)
}

fn block(p: &mut AudioProcessor) -> [f32; 2 * BLOCK] {
    let mut out = [0.0; 2 * BLOCK];
    p.process(&mut out, 2);
    out
}

fn master_mix() -> RoutedMix {
    routed(Mixer {
        tracks: vec![track(MASTER, None)],
        sources: vec![route(MASTER)],
        ..Mixer::default()
    })
}

#[test]
fn master_mute_retargets_in_sample_time_and_plain_mix_can_unmute() {
    let (_, mut reference) = tone(MixSetup::Routed(master_mix()));
    let (mut c, mut p) = tone(MixSetup::Routed(master_mix()));
    assert_eq!(block(&mut p), block(&mut reference));
    c.set_mix(MixSetup::Controlled {
        mix: master_mix(),
        controls: MixerControls {
            muted_tracks: vec![MASTER],
            ..MixerControls::default()
        },
    })
    .unwrap();
    for b in 0..12 {
        if b == 2 {
            c.set_mix(MixSetup::Routed(master_mix())).unwrap();
        }
        if b == 5 {
            c.set_mix(MixSetup::Routed(master_mix())).unwrap();
        }
        let dry = block(&mut reference);
        let actual = block(&mut p);
        for i in 0..BLOCK {
            let at = b * BLOCK + i;
            let gain = if b < 2 {
                1.0 - (at + 1) as f64 / 240.0
            } else {
                (112.0 / 240.0 + (128.0 / 240.0) * (at - 128 + 1) as f64 / 240.0).min(1.0)
            } as f32;
            for side in 0..2 {
                let i = 2 * i + side;
                let expected = dry[i] * gain;
                assert!(
                    (actual[i] - expected).abs() <= 6e-8,
                    "frame {at}: {} != {expected}",
                    actual[i]
                );
            }
        }
    }
}

const VOICE: MixerTrackId = MixerTrackId(4);
const C: MixerTrackId = MixerTrackId(5);
const KEYED: InsertId = InsertId(10);
const DRIVE: InsertId = InsertId(11);

fn source(id: u64, destination: MixerTrackId) -> Source {
    Source {
        id: SourceId::Channel(id),
        destination,
    }
}

fn send(id: u64, from: MixerTrackId, to: MixerTrackId, tap: Tap, gain: f32) -> Send {
    Send {
        id: SendId(id),
        from,
        to,
        tap,
        gain,
    }
}

fn config(rate: f32) -> PrepareConfig {
    PrepareConfig {
        sample_rate: rate,
        max_frames: BLOCK,
        max_bytes: 1 << 20,
    }
}

fn host(frame: usize, frames: usize, rate: f32) -> grooviq_dsp::HostTransport {
    grooviq_dsp::HostTransport {
        domain: grooviq_dsp::HostDomain::Song,
        frame: frame as u64,
        frames: frames as u32,
        sample_rate: rate,
        bpm: 120.0,
        playing: true,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 0,
        loop_end: 0,
        stream_generation: 0,
        discontinuity: 0,
        changed: false,
    }
}

fn render<const N: usize>(
    kernel: &mut PreparedMixer,
    at: usize,
    frames: usize,
    inputs: &[[f32; BLOCK]; N],
) -> [f32; BLOCK] {
    let ids: [SourceId; N] = std::array::from_fn(|i| kernel.source_order()[i]);
    assert_eq!(kernel.source_order(), &ids);
    let zero = [0.0; BLOCK];
    let blocks: [SourceBlock<'_>; N] = std::array::from_fn(|i| SourceBlock {
        id: ids[i],
        left: &zero[..frames],
        right: &inputs[i][..frames],
    });
    let [mut l, mut r] = [zero; 2];
    let report = kernel
        .render(
            &host(at, frames, kernel.sample_rate()),
            &blocks,
            &mut l,
            &mut r,
        )
        .unwrap();
    assert_eq!(report.silenced, 0);
    assert_eq!(l, zero);
    r
}

fn graph_for_solo() -> Mixer {
    // A -> selected bus (VOICE) -> Master and shared RETURN. B shares RETURN
    // through both taps. A's parallel send comes from outside the selected bus.
    Mixer {
        tracks: vec![
            track(MASTER, None),
            track(A, Some(VOICE)),
            track(B, Some(MASTER)),
            track(RETURN, Some(MASTER)),
            track(VOICE, Some(MASTER)),
        ],
        sources: vec![source(1, A), source(2, B), source(3, RETURN)],
        sends: vec![
            send(1, VOICE, RETURN, Tap::PostFader, 0.5),
            send(2, B, RETURN, Tap::PreFader, 0.5),
            send(3, B, RETURN, Tap::PostFader, 0.25),
            send(4, A, RETURN, Tap::PreFader, 0.25),
        ],
        ..Mixer::default()
    }
}

#[test]
fn solo_keeps_bus_ancestry_and_routes_only_selected_contributions_into_return() {
    let inputs = [[0.25; BLOCK], [0.5; BLOCK], [0.125; BLOCK]];
    let compile = compile(&graph_for_solo(), Budget::default()).unwrap();
    for (solo, muted, expected) in [
        (VOICE, vec![], 0.375),
        (VOICE, vec![A], 0.0),
        (VOICE, vec![VOICE], 0.0),
        (MASTER, vec![], 1.4375),
        (MASTER, vec![B], 0.5625),
        (MASTER, vec![MASTER], 0.0),
    ] {
        let controls = MixerControls {
            muted_tracks: muted,
            soloed_tracks: vec![solo],
            ..MixerControls::default()
        };
        let mut kernel = PreparedMixer::prepare_compensated_with_controls(
            compile.clone(),
            vec![],
            &[],
            &controls,
            config(RATE),
        )
        .unwrap();
        assert_eq!(
            render(&mut kernel, 0, BLOCK, &inputs),
            [expected; BLOCK],
            "solo {solo:?}, muted {:?}",
            controls.muted_tracks
        );
    }
}

fn comp(rate: f32) -> Box<dyn grooviq_dsp::Device> {
    let mut device = grooviq_dsp::create("comp", rate).unwrap();
    for (param, value) in [(0, -30.0), (1, 8.0), (2, 1.0), (3, 5.0), (7, 1.0)] {
        device.set_param(param, value);
    }
    device
}

fn keys() -> [DetectorFeed; 2] {
    [
        DetectorFeed {
            from: B,
            insert: KEYED,
            tap: Tap::PostFader,
        },
        DetectorFeed {
            from: RETURN,
            insert: KEYED,
            tap: Tap::PreFader,
        },
    ]
}

fn keyed_graph(through_bus: bool) -> Mixer {
    let [b, bus] = keys();
    let mut voice = track(VOICE, Some(MASTER));
    voice.inserts.push(Insert {
        id: KEYED,
        latency_frames: 0,
        memory_bytes: 0,
    });
    Mixer {
        tracks: vec![
            track(MASTER, None),
            track(A, Some(if through_bus { RETURN } else { MASTER })),
            track(B, Some(MASTER)),
            track(RETURN, Some(MASTER)),
            voice,
            track(C, Some(MASTER)),
        ],
        sources: vec![source(1, A), source(2, B), source(3, VOICE)],
        sidechains: [b, bus]
            .map(|feed| Sidechain {
                from: feed.from,
                to: feed.insert,
                tap: feed.tap,
            })
            .into(),
        ..Mixer::default()
    }
}

fn keyed_kernel(through_bus: bool, controls: &MixerControls) -> PreparedMixer {
    PreparedMixer::prepare_compensated_with_controls(
        compile(&keyed_graph(through_bus), Budget::default()).unwrap(),
        vec![PreparedInsert::new(KEYED, comp(RATE))],
        &[
            TrackGain {
                track: B,
                left: 0.25,
                right: 0.25,
            },
            TrackGain {
                track: RETURN,
                left: 0.5,
                right: 0.5,
            },
        ],
        controls,
        config(RATE),
    )
    .unwrap()
}

fn keyed_oracle(partitions: &[usize]) -> Vec<f32> {
    let [b, bus] = keys();
    let changes = [
        (0, vec![]),
        (640, vec![bus]),
        (691, vec![b, bus]),
        (723, vec![b, bus]),
        (8960, vec![b]),
    ];
    let controls = |muted_detectors| MixerControls {
        soloed_tracks: vec![VOICE],
        muted_tracks: vec![B],
        muted_detectors,
        ..MixerControls::default()
    };
    let mut actual = keyed_kernel(true, &controls(vec![]));
    let mut reference = comp(RATE);
    let mut current = [1.0_f32; 2];
    let mut start = current;
    let mut target = current;
    let mut changed_at = [0; 2];
    let mut frame = 0;
    let mut next = 0;
    let mut part = 0;
    let mut samples = Vec::new();
    let end = 9420;
    while frame < end {
        if next < changes.len() && changes[next].0 == frame {
            let (_, muted) = &changes[next];
            let mut candidate = keyed_kernel(true, &controls(muted.clone()));
            candidate.transfer_from(&mut actual, &[KEYED]).unwrap();
            actual = candidate;
            for (i, feed) in keys().iter().enumerate() {
                let new = f32::from(!muted.contains(feed));
                if target[i] != new {
                    start[i] = current[i];
                    target[i] = new;
                    changed_at[i] = frame;
                }
            }
            next += 1;
        }
        let boundary = changes.get(next).map_or(end, |(at, _)| *at);
        let frames = partitions[part % partitions.len()].min(boundary - frame);
        part += 1;
        let mut inputs = [[0.4; BLOCK]; 3];
        let mut keyed = [0.0; BLOCK];
        for (i, keyed) in keyed[..frames].iter_mut().enumerate() {
            let at = frame + i;
            inputs[2][i] = if at % 38 < 19 { 0.4 } else { -0.4 };
            inputs[0][i] = if at % 20 < 10 { 0.9 } else { -0.9 };
            inputs[1][i] = if at % 30 < 15 { 0.75 } else { -0.75 };
            for (j, current) in current.iter_mut().enumerate() {
                let progress = ((at - changed_at[j] + 1) as f64 / 240.0).min(1.0);
                *current =
                    (f64::from(start[j]) + f64::from(target[j] - start[j]) * progress) as f32;
            }
            *keyed = (inputs[1][i] * 0.25) * current[0] + inputs[0][i] * current[1];
        }
        let zero = [0.0; BLOCK];
        let mut left = zero;
        let mut expected = inputs[2];
        reference.process(
            &mut left[..frames],
            &mut expected[..frames],
            Some((&zero[..frames], &keyed[..frames])),
            &[],
        );
        let got = render(&mut actual, frame, frames, &inputs);
        for i in 0..frames {
            assert!(
                (got[i] - expected[i]).abs() < 2e-7,
                "detector sample {}: got {}, expected {}",
                frame + i,
                got[i],
                expected[i]
            );
        }
        samples.extend_from_slice(&got[..frames]);
        frame += frames;
    }
    samples
}

#[test]
fn inaudible_detector_bus_and_muted_source_keep_ducking_until_each_feed_is_muted() {
    let a = keyed_oracle(&[64]);
    assert_eq!(a, keyed_oracle(&[1, 13, 51, 5]));
    assert!(
        a[639].abs() < 0.2,
        "assigned detector bus still keys through solo"
    );
    assert!(
        a[8890].abs() > 0.38,
        "silent assigned detector stays external"
    );
    assert!(a[9419].abs() < 0.2, "re-enabled feed restores ducking");
}

#[test]
fn compatible_send_keeps_its_fade_and_reused_send_id_starts_at_its_new_target() {
    let prepare = |from, muted| {
        let mixer = Mixer {
            tracks: vec![
                track(MASTER, None),
                track(A, Some(MASTER)),
                track(B, Some(MASTER)),
                track(C, Some(MASTER)),
            ],
            sources: vec![source(1, A), source(2, B), source(3, C)],
            sends: vec![
                send(5, from, MASTER, Tap::PreFader, 1.0),
                send(6, B, MASTER, Tap::PreFader, 1.0),
            ],
            ..Mixer::default()
        };
        let gains = [A, B, C].map(|track| TrackGain {
            track,
            left: 0.0,
            right: 0.0,
        });
        PreparedMixer::prepare_compensated_with_controls(
            compile(&mixer, Budget::default()).unwrap(),
            vec![],
            &gains,
            &MixerControls {
                muted_tracks: muted,
                ..MixerControls::default()
            },
            config(RATE),
        )
        .unwrap()
    };
    let inputs = [[0.1; BLOCK], [0.2; BLOCK], [0.4; BLOCK]];
    let mut old = prepare(A, vec![]);
    let mut fading = prepare(A, vec![A, B]);
    fading.transfer_from(&mut old, &[]).unwrap();
    let first = render(&mut fading, 0, BLOCK, &inputs);
    assert!((first[0] - 0.3 * 239.0 / 240.0).abs() < 1e-7);

    let mut rerouted = prepare(C, vec![B, C]);
    rerouted.transfer_from(&mut fading, &[]).unwrap();
    let second = render(&mut rerouted, BLOCK, BLOCK, &inputs);
    for (i, sample) in second.into_iter().enumerate() {
        let expected = 0.2 * (175 - i) as f32 / 240.0;
        assert!(
            (sample - expected).abs() < 3e-8,
            "rerouted sample {i}: {sample} != {expected}"
        );
    }
    rerouted.reset();
    assert_eq!(render(&mut rerouted, 0, BLOCK, &inputs), [0.0; BLOCK]);
}

#[test]
fn aligned_history_warms_through_solo_and_dry_bypass_preserves_compensation() {
    let prepare = |soloed_tracks| {
        let mut drive_track = track(A, Some(MASTER));
        drive_track.inserts.push(Insert {
            id: DRIVE,
            latency_frames: 64,
            memory_bytes: 0,
        });
        let mixer = Mixer {
            tracks: vec![track(MASTER, None), drive_track, track(B, Some(MASTER))],
            sources: vec![source(1, A), source(2, B)],
            ..Mixer::default()
        };
        let mut drive = grooviq_dsp::create("drive", RATE).unwrap();
        drive.set_param(5, 2.0);
        let controls = MixerControls {
            soloed_tracks,
            bypass: vec![InsertBypass {
                insert: DRIVE,
                bypassed: true,
            }],
            ..MixerControls::default()
        };
        let kernel = PreparedMixer::prepare_compensated_with_controls(
            compile(&mixer, Budget::default()).unwrap(),
            vec![PreparedInsert::new(DRIVE, drive)],
            &[],
            &controls,
            config(RATE),
        )
        .unwrap();
        assert_eq!(kernel.master_latency_frames(), Some(49));
        kernel
    };
    let mut solo = prepare(vec![A]);
    let mut inputs = [[0.0; BLOCK]; 2];
    inputs[0][BLOCK - 1] = 0.25;
    inputs[1][BLOCK - 1] = 0.5;
    assert_eq!(render(&mut solo, 0, BLOCK, &inputs), [0.0; BLOCK]);
    let mut all = prepare(vec![]);
    all.transfer_from(&mut solo, &[DRIVE]).unwrap();
    let mut expected = [0.0; BLOCK];
    expected[48] = 0.25 + 0.5 * (49.0 / 240.0);
    assert_eq!(render(&mut all, BLOCK, BLOCK, &[[0.0; BLOCK]; 2]), expected);
}

#[test]
fn invalid_controls_refuse_atomically_and_reconfigure_replays_queued_targets() {
    use grooviq_engine::ControlError;
    let (mut c, mut p) = tone(MixSetup::Routed(master_mix()));
    assert!(block(&mut p).iter().any(|&x| x != 0.0));
    let muted = MixSetup::Controlled {
        mix: master_mix(),
        controls: MixerControls {
            muted_tracks: vec![MASTER],
            ..MixerControls::default()
        },
    };
    let revision = c.set_mix(muted.clone()).unwrap();
    for invalid in [
        MixerControls {
            muted_tracks: vec![A],
            ..MixerControls::default()
        },
        MixerControls {
            soloed_tracks: vec![MASTER, MASTER],
            ..MixerControls::default()
        },
        MixerControls {
            muted_detectors: vec![keys()[0]],
            ..MixerControls::default()
        },
        MixerControls {
            bypass: vec![InsertBypass {
                insert: DRIVE,
                bypassed: false,
            }],
            ..MixerControls::default()
        },
    ] {
        assert_eq!(
            c.set_mix(MixSetup::Controlled {
                mix: master_mix(),
                controls: invalid
            }),
            Err(ControlError::Rejected)
        );
        assert_eq!(c.mix_revision(), revision);
        assert_eq!(c.state().mix, muted);
    }
    assert!(c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96_000.0
        }
    ));
    assert_eq!(c.state().mix, muted);
    assert_eq!(block(&mut p), [0.0; 2 * BLOCK]);
    c.set_mix(MixSetup::Routed(master_mix())).unwrap();
    assert!(block(&mut p).iter().any(|&x| x != 0.0));
    let revision = c.mix_revision();
    while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    assert_eq!(c.set_mix(muted), Err(ControlError::Full));
    assert_eq!(c.mix_revision(), revision);
    assert_eq!(c.state().mix, MixSetup::Routed(master_mix()));
    assert!(block(&mut p).iter().any(|&x| x != 0.0));
}

#[test]
fn a_detector_target_does_not_attenuate_the_source_main_or_sends() {
    let mut mixer = keyed_graph(true);
    mixer.sidechains.push(Sidechain {
        from: B,
        to: KEYED,
        tap: Tap::PreFader,
    });
    mixer.sources.retain(|source| source.destination == B);
    mixer.sends = vec![
        send(1, B, RETURN, Tap::PreFader, 0.5),
        send(2, B, RETURN, Tap::PostFader, 0.25),
    ];
    let gain = [TrackGain {
        track: B,
        left: 0.5,
        right: 0.5,
    }];
    let prepare = |muted_detectors| {
        PreparedMixer::prepare_compensated_with_controls(
            compile(&mixer, Budget::default()).unwrap(),
            vec![PreparedInsert::new(KEYED, comp(RATE))],
            &gain,
            &MixerControls {
                muted_detectors,
                ..MixerControls::default()
            },
            config(RATE),
        )
        .unwrap()
    };
    let mut open = prepare(vec![]);
    let mut muted = prepare(vec![DetectorFeed {
        from: B,
        insert: KEYED,
        tap: Tap::PreFader,
    }]);
    muted.transfer_from(&mut open, &[KEYED]).unwrap();
    // A half-gain main, a half-gain pre send and a quarter-gain post send.
    assert_eq!(
        render(&mut muted, 0, BLOCK, &[[0.5; BLOCK]]),
        [0.5625; BLOCK]
    );
}
