//! Shared routed-mixer admission. Only the control side reserves, shrinks and drops charges.
//! The callback moves owning boxes through the command and trash queues.

use super::Budget;
use crate::ControlError;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub(crate) struct MixerMemory {
    used: Arc<AtomicU64>,
    limit: u64,
}

impl Default for MixerMemory {
    fn default() -> Self {
        Self {
            used: Arc::default(),
            limit: Budget::default().memory_bytes,
        }
    }
}

impl MixerMemory {
    pub(super) fn reserve(&self, bytes: u64) -> Result<Reservation, ControlError> {
        if bytes > self.limit {
            return Err(ControlError::Rejected);
        }
        self.used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|&sum| sum <= self.limit)
            })
            .map_err(|_| ControlError::Full)?;
        Ok(Reservation {
            used: Arc::clone(&self.used),
            bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_limit(&mut self, limit: u64) {
        self.limit = limit;
    }
}

pub(super) struct Reservation {
    used: Arc<AtomicU64>,
    bytes: u64,
}

impl Reservation {
    pub(super) fn shrink(&mut self, retained: u64) {
        let released = self
            .bytes
            .checked_sub(retained)
            .expect("retained mixer storage fits its preparation reservation");
        self.bytes = retained;
        self.used.fetch_sub(released, Ordering::Relaxed);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
