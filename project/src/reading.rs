//! The typed read of an untrusted `project.json`, as [`crate::file::load`] runs it: the
//! deserializers here see the file's format version and its channel ids (both found by the first,
//! lenient pass in [`crate::outline`]) through a context set for the length of the read.
//!
//! * A version 1 file's `instrument`, note `id` and `nextNoteId` are skipped unread, whatever
//!   their type: migration supplies them.
//! * A `notes` list keeps at most [`MAX_NOTES_PER_PATTERN`] notes that can play, and drops,
//!   counting them, notes of channels the file does not have and notes past the last step, so
//!   neither the cap nor memory is spent on notes that cleanup drops anyway.
//!
//! Outside a load (no context) the same deserializers read every field by its type and keep the
//! first [`MAX_NOTES_PER_PATTERN`] notes.

use crate::model::{MAX_NOTES_PER_PATTERN, MAX_TICKS, Note};
use serde::de::{Deserialize, Deserializer, IgnoredAny, SeqAccess, Visitor};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

/// The notes one `notes` list dropped while it was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NoteDrops {
    /// Notes of a channel the file does not have.
    pub(crate) orphans: usize,
    /// Notes that start past the last step a pattern can have.
    pub(crate) far: usize,
    /// Playable notes past [`MAX_NOTES_PER_PATTERN`].
    pub(crate) over_cap: usize,
}

struct Context {
    version: u32,
    channels: HashSet<u32>,
    drops: Vec<NoteDrops>,
}

thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

/// Runs `read` with `version` and the file's `channels` (the ids validation keeps) in reach of
/// the deserializers. Returns its result and what each `notes` list dropped, in reading order.
pub(crate) fn with_context<T>(
    version: u32,
    channels: HashSet<u32>,
    read: impl FnOnce() -> T,
) -> (T, Vec<NoteDrops>) {
    /// Clears the context however `read` ends, a panic included.
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            CONTEXT.with(|c| c.borrow_mut().take());
        }
    }
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(Context {
            version,
            channels,
            drops: Vec::new(),
        });
    });
    let _clear = Clear;
    let out = read();
    let drops = CONTEXT.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|c| std::mem::take(&mut c.drops))
            .unwrap_or_default()
    });
    (out, drops)
}

/// Whether the file being read is version 1, whose migration supplies the field.
pub(crate) fn supplied_by_migration() -> bool {
    CONTEXT.with(|c| c.borrow().as_ref().is_some_and(|c| c.version == 1))
}

/// `Some(known)` while a file is read, `None` otherwise.
fn known_channel(id: u32) -> Option<bool> {
    CONTEXT.with(|c| c.borrow().as_ref().map(|c| c.channels.contains(&id)))
}

/// A note id or `nextNoteId`: skipped in a version 1 file (migration numbers the notes).
pub(crate) fn id<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    if supplied_by_migration() {
        IgnoredAny::deserialize(d)?;
        return Ok(0);
    }
    u32::deserialize(d)
}

/// A pattern's notes (see the module notes).
pub(crate) fn notes<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Note>, D::Error> {
    struct Notes;
    impl<'de> Visitor<'de> for Notes {
        type Value = Vec<Note>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a list of notes")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<Note>, A::Error> {
            let mut notes = Vec::new();
            let mut drops = NoteDrops::default();
            // One note at a time: a dropped one is never held past its check.
            while let Some(note) = seq.next_element::<Note>()? {
                match known_channel(note.channel) {
                    Some(false) => drops.orphans += 1,
                    Some(true) if note.tick >= MAX_TICKS => drops.far += 1,
                    _ if notes.len() >= MAX_NOTES_PER_PATTERN => drops.over_cap += 1,
                    _ => notes.push(note),
                }
            }
            CONTEXT.with(|c| {
                if let Some(c) = c.borrow_mut().as_mut() {
                    c.drops.push(drops);
                }
            });
            Ok(notes)
        }
    }
    d.deserialize_seq(Notes)
}
