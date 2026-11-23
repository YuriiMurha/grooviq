//! Generated offline Session contract for the first pattern-clip UI slice.
//! Authoring this file is not a native execution or browser-parity claim.
use grooviq_engine::{self as engine, AudioProcessor, Controller, EngineConfig};
use grooviq_project::{
    ClipContent, DecodedSamples, Loaded, Note, Project,
    arrangement_edit::{ArrangementEdit as Op, HistoryEffect},
    session::Session,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

fn rig() -> (
    Session,
    Arc<Mutex<Controller>>,
    AudioProcessor,
    tempfile::TempDir,
) {
    let (controller, mut processor) = engine::engine(EngineConfig {
        sample_rate: 48000.0,
    });
    let controller = Arc::new(Mutex::new(controller));
    let mut session = Session::new(controller.clone());
    let mut project = Project::new();
    project.channels.push(
        serde_json::from_str(r#"{"id":1,"name":"Generated synth","color":0,"instrument":"synth"}"#)
            .unwrap(),
    );
    project.patterns[0].notes.push(Note {
        id: 1,
        channel: 1,
        tick: 0,
        length: 240,
        key: 60,
        velocity: 0.5,
        slide: false,
    });
    project.patterns[0].next_note_id = 2;
    project.next_id = 2;
    let dir = tempfile::tempdir().unwrap();
    session
        .open(
            &dir.path().join("generated.grooviq"),
            Loaded {
                project,
                samples: HashMap::new(),
                warnings: vec![],
            },
            DecodedSamples::default(),
        )
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    (session, controller, processor, dir)
}
fn edit(session: &mut Session, operations: Vec<Op>) -> Vec<u32> {
    let at = session.arrangement_view();
    session
        .edit_arrangement(at.generation, at.revision, operations, Instant::now())
        .unwrap()
        .created_ids
}
fn pattern(offset: u32, length: u64) -> ClipContent {
    ClipContent::Pattern {
        pattern: 0,
        offset_tick: offset,
        length_ticks: length,
    }
}

#[test]
fn pattern_clip_place_move_resize_copy_and_global_inverse_preserve_source_and_ids() {
    let (mut session, controller, mut processor, _dir) = rig();
    let initial = session.project().arrangement.clone();
    let ids = edit(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Track 1".into(),
                color: 0,
            },
            Op::CreateClipOnCreatedTrack {
                track_operation: 0,
                name: "Beat".into(),
                start_tick: 1920,
                muted: false,
                content: pattern(480, 1920),
            },
        ],
    );
    assert_eq!(ids.len(), 2);
    let (track, clip) = (ids[0], ids[1]);
    assert_ne!(track, clip);
    assert_eq!(session.project().arrangement.clips[0].track, track);
    edit(
        &mut session,
        vec![Op::Move {
            clip,
            track,
            start_tick: 2880,
        }],
    );
    edit(
        &mut session,
        vec![Op::Resize {
            clip,
            start_tick: 3120,
            content: pattern(720, 1680),
        }],
    );
    let copied = edit(
        &mut session,
        vec![Op::Copy {
            clip,
            track,
            start_tick: 4800,
        }],
    )[0];
    assert_ne!(copied, clip);
    assert_eq!(
        session.project().patterns.len(),
        1,
        "clip copy must remain linked"
    );
    let clips = &session.project().arrangement.clips;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].content, clips[1].content);
    assert_eq!(clips[0].content, pattern(720, 1680));
    assert_eq!(clips[0].start_tick, 3120);
    assert_eq!(clips[1].start_tick, 4800);
    let accepted = session.project().arrangement.clone();
    let next_id = session.project().next_id;
    for _ in 0..4 {
        let at = session.arrangement_view();
        let result = session
            .step_project_history(at.generation, at.revision, true, Instant::now())
            .unwrap();
        assert!(matches!(result.effect, HistoryEffect::Arrangement));
    }
    assert_eq!(session.project().arrangement, initial);
    assert_eq!(
        session.project().next_id,
        next_id,
        "Undo must not rewind the allocator"
    );
    for _ in 0..4 {
        let at = session.arrangement_view();
        assert!(matches!(
            session
                .step_project_history(at.generation, at.revision, false, Instant::now())
                .unwrap()
                .effect,
            HistoryEffect::Arrangement
        ));
    }
    assert_eq!(session.project().arrangement, accepted);
    assert_eq!(session.project().next_id, next_id);
    processor.process(&mut [0.0; 128], 2);
    assert!(
        !controller.lock().unwrap().live().playing,
        "editing must not start playback"
    );
}

