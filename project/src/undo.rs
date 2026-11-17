//! Undo and redo of note changes to the current pattern.
//!
//! Every note change (a [`crate::notes::NotesEdit`], one step edit, one recorded take) is kept
//! as a [`NoteChange`]: the changed notes as they were and as they became, by id, and the
//! pattern's length before and after. Undo puts the old notes back under their own ids; redo
//! puts the new ones back. Ids are never handed out twice (`Pattern::take_note_id` only moves
//! forward), so a note brought back can never meet another note with its id. The history is
//! per project and never saved.

use crate::model::{MAX_NOTES_PER_PATTERN, Note, Pattern};
use crate::session::SessionError;
use std::collections::{HashMap, HashSet, VecDeque};

/// The most note changes undo goes back through; the oldest drops past it.
pub const MAX_UNDO: usize = 200;
/// The most notes the history keeps over all its changes (about 48 MB): 200 moves of a full
/// pattern would hold 40 million. The oldest changes drop past it; the newest always stays.
const MAX_UNDO_NOTES: usize = 2_000_000;

/// One note change to a pattern, and so its exact inverse.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NoteChange {
    /// The changed notes as they were: deleted ones in full, and updated ones' old values.
    before: Vec<Note>,
    /// The changed notes as they became: added ones in full, and updated ones' new values.
    after: Vec<Note>,
    steps_before: u32,
    steps_after: u32,
}

impl NoteChange {
    /// What changed from `old` to `new`, or `None` if their notes are the same.
    pub(crate) fn between(old: &Pattern, new: &Pattern) -> Option<Self> {
        // Edits keep the order of the notes they leave and add at the end: skip the common
        // start, then match the rest by id.
        let same = old
            .notes
            .iter()
            .zip(&new.notes)
            .take_while(|(a, b)| a == b)
            .count();
        let (old_rest, new_rest) = (&old.notes[same..], &new.notes[same..]);
        if old_rest.is_empty() && new_rest.is_empty() {
            return None;
        }
        let mut was: HashMap<u32, &Note> = old_rest.iter().map(|n| (n.id, n)).collect();
        let mut before = Vec::new();
        let mut after = Vec::new();
        for n in new_rest {
            match was.remove(&n.id) {
                Some(o) if o == n => {}
                Some(o) => {
                    before.push(*o);
                    after.push(*n);
                }
                None => after.push(*n),
            }
        }
        before.extend(was.into_values().copied());
        if before.is_empty() && after.is_empty() {
            // Only the order changed.
            return None;
        }
        before.sort_unstable_by_key(|n| n.id);
        after.sort_unstable_by_key(|n| n.id);
        Some(Self {
            before,
            after,
            steps_before: old.steps,
            steps_after: new.steps,
        })
    }

    /// Notes held, for the history's budget.
    fn size(&self) -> usize {
        self.before.len() + self.after.len()
    }

    /// Takes `p` back to before the change (`undo`), or forward to after it.
    ///
    /// The notes it takes out are found by id, and the ones it puts back keep theirs. The
    /// length moves only if it is still the one the change left: a length set by hand since
    /// then stays. Refused, with `p` half changed (callers work on a copy), if the notes would
    /// pass [`MAX_NOTES_PER_PATTERN`] (they cannot: the history is cleared by every change it
    /// does not hold).
    pub(crate) fn apply(&self, p: &mut Pattern, undo: bool) -> Result<(), SessionError> {
        let (take, put, from, to) = if undo {
            (
                &self.after,
                &self.before,
                self.steps_after,
                self.steps_before,
            )
        } else {
            (
                &self.before,
                &self.after,
                self.steps_before,
                self.steps_after,
            )
        };
        let ids: HashSet<u32> = take.iter().chain(put).map(|n| n.id).collect();
        p.notes.retain(|n| !ids.contains(&n.id));
        if p.notes.len() + put.len() > MAX_NOTES_PER_PATTERN {
            return Err(SessionError::TooManyNotes);
        }
        debug_assert!(
            put.iter().all(|n| n.id < p.next_note_id),
            "an id not issued"
        );
        p.notes.extend_from_slice(put);
        // Ids rise in the order notes are added: back in place, the pattern is as it was.
        p.notes.sort_unstable_by_key(|n| n.id);
        if p.steps == from {
            p.steps = to;
        }
        Ok(())
    }
}

/// The current pattern's note changes, for undo and redo. Per project, never saved.
#[derive(Debug, Default)]
pub struct NoteHistory {
    undo: VecDeque<NoteChange>,
    redo: Vec<NoteChange>,
    /// Notes held by `undo` and `redo` together.
    notes: usize,
}

impl NoteHistory {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Forgets every change: a project replaced, or a change the history cannot undo.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Records a new change: redo is cleared, and the oldest changes drop past [`MAX_UNDO`]
    /// or the note budget.
    pub(crate) fn push(&mut self, change: NoteChange) {
        self.notes -= self.redo.iter().map(NoteChange::size).sum::<usize>();
        self.redo.clear();
        self.notes += change.size();
        self.undo.push_back(change);
        while self.undo.len() > MAX_UNDO || (self.notes > MAX_UNDO_NOTES && self.undo.len() > 1) {
            if let Some(old) = self.undo.pop_front() {
                self.notes -= old.size();
            }
        }
    }

    /// The change undo would take back.
    pub(crate) fn next_undo(&self) -> Option<&NoteChange> {
        self.undo.back()
    }

    /// The change redo would make again.
    pub(crate) fn next_redo(&self) -> Option<&NoteChange> {
        self.redo.last()
    }

    /// The change from [`Self::next_undo`] was undone: redo can make it again.
    pub(crate) fn undone(&mut self) {
        if let Some(change) = self.undo.pop_back() {
            self.redo.push(change);
        }
    }

    /// The change from [`Self::next_redo`] was made again: undo can take it back.
    pub(crate) fn redone(&mut self) {
        if let Some(change) = self.redo.pop() {
            self.undo.push_back(change);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32) -> Note {
        Note {
            id,
            channel: 1,
            tick: 0,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
        }
    }

    fn change(notes: usize) -> NoteChange {
        NoteChange {
            before: Vec::new(),
            after: (0..notes as u32).map(note).collect(),
            steps_before: 16,
            steps_after: 16,
        }
    }

    #[test]
    fn the_history_keeps_at_most_its_note_budget_but_always_the_newest() {
        let mut h = NoteHistory::default();
        let big = MAX_UNDO_NOTES / 4;
        for _ in 0..6 {
            h.push(change(big));
        }
        assert_eq!(h.undo.len(), 4);
        assert_eq!(h.notes, 4 * big);
        // One change over the whole budget still stays, alone.
        h.push(change(MAX_UNDO_NOTES + 1));
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.notes, MAX_UNDO_NOTES + 1);
        // Moving between the stacks keeps the count; a new change drops redo's notes.
        h.push(change(3));
        assert_eq!(h.undo.len(), 1, "the big one dropped");
        h.undone();
        assert_eq!(h.notes, 3);
        h.push(change(2));
        assert_eq!((h.notes, h.redo.len()), (2, 0));
    }

    #[test]
    fn a_reordering_is_no_change() {
        let mut old = Pattern::new(0, "P");
        old.notes = vec![note(1), note(2)];
        let mut new = old.clone();
        new.notes.reverse();
        assert_eq!(NoteChange::between(&old, &new), None);
        assert_eq!(NoteChange::between(&old, &old), None);
    }
}
