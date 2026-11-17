//! Crash recovery.
//!
//! While the app runs, its recovery folder holds `session.lock` (locked by the running app, so
//! a second instance cannot take the folder) and, once something changed, an autosave
//! `autosave-<n>.grooviq` with `autosave.json` (the project's own path, when it was written, and
//! which autosave file it describes). A clean exit removes all of it. If the next launch finds
//! the lock file and an autosave, the last session ended without a clean exit: the autosave is
//! moved to `pending-<n>.grooviq`, its metadata to `pending-<n>.json`, and offered back.
//!
//! The autosave and its metadata are one pair, and the pair changes in one step: each autosave
//! is written to a new file name, and the rename of `autosave.json` (which names that file) is
//! the commit. A crash before it leaves the old pair whole, plus the new snapshot's file if it
//! was complete; a crash after it leaves the new pair whole. A path is never paired with another
//! project's bytes, and a complete newer snapshot is never thrown away: the next launch offers
//! the old pair with its path and the newer snapshot untitled, newest first.
//!
//! A pending project stays on disk, and is offered on every launch (newest first), until the
//! person restores it (it becomes `recovered-<n>.grooviq`, of which the newest
//! [`MAX_BACKUPS`] are kept) or discards it (it is deleted). Quitting without choosing, or
//! crashing again, never loses it.

use crate::file::{
    Disk, RealDisk, SampleFiles, confirm_directory, directory_unconfirmed, stage_save,
};
use crate::model::{EXTENSION, Project};
use crate::session::MAX_BACKUPS;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK: &str = "session.lock";
/// An autosave is `autosave-<n>.grooviq`, a new number each time.
const AUTOSAVE: &str = "autosave-";
/// The one name earlier versions used: still recognised, never written.
const LEGACY_AUTOSAVE: &str = "autosave.grooviq";
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
    /// The autosave file this describes (a name in the folder); absent in files written by
    /// earlier versions, which meant [`LEGACY_AUTOSAVE`], and in a pending project's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
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
    /// The number of the newest autosave file this session wrote.
    written: AtomicU64,
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

/// `autosave-<n>.grooviq` (or the legacy `autosave.grooviq`): a name this module wrote.
fn is_autosave_name(name: &str) -> bool {
    name == LEGACY_AUTOSAVE || is_numbered(name, AUTOSAVE, EXTENSION)
}

/// `<prefix><digits>.<extension>`.
fn is_numbered(name: &str, prefix: &str, extension: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(extension))
        .and_then(|rest| rest.strip_suffix('.'))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Writes `bytes` to `path` so that `path` is always either what it was or all of `bytes`:
