//! The project as data: what a `.grooviq` file's `project.json` holds.
//!
//! Field names are camelCase in the file, unknown fields are ignored (so an older Grooviq opens
//! a file a newer one added a field to), and every value has one range, enforced by
//! [`Project::validate`].

use grooviq_dsp::sampler::{PARAM_IDS, SPEC};
use grooviq_engine::{MAX_CHANNELS, STEP_TICKS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// `project.json`'s `format` value.
pub const FORMAT: &str = "grooviq-project";
/// The file format version this build writes and reads.
pub const VERSION: u32 = 1;
/// A project file's extension, without the dot.
pub const EXTENSION: &str = "grooviq";
/// Velocity of a hit added with a click.
pub const DEFAULT_VELOCITY: f32 = 0.8;
/// Key of a hit added with a click (middle C, also the sampler's root).
pub const DEFAULT_KEY: u8 = 60;
/// The most steps a pattern may have.
pub const MAX_STEPS: u32 = 256;

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
    #[serde(default = "default_name")]
    pub name: String,
    /// 20..=400.
    #[serde(default = "default_bpm")]
    pub bpm: f32,
    #[serde(default)]
    pub time_signature: TimeSignature,
    /// In mixing order.
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub patterns: Vec<Pattern>,
    /// The `id` of the pattern the engine plays.
    #[serde(default)]
    pub current_pattern: u32,
    /// The next unused id for a channel or pattern (one counter for both). Ids are never
    /// reused, so a note's channel can never point at a different channel after a delete.
    #[serde(default)]
    pub next_id: u32,
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

/// A sampler channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: u32,
    #[serde(default)]
    pub name: String,
    /// An index into the UI's channel palette.
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub sample: Option<SampleRef>,
    /// Sampler parameters by `grooviq_dsp::sampler::PARAM_IDS` name; absent ones are defaults.
    #[serde(default)]
    pub sampler: BTreeMap<String, f32>,
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
    #[serde(default)]
    pub notes: Vec<Note>,
}

/// A hit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
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
        }
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
            name: default_name(),
            bpm: BPM.2,
            time_signature: TimeSignature::default(),
            channels: Vec::new(),
            patterns: vec![Pattern::new(0, "Pattern 1")],
            current_pattern: 0,
            next_id: 1,
        }
    }

    /// Clamps every value into its range, drops what cannot be played (notes of unknown
    /// channels, notes past the longest pattern, a second channel with a used id) and repairs
    /// `current_pattern` and `next_id`. Returns one line per fix, for the person to read.
    pub fn validate(&mut self) -> Vec<String> {
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

        self.validate_channels(&mut fixes);

        // The id counter must be above every id in use.
        let floor = self
            .channels
            .iter()
            .map(|c| c.id)
            .chain(self.patterns.iter().map(|p| p.id))
            .max()
            .map_or(0, |h| h.saturating_add(1));
        if self.next_id < floor {
            fixes.push(format!(
                "Next id {} was not above every used id; changed to {floor}.",
                self.next_id
            ));
            self.next_id = floor;
        }

        self.validate_patterns(&mut fixes);
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
                    c.id, c.name
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
            let name = c.name.clone();
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
            for (param, &(min, max, default)) in PARAM_IDS.iter().zip(SPEC.iter()) {
                if let Some(v) = c.sampler.get_mut(*param) {
                    *v = clamp_fix(
                        *v,
                        (min, max),
                        default,
                        &format!("Channel \"{name}\": sampler value \"{param}\""),
                        fixes,
                    );
                }
            }
        }
    }

    fn validate_patterns(&mut self, fixes: &mut Vec<String>) {
        let channels: HashSet<u32> = self.channels.iter().map(|c| c.id).collect();
        for p in &mut self.patterns {
            let name = p.name.clone();
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

            let before = p.notes.len();
            p.notes.retain(|n| channels.contains(&n.channel));
            let orphans = before - p.notes.len();
            if orphans > 0 {
                fixes.push(format!(
                    "Pattern \"{name}\": dropped {}.",
                    count(
                        orphans,
                        "note of a channel that does not exist",
                        "notes of channels that do not exist"
                    )
                ));
            }
            let before = p.notes.len();
            p.notes.retain(|n| n.tick < MAX_TICKS);
            let far = before - p.notes.len();
            if far > 0 {
                fixes.push(format!(
                    "Pattern \"{name}\": dropped {} past step {MAX_STEPS}.",
                    count(far, "note", "notes")
                ));
            }

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
        }

        // Pattern ids are unique too: a repeat gets a fresh one.
        let mut seen = HashSet::new();
        for p in &mut self.patterns {
            if !seen.insert(p.id) {
                let id = self.next_id;
                fixes.push(format!(
                    "Pattern id {} is used twice; \"{}\" got id {id}.",
                    p.id, p.name
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
                self.current_pattern, first.name
            ));
            self.current_pattern = first.id;
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

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
