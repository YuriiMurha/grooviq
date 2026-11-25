//! The live mixer: a cloneable logical setup the controller mirrors, and the prepared,
//! uniquely owned install the audio thread renders. Preparation (compilation, device
//! construction, buffer reservation, survivor planning) runs on the control thread; the
//! audio thread only swaps boxes and devices at a block boundary.
//!
//! Stage limits, on purpose: no plugin delay compensation (#478) — each path is heard with
//! its own uncompensated device latency, which preparation only checks against the declared
//! bound. Device memory is opaque: only the kernel's own buffers are measured. No pan law,
//! mute or solo beyond the fixed track coefficients.

use super::{
    Budget, InsertId, InsertIdentity, Mixer, MixerTrackId, PrepareConfig, PreparedInsert,
    PreparedMixer, Source, SourceBuffers, SourceId, TrackGain, compile,
};
use crate::BLOCK;
use crate::controller::ControlError;
use grooviq_dsp::Device;
use std::collections::BTreeMap;

/// Built-in devices a mixer insert may hold. The index is the transfer identity's kind.
/// Instruments (synth, sampler) are sources, never inserts.
const KINDS: [&str; 16] = [
    "lofi_colour",
    "bitcrusher",
    "eq",
    "fattener",
    "comp",
    "limiter",
    "drive",
    "saturation",
    "drum_shaper",
    "reverb",
    "shimmer_reverb",
    "algorithmic_reverb",
    "character_delay",
    "delay",
    "ott",
    "utility",
];

pub fn insert_kinds() -> &'static [&'static str] {
    &KINDS
}

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
    /// The checked form: devices sorted by insert, params clamped and padded.
    fn checked(&self) -> Result<(Vec<InsertDevice>, Vec<u32>), ControlError> {
        let mut devices = self.devices.clone();
        devices.sort_by_key(|d| d.insert);
        if devices.windows(2).any(|w| w[0].insert == w[1].insert) {
            return Err(ControlError::Rejected);
        }
        let mut kinds = Vec::with_capacity(devices.len());
        for d in &mut devices {
            let kind = KINDS
                .iter()
                .position(|k| *k == d.kind)
                .ok_or(ControlError::Rejected)?;
            let spec = grooviq_dsp::param_spec(KINDS[kind]).ok_or(ControlError::Rejected)?;
            if d.params.iter().take(spec.len()).any(|v| !v.is_finite()) {
                return Err(ControlError::Rejected);
            }
            d.params = spec
                .iter()
                .enumerate()
                .map(|(i, &(min, max, def))| d.params.get(i).map_or(def, |v| v.clamp(min, max)))
                .collect();
            kinds.push(kind as u32);
        }
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

    /// Topology and device descriptions only; constructs no device.
    pub(crate) fn validate(&self) -> Result<(), ControlError> {
        if let Self::Routed(mix) = self {
            mix.checked()?;
            compile(&mix.mixer, budget()).map_err(|_| ControlError::Rejected)?;
        }
        Ok(())
    }
}

fn budget() -> Budget {
    Budget {
        block_frames: BLOCK as u32,
        ..Budget::default()
    }
}

fn construct(kind: &str, seed: u64, rate: f32) -> Option<Box<dyn Device>> {
    // The persisted seed is u64. Fold both halves for devices with a 32-bit RNG,
    // preserving existing low-only seeds while making high bits affect their stream.
    let narrow = seed as u32 ^ (seed >> 32) as u32;
    match kind {
        "bitcrusher" => Some(Box::new(grooviq_dsp::bitcrusher::Bitcrusher::with_seed(
            rate, narrow,
        ))),
        "lofi_colour" => Some(Box::new(grooviq_dsp::lofi_colour::LofiColour::with_seed(
            rate, narrow,
        ))),
        "character_delay" => Some(Box::new(
            grooviq_dsp::character_delay::CharacterDelay::with_seed(rate, seed),
        )),
        "shimmer_reverb" => Some(Box::new(
            grooviq_dsp::shimmer_reverb::ShimmerReverb::with_seed(rate, seed),
        )),
        "algorithmic_reverb" => Some(Box::new(
            grooviq_dsp::algorithmic_reverb::AlgorithmicReverb::with_seed(rate, seed),
        )),
        _ => grooviq_dsp::create(kind, rate),
    }
}

