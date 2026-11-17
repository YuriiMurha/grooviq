//! Undo and redo of note changes to the current pattern.
//!
//! Every note change (a [`crate::notes::NotesEdit`], one step edit, one recorded take) is kept
//! as a [`NoteChange`]. A recorded take is committed poll by poll; its commits join one open
//! step until the take ends (see [`NoteHistory::record`]). Each step holds the changed notes
//! as they were and as they became, by id, and the length change it made, if any. Undo puts
//! the old notes back under their own ids; redo puts the new ones back. Ids are never handed out twice (`Pattern::take_note_id` only moves
//! forward), so a note brought back can never meet another note with its id. The history is
//! per project and never saved.

use crate::model::{MAX_NOTES_PER_PATTERN, Note, Pattern};
use crate::session::SessionError;
use std::collections::{HashMap, HashSet, VecDeque};

/// The most note changes undo goes back through; the oldest drops past it.
pub const MAX_UNDO: usize = 200;
/// The most notes the history keeps over all its changes (about 48 MB): 200 moves of a full
/// pattern would hold 40 million. The oldest changes drop past it; the newest always stays.
pub const MAX_UNDO_NOTES: usize = 2_000_000;

/// One note change to a pattern, and so its exact inverse.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NoteChange {
    /// The changed notes as they were: deleted ones in full, and updated ones' old values.
    before: Vec<Note>,
    /// The changed notes as they became: added ones in full, and updated ones' new values.
    after: Vec<Note>,
    /// The pattern's length before and after, if the change itself moved it. A length set by
    /// hand is not a note change and is never part of one.
    length: Option<(u32, u32)>,
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
            length: (old.steps != new.steps).then_some((old.steps, new.steps)),
        })
    }

    /// Folds `next`, a later note change, into this one if `next` only adds notes newer than
    /// every note this change touched (what a recording commit does): the added notes join
    /// `after`. Otherwise changes nothing and gives `next` back.
    ///
    /// A length change of `next` joins this one's when it carries on from it (the length
    /// before `next` is the one this change left); when the length was set by hand in
    /// between, only `next`'s own change is kept, so undo never takes back a length set by
    /// hand.
    fn absorb(&mut self, next: Self) -> Result<(), Self> {
        let newest = self.before.iter().chain(&self.after).map(|n| n.id).max();
        let fresh = next
            .after
            .iter()
            .all(|n| newest.is_none_or(|newest| n.id > newest));
        if !next.before.is_empty() || !fresh {
            return Err(next);
        }
        // Ids rise as notes are added: `after` stays sorted.
        self.after.extend(next.after);
        self.length = match (self.length, next.length) {
            (Some((first, left)), Some((from, to))) if from == left => Some((first, to)),
            (_, Some(moved)) => Some(moved),
            (own, None) => own,
        };
        Ok(())
    }

    /// Notes held, for the history's budget.
    fn size(&self) -> usize {
        self.before.len() + self.after.len()
    }

    /// Takes `p` back to before the change (`undo`), or forward to after it.
    ///
    /// The notes it takes out are found by id, and the ones it puts back keep theirs. The
    /// length moves only if the change moved it and it is still the one the change left: a
    /// length set by hand since then stays. Refused, with `p` half changed (callers work on a copy), if the notes would
    /// pass [`MAX_NOTES_PER_PATTERN`] (they cannot: the history is cleared by every change it
    /// does not hold).
    pub(crate) fn apply(&self, p: &mut Pattern, undo: bool) -> Result<(), SessionError> {
        let (take, put) = if undo {
            (&self.after, &self.before)
        } else {
            (&self.before, &self.after)
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
        if let Some((before, after)) = self.length {
            let (from, to) = if undo {
                (after, before)
            } else {
                (before, after)
            };
            if p.steps == from {
                p.steps = to;
            }
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
    /// The take (see `grooviq_engine::RecordEvent::Note`) the newest undo step holds, while
    /// it is open: that take's next commits join it.
    take: Option<u32>,
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

    /// Records a new change (it closes an open take): redo is cleared, and the oldest changes
    /// drop past [`MAX_UNDO`] or [`MAX_UNDO_NOTES`].
    pub(crate) fn push(&mut self, change: NoteChange) {
        self.take = None;
        self.notes -= self.redo.iter().map(NoteChange::size).sum::<usize>();
        self.redo.clear();
        self.notes += change.size();
        self.undo.push_back(change);
        self.trim();
    }

    /// Records a commit of recorded take `take`: it joins that take's step while the step is
    /// open (a take is one step however many commits it takes), or opens a new one. Another
    /// take, any other note change, undo, redo and clearing close the step.
    pub(crate) fn record(&mut self, mut change: NoteChange, take: u32) {
        if self.take == Some(take)
            && let Some(top) = self.undo.back_mut()
        {
            let size = change.size();
            match top.absorb(change) {
                Ok(()) => {
                    self.notes += size;
                    self.trim();
                    return;
                }
                Err(back) => change = back,
            }
        }
        self.push(change);
        self.take = Some(take);
    }

    /// Drops the oldest changes past [`MAX_UNDO`] or [`MAX_UNDO_NOTES`]; the newest stays.
    fn trim(&mut self) {
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

    /// The change from [`Self::next_undo`] was undone (a running take ends there): redo can
    /// make it again.
    pub(crate) fn undone(&mut self) {
        self.take = None;
        if let Some(change) = self.undo.pop_back() {
            self.redo.push(change);
        }
    }

    /// The change from [`Self::next_redo`] was made again: undo can take it back.
    pub(crate) fn redone(&mut self) {
        self.take = None;
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
            length: None,
        }
    }

    fn add(ids: std::ops::Range<u32>, length: Option<(u32, u32)>) -> NoteChange {
        NoteChange {
            before: Vec::new(),
            after: ids.map(note).collect(),
            length,
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
        // The live fixture's case (app/ui/e2e/m2-undo.e2e.ts): eleven edits of all 100,000
        // notes hold 200,000 changed notes each, and ten stay.
        h.clear();
        for _ in 0..11 {
            h.push(change(200_000));
        }
        assert_eq!(h.undo.len(), 10);
    }

    #[test]
    fn a_take_folds_only_its_own_fresh_additions() {
        let mut h = NoteHistory::default();
        h.record(add(1..3, None), 7);
        h.record(add(3..5, None), 7);
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.undo[0].after.len(), 4);
        assert_eq!(h.notes, 4);
        // An addition under an id the take already holds is not a fresh one.
        h.record(add(4..6, None), 7);
        assert_eq!(h.undo.len(), 2);
        assert_eq!(h.undo[0].after.len(), 4);
        // Another take opens its own step.
        h.record(add(6..7, None), 8);
        assert_eq!(h.undo.len(), 3);
        // A change that is not a pure addition never folds in: it opens its own step (and the
        // take goes on there).
        let mut update = add(1..2, None);
        update.before = vec![note(1)];
        h.record(update, 8);
        assert_eq!(h.undo.len(), 4);
        h.record(add(9..10, None), 8);
        assert_eq!(h.undo.len(), 4);
        // Any other change closes the take: its next commit opens a step of its own.
        h.push(add(10..11, None));
        h.record(add(11..12, None), 8);
        assert_eq!(h.undo.len(), 6);
    }

    #[test]
    fn a_take_owns_only_the_length_changes_it_made() {
        let fold = |first: Option<(u32, u32)>, next: Option<(u32, u32)>| {
            let mut c = add(1..2, first);
            c.absorb(add(2..3, next)).unwrap();
            c.length
        };
        assert_eq!(fold(None, None), None);
        assert_eq!(fold(Some((16, 32)), None), Some((16, 32)));
        assert_eq!(fold(None, Some((16, 32))), Some((16, 32)));
        // Growing on from its own growth: one change from the take's start.
        assert_eq!(fold(Some((16, 32)), Some((32, 48))), Some((16, 48)));
        // The length was set by hand in between: only the later growth is the take's.
        assert_eq!(fold(Some((16, 32)), Some((64, 80))), Some((64, 80)));
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
