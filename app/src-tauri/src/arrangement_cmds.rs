//! Thin native adapters. No UI workflow or gesture policy; blocking work owns no Session lock.
use crate::{Audio, lock};
use grooviq_project::{
    ClipContent,
    arrangement_edit::{self, ArrangementAck},
    audio_assets::AudioAssets,
    session::{MediaTarget, Session},
};
use serde::Serialize;
use std::{path::PathBuf, sync::Mutex, time::Instant};
use tauri::{State, ipc::Response};
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    status: &'static str,
    generation: u64,
    revision: u64,
    request: Option<u64>,
    created_ids: Vec<u32>,
    changed: bool,
    reload: bool,
    message: Option<String>,
    warnings: Vec<String>,
}
fn reply(s: &Session, result: Result<ArrangementAck, String>, request: Option<u64>) -> Reply {
    let v = s.arrangement_view();
    match result {
        Ok(ack) => Reply {
            status: "applied",
            generation: v.generation,
            revision: v.revision,
            request,
            created_ids: ack.created_ids,
            changed: ack.changed,
            reload: false,
            message: None,
            warnings: s.media_warnings(),
        },
        Err(message) => Reply {
            status: "refused",
            generation: v.generation,
            revision: v.revision,
            request,
            created_ids: vec![],
            changed: false,
            reload: true,
            message: Some(message),
            warnings: s.media_warnings(),
        },
    }
}
/// No curve arrays in paginated summaries or mutation replies.
#[tauri::command]
pub fn arrangement_view(
    audio: State<'_, Audio>,
    offset: usize,
    limit: usize,
) -> Result<serde_json::Value, String> {
    if limit == 0 || limit > 1024 {
        return Err("Read 1..1024 clips per page".into());
    }
    let s = lock(&audio.session);
    let p = s.project();
    let clips:Vec<_>=p.arrangement.clips.iter().skip(offset).take(limit).map(|c|{
  let content=match &c.content{
   ClipContent::Pattern{pattern,offset_tick,length_ticks}=>serde_json::json!({"type":"pattern","pattern":pattern,"offsetTick":offset_tick,"lengthTicks":length_ticks}),
   ClipContent::Audio{asset,source_start,source_end,gain,reverse,fade_in,fade_out}=>serde_json::json!({"type":"audio","asset":asset,"sourceStart":source_start,"sourceEnd":source_end,"gain":gain,"reverse":reverse,"fadeIn":fade_in,"fadeOut":fade_out}),
   ClipContent::Automation{target,priority,offset_tick,length_ticks,points}=>serde_json::json!({"type":"automation","target":target,"priority":priority,"offsetTick":offset_tick,"lengthTicks":length_ticks,"pointCount":points.len()}),
  };serde_json::json!({"id":c.id,"track":c.track,"name":c.name,"startTick":c.start_tick,"muted":c.muted,"content":content})
 }).collect();
    let stamp = s.arrangement_view();
    Ok(
        serde_json::json!({"generation":stamp.generation,"revision":stamp.revision,"offset":offset,"total":p.arrangement.clips.len(),"clips":clips,"tracks":p.arrangement.tracks,"audioAssets":p.arrangement.audio_assets,"loopRegion":p.arrangement.loop_region,"playMode":p.play_mode,"songError":s.song_error(),"pendingRequests":s.pending_media_requests(),"warnings":s.media_warnings()}),
    )
}
#[tauri::command]
pub fn arrangement_edit(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    edits_json: String,
) -> Reply {
    let ops = arrangement_edit::parse_batch(&edits_json);
    let mut s = lock(&audio.session);
    let result = ops.and_then(|ops| {
        s.edit_arrangement(generation, revision, ops, Instant::now())
            .map_err(|e| e.message)
    });
    reply(&s, result, None)
}
#[tauri::command]
pub fn arrangement_history(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    undo: bool,
) -> serde_json::Value {
    let mut s = lock(&audio.session);
    match s.step_project_history(generation, revision, undo, Instant::now()) {
        Ok(ack) => {
            serde_json::json!({"status":"applied","generation":ack.state.generation,"revision":ack.state.revision,"effect":ack.effect})
        }
        Err(e) => {
            serde_json::json!({"status":"refused","generation":e.state.generation,"revision":e.state.revision,"message":e.message,"reload":true})
        }
    }
}
#[tauri::command]
pub fn arrangement_curve(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    clip: u32,
) -> Result<serde_json::Value, String> {
    let s = lock(&audio.session);
    let v = s.arrangement_view();
    if (generation, revision) != (v.generation, v.revision) {
        return Err("Stale curve request; reload".into());
    }
    let c = s
        .project()
        .arrangement
        .clips
        .iter()
        .find(|c| c.id == clip)
        .ok_or("Unknown clip")?;
    if let ClipContent::Automation { points, .. } = &c.content {
        Ok(
            serde_json::json!({"generation":generation,"revision":revision,"clip":clip,"points":points}),
        )
    } else {
        Err("Not an automation clip".into())
    }
}
fn submit_media(
    session: &std::sync::Mutex<Session>,
    assets: &grooviq_project::audio_assets::AudioAssets,
    intent: &grooviq_project::session::MediaIntent,
    file: grooviq_project::SampleFile,
) -> Result<grooviq_project::audio_assets::AssetRequest, String> {
    let mut s = lock(session);
    s.check_media_intent(intent)?;
    let request = assets
        .submit_cancellable(
            0,
            intent.key(),
            intent.request(),
            file,
            intent.cancellation(),
        )
        .map_err(|e| format!("Media preparation refused: {e:?}"))?;
    if let Err(e) = s.accept_media_request(intent) {
        request.cancel();
        return Err(e);
    }
    Ok(request)
}
fn publish_received(
    s: &mut Session,
    intent: &grooviq_project::session::MediaIntent,
    prepared: std::sync::Arc<grooviq_project::audio_assets::PreparedAudio>,
) -> Result<ArrangementAck, String> {
    s.publish_media(intent, prepared, Instant::now())
}
async fn prepare(
    audio: &Audio,
    generation: u64,
    revision: u64,
    target: MediaTarget,
    path: String,
    ticket: u64,
) -> Reply {
    prepare_media(
        &audio.session,
        &audio.assets,
        generation,
        revision,
        target,
        path,
        ticket,
    )
    .await
}
async fn prepare_media(
    session: &Mutex<Session>,
    assets: &AudioAssets,
    generation: u64,
    revision: u64,
    target: MediaTarget,
    path: String,
    ticket: u64,
) -> Reply {
    let (intent, budget) = {
        let mut s = lock(session);
        let intent = match s.media_intent(generation, revision, target) {
            Ok(intent) => intent,
            Err(e) => return reply(&s, Err(e), None),
        };
        if let Err(e) = s.register_media_ticket(ticket, &intent) {
            return reply(&s, Err(e), None);
        }
        (intent, s.media_budget())
    };
    let prepared = async {
        let path = PathBuf::from(path);
        let file = tauri::async_runtime::spawn_blocking(move || {
            grooviq_project::read_sample_file_budgeted(&path, &budget).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        let request = submit_media(session, assets, &intent, file)?;
        tauri::async_runtime::spawn_blocking(move || request.receive())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("Media preparation failed: {e:?}"))
    }
    .await;
    let mut s = lock(session);
    let result = prepared.and_then(|prepared| publish_received(&mut s, &intent, prepared));
    if result.is_err() {
        s.cancel_media_request(intent.request());
    }
    // Replacement already cleared this ticket. A new project may have reused its number.
    if s.generation() == generation {
        s.release_media_ticket(ticket);
    }
    reply(&s, result, Some(intent.request()))
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
    Ok(prepare(
        &audio,
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
    Ok(prepare(
        &audio,
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
    lock(&audio.session).cancel_media_request(request)
}
#[tauri::command]
pub fn audio_cancel_ticket(audio: State<'_, Audio>, ticket: u64) -> bool {
    lock(&audio.session).cancel_media_ticket(ticket)
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
    let (sample, peaks) = lock(&audio.session).waveform_source(generation, asset)?;
    if start >= end || end > sample.len() || bins == 0 || bins > 16384 || bins > end - start {
        return Err("Invalid waveform range or bin count".into());
    }
    if generation > 9_007_199_254_740_991 || request > 9_007_199_254_740_991 {
        return Err("Invalid waveform identity".into());
    }
    let budget = lock(&audio.session).media_budget();
    let identity = sample.clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        let peaks = if let Some(peaks) = peaks {
            peaks
        } else {
            grooviq_engine::waveform::PeakPyramid::budgeted(sample, &budget)?
        };
        peaks
            .encode(generation, request, asset, start, end, bins)
            .map_err(|_| "Invalid waveform range or bins".to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let current = lock(&audio.session).waveform_source(generation, asset)?.0;
    if !std::sync::Arc::ptr_eq(&identity, &current) {
        return Err("Waveform source replaced; reload".into());
    }
    Ok(Response::new(bytes))
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
        s.release_media_ticket(ticket);
        assert!(s.pending_media_requests().is_empty());
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
        s.release_media_ticket(1);
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
