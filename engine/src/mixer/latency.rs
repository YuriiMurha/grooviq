//! Off-thread delay-compensation planning from actual per-insert processing latency.
//! This is planning only: nothing here allocates delay rings, renders or retimes sources.
//!
//! Timing rule, per track in compiled level order: every audible input (main, send,
//! source) is aligned to the latest audible arrival. Then, before each insert in chain
//! order, the audible chain and every detector feed naming that insert are aligned to their
//! joint latest arrival, and the insert's actual latency is added. Pre- and post-fader taps
//! share the chain's final arrival. Sources arrive at zero. Gain, including zero, never
//! changes timing: a silent edge is still a dependency.
//!
//! Declared `Insert::latency_frames` are upper bounds used only to refuse a snapshot.

use super::{CompiledMixer, InputId, InsertId, MASTER, MixerTrackId, PlanError, SourceId};
use std::mem::size_of;

/// One prepared device's actual compensable latency, queried after its final parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActualLatency {
    pub insert: InsertId,
    pub frames: u32,
}

/// Caller policy for one plan. All bounds are inclusive; there is no per-delay cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdcBudget {
    /// Retained delay and track-arrival descriptors.
    pub descriptors: u64,
    /// Prospective stereo history samples (both channels) for every nonzero delay.
    pub ring_samples: u64,
    /// Retained descriptors, the planner's temporary work and prospective ring bytes.
    pub memory_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdcLimit {
    Descriptors,
    RingSamples,
    Memory,
}

/// Measured from actual capacities. Ring figures are what a later integer delay needs as a
/// minimum (one `f32` per frame per channel); the plan itself allocates no ring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PdcResources {
    pub descriptors: u64,
    pub descriptor_bytes: u64,
    /// Peak temporary storage, freed before the plan is returned.
    pub work_bytes: u64,
    pub ring_samples: u64,
    pub ring_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyError {
    Plan(PlanError),
    MissingLatency(InsertId),
    DuplicateLatency(InsertId),
    UnknownLatency(InsertId),
    AboveDeclared {
        insert: InsertId,
        actual: u32,
        declared: u32,
    },
    Overflow,
    Budget {
        resource: PdcLimit,
        required: u64,
        limit: u64,
    },
    Allocation,
}

/// Where a compensation delay sits. Zero-frame entries are kept so every logical point
/// has exactly one well-defined descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PdcPoint {
    /// An audible input (main, send or source) before the track's gather.
    Audible { track: MixerTrackId, input: InputId },
    /// The audible chain immediately before this insert processes.
    Stage {
        track: MixerTrackId,
        insert: InsertId,
    },
    /// A sidechain feed into this insert's detector.
    Detector {
        track: MixerTrackId,
        insert: InsertId,
        input: InputId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdcDelay {
    pub at: PdcPoint,
    pub frames: u64,
}

/// Arrival in frames after sources. `gather` is the aligned audible sum; `tap` is both the
/// pre- and post-fader tap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackArrival {
    pub track: MixerTrackId,
    pub gather: u64,
    pub tap: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LatencyPlan {
    /// Execution order: per track (levels order), audible inputs in canonical input order,
    /// then per insert in chain order its stage delay and its detector feeds.
    pub delays: Vec<PdcDelay>,
    /// Execution order.
    pub tracks: Vec<TrackArrival>,
    /// Master's tap arrival: the total output processing latency.
    pub master_frames: u64,
    pub resources: PdcResources,
}

fn add(a: u64, b: u64) -> Result<u64, LatencyError> {
    a.checked_add(b).ok_or(LatencyError::Overflow)
}
fn mul(a: u64, b: u64) -> Result<u64, LatencyError> {
    a.checked_mul(b).ok_or(LatencyError::Overflow)
}
fn count(n: usize) -> Result<u64, LatencyError> {
    u64::try_from(n).map_err(|_| LatencyError::Overflow)
}
fn index(n: usize) -> Result<u32, LatencyError> {
    u32::try_from(n).map_err(|_| LatencyError::Overflow)
}
fn bytes_of<T>(n: u64) -> Result<u64, LatencyError> {
    mul(n, count(size_of::<T>())?)
}
fn held<T>(v: &Vec<T>) -> Result<u64, LatencyError> {
    bytes_of::<T>(count(v.capacity())?)
}
fn reserved<T>(n: u64) -> Result<Vec<T>, LatencyError> {
    let n = usize::try_from(n).map_err(|_| LatencyError::Overflow)?;
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| LatencyError::Allocation)?;
    Ok(v)
}
fn limit(resource: PdcLimit, required: u64, limit: u64) -> Result<(), LatencyError> {
    if required > limit {
        Err(LatencyError::Budget {
            resource,
            required,
            limit,
        })
    } else {
        Ok(())
    }
}
fn plan(e: PlanError) -> LatencyError {
    LatencyError::Plan(e)
}

