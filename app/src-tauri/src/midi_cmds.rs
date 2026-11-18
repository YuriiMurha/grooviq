//! The MIDI commands, the engine sink the keyboards play into, and the `grooviq-midi` thread that
//! polls for keyboards plugged in or out.
//!
//! The keyboards' notes go straight to the engine controller (its lock only, never the
//! session's), so the audio thread's record tap hears them like any live note.

use crate::audio_log::{AudioLog, Level};
use crate::midi::{MidiHub, Poll, Schedule};
use crate::{Audio, commands, lock};
use serde::Serialize;
use std::sync::Mutex;
use std::thread;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};

/// Sent (no payload) when the list of MIDI inputs or one's switch changed.
pub(crate) const INPUTS_CHANGED: &str = "midi-inputs-changed";
/// One MIDI input as the settings show it.
#[derive(Serialize)]
pub struct MidiInputView {
    id: String,
    name: String,
    enabled: bool,
}

/// The MIDI inputs plugged in, with whether each is on.
#[tauri::command]
pub async fn midi_inputs(audio: State<'_, Audio>) -> Result<Vec<MidiInputView>, String> {
    Ok(lock(&audio.midi)
        .input_views()
        .into_iter()
        .map(|(id, name, enabled)| MidiInputView { id, name, enabled })
        .collect())
}

/// Switches one MIDI input on or off and remembers it. Takes effect at once: switching off lets
/// go of the notes it holds.
#[tauri::command]
pub async fn midi_set_enabled(
    app: AppHandle,
    audio: State<'_, Audio>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut poll = Poll::default();
    commands::apply_and_save(
        &audio,
        || {},
        |settings| {
            poll = lock(&audio.midi).switch_configured(settings, &id, enabled);
        },
    );
    report(&poll, &audio.log, &app);
    Ok(())
}

/// Logs what `poll` did and tells the UI if the list changed.
fn report(poll: &Poll, log: &Mutex<AudioLog>, app: &AppHandle) {
    {
        let mut log = lock(log);
        for name in &poll.opened {
            log.push(Level::Info, format!("MIDI input {name} connected"));
        }
        for name in &poll.closed {
            log.push(Level::Info, format!("MIDI input {name} closed"));
        }
        for (name, why) in &poll.failed {
            log.push(
                Level::Warn,
                format!("MIDI input {name} could not be opened: {why}"),
            );
        }
    }
    if poll.changed {
        // Fails only while the app shuts down.
        let _ = app.emit(INPUTS_CHANGED, ());
    }
}

/// Runs the hub's timetable (see [`Schedule`]): a poll at once and every 2 s, and between polls
/// a retry of the releases the engine refused. Runs for the life of the app.
pub(crate) fn poll_loop(hub: &Mutex<MidiHub>, log: &Mutex<AudioLog>, app: &AppHandle) {
    let mut schedule = Schedule::new();
    loop {
        let (action, wait) = schedule.step(Instant::now());
        // The lock is released before reporting.
        let poll = lock(hub).run(action);
        if let Some(poll) = poll {
            report(&poll, log, app);
        }
        thread::sleep(wait);
    }
}
