//! The open project: its edits, the engine kept in step with them, dirty state and when to
//! autosave.
//!
//! A [`Session`] owns the [`Project`], each channel's sample bytes (for saving) and decoded
//! buffer (for the engine), and the path it was opened from or saved to. Every edit changes the
//! engine with the smallest message that does it (a mix or sampler value as one command, notes
//! as a new compiled pattern) and changes the model only once the engine took it, so a refused
//! edit changes nothing.
//!
//! Nothing here reads, decodes or writes a file while the caller holds the session: decoding
//! happens before ([`decode_samples`]), and saving is split into [`Session::begin_save`] (a cheap
//! snapshot, the sample bytes are shared), [`SaveJob::write`] (outside the lock) and
//! [`Session::finish_save`].

use crate::backup::prepare_backup_on;
use crate::compile::synth_values;
use crate::compile::{compile_pattern, engine_state, sampler_values};
use crate::file::{Disk, Loaded, LoadedForOpen, RealDisk, SampleFiles, matches_json, stage_save};
use crate::model::{
    BPM, Channel, DEFAULT_KEY, DEFAULT_VELOCITY, EXTENSION, MAX_NOTES_PER_PATTERN, MAX_STEPS, Note,
    Pattern, Project, ProjectKey, SCALES, SampleRef, TimeSignature, VOLUME, is_plain_step,
    roll_channels,
};
use crate::notes::{self, NewNote, NotesAck, NotesEdit, encode_notes};
use crate::recorder::Recorder;
use crate::undo::{Growth, NoteChange, NoteHistory};
use grooviq_dsp::sampler::{self, PARAM_IDS, SPEC, SampleBuf};
use grooviq_dsp::synth;
use grooviq_engine::{
    ChannelId, ChannelMix, Command, ControlError, Controller, EngineState, InstrumentKind,
    MAX_CHANNELS, MAX_CHOKE_GROUP, MAX_SAMPLE_SECONDS, NoteId, RecordEvent, STEP_TICKS,
    decode_sample,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The longest an edit stays out of the autosave (owner decision, issue #27).
pub const AUTOSAVE_PERIOD: Duration = Duration::from_secs(60);
/// How soon a structural change (a channel added or removed, a sample set, the pattern's length)
/// is autosaved.
pub const STRUCTURAL_DELAY: Duration = Duration::from_secs(2);
/// Rolling backups kept per project (and recovered files kept).
pub const MAX_BACKUPS: usize = 10;
/// Inks in the UI's channel palette (`--ink-1` .. `--ink-6`).
const INKS: u8 = 6;
/// The length of a hit added with a click: one step.
const HIT_TICKS: u32 = STEP_TICKS;
/// The most points [`Session::waveform`] draws (a wide screen's pixels, twice over).
pub const MAX_WAVEFORM_POINTS: u32 = 16_384;
/// The synth preset a new synth channel starts from when none is named, and the default patch.
const INIT_PRESET: &str = "Init";

/// One change the UI asks for. JSON: `{"kind": "toggleStep", "channel": 1, "step": 4}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Edit {
    RemoveChannel {
        channel: u32,
    },
    /// `volume` is clamped to 0..=2, `pan` to -1..=1.
    SetChannelMix {
        channel: u32,
        volume: f32,
        pan: f32,
        mute: bool,
        solo: bool,
    },
    /// `param` is a sampler parameter id (`grooviq_dsp::sampler::PARAM_IDS`); the value is
    /// clamped to its range.
    SetSamplerParam {
        channel: u32,
        param: String,
        value: f32,
    },
    /// Adds a hit (key 60, velocity 0.8, one step long) on the step, or removes the hits there.
    ToggleStep {
        channel: u32,
        step: u32,
    },
    /// Sets the velocity (clamped to 0..=1) of the step's hits, adding one if there is none.
    SetStepVelocity {
        channel: u32,
        step: u32,
        velocity: f32,
    },
    /// Removes the step's hits, if any.
    ClearStep {
        channel: u32,
        step: u32,
    },
    /// Clamped to 1..=256. Notes past the end stay in the project, unseen and unplayed, so
    /// lengthening brings them back.
    SetPatternLength {
        steps: u32,
    },
    /// Clamped to 0..=1.
    SetSwing {
        swing: f32,
    },
    /// Clamped to 20..=400.
    SetTempo {
        bpm: f32,
    },
    /// `beats` 1..=16, `unit` 2, 4, 8 or 16.
    SetTimeSignature {
        beats: u8,
        unit: u8,
    },
    /// `param` is a parameter id of the channel's instrument (`grooviq_dsp::sampler::PARAM_IDS`
    /// or `grooviq_dsp::synth::PARAM_IDS`); the value is clamped to its range.
    SetInstrumentParam {
        channel: u32,
        param: String,
        value: f32,
    },
    /// Sets all of a synth channel's parameters from a factory preset ("Init" or one of
    /// `grooviq_dsp::synth::PRESETS`) and remembers its name.
    LoadSynthPreset {
        channel: u32,
        preset: String,
    },
    /// Clamped to 0..=16 (0: no group).
    SetChoke {
        channel: u32,
        group: u8,
    },
    /// The key the piano roll highlights; `null` for none.
    SetProjectKey {
        key: Option<ProjectKey>,
    },
}

impl Edit {
    /// Changes the shape of the project, so it is autosaved within [`STRUCTURAL_DELAY`].
    fn structural(&self) -> bool {
        matches!(
            self,
            Self::RemoveChannel { .. } | Self::SetPatternLength { .. }
        )
    }
}

/// What the UI draws.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    /// The file's name without its extension, or the project's own name while untitled.
    pub name: String,
    pub path: Option<String>,
    /// Changed since it was opened or saved.
    pub dirty: bool,
    pub bpm: f32,
    pub time_signature: TimeSignature,
    pub channels: Vec<ChannelView>,
    pub pattern: PatternView,
    /// When the last autosave was written, in unix seconds.
    pub autosaved_at: Option<u64>,
    pub key: Option<ProjectKey>,
    /// The current pattern's revision: bumped by every note change, and the number
    /// [`Session::notes`] carries.
    pub notes_revision: u32,
    /// Live notes are recorded into the pattern while the transport plays.
    pub recording: bool,
    /// A note change can be undone ([`Session::undo_notes`]).
    pub can_undo: bool,
    /// An undone note change can be made again ([`Session::redo_notes`]).
    pub can_redo: bool,
    /// Candidate history exists in any project domain, as used by global dispatch.
    pub project_can_undo: bool,
    pub project_can_redo: bool,
    /// Counts projects ([`Session::generation`]): requests that edit the project carry it, so
    /// one made for a project that was replaced since is refused.
    pub generation: u64,
    pub pattern_revision: u64,
}

/// A channel's instrument, as the UI names it: `"sampler"` or `"synth"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChannelKind {
    Sampler,
    Synth,
}

/// Where the display name came from, independently of playback decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleNameBasis {
    AvailableFile,
    StoredReference,
}

/// One channel of the rack.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: u32,
    pub name: String,
    /// An index into the palette (0 = `--ink-1`).
    pub color: u8,
    /// The available file name, falling back to the stored reference basename.
    pub sample_name: Option<String>,
    pub sample_name_basis: Option<SampleNameBasis>,
    pub sample_reference: Option<String>,
    /// The channel has a sample, but it is not loaded (not in the file, or it did not decode):
    /// it is silent until relinked.
    pub missing: bool,
    /// The loaded sample's length in frames (`None` without one): the sampler editor keeps a
    /// region at least one frame long.
    pub sample_frames: Option<u64>,
    pub volume: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// Every sampler parameter by id, with its value or default (empty for a synth).
    pub sampler: BTreeMap<String, f32>,
    pub kind: ChannelKind,
    /// Every synth parameter by id, with its value or default (empty for a sampler).
    pub synth: BTreeMap<String, f32>,
    /// The synth preset last loaded.
    pub preset: Option<String>,
    /// Choke group, 0..=16 (0: none).
    pub choke: u8,
    /// The channel has notes that are not plain steps: the rack only previews them, and the
    /// step grid refuses to edit it.
    pub roll: bool,
}

/// The current pattern as a grid.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternView {
    pub id: u32,
    pub name: String,
    pub steps: u32,
    pub swing: f32,
    /// Per channel id: one entry per step, the velocity of the plain step hit there (see
    /// [`is_plain_step`]) or `null`.
    pub cells: BTreeMap<u32, Vec<Option<f32>>>,
}

/// What [`Session::collect_recorded`] did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recorded {
    /// The pattern's new revision, if recorded notes were added.
    pub revision: Option<u32>,
    /// Recording was switched off because recorded notes did not fit: tell the person.
    pub stopped: Option<RecordingStopped>,
}

/// Why recording stopped by itself, and what it could not keep. The `recording-stopped` event.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStopped {
    /// `"noteCap"` (the pattern holds [`MAX_NOTES_PER_PATTERN`]), `"noteIds"` (its note ids
    /// are used up), `"engineBusy"` (notes played after recording switched itself off, before
    /// the engine took the switch) or `"error"` (the pattern refused the notes).
    pub reason: &'static str,
    /// Recorded notes that were not added.
    pub dropped: u32,
    /// A sentence for the person.
    pub message: String,
}

/// What [`Session::open`] gives back.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OpenReport {
    pub view: ProjectView,
    /// Things the person should know: fixes, missing samples, samples that did not decode.
    pub warnings: Vec<String>,
}

/// The decoded samples of a loaded project, by channel id. Made by [`decode_samples`].
#[derive(Debug, Default)]
pub struct DecodedSamples {
    pub samples: HashMap<u32, Arc<SampleBuf>>,
    /// One sentence per sample that did not decode.
    pub warnings: Vec<String>,
    /// Channels that have a sample but no decoded buffer.
    pub missing: BTreeSet<u32>,
}

/// Decodes the embedded sample bytes of every channel (and nothing from disk). Slow: call it
/// before taking the session's lock. Bytes that fail to decode count as missing, with the
/// decoder's message.
///
/// Channels that hold the same bytes (the same allocation, or equal contents with the same file
/// extension) are decoded once and share one buffer: a kick used on four channels costs one
/// decode and one copy in memory. Each channel still gets its own warning if they fail.
pub fn decode_samples(loaded: &Loaded) -> DecodedSamples {
    decode_samples_inner(loaded, None)
}
pub fn decode_samples_budgeted(
    loaded: &Loaded,
    budget: &grooviq_engine::media_budget::MediaBudget,
) -> DecodedSamples {
    decode_samples_inner(loaded, Some(budget))
}
fn decode_samples_inner(
    loaded: &Loaded,
    budget: Option<&grooviq_engine::media_budget::MediaBudget>,
) -> DecodedSamples {
    /// One distinct sound seen so far, and what decoding it gave.
    struct Seen<'a> {
        bytes: &'a Arc<Vec<u8>>,
        extension: Option<&'a str>,
        result: Result<Arc<SampleBuf>, String>,
    }
    let mut out = DecodedSamples::default();
    let mut seen: Vec<Seen<'_>> = Vec::new();
    let sources = loaded
        .project
        .channels
        .iter()
        .filter(|c| c.sample.is_some())
        .map(|c| {
            (
                c.id,
                format!("Channel \"{}\"", crate::model::warning_label(&c.name)),
            )
        })
        .chain(
            loaded
                .project
                .arrangement
                .audio_assets
                .iter()
                .map(|a| (a.id, format!("Audio asset {}", a.id))),
        );
    for (id, label) in sources {
        let Some(file) = loaded.samples.get(&id) else {
            // The loader already said why.
            out.missing.insert(id);
            continue;
        };
        let extension = Path::new(&file.name).extension().and_then(|e| e.to_str());
        let known = seen.iter().position(|s| {
            s.extension == extension
                && (Arc::ptr_eq(s.bytes, &file.bytes) || **s.bytes == *file.bytes)
        });
        let at = known.unwrap_or_else(|| {
            let result = if let Some(budget) = budget {
                budget
                    .original(&file.bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|_| {
                        grooviq_engine::decode_sample_budgeted(
                            &file.bytes,
                            extension,
                            MAX_SAMPLE_SECONDS,
                            budget,
                        )
                        .map_err(|e| e.to_string())
                    })
            } else {
                decode_sample(&file.bytes, extension, MAX_SAMPLE_SECONDS)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            };
            seen.push(Seen {
                bytes: &file.bytes,
                extension,
                result,
            });
            seen.len() - 1
        });
        match &seen[at].result {
            Ok(buf) => {
                out.samples.insert(id, Arc::clone(buf));
            }
            Err(e) => {
                out.missing.insert(id);
                out.warnings.push(format!(
                    "{}: its sample \"{}\" could not be decoded ({e}). Relink it to hear the channel.",
                    label, crate::model::warning_label(&file.name)
                ));
            }
        }
    }
    out
}

/// Budget exhaustion is an admission refusal, not a silently missing decoder result.
pub fn decode_samples_budgeted_checked(
    loaded: &Loaded,
    budget: &grooviq_engine::media_budget::MediaBudget,
) -> Result<DecodedSamples, String> {
    let decoded = decode_samples_budgeted(loaded, budget);
    if let Some(error) = decoded
        .warnings
        .iter()
        .find(|s| s.contains("Retained media exceeds"))
    {
        return Err(error.clone());
    }
    Ok(decoded)
}

