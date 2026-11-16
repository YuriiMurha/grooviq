//! Crash recovery.
//!
//! While the app runs, its recovery folder holds `session.lock` (locked by the running app, so
//! a second instance cannot take the folder) and, once something changed, `autosave.grooviq`
//! with `autosave.json` (the project's own path and when it was written). A clean exit removes
//! all three. If the next launch finds the lock file and an autosave, the last session ended
//! without a clean exit: the autosave is moved to `recovered-<unix seconds>.grooviq` and
//! offered back.

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
const RECOVERED: &str = "recovered-";

/// The autosave's `autosave.json`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    original: Option<PathBuf>,
    saved_at: u64,
}

/// A project the previous session left unsaved.
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

impl RecoveryDir {
    /// Takes the recovery folder `root` (creating it) for this session, and returns the previous
    /// session's unsaved project if it ended without a clean exit. Fails with `WouldBlock` if
    /// another running Grooviq holds the folder: its autosave is left alone.
    pub fn start(root: &Path) -> io::Result<(Self, Option<Recovery>)> {
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
        let dir = Self {
            root: root.to_path_buf(),
            lock,
        };

        let autosave = root.join(AUTOSAVE);
        let recovery = if leftover && autosave.is_file() {
            Some(dir.recover(&autosave)?)
        } else {
            None
        };
        // Whatever is left belongs to no one now.
        remove(&autosave)?;
        remove(&root.join(META))?;
        dir.prune();

        let mut lock = &dir.lock;
        lock.set_len(0)?;
        writeln!(lock, "{}", std::process::id())?;
        Ok((dir, recovery))
    }

    /// Moves the autosave aside under a fresh name and reads its metadata.
    fn recover(&self, autosave: &Path) -> io::Result<Recovery> {
        let meta = fs::read(self.root.join(META))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Meta>(&bytes).ok());
        let saved_at = meta.as_ref().map_or_else(
            || {
                fs::metadata(autosave)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            },
            |m| m.saved_at,
        );
        // Numbers above every recovered file so far keep the names in order.
        let newest = self.recovered().last().map(|&(n, _)| n);
        let number = newest.map_or(unix_now(), |n| unix_now().max(n + 1));
        let file = self.root.join(format!("{RECOVERED}{number}.{EXTENSION}"));
        fs::rename(autosave, &file)?;
        Ok(Recovery {
            file,
            original: meta.and_then(|m| m.original),
            saved_at,
        })
    }

    /// The recovered files, oldest first, as `(number, path)`.
    fn recovered(&self) -> Vec<(u64, PathBuf)> {
        let suffix = format!(".{EXTENSION}");
        let mut found: Vec<(u64, PathBuf)> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let number = name
                    .strip_prefix(RECOVERED)?
                    .strip_suffix(&suffix)?
                    .parse::<u64>()
                    .ok()?;
                Some((number, entry.path()))
            })
            .collect();
        found.sort();
        found
    }

    /// Keeps the newest [`MAX_BACKUPS`] recovered files.
    fn prune(&self) {
        let all = self.recovered();
        for (_, old) in all.iter().take(all.len().saturating_sub(MAX_BACKUPS)) {
            // One that cannot be deleted is not worth failing the launch for.
            let _ = fs::remove_file(old);
        }
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

    /// Deletes a recovered project the person chose not to restore.
    pub fn discard(&self, recovery: Recovery) -> io::Result<()> {
        // Only ever a file in this folder.
        if recovery.file.parent() != Some(self.root.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a recovered project of this folder",
            ));
        }
        remove(&recovery.file)
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
