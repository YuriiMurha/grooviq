//! The Grooviq desktop app: the Tauri shell around the engine.
//!
//! At startup it creates the engine, plays it on the saved output (silent: nothing sounds
//! until the UI asks), starts the live-state stream for the meters and two threads: one
//! forwards output events, one checks the engine's health every 250 ms. Both log what happened
//! and emit it to the UI as `audio-event`; a third moves recorded live notes into the pattern
//! (see [`notes_cmds`]) and a fourth opens MIDI keyboards as they are plugged in (see [`midi`]).
//! The UI talks back through the async commands in [`commands`], `project_cmds`, [`notes_cmds`]
//! and `midi_cmds`. Started with `GROOVIQ_AUDIO=null` it opens no audio device at all (see
//! [`output_mode`]): the silent mode for QA runs.

mod arrangement_cmds;
#[doc(hidden)]
pub mod arrangement_core;
mod audio_log;
mod autosave_notice;
mod commands;
mod display;
mod events;
mod exit;
#[cfg(feature = "ipc-contract")]
#[doc(hidden)]
pub mod ipc_contract;
#[cfg(target_os = "macos")]
mod macos_quit;
mod menu;
pub mod midi;
mod midi_cmds;
mod notes_cmds;
mod output_mode;
pub mod panic_hook;
mod patterns_cmds;
mod project_cmds;
#[cfg(all(feature = "qa-driver", target_os = "macos"))]
mod qa_driver;
mod sampler_trim_cmds;
mod settings;

use audio_log::{AudioLog, Level};
use autosave_notice::{AutosaveStatus, FAILED_EVENT, disabled_notice};
use events::AudioEvent;
use grooviq_engine::{
    AudioOutput, Controller, EngineConfig, OutputChoice, OutputEvent, StreamServer, engine,
    start_stream,
};
use grooviq_project::autosave::{AutosaveTick, autosave_tick};
use grooviq_project::recovery::{Recovery, RecoveryDir};
use grooviq_project::session::Session;
use midi::{MidiHub, MidirPorts, NO_CHANNEL};
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

/// The rate the engine starts at; the output layer moves it to the device's own.
const START_RATE: f32 = 48_000.0;
/// Live frames per second to the UI.
const LIVE_HZ: u32 = 60;
/// How often the health thread asks the controller.
const HEALTH_PERIOD: Duration = Duration::from_millis(250);
/// The event the UI listens to.
const AUDIO_EVENT: &str = "audio-event";
/// How often the autosave thread asks the session whether an autosave is due.
const AUTOSAVE_CHECK: Duration = Duration::from_secs(1);
/// Sent with the unix seconds of each autosave written.
const AUTOSAVED_EVENT: &str = "project-autosaved";
/// Sent when closing or quitting waits for the person to save or discard.
const CLOSE_REQUESTED_EVENT: &str = "close-requested";

/// Everything the commands reach, kept in Tauri's managed state.
pub(crate) struct Audio {
    controller: Arc<Mutex<Controller>>,
    output: Arc<AudioOutput>,
    log: Arc<Mutex<AudioLog>>,
    settings: Mutex<Settings>,
    /// Where the settings live; `None` if the platform gave no config dir.
    settings_path: Option<PathBuf>,
    /// The live-state stream; `None` if it could not start (the meters stay still).
    stream: Option<StreamServer>,
    /// The open project.
    session: Arc<Mutex<Session>>,
    assets: Arc<grooviq_project::audio_assets::AudioAssets>,
    /// The recovery folder; `None` if it could not be taken (no autosave then). Taken out by a
    /// clean exit.
    recovery: Arc<Mutex<Option<RecoveryDir>>>,
    /// The newest project an earlier session left unsaved, until the person restores or
    /// discards it (undecided, it is offered again on the next launch).
    pending_recovery: Mutex<Option<Recovery>>,
    /// Where backups of overwritten project files go.
    backups: PathBuf,
    /// Set to stop the running export.
    export_cancel: Arc<AtomicBool>,
    export_running: AtomicBool,
    /// Whether the person chose to quit: closing is held back until then.
    exit: exit::ExitState,
    /// What the UI is owed about autosave: why it is off, and the failure going on. Retained,
    /// so the UI can ask once it listens (`autosave_status`); the failure event only carries
    /// later changes.
    autosave: Arc<Mutex<AutosaveStatus>>,
    /// The channel a MIDI keyboard plays ([`NO_CHANNEL`]: none).
    selected_channel: notes_cmds::SelectedChannel,
    /// The MIDI inputs; the `grooviq-midi` thread polls it.
    midi: Arc<Mutex<MidiHub>>,
}