/// Why an edit, open or save did not happen. `Display` is a sentence for the person.
#[derive(Debug)]
pub enum SessionError {
    UnknownChannel(u32),
    StepOutOfRange {
        step: u32,
        steps: u32,
    },
    /// The channel's instrument (`device`: "sampler" or "synth") has no such parameter.
    UnknownParam {
        device: &'static str,
        param: String,
    },
    /// A step edit on a channel that holds piano-roll notes (see [`roll_channels`]).
    RollChannel {
        name: String,
    },
    /// A value that is not a number, or a time signature that does not exist.
    InvalidValue(String),
    TooManyChannels,
    /// The pattern holds [`MAX_NOTES_PER_PATTERN`] notes: a file keeps no more.
    TooManyNotes,
    /// The pattern's note id counter is at its end; reopening the project renumbers the notes.
    NoteIdsUsedUp,
    /// The project has no file yet: ask where to save it.
    NoPath,
    /// Another save of this session is still being written.
    SaveRunning,
    /// The engine refused the change.
    Engine(ControlError),
    /// The previous file could not be backed up, so it was not overwritten.
    Backup(io::Error),
    /// `project::save` refused or failed; its message as it is.
    Save(io::Error),
    /// The request was made in a project that was replaced (New, Open, Restore) before it
    /// arrived: nothing was changed.
    ProjectReplaced,
    RecordingSettling,
    StalePatternRevision,
    Pattern(crate::pattern_edit::PatternError),
    PendingRecording(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownChannel(id) => write!(f, "There is no channel {id} in this project."),
            Self::StepOutOfRange { step, steps } => write!(
                f,
                "Step {} is past the end of the pattern ({steps} steps).",
                step.saturating_add(1)
            ),
            Self::UnknownParam { device, param } => {
                write!(f, "The {device} has no setting called \"{param}\".")
            }
            Self::RollChannel { name } => write!(
                f,
                "{name} has piano-roll notes. Edit them in the piano roll (F7)."
            ),
            Self::InvalidValue(what) => write!(f, "{what}"),
            Self::TooManyChannels => write!(
                f,
                "A project holds at most {MAX_CHANNELS} channels; remove one to add another."
            ),
            Self::TooManyNotes => write!(
                f,
                "This pattern already holds {MAX_NOTES_PER_PATTERN} notes, the most a pattern can hold; remove some to add more."
            ),
            Self::NoteIdsUsedUp => write!(
                f,
                "This pattern has used up its note ids. Save the project and open it again to renumber its notes."
            ),
            Self::NoPath => write!(f, "Choose where to save this project."),
            Self::SaveRunning => write!(
                f,
                "The project is already being saved; try again when that save is done."
            ),
            Self::Engine(e) => write!(f, "The audio engine did not take the change ({e})."),
            Self::Backup(e) => write!(
                f,
                "The previous version could not be backed up, so it was not overwritten: {e}."
            ),
            Self::Save(e) => write!(f, "{e}"),
            Self::StalePatternRevision => write!(f, "Patterns changed. Reload the pattern list."),
            Self::Pattern(e) => write!(f, "{e}"),
            Self::RecordingSettling => write!(
                f,
                "Recording is ending. Wait for its final notes, then retry."
            ),
            Self::PendingRecording(message) => write!(f, "Recorded notes are waiting: {message}"),
            Self::ProjectReplaced => write!(
                f,
                "The project changed before this finished, so it was not applied."
            ),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<ControlError> for SessionError {
    fn from(e: ControlError) -> Self {
        match e {
            ControlError::TooManyChannels => Self::TooManyChannels,
            e => Self::Engine(e),
        }
    }
}

/// A save in flight: what to write and where. Made by [`Session::begin_save`].
#[derive(Debug)]
pub struct SaveJob {
    pub project: Project,
    pub samples: SampleFiles,
    pub path: PathBuf,
    revision: u64,
    /// The project it belongs to: [`Session::finish_save`] ignores a job of a project that was
    /// replaced (opened, new, restored) while it was written.
    generation: u64,
}

impl SessionError {
    /// A completed rename whose directory sync failed.
    pub fn directory_unconfirmed(&self) -> bool {
        matches!(self, Self::Save(e) if crate::file::directory_unconfirmed(e))
    }
}

impl SaveJob {
    /// Stages the new file and protects the old bytes before replacement. Failed publication
    /// rolls back only its new backup; uncertain durability retains every copy. Retention
    /// runs only after confirmed publication. Run it without the session's lock.
    pub fn write(&self, backups: &Path) -> Result<(), SessionError> {
        self.write_on(backups, &mut RealDisk)
    }

    /// [`Self::write`] making its file operations with `disk`: the test hook for their order
    /// and for each one failing. Not part of the API.
    #[doc(hidden)]
    pub fn write_on(&self, backups: &Path, disk: &mut impl Disk) -> Result<(), SessionError> {
        let staged = stage_save(&self.project, &self.samples, &self.path, disk)
            .map_err(SessionError::Save)?;
        // On an error here the staged file is dropped, and the old file stays as it was.
        let backup = prepare_backup_on(&self.path, backups, MAX_BACKUPS, disk)
            .map_err(SessionError::Backup)?;
        match staged.commit(disk) {
            Ok(()) => {
                backup.confirm();
                Ok(())
            }
            Err(error) if crate::file::directory_unconfirmed(&error) => {
                // The old bytes remain protected until a later confirmed save.
                Err(SessionError::Save(error))
            }
            Err(error) => {
                if let Err(cleanup) = backup.rollback() {
                    return Err(SessionError::Save(io::Error::other(format!(
                        "{error}; backup candidate retained: {cleanup}"
                    ))));
                }
                Err(SessionError::Save(error))
            }
        }
    }
}

fn mixer_error(error: crate::mixer::MixerError) -> SessionError {
    SessionError::InvalidValue(error.to_string())
}

#[derive(PartialEq, Eq)]
struct HistoryPosition {
    notes: HashMap<u32, u64>,
    // Mixer and arrangement deltas share one stack.
    arrangement: Option<u64>,
    // A sampler's reset can be undone directly, out of order with other channels.
    trim: Vec<u64>,
}

struct SavedState {
    position: HistoryPosition,
    project_json: Vec<u8>,
    files: SampleFiles,
}

/// The open project. See the module docs.
pub struct Session {
    controller: Arc<Mutex<Controller>>,
    media_budget: grooviq_engine::media_budget::MediaBudget,
    media_requests: HashMap<u32, MediaIntent>,
    latest_media_requests: HashMap<u32, u64>,
    media_tickets: HashMap<u64, Arc<std::sync::atomic::AtomicBool>>,
    cancelled_media_tickets: VecDeque<u64>,
    media_peaks: HashMap<u32, Arc<grooviq_engine::waveform::PeakPyramid>>,
    project: Project,
    /// Each channel's sample bytes, saved with the project.
    files: SampleFiles,
    /// Each channel's decoded sample, for the engine.
    decoded: HashMap<u32, Arc<SampleBuf>>,
    path: Option<PathBuf>,
    /// Counts changes; a save clears `dirty` only if none came in while it was written.
    revision: u64,
    saved: Option<SavedState>,
    dirty: bool,
    last_history_order: Option<u64>,
    /// Counts projects: opening, starting a new one or restoring one moves it on.
    generation: u64,
    /// A save is being written (between `begin_save` and `finish_save` or `save_failed`).
    saving: bool,
    /// The first change since the last autosave (or save).
    pending_since: Option<Instant>,
    /// The first structural change since the last autosave (or save).
    structural_since: Option<Instant>,
    autosaved_at: Option<u64>,
    /// Pairs recorded live notes into notes.
    recorder: Recorder,
    /// Recording is switched on (it takes notes while the transport plays).
    recording: bool,
    recovery_stop_mark: Option<u32>,
    /// Recorded notes completed but not in the pattern yet: the engine did not take the
    /// pattern (its queue was full). Retried on the next collect. Each with its take (see
    /// `grooviq_engine::RecordEvent::Note`).
    pending: Vec<(u32, NewNote)>,
    /// Recording switched itself off (the pattern was full) but the engine's queue refused the
    /// switch: it is sent again, with its barrier, on every collect until the queue takes it.
    /// The recorder refuses notes from the decision until that barrier is read. Turning
    /// recording on (or off) by hand sends its own barrier instead; a new project keeps it
    /// (the engine records until it takes the switch).
    owed_disarm: bool,
    /// Accepted UI presses, kept off the audio thread until release or channel replacement.
    live_presses: Mutex<Vec<LivePress>>,
    /// The current pattern's note changes, for undo and redo.
    history: NoteHistory,
    pattern_histories: HashMap<u32, NoteHistory>,
    precommit: Option<PrecommitValidator>,
    arrangement_history: crate::arrangement_history::ArrangementHistory,
    trim_history: sampler_trim::TrimHistory,
    trim_identities: HashMap<u32, (u64, u64)>,
    trim_epoch: u64,
    history_limits: crate::HistoryLimits,
    arrangement_revision: u64,
    pattern_note_floors: HashMap<u32, (u32, u32)>,
    compiled_song: grooviq_engine::SongData,
    song_error: Option<String>,
    song_note_limit: usize,
    patterns_revision: u64,
    record_settle: Option<u32>,
}

enum PendingCommit {
    Empty,
    Deferred,
    Applied(u32),
}

struct LivePress {
    channel: u32,
    key: u8,
    press: NoteId,
    instance: u64,
    releasing: bool,
}

fn lock(controller: &Mutex<Controller>) -> MutexGuard<'_, Controller> {
    controller.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `key` if it is a MIDI key (0..=127).
fn checked_key(key: u8) -> Result<u8, SessionError> {
    if key > 127 {
        return Err(SessionError::InvalidValue(format!(
            "Key {key} is outside 0 to 127."
        )));
    }
    Ok(key)
}

fn live_note(channel: u32, key: u8, velocity: f32, press: NoteId) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity,
        on: true,
        id: press,
    }
}

fn live_release(channel: u32, key: u8, press: NoteId) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.0,
        on: false,
        id: press,
    }
}

/// `value` if it is a number, else `InvalidValue` naming `what`.
fn finite(value: f32, what: &str) -> Result<f32, SessionError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(SessionError::InvalidValue(format!(
            "The {what} must be a number."
        )))
    }
}

