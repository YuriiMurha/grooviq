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
//!
//! Every struct of the project must be written with named fields ([`Named`]): serde would also
//! read one as an array of its fields in order, which Grooviq never writes and the first pass
//! does not follow, so such a file is refused instead of half read.

use crate::model::{MAX_NOTES_PER_PATTERN, MAX_TICKS, Note};
use serde::de::value::MapAccessDeserializer;
use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;

/// What a struct written another way (an array of its fields) is refused for.
pub(crate) const NAMED: &str = "an object with named fields";

/// A `T` read only from an object with named fields.
pub(crate) struct Named<T>(pub(crate) T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Named<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Object<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for Object<T> {
            type Value = Named<T>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(NAMED)
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Named<T>, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(Named)
            }
        }
        d.deserialize_map(Object(PhantomData))
    }
}

/// A struct field read with [`Named`].
pub(crate) fn named<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Named::deserialize(d).map(|n| n.0)
}

/// An optional struct field read with [`Named`].
pub(crate) fn named_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Ok(Option::<Named<T>>::deserialize(d)?.map(|n| n.0))
}

/// A list of structs, each read with [`Named`].
pub(crate) fn named_vec<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    Ok(Vec::<Named<T>>::deserialize(d)?
        .into_iter()
        .map(|n| n.0)
        .collect())
}

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
    automation_points: usize,
    /// Whether a `mixer` was read (version 4 on).
    mixer: bool,
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
    let (out, drops, _) = with_context_mixer(version, channels, read);
    (out, drops)
}

/// [`with_context`], also telling whether the document had a `mixer` the read took.
pub(crate) fn with_context_mixer<T>(
    version: u32,
    channels: HashSet<u32>,
    read: impl FnOnce() -> T,
) -> (T, Vec<NoteDrops>, bool) {
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
            automation_points: 0,
            mixer: false,
        });
    });
    let _clear = Clear;
    let out = read();
    let (drops, mixer) = CONTEXT.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|c| (std::mem::take(&mut c.drops), c.mixer))
            .unwrap_or_default()
    });
    (out, drops, mixer)
}

/// Whether the file being read is version 1, whose migration supplies the field.
pub(crate) fn supplied_by_migration() -> bool {
    CONTEXT.with(|c| c.borrow().as_ref().is_some_and(|c| c.version == 1))
}

/// The project's `mixer`. Before version 4 it was an unknown field, so whatever it holds is
/// skipped unread (as an unknown field always was) and the legacy identity stands. Kept apart
/// from [`supplied_by_migration`], which is version 1 only.
pub(crate) fn mixer<'de, D: Deserializer<'de>>(d: D) -> Result<crate::mixer::Mixer, D::Error> {
    let version = CONTEXT.with(|c| c.borrow().as_ref().map(|c| c.version));
    if version.is_some_and(|v| v < 4) {
        return crate::mixer::skip(d);
    }
    let mixer = crate::mixer::Mixer::deserialize(d)?;
    CONTEXT.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            c.mixer = true;
        }
    });
    Ok(mixer)
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
            while let Some(Named(note)) = seq.next_element::<Named<Note>>()? {
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

/// Account before retaining each point, across all clips in a file read.
pub(crate) fn retain_automation_point() -> bool {
    CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let Some(context) = context.as_mut() else {
            return true;
        };
        if context.automation_points == crate::MAX_TOTAL_AUTOMATION_POINTS {
            return false;
        }
        context.automation_points += 1;
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;

    #[test]
    fn a_project_written_as_an_array_is_refused() {
        // `load` reads the document as `Named<Project>`.
        let err = serde_json::from_str::<Named<Project>>(r#"["Untitled", 140]"#)
            .err()
            .unwrap();
        assert!(err.to_string().contains(NAMED), "{err}");
        let named = serde_json::from_str::<Named<Project>>(r#"{"name": "Named"}"#).unwrap();
        assert_eq!(named.0.name, "Named");
    }
}
