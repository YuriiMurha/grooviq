//! Adapter projections for arrangement_cmds.rs. All edits, decoding, peaks and archive I/O
//! run through the real offline Session, with only the Tauri runtime and IPC removed.
use super::wire;
use grooviq_engine::{EngineConfig, engine};
use grooviq_project::{
    arrangement_edit::{ArrangementAck, parse_batch},
    audio_assets::AudioAssets,
    session::{MediaTarget, Session, decode_samples_budgeted_checked},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

fn reply(session: &Session, result: Result<ArrangementAck, String>, request: Option<u64>) -> Value {
    let stamp = session.arrangement_view();
    let (status, changed, ids, reload, message) = match result {
        Ok(ack) => ("applied", ack.changed, ack.created_ids, false, None),
        Err(message) => ("refused", false, vec![], true, Some(message)),
    };
    json!({
        "status": status, "generation": stamp.generation, "revision": stamp.revision,
        "request": request, "createdIds": ids, "changed": changed, "reload": reload,
        "message": message, "warnings": session.media_warnings(),
    })
}

fn prepare(
    session: &mut Session,
    assets: &AudioAssets,
    args: &Value,
    target: MediaTarget,
    file: &Path,
) -> Value {
    let n = |key: &str| args[key].as_u64().unwrap();
    let intent = match session.media_intent(n("generation"), n("revision"), target) {
        Ok(intent) => intent,
        Err(error) => return reply(session, Err(error), None),
    };
    let result = (|| {
        let file = grooviq_project::read_sample_file_budgeted(file, &session.media_budget())
            .map_err(|error| error.to_string())?;
        session.check_media_intent(&intent)?;
        let request = assets
            .submit_cancellable(
                0,
                intent.key(),
                intent.request(),
                file,
                intent.cancellation(),
            )
            .map_err(|error| format!("Media preparation refused: {error:?}"))?;
        session.accept_media_request(&intent)?;
        let prepared = request
            .receive()
            .map_err(|error| format!("Media preparation failed: {error:?}"))?;
        session.publish_media(&intent, prepared, Instant::now())
    })();
    if result.is_err() {
        session.cancel_media_request(intent.request());
    }
    reply(session, result, Some(intent.request()))
}

pub(super) fn verify() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/arrangement-media-replay.json"
    );
    let mut fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
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
    let mut session = Session::new(Arc::new(Mutex::new(controller)));
    let assets = AudioAssets::with_budget(session.media_budget());
    let mut actual = Vec::new();
    for step in fixture["steps"].as_array().unwrap() {
        let args = &step["args"];
        let n = |key: &str| args[key].as_u64().unwrap();
        let string = |key: &str| args[key].as_str().unwrap();
        let now = Instant::now();
        let outcome: Result<Value, String> = match step["command"].as_str().unwrap() {
            "arrangement_edit" => {
                let result = session
                    .edit_arrangement(
                        n("generation"),
                        n("revision"),
                        parse_batch(string("editsJson")).unwrap(),
                        now,
                    )
                    .map_err(|error| error.message);
                Ok(reply(&session, result, None))
            }
            "arrangement_view" => {
                // The replay contains only audio clips, whose full serde shape equals IPC.
                let stamp = session.arrangement_view();
                let arrangement = &stamp.arrangement;
                let (offset, limit) = (n("offset") as usize, n("limit") as usize);
                let clips: Vec<_> = arrangement.clips.iter().skip(offset).take(limit).collect();
                Ok(json!({
                    "generation": stamp.generation, "revision": stamp.revision, "offset": offset,
                    "total": arrangement.clips.len(), "clips": wire(clips), "tracks": wire(&arrangement.tracks),
                    "audioAssets": wire(&arrangement.audio_assets), "loopRegion": wire(&arrangement.loop_region),
                    "playMode": wire(session.project().play_mode), "songError": session.song_error(),
                    "pendingRequests": session.pending_media_requests(), "warnings": session.media_warnings(),
                }))
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
                Ok(prepare(
                    &mut session,
                    &assets,
                    args,
                    target,
                    &local(string("path")),
                ))
            }
            "arrangement_history" => Ok(
                match session.step_project_history(
                    n("generation"),
                    n("revision"),
                    args["undo"].as_bool().unwrap(),
                    now,
                ) {
                    Ok(ack) => json!({
                        "status": "applied", "generation": ack.state.generation,
                        "revision": ack.state.revision, "effect": ack.effect,
                    }),
                    Err(error) => json!({
                        "status": "refused", "generation": error.state.generation,
                        "revision": error.state.revision, "message": error.message, "reload": true,
                    }),
                },
            ),
            "arrangement_waveform" => (|| {
                let generation = n("generation");
                let asset = n("asset") as u32;
                let (sample, peaks) = session.waveform_source(generation, asset)?;
                let (start, end, bins) =
                    (n("start") as usize, n("end") as usize, n("bins") as usize);
                if start >= end
                    || end > sample.len()
                    || bins == 0
                    || bins > 16_384
                    || bins > end - start
                {
                    return Err("Invalid waveform range or bin count".into());
                }
                let peaks = match peaks {
                    Some(peaks) => peaks,
                    None => grooviq_engine::waveform::PeakPyramid::budgeted(
                        sample,
                        &session.media_budget(),
                    )?,
                };
                let bytes = peaks
                    .encode(generation, n("request"), asset, start, end, bins)
                    .map_err(|_| "Invalid waveform range or bins")?;
                Ok(json!(bytes))
            })(),
            // Project command replies include host-dependent paths. Their effects are
            // captured by the following view and waveform reads.
            "project_save_as" => {
                session
                    .save(Some(&local(string("path"))), dir.path())
                    .unwrap();
                Ok(Value::Null)
            }
            "project_new" => {
                session.new_project().unwrap();
                Ok(Value::Null)
            }
            "project_open" => {
                let path = local(string("path"));
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
    if std::env::var("GROOVIQ_UPDATE_REPLAY").as_deref() == Ok("1") {
        fixture["expected"] = json!(actual);
        std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
    }
    assert_eq!(
        json!(actual),
        fixture["expected"],
        "real Session media replay drift"
    );
}
