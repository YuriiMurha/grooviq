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
mod util;
mod wavetable;

pub mod algorithmic_reverb;
pub mod bitcrusher;
pub mod comp;
pub mod delay;
pub mod drive;
pub mod drum_shaper;
pub mod eq;
pub mod fattener;
pub mod limiter;
pub mod ott;
pub mod reverb;
pub mod sampler;
pub mod synth;
pub mod utility;

/// `(min, max, default)` of every parameter of a device, indexed like `set_param`
/// (`SPEC` index order). Useful for clamping, defaults and UI ranges.
pub fn param_spec(id: &str) -> Option<&'static [(f32, f32, f32)]> {
    Some(match id {
        "bitcrusher" => &bitcrusher::SPEC,
        "synth" => &synth::SPEC,
        "sampler" => &sampler::SPEC,
        "eq" => &eq::SPEC,
        "fattener" => &fattener::SPEC,
        "comp" => &comp::SPEC,
        "limiter" => &limiter::SPEC,
        "drive" => &drive::SPEC,
        "drum_shaper" => &drum_shaper::SPEC,
        "reverb" => &reverb::SPEC,
        "algorithmic_reverb" => &algorithmic_reverb::SPEC,
        "delay" => &delay::SPEC,
        "ott" => &ott::SPEC,
        "utility" => &utility::SPEC,
        _ => return None,
    })
}

/// Creates a device by its `id` ("synth", "sampler", "eq", "comp", "limiter", "drive",
/// "reverb", "delay", "ott", "utility"). A sampler made here has no sample: it stays silent
/// until the owner of the concrete [`sampler::Sampler`] calls `set_sample`.
pub fn create(id: &str, sample_rate: f32) -> Option<Box<dyn Device>> {
    Some(match id {
        "bitcrusher" => Box::new(bitcrusher::Bitcrusher::new(sample_rate)),
        "synth" => Box::new(synth::Synth::new(sample_rate)),
        "sampler" => Box::new(sampler::Sampler::new(sample_rate)),
        "eq" => Box::new(eq::Eq::new(sample_rate)),
        "fattener" => Box::new(fattener::Fattener::new(sample_rate)),
        "comp" => Box::new(comp::Comp::new(sample_rate)),
        "limiter" => Box::new(limiter::Limiter::new(sample_rate)),
        "drive" => Box::new(drive::Drive::new(sample_rate)),
        "drum_shaper" => Box::new(drum_shaper::DrumShaper::new(sample_rate)),
        "reverb" => Box::new(reverb::Reverb::new(sample_rate)),
        "algorithmic_reverb" => Box::new(algorithmic_reverb::AlgorithmicReverb::new(sample_rate)),
        "delay" => Box::new(delay::Delay::new(sample_rate)),
        "ott" => Box::new(ott::Ott::new(sample_rate)),
        "utility" => Box::new(utility::Utility::new(sample_rate)),
        _ => return None,
    })
}
