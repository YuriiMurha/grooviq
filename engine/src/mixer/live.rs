//! The live mixer: a cloneable logical setup the controller mirrors, and the prepared,
//! uniquely owned install the audio thread renders. Preparation (compilation, device
//! construction, buffer reservation, survivor planning) runs on the control thread; the
//! audio thread only swaps boxes and devices at a block boundary.
//!
//! The kernel prepares compensation from the devices' actual latency after preset setup.
//! Declared bounds still govern admission. No pan law, mute or solo beyond the fixed
//! track coefficients.

use super::compile::{ROUTING_BYTES_PER_ITEM, admit};
use super::memory::{MixerMemory, Reservation};
use super::{
    Budget, InsertId, InsertIdentity, Mixer, MixerTrackId, PrepareConfig, PreparedInsert,
    PreparedMixer, Source, SourceBuffers, SourceId, TrackGain, compile,
};
use crate::BLOCK;
use crate::controller::ControlError;
use grooviq_dsp::Device;

/// Built-in effect ids, in the registry's order.
pub fn insert_kinds() -> &'static [&'static str] {
    &grooviq_dsp::EFFECTS
}

/// Declares one built-in insert without constructing DSP. The registry bounds use fixed
/// state sizes or rounded durations in frames, including power-of-two ring capacities.
/// Limiter lookahead grows by the same conversion. The native upper rate therefore bounds
/// these payloads and latencies for every admitted rate and parameter bank.
///
/// The controller charges this registry bound, including constructor scratch, to each
/// prepared device for its entire lifetime. Allocator overhead is outside the payload bound.
pub fn describe_builtin_insert(insert: InsertId, kind: &str) -> Option<super::Insert> {
    let (_, highest_rate) = grooviq_dsp::sample_rate_range(kind)?;
    Some(super::Insert {
        id: insert,
        latency_frames: grooviq_dsp::max_latency_frames(kind, highest_rate)?,
        memory_bytes: grooviq_dsp::max_memory_bytes(kind, highest_rate).ok()?,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnsupportedInsertRate {
    kind: String,
    rate: f32,
}

impl std::fmt::Display for UnsupportedInsertRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Mixer insert {} cannot run at {} Hz",
            self.kind, self.rate
        )
    }
}

impl std::error::Error for UnsupportedInsertRate {}

