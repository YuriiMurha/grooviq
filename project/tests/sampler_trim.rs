use grooviq_dsp::sampler::SampleBuf;
use grooviq_engine::{AudioProcessor, Controller, EngineConfig, engine};
use grooviq_project::{
    ClipContent, SampleFile,
    arrangement_edit::{ArrangementEdit, HistoryEffect},
    pattern_edit::PatternEdit,
    session::{Edit, Session},
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};
fn rig(
    frames: usize,
    start: f32,
    end: f32,
) -> (Session, Arc<Mutex<Controller>>, AudioProcessor, u32) {
    let (c, p) = engine(EngineConfig {
        sample_rate: 48000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let id = s
        .add_channel(
            Path::new("tiny.wav"),
            SampleFile {
                name: "tiny.wav".into(),
                bytes: Arc::new(vec![]),
            },
            Arc::new(SampleBuf {
                rate: 48000.0,
                l: vec![0.5; frames],
                r: vec![0.5; frames],
            }),
            Instant::now(),
        )
        .unwrap()
        .channels[0]
        .id;
    for (param, value) in [
        ("start", start),
        ("end", end),
        ("reverse", 1.0),
        ("loop", 1.0),
        ("loop_start", 0.37),
    ] {
        s.apply(
            Edit::SetInstrumentParam {
                channel: id,
                param: param.into(),
                value,
            },
            Instant::now(),
        )
        .unwrap();
    }
    (s, c, p, id)
}
#[test]
fn legacy_pairs_reset_and_exact_inverse_preserve_current_unrelated_values() {
    for (frames, start, end) in [(1, 0.2, 0.7), (2, 0.6, 0.4)] {
        let (mut s, controller, _, id) = rig(frames, start, end);
        let before = s.project().channels[0].sampler.clone();
        let stamp = s.sampler_trim_view(id).unwrap();
        let ack = s
            .sampler_trim_reset(id, &stamp.stamp, Instant::now())
            .unwrap();
        assert!(ack.changed);
        assert_eq!(s.history_usage().0, 1);
        assert_eq!((ack.state.start, ack.state.end), (0.0, 1.0));
        s.apply(
            Edit::SetInstrumentParam {
                channel: id,
                param: "reverse".into(),
                value: 0.0,
            },
            Instant::now(),
        )
        .unwrap();
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now(),
        )
        .unwrap();
        let after = &s.project().channels[0].sampler;
        assert_eq!(after["start"].to_bits(), before["start"].to_bits());
        assert_eq!(after["end"].to_bits(), before["end"].to_bits());
        assert_eq!(after["loop_start"], before["loop_start"]);
        assert_eq!(after["reverse"], 0.0);
        let c = controller.lock().unwrap();
        let params = c.state().channels[0].instrument.params();
        assert_eq!(params[3].to_bits(), before["start"].to_bits());
        assert_eq!(params[4].to_bits(), before["end"].to_bits());
        assert_eq!(
            params[5], 0.0,
            "current reverse must survive in the engine too"
        );
    }
}
#[test]
fn trim_away_and_back_invalidates_inverse_and_old_reset() {
    let (mut s, _, _, id) = rig(2, 0.6, 0.4);
    let old = s.sampler_trim_view(id).unwrap();
    let ack = s
        .sampler_trim_reset(id, &old.stamp, Instant::now())
        .unwrap();
    for value in [0.1, 0.0] {
        s.apply(
            Edit::SetSamplerParam {
                channel: id,
                param: "start".into(),
                value,
            },
            Instant::now(),
        )
        .unwrap();
    }
    assert!(
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert!(
        s.sampler_trim_reset(id, &old.stamp, Instant::now())
            .is_err()
    );
    assert_eq!(s.project().channels[0].sampler["start"], 0.0);
}
#[test]
fn writer_queue_and_history_refusal_preserve_state_and_capability_then_retry() {
    let (mut s, c, mut p, id) = rig(2, 0.6, 0.4);
    let view = s.sampler_trim_view(id).unwrap();
    let before = s.project().clone();
    let usage = s.history_usage();
    s.set_precommit_validator(Some(Arc::new(|_| Err("writer refusal".into()))));
    assert!(
        s.sampler_trim_reset(id, &view.stamp, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage(), usage);
    s.set_precommit_validator(None);
    s.set_history_limits(grooviq_project::HistoryLimits {
        steps: 0,
        notes: 2_000_000,
        bytes: 64 * 1024 * 1024,
    });
    assert!(
        s.sampler_trim_reset(id, &view.stamp, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage(), usage);
    s.set_history_limits(grooviq_project::HistoryLimits::default());
    while c
        .lock()
        .unwrap()
        .send(grooviq_engine::Command::Stop)
        .is_ok()
    {}
    assert!(
        s.sampler_trim_reset(id, &view.stamp, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage(), usage);
    p.process(&mut [0.0; 128], 2);
    let ack = s
        .sampler_trim_reset(id, &view.stamp, Instant::now())
        .unwrap();
    let accepted = s.project().clone();
    let usage = s.history_usage();
    while c
        .lock()
        .unwrap()
        .send(grooviq_engine::Command::Stop)
        .is_ok()
    {}
    assert!(
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &accepted);
    assert_eq!(s.history_usage(), usage);
    assert_eq!(s.sampler_trim_view(id).unwrap().undo, ack.state.undo);
    p.process(&mut [0.0; 128], 2);
    s.sampler_trim_undo(
        id,
        &ack.state.stamp,
        ack.state.undo.as_ref().unwrap(),
        Instant::now(),
    )
    .unwrap();
}
#[test]
fn global_history_routes_trim_and_redo_without_manufacturing_noop_steps() {
    let (mut s, _, _, id) = rig(1, 0.2, 0.7);
    let v = s.sampler_trim_view(id).unwrap();
    let ack = s.sampler_trim_reset(id, &v.stamp, Instant::now()).unwrap();
    let usage = s.history_usage();
    assert!(
        !s.sampler_trim_reset(id, &ack.state.stamp, Instant::now())
            .unwrap()
            .changed
    );
    assert_eq!(s.history_usage(), usage);
    let a = s.arrangement_view();
    let effect = s
        .step_project_history(a.generation, a.revision, true, Instant::now())
        .unwrap()
        .effect;
    assert!(
        matches!(effect,grooviq_project::arrangement_edit::HistoryEffect::SamplerTrim{channel} if channel==id)
    );
    assert_eq!(
        s.project().channels[0].sampler["start"].to_bits(),
        0.2f32.to_bits()
    );
    let a = s.arrangement_view();
    s.step_project_history(a.generation, a.revision, false, Instant::now())
        .unwrap();
    assert_eq!(s.project().channels[0].sampler["end"], 1.0);
}

fn assert_trim_survives_pattern_edit(s: &mut Session, id: u32, edit: PatternEdit) {
    let assert_region = |s: &Session, expected: [f32; 2]| {
        let sampler = &s.project().channels[0].sampler;
        assert_eq!(
            [sampler["start"], sampler["end"]].map(f32::to_bits),
            expected.map(f32::to_bits),
        );
    };
    assert_region(s, [0.2, 0.7]);
    let trim = s.sampler_trim_view(id).unwrap();
    let reset = s
        .sampler_trim_reset(id, &trim.stamp, Instant::now())
        .unwrap();
    assert!(reset.changed);
    assert_region(s, [0.0, 1.0]);

    let patterns = s.patterns();
    let edited = s
        .edit_patterns(patterns.generation, patterns.revision, edit, Instant::now())
        .unwrap();
    assert!(
        edited.revision > patterns.revision,
        "pattern edit must apply"
    );
    assert_region(s, [0.0, 1.0]);

    let trim = s.sampler_trim_view(id).unwrap();
    let token = trim
        .undo
        .as_deref()
        .expect("trim Undo must survive the pattern edit");
    s.sampler_trim_undo(id, &trim.stamp, token, Instant::now())
        .unwrap();
    assert_region(s, [0.2, 0.7]);

    let arrangement = s.arrangement_view();
    let redo = s
        .step_project_history(
            arrangement.generation,
            arrangement.revision,
            false,
            Instant::now(),
        )
        .unwrap();
    assert!(matches!(redo.effect, HistoryEffect::SamplerTrim { channel } if channel == id));
    assert_region(s, [0.0, 1.0]);
}

#[test]
fn pattern_delete_preserves_trim_undo_and_redo() {
    let (mut s, _, _, id) = rig(8, 0.2, 0.7);
    let patterns = s.patterns();
    let created = s
        .edit_patterns(
            patterns.generation,
            patterns.revision,
            PatternEdit::Create {
                name: "Spare".into(),
            },
            Instant::now(),
        )
        .unwrap();
    let pattern = created.patterns[1].id;
    assert_trim_survives_pattern_edit(&mut s, id, PatternEdit::Delete { pattern });
    assert!(s.project().patterns.iter().all(|p| p.id != pattern));
}

#[test]
fn pattern_make_unique_preserves_trim_undo_and_redo() {
    let (mut s, _, _, id) = rig(8, 0.2, 0.7);
    let pattern = s.project().current_pattern;
    let arrangement = s.arrangement_view();
    let created = s
        .edit_arrangement(
            arrangement.generation,
            arrangement.revision,
            vec![
                ArrangementEdit::CreateTrack {
                    name: "Track".into(),
                    color: 0,
                },
                ArrangementEdit::CreateClipOnCreatedTrack {
                    track_operation: 0,
                    name: "Pattern".into(),
                    start_tick: 0,
                    muted: false,
                    content: ClipContent::Pattern {
                        pattern,
                        offset_tick: 0,
                        length_ticks: 960,
                    },
                },
            ],
            Instant::now(),
        )
        .unwrap();
    let clip = created.created_ids[1];
    assert_trim_survives_pattern_edit(&mut s, id, PatternEdit::MakeUnique { clip });
    assert!(matches!(
        s.project().arrangement.clips[0].content,
        ClipContent::Pattern { pattern: copy, .. }
            if copy != pattern && s.project().patterns.iter().any(|p| p.id == copy)
    ));
}

#[test]
fn sample_relink_generation_and_shared_eviction_retire_real_capabilities() {
    let (mut s, _, mut p, id) = rig(2, 0.6, 0.4);
    let old = s.sampler_trim_view(id).unwrap();
    let ack = s
        .sampler_trim_reset(id, &old.stamp, Instant::now())
        .unwrap();
    s.set_channel_sample(
        id,
        Path::new("tiny.wav"),
        SampleFile {
            name: "tiny.wav".into(),
            bytes: Arc::new(vec![]),
        },
        Arc::new(SampleBuf {
            rate: 48000.0,
            l: vec![0.2; 2],
            r: vec![0.2; 2],
        }),
        Instant::now(),
    )
    .unwrap();
    assert!(
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.sampler_trim_view(id).unwrap().undo, None);
    p.process(&mut [0.0; 128], 2);
    // A selected-pattern note action shares the one-step budget and evicts the trim entry.
    let (mut s, _, _, id) = rig(2, 0.6, 0.4);
    s.set_history_limits(grooviq_project::HistoryLimits {
        steps: 1,
        ..Default::default()
    });
    let v = s.sampler_trim_view(id).unwrap();
    let ack = s.sampler_trim_reset(id, &v.stamp, Instant::now()).unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: id,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    assert_eq!(s.history_usage().0, 1);
    assert!(
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.sampler_trim_view(id).unwrap().undo, None);
}
#[test]
fn exact_shared_byte_endpoint_and_failed_inverse_do_not_evict_or_dirty() {
    let (mut s, _, _, id) = rig(1, 0.2, 0.7);
    let v = s.sampler_trim_view(id).unwrap();
    s.sampler_trim_reset(id, &v.stamp, Instant::now()).unwrap();
    let bytes = s.history_usage().2;
    for (budget, accepted) in [(bytes - 1, false), (bytes, true)] {
        let (mut s, _, _, id) = rig(1, 0.2, 0.7);
        s.set_history_limits(grooviq_project::HistoryLimits {
            bytes: budget,
            ..Default::default()
        });
        let v = s.sampler_trim_view(id).unwrap();
        let before = s.project().clone();
        let result = s.sampler_trim_reset(id, &v.stamp, Instant::now());
        assert_eq!(result.is_ok(), accepted, "budget {budget}");
        if !accepted {
            assert_eq!(s.project(), &before);
            assert_eq!(s.history_usage().0, 0);
        }
    }
}
#[test]
fn new_project_reuses_channel_id_without_accepting_old_target() {
    let (mut s, _, mut p, id) = rig(1, 0.2, 0.7);
    let v = s.sampler_trim_view(id).unwrap();
    let ack = s.sampler_trim_reset(id, &v.stamp, Instant::now()).unwrap();
    p.process(&mut [0.0; 128], 2);
    s.new_project().unwrap();
    let new = s
        .add_channel(
            Path::new("tiny.wav"),
            SampleFile {
                name: "tiny.wav".into(),
                bytes: Arc::new(vec![]),
            },
            Arc::new(SampleBuf {
                rate: 48000.0,
                l: vec![0.5],
                r: vec![0.5],
            }),
            Instant::now(),
        )
        .unwrap()
        .channels[0]
        .id;
    assert_eq!(new, id);
    let before = s.project().clone();
    assert!(s.sampler_trim_reset(id, &v.stamp, Instant::now()).is_err());
    assert!(
        s.sampler_trim_undo(
            id,
            &ack.state.stamp,
            ack.state.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &before);
}

#[test]
fn dedicated_inverse_and_global_note_history_share_order_without_clobbering_notes() {
    let (mut s, _, _, id) = rig(2, 0.6, 0.4);
    let v = s.sampler_trim_view(id).unwrap();
    let ack = s.sampler_trim_reset(id, &v.stamp, Instant::now()).unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: id,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    s.sampler_trim_undo(
        id,
        &ack.state.stamp,
        ack.state.undo.as_ref().unwrap(),
        Instant::now(),
    )
    .unwrap();
    assert_eq!(s.project().patterns[0].notes.len(), 1);
    let stamp = s.arrangement_view();
    assert!(matches!(
        s.step_project_history(stamp.generation, stamp.revision, true, Instant::now())
            .unwrap()
            .effect,
        grooviq_project::arrangement_edit::HistoryEffect::Notes { .. }
    ));
    assert!(s.project().patterns[0].notes.is_empty());
    let stamp = s.arrangement_view();
    assert!(matches!(
        s.step_project_history(stamp.generation, stamp.revision, false, Instant::now())
            .unwrap()
            .effect,
        grooviq_project::arrangement_edit::HistoryEffect::SamplerTrim { .. }
    ));
    assert_eq!(s.project().channels[0].sampler["start"], 0.0);
}
