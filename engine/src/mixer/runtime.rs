//! Static serial renderer for a [`CompiledMixer`]. Everything owning (identity resolution,
//! buffer reservation, device binding) happens in [`PreparedMixer::prepare`] off-thread;
//! `render`, `reset` and `set_param` never allocate, free, lock, log or do I/O.
//!
//! Per node, in the compiled level order: audible inputs are summed in the compiled vector
//! order, the inserts run in chain order (each sidechain-carrying insert gets its own
//! detector sum, never audible), the result is the pre-fader tap and the stereo fader
//! coefficients make the post-fader tap. Master runs last and its post-fader tap is the output.
//!
//! Fault convention (same as the legacy graph): a node whose pre- or post-fader block holds
//! NaN or infinity is zeroed and all its inserts are reset; healthy nodes keep running. That
//! is containment only: downstream nodes still hear the silenced block, and a device that
//! faults again is silenced again. No other recovery is promised.
//!
//! Dropping a `PreparedMixer` frees its devices and buffers: do it off the audio thread.

use super::controls::{FeedGate, GATE_TABLE_BYTES, GatePlan, ROLE_BYTES};
use super::delay::{DelayBank, DelayIndex, Ring};
use super::insert::{BypassState, InsertSlot, ParamError, PreparedInsert, find};
use super::{
    ActualLatency, CompiledMixer, InputId, InsertId, LatencyError, MASTER, MixerControls,
    MixerTrackId, PdcBudget, PdcDelay, PdcPoint, SourceId, Tap, TrackArrival, plan_latency,
};
use crate::graph::ramp_len;
use grooviq_dsp::{HostTransport, MAX_BLOCK};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;

/// Target linear coefficients from the pre-fader to the post-fader tap. A track without an
/// entry has unity targets. No pan law, mute or solo is implied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackGain {
    pub track: MixerTrackId,
    pub left: f32,
    pub right: f32,
}

/// A prepared bypass target for one insert. Even a wet target opts in to dry history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InsertBypass {
    pub insert: InsertId,
    pub bypassed: bool,
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
    Latency(LatencyError),
    MissingInsert(InsertId),
    UnknownInsert(InsertId),
    DuplicateBinding(InsertId),
    UnknownGain(MixerTrackId),
    DuplicateGain(MixerTrackId),
    InvalidGain(MixerTrackId),
    UnorderedBypass,
    InvalidControls,
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
struct Feed {
    input: FeedInput,
    delay: DelayIndex,
}

#[derive(Clone, Copy, Debug)]
enum FeedInput {
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

struct RouteSpan<'a> {
    feeds: &'a [Feed],
    delays: &'a DelayBank,
    half_stride: usize,
}

