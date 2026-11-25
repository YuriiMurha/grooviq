use super::InsertId;
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
/// sample rate off-thread. The kernel neither inspects nor bounds its memory or latency.
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamError {
    UnknownInsert(InsertId),
}

/// Sorted insert identity to chain slot, built off-thread; lookups are a binary search.
pub(super) fn find(keys: &[(InsertId, u32)], id: InsertId) -> Result<usize, ParamError> {
    keys.binary_search_by_key(&id, |(key, _)| *key)
        .map(|at| keys[at].1 as usize)
        .map_err(|_| ParamError::UnknownInsert(id))
}
