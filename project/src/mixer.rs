//! The project's mixer as persisted data (file format version 4): tracks with ordered inserts,
//! fader controls, sends, sidechains and sparse source assignments.
//!
//! * [`Mode::LegacyDirect`] is the identity mixer every project before version 4 had: the
//!   master alone, neutral, nothing assigned. Sources keep the old reduction order
//!   (tone → legacy synth → channel vector order → song audio), which a flat graph summed
//!   into the master does not reproduce, so it is a mode, never an inferred graph.
//! * [`Mode::Routed`] is any other valid graph. [`require_runtime_support`] checks whether
//!   the live engine can render all of its settings.
//! * A source (an instrument channel or an audio playlist track) has one destination; one
//!   without an assignment goes to the master.
//! * Every list and string is bounded while it is read (see [`Limits`]), before more is kept.

use crate::model::Project;
use grooviq_engine::mixer as engine;
use serde::de::{self, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;
use std::mem::size_of;

/// The master track's id. Every mixer has exactly one master, and nothing else uses id 0.
pub const MASTER: u32 = 0;
/// Fader level range in dB: above the lower bound, up to the upper one ([`Level::Silence`] is
/// the off position).
pub const LEVEL_DB: (f32, f32) = (-60.0, 6.0);
/// The largest send gain (linear, about +12 dB).
pub const MAX_SEND_GAIN: f32 = 4.0;
/// The built-in effects an insert may hold (instruments are not inserts).
pub use grooviq_dsp::EFFECTS;

/// How much a mixer may hold. These bound the file model; they say nothing about the memory or
/// latency of prepared devices, which the runtime admits separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub tracks: usize,
    /// Inserts across all tracks; no track has its own ceiling.
    pub inserts: usize,
    pub sends: usize,
    pub sidechains: usize,
    pub assignments: usize,
    /// Tracks, inserts, insert parameters, sends, sidechains and assignments together.
    pub entries: usize,
    /// Bytes kept: each entry's size plus its strings' bytes.
    pub bytes: usize,
    /// Track names and preset labels, in UTF-8 bytes.
    pub name_bytes: usize,
    pub device_bytes: usize,
}

impl Limits {
    pub const DEFAULT: Self = Self {
        tracks: 16_384,
        inserts: 16_384,
        sends: 65_536,
        sidechains: 65_536,
        assignments: 65_536,
        entries: 131_072,
        bytes: 32 << 20,
        name_bytes: 256,
        device_bytes: 32,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether the mixer is the legacy identity or a routed graph (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    LegacyDirect,
    Routed,
}

/// A fader position: a finite level in dB, or off.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Db(f32),
    Silence,
}

impl Default for Level {
    fn default() -> Self {
        Self::Db(0.0)
    }
}

/// Where a send or sidechain taps its track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tap {
    /// After the inserts, before the fader.
    Pre,
    #[default]
    Post,
}

/// A source's identity: an instrument channel's id or an audio playlist track's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKey {
    Channel(u32),
    AudioTrack(u32),
}

/// An effect in a track's chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Insert {
    /// Unique across the mixer, never 0, below `next_insert_id`.
    pub id: u32,
    /// One of [`EFFECTS`].
    #[serde(deserialize_with = "device")]
    pub device: String,
    /// Every parameter in the device's `SPEC` order, each in its range. These are the sound;
    /// `preset` is only a label.
    #[serde(deserialize_with = "params")]
    pub params: Vec<f32>,
    #[serde(default, deserialize_with = "label")]
    pub preset: Option<String>,
    /// Seeds the device's deterministic modulation. Written as a decimal string so no reader
    /// loses precision.
    #[serde(serialize_with = "write_seed", deserialize_with = "seed")]
    pub seed: u64,
    #[serde(default)]
    pub bypass: bool,
}

/// A mixer track. The master has no output and no sends; every other track has one output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Track {
    pub id: u32,
    #[serde(default, deserialize_with = "name")]
    pub name: String,
    #[serde(default)]
    pub output: Option<u32>,
    /// Signal order.
    #[serde(default, deserialize_with = "inserts")]
    pub inserts: Vec<Insert>,
    #[serde(default)]
    pub level: Level,
    /// -1 (left) ..= 1 (right).
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
}

