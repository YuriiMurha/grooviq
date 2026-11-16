//! The `.grooviq` file: a zip holding a versioned `project.json` and every sample's original
//! bytes under `samples/`, so a project opens on another machine, or after the originals are
//! deleted.
//!
//! * Saving is atomic: the zip is written to `<path>.tmp`, synced, then renamed over the target,
//!   so a crash or a full disk leaves the previous file untouched.
//! * The bytes are a function of the project and the samples: entry order, names and timestamps
//!   are fixed, so saving twice gives the same file.
//! * Loading never trusts the file: sizes are capped, and a missing sample is a warning instead of
//!   a failed project. `load` never opens a path the file names; relinking is the user's choice.

use crate::model::{FORMAT, Project, VERSION};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, Write};
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
}

const LIMITS: Limits = Limits {
    sample: MAX_SAMPLE_BYTES,
    total: MAX_TOTAL_BYTES,
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

/// Sample files by channel id.
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
    // Everything that can be refused is refused before the first byte is written.
    let (project, entries) = plan(project, samples, limits)?;
    write_atomic(path, |file| {
        write_zip(file, &project, &entries)?;
        if fail_after_temp {
            return Err(io::Error::other(
                "injected failure after the temp file was written",
            ));
        }
        Ok(())
    })
}