/// Locks `mutex` even if a thread panicked while holding it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Logs `event` and sends it to the UI.
fn report(log: &Mutex<AudioLog>, app: &AppHandle, level: Level, mut event: AudioEvent) {
    let mut log = lock(log);
    event.id = Some(log.push(level, event.message.clone()));
    // Fails only while the app shuts down.
    let _ = app.emit(AUDIO_EVENT, event);
}

/// Starts the engine, the output, the stream and the two reporting threads.
fn start_audio(app: &AppHandle) -> Audio {
    let log = Arc::new(Mutex::new(AudioLog::default()));
    let settings_path = app
        .path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"));
    let settings = match &settings_path {
        Some(path) => {
            let (settings, note) = settings::load(path);
            if let Some((level, message)) = note {
                lock(&log).push(level, message);
            }
            settings
        }
        None => {
            lock(&log).push(Level::Warn, "No config folder: settings will not be saved");
            Settings::default()
        }
    };

    let (controller, processor) = engine(EngineConfig {
        sample_rate: START_RATE,
    });
    #[cfg(feature = "profiling")]
    let processor = {
        let mut processor = processor;
        if let Err(err) = processor.enable_profiling() {
            lock(&log).push(
                Level::Error,
                format!("Tracy profiling could not start: {err}"),
            );
        }
        processor
    };
    let live = controller.live_shared();
    let controller = Arc::new(Mutex::new(controller));
    let session = Arc::new(Mutex::new(Session::new(Arc::clone(&controller))));
    let data_dir = app.path().app_data_dir().ok();
    let (recovery, pending_recovery, autosave_status) = start_recovery(data_dir.as_deref(), &log);
    let autosave_status = Arc::new(Mutex::new(autosave_status));
    let backups = data_dir.map_or_else(
        || std::env::temp_dir().join("grooviq-backups"),
        |dir| dir.join("backups"),
    );
    let autosave_on = recovery.is_some();
    let recovery = Arc::new(Mutex::new(recovery));
    let stream = match start_stream(live, LIVE_HZ) {
        Ok(stream) => Some(stream),
        Err(e) => {
            lock(&log).push(
                Level::Error,
                format!("The live meters could not start: {e}"),
            );
            None
        }
    };

    // `on_event` only forwards: the forwarding thread reads `AudioOutput::current` and emits,
    // so nothing calls into the output from its own callback.
    let (events_tx, events_rx) = mpsc::channel::<OutputEvent>();
    let choice = settings
        .output
        .clone()
        .map_or(OutputChoice::SystemDefault, OutputChoice::Device);
    let (mode, note) =
        output_mode::output_mode(std::env::var_os(output_mode::AUDIO_ENV).as_deref());
    if let Some((level, message)) = note {
        lock(&log).push(level, message);
    }
    // Not holding the controller's lock: `start` takes it itself.
    let output = Arc::new(AudioOutput::start(
        output_mode::backend(mode),
        Arc::clone(&controller),
        processor,
        choice,
        settings.guard,
        Box::new(move |event| {
            // Fails only once the forwarding thread is gone, at shutdown.
            let _ = events_tx.send(event);
        }),
    ));

    spawn_named("grooviq-audio-events", {
        let (output, log, app) = (Arc::downgrade(&output), Arc::clone(&log), app.clone());
        move || forward_output_events(&events_rx, &output, &log, &app)
    });
    spawn_named("grooviq-health", {
        let (controller, log, app) = (Arc::clone(&controller), Arc::clone(&log), app.clone());
        move || watch_health(&controller, &log, &app)
    });
    spawn_named("grooviq-recorder", {
        let (session, controller, log, app) = (
            Arc::clone(&session),
            Arc::clone(&controller),
            Arc::clone(&log),
            app.clone(),
        );
        move || notes_cmds::record_loop(&session, &controller, &log, &app)
    });
    let selected_channel: notes_cmds::SelectedChannel = Arc::new(AtomicU32::new(NO_CHANNEL));
    let midi = Arc::new(Mutex::new(MidiHub::from_settings(
        Box::new(MidirPorts::new()),
        Arc::new(midi::ControllerSink::new(Arc::clone(&controller))),
        Arc::clone(&selected_channel),
        &settings,
    )));
    if !qa_automation() {
        spawn_named("grooviq-midi", {
            let (midi, log, app) = (Arc::clone(&midi), Arc::clone(&log), app.clone());
            move || midi_cmds::poll_loop(&midi, &log, &app)
        });
    }
    if autosave_on {
        spawn_named("grooviq-autosave", {
            let (session, recovery, log, status, app) = (
                Arc::clone(&session),
                Arc::clone(&recovery),
                Arc::clone(&log),
                Arc::clone(&autosave_status),
                app.clone(),
            );
            move || autosave_loop(&session, &recovery, &log, &status, &app)
        });
    }

    let assets = Arc::new(grooviq_project::audio_assets::AudioAssets::with_budget(
        lock(&session).media_budget(),
    ));
    Audio {
        assets,
        controller,
        output,
        log,
        settings: Mutex::new(settings),
        settings_path,
        stream,
        session,
        recovery,
        pending_recovery: Mutex::new(pending_recovery),
        backups,
        export_cancel: Arc::new(AtomicBool::new(false)),
        export_running: AtomicBool::new(false),
        exit: exit::ExitState::default(),
        autosave: autosave_status,
        selected_channel,
        midi,
    }
}

