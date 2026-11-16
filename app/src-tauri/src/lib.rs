//! The Grooviq desktop app: the Tauri shell around the engine.
//!
//! At startup it creates the engine, plays it on the saved output (silent: nothing sounds
//! until the UI asks), starts the live-state stream for the meters and two threads: one
//! forwards output events, one checks the engine's health every 250 ms. Both log what happened
//! and emit it to the UI as `audio-event`. The UI talks back through the async commands in
//! [`commands`].

mod audio_log;
mod commands;
mod events;
mod exit;
pub mod panic_hook;
mod project_cmds;
mod settings;

use audio_log::{AudioLog, Level};
use events::AudioEvent;
use grooviq_engine::{
    AudioOutput, Controller, EngineConfig, OutputChoice, OutputEvent, StreamServer, cpal_backend,
    engine, start_stream,
};
use grooviq_project::recovery::{Recovery, RecoveryDir};
use grooviq_project::session::Session;
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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
    /// The recovery folder; `None` if it could not be taken (no autosave then). Taken out by a
    /// clean exit.
    recovery: Arc<Mutex<Option<RecoveryDir>>>,
    /// What the last session left unsaved, until the person restores or discards it.
    pending_recovery: Mutex<Option<Recovery>>,
    /// Where backups of overwritten project files go.
    backups: PathBuf,
    /// Set to stop the running export.
    export_cancel: Arc<AtomicBool>,
    export_running: AtomicBool,
    /// Whether the person chose to quit: closing is held back until then.
    exit: exit::ExitState,
}

/// Locks `mutex` even if a thread panicked while holding it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Logs `event` and sends it to the UI.
fn report(log: &Mutex<AudioLog>, app: &AppHandle, level: Level, event: AudioEvent) {
    lock(log).push(level, event.message.clone());
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
    let live = controller.live_shared();
    let controller = Arc::new(Mutex::new(controller));
    let session = Arc::new(Mutex::new(Session::new(Arc::clone(&controller))));
    let data_dir = app.path().app_data_dir().ok();
    let (recovery, pending_recovery) = start_recovery(data_dir.as_deref(), &log);
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
    // Not holding the controller's lock: `start` takes it itself.
    let output = Arc::new(AudioOutput::start(
        cpal_backend(),
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
    if autosave_on {
        spawn_named("grooviq-autosave", {
            let (session, recovery, log, app) = (
                Arc::clone(&session),
                Arc::clone(&recovery),
                Arc::clone(&log),
                app.clone(),
            );
            move || autosave_loop(&session, &recovery, &log, &app)
        });
    }

    Audio {
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
    }
}

/// Takes the recovery folder in the app's data folder, logging why autosave is off if it
/// cannot.
fn start_recovery(
    data_dir: Option<&std::path::Path>,
    log: &Mutex<AudioLog>,
) -> (Option<RecoveryDir>, Option<Recovery>) {
    let Some(dir) = data_dir else {
        lock(log).push(Level::Warn, "No data folder: autosave and recovery are off");
        return (None, None);
    };
    match RecoveryDir::start(&dir.join("recovery")) {
        Ok((recovery, found)) => {
            if found.is_some() {
                lock(log).push(
                    Level::Info,
                    "The last session ended without saving; its autosave is offered back",
                );
            }
            (Some(recovery), found)
        }
        Err(e) => {
            lock(log).push(Level::Warn, format!("Autosave and recovery are off: {e}"));
            (None, None)
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every second: if an autosave is due, snapshots the project under the session's lock and
/// writes it outside it, then tells the UI. Stops once a clean exit took the folder.
fn autosave_loop(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
    log: &Mutex<AudioLog>,
    app: &AppHandle,
) {
    loop {
        thread::sleep(AUTOSAVE_CHECK);
        let now = Instant::now();
        let snapshot = {
            let mut session = lock(session);
            if session.autosave_due(now) {
                session.autosave_snapshot(now)
            } else {
                None
            }
        };
        let Some((project, samples, original)) = snapshot else {
            continue;
        };
        let written = match lock(recovery).as_ref() {
            Some(dir) => dir.write_autosave(&project, &samples, original.as_deref()),
            None => return,
        };
        match written {
            Ok(()) => {
                let at = unix_now();
                lock(session).autosaved(at);
                let _ = app.emit(AUTOSAVED_EVENT, at);
            }
            Err(e) => {
                lock(session).autosave_failed(Instant::now());
                lock(log).push(
                    Level::Warn,
                    format!("The autosave could not be written: {e}"),
                );
            }
        }
    }
}

/// The app is ending: a clean exit, or a last autosave that the next launch offers back (see
/// [`exit::end_session`]). Nothing to report to if it fails: the UI is gone.
fn end_session(audio: &Audio) {
    let _ = exit::end_session(&audio.exit, &audio.session, &audio.recovery);
}

/// Whether closing must wait for the person; if so, asks the UI.
fn hold_close(app: &AppHandle) -> bool {
    let Some(audio) = app.try_state::<Audio>() else {
        return false;
    };
    let hold = audio.exit.hold(&audio.session);
    if hold {
        // Fails only while the app shuts down.
        let _ = app.emit(CLOSE_REQUESTED_EVENT, ());
    }
    hold
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

/// Runs the app.
pub fn run() {
    panic_hook::install();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let audio = start_audio(app.handle());
            app.manage(audio);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
            project_cmds::recovery_status,
            project_cmds::recovery_restore,
            project_cmds::recovery_discard,
            project_cmds::app_exit,
        ])
        .build(tauri::generate_context!())
        .expect("the Tauri app builds");
    app.run(|app, event| match event {
        RunEvent::WindowEvent {
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            if hold_close(app) {
                api.prevent_close();
            }
        }
        RunEvent::ExitRequested { api, .. } => {
            if hold_close(app) {
                api.prevent_exit();
            }
        }
        RunEvent::Exit => {
            if let Some(audio) = app.try_state::<Audio>() {
                end_session(&audio);
            }
        }
        _ => {}
    });
}
