//! DSP: built-in synth, sampler and effects. Parameter ranges live in each device module's `SPEC` table;
//! the index there is the `set_param` index here.
//!
//! Rules for every device:
//! - Real-time safe: no allocation, locks or I/O inside `process`/`set_param` (allocate in `new`).
//! - Works at any sample rate 22050..192000 and any block length 1..=MAX_BLOCK.
//! - Parameter changes are smoothed where they would click (gain, cutoff, mix).
//! - Compiles for `wasm32-unknown-unknown` (std only, no threads, no time, no rand crates).

pub mod api;
pub use api::*;

mod fft;
mod insert_memory;
pub use insert_memory::max_memory_bytes;
mod oversampling;
pub mod saturation;
mod util;
mod wavetable;

pub mod algorithmic_reverb;
pub mod bitcrusher;
pub mod character_delay;
pub mod comp;
pub mod delay;
pub mod drive;
pub mod drum_shaper;
pub mod eq;
pub mod fattener;
pub mod limiter;
pub mod lofi_colour;
pub mod ott;
pub mod reverb;
pub mod sampler;
pub mod shimmer_reverb;
pub mod synth;
pub mod utility;

/// `(min, max, default)` of every parameter of a device, indexed like `set_param`
/// (`SPEC` index order). Useful for clamping, defaults and UI ranges.
pub fn param_spec(id: &str) -> Option<&'static [(f32, f32, f32)]> {
    Some(match id {
        "lofi_colour" => &lofi_colour::SPEC,
        "bitcrusher" => &bitcrusher::SPEC,
        "synth" => &synth::SPEC,
        "sampler" => &sampler::SPEC,
        "eq" => &eq::SPEC,
        "fattener" => &fattener::SPEC,
        "comp" => &comp::SPEC,
        "limiter" => &limiter::SPEC,
        "drive" => &drive::SPEC,
        "saturation" => &saturation::SPEC,
        "drum_shaper" => &drum_shaper::SPEC,
        "reverb" => &reverb::SPEC,
        "shimmer_reverb" => &shimmer_reverb::SPEC,
        "algorithmic_reverb" => &algorithmic_reverb::SPEC,
        "character_delay" => &character_delay::SPEC,
        "delay" => &delay::SPEC,
        "ott" => &ott::SPEC,
        "utility" => &utility::SPEC,
        _ => return None,
    })
}

/// Creates an instrument (`"synth"` or `"sampler"`) or one of [`EFFECTS`].
/// A sampler stays silent until the owner of the concrete [`sampler::Sampler`] calls `set_sample`.
pub fn create(id: &str, sample_rate: f32) -> Option<Box<dyn Device>> {
    Some(match id {
        "lofi_colour" => Box::new(lofi_colour::LofiColour::new(sample_rate)),
        "bitcrusher" => Box::new(bitcrusher::Bitcrusher::new(sample_rate)),
        "synth" => Box::new(synth::Synth::new(sample_rate)),
        "sampler" => Box::new(sampler::Sampler::new(sample_rate)),
        "eq" => Box::new(eq::Eq::new(sample_rate)),
        "fattener" => Box::new(fattener::Fattener::new(sample_rate)),
        "comp" => Box::new(comp::Comp::new(sample_rate)),
        "limiter" => Box::new(limiter::Limiter::new(sample_rate)),
        "drive" => Box::new(drive::Drive::new(sample_rate)),
        "saturation" => Box::new(saturation::Saturation::new(sample_rate)),
        "drum_shaper" => Box::new(drum_shaper::DrumShaper::new(sample_rate)),
        "reverb" => Box::new(reverb::Reverb::new(sample_rate)),
        "shimmer_reverb" => Box::new(shimmer_reverb::ShimmerReverb::new(sample_rate)),
        "algorithmic_reverb" => Box::new(algorithmic_reverb::AlgorithmicReverb::new(sample_rate)),
        "character_delay" => Box::new(character_delay::CharacterDelay::new(sample_rate)),
        "delay" => Box::new(delay::Delay::new(sample_rate)),
        "ott" => Box::new(ott::Ott::new(sample_rate)),
        "utility" => Box::new(utility::Utility::new(sample_rate)),
        _ => return None,
    })
}

