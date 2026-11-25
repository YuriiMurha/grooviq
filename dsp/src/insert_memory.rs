use std::ops::RangeInclusive;

use crate::{
    EFFECTS, InsertError, algorithmic_reverb, bitcrusher, character_delay, comp, delay, drive,
    drum_shaper, eq, fattener, limiter, lofi_colour, ott, reverb, saturation, shimmer_reverb,
    utility,
};

/// Accumulates the constructor's requested payload, including temporary allocations.
pub(crate) struct Payload(Option<u64>);

impl Payload {
    pub(crate) fn boxed<T>() -> Self {
        Self(u64::try_from(size_of::<T>()).ok())
    }

    pub(crate) fn array<T>(&mut self, len: usize, copies: usize) {
        self.0 = self.0.and_then(|bytes| {
            let len = u64::try_from(len).ok()?;
            let copies = u64::try_from(copies).ok()?;
            let element = u64::try_from(size_of::<T>()).ok()?;
            bytes.checked_add(len.checked_mul(copies)?.checked_mul(element)?)
        });
    }

    pub(crate) fn delay_lines(&mut self, max_delay: usize, copies: usize) {
        if let Some(capacity) = crate::util::DelayLine::capacity_for(max_delay) {
            self.array::<f32>(capacity, copies);
        } else {
            self.0 = None;
        }
    }

    fn finish(self) -> Result<u64, InsertError> {
        self.0.ok_or(InsertError::MemoryBoundUnavailable)
    }
}

const STANDARD_RATE: RangeInclusive<f32> = 22050.0..=192000.0;
const EXTENDED_RATE: RangeInclusive<f32> = 8000.0..=384000.0;
type MemoryBound = fn(f32) -> Payload;

/// Each effect's native sample-rate range and payload bound: the one table that admission
/// (`max_memory_bytes`), construction (`create_insert`) and `max_latency_frames` share.
fn table(id: &str) -> Result<(RangeInclusive<f32>, MemoryBound), InsertError> {
    Ok(match id {
        "algorithmic_reverb" => (STANDARD_RATE, algorithmic_reverb::memory_payload),
        "bitcrusher" => (STANDARD_RATE, |_| {
            Payload::boxed::<bitcrusher::Bitcrusher>()
        }),
        "character_delay" => (STANDARD_RATE, character_delay::memory_payload),
        "comp" => (EXTENDED_RATE, |_| Payload::boxed::<comp::Comp>()),
        "delay" => (EXTENDED_RATE, delay::memory_payload),
        "drive" => (EXTENDED_RATE, |_| drive::memory_payload()),
        "drum_shaper" => (STANDARD_RATE, |_| {
            Payload::boxed::<drum_shaper::DrumShaper>()
        }),
        "eq" => (EXTENDED_RATE, |_| eq::memory_payload()),
        "fattener" => (STANDARD_RATE, |_| Payload::boxed::<fattener::Fattener>()),
        "limiter" => (EXTENDED_RATE, limiter::memory_payload),
        "lofi_colour" => (STANDARD_RATE, lofi_colour::memory_payload),
        "ott" => (EXTENDED_RATE, |_| Payload::boxed::<ott::Ott>()),
        "reverb" => (EXTENDED_RATE, reverb::memory_payload),
        "saturation" => (STANDARD_RATE, |_| saturation::memory_payload()),
        "shimmer_reverb" => (STANDARD_RATE, shimmer_reverb::memory_payload),
        "utility" => (EXTENDED_RATE, |_| Payload::boxed::<utility::Utility>()),
        _ if EFFECTS.contains(&id) => return Err(InsertError::MemoryBoundUnavailable),
        _ => return Err(InsertError::UnknownEffect),
    })
}

/// Refuses a rate outside the effect's native range (NaN and infinities included) instead of
/// letting its constructor clamp it, so admission and construction always agree.
pub(crate) fn check_rate(id: &str, sample_rate: f32) -> Result<(), InsertError> {
    if table(id)?.0.contains(&sample_rate) {
        Ok(())
    } else {
        Err(InsertError::InvalidSampleRate)
    }
}

/// Bounds a built-in effect's device-owned payload before construction for any legal parameters.
/// Includes the boxed state, all owned buffer capacities, and any constructor scratch space.
/// Excludes allocator metadata and shared inputs. Rates outside the effect's native range
/// return an error, even when its standalone constructor would clamp the rate.
pub fn max_memory_bytes(id: &str, sample_rate: f32) -> Result<u64, InsertError> {
    let (_, bound) = table(id)?;
    // Validate the native rate before the constructor's bounded float-to-frame conversions.
    check_rate(id, sample_rate)?;
    bound(sample_rate).finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_refuses_arithmetic_overflow() {
        assert_eq!(Payload(Some(u64::MAX)).finish(), Ok(u64::MAX));
        let mut sum = Payload(Some(u64::MAX));
        sum.array::<f32>(1, 1);
        assert_eq!(sum.finish(), Err(InsertError::MemoryBoundUnavailable));
        let mut product = Payload(Some(0));
        product.array::<u64>(usize::MAX, usize::MAX);
        assert_eq!(product.finish(), Err(InsertError::MemoryBoundUnavailable));
        let mut capacity = Payload(Some(0));
        capacity.delay_lines(usize::MAX, 1);
        assert_eq!(capacity.finish(), Err(InsertError::MemoryBoundUnavailable));
    }
}
