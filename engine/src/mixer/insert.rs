use super::InsertId;
use grooviq_dsp::Device;

/// A built-in device the caller constructed, preset, seeded and prepared for the mixer's
/// sample rate off-thread. The kernel neither inspects nor bounds its memory or latency.
pub struct PreparedInsert {
    pub id: InsertId,
    pub device: Box<dyn Device>,
}

impl PreparedInsert {
    pub fn new(id: InsertId, device: Box<dyn Device>) -> Self {
        Self { id, device }
    }
}

/// One stage of a track's serial chain. `detector` indexes the prepared detector feeds;
/// an empty range means the device receives `None`, never a silent buffer.
pub(super) struct InsertSlot {
    pub(super) id: InsertId,
    pub(super) device: Box<dyn Device>,
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
