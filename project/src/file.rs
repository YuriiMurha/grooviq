//! The `.grooviq` file: a zip holding a versioned `project.json` and every sample's original
//! bytes under `samples/`, so a project opens on another machine, or after the originals are
//! deleted.
//!
//! * Saving is atomic: the zip is written to `<path>.tmp`, synced, then renamed over the target,
//!   so failures before rename leave the previous file untouched. A directory-sync failure
//!   after rename reports [`DirectoryUnconfirmed`]: the new file is already visible.
//! * `save` writes only what `load` reads back: `project.json` is compact and held to the same
//!   size budget, so a project too big to reopen is refused instead of saved.
//! * The bytes are a function of the project and the samples: entry order, names and timestamps
//!   are fixed, so saving twice gives the same file.
//! * Loading never trusts the file: sizes are capped, and a missing sample is a warning instead of
//!   a failed project. `load` never opens a path the file names; relinking is the user's choice.

use crate::model::{FORMAT, MAX_NOTES_PER_PATTERN, Project, VERSION};
use crate::reading::NoteDrops;
use crate::{outline, reading};
use grooviq_engine::InstrumentKind;
use serde::Serialize;
use serde::de::value::{MapAccessDeserializer, StrDeserializer};
use serde::de::{Deserialize, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

/// The zip entry that holds the project.
const PROJECT_JSON: &str = "project.json";
/// The folder of embedded samples.
const SAMPLES_DIR: &str = "samples/";
/// The most `project.json` may hold (a project of 256 channels is a few hundred KB).
const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;
/// The most one sample file may hold, read from the zip or from a file the user picked.
const MAX_SAMPLE_BYTES: u64 = 512 * 1024 * 1024;
/// The most sample bytes one `load` reads in total, whatever the number of channels.
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The size caps `save` and `load` both enforce, so a project one accepts the other opens.
/// Injectable (crate-private) so tests use tiny limits instead of 512 MB files.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    /// The most one sample may hold.
    pub(crate) sample: u64,
    /// The most all distinct samples of a project may hold together.
    pub(crate) total: u64,
    /// The most `project.json` may hold.
    pub(crate) json: u64,
}

const LIMITS: Limits = Limits {
    sample: MAX_SAMPLE_BYTES,
    total: MAX_TOTAL_BYTES,
    json: MAX_JSON_BYTES,
};

/// A size for a message: whole megabytes, or bytes when smaller.
fn human(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{} MB", bytes >> 20)
    } else {
        format!("{bytes} bytes")
    }
}

fn too_big(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
/// Entries over this need the zip64 header.
const ZIP64_THRESHOLD: u64 = u32::MAX as u64;

/// A sample's original file name and bytes, exactly as read from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleFile {
    pub name: String,
    pub bytes: Arc<Vec<u8>>,
}

/// Original sample files by channel or audio asset id (one disjoint identity domain).
pub type SampleFiles = HashMap<u32, SampleFile>;

/// What [`load`] returns.
#[derive(Debug)]
pub struct Loaded {
    /// Validated: every value is in range (see [`Project::validate`]).
    pub project: Project,
    /// The bytes of each channel's sample that could be found.
    pub samples: SampleFiles,
    /// Things the person should know, one sentence each: fixes, and samples that were not in
    /// the file.
    pub warnings: Vec<String>,
}

/// A load prepared for opening in a session, with provenance for its saved checkpoint.
pub struct LoadedForOpen {
    pub(crate) loaded: Loaded,
    pub(crate) unchanged_at: Option<PathBuf>,
}

impl LoadedForOpen {
    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }
}

impl From<Loaded> for LoadedForOpen {
    fn from(loaded: Loaded) -> Self {
        Self {
            loaded,
            unchanged_at: None,
        }
    }
}

/// Why a project did not open.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be read.
    Io(io::Error),
    /// It is not a Grooviq project (not a zip, no `project.json`, or another format).
    NotAProject,
    /// It is one, but damaged (cut short, invalid JSON, wrong types).
    Corrupt(String),
    /// A newer Grooviq saved it.
    NewerVersion { found: u32, supported: u32 },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "The project file could not be read: {e}."),
            Self::NotAProject => write!(f, "This file is not a Grooviq project."),
            Self::Corrupt(why) => write!(f, "This project file is damaged ({why})."),
            Self::NewerVersion { found, supported } => write!(
                f,
                "This project was saved by a newer version of Grooviq (file format {found}; \
                 this version reads up to {supported}). Update Grooviq to open it."
            ),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// `project.json`: the header, then the project's own fields.
#[derive(Serialize)]
struct Document<'a> {
    format: &'static str,
    version: u32,
    #[serde(flatten)]
    project: &'a Project,
}

/// Collects JSON text, failing once it would pass `limit` bytes: an oversized project is
/// refused without its whole text in memory.
struct Capped {
    bytes: Vec<u8>,
    written: u64,
    keep: bool,
    limit: u64,
    over: bool,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.saturating_add(buf.len() as u64) > self.limit {
            self.over = true;
            return Err(io::Error::other("over the limit"));
        }
        self.written += buf.len() as u64;
        if self.keep {
            self.bytes.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn matches_json(value: &impl Serialize, mut expected: impl BufRead) -> bool {
    struct Matching<'a, R>(&'a mut R);

    impl<R: BufRead> Write for Matching<'_, R> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.is_empty() {
                return Ok(0);
            }
            let expected = self.0.fill_buf()?;
            let n = bytes.len().min(expected.len());
            if n == 0 || bytes[..n] != expected[..n] {
                return Err(io::Error::other("project JSON differs"));
            }
            self.0.consume(n);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    serde_json::to_writer(Matching(&mut expected), value).is_ok()
        && expected.fill_buf().is_ok_and(|rest| rest.is_empty())
}

#[cfg(test)]
#[test]
fn json_comparison_rejects_short_long_and_different_input() {
    let matches = |bytes| matches_json(&[1, 2], BufReader::with_capacity(2, bytes));
    assert!(matches(b"[1,2]".as_slice()));
    assert!(!matches(b"[1,2".as_slice()));
    assert!(!matches(b"[1,2] ".as_slice()));
    assert!(!matches(b"[1,3]".as_slice()));
}

/// `project.json`'s bytes: compact (pretty printing tripled a note-heavy project), and at most
/// `limit`, the most `load` reads.
fn project_json_bytes(project: &Project, limit: u64) -> io::Result<Vec<u8>> {
    serialize_capped(project, limit, true)
}

fn serialize_capped(project: &Project, limit: u64, keep: bool) -> io::Result<Vec<u8>> {
    let mut out = Capped {
        bytes: Vec::new(),
        written: 0,
        keep,
        limit,
        over: false,
    };
    let document = Document {
        format: FORMAT,
        version: VERSION,
        project,
    };
    match serde_json::to_writer(&mut out, &document) {
        Ok(()) => Ok(out.bytes),
        Err(_) if out.over => Err(too_big(format!(
            "The project is too big to save: its notes and settings would take more than the {} a project file can hold. Remove some notes, then save again.",
            human(limit)
        ))),
        Err(e) => Err(io::Error::other(e)),
    }
}

/// Precommit admission using exactly the writer's schema, embedding and shared budgets.
/// Counts compact JSON into a capped sink without allocating its serialized bytes.
pub fn validate_save(project: &Project, samples: &SampleFiles) -> io::Result<()> {
    validate_save_with_limits(project, samples, LIMITS)
}

/// Scalar edits may preserve an already missing embedded sample. They still use the
/// writer's exact JSON and present-media budgets; Save retains its loss-prevention refusal.
pub(crate) fn validate_scalar_edit(project: &Project, samples: &SampleFiles) -> io::Result<()> {
    let (project, _) = plan_with_missing(project, samples, LIMITS, true)?;
    serialize_capped(&project, LIMITS.json, false).map(|_| ())
}

fn validate_save_with_limits(
    project: &Project,
    samples: &SampleFiles,
    limits: Limits,
) -> io::Result<()> {
    let (project, _) = plan(project, samples, limits)?;
    serialize_capped(&project, limits.json, false).map(|_| ())
}

/// One sample stored in the zip.
struct Entry {
    name: String,
    bytes: Arc<Vec<u8>>,
}

/// Reads a sample file from disk, keeping its name and bytes as they are. A regular file of at
/// most 512 MB; anything else (a folder, a device, a pipe, a bigger file) is an error.
pub fn read_sample_file(path: &Path) -> io::Result<SampleFile> {
    read_limited(path, MAX_SAMPLE_BYTES)
}

