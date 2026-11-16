//! Crash recovery.
//!
//! While the app runs, its recovery folder holds `session.lock` (locked by the running app, so
//! a second instance cannot take the folder) and, once something changed, `autosave.grooviq`
//! with `autosave.json` (the project's own path and when it was written). A clean exit removes
//! all three. If the next launch finds the lock file and an autosave, the last session ended
//! without a clean exit: the autosave is moved to `pending-<n>.grooviq`, its metadata to
//! `pending-<n>.json`, and offered back.
//!
//! A pending project stays on disk, and is offered on every launch (newest first), until the
//! person restores it (it becomes `recovered-<n>.grooviq`, of which the newest
//! [`MAX_BACKUPS`] are kept) or discards it (it is deleted). Quitting without choosing, or
//! crashing again, never loses it.

use crate::file::{SampleFiles, save};
use crate::model::{EXTENSION, Project};
use crate::session::MAX_BACKUPS;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK: &str = "session.lock";
const AUTOSAVE: &str = "autosave.grooviq";
const META: &str = "autosave.json";
/// Waiting for the person's choice.
const PENDING: &str = "pending-";
/// Restored: kept for a while as history.
const RECOVERED: &str = "recovered-";

/// The autosave's `autosave.json`, and a pending project's `pending-<n>.json`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    original: Option<PathBuf>,
    saved_at: u64,
}

/// A project an earlier session left unsaved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// The recovered project file (in the recovery folder).
    pub file: PathBuf,
    /// Where the project lived, or `None` if it was untitled.
    pub original: Option<PathBuf>,
    /// When the autosave was written, in unix seconds.
    pub saved_at: u64,
}

/// The running app's recovery folder. Dropping it without [`RecoveryDir::clean_exit`] (a crash)
/// leaves the lock file behind, so the next launch offers the autosave.
#[derive(Debug)]
pub struct RecoveryDir {
    root: PathBuf,
    /// Held, and locked, for the app's life.
    lock: File,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Removes `path`; it already being gone is fine.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Unix seconds of `path`'s last change, or 0.
fn modified(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn read_meta(path: &Path) -> Option<Meta> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes the session's process id into the lock file, for a person looking at the folder.
fn write_pid(mut lock: &File) -> io::Result<()> {
    lock.set_len(0)?;
    writeln!(lock, "{}", std::process::id())
}

impl RecoveryDir {
    /// Takes the recovery folder `root` (creating it) for this session, and returns every
    /// project earlier sessions left unsaved and nobody restored or discarded yet, newest
    /// first. Fails with `WouldBlock` if another running Grooviq holds the folder: its
    /// autosave is left alone.
    pub fn start(root: &Path) -> io::Result<(Self, Vec<Recovery>)> {
        Self::start_with(root, write_pid)
    }

    fn start_with(
        root: &Path,
        pid: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<(Self, Vec<Recovery>)> {
        fs::create_dir_all(root)?;
        let lock_path = root.join(LOCK);
        let leftover = lock_path.exists();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another Grooviq is running and keeps its autosave here",
                ));
            }
            Err(TryLockError::Error(e)) => return Err(e),
        }
        // Before anything moves, and never fatal: the OS lock guards the folder, the id only
        // informs. A full disk must not turn recovery off.
        let _ = pid(&lock);
        let dir = Self {
            root: root.to_path_buf(),
            lock,
        };

