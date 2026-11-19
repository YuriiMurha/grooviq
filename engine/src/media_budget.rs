//! Control-side admission. Weak allocation identities follow actual Arc ownership, including
//! retired engine banks/snapshots, without attaching a locking destructor to RT payloads.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
pub const MEDIA_BYTES: usize = 2 * 1024 * 1024 * 1024;
pub const ORIGINAL_BYTES: usize = 512 * 1024 * 1024;
#[derive(Clone)]
pub struct MediaBudget(Arc<Mutex<State>>);
struct State {
    limit: usize,
    high_water: usize,
    next: u64,
    pending: HashMap<u64, usize>,
    live: HashMap<(u8, usize), (usize, Weak<dyn Send + Sync>)>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaLimit;
impl std::fmt::Display for MediaLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Retained media exceeds its shared memory budget; remove unused media/history or close held exports."
        )
    }
}
impl std::error::Error for MediaLimit {}
impl State {
    fn used(&mut self) -> usize {
        self.live.retain(|_, (_, w)| w.strong_count() > 0);
        self.pending.values().sum::<usize>() + self.live.values().map(|(n, _)| n).sum::<usize>()
    }
}
impl Default for MediaBudget {
    fn default() -> Self {
        Self::with_limit(MEDIA_BYTES)
    }
}
impl MediaBudget {
    pub fn with_limit(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            limit: limit.min(MEDIA_BYTES),
            high_water: 0,
            next: 0,
            pending: HashMap::new(),
            live: HashMap::new(),
        })))
    }
    pub fn high_water(&self) -> usize {
        self.0.lock().unwrap().high_water
    }
    pub fn used(&self) -> usize {
        self.0.lock().unwrap().used()
    }
    pub fn reserve(&self, bytes: usize) -> Result<MediaReservation, MediaLimit> {
        let mut s = self.0.lock().unwrap();
        if bytes > s.limit.saturating_sub(s.used()) {
            return Err(MediaLimit);
        }
        let id = s.next;
        s.next = s.next.checked_add(1).ok_or(MediaLimit)?;
        s.pending.insert(id, bytes);
        s.high_water = s.high_water.max(s.used());
        Ok(MediaReservation {
            budget: self.clone(),
            id,
        })
    }
    pub fn track<T: Send + Sync + 'static>(
        &self,
        kind: u8,
        value: &Arc<T>,
        bytes: usize,
    ) -> Result<(), MediaLimit> {
        let key = (kind, Arc::as_ptr(value) as usize);
        let mut s = self.0.lock().unwrap();
        s.used();
        if s.live.contains_key(&key) {
            return Ok(());
        }
        if bytes > s.limit.saturating_sub(s.used()) {
            return Err(MediaLimit);
        }
        let erased: Arc<dyn Send + Sync> = value.clone();
        s.live.insert(key, (bytes, Arc::downgrade(&erased)));
        s.high_water = s.high_water.max(s.used());
        Ok(())
    }
    pub fn original(&self, value: &Arc<Vec<u8>>) -> Result<(), MediaLimit> {
        if value.len() > ORIGINAL_BYTES {
            return Err(MediaLimit);
        }
        self.track(0, value, value.capacity())
    }
    pub fn sample(&self, value: &Arc<crate::SampleBuf>) -> Result<(), MediaLimit> {
        self.track(1, value, (value.l.capacity() + value.r.capacity()) * 4)
    }
    pub fn peaks(&self, value: &Arc<crate::waveform::PeakPyramid>) -> Result<(), MediaLimit> {
        self.sample(value.sample())?;
        self.track(2, value, value.allocated_peak_bytes())
    }
}
pub struct MediaReservation {
    budget: MediaBudget,
    id: u64,
}
impl MediaReservation {
    pub fn resize(&mut self, bytes: usize) -> Result<(), MediaLimit> {
        let mut s = self.budget.0.lock().unwrap();
        let old = s.pending[&self.id];
        let used = s.used();
        if bytes > s.limit.saturating_sub(used - old) {
            return Err(MediaLimit);
        }
        s.pending.insert(self.id, bytes);
        s.high_water = s.high_water.max(s.used());
        Ok(())
    }
    pub fn bind<T: Send + Sync + 'static>(
        self,
        kind: u8,
        value: &Arc<T>,
        bytes: usize,
    ) -> Result<(), MediaLimit> {
        let mut s = self.budget.0.lock().unwrap();
        let reserved = s.pending[&self.id];
        if bytes > reserved {
            return Err(MediaLimit);
        }
        let erased: Arc<dyn Send + Sync> = value.clone();
        s.live
            .entry((kind, Arc::as_ptr(value) as usize))
            .or_insert((bytes, Arc::downgrade(&erased)));
        s.pending.remove(&self.id);
        Ok(())
    }
}
impl Drop for MediaReservation {
    fn drop(&mut self) {
        self.budget.0.lock().unwrap().pending.remove(&self.id);
    }
}