/// A prepared routed mix: the kernel, its pulled source buffers, and the checked device
/// descriptions it was built from (to plan the next transfer). Dropping it frees devices:
/// off the audio thread only.
pub(crate) struct LiveMix {
    pub(crate) kernel: PreparedMixer,
    pub(crate) sources: SourceBuffers,
}

/// One owning queue transaction: install `live` (or go back to LegacyDirect with `None`)
/// if the graph still runs revision `base` at `generation`. Afterwards it holds whatever
/// leaves the graph (the old mix, or the refused new one) and retires through the trash.
pub(crate) struct MixInstall {
    pub(crate) generation: u64,
    pub(crate) base: u64,
    pub(crate) revision: u64,
    pub(crate) live: Option<Box<LiveMix>>,
    /// Surviving inserts, strictly increasing.
    pub(crate) transfers: Vec<InsertId>,
    /// Survivor parameters that differ from the previous setup, applied after the swap:
    /// `(insert, index, value)`. Unchanged survivors receive nothing, so nothing resets.
    pub(crate) params: Vec<ParamDelta>,
    pub(crate) installed: bool,
}

/// Prepares `setup` for `rate`, off-thread, plus one engine-owned source per channel
/// instance in `aliases` (`SourceId::Instance`) on the given track: a channel that is removed
/// keeps that route while it fades, whatever later happens to its ID. `LegacyDirect`
/// prepares nothing.
pub(crate) fn prepare(
    setup: &MixSetup,
    rate: f32,
    aliases: &[(u64, MixerTrackId)],
) -> Result<Option<Box<LiveMix>>, ControlError> {
    let MixSetup::Routed(mix) = setup else {
        return Ok(None);
    };
    let (devices, kinds) = mix.checked()?;
    let mut mixer = mix.mixer.clone();
    mixer
        .sources
        .extend(aliases.iter().map(|&(instance, destination)| Source {
            id: SourceId::Instance(instance),
            destination,
        }));
    let compiled = compile(&mixer, budget()).map_err(|_| ControlError::Rejected)?;
    let declared: BTreeMap<_, _> = mix
        .mixer
        .tracks
        .iter()
        .flat_map(|t| &t.inserts)
        .map(|i| (i.id, i.latency_frames))
        .collect();
    let mut bindings = Vec::with_capacity(devices.len());
    for (d, kind) in devices.iter().zip(kinds) {
        let mut device = construct(&d.kind, d.seed, rate).ok_or(ControlError::Rejected)?;
        for (index, value) in (0u32..).zip(&d.params) {
            device.set_param(index, *value);
        }
        // Uncompensated stage: the device's actual latency must stay within its declaration.
        if device.latency_frames() > declared[&d.insert] {
            return Err(ControlError::Rejected);
        }
        bindings.push(PreparedInsert::with_identity(
            d.insert,
            device,
            InsertIdentity { kind, seed: d.seed },
        ));
    }
    let kernel = PreparedMixer::prepare(
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
    Ok(Some(Box::new(LiveMix { kernel, sources })))
}

/// A survivor parameter that differs from the previous setup: `(insert, index, value)`.
pub(crate) type ParamDelta = (InsertId, u32, f32);

/// Plans which devices of `old` (the mirrored previous setup) survive into `new`: same
/// insert, kind and seed (the rate and the actual identities are checked again on install),
/// and which of their parameters change. Off-thread.
pub(crate) fn plan(
    old: &MixSetup,
    new: &MixSetup,
) -> Result<(Vec<InsertId>, Vec<ParamDelta>), ControlError> {
    let (MixSetup::Routed(old), MixSetup::Routed(new)) = (old, new) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (old_devices, _) = old.checked()?;
    let (new_devices, _) = new.checked()?;
    let mut transfers = Vec::new();
    let mut params = Vec::new();
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
    Ok((transfers, params))
}

impl LiveMix {
    /// Takes the planned survivors out of `old`, then applies their parameter deltas. If the
    /// plan does not match the actual kernels nothing moves and the fresh devices (already
    /// at the new parameters) play. Real-time safe.
    pub(crate) fn adopt(
        &mut self,
        old: &mut LiveMix,
        transfers: &[InsertId],
        params: &[ParamDelta],
    ) {
        if self
            .kernel
            .transfer_from(&mut old.kernel, transfers)
            .is_ok()
        {
            for &(insert, index, value) in params {
                let _ = self.kernel.set_param(insert, index, value);
            }
        }
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
                let reference = render(oracle(kind, seed), params);
                let actual = render(construct(kind, seed, 48000.0).unwrap(), params);
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
