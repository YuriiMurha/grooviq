use super::runtime::{bytes, reserved};
use super::{InsertId, LatencyPlan, MixerTrackId, PdcDelay, PdcPoint, PrepareError, Tap};
use std::num::NonZeroU32;

pub(super) type DelayIndex = Option<NonZeroU32>;

/// Destination (in the point), original tap, serial stage and absolute start time.
/// A send ID may survive a retarget, and a delay may keep its length as both ends shift.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    point: PdcPoint,
    from: Option<(MixerTrackId, Tap)>,
    predecessor: Option<InsertId>,
    start: u64,
    frames: usize,
}

pub(super) struct Ring {
    audio: Vec<[f32; 2]>,
    cursor: usize,
    unfilled: usize,
}

impl Ring {
    pub(super) fn allocate(frames: usize) -> Result<Self, PrepareError> {
        let mut audio = reserved(frames)?;
        audio.resize(frames, [0.0; 2]);
        Ok(Self {
            audio,
            cursor: 0,
            unfilled: 0,
        })
    }

    pub(super) fn bytes(&self) -> Result<u64, PrepareError> {
        bytes(&self.audio)
    }

    pub(super) fn swap_compatible(&mut self, old: &mut Self) -> bool {
        if self.audio.capacity() != old.audio.capacity() {
            return false;
        }
        std::mem::swap(self, old);
        true
    }

    pub(super) fn exchange(&mut self, l: f32, r: f32) -> [f32; 2] {
        let frame = std::mem::replace(&mut self.audio[self.cursor], [l, r]);
        self.cursor += 1;
        if self.cursor == self.audio.len() {
            self.cursor = 0;
        }
        if self.unfilled == 0 {
            frame
        } else {
            self.unfilled -= 1;
            [0.0; 2]
        }
    }

    pub(super) fn clear(&mut self) {
        // Long histories are made silent in constant time, including on the fault path.
        // Each slot is overwritten before it can be read again.
        self.unfilled = self.audio.len();
    }
}

#[derive(Default)]
pub(super) struct DelayBank {
    rings: Vec<Ring>,
    /// Sorted off-thread for a linear install-time merge; indices are in render order.
    keys: Vec<(Key, usize)>,
}

impl DelayBank {
    pub(super) fn reserve(plan: &LatencyPlan) -> Result<Self, PrepareError> {
        let count = plan.delays.iter().filter(|delay| delay.frames != 0).count();
        Ok(Self {
            rings: reserved(count)?,
            keys: reserved(count)?,
        })
    }

    pub(super) fn register(
        &mut self,
        delay: PdcDelay,
        end: u64,
        from: Option<(MixerTrackId, Tap)>,
        predecessor: Option<InsertId>,
    ) -> Result<DelayIndex, PrepareError> {
        if delay.frames == 0 {
            return Ok(None);
        }
        let index = self.rings.len();
        let slot = u32::try_from(index + 1).map_err(|_| PrepareError::Overflow)?;
        let frames = usize::try_from(delay.frames).map_err(|_| PrepareError::Overflow)?;
        self.keys.push((
            Key {
                point: delay.at,
                from,
                predecessor,
                start: end - delay.frames,
                frames,
            },
            index,
        ));
        self.rings.push(Ring {
            audio: Vec::new(),
            cursor: 0,
            unfilled: 0,
        });
        Ok(NonZeroU32::new(slot))
    }

    pub(super) fn table_bytes(&self) -> Result<u64, PrepareError> {
        bytes(&self.rings)?
            .checked_add(bytes(&self.keys)?)
            .ok_or(PrepareError::Overflow)
    }

    /// Called after checking the aggregate audio and table budget.
    pub(super) fn allocate(&mut self) -> Result<u64, PrepareError> {
        self.keys.sort_unstable();
        let mut total = 0u64;
        for &(key, index) in &self.keys {
            let ring = Ring::allocate(key.frames)?;
            total = total
                .checked_add(ring.bytes()?)
                .ok_or(PrepareError::Overflow)?;
            self.rings[index] = ring;
        }
        Ok(total)
    }

    pub(super) fn frames(&self, index: DelayIndex) -> usize {
        index.map_or(0, |index| self.rings[index.get() as usize - 1].audio.len())
    }

    pub(super) fn apply(&mut self, index: DelayIndex, l: &mut [f32], r: &mut [f32]) {
        if let Some(index) = index {
            let ring = &mut self.rings[index.get() as usize - 1];
            for (l, r) in l.iter_mut().zip(r) {
                [*l, *r] = ring.exchange(*l, *r);
            }
        }
    }

    pub(super) fn accumulate(
        &mut self,
        index: NonZeroU32,
        acc_l: &mut [f32],
        acc_r: &mut [f32],
        l: &[f32],
        r: &[f32],
        gain: f32,
    ) {
        let ring = &mut self.rings[index.get() as usize - 1];
        for i in 0..l.len() {
            let [l, r] = ring.exchange(l[i], r[i]);
            acc_l[i] += l * gain;
            acc_r[i] += r * gain;
        }
    }

    pub(super) fn clear(&mut self, index: DelayIndex) {
        if let Some(index) = index {
            self.rings[index.get() as usize - 1].clear();
        }
    }

    pub(super) fn reset(&mut self) {
        self.rings.iter_mut().for_each(Ring::clear);
    }

    pub(super) fn transfer_from(&mut self, old: &mut Self) {
        let mut candidates = old.keys.iter().peekable();
        for &(key, index) in &self.keys {
            while candidates.peek().is_some_and(|(other, _)| *other < key) {
                candidates.next();
            }
            // The retained charges belong to their kernels, so allocations may only
            // trade places if their charged capacities agree.
            if let Some(&(_, other)) = candidates.next_if(|(other, _)| *other == key) {
                self.rings[index].swap_compatible(&mut old.rings[other]);
            }
        }
    }
}
