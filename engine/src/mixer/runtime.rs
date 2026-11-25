//! Static serial renderer for a [`CompiledMixer`]. Everything owning (identity resolution,
//! buffer reservation, device binding) happens in [`PreparedMixer::prepare`] off-thread;
//! `render`, `reset` and `set_param` never allocate, free, lock, log or do I/O.
//!
//! Per node, in the compiled level order: audible inputs are summed in the compiled vector
//! order, the inserts run in chain order (each sidechain-carrying insert gets its own
//! detector sum, never audible), the result is the pre-fader tap and the fixed stereo
//! coefficients make the post-fader tap. Master runs last and its post-fader tap is the output.
//!
//! Fault convention (same as the legacy graph): a node whose pre- or post-fader block holds
//! NaN or infinity is zeroed and all its inserts are reset; healthy nodes keep running. That
//! is containment only: downstream nodes still hear the silenced block, and a device that
//! faults again is silenced again. No other recovery is promised.
//!
//! Dropping a `PreparedMixer` frees its devices and buffers: do it off the audio thread.

use super::insert::{InsertSlot, ParamError, PreparedInsert, find};
use super::{CompiledMixer, InputId, InsertId, MASTER, MixerTrackId, SourceId, Tap};
use grooviq_dsp::{HostTransport, MAX_BLOCK};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;

/// Fixed linear coefficients from the pre-fader to the post-fader tap. A track without an
/// entry is unity. No pan law, mute or solo is implied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackGain {
    pub track: MixerTrackId,
    pub left: f32,
    pub right: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrepareConfig {
    /// The rate every insert was prepared for; each render's transport must match it exactly.
    pub sample_rate: f32,
    /// Largest accepted render span, 1..=MAX_BLOCK.
    pub max_frames: usize,
    /// Inclusive limit for the kernel's own audio buffers and tables (not the devices).
    pub max_bytes: u64,
}

/// The kernel's own reserved storage, measured from actual capacities after reservation.
/// Caller-owned devices are excluded: their memory is not known here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelResources {
    pub audio_bytes: u64,
    pub table_bytes: u64,
}

/// One caller-owned source block, in [`PreparedMixer::source_order`] position.
#[derive(Clone, Copy, Debug)]
pub struct SourceBlock<'a> {
    pub id: SourceId,
    pub left: &'a [f32],
    pub right: &'a [f32],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    pub frames: u32,
    /// Nodes the finite guard zeroed and reset in this render.
    pub silenced: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    UnknownTrack(MixerTrackId),
    DuplicateTrack(MixerTrackId),
    UnscheduledTrack(MixerTrackId),
    MasterNotLast,
    /// An input reads a track that is not in an earlier level.
    NotEarlier(MixerTrackId),
    MalformedInput(MixerTrackId),
    UnsortedInputs(MixerTrackId),
    DuplicateSource(SourceId),
    DuplicateInsert(InsertId),
    /// A sidechain names an insert that is not on the receiving track.
    ForeignInsert(InsertId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareError {
    InvalidSampleRate,
    InvalidMaxFrames,
    Plan(PlanError),
    MissingInsert(InsertId),
    UnknownInsert(InsertId),
    DuplicateBinding(InsertId),
    UnknownGain(MixerTrackId),
    DuplicateGain(MixerTrackId),
    InvalidGain(MixerTrackId),
    Overflow,
    Budget { required: u64, limit: u64 },
    Allocation,
}

/// Every refusal happens before any device or buffer changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    TooManyFrames { frames: u32, max: u32 },
    SampleRate,
    OutputCapacity,
    SourceCount { expected: u32, got: u32 },
    SourceOrder { index: u32 },
    SourceLength { index: u32 },
}

#[derive(Clone, Copy, Debug)]
enum Feed {
    /// Offset of a tap's left block in the node buffers; right is `max_frames` later.
    Tap {
        at: u32,
        gain: f32,
    },
    Source {
        index: u32,
        gain: f32,
    },
}

#[derive(Clone, Copy, Debug)]
struct Node {
    audible: (u32, u32),
    inserts: (u32, u32),
    left: f32,
    right: f32,
}

