//! Retained changed entities only; immutable curves share allocation ownership with live data.
use crate::mixer::{Assignment, Mixer, Mode, Send, Sidechain, SourceKey, Track};
use crate::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(crate) struct Delta<T> {
    pub before: Option<(usize, T)>,
    pub after: Option<(usize, T)>,
}
#[derive(Clone)]
pub(crate) struct MediaVersion {
    pub id: u32,
    pub file: Option<SampleFile>,
    pub decoded: Option<std::sync::Arc<grooviq_engine::SampleBuf>>,
    pub peaks: Option<std::sync::Arc<grooviq_engine::waveform::PeakPyramid>>,
}
impl std::fmt::Debug for MediaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaVersion")
            .field("id", &self.id)
            .finish()
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub clips: Vec<Delta<Clip>>,
    pub tracks: Vec<Delta<PlaylistTrack>>,
    pub patterns: Vec<Delta<Pattern>>,
    pub assets: Vec<Delta<AudioAsset>>,
    pub media: Vec<(MediaVersion, MediaVersion)>,
    pub region: Option<(Option<LoopRegion>, Option<LoopRegion>)>,
    pub mixer: Option<Box<MixerChange>>,
    pub order: u64,
}
fn diff<T: Clone + PartialEq, K: Copy + Ord + std::hash::Hash>(
    a: &[T],
    b: &[T],
    id: impl Fn(&T) -> K,
) -> Vec<Delta<T>> {
    let old: HashMap<_, _> = a.iter().enumerate().map(|(i, x)| (id(x), (i, x))).collect();
    let new: HashMap<_, _> = b.iter().enumerate().map(|(i, x)| (id(x), (i, x))).collect();
    let mut keys: Vec<_> = old.keys().chain(new.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter()
        .filter_map(|k| {
            let before = old.get(&k);
            let after = new.get(&k);
            if before.map(|(_, x)| *x) == after.map(|(_, x)| *x) {
                None
            } else {
                Some(Delta {
                    before: before.map(|(i, x)| (*i, (*x).clone())),
                    after: after.map(|(i, x)| (*i, (*x).clone())),
                })
            }
        })
        .collect()
}
fn apply<T: Clone, K: PartialEq>(
    list: &mut Vec<T>,
    deltas: &[Delta<T>],
    undo: bool,
    id: impl Fn(&T) -> K,
) {
    let mut insert = Vec::new();
    for d in deltas {
        let (take, put) = if undo {
            (&d.after, &d.before)
        } else {
            (&d.before, &d.after)
        };
        if let Some((_, x)) = take {
            list.retain(|c| id(c) != id(x));
        }
        if let Some((i, x)) = put {
            insert.push((*i, x.clone()));
        }
    }
    insert.sort_by_key(|(i, _)| *i);
    for (i, x) in insert {
        list.insert(i.min(list.len()), x);
    }
}
#[derive(Clone, Debug)]
pub(crate) struct MixerChange {
    tracks: Vec<Delta<Track>>,
    sends: Vec<Delta<Send>>,
    sidechains: Vec<Delta<Sidechain>>,
    sources: Vec<Delta<Assignment>>,
    mode: Option<(Mode, Mode)>,
    recheck_mode: bool,
}

fn sidechain_id(s: &Sidechain) -> (u32, u32, bool) {
    (s.from, s.to, s.tap == crate::mixer::Tap::Post)
}

impl MixerChange {
    fn between(before: &Mixer, after: &Mixer) -> Option<Box<Self>> {
        if before == after {
            return None;
        }
        let change = Self {
            tracks: diff(&before.tracks, &after.tracks, |t| t.id),
            sends: diff(&before.sends, &after.sends, |s| s.id),
            sidechains: diff(&before.sidechains, &after.sidechains, sidechain_id),
            sources: diff(&before.sources, &after.sources, |a| a.source),
            mode: (before.mode != after.mode).then_some((before.mode, after.mode)),
            recheck_mode: false,
        };
        (!change.is_empty()).then(|| Box::new(change))
    }

    fn is_empty(&self) -> bool {
        self.tracks.is_empty()
            && self.sends.is_empty()
            && self.sidechains.is_empty()
            && self.sources.is_empty()
            && self.mode.is_none()
    }

    fn apply(&self, mixer: &mut Mixer, undo: bool) {
        apply(&mut mixer.tracks, &self.tracks, undo, |t| t.id);
        apply(&mut mixer.sends, &self.sends, undo, |s| s.id);
        apply(&mut mixer.sidechains, &self.sidechains, undo, sidechain_id);
        apply(&mut mixer.sources, &self.sources, undo, |a| a.source);
        if self.recheck_mode {
            mixer.refresh_mode();
        } else if let Some((before, after)) = self.mode {
            mixer.mode = if undo { before } else { after };
        }
        // Allocation counters are taken from the current project and never part of an inverse.
    }

    fn forget_channel(&mut self, channel: u32) {
        self.sources.retain(|d| {
            d.before
                .iter()
                .chain(&d.after)
                .all(|(_, a)| a.source != SourceKey::Channel(channel))
        });
        self.sources.shrink_to_fit();
        // A removed assignment may have been the last difference from LegacyDirect
        // in an older project. Recompute the resulting mode as history crosses it.
        self.mode = None;
        self.recheck_mode = true;
    }

    fn bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            + self.tracks.capacity() * std::mem::size_of::<Delta<Track>>()
            + self.sends.capacity() * std::mem::size_of::<Delta<Send>>()
            + self.sidechains.capacity() * std::mem::size_of::<Delta<Sidechain>>()
            + self.sources.capacity() * std::mem::size_of::<Delta<Assignment>>();
        for (_, track) in self
            .tracks
            .iter()
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
        {
            bytes += track.name.capacity()
                + track.inserts.capacity() * std::mem::size_of::<crate::mixer::Insert>();
            for insert in &track.inserts {
                bytes += insert.device.capacity()
                    + insert.params.capacity() * std::mem::size_of::<f32>()
                    + insert.preset.as_ref().map_or(0, String::capacity);
            }
        }
        bytes
    }
}