/// Reserve retained original storage before reading, including a growing file.
pub fn read_sample_file_budgeted(
    path: &Path,
    budget: &grooviq_engine::media_budget::MediaBudget,
) -> io::Result<SampleFile> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut hold = budget.reserve(0).map_err(io::Error::other)?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let wanted = bytes
            .len()
            .checked_add(n)
            .ok_or_else(|| io::Error::other("Original too large"))?;
        if wanted > MAX_SAMPLE_BYTES as usize {
            return Err(io::Error::other("Original exceeds 512 MiB"));
        }
        if wanted > bytes.capacity() {
            let capacity = wanted.next_power_of_two();
            hold.resize(capacity).map_err(io::Error::other)?;
            bytes.reserve_exact(capacity - bytes.len());
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    let bytes = Arc::new(bytes);
    hold.bind(0, &bytes, bytes.capacity())
        .map_err(io::Error::other)?;
    Ok(SampleFile {
        name: path
            .file_name()
            .map_or_else(|| "sample".into(), |n| n.to_string_lossy().into_owned()),
        bytes,
    })
}

/// [`read_sample_file`] with a lower limit. The limit is enforced on the bytes read, not on the
/// size the file system reported, so a file that grows while it is read cannot get past it.
fn read_limited(path: &Path, limit: u64) -> io::Result<SampleFile> {
    // Look before opening: opening a FIFO for reading blocks until someone writes to it.
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the file is over the {} MB limit", limit >> 20),
        ));
    }
    let name = path
        .file_name()
        .map_or_else(|| "sample".into(), |n| n.to_string_lossy().into_owned());
    Ok(SampleFile {
        name,
        bytes: Arc::new(bytes),
    })
}

/// The file operations that make a save durable, in the order a save makes them: the zip is
/// written into the temp file, the temp file is synced, then renamed over the target. The test
/// seam for that order and for each step failing. Not part of the API.
#[doc(hidden)]
pub trait Disk {
    /// Sync the renamed entry's parent directory.
    fn sync_folder(&mut self, path: &Path) -> io::Result<()> {
        sync_folder_on(path, self)
    }
    /// Flush the completed temporary file before syncing it.
    fn flush(&mut self, file: &mut File) -> io::Result<()> {
        file.flush()
    }
    /// Runs once the zip is written and flushed into the temp file, before it is synced.
    fn written(&mut self, _file: &mut File) -> io::Result<()> {
        Ok(())
    }
    /// Syncs a temporary file or its parent directory (see [`sync_file`]).
    fn sync(&mut self, file: &File) -> io::Result<()> {
        sync_file(file)
    }
    /// Puts the temp file in place (on Windows `rename` replaces an existing file too).
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }
}

/// Syncs a file or directory through the operating system, whatever [`Disk`] runs it.
/// Tests count the syncs that succeed (on their own thread), so a save that stops syncing
/// fails one.
#[doc(hidden)]
pub fn sync_file(file: &File) -> io::Result<()> {
    file.sync_all().inspect(|()| {
        #[cfg(test)]
        if file.metadata().is_ok_and(|m| m.is_file()) {
            tests::SYNCED.with(|n| n.set(n.get() + 1));
        }
    })
}

/// The real disk.
#[doc(hidden)]
pub struct RealDisk;

impl Disk for RealDisk {}

/// Saves the project and its samples to `path`, atomically (see the module docs). `samples`
/// holds each channel's file by channel id; a channel without one is saved without a copy.
pub fn save(project: &Project, samples: &SampleFiles, path: &Path) -> io::Result<()> {
    save_with(project, samples, path, false)
}

/// [`save`], optionally failing after the temp file is fully written and before the rename:
/// the test hook for "a failed save leaves the previous file intact". Not part of the API.
#[doc(hidden)]
pub fn save_with(
    project: &Project,
    samples: &SampleFiles,
    path: &Path,
    fail_after_temp: bool,
) -> io::Result<()> {
    save_with_limits(project, samples, path, fail_after_temp, LIMITS)
}

pub(crate) fn save_with_limits(
    project: &Project,
    samples: &SampleFiles,
    path: &Path,
    fail_after_temp: bool,
    limits: Limits,
) -> io::Result<()> {
    let staged = stage_save_with_limits(project, samples, path, limits, &mut RealDisk)?;
    if fail_after_temp {
        // Dropping the staged file removes the temp file.
        return Err(io::Error::other(
            "injected failure after the temp file was written",
        ));
    }
    staged.commit(&mut RealDisk)
}

/// Writes `project` to a temp file beside `path` without touching `path`: a save that is
/// refused or fails stops here, before anything (a backup, the file itself) changes.
/// [`Staged::commit`] puts it in place. `disk` makes the file operations (see [`Disk`]).
pub(crate) fn stage_save(
    project: &Project,
    samples: &SampleFiles,
    path: &Path,
    disk: &mut impl Disk,
) -> io::Result<Staged> {
    stage_save_with_limits(project, samples, path, LIMITS, disk)
}

fn stage_save_with_limits(
    project: &Project,
    samples: &SampleFiles,
    path: &Path,
    limits: Limits,
    disk: &mut impl Disk,
) -> io::Result<Staged> {
    // Unsupported routing refuses before even the legacy stale-save cleanup runs.
    crate::mixer::require_runtime_support(project).map_err(|e| too_big(e.to_string()))?;
    // Temp files of saves that died (and only those: see `leftover`); best effort.
    let _ = grooviq_engine::remove_stale_owned(path, ".tmp");
    // Everything that can be refused is refused before the first byte is written.
    let (project, entries) = plan(project, samples, limits)?;
    let json = project_json_bytes(&project, limits.json)?;
    stage(path, disk, |file| write_zip(file, &json, &entries))
}

/// Which samples go in the zip, and the project with each channel's `SampleRef.file` set.
/// One entry per distinct byte content (the same `Arc` or equal bytes), in channel order.
///
/// Fails, naming the channel, when a sample is over `limits.sample`, the distinct samples
/// together pass `limits.total` (a file `load` would then not fully open), or a channel still
/// names a zip entry whose bytes are not here (a sample `load` skipped): saving would drop its
/// only copy. Fails too when a pattern holds more than [`MAX_NOTES_PER_PATTERN`] notes: `load`
/// would drop the rest. (`project.json` over `limits.json` is refused once it is written: see
/// [`project_json_bytes`].)
fn plan(
    project: &Project,
    samples: &SampleFiles,
    limits: Limits,
) -> io::Result<(Project, Vec<Entry>)> {
    plan_with_missing(project, samples, limits, false)
}

fn plan_with_missing(
    project: &Project,
    samples: &SampleFiles,
    limits: Limits,
    allow_missing: bool,
) -> io::Result<(Project, Vec<Entry>)> {
    // The resulting file must be valid and render with every stored mixer setting.
    crate::mixer::require_runtime_support(project).map_err(|e| too_big(e.to_string()))?;
    if let Some(p) = project
        .patterns
        .iter()
        .find(|p| p.notes.len() > MAX_NOTES_PER_PATTERN)
    {
        return Err(too_big(format!(
            "Pattern \"{}\" has {} notes, more than the {MAX_NOTES_PER_PATTERN} a pattern can hold, so the project was not saved.",
            crate::model::warning_label(&p.name),
            p.notes.len()
        )));
    }
    project.validate_arrangement().map_err(too_big)?;
    let mut project = project.clone();
    let mut entries: Vec<Entry> = Vec::new();
    let mut total = 0u64;
    for (id, name, sample_ref) in project
        .channels
        .iter_mut()
        .filter_map(|c| {
            c.sample.as_mut().map(|s| {
                (
                    c.id,
                    format!("Channel \"{}\"", crate::model::warning_label(&c.name)),
                    s,
                )
            })
        })
        .chain(
            project
                .arrangement
                .audio_assets
                .iter_mut()
                .map(|a| (a.id, format!("Audio asset {}", a.id), &mut a.sample)),
        )
    {
        let Some(file) = samples.get(&id) else {
            if !allow_missing && sample_ref.file.is_some() {
                let source = if name.starts_with("Audio asset ") {
                    "asset"
                } else {
                    "channel"
                };
                return Err(too_big(format!(
                    "{}: its sample was not loaded, so saving would lose it. Relink the sample or remove the {source} first.",
                    name
                )));
            }
            continue;
        };
        let size = file.bytes.len() as u64;
        if size > limits.sample {
            return Err(too_big(format!(
                "{}: its sample is {}, over the {} limit for one sample.",
                name,
                human(size),
                human(limits.sample)
            )));
        }
        let shared = entries
            .iter()
            .find(|e| Arc::ptr_eq(&e.bytes, &file.bytes) || *e.bytes == *file.bytes);
        let name = match shared {
            Some(entry) => entry.name.clone(),
            None => {
                total += size;
                if total > limits.total {
                    return Err(too_big(format!(
                        "{}: its sample would take the project past the {} limit for all samples.",
                        name,
                        human(limits.total)
                    )));
                }
                let name = entry_name(entries.len() + 1, &file.name);
                entries.push(Entry {
                    name: name.clone(),
                    bytes: file.bytes.clone(),
                });
                name
            }
        };
        sample_ref.file = Some(name);
    }
    Ok((project, entries))
}

