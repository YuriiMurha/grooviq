//! The project as data: what a `.grooviq` file's `project.json` holds.
//!
//! Field names are camelCase in the file, unknown fields are ignored (so an older Grooviq opens
//! a file a newer one added a field to), and every value has one range, enforced by
//! [`Project::validate`].

use crate::reading::NoteDrops;
use grooviq_dsp::{sampler, synth};
use grooviq_engine::{InstrumentKind, MAX_CHANNELS, MAX_CHOKE_GROUP, STEP_TICKS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

/// `project.json`'s `format` value.
pub const FORMAT: &str = "grooviq-project";
/// The file format version this build writes and reads.
pub const VERSION: u32 = 4;
/// A project file's extension, without the dot.
pub const EXTENSION: &str = "grooviq";
/// Velocity of a hit added with a click.
pub const DEFAULT_VELOCITY: f32 = 0.8;
/// Key of a hit added with a click (middle C, also the sampler's root).
pub const DEFAULT_KEY: u8 = 60;
/// The most steps a pattern may have.
pub const MAX_STEPS: u32 = 256;
/// The most notes a pattern may hold; a file with more keeps the first ones.
pub const MAX_NOTES_PER_PATTERN: usize = 100_000;
/// Scales a project key may name: the id, then the semitones from the root.
pub const SCALES: [(&str, &[u8]); 12] = [
    ("major", &[0, 2, 4, 5, 7, 9, 11]),
    ("minor", &[0, 2, 3, 5, 7, 8, 10]),
    ("harmonic_minor", &[0, 2, 3, 5, 7, 8, 11]),
    ("melodic_minor", &[0, 2, 3, 5, 7, 9, 11]),
    ("dorian", &[0, 2, 3, 5, 7, 9, 10]),
    ("phrygian", &[0, 1, 3, 5, 7, 8, 10]),
    ("lydian", &[0, 2, 4, 6, 7, 9, 11]),
    ("mixolydian", &[0, 2, 4, 5, 7, 9, 10]),
    ("locrian", &[0, 1, 3, 5, 6, 8, 10]),
    ("major_pentatonic", &[0, 2, 4, 7, 9]),
    ("minor_pentatonic", &[0, 3, 5, 7, 10]),
    ("blues", &[0, 3, 5, 6, 7, 10]),
];

/// Tempo range and default, in beats per minute.
pub(crate) const BPM: (f32, f32, f32) = (20.0, 400.0, 140.0);
/// Channel volume range (linear gain, 1 = 0 dB).
pub(crate) const VOLUME: (f32, f32) = (0.0, 2.0);
/// Steps in a new pattern.
pub(crate) const DEFAULT_STEPS: u32 = 16;
/// Ticks after which no note can start, whatever the pattern's length.
pub(crate) const MAX_TICKS: u32 = MAX_STEPS * STEP_TICKS;

fn one() -> f32 {
    1.0
}
fn first_revision() -> u32 {
    1
}
fn default_steps() -> u32 {
    DEFAULT_STEPS
}
fn default_key() -> u8 {
    DEFAULT_KEY
}
fn default_velocity() -> f32 {
    DEFAULT_VELOCITY
}
fn default_length() -> u32 {
    STEP_TICKS
}
fn default_bpm() -> f32 {
    BPM.2
}
fn default_name() -> String {
    "Untitled".into()
}

/// A whole project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(default)]
    pub play_mode: crate::arrangement::PlayMode,
    #[serde(default, deserialize_with = "crate::reading::named")]
    pub arrangement: crate::arrangement::Arrangement,
    #[serde(default = "default_name")]
    pub name: String,
    /// 20..=400.
    #[serde(default = "default_bpm")]
    pub bpm: f32,
    #[serde(default, deserialize_with = "crate::reading::named")]
    pub time_signature: TimeSignature,
    /// In mixing order.
    #[serde(default, deserialize_with = "crate::reading::named_vec")]
    pub channels: Vec<Channel>,
    #[serde(default, deserialize_with = "crate::reading::named_vec")]
    pub patterns: Vec<Pattern>,
    /// The `id` of the pattern the engine plays.
    #[serde(default)]
    pub current_pattern: u32,
    /// The next unused channel, pattern, arrangement entity or automation priority id. Ids are never
    /// reused, so a note's channel can never point at a different channel after a delete.
    #[serde(default)]
    pub next_id: u32,
    /// The key the piano roll highlights and snaps to; `None` for no key.
    #[serde(default, deserialize_with = "crate::reading::named_option")]
    pub key: Option<ProjectKey>,
    /// Required from version 4; an older file's `mixer`, whatever it holds, is not read
    /// (see [`crate::mixer`]).
    #[serde(default, deserialize_with = "crate::reading::mixer")]
    pub mixer: crate::mixer::Mixer,
}

