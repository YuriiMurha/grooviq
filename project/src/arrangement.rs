//! Editable song data. File validation refuses invalid arrangements atomically.
use crate::{Project, ProjectKey, SCALES, SampleRef};
use serde::de::{Deserializer, Error, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};
pub const MAX_SONG_TICKS: u64 = 38_400_000;
pub const MAX_TRACKS: usize = 256;
pub const MAX_CLIPS: usize = 100_000;
pub const MAX_AUDIO_ASSETS: usize = 4096;
pub const MAX_AUTOMATION_POINTS: usize = 16_384;
pub const MAX_TOTAL_AUTOMATION_POINTS: usize = 1_000_000;
fn bounded<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    struct List<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for List<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "at most {N} entries")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Vec<T>, A::Error> {
            let mut v = Vec::new();
            loop {
                if v.len() == N {
                    if a.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(A::Error::custom("arrangement list limit exceeded"));
                    }
                    break;
                }
                let Some(x) = a.next_element::<crate::reading::Named<T>>()? else {
                    break;
                };
                if N == MAX_AUTOMATION_POINTS && !crate::reading::retain_automation_point() {
                    return Err(A::Error::custom("total automation point limit exceeded"));
                }
                v.push(x.0);
            }
            Ok(v)
        }
    }
    d.deserialize_seq(List::<T, N>(std::marker::PhantomData))
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayMode {
    #[default]
    Pattern,
    Song,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Arrangement {
    #[serde(default, deserialize_with = "bounded::<_, _, MAX_TRACKS>")]
    pub tracks: Vec<PlaylistTrack>,
    #[serde(default, deserialize_with = "bounded::<_, _, MAX_CLIPS>")]
    pub clips: Vec<Clip>,
    #[serde(default, deserialize_with = "bounded::<_, _, MAX_AUDIO_ASSETS>")]
    pub audio_assets: Vec<AudioAsset>,
    #[serde(default, deserialize_with = "crate::reading::named_option")]
    pub loop_region: Option<LoopRegion>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistTrack {
    pub id: u32,
    pub name: String,
    pub color: u8,
    pub mute: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopRegion {
    pub start_tick: u64,
    pub end_tick: u64,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAsset {
    pub id: u32,
    #[serde(deserialize_with = "crate::reading::named")]
    pub sample: SampleRef,
    pub source_rate: u32,
    pub source_frames: u64,
    pub original_tempo: Option<f32>,
    pub original_key: Option<ProjectKey>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: u32,
    pub track: u32,
    pub name: String,
    pub start_tick: u64,
    pub muted: bool,
    pub content: ClipContent,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClipContent {
    Pattern {
        pattern: u32,
        offset_tick: u32,
        length_ticks: u64,
    },
    Audio {
        asset: u32,
        source_start: u64,
        source_end: u64,
        gain: f32,
        reverse: bool,
        fade_in: u64,
        fade_out: u64,
    },
    Automation {
        target: AutomationTarget,
        priority: u32,
        offset_tick: u32,
        length_ticks: u64,
        points: CurvePoints,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[derive(Eq, PartialOrd, Ord)]
pub enum AutomationTarget {
    ChannelVolume {
        channel: u32,
    },
    ChannelPan {
        channel: u32,
    },
    InstrumentParam {
        channel: u32,
        instrument: String,
        param: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SegmentMode {
    Linear,
    Hold,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPoint {
    pub tick: u64,
    pub value: f32,
    pub mode: SegmentMode,
}
/// Immutable shared curve allocation. Serialization remains the v3 point array; bounded
/// decoding still runs through Points before converting to shared storage.
#[derive(Clone, Debug)]
pub struct CurvePoints(std::sync::Arc<Vec<AutomationPoint>>);
impl PartialEq for CurvePoints {
    fn eq(&self, other: &Self) -> bool {
        self.shares_allocation(other) || self.0 == other.0
    }
}
impl From<Vec<AutomationPoint>> for CurvePoints {
    fn from(points: Vec<AutomationPoint>) -> Self {
        Self(std::sync::Arc::new(points))
    }
}
impl std::ops::Deref for CurvePoints {
    type Target = [AutomationPoint];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl CurvePoints {
    pub fn shares_allocation(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn allocation(&self) -> (usize, usize) {
        (
            std::sync::Arc::as_ptr(&self.0) as usize,
            std::mem::size_of::<Vec<AutomationPoint>>()
                + 2 * std::mem::size_of::<usize>()
                + self.0.capacity() * std::mem::size_of::<AutomationPoint>(),
        )
    }
    /// Explicit copy-on-write for source editing; ordinary geometry never calls this.
    pub fn make_mut(&mut self) -> &mut Vec<AutomationPoint> {
        std::sync::Arc::make_mut(&mut self.0)
    }
}
impl Serialize for CurvePoints {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_slice().serialize(serializer)
    }
}

// Flat readers deliberately avoid serde's internally-tagged Content buffer. Unknown fields
// stream through IgnoredAny; points reach their cap before the rest of the array is read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentFields {
    #[serde(rename = "type")]
    kind: String,
    pattern: Option<u32>,
    offset_tick: Option<u32>,
    length_ticks: Option<u64>,
    asset: Option<u32>,
    source_start: Option<u64>,
    source_end: Option<u64>,
    gain: Option<f32>,
    reverse: Option<bool>,
    fade_in: Option<u64>,
    fade_out: Option<u64>,
    target: Option<AutomationTarget>,
    priority: Option<u32>,
    points: Option<Points>,
}
#[derive(Deserialize)]
#[serde(transparent)]
pub(crate) struct Points(
    #[serde(deserialize_with = "bounded::<_, _, MAX_AUTOMATION_POINTS>")]
    pub(crate)  Vec<AutomationPoint>,
);
fn required<T, E: Error>(value: Option<T>, name: &'static str) -> Result<T, E> {
    value.ok_or_else(|| E::missing_field(name))
}
impl<'de> Deserialize<'de> for ClipContent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let f = crate::reading::named::<D, ContentFields>(d)?;
        Ok(match f.kind.as_str() {
            "pattern" => Self::Pattern {
                pattern: required(f.pattern, "pattern")?,
                offset_tick: required(f.offset_tick, "offsetTick")?,
                length_ticks: required(f.length_ticks, "lengthTicks")?,
            },
            "audio" => Self::Audio {
                asset: required(f.asset, "asset")?,
                source_start: required(f.source_start, "sourceStart")?,
                source_end: required(f.source_end, "sourceEnd")?,
                gain: required(f.gain, "gain")?,
                reverse: required(f.reverse, "reverse")?,
                fade_in: required(f.fade_in, "fadeIn")?,
                fade_out: required(f.fade_out, "fadeOut")?,
            },
            "automation" => Self::Automation {
                target: required(f.target, "target")?,
                priority: required(f.priority, "priority")?,
                offset_tick: required(f.offset_tick, "offsetTick")?,
                length_ticks: required(f.length_ticks, "lengthTicks")?,
                points: required(f.points, "points")?.0.into(),
            },
            _ => {
                return Err(D::Error::unknown_variant(
                    &f.kind,
                    &["pattern", "audio", "automation"],
                ));
            }
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetFields {
    #[serde(rename = "type")]
    kind: String,
    channel: u32,
    instrument: Option<String>,
    param: Option<String>,
}
impl<'de> Deserialize<'de> for AutomationTarget {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let f = crate::reading::named::<D, TargetFields>(d)?;
        Ok(match f.kind.as_str() {
            "channelVolume" => Self::ChannelVolume { channel: f.channel },
            "channelPan" => Self::ChannelPan { channel: f.channel },
            "instrumentParam" => Self::InstrumentParam {
                channel: f.channel,
                instrument: required(f.instrument, "instrument")?,
                param: required(f.param, "param")?,
            },
            _ => {
                return Err(D::Error::unknown_variant(
                    &f.kind,
                    &["channelVolume", "channelPan", "instrumentParam"],
                ));
            }
        })
    }
}
impl Project {
    /// Shared monotonic identity allocator. Exhaustion is permanent.
    pub fn take_id(&mut self) -> Option<u32> {
        let mut id = self.next_id;
        while id < u32::MAX {
            self.next_id = id + 1;
            if !self.channels.iter().any(|x| x.id == id)
                && !self.patterns.iter().any(|x| x.id == id)
                && !self.arrangement.ids().any(|x| x == id)
            {
                return Some(id);
            }
            id += 1;
        }
        self.next_id = u32::MAX;
        None
    }
    pub fn validate_arrangement(&self) -> Result<(), String> {
        let a = &self.arrangement;
        let bad = || {
            Err(
                "Invalid arrangement: bounds, identity, reference or native parameter range."
                    .into(),
            )
        };
        if a.tracks.len() > MAX_TRACKS
            || a.clips.len() > MAX_CLIPS
            || a.audio_assets.len() > MAX_AUDIO_ASSETS
        {
            return bad();
        }
        let mut patterns = HashSet::new();
        patterns
            .try_reserve(self.patterns.len())
            .map_err(|_| "Cannot index patterns")?;
        patterns.extend(self.patterns.iter().map(|p| p.id));
        let tracks: HashSet<_> = a.tracks.iter().map(|t| t.id).collect();
        let assets: HashMap<_, _> = a.audio_assets.iter().map(|a| (a.id, a)).collect();
        let mut channels = HashMap::new();
        channels
            .try_reserve(self.channels.len())
            .map_err(|_| "Cannot index channels")?;
        channels.extend(self.channels.iter().map(|c| (c.id, c)));
        let mut ids = HashSet::new();
        let capacity = self
            .channels
            .len()
            .checked_add(self.patterns.len())
            .and_then(|n| n.checked_add(a.tracks.len() + a.clips.len() + a.audio_assets.len()))
            .ok_or("Identity index overflow")?;
        ids.try_reserve(capacity)
            .map_err(|_| "Cannot index project identities")?;
        ids.extend(
            self.channels
                .iter()
                .map(|c| c.id)
                .chain(self.patterns.iter().map(|p| p.id)),
        );
        for id in a.entity_ids() {
            if id == u32::MAX || !ids.insert(id) {
                return bad();
            }
        }
        if let Some(l) = &a.loop_region
            && (l.start_tick >= l.end_tick || l.end_tick > MAX_SONG_TICKS)
        {
            return bad();
        }
        for asset in &a.audio_assets {
            let bad = || {
                Err(format!(
                    "Audio asset {}: invalid source rate, frame count, original tempo or key.",
                    asset.id
                ))
            };
            // Same provenance range as engine/sample_file.rs (decoder range is private).
            if !(8_000..=384_000).contains(&asset.source_rate)
                || asset.source_frames == 0
                || asset.source_frames
                    > u64::from(asset.source_rate)
                        .checked_mul(grooviq_engine::MAX_SAMPLE_SECONDS as u64)
                        .ok_or("Audio source duration overflow")?
                || asset
                    .original_tempo
                    .is_some_and(|t| !t.is_finite() || !(20.0..=400.0).contains(&t))
                || asset
                    .original_key
                    .as_ref()
                    .is_some_and(|k| k.root > 11 || !SCALES.iter().any(|(s, _)| *s == k.scale))
            {
                return bad();
            }
        }
        let mut priorities =
            std::collections::BTreeMap::<u32, Vec<(&AutomationTarget, u64, u64)>>::new();
        for clip in &a.clips {
            if let ClipContent::Automation {
                priority,
                target,
                length_ticks,
                ..
            } = &clip.content
            {
                if *priority == u32::MAX || ids.contains(priority) {
                    return bad();
                }
                let end = clip
                    .start_tick
                    .checked_add(*length_ticks)
                    .ok_or("automation end overflow")?;
                priorities
                    .entry(*priority)
                    .or_default()
                    .push((target, clip.start_tick, end));
            }
        }
        for fragments in priorities.values_mut() {
            fragments.sort_unstable();
            if fragments
                .windows(2)
                .any(|p| p[0].0 != p[1].0 || p[0].2 > p[1].1)
            {
                return Err("Automation fragments sharing a priority must use one target and remain disjoint; copy to create a new layer.".into());
            }
        }
        let mut total = 0usize;
        for c in &a.clips {
            let bad = || {
                Err(format!(
                    "Clip {} ({}): invalid track/source reference, bounds or automation value.",
                    c.id, c.name
                ))
            };
            if c.start_tick >= MAX_SONG_TICKS || !tracks.contains(&c.track) {
                return bad();
            }
            let length = match &c.content {
                ClipContent::Pattern {
                    pattern,
                    offset_tick,
                    length_ticks,
                } => {
                    let Some(_) = patterns.get(pattern) else {
                        return bad();
                    };
                    if u64::from(*offset_tick) > MAX_SONG_TICKS {
                        return bad();
                    }
                    *length_ticks
                }
                ClipContent::Audio {
                    asset,
                    source_start,
                    source_end,
                    gain,
                    fade_in,
                    fade_out,
                    ..
                } => {
                    let Some(a) = assets.get(asset) else {
                        return bad();
                    };
                    if source_start >= source_end
                        || *source_end > a.source_frames
                        || !gain.is_finite()
                        || !(0.0..=2.0).contains(gain)
                    {
                        return bad();
                    }
                    let frames = source_end - source_start;
                    if *fade_in > frames || *fade_out > frames {
                        return bad();
                    }
                    let ticks =
                        (frames as f64 / a.source_rate as f64 * self.bpm as f64 * 16.0).ceil();
                    if ticks > MAX_SONG_TICKS as f64 {
                        return bad();
                    }
                    ticks as u64
                }
                ClipContent::Automation {
                    target,
                    priority: _,
                    offset_tick,
                    length_ticks,
                    points,
                } => {
                    total = total
                        .checked_add(points.len())
                        .ok_or("automation point overflow")?;
                    if points.len() > MAX_AUTOMATION_POINTS || total > MAX_TOTAL_AUTOMATION_POINTS {
                        return bad();
                    }
                    let (channel, range) = match target {
                        AutomationTarget::ChannelVolume { channel } => (*channel, Some((0.0, 2.0))),
                        AutomationTarget::ChannelPan { channel } => (*channel, Some((-1.0, 1.0))),
                        AutomationTarget::InstrumentParam {
                            channel,
                            instrument,
                            param,
                        } => {
                            let Some(c) = channels.get(channel) else {
                                return bad();
                            };
                            let (names, spec) = match (instrument.as_str(), c.instrument) {
                                ("sampler", grooviq_engine::InstrumentKind::Sampler) => (
                                    grooviq_dsp::sampler::PARAM_IDS.as_slice(),
                                    grooviq_dsp::sampler::SPEC.as_slice(),
                                ),
                                ("synth", grooviq_engine::InstrumentKind::Synth) => (
                                    grooviq_dsp::synth::PARAM_IDS.as_slice(),
                                    grooviq_dsp::synth::SPEC.as_slice(),
                                ),
                                _ => return bad(),
                            };
                            (
                                *channel,
                                names
                                    .iter()
                                    .position(|p| *p == param)
                                    .map(|i| (spec[i].0, spec[i].1)),
                            )
                        }
                    };
                    let Some((min, max)) = range else {
                        return bad();
                    };
                    if !channels.contains_key(&channel) {
                        return bad();
                    }
                    let Some(end) = u64::from(*offset_tick).checked_add(*length_ticks) else {
                        return bad();
                    };
                    if end > MAX_SONG_TICKS
                        || points.windows(2).any(|p| p[0].tick > p[1].tick)
                        || points.iter().any(|p| {
                            p.tick > MAX_SONG_TICKS
                                || !p.value.is_finite()
                                || !(min..=max).contains(&p.value)
                        })
                    {
                        return bad();
                    }
                    *length_ticks
                }
            };
            if length == 0
                || c.start_tick
                    .checked_add(length)
                    .is_none_or(|end| end > MAX_SONG_TICKS)
            {
                return bad();
            }
        }
        Ok(())
    }
}
impl Arrangement {
    pub(crate) fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.entity_ids()
            .chain(self.clips.iter().filter_map(|c| match c.content {
                ClipContent::Automation { priority, .. } => Some(priority),
                _ => None,
            }))
    }

    pub(crate) fn entity_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.tracks
            .iter()
            .map(|x| x.id)
            .chain(self.clips.iter().map(|x| x.id))
            .chain(self.audio_assets.iter().map(|x| x.id))
    }
}

#[cfg(test)]
mod shared_curve_tests {
    use super::*;
    #[test]
    fn shared_curve_serialization_is_exact_v3_array_and_source_edits_use_cow() {
        let original = vec![
            AutomationPoint {
                tick: 0,
                value: 0.25,
                mode: SegmentMode::Linear,
            },
            AutomationPoint {
                tick: 960,
                value: 0.75,
                mode: SegmentMode::Hold,
            },
        ];
        let points: CurvePoints = original.clone().into();
        let mut copy = points.clone();
        assert!(points.shares_allocation(&copy));
        assert_eq!(
            serde_json::to_vec(&points).unwrap(),
            serde_json::to_vec(&original).unwrap()
        );
        copy.make_mut()[0].value = 0.5;
        assert!(!points.shares_allocation(&copy));
        assert_eq!(points[0].value, 0.25);
    }
}