impl RouteSpan<'_> {
    fn same_route(&self, a: Feed, old: &Self, b: Feed, paths: &[SignalPath]) -> bool {
        if self.delays.frames(a.delay) != old.delays.frames(b.delay) {
            return false;
        }
        match (a.input, b.input) {
            (FeedInput::Source { .. }, FeedInput::Source { .. }) => true, // same gate ID
            (FeedInput::Tap { at: a, .. }, FeedInput::Tap { at: b, .. }) => {
                let a = a as usize / self.half_stride;
                let b = b as usize / old.half_stride;
                let upstream = paths[a / 2];
                upstream.continues
                    && upstream.previous.map(|index| index as usize) == Some(b / 2)
                    && a % 2 == b % 2
            }
            _ => false,
        }
    }

    fn carry_gates(
        &self,
        new_gates: &mut [FeedGate],
        old: &Self,
        old_gates: &[FeedGate],
        paths: &[SignalPath],
        samples: u32,
    ) {
        let mut previous = old_gates.iter().zip(old.feeds).peekable();
        for (gate, feed) in new_gates.iter_mut().zip(self.feeds) {
            while previous.next_if(|(prior, _)| prior.id < gate.id).is_some() {}
            if let Some((prior, prior_feed)) = previous.next_if(|(prior, _)| prior.id == gate.id)
                && self.same_route(*feed, old, *prior_feed, paths)
            {
                carry_gain(&mut gate.gain, prior.gain, samples);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Node {
    audible: (u32, u32),
    inserts: (u32, u32),
    fader: [FaderGain; 2],
}

#[derive(Clone, Copy)]
struct SignalPath {
    track: MixerTrackId,
    // Set in execution order during bypass adoption for later dependent nodes.
    previous: Option<u32>,
    continues: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FaderGain {
    start: f32,
    current: f32,
    pub(super) target: f32,
    remaining: u32,
    step: f64,
}

impl FaderGain {
    pub(super) fn at(target: f32) -> Self {
        Self {
            start: target,
            current: target,
            target,
            remaining: 0,
            step: 0.0,
        }
    }

    pub(super) fn retarget(&mut self, target: f32, samples: u32) {
        if self.target.to_bits() == target.to_bits() {
            return;
        }
        if self.current.to_bits() == target.to_bits() {
            *self = Self::at(target);
            return;
        }
        self.start = self.current;
        self.target = target;
        self.remaining = samples;
        // TrackGain accepts every finite f32. Subtraction in f32 can overflow
        // when the endpoints have opposite signs.
        self.step = (f64::from(target) - f64::from(self.start)) / f64::from(samples);
    }

    fn is_unity(&self) -> bool {
        self.target == 1.0 && self.remaining == 0
    }

    pub(super) fn next(&mut self, samples: u32) -> f32 {
        if self.remaining != 0 {
            self.remaining -= 1;
            self.current = if self.remaining == 0 {
                self.target
            } else {
                (f64::from(self.start) + self.step * f64::from(samples - self.remaining)) as f32
            };
        }
        self.current
    }

    fn apply(&mut self, samples: u32, input: &[f32], output: &mut [f32]) {
        let active = input.len().min(self.remaining as usize);
        for (o, x) in output[..active].iter_mut().zip(&input[..active]) {
            *o = *x * self.next(samples);
        }
        for (o, x) in output[active..].iter_mut().zip(&input[active..]) {
            *o = *x * self.current;
        }
    }
}

// Per-track capacity reserved during live preparation.
pub(super) const TRACK_TABLE_BYTES: u64 =
    (size_of::<Node>() + size_of::<(MixerTrackId, u32)>()) as u64;
pub(super) const BYPASS_TABLE_BYTES: u64 = size_of::<BypassState>() as u64;
pub(super) const PATH_TABLE_BYTES: u64 = size_of::<SignalPath>() as u64;
pub(super) const STEREO_FRAME_BYTES: u64 = size_of::<[f32; 2]>() as u64;

struct Gates {
    audible: Vec<FeedGate>,
    detector: Vec<FeedGate>,
    master: FaderGain,
}

impl Gates {
    fn active(&self) -> bool {
        !self.master.is_unity()
            || self
                .audible
                .iter()
                .chain(&self.detector)
                .any(|gate| !gate.gain.is_unity())
    }

    fn reset(&mut self) {
        self.master = FaderGain::at(self.master.target);
        for gate in self.audible.iter_mut().chain(&mut self.detector) {
            gate.gain = FaderGain::at(gate.gain.target);
        }
    }
}

pub struct PreparedMixer {
    sample_rate: f32,
    fader_samples: u32,
    max_frames: usize,
    nodes: Vec<Node>,
    audible: Vec<Feed>,
    detector: Vec<Feed>,
    inserts: Vec<InsertSlot>,
    keys: Vec<(InsertId, u32)>,
    /// Sorted persistent track IDs and their indices in compiled execution order.
    track_nodes: Vec<(MixerTrackId, u32)>,
    sources: Vec<SourceId>,
    /// Sparse opted-in states in render slot order.
    bypass: Vec<BypassState>,
    /// One per node in a kernel that carries control state across installs.
    paths: Vec<SignalPath>,
    gates: Option<Gates>,
    /// Per node: pre L, pre R, post L, post R; then work L/R, detector L/R and
    /// optional shared dry L/R.
    buffers: Vec<f32>,
    resources: KernelResources,
    delays: DelayBank,
    /// None for the original uncompensated static renderer.
    master_latency: Option<u64>,
}

pub(super) fn reserved<T>(n: usize) -> Result<Vec<T>, PrepareError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| PrepareError::Allocation)?;
    Ok(v)
}

pub(super) fn bytes<T>(v: &Vec<T>) -> Result<u64, PrepareError> {
    (v.capacity() as u64)
        .checked_mul(size_of::<T>() as u64)
        .ok_or(PrepareError::Overflow)
}

fn plan(e: PlanError) -> PrepareError {
    PrepareError::Plan(e)
}

#[derive(Default)]
struct PrepareDelays {
    points: std::vec::IntoIter<PdcDelay>,
    tracks: std::vec::IntoIter<TrackArrival>,
    bank: DelayBank,
    ring_bytes: u64,
    master_latency: Option<u64>,
}

impl PrepareDelays {
    fn new(
        compiled: &CompiledMixer,
        snapshot: &[ActualLatency],
        limit: u64,
    ) -> Result<Self, PrepareError> {
        let timing = plan_latency(
            compiled,
            snapshot,
            PdcBudget {
                descriptors: u64::MAX,
                ring_samples: u64::MAX,
                memory_bytes: limit,
            },
        )
        .map_err(|error| match error {
            LatencyError::Plan(e) => plan(e),
            LatencyError::Budget { required, .. } => PrepareError::Budget { required, limit },
            LatencyError::Allocation => PrepareError::Allocation,
            LatencyError::Overflow => PrepareError::Overflow,
            e => PrepareError::Latency(e),
        })?;
        Ok(Self {
            bank: DelayBank::reserve(&timing)?,
            points: timing.delays.into_iter(),
            tracks: timing.tracks.into_iter(),
            ring_bytes: timing.resources.ring_bytes,
            master_latency: Some(timing.master_frames),
        })
    }

    fn gather(&mut self, track: MixerTrackId) -> u64 {
        if self.master_latency.is_none() {
            return 0;
        }
        let arrival = self.tracks.next().expect("planned track arrival");
        assert_eq!(arrival.track, track);
        arrival.gather
    }

    fn take(&mut self, at: PdcPoint) -> PdcDelay {
        if self.master_latency.is_none() {
            return PdcDelay { at, frames: 0 };
        }
        let delay = self.points.next().expect("planned compensation point");
        assert_eq!(delay.at, at);
        delay
    }
}

impl PreparedMixer {
    /// Prepares the original static renderer without delay compensation. Existing
    /// static kernel callers can replay their uncompensated mix through this constructor.
    pub fn prepare(
        compiled: CompiledMixer,
        bindings: Vec<PreparedInsert>,
        gains: &[TrackGain],
        config: PrepareConfig,
    ) -> Result<Self, PrepareError> {
        Self::prepare_inner(compiled, bindings, gains, &[], None, config, false)
    }

    /// Queries the prepared devices' actual latency and reserves compensation histories.
    /// Prepare devices with their final parameter banks before calling. Off-thread only.
    pub fn prepare_compensated(
        compiled: CompiledMixer,
        bindings: Vec<PreparedInsert>,
        gains: &[TrackGain],
        config: PrepareConfig,
    ) -> Result<Self, PrepareError> {
        Self::prepare_compensated_with_bypass(compiled, bindings, gains, &[], config)
    }

    /// Prepares a compensated kernel with optional insert bypass. Targets must be
    /// strictly increasing by insert ID. A fresh kernel starts at the supplied targets.
    /// Supply each controlled slot even when wet to warm its dry history for live changes.
    /// Off-thread only.
    pub fn prepare_compensated_with_bypass(
        compiled: CompiledMixer,
        bindings: Vec<PreparedInsert>,
        gains: &[TrackGain],
        bypass: &[InsertBypass],
        config: PrepareConfig,
    ) -> Result<Self, PrepareError> {
        Self::prepare_inner(compiled, bindings, gains, bypass, None, config, true)
    }

    /// Prepares an atomic set of track, detector and insert targets. A new
    /// kernel starts at these targets. Transfer from a compatible controlled kernel
    /// to de-click later changes. Off-thread only.
    pub fn prepare_compensated_with_controls(
        compiled: CompiledMixer,
        bindings: Vec<PreparedInsert>,
        gains: &[TrackGain],
        controls: &MixerControls,
        config: PrepareConfig,
    ) -> Result<Self, PrepareError> {
        Self::prepare_inner(
            compiled,
            bindings,
            gains,
            &controls.bypass,
            Some(controls),
            config,
            true,
        )
    }

    pub(super) fn validate_controls(
        compiled: &CompiledMixer,
        controls: &MixerControls,
    ) -> Result<(), PrepareError> {
        let node_of: BTreeMap<_, _> = compiled
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.track.id, i))
            .collect();
        GatePlan::validate(controls, compiled, &node_of)?;
        if controls
            .bypass
            .windows(2)
            .any(|pair| pair[0].insert >= pair[1].insert)
        {
            return Err(PrepareError::UnorderedBypass);
        }
        let found = compiled
            .tracks
            .iter()
            .flat_map(|t| &t.track.inserts)
            .filter(|i| {
                controls
                    .bypass
                    .binary_search_by_key(&i.id, |target| target.insert)
                    .is_ok()
            })
            .count();
        if found != controls.bypass.len() {
            return Err(PrepareError::InvalidControls);
        }
        Ok(())
    }

    fn prepare_inner(
        compiled: CompiledMixer,
        bindings: Vec<PreparedInsert>,
        gains: &[TrackGain],
        bypass: &[InsertBypass],
        controls: Option<&MixerControls>,
        config: PrepareConfig,
        compensate: bool,
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
        if bypass
            .windows(2)
            .any(|pair| pair[0].insert >= pair[1].insert)
        {
            return Err(PrepareError::UnorderedBypass);
        }
        if let Some(target) = bypass
            .iter()
            .find(|target| !owner.contains_key(&target.insert))
        {
            return Err(PrepareError::UnknownInsert(target.insert));
        }
        let mut devices = BTreeMap::new();
        for b in bindings {
            if !owner.contains_key(&b.id) {
                return Err(PrepareError::UnknownInsert(b.id));
            }
            let id = b.id;
            let latency = if compensate {
                b.device.latency_frames()
            } else {
                0
            };
            if devices
                .insert(id, (b.device, b.identity, latency))
                .is_some()
            {
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

        let mut timing = PrepareDelays::default();
        if compensate {
            let mut snapshot = reserved(devices.len())?;
            snapshot.extend(
                devices
                    .iter()
                    .map(|(&insert, (_, _, frames))| ActualLatency {
                        insert,
                        frames: *frames,
                    }),
            );
            timing = PrepareDelays::new(&compiled, &snapshot, config.max_bytes)?;
        }

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
        let mut track_nodes = reserved(order.len())?;
        let mut bypass_states = reserved(bypass.len())?;
        let track_paths = !bypass.is_empty() || controls.is_some();
        let mut paths = reserved(if track_paths { order.len() } else { 0 })?;
        let (mut gates, gate_plan) = if let Some(controls) = controls {
            let n_detector = compiled
                .tracks
                .iter()
                .flat_map(|t| &t.inputs)
                .filter(|input| matches!(input.id, InputId::Sidechain { .. }))
                .count();
            let requested = (n_inputs as u64)
                .checked_mul(GATE_TABLE_BYTES)
                .and_then(|n| n.checked_add((order.len() as u64) * (PATH_TABLE_BYTES + ROLE_BYTES)))
                .ok_or(PrepareError::Overflow)?;
            if requested > config.max_bytes {
                return Err(PrepareError::Budget {
                    required: requested,
                    limit: config.max_bytes,
                });
            }
            let plan = GatePlan::new(controls, &compiled, &order, &node_of)?;
            (
                Some(Gates {
                    audible: reserved(n_inputs - n_detector)?,
                    detector: reserved(n_detector)?,
                    master: FaderGain::at(f32::from(plan.track_open(MASTER))),
                }),
                Some(plan),
            )
        } else {
            (None, None)
        };
        let mut bypass_ring_bytes = 0u64;
        let mut sources = reserved(source_set.len())?;
        sources.extend(source_set.iter().copied());

        for (node_index, &ti) in order.iter().enumerate() {
            let t = &compiled.tracks[ti];
            let id = t.track.id;
            track_nodes.push((id, to_u32(nodes.len())?));
            let level = level_of[&id];
            for pair in t.inputs.windows(2) {
                if pair[0].id >= pair[1].id {
                    return Err(plan(PlanError::UnsortedInputs(id)));
                }
            }
            let start = to_u32(audible.len())?;
            let gather = timing.gather(id);
            for input in &t.inputs {
                if !input.gain.is_finite() {
                    return Err(plan(PlanError::MalformedInput(id)));
                }
                if let (Some(gates), Some(plan)) = (&mut gates, &gate_plan)
                    && !matches!(input.id, InputId::Sidechain { .. })
                {
                    gates.audible.push(FeedGate::at(
                        input.id,
                        plan.audible_open(input, node_index, &node_of),
                    ));
                }
                let from = match input.id {
                    InputId::Source(source) => {
                        if input.from.is_some() {
                            return Err(plan(PlanError::MalformedInput(id)));
                        }
                        let point = timing.take(PdcPoint::Audible {
                            track: id,
                            input: input.id,
                        });
                        audible.push(Feed {
                            input: FeedInput::Source {
                                index: to_u32(source_index[&source])?,
                                gain: input.gain,
                            },
                            delay: timing.bank.register(point, gather, None, None)?,
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
                let point = timing.take(PdcPoint::Audible {
                    track: id,
                    input: input.id,
                });
                audible.push(Feed {
                    input: FeedInput::Tap {
                        at: tap_at(node_of[&from], input.tap)?,
                        gain: input.gain,
                    },
                    delay: timing
                        .bank
                        .register(point, gather, Some((from, input.tap)), None)?,
                });
            }
            let audible_range = (start, to_u32(audible.len())?);
            let first_insert = to_u32(inserts.len())?;
            let mut arrival = gather;
            let mut predecessor = None;
            for insert in &t.track.inserts {
                let point = timing.take(PdcPoint::Stage {
                    track: id,
                    insert: insert.id,
                });
                arrival += point.frames;
                let delay = timing.bank.register(point, arrival, None, predecessor)?;
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
                        let point = timing.take(PdcPoint::Detector {
                            track: id,
                            insert: insert.id,
                            input: input.id,
                        });
                        if let (Some(gates), Some(plan)) = (&mut gates, &gate_plan) {
                            gates
                                .detector
                                .push(FeedGate::at(input.id, plan.detector_open(input.id)));
                        }
                        detector.push(Feed {
                            input: FeedInput::Tap {
                                at: tap_at(node_of[&from], tap)?,
                                gain: input.gain,
                            },
                            delay: timing.bank.register(
                                point,
                                arrival,
                                Some((from, tap)),
                                predecessor,
                            )?,
                        });
                    }
                }
                keys.push((insert.id, to_u32(inserts.len())?));
                let (device, identity, latency) = devices
                    .remove(&insert.id)
                    .ok_or(PrepareError::MissingInsert(insert.id))?;
                if let Ok(i) = bypass.binary_search_by_key(&insert.id, |b| b.insert) {
                    bypass_ring_bytes = bypass_ring_bytes
                        .checked_add(u64::from(latency) * STEREO_FRAME_BYTES)
                        .ok_or(PrepareError::Overflow)?;
                    bypass_states.push(BypassState {
                        slot: inserts.len(),
                        history: None,
                        blend: FaderGain::at(f32::from(bypass[i].bypassed)),
                    });
                }
                inserts.push(InsertSlot {
                    id: insert.id,
                    device,
                    identity,
                    detector: (d0, to_u32(detector.len())?),
                    delay,
                    latency,
                });
                arrival += u64::from(latency);
                predecessor = Some(insert.id);
            }
            let (left, right) = fixed.get(&id).copied().unwrap_or((1.0, 1.0));
            nodes.push(Node {
                audible: audible_range,
                inserts: (first_insert, to_u32(inserts.len())?),
                fader: [FaderGain::at(left), FaderGain::at(right)],
            });
            if track_paths {
                paths.push(SignalPath {
                    track: id,
                    previous: None,
                    continues: false,
                });
            }
        }
        assert!(timing.points.len() == 0 && timing.tracks.len() == 0);
        keys.sort_unstable_by_key(|(id, _)| *id);
        track_nodes.sort_unstable_by_key(|(id, _)| *id);

        drop(gate_plan);
        let samples = order
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(stride))
            .and_then(|n| n.checked_add(if bypass.is_empty() { 0 } else { 2 * mf }))
            .ok_or(PrepareError::Overflow)?;
        // Charge the budget before the large audio reservation.
        let audio_estimate = (samples as u64)
            .checked_mul(size_of::<f32>() as u64)
            .and_then(|bytes| bytes.checked_add(timing.ring_bytes))
            .and_then(|bytes| bytes.checked_add(bypass_ring_bytes))
            .ok_or(PrepareError::Overflow)?;
        let table_bytes = [
            bytes(&nodes)?,
            bytes(&audible)?,
            bytes(&detector)?,
            bytes(&inserts)?,
            bytes(&keys)?,
            bytes(&track_nodes)?,
            bytes(&sources)?,
            bytes(&bypass_states)?,
            bytes(&paths)?,
            if let Some(gates) = &gates {
                bytes(&gates.audible)?
            } else {
                0
            },
            if let Some(gates) = &gates {
                bytes(&gates.detector)?
            } else {
                0
            },
            timing.bank.table_bytes()?,
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
        let mut audio_bytes = bytes(&buffers)?
            .checked_add(timing.bank.allocate()?)
            .ok_or(PrepareError::Overflow)?;
        for bypass in &mut bypass_states {
            let latency = inserts[bypass.slot].latency;
            if latency != 0 {
                let frames = usize::try_from(latency).map_err(|_| PrepareError::Overflow)?;
                let ring = Ring::allocate(frames)?;
                audio_bytes = audio_bytes
                    .checked_add(ring.bytes()?)
                    .ok_or(PrepareError::Overflow)?;
                bypass.history = Some(ring);
            }
        }
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
            fader_samples: ramp_len(config.sample_rate),
            max_frames: mf,
            nodes,
            audible,
            detector,
            inserts,
            keys,
            track_nodes,
            sources,
            bypass: bypass_states,
            paths,
            gates,
            buffers,
            delays: timing.bank,
            master_latency: timing.master_latency,
            resources: KernelResources {
                audio_bytes,
                table_bytes,
            },
        })
    }

    /// Call after a successful render of `frames` samples, whose taps the guard has checked.
    pub(crate) fn post_fader_peaks(&self, track: MixerTrackId, frames: usize) -> Option<[f32; 2]> {
        if track == MASTER {
            return None;
        }
        let index = self
            .track_nodes
            .binary_search_by_key(&track, |&(id, _)| id)
            .ok()?;
        let node = self.track_nodes[index].1 as usize;
        let left = (4 * node + 2) * self.max_frames;
        let right = left + self.max_frames;
        let mut peak = [0.0f32; 2];
        for (&l, &r) in self.buffers[left..left + frames]
            .iter()
            .zip(&self.buffers[right..right + frames])
        {
            peak[0] = peak[0].max(l.abs());
            peak[1] = peak[1].max(r.abs());
        }
        Some(peak)
    }

    /// The exact source identities each render must supply, in this order.
    pub fn source_order(&self) -> &[SourceId] {
        &self.sources
    }

    pub fn resources(&self) -> KernelResources {
        self.resources
    }

    /// Actual processing latency from mixer sources to the master output, when compensated.
    pub fn master_latency_frames(&self) -> Option<u64> {
        self.master_latency
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
        Ok(self.run(transport, &Blocks(sources), out_l, out_r))
    }

    /// Allocates source buffers matching [`Self::source_order`] for [`Self::render_pulled`].
    /// Off-thread only.
    pub fn source_buffers(&self) -> Result<SourceBuffers, PrepareError> {
        let len = self
            .sources
            .len()
            .checked_mul(2 * self.max_frames)
            .ok_or(PrepareError::Overflow)?;
        let mut ids = reserved(self.sources.len())?;
        ids.extend_from_slice(&self.sources);
        let mut data = reserved(len)?;
        data.resize(len, 0.0);
        Ok(SourceBuffers {
            ids,
            max_frames: self.max_frames,
            data,
        })
    }

    /// [`Self::render`] reading each source from `sources`, which the caller filled in place:
    /// no per-render source list is built. Refuses buffers made for another source order.
    pub fn render_pulled(
        &mut self,
        transport: &HostTransport,
        sources: &SourceBuffers,
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
        if sources.ids.len() != self.sources.len() {
            return Err(RenderError::SourceCount {
                expected: self.sources.len() as u32,
                got: sources.ids.len().min(u32::MAX as usize) as u32,
            });
        }
        if let Some(i) = sources
            .ids
            .iter()
            .zip(&self.sources)
            .position(|(a, b)| a != b)
        {
            return Err(RenderError::SourceOrder { index: i as u32 });
        }
        if sources.max_frames < n {
            return Err(RenderError::SourceLength { index: 0 });
        }
        if n == 0 {
            return Ok(RenderReport::default());
        }
        Ok(self.run(transport, &Pulled(sources, n), out_l, out_r))
    }

    /// The validated render core shared by both source forms.
    fn run(
        &mut self,
        transport: &HostTransport,
        sources: &dyn SourceView,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) -> RenderReport {
        let n = transport.frames as usize;
        let mf = self.max_frames;
        let stride = 4 * mf;
        let (taps, scratch) = self.buffers.split_at_mut(self.nodes.len() * stride);
        let (wl, scratch) = scratch.split_at_mut(mf);
        let (wr, scratch) = scratch.split_at_mut(mf);
        let (dl, scratch) = scratch.split_at_mut(mf);
        let (dr, dry) = scratch.split_at_mut(mf);
        let (wl, wr, dl, dr) = (&mut wl[..n], &mut wr[..n], &mut dl[..n], &mut dr[..n]);
        let mut silenced = 0;
        let mut bypass_at = 0;

        for (k, node) in self.nodes.iter_mut().enumerate() {
            wl.fill(0.0);
            wr.fill(0.0);
            let feeds = &self.audible[node.audible.0 as usize..node.audible.1 as usize];
            for (offset, feed) in feeds.iter().enumerate() {
                let gate = self
                    .gates
                    .as_mut()
                    .map(|gates| &mut gates.audible[node.audible.0 as usize + offset].gain);
                let (l, r, gain) = match feed.input {
                    FeedInput::Tap { at, gain } => {
                        let at = at as usize;
                        (&taps[at..at + n], &taps[at + mf..at + mf + n], gain)
                    }
                    FeedInput::Source { index, gain } => {
                        let (l, r) = sources.block(index as usize);
                        (l, r, gain)
                    }
                };
                let mut target = StereoSum {
                    left: wl,
                    right: wr,
                };
                target.feed(
                    &mut self.delays,
                    feed.delay,
                    gate,
                    (l, r),
                    gain,
                    self.fader_samples,
                );
            }
            let chain = node.inserts.0 as usize..node.inserts.1 as usize;
            let first_bypass = bypass_at;
            let mut hidden_wet_fault = false;
            for (offset, slot) in self.inserts[chain.clone()].iter_mut().enumerate() {
                self.delays.apply(slot.delay, wl, wr);
                let (d0, d1) = (slot.detector.0 as usize, slot.detector.1 as usize);
                let sidechain = if d0 == d1 {
                    None
                } else {
                    dl.fill(0.0);
                    dr.fill(0.0);
                    for (offset, feed) in self.detector[d0..d1].iter().enumerate() {
                        if let FeedInput::Tap { at, gain } = feed.input {
                            let at = at as usize;
                            let (l, r) = (&taps[at..at + n], &taps[at + mf..at + mf + n]);
                            let gate = self
                                .gates
                                .as_mut()
                                .map(|gates| &mut gates.detector[d0 + offset].gain);
                            let mut target = StereoSum {
                                left: dl,
                                right: dr,
                            };
                            target.feed(
                                &mut self.delays,
                                feed.delay,
                                gate,
                                (l, r),
                                gain,
                                self.fader_samples,
                            );
                        }
                    }
                    Some((&dl[..], &dr[..]))
                };
                slot.device.set_transport(transport);
                slot.device.set_tempo(transport.bpm);
                if let Some(bypass) = self.bypass.get_mut(bypass_at)
                    && bypass.slot == chain.start + offset
                {
                    bypass_at += 1;
                    let (dry_l, dry_r) = dry.split_at_mut(mf);
                    let (dry_l, dry_r) = (&mut dry_l[..n], &mut dry_r[..n]);
                    bypass.capture(wl, wr, dry_l, dry_r);
                    slot.device.process(wl, wr, sidechain, &[]);
                    hidden_wet_fault |= bypass.mix(self.fader_samples, wl, wr, dry_l, dry_r);
                } else {
                    slot.device.process(wl, wr, sidechain, &[]);
                }
            }
            let block = &mut taps[k * stride..(k + 1) * stride];
            let (pre, post) = block.split_at_mut(2 * mf);
            let (pre_l, pre_r) = pre.split_at_mut(mf);
            let (post_l, post_r) = post.split_at_mut(mf);
            let (pre_l, pre_r) = (&mut pre_l[..n], &mut pre_r[..n]);
            let (post_l, post_r) = (&mut post_l[..n], &mut post_r[..n]);
            pre_l.copy_from_slice(wl);
            pre_r.copy_from_slice(wr);
            node.fader[0].apply(self.fader_samples, wl, post_l);
            node.fader[1].apply(self.fader_samples, wr, post_r);
            let finite = pre_l
                .iter()
                .chain(pre_r.iter())
                .chain(post_l.iter())
                .chain(post_r.iter())
                .all(|x| x.is_finite());
            if hidden_wet_fault || !finite {
                pre_l.fill(0.0);
                pre_r.fill(0.0);
                post_l.fill(0.0);
                post_r.fill(0.0);
                for feed in feeds {
                    self.delays.clear(feed.delay);
                }
                for slot in &mut self.inserts[chain] {
                    slot.device.reset();
                    self.delays.clear(slot.delay);
                    let (d0, d1) = (slot.detector.0 as usize, slot.detector.1 as usize);
                    for feed in &self.detector[d0..d1] {
                        self.delays.clear(feed.delay);
                    }
                }
                self.bypass[first_bypass..bypass_at]
                    .iter_mut()
                    .for_each(BypassState::reset);
                silenced += 1;
            }
        }
        // Master is the final node (checked in prepare).
        let master = (self.nodes.len() - 1) * stride + 2 * mf;
        let l = &taps[master..master + n];
        let r = &taps[master + mf..master + mf + n];
        if let Some(gates) = &mut self.gates
            && !gates.master.is_unity()
        {
            for i in 0..n {
                let gain = gates.master.next(self.fader_samples);
                out_l[i] = l[i] * gain;
                out_r[i] = r[i] * gain;
            }
        } else {
            out_l[..n].copy_from_slice(l);
            out_r[..n].copy_from_slice(r);
        }
        RenderReport {
            frames: n as u32,
            silenced,
        }
    }

    /// Swaps matching devices and compatible compensation histories with the previous
    /// kernel. The device list must be strictly increasing and agree on identity and
    /// actual latency. On refusal nothing moves. Real-time safe.
    pub fn transfer_from(
        &mut self,
        old: &mut PreparedMixer,
        inserts: &[InsertId],
    ) -> Result<(), TransferError> {
        self.validate_transfer(old, inserts, true)?;
        self.swap_survivors(old, inserts, &[]);
        Ok(())
    }

    /// Both kernels were prepared from their canonical parameter banks. A survivor whose
    /// latency changes keeps the candidate's freshly prepared device and parameters.
    /// Only equal-latency survivors receive deltas. Nothing renders between these writes.
    pub(super) fn adopt_prepared(
        &mut self,
        old: &mut PreparedMixer,
        inserts: &[InsertId],
        params: &[(InsertId, u32, f32)],
    ) {
        if self.validate_transfer(old, inserts, false).is_ok() {
            self.swap_survivors(old, inserts, params);
            self.carry_faders(old);
        }
    }

    fn carry_faders(&mut self, old: &PreparedMixer) {
        let mut previous = old.track_nodes.iter().copied().peekable();
        for &(id, new_index) in &self.track_nodes {
            while previous.next_if(|&(old_id, _)| old_id < id).is_some() {}
            if let Some((_, old_index)) = previous.next_if(|&(old_id, _)| old_id == id) {
                let new = &mut self.nodes[new_index as usize].fader;
                let old = old.nodes[old_index as usize].fader;
                for (gain, mut carried) in new.iter_mut().zip(old) {
                    carried.retarget(gain.target, self.fader_samples);
                    *gain = carried;
                }
            }
        }
    }

    fn validate_transfer(
        &self,
        old: &PreparedMixer,
        inserts: &[InsertId],
        require_same_latency: bool,
    ) -> Result<(), TransferError> {
        if old.sample_rate.to_bits() != self.sample_rate.to_bits() {
            return Err(TransferError::SampleRate);
        }
        if old.master_latency.is_some() != self.master_latency.is_some() {
            return Err(TransferError::CompensationMode);
        }
        for (i, &id) in inserts.iter().enumerate() {
            if i > 0 && inserts[i - 1] >= id {
                return Err(TransferError::Unordered);
            }
            let (Ok(a), Ok(b)) = (find(&self.keys, id), find(&old.keys, id)) else {
                return Err(TransferError::Missing(id));
            };
            if self.inserts[a].identity != old.inserts[b].identity {
                return Err(TransferError::Identity(id));
            }
            if require_same_latency && self.inserts[a].latency != old.inserts[b].latency {
                return Err(TransferError::Latency(id));
            }
        }
        Ok(())
    }

    fn compatible_slots(&self, old: &PreparedMixer, id: InsertId) -> Option<(usize, usize)> {
        let a = find(&self.keys, id).ok()?;
        let b = find(&old.keys, id).ok()?;
        (self.inserts[a].latency == old.inserts[b].latency).then_some((a, b))
    }

    fn swap_survivors(
        &mut self,
        old: &mut PreparedMixer,
        inserts: &[InsertId],
        params: &[(InsertId, u32, f32)],
    ) {
        let mut params = params.iter().copied().peekable();
        for &id in inserts {
            let slots = self.compatible_slots(old, id);
            if let Some((a, b)) = slots {
                std::mem::swap(&mut self.inserts[a].device, &mut old.inserts[b].device);
            }
            // Deltas and survivors are both ordered by insert ID.
            while let Some((target, index, value)) = params.next_if(|(target, ..)| *target <= id) {
                if let Some((a, _)) = slots
                    && id == target
                {
                    self.inserts[a].device.set_param(index, value);
                }
            }
        }
        self.delays.transfer_from(&mut old.delays);
        if !self.paths.is_empty() {
            self.carry_controls(old, inserts);
        }
    }

    /// Compare the signal-producing prefix of each chain in execution order. A retargeted
    /// upstream send or replaced predecessor can invalidate dry history even when the
    /// slot itself, its immediate predecessor and its latency all match.
    fn carry_controls(&mut self, old: &mut Self, survivors: &[InsertId]) {
        if self.bypass.is_empty()
            && !self.gates.as_ref().is_some_and(Gates::active)
            && !old.gates.as_ref().is_some_and(Gates::active)
        {
            return;
        }
        if let (Some(new), Some(old)) = (&mut self.gates, &old.gates) {
            carry_gain(&mut new.master, old.master, self.fader_samples);
        }
        let stride = 4 * self.max_frames;
        for i in 0..self.nodes.len() {
            let node = self.nodes[i];
            let track = self.paths[i].track;
            let previous_index = old
                .track_nodes
                .binary_search_by_key(&track, |(id, _)| *id)
                .ok()
                .map(|j| old.track_nodes[j].1);
            self.paths[i].previous = previous_index;
            let previous = previous_index.map(|j| old.nodes[j as usize]);
            let audible = span(&self.audible, node.audible);
            Self::clear_discontinuous_tap_delays(audible, &self.paths, stride, &mut self.delays);
            if let (Some(previous), Some(new), Some(prior)) =
                (previous, &mut self.gates, &old.gates)
            {
                RouteSpan {
                    feeds: audible,
                    delays: &self.delays,
                    half_stride: 2 * self.max_frames,
                }
                .carry_gates(
                    span_mut(&mut new.audible, node.audible),
                    &RouteSpan {
                        feeds: span(&old.audible, previous.audible),
                        delays: &old.delays,
                        half_stride: 2 * old.max_frames,
                    },
                    span(&prior.audible, previous.audible),
                    &self.paths,
                    self.fader_samples,
                );
            }
            let mut continues = previous.is_some_and(|previous| {
                same_gate_ids(
                    self.gates
                        .as_ref()
                        .map(|gates| span(&gates.audible, node.audible)),
                    old.gates
                        .as_ref()
                        .map(|gates| span(&gates.audible, previous.audible)),
                ) && self.same_feeds(audible, old, span(&old.audible, previous.audible))
            });
            let chain = &self.inserts[node.inserts.0 as usize..node.inserts.1 as usize];
            let old_range = previous.map_or(0..0, |previous| {
                previous.inserts.0 as usize..previous.inserts.1 as usize
            });
            let old_start = old_range.start;
            let old_chain = &old.inserts[old_range];
            for (offset, slot) in chain.iter().enumerate() {
                let detector = &self.detector[slot.detector.0 as usize..slot.detector.1 as usize];
                Self::clear_discontinuous_tap_delays(
                    detector,
                    &self.paths,
                    stride,
                    &mut self.delays,
                );
                let Some(prior) = old_chain.get(offset).filter(|prior| {
                    continues
                        && slot.id == prior.id
                        && slot.identity == prior.identity
                        && slot.latency == prior.latency
                        && self.delays.frames(slot.delay) == old.delays.frames(prior.delay)
                        && survivors.binary_search(&slot.id).is_ok()
                }) else {
                    self.delays.clear(slot.delay);
                    continues = false;
                    continue;
                };
                if let (Some(new), Some(old_gates)) = (&mut self.gates, &old.gates) {
                    RouteSpan {
                        feeds: detector,
                        delays: &self.delays,
                        half_stride: 2 * self.max_frames,
                    }
                    .carry_gates(
                        span_mut(&mut new.detector, slot.detector),
                        &RouteSpan {
                            feeds: span(&old.detector, prior.detector),
                            delays: &old.delays,
                            half_stride: 2 * old.max_frames,
                        },
                        span(&old_gates.detector, prior.detector),
                        &self.paths,
                        self.fader_samples,
                    );
                }
                let a = self
                    .bypass
                    .binary_search_by_key(&(node.inserts.0 as usize + offset), |b| b.slot);
                let b = old
                    .bypass
                    .binary_search_by_key(&(old_start + offset), |b| b.slot);
                if let (Ok(a), Ok(b)) = (a, b) {
                    self.bypass[a].transfer_from(&mut old.bypass[b], self.fader_samples);
                }
                // This slot's detector is not part of its dry input, but may affect
                // its output, which is on the path of every later slot and feed.
                continues &= a.is_ok() == b.is_ok()
                    && same_gate_ids(
                        self.gates
                            .as_ref()
                            .map(|gates| span(&gates.detector, slot.detector)),
                        old.gates
                            .as_ref()
                            .map(|gates| span(&gates.detector, prior.detector)),
                    )
                    && self.same_feeds(detector, old, span(&old.detector, prior.detector));
            }
            self.paths[i].continues = continues && chain.len() == old_chain.len();
        }
    }

    // Compensation keys describe the immediate tap. Discard its transferred samples if
    // the signal producing that tap changed transitively, without touching other feeds.
    fn clear_discontinuous_tap_delays(
        feeds: &[Feed],
        paths: &[SignalPath],
        stride: usize,
        delays: &mut DelayBank,
    ) {
        for feed in feeds {
            if let FeedInput::Tap { at, .. } = feed.input
                && !paths[at as usize / stride].continues
            {
                delays.clear(feed.delay);
            }
        }
    }

    fn same_feeds(&self, feeds: &[Feed], old: &Self, old_feeds: &[Feed]) -> bool {
        feeds.len() == old_feeds.len()
            && feeds.iter().zip(old_feeds).all(|(a, b)| {
                self.delays.frames(a.delay) == old.delays.frames(b.delay)
                    && match (a.input, b.input) {
                        (
                            FeedInput::Source { index: a, gain },
                            FeedInput::Source { index: b, gain: g },
                        ) => {
                            self.sources[a as usize] == old.sources[b as usize]
                                && gain.to_bits() == g.to_bits()
                        }
                        (FeedInput::Tap { at: a, gain }, FeedInput::Tap { at: b, gain: g }) => {
                            let a = a as usize / (2 * self.max_frames);
                            let b = b as usize / (2 * old.max_frames);
                            let upstream = self.paths[a / 2];
                            upstream.continues
                                && upstream.previous.map(|index| index as usize) == Some(b / 2)
                                && a % 2 == b % 2
                                && gain.to_bits() == g.to_bits()
                        }
                        _ => false,
                    }
            })
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Clears devices, taps and histories and snaps to the admitted control targets.
    pub fn reset(&mut self) {
        for slot in &mut self.inserts {
            slot.device.reset();
        }
        self.bypass.iter_mut().for_each(BypassState::reset);
        if let Some(gates) = &mut self.gates {
            gates.reset();
        }
        for node in &mut self.nodes {
            for gain in &mut node.fader {
                *gain = FaderGain::at(gain.target);
            }
        }
        self.buffers.fill(0.0);
        self.delays.reset();
    }

    /// Forwards a parameter to an insert in the original uncompensated static renderer.
    /// Compensated kernels require a prepared install with the final parameter bank.
    pub fn set_param(
        &mut self,
        insert: InsertId,
        index: u32,
        value: f32,
    ) -> Result<(), ParamError> {
        let slot = find(&self.keys, insert)?;
        debug_assert_eq!(self.inserts[slot].id, insert);
        if self.master_latency.is_some() {
            return Err(ParamError::RequiresPreparedInstall);
        }
        self.inserts[slot].device.set_param(index, value);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferError {
    SampleRate,
    CompensationMode,
    Unordered,
    Missing(InsertId),
    Identity(InsertId),
    Latency(InsertId),
}

/// Caller-owned stereo blocks in [`PreparedMixer::source_order`], reserved off-thread by
/// [`PreparedMixer::source_buffers`]. Filling, clearing and reading never allocate.
pub struct SourceBuffers {
    ids: Vec<SourceId>,
    max_frames: usize,
    /// Per source: left `max_frames`, then right `max_frames`.
    data: Vec<f32>,
}

impl SourceBuffers {
    pub(super) fn storage_bytes(&self) -> Result<u64, PrepareError> {
        bytes(&self.ids)?
            .checked_add(bytes(&self.data)?)
            .ok_or(PrepareError::Overflow)
    }

    pub fn ids(&self) -> &[SourceId] {
        &self.ids
    }

    /// The buffer of `id`, by binary search over the canonical order.
    pub fn index(&self, id: SourceId) -> Option<usize> {
        self.ids.binary_search(&id).ok()
    }

    /// Zeroes the first `frames` of every source.
    pub fn clear(&mut self, frames: usize) {
        let frames = frames.min(self.max_frames);
        for block in self.data.chunks_exact_mut(self.max_frames) {
            block[..frames].fill(0.0);
        }
    }

    pub fn block(&self, index: usize) -> (&[f32], &[f32]) {
        let at = index * 2 * self.max_frames;
        let (l, r) = self.data[at..at + 2 * self.max_frames].split_at(self.max_frames);
        (l, r)
    }

    pub fn block_mut(&mut self, index: usize) -> (&mut [f32], &mut [f32]) {
        let at = index * 2 * self.max_frames;
        self.data[at..at + 2 * self.max_frames].split_at_mut(self.max_frames)
    }
}

trait SourceView {
    fn block(&self, index: usize) -> (&[f32], &[f32]);
}

struct Blocks<'a, 'b>(&'a [SourceBlock<'b>]);
impl SourceView for Blocks<'_, '_> {
    fn block(&self, index: usize) -> (&[f32], &[f32]) {
        (self.0[index].left, self.0[index].right)
    }
}

struct Pulled<'a>(&'a SourceBuffers, usize);
impl SourceView for Pulled<'_> {
    fn block(&self, index: usize) -> (&[f32], &[f32]) {
        let (l, r) = self.0.block(index);
        (&l[..self.1], &r[..self.1])
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

struct StereoSum<'a> {
    left: &'a mut [f32],
    right: &'a mut [f32],
}

impl StereoSum<'_> {
    fn feed(
        &mut self,
        delays: &mut DelayBank,
        delay: DelayIndex,
        gate: Option<&mut FaderGain>,
        (left, right): (&[f32], &[f32]),
        gain: f32,
        duration: u32,
    ) {
        if let Some(gate) = gate
            && !gate.is_unity()
        {
            let mut ring = delays.ring(delay);
            for i in 0..left.len() {
                let [l, r] = ring
                    .as_deref_mut()
                    .map_or([left[i], right[i]], |ring| ring.exchange(left[i], right[i]));
                let amount = gate.next(duration);
                if amount != 0.0 {
                    let gain = gain * amount;
                    self.left[i] += l * gain;
                    self.right[i] += r * gain;
                }
            }
        } else if let Some(delay) = delay {
            delays.accumulate(delay, self.left, self.right, left, right, gain);
        } else {
            accumulate(self.left, self.right, left, right, gain);
        }
    }
}

fn carry_gain(new: &mut FaderGain, mut old: FaderGain, duration: u32) {
    old.retarget(new.target, duration);
    *new = old;
}

fn span<T>(items: &[T], (start, end): (u32, u32)) -> &[T] {
    &items[start as usize..end as usize]
}

fn span_mut<T>(items: &mut [T], (start, end): (u32, u32)) -> &mut [T] {
    &mut items[start as usize..end as usize]
}

fn same_gate_ids(new: Option<&[FeedGate]>, old: Option<&[FeedGate]>) -> bool {
    new.zip(old).is_none_or(|(new, old)| {
        new.iter()
            .map(|gate| gate.id)
            .eq(old.iter().map(|gate| gate.id))
    })
}
