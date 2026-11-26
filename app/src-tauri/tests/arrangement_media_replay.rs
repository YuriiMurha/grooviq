//! Drives the same command core as the Tauri adapters without opening a device.
use grooviq_app::arrangement_core::{
    arrangement_edit, arrangement_history, arrangement_view, arrangement_waveform, audio_cancel,
    audio_cancel_ticket, prepare_media,
};
use grooviq_engine::{EngineConfig, engine};
use grooviq_project::{
    audio_assets::AudioAssets,
    session::{MediaTarget, Session, decode_samples_budgeted_checked},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

fn steps() -> Vec<Value> {
    let step = |command: &str, args: Value| json!({"command": command, "args": args});
    let view = || step("arrangement_view", json!({"offset": 0, "limit": 32}));
    let edit = |generation, revision, edits_json: &str| {
        step(
            "arrangement_edit",
            json!({"generation": generation, "revision": revision, "editsJson": edits_json}),
        )
    };
    let history = |generation, revision, undo| {
        step(
            "arrangement_history",
            json!({"generation": generation, "revision": revision, "undo": undo}),
        )
    };
    let import = |revision, track, path: &str, ticket: u64| {
        step(
            "audio_import",
            json!({"generation": 0, "revision": revision, "track": track, "startTick": 240,
            "name": "  Take 01 / edit  ", "path": path, "ticket": ticket}),
        )
    };
    let relink = |generation, revision, path: &str, ticket: u64| {
        step(
            "audio_relink",
            json!({"generation": generation, "revision": revision, "asset": 2, "path": path, "ticket": ticket}),
        )
    };
    let waveform = |generation, start, end| {
        step(
            "arrangement_waveform",
            json!({"generation": generation, "asset": 2, "request": 777, "start": start, "end": end, "bins": 2}),
        )
    };
    let audio_resize = |asset, source_start, source_end, gain, fade_in, fade_out| {
        json!({"kind":"resize", "clip":3, "startTick":480, "content":{
            "type":"audio", "asset":asset, "sourceStart":source_start, "sourceEnd":source_end,
            "gain":gain, "reverse":true, "fadeIn":fade_in, "fadeOut":fade_out
        }})
    };
    let edit_audio = |revision, ops: Vec<Value>| edit(0, revision, &json!(ops).to_string());
    vec![
        edit(0, 0, r#"[{"kind":"createTrack","name":"Audio","color":0}]"#),
        import(0, 1, "/qa/generated.wav", 11),
        import(1, 2, "/qa/generated.wav", 12),
        step("audio_cancel_ticket", json!({"ticket": 0})),
        step(
            "audio_cancel_ticket",
            json!({"ticket": 9_007_199_254_740_992_u64}),
        ),
        import(1, 1, "/qa/never-read.wav", 0),
        import(1, 1, "/qa/never-read.wav", 9_007_199_254_740_992),
        step(
            "audio_cancel_ticket",
            json!({"ticket": 9_007_199_254_740_991_u64}),
        ),
        import(1, 1, "/qa/never-read.wav", 9_007_199_254_740_991),
        import(1, 1, "/qa/refused.wav", 13),
        view(),
        import(1, 1, "/qa/generated.wav", 13),
        step("audio_cancel", json!({"request": 5})),
        step("audio_cancel_ticket", json!({"ticket": 13})),
        view(),
        history(0, 2, true),
        view(),
        waveform(0, 1, 8),
        history(0, 3, false),
        waveform(0, 1, 8),
        edit(
            0,
            4,
            r#"[{"kind":"createTrack","name":"Patterns","color":1}]"#,
        ),
        edit(
            0,
            5,
            r#"[{"kind":"createClip","track":4,"name":"Pattern clip","startTick":0,"muted":false,"content":{"type":"pattern","pattern":0,"offsetTick":0,"lengthTicks":960}}]"#,
        ),
        relink(0, 6, "/qa/relinked.wav", 24),
        view(),
        waveform(0, 1, 8),
        waveform(0, 0, 3),
        history(0, 7, true),
        view(),
        waveform(0, 1, 8),
        history(0, 8, false),
        view(),
        waveform(0, 0, 3),
        // Relink has three decoded frames, but resizing and slicing use the saved eight.
        edit_audio(9, vec![audio_resize(2, 1, 9, 1.0, 0, 0)]),
        edit_audio(9, vec![audio_resize(2, 2, 2, 1.0, 0, 0)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, 1.0, 7, 0)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, 1.0, 0, 7)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, 2.1, 0, 0)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, -0.1, 0, 0)]),
        edit_audio(9, vec![audio_resize(99, 1, 7, 1.0, 0, 0)]),
        edit(
            0,
            9,
            r#"[{"kind":"move","clip":3,"track":99,"startTick":480}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"move","clip":99,"track":98,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"copy","clip":99,"track":98,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"copy","clip":99,"track":4,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"move","clip":5,"track":98,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"copy","clip":3,"track":4,"startTick":38400000}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":240,"audioFirstFrames":3}]"#,
        ),
        edit(0, 9, r#"[{"kind":"slice","clip":3,"tick":241}]"#),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":241,"audioFirstFrames":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":241,"audioFirstFrames":8}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":242,"audioFirstFrames":3}]"#,
        ),
        edit_audio(
            9,
            vec![
                audio_resize(2, 7, 1, 1.0, 0, 0),
                json!({"kind":"slice","clip":3,"tick":481,"audioFirstFrames":3}),
            ],
        ),
        edit_audio(
            9,
            vec![
                json!({"kind":"copy","clip":3,"track":1,"startTick":720}),
                audio_resize(2, 1, 9, 1.0, 0, 0),
            ],
        ),
        view(),
        edit_audio(
            9,
            vec![
                json!({"kind":"move","clip":3,"track":4,"startTick":360}),
                audio_resize(2, 1, 7, 2.0, 5, 5),
                json!({"kind":"setClip","clip":3,"name":"Reverse take","muted":true}),
                json!({"kind":"copy","clip":3,"track":1,"startTick":720}),
            ],
        ),
        view(),
        edit(
            0,
            10,
            r#"[{"kind":"slice","clip":3,"tick":481,"audioFirstFrames":2},{"kind":"deleteClip","clip":6}]"#,
        ),
        view(),
        history(0, 11, true),
        view(),
        history(0, 12, false),
        view(),
        // Even with no visible clip delta, the batch issued an ID and owns one undo step.
        edit(
            0,
            13,
            r#"[{"kind":"copy","clip":3,"track":1,"startTick":720},{"kind":"deleteClip","clip":8}]"#,
        ),
        history(0, 14, true),
        view(),
        edit(
            0,
            15,
            r#"[{"kind":"copy","clip":5,"track":4,"startTick":960},{"kind":"deleteClip","clip":9}]"#,
        ),
        history(0, 16, true),
        view(),
        step("project_save_as", json!({"path": "/qa/media.grooviq"})),
        step("project_new", json!({})),
        step("project_open", json!({"path": "/qa/media.grooviq"})),
        view(),
        waveform(2, 0, 3),
        relink(2, 0, "/qa/mono.wav", 9_007_199_254_740_991),
        view(),
        waveform(2, 0, 4),
        history(2, 1, true),
        view(),
        waveform(2, 0, 3),
        history(2, 2, false),
        view(),
        waveform(2, 0, 4),
    ]
}