impl Change {
    pub fn between(a: &Project, b: &Project) -> Self {
        Self {
            assets: diff(
                &a.arrangement.audio_assets,
                &b.arrangement.audio_assets,
                |a| a.id,
            ),
            media: Vec::new(),
            clips: diff(&a.arrangement.clips, &b.arrangement.clips, |c| c.id),
            tracks: diff(&a.arrangement.tracks, &b.arrangement.tracks, |c| c.id),
            patterns: diff(&a.patterns, &b.patterns, |p| p.id),
            region: (a.arrangement.loop_region != b.arrangement.loop_region).then(|| {
                (
                    a.arrangement.loop_region.clone(),
                    b.arrangement.loop_region.clone(),
                )
            }),
            mixer: MixerChange::between(&a.mixer, &b.mixer),
            order: crate::undo::next_order(),
        }
    }
    pub fn apply(&self, p: &mut Project, undo: bool) {
        apply(&mut p.arrangement.audio_assets, &self.assets, undo, |a| {
            a.id
        });
        apply(&mut p.arrangement.clips, &self.clips, undo, |c| c.id);
        apply(&mut p.arrangement.tracks, &self.tracks, undo, |c| c.id);
        apply(&mut p.patterns, &self.patterns, undo, |p| p.id);
        if let Some(mixer) = &self.mixer {
            mixer.apply(&mut p.mixer, undo);
        }
        if let Some((a, b)) = &self.region {
            p.arrangement.loop_region = if undo { a.clone() } else { b.clone() };
        }
    }
    fn touches_pattern(&self, pattern: u32, deleted_index: Option<usize>) -> bool {
        self.clips
            .iter()
            .flat_map(|d| d.before.iter().chain(&d.after))
            .any(|(_, clip)| {
                matches!(clip.content, ClipContent::Pattern { pattern: id, .. } if id == pattern)
            })
            || self
                .patterns
                .iter()
                .flat_map(|d| d.before.iter().chain(&d.after))
                .any(|(index, p)| p.id == pattern || deleted_index.is_some_and(|i| *index >= i))
    }

    fn effects(&self) -> HistoryEffects {
        HistoryEffects {
            playlist: !self.clips.is_empty()
                || !self.tracks.is_empty()
                || !self.patterns.is_empty()
                || !self.assets.is_empty()
                || !self.media.is_empty(),
            // Mixer source assignments can depend on playlist track membership.
            mixer: self.mixer.is_some()
                || self
                    .tracks
                    .iter()
                    .any(|d| d.before.is_none() || d.after.is_none()),
            region: self.region.is_some(),
        }
    }

