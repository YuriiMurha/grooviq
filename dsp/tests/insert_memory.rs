use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use grooviq_dsp::{EFFECTS, InsertError, create_insert, is_stepped, max_memory_bytes, param_spec};

struct CountingAllocator;

thread_local! {
    // Count requested bytes on the test thread, including temporary allocations.
    static BYTES: Cell<Option<u64>> = const { Cell::new(None) };
}

fn record(size: usize) {
    let _ = BYTES.try_with(|bytes| {
        if let Some(previous) = bytes.get() {
            bytes.set(Some(previous.saturating_add(size as u64)));
        }
    });
}

// SAFETY: All allocation operations forward their original layouts and pointers to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure<T>(f: impl FnOnce() -> T) -> (T, u64) {
    BYTES.set(Some(0));
    let value = f();
    let bytes = BYTES.replace(None).unwrap();
    (value, bytes)
}

fn assert_payload_bound(id: &str, rate: f32) {
    let spec = param_spec(id).unwrap();
    let (bound, query_bytes) = measure(|| max_memory_bytes(id, rate));
    let bound = bound.unwrap();
    assert_eq!(query_bytes, 0, "{id} {rate}: bound query allocated");
    for setting in 0..3 {
        let params: Vec<_> = spec
            .iter()
            .map(|&(min, max, default)| [min, max, default][setting])
            .collect();
        let (device, allocated) =
            measure(|| create_insert(id, rate, 0x1234_5678_9abc_def0, &params));
        let mut device = device.unwrap();
        assert!(allocated >= size_of_val(device.as_ref()) as u64);
        assert!(
            allocated <= bound,
            "{id} {rate}: {allocated} > bound {bound}"
        );
        // Allow 256 bytes or 1% extra for rounding and conservative scratch-space bounds.
        assert!(
            bound - allocated <= 256.max(allocated / 100),
            "{id} {rate}: bound {bound} is too loose for {allocated} bytes"
        );
        let (mut left, mut right) = ([0.25], [-0.25]);
        device.process(&mut left, &mut right, None, &[]);
        let (_, live_bytes) = measure(|| {
            for (i, &(min, max, default)) in spec.iter().enumerate() {
                let index = i as u32;
                let steps = if is_stepped(id, index).unwrap() {
                    (max - min) as u32
                } else {
                    4
                };
                for step in 0..=steps {
                    device.set_param(index, min + (max - min) * (step as f32 / steps as f32));
                }
                device.set_param(index, default);
            }
        });
        assert_eq!(live_bytes, 0, "{id} {rate}: parameter updates allocated");
    }
}

#[test]
fn reported_memory_bounds_box_buffers_and_constructor_scratch() {
    for id in EFFECTS {
        for rate in [22050., 44100., 48000., 96000., 192000.] {
            assert_payload_bound(id, rate);
        }
    }
    for id in [
        "comp", "delay", "drive", "eq", "limiter", "ott", "reverb", "utility",
    ] {
        for rate in [8000., 384000.] {
            assert_payload_bound(id, rate);
        }
    }
}

#[test]
fn memory_bound_rejects_unknown_effects_and_rates_outside_native_range() {
    assert_eq!(
        max_memory_bytes("unknown", 48000.),
        Err(InsertError::UnknownEffect)
    );
    assert_eq!(
        max_memory_bytes("synth", 48000.),
        Err(InsertError::UnknownEffect)
    );
    for id in EFFECTS {
        for rate in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 7999., 384001.] {
            assert_eq!(
                max_memory_bytes(id, rate),
                Err(InsertError::InvalidSampleRate),
                "{id} {rate}"
            );
        }
    }
    for id in [
        "algorithmic_reverb",
        "bitcrusher",
        "character_delay",
        "drum_shaper",
        "fattener",
        "lofi_colour",
        "saturation",
        "shimmer_reverb",
    ] {
        for rate in [22049., 192001.] {
            assert_eq!(
                max_memory_bytes(id, rate),
                Err(InsertError::InvalidSampleRate),
                "{id} {rate}"
            );
        }
    }
}
