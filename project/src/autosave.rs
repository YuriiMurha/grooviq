//! The autosave clock and cleanup of snapshots superseded by a save, history replay or project
//! replacement. When both locks are needed, recovery comes first. The session lock is released
//! before filesystem work, so an edit can proceed during a slow write or cleanup.

use crate::recovery::RecoveryDir;
use crate::session::Session;
use std::io;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// What a tick did.
#[derive(Debug)]
pub enum AutosaveTick {
    /// No new snapshot was due. Any pending cleanup succeeded.
    Idle,
    /// The recovery folder is gone (a clean exit took it): stop ticking.
    Stopped,
    /// An autosave was written at this time, in unix seconds.
    Written(u64),
    /// A write or cleanup failed. The clock will retry it.
    Failed(io::Error),
    /// The snapshot was superseded while it was being written and is no longer recoverable.
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

/// [`autosave_tick`] with a hook after the write attempt and before the session records its
/// outcome. Not part of the API.
#[doc(hidden)]
pub fn autosave_tick_with(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
    now: Instant,
    after_write: impl FnOnce(),
) -> AutosaveTick {
    autosave_tick_on(
        session,
        recovery,
        now,
        after_write,
        &mut crate::file::RealDisk,
    )
}

/// Completes any cleanup owed by the session. Native commands do this before returning a
/// clean result. The autosave clock retries a failure and also handles SDK-free callers.
pub fn clear_retired_autosaves(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
) -> io::Result<()> {
    let pending = lock(session).autosave_cleanup_pending();
    if pending && let Some(dir) = lock(recovery).as_ref() {
        clear_retired_in(session, dir)?;
    }
    Ok(())
}

fn clear_retired_in(session: &Mutex<Session>, dir: &RecoveryDir) -> io::Result<()> {
    let through = lock(session).autosave_cleanup();
    if let Some(through) = through {
        dir.clear_autosaves_through(through)?;
        lock(session).autosave_cleanup_finished(through);
    }
    Ok(())
}

/// Fault-injection boundary for the actual autosave lifecycle.
#[doc(hidden)]
pub fn autosave_tick_on(
    session: &Mutex<Session>,
    recovery: &Mutex<Option<RecoveryDir>>,
    now: Instant,
    after_write: impl FnOnce(),
    disk: &mut impl crate::file::Disk,
) -> AutosaveTick {
    let work_due = {
        let s = lock(session);
        s.autosave_due(now) || s.autosave_cleanup().is_some()
    };
    if !work_due {
        return AutosaveTick::Idle;
    }
    let (written, sequence, generation) = {
        let recovery = lock(recovery);
        let Some(dir) = recovery.as_ref() else {
            return AutosaveTick::Stopped;
        };
        let snapshot = {
            let mut s = lock(session);
            if s.autosave_due(now) {
                s.autosave_snapshot(now).map(|snapshot| {
                    let sequence = dir.reserve_autosave();
                    s.autosave_started(sequence);
                    (sequence, s.generation(), snapshot)
                })
            } else {
                None
            }
        };
        let Some((sequence, generation, (project, samples, original))) = snapshot else {
            return match clear_retired_in(session, dir) {
                Ok(()) => AutosaveTick::Idle,
                Err(e) => AutosaveTick::Failed(e),
            };
        };
        let written = dir.write_reserved_autosave(
            sequence,
            &project,
            &samples,
            original.as_deref(),
            || {
                if lock(session).autosave_is_current(sequence) {
                    Ok(())
                } else {
                    Err(io::Error::other(
                        "Autosave superseded before metadata commit",
                    ))
                }
            },
            disk,
        );
        (written, sequence, generation)
    };
    after_write();
    let cleanup = clear_retired_autosaves(session, recovery);
    let mut s = lock(session);
    if written.is_err() && s.autosave_is_current(sequence) {
        s.autosave_failed(Instant::now());
    }
    if let Err(e) = cleanup {
        return AutosaveTick::Failed(e);
    }
    if !s.autosave_is_current(sequence) {
        return AutosaveTick::Replaced;
    }
    match written {
        Ok(()) => {
            let at = unix_now();
            if s.autosaved(generation, at) {
                AutosaveTick::Written(at)
            } else {
                AutosaveTick::Replaced
            }
        }
        Err(e) => AutosaveTick::Failed(e),
    }
}