/// A project's key: a root note and a scale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectKey {
    /// 0..=11, 0 = C.
    pub root: u8,
    /// One of the [`SCALES`] ids.
    pub scale: String,
}

/// A time signature as it is written: `beats` / `unit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeSignature {
    /// 1..=16.
    pub beats: u8,
    /// The note value of a beat: 2, 4, 8 or 16.
    pub unit: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self { beats: 4, unit: 4 }
    }
}

/// A channel: a sampler or a synth, and its mix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: u32,
    #[serde(default)]
    pub name: String,
    /// An index into the UI's channel palette.
    #[serde(default)]
    pub color: u8,
    /// `"sampler"` or `"synth"`.
    #[serde(default = "sampler_kind", with = "instrument_kind")]
    pub instrument: InstrumentKind,
    /// A sampler's sample.
    #[serde(default, deserialize_with = "crate::reading::named_option")]
    pub sample: Option<SampleRef>,
    /// Sampler parameters by `grooviq_dsp::sampler::PARAM_IDS` name; absent ones are defaults.
    #[serde(default)]
    pub sampler: BTreeMap<String, f32>,
    /// Synth parameters by `grooviq_dsp::synth::PARAM_IDS` name; absent ones are defaults.
    #[serde(default)]
    pub synth: BTreeMap<String, f32>,
    /// The factory preset the synth values started from, for display.
    #[serde(default)]
    pub preset: Option<String>,
    /// Linear gain, 0..=2.
    #[serde(default = "one")]
    pub volume: f32,
    /// -1 (left) ..= 1 (right).
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Choke group, 0..=16 (0: none).
    #[serde(default)]
    pub choke: u8,
}

fn sampler_kind() -> InstrumentKind {
    InstrumentKind::Sampler
}

/// [`InstrumentKind`] as `"sampler"` or `"synth"` (the engine type has no serde).
mod instrument_kind {
    use grooviq_engine::InstrumentKind;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub(super) fn serialize<S: Serializer>(kind: &InstrumentKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match kind {
            InstrumentKind::Sampler => "sampler",
            InstrumentKind::Synth => "synth",
        })
    }

    /// In a version 1 file the value is skipped unread: migration makes every channel a sampler.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<InstrumentKind, D::Error> {
        if crate::reading::supplied_by_migration() {
            serde::de::IgnoredAny::deserialize(d)?;
            return Ok(InstrumentKind::Sampler);
        }
        match String::deserialize(d)?.as_str() {
            "sampler" => Ok(InstrumentKind::Sampler),
            "synth" => Ok(InstrumentKind::Synth),
            other => Err(D::Error::unknown_variant(other, &["sampler", "synth"])),
        }
    }
}

/// Where a channel's sample came from, and where the file keeps a copy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleRef {
    /// The original file, for display and relinking.
    pub path: PathBuf,
    /// The sample's entry in the project file (`samples/<n>-<file name>`); `None` while the
    /// channel's sample is missing.
    #[serde(default)]
    pub file: Option<String>,
}

/// A loop of notes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pattern {
    pub id: u32,
    #[serde(default)]
    pub name: String,
    /// Length in 1/16 steps, 1..=256.
    #[serde(default = "default_steps")]
    pub steps: u32,
    /// 0..=1: odd steps start later by `round(swing * 120)` ticks.
    #[serde(default)]
    pub swing: f32,
    /// At most [`MAX_NOTES_PER_PATTERN`] are read (see [`crate::reading`]).
    #[serde(default, deserialize_with = "crate::reading::notes")]
    pub notes: Vec<Note>,
    /// Above every note id in the pattern: the id the next new note gets.
    #[serde(default, deserialize_with = "crate::reading::id")]
    pub next_note_id: u32,
    /// Counts the pattern's edits in this session, so a stale note edit can be told apart.
    /// Not saved: 1 after a load.
    #[serde(skip, default = "first_revision")]
    pub revision: u32,
}

