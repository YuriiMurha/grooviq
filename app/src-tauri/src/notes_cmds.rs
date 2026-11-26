//! The note commands: the pattern's notes as a binary buffer, note edits by id, synth
//! channels, live notes and parameters, the MIDI channel, waveforms, recording and the
//! metronome. Also the recorder thread that moves recorded live notes into the pattern.
//!
//! Every path that changes the current pattern's notes tells the UI with `notes-changed` (the
//! new revision), after the session's lock is released. Lock order as in `project_cmds`:
//! session, then controller.

use crate::audio_log::{AudioLog, Level};
use crate::{Audio, lock};
use grooviq_engine::{Controller, NoteId, RecordEvent};
use grooviq_project::Pattern;
use grooviq_project::notes::{NotesAck, NotesEdit, encode_notes};
use grooviq_project::session::{ProjectView, Session, SessionError, waveform_peaks};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, State};

/// Sent with the current pattern's new revision whenever its notes changed.
pub(crate) const NOTES_CHANGED: &str = "notes-changed";
/// Sent with `{ reason, dropped, message }` when recording switched itself off because recorded
/// notes did not fit (`grooviq_project::session::RecordingStopped`).
pub(crate) const RECORDING_STOPPED: &str = "recording-stopped";
/// How often the recorder thread drains the engine's recording queue.
const RECORD_PERIOD: Duration = Duration::from_millis(20);
pub(crate) use crate::midi::NO_CHANNEL;

/// The channel a MIDI keyboard plays, shared with the MIDI input: [`NO_CHANNEL`] for none.
pub(crate) type SelectedChannel = Arc<AtomicU32>;

/// Tells the UI the notes changed if the revision moved from `before`.
pub(crate) fn notes_changed(app: &AppHandle, before: u32, after: u32) {
    if before != after {
        // Fails only while the app shuts down.
        let _ = app.emit(NOTES_CHANGED, after);
    }
}

fn text(e: SessionError) -> String {
    e.to_string()
}

/// The current pattern's notes, wire format v1, as raw bytes, made on a blocking worker (see
/// [`encode_unlocked`]).
#[tauri::command]
pub async fn pattern_notes(audio: State<'_, Audio>) -> Result<Response, String> {
    let session = Arc::clone(&audio.session);
    let bytes =
        tauri::async_runtime::spawn_blocking(move || encode_unlocked(&session, encode_notes))
            .await
            .map_err(|e| format!("The work stopped unexpectedly: {e}"))?;
    Ok(Response::new(bytes))
}

/// Copies the current pattern under the session's lock, then runs `encode` on the copy with
/// the lock released: sorting and writing 100,000 notes must not hold up recording, live notes
/// or edits.
fn encode_unlocked(session: &Mutex<Session>, encode: impl FnOnce(&Pattern) -> Vec<u8>) -> Vec<u8> {
    let pattern = lock(session).current_pattern();
    encode(&pattern)
}

/// Applies one note edit (deletes, updates, adds) made in the project of `edit.generation`;
/// one for a project replaced since changes nothing and asks for a reload.
#[tauri::command]
pub async fn notes_edit(
    app: AppHandle,
    audio: State<'_, Audio>,
    edit: NotesEdit,
) -> Result<NotesAck, String> {
    let (ack, before) = {
        let mut session = lock(&audio.session);
        let before = session.notes_revision();
        (
            session.edit_notes(edit, Instant::now()).map_err(text)?,
            before,
        )
    };
    notes_changed(&app, before, ack.revision);
    Ok(ack)
}

/// Takes back the last note change of the project of `generation` (`null`: nothing to undo).
/// The ack asks for a reload.
#[tauri::command]
pub async fn notes_undo(
    app: AppHandle,
    audio: State<'_, Audio>,
    stamp: grooviq_project::session::PatternStamp,
) -> Result<Option<NotesAck>, String> {
    history(&app, &audio, stamp, true)
}

/// Makes the last undone note change of the project of `generation` again (`null`: nothing to
/// redo).
#[tauri::command]
pub async fn notes_redo(
    app: AppHandle,
    audio: State<'_, Audio>,
    stamp: grooviq_project::session::PatternStamp,
) -> Result<Option<NotesAck>, String> {
    history(&app, &audio, stamp, false)
}

