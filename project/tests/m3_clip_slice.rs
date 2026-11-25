//! Generated offline contracts for the UI Slice gesture; no audio device or owner data.
use grooviq_engine::{self as engine, AudioProcessor, Controller, EngineConfig};
use grooviq_project::{
    Clip, ClipContent, DecodedSamples, Loaded, Note, PlaylistTrack, Project,
    arrangement_edit::ArrangementEdit as Op, session::Session,
};
use serde_json::{Value, json};
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
        serde_json::from_str(r#"{"id":1,"name":"Generated","color":0,"instrument":"synth"}"#)
            .unwrap(),
    );
    for (index, tick) in [0, 960, 1920, 2880].into_iter().enumerate() {
        project.patterns[0].notes.push(Note {
            id: index as u32 + 1,
            channel: 1,
            tick,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
        });
    }
    project.patterns[0].next_note_id = 5;
    let mut selected = project.patterns[0].clone();
    selected.id = 3;
    selected.name = "Unrelated short pattern".into();
    selected.steps = 4;
    selected.notes.clear();
    selected.next_note_id = 1;
    project.patterns.push(selected);
    project.current_pattern = 3;
    project.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "Retained track".into(),
        color: 0,
        mute: false,
    });
    for (id, start_tick, length_ticks, name) in [
        (4, 960, 7680, "Original instance"),
        (5, 9600, 3840, "Linked instance"),
    ] {
        project.arrangement.clips.push(Clip {
            id,
            track: 2,
            name: name.into(),
            start_tick,
            muted: false,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 480,
                length_ticks,
            },
        });
    }
    project.next_id = 6;
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
fn slice(session: &mut Session, tick: u64) -> Vec<u32> {
    let at = session.arrangement_view();
    session
        .edit_arrangement(
            at.generation,
            at.revision,
            vec![Op::Slice {
                clip: 4,
                tick,
                audio_first_frames: None,
            }],
            Instant::now(),
        )
        .unwrap()
        .created_ids
}
fn history(session: &mut Session, undo: bool) {
    let at = session.arrangement_view();
    session
        .step_project_history(at.generation, at.revision, undo, Instant::now())
        .unwrap();
}
fn observed(session: &Session) -> Value {
    let view = session.view();
    json!({"clips":session.project().arrangement.clips,"tracks":session.project().arrangement.tracks,
        "nextId":session.project().next_id,"dirty":view.dirty,
        "canUndo":view.project_can_undo,"canRedo":view.project_can_redo,"selectedPattern":view.pattern.id})
}

#[test]
fn slice_preserves_left_identity_source_and_global_inverse_without_rewinding_allocator() {
    let (mut session, controller, mut processor, _dir) = rig();
    let initial = session.project().clone();
    assert_eq!(session.view().pattern.id, 3);
    assert!(!session.view().project_can_undo);
    let mut trace = vec![json!({"stage":"initial","value":observed(&session)})];
    assert_eq!(slice(&mut session, 2880), vec![6]);
    let mut left = initial.arrangement.clips[0].clone();
    left.content = ClipContent::Pattern {
        pattern: 0,
        offset_tick: 480,
        length_ticks: 1920,
    };
    let mut right = initial.arrangement.clips[0].clone();
    right.id = 6;
    right.start_tick = 2880;
    right.content = ClipContent::Pattern {
        pattern: 0,
        offset_tick: 2400,
        length_ticks: 5760,
    };
    let expected = vec![left, initial.arrangement.clips[1].clone(), right];
    assert_eq!(session.project().arrangement.clips, expected);
    assert_eq!(session.project().patterns, initial.patterns);
    assert_eq!(
        session.project().arrangement.tracks,
        initial.arrangement.tracks
    );
    assert_eq!(session.project().next_id, 7);
    assert_eq!(session.project().current_pattern, 3);
    assert!(session.view().dirty);
    trace.push(json!({"stage":"sliced","value":observed(&session)}));
    history(&mut session, true);
    assert_eq!(session.project().arrangement, initial.arrangement);
    assert_eq!(session.project().next_id, 7);
    assert!(!session.view().project_can_undo);
    assert!(session.view().project_can_redo);
    trace.push(json!({"stage":"undone","value":observed(&session)}));
    history(&mut session, false);
    assert_eq!(session.project().arrangement.clips, expected);
    assert_eq!(session.project().next_id, 7);
    trace.push(json!({"stage":"redone","value":observed(&session)}));
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
    println!(
        "M3_CLIP_SLICE_TRACE:{}",
        serde_json::to_string(&trace).unwrap()
    );
}