/// How the engine's sources reach the output.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum MixSetup {
    /// The original fixed sum: tone, synth, each channel in order, then song audio, each
    /// added to the master in that order. Byte-identical to the engine before mixers.
    #[default]
    LegacyDirect,
    /// Keyed sources into a compiled mixer. Sources the mixer does not name are still
    /// rendered (their voices and time advance) but are not heard.
    Routed(RoutedMix),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoutedMix {
    pub mixer: Mixer,
    pub gains: Vec<TrackGain>,
    /// Exactly one per insert in `mixer`.
    pub devices: Vec<InsertDevice>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InsertDevice {
    pub insert: InsertId,
    /// One of [`insert_kinds`].
    pub kind: String,
    /// Part of the device's identity; only seeded devices use it.
    pub seed: u64,
    /// Indexed like the device's `SPEC`; missing trailing values take defaults and values
    /// are clamped, as channel parameters are.
    pub params: Vec<f32>,
}

impl RoutedMix {
    // Checked size of the normalized copies, independent of caller Vec capacities and
    // ignored trailing parameters. Reject excess device entries before allocating.
    fn normalized_bytes(&self) -> Result<u64, ControlError> {
        let inserts = self.mixer.tracks.iter().try_fold(0usize, |n, t| {
            n.checked_add(t.inserts.len()).ok_or(ControlError::Rejected)
        })?;
        if inserts != self.devices.len() || self.gains.len() > self.mixer.tracks.len() {
            return Err(ControlError::Rejected);
        }
        self.devices.iter().try_fold(0, |bytes, d| {
            let spec = grooviq_dsp::param_spec(&d.kind).ok_or(ControlError::Rejected)?;
            sum([
                bytes,
                size_of::<InsertDevice>() as u64,
                size_of::<u32>() as u64,
                d.kind.len() as u64,
                product(spec.len() as u64, size_of::<f32>() as u64)?,
            ])
        })
    }

    /// The checked form: devices sorted by insert, params clamped and padded.
    fn checked(&self) -> Result<(Vec<InsertDevice>, Vec<u32>), ControlError> {
        let mut devices = Vec::with_capacity(self.devices.len());
        for d in &self.devices {
            let kind = insert_kinds()
                .iter()
                .copied()
                .find(|&k| k == d.kind)
                .ok_or(ControlError::Rejected)?;
            let spec = grooviq_dsp::param_spec(kind).ok_or(ControlError::Rejected)?;
            if d.params.iter().take(spec.len()).any(|v| !v.is_finite()) {
                return Err(ControlError::Rejected);
            }
            devices.push(InsertDevice {
                insert: d.insert,
                kind: kind.into(),
                seed: d.seed,
                params: spec
                    .iter()
                    .enumerate()
                    .map(|(i, &(min, max, def))| d.params.get(i).map_or(def, |v| v.clamp(min, max)))
                    .collect(),
            });
        }
        devices.sort_unstable_by_key(|d| d.insert);
        if devices.windows(2).any(|w| w[0].insert == w[1].insert) {
            return Err(ControlError::Rejected);
        }
        let kinds = devices
            .iter()
            .map(|d| insert_kinds().iter().position(|k| *k == d.kind).unwrap() as u32)
            .collect();
        let mut declared = 0;
        for t in &self.mixer.tracks {
            for i in &t.inserts {
                declared += 1;
                devices
                    .binary_search_by_key(&i.id, |d| d.insert)
                    .map_err(|_| ControlError::Rejected)?;
            }
        }
        if declared != devices.len()
            || self
                .mixer
                .sources
                .iter()
                .any(|s| matches!(s.id, SourceId::Instance(_)))
        {
            return Err(ControlError::Rejected);
        }
        Ok((devices, kinds))
    }
}

impl MixSetup {
    /// Checks this mix against the actual engine rate before replacing a project or exporting.
    pub fn check_sample_rate(&self, rate: f32) -> Result<(), UnsupportedInsertRate> {
        if let Self::Routed(mix) = self
            && let Some(device) = mix.devices.iter().find(|d| {
                grooviq_dsp::sample_rate_range(&d.kind)
                    .is_none_or(|(min, max)| !(min..=max).contains(&rate))
            })
        {
            return Err(UnsupportedInsertRate {
                kind: device.kind.clone(),
                rate,
            });
        }
        Ok(())
    }

    /// Where the mix sends `source`, if it names it.
    pub(crate) fn destination(&self, source: SourceId) -> Option<MixerTrackId> {
        match self {
            Self::LegacyDirect => None,
            Self::Routed(mix) => mix
                .mixer
                .sources
                .iter()
                .find(|s| s.id == source)
                .map(|s| s.destination),
        }
    }

    /// [`Self::validate`] plus the fade routes a graph built for `channels` adds (one per
    /// routed channel), so a state admitted here also builds: compile budgets included.
    /// Checks a logical mix and reserves one route per channel for its eventual fade.
    /// Construction and verification of actual devices still happen on live preparation.
    pub fn validate_for_channels(
        &self,
        channels: impl IntoIterator<Item = crate::ChannelId>,
    ) -> Result<(), ControlError> {
        if let Self::Routed(mix) = self {
            mix.normalized_bytes()?;
            mix.checked()?;
            let mut mixer = mix.mixer.clone();
            for (alias, c) in (0u64..).zip(channels) {
                if let Some(destination) = self.destination(SourceId::Channel(u64::from(c.0))) {
                    mixer.sources.push(Source {
                        id: SourceId::Instance(alias),
                        destination,
                    });
                }
            }
            compile(&mixer, budget()).map_err(|_| ControlError::Rejected)?;
        }
        Ok(())
    }

    /// A routed mix names every channel and every playlist track the song's audio uses;
    /// LegacyDirect hears everything. Otherwise a source could be admitted and never heard.
    pub(crate) fn covers(
        &self,
        channels: &[crate::ChannelSetup],
        song: &crate::SongData,
    ) -> Result<(), ControlError> {
        if let Self::LegacyDirect = self {
            return Ok(());
        }
        let named = |id| self.destination(id).is_some();
        if channels
            .iter()
            .all(|c| named(SourceId::Channel(u64::from(c.id.0))))
            && song
                .audio
                .entry_routes()
                .all(|t| t.is_some_and(|t| named(SourceId::AudioTrack(t))))
        {
            Ok(())
        } else {
            Err(ControlError::Rejected)
        }
    }

    pub(crate) fn has_track(&self, track: MixerTrackId) -> bool {
        matches!(self, Self::Routed(mix) if mix.mixer.tracks.iter().any(|t| t.id == track))
    }
}

fn budget() -> Budget {
    Budget {
        block_frames: BLOCK as u32,
        ..Budget::default()
    }
}

fn construct(device: &InsertDevice, rate: f32) -> Result<Box<dyn Device>, ControlError> {
    grooviq_dsp::create_insert(&device.kind, rate, device.seed, &device.params)
        .map_err(|_| ControlError::Rejected)
}

/// The routed kernel, pulled inputs and their shared memory charge. The charge is last
/// so Rust drops it after the allocations. The callback only moves this owning box.
pub(crate) struct LiveMix {
    pub(crate) kernel: PreparedMixer,
    pub(crate) sources: SourceBuffers,
    _charge: Reservation,
}

/// One owning queue transaction: install `live` (or go back to LegacyDirect with `None`)
/// if the graph still runs revision `base` at `generation`. Afterwards it holds whatever
/// leaves the graph (the old mix, or the refused new one) and retires through the trash.
pub(crate) struct MixInstall {
    pub(crate) generation: u64,
    pub(crate) base: u64,
    pub(crate) revision: u64,
    pub(crate) live: Option<Box<LiveMix>>,
    pub(crate) changes: MixChanges,
    pub(crate) installed: bool,
}

/// Prepares `setup` for `rate`, off-thread, plus one engine-owned source per channel
/// instance in `aliases` (`SourceId::Instance`) on the given track: a channel that is removed
/// keeps that route while it fades, whatever later happens to its ID. `LegacyDirect`
/// prepares nothing.
pub(crate) fn prepare(
    setup: &MixSetup,
    rate: f32,
    aliases: impl Iterator<Item = (u64, MixerTrackId)> + Clone,
    memory: &MixerMemory,
) -> Result<Option<Box<LiveMix>>, ControlError> {
    let MixSetup::Routed(mix) = setup else {
        return Ok(None);
    };
    setup
        .check_sample_rate(rate)
        .map_err(|_| ControlError::Rejected)?;
    let (peak, device_bytes) = preparation_bound(mix, aliases.clone().count() as u64)?;
    let mut charge = memory.reserve(peak)?;
    let (devices, kinds) = mix.checked()?;
    let mut mixer = mix.mixer.clone();
    mixer
        .sources
        .extend(aliases.map(|(instance, destination)| Source {
            id: SourceId::Instance(instance),
            destination,
        }));
    let compiled = compile(&mixer, budget()).map_err(|_| ControlError::Rejected)?;
    let mut bindings = Vec::with_capacity(devices.len());
    for (d, kind) in devices.iter().zip(kinds) {
        let device = construct(d, rate)?;
        bindings.push(PreparedInsert::with_identity(
            d.insert,
            device,
            InsertIdentity { kind, seed: d.seed },
        ));
    }
    let kernel = PreparedMixer::prepare_compensated(
        compiled,
        bindings,
        &mix.gains,
        PrepareConfig {
            sample_rate: rate,
            max_frames: BLOCK,
            max_bytes: budget().memory_bytes,
        },
    )
    .map_err(|_| ControlError::Rejected)?;
    let sources = kernel
        .source_buffers()
        .map_err(|_| ControlError::BuildFailed)?;
    let retained = sum([
        device_bytes,
        kernel.resources().audio_bytes,
        kernel.resources().table_bytes,
        sources
            .storage_bytes()
            .map_err(|_| ControlError::Rejected)?,
        size_of::<LiveMix>() as u64,
    ])?;
    drop((mixer, devices));
    charge.shrink(retained);
    Ok(Some(Box::new(LiveMix {
        kernel,
        sources,
        _charge: charge,
    })))
}

// The compiler allowance covers topology copies, adjacency, sort scratch and the
// conservative delay/audio bound. Reserve another routing-table allowance for the
// simultaneous compiled topology, planner work/descriptors and kernel maps/tables.
// Count normalized banks, pulled storage and the fixed stereo scratch separately.
// This bounds requested payload, not allocator bookkeeping or caller-owned sessions,
// channel devices, graph/timeline shells or fixed-size queue slots.
fn preparation_bound(mix: &RoutedMix, aliases: u64) -> Result<(u64, u64), ControlError> {
    let normalized = mix.normalized_bytes()?;
    let device_bytes = mix.devices.iter().try_fold(0, |total, d| {
        let (_, highest) = grooviq_dsp::sample_rate_range(&d.kind).ok_or(ControlError::Rejected)?;
        let bytes =
            grooviq_dsp::max_memory_bytes(&d.kind, highest).map_err(|_| ControlError::Rejected)?;
        sum([total, bytes])
    })?;
    let topology =
        admit(&mix.mixer, budget(), aliases, device_bytes).map_err(|_| ControlError::Rejected)?;
    let routing_tables = product(
        sum([topology.nodes, topology.edges])?,
        ROUTING_BYTES_PER_ITEM,
    )?;
    let source_count = sum([mix.mixer.sources.len() as u64, aliases])?;
    let stereo_block = (2 * BLOCK * size_of::<f32>()) as u64;
    let pulls = product(
        source_count,
        sum([stereo_block, size_of::<SourceId>() as u64])?,
    )?;
    let peak = sum([
        topology.memory_bytes,
        routing_tables,
        normalized,
        pulls,
        2 * stereo_block,
        size_of::<LiveMix>() as u64,
    ])?;
    Ok((peak, device_bytes))
}

fn sum(values: impl IntoIterator<Item = u64>) -> Result<u64, ControlError> {
    values
        .into_iter()
        .try_fold(0u64, |a, b| a.checked_add(b).ok_or(ControlError::Rejected))
}

fn product(a: u64, b: u64) -> Result<u64, ControlError> {
    a.checked_mul(b).ok_or(ControlError::Rejected)
}

/// A survivor parameter that differs from the previous setup: `(insert, index, value)`.
type ParamDelta = (InsertId, u32, f32);

#[derive(Default)]
pub(crate) struct MixChanges {
    // Both lists are ordered by insert ID.
    transfers: Vec<InsertId>,
    params: Vec<ParamDelta>,
    _charge: Option<Reservation>,
}

/// Plans which devices of `old` (the mirrored previous setup) survive into `new`: same
/// insert, kind and seed (the rate and the actual identities are checked again on install),
/// and which of their parameters change. Off-thread.
pub(crate) fn plan(
    old: &MixSetup,
    new: &MixSetup,
    memory: &MixerMemory,
) -> Result<MixChanges, ControlError> {
    let (MixSetup::Routed(old), MixSetup::Routed(new)) = (old, new) else {
        return Ok(MixChanges::default());
    };
    let inserts = new.devices.len();
    let max_params = new.devices.iter().try_fold(0usize, |total, d| {
        let spec = grooviq_dsp::param_spec(&d.kind).ok_or(ControlError::Rejected)?;
        total.checked_add(spec.len()).ok_or(ControlError::Rejected)
    })?;
    let mut charge = memory.reserve(sum([
        old.normalized_bytes()?,
        new.normalized_bytes()?,
        product(inserts as u64, size_of::<InsertId>() as u64)?,
        product(max_params as u64, size_of::<ParamDelta>() as u64)?,
    ])?)?;
    let (old_devices, _) = old.checked()?;
    let (new_devices, _) = new.checked()?;
    let mut transfers = Vec::with_capacity(inserts);
    let mut params = Vec::with_capacity(max_params);
    // Both sorted by insert, so `transfers` is strictly increasing.
    for d in &new_devices {
        let Ok(at) = old_devices.binary_search_by_key(&d.insert, |o| o.insert) else {
            continue;
        };
        let o = &old_devices[at];
        if o.kind != d.kind || o.seed != d.seed {
            continue;
        }
        transfers.push(d.insert);
        for (index, (a, b)) in (0u32..).zip(o.params.iter().zip(&d.params)) {
            if a.to_bits() != b.to_bits() {
                params.push((d.insert, index, *b));
            }
        }
    }
    drop((old_devices, new_devices));
    charge.shrink(sum([
        product(transfers.capacity() as u64, size_of::<InsertId>() as u64)?,
        product(params.capacity() as u64, size_of::<ParamDelta>() as u64)?,
    ])?);
    Ok(MixChanges {
        transfers,
        params,
        _charge: Some(charge),
    })
}

impl LiveMix {
    /// Takes compatible device and delay histories out of `old`, applying parameter deltas
    /// only to transferred devices. The other inserts use their prepared parameter banks.
    /// Real-time safe.
    pub(crate) fn adopt(&mut self, old: &mut LiveMix, changes: &MixChanges) {
        // Same kind and sample rate imply the same registry payload bound. Only those
        // devices, and histories of equal capacity, can trade places with the old mix.
        self.kernel
            .adopt_prepared(&mut old.kernel, &changes.transfers, &changes.params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oracle(kind: &str, seed: u64) -> Box<dyn Device> {
        // Independent expected mapping: XOR the two 32-bit halves for 32-bit RNGs.
        let narrow = (seed % 4_294_967_296) as u32 ^ (seed / 4_294_967_296) as u32;
        match kind {
            "bitcrusher" => Box::new(grooviq_dsp::bitcrusher::Bitcrusher::with_seed(
                48000.0, narrow,
            )),
            "lofi_colour" => Box::new(grooviq_dsp::lofi_colour::LofiColour::with_seed(
                48000.0, narrow,
            )),
            "shimmer_reverb" => Box::new(grooviq_dsp::shimmer_reverb::ShimmerReverb::with_seed(
                48000.0, seed,
            )),
            "character_delay" => Box::new(grooviq_dsp::character_delay::CharacterDelay::with_seed(
                48000.0, seed,
            )),
            "algorithmic_reverb" => Box::new(
                grooviq_dsp::algorithmic_reverb::AlgorithmicReverb::with_seed(48000.0, seed),
            ),
            _ => unreachable!(),
        }
    }

    fn render(mut device: Box<dyn Device>, params: &[(u32, f32)]) -> Vec<u32> {
        for &(index, value) in params {
            device.set_param(index, value);
        }
        let mut out = Vec::with_capacity(32_000);
        for block in 0..250 {
            let mut l = std::array::from_fn::<_, 64, _>(|i| {
                let frame = block * 64 + i;
                ((frame * 7919 % 2003) as f32 / 2003.0 - 0.5) * 0.4
            });
            let mut r = std::array::from_fn::<_, 64, _>(|i| {
                let frame = block * 64 + i;
                ((frame * 104729 % 1999) as f32 / 1999.0 - 0.3) * 0.25
            });
            device.process(&mut l, &mut r, None, &[]);
            for (a, b) in l.into_iter().zip(r) {
                out.extend([a.to_bits(), b.to_bits()]);
            }
        }
        out
    }

    #[test]
    fn built_in_declarations_cover_sampled_native_rates_and_widest_limiter_latency() {
        for kind in insert_kinds() {
            let declared = describe_builtin_insert(InsertId(1), kind).unwrap();
            for rate in [
                8000.0, 22050.0, 44100.0, 48000.0, 96000.0, 192000.0, 384000.0,
            ] {
                if let Some(latency) = grooviq_dsp::max_latency_frames(kind, rate) {
                    assert!(declared.latency_frames >= latency, "{kind} at {rate}");
                    let payload = grooviq_dsp::max_memory_bytes(kind, rate).unwrap();
                    assert!(declared.memory_bytes >= payload, "{kind} at {rate}");
                }
            }
        }
        // The limiter's widest 1.5 ms lookahead is 576 frames, with one frame on the input.
        assert_eq!(
            describe_builtin_insert(InsertId(99), "limiter")
                .unwrap()
                .latency_frames,
            575
        );
    }

    #[test]
    fn insert_factory_honors_every_seeded_device_and_both_seed_halves() {
        let cases: [(&str, &[(u32, f32)]); 5] = [
            (
                "bitcrusher",
                &[(0, 4.0), (1, 12.0), (2, 0.5), (3, 0.4), (4, 1.0), (5, 0.8)],
            ),
            (
                "lofi_colour",
                &[(0, 1.0), (1, 0.7), (2, 0.5), (3, 0.2), (5, 0.3), (7, 0.0)],
            ),
            ("shimmer_reverb", &[(0, 1.0), (4, 0.8), (5, 0.8)]),
            (
                "character_delay",
                &[(0, 0.0), (2, 2.0), (3, 0.25), (7, 1.0), (9, 2.0), (10, 1.0)],
            ),
            ("algorithmic_reverb", &[]),
        ];
        for (kind, params) in cases {
            let mut expected = Vec::new();
            for seed in [123, 456, (1_u64 << 32) | 123] {
                let mut full: Vec<_> = grooviq_dsp::param_spec(kind)
                    .unwrap()
                    .iter()
                    .map(|&(_, _, default)| default)
                    .collect();
                for &(index, value) in params {
                    full[index as usize] = value;
                }
                let bank: Vec<_> = (0u32..).zip(full.iter().copied()).collect();
                let reference = render(oracle(kind, seed), &bank);
                let device = InsertDevice {
                    insert: InsertId(1),
                    kind: kind.into(),
                    seed,
                    params: full,
                };
                let actual = render(construct(&device, 48000.0).unwrap(), &[]);
                assert_eq!(
                    actual.iter().zip(&reference).position(|(a, b)| a != b),
                    None,
                    "{kind} must use the requested seed {seed}"
                );
                expected.push(reference);
            }
            assert!(
                expected[0] != expected[1],
                "{kind} fixture must expose seed changes"
            );
            // Native u64 devices may intentionally map several seeds to one phase.
            // The host's explicit 32-bit fold must retain the high-half contribution.
            if matches!(kind, "bitcrusher" | "lofi_colour") {
                assert!(
                    expected[0] != expected[2],
                    "{kind} must use both seed halves"
                );
            }
        }
    }
}