/// `samples/<n>-<file name>`, with the file name cut down to one safe path component: no
/// folders, no characters a zip tool or Windows would trip on, never empty.
fn entry_name(n: usize, original: &str) -> String {
    let last = original.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = last
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim_matches(['.', ' ']);
    let clean = if clean.is_empty() { "sample" } else { clean };
    format!("{SAMPLES_DIR}{n}-{clean}")
}

/// Writes the zip: `project.json` deflated, then the samples stored as they are. Every entry
/// gets the same timestamp (1980-01-01, the zip epoch) and permissions.
fn write_zip(file: &mut File, json: &[u8], entries: &[Entry]) -> io::Result<()> {
    let epoch = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .map_err(|_| io::Error::other("the zip epoch is not a valid timestamp"))?;
    let options = SimpleFileOptions::default()
        .last_modified_time(epoch)
        // Not the host's default (DOS on Windows, Unix elsewhere): the same bytes everywhere.
        .system(zip::System::Unix)
        .unix_permissions(0o644);

    let mut zip = ZipWriter::new(BufWriter::new(file));
    zip.start_file(
        PROJECT_JSON,
        options.compression_method(CompressionMethod::Deflated),
    )?;
    zip.write_all(json)?;
    for entry in entries {
        // Samples are already compressed audio, or PCM the person may want to read back as is.
        zip.start_file(
            entry.name.as_str(),
            options
                .compression_method(CompressionMethod::Stored)
                .large_file(entry.bytes.len() as u64 >= ZIP64_THRESHOLD),
        )?;
        zip.write_all(&entry.bytes)?;
    }
    zip.finish()?.flush()
}

/// A file fully written (and synced) to its temp name beside its final one, not yet in
/// place. Dropped without [`Staged::commit`], the temp file is removed.
#[derive(Debug)]
pub(crate) struct Staged {
    tmp: PathBuf,
    path: PathBuf,
    /// Held (and locked: see `leftover`) until the rename or the drop, so a cleanup elsewhere
    /// can tell this temp file has a live writer. Never closed early: a lock goes with its handle.
    _lock: File,
    done: bool,
}

impl Staged {
    /// Renames the temp file over the final one with `disk`, then syncs the folder.
    pub(crate) fn commit(mut self, disk: &mut impl Disk) -> io::Result<()> {
        disk.rename(&self.tmp, &self.path)?;
        self.done = true;
        confirm_directory(disk, &self.path)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

/// Writes the temp file for `path` with `write`, flushed and synced. The temp file has a name
/// of its own (`.<name>.grooviq-<pid>-<random>.tmp`), so it can never be a file of the
/// person's or another save's.
fn stage(
    path: &Path,
    disk: &mut impl Disk,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<Staged> {
    let grooviq_engine::OwnedTemp { file, path: tmp } =
        grooviq_engine::create_owned_temp(path, ".tmp")?;
    // Made first, so a failure below removes the partial temp file.
    let mut staged = Staged {
        tmp,
        path: path.to_path_buf(),
        _lock: file,
        done: false,
    };
    write(&mut staged._lock)?;
    disk.flush(&mut staged._lock)?;
    disk.written(&mut staged._lock)?;
    // Before the rename: a power cut after it must find the new bytes, not an empty file.
    disk.sync(&staged._lock)?;
    Ok(staged)
}

/// The target was renamed successfully; its directory durability is unconfirmed.
/// This is distinct from an error before rename: callers must retain the new target.
#[derive(Debug)]
pub struct DirectoryUnconfirmed(pub io::Error);
impl fmt::Display for DirectoryUnconfirmed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "The file was written, but directory durability could not be confirmed: {}. Keep recovery copies and try saving again.",
            self.0
        )
    }
}
impl std::error::Error for DirectoryUnconfirmed {}

/// Whether an I/O error reports a completed rename, rather than an unchanged target.
pub fn directory_unconfirmed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|e| e.is::<DirectoryUnconfirmed>())
}

pub(crate) fn confirm_directory(disk: &mut impl Disk, path: &Path) -> io::Result<()> {
    disk.sync_folder(path)
        .map_err(|e| io::Error::other(DirectoryUnconfirmed(e)))
}

#[cfg(unix)]
fn sync_folder_on(path: &Path, disk: &mut (impl Disk + ?Sized)) -> io::Result<()> {
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    disk.sync(&File::open(folder)?)
}

// Windows has no Unix directory-fsync contract. Saves retain the platform's rename
// guarantees; success here does not claim directory durability on unsupported platforms.
#[cfg(not(unix))]
fn sync_folder_on(_path: &Path, _disk: &mut (impl Disk + ?Sized)) -> io::Result<()> {
    Ok(())
}

/// Opens a project: reads `project.json`, migrates it to this version, validates it and finds
/// each channel's sample bytes (see [`Loaded`]).
///
/// A channel's sample is read from its zip entry and from nowhere else: `load` never opens a path
/// the project file names (`SampleRef.path` is only shown, and offered for relinking). If the entry
/// is missing or unreadable the channel has no bytes and no entry name, with a warning; the path
/// stays so the person can relink it.
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    load_with_limits(path, LIMITS)
}

/// [`load`] with the total budget alone injected (for tests).
#[cfg(test)]
fn load_with_budget(path: &Path, budget: u64) -> Result<Loaded, LoadError> {
    load_with_limits(
        path,
        Limits {
            total: budget,
            ..LIMITS
        },
    )
}

/// [`load`] reading at most `limits.sample` bytes per sample and `limits.total` in all, so a file
/// claiming hundreds of channels of big samples cannot exhaust memory. An embedded sample over a
/// limit is not loaded: the warning says it is too big, and the channel keeps its entry name (a
/// later save refuses rather than drop it).
fn load_with_limits(path: &Path, limits: Limits) -> Result<Loaded, LoadError> {
    load_inner(path, limits, None, false).map(|result| result.loaded)
}
pub fn load_budgeted(
    path: &Path,
    budget: &grooviq_engine::media_budget::MediaBudget,
) -> Result<Loaded, LoadError> {
    load_inner(path, LIMITS, Some(budget), false).map(|result| result.loaded)
}