impl Track {
    /// The master track: neutral and empty.
    pub fn master() -> Self {
        Self {
            id: MASTER,
            name: "Master".into(),
            output: None,
            inserts: Vec::new(),
            level: Level::default(),
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Send {
    /// Unique, never 0, below `next_send_id`.
    pub id: u32,
    pub from: u32,
    pub to: u32,
    #[serde(default)]
    pub tap: Tap,
    /// Linear, 0..=[`MAX_SEND_GAIN`].
    pub gain: f32,
}

/// A track feeding an insert's detector (never summed into audio).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sidechain {
    pub from: u32,
    /// The insert's id.
    pub to: u32,
    #[serde(default)]
    pub tap: Tap,
}

/// An explicit destination for one source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Assignment {
    pub source: SourceKey,
    pub destination: u32,
}

/// The whole mixer (see the module docs).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mixer {
    pub mode: Mode,
    pub tracks: Vec<Track>,
    pub sends: Vec<Send>,
    pub sidechains: Vec<Sidechain>,
    /// Sparse: a source not listed goes to the master.
    pub sources: Vec<Assignment>,
    /// Above every track id; `u32::MAX` once used up. Ids are never reused.
    pub next_track_id: u32,
    pub next_insert_id: u32,
    pub next_send_id: u32,
}

impl Default for Mixer {
    /// The legacy identity: the master alone.
    fn default() -> Self {
        Self {
            mode: Mode::LegacyDirect,
            tracks: vec![Track::master()],
            sends: Vec::new(),
            sidechains: Vec::new(),
            sources: Vec::new(),
            next_track_id: 1,
            next_insert_id: 1,
            next_send_id: 1,
        }
    }
}

/// Why a mixer is not valid, or cannot be used yet.
#[derive(Clone, Debug, PartialEq)]
pub enum MixerError {
    /// A count or the kept bytes passed a [`Limits`] bound.
    TooBig(&'static str),
    MissingMaster,
    DuplicateTrack(u32),
    DuplicateInsert(u32),
    DuplicateSend(u32),
    DuplicateSource(SourceKey),
    /// A track's output is missing (or set on the master), or the master has a send.
    InvalidOutput(u32),
    /// An id is 0 where 0 is reserved, or not below its counter.
    InvalidId(&'static str, u32),
    /// The insert's device is not a built-in effect.
    UnknownDevice(u32),
    /// The insert's parameters are not the device's full list, each in range.
    InvalidParams(u32),
    InvalidLevel(u32),
    InvalidPan(u32),
    InvalidGain(u32),
    /// A name or label is too long.
    InvalidName,
    /// The source names a channel or playlist track the project does not have.
    UnknownSource(SourceKey),
    /// The graph does not compile: a dangling reference, a self edge or a cycle.
    Graph(String),
    /// Legacy direct mode with anything but the neutral master alone.
    NotIdentity,
    /// Routed mode for the identity mixer.
    IdentityRouted,
    /// An id counter is used up.
    IdsUsedUp(&'static str),
    /// The runtime cannot apply this track's mute or solo state.
    UnsupportedTrackControls(u32),
    /// The runtime cannot bypass this insert.
    UnsupportedBypass(u32),
    /// The runtime cannot prepare this insert with a supported declaration.
    UnsupportedInsert(u32),
}

impl fmt::Display for MixerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooBig(what) => write!(f, "Mixer: too many {what}."),
            Self::MissingMaster => write!(f, "Mixer: the master track is missing."),
            Self::DuplicateTrack(id) => write!(f, "Mixer: track id {id} is used twice."),
            Self::DuplicateInsert(id) => write!(f, "Mixer: insert id {id} is used twice."),
            Self::DuplicateSend(id) => write!(f, "Mixer: send id {id} is used twice."),
            Self::DuplicateSource(s) => write!(f, "Mixer: {s:?} is assigned twice."),
            Self::InvalidOutput(id) => write!(f, "Mixer: track {id} has an invalid output."),
            Self::InvalidId(what, id) => write!(f, "Mixer: {what} id {id} is not valid."),
            Self::UnknownDevice(id) => write!(f, "Mixer: insert {id} is not a built-in effect."),
            Self::InvalidParams(id) => {
                write!(f, "Mixer: insert {id} has invalid parameter values.")
            }
            Self::InvalidLevel(id) => write!(f, "Mixer: track {id} has an invalid level."),
            Self::InvalidPan(id) => write!(f, "Mixer: track {id} has an invalid pan."),
            Self::InvalidGain(id) => write!(f, "Mixer: send {id} has an invalid gain."),
            Self::InvalidName => write!(f, "Mixer: a name or label is too long."),
            Self::UnknownSource(s) => write!(f, "Mixer: {s:?} does not exist."),
            Self::Graph(why) => write!(f, "Mixer: the routing is not valid ({why})."),
            Self::NotIdentity => write!(
                f,
                "Mixer: legacy direct mode holds routing; only the neutral master is allowed."
            ),
            Self::IdentityRouted => write!(f, "Mixer: routed mode without any routing."),
            Self::IdsUsedUp(what) => write!(f, "Mixer: no {what} ids are left."),
            Self::UnsupportedTrackControls(id) => write!(
                f,
                "Mixer: track {id} uses mute or solo, which the runtime does not support yet."
            ),
            Self::UnsupportedBypass(id) => write!(
                f,
                "Mixer: insert {id} uses bypass, which the runtime does not support yet."
            ),
            Self::UnsupportedInsert(id) => write!(f, "Mixer: insert {id} cannot be prepared."),
        }
    }
}

