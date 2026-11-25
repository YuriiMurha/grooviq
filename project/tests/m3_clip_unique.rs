//! Generated offline MakeUnique/selection/global-history contracts; no audio device.
use grooviq_engine::{self as engine, AudioProcessor, Controller, EngineConfig};
use grooviq_project::{
    Clip, ClipContent, DecodedSamples, Loaded, Note, PlaylistTrack, Project,
    arrangement_edit::ArrangementEdit as Op,
    notes::{NewNote, NotesEdit},
    pattern_edit::PatternEdit,
    session::Session,
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
    project.patterns[0].name = "Source pattern".into();
    for (index, tick) in [0, 960, 1920, 2880].into_iter().enumerate() {
        project.patterns[0].notes.push(Note {
            id: index as u32 + 1,
            channel: 1,
            tick,
            length: 240,
            key: 60,
            velocity: 0.5,
            slide: false,
        });
    }
    project.patterns[0].next_note_id = 5;
    let mut other = project.patterns[0].clone();
    other.id = 3;
    other.name = "Unrelated short pattern".into();
    other.steps = 4;
    other.notes.clear();
    other.next_note_id = 1;
    project.patterns.push(other);
    project.current_pattern = 3;
    project.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "Retained track".into(),
        color: 0,
        mute: false,
    });
    for (id, start_tick, name) in [(4, 960, "Original instance"), (5, 6720, "Linked instance")] {
        project.arrangement.clips.push(Clip {
            id,
            track: 2,
            name: name.into(),
            start_tick,
            muted: false,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 480,
                length_ticks: 3840,
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
fn unique(session: &mut Session) -> Vec<u32> {
    let at = session.arrangement_view();
    session
        .edit_arrangement(
            at.generation,
            at.revision,
            vec![Op::MakeUnique { clip: 4 }],
            Instant::now(),
        )
        .unwrap()
        .created_ids
}
fn select(session: &mut Session, pattern: u32) {
    let at = session.patterns();
    session
        .edit_patterns(
            at.generation,
            at.revision,
            PatternEdit::Select { pattern },
            Instant::now(),
        )
        .unwrap();
}
fn history(session: &mut Session, undo: bool) {
    let at = session.arrangement_view();
    session
        .step_project_history(at.generation, at.revision, undo, Instant::now())
        .unwrap();
}
fn observed(session: &Session) -> Value {
    let view = session.view();
    // Saved Pattern fields intentionally omit its transient revision; admission stamps
    // are asserted independently, not inferred or removed after a failed comparison.
    json!({"clips":session.project().arrangement.clips,"tracks":session.project().arrangement.tracks,
        "patterns":session.project().patterns,"nextId":session.project().next_id,
        "selectedPattern":view.pattern.id,"dirty":view.dirty,
        "canUndo":view.project_can_undo,"canRedo":view.project_can_redo})
}

#[test]
fn unique_clones_the_actual_unselected_source_and_only_retargets_the_original_instance() {
    let (mut session, _controller, _processor, _dir) = rig();
    let before = session.project().clone();
    assert_eq!(session.view().pattern.id, 3);
    assert_eq!(unique(&mut session), vec![6]);
    let after = session.project();
    let mut clone = before.patterns[0].clone();
    clone.id = 6;
    clone.revision = 1;
    assert_eq!(
        after.patterns,
        vec![
            before.patterns[0].clone(),
            before.patterns[1].clone(),
            clone
        ]
    );
    let mut clip = before.arrangement.clips[0].clone();
    clip.content = ClipContent::Pattern {
        pattern: 6,
        offset_tick: 480,
        length_ticks: 3840,
    };
    assert_eq!(
        after.arrangement.clips,
        vec![clip, before.arrangement.clips[1].clone()]
    );
    assert_eq!(after.arrangement.tracks, before.arrangement.tracks);
    assert_eq!(after.current_pattern, 3);
    assert_eq!(after.next_id, 7);
    assert!(session.view().project_can_undo);
    select(&mut session, 6);
    assert_eq!(session.view().pattern.id, 6);
    assert_eq!(session.current_pattern().notes, before.patterns[0].notes);
    assert!(session.view().project_can_undo);
}

#[test]
fn cloned_notes_and_global_inverse_order_preserve_source_and_monotonic_allocators() {
    let (mut session, controller, mut processor, _dir) = rig();
    let before = session.project().clone();
    let mut trace = vec![json!({"stage":"initial","value":observed(&session)})];
    unique(&mut session);
    trace.push(json!({"stage":"unique","value":observed(&session)}));
    select(&mut session, 6);
    trace.push(json!({"stage":"selected","value":observed(&session)}));
    let view = session.view();
    let ack = session
        .edit_notes(
            NotesEdit {
                generation: view.generation,
                pattern_id: 6,
                base_revision: view.notes_revision,
                add: vec![NewNote {
                    channel: 1,
                    tick: 240,
                    length: 240,
                    key: 60,
                    velocity: 0.5,
                    slide: false,
                }],
                update: vec![],
                delete: vec![],
            },
            Instant::now(),
        )
        .unwrap();
    assert_eq!(ack.added_ids, vec![5]);
    assert!(!ack.reload);
    assert_eq!(session.project().patterns[0], before.patterns[0]);
    assert_eq!(
        session.project().arrangement.clips[1],
        before.arrangement.clips[1]
    );
    assert_eq!(session.current_pattern().notes.len(), 5);
    trace.push(json!({"stage":"edited","value":observed(&session)}));
    history(&mut session, true);
    assert_eq!(session.current_pattern().notes, before.patterns[0].notes);
    assert_eq!(session.current_pattern().next_note_id, 6);
    trace.push(json!({"stage":"noteUndone","value":observed(&session)}));
    history(&mut session, true);
    assert_eq!(session.project().arrangement, before.arrangement);
    assert_eq!(session.project().patterns, before.patterns);
    assert_eq!(session.project().next_id, 7);
    assert_eq!(session.view().pattern.id, 0);
    trace.push(json!({"stage":"uniqueUndone","value":observed(&session)}));
    history(&mut session, false);
    assert_eq!(
        session
            .project()
            .patterns
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        vec![0, 3, 6]
    );
    assert_eq!(session.project().patterns[2].next_note_id, 6);
    assert_eq!(session.view().pattern.id, 0);
    trace.push(json!({"stage":"uniqueRedone","value":observed(&session)}));
    history(&mut session, false);
    assert_eq!(session.project().patterns[2].notes.len(), 5);
    assert_eq!(session.project().patterns[2].notes.last().unwrap().id, 5);
    assert_eq!(session.project().patterns[0], before.patterns[0]);
    assert_eq!(session.project().next_id, 7);
    trace.push(json!({"stage":"noteRedone","value":observed(&session)}));
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
    println!(
        "M3_CLIP_UNIQUE_TRACE:{}",
        serde_json::to_string(&trace).unwrap()
    );
}

#[test]
fn clone_refusal_does_not_allocate_and_selection_refusal_keeps_the_accepted_clone() {
    let (mut session, _controller, _processor, _dir) = rig();
    let before = session.project().clone();
    session.set_precommit_validator(Some(Arc::new(|_| Err("generated unique refusal".into()))));
    let at = session.arrangement_view();
    assert!(
        session
            .edit_arrangement(
                at.generation,
                at.revision,
                vec![Op::MakeUnique { clip: 4 }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &before);
    assert!(!session.view().project_can_undo);
    session.set_precommit_validator(None);
    assert_eq!(unique(&mut session), vec![6]);
    let accepted = session.project().clone();
    session.set_precommit_validator(Some(Arc::new(
        |_| Err("generated selection refusal".into()),
    )));
    let at = session.patterns();
    assert!(
        session
            .edit_patterns(
                at.generation,
                at.revision,
                PatternEdit::Select { pattern: 6 },
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &accepted);
    assert!(session.view().project_can_undo);
    assert_eq!(session.project().next_id, 7);
    assert_eq!(session.view().pattern.id, 3);
    session.set_precommit_validator(None);
    select(&mut session, 6);
    assert_eq!(session.project().next_id, 7);
    assert_eq!(session.current_pattern().notes, before.patterns[0].notes);
}

#[test]
fn stale_generation_and_revision_cannot_clone_a_different_or_reused_clip() {
    let (mut session, _controller, mut processor, _dir) = rig();
    let old = session.arrangement_view();
    unique(&mut session);
    let accepted = session.project().clone();
    assert!(
        session
            .edit_arrangement(
                old.generation,
                old.revision,
                vec![Op::MakeUnique { clip: 5 }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &accepted);
    session.new_project().unwrap();
    processor.process(&mut [0.0; 128], 2);
    let at = session.arrangement_view();
    let mut ops = vec![Op::CreateTrack {
        name: "Replacement".into(),
        color: 0,
    }];
    for start_tick in [0, 3840, 7680] {
        ops.push(Op::CreateClipOnCreatedTrack {
            track_operation: 0,
            name: "New instance".into(),
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
        .edit_arrangement(at.generation, at.revision, ops, Instant::now())
        .unwrap();
    let replacement = session.project().clone();
    assert!(replacement.arrangement.clips.iter().any(|c| c.id == 4));
    let at = session.arrangement_view();
    assert!(
        session
            .edit_arrangement(
                old.generation,
                at.revision,
                vec![Op::MakeUnique { clip: 4 }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &replacement);
}
