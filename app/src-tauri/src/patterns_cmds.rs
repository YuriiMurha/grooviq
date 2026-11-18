//! Canonical pattern management. The UI settles pending gestures before calling these.
use crate::{Audio, lock};
use grooviq_project::{
    pattern_edit::PatternEdit,
    session::{PatternReply, PatternsView},
};
use std::time::Instant;
use tauri::State;

#[tauri::command]
pub async fn pattern_list(audio: State<'_, Audio>) -> Result<PatternsView, String> {
    Ok(lock(&audio.session).patterns())
}

#[tauri::command]
pub async fn pattern_edit(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    edit: PatternEdit,
) -> Result<PatternReply, String> {
    Ok(lock(&audio.session).pattern_command(generation, revision, edit, Instant::now()))
}
