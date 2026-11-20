//! Small canonical sampler repair replies; model publication stays inside Session admission.
use crate::{Audio, lock};
use grooviq_project::session::sampler_trim::{TrimAck, TrimView};
use serde::Serialize;
use std::time::Instant;
use tauri::State;
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Reply {
    Applied {
        state: TrimView,
        changed: bool,
    },
    Refused {
        state: Option<TrimView>,
        message: String,
    },
}
#[tauri::command]
pub fn sampler_trim_view(audio: State<'_, Audio>, channel: u32) -> Result<TrimView, String> {
    lock(&audio.session)
        .sampler_trim_view(channel)
        .map_err(|e| e.to_string())
}
fn reply(
    s: &grooviq_project::session::Session,
    channel: u32,
    result: Result<TrimAck, grooviq_project::session::SessionError>,
) -> Reply {
    match result {
        Ok(ack) => Reply::Applied {
            state: ack.state,
            changed: ack.changed,
        },
        Err(e) => Reply::Refused {
            state: s.sampler_trim_view(channel).ok(),
            message: e.to_string(),
        },
    }
}
#[tauri::command]
pub fn sampler_trim_reset(audio: State<'_, Audio>, channel: u32, stamp: String) -> Reply {
    let mut s = lock(&audio.session);
    let result = s.sampler_trim_reset(channel, &stamp, Instant::now());
    reply(&s, channel, result)
}
#[tauri::command]
pub fn sampler_trim_undo(
    audio: State<'_, Audio>,
    channel: u32,
    stamp: String,
    token: String,
) -> Reply {
    let mut s = lock(&audio.session);
    let result = s.sampler_trim_undo(channel, &stamp, &token, Instant::now());
    reply(&s, channel, result)
}
