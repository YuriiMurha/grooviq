//! Control-built winner intervals; rendering performs two binary searches per target.
use crate::{ChannelId, ChannelSetup, ControlError, InstrumentKind, MAX_SONG_TICKS};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationSegment {
    Linear,
    Hold,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomationPoint {
    pub tick: u32,
    pub value: f32,
    pub segment: AutomationSegment,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomationTarget {
    ChannelVolume(ChannelId),
    ChannelPan(ChannelId),
    InstrumentParam {
        channel: ChannelId,
        instrument: InstrumentKind,
        param: u32,
    },
}
impl AutomationTarget {
    pub fn channel(self) -> ChannelId {
        match self {
            Self::ChannelVolume(c)
            | Self::ChannelPan(c)
            | Self::InstrumentParam { channel: c, .. } => c,
        }
    }
    fn key(self) -> (u32, u32, u32, u32) {
        match self {
            Self::ChannelVolume(c) => (c.0, 0, 0, 0),
            Self::ChannelPan(c) => (c.0, 1, 0, 0),
            Self::InstrumentParam {
                channel,
                instrument,
                param,
            } => (channel.0, 2, instrument as u32, param),
        }
    }
    fn range(self, channels: &[ChannelSetup]) -> Option<(f32, f32)> {
        let c = channels.iter().find(|c| c.id == self.channel())?;
        match self {
            Self::ChannelVolume(_) => Some((0.0, 2.0)),
            Self::ChannelPan(_) => Some((-1.0, 1.0)),
            Self::InstrumentParam {
                instrument, param, ..
            } if c.instrument.kind() == instrument => instrument
                .spec()
                .get(param as usize)
                .map(|&(a, b, _)| (a, b)),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationClip {
    pub target: AutomationTarget,
    pub priority: u32,
    pub start_tick: u64,
    pub offset_tick: u32,
    pub length_ticks: u64,
    pub points: Vec<AutomationPoint>,
}
#[derive(Clone, Debug, PartialEq)]
struct Winner {
    start: u64,
    end: u64,
    clip: usize,
}
#[derive(Clone, Debug, PartialEq)]
struct Lane {
    target: AutomationTarget,
    winners: Vec<Winner>,
}
#[derive(Clone, Debug, Default, PartialEq)]
struct Bank {
    clips: Vec<AutomationClip>,
    lanes: Vec<Lane>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutomationSchedule(Arc<Bank>);
impl AutomationSchedule {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.lanes.is_empty()
    }

    /// Export prefix: preserve source-local points/priority while ending activity at cut.
    pub(crate) fn before_tick(
        &self,
        end: u64,
        channels: &[ChannelSetup],
    ) -> Result<Self, ControlError> {
        let clips = self
            .0
            .clips
            .iter()
            .filter(|c| c.start_tick < end)
            .cloned()
            .map(|mut c| {
                c.length_ticks = c.length_ticks.min(end - c.start_tick);
                c
            })
            .collect();
        Self::build(clips, channels)
    }

    pub fn build(
        mut clips: Vec<AutomationClip>,
        channels: &[ChannelSetup],
    ) -> Result<Self, ControlError> {
        if clips.len() > 100_000 || clips.iter().map(|c| c.points.len()).sum::<usize>() > 1_000_000
        {
            return Err(ControlError::Rejected);
        }
        let mut lanes: Vec<Lane> = Vec::new();
        for c in &mut clips {
            let (min, max) = c.target.range(channels).ok_or(ControlError::Rejected)?;
            if c.length_ticks == 0
                || c.start_tick
                    .checked_add(c.length_ticks)
                    .is_none_or(|e| e > MAX_SONG_TICKS)
                || u64::from(c.offset_tick)
                    .checked_add(c.length_ticks)
                    .is_none_or(|e| e > MAX_SONG_TICKS)
                || c.points.len() > 16_384
                || c.points.iter().any(|p| {
                    u64::from(p.tick) > MAX_SONG_TICKS
                        || !p.value.is_finite()
                        || p.value < min
                        || p.value > max
                })
            {
                return Err(ControlError::Rejected);
            }
            // Stable sorting retains supplied order within a tick; overwrite selects the last.
            c.points.sort_by_key(|p| p.tick);
            let mut normalized: Vec<AutomationPoint> = Vec::with_capacity(c.points.len());
            for p in c.points.drain(..) {
                if normalized.last().is_some_and(|q| q.tick == p.tick) {
                    *normalized.last_mut().unwrap() = p;
                } else {
                    normalized.push(p)
                }
            }
            c.points = normalized;
        }
        let mut groups = BTreeMap::new();
        for (i, c) in clips
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.points.is_empty())
        {
            let entry = groups
                .entry(c.target.key())
                .or_insert_with(|| (c.target, Vec::new()));
            entry.1.push((c.start_tick, true, i));
            entry.1.push((c.start_tick + c.length_ticks, false, i));
        }
        for (_, (target, mut edges)) in groups {
            let mut lane = Lane {
                target,
                winners: Vec::new(),
            };
            edges.sort_unstable();
            let mut active = BTreeMap::new();
            let mut previous = 0;
            for (tick, on, i) in edges {
                if tick > previous
                    && let Some((_, &clip)) = active.last_key_value()
                {
                    lane.winners.push(Winner {
                        start: previous,
                        end: tick,
                        clip,
                    });
                }
                let priority = clips[i].priority;
                if on {
                    if active.insert(priority, i).is_some() {
                        return Err(ControlError::Rejected);
                    }
                } else {
                    active.remove(&priority);
                }
                previous = tick;
            }
            lanes.push(lane);
        }
        Ok(Self(Arc::new(Bank { clips, lanes })))
    }
    pub fn validate(&self, channels: &[ChannelSetup]) -> Result<(), ControlError> {
        if self
            .0
            .lanes
            .iter()
            .any(|l| l.target.range(channels).is_none())
        {
            Err(ControlError::Rejected)
        } else {
            Ok(())
        }
    }
    pub(crate) fn references(&self, id: ChannelId) -> bool {
        self.0.lanes.iter().any(|l| l.target.channel() == id)
    }
    pub fn end_tick(&self) -> u64 {
        self.0
            .clips
            .iter()
            .map(|c| c.start_tick + c.length_ticks)
            .max()
            .unwrap_or(0)
    }
    pub fn value_at(&self, target: AutomationTarget, tick: f64) -> Option<f32> {
        let lane = &self.0.lanes[self
            .0
            .lanes
            .binary_search_by_key(&target.key(), |l| l.target.key())
            .ok()?];
        self.lane_value(lane, tick)
    }
    fn lane_value(&self, lane: &Lane, tick: f64) -> Option<f32> {
        if !tick.is_finite() || tick < 0.0 {
            return None;
        }
        let i = lane
            .winners
            .partition_point(|w| w.start as f64 <= tick)
            .checked_sub(1)?;
        let w = &lane.winners[i];
        if tick >= w.end as f64 {
            return None;
        }
        let c = &self.0.clips[w.clip];
        let source = tick - c.start_tick as f64 + f64::from(c.offset_tick);
        let i = c.points.partition_point(|p| f64::from(p.tick) <= source);
        if i == 0 {
            return Some(c.points[0].value);
        }
        let a = c.points[i - 1];
        if i == c.points.len() || a.segment == AutomationSegment::Hold {
            return Some(a.value);
        }
        let b = c.points[i];
        let f = (source - f64::from(a.tick)) / f64::from(b.tick - a.tick);
        Some((f64::from(a.value) + (f64::from(b.value) - f64::from(a.value)) * f) as f32)
    }
}
