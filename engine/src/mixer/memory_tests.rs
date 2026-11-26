use crate::mixer::*;
use crate::*;

const FRAMES: usize = BLOCK * 2;

fn setup(declared: u32) -> MixSetup {
    let id = InsertId(1);
    MixSetup::Routed(RoutedMix {
        mixer: Mixer {
            tracks: vec![
                Track {
                    id: MASTER,
                    output: None,
                    inserts: vec![],
                },
                Track {
                    id: MixerTrackId(1),
                    output: Some(MASTER),
                    inserts: vec![Insert {
                        id,
                        latency_frames: declared,
                        memory_bytes: 0,
                    }],
                },
            ],
            sources: vec![Source {
                id: SourceId::System(SystemSource::Tone),
                destination: MASTER,
            }],
            ..Mixer::default()
        },
        devices: vec![InsertDevice {
            insert: id,
            kind: "drive".into(),
            seed: 0,
            params: vec![1.0, 0.0, 0.0, 0.0, 0.0, 2.0],
        }],
        gains: vec![],
    })
}

fn fixture() -> (Controller, AudioProcessor) {
    let (mut c, p) = engine_with(
        EngineConfig {
            sample_rate: 48_000.0,
        },
        EngineState {
            mix: setup(49),
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

fn block(p: &mut AudioProcessor) -> [f32; FRAMES] {
    let mut out = [0.0; FRAMES];
    p.process(&mut out, 2);
    out
}

fn compound(mix: MixSetup) -> SourceEdit {
    SourceEdit {
        mix,
        remove: vec![],
        add: vec![],
        timeline: None,
    }
}

#[test]
fn mixer_memory_is_charged_through_queue_render_and_control_reclaim() {
    let (mut c, mut p) = fixture();
    let (_, mut reference) = fixture();
    // Each candidate fits with exactly one live mixer, but not with two owners.
    c.set_mixer_memory_limit(45_000);
    let first = block(&mut p);
    assert_eq!(first, block(&mut reference));
    assert_eq!(first[..49 * 2], [0.0; 49 * 2]);
    assert!(first[49 * 2..].iter().any(|x| *x != 0.0));

    let before = c.mix_revision();
    assert_eq!(c.set_mix(setup(49)), Ok(before + 1));
    let mut refused = setup(49);
    let MixSetup::Routed(mix) = &mut refused else {
        unreachable!()
    };
    mix.gains.push(TrackGain {
        track: MASTER,
        left: 0.5,
        right: 0.5,
    });
    assert_eq!(
        c.set_sources_and_mix(compound(refused)),
        Err(ControlError::Full)
    );
    assert_eq!(c.mix_revision(), before + 1);
    assert_eq!(c.state().mix, setup(49));

    let rate = EngineConfig {
        sample_rate: 96_000.0,
    };
    assert!(!c.reconfigure(&mut p, rate));
    assert_eq!(c.sample_rate(), 48_000.0);
    assert_eq!(c.mix_revision(), before + 1);
    assert_eq!(
        block(&mut p),
        block(&mut reference),
        "accepted install keeps the pending stereo history"
    );
    assert_eq!(
        c.set_mix(setup(49)),
        Err(ControlError::Full),
        "retirement is not reclamation"
    );
    c.free_trash();
    assert_eq!(c.set_sources_and_mix(compound(setup(49))), Ok(before + 2));
    assert_eq!(block(&mut p), block(&mut reference));
    c.free_trash();
    assert!(c.reconfigure(&mut p, rate));
    assert_eq!(c.sample_rate(), 96_000.0);
}

#[test]
fn prepare_rejection_and_queue_full_release_candidate_reservations() {
    let (mut c, mut p) = fixture();
    c.set_mixer_memory_limit(45_000);
    assert_eq!(c.set_mix(setup(48)), Err(ControlError::Rejected));
    while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    assert_eq!(
        c.set_sources_and_mix(compound(setup(49))),
        Err(ControlError::Full)
    );
    assert_eq!(c.mix_revision(), 0);
    block(&mut p);
    assert!(c.set_mix(setup(49)).is_ok());
}

#[cfg(feature = "test-hooks")]
#[test]
fn faulted_mixer_retires_so_recovery_fits_without_two_graphs() {
    let (mut c, mut p) = fixture();
    c.set_mixer_memory_limit(33_000);
    assert_eq!(c.set_mix(setup(49)), Err(ControlError::Full));
    c.send(Command::InjectPanic).unwrap();
    assert_eq!(block(&mut p), [0.0; FRAMES]);
    assert!(
        c.check_health().is_empty(),
        "temporary pressure is not a failed recovery attempt"
    );
    assert_eq!(block(&mut p), [0.0; FRAMES]);
    assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
    assert_eq!(c.state().mix, setup(49));
    assert!(block(&mut p).iter().any(|x| *x != 0.0));
}

#[test]
fn registry_payload_cannot_be_understated_by_a_caller() {
    let (mut c, _) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let limit = 64_000;
    c.set_mixer_memory_limit(limit);
    let mut expensive = setup(49);
    let MixSetup::Routed(mix) = &mut expensive else {
        unreachable!()
    };
    mix.devices[0].kind = "delay".into();
    mix.devices[0].params.clear();
    let (_, highest) = grooviq_dsp::sample_rate_range("delay").unwrap();
    assert!(grooviq_dsp::max_memory_bytes("delay", highest).unwrap() > limit);
    assert_eq!(c.set_mix(expensive), Err(ControlError::Rejected));
    assert_eq!(c.mix_revision(), 0);
    assert_eq!(c.set_mix(setup(49)), Ok(1));
}

#[test]
fn replacement_graph_keeps_its_charge_in_the_graph_return_queue() {
    let (mut c, mut p) = fixture();
    c.set_mixer_memory_limit(45_000);
    let replacement = || EngineState {
        mix: setup(49),
        ..EngineState::default()
    };
    assert_eq!(c.load_state(replacement()), Ok(()));
    let revision = c.mix_revision();
    let mark = c.record_mark();
    let mut refused = replacement();
    refused.bpm = 100.0;
    assert_eq!(c.load_state(refused), Err(ControlError::Full));
    assert_ne!(c.state().bpm, 100.0);
    assert_eq!(c.mix_revision(), revision);
    assert_eq!(c.record_mark(), mark);
    block(&mut p);
    assert_eq!(c.load_state(replacement()), Err(ControlError::Full));
    c.free_trash();
    assert_eq!(c.load_state(replacement()), Ok(()));
}

#[cfg(feature = "test-hooks")]
#[test]
fn detached_fault_releases_its_mixer_for_a_rate_rebuild() {
    let (mut c, mut p) = fixture();
    c.set_mixer_memory_limit(33_000);
    c.send(Command::InjectPanic).unwrap();
    assert_eq!(block(&mut p), [0.0; FRAMES]);
    c.settle_recovery_stop(&mut p);
    assert!(c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96_000.0
        }
    ));
    assert_eq!(c.sample_rate(), 96_000.0);
    assert_eq!(c.state().mix, setup(49));
    assert!(block(&mut p).iter().any(|x| *x != 0.0));
}

#[test]
fn detached_rate_rebuild_reclaims_parked_returns_without_a_callback() {
    let (mut c, mut p) = fixture();
    for _ in 0..=TRASH_CAPACITY {
        c.set_mix(setup(49)).unwrap();
        block(&mut p);
    }
    // The return queue is full with one more retirement parked in the processor.
    // The device callback is stopped now. Both forms of return are reclaimable.
    c.set_mixer_memory_limit(45_000);
    assert!(c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96_000.0
        }
    ));
    assert_eq!(c.sample_rate(), 96_000.0);
}
