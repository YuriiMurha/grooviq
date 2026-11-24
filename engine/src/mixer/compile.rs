use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// This is the exact future gather order. Audible and sidechain inputs stay distinct.
/// Main outputs sort first by track, then sends by persistent send ID, then sidechains.
/// Sources sort by persistent source identity; no worker completion order is observable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputId {
    Main(MixerTrackId),
    Send(SendId),
    Sidechain {
        from: MixerTrackId,
        insert: InsertId,
        tap: Tap,
    },
    Source(SourceId),
}
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: InputId,
    pub from: Option<MixerTrackId>,
    pub tap: Tap,
    pub gain: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledTrack {
    pub track: Track,
    pub inputs: Vec<Input>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledMixer {
    /// Sorted by track identity. Input vectors are canonical, never arrival order.
    pub tracks: Vec<CompiledTrack>,
    /// Each level is sorted by track identity. Master is necessarily the final level.
    pub levels: Vec<Vec<MixerTrackId>>,
    pub resources: Resources,
}
fn add(a: u64, b: u64) -> Result<u64, CompileError> {
    a.checked_add(b).ok_or(CompileError::Overflow)
}
fn mul(a: u64, b: u64) -> Result<u64, CompileError> {
    a.checked_mul(b).ok_or(CompileError::Overflow)
}
fn limit(resource: Limit, required: u64, limit: u64) -> Result<(), CompileError> {
    if required > limit {
        Err(CompileError::Budget {
            resource,
            required,
            limit,
        })
    } else {
        Ok(())
    }
}
fn admit(m: &Mixer, b: Budget) -> Result<Resources, CompileError> {
    if b.block_frames == 0 {
        return Err(CompileError::InvalidBlockSize);
    }
    let mut nodes = add(m.tracks.len() as u64, m.sources.len() as u64)?;
    // Count implied main edges without building them.
    let mut edges = add(m.sends.len() as u64, m.sidechains.len() as u64)?;
    let mut latency = 0;
    let mut devices = 0;
    for t in &m.tracks {
        nodes = add(nodes, t.inserts.len() as u64)?;
        edges = add(edges, u64::from(t.output.is_some()))?;
        for i in &t.inserts {
            latency = add(latency, u64::from(i.latency_frames))?;
            devices = add(devices, i.memory_bytes)?;
        }
    }
    edges = add(edges, m.sources.len() as u64)?;
    limit(Limit::Nodes, nodes, b.nodes)?;
    limit(Limit::Edges, edges, b.edges)?;
    limit(Limit::Latency, latency, b.latency_frames)?;
    // Three stereo work buffers per node plus one per edge; no reuse assumed.
    let buffers = add(mul(nodes, 3)?, edges)?;
    limit(Limit::Buffers, buffers, b.buffers)?;
    // Every edge could require the full latency sum in compensation storage.
    // 1024 bytes per node/edge conservatively reserves compiler maps, adjacency,
    // levels, descriptor copies and input records, including allocation overhead.
    let memory = add(
        add(devices, mul(add(nodes, edges)?, 1024)?)?,
        add(
            mul(mul(buffers, u64::from(b.block_frames))?, 8)?,
            mul(mul(edges, latency)?, 8)?,
        )?,
    )?;
    limit(Limit::Memory, memory, b.memory_bytes)?;
    Ok(Resources {
        nodes,
        edges,
        buffers,
        latency_frames: latency,
        memory_bytes: memory,
    })
}
/// Iterative Kahn compilation: O((nodes + edges) log(nodes + edges)), bounded
/// by admission before allocating graph storage; no recursive untrusted traversal.
pub fn compile(m: &Mixer, budget: Budget) -> Result<CompiledMixer, CompileError> {
    let resources = admit(m, budget)?;
    let mut tracks = BTreeMap::new();
    let mut inserts = BTreeMap::new();
    for t in &m.tracks {
        if tracks
            .insert(
                t.id,
                CompiledTrack {
                    track: t.clone(),
                    inputs: Vec::new(),
                },
            )
            .is_some()
        {
            return Err(CompileError::DuplicateTrack(t.id));
        }
        for i in &t.inserts {
            if inserts.insert(i.id, t.id).is_some() {
                return Err(CompileError::DuplicateInsert(i.id));
            }
        }
        if (t.id == MASTER) != t.output.is_none() {
            return Err(CompileError::InvalidOutput(t.id));
        }
    }
    if !tracks.contains_key(&MASTER) {
        return Err(CompileError::MissingMaster);
    }
    let mut adjacent: BTreeMap<_, Vec<_>> = tracks.keys().map(|id| (*id, Vec::new())).collect();
    let mut degrees: BTreeMap<_, usize> = tracks.keys().map(|id| (*id, 0)).collect();
    let mut edge = |from, to, id, tap, gain| -> Result<(), CompileError> {
        if !tracks.contains_key(&from) {
            return Err(CompileError::DanglingTrack(from));
        }
        let dest = tracks.get_mut(&to).ok_or(CompileError::DanglingTrack(to))?;
        if from == to {
            return Err(CompileError::SelfEdge(from));
        }
        if from == MASTER {
            return Err(CompileError::MasterOutgoing);
        }
        dest.inputs.push(Input {
            id,
            from: Some(from),
            tap,
            gain,
        });
        adjacent.get_mut(&from).unwrap().push(to);
        *degrees.get_mut(&to).unwrap() += 1;
        Ok(())
    };
    for t in &m.tracks {
        if let Some(to) = t.output {
            edge(t.id, to, InputId::Main(t.id), Tap::PostFader, 1.0)?;
        }
    }
    let mut sends = BTreeSet::new();
    for s in &m.sends {
        if !sends.insert(s.id) {
            return Err(CompileError::DuplicateSend(s.id));
        }
        if !s.gain.is_finite() || !(0.0..=4.0).contains(&s.gain) {
            return Err(CompileError::InvalidGain(s.id));
        }
        edge(s.from, s.to, InputId::Send(s.id), s.tap, s.gain)?;
    }
    let mut sidechains = BTreeSet::new();
    for s in &m.sidechains {
        let to = *inserts
            .get(&s.to)
            .ok_or(CompileError::DanglingInsert(s.to))?;
        let id = InputId::Sidechain {
            from: s.from,
            insert: s.to,
            tap: s.tap,
        };
        if !sidechains.insert(id) {
            return Err(CompileError::DuplicateSidechain);
        }
        edge(s.from, to, id, s.tap, 1.0)?;
    }
    let mut sources = BTreeSet::new();
    for s in &m.sources {
        if !sources.insert(s.id) {
            return Err(CompileError::DuplicateSource(s.id));
        }
        tracks
            .get_mut(&s.destination)
            .ok_or(CompileError::DanglingTrack(s.destination))?
            .inputs
            .push(Input {
                id: InputId::Source(s.id),
                from: None,
                tap: Tap::PostFader,
                gain: 1.0,
            });
    }
    for t in tracks.values_mut() {
        t.inputs.sort_by_key(|i| i.id);
    }
    let mut ready: BTreeSet<_> = degrees
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut levels = Vec::new();
    let mut visited = 0;
    while !ready.is_empty() {
        let level: Vec<_> = ready.into_iter().collect();
        ready = BTreeSet::new();
        visited += level.len();
        for id in &level {
            for to in &adjacent[id] {
                let degree = degrees.get_mut(to).unwrap();
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(*to);
                }
            }
        }
        levels.push(level);
    }
    if visited != tracks.len() {
        return Err(CompileError::Cycle);
    }
    Ok(CompiledMixer {
        tracks: tracks.into_values().collect(),
        levels,
        resources,
    })
}
