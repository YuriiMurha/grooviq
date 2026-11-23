//! Selected-instance tool contracts through the existing Session; generated and offline.
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
    project.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "Retained track".into(),
        color: 0,
        mute: false,
    });
    for (id, start_tick, name) in [(3, 960, "Original instance"), (4, 6720, "Linked instance")] {
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
    project.next_id = 5;
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
fn edit(session: &mut Session, op: Op) {
    let at = session.arrangement_view();
    session
        .edit_arrangement(at.generation, at.revision, vec![op], Instant::now())
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
    json!({"clips":session.project().arrangement.clips,"tracks":session.project().arrangement.tracks,
        "nextId":session.project().next_id,"dirty":view.dirty,
        "canUndo":view.project_can_undo,"canRedo":view.project_can_redo})
}

#[test]
fn mute_and_unmute_touch_only_the_original_instance_and_keep_global_inverses() {
    let (mut session, controller, mut processor, _dir) = rig();
    let initial = session.project().clone();
    assert!(!session.view().project_can_undo);
    let mut trace = vec![json!({"stage":"initial","value":observed(&session)})];
    edit(
        &mut session,
        Op::SetClip {
            clip: 3,
            name: "Original instance".into(),
            muted: true,
        },
    );
    assert!(session.project().arrangement.clips[0].muted);
    assert_eq!(
        session.project().arrangement.clips[1],
        initial.arrangement.clips[1]
    );
    assert_eq!(
        session.project().arrangement.tracks,
        initial.arrangement.tracks
    );
    assert_eq!(session.project().patterns, initial.patterns);
    assert_eq!(session.project().next_id, initial.next_id);
    trace.push(json!({"stage":"muted","value":observed(&session)}));
    history(&mut session, true);
    assert_eq!(session.project().arrangement, initial.arrangement);
    trace.push(json!({"stage":"undone","value":observed(&session)}));
    history(&mut session, false);
    assert!(session.project().arrangement.clips[0].muted);
    trace.push(json!({"stage":"redone","value":observed(&session)}));
    edit(
        &mut session,
        Op::SetClip {
            clip: 3,
            name: "Original instance".into(),
            muted: false,
        },
    );
    assert_eq!(session.project().arrangement, initial.arrangement);
    trace.push(json!({"stage":"unmuted","value":observed(&session)}));
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
    println!(
        "M3_CLIP_ACTIONS_TRACE:{}",
        serde_json::to_string(&trace).unwrap()
    );
}

#[test]
fn delete_leaves_track_source_and_linked_instance_and_undo_restores_exact_identity() {
    let (mut session, controller, mut processor, _dir) = rig();
    let initial = session.project().clone();
    edit(&mut session, Op::DeleteClip { clip: 3 });
    assert_eq!(
        session.project().arrangement.clips,
        vec![initial.arrangement.clips[1].clone()]
    );
    assert_eq!(
        session.project().arrangement.tracks,
        initial.arrangement.tracks
    );
    assert_eq!(session.project().patterns, initial.patterns);
    assert_eq!(session.project().next_id, initial.next_id);
    history(&mut session, true);
    assert_eq!(session.project().arrangement, initial.arrangement);
    history(&mut session, false);
    assert_eq!(
        session.project().arrangement.clips,
        vec![initial.arrangement.clips[1].clone()]
    );
    assert_eq!(session.project().next_id, initial.next_id);
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
}

#[test]
fn delete_refusal_and_old_generation_cannot_consume_a_reused_identity() {
    let (mut session, controller, mut processor, _dir) = rig();
    let initial = session.project().clone();
    session.set_precommit_validator(Some(Arc::new(|_| Err("generated delete refusal".into()))));
    let at = session.arrangement_view();
    let error = session
        .edit_arrangement(
            at.generation,
            at.revision,
            vec![Op::DeleteClip { clip: 3 }],
            Instant::now(),
        )
        .unwrap_err();
    assert!(error.message.contains("generated delete refusal"));
    assert_eq!(session.project(), &initial);
    assert!(!session.view().project_can_undo);
    session.set_precommit_validator(None);
    let old = session.arrangement_view();
    session.new_project().unwrap();
    let at = session.arrangement_view();
    session
        .edit_arrangement(
            at.generation,
            at.revision,
            vec![
                Op::CreateTrack {
                    name: "Replacement".into(),
                    color: 1,
                },
                Op::CreateClipOnCreatedTrack {
                    track_operation: 0,
                    name: "New identity".into(),
                    start_tick: 0,
                    muted: false,
                    content: ClipContent::Pattern {
                        pattern: 0,
                        offset_tick: 0,
                        length_ticks: 3840,
                    },
                },
                Op::CreateClipOnCreatedTrack {
                    track_operation: 0,
                    name: "Reused numeric id".into(),
                    start_tick: 3840,
                    muted: false,
                    content: ClipContent::Pattern {
                        pattern: 0,
                        offset_tick: 0,
                        length_ticks: 3840,
                    },
                },
            ],
            Instant::now(),
        )
        .unwrap();
    let replacement = session.project().clone();
    assert!(
        replacement
            .arrangement
            .clips
            .iter()
            .any(|clip| clip.id == 3)
    );
    assert!(
        session
            .edit_arrangement(
                old.generation,
                old.revision,
                vec![Op::DeleteClip { clip: 3 }],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(session.project(), &replacement);
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
}