impl std::error::Error for MixerError {}

/// Refuses a mixer the runtime cannot render as saved, before state or file changes.
pub fn require_runtime_support(project: &Project) -> Result<(), MixerError> {
    crate::mixer_compile::compile_mix(project).map(|_| ())
}

fn neutral(t: &Track) -> bool {
    t.id == MASTER
        && t.output.is_none()
        && t.inserts.is_empty()
        && t.level == Level::Db(0.0)
        && t.pan == 0.0
        && !t.mute
        && !t.solo
}

impl Mixer {
    /// The neutral master alone, with nothing sent, sidechained or assigned.
    pub fn is_identity(&self) -> bool {
        matches!(self.tracks.as_slice(), [master] if neutral(master))
            && self.sends.is_empty()
            && self.sidechains.is_empty()
            && self.sources.is_empty()
    }

    pub(crate) fn refresh_mode(&mut self) {
        self.mode = if self.is_identity() {
            Mode::LegacyDirect
        } else {
            Mode::Routed
        };
    }

    /// Where `source` goes: its assignment, or the master.
    pub fn destination(&self, source: SourceKey) -> u32 {
        self.sources
            .iter()
            .find(|a| a.source == source)
            .map_or(MASTER, |a| a.destination)
    }

    /// A fresh track id, or [`MixerError::IdsUsedUp`]. The counter never wraps or resets.
    pub fn take_track_id(&mut self) -> Result<u32, MixerError> {
        take(&mut self.next_track_id, "track")
    }

    pub fn take_insert_id(&mut self) -> Result<u32, MixerError> {
        take(&mut self.next_insert_id, "insert")
    }

    pub fn take_send_id(&mut self) -> Result<u32, MixerError> {
        take(&mut self.next_send_id, "send")
    }

