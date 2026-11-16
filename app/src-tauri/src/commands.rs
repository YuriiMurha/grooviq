//! The commands the UI invokes. All async, so none blocks the window's main thread; none holds
//! the controller's lock while it calls the output.

use crate::audio_log::{Level, LogEntry};
use crate::events::{OutputDto, output_dto};
use crate::{Audio, lock, settings};
use grooviq_engine::{Command, OutputChoice};
use serde::Serialize;
use tauri::State;

/// The id `audio_select` takes for "follow the system default".
const SYSTEM_DEFAULT: &str = "default";
/// The test tone: A4, quiet enough not to startle.
const TONE_HZ: f32 = 440.0;
const TONE_DB: f32 = -18.0;
/// Velocity of notes from the UI.
const NOTE_VELOCITY: f32 = 0.8;

/// Where the live stream listens.
#[derive(Serialize)]
pub struct LiveEndpoint {
    port: u16,
    token: String,
}

/// The saved choice (`"default"` or a device id) and the speaker guard, for the popover.
#[derive(Serialize)]
pub struct AudioSettings {
    output: String,
    guard: bool,
}

/// Every output device, with the one that plays marked `current`.
#[tauri::command]
pub async fn audio_outputs(audio: State<'_, Audio>) -> Result<Vec<OutputDto>, String> {
    let current = audio.output.current();
    Ok(audio
        .output
        .outputs()
        .iter()
        .map(|info| output_dto(info, current.as_ref()))
        .collect())
}

/// Plays on the output `id` (`"default"`: follow the system default) and remembers it. The
/// guard lets this move play on: the user chose it.
#[tauri::command]
pub async fn audio_select(audio: State<'_, Audio>, id: String) -> Result<(), String> {
    let (choice, saved, message) = if id == SYSTEM_DEFAULT {
        (
            OutputChoice::SystemDefault,
            None,
            "Output set to follow the system default".to_owned(),
        )
    } else {
        let outputs = audio.output.outputs();
        let Some(info) = outputs.iter().find(|o| o.id == id) else {
            return Err(format!("No output with the id {id:?}"));
        };
        let message = format!("Output set to {}", info.name);
        (OutputChoice::Device(id.clone()), Some(id), message)
    };
    audio.output.select(choice);
    lock(&audio.log).push(Level::Info, message);
    save_settings(&audio, |s| s.output = saved);
    Ok(())
}

/// The saved output choice and guard.
#[tauri::command]
pub async fn audio_settings(audio: State<'_, Audio>) -> Result<AudioSettings, String> {
    let settings = lock(&audio.settings);
    Ok(AudioSettings {
        output: settings
            .output
            .clone()
            .unwrap_or_else(|| SYSTEM_DEFAULT.to_owned()),
        guard: settings.guard,
    })
}

/// Turns the speaker guard on or off and remembers it.
#[tauri::command]
pub async fn audio_set_guard(audio: State<'_, Audio>, on: bool) -> Result<(), String> {
    audio.output.set_guard(on);
    let message = if on {
        "Speaker guard on: playback stops if sound would move to speakers"
    } else {
        "Speaker guard off"
    };
    lock(&audio.log).push(Level::Info, message);
    save_settings(&audio, |s| s.guard = on);
    Ok(())
}

/// Switches the test tone.
#[tauri::command]
pub async fn test_tone(audio: State<'_, Audio>, on: bool) -> Result<(), String> {
    send(
        &audio,
        Command::TestTone {
            on,
            hz: TONE_HZ,
            gain_db: TONE_DB,
        },
    )
}

/// Starts or releases a synth note (`key` 0..=127).
#[tauri::command]
pub async fn synth_note(audio: State<'_, Audio>, key: u8, on: bool) -> Result<(), String> {
    send(
        &audio,
        Command::Note {
            key,
            velocity: NOTE_VELOCITY,
            on,
        },
    )
}

/// Where the UI connects for live frames.
#[tauri::command]
pub async fn live_endpoint(audio: State<'_, Audio>) -> Result<LiveEndpoint, String> {
    audio
        .stream
        .as_ref()
        .map(|s| LiveEndpoint {
            port: s.port,
            token: s.token.clone(),
        })
        .ok_or_else(|| "The live stream is not running".to_owned())
}

/// The audio log, oldest first.
#[tauri::command]
pub async fn audio_log(audio: State<'_, Audio>) -> Result<Vec<LogEntry>, String> {
    Ok(lock(&audio.log).entries())
}

fn send(audio: &Audio, cmd: Command) -> Result<(), String> {
    lock(&audio.controller).send(cmd).map_err(|_| {
        "The engine did not take the command (a value out of range, or it is busy)".to_owned()
    })
}

/// Changes the settings and writes them, logging a failure (the change still applies now).
fn save_settings(audio: &Audio, change: impl FnOnce(&mut settings::Settings)) {
    let mut current = lock(&audio.settings);
    change(&mut current);
    if let Some(path) = &audio.settings_path
        && let Err(e) = settings::save(path, &current)
    {
        lock(&audio.log).push(Level::Warn, format!("Settings could not be saved: {e}"));
    }
}