/// Which samples go in the zip, and the project with each channel's `SampleRef.file` set.
/// One entry per distinct byte content (the same `Arc` or equal bytes), in channel order.
///
/// Fails, naming the channel, when a sample is over `limits.sample`, the distinct samples
/// together pass `limits.total` (a file `load` would then not fully open), or a channel still
/// names a zip entry whose bytes are not here (a sample `load` skipped): saving would drop its
/// only copy.
fn plan(
    project: &Project,
    samples: &SampleFiles,
    limits: Limits,
) -> io::Result<(Project, Vec<Entry>)> {
    let mut project = project.clone();
    let mut entries: Vec<Entry> = Vec::new();
    let mut total = 0u64;
    for channel in &mut project.channels {
        let Some(sample_ref) = channel.sample.as_mut() else {
            continue;
        };
        let Some(file) = samples.get(&channel.id) else {
            if sample_ref.file.is_some() {
                return Err(too_big(format!(
                    "Channel \"{}\": its sample was not loaded, so saving would lose it. Relink the sample or remove the channel first.",
                    channel.name
                )));
            }
            continue;
        };
        let size = file.bytes.len() as u64;
        if size > limits.sample {
            return Err(too_big(format!(
                "Channel \"{}\": its sample is {}, over the {} limit for one sample.",
                channel.name,
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
                        "Channel \"{}\": its sample would take the project past the {} limit for all samples.",
                        channel.name,
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
fn write_zip(file: &mut File, project: &Project, entries: &[Entry]) -> io::Result<()> {
    let epoch = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .map_err(|_| io::Error::other("the zip epoch is not a valid timestamp"))?;
    let options = SimpleFileOptions::default()
        .last_modified_time(epoch)
        // Not the host's default (DOS on Windows, Unix elsewhere): the same bytes everywhere.
        .system(zip::System::Unix)
        .unix_permissions(0o644);

    let json = serde_json::to_vec_pretty(&Document {
        format: FORMAT,
        version: VERSION,
        project,
    })
    .map_err(io::Error::other)?;

    let mut zip = ZipWriter::new(BufWriter::new(file));
    zip.start_file(
        PROJECT_JSON,
        options.compression_method(CompressionMethod::Deflated),
    )?;
    zip.write_all(&json)?;
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

/// Runs `write` against `<path>.tmp`, syncs it and renames it over `path`. The temp file is
/// removed on any error, and `path` is only ever replaced by a complete file.
fn write_atomic(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let tmp = temp_path(path);
    let written = (|| {
        let mut file = File::create(&tmp)?;
        write(&mut file)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        // On Windows `rename` replaces an existing file too.
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_folder(path);
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Syncs the folder so the rename itself survives a power cut. Best effort: some file systems
/// refuse to sync a folder, and the file is already in place.
#[cfg(unix)]
fn sync_folder(path: &Path) {
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if let Ok(dir) = File::open(folder) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_folder(_path: &Path) {}

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

    let json = {
        let entry = match zip.by_name(PROJECT_JSON) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => return Err(LoadError::NotAProject),
            Err(e) => return Err(zip_error(e)),
        };
        if entry.size() > MAX_JSON_BYTES {
            return Err(LoadError::Corrupt(format!(
                "{PROJECT_JSON} is larger than {} MB",
                MAX_JSON_BYTES >> 20
            )));
        }
        let mut text = Vec::new();
        entry
            .take(MAX_JSON_BYTES)
            .read_to_end(&mut text)
            .map_err(|e| LoadError::Corrupt(format!("{PROJECT_JSON} cannot be read: {e}")))?;
        text
    };
    let value: Value = serde_json::from_slice(&json)
        .map_err(|e| LoadError::Corrupt(format!("{PROJECT_JSON} is not valid JSON: {e}")))?;
    let value = migrate(value)?;
    let mut project: Project = serde_json::from_value(value)
        .map_err(|e| LoadError::Corrupt(format!("{PROJECT_JSON} does not fit: {e}")))?;

    let mut warnings = project.validate();
    let mut samples = SampleFiles::new();
    let mut entries: HashMap<String, EntryRead> = HashMap::new();
    // Channels whose embedded sample is too big: their entry name stays.
    let mut skipped = HashSet::new();
    for channel in &project.channels {
        let Some(sample_ref) = &channel.sample else {
            continue;
        };
        let embedded = sample_ref.file.as_deref().map(|name| {
            entries
                .entry(name.to_string())
                .or_insert_with(|| read_entry(&mut zip, name, limits.sample, &mut left))
                .clone()
        });
        match embedded {
            Some(EntryRead::Found(found)) => {
                samples.insert(channel.id, found);
                continue;
            }
            Some(EntryRead::TooBig { over_total }) => {
                warnings.push(if over_total {
                    format!(
                        "Channel \"{}\": its sample would take the project past the {} limit for all samples, so it was not loaded.",
                        channel.name,
                        human(limits.total)
                    )
                } else {
                    format!(
                        "Channel \"{}\": its sample is larger than {}, so it was not loaded.",
                        channel.name,
                        human(limits.sample)
                    )
                });
                skipped.insert(channel.id);
                continue;
            }
            Some(EntryRead::Missing) | None => {}
        }
        // Missing or unreadable entry: no bytes. The path in the file is never opened.
        warnings.push(format!(
            "Channel \"{}\": its sample is missing from the project file. Relink it to hear it.",
            channel.name
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
    Ok(Loaded {
        project,
        samples,
        warnings,
    })
}

/// Brings a parsed `project.json` up to [`VERSION`]. Version 1 passes through unchanged; a
/// version 2 will add its upgrade step here, one version at a time.
pub fn migrate(json: Value) -> Result<Value, LoadError> {
    let header = json.as_object().ok_or(LoadError::NotAProject)?;
    if header.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(LoadError::NotAProject);
    }
    let version = match header.get("version").and_then(Value::as_u64) {
        Some(v) if v >= 1 => v,
        _ => {
            return Err(LoadError::Corrupt(
                "its format version is missing or invalid".into(),
            ));
        }
    };
    if version > u64::from(VERSION) {
        return Err(LoadError::NewerVersion {
            found: u32::try_from(version).unwrap_or(u32::MAX),
            supported: VERSION,
        });
    }
    Ok(json)
}

/// What reading a sample's zip entry gave.
#[derive(Clone)]
enum EntryRead {
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
fn read_entry<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
    sample_limit: u64,
    left: &mut u64,
) -> EntryRead {
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
    let mut bytes = Vec::new();
    if entry.take(size + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 != size {
        return EntryRead::Missing;
    }
    *left -= size;
    EntryRead::Found(SampleFile {
        name: original_name(name),
        bytes: Arc::new(bytes),
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

#[cfg(test)]
mod tests {
    use super::*;
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
                sampler: Default::default(),
                volume: 1.0,
                pan: 0.0,
                mute: false,
                solo: false,
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
        if !std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success())
        {
            return; // no mkfifo here
        }
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