fn history(
    app: &AppHandle,
    audio: &Audio,
    stamp: grooviq_project::session::PatternStamp,
    undo: bool,
) -> Result<Option<NotesAck>, String> {
    let (ack, before) = {
        let mut session = lock(&audio.session);
        let before = session.notes_revision();
        let ack = session.history_stamped(stamp, undo, Instant::now());
        (ack.map_err(text)?, before)
    };
    if let Some(ack) = &ack {
        notes_changed(app, before, ack.revision);
    }
    Ok(ack)
}

/// Adds a synth channel from a factory preset, named after it, to the project of `generation`.
#[tauri::command]
pub async fn channel_add_synth(
    audio: State<'_, Audio>,
    preset: String,
    generation: u64,
) -> Result<ProjectView, String> {
    lock(&audio.session)
        .add_synth_channel_in(generation, &preset, Instant::now())
        .map_err(text)
}

/// Starts a note on a channel now (`on`), returning its press, or releases the press `press`.
/// Only from a key or a click.
#[tauri::command]
pub async fn channel_note(
    audio: State<'_, Audio>,
    channel: u32,
    key: u8,
    velocity: f32,
    on: bool,
    press: Option<u64>,
) -> Result<Option<u64>, String> {
    let session = lock(&audio.session);
    if on {
        return session
            .live_note_on(channel, key, velocity)
            .map(|press| Some(press.0))
            .map_err(text);
    }
    let press = press.ok_or("A release names the press it ends.")?;
    session
        .live_note_off(channel, key, NoteId(press))
        .map(|()| None)
        .map_err(text)
}

/// Sets an instrument parameter in the engine only, while a control moves.
#[tauri::command]
pub async fn channel_param_live(
    audio: State<'_, Audio>,
    channel: u32,
    param: String,
    value: f32,
) -> Result<(), String> {
    lock(&audio.session)
        .param_live(channel, &param, value)
        .map_err(text)
}

/// The channel a MIDI keyboard plays (`null`: none).
#[tauri::command]
pub async fn channel_select(audio: State<'_, Audio>, channel: Option<u32>) -> Result<(), String> {
    if let Some(id) = channel
        && !lock(&audio.session)
            .project()
            .channels
            .iter()
            .any(|c| c.id == id)
    {
        return Err(SessionError::UnknownChannel(id).to_string());
    }
    audio
        .selected_channel
        .store(channel.unwrap_or(NO_CHANNEL), Ordering::Release);
    // A MIDI key held on the old channel is let go now, not at the next poll.
    lock(&audio.midi).release_stale();
    Ok(())
}

/// `points` (min, max) f32 pairs of the channel's sample, little-endian, as raw bytes. The
/// peaks are computed without the session's lock.
#[tauri::command]
pub async fn channel_waveform(
    audio: State<'_, Audio>,
    channel: u32,
    points: u32,
) -> Result<Response, String> {
    let sample = {
        let session = lock(&audio.session);
        let Some(c) = session.project().channels.iter().find(|c| c.id == channel) else {
            return Err(SessionError::UnknownChannel(channel).to_string());
        };
        session
            .sample(channel)
            .ok_or_else(|| format!("{} has no sample to draw.", c.name))?
    };
    let bytes = tauri::async_runtime::spawn_blocking(move || waveform_peaks(&sample, points))
        .await
        .map_err(|e| format!("The work stopped unexpectedly: {e}"))?;
    Ok(Response::new(bytes))
}

/// Switches recording; the view shows it.
#[tauri::command]
pub async fn record_set(audio: State<'_, Audio>, on: bool) -> Result<ProjectView, String> {
    let mut session = lock(&audio.session);
    session.set_recording(on).map_err(text)?;
    Ok(session.view())
}

/// Switches the metronome.
#[tauri::command]
pub async fn metronome_set(audio: State<'_, Audio>, on: bool) -> Result<(), String> {
    lock(&audio.session).set_metronome(on).map_err(text)
}