/// Takes the recovery folder in the app's data folder, logging why autosave is off if it
/// cannot.
fn start_recovery(
    data_dir: Option<&std::path::Path>,
    log: &Mutex<AudioLog>,
) -> (Option<RecoveryDir>, Option<Recovery>, AutosaveStatus) {
    let Some(dir) = data_dir else {
        lock(log).push(Level::Warn, "No data folder: autosave and recovery are off");
        return (
            None,
            None,
            AutosaveStatus::off(disabled_notice(&std::io::Error::other(
                "No data folder is available",
            ))),
        );
    };
    match RecoveryDir::start(&dir.join("recovery")) {
        Ok((recovery, found)) => {
            if !found.is_empty() {
                lock(log).push(
                    Level::Info,
                    format!(
                        "An earlier session ended without saving; its autosave is offered back \
                         ({} waiting)",
                        found.len()
                    ),
                );
            }
            // The newest first; the others wait for later launches.
            (
                Some(recovery),
                found.into_iter().next(),
                AutosaveStatus::default(),
            )
        }
        Err(e) => {
            lock(log).push(Level::Warn, format!("Autosave and recovery are off: {e}"));
            (None, None, AutosaveStatus::off(disabled_notice(&e)))
        }
    }
}

/// Every second: if an autosave is due, writes it (see [`autosave_tick`]) and tells the UI: the
/// time of each one written, and, once per distinct error since the last success, why one could
/// not be (the status keeps the latest failure for a UI that asks). Stops once a
/// clean exit took the folder.
fn autosave_loop(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
    log: &Mutex<AudioLog>,
    status: &Mutex<AutosaveStatus>,
    app: &AppHandle,
) {
    loop {
        thread::sleep(AUTOSAVE_CHECK);
        match autosave_tick(session, recovery, Instant::now()) {
            AutosaveTick::Idle | AutosaveTick::Replaced => {}
            AutosaveTick::Stopped => return,
            AutosaveTick::Written(at) => {
                lock(status).succeeded();
                let _ = app.emit(AUTOSAVED_EVENT, at);
            }
            AutosaveTick::Failed(e) => {
                // The log has every failure; the person is told once per distinct error.
                lock(log).push(Level::Warn, format!("Autosave needs attention: {e}"));
                if let Some(message) = lock(status).failed(&e) {
                    let _ = app.emit(FAILED_EVENT, message);
                }
            }
        }
    }
}

/// The app is ending: a clean exit, or a last autosave that the next launch offers back (see
/// [`exit::end_session`]). Nothing to report to if it fails: the UI is gone.
fn end_session(audio: &Audio) {
    let _ = exit::end_session(&audio.exit, &audio.session, &audio.recovery);
}