    /// The engine's routing descriptors. Inserts declare zero latency and memory: these serve
    /// graph validation only, the runtime prepares and admits real devices.
    pub fn descriptors(&self) -> engine::Mixer {
        let tap = |t: Tap| match t {
            Tap::Pre => engine::Tap::PreFader,
            Tap::Post => engine::Tap::PostFader,
        };
        engine::Mixer {
            tracks: self
                .tracks
                .iter()
                .map(|t| engine::Track {
                    id: engine::MixerTrackId(t.id.into()),
                    output: t.output.map(|o| engine::MixerTrackId(o.into())),
                    inserts: t
                        .inserts
                        .iter()
                        .map(|i| engine::Insert {
                            id: engine::InsertId(i.id.into()),
                            latency_frames: 0,
                            memory_bytes: 0,
                        })
                        .collect(),
                })
                .collect(),
            sends: self
                .sends
                .iter()
                .map(|s| engine::Send {
                    id: engine::SendId(s.id.into()),
                    from: engine::MixerTrackId(s.from.into()),
                    to: engine::MixerTrackId(s.to.into()),
                    tap: tap(s.tap),
                    gain: s.gain,
                })
                .collect(),
            sidechains: self
                .sidechains
                .iter()
                .map(|s| engine::Sidechain {
                    from: engine::MixerTrackId(s.from.into()),
                    to: engine::InsertId(s.to.into()),
                    tap: tap(s.tap),
                })
                .collect(),
            sources: self
                .sources
                .iter()
                .map(|a| engine::Source {
                    id: match a.source {
                        SourceKey::Channel(id) => engine::SourceId::Channel(id.into()),
                        SourceKey::AudioTrack(id) => engine::SourceId::AudioTrack(id.into()),
                    },
                    destination: engine::MixerTrackId(a.destination.into()),
                })
                .collect(),
        }
    }

    /// Entries and kept bytes, counted exactly as a read charges them.
    fn retained(&self) -> Option<(usize, usize)> {
        let mut entries = 0usize;
        let mut bytes = 0usize;
        let mut add = |n: usize, size: usize, extra: usize| -> Option<()> {
            entries = entries.checked_add(n)?;
            bytes = bytes
                .checked_add(n.checked_mul(size)?)?
                .checked_add(extra)?;
            Some(())
        };
        for t in &self.tracks {
            add(1, size_of::<Track>(), t.name.len())?;
            for i in &t.inserts {
                let label = i.preset.as_ref().map_or(0, String::len);
                add(1, size_of::<Insert>(), i.device.len().checked_add(label)?)?;
                add(i.params.len(), size_of::<f32>(), 0)?;
            }
        }
        add(self.sends.len(), size_of::<Send>(), 0)?;
        add(self.sidechains.len(), size_of::<Sidechain>(), 0)?;
        add(self.sources.len(), size_of::<Assignment>(), 0)?;
        Some((entries, bytes))
    }

    /// Checks every rule of the format against [`Limits::DEFAULT`]; `channels` and
    /// `audio_tracks` are the sources the project has.
    pub fn validate(
        &self,
        channels: &HashSet<u32>,
        audio_tracks: &HashSet<u32>,
    ) -> Result<(), MixerError> {
        self.validate_with(channels, audio_tracks, Limits::DEFAULT)
    }

