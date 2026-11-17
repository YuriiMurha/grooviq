//! The commands the UI invokes. All async, so none blocks the window's main thread; none holds
//! the controller's lock while it calls the output.

use crate::audio_log::{Level, LogEntry};
use crate::events::{OutputDto, output_dto};
use crate::{Audio, lock, settings};
use grooviq_engine::{Command, NoteId, OutputChoice};
use serde::Serialize;
use std::sync::Mutex;
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
    // One settings lock around the switch and the save: two quick picks must save the output
    // that ended up playing, not the one that applied first. `select` never blocks.
    apply_and_save(&audio, || audio.output.select(choice), |s| s.output = saved);
    lock(&audio.log).push(Level::Info, message);
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
    // Same as `audio_select`: the switch and the save under one settings lock. `set_guard` is
    // one atomic store.
    apply_and_save(&audio, || audio.output.set_guard(on), |s| s.guard = on);
    let message = if on {
        "Speaker guard on: playback stops if sound would move to speakers"
    } else {
        "Speaker guard off"
    };
    lock(&audio.log).push(Level::Info, message);
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
            // One note per key on this test path: the key names it.
            id: NoteId(u64::from(key)),
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

/// Runs `apply`, then changes the settings and writes them, all under the settings lock. A
/// failed save is logged (the change still applies now).
///
/// Lock order: settings, then log (a failed save logs while holding the settings lock). Nothing
/// takes the settings lock while holding the log lock, and `apply` must not take either.
pub(crate) fn apply_and_save(
    audio: &Audio,
    apply: impl FnOnce(),
    change: impl FnOnce(&mut settings::Settings),
) {
    under_lock(&audio.settings, apply, change, |current| {
        if let Some(path) = &audio.settings_path
            && let Err(e) = settings::save(path, current)
        {
            lock(&audio.log).push(Level::Warn, format!("Settings could not be saved: {e}"));
        }
    });
}

/// Holds `state`'s lock while it runs `apply`, `change` and `save`, so concurrent callers
/// cannot interleave: what `save` writes is what the last `apply` did.
fn under_lock<S>(
    state: &Mutex<S>,
    apply: impl FnOnce(),
    change: impl FnOnce(&mut S),
    save: impl FnOnce(&S),
) {
    let mut current = lock(state);
    apply();
    change(&mut current);
    save(&current);
}

#[cfg(test)]
mod tests {
    use super::under_lock;
    use std::sync::mpsc;
    use std::sync::{Barrier, Mutex};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn the_lock_is_held_while_applying_and_saving() {
        let state = Mutex::new(0);
        under_lock(
            &state,
            || assert!(state.try_lock().is_err(), "unlocked while applying"),
            |s| *s = 1,
            |_| assert!(state.try_lock().is_err(), "unlocked while saving"),
        );
        assert_eq!(*state.lock().unwrap(), 1);
    }

    /// Two racing picks: whichever applies last must also be what is saved. The first pick
    /// waits (briefly) for the other to apply; without the lock it gets it, and the second
    /// pick saves before the first, so the file ends on the output that stopped playing.
    #[test]
    fn two_quick_picks_save_the_output_that_plays() {
        let state = Mutex::new(String::new());
        let playing = Mutex::new(String::new());
        let saved = Mutex::new(String::new());
        let (applied_tx, applied_rx) = mpsc::channel::<()>();
        let applied_rx = Mutex::new(applied_rx);
        let start = Barrier::new(2);

        thread::scope(|scope| {
            for (name, first) in [("A", true), ("B", false)] {
                let (state, playing, saved, start) = (&state, &playing, &saved, &start);
                let (applied_tx, applied_rx) = (applied_tx.clone(), &applied_rx);
                scope.spawn(move || {
                    start.wait();
                    if !first {
                        // Let A get into its apply first.
                        thread::sleep(Duration::from_millis(20));
                    }
                    under_lock(
                        state,
                        || {
                            *playing.lock().unwrap() = name.to_owned();
                            if first {
                                // Hold the pick open until B applies (never, under the lock).
                                let _ = applied_rx
                                    .lock()
                                    .unwrap()
                                    .recv_timeout(Duration::from_millis(300));
                            } else {
                                let _ = applied_tx.send(());
                            }
                        },
                        |s| *s = name.to_owned(),
                        |s| *saved.lock().unwrap() = s.clone(),
                    );
                });
            }
        });

        assert_eq!(*saved.lock().unwrap(), *playing.lock().unwrap());
    }
}