/// Whether closing must wait for the person. An error means the UI could not be asked.
fn hold_close(app: &AppHandle) -> tauri::Result<bool> {
    #[cfg(all(feature = "qa-driver", target_os = "macos"))]
    let forced = qa_driver::forced_shutdown_requested();
    #[cfg(not(all(feature = "qa-driver", target_os = "macos")))]
    let forced = false;

    let Some(audio) = app.try_state::<Audio>() else {
        return Ok(false);
    };
    let hold = audio.exit.hold(&audio.session, forced);
    if hold {
        app.emit(CLOSE_REQUESTED_EVENT, ())?;
    }
    Ok(hold)
}

fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("a reporting thread starts");
}

/// Emits output events in order until the output is dropped. Holds the output only weakly, so
/// it never keeps it alive, and reads what plays after each event (a switch can be followed by
/// a failed open).
fn forward_output_events(
    events: &Receiver<OutputEvent>,
    output: &Weak<AudioOutput>,
    log: &Mutex<AudioLog>,
    app: &AppHandle,
) {
    for event in events {
        let current = output.upgrade().and_then(|output| output.current());
        let (level, payload) = events::from_output(&event, current.as_ref());
        report(log, app, level, payload);
    }
}

/// Every 250 ms: frees what the engine handed back, restarts it after a panic and reports what
/// happened.
fn watch_health(controller: &Mutex<Controller>, log: &Mutex<AudioLog>, app: &AppHandle) {
    loop {
        thread::sleep(HEALTH_PERIOD);
        // The lock is released before reporting.
        let found = lock(controller).check_health();
        for event in found {
            let (level, payload) = events::from_health(event);
            report(log, app, level, payload);
        }
    }
}

/// View > High Refresh Rate: flips the setting, saves it, ticks the item and applies it.
#[cfg(target_os = "macos")]
fn toggle_high_refresh(app: &AppHandle) {
    let Some(audio) = app.try_state::<Audio>() else {
        return;
    };
    let on = !lock(&audio.settings).high_refresh;
    commands::apply_and_save(&audio, || {}, |s| s.high_refresh = on);
    menu::set_high_refresh_checked(app, on);
    // After the settings lock is released: it logs.
    display::apply(app, on);
}

fn qa_automation() -> bool {
    #[cfg(all(feature = "qa-driver", target_os = "macos"))]
    {
        qa_driver::enabled()
    }
    #[cfg(not(all(feature = "qa-driver", target_os = "macos")))]
    {
        false
    }
}

