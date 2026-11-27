use grooviq_dsp::{Device, HostDomain, HostTransport, create};
use grooviq_engine::mixer::{
    Budget, Insert, InsertBypass, InsertId, MASTER, Mixer, PrepareConfig, PreparedInsert,
    PreparedMixer, Source, SourceBlock, SourceId, Track, compile,
};

const SLOT: InsertId = InsertId(12);
const INPUT: SourceId = SourceId::Channel(4);
const BLOCK: usize = 64;

fn device(kind: &str, rate: f32) -> Box<dyn Device> {
    let mut device = create(kind, rate).unwrap();
    match kind {
        "drive" => {
            device.set_param(3, 0.75);
            device.set_param(5, 2.0);
        }
        "saturation" => device.set_param(1, 10.0),
        "limiter" => device.set_param(0, 8.0),
        _ => {}
    }
    device
}

fn kernel(kind: &str, rate: f32, bypassed: Option<bool>) -> PreparedMixer {
    let mixer = Mixer {
        tracks: vec![Track {
            id: MASTER,
            output: None,
            inserts: vec![Insert {
                id: SLOT,
                latency_frames: 1024,
                memory_bytes: 0,
            }],
        }],
        sources: vec![Source {
            id: INPUT,
            destination: MASTER,
        }],
        ..Mixer::default()
    };
    let targets = bypassed.map(|bypassed| InsertBypass {
        insert: SLOT,
        bypassed,
    });
    PreparedMixer::prepare_compensated_with_bypass(
        compile(&mixer, Budget::default()).unwrap(),
        vec![PreparedInsert::new(SLOT, device(kind, rate))],
        &[],
        targets.as_slice(),
        PrepareConfig {
            sample_rate: rate,
            max_frames: BLOCK,
            max_bytes: 1 << 20,
        },
    )
    .unwrap()
}