    #[doc(hidden)]
    pub fn validate_with(
        &self,
        channels: &HashSet<u32>,
        audio_tracks: &HashSet<u32>,
        limits: Limits,
    ) -> Result<(), MixerError> {
        let inserts: usize = self.tracks.iter().map(|t| t.inserts.len()).sum();
        for (n, limit, what) in [
            (self.tracks.len(), limits.tracks, "tracks"),
            (inserts, limits.inserts, "inserts"),
            (self.sends.len(), limits.sends, "sends"),
            (self.sidechains.len(), limits.sidechains, "sidechains"),
            (self.sources.len(), limits.assignments, "assignments"),
        ] {
            if n > limit {
                return Err(MixerError::TooBig(what));
            }
        }
        let (entries, bytes) = self.retained().ok_or(MixerError::TooBig("entries"))?;
        if entries > limits.entries {
            return Err(MixerError::TooBig("entries"));
        }
        if bytes > limits.bytes {
            return Err(MixerError::TooBig("bytes"));
        }

        let mut tracks = HashSet::new();
        let mut insert_ids = HashSet::new();
        for t in &self.tracks {
            if !tracks.insert(t.id) {
                return Err(MixerError::DuplicateTrack(t.id));
            }
            if t.id != MASTER && t.id >= self.next_track_id {
                return Err(MixerError::InvalidId("track", t.id));
            }
            if (t.id == MASTER) != t.output.is_none() {
                return Err(MixerError::InvalidOutput(t.id));
            }
            if t.name.len() > limits.name_bytes {
                return Err(MixerError::InvalidName);
            }
            match t.level {
                Level::Db(db) if db.is_finite() && db > LEVEL_DB.0 && db <= LEVEL_DB.1 => {}
                Level::Silence => {}
                Level::Db(_) => return Err(MixerError::InvalidLevel(t.id)),
            }
            if !(t.pan.is_finite() && (-1.0..=1.0).contains(&t.pan)) {
                return Err(MixerError::InvalidPan(t.id));
            }
            for i in &t.inserts {
                if !insert_ids.insert(i.id) {
                    return Err(MixerError::DuplicateInsert(i.id));
                }
                if i.id == 0 || i.id >= self.next_insert_id {
                    return Err(MixerError::InvalidId("insert", i.id));
                }
                if i.device.len() > limits.device_bytes || !EFFECTS.contains(&i.device.as_str()) {
                    return Err(MixerError::UnknownDevice(i.id));
                }
                let spec =
                    grooviq_dsp::param_spec(&i.device).ok_or(MixerError::UnknownDevice(i.id))?;
                if i.params.len() != spec.len()
                    || !i
                        .params
                        .iter()
                        .zip(spec)
                        .all(|(v, &(min, max, _))| v.is_finite() && (min..=max).contains(v))
                {
                    return Err(MixerError::InvalidParams(i.id));
                }
                if i.preset
                    .as_ref()
                    .is_some_and(|p| p.len() > limits.name_bytes)
                {
                    return Err(MixerError::InvalidName);
                }
            }
        }
        if !tracks.contains(&MASTER) {
            return Err(MixerError::MissingMaster);
        }
        if self.next_track_id == 0 {
            return Err(MixerError::InvalidId("next track", 0));
        }
        if self.next_insert_id == 0 {
            return Err(MixerError::InvalidId("next insert", 0));
        }
        if self.next_send_id == 0 {
            return Err(MixerError::InvalidId("next send", 0));
        }
        let mut sends = HashSet::new();
        for s in &self.sends {
            if !sends.insert(s.id) {
                return Err(MixerError::DuplicateSend(s.id));
            }
            if s.id == 0 || s.id >= self.next_send_id {
                return Err(MixerError::InvalidId("send", s.id));
            }
            if !(s.gain.is_finite() && (0.0..=MAX_SEND_GAIN).contains(&s.gain)) {
                return Err(MixerError::InvalidGain(s.id));
            }
            if s.from == MASTER {
                return Err(MixerError::InvalidOutput(MASTER));
            }
        }
        let mut sources = HashSet::new();
        for a in &self.sources {
            if !sources.insert(a.source) {
                return Err(MixerError::DuplicateSource(a.source));
            }
            let known = match a.source {
                SourceKey::Channel(id) => channels.contains(&id),
                SourceKey::AudioTrack(id) => audio_tracks.contains(&id),
            };
            if !known {
                return Err(MixerError::UnknownSource(a.source));
            }
        }
        // Topology only: unbounded resources here, the runtime admits real devices.
        let budget = engine::Budget {
            nodes: u64::MAX,
            edges: u64::MAX,
            buffers: u64::MAX,
            latency_frames: u64::MAX,
            memory_bytes: u64::MAX,
            block_frames: 1,
        };
        engine::compile(&self.descriptors(), budget)
            .map_err(|e| MixerError::Graph(format!("{e:?}")))?;
        match (self.mode, self.is_identity()) {
            (Mode::LegacyDirect, false) => Err(MixerError::NotIdentity),
            (Mode::Routed, true) => Err(MixerError::IdentityRouted),
            _ => Ok(()),
        }
    }
}

fn take(counter: &mut u32, what: &'static str) -> Result<u32, MixerError> {
    let id = (*counter).max(1);
    if id == u32::MAX {
        return Err(MixerError::IdsUsedUp(what));
    }
    *counter = id + 1;
    Ok(id)
}

impl Project {
    /// [`Mixer::validate`] against this project's channels and playlist tracks. Run after the
    /// legacy repairs of [`Project::validate`], so it sees the channels that are kept.
    pub fn validate_mixer(&self) -> Result<(), MixerError> {
        let channels = self.channels.iter().map(|c| c.id).collect();
        let tracks = self.arrangement.tracks.iter().map(|t| t.id).collect();
        self.mixer.validate(&channels, &tracks)
    }
}

