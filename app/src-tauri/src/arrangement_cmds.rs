//! Thin native adapters. No UI workflow or gesture policy; blocking work owns no Session lock.
use crate::{
    Audio,
    arrangement_core::{self, Reply, prepare_media, reply},
    lock,
};
use grooviq_project::{
    arrangement_edit::{self, ArrangementAck},
    session::MediaTarget,
};
use std::time::Instant;
use tauri::{State, ipc::Response};

#[tauri::command]
pub fn arrangement_view(
    audio: State<'_, Audio>,
    offset: usize,
    limit: usize,
) -> Result<serde_json::Value, String> {
    arrangement_core::arrangement_view(&audio.session, offset, limit)
}
#[tauri::command]
pub fn arrangement_edit(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    edits_json: String,
) -> Reply {
    arrangement_core::arrangement_edit(&audio.session, generation, revision, &edits_json)
}
#[tauri::command]
pub fn arrangement_history(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    undo: bool,
) -> serde_json::Value {
    arrangement_core::arrangement_history(&audio.session, generation, revision, undo)
}
#[tauri::command]
pub fn arrangement_curve(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    clip: u32,
) -> Result<serde_json::Value, String> {
    arrangement_core::arrangement_curve(&audio.session, generation, revision, clip)
}
#[tauri::command]
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri passes flat import arguments"
)]
pub async fn audio_import(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    track: u32,
    start_tick: u64,
    name: String,
    path: String,
    ticket: u64,
) -> Result<Reply, String> {
    Ok(prepare_media(
        &audio.session,
        &audio.assets,
        generation,
        revision,
        MediaTarget::Import {
            track,
            start_tick,
            name,
        },
        path,
        ticket,
    )
    .await)
}
#[tauri::command]
pub async fn audio_relink(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    asset: u32,
    path: String,
    ticket: u64,
) -> Result<Reply, String> {
    Ok(prepare_media(
        &audio.session,
        &audio.assets,
        generation,
        revision,
        MediaTarget::Relink { asset },
        path,
        ticket,
    )
    .await)
}
#[tauri::command]
pub fn audio_cancel(audio: State<'_, Audio>, request: u64) -> bool {
    arrangement_core::audio_cancel(&audio.session, request)
}
#[tauri::command]
pub fn audio_cancel_ticket(audio: State<'_, Audio>, ticket: u64) -> bool {
    arrangement_core::audio_cancel_ticket(&audio.session, ticket)
}
#[tauri::command]
pub async fn arrangement_waveform(
    audio: State<'_, Audio>,
    generation: u64,
    asset: u32,
    request: u64,
    start: usize,
    end: usize,
    bins: usize,
) -> Result<Response, String> {
    arrangement_core::arrangement_waveform(
        &audio.session,
        generation,
        asset,
        request,
        start,
        end,
        bins,
    )
    .await
    .map(Response::new)
}
#[tauri::command]
pub fn arrangement_mode(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    mode: grooviq_project::PlayMode,
) -> Reply {
    let mut s = lock(&audio.session);
    let before = s.arrangement_view().revision;
    let result = s
        .set_play_mode_in(generation, revision, mode, Instant::now())
        .map(|_| ArrangementAck {
            state: s.arrangement_view(),
            created_ids: vec![],
            changed: s.arrangement_view().revision != before,
        });
    reply(&s, result, None)
}
#[tauri::command]
pub fn arrangement_loop(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    region: Option<grooviq_project::LoopRegion>,
) -> Reply {
    let mut s = lock(&audio.session);
    let result = s
        .edit_arrangement(
            generation,
            revision,
            vec![arrangement_edit::ArrangementEdit::SetLoop { region }],
            Instant::now(),
        )
        .map_err(|e| e.message);
    reply(&s, result, None)
}
#[tauri::command]
pub fn arrangement_seek(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    tick: u64,
) -> Reply {
    let s = lock(&audio.session);
    let result = s
        .seek_song(generation, revision, tick)
        .map(|_| ArrangementAck {
            state: s.arrangement_view(),
            created_ids: vec![],
            changed: false,
        });
    reply(&s, result, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrangement_core::{publish_received, submit_media};
    use grooviq_project::session::Session;
    use grooviq_project::{
        SampleFile, arrangement_edit::ArrangementEdit, audio_assets::AudioAssets,
    };
    use std::sync::{Arc, Mutex};
    fn wav() -> Vec<u8> {
        let n = 128u32;
        let mut b = vec![];
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&48000u32.to_le_bytes());
        b.extend_from_slice(&96000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 2).to_le_bytes());
        for _ in 0..n {
            b.extend_from_slice(&1000i16.to_le_bytes());
        }
        b
    }
    fn media_rig() -> (Mutex<Session>, AudioAssets, u32) {
        let (c, _processor) = grooviq_engine::engine(grooviq_engine::EngineConfig {
            sample_rate: 48000.0,
        });
        let mut s = Session::new(Arc::new(Mutex::new(c)));
        let v = s.arrangement_view();
        let ack = s
            .edit_arrangement(
                v.generation,
                v.revision,
                vec![ArrangementEdit::CreateTrack {
                    name: "T".into(),
                    color: 0,
                }],
                Instant::now(),
            )
            .unwrap();
        let track = ack.created_ids[0];
        let session = Mutex::new(s);
        let assets = AudioAssets::with_budget(lock(&session).media_budget());
        (session, assets, track)
    }
    fn import(track: u32) -> MediaTarget {
        MediaTarget::Import {
            track,
            start_tick: 0,
            name: "Imported".into(),
        }
    }
    fn generated_file() -> SampleFile {
        SampleFile {
            name: "native.wav".into(),
            bytes: Arc::new(wav()),
        }
    }
    fn prepare_import(
        session: &Mutex<Session>,
        assets: &AudioAssets,
        track: u32,
        path: &std::path::Path,
        ticket: u64,
    ) -> Reply {
        let v = lock(session).arrangement_view();
        tauri::async_runtime::block_on(prepare_media(
            session,
            assets,
            v.generation,
            v.revision,
            import(track),
            path.to_str().unwrap().to_owned(),
            ticket,
        ))
    }
    fn assert_ticket_released(session: &Mutex<Session>, track: u32, ticket: u64) {
        let mut s = lock(session);
        let v = s.arrangement_view();
        let intent = s
            .media_intent(v.generation, v.revision, import(track))
            .unwrap();
        s.register_media_ticket(ticket, &intent)
            .expect("finished command left its ticket live");
        s.release_media_ticket(ticket, &intent);
        assert!(s.pending_media_requests().is_empty());
    }
    fn cancel_held_import(stage: crate::arrangement_core::media_test_gate::Stage) {
        use crate::arrangement_core::media_test_gate::{GATE, Gate, Stage, TIMEOUT};
        use std::{sync::mpsc, thread};

        const TICKET: u64 = 1;
        let (session, assets, track) = media_rig();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("generated.wav");
        std::fs::write(&path, wav()).unwrap();
        let (before, stamp, history) = {
            let s = lock(&session);
            (s.project().clone(), s.arrangement_view(), s.history_usage())
        };
        // Any new worker submission for this import key would cancel the earlier request.
        let previous_worker = (stage == Stage::Read).then(|| {
            let intent = lock(&session)
                .media_intent(stamp.generation, stamp.revision, import(track))
                .unwrap();
            assets
                .submit(0, intent.key(), intent.request(), generated_file())
                .unwrap()
        });
        let (pending, cancelled, response) = thread::scope(|scope| {
            let (arrived, entered) = mpsc::channel();
            let (resume, proceed) = mpsc::channel();
            let (session, assets, path) = (&session, &assets, &path);
            let running = scope.spawn(move || {
                GATE.set(Some(Gate {
                    stage,
                    arrived,
                    proceed,
                }));
                prepare_import(session, assets, track, path, TICKET)
            });
            entered
                .recv_timeout(TIMEOUT)
                .expect("import did not reach gate");
            let pending = lock(session).pending_media_requests();
            let cancelled = crate::arrangement_core::audio_cancel_ticket(session, TICKET);
            resume.send(()).unwrap();
            (pending, cancelled, running.join().unwrap())
        });

        assert_eq!(pending.len(), usize::from(stage == Stage::Admitted));
        if stage == Stage::Admitted {
            assert_eq!(response.request, Some(pending[0]));
        }
        assert_eq!(
            response.status, "refused",
            "cancel by ticket returned {cancelled}"
        );
        assert!(
            cancelled,
            "ticket must still be live while the import is held"
        );
        if let Some(previous_worker) = previous_worker {
            previous_worker
                .receive()
                .expect("cancelled import reached the worker and superseded the earlier request");
            assert_eq!(
                response.message.as_deref(),
                Some("Older or cancelled media request")
            );
        }
        assert!(response.request.is_some());
        assert!(!response.changed);
        assert!(response.created_ids.is_empty());
        assert_eq!(response.generation, stamp.generation);
        assert_eq!(response.revision, stamp.revision);
        {
            let mut s = lock(&session);
            assert_eq!(s.project(), &before);
            assert_eq!(s.history_usage(), history);
            assert!(s.pending_media_requests().is_empty());
            let save = s
                .begin_save(Some(&dir.path().join("snapshot.grooviq")))
                .unwrap();
            s.save_failed();
            assert!(save.samples.is_empty());
        }
        assert_ticket_released(&session, track, TICKET);

        let fresh = prepare_import(&session, &assets, track, &path, TICKET + 1);
        assert_eq!(fresh.status, "applied");
        assert_eq!(fresh.created_ids, [before.next_id, before.next_id + 1]);
    }
    #[test]
    fn native_cancel_ticket_during_held_read() {
        cancel_held_import(crate::arrangement_core::media_test_gate::Stage::Read);
    }
    #[test]
    fn native_cancel_ticket_during_held_admission() {
        cancel_held_import(crate::arrangement_core::media_test_gate::Stage::Admitted);
    }
    #[test]
    fn native_cancel_before_submit_leaves_the_worker_and_project_unchanged() {
        let (session, assets, track) = media_rig();
        let mut s = lock(&session);
        let v = s.arrangement_view();
        let before = s.project().clone();
        let history = s.history_usage();
        let previous = s
            .media_intent(v.generation, v.revision, import(track))
            .unwrap();
        let intent = s
            .media_intent(v.generation, v.revision, import(track))
            .unwrap();
        s.register_media_ticket(1, &intent).unwrap();
        drop(s);

        // A cancelled import must never reach the worker, which would supersede this job.
        let previous_request =
            submit_media(&session, &assets, &previous, generated_file()).unwrap();
        assert!(lock(&session).cancel_media_ticket(1));
        assert!(submit_media(&session, &assets, &intent, generated_file()).is_err());
        assert!(previous_request.receive().is_ok());

        let mut s = lock(&session);
        assert_eq!(s.pending_media_requests(), vec![previous.request()]);
        assert!(s.cancel_media_request(previous.request()));
        s.release_media_ticket(1, &intent);
        assert!(s.pending_media_requests().is_empty());
        assert_eq!(s.project(), &before);
        assert_eq!(s.history_usage(), history);
    }
    #[test]
    fn native_tombstoned_import_is_refused_before_reading_file() {
        let (session, assets, track) = media_rig();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.wav");
        assert!(!lock(&session).cancel_media_ticket(1));
        let response = prepare_import(&session, &assets, track, &missing, 1);
        assert_eq!(response.status, "refused");
        assert_eq!(response.message.as_deref(), Some("Media ticket cancelled"));
        assert!(response.request.is_none());
    }
    #[test]
    fn native_import_releases_its_ticket_after_each_error() {
        let (session, mut assets, track) = media_rig();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("generated.wav");
        let before = lock(&session).project().clone();
        let history = lock(&session).history_usage();
        let response = prepare_import(&session, &assets, track, &path, 1);
        assert_eq!(response.status, "refused");
        assert!(response.request.is_some());
        assert_ticket_released(&session, track, 1);

        std::fs::write(&path, b"invalid WAV").unwrap();
        let response = prepare_import(&session, &assets, track, &path, 1);
        assert!(
            response
                .message
                .unwrap()
                .starts_with("Media preparation failed: Decode")
        );
        assert_ticket_released(&session, track, 1);

        std::fs::write(&path, wav()).unwrap();
        lock(&session)
            .set_precommit_validator(Some(Arc::new(|_| Err("refused publication".into()))));
        let response = prepare_import(&session, &assets, track, &path, 1);
        assert!(response.message.unwrap().contains("refused publication"));
        assert_ticket_released(&session, track, 1);

        assets.replace_project();
        let response = prepare_import(&session, &assets, track, &path, 1);
        assert_eq!(
            response.message.as_deref(),
            Some("Media preparation refused: Cancelled")
        );
        assert_ticket_released(&session, track, 1);
        assert_eq!(lock(&session).project(), &before);
        assert_eq!(lock(&session).history_usage(), history);
    }
    #[test]
    fn native_cancel_after_publish_has_no_effect_and_import_is_undoable() {
        let (session, assets, track) = media_rig();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("generated.wav");
        std::fs::write(&path, wav()).unwrap();
        let response = prepare_import(&session, &assets, track, &path, 1);
        assert_eq!(response.status, "applied");
        let mut s = lock(&session);
        let published = s.project().clone();
        let history = s.history_usage();
        assert!(!s.cancel_media_ticket(1));
        assert_eq!(s.project(), &published);
        assert_eq!(s.history_usage(), history);
        assert!(s.pending_media_requests().is_empty());
        s.step_project_history(response.generation, response.revision, true, Instant::now())
            .unwrap();
        assert!(s.project().arrangement.clips.is_empty());
        assert!(s.project().arrangement.audio_assets.is_empty());
    }
    #[test]
    fn native_media_producer_rechecks_final_generation_and_emits_canonical_reply() {
        let (session, assets, track) = media_rig();
        let v = lock(&session).arrangement_view();
        let intent = lock(&session)
            .media_intent(
                v.generation,
                v.revision,
                MediaTarget::Import {
                    track,
                    start_tick: 0,
                    name: "Imported".into(),
                },
            )
            .unwrap();
        let request = submit_media(
            &session,
            &assets,
            &intent,
            SampleFile {
                name: "native.wav".into(),
                bytes: Arc::new(wav()),
            },
        )
        .unwrap();
        let prepared = request.receive().unwrap();
        let mut s = lock(&session);
        let result = publish_received(&mut s, &intent, prepared);
        let response = reply(&s, result, Some(intent.request()));
        assert_eq!(response.status, "applied");
        assert_eq!(response.request, Some(intent.request()));
        assert_eq!(response.created_ids.len(), 2);
        assert_eq!(response.revision, s.arrangement_view().revision);
        let v = s.arrangement_view();
        let late = s
            .media_intent(
                v.generation,
                v.revision,
                MediaTarget::Relink {
                    asset: response.created_ids[0],
                },
            )
            .unwrap();
        drop(s);
        let prepared = submit_media(
            &session,
            &assets,
            &late,
            SampleFile {
                name: "late.wav".into(),
                bytes: Arc::new(wav()),
            },
        )
        .unwrap()
        .receive()
        .unwrap();
        let mut s = lock(&session);
        s.new_project().unwrap();
        let before = s.project().clone();
        let failed = publish_received(&mut s, &late, prepared);
        let response = reply(&s, failed, Some(late.request()));
        assert_eq!(response.status, "refused");
        assert!(response.reload);
        assert_eq!(s.project(), &before);
    }
    #[test]
    fn native_batch_parser_enforces_whole_request_point_and_operation_bounds() {
        let ops=arrangement_edit::parse_batch(r#"[{"kind":"createTrack","name":"T","color":0},{"kind":"createClipOnCreatedTrack","trackOperation":0,"name":"P","startTick":0,"muted":false,"content":{"type":"pattern","pattern":0,"offsetTick":0,"lengthTicks":960}}]"#).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(arrangement_edit::parse_batch("[] false").is_err());
        let points = (0..16385)
            .map(|n| format!("{{\"tick\":{n},\"value\":0.5,\"mode\":\"hold\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            r#"[{{"kind":"createClip","track":1,"name":"A","startTick":0,"muted":false,"content":{{"type":"automation","target":{{"type":"channelPan","channel":2}},"priority":0,"offsetTick":0,"lengthTicks":960,"points":[{points}]}}}}]"#
        );
        assert!(arrangement_edit::parse_batch(&json).is_err());
    }
}