/// A note.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    /// Unique within its pattern, never 0.
    #[serde(default, deserialize_with = "crate::reading::id")]
    pub id: u32,
    /// The channel's `id`.
    pub channel: u32,
    /// Start, in ticks (960 per quarter note, 240 per step).
    pub tick: u32,
    /// Ticks the note is held, at least 1.
    #[serde(default = "default_length")]
    pub length: u32,
    /// 0..=127.
    #[serde(default = "default_key")]
    pub key: u8,
    /// 0..=1.
    #[serde(default = "default_velocity")]
    pub velocity: f32,
    /// An 808-style slide: the channel's held note glides to this key instead of a new note
    /// starting.
    #[serde(default)]
    pub slide: bool,
}

/// Whether the step grid may edit `note`: it starts on a step, lasts one step, is on the
/// default key and does not slide.
pub fn is_plain_step(note: &Note) -> bool {
    note.tick.is_multiple_of(STEP_TICKS)
        && note.length == STEP_TICKS
        && note.key == DEFAULT_KEY
        && !note.slide
}

/// The channels of `pattern` that hold a note that is not a plain step (see [`is_plain_step`]):
/// they belong to the piano roll, and the step grid only previews them.
pub fn roll_channels(pattern: &Pattern) -> BTreeSet<u32> {
    pattern
        .notes
        .iter()
        .filter(|n| !is_plain_step(n))
        .map(|n| n.channel)
        .collect()
}

impl Pattern {
    /// An empty pattern of the default length.
    pub fn new(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            steps: DEFAULT_STEPS,
            swing: 0.0,
            notes: Vec::new(),
            next_note_id: 1,
            revision: first_revision(),
        }
    }

    /// A fresh note id, above every id in the pattern, or `None` once the counter has reached
    /// `u32::MAX` (the used-up mark: that id is never handed out). Ids are never reused within
    /// a session; [`Project::validate`] renumbers a used-up pattern on the next load.
    pub(crate) fn take_note_id(&mut self) -> Option<u32> {
        let id = self.next_note_id.max(1);
        if id == u32::MAX {
            return None;
        }
        self.next_note_id = id + 1;
        Some(id)
    }
}

impl Default for Project {
    fn default() -> Self {
        Self::new()
    }
}

impl Project {
    /// "Untitled", 140 BPM, 4/4, no channels, one empty 16-step pattern "Pattern 1".
    pub fn new() -> Self {
        Self {
            play_mode: Default::default(),
            arrangement: Default::default(),
            name: default_name(),
            bpm: BPM.2,
            time_signature: TimeSignature::default(),
            channels: Vec::new(),
            patterns: vec![Pattern::new(0, "Pattern 1")],
            current_pattern: 0,
            next_id: 1,
            key: None,
            mixer: Default::default(),
        }
    }

    /// Clamps every value into its range, drops what cannot be played (notes of unknown
    /// channels, notes past the longest pattern, a second channel with a used id) and repairs
    /// `current_pattern` and `next_id`. Returns one line per fix, for the person to read.
    pub fn validate(&mut self) -> Vec<String> {
        self.validate_read(&[])
    }

