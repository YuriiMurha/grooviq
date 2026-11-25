//! Finite production Session trace for the real first playlist UI flow; no audio device.
use grooviq_engine::{self as engine, EngineConfig};
use grooviq_project::{
    ClipContent, DecodedSamples, Loaded, Note, Project, arrangement_edit::ArrangementEdit as Op,
    session::Session,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

fn state(session: &Session) -> Value {
    let view = session.view();
    json!({"clips": session.project().arrangement.clips, "tracks": session.project().arrangement.tracks,
        "nextId": session.project().next_id, "selected": session.patterns().selected,
        "playMode": session.project().play_mode, "dirty": view.dirty,
        "canUndo": view.project_can_undo, "canRedo": view.project_can_redo})
}
fn edit(session: &mut Session, operations: Vec<Op>) -> Vec<u32> {
    let at = session.arrangement_view();
    session
        .edit_arrangement(at.generation, at.revision, operations, Instant::now())
        .unwrap()
        .created_ids
}
fn content(length: u64) -> ClipContent {
    ClipContent::Pattern {
        pattern: 0,
        offset_tick: 0,
        length_ticks: length,
    }
}

#[test]
fn first_playlist_ui_sequence_records_actual_linked_clips_and_global_history() {
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
    let mut trace = vec![json!({"stage":"initial","value":state(&session)})];
    let ids = edit(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Track 1".into(),
                color: 0,
            },
            Op::CreateClipOnCreatedTrack {
                track_operation: 0,
                name: "Pattern 1".into(),
                start_tick: 960,
                muted: false,
                content: content(3840),
            },
        ],
    );
    let (track, clip) = (ids[0], ids[1]);
    assert_eq!(
        session.project().arrangement.clips[0].content,
        content(3840)
    );
    trace.push(json!({"stage":"placed","value":state(&session)}));
    edit(
        &mut session,
        vec![Op::Move {
            clip,
            track,
            start_tick: 2880,
        }],
    );
    assert_eq!(session.project().arrangement.clips[0].start_tick, 2880);
    trace.push(json!({"stage":"moved","value":state(&session)}));
    edit(
        &mut session,
        vec![Op::Resize {
            clip,
            start_tick: 2880,
            content: content(4800),
        }],
    );
    trace.push(json!({"stage":"resized","value":state(&session)}));
    edit(
        &mut session,
        vec![Op::Resize {
            clip,
            start_tick: 3840,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 960,
                length_ticks: 3840,
            },
        }],
    );
    assert_eq!(session.project().arrangement.clips[0].start_tick, 3840);
    trace.push(json!({"stage":"trimmed","value":state(&session)}));
    let copied = edit(
        &mut session,
        vec![Op::Copy {
            clip,
            track,
            start_tick: 7680,
        }],
    )[0];
    assert_ne!(copied, clip);
    assert_eq!(session.project().patterns.len(), 1);
    let accepted = session.project().arrangement.clone();
    let allocator = session.project().next_id;
    trace.push(json!({"stage":"copied","value":state(&session)}));
    let at = session.arrangement_view();
    session
        .step_project_history(at.generation, at.revision, true, Instant::now())
        .unwrap();
    assert_eq!(session.project().arrangement.clips.len(), 1);
    assert_eq!(session.project().next_id, allocator);
    trace.push(json!({"stage":"undone","value":state(&session)}));
    let at = session.arrangement_view();
    session
        .step_project_history(at.generation, at.revision, false, Instant::now())
        .unwrap();
    assert_eq!(session.project().arrangement, accepted);
    assert_eq!(session.project().next_id, allocator);
    trace.push(json!({"stage":"redone","value":state(&session)}));
    processor.process(&mut [0.0; 128], 2);
    assert!(!controller.lock().unwrap().live().playing);
    println!(
        "M3_PLAYLIST_UI_TRACE:{}",
        serde_json::to_string(&trace).unwrap()
    );
}
