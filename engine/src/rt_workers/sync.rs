//! Select Loom's checked primitives for the production protocol under model tests.

#[cfg(all(test, loom))]
pub(super) use loom::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, fence,
};
pub(super) use std::sync::atomic::Ordering;
#[cfg(not(all(test, loom)))]
pub(super) use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, fence,
};

#[cfg(all(test, loom))]
pub(super) use loom::cell::UnsafeCell as MailboxCell;

/// Closure access keeps Loom's checked access alive for the complete read or write.
#[cfg(not(all(test, loom)))]
pub(super) struct MailboxCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(all(test, loom)))]
impl<T> MailboxCell<T> {
    pub(super) fn new(value: T) -> Self {
        Self(std::cell::UnsafeCell::new(value))
    }

    pub(super) fn with<R>(&self, access: impl FnOnce(*const T) -> R) -> R {
        access(self.0.get())
    }

    pub(super) fn with_mut<R>(&self, access: impl FnOnce(*mut T) -> R) -> R {
        access(self.0.get())
    }
}

#[cfg(all(test, loom))]
pub(super) use loom::thread::yield_now as spin_loop;
#[cfg(not(all(test, loom)))]
pub(super) use std::hint::spin_loop;
