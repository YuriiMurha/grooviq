//! Arrangement command behavior shared by Tauri and the offline media replay.
use crate::lock;
use grooviq_project::{
    ClipContent,
    arrangement_edit::{self, ArrangementAck},
    audio_assets::AudioAssets,
    session::{MediaTarget, Session},
};
use serde::Serialize;
use std::{path::PathBuf, sync::Mutex, time::Instant};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub(crate) status: &'static str,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) request: Option<u64>,
    pub(crate) created_ids: Vec<u32>,
    pub(crate) changed: bool,
    pub(crate) reload: bool,
    pub(crate) message: Option<String>,
    warnings: Vec<String>,
}
pub(crate) fn reply(
    s: &Session,
    result: Result<ArrangementAck, String>,
    request: Option<u64>,
) -> Reply {
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
pub fn arrangement_view(
    session: &Mutex<Session>,
    offset: usize,
    limit: usize,
) -> Result<serde_json::Value, String> {
    if limit == 0 || limit > 1024 {
        return Err("Read 1..1024 clips per page".into());
    }
    let s = lock(session);
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
pub fn arrangement_edit(
    session: &Mutex<Session>,
    generation: u64,
    revision: u64,
    edits_json: &str,
) -> Reply {
    let ops = arrangement_edit::parse_batch(edits_json);
    let mut s = lock(session);
    let result = ops.and_then(|ops| {
        s.edit_arrangement(generation, revision, ops, Instant::now())
            .map_err(|e| e.message)
    });
    reply(&s, result, None)
}
pub fn arrangement_history(
    session: &Mutex<Session>,
    generation: u64,
    revision: u64,
    undo: bool,
) -> serde_json::Value {
    let mut s = lock(session);
    match s.step_project_history(generation, revision, undo, Instant::now()) {
        Ok(ack) => {
            serde_json::json!({"status":"applied","generation":ack.state.generation,"revision":ack.state.revision,"effect":ack.effect})
        }
        Err(e) => {
            serde_json::json!({"status":"refused","generation":e.state.generation,"revision":e.state.revision,"message":e.message,"reload":true})
        }
    }
}
pub(crate) fn submit_media(
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
pub(crate) fn publish_received(
    s: &mut Session,
    intent: &grooviq_project::session::MediaIntent,
    prepared: std::sync::Arc<grooviq_project::audio_assets::PreparedAudio>,
) -> Result<ArrangementAck, String> {
    s.publish_media(intent, prepared, Instant::now())
}
pub async fn prepare_media(
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
    #[cfg(test)]
    let (read_gate, admission_gate) = media_test_gate::take();
    let prepared = async {
        let path = PathBuf::from(path);
        let file = tauri::async_runtime::spawn_blocking(move || {
            #[cfg(test)]
            media_test_gate::hold(read_gate);
            grooviq_project::read_sample_file_budgeted(&path, &budget).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        let request = submit_media(session, assets, &intent, file)?;
        tauri::async_runtime::spawn_blocking(move || {
            #[cfg(test)]
            media_test_gate::hold(admission_gate);
            request.receive()
        })
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
    // Identity-checked: a replacement may have cleared this ticket and a new import reused it.
    s.release_media_ticket(ticket, &intent);
    reply(&s, result, Some(intent.request()))
}

#[cfg(test)]
pub(crate) mod media_test_gate {
    use std::{cell::Cell, sync::mpsc, time::Duration};

    pub const TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Stage {
        Read,
        Admitted,
    }

    pub struct Gate {
        pub stage: Stage,
        pub arrived: mpsc::Sender<()>,
        pub proceed: mpsc::Receiver<()>,
    }

    thread_local! {
        // The caller installs a one-shot gate before polling prepare_media on its own thread.
        pub static GATE: Cell<Option<Gate>> = const { Cell::new(None) };
    }

    pub(super) fn take() -> (Option<Gate>, Option<Gate>) {
        let gate = GATE.take();
        match gate.as_ref().map(|gate| gate.stage) {
            Some(Stage::Read) => (gate, None),
            Some(Stage::Admitted) => (None, gate),
            None => (None, None),
        }
    }

    pub(super) fn hold(gate: Option<Gate>) {
        if let Some(gate) = gate {
            gate.arrived.send(()).expect("test stopped before gate");
            gate.proceed
                .recv_timeout(TIMEOUT)
                .expect("test did not resume import");
        }
    }
}

pub fn audio_cancel(session: &Mutex<Session>, request: u64) -> bool {
    lock(session).cancel_media_request(request)
}

pub fn audio_cancel_ticket(session: &Mutex<Session>, ticket: u64) -> bool {
    lock(session).cancel_media_ticket(ticket)
}

pub async fn arrangement_waveform(
    session: &Mutex<Session>,
    generation: u64,
    asset: u32,
    request: u64,
    start: usize,
    end: usize,
    bins: usize,
) -> Result<Vec<u8>, String> {
    let (sample, peaks) = lock(session).waveform_source(generation, asset)?;
    if start >= end || end > sample.len() || bins == 0 || bins > 16384 || bins > end - start {
        return Err("Invalid waveform range or bin count".into());
    }
    if generation > 9_007_199_254_740_991 || request > 9_007_199_254_740_991 {
        return Err("Invalid waveform identity".into());
    }
    let budget = lock(session).media_budget();
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
    let current = lock(session).waveform_source(generation, asset)?.0;
    if !std::sync::Arc::ptr_eq(&identity, &current) {
        return Err("Waveform source replaced; reload".into());
    }
    Ok(bytes)
}
