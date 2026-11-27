use super::InsertId;
use super::delay::{DelayIndex, Ring};
use super::runtime::FaderGain;
use grooviq_dsp::Device;

/// What makes two prepared devices interchangeable for a survivor transfer: the same
/// built-in kind (an index the caller assigns) and the same seed. Sample rate is the
/// kernel's own and is compared separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InsertIdentity {
    pub kind: u32,
    pub seed: u64,
}

/// A built-in device the caller constructed, preset, seeded and prepared for the mixer's
/// sample rate off-thread. Compensated preparation checks its reported latency against the insert's bound.
pub struct PreparedInsert {
    pub id: InsertId,
    pub device: Box<dyn Device>,
    pub identity: InsertIdentity,
}

impl PreparedInsert {
    /// A device with no transfer identity: it only survives into a kernel whose matching
    /// insert also has the default identity.
    pub fn new(id: InsertId, device: Box<dyn Device>) -> Self {
        Self::with_identity(id, device, InsertIdentity::default())
    }

    pub fn with_identity(id: InsertId, device: Box<dyn Device>, identity: InsertIdentity) -> Self {
        Self {
            id,
            device,
            identity,
        }
    }
}

/// One stage of a track's serial chain. `detector` indexes the prepared detector feeds;
/// an empty range means the device receives `None`, never a silent buffer.
pub(super) struct InsertSlot {
    pub(super) id: InsertId,
    pub(super) device: Box<dyn Device>,
    pub(super) identity: InsertIdentity,
    pub(super) detector: (u32, u32),
    pub(super) delay: DelayIndex,
    pub(super) latency: u32,
}

pub(super) struct BypassState {
    pub(super) slot: usize,
    pub(super) history: Option<Ring>,
    pub(super) blend: FaderGain,
}

impl BypassState {
    pub(super) fn capture(&mut self, wet_l: &[f32], wet_r: &[f32], l: &mut [f32], r: &mut [f32]) {
        if let Some(history) = &mut self.history {
            for i in 0..l.len() {
                [l[i], r[i]] = history.exchange(wet_l[i], wet_r[i]);
            }
        } else {
            l.copy_from_slice(wet_l);
            r.copy_from_slice(wet_r);
        }
    }

    /// A wet fault still trips the node guard when bypass hides the wet output.
    pub(super) fn mix(
        &mut self,
        samples: u32,
        wet_l: &mut [f32],
        wet_r: &mut [f32],
        l: &[f32],
        r: &[f32],
    ) -> bool {
        let mut wet_fault = false;
        for i in 0..l.len() {
            let dry = self.blend.next(samples);
            if dry == 0.0 {
                continue;
            }
            wet_fault |= !wet_l[i].is_finite() || !wet_r[i].is_finite();
            if dry == 1.0 {
                [wet_l[i], wet_r[i]] = [l[i], r[i]];
            } else {
                let dry = f64::from(dry);
                let wet = 1.0 - dry;
                wet_l[i] = (f64::from(wet_l[i]) * wet + f64::from(l[i]) * dry) as f32;
                wet_r[i] = (f64::from(wet_r[i]) * wet + f64::from(r[i]) * dry) as f32;
            }
        }
        wet_fault
    }

    pub(super) fn transfer_from(&mut self, old: &mut Self, samples: u32) {
        match (&mut self.history, &mut old.history) {
            (Some(new), Some(old)) => {
                if !new.swap_compatible(old) {
                    return;
                }
            }
            (None, None) => {}
            _ => return,
        }
        let target = self.blend.target;
        self.blend = old.blend;
        self.blend.retarget(target, samples);
    }

    pub(super) fn reset(&mut self) {
        if let Some(history) = &mut self.history {
            history.clear();
        }
        self.blend = FaderGain::at(self.blend.target);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamError {
    UnknownInsert(InsertId),
    RequiresPreparedInstall,
}

/// Sorted insert identity to chain slot, built off-thread; lookups are a binary search.
pub(super) fn find(keys: &[(InsertId, u32)], id: InsertId) -> Result<usize, ParamError> {
    keys.binary_search_by_key(&id, |(key, _)| *key)
        .map(|at| keys[at].1 as usize)
        .map_err(|_| ParamError::UnknownInsert(id))
}