/// a temp file beside it, flushed and synced, renamed over it, then the folder synced.
fn write_atomic(path: &Path, bytes: &[u8], disk: &mut impl Disk) -> io::Result<()> {
    let tmp = grooviq_engine::sibling(path, ".tmp");
    let written = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        disk.sync(&file)?;
        drop(file);
        disk.rename(&tmp, path)
    })();
    match written {
        Ok(()) => confirm_directory(disk, path),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
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
            written: AtomicU64::new(0),
        };

        if leftover {
            // Oldest first: the pending numbers grow in the order they are set aside, so the
            // newer work stays the newer one even if this launch dies half way.
            for (autosave, meta) in dir.recoverable() {
                dir.set_aside(&autosave, meta)?;
            }
        }
        // Whatever is left (an unfinished write, a metadata file without its autosave) belongs
        // to no one now.
        dir.remove_autosave_files(None)?;
        remove(&root.join(META))?;
        dir.remove_unfinished_writes();
        dir.prune();
        let pending = dir.pending();
        Ok((dir, pending))
    }

    /// What a crash left to offer, oldest first, each with its metadata if it has any:
    ///
    /// * the autosave `autosave.json` names, if both are there: the last committed pair, with
    ///   its original path;
    /// * the newest autosave file newer than that (a write that finished its file and died
    ///   before its metadata), untitled: the work is kept, and no path is claimed for it, since
    ///   the metadata that is there describes another snapshot. With no committed pair at all
    ///   (a crash before the very first metadata, or a metadata left behind), the newest
    ///   autosave file alone, likewise untitled.
    ///
    /// Older leftovers (a removal that failed after a commit) are superseded copies: not offered.
    fn recoverable(&self) -> Vec<(PathBuf, Option<Meta>)> {
        let files = self.numbered(AUTOSAVE);
        let legacy = self.root.join(LEGACY_AUTOSAVE);
        // The committed pair, as `(number, file, metadata)`; the legacy name counts as number 0.
        let committed = read_meta(&self.root.join(META)).and_then(|meta| {
            let name = meta
                .file
                .clone()
                .unwrap_or_else(|| LEGACY_AUTOSAVE.to_owned());
            // Only ever a file of ours in this folder, whatever the metadata says.
            if !is_autosave_name(&name) || !self.root.join(&name).is_file() {
                return None;
            }
            let number = files
                .iter()
                .find(|(_, path)| path.file_name().is_some_and(|n| n == name.as_str()))
                .map_or(0, |(n, _)| *n);
            Some((number, self.root.join(&name), meta))
        });
        let floor = committed.as_ref().map(|(n, ..)| *n);
        let newer = files
            .into_iter()
            .rfind(|(n, _)| floor.is_none_or(|floor| *n > floor))
            .map(|(_, path)| path)
            // Only an earlier version's file, and nothing numbered or committed.
            .or_else(|| (committed.is_none() && legacy.is_file()).then_some(legacy));
        let mut out = Vec::new();
        if let Some((_, path, meta)) = committed {
            out.push((path, Some(meta)));
        }
        if let Some(path) = newer {
            out.push((path, None));
        }
        out
    }

    /// Removes every autosave file but `keep` (a file name). Never the metadata.
    fn remove_autosave_files(&self, keep: Option<&str>) -> io::Result<()> {
        for (_, path) in self.numbered(AUTOSAVE) {
            if keep != path.file_name().and_then(|n| n.to_str()) {
                remove(&path)?;
            }
        }
        if keep != Some(LEGACY_AUTOSAVE) {
            remove(&self.root.join(LEGACY_AUTOSAVE))?;
        }
        Ok(())
    }

    /// Removes the temp files of writes that died (`autosave.json.tmp`,
    /// `autosave-<n>.grooviq.tmp`, `pending-<n>.json.tmp`). Only at the start, while this
    /// session alone holds the folder, so no write of ours is under way.
    fn remove_unfinished_writes(&self) {
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_ours = name.strip_suffix(".tmp").is_some_and(|base| {
                base == META
                    || base == LEGACY_AUTOSAVE
                    || is_autosave_name(base)
                    || is_numbered(base, PENDING, "json")
            });
            if is_ours && entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// Moves the autosave aside as a pending project, its metadata first.
    fn set_aside(&self, autosave: &Path, meta: Option<Meta>) -> io::Result<()> {
        let meta = meta.map_or_else(
            || Meta {
                original: None,
                saved_at: modified(autosave),
                file: None,
            },
            |meta| Meta { file: None, ..meta },
        );
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
            let _ = write_atomic(&file.with_extension("json"), &json, &mut RealDisk);
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

    /// Writes an autosave (atomically, through `save`, to a new file name), then commits it by
    /// atomically replacing `autosave.json` with `original`, the time and that file's name. See
    /// the module docs: at every moment of a crash the folder holds a whole pair. On an error
    /// before metadata rename the new file is removed and the old pair stays. A
    /// post-rename durability warning retains both files, including the referenced snapshot.
    pub fn write_autosave(
        &self,
        project: &Project,
        samples: &SampleFiles,
        original: Option<&Path>,
    ) -> io::Result<()> {
        self.write_autosave_with(project, samples, original, || Ok(()))
    }

    /// [`Self::write_autosave`] with `between` run after the autosave file is in place and
    /// before it is committed: the test hook for "a crash between the two writes". Not part of
    /// the API.
    #[doc(hidden)]
    pub fn write_autosave_with(
        &self,
        project: &Project,
        samples: &SampleFiles,
        original: Option<&Path>,
        between: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.write_autosave_on(project, samples, original, between, &mut RealDisk)
    }

    fn write_autosave_on(
        &self,
        project: &Project,
        samples: &SampleFiles,
        original: Option<&Path>,
        between: impl FnOnce() -> io::Result<()>,
        disk: &mut impl Disk,
    ) -> io::Result<()> {
        let number = self.written.fetch_add(1, Ordering::AcqRel) + 1;
        let name = format!("{AUTOSAVE}{number}.{EXTENSION}");
        let file = self.root.join(&name);
        stage_save(project, samples, &file, disk)?.commit(disk)?;
        let committed = between().and_then(|()| {
            let meta = Meta {
                original: original.map(Path::to_path_buf),
                saved_at: unix_now(),
                file: Some(name.clone()),
            };
            let json = serde_json::to_vec(&meta).map_err(io::Error::other)?;
            write_atomic(&self.root.join(META), &json, disk)
        });
        if let Err(e) = committed {
            // Metadata may already name this file. Retain both snapshots until a
            // later confirmed commit makes cleanup safe.
            if !directory_unconfirmed(&e) {
                let _ = fs::remove_file(&file);
            }
            return Err(e);
        }
        // The old autosave is only an older copy now; one that cannot be removed is taken
        // care of at the next launch, or the next autosave.
        let _ = self.remove_autosave_files(Some(&name));
        Ok(())
    }

    /// Removes the autosave: the project was just saved, so there is nothing to recover.
    pub fn clear_autosave(&self) -> io::Result<()> {
        // The files first: a crash half way leaves a metadata file naming nothing, which offers
        // nothing, rather than an autosave to offer.
        self.remove_autosave_files(None)?;
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
        let Self { root, lock, .. } = self;
        // Closed before it is removed: Windows refuses to delete an open file.
        drop(lock);
        remove(&root.join(LOCK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_at_snapshot_and_metadata_boundaries_keep_a_valid_pair() {
        for boundary in 1..=6 {
            struct FailAt {
                count: usize,
                boundary: usize,
            }
            impl Disk for FailAt {
                fn sync(&mut self, file: &File) -> io::Result<()> {
                    self.count += 1;
                    if self.count == self.boundary {
                        return Err(io::Error::other("sync refused"));
                    }
                    file.sync_all()
                }
                fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
                    self.count += 1;
                    if self.count == self.boundary {
                        return Err(io::Error::other("rename refused"));
                    }
                    fs::rename(from, to)
                }
                fn sync_folder(&mut self, _: &Path) -> io::Result<()> {
                    self.count += 1;
                    if self.count == self.boundary {
                        return Err(io::Error::other("directory refused"));
                    }
                    Ok(())
                }
            }
            let root = tempfile::tempdir().unwrap();
            let (dir, _) = RecoveryDir::start(root.path()).unwrap();
            dir.write_autosave(&Project::new(), &SampleFiles::new(), None)
                .unwrap();
            let old = fs::read(root.path().join(META)).unwrap();
            let result = dir.write_autosave_on(
                &Project::new(),
                &SampleFiles::new(),
                None,
                || Ok(()),
                &mut FailAt { count: 0, boundary },
            );
            let error = result.unwrap_err();
            assert_eq!(
                directory_unconfirmed(&error),
                boundary == 3 || boundary == 6
            );
            let meta = read_meta(&root.path().join(META)).unwrap();
            assert!(root.path().join(meta.file.unwrap()).is_file());
            if boundary < 6 {
                assert_eq!(fs::read(root.path().join(META)).unwrap(), old);
            }
            dir.write_autosave(&Project::new(), &SampleFiles::new(), None)
                .unwrap();
            assert_eq!(dir.numbered(AUTOSAVE).len(), 1);
        }
    }

    #[test]
    fn metadata_directory_warning_retains_both_snapshots_until_retry() {
        struct RefuseMetadata;
        impl crate::file::Disk for RefuseMetadata {
            fn sync_folder(&mut self, path: &Path) -> io::Result<()> {
                if path.ends_with(META) {
                    Err(io::Error::other("folder sync refused"))
                } else {
                    Ok(())
                }
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (dir, _) = RecoveryDir::start(root.path()).unwrap();
        dir.write_autosave(&Project::new(), &SampleFiles::new(), None)
            .unwrap();
        let mut newer = Project::new();
        newer.bpm = 137.0;
        let error = dir
            .write_autosave_on(
                &newer,
                &SampleFiles::new(),
                Some(Path::new("new.grooviq")),
                || Ok(()),
                &mut RefuseMetadata,
            )
            .unwrap_err();
        assert!(crate::file::directory_unconfirmed(&error));
        let meta = read_meta(&root.path().join(META)).unwrap();
        let referenced = root.path().join(meta.file.unwrap());
        assert_eq!(crate::file::load(&referenced).unwrap().project.bpm, 137.0);
        assert_eq!(meta.original, Some(PathBuf::from("new.grooviq")));
        assert_eq!(dir.numbered(AUTOSAVE).len(), 2);
        dir.write_autosave(&Project::new(), &SampleFiles::new(), None)
            .unwrap();
        assert_eq!(dir.numbered(AUTOSAVE).len(), 1);
    }

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