#[derive(Clone, Copy)]
struct InsertEntry {
    id: InsertId,
    track: u32,
    /// Chain position on its track.
    pos: u32,
    declared: u32,
    actual: u32,
}

/// Temporary storage, all reserved up front from admitted counts.
struct Work {
    /// (track id, index into `compiled.tracks`), sorted by id.
    tracks: Vec<(MixerTrackId, u32)>,
    /// Per `compiled.tracks` index: level, then tap arrival.
    level: Vec<u32>,
    arrival: Vec<u64>,
    inserts: Vec<InsertEntry>,
    snapshot: Vec<ActualLatency>,
    sources: Vec<SourceId>,
    /// One track's detector feeds: (chain position, input index), reused per track.
    feeds: Vec<(u32, u32)>,
    /// One track's per-chain-position detector arrival, reused per track.
    detector: Vec<Option<u64>>,
}

impl Work {
    fn bytes(&self) -> Result<u64, LatencyError> {
        let mut total = held(&self.tracks)?;
        total = add(total, held(&self.level)?)?;
        total = add(total, held(&self.arrival)?)?;
        total = add(total, held(&self.inserts)?)?;
        total = add(total, held(&self.snapshot)?)?;
        total = add(total, held(&self.sources)?)?;
        total = add(total, held(&self.feeds)?)?;
        add(total, held(&self.detector)?)
    }
}

struct Counts {
    tracks: u64,
    inserts: u64,
    audible: u64,
    sidechains: u64,
    sources: u64,
    max_inputs: u64,
    max_inserts: u64,
    snapshot: u64,
}

fn counts(compiled: &CompiledMixer, snapshot: &[ActualLatency]) -> Result<Counts, LatencyError> {
    let mut c = Counts {
        tracks: count(compiled.tracks.len())?,
        inserts: 0,
        audible: 0,
        sidechains: 0,
        sources: 0,
        max_inputs: 0,
        max_inserts: 0,
        snapshot: count(snapshot.len())?,
    };
    for t in &compiled.tracks {
        let inserts = count(t.track.inserts.len())?;
        c.inserts = add(c.inserts, inserts)?;
        c.max_inserts = c.max_inserts.max(inserts);
        c.max_inputs = c.max_inputs.max(count(t.inputs.len())?);
        for input in &t.inputs {
            match input.id {
                InputId::Sidechain { .. } => c.sidechains = add(c.sidechains, 1)?,
                InputId::Source(_) => {
                    c.sources = add(c.sources, 1)?;
                    c.audible = add(c.audible, 1)?;
                }
                InputId::Main(_) | InputId::Send(_) => c.audible = add(c.audible, 1)?,
            }
        }
    }
    Ok(c)
}