/// Every [`RECORD_PERIOD`]: moves the engine's recorded events into the pattern and tells the
/// UI the new revision when notes were added, or [`RECORDING_STOPPED`] when recorded notes did
/// not fit and recording was switched off. Logs when the engine's queue dropped events.
/// Runs for the life of the app.
pub(crate) fn record_loop(
    session: &Mutex<Session>,
    controller: &Mutex<Controller>,
    log: &Mutex<AudioLog>,
    app: &AppHandle,
) {
    let mut events: Vec<RecordEvent> = Vec::new();
    let mut dropped = lock(controller).record_dropped();
    loop {
        thread::sleep(RECORD_PERIOD);
        events.clear();
        // The session first, so a project opened meanwhile cannot take another's events.
        let (recorded, now_dropped) = {
            let mut session = lock(session);
            let now_dropped = {
                let mut controller = lock(controller);
                controller.take_recorded(&mut events);
                controller.record_dropped()
            };
            // Every poll, even idle/unarmed: retry UI releases and waiting recorded notes.
            let recorded = session.collect_recorded(&events, Instant::now());
            (recorded, now_dropped)
        };
        if now_dropped != dropped {
            lock(log).push(
                Level::Warn,
                format!(
                    "Recording missed {} note events: the engine's recording queue was full",
                    now_dropped.wrapping_sub(dropped)
                ),
            );
            dropped = now_dropped;
        }
        // Emits fail only while the app shuts down.
        if let Some(revision) = recorded.revision {
            let _ = app.emit(NOTES_CHANGED, revision);
        }
        if let Some(stopped) = recorded.stopped {
            lock(log).push(Level::Warn, stopped.message.clone());
            let _ = app.emit(RECORDING_STOPPED, stopped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grooviq_engine::{EngineConfig, engine};

    #[test]
    fn synth_meter_wire_uses_capture_frame_and_appends_motion() {
        assert_eq!(encode_synth_meter(7, 9000, None), vec![7.0, 9000.0]);
        let encoded = encode_synth_meter(
            7,
            9000,
            Some(grooviq_engine::SynthMeter {
                instance: 7,
                frames: 1024,
                levels: [0.25; 6],
                scope: [0.5; 256],
                motion: [1.0, 4.0, 0.75],
            }),
        );
        assert_eq!(encoded.len(), 267);
        assert_eq!(&encoded[..2], &[7.0, 1024.0]);
        assert_eq!(&encoded[2..8], &[0.25; 6]);
        assert_eq!(&encoded[8..264], &[0.5; 256]);
        assert_eq!(&encoded[264..], &[1.0, 4.0, 0.75]);
    }

    #[test]
    fn notes_are_encoded_without_the_session_lock() {
        let (controller, _processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let session = Mutex::new(Session::new(Arc::new(Mutex::new(controller))));
        let bytes = encode_unlocked(&session, |pattern| {
            assert!(
                session.try_lock().is_ok(),
                "the session is locked while the notes are encoded"
            );
            encode_notes(pattern)
        });
        assert_eq!(bytes, lock(&session).notes());
    }
}

/// Read-only display request, fenced to the project and channel incarnation.
#[tauri::command]
pub async fn synth_meter(
    audio: State<'_, Audio>,
    channel: u32,
    generation: u64,
    instance: Option<u64>,
) -> Result<Option<Vec<f64>>, String> {
    let session = lock(&audio.session);
    if session.generation() != generation {
        return Ok(None);
    }
    let controller = lock(&audio.controller);
    let Some(current) = controller.channel_instance(grooviq_engine::ChannelId(channel)) else {
        return Ok(None);
    };
    if instance.is_some_and(|i| i != current) {
        return Ok(None);
    }
    let live = controller.live_shared();
    let meter = live.synth_meter(current);
    Ok(Some(encode_synth_meter(
        current,
        controller.live().frames,
        meter,
    )))
}

fn encode_synth_meter(
    current: u64,
    pending_frames: u64,
    meter: Option<grooviq_engine::SynthMeter>,
) -> Vec<f64> {
    // Identity is returned even before the audio thread has answered the first request.
    let mut result = vec![current as f64, pending_frames as f64];
    if let Some(meter) = meter {
        result[1] = meter.frames as f64;
        result.extend(
            meter
                .levels
                .iter()
                .chain(meter.scope.iter())
                .chain(meter.motion.iter())
                .map(|v| f64::from(*v)),
        );
    }
    result
}
