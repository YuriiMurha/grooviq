//! Leftovers of a write that was cut short: `<target>.tmp` (a project save) and
//! `<target>.part` (a WAV export) beside the file they were becoming.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A leftover this old is from a write that died; a younger one may be a write still going
/// (a second Grooviq, a slow disk) and is never touched.
pub const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// `<target><suffix>`: the target's whole file name with `suffix` added, in the same folder.
/// Every leftover the app writes is named this way.
pub fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(target.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Removes `<target><suffix>` if it is a stale leftover: a regular file (not a link, a folder
/// or a device) last changed more than [`STALE_AFTER`] ago. Anything else at that name stays.
/// Returns whether it removed one. The name is built here from `target`, so only a file in
/// the target's own folder with our naming can ever be removed.
pub fn remove_stale_sibling(target: &Path, suffix: &str) -> io::Result<bool> {
    remove_stale_sibling_at(target, suffix, SystemTime::now())
}

fn remove_stale_sibling_at(target: &Path, suffix: &str, now: SystemTime) -> io::Result<bool> {
    let leftover = sibling(target, suffix);
    // Not `metadata`: that follows a link and would judge the file behind it.
    let meta = match fs::symlink_metadata(&leftover) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_file() {
        return Ok(false);
    }
    // A clock set back makes the age unknown (an error): the file is treated as fresh.
    let age = now.duration_since(meta.modified()?).unwrap_or_default();
    if age <= STALE_AFTER {
        return Ok(false);
    }
    match fs::remove_file(&leftover) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
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

    #[test]
    fn the_name_is_the_whole_file_name_plus_the_suffix() {
        assert_eq!(
            sibling(Path::new("out/beat.grooviq"), ".tmp"),
            Path::new("out/beat.grooviq.tmp")
        );
    }

    #[test]
    fn only_a_file_older_than_ten_minutes_goes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song.grooviq");
        let tmp = sibling(&target, ".tmp");
        assert!(!remove_stale_sibling(&target, ".tmp").unwrap(), "none yet");

        fs::write(&tmp, b"x").unwrap();
        assert!(!remove_stale_sibling(&target, ".tmp").unwrap(), "fresh");
        aged(&tmp, STALE_AFTER - Duration::from_secs(5));
        assert!(!remove_stale_sibling(&target, ".tmp").unwrap(), "9m55s");
        aged(&tmp, STALE_AFTER + Duration::from_secs(5));
        assert!(remove_stale_sibling(&target, ".tmp").unwrap(), "10m05s");
        assert!(!tmp.exists());
    }

    #[test]
    fn a_file_dated_in_the_future_counts_as_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song.grooviq");
        let tmp = sibling(&target, ".tmp");
        fs::write(&tmp, b"x").unwrap();
        let later = SystemTime::now() + Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&tmp)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!remove_stale_sibling(&target, ".tmp").unwrap());
        assert!(tmp.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_at_the_leftover_name_is_not_a_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song.grooviq");
        let tmp = sibling(&target, ".tmp");
        fs::create_dir(&tmp).unwrap();
        aged(&tmp, Duration::from_secs(3600));
        // Not an error and not removed: it is simply none of ours.
        assert!(!remove_stale_sibling(&target, ".tmp").unwrap());
        assert!(tmp.is_dir());
    }

    #[test]
    fn the_target_itself_and_other_names_are_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("song.grooviq");
        let other = dir.path().join("other.grooviq.tmp");
        fs::write(&target, b"the project").unwrap();
        fs::write(&other, b"someone else's").unwrap();
        aged(&target, Duration::from_secs(86_400));
        aged(&other, Duration::from_secs(86_400));
        assert!(!remove_stale_sibling(&target, ".tmp").unwrap());
        assert!(target.exists() && other.exists());
    }
}
