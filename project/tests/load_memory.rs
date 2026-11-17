//! Loading an untrusted file keeps at most `MAX_NOTES_PER_PATTERN` notes in memory, however
//! many the file holds. This test binary counts every allocation (its own global allocator),
//! so it lives alone.

use grooviq_project::{MAX_NOTES_PER_PATTERN, load};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A project file whose one pattern holds `notes` minimal notes.
fn file_with_notes(dir: &std::path::Path, notes: usize) -> std::path::PathBuf {
    let mut json = String::from(
        r#"{"format":"grooviq-project","version":2,"channels":[{"id":1}],"nextId":2,"patterns":[{"id":0,"name":"Big","notes":["#,
    );
    for i in 0..notes {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(r#"{{"id":{},"channel":1,"tick":0}}"#, i + 1));
    }
    json.push_str("]}]}");
    let path = dir.join(format!("{notes}.grooviq"));
    let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("project.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(json.as_bytes()).unwrap();
    zip.finish().unwrap();
    path
}

/// The most memory `load(path)` had allocated at once, above what was allocated before it.
fn peak_while_loading(path: &std::path::Path) -> (usize, Vec<String>, usize) {
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let loaded = load(path).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - base;
    let kept = loaded.project.patterns[0].notes.len();
    (peak, loaded.warnings, kept)
}

#[test]
fn a_million_notes_load_in_the_memory_of_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let at_cap = file_with_notes(dir.path(), MAX_NOTES_PER_PATTERN);
    let million = file_with_notes(dir.path(), 1_000_000);

    let (cap_peak, cap_warnings, cap_kept) = peak_while_loading(&at_cap);
    assert!(cap_warnings.is_empty(), "{cap_warnings:?}");
    assert_eq!(cap_kept, MAX_NOTES_PER_PATTERN);

    let (peak, warnings, kept) = peak_while_loading(&million);
    assert_eq!(kept, MAX_NOTES_PER_PATTERN);
    assert_eq!(
        warnings,
        vec![format!(
            "Pattern \"Big\": dropped 900000 notes past the limit of {MAX_NOTES_PER_PATTERN} notes per pattern."
        )]
    );
    // Ten times the notes in the file, about the same memory: the extra notes are counted
    // and skipped, never held (as JSON text, a JSON tree or notes).
    assert!(
        peak <= cap_peak + cap_peak / 4,
        "1,000,000 notes peaked at {peak} bytes, the cap's 100,000 at {cap_peak}"
    );
    // And the cap's own load stays small: 100,000 notes are 2.4 MB as notes, while the file's
    // JSON text alone is 3.4 MB (35 MB for the million).
    assert!(
        cap_peak < 8 << 20,
        "100,000 notes peaked at {cap_peak} bytes"
    );
}