/// Runs the app.
pub fn run() {
    panic_hook::install();
    if std::env::var_os("GROOVIQ_QA_DRIVER").is_some() && !qa_automation() {
        eprintln!("automation requires a macOS qa-driver build");
        std::process::exit(2);
    }
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    #[cfg(all(feature = "qa-driver", target_os = "macos"))]
    let qa_dir = if qa_automation() {
        let dir = qa_driver::validate().unwrap_or_else(|error| {
            eprintln!("QA launch refused: {error}");
            std::process::exit(2);
        });
        for window in &mut context.config_mut().app.windows {
            window.create = false;
            window.visible = false;
            window.focus = false;
        }
        Some(dir)
    } else {
        None
    };
    let builder = tauri::Builder::default();
    // Cmd+Q through our own Quit item, so it asks about unsaved changes (see `menu`).
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu::build).on_menu_event(|app, event| {
        let id = event.id().as_ref();
        if menu::is_quit(id) {
            // Raises a preventable `ExitRequested`, held below like a window close.
            app.exit(0);
        } else if let Some(redo) = menu::history_action(id) {
            let _ = app.emit("edit-history", redo);
        } else if menu::is_high_refresh(id) {
            toggle_high_refresh(app);
        }
    });
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            #[cfg(all(feature = "qa-driver", target_os = "macos"))]
            if let Some(dir) = qa_dir {
                app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
                for config in &app.config().app.windows {
                    tauri::WebviewWindowBuilder::from_config(app, config)?.build()?;
                }
                qa_driver::start(app.handle().clone(), dir)?;
            }
            let audio = start_audio(app.handle());
            let high_refresh = lock(&audio.settings).high_refresh;
            app.manage(audio);
            #[cfg(target_os = "macos")]
            if let Err(reason) = macos_quit::install(app.handle()) {
                lock(&app.state::<Audio>().log).push(Level::Warn, reason);
            }
            if high_refresh {
                menu::set_high_refresh_checked(app.handle(), true);
                display::apply(app.handle(), true);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sampler_trim_cmds::sampler_trim_view,
            sampler_trim_cmds::sampler_trim_reset,
            sampler_trim_cmds::sampler_trim_undo,
            arrangement_cmds::arrangement_view,
            arrangement_cmds::arrangement_edit,
            arrangement_cmds::arrangement_history,
            arrangement_cmds::arrangement_curve,
            arrangement_cmds::audio_import,
            arrangement_cmds::audio_relink,
            arrangement_cmds::audio_cancel,
            arrangement_cmds::audio_cancel_ticket,
            arrangement_cmds::arrangement_waveform,
            arrangement_cmds::arrangement_mode,
            arrangement_cmds::arrangement_loop,
            arrangement_cmds::arrangement_seek,
            menu::edit_history_enabled,
            commands::audio_outputs,
            commands::audio_select,
            commands::audio_settings,
            commands::audio_set_guard,
            commands::test_tone,
            commands::synth_note,
            commands::live_endpoint,
            commands::audio_log,
            project_cmds::project_view,
            project_cmds::project_edit,
            project_cmds::channel_add,
            project_cmds::channel_set_sample,
            project_cmds::channel_preview,
            project_cmds::transport_play,
            project_cmds::transport_stop,
            project_cmds::project_new,
            project_cmds::project_open,
            project_cmds::project_save,
            project_cmds::project_save_as,
            project_cmds::project_export,
            project_cmds::project_export_cancel,
            project_cmds::autosave_status,
            project_cmds::recovery_status,
            project_cmds::recovery_restore,
            project_cmds::recovery_discard,
            project_cmds::app_exit,
            patterns_cmds::pattern_list,
            patterns_cmds::pattern_edit,
            notes_cmds::pattern_notes,
            notes_cmds::notes_edit,
            notes_cmds::notes_undo,
            notes_cmds::notes_redo,
            notes_cmds::channel_add_synth,
            notes_cmds::channel_note,
            notes_cmds::channel_param_live,
            notes_cmds::synth_meter,
            notes_cmds::channel_select,
            notes_cmds::channel_waveform,
            notes_cmds::record_set,
            notes_cmds::metronome_set,
            midi_cmds::midi_inputs,
            midi_cmds::midi_set_enabled,
        ])
        .build(context)
        .expect("the Tauri app builds");
    app.run(|app, event| match event {
        RunEvent::WindowEvent {
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            if hold_close(app).unwrap_or(true) {
                api.prevent_close();
            }
        }
        RunEvent::ExitRequested { api, .. } => {
            if hold_close(app).unwrap_or(true) {
                api.prevent_exit();
            }
        }
        RunEvent::Exit => {
            #[cfg(all(feature = "qa-driver", target_os = "macos"))]
            qa_driver::cleanup(app);
            if let Some(audio) = app.try_state::<Audio>() {
                end_session(&audio);
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod recovery_startup_tests {
    use super::*;

    #[test]
    fn startup_failures_retain_disabled_status_without_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-a-folder");
        std::fs::write(&file, b"keep").unwrap();
        for path in [None, Some(file.as_path())] {
            let log = Mutex::new(AudioLog::default());
            let (recovery, pending, status) = start_recovery(path, &log);
            assert!(recovery.is_none());
            assert!(pending.is_none());
            assert!(status.disabled.is_some(), "{path:?}");
            assert_eq!(status.clone().disabled, status.disabled);
            assert!(status.failure.is_none());
            let entries = lock(&log).entries();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].level, Level::Warn);
        }
        assert_eq!(std::fs::read(file).unwrap(), b"keep");
    }

    #[test]
    fn successful_startup_is_quiet_and_second_copy_retains_disabled_status() {
        let temp = tempfile::tempdir().unwrap();
        let log = Mutex::new(AudioLog::default());
        let (owner, pending, status) = start_recovery(Some(temp.path()), &log);
        assert!(owner.is_some());
        assert!(pending.is_none());
        assert_eq!(status, AutosaveStatus::default());
        let (second, pending, status) = start_recovery(Some(temp.path()), &log);
        assert!(second.is_none());
        assert!(pending.is_none());
        assert_eq!(
            status.disabled,
            disabled_notice(&std::io::ErrorKind::WouldBlock.into())
        );
        drop(owner);
    }
}