    /// [`Project::validate`] after a read that already dropped `read[i]` notes from pattern `i`
    /// (see [`crate::reading`]): they are reported with the notes validation drops itself.
    pub(crate) fn validate_read(&mut self, read: &[NoteDrops]) -> Vec<String> {
        let mut fixes = Vec::new();

        let bpm = self.bpm;
        if !(BPM.0..=BPM.1).contains(&bpm) {
            let fixed = if bpm.is_finite() {
                bpm.clamp(BPM.0, BPM.1)
            } else {
                BPM.2
            };
            fixes.push(format!(
                "Tempo {bpm} is outside {} to {} BPM; changed to {fixed}.",
                BPM.0, BPM.1
            ));
            self.bpm = fixed;
        }

        let sig = self.time_signature;
        let fixed = TimeSignature {
            beats: sig.beats.clamp(1, 16),
            unit: if matches!(sig.unit, 2 | 4 | 8 | 16) {
                sig.unit
            } else {
                4
            },
        };
        if fixed != sig {
            fixes.push(format!(
                "Time signature {}/{} is not valid; changed to {}/{}.",
                sig.beats, sig.unit, fixed.beats, fixed.unit
            ));
            self.time_signature = fixed;
        }

        if let Some(key) = &self.key
            && (key.root > 11 || !SCALES.iter().any(|(id, _)| *id == key.scale))
        {
            fixes.push(format!(
                "The project's key (root {}, scale \"{}\") is not valid; it was cleared.",
                key.root,
                warning_label(&key.scale)
            ));
            self.key = None;
        }

        self.validate_channels(&mut fixes);

        // The id counter must be above every id in use.
        let floor = self
            .channels
            .iter()
            .map(|c| c.id)
            .chain(self.patterns.iter().map(|p| p.id))
            .chain(self.arrangement.ids())
            .max()
            .map_or(0, |h| h.saturating_add(1));
        if self.next_id < floor {
            fixes.push(format!(
                "Next id {} was not above every used id; changed to {floor}.",
                self.next_id
            ));
            self.next_id = floor;
        }

        self.validate_patterns(read, &mut fixes);
        fixes
    }

    fn validate_channels(&mut self, fixes: &mut Vec<String>) {
        // Ids are unique: a second channel with a used id is dropped.
        let mut seen = HashSet::new();
        self.channels.retain(|c| {
            let fresh = seen.insert(c.id);
            if !fresh {
                fixes.push(format!(
                    "Channel id {} is used twice; the second channel \"{}\" was dropped.",
                    c.id,
                    warning_label(&c.name)
                ));
            }
            fresh
        });
        if self.channels.len() > MAX_CHANNELS {
            self.channels.truncate(MAX_CHANNELS);
            fixes.push(format!(
                "More than {MAX_CHANNELS} channels; the extra ones were dropped."
            ));
        }

        for c in &mut self.channels {
            let name = warning_label(&c.name);
            c.volume = clamp_fix(
                c.volume,
                VOLUME,
                1.0,
                &format!("Channel \"{name}\": volume"),
                fixes,
            );
            c.pan = clamp_fix(
                c.pan,
                (-1.0, 1.0),
                0.0,
                &format!("Channel \"{name}\": pan"),
                fixes,
            );
            if c.choke > MAX_CHOKE_GROUP {
                fixes.push(format!(
                    "Channel \"{name}\": choke group {} is outside 0 to {MAX_CHOKE_GROUP}; changed to {MAX_CHOKE_GROUP}.",
                    c.choke
                ));
                c.choke = MAX_CHOKE_GROUP;
            }
            clamp_params(
                &mut c.sampler,
                &sampler::PARAM_IDS,
                &sampler::SPEC,
                &name,
                "sampler",
                fixes,
            );
            // Labels are display metadata; the stored parameters remain the sound source.
            if c.instrument == InstrumentKind::Synth
                && c.preset
                    .as_deref()
                    .is_some_and(|label| label != "Init" && !synth::PRESETS.contains(&label))
            {
                c.preset = None;
                fixes.push(format!(
                    "Channel id {}: unknown synth preset label was cleared; sound values were kept.",
                    c.id
                ));
            }
            // Unknown synth names are dropped; unknown sampler names stay ignored (M1 kept them).
            c.synth
                .retain(|param, _| synth::PARAM_IDS.contains(&param.as_str()));
            clamp_params(
                &mut c.synth,
                &synth::PARAM_IDS,
                &synth::SPEC,
                &name,
                "synth",
                fixes,
            );
        }
    }

