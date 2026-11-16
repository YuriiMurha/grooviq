//! One tick of the autosave clock: when the session says an autosave is due, snapshot it, write
//! it into the recovery folder, and tell the session. The three phases take their locks one at a
//! time (the session, then the recovery folder, then the session again), so a slow write never
//! holds up an edit and the locks never nest.

use crate::recovery::RecoveryDir;
use crate::session::Session;
use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// What a tick did.
#[derive(Debug)]
pub enum AutosaveTick {
    /// Nothing was due.
    Idle,
    /// The recovery folder is gone (a clean exit took it): stop ticking.
    Stopped,
    /// An autosave was written at this time, in unix seconds.
    Written(u64),
    /// The autosave could not be written; the session will try again after a minute.
    Failed(io::Error),
    /// A project was opened, started or restored while the autosave was written, so it held the
    /// old one: it was removed again, and nothing is recorded.
    Replaced,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Runs one tick at `now`.
pub fn autosave_tick(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
    now: Instant,
) -> AutosaveTick {
    autosave_tick_with(session, recovery, now, || {})
}

/// [`autosave_tick`] with `after_write` run once the autosave is written and before the session
/// hears of it: the test hook for "the project was replaced while it was written". Not part of
/// the API.
#[doc(hidden)]
pub fn autosave_tick_with(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
    now: Instant,
    after_write: impl FnOnce(),
) -> AutosaveTick {
    let snapshot = {
        let mut session = lock(session);
        if session.autosave_due(now) {
            let generation = session.generation();
            session
                .autosave_snapshot(now)
                .map(|snapshot| (generation, snapshot))
        } else {
            None
        }
    };
    let Some((generation, (project, samples, original))) = snapshot else {
        return AutosaveTick::Idle;
    };
    let written = match lock(recovery).as_ref() {
        Some(dir) => dir.write_autosave(&project, &samples, original.as_deref()),
        None => return AutosaveTick::Stopped,
    };
    after_write();
    match written {
        Ok(()) => {
            let at = unix_now();
            if lock(session).autosaved(generation, at) {
                return AutosaveTick::Written(at);
            }
            // A crash now would offer the project the person just left. If the folder cannot
            // be cleared, the next autosave of the open project replaces it.
            if let Some(dir) = lock(recovery).as_ref() {
                let _ = dir.clear_autosave();
            }
            AutosaveTick::Replaced
        }
        Err(e) => {
            lock(session).autosave_failed(Instant::now());
            AutosaveTick::Failed(e)
        }
    }
}