// ---- Bounded reading ----------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    Track,
    Insert,
    Param,
    Send,
    Sidechain,
    Assignment,
}

struct Budget {
    limits: Limits,
    tracks: usize,
    inserts: usize,
    sends: usize,
    sidechains: usize,
    assignments: usize,
    entries: usize,
    bytes: usize,
}

thread_local! {
    static BUDGET: RefCell<Option<Budget>> = const { RefCell::new(None) };
}

/// Charges one retained entry of `kind` (of `size` bytes) or a string's bytes (`kind` `None`)
/// against the read in progress. Outside a [`Mixer`] read nothing is charged.
fn charge(kind: Option<Kind>, size: usize) -> Result<(), &'static str> {
    BUDGET.with(|b| {
        let mut b = b.borrow_mut();
        let Some(b) = b.as_mut() else {
            return Ok(());
        };
        if let Some(kind) = kind {
            let l = b.limits;
            let counted = match kind {
                Kind::Track => Some((&mut b.tracks, l.tracks, "mixer track limit exceeded")),
                Kind::Insert => Some((&mut b.inserts, l.inserts, "mixer insert limit exceeded")),
                Kind::Send => Some((&mut b.sends, l.sends, "mixer send limit exceeded")),
                Kind::Sidechain => Some((
                    &mut b.sidechains,
                    l.sidechains,
                    "mixer sidechain limit exceeded",
                )),
                Kind::Assignment => Some((
                    &mut b.assignments,
                    l.assignments,
                    "mixer assignment limit exceeded",
                )),
                Kind::Param => None,
            };
            if let Some((count, limit, what)) = counted {
                if *count == limit {
                    return Err(what);
                }
                *count += 1;
            }
            if b.entries == l.entries {
                return Err("mixer entry limit exceeded");
            }
            b.entries += 1;
        }
        match b.bytes.checked_add(size) {
            Some(bytes) if bytes <= b.limits.bytes => {
                b.bytes = bytes;
                Ok(())
            }
            _ => Err("mixer byte limit exceeded"),
        }
    })
}

/// A list whose elements are charged as `kind` one at a time, before each is kept.
fn list<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
    kind: Kind,
) -> Result<Vec<T>, D::Error> {
    struct List<T>(Kind, PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for List<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a bounded list")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let mut out = Vec::new();
            while let Some(crate::reading::Named(x)) = seq.next_element()? {
                charge(Some(self.0), size_of::<T>()).map_err(de::Error::custom)?;
                out.push(x);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(List(kind, PhantomData))
}

fn tracks<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Track>, D::Error> {
    list(d, Kind::Track)
}
fn inserts<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Insert>, D::Error> {
    list(d, Kind::Insert)
}
fn sends<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Send>, D::Error> {
    list(d, Kind::Send)
}
fn sidechains<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Sidechain>, D::Error> {
    list(d, Kind::Sidechain)
}
fn assignments<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Assignment>, D::Error> {
    list(d, Kind::Assignment)
}

/// The most parameters any effect has: a longer list is refused while it is read.
fn max_params() -> usize {
    EFFECTS
        .iter()
        .filter_map(|id| grooviq_dsp::param_spec(id))
        .map(<[_]>::len)
        .max()
        .unwrap_or(0)
}

fn params<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f32>, D::Error> {
    struct Params;
    impl<'de> Visitor<'de> for Params {
        type Value = Vec<f32>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a list of parameter values")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<f32>, A::Error> {
            let most = max_params();
            let mut out = Vec::new();
            while let Some(v) = seq.next_element::<f32>()? {
                if out.len() == most {
                    return Err(de::Error::custom("too many insert parameters"));
                }
                charge(Some(Kind::Param), size_of::<f32>()).map_err(de::Error::custom)?;
                out.push(v);
            }
            Ok(out)
        }
    }
    d.deserialize_seq(Params)
}

/// A string of at most `max` bytes (from the read's limits), charged before it is copied.
fn bounded_str<'de, D: Deserializer<'de>>(
    d: D,
    max: fn(&Limits) -> usize,
) -> Result<String, D::Error> {
    struct Str(fn(&Limits) -> usize);
    impl Visitor<'_> for Str {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a short string")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            let max = BUDGET.with(|b| b.borrow().as_ref().map(|b| (self.0)(&b.limits)));
            if max.is_some_and(|max| v.len() > max) {
                return Err(E::custom("mixer name too long"));
            }
            charge(None, v.len()).map_err(E::custom)?;
            Ok(v.to_owned())
        }
    }
    d.deserialize_str(Str(max))
}

