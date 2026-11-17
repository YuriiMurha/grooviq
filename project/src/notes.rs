//! Notes as the UI gets them, and note edits by id.
//!
//! The UI reads the current pattern's notes as one binary buffer ([`encode_notes`], wire
//! format v1) and changes them with a [`NotesEdit`]: deletes, then updates, then adds, made
//! against the pattern's `revision`. The session applies it ([`crate::session::Session::edit_notes`]).

use crate::model::{DEFAULT_VELOCITY, MAX_NOTES_PER_PATTERN, MAX_STEPS, MAX_TICKS, Note, Pattern};
use crate::session::{ProjectView, SessionError};
use crate::undo::Growth;
use grooviq_engine::STEP_TICKS;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// "GQN1", the first four bytes of a notes buffer (little-endian).
pub const NOTES_MAGIC: u32 = 0x314E_5147;
/// Bytes before the first note.
const HEADER_BYTES: usize = 16;
/// Bytes per note.
const NOTE_BYTES: usize = 24;
/// Bit 0 of a note's flags byte.
const FLAG_SLIDE: u8 = 1;

/// The pattern's notes in wire format v1, little-endian: a 16-byte header (magic, revision,
/// count, 0), then 24 bytes per note (id, channel, tick, length as u32, velocity as f32, key,
/// flags with bit 0 = slide, two zero bytes), sorted by (tick, channel, id).
pub fn encode_notes(pattern: &Pattern) -> Vec<u8> {
    let mut notes: Vec<&Note> = pattern.notes.iter().collect();
    notes.sort_unstable_by_key(|n| (n.tick, n.channel, n.id));
    let mut out = Vec::with_capacity(HEADER_BYTES + notes.len() * NOTE_BYTES);
    out.extend_from_slice(&NOTES_MAGIC.to_le_bytes());
    out.extend_from_slice(&pattern.revision.to_le_bytes());
    // At most `MAX_NOTES_PER_PATTERN` (validated on load, refused on add): it fits.
    out.extend_from_slice(&(notes.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for n in notes {
        out.extend_from_slice(&n.id.to_le_bytes());
        out.extend_from_slice(&n.channel.to_le_bytes());
        out.extend_from_slice(&n.tick.to_le_bytes());
        out.extend_from_slice(&n.length.to_le_bytes());
        out.extend_from_slice(&n.velocity.to_le_bytes());
        out.push(n.key);
        out.push(if n.slide { FLAG_SLIDE } else { 0 });
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// A note to add: the session gives it an id.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewNote {
    pub channel: u32,
    pub tick: u32,
    pub length: u32,
    pub key: u8,
    pub velocity: f32,
    pub slide: bool,
}

/// One edit to the current pattern's notes, made against `base_revision`: deletes, then
/// updates (whole notes, by id), then adds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesEdit {
    pub base_revision: u32,
    #[serde(default)]
    pub add: Vec<NewNote>,
    #[serde(default)]
    pub update: Vec<Note>,
    #[serde(default)]
    pub delete: Vec<u32>,
}

/// What [`crate::session::Session::edit_notes`] gives back.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesAck {
    /// The pattern's revision after the edit.
    pub revision: u32,
    /// The ids of `add`, in order.
    pub added_ids: Vec<u32>,
    /// Ids of `delete` and `update` the pattern did not have (in that order).
    pub ignored_ids: Vec<u32>,
    /// The edit was made against an older revision, or had values clamped: the UI's copy of
    /// the notes differs from the pattern, so it fetches them again.
    pub reload: bool,
    pub view: ProjectView,
}

/// What [`apply`] did.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    pub added_ids: Vec<u32>,
    pub ignored_ids: Vec<u32>,
    pub clamped: bool,
    /// What the edit's notes did to the length (see [`growth`]), if it updated or added any.
    pub growth: Option<Growth>,
}