fn transport(rate: f32, frame: usize, frames: usize) -> HostTransport {
    HostTransport {
        sample_rate: rate,
        frames: frames as u32,
        bpm: 120.0,
        frame: frame as u64,
        playing: true,
        domain: HostDomain::Song,
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

fn input(frame: usize) -> f32 {
    (((frame * 19 + 11) % 47) as f32 - 23.0) * 0.036
}

fn check(kind: &str, rate: f32, latency: usize, parts: &[usize]) -> Vec<f32> {
    // The DSP reference always processes the same block, including while dry is audible.
    let mut reference = device(kind, rate);
    assert_eq!(reference.latency_frames() as usize, latency);
    let mut actual = kernel(kind, rate, Some(false));
    assert_eq!(actual.master_latency_frames(), Some(latency as u64));
    let duration = (rate as usize + 100) / 200;
    let changes = [
        (13, true),
        (29, false),
        (78, true),
        (100, true), // Reinstall midway through a ramp with its existing target.
        (78 + duration + 9, false),
        (78 + 2 * duration + 33, true),
    ];
    let mut target = 0.0;
    let mut start = 0.0;
    let mut changed_at = 0;
    let mut blend = 0.0;
    let mut samples = Vec::new();
    let mut frame = 0;
    let mut part = 0;
    let stop = 78 + 3 * duration + 64;
    while frame < stop {
        if let Some(&(_, bypass)) = changes.iter().find(|&&(at, _)| at == frame) {
            install(&mut actual, kind, rate, bypass);
            if target != f64::from(bypass) {
                start = blend;
                target = f64::from(bypass);
                changed_at = frame;
            }
            // An empty block renders between install and the first blend sample.
            let mut empty = [];
            let report = actual
                .render(
                    &transport(rate, frame, 0),
                    &[SourceBlock {
                        id: INPUT,
                        left: &[],
                        right: &[],
                    }],
                    &mut empty,
                    &mut [],
                )
                .unwrap();
            assert_eq!(report.frames, 0);
        }
        let boundary = changes
            .iter()
            .find(|&&(at, _)| at > frame)
            .map_or(stop, |x| x.0);
        let n = parts[part % parts.len()].min(boundary - frame);
        part += 1;
        let t = transport(rate, frame, n);
        let zero = [0.0; BLOCK];
        let mut right = [0.0; BLOCK];
        for (i, sample) in right[..n].iter_mut().enumerate() {
            *sample = input(frame + i);
        }
        let mut wet_l = zero;
        let mut wet_r = right;
        reference.set_transport(&t);
        reference.set_tempo(t.bpm);
        reference.process(&mut wet_l[..n], &mut wet_r[..n], None, &[]);
        let mut out_l = zero;
        let mut out_r = zero;
        let report = actual
            .render(
                &t,
                &[SourceBlock {
                    id: INPUT,
                    left: &zero[..n],
                    right: &right[..n],
                }],
                &mut out_l,
                &mut out_r,
            )
            .unwrap();
        assert_eq!(report.silenced, 0);
        for i in 0..n {
            let at = frame + i;
            let progress = ((at + 1 - changed_at) as f64 / duration as f64).min(1.0);
            blend = start + (target - start) * progress;
            let dry = at.checked_sub(latency).map_or(0.0, input);
            for (output, wet, dry) in [(out_l[i], wet_l[i], 0.0), (out_r[i], wet_r[i], dry)] {
                let expected = wet as f64 * (1.0 - blend) + dry as f64 * blend;
                assert!(
                    (output as f64 - expected).abs() < 2e-7,
                    "{kind} {rate} frame {at}: output {output}, expected {expected} (dry {dry}, wet {wet}, blend {blend})"
                );
                if blend == 0.0 {
                    assert_eq!(output, wet);
                } else if blend == 1.0 {
                    assert_eq!(output, dry);
                }
            }
            samples.push(out_r[i]);
        }
        frame += n;
    }
    samples
}

fn install(actual: &mut PreparedMixer, kind: &str, rate: f32, bypass: bool) {
    let mut new = kernel(kind, rate, Some(bypass));
    new.transfer_from(actual, &[SLOT]).unwrap();
    *actual = new;
}

#[test]
fn bypass_matches_continuous_wet_and_indexed_dry_across_installs() {
    for (kind, rate, latency) in [
        ("drive", 48_000.0, 49),
        ("saturation", 44_100.0, 49),
        ("limiter", 96_000.0, 143),
        ("delay", 48_000.0, 0),
    ] {
        assert_eq!(
            check(kind, rate, latency, &[64]),
            check(kind, rate, latency, &[1, 63, 9, 27]),
            "{kind} partition changed the output",
        );
    }
}

fn render_right(kernel: &mut PreparedMixer, at: usize, input: &[f32]) -> [f32; BLOCK] {
    let mut out_l = [0.0; BLOCK];
    let mut out_r = [0.0; BLOCK];
    let left = [0.0; BLOCK];
    let report = kernel
        .render(
            &transport(kernel.sample_rate(), at, input.len()),
            &[SourceBlock {
                id: INPUT,
                left: &left[..input.len()],
                right: input,
            }],
            &mut out_l,
            &mut out_r,
        )
        .unwrap();
    assert_eq!(report.silenced, 0);
    assert_eq!(out_l, [0.0; BLOCK]);
    out_r
}

#[test]
fn cold_and_reset_bypass_have_the_reported_stereo_delay() {
    let mut actual = kernel("drive", 48_000.0, Some(false));
    assert_ne!(render_right(&mut actual, 0, &[0.5; BLOCK]), [0.0; BLOCK]);
    install(&mut actual, "drive", 48_000.0, true);
    render_right(&mut actual, BLOCK, &[0.25; 11]);
    actual.reset(); // Clear a warm ring and snap an in-progress ramp to dry.
    let mut fresh = kernel("drive", 48_000.0, Some(true));
    let mut pin = [0.0; BLOCK];
    pin[2] = -0.75;
    let mut expected = [0.0; BLOCK];
    expected[51] = -0.75;
    assert_eq!(render_right(&mut actual, 0, &pin), expected);
    assert_eq!(render_right(&mut fresh, 0, &pin), expected);
}

#[test]
fn default_and_opted_in_hard_wet_are_exact() {
    let mut original = kernel("drive", 48_000.0, None);
    let mut opted_in = kernel("drive", 48_000.0, Some(false));
    for at in [0, 64, 128] {
        let right = std::array::from_fn::<_, BLOCK, _>(|i| input(at + i));
        assert_eq!(
            render_right(&mut original, at, &right),
            render_right(&mut opted_in, at, &right)
        );
    }
}

fn routed_kernel(route: u64, extra_track: bool, bypassed: bool) -> PreparedMixer {
    use grooviq_engine::mixer::MixerTrackId;
    let prior = InsertId(8);
    let relay = MixerTrackId(3);
    let receiver = MixerTrackId(4);
    let leaf = |id| Track {
        id: MixerTrackId(id),
        output: Some(relay),
        inserts: vec![],
    };
    let binding = |id| Insert {
        id,
        latency_frames: 49,
        memory_bytes: 0,
    };
    let mut tracks = vec![
        Track {
            id: MASTER,
            output: None,
            inserts: vec![],
        },
        Track {
            id: receiver,
            output: Some(MASTER),
            inserts: vec![binding(prior), binding(SLOT)],
        },
        Track {
            id: relay,
            output: Some(receiver),
            inserts: vec![],
        },
        leaf(1),
        leaf(2),
    ];
    if extra_track {
        tracks.push(Track {
            id: MixerTrackId(19),
            output: Some(MASTER),
            inserts: vec![],
        });
    }
    let mixer = Mixer {
        tracks,
        sources: vec![Source {
            id: INPUT,
            destination: MixerTrackId(route),
        }],
        ..Mixer::default()
    };
    let bindings = [prior, SLOT]
        .map(|id| PreparedInsert::new(id, device("drive", 48_000.0)))
        .into();
    PreparedMixer::prepare_compensated_with_bypass(
        compile(&mixer, Budget::default()).unwrap(),
        bindings,
        &[],
        &[
            InsertBypass {
                insert: prior,
                bypassed: true,
            },
            InsertBypass {
                insert: SLOT,
                bypassed,
            },
        ],
        PrepareConfig {
            sample_rate: 48_000.0,
            max_frames: BLOCK,
            max_bytes: 1 << 20,
        },
    )
    .unwrap()
}

#[test]
fn dry_history_follows_the_serial_stage_and_its_transitive_input() {
    let mut original = routed_kernel(1, false, true);
    assert_eq!(original.master_latency_frames(), Some(98));
    let mut impulse = [0.0; BLOCK];
    impulse[41] = 0.75;
    assert_eq!(render_right(&mut original, 0, &impulse), [0.0; BLOCK]);
    assert_eq!(render_right(&mut original, 64, &[0.0; BLOCK]), [0.0; BLOCK]);
    let mut extended = routed_kernel(1, true, true);
    extended
        .transfer_from(&mut original, &[InsertId(8), SLOT])
        .unwrap();
    let mut expected = [0.0; BLOCK];
    expected[11] = 0.75; // Absolute frame 41 + 49 at each stage.
    assert_eq!(render_right(&mut extended, 128, &[0.0; BLOCK]), expected);

    render_right(&mut extended, 192, &impulse);
    render_right(&mut extended, 256, &[0.0; BLOCK]);
    let mut retargeted = routed_kernel(2, true, true);
    retargeted
        .transfer_from(&mut extended, &[InsertId(8), SLOT])
        .unwrap();
    assert_eq!(
        render_right(&mut retargeted, 320, &[0.0; BLOCK]),
        [0.0; BLOCK]
    );
}

#[test]
fn compensation_keeps_only_compatible_inputs_to_a_rerouted_dry_path() {
    use grooviq_engine::mixer::{MixerTrackId, Sidechain, Tap};

    const STABLE: SourceId = SourceId::Channel(5);
    const ALIGNER: InsertId = InsertId(8);
    const COMPRESSOR: InsertId = InsertId(11);
    let build = |route, opted_in: bool, stage_delay: bool| {
        let track = |id, output: Option<u64>, inserts| Track {
            id: MixerTrackId(id),
            output: output.map(MixerTrackId),
            inserts,
        };
        let insert = |id, latency_frames| Insert {
            id,
            latency_frames,
            memory_bytes: 0,
        };
        let master_chain = if stage_delay {
            vec![]
        } else {
            vec![insert(SLOT, 49)]
        };
        let mut tracks = vec![
            track(0, None, master_chain),
            track(1, Some(3), vec![]),
            track(2, Some(3), vec![]),
            track(3, Some(if stage_delay { 6 } else { 0 }), vec![]),
            track(4, Some(0), vec![insert(ALIGNER, 49)]),
            track(5, Some(0), vec![]),
        ];
        let mut bindings = vec![
            PreparedInsert::new(ALIGNER, device("drive", 48_000.0)),
            PreparedInsert::new(SLOT, device("drive", 48_000.0)),
        ];
        let mut sidechains = Vec::new();
        if stage_delay {
            // The later detector requires a serial stage delay on the compressor input.
            tracks.push(track(
                6,
                Some(0),
                vec![insert(COMPRESSOR, 0), insert(SLOT, 49)],
            ));
            bindings.push(PreparedInsert::new(COMPRESSOR, device("comp", 48_000.0)));
            sidechains.push(Sidechain {
                from: MixerTrackId(4),
                to: COMPRESSOR,
                tap: Tap::PostFader,
            });
        }
        let mixer = Mixer {
            tracks,
            sidechains,
            sources: vec![
                Source {
                    id: INPUT,
                    destination: MixerTrackId(route),
                },
                Source {
                    id: STABLE,
                    destination: MixerTrackId(5),
                },
            ],
            ..Mixer::default()
        };
        let target = opted_in.then_some(InsertBypass {
            insert: SLOT,
            bypassed: true,
        });
        PreparedMixer::prepare_compensated_with_bypass(
            compile(&mixer, Budget::default()).unwrap(),
            bindings,
            &[],
            target.as_slice(),
            PrepareConfig {
                sample_rate: 48_000.0,
                max_frames: BLOCK,
                max_bytes: 1 << 20,
            },
        )
        .unwrap()
    };
    let render = |kernel: &mut PreparedMixer, frame, changed: &[f32], stable: &[f32]| {
        let zero = [0.0; BLOCK];
        let frames = changed.len();
        let inputs = [
            SourceBlock {
                id: INPUT,
                left: &zero[..frames],
                right: changed,
            },
            SourceBlock {
                id: STABLE,
                left: &zero[..frames],
                right: stable,
            },
        ];
        let [mut left, mut right] = [[0.0; BLOCK]; 2];
        kernel
            .render(
                &transport(48_000.0, frame, frames),
                &inputs,
                &mut left,
                &mut right,
            )
            .unwrap();
        assert_eq!(left, zero);
        right
    };

    let mut changed = [0.0; 32];
    let mut stable = [0.0; 32];
    changed[0] = 0.75;
    stable[3] = 0.375;
    let zero = [0.0; BLOCK];
    let mut expected = zero;
    expected[5] = 0.375; // The unrelated feed still arrives at frame 3 + 49 + 49.
    for stage_delay in [false, true] {
        let survivors: &[_] = if stage_delay {
            &[ALIGNER, COMPRESSOR, SLOT]
        } else {
            &[ALIGNER, SLOT]
        };
        for previously_opted_in in [true, false] {
            let mut old = build(1, previously_opted_in, stage_delay);
            let mut rerouted = build(2, true, stage_delay);
            assert_eq!(old.master_latency_frames(), Some(98));
            assert_eq!(render(&mut old, 0, &changed, &stable), zero);
            rerouted.transfer_from(&mut old, survivors).unwrap();

            assert_eq!(render(&mut rerouted, 32, &zero, &zero), zero);
            let actual = render(&mut rerouted, 96, &zero, &zero);
            assert_eq!(actual[2], 0.0, "the retired input must not reach frame 98");
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn sample_rate_and_latency_refuse_partial_kernel_transfer() {
    use grooviq_engine::mixer::TransferError;
    let mut old = kernel("limiter", 48_000.0, Some(true));
    let mut impulse = [0.0; BLOCK];
    impulse[7] = 0.75;
    render_right(&mut old, 0, &impulse);
    let mut rerated = kernel("limiter", 96_000.0, Some(true));
    assert_eq!(
        rerated.transfer_from(&mut old, &[SLOT]),
        Err(TransferError::SampleRate)
    );
    assert_eq!(rerated.master_latency_frames(), Some(143));
    assert_eq!(render_right(&mut rerated, 0, &impulse), [0.0; BLOCK]);
    let mut changed = kernel("drive", 48_000.0, Some(true));
    assert_eq!(
        changed.transfer_from(&mut old, &[SLOT]),
        Err(TransferError::Latency(SLOT))
    );
    let mut expected = [0.0; BLOCK];
    expected[56] = 0.75;
    assert_eq!(render_right(&mut changed, 0, &impulse), expected);
    expected = [0.0; BLOCK];
    expected[14] = 0.75;
    assert_eq!(render_right(&mut old, 64, &[0.0; BLOCK]), expected);
}

#[test]
fn external_detector_keeps_driving_the_wet_compressor_while_bypassed() {
    use grooviq_engine::mixer::{MixerTrackId, Sidechain, Tap, TrackGain};

    const RATE: f32 = 48_000.0;
    let key_track = MixerTrackId(1);
    let key = SourceId::Channel(5);
    let compressor = || {
        let mut comp = create("comp", RATE).unwrap();
        for (index, value) in [(0, -35.0), (1, 8.0), (2, 1.0), (7, 1.0)] {
            comp.set_param(index, value);
        }
        comp
    };
    let mixer = Mixer {
        tracks: vec![
            Track {
                id: MASTER,
                output: None,
                inserts: vec![Insert {
                    id: SLOT,
                    latency_frames: 0,
                    memory_bytes: 0,
                }],
            },
            Track {
                id: key_track,
                output: Some(MASTER),
                inserts: vec![],
            },
        ],
        sources: vec![
            Source {
                id: INPUT,
                destination: MASTER,
            },
            Source {
                id: key,
                destination: key_track,
            },
        ],
        sidechains: vec![Sidechain {
            from: key_track,
            to: SLOT,
            tap: Tap::PreFader,
        }],
        ..Mixer::default()
    };
    let prepare = |bypassed| {
        PreparedMixer::prepare_compensated_with_bypass(
            compile(&mixer, Budget::default()).unwrap(),
            vec![PreparedInsert::new(SLOT, compressor())],
            &[TrackGain {
                track: key_track,
                left: 0.0,
                right: 0.0,
            }],
            &[InsertBypass {
                insert: SLOT,
                bypassed,
            }],
            PrepareConfig {
                sample_rate: RATE,
                max_frames: BLOCK,
                max_bytes: 1 << 20,
            },
        )
        .unwrap()
    };
    let mut actual = prepare(true);
    let mut wet = compressor();
    let zero = [0.0; BLOCK];
    let dry = [0.4; BLOCK];
    for block in 0..13 {
        if block == 8 {
            let mut next = prepare(false);
            next.transfer_from(&mut actual, &[SLOT]).unwrap();
            actual = next;
        }
        let mut key_r = [0.0; BLOCK];
        if block < 8 {
            for (i, sample) in key_r.iter_mut().enumerate() {
                *sample = if (i + block * BLOCK) % 20 < 10 {
                    0.8
                } else {
                    -0.8
                };
            }
        }
        let [mut out_l, mut out_r, mut wet_l, mut wet_r] = [zero, zero, zero, dry];
        wet.process(&mut wet_l, &mut wet_r, Some((&zero, &key_r)), &[]);
        actual
            .render(
                &transport(RATE, block * BLOCK, BLOCK),
                &[
                    SourceBlock {
                        id: INPUT,
                        left: &zero,
                        right: &dry,
                    },
                    SourceBlock {
                        id: key,
                        left: &zero,
                        right: &key_r,
                    },
                ],
                &mut out_l,
                &mut out_r,
            )
            .unwrap();
        assert_eq!(out_l, zero);
        for i in 0..BLOCK {
            if block < 8 {
                assert_eq!(out_r[i], dry[i]);
            } else {
                let wet_fraction = (((block - 8) * BLOCK + i + 1) as f64 / 240.0).min(1.0);
                assert!(
                    wet_r[i] < 0.25,
                    "detector history makes the wet oracle distinguishable"
                );
                let expected =
                    f64::from(wet_r[i]) * wet_fraction + f64::from(dry[i]) * (1.0 - wet_fraction);
                assert!(
                    (f64::from(out_r[i]) - expected).abs() < 5e-8,
                    "detector history mismatch at {block}:{i}"
                );
            }
        }
    }
}