#[test]
fn stale_clip_stamp_admission_failure_and_project_replacement_cannot_mutate_the_new_project() {
    let (mut session, controller, mut processor, _dir) = rig();
    let stale = session.arrangement_view();
    let track = edit(
        &mut session,
        vec![Op::CreateTrack {
            name: "Track 1".into(),
            color: 0,
        }],
    )[0];
    let before = session.project().clone();
    assert!(
        session
            .edit_arrangement(
                stale.generation,
                stale.revision,
                vec![Op::CreateClip {
                    track,
                    name: "Old".into(),
                    start_tick: 0,
                    muted: false,
                    content: pattern(0, 3840)
                }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &before);
    session.set_precommit_validator(Some(Arc::new(
        |_| Err("generated admission refusal".into()),
    )));
    let at = session.arrangement_view();
    assert!(
        session
            .edit_arrangement(
                at.generation,
                at.revision,
                vec![Op::CreateClip {
                    track,
                    name: "Refused".into(),
                    start_tick: 0,
                    muted: false,
                    content: pattern(0, 3840)
                }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &before);
    session.set_precommit_validator(None);
    let old = session.arrangement_view();
    session.new_project().unwrap();
    let replacement = session.project().clone();
    assert!(
        session
            .edit_arrangement(
                old.generation,
                old.revision,
                vec![Op::CreateClip {
                    track,
                    name: "Retired".into(),
                    start_tick: 0,
                    muted: false,
                    content: pattern(0, 3840)
                }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &replacement);
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
}

#[test]
fn pattern_source_metadata_is_actual_and_bound_to_the_arrangement_observation() {
    use grooviq_project::{pattern_edit::PatternEdit, session::Edit};
    let (mut session, controller, mut processor, _dir) = rig();
    let at = session.patterns();
    session
        .edit_patterns(
            at.generation,
            at.revision,
            PatternEdit::Create {
                name: "Short selected source".into(),
            },
            Instant::now(),
        )
        .unwrap();
    let at = session.patterns();
    let other = at.patterns.iter().find(|p| p.id != 0).unwrap().id;
    session
        .edit_patterns(
            at.generation,
            at.revision,
            PatternEdit::Select { pattern: other },
            Instant::now(),
        )
        .unwrap();
    session
        .apply(Edit::SetPatternLength { steps: 4 }, Instant::now())
        .unwrap();
    let data = serde_json::to_value(session.patterns()).unwrap();
    assert_eq!(
        data["arrangementRevision"],
        session.arrangement_view().revision
    );
    let sources = data["patterns"].as_array().unwrap();
    assert_eq!(sources.iter().find(|p| p["id"] == 0).unwrap()["steps"], 16);
    assert_eq!(
        sources.iter().find(|p| p["id"] == other).unwrap()["steps"],
        4
    );
    let old = session.arrangement_view().revision;
    session
        .apply(Edit::SetPatternLength { steps: 8 }, Instant::now())
        .unwrap();
    let data = serde_json::to_value(session.patterns()).unwrap();
    assert_eq!(
        data["arrangementRevision"],
        session.arrangement_view().revision
    );
    assert!(data["arrangementRevision"].as_u64().unwrap() > old);
    assert_eq!(
        data["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == other)
            .unwrap()["steps"],
        8
    );
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
}

#[test]
fn native_resize_requires_canonical_phase_below_at_and_above_the_source_cycle() {
    let (mut session, controller, mut processor, _dir) = rig();
    let ids = edit(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Original track".into(),
                color: 0,
            },
            Op::CreateClipOnCreatedTrack {
                track_operation: 0,
                name: "Repeated source".into(),
                start_tick: 960,
                muted: false,
                content: pattern(0, 11520),
            },
        ],
    );
    let clip = ids[1];
    for (shift, phase) in [(2880, 2880), (3840, 0), (4800, 960)] {
        let original = session.project().clone();
        let at = session.arrangement_view();
        if shift >= 3840 {
            let error = session
                .edit_arrangement(
                    at.generation,
                    at.revision,
                    vec![Op::Resize {
                        clip,
                        start_tick: 960 + shift,
                        content: pattern(shift as u32, 11520 - shift),
                    }],
                    Instant::now(),
                )
                .unwrap_err();
            assert!(error.message.contains("preserve pattern source phase"));
            assert_eq!(session.project(), &original);
        }
        edit(
            &mut session,
            vec![Op::Resize {
                clip,
                start_tick: 960 + shift,
                content: pattern(phase, 11520 - shift),
            }],
        );
        let current = &session.project().arrangement.clips[0];
        assert_eq!(current.id, clip);
        assert_eq!(current.start_tick, 960 + shift);
        assert_eq!(current.content, pattern(phase, 11520 - shift));
        assert!(session.view().project_can_undo);
        let at = session.arrangement_view();
        session
            .step_project_history(at.generation, at.revision, true, Instant::now())
            .unwrap();
        assert_eq!(session.project().arrangement, original.arrangement);
        assert_eq!(session.project().next_id, original.next_id);
    }
    // Creation/load permits a larger stored offset; Resize still needs its canonical phase.
    let loaded = edit(
        &mut session,
        vec![Op::CreateClip {
            track: ids[0],
            name: "Stored multi-cycle offset".into(),
            start_tick: 15360,
            muted: false,
            content: pattern(4320, 1920),
        }],
    )[0];
    edit(
        &mut session,
        vec![Op::Resize {
            clip: loaded,
            start_tick: 15360,
            content: pattern(480, 2160),
        }],
    );
    assert_eq!(
        session
            .project()
            .arrangement
            .clips
            .iter()
            .find(|c| c.id == loaded)
            .unwrap()
            .content,
        pattern(480, 2160)
    );
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
}
