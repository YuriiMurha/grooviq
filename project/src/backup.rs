//! Rolling backups: a copy of a project file just before it is overwritten.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The folder in `backups` that holds `target`'s backups: `<stem>-<8 hex>`, the hex being a
/// short stable hash of the folder `target` lives in, so two `song.grooviq` in different folders
/// never share (and never prune) each other's backups. Folders named after the stem alone, from
/// earlier versions, are not used and not touched.
pub fn backup_folder(target: &Path, backups: &Path) -> io::Result<PathBuf> {
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the project has no file name")
        })?;
    let parent = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // Resolved when the folder exists (`a/../a`, a symlink, `/tmp` for `/private/tmp`), so one
    // folder has one hash however it is spelled; else made absolute.
    let parent = fs::canonicalize(parent).or_else(|_| std::path::absolute(parent))?;
    Ok(backups.join(format!("{stem}-{:08x}", short_hash(&parent))))
}

/// 32 bits of FNV-1a over the path's bytes: stable across runs and versions (the standard
/// library's hashers promise neither).
fn short_hash(path: &Path) -> u32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // Fold the halves so every byte of the path reaches all 32 bits.
    ((hash >> 32) ^ hash) as u32
}

/// Copies an existing `target` to `<backups>/<stem>-<hash>/<stem>-<unix seconds>.grooviq` (see
/// [`backup_folder`]) and deletes the oldest backups of that project beyond `keep`. Returns the copy, or `None` when `target`
/// does not exist yet (nothing to back up), is byte for byte the newest backup already (saves
/// that change nothing must not rotate older versions out), or `keep` is 0. Other files in the
/// folder are left alone. Two backups in one second get consecutive numbers, and a new backup
/// always numbers above every existing one, so the order never breaks.
pub fn backup_before_save(
    target: &Path,
    backups: &Path,
    keep: usize,
) -> io::Result<Option<PathBuf>> {
    let prepared = prepare_backup(target, backups, keep)?;
    let copy = prepared.candidate.clone();
    prepared.confirm();
    Ok(copy)
}

/// Only this transaction's candidate may be rolled back. Retention uses the pre-save
/// snapshot, so a later save's candidate can never be selected for deletion.
pub(crate) struct PreparedBackup {
    candidate: Option<PathBuf>,
    prune: Vec<PathBuf>,
}

impl PreparedBackup {
    pub(crate) fn rollback(self) -> io::Result<()> {
        if let Some(path) = self.candidate {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub(crate) fn confirm(self) {
        // Publication has succeeded. Cleanup must not turn that into an untouched-target
        // error; retaining extra copies on failure is safer than failing the save.
        for path in self.prune {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn prepare_backup(
    target: &Path,
    backups: &Path,
    keep: usize,
) -> io::Result<PreparedBackup> {
    let mut prepared = PreparedBackup {
        candidate: None,
        prune: Vec::new(),
    };
    if keep == 0 || !target.is_file() {
        return Ok(prepared);
    }
    let folder = backup_folder(target, backups)?;
    let stem = target.file_stem().unwrap().to_string_lossy();
    fs::create_dir_all(&folder)?;
    let found = existing(&folder, &stem)?;
    let duplicate = found
        .last()
        .is_some_and(|(_, newest)| same_bytes(target, newest));
    if !duplicate {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut number = found
            .last()
            .map_or(now, |(n, _)| now.max(n.saturating_add(1)));
        loop {
            let copy = folder.join(format!("{stem}-{number}.{}", crate::EXTENSION));
            let partial = folder.join(format!("{stem}-{number}.partial"));
            // Exclusive creation and publication never overwrite another transaction or
            // an abandoned partial. A collision advances rather than claiming ownership.
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial);
            let mut file = match file {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    number = number
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("backup numbers exhausted"))?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let result = (|| {
                io::copy(&mut fs::File::open(target)?, &mut file)?;
                crate::file::sync_file(&file)?;
                fs::hard_link(&partial, &copy)
            })();
            drop(file);
            let cleanup = fs::remove_file(&partial);
            match result {
                Ok(()) => {
                    prepared.candidate = Some(copy);
                    // A failed partial cleanup retains extra data, not a failed save.
                    let _ = cleanup;
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    cleanup?;
                    number = number
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("backup numbers exhausted"))?;
                }
                Err(e) => return Err(e),
            }
        }
    }
    let excess = (found.len() + usize::from(prepared.candidate.is_some())).saturating_sub(keep);
    prepared.prune = found
        .into_iter()
        .take(excess)
        .map(|(_, path)| path)
        .collect();
    Ok(prepared)
}

/// Whether files `a` and `b` hold the same bytes (read in chunks; false if either cannot be
/// read, so a backup is made).
fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(fa), Ok(fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let same_len = match (fa.metadata(), fb.metadata()) {
        (Ok(ma), Ok(mb)) => ma.len() == mb.len(),
        _ => false,
    };
    if !same_len {
        return false;
    }
    let (mut ra, mut rb) = (io::BufReader::new(fa), io::BufReader::new(fb));
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let Ok(n) = read_full(&mut ra, &mut ba) else {
            return false;
        };
        let Ok(m) = read_full(&mut rb, &mut bb) else {
            return false;
        };
        if n != m || ba[..n] != bb[..m] {
            return false;
        }
        if n == 0 {
            return true;
        }
    }
}

/// Reads until `buf` is full or the file ends; the count read.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// The folder's backups of `stem`, oldest first, as `(number, path)`.
fn existing(folder: &Path, stem: &str) -> io::Result<Vec<(u64, PathBuf)>> {
    let prefix = format!("{stem}-");
    let suffix = format!(".{}", crate::EXTENSION);
    let mut found = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let number = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(&suffix))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u64>().ok());
        if let Some(n) = number {
            found.push((n, entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_rollback_does_not_own_the_existing_copy() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song.grooviq");
        let backups = dir.path().join("backups");
        fs::write(&target, b"old bytes").unwrap();
        let copy = backup_before_save(&target, &backups, 1).unwrap().unwrap();
        prepare_backup(&target, &backups, 1)
            .unwrap()
            .rollback()
            .unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"old bytes");
        assert_eq!(fs::read_dir(copy.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn cleanup_failure_retains_history_and_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        fs::write(&old, b"history").unwrap();
        let candidate = dir.path().join("candidate");
        fs::create_dir(&candidate).unwrap();
        let prepared = PreparedBackup {
            candidate: Some(candidate.clone()),
            prune: vec![old.clone()],
        };
        assert!(prepared.rollback().is_err());
        assert_eq!(fs::read(&old).unwrap(), b"history");
        assert!(candidate.is_dir());
        // A pruning failure after publication is harmless and retains the obstructed copy.
        PreparedBackup {
            candidate: None,
            prune: vec![candidate.clone()],
        }
        .confirm();
        assert!(candidate.is_dir());
    }

    #[test]
    fn abandoned_partial_and_existing_candidate_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song.grooviq");
        let backups = dir.path().join("backups");
        fs::write(&target, b"new old bytes").unwrap();
        let folder = backup_folder(&target, &backups).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let existing = folder.join(format!("song-{}.grooviq", u64::MAX - 1));
        let partial = folder.join(format!("song-{}.partial", u64::MAX));
        fs::write(&existing, b"other save").unwrap();
        fs::write(&partial, b"abandoned").unwrap();
        assert!(prepare_backup(&target, &backups, 1).is_err());
        assert_eq!(fs::read(existing).unwrap(), b"other save");
        assert_eq!(fs::read(partial).unwrap(), b"abandoned");
    }
}
