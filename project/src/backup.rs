//! Rolling backups: a copy of a project file just before it is overwritten.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Copies an existing `target` to `<backups>/<stem>/<stem>-<unix seconds>.grooviq` and deletes
/// the oldest backups of that project beyond `keep`. Returns the copy, or `None` when `target`
/// does not exist yet (nothing to back up) or `keep` is 0. Other files in the folder are left
/// alone. Two backups in one second get consecutive numbers, and a new backup always numbers
/// above every existing one, so the order never breaks.
pub fn backup_before_save(
    target: &Path,
    backups: &Path,
    keep: usize,
) -> io::Result<Option<PathBuf>> {
    if keep == 0 || !target.is_file() {
        return Ok(None);
    }
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the project has no file name")
        })?;
    let folder = backups.join(&stem);
    fs::create_dir_all(&folder)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let newest = existing(&folder, &stem)?.last().map(|&(n, _)| n);
    let number = newest.map_or(now, |n| now.max(n.saturating_add(1)));

    let copy = folder.join(format!("{stem}-{number}.{}", crate::EXTENSION));
    // Copy beside the final name, then rename: a crash leaves no half-written backup that
    // would count as one.
    let partial = folder.join(format!("{stem}-{number}.partial"));
    let copied = fs::copy(target, &partial).and_then(|_| fs::rename(&partial, &copy));
    if let Err(e) = copied {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }

    let all = existing(&folder, &stem)?;
    for (_, old) in all.iter().take(all.len().saturating_sub(keep)) {
        // A backup that cannot be deleted is not worth failing the save for.
        let _ = fs::remove_file(old);
    }
    Ok(Some(copy))
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
