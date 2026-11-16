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
use crate::compile::{compile_pattern, engine_state, sampler_values};
use crate::file::{Loaded, SampleFiles, save};
use crate::model::{
    BPM, Channel, DEFAULT_KEY, DEFAULT_VELOCITY, EXTENSION, MAX_STEPS, Note, Pattern, Project,
    SampleRef, TimeSignature, VOLUME,
};
use grooviq_dsp::sampler::{PARAM_IDS, SPEC, SampleBuf};
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, Command, ControlError, Controller, EngineState,
    MAX_CHANNELS, MAX_SAMPLE_SECONDS, STEP_TICKS, decode_sample,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    pub volume: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// Every sampler parameter by id, with its value or default.
    pub sampler: BTreeMap<String, f32>,
}

/// The current pattern as a grid.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternView {
    pub id: u32,
    pub name: String,
    pub steps: u32,
    pub swing: f32,
    /// Per channel id: one entry per step, the velocity of the hit that starts exactly on it or
    /// `null`.
    pub cells: BTreeMap<u32, Vec<Option<f32>>>,
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
pub fn decode_samples(loaded: &Loaded) -> DecodedSamples {
    let mut out = DecodedSamples::default();
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
        match decode_sample(&file.bytes, extension, MAX_SAMPLE_SECONDS) {
            Ok(buf) => {
                out.samples.insert(channel.id, Arc::new(buf));
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
    UnknownParam(String),
    /// A value that is not a number, or a time signature that does not exist.
    InvalidValue(String),
    TooManyChannels,
    /// The project has no file yet: ask where to save it.
    NoPath,
    /// The engine refused the change.
    Engine(ControlError),
    /// The previous file could not be backed up, so it was not overwritten.
    Backup(io::Error),
    /// `project::save` refused or failed; its message as it is.
    Save(io::Error),
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
            Self::UnknownParam(p) => write!(f, "The sampler has no setting called \"{p}\"."),
            Self::InvalidValue(what) => write!(f, "{what}"),
            Self::TooManyChannels => write!(
                f,
                "A project holds at most {MAX_CHANNELS} channels; remove one to add another."
            ),
            Self::NoPath => write!(f, "Choose where to save this project."),
            Self::Engine(e) => write!(f, "The audio engine did not take the change ({e})."),
            Self::Backup(e) => write!(
                f,
                "The previous version could not be backed up, so it was not overwritten: {e}."
            ),
            Self::Save(e) => write!(f, "{e}"),
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
}

impl SaveJob {
    /// Backs up the file it replaces (keeping [`MAX_BACKUPS`] in `backups`), then saves. Run it
    /// without the session's lock.
    pub fn write(&self, backups: &Path) -> Result<(), SessionError> {
        backup_before_save(&self.path, backups, MAX_BACKUPS).map_err(SessionError::Backup)?;
        save(&self.project, &self.samples, &self.path).map_err(SessionError::Save)
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
    /// The first change since the last autosave (or save).
    pending_since: Option<Instant>,
    /// The first structural change since the last autosave (or save).
    structural_since: Option<Instant>,
    autosaved_at: Option<u64>,
}

fn lock(controller: &Mutex<Controller>) -> MutexGuard<'_, Controller> {
    controller.lock().unwrap_or_else(PoisonError::into_inner)
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
            pending_since: None,
            structural_since: None,
            autosaved_at: None,
        };
        // A fresh queue always has room for a whole state.
        let synced = session.sync_engine();
        debug_assert!(synced.is_ok(), "{synced:?}");
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
            if note.tick % STEP_TICKS == 0
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
                .map(|c| self.channel_view(c))
                .collect(),
            pattern: PatternView {
                id: pattern.id,
                name: pattern.name.clone(),
                steps: pattern.steps,
                swing: pattern.swing,
                cells,
            },
            autosaved_at: self.autosaved_at,
        }
    }

    fn channel_view(&self, c: &Channel) -> ChannelView {
        let sample_name = self.files.get(&c.id).map(|f| f.name.clone()).or_else(|| {
            c.sample
                .as_ref()
                .and_then(|s| s.path.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        });
        let values = sampler_values(c);
        ChannelView {
            id: c.id,
            name: c.name.clone(),
            color: c.color,
            sample_name,
            missing: c.sample.is_some() && !self.decoded.contains_key(&c.id),
            volume: c.volume,
            pan: c.pan,
            mute: c.mute,
            solo: c.solo,
            sampler: PARAM_IDS
                .iter()
                .zip(values)
                .map(|(id, v)| ((*id).to_owned(), v))
                .collect(),
        }
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
                    return Err(SessionError::UnknownParam(param));
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
                        p.notes.push(hit(channel, tick, DEFAULT_VELOCITY));
                    }
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
                        p.notes.push(hit(channel, tick, velocity));
                    }
                })
            }
            Edit::ClearStep { channel, step } => {
                let tick = self.step_tick(channel, step)?;
                self.edit_pattern(|p| {
                    p.notes
                        .retain(|n| !(n.channel == channel && n.tick == tick));
                })
            }
            Edit::SetPatternLength { steps } => {
                let steps = steps.clamp(1, MAX_STEPS);
                self.edit_pattern(|p| p.steps = steps)
            }
            Edit::SetSwing { swing } => {
                let swing = finite(swing, "swing")?.clamp(0.0, 1.0);
                self.edit_pattern(|p| p.swing = swing)
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
                // The engine plays steps; the signature only draws the bars.
                self.project.time_signature = signature;
                Ok(true)
            }
        }
    }

    /// The tick of `step` on `channel`, checking both exist.
    fn step_tick(&self, channel: u32, step: u32) -> Result<u32, SessionError> {
        self.channel_index(channel)?;
        let steps = self.current().steps;
        if step >= steps {
            return Err(SessionError::StepOutOfRange { step, steps });
        }
        Ok(step * STEP_TICKS)
    }

    /// Changes a copy of the current pattern, sends it to the engine, then keeps it.
    fn edit_pattern(&mut self, change: impl FnOnce(&mut Pattern)) -> Result<bool, SessionError> {
        let mut pattern = self.current().clone();
        change(&mut pattern);
        if pattern == *self.current() {
            return Ok(false);
        }
        let compiled = compile_pattern(&self.project, &pattern);
        lock(&self.controller).set_pattern(compiled)?;
        *self.current_mut() = pattern;
        Ok(true)
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
            p.notes.retain(|n| n.channel != channel);
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
        Ok(())
    }

    /// Adds a channel playing `sample` after the others: named after the file's stem, in the
    /// next ink. The session keeps `file`'s bytes for saving.
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
        let color = self
            .project
            .channels
            .last()
            .map_or(0, |c| (c.color.wrapping_add(1)) % INKS);
        let channel = Channel {
            id,
            name,
            color,
            sample: Some(SampleRef {
                path: path.to_path_buf(),
                file: None,
            }),
            sampler: BTreeMap::new(),
            volume: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
        };
        lock(&self.controller).add_channel(ChannelSetup {
            id: ChannelId(id),
            sample: Some(Arc::clone(&sample)),
            sampler: sampler_values(&channel),
            mix: ChannelMix::default(),
        })?;
        self.project.channels.push(channel);
        self.project.next_id = id.saturating_add(1);
        self.files.insert(id, file);
        self.decoded.insert(id, sample);
        self.changed(true, now);
        Ok(self.view())
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

    /// Starts (`on`) or releases a preview of `channel`: key 60 at velocity 0.8. Only on the
    /// person's click or key: never call it on its own.
    pub fn preview(&self, channel: u32, on: bool) -> Result<(), SessionError> {
        self.channel_index(channel)?;
        self.send(Command::ChannelNote {
            channel: ChannelId(channel),
            key: DEFAULT_KEY,
            velocity: DEFAULT_VELOCITY,
            on,
        })
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
        // `load_state` stops the transport, then swaps the graph: opening never plays.
        lock(&self.controller).load_state(state)?;
        self.project = project;
        self.files = files;
        self.decoded = decoded;
        self.revision += 1;
        self.saved_revision = self.revision;
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
    /// project lives, or is `NoPath` if it is untitled.
    pub fn begin_save(&self, path: Option<&Path>) -> Result<SaveJob, SessionError> {
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
        Ok(SaveJob {
            project,
            samples: self.files.clone(),
            path,
            revision: self.revision,
        })
    }

    /// Records a finished save: the project lives at the job's path now, and is clean unless
    /// it changed while the file was written.
    pub fn finish_save(&mut self, job: &SaveJob) {
        self.path = Some(job.path.clone());
        self.project.name = job.project.name.clone();
        if self.revision == job.revision {
            self.saved_revision = job.revision;
            self.pending_since = None;
            self.structural_since = None;
        }
    }

    /// Saves in one go (begin, write, finish). Holds the session the whole time: the app splits
    /// the steps instead.
    pub fn save(&mut self, path: Option<&Path>, backups: &Path) -> Result<PathBuf, SessionError> {
        let job = self.begin_save(path)?;
        job.write(backups)?;
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

    /// Records that the autosave taken last was written at `unix_seconds`.
    pub fn autosaved(&mut self, unix_seconds: u64) {
        self.autosaved_at = Some(unix_seconds);
    }

    /// Records that the autosave taken last could not be written: it is tried again after
    /// [`AUTOSAVE_PERIOD`].
    pub fn autosave_failed(&mut self, now: Instant) {
        if self.dirty() {
            self.pending_since.get_or_insert(now);
        }
    }
}

fn hit(channel: u32, tick: u32, velocity: f32) -> Note {
    Note {
        channel,
        tick,
        length: HIT_TICKS,
        key: DEFAULT_KEY,
        velocity,
    }
}