    fn validate_patterns(&mut self, read: &[NoteDrops], fixes: &mut Vec<String>) {
        let channels: HashSet<u32> = self.channels.iter().map(|c| c.id).collect();
        for (i, p) in self.patterns.iter_mut().enumerate() {
            let name = warning_label(&p.name);
            if !(1..=MAX_STEPS).contains(&p.steps) {
                let fixed = p.steps.clamp(1, MAX_STEPS);
                fixes.push(format!(
                    "Pattern \"{name}\": {} steps is outside 1 to {MAX_STEPS}; changed to {fixed}.",
                    p.steps
                ));
                p.steps = fixed;
            }
            p.swing = clamp_fix(
                p.swing,
                (0.0, 1.0),
                0.0,
                &format!("Pattern \"{name}\": swing"),
                fixes,
            );

            let mut drops = read.get(i).copied().unwrap_or_default();
            let before = p.notes.len();
            p.notes.retain(|n| channels.contains(&n.channel));
            drops.orphans += before - p.notes.len();
            let before = p.notes.len();
            p.notes.retain(|n| n.tick < MAX_TICKS);
            drops.far += before - p.notes.len();
            drops.over_cap += p.notes.len().saturating_sub(MAX_NOTES_PER_PATTERN);
            p.notes.truncate(MAX_NOTES_PER_PATTERN);
            fixes.extend(dropped_notes(&name, drops));

            let mut clamped = 0;
            for n in &mut p.notes {
                let fixed = Note {
                    key: n.key.min(127),
                    velocity: if n.velocity.is_finite() {
                        n.velocity.clamp(0.0, 1.0)
                    } else {
                        DEFAULT_VELOCITY
                    },
                    length: n.length.clamp(1, MAX_TICKS),
                    ..*n
                };
                if fixed != *n {
                    clamped += 1;
                    *n = fixed;
                }
            }
            if clamped > 0 {
                fixes.push(format!(
                    "Pattern \"{name}\": {} had values out of range and {} clamped.",
                    count(clamped, "note", "notes"),
                    if clamped == 1 { "was" } else { "were" }
                ));
            }

            if let Some(fix) = repair_note_ids(p) {
                fixes.push(fix);
            }
        }

        // Pattern ids are unique too: a repeat gets a fresh one.
        let mut seen = HashSet::new();
        for p in &mut self.patterns {
            if !seen.insert(p.id) {
                let id = self.next_id;
                fixes.push(format!(
                    "Pattern id {} is used twice; \"{}\" got id {id}.",
                    p.id,
                    warning_label(&p.name)
                ));
                p.id = id;
                seen.insert(id);
                self.next_id = id.saturating_add(1);
            }
        }

        if self.patterns.is_empty() {
            let id = self.next_id;
            self.next_id = id.saturating_add(1);
            self.patterns.push(Pattern::new(id, "Pattern 1"));
            self.current_pattern = id;
            fixes.push("The project had no pattern; added an empty \"Pattern 1\".".into());
        } else if !self.patterns.iter().any(|p| p.id == self.current_pattern) {
            let first = &self.patterns[0];
            fixes.push(format!(
                "Current pattern {} does not exist; switched to \"{}\".",
                self.current_pattern,
                warning_label(&first.name)
            ));
            self.current_pattern = first.id;
        }
    }
}

// Display only: cap work before copying untrusted names. 48 scalars cost at most
// 192 UTF-8 bytes plus the ellipsis; controls and bidi overrides cannot spoof lines.
pub(crate) fn warning_label(name: &str) -> String {
    let mut chars = name.chars();
    let mut label = String::with_capacity(195);
    for c in chars.by_ref().take(48) {
        label.push(if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            '\u{fffd}'
        } else {
            c
        });
    }
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

