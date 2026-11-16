//! Quitting: when a window close or a quit waits for the person, and what each way of ending
//! leaves in the recovery folder. Pure of Tauri, so every case is tested here; the run loop in
//! `lib.rs` only calls these.

use crate::lock;
use grooviq_project::recovery::RecoveryDir;
use grooviq_project::session::Session;
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Whether the person already chose to quit.
#[derive(Debug, Default)]
pub(crate) struct ExitState {
    confirmed: AtomicBool,
}

impl ExitState {
    /// Whether a window close or a quit must wait for the person: there are unsaved changes,
    /// and they have not chosen (through `app_exit`) to quit anyway.
    pub(crate) fn hold(&self, session: &Mutex<Session>) -> bool {
        !self.confirmed.load(Ordering::Acquire) && lock(session).needs_confirmation()
    }
}

/// Removes the autosave and the lock: the next launch offers nothing. Once only; later calls
/// do nothing.
fn clean_exit(recovery: &Mutex<Option<RecoveryDir>>) -> io::Result<()> {
    match lock(recovery).take() {
        Some(dir) => dir.clean_exit(),
        None => Ok(()),
    }
}

/// The person chose to quit (they saved, or chose to lose the changes): from now on nothing
/// holds the exit, and the recovery folder is cleaned.
pub(crate) fn confirm_exit(
    exit: &ExitState,
    recovery: &Mutex<Option<RecoveryDir>>,
) -> io::Result<()> {
    exit.confirmed.store(true, Ordering::Release);
    clean_exit(recovery)
}

/// The app is ending. With unsaved changes the person did not choose to lose (a forced quit),
/// a last autosave is written and the lock stays, so the next launch offers the work back;
/// otherwise the exit is clean.
pub(crate) fn end_session(
    exit: &ExitState,
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
) -> io::Result<()> {
    if !exit.hold(session) {
        return clean_exit(recovery);
    }
    let snapshot = lock(session).autosave_snapshot(Instant::now());
    // Taken out and dropped without a clean exit: the lock file stays behind.
    let Some(dir) = lock(recovery).take() else {
        return Ok(());
    };
    match snapshot {
        Some((project, samples, original)) => {
            dir.write_autosave(&project, &samples, original.as_deref())
        }
        // Everything unsaved is in the last autosave already.
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grooviq_engine::{EngineConfig, engine};
    use grooviq_project::session::Edit;
    use std::path::Path;
    use std::sync::Arc;

    fn session(dirty: bool) -> Mutex<Session> {
        let (controller, _processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let mut session = Session::new(Arc::new(Mutex::new(controller)));
        if dirty {
            session
                .apply(Edit::SetTempo { bpm: 99.0 }, Instant::now())
                .unwrap();
        }
        Mutex::new(session)
    }

    fn recovery(root: &Path) -> Mutex<Option<RecoveryDir>> {
        let (dir, found) = RecoveryDir::start(root).unwrap();
        assert!(found.is_empty());
        Mutex::new(Some(dir))
    }

    /// What the next launch finds: the newest recovered project's tempo, if any is offered.
    fn next_launch(root: &Path) -> Option<f32> {
        let (dir, found) = RecoveryDir::start(root).unwrap();
        let bpm = found
            .first()
            .map(|r| grooviq_project::load(&r.file).unwrap().project.bpm);
        dir.clean_exit().unwrap();
        bpm
    }

    #[test]
    fn closing_without_unsaved_changes_is_not_held() {
        let exit = ExitState::default();
        assert!(!exit.hold(&session(false)));
    }

    #[test]
    fn closing_with_unsaved_changes_waits_for_the_person() {
        let exit = ExitState::default();
        let session = session(true);
        assert!(exit.hold(&session), "window close and quit are both held");
        assert!(
            exit.hold(&session),
            "and stay held until the person chooses"
        );
    }

    #[test]
    fn after_app_exit_nothing_holds_and_nothing_is_left_to_recover() {
        let root = tempfile::tempdir().unwrap();
        let exit = ExitState::default();
        let session = session(true);
        let recovery = recovery(root.path());
        lock(&recovery)
            .as_ref()
            .unwrap()
            .write_autosave(&lock(&session).project().clone(), &Default::default(), None)
            .unwrap();
        confirm_exit(&exit, &recovery).unwrap();
        assert!(
            !exit.hold(&session),
            "the quit app_exit asks for goes through"
        );
        assert!(lock(&recovery).is_none());
        assert!(!root.path().join("session.lock").exists());
        assert!(!root.path().join("autosave.grooviq").exists());
        // The run loop then ends the session again: nothing changes.
        end_session(&exit, &session, &recovery).unwrap();
        assert_eq!(next_launch(root.path()), None);
    }

    #[test]
    fn a_forced_exit_with_unsaved_changes_offers_them_on_the_next_launch() {
        let root = tempfile::tempdir().unwrap();
        let exit = ExitState::default();
        let session = session(true);
        let recovery = recovery(root.path());
        end_session(&exit, &session, &recovery).unwrap();
        assert!(lock(&recovery).is_none());
        assert!(root.path().join("session.lock").exists(), "the lock stays");
        assert_eq!(next_launch(root.path()), Some(99.0));
    }

    #[test]
    fn a_forced_exit_after_an_autosave_offers_that_autosave() {
        let root = tempfile::tempdir().unwrap();
        let exit = ExitState::default();
        let session = session(true);
        let recovery = recovery(root.path());
        let (project, samples, _) = lock(&session).autosave_snapshot(Instant::now()).unwrap();
        lock(&recovery)
            .as_ref()
            .unwrap()
            .write_autosave(&project, &samples, None)
            .unwrap();
        end_session(&exit, &session, &recovery).unwrap();
        assert_eq!(next_launch(root.path()), Some(99.0));
    }

    #[test]
    fn the_menu_quit_is_held_until_the_person_chooses() {
        // Cmd+Q calls `AppHandle::exit`, whose `ExitRequested` the run loop holds with
        // `hold`: asked while dirty, let through when clean or once `app_exit` confirmed.
        let root = tempfile::tempdir().unwrap();
        let exit = ExitState::default();
        assert!(!exit.hold(&session(false)), "nothing unsaved: Cmd+Q quits");
        let dirty = session(true);
        assert!(exit.hold(&dirty), "unsaved: Cmd+Q asks first");
        confirm_exit(&exit, &recovery(root.path())).unwrap();
        assert!(
            !exit.hold(&dirty),
            "the app.exit(0) after app_exit goes through"
        );
    }

    #[test]
    fn ending_without_unsaved_changes_is_clean() {
        let root = tempfile::tempdir().unwrap();
        let exit = ExitState::default();
        let session = session(false);
        let recovery = recovery(root.path());
        end_session(&exit, &session, &recovery).unwrap();
        assert!(!root.path().join("session.lock").exists());
        assert_eq!(next_launch(root.path()), None);
    }
}
