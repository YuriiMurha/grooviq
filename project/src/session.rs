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

use crate::backup::backup_before_save;
use crate::compile::synth_values;
use crate::compile::{compile_pattern, engine_state, sampler_values};
use crate::file::{Disk, Loaded, RealDisk, SampleFiles, stage_save};
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
    ChannelId, ChannelMix, ChannelSetup, Command, ControlError, Controller, EngineState,
    InstrumentKind, MAX_CHANNELS, MAX_CHOKE_GROUP, MAX_SAMPLE_SECONDS, NoteId, RecordEvent,
    STEP_TICKS, decode_sample,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
    /// Counts projects ([`Session::generation`]): requests that edit the project carry it, so
    /// one made for a project that was replaced since is refused.
    pub generation: u64,
}

/// A channel's instrument, as the UI names it: `"sampler"` or `"synth"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChannelKind {
    Sampler,
    Synth,
}

/// One channel of the rack.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: u32,
    pub name: String,
    /// An index into the palette (0 = `--ink-1`).
    pub color: u8,
    /// The sample's file name.
    pub sample_name: Option<String>,
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
    /// One distinct sound seen so far, and what decoding it gave.
    struct Seen<'a> {
        bytes: &'a Arc<Vec<u8>>,
        extension: Option<&'a str>,
        result: Result<Arc<SampleBuf>, String>,
    }
    let mut out = DecodedSamples::default();
    let mut seen: Vec<Seen<'_>> = Vec::new();
    for channel in &loaded.project.channels {
        if channel.sample.is_none() {
            continue;
        }
        let Some(file) = loaded.samples.get(&channel.id) else {
            // The loader already said why.
            out.missing.insert(channel.id);
            continue;
        };
        let extension = Path::new(&file.name).extension().and_then(|e| e.to_str());
        let known = seen.iter().position(|s| {
            s.extension == extension
                && (Arc::ptr_eq(s.bytes, &file.bytes) || **s.bytes == *file.bytes)
        });
        let at = known.unwrap_or_else(|| {
            let result = decode_sample(&file.bytes, extension, MAX_SAMPLE_SECONDS)
                .map(Arc::new)
                .map_err(|e| e.to_string());
            seen.push(Seen {
                bytes: &file.bytes,
                extension,
                result,
            });
            seen.len() - 1
        });
        match &seen[at].result {
            Ok(buf) => {
                out.samples.insert(channel.id, Arc::clone(buf));
            }
            Err(e) => {
                out.missing.insert(channel.id);
                out.warnings.push(format!(
                    "Channel \"{}\": its sample \"{}\" could not be decoded ({e}). Relink it to hear the channel.",
                    channel.name, file.name
                ));
            }
        }
    }
    out
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

impl SaveJob {
    /// Saves: writes the new file beside the old one, then backs up the file it replaces
    /// (keeping [`MAX_BACKUPS`] in `backups`), then puts the new one in place. A save that is
    /// refused or fails before that point makes no backup, so it never rotates an older one
    /// out. Run it without the session's lock.
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
        backup_before_save(&self.path, backups, MAX_BACKUPS).map_err(SessionError::Backup)?;
        staged.commit(disk).map_err(SessionError::Save)
    }
}

/// The open project. See the module docs.
pub struct Session {
    controller: Arc<Mutex<Controller>>,
    project: Project,
    /// Each channel's sample bytes, saved with the project.
    files: SampleFiles,
    /// Each channel's decoded sample, for the engine.
    decoded: HashMap<u32, Arc<SampleBuf>>,
    path: Option<PathBuf>,
    /// Counts changes; a save clears `dirty` only if none came in while it was written.
    revision: u64,
    saved_revision: u64,
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
}

struct LivePress {
    channel: u32,
    key: u8,
    press: NoteId,
    instance: u64,
    releasing: bool,
}