#[test]
fn arrangement_media_matches_shared_replay() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/arrangement-media-replay.json"
    );
    let mut fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let steps = steps();
    let update = std::env::var("GROOVIQ_UPDATE_REPLAY").as_deref() == Ok("1");
    if update {
        fixture["steps"] = json!(steps);
    }
    assert_eq!(json!(steps), fixture["steps"]);
    let dir = tempfile::tempdir().unwrap();
    let local = |path: &str| dir.path().join(Path::new(path).file_name().unwrap());
    for (virtual_path, bytes) in fixture["files"].as_object().unwrap() {
        let bytes = if bytes.is_null() {
            // A long enough file for probing, with no recognized container.
            vec![0; 128]
        } else {
            serde_json::from_value::<Vec<u8>>(bytes.clone()).unwrap()
        };
        std::fs::write(local(virtual_path), bytes).unwrap();
    }
    let (controller, mut processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let session = Mutex::new(Session::new(Arc::new(Mutex::new(controller))));
    let assets = AudioAssets::with_budget(session.lock().unwrap().media_budget());
    let mut actual = Vec::new();
    for step in &steps {
        let args = &step["args"];
        let n = |key: &str| args[key].as_u64().unwrap();
        let string = |key: &str| args[key].as_str().unwrap();
        let outcome: Result<Value, String> = match step["command"].as_str().unwrap() {
            "arrangement_edit" => Ok(json!(arrangement_edit(
                &session,
                n("generation"),
                n("revision"),
                string("editsJson"),
            ))),
            "arrangement_view" => {
                arrangement_view(&session, n("offset") as usize, n("limit") as usize)
            }
            "audio_import" | "audio_relink" => {
                let target = if step["command"] == "audio_import" {
                    MediaTarget::Import {
                        track: n("track") as u32,
                        start_tick: n("startTick"),
                        name: string("name").into(),
                    }
                } else {
                    MediaTarget::Relink {
                        asset: n("asset") as u32,
                    }
                };
                Ok(json!(tauri::async_runtime::block_on(prepare_media(
                    &session,
                    &assets,
                    n("generation"),
                    n("revision"),
                    target,
                    local(string("path")).to_str().unwrap().into(),
                    n("ticket"),
                ))))
            }
            "audio_cancel" => Ok(json!(audio_cancel(&session, n("request")))),
            "audio_cancel_ticket" => Ok(json!(audio_cancel_ticket(&session, n("ticket")))),
            "arrangement_history" => Ok(arrangement_history(
                &session,
                n("generation"),
                n("revision"),
                args["undo"].as_bool().unwrap(),
            )),
            "arrangement_waveform" => tauri::async_runtime::block_on(arrangement_waveform(
                &session,
                n("generation"),
                n("asset") as u32,
                n("request"),
                n("start") as usize,
                n("end") as usize,
                n("bins") as usize,
            ))
            .map(|bytes| json!(bytes)),
            // Project command replies include host-dependent paths. Their effects are
            // captured by the following view and waveform reads.
            "project_save_as" => {
                session
                    .lock()
                    .unwrap()
                    .save(Some(&local(string("path"))), dir.path())
                    .unwrap();
                Ok(Value::Null)
            }
            "project_new" => {
                session.lock().unwrap().new_project().unwrap();
                Ok(Value::Null)
            }
            "project_open" => {
                let path = local(string("path"));
                let mut session = session.lock().unwrap();
                let budget = session.media_budget();
                let loaded = grooviq_project::load_budgeted(&path, &budget).unwrap();
                let decoded = decode_samples_budgeted_checked(&loaded, &budget).unwrap();
                let opened = session.open(&path, loaded, decoded).unwrap();
                Ok(json!(opened.warnings))
            }
            other => panic!("unhandled arrangement media replay command {other}"),
        };
        actual.push(match outcome {
            Ok(value) => json!({"ok": value}),
            Err(error) => json!({"error": error}),
        });
        processor.process(&mut [0.0; 128], 2);
    }
    if update {
        fixture["expected"] = json!(actual);
        std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
    }
    assert_eq!(
        json!(actual),
        fixture["expected"],
        "real Session media replay drift"
    );
}