    fn forget_channel(&mut self, channel: u32) -> bool {
        for d in &mut self.clips {
            for version in [&mut d.before, &mut d.after] {
                if version.as_ref().is_some_and(|(_, clip)| {
                    matches!(&clip.content, ClipContent::Automation { target, .. }
                    if match target {
                        AutomationTarget::ChannelVolume { channel: id }
                        | AutomationTarget::ChannelPan { channel: id }
                        | AutomationTarget::InstrumentParam { channel: id, .. } => *id == channel,
                    })
                }) {
                    *version = None;
                }
            }
        }
        self.clips
            .retain(|d| d.before.is_some() || d.after.is_some());
        self.clips.shrink_to_fit();

        for d in &mut self.patterns {
            for (_, p) in [&mut d.before, &mut d.after].into_iter().flatten() {
                p.notes.retain(|note| note.channel != channel);
                p.notes.shrink_to_fit();
            }
        }

        if let Some(mixer) = &mut self.mixer {
            mixer.forget_channel(channel);
            if mixer.is_empty() {
                self.mixer = None;
            }
        }
        !self.clips.is_empty()
            || !self.tracks.is_empty()
            || !self.patterns.is_empty()
            || !self.assets.is_empty()
            || !self.media.is_empty()
            || self.region.is_some()
            || self.mixer.is_some()
    }

    pub fn notes(&self) -> usize {
        self.patterns
            .iter()
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
            .map(|(_, p)| p.notes.len())
            .sum()
    }
    pub fn bytes(&self, curves: &mut HashMap<usize, usize>) -> usize {
        let mut n = std::mem::size_of::<Self>()
            + self.mixer.as_deref().map_or(0, MixerChange::bytes)
            + self.assets.capacity() * std::mem::size_of::<Delta<AudioAsset>>()
            + self.media.capacity() * std::mem::size_of::<(MediaVersion, MediaVersion)>();
        for (_, a) in self
            .assets
            .iter()
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
        {
            n += a.sample.path.as_os_str().len()
                + a.sample.file.as_ref().map_or(0, |s| s.capacity());
        }
        for (a, b) in &self.media {
            for v in [a, b] {
                if let Some(file) = &v.file {
                    n += file.name.capacity();
                }
            }
        }
        n += self.clips.capacity() * std::mem::size_of::<Delta<Clip>>()
            + self.tracks.capacity() * std::mem::size_of::<Delta<PlaylistTrack>>()
            + self.patterns.capacity() * std::mem::size_of::<Delta<Pattern>>();
        for (_, c) in self
            .clips
            .iter()
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
        {
            n += c.name.capacity();
            if let ClipContent::Automation { points, target, .. } = &c.content {
                let (id, bytes) = points.allocation();
                curves.entry(id).or_insert(bytes);
                if let AutomationTarget::InstrumentParam {
                    instrument, param, ..
                } = target
                {
                    n += instrument.capacity() + param.capacity();
                }
            }
        }
        for (_, t) in self
            .tracks
            .iter()
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
        {
            n += t.name.capacity();
        }
        for (_, p) in self
            .patterns
            .iter()
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
        {
            n += p.name.capacity() + p.notes.capacity() * std::mem::size_of::<Note>();
        }
        n
    }
}
#[derive(Default)]
struct HistoryEffects {
    playlist: bool,
    mixer: bool,
    region: bool,
}
impl HistoryEffects {
    fn overlaps(&self, other: &Self) -> bool {
        (self.playlist && other.playlist)
            || (self.mixer && other.mixer)
            || (self.region && other.region)
    }