/// What committing waiting recorded notes did.
enum Commit {
    /// Added (the pattern's new revision, if any note was).
    Done(Option<u32>),
    /// The engine's queue was full: the notes wait for the next collect.
    Busy,
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

impl Session {
    /// An untitled, empty project, with the engine set to match it.
    pub fn new(controller: Arc<Mutex<Controller>>) -> Self {
        let session = Self {
            controller,
            project: Project::new(),
            files: SampleFiles::new(),
            decoded: HashMap::new(),
            path: None,
            revision: 0,
            saved_revision: 0,
            generation: 0,
            saving: false,
            pending_since: None,
            structural_since: None,
            autosaved_at: None,
            recorder: Recorder::new(),
            recording: false,
            pending: Vec::new(),
            owed_disarm: false,
            live_presses: Mutex::new(Vec::new()),
            history: NoteHistory::default(),
        };
        // A fresh queue always has room for a whole state.
        let synced = session.sync_engine();
        debug_assert!(synced.is_ok(), "{synced:?}");
        let mut session = session;
        let mark = lock(&session.controller).record_mark();
        session.recorder.reset_until_mark(mark);
        session
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
        self.revision != self.saved_revision
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
            generation: self.generation,
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
        self.pending_since.get_or_insert(now);
        if structural {
            self.structural_since.get_or_insert(now);
        }
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
        let structural = edit.structural();
        if self.apply_inner(edit)? {
            self.changed(structural, now);
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
                let changed = self.edit_pattern(|p| {
                    p.steps = steps;
                    Ok(())
                })?;
                // Set by hand, even to the length it has: undo never takes it back.
                self.history.set_length(steps);
                Ok(changed)
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
                self.send(Command::SetTempo { bpm })?;
                self.project.bpm = bpm;
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
        if pattern.notes != self.current().notes {
            pattern.revision = pattern.revision.wrapping_add(1);
        }
        self.keep_pattern(pattern)?;
        match (notes, take) {
            (Some(notes), Some(take)) => self.history.record(notes, take, steps, growth),
            (Some(notes), None) => self.history.push(notes, steps, growth),
            // Notes resubmitted as they were, growing a pattern shortened by hand: a step
            // that holds only that growth, so undo and redo play the length it gave.
            (None, _) if growth.is_some() && self.current().steps != steps => {
                self.history.push(NoteChange::none(), steps, growth);
            }
            (None, _) => {}
        }
        Ok(true)
    }

    /// Sends `pattern` to the engine (which goes on playing), then makes it the current one.
    fn keep_pattern(&mut self, pattern: Pattern) -> Result<(), SessionError> {
        let compiled = compile_pattern(&self.project, &pattern);
        lock(&self.controller).set_pattern(compiled)?;
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
        pattern.revision = pattern.revision.wrapping_add(1);
        self.keep_pattern(pattern)?;
        if undo {
            self.history.undone();
        } else {
            self.history.redone();
        }
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
        let mut patterns = self.project.patterns.clone();
        for p in &mut patterns {
            let before = p.notes.len();
            p.notes.retain(|n| n.channel != channel);
            if p.notes.len() != before {
                p.revision = p.revision.wrapping_add(1);
            }
        }
        {
            let mut controller = lock(&self.controller);
            controller.remove_channel(ChannelId(channel))?;
            // The engine skips notes of a channel it lacks; the pattern is sent anyway so its
            // state matches the project's.
            let mut rest = self.project.clone();
            rest.channels.remove(index);
            let current = patterns
                .iter()
                .find(|p| p.id == rest.current_pattern)
                .unwrap_or(&patterns[0]);
            let _ = controller.set_pattern(compile_pattern(&rest, current));
        }
        self.project.channels.remove(index);
        self.project.patterns = patterns;
        self.files.remove(&channel);
        self.decoded.remove(&channel);
        // Undo cannot bring a channel back (structural undo is M6), nor its notes.
        self.history.clear();
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
        let id = self.project.next_id;
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
        lock(&self.controller).add_channel(ChannelSetup::sampler(
            ChannelId(id),
            Some(Arc::clone(&sample)),
            sampler_values(&channel),
            ChannelMix::default(),
        ))?;
        self.project.channels.push(channel);
        self.project.next_id = id.saturating_add(1);
        self.files.insert(id, file);
        self.decoded.insert(id, sample);
        self.changed(true, now);
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
        let id = self.project.next_id;
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
        lock(&self.controller).add_channel(ChannelSetup::synth(
            ChannelId(id),
            synth_values(&channel),
            ChannelMix::default(),
        ))?;
        self.project.channels.push(channel);
        self.project.next_id = id.saturating_add(1);
        self.changed(true, now);
        Ok(self.view())
    }

    /// Applies a note edit to the current pattern (see [`NotesEdit`] and [`notes`]): all of it,
    /// or nothing for an unknown channel, the note cap or used-up ids. An edit that changes
    /// nothing (only unknown ids) leaves the revision and the dirty state alone.
    ///
    /// An edit made for another project (`edit.generation`: one replaced since) is refused
    /// before anything is applied: its ids name that project's notes. An older revision of this
    /// project (recording added notes meanwhile) is applied, and asks for a reload.
    pub fn edit_notes(&mut self, edit: NotesEdit, now: Instant) -> Result<NotesAck, SessionError> {
        if edit.generation != self.generation {
            return Ok(self.refused_ack());
        }
        let stale = edit.base_revision != self.current().revision;
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
    pub fn param_live(&self, channel: u32, param: &str, value: f32) -> Result<(), SessionError> {
        let (kind, at) = self.param_target(channel, param)?;
        let (min, max, _) = kind.spec()[at];
        let value = finite(value, &format!("{} value", device(kind)))?.clamp(min, max);
        self.send(Command::SetChannelParam {
            channel: ChannelId(channel),
            index: at as u32,
            value,
        })
    }

    /// Switches recording: while it is on and the transport plays, live notes are recorded
    /// into the pattern (see [`Self::collect_recorded`]). Switching off ends the take.
    ///
    /// After recording switched itself off (see [`Self::collect_recorded`]) and before the
    /// engine confirmed it, the switch goes with a fresh barrier: notes recorded before it are
    /// still refused (and reported), the new take's after it are kept, and an owed record-off
    /// is cancelled.
    pub fn set_recording(&mut self, on: bool) -> Result<(), SessionError> {
        if self.owed_disarm || self.recorder.refusing() {
            let id = lock(&self.controller).send_marked(Command::SetRecording { on })?;
            self.recorder.refuse_until_mark(Some(id));
            self.owed_disarm = false;
        } else {
            self.send(Command::SetRecording { on })?;
        }
        self.recording = on;
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
        if self.owed_disarm {
            self.send_disarm();
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
        self.pending.truncate(fits);

        let mut out = Recorded::default();
        let mut error = None;
        match self.commit_pending(now) {
            Ok(revision) => out.revision = revision,
            Err((e, lost, revision)) => {
                out.revision = revision;
                dropped += lost;
                reason = "error";
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
                self.recorder.await_mark(id);
            }
            Err(_) => self.owed_disarm = true,
        }
    }

    /// Adds the waiting recorded notes, take by take in the order they were played: each
    /// run of one take is a commit of that take (one undo step per take, however many commits
    /// it takes). A full engine queue keeps what is not in yet for the next collect; any other
    /// refusal loses it and says how many (with the revision of what did go in).
    fn commit_pending(
        &mut self,
        now: Instant,
    ) -> Result<Option<u32>, (SessionError, usize, Option<u32>)> {
        let mut revision = None;
        while let Some(&(take, _)) = self.pending.first() {
            let run = self.pending.iter().take_while(|(t, _)| *t == take).count();
            match self.commit_first(run, take, now) {
                Ok(Commit::Done(r)) => revision = r.or(revision),
                Ok(Commit::Busy) => break,
                Err((e, lost)) => return Err((e, lost, revision)),
            }
        }
        Ok(revision)
    }

    /// Adds the first `n` waiting recorded notes, all of `take`, as one edit.
    fn commit_first(
        &mut self,
        n: usize,
        take: u32,
        now: Instant,
    ) -> Result<Commit, (SessionError, usize)> {
        let edit = NotesEdit {
            generation: self.generation,
            base_revision: self.current().revision,
            add: self.pending[..n].iter().map(|(_, note)| *note).collect(),
            update: Vec::new(),
            delete: Vec::new(),
        };
        match self.apply_notes(&edit, Some(take), now) {
            Ok(applied) => {
                self.pending.drain(..n);
                Ok(Commit::Done(
                    (!applied.added_ids.is_empty()).then(|| self.current().revision),
                ))
            }
            Err(SessionError::Engine(_)) => Ok(Commit::Busy),
            Err(e) => {
                let lost = self.pending.len();
                self.pending.clear();
                Err((e, lost))
            }
        }
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
        lock(&self.controller).set_channel_sample(ChannelId(channel), Some(Arc::clone(&sample)))?;
        self.project.channels[index].sample = Some(SampleRef {
            path: path.to_path_buf(),
            file: None,
        });
        self.files.insert(channel, file);
        self.decoded.insert(channel, sample);
        self.changed(true, now);
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
        let state = engine_state(&self.project, &self.decoded);
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
        let state = engine_state(&project, &decoded);
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
        self.project = project;
        self.files = files;
        self.decoded = decoded;
        self.revision += 1;
        self.saved_revision = self.revision;
        self.generation += 1;
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
        loaded: Loaded,
        decoded: DecodedSamples,
    ) -> Result<OpenReport, SessionError> {
        let Loaded {
            mut project,
            samples,
            mut warnings,
        } = loaded;
        warnings.extend(project.validate());
        warnings.extend(decoded.warnings);
        self.replace(project, samples, decoded.samples)?;
        self.path = Some(path.to_path_buf());
        Ok(OpenReport {
            view: self.view(),
            warnings,
        })
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
        let mut report = self.open(original.unwrap_or(Path::new("")), loaded, decoded)?;
        self.path = original.map(Path::to_path_buf);
        self.changed(true, now);
        report.view = self.view();
        Ok(report)
    }

    /// Snapshots what to save, cheaply (the sample bytes are shared). `path` is a new place
    /// to save to (`.grooviq` is added if it has another extension); `None` saves where the
    /// project lives, or is `NoPath` if it is untitled. One save at a time: while a job is out,
    /// this is `SaveRunning`. End every job with [`Self::finish_save`] or [`Self::save_failed`].
    pub fn begin_save(&mut self, path: Option<&Path>) -> Result<SaveJob, SessionError> {
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
            self.saved_revision = job.revision;
            self.pending_since = None;
            self.structural_since = None;
        }
        true
    }

    /// Records that the job from [`Self::begin_save`] was not written: the next save may start.
    pub fn save_failed(&mut self) {
        self.saving = false;
    }

    /// Saves in one go (begin, write, finish). Holds the session the whole time: the app splits
    /// the steps instead.
    pub fn save(&mut self, path: Option<&Path>, backups: &Path) -> Result<PathBuf, SessionError> {
        let job = self.begin_save(path)?;
        if let Err(e) = job.write(backups) {
            self.save_failed();
            return Err(e);
        }
        self.finish_save(&job);
        Ok(job.path)
    }

    /// The engine's state for an export: the project as it plays now.
    pub fn export_state(&self) -> EngineState {
        engine_state(&self.project, &self.decoded)
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