/// Loads outside the session lock and checks whether the current-format manifest was
/// preserved, including fields the loader can repair silently.
pub fn load_for_open(
    path: &Path,
    budget: &grooviq_engine::media_budget::MediaBudget,
) -> Result<LoadedForOpen, LoadError> {
    load_inner(path, LIMITS, Some(budget), true)
}
fn load_inner(
    path: &Path,
    limits: Limits,
    budget: Option<&grooviq_engine::media_budget::MediaBudget>,
    check_unchanged: bool,
) -> Result<LoadedForOpen, LoadError> {
    let mut left = limits.total;
    let mut file = File::open(path)?;
    // A zip starts with "PK"; anything else is not one of ours, and says so plainly instead of
    // as a zip parser's complaint.
    let mut magic = Vec::with_capacity(2);
    (&mut file).take(2).read_to_end(&mut magic)?;
    if magic != b"PK" {
        return Err(LoadError::NotAProject);
    }
    file.rewind()?;
    let mut zip = ZipArchive::new(file).map_err(zip_error)?;

    // `project.json` is streamed from the zip: header-only refusal, then the outline with
    // channel ids and which patterns have notes (a newer version is refused before anything
    // is typed), then the typed project, which keeps at most `MAX_NOTES_PER_PATTERN` notes per
    // pattern.
    let does_not_fit = |e: serde_json::Error| match e.classify() {
        serde_json::error::Category::Data => {
            LoadError::Corrupt(format!("{PROJECT_JSON} does not fit: {e}"))
        }
        _ => json_error(e),
    };
    let header = outline::read_header(project_json(&mut zip, limits.json)?)
        .map_err(does_not_fit)?
        .ok_or(LoadError::NotAProject)?;
    if header.format.as_deref() != Some(FORMAT) {
        return Err(LoadError::NotAProject);
    }
    checked_version(header.version)?;
    let outline = outline::read(project_json(&mut zip, limits.json)?)
        .map_err(does_not_fit)?
        .ok_or(LoadError::NotAProject)?;
    if outline.format.as_deref() != Some(FORMAT) {
        return Err(LoadError::NotAProject);
    }
    let version = checked_version(outline.version)?;
    let (project, drops, mixer) = reading::with_context_mixer(version, outline.channels, || {
        let json = project_json(&mut zip, limits.json)?;
        let result = if version < 4 {
            let mut de = serde_json::Deserializer::from_reader(json);
            de.deserialize_map(LegacyProjectFields)
                .and_then(|project| de.end().map(|()| project))
        } else {
            serde_json::from_reader::<_, reading::Named<Project>>(json).map(|named| named.0)
        };
        result.map_err(does_not_fit)
    });
    let mut project = project?;
    if version >= 4 && !mixer {
        return Err(LoadError::Corrupt(format!("{PROJECT_JSON} has no mixer")));
    }
    migrate(&mut project, version)?;

    // What each pattern lost while it was read, by pattern. The typed pass read the `notes` of
    // exactly the patterns the outline saw a `notes` key in, in order (a repeated key or another
    // shape fails the typed pass), so the two line up.
    let mut drops = drops.into_iter();
    let read: Vec<NoteDrops> = outline
        .has_notes
        .iter()
        .map(|&has| {
            if has {
                drops.next()
            } else {
                Some(NoteDrops::default())
            }
        })
        .collect::<Option<_>>()
        .filter(|read: &Vec<NoteDrops>| {
            read.len() == project.patterns.len() && drops.next().is_none()
        })
        .ok_or_else(|| LoadError::Corrupt(format!("{PROJECT_JSON} reads differently twice")))?;
    let mut warnings = project.validate_read(&read);
    project.validate_arrangement().map_err(LoadError::Corrupt)?;
    // Strict, and after the legacy channel repairs: sources name the channels that are kept.
    project
        .validate_mixer()
        .map_err(|e| LoadError::Corrupt(e.to_string()))?;
    let mut samples = SampleFiles::new();
    let mut entries: HashMap<String, EntryRead> = HashMap::new();
    // Channels whose embedded sample is too big: their entry name stays.
    let mut skipped = HashSet::new();
    for (id, name, sample_ref) in project
        .channels
        .iter()
        .filter_map(|c| {
            c.sample.as_ref().map(|s| {
                (
                    c.id,
                    format!("Channel \"{}\"", crate::model::warning_label(&c.name)),
                    s,
                )
            })
        })
        .chain(
            project
                .arrangement
                .audio_assets
                .iter()
                .map(|a| (a.id, format!("Audio asset {}", a.id), &a.sample)),
        )
    {
        let embedded = sample_ref.file.as_deref().map(|name| {
            entries
                .entry(name.to_string())
                .or_insert_with(|| {
                    read_entry_budgeted(&mut zip, name, limits.sample, &mut left, budget)
                })
                .clone()
        });
        match embedded {
            Some(EntryRead::MediaLimit) => {
                return Err(LoadError::Io(io::Error::other(
                    "Shared retained-media budget exhausted",
                )));
            }
            Some(EntryRead::Found(found)) => {
                samples.insert(id, found);
                continue;
            }
            Some(EntryRead::TooBig { over_total }) => {
                warnings.push(if over_total {
                    format!(
                        "{}: its sample would take the project past the {} limit for all samples, so it was not loaded.",
                        name,
                        human(limits.total)
                    )
                } else {
                    format!(
                        "{}: its sample is larger than {}, so it was not loaded.",
                        name,
                        human(limits.sample)
                    )
                });
                skipped.insert(id);
                continue;
            }
            Some(EntryRead::Missing) | None => {}
        }
        // Missing or unreadable entry: no bytes. The path in the file is never opened.
        warnings.push(format!(
            "{}: its sample is missing from the project file. Relink it to hear it.",
            name
        ));
    }
    // A channel whose bytes could not be had keeps its path (to relink) but has no entry.
    for channel in &mut project.channels {
        if let Some(sample_ref) = &mut channel.sample
            && !samples.contains_key(&channel.id)
            && !skipped.contains(&channel.id)
        {
            sample_ref.file = None;
        }
    }
    for asset in &mut project.arrangement.audio_assets {
        if !samples.contains_key(&asset.id) && !skipped.contains(&asset.id) {
            asset.sample.file = None;
        }
    }
    // Advisory only: a failed read or mismatch just keeps the session dirty, never fails the load.
    let unchanged = check_unchanged
        && version == VERSION
        && warnings.is_empty()
        && project_json(&mut zip, limits.json).is_ok_and(|reader| {
            matches_json(
                &Document {
                    format: FORMAT,
                    version: VERSION,
                    project: &project,
                },
                BufReader::new(reader),
            )
        });
    Ok(LoadedForOpen {
        loaded: Loaded {
            project,
            samples,
            warnings,
        },
        unchanged_at: unchanged.then(|| path.to_path_buf()),
    })
}

struct LegacyProjectFields;

impl<'de> Visitor<'de> for LegacyProjectFields {
    type Value = Project;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(reading::NAMED)
    }

    fn visit_map<A: MapAccess<'de>>(self, inner: A) -> Result<Project, A::Error> {
        Project::deserialize(MapAccessDeserializer::new(SkipRepeatedMixer {
            inner,
            seen_mixer: false,
        }))
    }
}

/// The existing legacy field reader discards the first mixer value. Discard later values
/// here so the derived project reader keeps its normal duplicate checks for every other key.
struct SkipRepeatedMixer<A> {
    inner: A,
    seen_mixer: bool,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for SkipRepeatedMixer<A> {
    type Error = A::Error;

    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        mut seed: S,
    ) -> Result<Option<S::Value>, A::Error> {
        while let Some(key) = self.inner.next_key_seed(ProjectField {
            seed,
            seen_mixer: &mut self.seen_mixer,
        })? {
            match key {
                ControlFlow::Break(value) => return Ok(Some(value)),
                ControlFlow::Continue(reuse_seed) => {
                    self.inner.next_value::<IgnoredAny>()?;
                    seed = reuse_seed;
                }
            }
        }
        Ok(None)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, A::Error> {
        self.inner.next_value_seed(seed)
    }
}

struct ProjectField<'a, S> {
    seed: S,
    seen_mixer: &'a mut bool,
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for ProjectField<'_, S> {
    type Value = ControlFlow<S::Value, S>;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_identifier(self)
    }
}

impl<'de, S: DeserializeSeed<'de>> Visitor<'de> for ProjectField<'_, S> {
    type Value = ControlFlow<S::Value, S>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a project field name")
    }

    fn visit_str<E: serde::de::Error>(self, key: &str) -> Result<Self::Value, E> {
        if key == "mixer" && std::mem::replace(self.seen_mixer, true) {
            Ok(ControlFlow::Continue(self.seed))
        } else {
            self.seed
                .deserialize(StrDeserializer::new(key))
                .map(ControlFlow::Break)
        }
    }
}

/// `project.json` from the zip, streamed and buffered, never more than `limit` bytes.
fn project_json<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    limit: u64,
) -> Result<impl Read + '_, LoadError> {
    let entry = match zip.by_name(PROJECT_JSON) {
        Ok(entry) => entry,
        Err(ZipError::FileNotFound) => return Err(LoadError::NotAProject),
        Err(e) => return Err(zip_error(e)),
    };
    if entry.size() > limit {
        return Err(LoadError::Corrupt(format!(
            "{PROJECT_JSON} is larger than {}",
            human(limit)
        )));
    }
    Ok(BufReader::new(entry.take(limit)))
}

/// A broken or unreadable `project.json` (a zip checksum mismatch is an I/O error).
fn json_error(e: serde_json::Error) -> LoadError {
    if e.is_io() {
        LoadError::Corrupt(format!("{PROJECT_JSON} cannot be read: {e}"))
    } else {
        LoadError::Corrupt(format!("{PROJECT_JSON} is not valid JSON: {e}"))
    }
}

/// A file's format version, 1..=[`VERSION`]; above that is [`LoadError::NewerVersion`].
fn checked_version(version: Option<u64>) -> Result<u32, LoadError> {
    match version {
        Some(v) if v > u64::from(VERSION) => Err(LoadError::NewerVersion {
            found: u32::try_from(v).unwrap_or(u32::MAX),
            supported: VERSION,
        }),
        Some(v) if v >= 1 => Ok(v as u32),
        _ => Err(LoadError::Corrupt(
            "its format version is missing or invalid".into(),
        )),
    }
}