        let autosave = root.join(AUTOSAVE);
        if leftover && autosave.is_file() {
            dir.set_aside(&autosave)?;
        }
        // Whatever is left belongs to no one now.
        remove(&autosave)?;
        remove(&root.join(META))?;
        dir.prune();
        let pending = dir.pending();
        Ok((dir, pending))
    }

    /// Moves the autosave aside as a pending project, its metadata first.
    fn set_aside(&self, autosave: &Path) -> io::Result<()> {
        let meta = read_meta(&self.root.join(META)).unwrap_or_else(|| Meta {
            original: None,
            saved_at: modified(autosave),
        });
        // Numbers above every pending or recovered file so far keep the names in order.
        let newest = self
            .numbered(PENDING)
            .into_iter()
            .chain(self.numbered(RECOVERED))
            .map(|(n, _)| n)
            .max();
        let number = newest.map_or(unix_now(), |n| unix_now().max(n + 1));
        let file = self.root.join(format!("{PENDING}{number}.{EXTENSION}"));
        // Without its metadata the project is still offered (untitled, dated by the file),
        // so a failed write here is not worth losing it over.
        if let Ok(json) = serde_json::to_vec(&meta) {
            let _ = fs::write(file.with_extension("json"), json);
        }
        fs::rename(autosave, &file)
    }

    /// The files named `<prefix><number>.grooviq`, oldest first, as `(number, path)`.
    fn numbered(&self, prefix: &str) -> Vec<(u64, PathBuf)> {
        let suffix = format!(".{EXTENSION}");
        let mut found: Vec<(u64, PathBuf)> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let number = name
                    .strip_prefix(prefix)?
                    .strip_suffix(&suffix)?
                    .parse::<u64>()
                    .ok()?;
                Some((number, entry.path()))
            })
            .collect();
        found.sort();
        found
    }

    /// Keeps the newest [`MAX_BACKUPS`] restored files. Pending ones are never pruned.
    fn prune(&self) {
        let all = self.numbered(RECOVERED);
        for (_, old) in all.iter().take(all.len().saturating_sub(MAX_BACKUPS)) {
            // One that cannot be deleted is not worth failing the launch for.
            let _ = fs::remove_file(old);
        }
    }

    /// The pending projects, newest first. A metadata file whose project is gone (a crash
    /// between the two writes) is removed.
    fn pending(&self) -> Vec<Recovery> {
        let suffix = ".json";
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(PENDING)
                && name.ends_with(suffix)
                && !entry.path().with_extension(EXTENSION).exists()
            {
                let _ = fs::remove_file(entry.path());
            }
        }
        let mut found: Vec<Recovery> = self
            .numbered(PENDING)
            .into_iter()
            .map(|(_, file)| {
                let meta = read_meta(&file.with_extension("json"));
                Recovery {
                    original: meta.as_ref().and_then(|m| m.original.clone()),
                    saved_at: meta.map_or_else(|| modified(&file), |m| m.saved_at),
                    file,
                }
            })
            .collect();
        found.reverse();
        found
    }

    /// Checks that `recovery` is a pending project of this folder.
    fn own(&self, recovery: &Recovery) -> io::Result<()> {
        let name = recovery.file.file_name().map(|n| n.to_string_lossy());
        let pending = name.is_some_and(|n| n.starts_with(PENDING));
        if recovery.file.parent() != Some(self.root.as_path()) || !pending {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a recovered project of this folder",
            ));
        }
        Ok(())
    }

    /// Writes the autosave atomically (through `save`), then `autosave.json` with `original`
    /// and the time.
    pub fn write_autosave(
        &self,
        project: &Project,
        samples: &SampleFiles,
        original: Option<&Path>,
    ) -> io::Result<()> {
        save(project, samples, &self.root.join(AUTOSAVE))?;
        let meta = Meta {
            original: original.map(Path::to_path_buf),
            saved_at: unix_now(),
        };
        let json = serde_json::to_vec(&meta).map_err(io::Error::other)?;
        let tmp = self.root.join(format!("{META}.tmp"));
        fs::write(&tmp, json)?;
        fs::rename(&tmp, self.root.join(META))
    }

    /// Removes the autosave: the project was just saved, so there is nothing to recover.
    pub fn clear_autosave(&self) -> io::Result<()> {
        remove(&self.root.join(AUTOSAVE))?;
        remove(&self.root.join(META))
    }

    /// The person restored `recovery` (it is open now): it is no longer offered, and is kept
    /// as `recovered-<n>.grooviq` for a while.
    pub fn restored(&self, recovery: &Recovery) -> io::Result<()> {
        self.own(recovery)?;
        let name = recovery
            .file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let kept = name.replacen(PENDING, RECOVERED, 1);
        fs::rename(&recovery.file, self.root.join(kept))?;
        remove(&recovery.file.with_extension("json"))?;
        self.prune();
        Ok(())
    }

    /// Deletes a recovered project the person chose not to restore.
    pub fn discard(&self, recovery: Recovery) -> io::Result<()> {
        // Only ever a pending file in this folder.
        self.own(&recovery)?;
        remove(&recovery.file)?;
        remove(&recovery.file.with_extension("json"))
    }

    /// Ends the session cleanly: removes the autosave, then the lock, so the next launch
    /// offers nothing.
    pub fn clean_exit(self) -> io::Result<()> {
        self.clear_autosave()?;
        let Self { root, lock } = self;
        // Closed before it is removed: Windows refuses to delete an open file.
        drop(lock);
        remove(&root.join(LOCK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_pid_write_still_offers_the_recovery_and_keeps_autosave_on() {
        let dir = tempfile::tempdir().unwrap();
        let (session, _) = RecoveryDir::start(dir.path()).unwrap();
        session
            .write_autosave(&Project::new(), &SampleFiles::new(), None)
            .unwrap();
        drop(session);
        // The disk is full when the next launch writes its process id.
        let full = |_: &File| Err(io::Error::other("no space left on device"));
        let (session, found) = RecoveryDir::start_with(dir.path(), full).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].file.is_file());
        // And it is still there for the launch after, had this one not decided.
        drop(session);
        let (_session, again) = RecoveryDir::start_with(dir.path(), full).unwrap();
        assert_eq!(again, found);
    }
}