fn name<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_str(d, |l| l.name_bytes)
}
fn device<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_str(d, |l| l.device_bytes)
}
fn label<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    struct Label(String);
    impl<'de> Deserialize<'de> for Label {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            name(d).map(Label)
        }
    }
    Ok(Option::<Label>::deserialize(d)?.map(|l| l.0))
}

fn write_seed<S: Serializer>(seed: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(seed)
}

/// A canonical decimal `u64`: digits only, no leading zero (but "0"), at most 20 digits.
fn seed<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    struct Seed;
    impl Visitor<'_> for Seed {
        type Value = u64;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a decimal seed string")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<u64, E> {
            let canonical = !v.is_empty()
                && v.len() <= 20
                && v.bytes().all(|b| b.is_ascii_digit())
                && (v == "0" || !v.starts_with('0'));
            canonical
                .then(|| v.parse().ok())
                .flatten()
                .ok_or_else(|| E::custom("insert seed is not a canonical decimal u64"))
        }
    }
    d.deserialize_str(Seed)
}

/// The mixer as written: the same fields, read strictly.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Raw {
    mode: Mode,
    #[serde(deserialize_with = "tracks")]
    tracks: Vec<Track>,
    #[serde(default, deserialize_with = "sends")]
    sends: Vec<Send>,
    #[serde(default, deserialize_with = "sidechains")]
    sidechains: Vec<Sidechain>,
    #[serde(default, deserialize_with = "assignments")]
    sources: Vec<Assignment>,
    next_track_id: u32,
    next_insert_id: u32,
    next_send_id: u32,
}

impl<'de> Deserialize<'de> for Mixer {
    /// Reads with [`Limits::DEFAULT`] (see [`read_with_limits`]).
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        deserialize_with_limits(d, Limits::DEFAULT)
    }
}

fn deserialize_with_limits<'de, D: Deserializer<'de>>(
    d: D,
    limits: Limits,
) -> Result<Mixer, D::Error> {
    /// Removes the budget however the read ends.
    struct Clear(Option<Budget>);
    impl Drop for Clear {
        fn drop(&mut self) {
            BUDGET.with(|b| *b.borrow_mut() = self.0.take());
        }
    }
    let previous = BUDGET.with(|b| {
        b.borrow_mut().replace(Budget {
            limits,
            tracks: 0,
            inserts: 0,
            sends: 0,
            sidechains: 0,
            assignments: 0,
            entries: 0,
            bytes: 0,
        })
    });
    let _clear = Clear(previous);
    let raw = crate::reading::Named::<Raw>::deserialize(d)?.0;
    Ok(Mixer {
        mode: raw.mode,
        tracks: raw.tracks,
        sends: raw.sends,
        sidechains: raw.sidechains,
        sources: raw.sources,
        next_track_id: raw.next_track_id,
        next_insert_id: raw.next_insert_id,
        next_send_id: raw.next_send_id,
    })
}

/// Reads a mixer from JSON with injected `limits`: the test hook for exact bounds. Not part
/// of the API.
#[doc(hidden)]
pub fn read_with_limits(json: &str, limits: Limits) -> serde_json::Result<Mixer> {
    let mut de = serde_json::Deserializer::from_str(json);
    let mixer = deserialize_with_limits(&mut de, limits)?;
    de.end()?;
    Ok(mixer)
}

/// Consumes a value without reading it: a mixer field in a file older than version 4 was
/// unknown then, whatever it holds.
pub(crate) fn skip<'de, D: Deserializer<'de>>(d: D) -> Result<Mixer, D::Error> {
    IgnoredAny::deserialize(d)?;
    Ok(Mixer::default())
}