/// Brings a project read from a file of format `version` up to [`VERSION`], one version at a
/// time. `load` runs it on the typed project, never on a JSON tree, so an untrusted file is never
/// held whole.
///
/// * 1 → 2: every channel is a sampler, and each pattern's notes get the ids `1..=n` in stored
///   order, with `next_note_id` n + 1.
/// * 2 → 3: Pattern play mode and an empty arrangement.
/// * 3 → 4: the legacy direct mixer, which plays exactly as before.
///
/// A version above [`VERSION`] is [`LoadError::NewerVersion`], 0 is [`LoadError::Corrupt`].
pub fn migrate(project: &mut Project, version: u32) -> Result<(), LoadError> {
    let version = checked_version(Some(u64::from(version)))?;
    if version < 3 {
        project.play_mode = crate::PlayMode::Pattern;
        project.arrangement = crate::Arrangement::default();
    }
    if version < 4 {
        project.mixer = crate::mixer::Mixer::default();
    }
    if version == 1 {
        for channel in &mut project.channels {
            channel.instrument = InstrumentKind::Sampler;
        }
        for pattern in &mut project.patterns {
            for (note, id) in pattern.notes.iter_mut().zip(1..) {
                note.id = id;
            }
            // At most `MAX_NOTES_PER_PATTERN` notes were kept: no overflow.
            pattern.next_note_id = pattern.notes.len() as u32 + 1;
        }
    }
    Ok(())
}

/// What reading a sample's zip entry gave.
#[derive(Clone)]
enum EntryRead {
    MediaLimit,
    Found(SampleFile),
    /// Not in the zip, or unreadable (a checksum mismatch is an unreadable entry).
    Missing,
    /// In the zip but over the per-sample limit, or past what is left of the total budget.
    TooBig {
        over_total: bool,
    },
}

/// A sample's bytes from its entry (see [`EntryRead`]); a sample read reduces `left`. At most
/// the declared size is ever decompressed (one byte more is read to catch a header that lies),
/// so a compressed entry cannot grow past what was budgeted.
fn read_entry_budgeted<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
    sample_limit: u64,
    left: &mut u64,
    budget: Option<&grooviq_engine::media_budget::MediaBudget>,
) -> EntryRead {
    if !name.starts_with(SAMPLES_DIR) || name[SAMPLES_DIR.len()..].contains(['/', '\\']) {
        return EntryRead::Missing;
    }
    let Ok(entry) = zip.by_name(name) else {
        return EntryRead::Missing;
    };
    let size = entry.size();
    if size > sample_limit {
        return EntryRead::TooBig { over_total: false };
    }
    if size > *left {
        return EntryRead::TooBig { over_total: true };
    }
    let reserve = match budget.map(|b| b.reserve(size as usize)) {
        Some(Ok(r)) => Some(r),
        Some(Err(_)) => return EntryRead::MediaLimit,
        None => None,
    };
    let mut bytes = vec![0u8; size as usize];
    let mut entry = entry;
    if entry.read_exact(&mut bytes).is_err() || entry.read(&mut [0u8; 1]).ok() != Some(0) {
        return EntryRead::Missing;
    }
    let bytes = Arc::new(bytes);
    if let Some(hold) = reserve
        && hold.bind(0, &bytes, bytes.capacity()).is_err()
    {
        return EntryRead::MediaLimit;
    }
    *left -= size;
    EntryRead::Found(SampleFile {
        name: original_name(name),
        bytes,
    })
}