#[test]
fn source_phase_wraps_at_actual_unselected_source_cycle() {
    // Wrong selected-pattern period960 would produce480 in the first/last rows.
    for (tick, left_length, right_phase, right_length) in [
        (2880, 1920, 2400, 5760),
        (4800, 3840, 480, 3840),
        (5760, 4800, 1440, 2880),
    ] {
        let (mut session, _controller, _processor, _dir) = rig();
        let initial = session.project().clone();
        assert_eq!(slice(&mut session, tick), vec![6]);
        assert_eq!(
            session.project().arrangement.clips[0].content,
            ClipContent::Pattern {
                pattern: 0,
                offset_tick: 480,
                length_ticks: left_length
            }
        );
        assert_eq!(
            session.project().arrangement.clips[2].content,
            ClipContent::Pattern {
                pattern: 0,
                offset_tick: right_phase,
                length_ticks: right_length
            }
        );
        assert_eq!(
            session.project().arrangement.clips[1],
            initial.arrangement.clips[1]
        );
        assert_eq!(session.project().patterns, initial.patterns);
    }
}

#[test]
fn endpoint_and_precommit_refusal_leave_all_content_allocation_and_history_unchanged() {
    let (mut session, _controller, _processor, _dir) = rig();
    let initial = session.project().clone();
    for tick in [0, 960, 8640, 9600] {
        let at = session.arrangement_view();
        assert!(
            session
                .edit_arrangement(
                    at.generation,
                    at.revision,
                    vec![Op::Slice {
                        clip: 4,
                        tick,
                        audio_first_frames: None
                    }],
                    Instant::now()
                )
                .is_err()
        );
        assert_eq!(session.project(), &initial);
        assert_eq!(session.arrangement_view().revision, at.revision);
        assert!(!session.view().project_can_undo);
    }
    session.set_precommit_validator(Some(Arc::new(|_| Err("generated slice refusal".into()))));
    let at = session.arrangement_view();
    let error = session
        .edit_arrangement(
            at.generation,
            at.revision,
            vec![Op::Slice {
                clip: 4,
                tick: 2880,
                audio_first_frames: None,
            }],
            Instant::now(),
        )
        .unwrap_err();
    assert!(error.message.contains("generated slice refusal"));
    assert_eq!(session.project(), &initial);
    assert!(!session.view().project_can_undo);
    session.set_precommit_validator(None);
    assert_eq!(slice(&mut session, 2880), vec![6]);
}

#[test]
fn stale_revision_and_replacement_cannot_slice_an_original_or_reused_numeric_identity() {
    let (mut session, _controller, mut processor, _dir) = rig();
    let old = session.arrangement_view();
    slice(&mut session, 2880);
    let accepted = session.project().clone();
    assert!(
        session
            .edit_arrangement(
                old.generation,
                old.revision,
                vec![Op::Slice {
                    clip: 4,
                    tick: 1920,
                    audio_first_frames: None
                }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &accepted);
    session.new_project().unwrap();
    processor.process(&mut [0.0; 128], 2);
    let at = session.arrangement_view();
    let mut operations = vec![Op::CreateTrack {
        name: "Replacement".into(),
        color: 0,
    }];
    for start_tick in [0, 3840, 7680] {
        operations.push(Op::CreateClipOnCreatedTrack {
            track_operation: 0,
            name: "Replacement instance".into(),
            start_tick,
            muted: false,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 0,
                length_ticks: 3840,
            },
        });
    }
    session
        .edit_arrangement(at.generation, at.revision, operations, Instant::now())
        .unwrap();
    let replacement = session.project().clone();
    assert!(
        replacement
            .arrangement
            .clips
            .iter()
            .any(|clip| clip.id == 4)
    );
    let current = session.arrangement_view();
    assert_ne!(current.generation, old.generation);
    assert!(
        session
            .edit_arrangement(
                old.generation,
                current.revision,
                vec![Op::Slice {
                    clip: 4,
                    tick: 8640,
                    audio_first_frames: None
                }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &replacement);
}