pub struct PreparedMixer {
    sample_rate: f32,
    max_frames: usize,
    nodes: Vec<Node>,
    audible: Vec<Feed>,
    detector: Vec<Feed>,
    inserts: Vec<InsertSlot>,
    keys: Vec<(InsertId, u32)>,
    sources: Vec<SourceId>,
    /// Per node: pre L, pre R, post L, post R; then work L/R and detector L/R.
    buffers: Vec<f32>,
    resources: KernelResources,
}

fn reserved<T>(n: usize) -> Result<Vec<T>, PrepareError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| PrepareError::Allocation)?;
    Ok(v)
}

fn bytes<T>(v: &Vec<T>) -> Result<u64, PrepareError> {
    (v.capacity() as u64)
        .checked_mul(size_of::<T>() as u64)
        .ok_or(PrepareError::Overflow)
}

fn plan(e: PlanError) -> PrepareError {
    PrepareError::Plan(e)
}

impl PreparedMixer {
    /// Resolves every identity, tap and slot and reserves all buffers. Off-thread only.
    pub fn prepare(
        compiled: CompiledMixer,
        bindings: Vec<PreparedInsert>,
        gains: &[TrackGain],
        config: PrepareConfig,
    ) -> Result<Self, PrepareError> {
        if !config.sample_rate.is_finite() || config.sample_rate <= 0.0 {
            return Err(PrepareError::InvalidSampleRate);
        }
        if config.max_frames == 0 || config.max_frames > MAX_BLOCK {
            return Err(PrepareError::InvalidMaxFrames);
        }
        let mf = config.max_frames;

        // Execution order and each track's level.
        let by_id: BTreeMap<_, _> = compiled
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.track.id, i))
            .collect();
        if by_id.len() != compiled.tracks.len() {
            let mut seen = BTreeSet::new();
            for t in &compiled.tracks {
                if !seen.insert(t.track.id) {
                    return Err(plan(PlanError::DuplicateTrack(t.track.id)));
                }
            }
        }
        let mut order = Vec::new();
        let mut node_of = BTreeMap::new();
        let mut level_of = BTreeMap::new();
        for (level, ids) in compiled.levels.iter().enumerate() {
            for id in ids {
                let track = *by_id.get(id).ok_or(plan(PlanError::UnknownTrack(*id)))?;
                if node_of.insert(*id, order.len()).is_some() {
                    return Err(plan(PlanError::DuplicateTrack(*id)));
                }
                level_of.insert(*id, level);
                order.push(track);
            }
        }
        if let Some(t) = compiled
            .tracks
            .iter()
            .find(|t| !node_of.contains_key(&t.track.id))
        {
            return Err(plan(PlanError::UnscheduledTrack(t.track.id)));
        }
        if compiled.levels.last().map(Vec::as_slice) != Some(&[MASTER][..]) {
            return Err(plan(PlanError::MasterNotLast));
        }

        // Insert identities -> owning track, then the caller's bindings.
        let mut owner = BTreeMap::new();
        for t in &compiled.tracks {
            for i in &t.track.inserts {
                if owner.insert(i.id, t.track.id).is_some() {
                    return Err(plan(PlanError::DuplicateInsert(i.id)));
                }
            }
        }
        let mut devices = BTreeMap::new();
        for b in bindings {
            if !owner.contains_key(&b.id) {
                return Err(PrepareError::UnknownInsert(b.id));
            }
            let id = b.id;
            if devices.insert(id, b.device).is_some() {
                return Err(PrepareError::DuplicateBinding(id));
            }
        }
        if let Some(id) = owner.keys().find(|id| !devices.contains_key(id)) {
            return Err(PrepareError::MissingInsert(*id));
        }

        let mut fixed = BTreeMap::new();
        for g in gains {
            if !by_id.contains_key(&g.track) {
                return Err(PrepareError::UnknownGain(g.track));
            }
            if !g.left.is_finite() || !g.right.is_finite() {
                return Err(PrepareError::InvalidGain(g.track));
            }
            if fixed.insert(g.track, (g.left, g.right)).is_some() {
                return Err(PrepareError::DuplicateGain(g.track));
            }
        }

        // Source order is canonical identity order, independent of destinations.
        let mut source_set = BTreeSet::new();
        for t in &compiled.tracks {
            for input in &t.inputs {
                if let InputId::Source(id) = input.id
                    && !source_set.insert(id)
                {
                    return Err(plan(PlanError::DuplicateSource(id)));
                }
            }
        }
        let source_index: BTreeMap<_, _> = source_set
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();

        let stride = mf.checked_mul(4).ok_or(PrepareError::Overflow)?;
        let tap_at = |node: usize, tap: Tap| -> Result<u32, PrepareError> {
            let base = node.checked_mul(stride).ok_or(PrepareError::Overflow)?;
            let at = if tap == Tap::PostFader {
                base + 2 * mf
            } else {
                base
            };
            u32::try_from(at).map_err(|_| PrepareError::Overflow)
        };
        let to_u32 = |n: usize| u32::try_from(n).map_err(|_| PrepareError::Overflow);

        let n_inputs: usize = compiled.tracks.iter().map(|t| t.inputs.len()).sum();
        let mut nodes = reserved(order.len())?;
        let mut audible = reserved(n_inputs)?;
        let mut detector = reserved(n_inputs)?;
        let mut inserts = reserved(owner.len())?;
        let mut keys = reserved(owner.len())?;
        let mut sources = reserved(source_set.len())?;
        sources.extend(source_set.iter().copied());

        for &ti in &order {
            let t = &compiled.tracks[ti];
            let id = t.track.id;
            let level = level_of[&id];
            for pair in t.inputs.windows(2) {
                if pair[0].id >= pair[1].id {
                    return Err(plan(PlanError::UnsortedInputs(id)));
                }
            }
            let start = to_u32(audible.len())?;
            for input in &t.inputs {
                if !input.gain.is_finite() {
                    return Err(plan(PlanError::MalformedInput(id)));
                }
                let from = match input.id {
                    InputId::Source(source) => {
                        if input.from.is_some() {
                            return Err(plan(PlanError::MalformedInput(id)));
                        }
                        audible.push(Feed::Source {
                            index: to_u32(source_index[&source])?,
                            gain: input.gain,
                        });
                        continue;
                    }
                    InputId::Main(from) => (input.from == Some(from)).then_some(from),
                    InputId::Send(_) => input.from,
                    InputId::Sidechain { from, tap, .. } => {
                        (input.from == Some(from) && input.tap == tap).then_some(from)
                    }
                };
                let from = from.ok_or(plan(PlanError::MalformedInput(id)))?;
                let from_level = *level_of
                    .get(&from)
                    .ok_or(plan(PlanError::UnknownTrack(from)))?;
                if from_level >= level {
                    return Err(plan(PlanError::NotEarlier(from)));
                }
                if let InputId::Sidechain { insert, .. } = input.id {
                    if owner.get(&insert) != Some(&id) {
                        return Err(plan(PlanError::ForeignInsert(insert)));
                    }
                    continue;
                }
                audible.push(Feed::Tap {
                    at: tap_at(node_of[&from], input.tap)?,
                    gain: input.gain,
                });
            }
            let audible_range = (start, to_u32(audible.len())?);
            let first_insert = to_u32(inserts.len())?;
            for insert in &t.track.inserts {
                let d0 = to_u32(detector.len())?;
                // Sidechains to this insert, in the compiled (canonical) input order.
                for input in &t.inputs {
                    if let InputId::Sidechain {
                        from,
                        insert: target,
                        tap,
                    } = input.id
                        && target == insert.id
                    {
                        detector.push(Feed::Tap {
                            at: tap_at(node_of[&from], tap)?,
                            gain: input.gain,
                        });
                    }
                }
                keys.push((insert.id, to_u32(inserts.len())?));
                inserts.push(InsertSlot {
                    id: insert.id,
                    device: devices
                        .remove(&insert.id)
                        .ok_or(PrepareError::MissingInsert(insert.id))?,
                    detector: (d0, to_u32(detector.len())?),
                });
            }
            let (left, right) = fixed.get(&id).copied().unwrap_or((1.0, 1.0));
            nodes.push(Node {
                audible: audible_range,
                inserts: (first_insert, to_u32(inserts.len())?),
                left,
                right,
            });
        }
        keys.sort_unstable_by_key(|(id, _)| *id);

        let samples = order
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(stride))
            .ok_or(PrepareError::Overflow)?;
        // Charge the budget before the large audio reservation.
        let audio_estimate = (samples as u64)
            .checked_mul(size_of::<f32>() as u64)
            .ok_or(PrepareError::Overflow)?;
        let table_bytes = [
            bytes(&nodes)?,
            bytes(&audible)?,
            bytes(&detector)?,
            bytes(&inserts)?,
            bytes(&keys)?,
            bytes(&sources)?,
        ]
        .into_iter()
        .try_fold(0u64, |a, b| a.checked_add(b))
        .ok_or(PrepareError::Overflow)?;
        let required = audio_estimate
            .checked_add(table_bytes)
            .ok_or(PrepareError::Overflow)?;
        if required > config.max_bytes {
            return Err(PrepareError::Budget {
                required,
                limit: config.max_bytes,
            });
        }
        let mut buffers = reserved(samples)?;
        buffers.resize(samples, 0.0);
        let audio_bytes = bytes(&buffers)?;
        let total = audio_bytes
            .checked_add(table_bytes)
            .ok_or(PrepareError::Overflow)?;
        if total > config.max_bytes {
            return Err(PrepareError::Budget {
                required: total,
                limit: config.max_bytes,
            });
        }
        Ok(Self {
            sample_rate: config.sample_rate,
            max_frames: mf,
            nodes,
            audible,
            detector,
            inserts,
            keys,
            sources,
            buffers,
            resources: KernelResources {
                audio_bytes,
                table_bytes,
            },
        })
    }

    /// The exact source identities each render must supply, in this order.
    pub fn source_order(&self) -> &[SourceId] {
        &self.sources
    }

    pub fn resources(&self) -> KernelResources {
        self.resources
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Renders `transport.frames` frames into `out_l`/`out_r[..frames]`. All metadata is
    /// checked before any device or buffer changes; a zero-frame call changes nothing.
    pub fn render(
        &mut self,
        transport: &HostTransport,
        sources: &[SourceBlock<'_>],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) -> Result<RenderReport, RenderError> {
        let n = transport.frames as usize;
        if n > self.max_frames {
            return Err(RenderError::TooManyFrames {
                frames: transport.frames,
                max: self.max_frames as u32,
            });
        }
        if transport.sample_rate.to_bits() != self.sample_rate.to_bits() {
            return Err(RenderError::SampleRate);
        }
        if out_l.len() < n || out_r.len() < n {
            return Err(RenderError::OutputCapacity);
        }
        if sources.len() != self.sources.len() {
            return Err(RenderError::SourceCount {
                expected: self.sources.len() as u32,
                got: sources.len().min(u32::MAX as usize) as u32,
            });
        }
        for (i, (block, id)) in sources.iter().zip(&self.sources).enumerate() {
            if block.id != *id {
                return Err(RenderError::SourceOrder { index: i as u32 });
            }
            if block.left.len() != n || block.right.len() != n {
                return Err(RenderError::SourceLength { index: i as u32 });
            }
        }
        if n == 0 {
            return Ok(RenderReport::default());
        }

        let mf = self.max_frames;
        let stride = 4 * mf;
        let (taps, scratch) = self.buffers.split_at_mut(self.nodes.len() * stride);
        let (wl, scratch) = scratch.split_at_mut(mf);
        let (wr, scratch) = scratch.split_at_mut(mf);
        let (dl, dr) = scratch.split_at_mut(mf);
        let (wl, wr, dl, dr) = (&mut wl[..n], &mut wr[..n], &mut dl[..n], &mut dr[..n]);
        let mut silenced = 0;

        for (k, node) in self.nodes.iter().enumerate() {
            wl.fill(0.0);
            wr.fill(0.0);
            let feeds = &self.audible[node.audible.0 as usize..node.audible.1 as usize];
            for feed in feeds {
                let (l, r, gain) = match *feed {
                    Feed::Tap { at, gain } => {
                        let at = at as usize;
                        (&taps[at..at + n], &taps[at + mf..at + mf + n], gain)
                    }
                    Feed::Source { index, gain } => {
                        let s = &sources[index as usize];
                        (s.left, s.right, gain)
                    }
                };
                accumulate(wl, wr, l, r, gain);
            }
            let chain = node.inserts.0 as usize..node.inserts.1 as usize;
            for slot in &mut self.inserts[chain.clone()] {
                let (d0, d1) = (slot.detector.0 as usize, slot.detector.1 as usize);
                let sidechain = if d0 == d1 {
                    None
                } else {
                    dl.fill(0.0);
                    dr.fill(0.0);
                    for feed in &self.detector[d0..d1] {
                        if let Feed::Tap { at, gain } = *feed {
                            let at = at as usize;
                            accumulate(
                                dl,
                                dr,
                                &taps[at..at + n],
                                &taps[at + mf..at + mf + n],
                                gain,
                            );
                        }
                    }
                    Some((&dl[..], &dr[..]))
                };
                slot.device.set_transport(transport);
                slot.device.set_tempo(transport.bpm);
                slot.device.process(wl, wr, sidechain, &[]);
            }
            let block = &mut taps[k * stride..(k + 1) * stride];
            let (pre, post) = block.split_at_mut(2 * mf);
            let (pre_l, pre_r) = pre.split_at_mut(mf);
            let (post_l, post_r) = post.split_at_mut(mf);
            let (pre_l, pre_r) = (&mut pre_l[..n], &mut pre_r[..n]);
            let (post_l, post_r) = (&mut post_l[..n], &mut post_r[..n]);
            pre_l.copy_from_slice(wl);
            pre_r.copy_from_slice(wr);
            for (o, x) in post_l.iter_mut().zip(wl.iter()) {
                *o = *x * node.left;
            }
            for (o, x) in post_r.iter_mut().zip(wr.iter()) {
                *o = *x * node.right;
            }
            let finite = pre_l
                .iter()
                .chain(pre_r.iter())
                .chain(post_l.iter())
                .chain(post_r.iter())
                .all(|x| x.is_finite());
            if !finite {
                pre_l.fill(0.0);
                pre_r.fill(0.0);
                post_l.fill(0.0);
                post_r.fill(0.0);
                for slot in &mut self.inserts[chain] {
                    slot.device.reset();
                }
                silenced += 1;
            }
        }
        // Master is the final node (checked in prepare).
        let master = (self.nodes.len() - 1) * stride + 2 * mf;
        out_l[..n].copy_from_slice(&taps[master..master + n]);
        out_r[..n].copy_from_slice(&taps[master + mf..master + mf + n]);
        Ok(RenderReport {
            frames: n as u32,
            silenced,
        })
    }

    /// Clears every device and tap without resizing or freeing anything.
    pub fn reset(&mut self) {
        for slot in &mut self.inserts {
            slot.device.reset();
        }
        self.buffers.fill(0.0);
    }

    /// Forwards a parameter to one insert; call between renders at the caller's boundary.
    pub fn set_param(
        &mut self,
        insert: InsertId,
        index: u32,
        value: f32,
    ) -> Result<(), ParamError> {
        let slot = find(&self.keys, insert)?;
        debug_assert_eq!(self.inserts[slot].id, insert);
        self.inserts[slot].device.set_param(index, value);
        Ok(())
    }
}

/// `acc += x * gain`, one product and one sum per sample, no fused or reordered maths.
fn accumulate(acc_l: &mut [f32], acc_r: &mut [f32], l: &[f32], r: &[f32], gain: f32) {
    for (a, x) in acc_l.iter_mut().zip(l) {
        *a += *x * gain;
    }
    for (a, x) in acc_r.iter_mut().zip(r) {
        *a += *x * gain;
    }
}
