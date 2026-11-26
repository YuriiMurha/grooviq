//! Shared semantic transcripts: the browser mock must produce these same replies and bytes.
#[path = "semantic_replay/media.rs"]
mod media;
use grooviq_engine::{EngineConfig, engine};
use grooviq_project::{
    notes::NotesEdit,
    session::{Edit, PatternStamp, Session},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

// Serialize through the actual JSON wire format: to_value widens f32 to f64.
fn wire(value: impl serde::Serialize) -> Value {
    serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap()
}

#[test]
fn session_matches_shared_semantic_replay() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/session-replay.json"
    );
    let mut fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (controller, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut session = Session::new(Arc::new(Mutex::new(controller)));
    let mut actual = Vec::new();
    for step in fixture["steps"].as_array().unwrap() {
        let a = &step["args"];
        let generation = || {
            a["generation"]
                .as_u64()
                .expect("command requires generation")
        };
        let now = Instant::now();
        let result = match step["command"].as_str().unwrap() {
            "project_view" => Ok(wire(session.view())),
            "project_edit" => {
                let edit = serde_json::from_value::<Edit>(a["edit"].clone()).unwrap();
                let scoped = matches!(
                    edit,
                    Edit::ToggleStep { .. }
                        | Edit::SetStepVelocity { .. }
                        | Edit::ClearStep { .. }
                        | Edit::SetPatternLength { .. }
                        | Edit::SetSwing { .. }
                );
                if generation() != session.generation() || !scoped {
                    session.apply_in(generation(), edit, now).map(wire)
                } else {
                    session
                        .apply_stamped(
                            serde_json::from_value::<PatternStamp>(a["stamp"].clone()).unwrap(),
                            edit,
                            now,
                        )
                        .map(wire)
                }
            }
            "channel_add_synth" => session
                .add_synth_channel_in(generation(), a["preset"].as_str().unwrap(), now)
                .map(wire),
            "notes_edit" => session
                .edit_notes(
                    serde_json::from_value::<NotesEdit>(a["edit"].clone()).unwrap(),
                    now,
                )
                .map(wire),
            "notes_undo" | "notes_redo" => session
                .history_stamped(
                    serde_json::from_value::<PatternStamp>(a["stamp"].clone()).unwrap(),
                    step["command"] == "notes_undo",
                    now,
                )
                .map(wire),
            "project_new" => session.new_project().map(wire),
            other => panic!("unhandled replay command {other}"),
        };
        let reply = match result {
            Ok(v) => json!({"ok":v}),
            Err(e) => json!({"error":e.to_string()}),
        };
        actual.push(json!({"reply":reply,"view":wire(session.view()),"notes":session.notes()}));
    }
    // Explicit maintenance mode; ordinary cargo tests only verify, never bless drift.
    if std::env::var("GROOVIQ_UPDATE_REPLAY").as_deref() == Ok("1") {
        fixture["expected"] = json!(actual);
        std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
    }
    assert_eq!(
        json!(actual),
        fixture["expected"],
        "real Session replay drift"
    );
    media::verify();
}