/// Gives every note of `p` an id that is not 0 and not used before it in the pattern, and moves
/// `next_note_id` above every id (silently: it is bookkeeping). The first note with an id keeps
/// it; a repaired one gets an id above the largest. If that leaves no id for a new note (ids
/// near `u32::MAX`, or a used-up counter: see [`Pattern::take_note_id`]), every note is
/// renumbered from 1 in stored order. Returns the warning, if any note changed.
fn repair_note_ids(p: &mut Pattern) -> Option<String> {
    let mut seen = HashSet::with_capacity(p.notes.len());
    let bad: Vec<usize> = (0..p.notes.len())
        .filter(|&i| {
            let id = p.notes[i].id;
            id == 0 || !seen.insert(id)
        })
        .collect();
    let largest = p.notes.iter().map(|n| n.id).max().unwrap_or(0);
    // Room for every repaired id and, above them, at least one id for a new note (the counter
    // stays below the used-up mark).
    let used_up = p.next_note_id == u32::MAX;
    let fix = if p.notes.is_empty() {
        if used_up {
            p.next_note_id = 1;
        }
        None
    } else if used_up || u64::from(largest) + bad.len() as u64 + 1 >= u64::from(u32::MAX) {
        for (n, id) in p.notes.iter_mut().zip(1..) {
            n.id = id;
        }
        p.next_note_id = 0;
        Some(format!(
            "Pattern \"{}\": its note ids left no room for new notes; {} renumbered.",
            warning_label(&p.name),
            count(p.notes.len(), "note was", "notes were")
        ))
    } else if bad.is_empty() {
        None
    } else {
        for (i, id) in bad.iter().zip(largest + 1..) {
            p.notes[*i].id = id;
        }
        Some(format!(
            "Pattern \"{}\": {} a missing or repeated id and got new ones.",
            warning_label(&p.name),
            count(bad.len(), "note had", "notes had")
        ))
    };
    let floor = p.notes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
    p.next_note_id = p.next_note_id.max(floor);
    fix
}

/// Clamps each known parameter of `values` into its `spec` range, noting the fixes as
/// `Channel "<channel>": <device> value "<name>" ...`.
fn clamp_params(
    values: &mut BTreeMap<String, f32>,
    ids: &[&str],
    spec: &[(f32, f32, f32)],
    channel: &str,
    device: &str,
    fixes: &mut Vec<String>,
) {
    for (param, &(min, max, default)) in ids.iter().zip(spec) {
        if let Some(v) = values.get_mut(*param) {
            *v = clamp_fix(
                *v,
                (min, max),
                default,
                &format!("Channel \"{channel}\": {device} value \"{param}\""),
                fixes,
            );
        }
    }
}

/// Clamps `value` into `range` (a non-finite value becomes `default`), noting the fix.
fn clamp_fix(
    value: f32,
    range: (f32, f32),
    default: f32,
    what: &str,
    fixes: &mut Vec<String>,
) -> f32 {
    if (range.0..=range.1).contains(&value) {
        return value;
    }
    let fixed = if value.is_finite() {
        value.clamp(range.0, range.1)
    } else {
        default
    };
    fixes.push(format!(
        "{what} {value} is outside {} to {}; changed to {fixed}.",
        range.0, range.1
    ));
    fixed
}

/// One line per kind of note a pattern lost.
fn dropped_notes(pattern: &str, drops: NoteDrops) -> Vec<String> {
    let mut lines = Vec::new();
    if drops.orphans > 0 {
        lines.push(format!(
            "Pattern \"{pattern}\": dropped {}.",
            count(
                drops.orphans,
                "note of a channel that does not exist",
                "notes of channels that do not exist"
            )
        ));
    }
    if drops.far > 0 {
        lines.push(format!(
            "Pattern \"{pattern}\": dropped {} past step {MAX_STEPS}.",
            count(drops.far, "note", "notes")
        ));
    }
    if drops.over_cap > 0 {
        lines.push(format!(
            "Pattern \"{pattern}\": dropped {} past the limit of {MAX_NOTES_PER_PATTERN} notes per pattern.",
            count(drops.over_cap, "note", "notes")
        ));
    }
    lines
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_ids_run_up_to_the_used_up_mark_and_never_repeat() {
        let mut p = Pattern::new(0, "P");
        p.next_note_id = 0;
        assert_eq!(p.take_note_id(), Some(1), "0 is never an id");
        p.next_note_id = u32::MAX - 1;
        assert_eq!(p.take_note_id(), Some(u32::MAX - 1));
        assert_eq!(p.next_note_id, u32::MAX);
        assert_eq!(p.take_note_id(), None);
        assert_eq!(p.take_note_id(), None, "still used up");
        assert_eq!(p.next_note_id, u32::MAX);
    }
}