/// Built-in effects supported as mixer inserts, sorted by id.
pub const EFFECTS: [&str; 16] = [
    "algorithmic_reverb",
    "bitcrusher",
    "character_delay",
    "comp",
    "delay",
    "drive",
    "drum_shaper",
    "eq",
    "fattener",
    "limiter",
    "lofi_colour",
    "ott",
    "reverb",
    "saturation",
    "shimmer_reverb",
    "utility",
];

type SteppedControl = fn(u32) -> bool;
type LatencyBound = fn(f32) -> u32;

struct InsertSpec {
    ids: &'static [&'static str],
    stepped: SteppedControl,
    latency: LatencyBound,
}

fn continuous(_: u32) -> bool {
    false
}

fn zero_latency(_: f32) -> u32 {
    0
}

fn insert_spec(id: &str) -> Option<InsertSpec> {
    let (ids, stepped, latency): (&[&str], SteppedControl, LatencyBound) = match id {
        "algorithmic_reverb" => (&algorithmic_reverb::PARAM_IDS, continuous, zero_latency),
        "bitcrusher" => (&bitcrusher::PARAM_IDS, continuous, zero_latency),
        "character_delay" => (
            &character_delay::PARAM_IDS,
            |i| matches!(i, 0 | 1 | 4),
            zero_latency,
        ),
        "comp" => (&comp::PARAM_IDS, |i| i == 7, zero_latency),
        "delay" => (&delay::PARAM_IDS, |i| matches!(i, 0 | 1 | 4), zero_latency),
        "drive" => (
            &drive::PARAM_IDS,
            |i| matches!(i, 0 | 5),
            |_| drive::MAX_LATENCY_FRAMES,
        ),
        "drum_shaper" => (&drum_shaper::PARAM_IDS, continuous, zero_latency),
        "eq" => (&eq::PARAM_IDS, eq::is_stepped, zero_latency),
        "fattener" => (&fattener::PARAM_IDS, continuous, zero_latency),
        "limiter" => (&limiter::PARAM_IDS, continuous, limiter::max_latency_frames),
        "lofi_colour" => (&lofi_colour::PARAM_IDS, continuous, zero_latency),
        "ott" => (&ott::PARAM_IDS, continuous, zero_latency),
        "reverb" => (&reverb::PARAM_IDS, continuous, zero_latency),
        "saturation" => (
            &saturation::PARAM_IDS,
            |i| i == 0,
            |_| saturation::MAX_LATENCY_FRAMES,
        ),
        "shimmer_reverb" => (&shimmer_reverb::PARAM_IDS, continuous, zero_latency),
        "utility" => (&utility::PARAM_IDS, |i| i == 4, zero_latency),
        _ => return None,
    };
    Some(InsertSpec {
        ids,
        stepped,
        latency,
    })
}

/// Stable parameter ids in [`param_spec`] order for a built-in effect.
pub fn param_ids(id: &str) -> Option<&'static [&'static str]> {
    Some(insert_spec(id)?.ids)
}

/// Whether the effect interprets the parameter as a switch or discrete choice.
pub fn is_stepped(id: &str, index: u32) -> Option<bool> {
    let spec = insert_spec(id)?;
    spec.ids.get(index as usize)?;
    Some((spec.stepped)(index))
}

type PresetLookup = fn(&str) -> Option<Vec<(u32, f32)>>;

fn preset_bank(id: &str) -> Option<(&'static [&'static str], PresetLookup)> {
    Some(match id {
        "algorithmic_reverb" => (algorithmic_reverb::PRESETS, algorithmic_reverb::preset),
        "bitcrusher" => (&bitcrusher::PRESETS, bitcrusher::preset),
        "character_delay" => (&character_delay::PRESETS, character_delay::preset),
        "drum_shaper" => (drum_shaper::PRESETS, drum_shaper::preset),
        "fattener" => (&fattener::PRESETS, fattener::preset),
        "lofi_colour" => (&lofi_colour::PRESETS, lofi_colour::preset),
        "ott" => (&ott::PRESETS, ott::preset),
        "saturation" => (&saturation::PRESETS, saturation::preset),
        "shimmer_reverb" => (&shimmer_reverb::PRESETS, shimmer_reverb::preset),
        _ => return None,
    })
}