/// T7 installs song expansion admission here; file admission always runs first.
pub type PrecommitValidator = Arc<dyn Fn(&Project) -> Result<(), String> + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternsView {
    pub generation: u64,
    pub revision: u64,
    pub arrangement_revision: u64,
    pub selected: u32,
    pub patterns: Vec<PatternSummary>,
    pub view: ProjectView,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternSummary {
    pub id: u32,
    pub name: String,
    pub revision: u32,
    pub steps: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum PatternReply {
    Applied {
        state: PatternsView,
        settlement: PatternSettlement,
    },
    Refused {
        reason: crate::pattern_edit::RefusalReason,
        message: String,
        state: PatternsView,
        settlement: PatternSettlement,
    },
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternSettlement {
    pub recording_disarmed: bool,
    pub waiting: bool,
}

/// A gesture's origin, captured before it can be queued or held.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatternStamp {
    pub generation: u64,
    pub pattern_id: u32,
    pub revision: u32,
}

impl Session {
    /// An untitled, empty project, with the engine set to match it.
    pub fn new(controller: Arc<Mutex<Controller>>) -> Self {
        let session = Self {
            controller,
            project: Project::new(),
            media_budget: Default::default(),
            media_requests: Default::default(),
            latest_media_requests: Default::default(),
            media_tickets: Default::default(),
            cancelled_media_tickets: Default::default(),
            media_peaks: Default::default(),
            files: SampleFiles::new(),
            decoded: HashMap::new(),
            path: None,
            revision: 0,
            saved: None,
            dirty: false,
            last_history_order: None,
            generation: 0,
            saving: false,
            pending_since: None,
            structural_since: None,
            autosaved_at: None,
            recorder: Recorder::new(),
            recording: false,
            recovery_stop_mark: None,
            pending: Vec::new(),
            owed_disarm: false,
            live_presses: Mutex::new(Vec::new()),
            history: NoteHistory::default(),
            pattern_histories: HashMap::new(),
            precommit: None,
            arrangement_history: Default::default(),
            trim_history: Default::default(),
            trim_identities: Default::default(),
            trim_epoch: 0,
            history_limits: Default::default(),
            arrangement_revision: 0,
            pattern_note_floors: HashMap::new(),
            compiled_song: Default::default(),
            song_error: None,
            song_note_limit: grooviq_engine::MAX_SONG_NOTES,
            patterns_revision: 0,
            record_settle: None,
        };
        // A fresh queue always has room for a whole state.
        let synced = session.sync_engine();
        debug_assert!(synced.is_ok(), "{synced:?}");
        let mut session = session;
        let mark = lock(&session.controller).record_mark();
        session.recorder.reset_until_mark(mark);
        session
    }

    pub fn pattern_stamp(&self) -> PatternStamp {
        PatternStamp {
            generation: self.generation,
            pattern_id: self.project.current_pattern,
            revision: self.current().revision,
        }
    }
    fn check_stamp(&self, stamp: PatternStamp) -> Result<(), SessionError> {
        if stamp.generation != self.generation {
            return Err(SessionError::ProjectReplaced);
        }
        if stamp.pattern_id != self.project.current_pattern
            || stamp.revision != self.current().revision
        {
            return Err(SessionError::InvalidValue(
                "The pattern changed before this gesture finished. Reload its notes.".into(),
            ));
        }
        Ok(())
    }
    pub fn apply_stamped(
        &mut self,
        stamp: PatternStamp,
        edit: Edit,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        self.check_stamp(stamp)?;
        self.apply(edit, now)
    }
    pub fn history_stamped(
        &mut self,
        stamp: PatternStamp,
        undo: bool,
        now: Instant,
    ) -> Result<Option<NotesAck>, SessionError> {
        self.check_stamp(stamp)?;
        self.step_history(undo, now)
    }
    pub fn pattern_command(
        &mut self,
        generation: u64,
        revision: u64,
        edit: crate::pattern_edit::PatternEdit,
        now: Instant,
    ) -> PatternReply {
        use crate::pattern_edit::RefusalReason as R;
        let was_recording = self.recording;
        let result = self.edit_patterns(generation, revision, edit, now);
        let settlement = PatternSettlement {
            recording_disarmed: was_recording && !self.recording,
            waiting: self.record_settle.is_some() || !self.pending.is_empty() || self.owed_disarm,
        };
        match result {
            Ok(state) => PatternReply::Applied { state, settlement },
            Err(e) => {
                let message = e.to_string();
                let reason = match &e {
                    SessionError::ProjectReplaced => R::ProjectReplaced,
                    SessionError::RecordingSettling => R::RecordingSettling,
                    SessionError::PendingRecording(_) => R::PendingRecording,
                    SessionError::Engine(_) => R::EngineBusy,
                    SessionError::Save(_) => R::FileBudget,
                    SessionError::StalePatternRevision => R::StaleRevision,
                    SessionError::Pattern(error) => error.reason(),
                    _ => R::Validation,
                };
                PatternReply::Refused {
                    reason,
                    message,
                    state: self.patterns(),
                    settlement,
                }
            }
        }
    }

    fn staged_histories(&self) -> crate::arrangement_history::Histories {
        let mut current = self.history.clone();
        current.staged = true;
        let mut patterns = self.pattern_histories.clone();
        for h in patterns.values_mut() {
            h.staged = true;
        }
        crate::arrangement_history::Histories {
            current,
            patterns,
            arrangement: self.arrangement_history.clone(),
            trim: self.trim_history.clone(),
            floors: self.pattern_note_floors.clone(),
            protected: Default::default(),
        }
    }
    fn install_histories(&mut self, mut h: crate::arrangement_history::Histories) {
        let is_new = |order| self.last_history_order.is_none_or(|last| order > last);
        let new_steps = h.protected.iter().filter(|&&order| is_new(order)).count();
        if self.saved.is_some() {
            // Admission protects every new step. A missing step was evicted or its redo
            // branch was discarded. An empty stack can now stand for a different base.
            let before = Self::history_steps(
                &self.history,
                &self.pattern_histories,
                &self.arrangement_history,
                &self.trim_history,
            );
            let after = Self::history_steps(&h.current, &h.patterns, &h.arrangement, &h.trim);
            let replaced_media = h.arrangement.undo.last().is_some_and(|step| {
                is_new(step.order) && step.media.iter().any(|(_, after)| after.file.is_some())
            });
            if after < before + new_steps || replaced_media {
                self.saved = None;
            }
        }
        if let Some(&order) = h.protected.iter().max()
            && self.last_history_order.is_none_or(|last| order > last)
        {
            self.last_history_order = Some(order);
        }
        h.current.staged = false;
        for p in h.patterns.values_mut() {
            p.staged = false;
        }
        self.history = h.current;
        self.pattern_histories = h.patterns;
        self.arrangement_history = h.arrangement;
        self.trim_history = h.trim;
        self.pattern_note_floors = h.floors;
    }
    fn history_steps(
        current: &NoteHistory,
        patterns: &HashMap<u32, NoteHistory>,
        arrangement: &crate::arrangement_history::ArrangementHistory,
        trim: &sampler_trim::TrimHistory,
    ) -> usize {
        current.retained().0
            + patterns.values().map(|h| h.retained().0).sum::<usize>()
            + arrangement.undo.len()
            + arrangement.redo.len()
            + trim.undo.len()
            + trim.redo.len()
    }
    fn admit_histories(
        &self,
        h: &mut crate::arrangement_history::Histories,
    ) -> Result<(), SessionError> {
        h.admit(self.history_limits, self.project.current_pattern)
            .map_err(SessionError::InvalidValue)
    }
    #[doc(hidden)]
    pub fn set_history_limits(&mut self, limits: crate::HistoryLimits) {
        self.history_limits = crate::HistoryLimits {
            steps: limits.steps.min(200),
            notes: limits.notes.min(2_000_000),
            bytes: limits.bytes.min(64 * 1024 * 1024),
        };
    }
    #[doc(hidden)]
    pub fn history_usage(&self) -> (usize, usize, usize) {
        crate::arrangement_history::Histories::usage_of(
            &self.history,
            &self.pattern_histories,
            &self.arrangement_history,
            &self.pattern_note_floors,
            &self.trim_history,
        )
    }

    #[doc(hidden)]
    pub fn retained_note_history(&self) -> (usize, usize) {
        self.pattern_histories
            .values()
            .fold(self.history.retained(), |(s, n), h| {
                let (hs, hn) = h.retained();
                (s + hs, n + hn)
            })
    }

    pub fn set_precommit_validator(&mut self, validator: Option<PrecommitValidator>) {
        self.precommit = validator;
    }

    pub fn media_budget(&self) -> grooviq_engine::media_budget::MediaBudget {
        self.media_budget.clone()
    }
    #[doc(hidden)]
    pub fn set_media_budget(&mut self, budget: grooviq_engine::media_budget::MediaBudget) {
        self.media_budget = budget;
    }
    fn admit_media(
        &self,
        files: &SampleFiles,
        decoded: &HashMap<u32, Arc<SampleBuf>>,
    ) -> Result<(), SessionError> {
        for file in files.values() {
            self.media_budget
                .original(&file.bytes)
                .map_err(|e| SessionError::InvalidValue(e.to_string()))?;
        }
        for sample in decoded.values() {
            self.media_budget
                .sample(sample)
                .map_err(|e| SessionError::InvalidValue(e.to_string()))?;
        }
        Ok(())
    }
    pub fn arrangement_view(&self) -> crate::arrangement_edit::ArrangementView {
        crate::arrangement_edit::ArrangementView {
            generation: self.generation,
            revision: self.arrangement_revision,
            arrangement: self.project.arrangement.clone(),
            next_id: self.project.next_id,
        }
    }
    fn arrangement_stamp(&self, generation: u64, revision: u64) -> Result<(), String> {
        if generation != self.generation {
            return Err("Project replaced; reload arrangement".into());
        }
        if revision != self.arrangement_revision {
            return Err("Stale arrangement revision; reload arrangement".into());
        }
        if revision >= 9_007_199_254_740_991 {
            return Err("Arrangement revisions exhausted; reopen project".into());
        }
        Ok(())
    }
    pub fn edit_arrangement(
        &mut self,
        generation: u64,
        revision: u64,
        ops: Vec<crate::arrangement_edit::ArrangementEdit>,
        now: Instant,
    ) -> Result<crate::arrangement_edit::ArrangementAck, crate::arrangement_edit::ArrangementRefusal>
    {
        let result = (|| -> Result<_, String> {
            self.arrangement_stamp(generation, revision)?;
            if ops.len() > crate::MAX_CLIPS {
                return Err("Arrangement operation limit exceeded".into());
            }
            let mut candidate = self.project.clone();
            let ids = crate::arrangement_edit::apply(&mut candidate, ops)?;
            let playlist_tracks: HashSet<_> =
                candidate.arrangement.tracks.iter().map(|t| t.id).collect();
            candidate.mixer.sources.retain(|a| match a.source {
                crate::mixer::SourceKey::AudioTrack(id) => playlist_tracks.contains(&id),
                crate::mixer::SourceKey::Channel(_) => true,
            });
            candidate.mixer.refresh_mode();
            if candidate == self.project {
                return Ok((vec![], false));
            }
            let mut change = crate::arrangement_history::Change::between(&self.project, &candidate);
            let mut files = self.files.clone();
            let mut decoded = self.decoded.clone();
            let mut peaks = self.media_peaks.clone();
            for asset in self.project.arrangement.audio_assets.iter().filter(|a| {
                !candidate
                    .arrangement
                    .audio_assets
                    .iter()
                    .any(|b| b.id == a.id)
            }) {
                change.media.push((
                    self.media_version(asset.id),
                    crate::arrangement_history::MediaVersion {
                        id: asset.id,
                        file: None,
                        decoded: None,
                        peaks: None,
                    },
                ));
                files.remove(&asset.id);
                decoded.remove(&asset.id);
                peaks.remove(&asset.id);
            }
            let media_changed = !change.media.is_empty();
            let mut histories = self.staged_histories();
            histories.clear_redo();
            histories.protected.insert(change.order);
            histories.arrangement.undo.push(change);
            self.admit_histories(&mut histories)
                .map_err(|e| e.to_string())?;
            if media_changed {
                self.publish_media_candidate(&candidate, &files, &decoded)
                    .map_err(|e| e.to_string())?;
            } else {
                self.publish_candidate(&candidate)
                    .map_err(|e| e.to_string())?;
            }
            self.files = files;
            self.decoded = decoded;
            self.media_peaks = peaks;
            let patterns_changed = self.project.patterns != candidate.patterns;
            self.project = candidate;
            self.install_histories(histories);
            if patterns_changed {
                self.patterns_revision += 1;
            }
            self.changed(true, now);
            Ok((ids, true))
        })();
        match result {
            Ok((created_ids, changed)) => Ok(crate::arrangement_edit::ArrangementAck {
                state: self.arrangement_view(),
                created_ids,
                changed,
            }),
            Err(message) => Err(crate::arrangement_edit::ArrangementRefusal {
                message,
                reload: true,
                state: Box::new(self.arrangement_view()),
            }),
        }
    }
    /// Typed project-wide dispatch; legacy notes_undo remains restricted to selected pattern.
    pub fn step_project_history(
        &mut self,
        generation: u64,
        revision: u64,
        undo: bool,
        now: Instant,
    ) -> Result<crate::arrangement_edit::HistoryAck, crate::arrangement_edit::ArrangementRefusal>
    {
        use crate::arrangement_edit::{ArrangementRefusal, HistoryAck, HistoryEffect};
        let result = (|| -> Result<HistoryEffect, String> {
            self.arrangement_stamp(generation, revision)?;
            let note = std::iter::once((self.project.current_pattern, &self.history))
                .chain(self.pattern_histories.iter().map(|(id, h)| (*id, h)))
                .filter_map(|(id, h)| h.next_order(undo).map(|o| (o, Some(id))));
            let arr = if undo {
                self.arrangement_history.undo.last()
            } else {
                self.arrangement_history.redo.last()
            };
            let mut choices: Vec<_> = note.chain(arr.map(|s| (s.order, None))).collect();
            choices.sort_by_key(|x| x.0);
            let chosen = if undo {
                choices.last()
            } else {
                choices.first()
            };
            let trim = if undo {
                self.trim_history.undo.last()
            } else {
                self.trim_history.redo.last()
            };
            if let Some(trim) = trim
                && chosen.is_none_or(|(order, _)| {
                    if undo {
                        trim.order > *order
                    } else {
                        trim.order < *order
                    }
                })
            {
                let channel = trim.channel;
                self.step_trim_history(channel, trim.order, undo, now)
                    .map_err(|e| e.to_string())?;
                return Ok(HistoryEffect::SamplerTrim { channel });
            }
            let Some((_, target)) = chosen else {
                return Ok(HistoryEffect::Empty);
            };
            if let Some(id) = *target {
                if id == self.project.current_pattern {
                    self.step_history(undo, now).map_err(|e| e.to_string())?;
                } else {
                    let h = self
                        .pattern_histories
                        .get(&id)
                        .ok_or("Missing pattern history")?;
                    let mut candidate = self.project.clone();
                    let p = candidate
                        .patterns
                        .iter_mut()
                        .find(|p| p.id == id)
                        .ok_or("Pattern removed")?;
                    let change =
                        if undo { h.next_undo() } else { h.next_redo() }.ok_or("History empty")?;
                    change.apply(p, undo).map_err(|e| e.to_string())?;
                    p.steps = h.length_after(undo);
                    p.revision = p
                        .revision
                        .checked_add(1)
                        .ok_or("Pattern revision exhausted")?;
                    let mut histories = self.staged_histories();
                    let h = histories.patterns.get_mut(&id).unwrap();
                    if undo {
                        h.undone()
                    } else {
                        h.redone()
                    };
                    histories.current.close();
                    for h in histories.patterns.values_mut() {
                        h.close();
                    }
                    self.admit_histories(&mut histories)
                        .map_err(|e| e.to_string())?;
                    self.publish_candidate(&candidate)
                        .map_err(|e| e.to_string())?;
                    self.project = candidate;
                    self.install_histories(histories);
                    self.changed(true, now);
                }
                Ok(HistoryEffect::Notes { pattern: id })
            } else {
                let mut histories = self.staged_histories();
                histories.current.close();
                for h in histories.patterns.values_mut() {
                    h.close();
                }
                let change = if undo {
                    histories.arrangement.undo.pop()
                } else {
                    histories.arrangement.redo.pop()
                }
                .ok_or("History empty")?;
                let mut candidate = self.project.clone();
                let mut floors = self.pattern_note_floors.clone();
                for p in &self.project.patterns {
                    floors
                        .entry(p.id)
                        .and_modify(|n| {
                            n.0 = n.0.max(p.next_note_id);
                            n.1 = n.1.max(p.revision);
                        })
                        .or_insert((p.next_note_id, p.revision));
                }
                change.apply(&mut candidate, undo);
                for p in &mut candidate.patterns {
                    if let Some(floor) = floors.get(&p.id) {
                        p.next_note_id = p.next_note_id.max(floor.0);
                        if change.patterns.iter().any(|d| {
                            d.before.as_ref().is_some_and(|(_, old)| old.id == p.id)
                                || d.after.as_ref().is_some_and(|(_, new)| new.id == p.id)
                        }) {
                            p.revision = p
                                .revision
                                .max(floor.1)
                                .checked_add(1)
                                .ok_or("Pattern revisions exhausted")?;
                        }
                    }
                }
                if !candidate
                    .patterns
                    .iter()
                    .any(|p| p.id == candidate.current_pattern)
                {
                    candidate.current_pattern = candidate.patterns[0].id;
                }
                let selection_changed = candidate.current_pattern != self.project.current_pattern;
                if selection_changed {
                    let selected = histories
                        .patterns
                        .remove(&candidate.current_pattern)
                        .unwrap_or_default();
                    let previous = std::mem::replace(&mut histories.current, selected);
                    if previous.can_undo() || previous.can_redo() {
                        histories
                            .patterns
                            .insert(self.project.current_pattern, previous);
                    }
                }
                let patterns_changed = !change.patterns.is_empty() || selection_changed;
                let media = change.media.clone();
                let mut files = self.files.clone();
                let mut decoded = self.decoded.clone();
                let mut peaks = self.media_peaks.clone();
                for (a, b) in &media {
                    let v = if undo { a } else { b };
                    if let Some(file) = &v.file {
                        files.insert(v.id, file.clone());
                    } else {
                        files.remove(&v.id);
                    }
                    if let Some(sample) = &v.decoded {
                        decoded.insert(v.id, sample.clone());
                    } else {
                        decoded.remove(&v.id);
                    }
                    if let Some(p) = &v.peaks {
                        peaks.insert(v.id, p.clone());
                    } else {
                        peaks.remove(&v.id);
                    }
                }

                // Shared allocators never rewind. A restored pattern's note allocator is
                // bounded by retained note identities; new pattern IDs never alias old ones.
                histories.protected.insert(change.order);
                if undo {
                    histories.arrangement.redo.push(change)
                } else {
                    histories.arrangement.undo.push(change)
                };
                histories.floors = floors;
                self.admit_histories(&mut histories)
                    .map_err(|e| e.to_string())?;
                if media.is_empty() {
                    self.publish_candidate(&candidate)
                        .map_err(|e| e.to_string())?;
                } else {
                    self.publish_media_candidate(&candidate, &files, &decoded)
                        .map_err(|e| e.to_string())?;
                }
                self.project = candidate;
                self.files = files;
                self.decoded = decoded;
                self.media_peaks = peaks;
                self.install_histories(histories);
                if patterns_changed {
                    self.patterns_revision += 1;
                }
                self.changed(true, now);
                Ok(HistoryEffect::Arrangement)
            }
        })();
        result
            .map(|effect| HistoryAck {
                effect,
                state: self.arrangement_view(),
            })
            .map_err(|message| ArrangementRefusal {
                message,
                reload: true,
                state: Box::new(self.arrangement_view()),
            })
    }

    /// Persistent compilation diagnostic for editable but unplayable loaded songs.
    pub fn song_error(&self) -> Option<&str> {
        self.song_error.as_deref()
    }

    #[doc(hidden)]
    pub fn set_song_note_limit(&mut self, limit: usize) {
        self.song_note_limit = limit.min(grooviq_engine::MAX_SONG_NOTES);
    }

    fn same_audio(&self, project: &Project) -> bool {
        fn runtime(p: &Project) -> Vec<(u32, u64, &crate::ClipContent, u32, u64)> {
            let tracks: HashMap<_, _> = p
                .arrangement
                .tracks
                .iter()
                .map(|t| (t.id, t.mute))
                .collect();
            let assets: HashMap<_, _> = p
                .arrangement
                .audio_assets
                .iter()
                .map(|a| (a.id, a))
                .collect();
            let mut clips: Vec<_> = p
                .arrangement
                .clips
                .iter()
                .filter_map(|c| {
                    if let crate::ClipContent::Audio { asset, .. } = &c.content
                        && !c.muted
                        && tracks.get(&c.track) == Some(&false)
                        && let Some(a) = assets.get(asset)
                    {
                        Some((
                            c.id,
                            c.start_tick,
                            &c.content,
                            a.source_rate,
                            a.source_frames,
                        ))
                    } else {
                        None
                    }
                })
                .collect();
            clips.sort_unstable_by_key(|c| c.0);
            clips
        }
        // Metadata and equivalent track organization do not affect the runtime bank.
        // prepare_song_with also checks decoded allocation identity before it reuses the bank.
        runtime(&self.project) == runtime(project)
    }

    fn prepare_song(&self, project: &Project) -> Result<grooviq_engine::SongData, SessionError> {
        self.prepare_song_with(project, &self.decoded)
    }

    fn prepare_song_with(
        &self,
        project: &Project,
        decoded: &HashMap<u32, Arc<SampleBuf>>,
    ) -> Result<grooviq_engine::SongData, SessionError> {
        let before = &self.project.arrangement.audio_assets;
        let after = &project.arrangement.audio_assets;
        let same_assets = before.len() == after.len()
            && before.iter().zip(after).all(|(a, b)| {
                a.id == b.id
                    && a.source_rate == b.source_rate
                    && a.source_frames == b.source_frames
                    && match (self.decoded.get(&a.id), decoded.get(&b.id)) {
                        (Some(before), Some(after)) => Arc::ptr_eq(before, after),
                        (None, None) => true,
                        _ => false,
                    }
            });
        crate::compile_song::compile_song(
            project,
            decoded,
            (self.song_error.is_none() && same_assets && self.same_audio(project))
                .then_some(&self.compiled_song.audio),
            self.song_note_limit,
        )
        .map_err(SessionError::InvalidValue)
    }

    fn timeline(
        project: &Project,
        song: grooviq_engine::SongData,
    ) -> grooviq_engine::TimelineUpdate {
        let pattern = project
            .patterns
            .iter()
            .find(|p| p.id == project.current_pattern)
            .or_else(|| project.patterns.first())
            .map(|p| compile_pattern(project, p))
            .unwrap_or_default();
        grooviq_engine::TimelineUpdate {
            remove_channel: None,
            pattern: Box::new(pattern),
            song: Box::new(song),
            bpm: project.bpm,
            mode: match project.play_mode {
                crate::PlayMode::Pattern => grooviq_engine::PlaybackMode::Pattern,
                crate::PlayMode::Song => grooviq_engine::PlaybackMode::Song,
            },
            region: project
                .arrangement
                .loop_region
                .as_ref()
                .filter(|r| r.enabled)
                .map(|r| grooviq_engine::SongLoop {
                    start_tick: r.start_tick,
                    end_tick: r.end_tick,
                }),
        }
    }

    fn publish_candidate(&mut self, project: &Project) -> Result<(), SessionError> {
        self.arrangement_stamp(self.generation, self.arrangement_revision)
            .map_err(SessionError::InvalidValue)?;
        let song = self.admit(project)?;
        self.publish_prepared_candidate(project, song, None)
    }

    pub fn set_play_mode(
        &mut self,
        mode: crate::PlayMode,
        now: Instant,
    ) -> Result<(), SessionError> {
        if self.project.play_mode == mode {
            return Ok(());
        }
        let mut candidate = self.project.clone();
        candidate.play_mode = mode;
        if mode == crate::PlayMode::Pattern && self.song_error.is_some() {
            crate::validate_save(&candidate, &self.files).map_err(SessionError::Save)?;
            let mut update = Self::timeline(&candidate, self.compiled_song.clone());
            update.region = None;
            lock(&self.controller).set_timeline(update)?;
        } else {
            self.publish_candidate(&candidate)?;
        }
        self.project = candidate;
        self.changed_outside_history(true, now);
        Ok(())
    }

    pub fn play(&self) -> Result<(), SessionError> {
        if self.project.play_mode == crate::PlayMode::Song
            && let Some(error) = &self.song_error
        {
            return Err(SessionError::InvalidValue(error.clone()));
        }
        self.send(Command::Play)
    }

    fn admit(&self, project: &Project) -> Result<grooviq_engine::SongData, SessionError> {
        crate::validate_save(project, &self.files).map_err(SessionError::Save)?;
        if let Some(validate) = &self.precommit {
            validate(project).map_err(SessionError::InvalidValue)?;
        }
        self.prepare_song(project)
    }

    pub fn patterns(&self) -> PatternsView {
        PatternsView {
            generation: self.generation,
            revision: self.patterns_revision,
            arrangement_revision: self.arrangement_revision,
            selected: self.project.current_pattern,
            patterns: self
                .project
                .patterns
                .iter()
                .map(|p| PatternSummary {
                    id: p.id,
                    name: p.name.clone(),
                    revision: p.revision,
                    steps: p.steps,
                })
                .collect(),
            view: self.view(),
        }
    }

    pub fn edit_patterns(
        &mut self,
        generation: u64,
        revision: u64,
        edit: crate::pattern_edit::PatternEdit,
        now: Instant,
    ) -> Result<PatternsView, SessionError> {
        if generation != self.generation {
            return Err(SessionError::ProjectReplaced);
        }
        if revision != self.patterns_revision {
            return Err(SessionError::StalePatternRevision);
        }
        if matches!(edit, crate::pattern_edit::PatternEdit::Settle) {
            self.settle_recording()?;
            return Ok(self.patterns());
        }
        use crate::pattern_edit::PatternEdit;
        let invalidates_arrangement_history = match &edit {
            PatternEdit::Delete { pattern } => self
                .project
                .patterns
                .iter()
                .position(|p| p.id == *pattern)
                .map(|index| (*pattern, Some(index))),
            PatternEdit::MakeUnique { clip } => {
                self.project
                    .arrangement
                    .clips
                    .iter()
                    .find_map(|c| match c.content {
                        crate::ClipContent::Pattern { pattern, .. } if c.id == *clip => {
                            Some((pattern, None))
                        }
                        _ => None,
                    })
            }
            _ => None,
        };
        let mut candidate = self.project.clone();
        crate::pattern_edit::apply(&mut candidate, edit).map_err(SessionError::Pattern)?;
        if candidate == self.project {
            return Ok(self.patterns());
        }
        self.admit(&candidate)?;
        let old = self.project.current_pattern;
        let selected = candidate.current_pattern;
        if old != selected {
            // The caller settles its edit queue first. A take must finish on its original
            // pattern before switching; never reinterpret late recorded notes as new input.
            self.settle_recording()?;
            let index = candidate
                .patterns
                .iter()
                .position(|p| p.id == selected)
                .unwrap();
            candidate.patterns[index].revision = candidate.patterns[index]
                .revision
                .checked_add(1)
                .ok_or_else(|| {
                    SessionError::InvalidValue(
                        "Pattern revisions exhausted. Reopen the project.".into(),
                    )
                })?;
        }
        self.publish_candidate(&candidate)?;
        if old != selected {
            let history = self.pattern_histories.remove(&selected).unwrap_or_default();
            let previous = std::mem::replace(&mut self.history, history);
            if previous.can_undo() || previous.can_redo() {
                self.pattern_histories.insert(old, previous);
            }
        }
        self.pattern_histories
            .retain(|id, _| candidate.patterns.iter().any(|p| p.id == *id));
        self.project = candidate;
        if let Some((pattern, deleted_index)) = invalidates_arrangement_history {
            self.arrangement_history.forget_pattern(
                pattern,
                deleted_index,
                &mut self.pattern_note_floors,
            );
        }
        self.patterns_revision += 1;
        self.changed_outside_history(true, now);
        Ok(self.patterns())
    }

    /// The project as it is now.
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Where the project lives, if it has been saved or opened.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn history_position(&self) -> HistoryPosition {
        HistoryPosition {
            notes: std::iter::once((self.project.current_pattern, &self.history))
                .chain(self.pattern_histories.iter().map(|(&id, h)| (id, h)))
                .filter_map(|(id, h)| h.next_order(true).map(|order| (id, order)))
                .collect(),
            arrangement: self.arrangement_history.undo.last().map(|step| step.order),
            trim: self
                .trim_history
                .undo
                .iter()
                .map(|step| step.order)
                .collect(),
        }
    }

    fn remember_saved_state(&mut self) {
        self.saved = serde_json::to_vec(&self.project)
            .ok()
            .map(|project_json| SavedState {
                position: self.history_position(),
                project_json,
                files: self.files.clone(),
            });
        self.dirty = self.saved.is_none();
    }

    fn matches_saved_state(&self, saved: &SavedState) -> bool {
        self.history_position() == saved.position
            && self.files.len() == saved.files.len()
            && self.files.iter().all(|(id, file)| {
                saved.files.get(id).is_some_and(|old| {
                    old.name == file.name && Arc::ptr_eq(&old.bytes, &file.bytes)
                })
            })
            // History can restore a whole pattern or advance an allocator floor. Compare
            // the serialized model too, so a matching position alone cannot hide changes.
            && matches_json(&self.project, saved.project_json.as_slice())
    }

    fn current(&self) -> &Pattern {
        let current = self.project.current_pattern;
        // `Project::new` and `validate` both leave a current pattern.
        self.project
            .patterns
            .iter()
            .find(|p| p.id == current)
            .unwrap_or(&self.project.patterns[0])
    }

    fn current_mut(&mut self) -> &mut Pattern {
        let current = self.project.current_pattern;
        let index = self
            .project
            .patterns
            .iter()
            .position(|p| p.id == current)
            .unwrap_or(0);
        &mut self.project.patterns[index]
    }

    /// What the UI draws.
    pub fn view(&self) -> ProjectView {
        let pattern = self.current();
        let mut cells: BTreeMap<u32, Vec<Option<f32>>> = self
            .project
            .channels
            .iter()
            .map(|c| (c.id, vec![None; pattern.steps as usize]))
            .collect();
        for note in &pattern.notes {
            let step = note.tick / STEP_TICKS;
            if is_plain_step(note)
                && step < pattern.steps
                && let Some(row) = cells.get_mut(&note.channel)
            {
                row[step as usize] = Some(note.velocity);
            }
        }
        let name = self.path.as_deref().and_then(Path::file_stem).map_or_else(
            || self.project.name.clone(),
            |s| s.to_string_lossy().into_owned(),
        );
        let roll = roll_channels(pattern);
        ProjectView {
            name,
            path: self.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            dirty: self.dirty(),
            bpm: self.project.bpm,
            time_signature: self.project.time_signature,
            channels: self
                .project
                .channels
                .iter()
                .map(|c| self.channel_view(c, roll.contains(&c.id)))
                .collect(),
            pattern: PatternView {
                id: pattern.id,
                name: pattern.name.clone(),
                steps: pattern.steps,
                swing: pattern.swing,
                cells,
            },
            autosaved_at: self.autosaved_at,
            key: self.project.key.clone(),
            notes_revision: pattern.revision,
            recording: self.recording,
            can_undo: self.history.can_undo(),
            can_redo: self.history.can_redo(),
            project_can_undo: self.history.can_undo()
                || self.pattern_histories.values().any(|h| h.can_undo())
                || !self.arrangement_history.undo.is_empty()
                || !self.trim_history.undo.is_empty(),
            project_can_redo: self.history.can_redo()
                || self.pattern_histories.values().any(|h| h.can_redo())
                || !self.arrangement_history.redo.is_empty()
                || !self.trim_history.redo.is_empty(),
            generation: self.generation,
            pattern_revision: self.patterns_revision,
        }
    }

    fn channel_view(&self, c: &Channel, roll: bool) -> ChannelView {
        let sample_name = self.files.get(&c.id).map(|f| f.name.clone()).or_else(|| {
            c.sample
                .as_ref()
                .and_then(|s| s.path.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        });
        let by_id = |ids: &[&str], values: Vec<f32>| -> BTreeMap<String, f32> {
            ids.iter()
                .zip(values)
                .map(|(id, v)| ((*id).to_owned(), v))
                .collect()
        };
        let (kind, sampler, synth) = match c.instrument {
            InstrumentKind::Sampler => (
                ChannelKind::Sampler,
                by_id(&PARAM_IDS, sampler_values(c)),
                BTreeMap::new(),
            ),
            InstrumentKind::Synth => (
                ChannelKind::Synth,
                BTreeMap::new(),
                by_id(&synth::PARAM_IDS, synth_values(c)),
            ),
        };
        ChannelView {
            id: c.id,
            name: c.name.clone(),
            color: c.color,
            sample_name,
            sample_name_basis: if self.files.contains_key(&c.id) {
                Some(SampleNameBasis::AvailableFile)
            } else {
                c.sample.as_ref().map(|_| SampleNameBasis::StoredReference)
            },
            sample_reference: c
                .sample
                .as_ref()
                .map(|s| s.path.to_string_lossy().into_owned()),
            missing: c.sample.is_some() && !self.decoded.contains_key(&c.id),
            sample_frames: self.decoded.get(&c.id).map(|s| s.len() as u64),
            volume: c.volume,
            pan: c.pan,
            mute: c.mute,
            solo: c.solo,
            sampler,
            kind,
            synth,
            preset: c.preset.clone(),
            choke: c.choke,
            roll,
        }
    }

    /// Entries in the undo history's length log: a test hook for its bound. Not part of the
    /// API.
    #[doc(hidden)]
    pub fn history_log_len(&self) -> usize {
        self.history.log_len()
    }

    /// The current pattern's revision (see [`ProjectView::notes_revision`]).
    pub fn notes_revision(&self) -> u32 {
        self.current().revision
    }

    /// The current pattern's notes in the wire format (see [`encode_notes`]).
    pub fn notes(&self) -> Vec<u8> {
        encode_notes(self.current())
    }

    /// A copy of the current pattern (its revision and notes), cheap next to encoding it: the
    /// app takes it under the session's lock and runs [`encode_notes`] without.
    pub fn current_pattern(&self) -> Pattern {
        self.current().clone()
    }

    fn channel_index(&self, id: u32) -> Result<usize, SessionError> {
        self.project
            .channels
            .iter()
            .position(|c| c.id == id)
            .ok_or(SessionError::UnknownChannel(id))
    }

    /// Records a change made at `now`.
    fn changed(&mut self, structural: bool, now: Instant) {
        self.revision += 1;
        self.arrangement_revision += 1;
        self.dirty = self.saving
            || self
                .saved
                .as_ref()
                .is_none_or(|saved| !self.matches_saved_state(saved));
        self.pending_since.get_or_insert(now);
        if structural {
            self.structural_since.get_or_insert(now);
        }
    }

    fn changed_outside_history(&mut self, structural: bool, now: Instant) {
        self.saved = None;
        self.changed(structural, now);
    }

    /// [`Self::apply`] for a request made in the project of `generation` (the UI's): if another
    /// project was opened, started or restored since, nothing is applied, and the view of the
    /// open one comes back for the UI to draw. A step edit or a channel removal made in one
    /// project must never land on the same ids in the next.
    pub fn apply_in(
        &mut self,
        generation: u64,
        edit: Edit,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        if generation != self.generation {
            return Ok(self.view());
        }
        self.apply(edit, now)
    }

    /// Applies `edit` to the model and the engine. A refused edit (an unknown channel, step or
    /// parameter, a value that is not a number, or the engine's queue full) changes nothing.
    /// An edit that leaves everything as it was does not make the project dirty.
    pub fn apply(&mut self, edit: Edit, now: Instant) -> Result<ProjectView, SessionError> {
        self.arrangement_stamp(self.generation, self.arrangement_revision)
            .map_err(SessionError::InvalidValue)?;
        let invalidates = match &edit {
            Edit::SetSamplerParam { channel, param, .. }
            | Edit::SetInstrumentParam { channel, param, .. }
                if param == "start" || param == "end" =>
            {
                Some(*channel)
            }
            Edit::RemoveChannel { channel } => Some(*channel),
            _ => None,
        };
        if invalidates.is_some() {
            self.next_trim_epoch()?;
        }
        let structural = edit.structural();
        let undoable = matches!(
            edit,
            Edit::ToggleStep { .. } | Edit::SetStepVelocity { .. } | Edit::ClearStep { .. }
        );
        if self.apply_inner(edit)? {
            if let Some(channel) = invalidates {
                self.invalidate_trim(channel, false);
                if self.channel_index(channel).is_err() {
                    self.trim_identities.remove(&channel);
                }
            }
            if undoable {
                self.changed(structural, now);
            } else {
                self.changed_outside_history(structural, now);
            }
        }
        Ok(self.view())
    }

    /// Returns whether anything changed.
    fn apply_inner(&mut self, edit: Edit) -> Result<bool, SessionError> {
        match edit {
            Edit::RemoveChannel { channel } => self.remove_channel(channel).map(|()| true),
            Edit::SetChannelMix {
                channel,
                volume,
                pan,
                mute,
                solo,
            } => {
                let index = self.channel_index(channel)?;
                let mix = ChannelMix {
                    volume: finite(volume, "volume")?.clamp(VOLUME.0, VOLUME.1),
                    pan: finite(pan, "pan")?.clamp(-1.0, 1.0),
                    mute,
                    solo,
                };
                let c = &self.project.channels[index];
                if (c.volume, c.pan, c.mute, c.solo) == (mix.volume, mix.pan, mute, solo) {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                let c = &mut candidate.channels[index];
                (c.volume, c.pan, c.mute, c.solo) = (mix.volume, mix.pan, mute, solo);
                self.admit(&candidate)?;
                self.send(Command::SetChannelMix {
                    channel: ChannelId(channel),
                    mix,
                })?;
                let c = &mut self.project.channels[index];
                (c.volume, c.pan, c.mute, c.solo) = (mix.volume, mix.pan, mute, solo);
                Ok(true)
            }
            Edit::SetSamplerParam {
                channel,
                param,
                value,
            } => {
                let index = self.channel_index(channel)?;
                let Some(at) = PARAM_IDS.iter().position(|p| *p == param) else {
                    return Err(SessionError::UnknownParam {
                        device: "sampler",
                        param,
                    });
                };
                let (min, max, _) = SPEC[at];
                let value = finite(value, "sampler value")?.clamp(min, max);
                if self.project.channels[index].sampler.get(&param) == Some(&value) {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                candidate.channels[index]
                    .sampler
                    .insert(param.clone(), value);
                self.admit(&candidate)?;
                self.send(Command::SetSamplerParam {
                    channel: ChannelId(channel),
                    index: at as u32,
                    value,
                })?;
                self.project.channels[index].sampler.insert(param, value);
                Ok(true)
            }
            Edit::ToggleStep { channel, step } => {
                let tick = self.step_tick(channel, step)?;
                self.edit_pattern(|p| {
                    let before = p.notes.len();
                    p.notes
                        .retain(|n| !(n.channel == channel && n.tick == tick));
                    if p.notes.len() == before {
                        add_hit(p, channel, tick, DEFAULT_VELOCITY)?;
                    }
                    Ok(())
                })
            }
            Edit::SetStepVelocity {
                channel,
                step,
                velocity,
            } => {
                let tick = self.step_tick(channel, step)?;
                let velocity = finite(velocity, "velocity")?.clamp(0.0, 1.0);
                self.edit_pattern(|p| {
                    let mut found = false;
                    for n in p.notes.iter_mut() {
                        if n.channel == channel && n.tick == tick {
                            n.velocity = velocity;
                            found = true;
                        }
                    }
                    if !found {
                        add_hit(p, channel, tick, velocity)?;
                    }
                    Ok(())
                })
            }
            Edit::ClearStep { channel, step } => {
                let tick = self.step_tick(channel, step)?;
                self.edit_pattern(|p| {
                    p.notes
                        .retain(|n| !(n.channel == channel && n.tick == tick));
                    Ok(())
                })
            }
            Edit::SetPatternLength { steps } => {
                let steps = steps.clamp(1, MAX_STEPS);
                if self.current().steps == steps {
                    let mut histories = self.staged_histories();
                    histories.current.set_length(steps);
                    self.admit_histories(&mut histories)?;
                    self.install_histories(histories);
                    return Ok(false);
                }
                self.edit_pattern(|p| {
                    p.steps = steps;
                    Ok(())
                })
            }
            Edit::SetSwing { swing } => {
                let swing = finite(swing, "swing")?.clamp(0.0, 1.0);
                self.edit_pattern(|p| {
                    p.swing = swing;
                    Ok(())
                })
            }
            Edit::SetTempo { bpm } => {
                let bpm = finite(bpm, "tempo")?.clamp(BPM.0, BPM.1);
                if self.project.bpm == bpm {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                candidate.bpm = bpm;
                crate::file::validate_scalar_edit(&candidate, &self.files)
                    .map_err(SessionError::Save)?;
                if let Some(validate) = &self.precommit {
                    validate(&candidate).map_err(SessionError::InvalidValue)?;
                }
                let song = self.prepare_song(&candidate)?;
                lock(&self.controller).set_timeline(Self::timeline(&candidate, song.clone()))?;
                self.compiled_song = song;
                self.song_error = None;
                self.project = candidate;
                Ok(true)
            }
            Edit::SetTimeSignature { beats, unit } => {
                if !(1..=16).contains(&beats) || !matches!(unit, 2 | 4 | 8 | 16) {
                    return Err(SessionError::InvalidValue(format!(
                        "{beats}/{unit} is not a time signature Grooviq supports (1 to 16 beats of 2, 4, 8 or 16)."
                    )));
                }
                let signature = TimeSignature { beats, unit };
                if self.project.time_signature == signature {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                candidate.time_signature = signature;
                self.admit(&candidate)?;
                // The metronome counts the beats; the bars are drawn by the UI.
                self.send(Command::SetTimeSignature { beats, unit })?;
                self.project.time_signature = signature;
                Ok(true)
            }
            Edit::SetInstrumentParam {
                channel,
                param,
                value,
            } => {
                let index = self.channel_index(channel)?;
                let kind = self.project.channels[index].instrument;
                let at = param_index(kind, &param)?;
                let (min, max, _) = kind.spec()[at];
                let value = finite(value, &format!("{} value", device(kind)))?.clamp(min, max);
                let c = &self.project.channels[index];
                let values = match kind {
                    InstrumentKind::Sampler => &c.sampler,
                    InstrumentKind::Synth => &c.synth,
                };
                if values.get(&param) == Some(&value) {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                match kind {
                    InstrumentKind::Sampler => candidate.channels[index]
                        .sampler
                        .insert(param.clone(), value),
                    InstrumentKind::Synth => {
                        candidate.channels[index].synth.insert(param.clone(), value)
                    }
                };
                self.admit(&candidate)?;
                self.send(Command::SetChannelParam {
                    channel: ChannelId(channel),
                    index: at as u32,
                    value,
                })?;
                let c = &mut self.project.channels[index];
                match kind {
                    InstrumentKind::Sampler => c.sampler.insert(param, value),
                    InstrumentKind::Synth => c.synth.insert(param, value),
                };
                Ok(true)
            }
            Edit::LoadSynthPreset { channel, preset } => {
                let index = self.channel_index(channel)?;
                let c = &self.project.channels[index];
                if c.instrument != InstrumentKind::Synth {
                    return Err(SessionError::InvalidValue(format!(
                        "{} is not a synth channel.",
                        c.name
                    )));
                }
                let values = synth_preset(&preset)?;
                let mut candidate = self.project.clone();
                candidate.channels[index].synth = by_param_id(&values);
                candidate.channels[index].preset = Some(preset.clone());
                self.admit(&candidate)?;
                lock(&self.controller).set_channel_params(ChannelId(channel), &values)?;
                let c = &mut self.project.channels[index];
                c.synth = by_param_id(&values);
                c.preset = Some(preset);
                Ok(true)
            }
            Edit::SetChoke { channel, group } => {
                let index = self.channel_index(channel)?;
                let group = group.min(MAX_CHOKE_GROUP);
                if self.project.channels[index].choke == group {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                candidate.channels[index].choke = group;
                self.admit(&candidate)?;
                self.send(Command::SetChoke {
                    channel: ChannelId(channel),
                    group,
                })?;
                self.project.channels[index].choke = group;
                Ok(true)
            }
            Edit::SetProjectKey { key } => {
                if let Some(k) = &key
                    && (k.root > 11 || !SCALES.iter().any(|(id, _)| *id == k.scale))
                {
                    return Err(SessionError::InvalidValue(format!(
                        "{} on {} is not a key Grooviq knows.",
                        k.scale, k.root
                    )));
                }
                if self.project.key == key {
                    return Ok(false);
                }
                let mut candidate = self.project.clone();
                candidate.key = key.clone();
                self.admit(&candidate)?;
                self.project.key = key;
                Ok(true)
            }
        }
    }

    /// The tick of `step` on `channel`, checking both exist.
    /// Refused on a channel with piano-roll notes: the step grid would delete or change them.
    fn step_tick(&self, channel: u32, step: u32) -> Result<u32, SessionError> {
        let index = self.channel_index(channel)?;
        if roll_channels(self.current()).contains(&channel) {
            return Err(SessionError::RollChannel {
                name: self.project.channels[index].name.clone(),
            });
        }
        let steps = self.current().steps;
        if step >= steps {
            return Err(SessionError::StepOutOfRange { step, steps });
        }
        Ok(step * STEP_TICKS)
    }

    /// Changes a copy of the current pattern, sends it to the engine, then keeps it, with its
    /// revision bumped if its notes changed. A change that fails leaves the pattern as it was.
    /// A note change goes into the undo history, as one step.
    fn edit_pattern(
        &mut self,
        change: impl FnOnce(&mut Pattern) -> Result<(), SessionError>,
    ) -> Result<bool, SessionError> {
        self.edit_pattern_as(None, |p| change(p).map(|()| None))
    }

    /// [`Self::edit_pattern`]; with `take`, the note change is a commit of that recorded take
    /// and joins the take's undo step while it is open (see [`NoteHistory::record`]).
    /// `change` says what its notes did to the length (see [`Growth`]).
    fn edit_pattern_as(
        &mut self,
        take: Option<u32>,
        change: impl FnOnce(&mut Pattern) -> Result<Option<Growth>, SessionError>,
    ) -> Result<bool, SessionError> {
        let steps = self.current().steps;
        let mut pattern = self.current().clone();
        let growth = change(&mut pattern)?;
        if pattern == *self.current() {
            return Ok(false);
        }
        let notes = NoteChange::between(self.current(), &pattern);
        pattern.revision = pattern.revision.checked_add(1).ok_or_else(|| {
            SessionError::InvalidValue("Pattern revisions exhausted. Reopen the project.".into())
        })?;
        let new_steps = pattern.steps;
        let mut histories = self.staged_histories();
        histories.arrangement.redo.clear();
        for h in histories.patterns.values_mut() {
            h.clear_redo();
        }
        if take.is_none() && (notes.is_some() || growth.is_some()) {
            histories.current.clear_redo();
        }
        let undoable = notes.is_some() || growth.is_some();
        if growth.is_none() && new_steps != steps {
            histories.current.set_length(new_steps);
        }
        match (notes, take) {
            (Some(notes), Some(take)) => histories.current.record(notes, take, steps, growth),
            (Some(notes), None) => histories.current.push(notes, steps, growth),
            // Notes resubmitted as they were, growing a pattern shortened by hand: a step
            // that holds only that growth, so undo and redo play the length it gave.
            (None, _) if growth.is_some() && new_steps != steps => {
                histories.current.push(NoteChange::none(), steps, growth);
            }
            (None, _) => {}
        }
        if undoable
            && let Some(order) = take
                .and_then(|t| histories.current.take_order(t))
                .or_else(|| histories.current.next_order(true))
        {
            histories.protected.insert(order);
        }
        self.admit_histories(&mut histories)?;
        self.keep_pattern(pattern)?;
        self.install_histories(histories);
        Ok(true)
    }

    /// Sends `pattern` to the engine (which goes on playing), then makes it the current one.
    fn keep_pattern(&mut self, mut pattern: Pattern) -> Result<(), SessionError> {
        self.arrangement_stamp(self.generation, self.arrangement_revision)
            .map_err(SessionError::InvalidValue)?;
        // A borrowed candidate avoids cloning every other pattern for each note gesture.
        // Drop restores the live model on validation error AND unwinding.
        struct Candidate<'a> {
            project: &'a mut Project,
            pattern: &'a mut Pattern,
            index: usize,
        }
        impl Drop for Candidate<'_> {
            fn drop(&mut self) {
                std::mem::swap(&mut self.project.patterns[self.index], self.pattern);
            }
        }
        let index = self
            .project
            .patterns
            .iter()
            .position(|p| p.id == pattern.id)
            .unwrap();
        std::mem::swap(&mut self.project.patterns[index], &mut pattern);
        let candidate = Candidate {
            project: &mut self.project,
            pattern: &mut pattern,
            index,
        };
        crate::validate_save(candidate.project, &self.files).map_err(SessionError::Save)?;
        if let Some(validate) = &self.precommit {
            validate(candidate.project).map_err(SessionError::InvalidValue)?;
        }
        let song = crate::compile_song::compile_song(
            candidate.project,
            &self.decoded,
            self.song_error
                .is_none()
                .then_some(&self.compiled_song.audio),
            self.song_note_limit,
        )
        .map_err(SessionError::InvalidValue)?;
        let update = Self::timeline(candidate.project, song.clone());
        drop(candidate);
        lock(&self.controller).set_timeline(update)?;
        self.compiled_song = song;
        self.song_error = None;
        *self.current_mut() = pattern;
        Ok(())
    }

    /// Takes back the last note change (a note edit, a step edit or a recorded take), or
    /// `None` if there is none. The notes (under their own ids) and the length go back to what
    /// they were; the engine takes the pattern without stopping. Its revision moves on and the
    /// project counts as changed; the ack asks the UI to fetch the notes again. A refused undo
    /// (the engine's queue full) changes nothing and stays next in line. Recording is left as
    /// it is.
    pub fn undo_notes(&mut self, now: Instant) -> Result<Option<NotesAck>, SessionError> {
        self.step_history(true, now)
    }

    /// [`Self::undo_notes`] (`undo`) or [`Self::redo_notes`] for a request made in the project
    /// of `generation` (the UI's): if another project was opened, started or restored since,
    /// nothing changes and the ack asks the UI to fetch the open project's notes.
    pub fn step_history_in(
        &mut self,
        generation: u64,
        undo: bool,
        now: Instant,
    ) -> Result<Option<NotesAck>, SessionError> {
        if generation != self.generation {
            return Ok(Some(self.refused_ack()));
        }
        self.step_history(undo, now)
    }

    /// The ack of a note request refused because it was made for a replaced project: nothing
    /// changed, and the UI fetches the notes of the open one.
    fn refused_ack(&self) -> NotesAck {
        NotesAck {
            revision: self.current().revision,
            added_ids: Vec::new(),
            ignored_ids: Vec::new(),
            reload: true,
            view: self.view(),
        }
    }

    /// Makes the last undone note change again, or `None` if there is none (see
    /// [`Self::undo_notes`]). Any new note change clears what redo could make.
    pub fn redo_notes(&mut self, now: Instant) -> Result<Option<NotesAck>, SessionError> {
        self.step_history(false, now)
    }

    fn step_history(&mut self, undo: bool, now: Instant) -> Result<Option<NotesAck>, SessionError> {
        let next = if undo {
            self.history.next_undo()
        } else {
            self.history.next_redo()
        };
        let Some(change) = next else {
            return Ok(None);
        };
        let mut pattern = self.current().clone();
        change.apply(&mut pattern, undo)?;
        pattern.steps = self.history.length_after(undo);
        let steps = self.current().steps;
        pattern.revision = pattern.revision.checked_add(1).ok_or_else(|| {
            SessionError::InvalidValue("Pattern revisions exhausted. Reopen the project.".into())
        })?;
        let mut histories = self.staged_histories();
        if undo {
            histories.current.undone()
        } else {
            histories.current.redone()
        };
        for h in histories.patterns.values_mut() {
            h.close();
        }
        self.admit_histories(&mut histories)?;
        self.keep_pattern(pattern)?;
        self.install_histories(histories);
        self.changed(self.current().steps != steps, now);
        Ok(Some(NotesAck {
            revision: self.current().revision,
            added_ids: Vec::new(),
            ignored_ids: Vec::new(),
            reload: true,
            view: self.view(),
        }))
    }

    fn send(&self, cmd: Command) -> Result<(), SessionError> {
        lock(&self.controller)
            .send(cmd)
            .map_err(|_| SessionError::Engine(ControlError::Rejected))
    }

    fn remove_channel(&mut self, channel: u32) -> Result<(), SessionError> {
        let index = self.channel_index(channel)?;
        // Persisted automation must remain valid even while Song mode is inactive.
        if self.project.arrangement.clips.iter().any(|clip| {
            matches!(&clip.content, crate::ClipContent::Automation { target, .. }
                if match target {
                    crate::AutomationTarget::ChannelVolume { channel: target }
                    | crate::AutomationTarget::ChannelPan { channel: target }
                    | crate::AutomationTarget::InstrumentParam { channel: target, .. } => *target == channel,
                })
        }) {
            return Err(SessionError::InvalidValue(
                "Remove this channel's automation clips before removing the channel.".into(),
            ));
        }
        let mut patterns = self.project.patterns.clone();
        for p in &mut patterns {
            let before = p.notes.len();
            p.notes.retain(|n| n.channel != channel);
            if p.notes.len() != before {
                p.revision = p.revision.wrapping_add(1);
            }
        }
        let mut candidate = self.project.clone();
        candidate.channels.remove(index);
        candidate.patterns = patterns;
        candidate
            .mixer
            .sources
            .retain(|a| a.source != crate::mixer::SourceKey::Channel(channel));
        candidate.mixer.refresh_mode();
        let mut files = self.files.clone();
        let mut decoded = self.decoded.clone();
        files.remove(&channel);
        decoded.remove(&channel);
        // Structural deletion has no inverse. Remove its references from other steps
        // before dropping the channel, so unrelated gestures still work.
        let mut histories = self.staged_histories();
        histories.forget_channel(&self.project, channel);
        self.admit_histories(&mut histories)?;
        self.publish_media_candidate(&candidate, &files, &decoded)?;
        self.project = candidate;
        self.files = files;
        self.decoded = decoded;
        self.install_histories(histories);
        Ok(())
    }

    /// Adds a channel playing `sample` after the others: named after the file's stem, in the
    /// next ink. The session keeps `file`'s bytes for saving.
    /// [`Self::add_channel`] for a request made in the project of `generation`: refused with
    /// [`SessionError::ProjectReplaced`] if another project was opened, started or restored
    /// since (the sample was decoded meanwhile; the check is made at the change).
    pub fn add_channel_in(
        &mut self,
        generation: u64,
        path: &Path,
        file: crate::SampleFile,
        sample: Arc<SampleBuf>,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        self.same_project(generation)?;
        self.add_channel(path, file, sample, now)
    }

    /// [`Self::add_synth_channel`] for a request made in the project of `generation` (see
    /// [`Self::add_channel_in`]).
    pub fn add_synth_channel_in(
        &mut self,
        generation: u64,
        preset: &str,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        self.same_project(generation)?;
        self.add_synth_channel(preset, now)
    }

    /// [`Self::set_channel_sample`] for a request made in the project of `generation` (see
    /// [`Self::add_channel_in`]).
    pub fn set_channel_sample_in(
        &mut self,
        generation: u64,
        channel: u32,
        path: &Path,
        file: crate::SampleFile,
        sample: Arc<SampleBuf>,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        self.same_project(generation)?;
        self.set_channel_sample(channel, path, file, sample, now)
    }

    /// Refuses a request made in a project that was replaced since.
    fn same_project(&self, generation: u64) -> Result<(), SessionError> {
        if generation == self.generation {
            Ok(())
        } else {
            Err(SessionError::ProjectReplaced)
        }
    }

    pub fn add_channel(
        &mut self,
        path: &Path,
        file: crate::SampleFile,
        sample: Arc<SampleBuf>,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        if self.project.channels.len() >= MAX_CHANNELS {
            return Err(SessionError::TooManyChannels);
        }
        let mut candidate = self.project.clone();
        let id = candidate.take_id().ok_or(SessionError::Pattern(
            crate::pattern_edit::PatternError::IdExhausted,
        ))?;
        let name = path
            .file_stem()
            .map_or_else(|| "Sample".into(), |s| s.to_string_lossy().into_owned());
        let channel = Channel {
            id,
            name,
            color: self.next_color(),
            sample: Some(SampleRef {
                path: path.to_path_buf(),
                file: None,
            }),
            instrument: InstrumentKind::Sampler,
            sampler: BTreeMap::new(),
            synth: BTreeMap::new(),
            preset: None,
            volume: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
            choke: 0,
        };
        candidate.channels.push(channel);
        let mut files = self.files.clone();
        let mut decoded = self.decoded.clone();
        files.insert(id, file);
        decoded.insert(id, sample);
        self.publish_media_candidate(&candidate, &files, &decoded)?;
        self.project = candidate;
        self.files = files;
        self.decoded = decoded;
        self.changed_outside_history(true, now);
        Ok(self.view())
    }

    /// The ink after the last channel's.
    fn next_color(&self) -> u8 {
        self.project
            .channels
            .last()
            .map_or(0, |c| (c.color.wrapping_add(1)) % INKS)
    }

    /// Adds a synth channel after the others, named after `preset` ("Init" or one of
    /// `grooviq_dsp::synth::PRESETS`) and holding all its values.
    pub fn add_synth_channel(
        &mut self,
        preset: &str,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        if self.project.channels.len() >= MAX_CHANNELS {
            return Err(SessionError::TooManyChannels);
        }
        let values = synth_preset(preset)?;
        let mut candidate = self.project.clone();
        let id = candidate.take_id().ok_or(SessionError::Pattern(
            crate::pattern_edit::PatternError::IdExhausted,
        ))?;
        let channel = Channel {
            id,
            name: preset.to_owned(),
            color: self.next_color(),
            sample: None,
            instrument: InstrumentKind::Synth,
            sampler: BTreeMap::new(),
            synth: by_param_id(&values),
            preset: Some(preset.to_owned()),
            volume: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
            choke: 0,
        };
        candidate.channels.push(channel);
        self.publish_candidate(&candidate)?;
        self.project = candidate;
        self.changed_outside_history(true, now);
        Ok(self.view())
    }

    /// Applies a note edit to the current pattern (see [`NotesEdit`] and [`notes`]): all of it,
    /// or nothing for an unknown channel, the note cap or used-up ids. An edit that changes
    /// nothing (only unknown ids) leaves the revision and the dirty state alone.
    ///
    /// An edit made for another project (`edit.generation`: one replaced since) is refused
    /// before anything is applied: its ids name that project's notes. An older revision of this
    /// project is refused too: a held gesture must not overwrite a concurrent edit.
    pub fn edit_notes(&mut self, edit: NotesEdit, now: Instant) -> Result<NotesAck, SessionError> {
        if edit.generation != self.generation || edit.pattern_id != self.project.current_pattern {
            return Ok(self.refused_ack());
        }
        let stale = edit.base_revision != self.current().revision;
        if stale {
            return Err(SessionError::InvalidValue(
                "The pattern changed before this note edit arrived. Reload its notes.".into(),
            ));
        }
        let applied = self.apply_notes(&edit, None, now)?;
        Ok(NotesAck {
            revision: self.current().revision,
            added_ids: applied.added_ids,
            ignored_ids: applied.ignored_ids,
            reload: stale || applied.clamped,
            view: self.view(),
        })
    }

    /// Applies `edit` (with `take`, as a recorded take's commit: see [`Self::edit_pattern_as`]).
    fn apply_notes(
        &mut self,
        edit: &NotesEdit,
        take: Option<u32>,
        now: Instant,
    ) -> Result<notes::Applied, SessionError> {
        let channels: HashSet<u32> = self.project.channels.iter().map(|c| c.id).collect();
        let sig = self.project.time_signature;
        let bar = u32::from(sig.beats) * 3840 / u32::from(sig.unit.max(1));
        let steps = self.current().steps;
        let mut applied = None;
        let changed = self.edit_pattern_as(take, |p| {
            let done = notes::apply(p, edit, &channels, bar)?;
            let growth = done.growth;
            applied = Some(done);
            Ok(growth)
        })?;
        if changed {
            // A longer pattern changes the project's shape.
            self.changed(self.current().steps != steps, now);
        }
        Ok(applied.unwrap_or_default())
    }

    fn param_target(
        &self,
        channel: u32,
        param: &str,
    ) -> Result<(InstrumentKind, usize), SessionError> {
        let kind = self.project.channels[self.channel_index(channel)?].instrument;
        Ok((kind, param_index(kind, param)?))
    }

    /// Starts `key` on `channel` now: the typing keyboard, the roll's keys, a synth row's
    /// preview. Only on the person's key or click. The press it made: its release names it.
    pub fn live_note_on(
        &self,
        channel: u32,
        key: u8,
        velocity: f32,
    ) -> Result<NoteId, SessionError> {
        self.channel_index(channel)?;
        let key = checked_key(key)?;
        let velocity = finite(velocity, "velocity")?.clamp(0.0, 1.0);
        let press = NoteId::live();
        let mut controller = lock(&self.controller);
        let instance = controller
            .channel_instance(ChannelId(channel))
            .ok_or(ControlError::UnknownChannel)?;
        controller.send_at(0, live_note(channel, key, velocity, press))?;
        let mut presses = self
            .live_presses
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        presses.retain(|p| controller.channel_instance(ChannelId(p.channel)) == Some(p.instance));
        presses.push(LivePress {
            channel,
            key,
            press,
            instance,
            releasing: false,
        });
        Ok(press)
    }

    /// Releases `press` (what [`Self::live_note_on`] returned) of `key` on `channel`. Each
    /// producer names its own press, so a release never ends another one's note on the key.
    /// A full queue transfers the release to session maintenance; success means it is owned
    /// here, not necessarily delivered yet. Duplicate releases are harmless.
    pub fn live_note_off(&self, channel: u32, key: u8, press: NoteId) -> Result<(), SessionError> {
        self.channel_index(channel)?;
        let key = checked_key(key)?;
        let mut controller = lock(&self.controller);
        let mut presses = self
            .live_presses
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(at) = presses
            .iter()
            .position(|p| p.channel == channel && p.key == key && p.press == press)
        else {
            return Ok(()); // Duplicate or unaccepted press: nothing is owed.
        };
        if controller.channel_instance(ChannelId(channel)) != Some(presses[at].instance) {
            presses.remove(at);
            return Ok(());
        }
        match controller.send_at(0, live_release(channel, key, press)) {
            Ok(()) => {
                presses.remove(at);
                Ok(())
            }
            Err(ControlError::Full) => {
                presses[at].releasing = true;
                Ok(())
            }
            Err(e) => {
                presses.remove(at);
                Err(e.into())
            }
        }
    }

    /// Called even when idle/unarmed. Releases record at engine delivery, not physical key-up.
    fn retry_live_releases(&self) {
        let mut controller = lock(&self.controller);
        let mut presses = self
            .live_presses
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        presses.retain(|p| {
            if controller.channel_instance(ChannelId(p.channel)) != Some(p.instance) {
                return false;
            }
            !p.releasing
                || matches!(
                    controller.send_at(0, live_release(p.channel, p.key, p.press)),
                    Err(ControlError::Full)
                )
        });
    }

    /// Sets an instrument parameter in the engine only, while a control moves: the project
    /// changes when the UI commits the value with [`Edit::SetInstrumentParam`].
    pub fn param_live(
        &mut self,
        channel: u32,
        param: &str,
        value: f32,
    ) -> Result<(), SessionError> {
        let (kind, at) = self.param_target(channel, param)?;
        let (min, max, _) = kind.spec()[at];
        let value = finite(value, &format!("{} value", device(kind)))?.clamp(min, max);
        let trim = kind == InstrumentKind::Sampler && (param == "start" || param == "end");
        if trim {
            self.next_trim_epoch()?;
        }
        self.send(Command::SetChannelParam {
            channel: ChannelId(channel),
            index: at as u32,
            value,
        })?;
        if trim {
            self.invalidate_trim(channel, false);
        }
        Ok(())
    }

    /// Switches recording: while it is on and the transport plays, live notes are recorded
    /// into the pattern (see [`Self::collect_recorded`]). Switching off ends the take.
    ///
    /// After recording switched itself off (see [`Self::collect_recorded`]) and before the
    /// engine confirmed it, the switch goes with a fresh barrier: notes recorded before it are
    /// still refused (and reported), the new take's after it are kept, and an owed record-off
    /// is cancelled.
    pub fn set_recording(&mut self, on: bool) -> Result<(), SessionError> {
        if self.owed_disarm || self.recorder.refusing() || !on {
            let id = lock(&self.controller).send_marked(Command::SetRecording { on })?;
            if self.recorder.refusing() || self.owed_disarm {
                self.recorder.refuse_until_mark(Some(id));
            }
            if !on {
                self.record_settle = Some(id);
            }
            self.owed_disarm = false;
        } else {
            self.send(Command::SetRecording { on })?;
        }
        self.recording = on;
        Ok(())
    }

    /// A switch/save/replacement never overtakes a take. A successful record-off is an
    /// explicit settlement side effect; the project is not changed until its barrier arrives.
    pub fn settle_recording(&mut self) -> Result<(), SessionError> {
        if self.recording {
            self.set_recording(false)?;
        }
        if !self.pending.is_empty() {
            return Err(SessionError::PendingRecording(
                "retry the recording poll or repair the admission limit before continuing".into(),
            ));
        }
        if self.record_settle.is_some() || self.owed_disarm || self.recorder.open() != 0 {
            return Err(SessionError::RecordingSettling);
        }
        Ok(())
    }

    /// Switches the metronome (it clicks only while the transport plays; never in an export).
    pub fn set_metronome(&self, on: bool) -> Result<(), SessionError> {
        self.send(Command::SetMetronome { on })
    }

    /// Takes the engine's recorded events (from `Controller::take_recorded`, in order; empty
    /// is fine) and adds the notes they complete to the current pattern as one edit (overdub).
    /// Call it on every poll: notes the engine could not take yet (its queue full) wait here
    /// and go in on a later call, once. Notes of channels the project no longer has are
    /// dropped. When notes do not fit (the note cap, or used-up ids), the ones that fit go in,
    /// recording is switched off, and [`Recorded::stopped`] says what was lost.
    pub fn collect_recorded(&mut self, events: &[RecordEvent], now: Instant) -> Recorded {
        self.retry_live_releases();
        let mut recovery_stopped = false;
        {
            let controller = lock(&self.controller);
            let mark = controller.recovery_stop_mark();
            if mark != self.recovery_stop_mark {
                self.recovery_stop_mark = mark;
                // Controller health can end a take independently of the record switch.
                // Consume all accepted notes through its End and barrier, without refusal.
                if !controller.recording_armed() {
                    recovery_stopped = self.recording;
                    self.recording = false;
                    self.record_settle = Some(controller.record_mark());
                }
            }
        }
        if self.owed_disarm {
            self.send_disarm();
        }
        if events
            .iter()
            .any(|e| matches!(e, RecordEvent::Mark { id } if Some(*id) == self.record_settle))
        {
            self.record_settle = None;
        }
        let fed = self.recorder.feed_takes(events);
        self.pending.extend(fed);
        let channels: HashSet<u32> = self.project.channels.iter().map(|c| c.id).collect();
        self.pending.retain(|(_, n)| channels.contains(&n.channel));
        // What fits: the note cap, and the ids left (the used-up mark is never handed out).
        let pattern = self.current();
        let room = MAX_NOTES_PER_PATTERN.saturating_sub(pattern.notes.len());
        let ids = (u32::MAX - pattern.next_note_id.max(1)) as usize;
        let (fits, mut reason) = if ids < room {
            (ids, "noteIds")
        } else {
            (room, "noteCap")
        };
        let mut dropped = self.pending.len().saturating_sub(fits);
        // Capacity overflow is only discarded after admission succeeds (or its explicit
        // capacity-stop acknowledgement). A compiler refusal retains the whole poll.
        let overflow = self.pending.split_off(fits.min(self.pending.len()));

        let mut out = Recorded::default();
        if recovery_stopped {
            out.stopped = Some(RecordingStopped {
                reason: "engineRecovery",
                dropped: 0,
                message: "Recording stopped because sound engine recovery was exhausted.".into(),
            });
        }
        let mut error = None;
        match self.commit_pending(now) {
            Ok(PendingCommit::Empty) => {}
            Ok(PendingCommit::Deferred) => {
                // No prefix reached the engine: capacity overflow is still retryable too.
                self.pending.extend(overflow);
                return out;
            }
            Ok(PendingCommit::Applied(revision)) => out.revision = Some(revision),
            Err((e, lost, revision)) => {
                out.revision = revision;
                dropped += lost;
                reason = "error";
                if lost == 0 {
                    self.pending.extend(overflow);
                    // End the original take through its normal End/barrier, preserving held
                    // notes. A full command queue leaves recording on and retries next poll.
                    let ending = !self.recording || self.set_recording(false).is_ok();
                    out.stopped = Some(RecordingStopped {
                        reason: "admission",
                        dropped: 0,
                        message: format!(
                            "{} Recorded notes are retained for retry: {e}",
                            if ending {
                                "Recording ended."
                            } else {
                                "Recording could not end yet; its queue will retry."
                            }
                        ),
                    });
                    return out;
                }
                error = Some(e);
            }
        }
        if dropped > 0 {
            self.disarm();
        }
        // Counted after the disarm: the notes held when it began are refused too.
        let refused = self.recorder.take_refused();
        if dropped + refused == 0 {
            return out;
        }
        let lost = dropped + refused;
        let notes = if lost == 1 {
            "1 recorded note was".to_owned()
        } else {
            format!("{lost} recorded notes were")
        };
        let (reason, message) = match (dropped > 0, error) {
            (true, Some(e)) => (reason, format!("Recording stopped: {e}")),
            (true, None) if reason == "noteIds" => (
                reason,
                format!(
                    "Recording stopped: this pattern has used up its note ids (save and reopen the project to renumber them). {notes} not added."
                ),
            ),
            (true, None) => (
                reason,
                format!(
                    "Recording stopped: this pattern holds {MAX_NOTES_PER_PATTERN} notes, the most a pattern can hold. {notes} not added."
                ),
            ),
            (false, _) => (
                "engineBusy",
                format!("Recording had switched itself off, so {notes} not added."),
            ),
        };
        out.stopped = Some(RecordingStopped {
            reason,
            dropped: u32::try_from(lost).unwrap_or(u32::MAX),
            message,
        });
        out
    }

    /// Switches recording off by itself (recorded notes did not fit): from now until the
    /// engine's barrier behind the switch is read, every recorded note is refused and counted.
    fn disarm(&mut self) {
        self.recording = false;
        self.recorder.refuse_until_mark(None);
        self.send_disarm();
    }

    /// Sends the record-off with its barrier; owed (sent again on every collect) while the
    /// engine's queue is full. The refusal ends at the barrier, not when the switch is queued.
    fn send_disarm(&mut self) {
        match lock(&self.controller).send_marked(Command::SetRecording { on: false }) {
            Ok(id) => {
                self.owed_disarm = false;
                self.record_settle = Some(id);
                self.recorder.await_mark(id);
            }
            Err(_) => self.owed_disarm = true,
        }
    }

    /// Stage every completed take in a poll, admit once, enqueue once, then publish its
    /// independent history steps. No earlier take can slip through a later refusal.
    fn commit_pending(
        &mut self,
        now: Instant,
    ) -> Result<PendingCommit, (SessionError, usize, Option<u32>)> {
        if self.pending.is_empty() {
            return Ok(PendingCommit::Empty);
        }
        let channels: HashSet<u32> = self.project.channels.iter().map(|c| c.id).collect();
        let sig = self.project.time_signature;
        let bar = u32::from(sig.beats) * 3840 / u32::from(sig.unit.max(1));
        let mut pattern = self.current().clone();
        let old_steps = pattern.steps;
        let mut staged = Vec::new();
        let mut at = 0;
        while at < self.pending.len() {
            let take = self.pending[at].0;
            let run = self.pending[at..]
                .iter()
                .take_while(|(t, _)| *t == take)
                .count();
            let steps = pattern.steps;
            let start = pattern.notes.len();
            let edit = NotesEdit {
                generation: self.generation,
                pattern_id: pattern.id,
                base_revision: pattern.revision,
                add: self.pending[at..at + run].iter().map(|(_, n)| *n).collect(),
                update: Vec::new(),
                delete: Vec::new(),
            };
            let applied =
                notes::apply(&mut pattern, &edit, &channels, bar).map_err(|e| (e, 0, None))?;
            staged.push((
                take,
                steps,
                NoteChange::added(pattern.notes[start..].to_vec()),
                applied.growth,
            ));
            at += run;
        }
        pattern.revision = pattern.revision.checked_add(1).ok_or_else(|| {
            (
                SessionError::InvalidValue(
                    "Pattern revisions exhausted. Reopen the project.".into(),
                ),
                0,
                None,
            )
        })?;
        let mut histories = self.staged_histories();
        // A recording continuation preserves its open take; other domains' redo expires.
        histories.arrangement.redo.clear();
        for h in histories.patterns.values_mut() {
            h.clear_redo();
        }
        for (take, steps, change, growth) in staged {
            histories.current.record(change, take, steps, growth);
            if let Some(order) = histories.current.take_order(take) {
                histories.protected.insert(order);
            }
        }
        self.admit_histories(&mut histories)
            .map_err(|e| (e, 0, None))?;
        match self.keep_pattern(pattern) {
            Err(SessionError::Engine(_)) => return Ok(PendingCommit::Deferred),
            Err(e) => return Err((e, 0, None)),
            Ok(()) => {}
        }
        self.install_histories(histories);
        self.pending.clear();
        self.changed(self.current().steps != old_steps, now);
        Ok(PendingCommit::Applied(self.current().revision))
    }

    /// The decoded sample of a sampler channel, if it has one.
    pub fn sample(&self, channel: u32) -> Option<Arc<SampleBuf>> {
        self.decoded.get(&channel).cloned()
    }

    /// `points` (min, max) pairs of the channel's sample (see [`waveform_peaks`]), or `None`
    /// without a loaded sample. Slow for a long sample: the app takes [`Self::sample`] and
    /// computes the peaks without the session's lock.
    pub fn waveform(&self, channel: u32, points: u32) -> Option<Vec<u8>> {
        self.sample(channel).map(|s| waveform_peaks(&s, points))
    }

    /// Gives `channel` a new sample (relinking a missing one, or replacing it). Its name stays.
    pub fn set_channel_sample(
        &mut self,
        channel: u32,
        path: &Path,
        file: crate::SampleFile,
        sample: Arc<SampleBuf>,
        now: Instant,
    ) -> Result<ProjectView, SessionError> {
        let index = self.channel_index(channel)?;
        self.next_trim_epoch()?;
        let mut candidate = self.project.clone();
        candidate.channels[index].sample = Some(SampleRef {
            path: path.to_path_buf(),
            file: None,
        });
        let mut files = self.files.clone();
        files.insert(channel, file.clone());
        self.media_budget
            .original(&file.bytes)
            .map_err(|e| SessionError::InvalidValue(e.to_string()))?;
        self.media_budget
            .sample(&sample)
            .map_err(|e| SessionError::InvalidValue(e.to_string()))?;
        crate::validate_save(&candidate, &files).map_err(SessionError::Save)?;
        if let Some(validate) = &self.precommit {
            validate(&candidate).map_err(SessionError::InvalidValue)?;
        }
        self.prepare_song(&candidate)?;
        lock(&self.controller).set_channel_sample(ChannelId(channel), Some(Arc::clone(&sample)))?;
        self.project.channels[index].sample = Some(SampleRef {
            path: path.to_path_buf(),
            file: None,
        });
        self.files.insert(channel, file);
        self.decoded.insert(channel, sample);
        self.invalidate_trim(channel, true);
        self.changed_outside_history(true, now);
        Ok(self.view())
    }

    /// Starts a preview of `channel`: key 60 at velocity 0.8. Only on the person's click or
    /// key: never call it on its own. The press it made: its release names it.
    pub fn preview_on(&self, channel: u32) -> Result<NoteId, SessionError> {
        self.live_note_on(channel, DEFAULT_KEY, DEFAULT_VELOCITY)
    }

    /// Releases the preview `press` of `channel`.
    pub fn preview_off(&self, channel: u32, press: NoteId) -> Result<(), SessionError> {
        self.live_note_off(channel, DEFAULT_KEY, press)
    }

    /// Replaces the engine's state with the project's (stopping the transport first).
    fn sync_engine(&self) -> Result<(), SessionError> {
        let state = self.export_state()?;
        lock(&self.controller).load_state(state)?;
        Ok(())
    }

    /// Swaps in `project` and its samples, after the engine took them.
    fn replace(
        &mut self,
        project: Project,
        files: SampleFiles,
        decoded: HashMap<u32, Arc<SampleBuf>>,
    ) -> Result<(), SessionError> {
        // Before anything changes: the generic song fallback below must never take a mixer
        // the engine cannot play and open it dry.
        let mut state = engine_state(&project, &decoded).map_err(mixer_error)?;
        state
            .mix
            .check_sample_rate(lock(&self.controller).sample_rate())
            .map_err(|e| SessionError::InvalidValue(e.to_string()))?;
        self.settle_recording()?;
        self.admit_media(&files, &decoded)?;
        let (song, error) =
            match crate::compile_song::compile_song(&project, &decoded, None, self.song_note_limit)
            {
                Ok(song) => (song, None),
                Err(error) => (Default::default(), Some(error)),
            };
        state.song = song.clone();
        state.playback_mode = if error.is_some() {
            grooviq_engine::PlaybackMode::Pattern
        } else {
            Self::timeline(&project, song.clone()).mode
        };
        state.song_loop = if error.is_some() {
            None
        } else {
            Self::timeline(&project, song.clone()).region
        };
        let mark = {
            let mut controller = lock(&self.controller);
            // `load_state` stops the transport, then swaps the graph: opening never plays.
            controller.load_state(state)?;
            controller.record_mark()
        };
        // The new notes get a revision above the old ones, so an edit made against the old
        // project reads as stale.
        let revision = self.current().revision.wrapping_add(1);
        let mut project = project;
        for p in &mut project.patterns {
            p.revision = revision;
        }
        // Nothing recorded for the old project reaches this one: every event before the new
        // state's barrier is dropped, and so are the old project's notes still waiting.
        //
        // The recorder always waits for the newest barrier: the engine keeps only the newest
        // one that did not fit in a full queue, so an older one may never come. A refusal
        // running (recording switched itself off) keeps counting: its barrier moves forward
        // to this state's, which comes after the queued record-off or the person's own
        // switch. Only an owed record-off, not queued yet, keeps waiting for its own barrier,
        // which will come after this one.
        if !self.recorder.refusing() {
            self.recorder.reset_until_mark(mark);
        } else if !self.owed_disarm {
            self.recorder.await_mark(mark);
        }
        self.pending.clear();
        self.live_presses
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.history.clear();
        self.pattern_histories.clear();
        self.arrangement_history.clear();
        self.trim_history = Default::default();
        self.record_settle = None;
        self.project = project;
        self.compiled_song = song;
        self.song_error = error;
        self.files = files;
        self.decoded = decoded;
        for token in self.media_requests.values() {
            token
                .cancellation()
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.media_requests.clear();
        self.latest_media_requests.clear();
        self.media_tickets.clear();
        self.cancelled_media_tickets.clear();
        self.media_peaks.clear();
        self.revision += 1;
        self.last_history_order = None;
        self.saved = None;
        self.dirty = false;
        self.generation += 1;
        self.trim_identities.clear();
        self.arrangement_revision = 0;
        self.pattern_note_floors.clear();
        self.pending_since = None;
        self.structural_since = None;
        self.autosaved_at = None;
        Ok(())
    }

    /// Starts an untitled, empty project (the transport stops).
    pub fn new_project(&mut self) -> Result<ProjectView, SessionError> {
        self.replace(Project::new(), SampleFiles::new(), HashMap::new())?;
        self.path = None;
        Ok(self.view())
    }

    /// Opens a loaded project: the transport stops, then the engine takes its state. Nothing
    /// plays. `warnings` are the loader's, the decoder's and any fixes `validate` makes.
    pub fn open(
        &mut self,
        path: &Path,
        loaded: impl Into<LoadedForOpen>,
        decoded: DecodedSamples,
    ) -> Result<OpenReport, SessionError> {
        let LoadedForOpen {
            loaded,
            unchanged_at,
        } = loaded.into();
        let warnings = self.replace_loaded(path, loaded, decoded)?;
        if warnings.is_empty()
            && unchanged_at.as_deref() == Some(path)
            && path
                .file_stem()
                .is_some_and(|stem| stem.to_string_lossy() == self.project.name)
        {
            self.remember_saved_state();
        }
        Ok(OpenReport {
            view: self.view(),
            warnings,
        })
    }

    fn replace_loaded(
        &mut self,
        path: &Path,
        loaded: Loaded,
        decoded: DecodedSamples,
    ) -> Result<Vec<String>, SessionError> {
        let Loaded {
            mut project,
            samples,
            mut warnings,
        } = loaded;
        warnings.extend(project.validate());
        warnings.extend(decoded.warnings);
        for asset in &project.arrangement.audio_assets {
            if let Some(sample) = decoded.samples.get(&asset.id)
                && (sample.rate != asset.source_rate as f32
                    || sample.l.len() as u64 != asset.source_frames)
            {
                warnings.push(format!("Audio asset {}: decoded geometry differs from saved source metadata; saved trim duration is preserved.", asset.id));
            }
        }
        self.replace(project, samples, decoded.samples)?;
        if let Some(error) = &self.song_error {
            warnings.push(error.clone());
        }
        self.path = Some(path.to_path_buf());
        Ok(warnings)
    }

    /// Opens a recovered autosave as the project it was: at its `original` path (or untitled),
    /// unsaved, and due for an autosave.
    pub fn open_recovered(
        &mut self,
        original: Option<&Path>,
        loaded: Loaded,
        decoded: DecodedSamples,
        now: Instant,
    ) -> Result<OpenReport, SessionError> {
        let warnings = self.replace_loaded(original.unwrap_or(Path::new("")), loaded, decoded)?;
        self.path = original.map(Path::to_path_buf);
        self.changed_outside_history(true, now);
        Ok(OpenReport {
            view: self.view(),
            warnings,
        })
    }

    /// Snapshots what to save, cheaply (the sample bytes are shared). `path` is a new place
    /// to save to (`.grooviq` is added if it has another extension); `None` saves where the
    /// project lives, or is `NoPath` if it is untitled. One save at a time: while a job is out,
    /// this is `SaveRunning`. End every job with [`Self::finish_save`] or [`Self::save_failed`].
    pub fn begin_save(&mut self, path: Option<&Path>) -> Result<SaveJob, SessionError> {
        self.settle_recording()?;
        if self.saving {
            return Err(SessionError::SaveRunning);
        }
        let path = match path {
            Some(p) if p.extension().is_some_and(|e| e == EXTENSION) => p.to_path_buf(),
            Some(p) => {
                let mut name = p.as_os_str().to_owned();
                name.push(".");
                name.push(EXTENSION);
                PathBuf::from(name)
            }
            None => self.path.clone().ok_or(SessionError::NoPath)?,
        };
        let mut project = self.project.clone();
        if let Some(stem) = path.file_stem() {
            project.name = stem.to_string_lossy().into_owned();
        }
        self.saving = true;
        Ok(SaveJob {
            project,
            samples: self.files.clone(),
            path,
            revision: self.revision,
            generation: self.generation,
        })
    }

    /// Records a finished save: the project lives at the job's path now, and is clean unless
    /// it changed while the file was written. Returns false, changing nothing, if another
    /// project was opened or started meanwhile: the file holds the old project, and the new
    /// one must not take its path.
    pub fn finish_save(&mut self, job: &SaveJob) -> bool {
        self.saving = false;
        if job.generation != self.generation {
            return false;
        }
        self.path = Some(job.path.clone());
        self.project.name = job.project.name.clone();
        if self.revision == job.revision {
            self.remember_saved_state();
            self.pending_since = None;
            self.structural_since = None;
        } else {
            // The old checkpoint is obsolete as soon as this save replaces its file.
            self.saved = None;
            self.dirty = true;
        }
        true
    }

    /// Records disk facts after rename without clearing dirty or autosave protection.
    pub fn save_unconfirmed(&mut self, job: &SaveJob) {
        self.saving = false;
        if job.generation != self.generation {
            return;
        }
        self.path = Some(job.path.clone());
        self.project.name = job.project.name.clone();
        self.saved = None;
        self.dirty = true;
        self.pending_since.get_or_insert_with(Instant::now);
    }

    /// Records that the job from [`Self::begin_save`] was not written: the next save may start.
    pub fn save_failed(&mut self) {
        self.saving = false;
    }

    /// Applies the disk outcome once for both synchronous and desktop saves.
    pub fn end_save(
        &mut self,
        job: &SaveJob,
        result: Result<(), SessionError>,
    ) -> Result<bool, SessionError> {
        match result {
            Ok(()) => Ok(self.finish_save(job)),
            Err(e) => {
                if e.directory_unconfirmed() {
                    self.save_unconfirmed(job);
                } else {
                    self.save_failed();
                }
                Err(e)
            }
        }
    }

    /// Saves in one go (begin, write, finish). Holds the session the whole time: the app splits
    /// the steps instead.
    pub fn save(&mut self, path: Option<&Path>, backups: &Path) -> Result<PathBuf, SessionError> {
        self.save_on(path, backups, &mut RealDisk)
    }

    /// Synchronous save with an injectable disk boundary.
    #[doc(hidden)]
    pub fn save_on(
        &mut self,
        path: Option<&Path>,
        backups: &Path,
        disk: &mut impl Disk,
    ) -> Result<PathBuf, SessionError> {
        let job = self.begin_save(path)?;
        let result = job.write_on(backups, disk);
        self.end_save(&job, result)?;
        Ok(job.path)
    }

    /// The engine's state for an export: the project as it plays now.
    pub fn export_state(&self) -> Result<EngineState, SessionError> {
        let mut state = engine_state(&self.project, &self.decoded).map_err(mixer_error)?;
        state.song = self.compiled_song.clone();
        state.playback_mode = Self::timeline(&self.project, state.song.clone()).mode;
        state.song_loop = if self.song_error.is_none() {
            Self::timeline(&self.project, state.song.clone()).region
        } else {
            None
        };
        Ok(state)
    }

    pub fn checked_export_state(&self) -> Result<EngineState, SessionError> {
        if self.project.play_mode == crate::PlayMode::Song
            && let Some(error) = &self.song_error
        {
            return Err(SessionError::InvalidValue(error.clone()));
        }
        self.export_state()
    }

    /// Explicit backend export selection. Whole Song ignores transport looping; a region
    /// is returned only when requested and a valid enabled Song loop exists.
    pub fn export_selection(
        &self,
        loop_region: bool,
    ) -> Result<(EngineState, Option<grooviq_engine::SongLoop>), SessionError> {
        let state = self.checked_export_state()?;
        let region = if loop_region {
            if state.playback_mode != grooviq_engine::PlaybackMode::Song {
                return Err(SessionError::InvalidValue(
                    "Loop-region export requires Song mode.".into(),
                ));
            }
            Some(state.song_loop.ok_or_else(|| {
                SessionError::InvalidValue("Enable a Song loop region before exporting it.".into())
            })?)
        } else {
            None
        };
        Ok((state, region))
    }

    /// Whether closing would lose changes.
    pub fn needs_confirmation(&self) -> bool {
        self.dirty()
    }

    /// Whether to write an autosave now: something changed since the last one, and either the
    /// first such change is [`AUTOSAVE_PERIOD`] old or a structural one is [`STRUCTURAL_DELAY`]
    /// old.
    ///
    /// The minute counts from the first change since the last autosave, not from the autosave
    /// itself: no edit stays out of the autosave longer than [`AUTOSAVE_PERIOD`], and editing
    /// without a pause still autosaves once a minute. (Counting from the last autosave would
    /// risk the same 60 s of work at most.)
    pub fn autosave_due(&self, now: Instant) -> bool {
        let old = |since: Option<Instant>, age: Duration| {
            since.is_some_and(|t| now.saturating_duration_since(t) >= age)
        };
        self.dirty()
            && (old(self.pending_since, AUTOSAVE_PERIOD)
                || old(self.structural_since, STRUCTURAL_DELAY))
    }

    /// What to autosave (the project, its sample bytes, its path), or `None` if nothing changed
    /// since the last autosave. Restarts the clock: changes after `now` wait for the next one.
    /// Cheap: write the result without the session's lock.
    pub fn autosave_snapshot(
        &mut self,
        _now: Instant,
    ) -> Option<(Project, SampleFiles, Option<PathBuf>)> {
        if !self.dirty() || self.pending_since.is_none() {
            return None;
        }
        self.pending_since = None;
        self.structural_since = None;
        Some((self.project.clone(), self.files.clone(), self.path.clone()))
    }

    /// Counts projects: opening, starting a new one or restoring one moves it on. Read it
    /// together with [`Self::autosave_snapshot`] to know which project a snapshot is of.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Records that an autosave of the project at `generation` was written at `unix_seconds`.
    /// Returns false, recording nothing, if another project was opened or started meanwhile:
    /// that autosave holds the old project, and the caller must remove it.
    pub fn autosaved(&mut self, generation: u64, unix_seconds: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.autosaved_at = Some(unix_seconds);
        true
    }

    /// Records that the autosave taken last could not be written: it is tried again after
    /// [`AUTOSAVE_PERIOD`].
    pub fn autosave_failed(&mut self, now: Instant) {
        if self.dirty() {
            self.pending_since.get_or_insert(now);
        }
    }
}

/// The peaks of `sample`'s mono mix (`(l + r) / 2`) as `points` (clamped to
/// 1..=[`MAX_WAVEFORM_POINTS`]) pairs of little-endian `(min f32, max f32)`: point `i` covers
/// frames `i * len / points` up to the next point's first frame (at least one frame). Empty
/// for an empty sample.
pub fn waveform_peaks(sample: &SampleBuf, points: u32) -> Vec<u8> {
    let len = sample.l.len().min(sample.r.len());
    if len == 0 {
        return Vec::new();
    }
    let points = points.clamp(1, MAX_WAVEFORM_POINTS) as usize;
    let mut out = Vec::with_capacity(points * 8);
    for i in 0..points {
        let start = (i * len / points).min(len - 1);
        let end = ((i + 1) * len / points).max(start + 1).min(len);
        let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
        for (l, r) in sample.l[start..end].iter().zip(&sample.r[start..end]) {
            let v = (l + r) * 0.5;
            min = min.min(v);
            max = max.max(v);
        }
        out.extend_from_slice(&min.to_le_bytes());
        out.extend_from_slice(&max.to_le_bytes());
    }
    out
}

/// `"sampler"` or `"synth"`, for messages.
fn device(kind: InstrumentKind) -> &'static str {
    match kind {
        InstrumentKind::Sampler => "sampler",
        InstrumentKind::Synth => "synth",
    }
}

/// The index of `param` among the parameters of `kind`.
fn param_index(kind: InstrumentKind, param: &str) -> Result<usize, SessionError> {
    let ids: &[&str] = match kind {
        InstrumentKind::Sampler => &sampler::PARAM_IDS,
        InstrumentKind::Synth => &synth::PARAM_IDS,
    };
    ids.iter()
        .position(|p| *p == param)
        .ok_or_else(|| SessionError::UnknownParam {
            device: device(kind),
            param: param.to_owned(),
        })
}

/// All values of a factory synth preset, `(index, value)` in `SPEC` order.
fn synth_preset(name: &str) -> Result<Vec<(u32, f32)>, SessionError> {
    if name != INIT_PRESET && !synth::PRESETS.contains(&name) {
        return Err(SessionError::InvalidValue(format!(
            "There is no synth preset called \"{name}\"."
        )));
    }
    synth::preset(name).ok_or_else(|| {
        SessionError::InvalidValue(format!("There is no synth preset called \"{name}\"."))
    })
}

/// `(index, value)` pairs as a synth parameter map by id.
fn by_param_id(values: &[(u32, f32)]) -> BTreeMap<String, f32> {
    values
        .iter()
        .filter_map(|&(i, v)| {
            synth::PARAM_IDS
                .get(i as usize)
                .map(|id| ((*id).to_owned(), v))
        })
        .collect()
}

/// Adds a one-step hit with a fresh id, or refuses: at [`MAX_NOTES_PER_PATTERN`] (a file keeps
/// no more, so a save would lose it) or when the pattern's ids are used up.
fn add_hit(p: &mut Pattern, channel: u32, tick: u32, velocity: f32) -> Result<(), SessionError> {
    if p.notes.len() >= MAX_NOTES_PER_PATTERN {
        return Err(SessionError::TooManyNotes);
    }
    let id = p.take_note_id().ok_or(SessionError::NoteIdsUsedUp)?;
    p.notes.push(hit(id, channel, tick, velocity));
    Ok(())
}

fn hit(id: u32, channel: u32, tick: u32, velocity: f32) -> Note {
    Note {
        id,
        channel,
        tick,
        length: HIT_TICKS,
        key: DEFAULT_KEY,
        velocity,
        slide: false,
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;
    use grooviq_engine::{EngineConfig, engine};

    #[test]
    fn delivered_ui_releases_leave_no_ledger_entries() {
        let (c, mut processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let c = Arc::new(Mutex::new(c));
        let mut s = Session::new(c.clone());
        let ch = s
            .add_synth_channel("Init", Instant::now())
            .unwrap()
            .channels[0]
            .id;

        let press = s.live_note_on(ch, 60, 0.8).unwrap();
        assert_eq!(s.live_presses.lock().unwrap().len(), 1);
        s.live_note_off(ch, 60, press).unwrap();
        assert!(s.live_presses.lock().unwrap().is_empty());

        let press = s.live_note_on(ch, 60, 0.8).unwrap();
        while lock(&c).send(Command::SetMetronome { on: false }).is_ok() {}
        s.live_note_off(ch, 60, press).unwrap();
        assert_eq!(s.live_presses.lock().unwrap().len(), 1);
        processor.process(&mut [0.0; 128], 2);
        s.collect_recorded(&[], Instant::now());
        assert!(s.live_presses.lock().unwrap().is_empty());
    }

    #[test]
    fn ui_release_debt_is_bounded_by_accepted_presses_and_channel_lifetime() {
        let (c, mut processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let c = Arc::new(Mutex::new(c));
        let mut s = Session::new(c.clone());
        let ch = s
            .add_synth_channel("Init", Instant::now())
            .unwrap()
            .channels[0]
            .id;
        let press = s.live_note_on(ch, 60, 0.8).unwrap();
        while lock(&c).send(Command::SetMetronome { on: false }).is_ok() {}
        assert!(s.live_note_on(ch, 61, 0.8).is_err());
        s.live_note_off(ch, 61, NoteId::live()).unwrap();
        s.live_note_off(ch, 60, press).unwrap();
        s.live_note_off(ch, 60, press).unwrap();
        assert_eq!(s.live_presses.lock().unwrap().len(), 1);
        processor.process(&mut [0.0; 128], 2);
        // Replace the channel while keeping its numeric id, as remove/re-add and load do.
        let state = lock(&c).state().clone();
        lock(&c).load_state(state).unwrap();
        let fresh = s.live_note_on(ch, 60, 0.8).unwrap();
        // Inject old debt explicitly so the maintenance instance guard is independently tested.
        let instance = lock(&c).channel_instance(ChannelId(ch)).unwrap();
        s.live_presses.lock().unwrap().push(LivePress {
            channel: ch,
            key: 60,
            press,
            instance: instance - 1,
            releasing: true,
        });
        while lock(&c).send(Command::SetMetronome { on: false }).is_ok() {}
        s.collect_recorded(&[], Instant::now());
        assert_eq!(s.live_presses.lock().unwrap().len(), 1);
        assert_eq!(s.live_presses.lock().unwrap()[0].press, fresh);
        processor.process(&mut [0.0; 128], 2);
        s.new_project().unwrap();
        assert!(s.live_presses.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod release_progress_tests {
    use super::*;
    use grooviq_engine::{EngineConfig, engine};

    #[test]
    fn ui_release_maintenance_makes_partial_progress() {
        let (c, mut p) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let c = Arc::new(Mutex::new(c));
        let mut s = Session::new(c.clone());
        let ch = s
            .add_synth_channel("Init", Instant::now())
            .unwrap()
            .channels[0]
            .id;
        let presses = [60, 61, 62].map(|k| s.live_note_on(ch, k, 0.8).unwrap());
        p.process(&mut [0.0; 128], 2);
        lock(&c).send(Command::SetMetronome { on: false }).unwrap();
        while lock(&c)
            .send_at(4800, Command::SetMetronome { on: false })
            .is_ok()
        {}
        for (key, press) in [60, 61, 62].into_iter().zip(presses) {
            s.live_note_off(ch, key, press).unwrap();
        }
        p.process(&mut [0.0; 128], 2); // Exactly one queue slot is free.
        s.collect_recorded(&[], Instant::now());
        assert_eq!(s.live_presses.lock().unwrap().len(), 2);
        assert_eq!(s.live_presses.lock().unwrap()[0].press, presses[1]);
        for _ in 0..80 {
            p.process(&mut [0.0; 128], 2);
        }
        s.collect_recorded(&[], Instant::now());
        assert!(s.live_presses.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod dirty_tests {
    use super::*;
    use crate::arrangement_edit::{ArrangementEdit, HistoryEffect};
    use crate::mixer::Level;
    use crate::mixer_edit::{IdRef, MixerEdit};
    use crate::pattern_edit::PatternEdit;
    use grooviq_engine::{EngineConfig, engine};

    fn rig() -> (Session, tempfile::TempDir, Instant) {
        let (controller, _processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        (
            Session::new(Arc::new(Mutex::new(controller))),
            tempfile::tempdir().unwrap(),
            Instant::now(),
        )
    }

    fn save(s: &mut Session, dir: &tempfile::TempDir) -> PathBuf {
        s.save(Some(&dir.path().join("beat.grooviq")), dir.path())
            .unwrap()
    }

    fn fader(s: &mut Session, db: f32, now: Instant) {
        let stamp = s.mixer_view();
        assert!(
            s.edit_mixer(
                stamp.generation,
                stamp.revision,
                vec![MixerEdit::SetFader {
                    track: IdRef::Id(0),
                    level: Level::Db(db),
                    pan: 0.0,
                }],
                now,
            )
            .unwrap()
            .changed
        );
    }

    fn step(s: &mut Session, undo: bool, now: Instant) -> HistoryEffect {
        let stamp = s.arrangement_view();
        s.step_project_history(stamp.generation, stamp.revision, undo, now)
            .unwrap()
            .effect
    }

    fn open_file(s: &mut Session, path: &Path) -> OpenReport {
        let loaded = crate::file::load_for_open(path, &s.media_budget()).unwrap();
        let decoded = decode_samples(loaded.loaded());
        s.open(path, loaded, decoded).unwrap()
    }

    #[test]
    fn clean_v4_open_saves_identical_bytes() {
        let (mut s, dir, now) = rig();
        // Four frames of mono 48 kHz, 16-bit PCM.
        let file = crate::SampleFile {
            name: "unit.wav".into(),
            bytes: Arc::new(
                b"RIFF\x2c\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0\x77\x01\0\x02\0\x10\0data\x08\0\0\0\0\0\x0a\0\xf6\xff\0\0"
                    .to_vec(),
            ),
        };
        s.add_channel(
            Path::new("unit.wav"),
            file,
            Arc::new(SampleBuf {
                rate: 48_000.0,
                l: vec![0.0; 4],
                r: vec![0.0; 4],
            }),
            now,
        )
        .unwrap();
        let path = save(&mut s, &dir);
        let original = std::fs::read(&path).unwrap();
        let report = open_file(&mut s, &path);
        assert!(report.warnings.is_empty());
        assert!(!report.view.dirty);
        s.save(None, dir.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn mixer_undo_returns_to_a_clean_v4_open() {
        let (mut s, dir, now) = rig();
        let path = save(&mut s, &dir);
        assert!(!open_file(&mut s, &path).view.dirty);
        fader(&mut s, -3.0, now);
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
        step(&mut s, false, now);
        assert!(s.needs_confirmation());
    }

    #[test]
    fn a_migrated_open_stays_dirty_after_undo() {
        use std::io::Write;

        let (mut s, dir, now) = rig();
        for version in 1..crate::model::VERSION {
            let path = dir.path().join("legacy.grooviq");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            zip.start_file("project.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(
                format!(
                    r#"{{"format":"grooviq-project","version":{version},"name":"legacy","nextId":1,"patterns":[{{"id":0,"nextNoteId":1}}]}}"#
                )
                .as_bytes(),
            )
            .unwrap();
            zip.finish().unwrap();
            let report = open_file(&mut s, &path);
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            assert!(!report.view.dirty);
            fader(&mut s, -3.0, now);
            step(&mut s, true, now);
            assert!(s.needs_confirmation(), "version {version}");
        }
    }

    #[test]
    fn open_repairs_with_or_without_warnings_stay_dirty_after_undo() {
        let (mut s, dir, now) = rig();
        let path = dir.path().join("beat.grooviq");
        for warned in [true, false] {
            let mut p = Project::new();
            p.name = "beat".into();
            if warned {
                p.bpm = 1.0;
            } else {
                p.patterns[0].next_note_id = 0;
            }
            crate::save(&p, &SampleFiles::new(), &path).unwrap();
            let report = open_file(&mut s, &path);
            assert_eq!(!report.warnings.is_empty(), warned);
            assert!(!report.view.dirty);
            fader(&mut s, -3.0, now);
            step(&mut s, true, now);
            assert!(s.needs_confirmation());
        }
    }

    #[test]
    fn a_renamed_file_stays_dirty_after_undo() {
        let (mut s, dir, now) = rig();
        let original = save(&mut s, &dir);
        let path = dir.path().join("renamed.grooviq");
        std::fs::rename(original, &path).unwrap();
        let report = open_file(&mut s, &path);
        assert!(report.warnings.is_empty());
        assert!(!report.view.dirty);
        fader(&mut s, -3.0, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
    }

    #[test]
    fn mixer_undo_and_redo_report_the_saved_position() {
        let (mut s, dir, now) = rig();
        save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        assert!(s.view().dirty);
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
        step(&mut s, false, now);
        assert!(s.needs_confirmation());
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
    }

    #[test]
    fn save_after_edit_makes_undo_dirty_and_redo_clean() {
        let (mut s, dir, now) = rig();
        save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        save(&mut s, &dir);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
        step(&mut s, false, now);
        assert!(!s.needs_confirmation());
    }

    #[test]
    fn redo_can_restore_saved_notes_in_an_inactive_pattern() {
        let (mut s, dir, now) = rig();
        let channel = s.add_synth_channel("Init", now).unwrap().channels[0].id;
        let pattern = s.current().id;
        s.apply(Edit::ToggleStep { channel, step: 0 }, now).unwrap();
        let stamp = s.patterns();
        s.edit_patterns(
            stamp.generation,
            stamp.revision,
            PatternEdit::Create {
                name: "Other".into(),
            },
            now,
        )
        .unwrap();
        let other = s.project.patterns.last().unwrap().id;
        let stamp = s.patterns();
        s.edit_patterns(
            stamp.generation,
            stamp.revision,
            PatternEdit::Select { pattern: other },
            now,
        )
        .unwrap();
        save(&mut s, &dir);
        assert!(
            matches!(step(&mut s, true, now), HistoryEffect::Notes { pattern: id } if id == pattern)
        );
        assert!(s.needs_confirmation());
        step(&mut s, false, now);
        assert_eq!(s.current().id, other);
        assert!(!s.needs_confirmation());
        step(&mut s, true, now);
        save(&mut s, &dir);
        step(&mut s, false, now);
        assert!(s.needs_confirmation());
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
    }

    #[test]
    fn remaining_note_and_arrangement_edits_keep_the_project_dirty() {
        let (mut s, dir, now) = rig();
        let channel = s.add_synth_channel("Init", now).unwrap().channels[0].id;
        s.apply(Edit::ToggleStep { channel, step: 0 }, now).unwrap();
        save(&mut s, &dir);
        s.apply(Edit::ClearStep { channel, step: 0 }, now).unwrap();
        let stamp = s.arrangement_view();
        s.edit_arrangement(
            stamp.generation,
            stamp.revision,
            vec![ArrangementEdit::SetLoop {
                region: Some(crate::LoopRegion {
                    start_tick: 0,
                    end_tick: 3840,
                    enabled: false,
                }),
            }],
            now,
        )
        .unwrap();
        fader(&mut s, -3.0, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
    }

    #[test]
    fn replacing_sample_bytes_keeps_confirmation_through_undo_and_redo() {
        let (mut s, dir, now) = rig();
        let sample = Arc::new(SampleBuf {
            rate: 48_000.0,
            l: vec![0.0; 8],
            r: vec![0.0; 8],
        });
        let file = |byte| crate::SampleFile {
            name: "beat.wav".into(),
            bytes: Arc::new(vec![byte; 8]),
        };
        let path = Path::new("beat.wav");
        let channel = s
            .add_channel(path, file(0), sample.clone(), now)
            .unwrap()
            .channels[0]
            .id;
        save(&mut s, &dir);
        s.set_channel_sample(channel, path, file(1), sample, now)
            .unwrap();
        fader(&mut s, -3.0, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
        step(&mut s, false, now);
        assert!(s.needs_confirmation());
    }

    #[test]
    fn undo_sampler_trim_restores_the_saved_configuration() {
        let (mut s, dir, now) = rig();
        let channel = s
            .add_channel(
                Path::new("clip.wav"),
                crate::SampleFile {
                    name: "clip.wav".into(),
                    bytes: Arc::new(Vec::new()),
                },
                Arc::new(SampleBuf {
                    rate: 48_000.0,
                    l: vec![0.0; 8],
                    r: vec![0.0; 8],
                }),
                now,
            )
            .unwrap()
            .channels[0]
            .id;
        s.apply(
            Edit::SetSamplerParam {
                channel,
                param: "start".into(),
                value: 0.25,
            },
            now,
        )
        .unwrap();
        save(&mut s, &dir);
        let before = s.sampler_trim_view(channel).unwrap();
        let reset = s.sampler_trim_reset(channel, &before.stamp, now).unwrap();
        assert!(reset.changed);
        assert!(s.needs_confirmation());
        s.sampler_trim_undo(
            channel,
            &reset.state.stamp,
            reset.state.undo.as_deref().unwrap(),
            now,
        )
        .unwrap();
        assert!(!s.needs_confirmation());
    }

    #[test]
    fn autosave_and_recovery_do_not_establish_a_clean_position() {
        let (mut s, dir, now) = rig();
        let path = save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        let (project, samples, original) = s.autosave_snapshot(now).unwrap();
        assert!(s.autosaved(s.generation(), 1));
        assert!(s.needs_confirmation());
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
        let loaded = Loaded {
            project,
            samples,
            warnings: Vec::new(),
        };
        let decoded = decode_samples(&loaded);
        assert_eq!(original.as_ref(), Some(&path));
        s.open_recovered(original.as_deref(), loaded, decoded, now)
            .unwrap();
        assert!(s.needs_confirmation());
        fader(&mut s, -6.0, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
    }

    #[test]
    fn replacing_a_redo_branch_conservatively_keeps_the_project_dirty() {
        let (mut s, dir, now) = rig();
        save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        step(&mut s, true, now);
        assert!(!s.needs_confirmation());
        fader(&mut s, -6.0, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
        save(&mut s, &dir);
        assert!(!s.needs_confirmation());
    }

    #[test]
    fn history_eviction_conservatively_keeps_the_project_dirty() {
        let (mut s, dir, now) = rig();
        s.set_history_limits(crate::HistoryLimits {
            steps: 1,
            ..Default::default()
        });
        save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        fader(&mut s, 0.0, now);
        fader(&mut s, -6.0, now);
        assert_eq!(s.history_usage().0, 1);
        step(&mut s, true, now);
        assert!(matches!(step(&mut s, true, now), HistoryEffect::Empty));
        assert_eq!(s.project.mixer, crate::mixer::Mixer::default());
        assert!(s.needs_confirmation());
    }

    #[test]
    fn a_reentered_setting_conservatively_keeps_the_project_dirty() {
        let (mut s, dir, now) = rig();
        let saved_bpm = s.project.bpm;
        save(&mut s, &dir);
        s.apply(Edit::SetTempo { bpm: 120.0 }, now).unwrap();
        s.apply(Edit::SetTempo { bpm: saved_bpm }, now).unwrap();
        fader(&mut s, -3.0, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
    }

    #[test]
    fn undo_does_not_hide_a_changed_persisted_id_floor() {
        let (mut s, dir, now) = rig();
        let channel = s.add_synth_channel("Init", now).unwrap().channels[0].id;
        let path = save(&mut s, &dir);
        s.apply(Edit::ToggleStep { channel, step: 0 }, now).unwrap();
        step(&mut s, true, now);
        assert!(s.current().notes.is_empty());
        assert!(
            s.current().next_note_id > crate::load(&path).unwrap().project.patterns[0].next_note_id
        );
        assert!(s.needs_confirmation());
    }

    #[test]
    fn completed_in_flight_save_cannot_leave_the_previous_file_as_checkpoint() {
        let (mut s, dir, now) = rig();
        save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        let job = s.begin_save(None).unwrap();
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
        job.write(dir.path()).unwrap();
        assert!(s.finish_save(&job));
        assert!(s.needs_confirmation());
        step(&mut s, false, now);
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
    }

    #[test]
    fn unconfirmed_save_cannot_leave_the_previous_file_as_checkpoint() {
        struct RefuseTargetDirectory<'a>(&'a Path);

        impl Disk for RefuseTargetDirectory<'_> {
            fn sync_folder(&mut self, path: &Path) -> io::Result<()> {
                if path == self.0 {
                    return Err(io::Error::other("target directory sync refused"));
                }
                RealDisk.sync_folder(path)
            }
        }

        let (mut s, dir, now) = rig();
        let path = save(&mut s, &dir);
        fader(&mut s, -3.0, now);
        let error = s
            .save_on(None, dir.path(), &mut RefuseTargetDirectory(&path))
            .unwrap_err();
        assert!(error.directory_unconfirmed());
        assert_eq!(
            crate::load(&path).unwrap().project.mixer.tracks[0].level,
            Level::Db(-3.0)
        );
        step(&mut s, true, now);
        assert!(s.needs_confirmation());
    }
}

mod media;
mod mixer;
pub use media::{MediaIntent, MediaTarget};

pub mod sampler_trim;