/// The file name an entry was saved from: `samples/3-kick.wav` is `kick.wav`.
fn original_name(entry: &str) -> String {
    let rest = entry.strip_prefix(SAMPLES_DIR).unwrap_or(entry);
    match rest.split_once('-') {
        Some((n, name)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => name.into(),
        _ => rest.into(),
    }
}

/// A zip that cannot be opened: damaged, unless the disk itself failed (a file cut short reads
/// as an unexpected end, which is damage too).
fn zip_error(e: ZipError) -> LoadError {
    match e {
        ZipError::Io(io) if io.kind() != io::ErrorKind::UnexpectedEof => LoadError::Io(io),
        other => LoadError::Corrupt(other.to_string()),
    }
}

#[cfg(all(test, unix))]
pub(crate) mod order_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    pub(crate) struct Audit {
        pub target: PathBuf,
        pub ops: Vec<&'static str>,
        pub fail: &'static str,
        synced_file: Option<(u64, u64)>,
    }
    impl Audit {
        pub fn new(target: &Path, fail: &'static str) -> Self {
            Self {
                target: target.into(),
                ops: Vec::new(),
                fail,
                synced_file: None,
            }
        }
        fn step(&mut self, op: &'static str) -> io::Result<()> {
            self.ops.push(op);
            if op == self.fail {
                Err(io::Error::other(op))
            } else {
                Ok(())
            }
        }
    }
    impl Disk for Audit {
        fn flush(&mut self, file: &mut File) -> io::Result<()> {
            assert!(self.ops.is_empty());
            self.step("flush")?;
            file.flush()
        }
        fn sync(&mut self, file: &File) -> io::Result<()> {
            if file.metadata()?.is_dir() {
                let actual = file.metadata()?;
                let expected = fs::metadata(self.target.parent().unwrap())?;
                assert_eq!(
                    (actual.dev(), actual.ino()),
                    (expected.dev(), expected.ino())
                );
                assert_eq!(self.ops.last(), Some(&"rename"));
                assert_ne!(fs::read(&self.target)?, b"old");
                self.step("directory")?;
            } else {
                assert_eq!(self.ops, ["flush"]);
                assert!(file.metadata()?.len() > 0);
                assert_eq!(fs::read(&self.target)?, b"old");
                let metadata = file.metadata()?;
                self.synced_file = Some((metadata.dev(), metadata.ino()));
                self.step("file")?;
            }
            file.sync_all()
        }
        fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
            assert_eq!(to, self.target);
            let metadata = fs::metadata(from)?;
            assert_eq!(self.synced_file, Some((metadata.dev(), metadata.ino())));
            assert_eq!(self.ops, ["flush", "file"]);
            assert_eq!(fs::read(to)?, b"old");
            self.step("rename")?;
            fs::rename(from, to)
        }
    }

    #[test]
    fn save_sync_order_and_failures() {
        for fail in ["", "flush", "file", "rename", "directory"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("song.grooviq");
            fs::write(&path, b"old").unwrap();
            let mut disk = Audit::new(&path, fail);
            let result = stage_save(&Project::default(), &SampleFiles::new(), &path, &mut disk)
                .and_then(|staged| staged.commit(&mut disk));
            let expected = match fail {
                "flush" => vec!["flush"],
                "file" => vec!["flush", "file"],
                "rename" => vec!["flush", "file", "rename"],
                _ => vec!["flush", "file", "rename", "directory"],
            };
            assert_eq!(disk.ops, expected);
            assert_eq!(result.is_ok(), fail.is_empty());
            if matches!(fail, "flush" | "file" | "rename") {
                assert_eq!(fs::read(&path).unwrap(), b"old");
            } else {
                assert!(load(&path).is_ok());
            }
            if fail == "directory" {
                assert!(directory_unconfirmed(&result.unwrap_err()));
            }
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// Syncs [`sync_file`] made on this thread.
        pub(super) static SYNCED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn load_mixer_fixture(version: u32, extra: &str) -> Result<Loaded, LoadError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixer.grooviq");
        let json = format!(
            r#"{{"format":"grooviq-project","version":{version},"name":"Beat","bpm":120,
            "patterns":[{{"id":0,"name":"Beat","steps":16,"notes":[]}}],
            "currentPattern":0,"nextId":1{extra}}}"#
        );
        write_zip(&mut File::create(&path).unwrap(), json.as_bytes(), &[]).unwrap();
        load(&path)
    }

    #[test]
    fn file_mixer_keys_ignore_repeated_legacy_values() {
        for version in 1..4 {
            let loaded = load_mixer_fixture(
                version,
                r#", "mixer":"obsolete", "m\u0069xer":{"mode":"routed"}, "mixer":[1,2]"#,
            )
            .unwrap();
            assert_eq!(
                loaded.project,
                load_mixer_fixture(version, "").unwrap().project
            );
            assert_eq!(loaded.project.mixer.mode, crate::mixer::Mode::LegacyDirect);
            assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        }
    }

    #[test]
    fn file_mixer_keys_refuse_v4_duplicates() {
        let identity = r#"{"mode":"legacyDirect","tracks":[{"id":0,"name":"Master"}],"nextTrackId":1,"nextInsertId":1,"nextSendId":1}"#;
        let error = load_mixer_fixture(4, &format!(r#", "mixer":{identity}, "mixer":{identity}"#))
            .unwrap_err();
        assert!(
            matches!(error, LoadError::Corrupt(ref why) if why.contains("duplicate field `mixer`")),
            "{error:?}"
        );
    }

    #[test]
    fn file_mixer_keys_keep_other_known_legacy_keys_strict() {
        for version in 1..4 {
            let error =
                load_mixer_fixture(version, r#", "mixer":null, "mixer":{}, "name":"Other""#)
                    .unwrap_err();
            assert!(
                matches!(error, LoadError::Corrupt(ref why) if why.contains("duplicate field `name`")),
                "{error:?}"
            );
        }
    }

    #[test]
    fn file_mixer_keys_refuse_future_header_before_typed_fields() {
        assert!(matches!(
            load_mixer_fixture(5, r#", "mixer":false, "mixer":{}, "channels":[[]]"#),
            Err(LoadError::NewerVersion {
                found: 5,
                supported: 4
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_open_errors_are_propagated() {
        let root = tempfile::tempdir().unwrap();
        let error = RealDisk
            .sync_folder(&root.path().join("missing").join("song.grooviq"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn skipped_asset_entry_retains_refusal_and_source_bytes() {
        let (mut p, mut files) = project_of(1);
        p.arrangement.audio_assets.push(crate::AudioAsset {
            id: 100,
            sample: crate::SampleRef {
                path: "asset.wav".into(),
                file: None,
            },
            source_rate: 48000,
            source_frames: 4,
            original_tempo: None,
            original_key: None,
        });
        files.insert(100, files[&1].clone());
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("skipped.grooviq");
        save(&p, &files, &path).unwrap();
        let good = fs::read(&path).unwrap();
        let loaded = load_with_limits(
            &path,
            Limits {
                sample: 1,
                ..LIMITS
            },
        )
        .unwrap();
        assert!(
            loaded.project.channels[0]
                .sample
                .as_ref()
                .unwrap()
                .file
                .is_some()
        );
        assert!(
            loaded.project.arrangement.audio_assets[0]
                .sample
                .file
                .is_some()
        );
        assert!(validate_save(&loaded.project, &loaded.samples).is_err());
        assert!(save(&loaded.project, &loaded.samples, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), good);
    }

    #[test]
    fn admission_and_writer_share_exact_json_boundary() {
        let mut p = Project::new();
        p.name = "near budget".repeat(100);
        let files = SampleFiles::new();
        let size = project_json_bytes(&p, LIMITS.json).unwrap().len() as u64;
        let limits = Limits {
            json: size,
            ..LIMITS
        };
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("boundary.grooviq");
        validate_save_with_limits(&p, &files, limits).unwrap();
        assert!(serialize_capped(&p, size, false).unwrap().is_empty());
        save_with_limits(&p, &files, &path, false, limits).unwrap();
        assert_eq!(load_with_limits(&path, limits).unwrap().project, p);
        let good = fs::read(&path).unwrap();
        let tight = Limits {
            json: size - 1,
            ..limits
        };
        assert!(validate_save_with_limits(&p, &files, tight).is_err());
        assert!(save_with_limits(&p, &files, &path, false, tight).is_err());
        assert_eq!(fs::read(&path).unwrap(), good);
    }

    #[test]
    fn assets_share_channel_sample_budgets() {
        let (mut p, mut files) = project_of(1);
        p.arrangement.audio_assets.push(crate::AudioAsset {
            id: 100,
            sample: crate::SampleRef {
                path: "asset.wav".into(),
                file: None,
            },
            source_rate: 48000,
            source_frames: 4,
            original_tempo: None,
            original_key: None,
        });
        let sample = files[&1].clone();
        files.insert(100, sample.clone());
        let size = sample.bytes.len() as u64;
        let limits = Limits {
            sample: size,
            total: size,
            ..LIMITS
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asset.grooviq");
        save_with_limits(&p, &files, &path, false, limits).unwrap();
        assert_eq!(load_with_limits(&path, limits).unwrap().samples.len(), 2);
        let good = fs::read(&path).unwrap();
        files.insert(
            100,
            SampleFile {
                name: "different.wav".into(),
                bytes: Arc::new(vec![42; size as usize]),
            },
        );
        assert!(save_with_limits(&p, &files, &path, false, limits).is_err());
        assert_eq!(fs::read(&path).unwrap(), good);
        let tight = Limits {
            sample: size - 1,
            ..limits
        };
        assert!(save_with_limits(&p, &files, &path, false, tight).is_err());
    }

    #[test]
    fn a_save_syncs_its_temp_file_before_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = project_of(1);
        let before = SYNCED.with(|n| n.get());
        save(&p, &samples, &path).unwrap();
        assert_eq!(
            SYNCED.with(|n| n.get()) - before,
            1,
            "the temp file was not synced"
        );
        // A refused save syncs nothing (nothing was written).
        let tight = Limits { json: 10, ..LIMITS };
        save_with_limits(&p, &samples, &path, false, tight).unwrap_err();
        assert_eq!(SYNCED.with(|n| n.get()) - before, 1);
    }
    use crate::model::{Channel, SampleRef};

    /// A tiny valid mono 16-bit WAV (`frames` frames of a ramp), built by hand.
    fn wav_bytes(frames: u32) -> Vec<u8> {
        let data = frames * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&44_100u32.to_le_bytes());
        b.extend_from_slice(&88_200u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            b.extend_from_slice(&((i as i16) * 100).to_le_bytes());
        }
        b
    }

    fn project_of(n: u32) -> (Project, SampleFiles) {
        let mut p = Project::new();
        let mut samples = SampleFiles::new();
        for id in 1..=n {
            p.channels.push(Channel {
                id,
                name: format!("C{id}"),
                color: 0,
                sample: Some(SampleRef {
                    path: PathBuf::from("/nowhere/x.wav"),
                    file: None,
                }),
                instrument: grooviq_engine::InstrumentKind::Sampler,
                sampler: Default::default(),
                synth: Default::default(),
                preset: None,
                volume: 1.0,
                pan: 0.0,
                mute: false,
                solo: false,
                choke: 0,
            });
            samples.insert(
                id,
                SampleFile {
                    name: format!("{id}.wav"),
                    bytes: Arc::new(vec![id as u8; 6]),
                },
            );
        }
        p.next_id = n + 1;
        (p, samples)
    }

    #[test]
    fn diagnostic_labels_preserve_save_refusal_and_decode_data() {
        let names = [
            "Kick".into(),
            "x".repeat(40_000),
            "🎵é\n\u{202e}".repeat(8_000),
            "a\n\u{2066}é".into(),
        ];
        for name in names {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("diagnostic.grooviq");
            let (mut p, mut files) = project_of(2);
            p.name = "Preserved project".into();
            p.channels[0].name = name.clone();
            p.channels[0].volume = 0.37;
            p.channels[1].pan = -0.25;
            p.patterns[0].name = "Preserved pattern".into();
            p.patterns[0].notes.push(crate::Note {
                id: 1,
                channel: 2,
                tick: 0,
                length: 240,
                key: 60,
                velocity: 0.6,
                slide: false,
            });
            p.patterns[0].next_note_id = 2;
            // The first payload is an invalid WAV; the second is valid audio.
            files.get_mut(&2).unwrap().bytes = Arc::new(wav_bytes(8));
            save(&p, &files, &path).unwrap();
            let old = fs::read(&path).unwrap();
            let mut loaded = load(&path).unwrap();
            let mut expected = p.clone();
            for channel in &mut expected.channels {
                channel.sample.as_mut().unwrap().file =
                    Some(format!("samples/{}-{}.wav", channel.id, channel.id));
            }
            assert_eq!(loaded.project, expected);
            assert!(loaded.warnings.is_empty());
            let original = loaded.project.clone();
            let original_files = loaded.samples.clone();
            for case in 0..3 {
                let mut absent = loaded.samples.clone();
                if case == 0 {
                    absent.remove(&1);
                }
                let limits = Limits {
                    sample: if case == 1 { 5 } else { LIMITS.sample },
                    total: if case == 2 { 5 } else { LIMITS.total },
                    ..LIMITS
                };
                assert!(validate_save_with_limits(&loaded.project, &absent, limits).is_err());
                let error =
                    save_with_limits(&loaded.project, &absent, &path, false, limits).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                let text = error.to_string();
                assert!(text.len() <= 400, "save diagnostic bytes: {}", text.len());
                assert!(
                    !text
                        .chars()
                        .any(|c| c.is_control() || c == '\u{202e}' || c == '\u{2066}')
                );
                if name == "Kick" {
                    assert!(text.contains("Kick"));
                }
                assert_eq!(fs::read(&path).unwrap(), old);
            }
            // Isolate each untrusted decoder label as well as their combination.
            for (channel_name, filename) in [
                (name.as_str(), "bad.wav"),
                ("Kick", name.as_str()),
                (name.as_str(), name.as_str()),
            ] {
                loaded.project.channels[0].name = channel_name.into();
                loaded.samples.get_mut(&1).unwrap().name = filename.into();
                let before = loaded.project.clone();
                let sample_before = loaded.samples.clone();
                let decoded = crate::decode_samples(&loaded);
                assert_eq!(decoded.missing.iter().copied().collect::<Vec<_>>(), [1]);
                assert_eq!(decoded.samples.len(), 1);
                assert_eq!(decoded.warnings.len(), 1);
                let warning = &decoded.warnings[0];
                assert!(
                    warning.len() <= 700,
                    "decode diagnostic bytes: {}",
                    warning.len()
                );
                assert!(
                    !warning
                        .chars()
                        .any(|c| c.is_control() || c == '\u{202e}' || c == '\u{2066}')
                );
                if name == "Kick" {
                    assert!(warning.contains("Kick"));
                }
                assert_eq!(loaded.project, before);
                for (id, file) in sample_before {
                    assert_eq!(loaded.samples[&id].name, file.name);
                    assert_eq!(loaded.samples[&id].bytes, file.bytes);
                }
                assert_eq!(crate::decode_samples(&loaded).warnings, decoded.warnings);
            }
            loaded.project = original.clone();
            loaded.samples = original_files;
            save(&loaded.project, &loaded.samples, &path).unwrap();
            let again = load(&path).unwrap();
            assert_eq!(again.project, original);
            for (id, file) in files {
                assert_eq!(again.samples[&id].bytes, file.bytes);
            }
        }
    }

    #[test]
    fn sample_warning_labels_preserve_zip_data_and_protection() {
        let names = [
            "Kick".to_string(),
            "a\n\u{202e}é".to_string(),
            format!("{}end", "🎵é\n\u{2066}".repeat(250_000)),
        ];
        for name in names {
            // Missing, CRC-unreadable, per-sample overflow and total overflow.
            for case in 0..4 {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("song.grooviq");
                let (mut p, samples) = project_of(2);
                p.channels[0].name = name.clone();
                save(&p, &samples, &path).unwrap();
                let initial = load(&path).unwrap();
                let reference = initial.project.channels[0].sample.clone().unwrap();
                if case < 2 {
                    let mut bytes = fs::read(&path).unwrap();
                    let mut zip = ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
                    let entry = zip.by_name(reference.file.as_ref().unwrap()).unwrap();
                    let offset = entry.data_start().unwrap() as usize;
                    drop(entry);
                    drop(zip);
                    if case == 0 {
                        // Rename both ZIP headers without changing project.json's reference.
                        let needle = reference.file.as_ref().unwrap().as_bytes();
                        let offsets: Vec<_> = bytes
                            .windows(needle.len())
                            .enumerate()
                            .filter_map(|(i, b)| (b == needle).then_some(i))
                            .collect();
                        assert_eq!(offsets.len(), 2);
                        for offset in offsets {
                            bytes[offset] = b'X';
                        }
                    } else {
                        bytes[offset] ^= 0xff;
                    }
                    fs::write(&path, bytes).unwrap();
                }
                let old = fs::read(&path).unwrap();
                let loaded = load_with_limits(
                    &path,
                    Limits {
                        sample: if case == 2 { 5 } else { 100 },
                        total: if case == 3 { 5 } else { 100 },
                        ..LIMITS
                    },
                )
                .unwrap();
                let expected_count = if case >= 2 { 2 } else { 1 };
                assert_eq!(loaded.warnings.len(), expected_count);
                for warning in &loaded.warnings {
                    assert!(warning.len() <= 400, "unbounded warning: {}", warning.len());
                    assert!(
                        !warning
                            .chars()
                            .any(|c| c.is_control() || c == '\u{202e}' || c == '\u{2066}')
                    );
                }
                if name == "Kick" {
                    assert!(loaded.warnings[0].contains(&name));
                } else {
                    assert!(loaded.warnings[0].contains('�'));
                }
                assert_eq!(loaded.project.channels[0].name, name);
                let actual = loaded.project.channels[0].sample.as_ref().unwrap();
                assert_eq!(actual.path, reference.path);
                assert!(!loaded.samples.contains_key(&1));
                if case >= 2 {
                    assert_eq!(actual.file, reference.file);
                    assert!(save(&loaded.project, &loaded.samples, &path).is_err());
                    assert_eq!(fs::read(&path).unwrap(), old);
                } else {
                    assert_eq!(actual.file, None);
                    assert_eq!(*loaded.samples[&2].bytes, *samples[&2].bytes);
                    save(&loaded.project, &loaded.samples, &path).unwrap();
                    let again = load(&path).unwrap();
                    assert_eq!(again.project.channels[0].name, name);
                    assert_eq!(
                        again.project.channels[0].sample.as_ref().unwrap().path,
                        reference.path
                    );
                    assert_eq!(
                        again.project.channels[0].sample.as_ref().unwrap().file,
                        None
                    );
                    assert_eq!(*again.samples[&2].bytes, *samples[&2].bytes);
                }
            }
        }
    }

    #[test]
    fn the_total_of_sample_bytes_read_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = project_of(3);
        save(&p, &samples, &path).unwrap();

        // 6 bytes each: a budget of 13 holds two, the third is treated as missing.
        let loaded = load_with_budget(&path, 13).unwrap();
        assert_eq!(loaded.samples.len(), 2);
        assert!(loaded.samples.contains_key(&1) && loaded.samples.contains_key(&2));
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(loaded.warnings[0].starts_with("Channel \"C3\""));
        assert_eq!(load_with_budget(&path, 18).unwrap().samples.len(), 3);
    }

    #[test]
    fn a_deflated_entry_loads_but_never_past_its_declared_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = project_of(1);
        save(&p, &samples, &path).unwrap();
        // Rewrite the zip with the sample deflated (a tool that re-zipped it might).
        let mut zip = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut json = Vec::new();
        zip.by_name(PROJECT_JSON)
            .unwrap()
            .read_to_end(&mut json)
            .unwrap();
        let mut out = ZipWriter::new(File::create(&path).unwrap());
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        out.start_file(PROJECT_JSON, deflated).unwrap();
        out.write_all(&json).unwrap();
        out.start_file("samples/1-1.wav", deflated).unwrap();
        out.write_all(&[7u8; 4096]).unwrap();
        out.finish().unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(*loaded.samples[&1].bytes, vec![7u8; 4096]);
        // The budget counts decompressed bytes: 4096 do not fit in 4095.
        let tight = load_with_budget(&path, 4095).unwrap();
        assert!(tight.samples.is_empty());
        assert_eq!(tight.warnings.len(), 1, "{:?}", tight.warnings);
    }

    #[test]
    fn disk_reads_are_regular_files_within_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_sample_file(dir.path()).is_err(), "a folder");
        let file = dir.path().join("x.wav");
        fs::write(&file, [0u8; 11]).unwrap();
        assert_eq!(read_limited(&file, 11).unwrap().bytes.len(), 11);
        let err = read_limited(&file, 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.wav");
        // Not the `mkfifo` program: a child spawned here holds a copy of every open descriptor
        // between fork and exec, and a `flock` follows the open file description, so another
        // test's "dead" temp file would look locked for that instant (#129).
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `name` is a valid NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(read_sample_file(&fifo).is_err()));
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)),
            Ok(true),
            "opening a FIFO blocked, or it was read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn disk_reads_refuse_devices_and_stop_at_the_limit() {
        // Not a regular file, though it opens and reads (as zero bytes).
        assert!(read_sample_file(Path::new("/dev/null")).is_err());
        // Endless: only the read cap keeps this from running out of memory.
        let err = read_limited(Path::new("/dev/zero"), 1000);
        assert!(err.is_err());
    }

    fn with_sizes(sizes: &[usize]) -> (Project, SampleFiles) {
        let (p, mut samples) = project_of(sizes.len() as u32);
        for (i, &size) in sizes.iter().enumerate() {
            let id = i as u32 + 1;
            samples.insert(
                id,
                SampleFile {
                    name: format!("{id}.wav"),
                    bytes: Arc::new(vec![id as u8; size]),
                },
            );
        }
        (p, samples)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn save_refuses_a_sample_over_the_per_sample_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = with_sizes(&[6, 6]);
        save(&p, &samples, &path).unwrap();
        let old = fs::read(&path).unwrap();

        let (p, samples) = with_sizes(&[6, 11]);
        let limits = Limits {
            sample: 10,
            total: 100,
            json: MAX_JSON_BYTES,
        };
        let err = save_with_limits(&p, &samples, &path, false, limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            "Channel \"C2\": its sample is 11 bytes, over the 10 bytes limit for one sample."
        );
        assert_eq!(fs::read(&path).unwrap(), old, "the old file is intact");
        assert_eq!(leftovers(dir.path()), vec!["a.grooviq"], "nothing written");
        // At the cap exactly is fine.
        let (p, samples) = with_sizes(&[10, 10]);
        save_with_limits(&p, &samples, &path, false, limits).unwrap();
    }

    fn age(path: &Path, seconds: u64) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn a_save_never_touches_a_tmp_file_that_is_not_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = with_sizes(&[6]);
        let refuse = Limits {
            sample: 5,
            total: 100,
            json: MAX_JSON_BYTES,
        };
        // The person's files, named like temp files, a day old.
        let theirs = ["a.grooviq.tmp", "b.grooviq.tmp"];
        for name in theirs {
            let file = dir.path().join(name);
            fs::write(&file, b"mine").unwrap();
            age(&file, 86_400);
        }
        // A refused save, then one that goes through: neither removes them.
        save_with_limits(&p, &samples, &path, false, refuse).unwrap_err();
        assert_eq!(leftovers(dir.path()), theirs);
        save(&p, &samples, &path).unwrap();
        assert_eq!(
            leftovers(dir.path()),
            ["a.grooviq", "a.grooviq.tmp", "b.grooviq.tmp"]
        );
        assert_eq!(fs::read(dir.path().join("a.grooviq.tmp")).unwrap(), b"mine");
    }

    #[test]
    fn a_save_clears_the_old_temp_file_of_a_dead_save_but_not_a_live_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = with_sizes(&[6]);
        let refuse = Limits {
            sample: 5,
            total: 100,
            json: MAX_JSON_BYTES,
        };
        let owned = |old: bool| {
            let temp = grooviq_engine::create_owned_temp(&path, ".tmp").unwrap();
            if old {
                age(&temp.path, 11 * 60);
            }
            temp
        };
        let dead = owned(true);
        let dead_path = dead.path.clone();
        drop(dead.file); // that save's process died: its lock is gone
        let live = owned(true); // a save in another copy, stalled but alive
        let young = owned(false);
        let young_path = young.path.clone();
        drop(young.file);

        // A refused save writes nothing itself, so only its cleanup runs.
        save_with_limits(&p, &samples, &path, false, refuse).unwrap_err();
        assert!(!dead_path.exists(), "dead and old: removed");
        assert!(live.path.exists(), "a live writer's file was removed");
        assert!(young_path.exists(), "young: kept");
    }

    #[test]
    fn an_unsupported_mixer_refusal_does_not_clean_a_dead_save_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (mut project, samples) = with_sizes(&[6]);
        project.mixer.mode = crate::mixer::Mode::Routed;
        project.mixer.tracks[0].mute = true;
        project.validate_mixer().unwrap();

        let dead = grooviq_engine::create_owned_temp(&path, ".tmp").unwrap();
        age(&dead.path, 11 * 60);
        let dead_path = dead.path.clone();
        drop(dead.file);

        let error = save(&project, &samples, &path).unwrap_err();
        let unsupported = crate::mixer::MixerError::UnsupportedTrackControls(0);
        assert_eq!(error.to_string(), unsupported.to_string());
        assert!(
            dead_path.exists(),
            "unsupported routing must refuse before cleanup"
        );
        assert!(
            !path.exists(),
            "unsupported routing must not create a project file"
        );

        // Supported legacy saves still perform their existing stale-temp cleanup.
        project.mixer = crate::mixer::Mixer::default();
        save(&project, &samples, &path).unwrap();
        assert!(!dead_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn save_refuses_samples_over_the_total_cap_and_counts_shared_bytes_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = with_sizes(&[6, 6]);
        save(&p, &samples, &path).unwrap();
        let old = fs::read(&path).unwrap();

        let (p, samples) = with_sizes(&[6, 6, 6]);
        let limits = Limits {
            sample: 10,
            total: 13,
            json: MAX_JSON_BYTES,
        };
        let err = save_with_limits(&p, &samples, &path, false, limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            "Channel \"C3\": its sample would take the project past the 13 bytes limit for all samples."
        );
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(leftovers(dir.path()), vec!["a.grooviq"]);

        // Equal bytes are stored once, so three channels with one sample fit in 13.
        let (p, mut samples) = with_sizes(&[6, 6, 6]);
        let shared = samples[&1].bytes.clone();
        for id in [2, 3] {
            samples.get_mut(&id).unwrap().bytes = shared.clone();
        }
        save_with_limits(&p, &samples, &path, false, limits).unwrap();
    }

    #[test]
    fn an_over_cap_entry_warns_truthfully_is_not_read_from_disk_and_keeps_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (mut p, samples) = with_sizes(&[6, 4]);
        // A valid WAV at the channel's own path: `load` must not look at it.
        let on_disk = dir.path().join("big.wav");
        fs::write(&on_disk, wav_bytes(10)).unwrap();
        p.channels[0].sample.as_mut().unwrap().path = on_disk;
        save(&p, &samples, &path).unwrap();

        // Over the per-sample cap: channel 1 (6 bytes) is too big, channel 2 (4 bytes) loads.
        let loaded = load_with_limits(
            &path,
            Limits {
                sample: 5,
                total: 100,
                json: MAX_JSON_BYTES,
            },
        )
        .unwrap();
        assert_eq!(loaded.samples.keys().copied().collect::<Vec<_>>(), vec![2]);
        assert_eq!(
            loaded.warnings,
            vec!["Channel \"C1\": its sample is larger than 5 bytes, so it was not loaded."]
        );
        let kept =
            |l: &Loaded, i: usize| l.project.channels[i].sample.as_ref().unwrap().file.clone();
        assert_eq!(kept(&loaded, 0).as_deref(), Some("samples/1-1.wav"));
        assert_eq!(kept(&loaded, 1).as_deref(), Some("samples/2-2.wav"));

        // Past the total: channel 1 fits (6 of 8), channel 2 (4 more) does not.
        let loaded = load_with_limits(
            &path,
            Limits {
                sample: 100,
                total: 8,
                json: MAX_JSON_BYTES,
            },
        )
        .unwrap();
        assert_eq!(loaded.samples.keys().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            loaded.warnings,
            vec![
                "Channel \"C2\": its sample would take the project past the 8 bytes limit for all samples, so it was not loaded."
            ]
        );
        assert_eq!(kept(&loaded, 1).as_deref(), Some("samples/2-2.wav"));
    }

    #[test]
    fn save_and_load_hold_project_json_to_one_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = project_of(2);
        save(&p, &samples, &path).unwrap();
        let old = fs::read(&path).unwrap();
        let planned = plan(&p, &samples, LIMITS).unwrap().0;
        let size = project_json_bytes(&planned, u64::MAX).unwrap().len() as u64;
        let budget = |json| Limits { json, ..LIMITS };

        // One byte over: refused before anything is written, the old file kept.
        let err = save_with_limits(&p, &samples, &path, false, budget(size - 1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            format!(
                "The project is too big to save: its notes and settings would take more than the {} bytes a project file can hold. Remove some notes, then save again.",
                size - 1
            )
        );
        assert_eq!(fs::read(&path).unwrap(), old, "the old file is intact");
        assert_eq!(leftovers(dir.path()), vec!["a.grooviq"], "nothing written");
        // Exactly at the budget: saved, and the reader with the same budget opens it.
        save_with_limits(&p, &samples, &path, false, budget(size)).unwrap();
        let loaded = load_with_limits(&path, budget(size)).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.project.channels, planned.channels);
        assert_eq!(loaded.samples.len(), 2);
        // And the reader refuses it a byte tighter: the two budgets are the same number.
        assert!(matches!(
            load_with_limits(&path, budget(size - 1)),
            Err(LoadError::Corrupt(_))
        ));
        // Written compact: no pretty printing.
        let mut zip = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut json = String::new();
        zip.by_name(PROJECT_JSON)
            .unwrap()
            .read_to_string(&mut json)
            .unwrap();
        assert!(!json.contains('\n') && !json.contains(": "), "{json}");
    }

    #[test]
    fn a_save_after_a_load_that_skipped_a_sample_refuses_instead_of_dropping_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.grooviq");
        let (p, samples) = with_sizes(&[6, 4]);
        save(&p, &samples, &path).unwrap();
        let old = fs::read(&path).unwrap();

        let loaded = load_with_limits(
            &path,
            Limits {
                sample: 5,
                total: 100,
                json: MAX_JSON_BYTES,
            },
        )
        .unwrap();
        assert!(!loaded.samples.contains_key(&1));
        let err = save(&loaded.project, &loaded.samples, &path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            err.to_string(),
            "Channel \"C1\": its sample was not loaded, so saving would lose it. Relink the sample or remove the channel first."
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            old,
            "the only copy is still in the file"
        );
        assert_eq!(leftovers(dir.path()), vec!["a.grooviq"]);

        // Removing the channel is the way out; the other sample is kept.
        let mut project = loaded.project;
        project.channels.remove(0);
        save(&project, &loaded.samples, &path).unwrap();
        let again = load(&path).unwrap();
        assert!(again.warnings.is_empty(), "{:?}", again.warnings);
        assert_eq!(*again.samples[&2].bytes, vec![2u8; 4]);

        // A channel whose sample simply was not found (no entry name) saves fine.
        let (mut p, _) = with_sizes(&[6]);
        p.channels[0].sample.as_mut().unwrap().file = None;
        save(&p, &SampleFiles::new(), &path).unwrap();
    }
}
