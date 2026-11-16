//! Temporary files of writes that may be cut short: the zip of a project save (`.tmp`) and the
//! WAV of an export (`.part`), beside the file they are becoming.
//!
//! Every such file has a name of Grooviq's own, `.<target file name>.grooviq-<pid>-<16 hex>.<tmp
//! or part>`, is created new (so it is never anything that was there) and is held under an
//! exclusive OS lock by its writer until the rename or the error cleanup. That makes cleanup
//! provable: it touches only files of this exact naming for the same target, at least
//! [`STALE_AFTER`] old, whose lock it can take. A writer that is alive (a second copy, a stalled
//! one) still holds its lock, and a forward clock jump does not change that; a writer that died
//! has had its lock released by the OS. A file named `<target>.part`, or anything else, is
//! never touched.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A leftover younger than this is never removed, whoever it belongs to.
pub const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// Hex digits of the random part of an owned name.
const RANDOM_HEX: usize = 16;

/// `<target><suffix>`: the target's whole file name with `suffix` added, in the same folder.
pub fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(target.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// A new temp file of this app's, locked by its writer. Keep `file` (never a copy of its path
/// alone) until the file is renamed or removed: dropping it releases the lock.
#[derive(Debug)]
pub struct OwnedTemp {
    pub file: File,
    pub path: PathBuf,
}

/// Where `target`'s owned temp files go, and how they are named: `(folder, name prefix)`.
fn namespace(target: &Path) -> io::Result<(PathBuf, OsString)> {
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the file has no name"))?;
    let folder = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let mut prefix = OsString::from(".");
    prefix.push(name);
    prefix.push(".grooviq-");
    Ok((folder, prefix))
}

/// Creates `.<target name>.grooviq-<pid>-<random>.<suffix>` beside `target` (`suffix` is `.tmp`
/// or `.part`), failing if the name exists, and locks it exclusively.
pub fn create_owned_temp(target: &Path, suffix: &str) -> io::Result<OwnedTemp> {
    let (folder, prefix) = namespace(target)?;
    let mut random = [0u8; RANDOM_HEX / 2];
    getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
    let mut name = prefix;
    name.push(format!("{}-", std::process::id()));
    for byte in random {
        name.push(format!("{byte:02x}"));
    }
    name.push(suffix);
    let path = folder.join(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    // Nobody else can hold a lock on a file created a moment ago under a random name.
    file.try_lock().map_err(|e| match e {
        TryLockError::Error(e) => e,
        TryLockError::WouldBlock => io::Error::other("a new temp file was locked by another"),
    })?;
    Ok(OwnedTemp { file, path })
}

/// Whether `name` is `<prefix><pid digits>-<16 lowercase hex><suffix>`.
fn is_owned_name(name: &OsStr, prefix: &OsStr, suffix: &str) -> bool {
    let (name, prefix) = (name.as_encoded_bytes(), prefix.as_encoded_bytes());
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(suffix.as_bytes()) else {
        return false;
    };
    let Some(dash) = rest.iter().position(|&b| b == b'-') else {
        return false;
    };
    let (pid, random) = (&rest[..dash], &rest[dash + 1..]);
    !pid.is_empty()
        && pid.iter().all(u8::is_ascii_digit)
        && random.len() == RANDOM_HEX
        && random
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// Removes `target`'s owned temp files (see the module docs) that are at least [`STALE_AFTER`]
/// old and have no live writer. Returns how many it removed. Errors on one file do not stop
/// the others; the first is returned after the sweep.
pub fn remove_stale_owned(target: &Path, suffix: &str) -> io::Result<usize> {
    remove_stale_owned_at(target, suffix, SystemTime::now())
}

fn remove_stale_owned_at(target: &Path, suffix: &str, now: SystemTime) -> io::Result<usize> {
    let (folder, prefix) = namespace(target)?;
    let mut removed = 0;
    let mut first_error = None;
    for entry in fs::read_dir(&folder)? {
        let entry = entry?;
        if !is_owned_name(&entry.file_name(), &prefix, suffix) {
            continue;
        }
        match remove_if_dead(&entry.path(), now) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    first_error.map_or(Ok(removed), Err)
}

/// Removes the owned file at `path` if it is a regular file, old enough, and nobody holds its
/// lock. The lock is taken first and kept while the file goes, so a writer cannot start using
/// it in between (and none can, since names are unique).
fn remove_if_dead(path: &Path, now: SystemTime) -> io::Result<bool> {
    // Not `metadata`: that follows a link, and a link is never ours.
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_file() {
        return Ok(false);
    }
    // A clock set back makes the age unknown (an error): the file is treated as fresh.
    let age = now.duration_since(meta.modified()?).unwrap_or_default();
    if age < STALE_AFTER {
        return Ok(false);
    }
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    match file.try_lock() {
        Ok(()) => {}
        // A live writer.
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    // What was inspected must be what is opened and what is removed: a file swapped in at that
    // name since (impossible for a unique name, but cheap to check) is left alone.
    if !same_file(&file, path)? {
        return Ok(false);
    }
    // The lock is still held here; it goes when `file` drops. On Windows the handle was opened
    // with delete sharing, so the name can be removed while it is open.
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether the open `file` is the file now at `path` (not a link).
#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let open = file.metadata()?;
    match fs::symlink_metadata(path) {
        Ok(now) => {
            Ok(now.file_type().is_file() && (now.dev(), now.ino()) == (open.dev(), open.ino()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Windows names the open handle's file by the handle, and removes it by that name while the
/// handle (opened with delete sharing) is held: no gap to check.
#[cfg(not(unix))]
fn same_file(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(path: &Path, age: Duration) {
        // A folder cannot be opened for writing; a read handle sets its time on Unix.
        fs::File::options()
            .write(true)
            .open(path)
            .or_else(|_| fs::File::open(path))
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    const OLD: Duration = Duration::from_secs(11 * 60);

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn an_owned_temp_has_a_unique_hidden_name_in_the_targets_folder() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mix.wav");
        let a = create_owned_temp(&target, ".part").unwrap();
        let b = create_owned_temp(&target, ".part").unwrap();
        assert_ne!(a.path, b.path);
        for temp in [&a, &b] {
            assert_eq!(temp.path.parent(), Some(dir.path()));
            let name = temp
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let rest = name.strip_prefix(".mix.wav.grooviq-").expect(&name);
            let rest = rest.strip_suffix(".part").expect(&name);
            let (pid, random) = rest.split_once('-').expect(&name);
            assert_eq!(pid, std::process::id().to_string());
            assert!(random.len() == 16 && random.bytes().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn an_owned_temp_is_locked_for_as_long_as_its_writer_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let temp = create_owned_temp(&dir.path().join("a.grooviq"), ".tmp").unwrap();
        let other = fs::File::open(&temp.path).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(temp.file);
        assert!(other.try_lock().is_ok(), "the lock goes with the writer");
    }

    #[test]
    fn a_dead_writers_old_leftover_goes_and_a_live_ones_stays() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.grooviq");
        let dead = create_owned_temp(&target, ".tmp").unwrap();
        let live = create_owned_temp(&target, ".tmp").unwrap();
        let fresh = create_owned_temp(&target, ".tmp").unwrap();
        aged(&dead.path, OLD);
        aged(&live.path, OLD);
        let dead_path = dead.path.clone();
        drop(dead.file); // the writer died: the OS released its lock

        assert_eq!(remove_stale_owned(&target, ".tmp").unwrap(), 1);
        assert!(!dead_path.exists(), "dead and old: removed");
        assert!(live.path.exists(), "the writer still holds it: kept");
        assert!(fresh.path.exists(), "not old enough: kept");
        // And the live writer can still finish: its handle is intact.
        use std::io::Write;
        (&live.file).write_all(b"still writing").unwrap();
        assert_eq!(live.file.metadata().unwrap().len(), 13);
    }

    #[test]
    fn nothing_that_is_not_ours_is_ever_removed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mix.wav");
        let theirs = [
            "mix.wav.part", // the plain name
            "mix.wav.tmp",
            ".mix.wav.grooviq-1-0123456789abcdef.tmp", // wrong suffix for export
            ".mix.wav.grooviq-x-0123456789abcdef.part", // pid not a number
            ".mix.wav.grooviq-1-0123456789abcde.part", // random too short
            ".mix.wav.grooviq-1-0123456789abcdeg.part", // not hex
            ".other.wav.grooviq-1-0123456789abcdef.part", // another target
            ".mix.wav.grooviq-1-0123456789abcdef.part.bak",
            "notes.part",
        ];
        for name in theirs {
            let p = dir.path().join(name);
            fs::write(&p, b"mine").unwrap();
            aged(&p, Duration::from_secs(86_400));
        }
        let before = names(dir.path());
        assert_eq!(remove_stale_owned(&target, ".part").unwrap(), 0);
        assert_eq!(names(dir.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn an_owned_looking_symlink_or_folder_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mix.wav");
        let precious = dir.path().join("precious.txt");
        fs::write(&precious, b"precious").unwrap();
        aged(&precious, OLD);
        let link = dir.path().join(".mix.wav.grooviq-1-0123456789abcdef.part");
        std::os::unix::fs::symlink(&precious, &link).unwrap();
        let folder = dir.path().join(".mix.wav.grooviq-2-0123456789abcdef.part");
        fs::create_dir(&folder).unwrap();
        aged(&folder, OLD);
        assert_eq!(remove_stale_owned(&target, ".part").unwrap(), 0);
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(folder.is_dir());
        assert_eq!(fs::read(&precious).unwrap(), b"precious");
    }

    #[test]
    fn the_age_limit_is_ten_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.grooviq");
        let temp = create_owned_temp(&target, ".tmp").unwrap();
        let path = temp.path.clone();
        drop(temp.file);
        aged(&path, STALE_AFTER - Duration::from_secs(5));
        assert_eq!(remove_stale_owned(&target, ".tmp").unwrap(), 0);
        aged(&path, STALE_AFTER + Duration::from_secs(5));
        assert_eq!(remove_stale_owned(&target, ".tmp").unwrap(), 1);
    }

    #[test]
    fn a_file_dated_in_the_future_counts_as_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.grooviq");
        let temp = create_owned_temp(&target, ".tmp").unwrap();
        let path = temp.path.clone();
        drop(temp.file);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        assert_eq!(remove_stale_owned(&target, ".tmp").unwrap(), 0);
        assert!(path.exists());
    }
}
