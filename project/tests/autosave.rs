//! The autosave clock between the session and the recovery folder.

use grooviq_engine::{EngineConfig, engine};
use grooviq_project::autosave::{AutosaveTick, autosave_tick, autosave_tick_with};
use grooviq_project::load;
use grooviq_project::recovery::RecoveryDir;
use grooviq_project::session::{AUTOSAVE_PERIOD, Edit, Session};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn session() -> Mutex<Session> {
    let (controller, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    Mutex::new(Session::new(Arc::new(Mutex::new(controller))))
}

fn edit(session: &Mutex<Session>, bpm: f32, at: Instant) {
    session
        .lock()
        .unwrap()
        .apply(Edit::SetTempo { bpm }, at)
        .unwrap();
}

/// What the next launch offers: the tempo of each pending project, newest first.
fn next_launch(root: &Path) -> Vec<f32> {
    let (dir, found) = RecoveryDir::start(root).unwrap();
    let tempos = found
        .iter()
        .map(|r| load(&r.file).unwrap().project.bpm)
        .collect();
    drop(dir);
    tempos
}

#[test]
fn a_due_autosave_is_written_and_recorded() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let t0 = Instant::now();
    assert!(matches!(
        autosave_tick(&session, &recovery, t0),
        AutosaveTick::Idle
    ));
    edit(&session, 120.0, t0);
    assert!(matches!(
        autosave_tick(&session, &recovery, t0),
        AutosaveTick::Idle
    ));
    let tick = autosave_tick(&session, &recovery, t0 + AUTOSAVE_PERIOD);
    assert!(matches!(tick, AutosaveTick::Written(_)), "{tick:?}");
    assert!(session.lock().unwrap().view().autosaved_at.is_some());
    drop(recovery);
    assert_eq!(next_launch(root.path()), [120.0]);
}

#[test]
fn an_autosave_written_while_a_new_project_starts_is_removed_again() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let t0 = Instant::now();
    edit(&session, 120.0, t0);

    // New is chosen while the old project's autosave is being written.
    let tick = autosave_tick_with(&session, &recovery, t0 + AUTOSAVE_PERIOD, || {
        session.lock().unwrap().new_project().unwrap();
    });
    assert!(matches!(tick, AutosaveTick::Replaced), "{tick:?}");
    assert_eq!(session.lock().unwrap().view().autosaved_at, None);
    // The app then crashes: the old project must not be offered back.
    drop(recovery);
    assert_eq!(next_launch(root.path()), Vec::<f32>::new());
}

#[test]
fn a_failed_autosave_is_reported_and_retried() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    // The folder vanishes under the running session.
    std::fs::remove_dir_all(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let t0 = Instant::now();
    edit(&session, 120.0, t0);
    let tick = autosave_tick(&session, &recovery, t0 + AUTOSAVE_PERIOD);
    assert!(matches!(tick, AutosaveTick::Failed(_)), "{tick:?}");
    assert!(
        session
            .lock()
            .unwrap()
            .autosave_due(t0 + AUTOSAVE_PERIOD * 3)
    );
}

#[test]
fn a_taken_recovery_folder_stops_the_clock() {
    let session = session();
    let recovery = Mutex::new(None);
    let t0 = Instant::now();
    edit(&session, 120.0, t0);
    assert!(matches!(
        autosave_tick(&session, &recovery, t0 + AUTOSAVE_PERIOD),
        AutosaveTick::Stopped
    ));
}
