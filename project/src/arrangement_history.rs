//! Retained changed entities only; immutable curves share allocation ownership with live data.
use crate::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(crate) struct Delta<T> {
    pub before: Option<(usize, T)>,
    pub after: Option<(usize, T)>,
}
#[derive(Clone, Debug)]
pub(crate) struct Change {
    pub clips: Vec<Delta<Clip>>,
    pub tracks: Vec<Delta<PlaylistTrack>>,
    pub patterns: Vec<Delta<Pattern>>,
    pub region: Option<(Option<LoopRegion>, Option<LoopRegion>)>,
    pub order: u64,
}
fn diff<T: Clone + PartialEq>(a: &[T], b: &[T], id: impl Fn(&T) -> u32) -> Vec<Delta<T>> {
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
fn apply<T: Clone>(list: &mut Vec<T>, deltas: &[Delta<T>], undo: bool, id: impl Fn(&T) -> u32) {
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
impl Change {
    pub fn between(a: &Project, b: &Project) -> Self {
        Self {
            clips: diff(&a.arrangement.clips, &b.arrangement.clips, |c| c.id),
            tracks: diff(&a.arrangement.tracks, &b.arrangement.tracks, |c| c.id),
            patterns: diff(&a.patterns, &b.patterns, |p| p.id),
            region: (a.arrangement.loop_region != b.arrangement.loop_region).then(|| {
                (
                    a.arrangement.loop_region.clone(),
                    b.arrangement.loop_region.clone(),
                )
            }),
            order: crate::undo::next_order(),
        }
    }
    pub fn apply(&self, p: &mut Project, undo: bool) {
        apply(&mut p.arrangement.clips, &self.clips, undo, |c| c.id);
        apply(&mut p.arrangement.tracks, &self.tracks, undo, |c| c.id);
        apply(&mut p.patterns, &self.patterns, undo, |p| p.id);
        if let Some((a, b)) = &self.region {
            p.arrangement.loop_region = if undo { a.clone() } else { b.clone() };
        }
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
            + self.clips.capacity() * std::mem::size_of::<Delta<Clip>>()
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
#[derive(Clone, Debug, Default)]
pub(crate) struct ArrangementHistory {
    pub undo: Vec<Change>,
    pub redo: Vec<Change>,
}
impl ArrangementHistory {
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
    pub floors: HashMap<u32, (u32, u32)>,
    pub protected: std::collections::HashSet<u64>,
}
impl Histories {
    pub fn clear_redo(&mut self) {
        self.current.clear_redo();
        for h in self.patterns.values_mut() {
            h.clear_redo();
        }
        self.arrangement.redo.clear();
    }
    pub fn usage(&self) -> (usize, usize, usize) {
        Self::usage_of(
            &self.current,
            &self.patterns,
            &self.arrangement,
            &self.floors,
        )
    }
    pub fn usage_of(
        current: &crate::undo::NoteHistory,
        patterns: &HashMap<u32, crate::undo::NoteHistory>,
        arrangement: &ArrangementHistory,
        floors: &HashMap<u32, (u32, u32)>,
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
        (steps, notes, bytes)
    }
    fn prune_floors(&mut self) {
        let ids: std::collections::HashSet<_> = self
            .arrangement
            .undo
            .iter()
            .chain(&self.arrangement.redo)
            .flat_map(|c| &c.patterns)
            .flat_map(|d| [&d.before, &d.after])
            .flatten()
            .map(|(_, p)| p.id)
            .collect();
        self.floors.retain(|id, _| ids.contains(id));
        self.floors.shrink_to_fit();
    }
    pub fn admit(&mut self, limits: HistoryLimits, current: u32) -> Result<(), String> {
        self.prune_floors();
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
                .min_by_key(|(o, _)| *o);
            match oldest {
                Some((order, None)) => {
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
            self.prune_floors();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