/// Clamps `n` into the ranges `validate` keeps (a start past the last step is moved onto it);
/// true if anything changed.
fn clamp_note(n: &mut Note) -> bool {
    let fixed = Note {
        tick: n.tick.min(MAX_TICKS - 1),
        length: n.length.clamp(1, MAX_TICKS),
        key: n.key.min(127),
        velocity: if n.velocity.is_finite() {
            n.velocity.clamp(0.0, 1.0)
        } else {
            DEFAULT_VELOCITY
        },
        ..*n
    };
    // A NaN velocity compares unequal, and counts as clamped.
    let changed = fixed != *n;
    *n = fixed;
    changed
}

/// Applies `edit` to `p` (deletes, updates, adds), clamping values and growing `p.steps` to
/// the end of the bar (`bar_ticks` long) that holds the latest end among the notes it updated
/// or added. Notes left past a shortened end do not grow it back. Refused, with `p` half
/// changed (callers work on a copy), for a channel not in `channels`, an add past
/// [`MAX_NOTES_PER_PATTERN`] or used-up ids.
pub(crate) fn apply(
    p: &mut Pattern,
    edit: &NotesEdit,
    channels: &HashSet<u32>,
    bar_ticks: u32,
) -> Result<Applied, SessionError> {
    let mut out = Applied::default();
    let mut latest_end = 0u32;

    if !edit.delete.is_empty() {
        let delete: HashSet<u32> = edit.delete.iter().copied().collect();
        let present: HashSet<u32> = p
            .notes
            .iter()
            .filter(|n| delete.contains(&n.id))
            .map(|n| n.id)
            .collect();
        out.ignored_ids
            .extend(edit.delete.iter().filter(|id| !present.contains(id)));
        p.notes.retain(|n| !present.contains(&n.id));
    }

    if !edit.update.is_empty() {
        let at: HashMap<u32, usize> = p.notes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
        for u in &edit.update {
            let Some(&i) = at.get(&u.id) else {
                out.ignored_ids.push(u.id);
                continue;
            };
            if !channels.contains(&u.channel) {
                return Err(SessionError::UnknownChannel(u.channel));
            }
            let mut n = *u;
            out.clamped |= clamp_note(&mut n);
            latest_end = latest_end.max(n.tick + n.length);
            p.notes[i] = n;
        }
    }

    if p.notes.len() + edit.add.len() > MAX_NOTES_PER_PATTERN {
        return Err(SessionError::TooManyNotes);
    }
    p.notes.reserve(edit.add.len());
    for a in &edit.add {
        if !channels.contains(&a.channel) {
            return Err(SessionError::UnknownChannel(a.channel));
        }
        let id = p.take_note_id().ok_or(SessionError::NoteIdsUsedUp)?;
        let mut n = Note {
            id,
            channel: a.channel,
            tick: a.tick,
            length: a.length,
            key: a.key,
            velocity: a.velocity,
            slide: a.slide,
        };
        out.clamped |= clamp_note(&mut n);
        latest_end = latest_end.max(n.tick + n.length);
        p.notes.push(n);
        out.added_ids.push(id);
    }

    grow_to(p, latest_end, bar_ticks);
    out.growth = growth(latest_end, bar_ticks);
    Ok(out)
}

/// The growth [`grow_to`] applies for notes ending at `end` with bars `bar_ticks` long, for
/// the undo history's length log: `None` if it never grows anything.
fn growth(end: u32, bar_ticks: u32) -> Option<Growth> {
    if end == 0 || bar_ticks == 0 {
        return None;
    }
    let bars = end.div_ceil(bar_ticks);
    Some(Growth {
        end,
        to: (bars * bar_ticks / STEP_TICKS).min(MAX_STEPS),
    })
}

/// Lengthens `p` to the end of the bar holding tick `end` if `end` is past its end: never
/// shorter, at most [`MAX_STEPS`].
fn grow_to(p: &mut Pattern, end: u32, bar_ticks: u32) {
    if end <= p.steps * STEP_TICKS || bar_ticks == 0 {
        return;
    }
    let bars = end.div_ceil(bar_ticks);
    let steps = (bars * bar_ticks / STEP_TICKS).min(MAX_STEPS);
    p.steps = p.steps.max(steps);
}
