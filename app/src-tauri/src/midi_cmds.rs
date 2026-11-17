//! The MIDI commands, the engine sink the keyboards play into, and the `grooviq-midi` thread that
//! polls for keyboards plugged in or out.
//!
//! The keyboards' notes go straight to the engine controller (its lock only, never the
//! session's), so the audio thread's record tap hears them like any live note.

use crate::audio_log::{AudioLog, Level};
use crate::midi::{CommandSink, MidiHub, Poll};
use crate::{Audio, commands, lock};
use grooviq_engine::{Command, Controller};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

/// Sent (no payload) when the list of MIDI inputs or one's switch changed.
pub(crate) const INPUTS_CHANGED: &str = "midi-inputs-changed";
/// How often the MIDI thread looks for keyboards plugged in or out.
const POLL_PERIOD: Duration = Duration::from_secs(2);
/// How often a note-off the engine did not take is tried again, 1 ms apart.
const RELEASE_RETRIES: u32 = 10;

/// One MIDI input as the settings show it.
#[derive(Serialize)]
pub struct MidiInputView {
    name: String,
    enabled: bool,
}

/// The keyboards' side of the engine: commands go to the controller as they come.
pub(crate) struct ControllerSink(pub(crate) Arc<Mutex<Controller>>);

impl CommandSink for ControllerSink {
    fn send(&self, cmd: Command) {
        // A note-off the engine's queue could not take would leave the note ringing, so it is
        // tried again for a few milliseconds (this is a MIDI thread, not the audio one). A note
        // on a channel that is gone is refused the same way: nothing is left to ring.
        let release = matches!(cmd, Command::ChannelNote { on: false, .. });
        for attempt in 0..=RELEASE_RETRIES {
            if lock(&self.0).send(cmd).is_ok() || !release {
                return;
            }
            if attempt < RELEASE_RETRIES {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// The MIDI inputs plugged in, with whether each is on.
#[tauri::command]
pub async fn midi_inputs(audio: State<'_, Audio>) -> Result<Vec<MidiInputView>, String> {
    Ok(lock(&audio.midi)
        .inputs()
        .into_iter()
        .map(|(name, enabled)| MidiInputView { name, enabled })
        .collect())
}

/// Switches one MIDI input on or off and remembers it. Takes effect at once: switching off lets
/// go of the notes it holds.
#[tauri::command]
pub async fn midi_set_enabled(
    app: AppHandle,
    audio: State<'_, Audio>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    let mut poll = Poll::default();
    commands::apply_and_save(
        &audio,
        || poll = lock(&audio.midi).set_enabled(&name, enabled),
        |settings| {
            settings.midi_disabled.retain(|n| *n != name);
            if !enabled {
                settings.midi_disabled.push(name.clone());
            }
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

/// At once and then every [`POLL_PERIOD`]: opens the keyboards that were plugged in and closes
/// the ones that were unplugged. Runs for the life of the app.
pub(crate) fn poll_loop(hub: &Mutex<MidiHub>, log: &Mutex<AudioLog>, app: &AppHandle) {
    loop {
        // The lock is released before reporting.
        let poll = lock(hub).poll();
        report(&poll, log, app);
        thread::sleep(POLL_PERIOD);
    }
}