fn work_bytes(c: &Counts) -> Result<u64, LatencyError> {
    let mut total = bytes_of::<(MixerTrackId, u32)>(c.tracks)?;
    total = add(total, bytes_of::<u32>(c.tracks)?)?;
    total = add(total, bytes_of::<u64>(c.tracks)?)?;
    total = add(total, bytes_of::<InsertEntry>(c.inserts)?)?;
    total = add(total, bytes_of::<ActualLatency>(c.snapshot)?)?;
    total = add(total, bytes_of::<SourceId>(c.sources)?)?;
    total = add(total, bytes_of::<(u32, u32)>(c.max_inputs)?)?;
    add(total, bytes_of::<Option<u64>>(c.max_inserts)?)
}

/// Plans compensation for `compiled` from one actual latency per insert. Every refusal is
/// typed and happens before a plan is returned; the order of `snapshot` never matters.
/// Off-thread only: it allocates. Traversal is iterative over the compiled levels.
pub fn plan_latency(
    compiled: &CompiledMixer,
    snapshot: &[ActualLatency],
    budget: PdcBudget,
) -> Result<LatencyPlan, LatencyError> {
    // Admission from public lengths, before any allocation.
    let c = counts(compiled, snapshot)?;
    let n_delays = add(add(c.audible, c.inserts)?, c.sidechains)?;
    let descriptors = add(n_delays, c.tracks)?;
    limit(PdcLimit::Descriptors, descriptors, budget.descriptors)?;
    let wanted = add(
        work_bytes(&c)?,
        add(
            bytes_of::<PdcDelay>(n_delays)?,
            bytes_of::<TrackArrival>(c.tracks)?,
        )?,
    )?;
    limit(PdcLimit::Memory, wanted, budget.memory_bytes)?;

    let mut w = Work {
        tracks: reserved(c.tracks)?,
        level: reserved(c.tracks)?,
        arrival: reserved(c.tracks)?,
        inserts: reserved(c.inserts)?,
        snapshot: reserved(c.snapshot)?,
        sources: reserved(c.sources)?,
        feeds: reserved(c.max_inputs)?,
        detector: reserved(c.max_inserts)?,
    };
    let mut delays: Vec<PdcDelay> = reserved(n_delays)?;
    let mut arrivals: Vec<TrackArrival> = reserved(c.tracks)?;
    let work = w.bytes()?;
    let descriptor_bytes = add(held(&delays)?, held(&arrivals)?)?;
    limit(
        PdcLimit::Memory,
        add(work, descriptor_bytes)?,
        budget.memory_bytes,
    )?;

    // Track identities.
    for (i, t) in compiled.tracks.iter().enumerate() {
        w.tracks.push((t.track.id, index(i)?));
    }
    w.tracks.sort_unstable_by_key(|(id, _)| *id);
    for pair in w.tracks.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(plan(PlanError::DuplicateTrack(pair[0].0)));
        }
    }
    let find_track = |tracks: &[(MixerTrackId, u32)], id| {
        tracks
            .binary_search_by_key(&id, |(key, _)| *key)
            .map(|at| tracks[at].1 as usize)
            .map_err(|_| plan(PlanError::UnknownTrack(id)))
    };

    // Levels: each track exactly once, master alone and last.
    w.level.resize(w.tracks.len(), u32::MAX);
    for (level, ids) in compiled.levels.iter().enumerate() {
        let level = index(level)?;
        for &id in ids {
            let at = find_track(&w.tracks, id)?;
            if w.level[at] != u32::MAX {
                return Err(plan(PlanError::DuplicateTrack(id)));
            }
            w.level[at] = level;
        }
    }
    for (at, t) in compiled.tracks.iter().enumerate() {
        if w.level[at] == u32::MAX {
            return Err(plan(PlanError::UnscheduledTrack(t.track.id)));
        }
    }
    if compiled.levels.last().map(Vec::as_slice) != Some(&[MASTER][..]) {
        return Err(plan(PlanError::MasterNotLast));
    }

    // Insert identities and their declared bounds.
    for (at, t) in compiled.tracks.iter().enumerate() {
        for (pos, i) in t.track.inserts.iter().enumerate() {
            w.inserts.push(InsertEntry {
                id: i.id,
                track: index(at)?,
                pos: index(pos)?,
                declared: i.latency_frames,
                actual: 0,
            });
        }
    }
    w.inserts.sort_unstable_by_key(|e| e.id);
    for pair in w.inserts.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(plan(PlanError::DuplicateInsert(pair[0].id)));
        }
    }
    let find_insert =
        |inserts: &[InsertEntry], id: InsertId| inserts.binary_search_by_key(&id, |e| e.id).ok();

    // Snapshot: sorted so every refusal is independent of caller order.
    w.snapshot.extend_from_slice(snapshot);
    w.snapshot.sort_unstable_by_key(|s| (s.insert, s.frames));
    for pair in w.snapshot.windows(2) {
        if pair[0].insert == pair[1].insert {
            return Err(LatencyError::DuplicateLatency(pair[0].insert));
        }
    }
    let mut seen = 0usize;
    for s in &w.snapshot {
        let at = find_insert(&w.inserts, s.insert).ok_or(LatencyError::UnknownLatency(s.insert))?;
        let e = &mut w.inserts[at];
        if s.frames > e.declared {
            return Err(LatencyError::AboveDeclared {
                insert: s.insert,
                actual: s.frames,
                declared: e.declared,
            });
        }
        e.actual = s.frames;
        seen += 1;
    }
    if seen != w.inserts.len() {
        // Both lists are sorted and the snapshot holds only known, unique inserts.
        let mut given = w.snapshot.iter().map(|s| s.insert).peekable();
        for e in &w.inserts {
            if given.peek() == Some(&e.id) {
                given.next();
            } else {
                return Err(LatencyError::MissingLatency(e.id));
            }
        }
    }

    // Inputs: canonical, consistent and strictly earlier; sources unique.
    for t in &compiled.tracks {
        let id = t.track.id;
        let level = w.level[find_track(&w.tracks, id)?];
        for pair in t.inputs.windows(2) {
            if pair[0].id >= pair[1].id {
                return Err(plan(PlanError::UnsortedInputs(id)));
            }
        }
        for input in &t.inputs {
            if !input.gain.is_finite() {
                return Err(plan(PlanError::MalformedInput(id)));
            }
            let from = match input.id {
                InputId::Source(source) => {
                    if input.from.is_some() {
                        return Err(plan(PlanError::MalformedInput(id)));
                    }
                    w.sources.push(source);
                    continue;
                }
                InputId::Main(from) => (input.from == Some(from)).then_some(from),
                InputId::Send(_) => input.from,
                InputId::Sidechain { from, tap, .. } => {
                    (input.from == Some(from) && input.tap == tap).then_some(from)
                }
            };
            let from = from.ok_or(plan(PlanError::MalformedInput(id)))?;
            if w.level[find_track(&w.tracks, from)?] >= level {
                return Err(plan(PlanError::NotEarlier(from)));
            }
            if let InputId::Sidechain { insert, .. } = input.id {
                let owner = find_insert(&w.inserts, insert)
                    .map(|at| compiled.tracks[w.inserts[at].track as usize].track.id);
                if owner != Some(id) {
                    return Err(plan(PlanError::ForeignInsert(insert)));
                }
            }
        }
    }
    w.sources.sort_unstable();
    for pair in w.sources.windows(2) {
        if pair[0] == pair[1] {
            return Err(plan(PlanError::DuplicateSource(pair[0])));
        }
    }

    // Timing, level by level. Every input is from an earlier level, so its arrival is final.
    w.arrival.resize(w.tracks.len(), 0);
    for ids in &compiled.levels {
        for &id in ids {
            let at = find_track(&w.tracks, id)?;
            let t = &compiled.tracks[at];
            let arrival_of = |input: &super::Input| -> Result<u64, LatencyError> {
                match input.from {
                    None => Ok(0),
                    Some(from) => Ok(w.arrival[find_track(&w.tracks, from)?]),
                }
            };
            let mut gather = 0;
            for input in &t.inputs {
                if !matches!(input.id, InputId::Sidechain { .. }) {
                    gather = gather.max(arrival_of(input)?);
                }
            }
            for input in &t.inputs {
                if !matches!(input.id, InputId::Sidechain { .. }) {
                    delays.push(PdcDelay {
                        at: PdcPoint::Audible {
                            track: id,
                            input: input.id,
                        },
                        frames: gather - arrival_of(input)?,
                    });
                }
            }

            // Detector feeds grouped by chain position, canonical order within a position.
            w.feeds.clear();
            w.detector.clear();
            w.detector.resize(t.track.inserts.len(), None);
            for (n, input) in t.inputs.iter().enumerate() {
                if let InputId::Sidechain { insert, .. } = input.id {
                    // Validated above: the insert exists and is on this track.
                    let e = find_insert(&w.inserts, insert)
                        .ok_or(plan(PlanError::ForeignInsert(insert)))?;
                    let pos = w.inserts[e].pos;
                    let arrival = arrival_of(input)?;
                    let slot = &mut w.detector[pos as usize];
                    *slot = Some(slot.map_or(arrival, |s| s.max(arrival)));
                    w.feeds.push((pos, index(n)?));
                }
            }
            // The input index breaks ties canonically without stable-sort scratch.
            w.feeds.sort_unstable();

            let mut chain = gather;
            let mut feed = 0;
            for (pos, insert) in t.track.inserts.iter().enumerate() {
                let aligned = chain.max(w.detector[pos].unwrap_or(0));
                delays.push(PdcDelay {
                    at: PdcPoint::Stage {
                        track: id,
                        insert: insert.id,
                    },
                    frames: aligned - chain,
                });
                let pos = index(pos)?;
                while let Some(&(p, n)) = w.feeds.get(feed)
                    && p == pos
                {
                    let input = &t.inputs[n as usize];
                    delays.push(PdcDelay {
                        at: PdcPoint::Detector {
                            track: id,
                            insert: insert.id,
                            input: input.id,
                        },
                        frames: aligned - arrival_of(input)?,
                    });
                    feed += 1;
                }
                let e = find_insert(&w.inserts, insert.id)
                    .ok_or(plan(PlanError::DuplicateInsert(insert.id)))?;
                chain = add(aligned, u64::from(w.inserts[e].actual))?;
            }
            w.arrival[at] = chain;
            arrivals.push(TrackArrival {
                track: id,
                gather,
                tap: chain,
            });
        }
    }
    let master_frames = w.arrival[find_track(&w.tracks, MASTER)?];

    let mut ring_samples = 0;
    for d in &delays {
        ring_samples = add(ring_samples, mul(d.frames, 2)?)?;
    }
    let ring_bytes = bytes_of::<f32>(ring_samples)?;
    limit(PdcLimit::RingSamples, ring_samples, budget.ring_samples)?;
    limit(
        PdcLimit::Memory,
        add(add(work, descriptor_bytes)?, ring_bytes)?,
        budget.memory_bytes,
    )?;
    Ok(LatencyPlan {
        delays,
        tracks: arrivals,
        master_frames,
        resources: PdcResources {
            descriptors,
            descriptor_bytes,
            work_bytes: work,
            ring_samples,
            ring_bytes,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_helpers_refuse_overflow() {
        assert_eq!(add(u64::MAX, 1), Err(LatencyError::Overflow));
        assert_eq!(mul(u64::MAX, 2), Err(LatencyError::Overflow));
        assert_eq!(bytes_of::<u64>(u64::MAX / 4), Err(LatencyError::Overflow));
        assert_eq!(index(usize::MAX), Err(LatencyError::Overflow));
        assert_eq!(add(u64::MAX - 1, 1), Ok(u64::MAX));
    }
}