/// Factory preset names for a built-in effect, or an empty list if it has none.
pub fn presets(id: &str) -> &'static [&'static str] {
    preset_bank(id).map_or(&[], |(names, _)| names)
}

/// Factory preset assignments by [`param_spec`] index. Allocates off the audio thread.
pub fn preset(id: &str, name: &str) -> Option<Vec<(u32, f32)>> {
    let (_, lookup) = preset_bank(id)?;
    lookup(name)
}

/// Upper bound on an effect's [`Device::latency_frames`] at this sample rate for any parameters.
/// `None` for an unknown effect or a rate outside the effect's native range.
pub fn max_latency_frames(id: &str, sample_rate: f32) -> Option<u32> {
    insert_memory::check_rate(id, sample_rate).ok()?;
    Some((insert_spec(id)?.latency)(sample_rate))
}

/// A project insert's effect id or parameter vector was invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertError {
    UnknownEffect,
    InvalidSampleRate,
    MemoryBoundUnavailable,
    ParameterCount { expected: usize, actual: usize },
    NonFiniteParameter { index: usize },
    ParameterOutOfRange { index: usize },
}

impl std::fmt::Display for InsertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEffect => f.write_str("unknown insert effect"),
            Self::InvalidSampleRate => f.write_str("sample rate outside the effect's native range"),
            Self::MemoryBoundUnavailable => f.write_str("cannot bound the effect's memory payload"),
            Self::ParameterCount { expected, actual } => {
                write!(f, "expected {expected} insert parameters, got {actual}")
            }
            Self::NonFiniteParameter { index } => {
                write!(f, "insert parameter {index} is not finite")
            }
            Self::ParameterOutOfRange { index } => {
                write!(f, "insert parameter {index} is out of range")
            }
        }
    }
}

impl std::error::Error for InsertError {}

/// Constructs and configures an effect off the audio thread. `params` is the full vector in
/// [`param_spec`] order. Bitcrusher and Lo-fi Colour use a 32-bit seed formed by XORing the high
/// and low halves of `seed`. Other seeded constructors receive `seed` unchanged.
/// The first processed block starts at the supplied parameter values.
/// A rate outside the effect's native range (the one [`max_memory_bytes`] checks) is refused.
pub fn create_insert(
    id: &str,
    sample_rate: f32,
    seed: u64,
    params: &[f32],
) -> Result<Box<dyn Device>, InsertError> {
    if !EFFECTS.contains(&id) {
        return Err(InsertError::UnknownEffect);
    }
    // Constructors clamp or panic on rates outside their range; refuse those before building.
    insert_memory::check_rate(id, sample_rate)?;
    let spec = param_spec(id).ok_or(InsertError::UnknownEffect)?;
    if params.len() != spec.len() {
        return Err(InsertError::ParameterCount {
            expected: spec.len(),
            actual: params.len(),
        });
    }
    for (index, (&value, &(min, max, _))) in params.iter().zip(spec).enumerate() {
        if !value.is_finite() {
            return Err(InsertError::NonFiniteParameter { index });
        }
        if !(min..=max).contains(&value) {
            return Err(InsertError::ParameterOutOfRange { index });
        }
    }

    let folded = seed as u32 ^ (seed >> 32) as u32;
    let mut device: Box<dyn Device> = match id {
        "algorithmic_reverb" => Box::new(algorithmic_reverb::AlgorithmicReverb::with_seed(
            sample_rate,
            seed,
        )),
        "bitcrusher" => Box::new(bitcrusher::Bitcrusher::with_seed(sample_rate, folded)),
        "character_delay" => Box::new(character_delay::CharacterDelay::with_seed(
            sample_rate,
            seed,
        )),
        "lofi_colour" => Box::new(lofi_colour::LofiColour::with_seed(sample_rate, folded)),
        "shimmer_reverb" => Box::new(shimmer_reverb::ShimmerReverb::with_seed(sample_rate, seed)),
        _ => create(id, sample_rate).ok_or(InsertError::UnknownEffect)?,
    };

    // Fresh devices snap controls in set_param, before the first process call.
    for (i, &value) in params.iter().enumerate() {
        device.set_param(i as u32, value);
    }
    Ok(device)
}
