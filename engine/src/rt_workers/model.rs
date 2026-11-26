//! Small interleaving models of the mailbox and callback pin protocols.

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering, fence};
use loom::thread;

#[test]
fn mailbox_cancel_or_claim_and_acquire_completion() {
    loom::model(|| {
        const OFFERED: usize = 1;
        const CLAIMED: usize = 2;
        const CANCELED: usize = 3;
        let state = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let input = Arc::new(UnsafeCell::new(0));
        let output = Arc::new(UnsafeCell::new(0));
        let worker = {
            let (state, done, input, output) =
                (state.clone(), done.clone(), input.clone(), output.clone());
            thread::spawn(move || {
                if state
                    .compare_exchange(OFFERED, CLAIMED, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let source = input.with(|p| unsafe { *p });
                    output.with_mut(|p| unsafe { *p = source });
                    done.store(true, Ordering::Release);
                    true
                } else {
                    false
                }
            })
        };
        input.with_mut(|p| unsafe { *p = 42 });
        state.store(OFFERED, Ordering::Release);
        let canceled = state
            .compare_exchange(OFFERED, CANCELED, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok();
        if canceled {
            output.with_mut(|p| unsafe { *p = 42 });
        } else {
            while !done.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }
        assert_eq!(output.with(|p| unsafe { *p }), 42);
        input.with_mut(|p| unsafe { *p = 43 });
        assert_eq!(worker.join().unwrap(), !canceled);
    });
}

#[test]
fn callback_pin_or_new_generation_and_outstanding_claim() {
    loom::model(|| {
        let active = Arc::new(AtomicUsize::new(1));
        let pin = Arc::new(AtomicUsize::new(0));
        let outstanding = Arc::new(AtomicUsize::new(0));
        let memory = Arc::new(UnsafeCell::new(0));
        let callback = {
            let (active, pin, outstanding, memory) = (
                active.clone(),
                pin.clone(),
                outstanding.clone(),
                memory.clone(),
            );
            thread::spawn(move || {
                let candidate = active.load(Ordering::Acquire);
                pin.store(candidate, Ordering::Release);
                fence(Ordering::SeqCst);
                if active.load(Ordering::Acquire) == candidate && candidate == 1 {
                    outstanding.fetch_add(1, Ordering::Relaxed);
                    let worker = thread::spawn(move || {
                        memory.with_mut(|p| unsafe { *p = 1 });
                        outstanding.fetch_sub(1, Ordering::AcqRel);
                    });
                    pin.store(0, Ordering::Release);
                    worker.join().unwrap();
                } else {
                    pin.store(0, Ordering::Release);
                }
            })
        };
        active.store(2, Ordering::Release);
        fence(Ordering::SeqCst);
        // Like a worker that observed the new publication before an earlier callback dispatched.
        if pin.load(Ordering::Acquire) != 1 && outstanding.load(Ordering::Acquire) == 0 {
            memory.with_mut(|p| unsafe { *p = 2 });
        }
        callback.join().unwrap();
    });
}