    fn include(&mut self, other: &Self) {
        self.playlist |= other.playlist;
        self.mixer |= other.mixer;
        self.region |= other.region;
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ArrangementHistory {
    pub undo: Vec<Change>,
    pub redo: Vec<Change>,
}
impl ArrangementHistory {
    pub fn forget_pattern(
        &mut self,
        pattern: u32,
        deleted_index: Option<usize>,
        floors: &mut HashMap<u32, (u32, u32)>,
    ) {
        for stack in [&mut self.undo, &mut self.redo] {
            // Pending applications run from the top of each stack. An omitted
            // step freezes its applied state on undo and skips it on redo.
            // Whole-entity snapshots farther away can depend on that step.
            let mut blocked = HistoryEffects::default();
            stack.reverse();
            stack.retain(|step| {
                let effects = step.effects();
                if step.touches_pattern(pattern, deleted_index) || blocked.overlaps(&effects) {
                    blocked.include(&effects);
                    false
                } else {
                    true
                }
            });
            stack.reverse();
            stack.shrink_to_fit();
        }
        self.prune_floors(floors);
    }

    fn prune_floors(&self, floors: &mut HashMap<u32, (u32, u32)>) {
        let ids: std::collections::HashSet<_> = self
            .undo
            .iter()
            .chain(&self.redo)
            .flat_map(|c| &c.patterns)
            .flat_map(|d| d.before.iter().chain(&d.after))
            .map(|(_, p)| p.id)
            .collect();
        floors.retain(|id, _| ids.contains(id));
        floors.shrink_to_fit();
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
#[derive(Clone, Copy, Debug)]
pub struct HistoryLimits {
    pub steps: usize,
    pub notes: usize,
    pub bytes: usize,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            steps: 200,
            notes: 2_000_000,
            bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Histories {
    pub current: crate::undo::NoteHistory,
    pub patterns: HashMap<u32, crate::undo::NoteHistory>,
    pub arrangement: ArrangementHistory,
    pub trim: crate::session::sampler_trim::TrimHistory,
    pub floors: HashMap<u32, (u32, u32)>,
    pub protected: std::collections::HashSet<u64>,
}
impl Histories {
    pub fn forget_channel(&mut self, project: &Project, channel: u32) {
        let notes = |pattern| {
            project
                .patterns
                .iter()
                .find(|p| p.id == pattern)
                .map_or(&[][..], |p| p.notes.as_slice())
        };
        self.current
            .forget_channel(channel, notes(project.current_pattern));
        for (&id, history) in &mut self.patterns {
            history.forget_channel(channel, notes(id));
        }
        self.patterns.retain(|_, h| h.can_undo() || h.can_redo());
        self.arrangement
            .undo
            .retain_mut(|step| step.forget_channel(channel));
        self.arrangement
            .redo
            .retain_mut(|step| step.forget_channel(channel));
        self.arrangement.undo.shrink_to_fit();
        self.arrangement.redo.shrink_to_fit();
        self.trim.undo.retain(|step| step.channel != channel);
        self.trim.redo.retain(|step| step.channel != channel);
        self.trim.undo.shrink_to_fit();
        self.trim.redo.shrink_to_fit();
    }

    pub fn clear_redo(&mut self) {
        self.current.clear_redo();
        for h in self.patterns.values_mut() {
            h.clear_redo();
        }
        self.arrangement.redo.clear();
        self.trim.redo.clear();
    }
    pub fn usage(&self) -> (usize, usize, usize) {
        Self::usage_of(
            &self.current,
            &self.patterns,
            &self.arrangement,
            &self.floors,
            &self.trim,
        )
    }
    pub fn usage_of(
        current: &crate::undo::NoteHistory,
        patterns: &HashMap<u32, crate::undo::NoteHistory>,
        arrangement: &ArrangementHistory,
        floors: &HashMap<u32, (u32, u32)>,
        trim: &crate::session::sampler_trim::TrimHistory,
    ) -> (usize, usize, usize) {
        let (mut steps, mut notes) = current.retained();
        let mut bytes = current.bytes()
            + std::mem::size_of_val(floors)
            + floors.capacity()
                * (std::mem::size_of::<(u32, (u32, u32))>() + std::mem::size_of::<usize>())
            + std::mem::size_of_val(patterns)
            + patterns.capacity()
                * (std::mem::size_of::<(u32, crate::undo::NoteHistory)>()
                    + std::mem::size_of::<usize>());
        for h in patterns.values() {
            let (s, n) = h.retained();
            steps += s;
            notes += n;
            bytes += h.bytes();
        }
        bytes += (arrangement.undo.capacity() + arrangement.redo.capacity())
            * std::mem::size_of::<Change>();
        let mut curves = HashMap::new();
        for c in arrangement.undo.iter().chain(&arrangement.redo) {
            steps += 1;
            notes += c.notes();
            bytes += c.bytes(&mut curves);
        }
        bytes += curves.values().sum::<usize>();
        steps += trim.undo.len() + trim.redo.len();
        bytes += trim.bytes();
        (steps, notes, bytes)
    }
    pub fn admit(&mut self, limits: HistoryLimits, current: u32) -> Result<(), String> {
        self.arrangement.prune_floors(&mut self.floors);
        for h in std::iter::once(&self.current).chain(self.patterns.values()) {
            let (n, b) = h.largest(&self.protected);
            if n > limits.notes || b > limits.bytes {
                return Err("One undo step exceeds the shared history budget".into());
            }
        }
        for c in self
            .arrangement
            .undo
            .iter()
            .chain(&self.arrangement.redo)
            .filter(|c| self.protected.contains(&c.order))
        {
            let mut curves = HashMap::new();
            let b = c.bytes(&mut curves) + curves.values().sum::<usize>();
            if c.notes() > limits.notes || b > limits.bytes {
                return Err("One arrangement gesture exceeds the shared history budget".into());
            }
        }
        if self
            .trim
            .undo
            .iter()
            .chain(&self.trim.redo)
            .any(|c| self.protected.contains(&c.order) && std::mem::size_of_val(c) > limits.bytes)
        {
            return Err("One trim step exceeds shared history budget".into());
        }
        if limits.steps == 0 && self.usage().0 > 0 {
            return Err("History step budget exhausted".into());
        }
        loop {
            let (s, n, b) = self.usage();
            if s <= limits.steps && n <= limits.notes && b <= limits.bytes {
                break;
            }
            if s <= 1 {
                return Err("One undo step including metadata exceeds history budget".into());
            }
            let oldest = std::iter::once((current, &self.current))
                .chain(self.patterns.iter().map(|(id, h)| (*id, h)))
                .filter_map(|(id, h)| h.oldest_excluding(&self.protected).map(|o| (o, Some(id))))
                .chain(
                    self.arrangement
                        .undo
                        .iter()
                        .chain(&self.arrangement.redo)
                        .map(|s| s.order)
                        .filter(|o| !self.protected.contains(o))
                        .min()
                        .map(|o| (o, None)),
                )
                .chain(
                    self.trim
                        .undo
                        .iter()
                        .chain(&self.trim.redo)
                        .filter(|c| !self.protected.contains(&c.order))
                        .map(|c| (c.order, None)),
                )
                .min_by_key(|(o, _)| *o);
            match oldest {
                Some((order, None)) => {
                    self.trim.undo.retain(|c| c.order != order);
                    self.trim.redo.retain(|c| c.order != order);
                    self.trim.undo.shrink_to_fit();
                    self.trim.redo.shrink_to_fit();
                    self.arrangement.undo.retain(|s| s.order != order);
                    self.arrangement.redo.retain(|s| s.order != order);
                    self.arrangement.undo.shrink_to_fit();
                    self.arrangement.redo.shrink_to_fit();
                }
                Some((order, Some(id))) if id == current => self.current.evict_order(order),
                Some((order, Some(id))) => self.patterns.get_mut(&id).unwrap().evict_order(order),
                None => return Err("History metadata exceeds the control-byte budget".into()),
            }
            self.patterns.retain(|_, h| h.retained().0 > 0);
            self.arrangement.prune_floors(&mut self.floors);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pattern_clip(id: u32, pattern: u32) -> Clip {
        Clip {
            id,
            track: 1,
            name: String::new(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Pattern {
                pattern,
                offset_tick: 0,
                length_ticks: 1,
            },
        }
    }

    #[test]
    fn forgotten_mixed_step_blocks_dependent_mixer_history_on_both_stacks() {
        for undo in [true, false] {
            let mut p = Project::new();
            let mut before = p.clone();
            p.mixer.tracks[0].pan = -0.5;
            let first = Change::between(&before, &p);
            before = p.clone();
            p.mixer.tracks[0].name = "Mixed edit".into();
            p.arrangement.clips.push(pattern_clip(2, 0));
            let mixed = Change::between(&before, &p);
            before = p.clone();
            p.mixer.tracks[0].pan = 0.5;
            let last = Change::between(&before, &p);
            let usable_order = if undo { last.order } else { first.order };
            let mut history = ArrangementHistory::default();
            if undo {
                history.undo = vec![first, mixed, last];
            } else {
                history.redo = vec![last, mixed, first];
            }

            history.forget_pattern(0, None, &mut HashMap::new());
            let stack = if undo { &history.undo } else { &history.redo };
            assert_eq!(
                stack.iter().map(|step| step.order).collect::<Vec<_>>(),
                [usable_order]
            );
        }
    }

    #[test]
    fn deleting_pattern_drops_shifted_slots_and_releases_history_bytes() {
        let mut p = Project::new();
        p.arrangement.tracks.push(PlaylistTrack {
            id: 1,
            name: "Playlist".into(),
            color: 0,
            mute: false,
        });
        p.patterns.push(Pattern::new(2, "Deleted"));
        let mut before = p.clone();
        p.patterns.push(Pattern::new(3, "Shifted"));
        let shifted = Change::between(&before, &p);
        before = p.clone();
        p.arrangement.clips.push(pattern_clip(4, 0));
        let other_clip = Change::between(&before, &p);
        let other_order = other_clip.order;
        let mut histories = Histories {
            current: Default::default(),
            patterns: Default::default(),
            arrangement: ArrangementHistory {
                undo: vec![shifted, other_clip],
                ..Default::default()
            },
            trim: Default::default(),
            floors: HashMap::from([(3, (17, 4))]),
            protected: Default::default(),
        };
        let base = Histories::usage_of(
            &histories.current,
            &histories.patterns,
            &ArrangementHistory::default(),
            &HashMap::new(),
            &histories.trim,
        )
        .2;

        histories
            .arrangement
            .forget_pattern(2, Some(1), &mut histories.floors);

        assert!(histories.floors.is_empty());
        assert_eq!(histories.arrangement.undo.len(), 1);
        assert!(histories.arrangement.redo.is_empty());
        let retained = &histories.arrangement.undo[0];
        assert_eq!(retained.order, other_order);
        assert_eq!(
            histories.usage(),
            (
                1,
                0,
                base + std::mem::size_of::<Change>() + retained.bytes(&mut HashMap::new())
            )
        );
        p.patterns.remove(1);
        retained.apply(&mut p, true);
        assert!(p.arrangement.clips.is_empty());
        retained.apply(&mut p, false);
        assert_eq!(p.arrangement.clips[0].content, pattern_clip(4, 0).content);
        assert_eq!(p.patterns[1].id, 3);
    }

    #[test]
    fn maximum_shape_geometry_delta_shares_curve_payloads_and_has_bounded_retention() {
        let mut p = Project::new();
        p.arrangement.tracks.push(PlaylistTrack {
            id: 1,
            name: "T".into(),
            color: 0,
            mute: false,
        });
        let points: CurvePoints = (0..16000)
            .map(|n| AutomationPoint {
                tick: n,
                value: 0.5,
                mode: SegmentMode::Hold,
            })
            .collect::<Vec<_>>()
            .into();
        for i in 0..100000 {
            p.arrangement.clips.push(Clip {
                id: i + 2,
                track: 1,
                name: String::new(),
                start_tick: i as u64,
                muted: true,
                content: if i < 62 {
                    ClipContent::Automation {
                        target: AutomationTarget::ChannelPan { channel: 500000 },
                        priority: i + 200000,
                        offset_tick: 0,
                        length_ticks: 1,
                        points: points.clone(),
                    }
                } else {
                    ClipContent::Pattern {
                        pattern: 0,
                        offset_tick: 0,
                        length_ticks: 1,
                    }
                },
            });
        }
        let began = std::time::Instant::now();
        let mut candidate = p.clone();
        candidate.arrangement.clips[0].start_tick += 1;
        let change = Change::between(&p, &candidate);
        assert_eq!(change.clips.len(), 1);
        let mut allocations = HashMap::new();
        let bytes = change.bytes(&mut allocations) + allocations.values().sum::<usize>();
        assert_eq!(allocations.len(), 1);
        assert!(bytes < 400000);
        let ClipContent::Automation { points: after, .. } = &candidate.arrangement.clips[0].content
        else {
            panic!()
        };
        assert!(points.shares_allocation(after));
        eprintln!(
            "scale probe: 100000 clips, 992000 supplied points, one geometry candidate/delta {:?}, retained {} bytes",
            began.elapsed(),
            bytes
        );
    }
}
