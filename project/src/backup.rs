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
    if keep == 0 || !target.is_file() {
        return Ok(None);
    }
    let folder = backup_folder(target, backups)?;
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    fs::create_dir_all(&folder)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let found = existing(&folder, &stem)?;
    if let Some((_, newest)) = found.last()
        && same_bytes(target, newest)
    {
        return Ok(None);
    }
    let newest = found.last().map(|&(n, _)| n);
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
