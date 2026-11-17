//! Control-thread compilation of linked pattern clips. Never call from the audio callback.
use crate::{ChannelId, PatternData, STEP_TICKS};
use std::fmt;

pub const MAX_SONG_TICKS: u64 = 38_400_000;
pub const MAX_PATTERN_CLIPS: usize = 100_000;
pub const MAX_SONG_NOTES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatternClip {
    pub id: u32,
    pub pattern: u32,
    pub start: u64,
    pub length: u64,
    /// Source phase; offsets beyond one pattern are reduced modulo its length.
    pub offset: u32,
    pub muted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SongNote {
    pub tick: u64,
    pub length: u64,
    pub channel: ChannelId,
    pub key: u8,
    pub velocity: f32,
    pub slide: bool,
    pub clip_id: u32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementData {
    /// Maximum end of the supplied pattern clips, including muted clips.
    /// Note-only extent: callers compute the all-clip song tick/frame horizon separately.
    pub length: u64,
    pub notes: Vec<SongNote>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArrangementError {
    TooManyClips,
    DuplicatePattern(u32),
    DuplicateClip(u32),
    UnknownPattern(u32),
    InvalidPattern(u32),
    InvalidNote { pattern: u32, index: usize },
    InvalidClip(u32),
    TooManyNotes { limit: usize },
    AllocationFailed,
}
impl fmt::Display for ArrangementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "arrangement compilation refused: {self:?}")
    }
}
impl std::error::Error for ArrangementError {}

fn reserved<T>(count: usize) -> Result<Vec<T>, ArrangementError> {
    let mut v = Vec::new();
    v.try_reserve_exact(count)
        .map_err(|_| ArrangementError::AllocationFailed)?;
    Ok(v)
}

/// Validates every supplied pattern and clip, including unused/muted sources, before
/// expanding. The output budget is the smaller of `max_notes` and [`MAX_SONG_NOTES`].
/// Pattern notes must start inside their source, have positive duration at most 256 steps,
/// MIDI keys and finite velocities in 0..=1. Long releases may cross source-loop edges.
/// IDs are opaque (zero is valid); channel existence is the caller's responsibility.
pub fn compile_pattern_clips(
    patterns: &[(u32, PatternData)],
    clips: &[PatternClip],
    max_notes: usize,
) -> Result<ArrangementData, ArrangementError> {
    if clips.len() > MAX_PATTERN_CLIPS {
        return Err(ArrangementError::TooManyClips);
    }
    let mut sources = reserved(patterns.len())?;
    for (id, p) in patterns {
        if !(1..=256 * STEP_TICKS).contains(&p.length) {
            return Err(ArrangementError::InvalidPattern(*id));
        }
        for (index, n) in p.notes.iter().enumerate() {
            if n.tick >= p.length
                || n.length == 0
                || n.length > 256 * STEP_TICKS
                || n.key > 127
                || !n.velocity.is_finite()
                || !(0.0..=1.0).contains(&n.velocity)
            {
                return Err(ArrangementError::InvalidNote {
                    pattern: *id,
                    index,
                });
            }
        }
        sources.push((*id, p));
    }
    sources.sort_unstable_by_key(|(id, _)| *id);
    for pair in sources.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(ArrangementError::DuplicatePattern(pair[0].0));
        }
    }
    let mut ids = reserved(clips.len())?;
    let mut length = 0;
    for c in clips {
        let end = c
            .start
            .checked_add(c.length)
            .ok_or(ArrangementError::InvalidClip(c.id))?;
        if c.length == 0 || end > MAX_SONG_TICKS {
            return Err(ArrangementError::InvalidClip(c.id));
        }
        if sources
            .binary_search_by_key(&c.pattern, |(id, _)| *id)
            .is_err()
        {
            return Err(ArrangementError::UnknownPattern(c.pattern));
        }
        length = length.max(end);
        ids.push(c.id);
    }
    ids.sort_unstable();
    for pair in ids.windows(2) {
        if pair[0] == pair[1] {
            return Err(ArrangementError::DuplicateClip(pair[0]));
        }
    }
    let limit = max_notes.min(MAX_SONG_NOTES);
    let mut count = 0usize;
    for c in clips.iter().filter(|c| !c.muted) {
        let p = sources[sources
            .binary_search_by_key(&c.pattern, |(id, _)| *id)
            .unwrap()]
        .1;
        let period = u64::from(p.length);
        for n in &p.notes {
            let first = (u64::from(n.tick) + period - u64::from(c.offset) % period) % period;
            if first < c.length {
                let occurrences = 1 + (c.length - 1 - first) / period;
                count = count
                    .checked_add(
                        usize::try_from(occurrences)
                            .map_err(|_| ArrangementError::TooManyNotes { limit })?,
                    )
                    .filter(|n| *n <= limit)
                    .ok_or(ArrangementError::TooManyNotes { limit })?;
            }
        }
    }
    // Carry the original traversal ordinal so an allocation-free unstable sort still
    // preserves exact ties, independent of the sorting algorithm or platform.
    let mut expanded = reserved(count)?;
    for c in clips.iter().filter(|c| !c.muted) {
        let p = sources[sources
            .binary_search_by_key(&c.pattern, |(id, _)| *id)
            .unwrap()]
        .1;
        let period = u64::from(p.length);
        for n in &p.notes {
            let mut local = (u64::from(n.tick) + period - u64::from(c.offset) % period) % period;
            while local < c.length {
                let ordinal = expanded.len();
                expanded.push((
                    SongNote {
                        tick: c.start + local,
                        length: u64::from(n.length).min(c.length - local),
                        channel: n.channel,
                        key: n.key,
                        velocity: n.velocity,
                        slide: n.slide,
                        clip_id: c.id,
                    },
                    ordinal,
                ));
                local = local
                    .checked_add(period)
                    .ok_or(ArrangementError::InvalidClip(c.id))?;
            }
        }
    }
    expanded.sort_unstable_by_key(|(n, ordinal)| {
        (n.tick, n.clip_id, n.channel, n.slide, n.key, *ordinal)
    });
    let mut notes = reserved(count)?;
    notes.extend(expanded.into_iter().map(|(n, _)| n));
    Ok(ArrangementData { length, notes })
}
