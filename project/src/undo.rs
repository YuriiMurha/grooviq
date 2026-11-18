//! Undo and redo of note changes to the current pattern.
//!
//! Every note change (a [`crate::notes::NotesEdit`], one step edit, one recorded take) is kept
//! as a [`NoteChange`]: the changed notes as they were and as they became, by id. A recorded
//! take is committed poll by poll; its commits join its step (see [`NoteHistory::record`]).
//! Undo puts the old notes back under their own ids; redo puts the new ones back. Ids are
//! never handed out twice (`Pattern::take_note_id` only moves forward), so a note brought back
//! can never meet another note with its id. The history is per project and never saved.
//!
//! The pattern's length is kept apart from the notes, as a log ([`NoteHistory`]): the length
//! when the history began, then, in order, every commit's growth (owned by its step) and every
//! length set by hand. The length is always the log played through with the growth of the
//! steps in effect, so undoing a take whose growth is interleaved with newer takes leaves the
//! length the remaining notes produce, undoing everything leaves the first length (or the last
//! one set by hand), and redo puts the same growth back in the same place.

use crate::model::{MAX_NOTES_PER_PATTERN, Note, Pattern};
use crate::session::SessionError;
use grooviq_engine::STEP_TICKS;
use std::collections::{HashMap, HashSet, VecDeque};

/// The most note changes undo goes back through; the oldest drops past it.
pub const MAX_UNDO: usize = 200;
/// The most notes the history keeps over all its changes (about 48 MB): 200 moves of a full
/// pattern would hold 40 million. The oldest changes drop past it; the newest always stays.
pub const MAX_UNDO_NOTES: usize = 2_000_000;

/// One note change to a pattern, and so its exact inverse (its length is in the log).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NoteChange {
    /// The changed notes as they were: deleted ones in full, and updated ones' old values.
    before: Vec<Note>,
    /// The changed notes as they became: added ones in full, and updated ones' new values.
    after: Vec<Note>,
}

/// What a commit did to the length: the latest end among the notes it added or moved, and the
/// length (in steps) that end grows the pattern to (`notes::apply`'s rule, with the bar of the
/// time, `bar` ticks). The pattern grows to `to` if `end` is past its end then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Growth {
    pub end: u32,
    pub to: u32,
    pub bar: u32,
}

impl Growth {
    /// The length after this growth, from `steps`.
    fn from(self, steps: u32) -> u32 {
        if self.end > steps * STEP_TICKS {
            steps.max(self.to)
        } else {
            steps
        }
    }
}

impl NoteChange {
    pub(crate) fn added(notes: Vec<Note>) -> Self {
        Self {
            before: Vec::new(),
            after: notes,
        }
    }
    /// A change of no notes: an edit that only grew the pattern (it resubmitted notes as they
    /// were, after the length was shortened by hand). Its growth is its whole effect.
    pub(crate) fn none() -> Self {
        Self {
            before: Vec::new(),
            after: Vec::new(),
        }
    }

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
        Some(Self { before, after })
    }

    /// Folds `next`, a later note change, into this one if `next` only adds notes newer than
    /// every note this change touched (what a recording commit does): the added notes join
    /// `after`. Otherwise changes nothing and gives `next` back.
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
        Ok(())
    }

    /// Notes held, for the history's budget.
    fn size(&self) -> usize {
        self.before.len() + self.after.len()
    }

    /// Takes `p`'s notes back to before the change (`undo`), or forward to after it; the
    /// length is [`NoteHistory::length_after`]'s. The notes it takes out are found by id, and
    /// the ones it puts back keep theirs. Refused, with `p` half changed (callers work on a
    /// copy), if the notes would pass [`MAX_NOTES_PER_PATTERN`] (they cannot: the history is
    /// cleared by every change it does not hold).
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
        Ok(())
    }
}

/// One undo step: its change, its id in the length log, and the recorded take it holds while
/// it is open to that take's later notes.
#[derive(Debug)]
struct Step {
    change: NoteChange,
    id: u64,
    take: Option<u32>,
    order: u64,
}

/// One entry of the length log.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Length {
    /// A commit's growth, owned by step `owner` (`None`: a step that dropped out of the
    /// history, whose growth stays for good).
    Grew { owner: Option<u64>, growth: Growth },
    /// A length set by hand. Undo never takes it back.
    Set(u32),
}

/// The current pattern's note changes, for undo and redo, and its length log (see the module
/// docs). Per project, never saved.
///
/// A recorded take's step stays open to the take's later notes (a note can complete long
/// after its take's other notes, below a newer take's step) until any other note change, an
/// undo, a redo or a clear closes every open step. Newer steps above an open one are then only
/// other takes' additions, which never touch its notes, so folding into it keeps every inverse
/// exact.
#[derive(Debug, Default)]
pub struct NoteHistory {
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// Notes held by `undo` and `redo` together.
    notes: usize,
    /// The length the log starts from (when the history began, then with the entries that can
    /// no longer change folded in).
    base: u32,
    log: Vec<Length>,
    next_id: u64,
}

impl NoteHistory {
    pub(crate) fn retained(&self) -> (usize, usize) {
        (self.undo.len() + self.redo.len(), self.notes)
    }
    pub(crate) fn oldest(&self) -> Option<u64> {
        self.undo.iter().chain(&self.redo).map(|s| s.order).min()
    }
    pub(crate) fn evict_oldest(&mut self) {
        let Some(order) = self.oldest() else {
            return;
        };
        let active = self.undo.iter().any(|s| s.order == order);
        let list = if active {
            None
        } else {
            self.redo.iter().position(|s| s.order == order)
        };
        let step = if let Some(i) = list {
            self.redo.remove(i)
        } else {
            self.undo.pop_front().unwrap()
        };
        self.notes -= step.change.size();
        if active {
            for l in &mut self.log {
                if let Length::Grew { owner, .. } = l
                    && *owner == Some(step.id)
                {
                    *owner = None;
                }
            }
        } else {
            self.log
                .retain(|l| !matches!(l, Length::Grew { owner: Some(id), .. } if *id == step.id));
        }
        self.compact();
    }

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

    /// Entries in the length log: a test hook for its bound. Not part of the API.
    #[doc(hidden)]
    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    fn empty(&self) -> bool {
        self.undo.is_empty() && self.redo.is_empty()
    }

    /// Records a new change, made from a pattern `steps` long and growing it by `growth`; it
    /// closes every open take step. Redo is cleared, and the oldest changes drop past
    /// [`MAX_UNDO`] or [`MAX_UNDO_NOTES`].
    pub(crate) fn push(&mut self, change: NoteChange, steps: u32, growth: Option<Growth>) {
        self.close();
        let id = self.add(change, None, steps);
        self.grew(id, growth);
    }

    /// Records a commit of recorded take `take` (made from a pattern `steps` long, growing it
    /// by `growth`): it joins that take's open step, wherever it sits (a take is one step
    /// however many commits it takes), or opens a new step.
    pub(crate) fn record(
        &mut self,
        mut change: NoteChange,
        take: u32,
        steps: u32,
        growth: Option<Growth>,
    ) {
        if let Some(step) = self.undo.iter_mut().rev().find(|s| s.take == Some(take)) {
            let size = change.size();
            match step.change.absorb(change) {
                Ok(()) => {
                    let id = step.id;
                    self.notes += size;
                    self.grew(id, growth);
                    self.trim();
                    return;
                }
                Err(back) => change = back,
            }
        }
        let id = self.add(change, Some(take), steps);
        self.grew(id, growth);
    }

    /// The pattern's length was set by hand to `steps`: no undo takes it back.
    pub(crate) fn set_length(&mut self, steps: u32) {
        if !self.empty() {
            append(&mut self.log, Length::Set(steps));
            self.fold();
        }
    }

    fn grew(&mut self, owner: u64, growth: Option<Growth>) {
        if let Some(growth) = growth {
            let owner = Some(owner);
            append(&mut self.log, Length::Grew { owner, growth });
        }
    }

    /// Plays the log's leading entries that no step can take back (lengths set by hand, and
    /// the growth of steps that left the history) into its start.
    fn fold(&mut self) {
        let fixed = self
            .log
            .iter()
            .take_while(|l| !matches!(l, Length::Grew { owner: Some(_), .. }))
            .count();
        if fixed > 0 {
            self.base = play(self.base, &self.log[..fixed], |_| true);
            self.log.drain(..fixed);
        }
    }

    /// Merges what [`append`] merges all through the log, then folds its start: after entries
    /// left it (redo cleared) or became fixed (steps dropped). The log then holds at most one
    /// length set by hand between two growths, and one growth per run of a step's commits
    /// with one bar, so its size follows the history's steps and commits, never the number of
    /// commands.
    fn compact(&mut self) {
        let mut log = Vec::with_capacity(self.log.len());
        for l in self.log.drain(..) {
            append(&mut log, l);
        }
        self.log = log;
        self.fold();
    }

    /// Adds a step (open to `take`, if any) and returns its id: redo is cleared (it is a note
    /// change, and the growth of what it held leaves the log), and the history trimmed. The
    /// first step starts the log at `steps`.
    fn add(&mut self, change: NoteChange, take: Option<u32>, steps: u32) -> u64 {
        if self.empty() {
            self.base = steps;
            self.log.clear();
        }
        self.notes -= self.redo.iter().map(|s| s.change.size()).sum::<usize>();
        let gone: HashSet<u64> = self.redo.drain(..).map(|s| s.id).collect();
        if !gone.is_empty() {
            self.log
                .retain(|l| !matches!(l, Length::Grew { owner: Some(o), .. } if gone.contains(o)));
            self.compact();
        }
        let id = self.next_id;
        self.next_id += 1;
        self.notes += change.size();
        static ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let order = ORDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.undo.push_back(Step {
            change,
            id,
            take,
            order,
        });
        self.trim();
        id
    }

    /// Closes every open take step: a take's later notes start a new step.
    fn close(&mut self) {
        for step in &mut self.undo {
            step.take = None;
        }
    }

    /// Drops the oldest changes past [`MAX_UNDO`] or [`MAX_UNDO_NOTES`] (the newest stays):
    /// their growth stays for good, folded into the log's start.
    fn trim(&mut self) {
        let mut dropped = HashSet::new();
        while self.undo.len() > MAX_UNDO || (self.notes > MAX_UNDO_NOTES && self.undo.len() > 1) {
            if let Some(old) = self.undo.pop_front() {
                self.notes -= old.change.size();
                dropped.insert(old.id);
            }
        }
        if dropped.is_empty() {
            return;
        }
        for l in &mut self.log {
            if let Length::Grew { owner, .. } = l
                && owner.is_some_and(|o| dropped.contains(&o))
            {
                *owner = None;
            }
        }
        self.compact();
    }

    /// The change undo would take back.
    pub(crate) fn next_undo(&self) -> Option<&NoteChange> {
        self.undo.back().map(|s| &s.change)
    }

    /// The change redo would make again.
    pub(crate) fn next_redo(&self) -> Option<&NoteChange> {
        self.redo.last().map(|s| &s.change)
    }

    /// The pattern's length once [`Self::next_undo`] is undone (`undo`), or [`Self::next_redo`]
    /// made again: the log played through with the growth of every step in effect then.
    pub(crate) fn length_after(&self, undo: bool) -> u32 {
        let mut live: HashSet<u64> = self.undo.iter().map(|s| s.id).collect();
        if undo {
            if let Some(top) = self.undo.back() {
                live.remove(&top.id);
            }
        } else if let Some(top) = self.redo.last() {
            live.insert(top.id);
        }
        play(self.base, &self.log, |owner| live.contains(&owner))
    }

    /// The change from [`Self::next_undo`] was undone (open takes close): redo can make it
    /// again.
    pub(crate) fn undone(&mut self) {
        self.close();
        if let Some(step) = self.undo.pop_back() {
            self.redo.push(step);
        }
    }

    /// The change from [`Self::next_redo`] was made again (open takes close): undo can take
    /// it back.
    pub(crate) fn redone(&mut self) {
        self.close();
        if let Some(mut step) = self.redo.pop() {
            step.take = None;
            self.undo.push_back(step);
        }
    }
}

/// Adds `entry` to the end of `log`, merging it with the last entry when that changes nothing
/// the log plays to: a length set by hand replaces one set right before it, and a step's
/// growth after its own growth with the same bar keeps only the one reaching further (with one
/// bar, the length a note end grows to rises with the end, so growing to the further end does
/// what growing to both in turn does, from any length).
fn append(log: &mut Vec<Length>, entry: Length) {
    match (log.last_mut(), entry) {
        (Some(Length::Set(last)), Length::Set(steps)) => *last = steps,
        (
            Some(Length::Grew {
                owner: was,
                growth: last,
            }),
            Length::Grew { owner, growth },
        ) if *was == owner && last.bar == growth.bar => {
            if growth.end > last.end {
                *last = growth;
            }
        }
        _ => log.push(entry),
    }
}

/// Plays `log` from `base`: every length set by hand, and the growth of the steps `live` says
/// are in effect (and of steps that left the history).
fn play(base: u32, log: &[Length], live: impl Fn(u64) -> bool) -> u32 {
    log.iter().fold(base, |steps, l| match *l {
        Length::Set(set) => set,
        Length::Grew { owner, growth } if owner.is_none_or(&live) => growth.from(steps),
        Length::Grew { .. } => steps,
    })
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
        }
    }

    fn add(ids: std::ops::Range<u32>) -> NoteChange {
        NoteChange {
            before: Vec::new(),
            after: ids.map(note).collect(),
        }
    }

    /// Growth to `to` steps by a note ending one tick past `from` steps.
    fn grow(from: u32, to: u32) -> Option<Growth> {
        Some(Growth {
            end: from * STEP_TICKS + 1,
            to,
            bar: 3_840,
        })
    }

    #[test]
    fn the_history_keeps_at_most_its_note_budget_but_always_the_newest() {
        let mut h = NoteHistory::default();
        let big = MAX_UNDO_NOTES / 4;
        for _ in 0..6 {
            h.push(change(big), 16, None);
        }
        assert_eq!(h.undo.len(), 4);
        assert_eq!(h.notes, 4 * big);
        // One change over the whole budget still stays, alone.
        h.push(change(MAX_UNDO_NOTES + 1), 16, None);
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.notes, MAX_UNDO_NOTES + 1);
        // Moving between the stacks keeps the count; a new change drops redo's notes.
        h.push(change(3), 16, None);
        assert_eq!(h.undo.len(), 1, "the big one dropped");
        h.undone();
        assert_eq!(h.notes, 3);
        h.push(change(2), 16, None);
        assert_eq!((h.notes, h.redo.len()), (2, 0));
        // The live fixture's case (app/ui/e2e/m2-undo.e2e.ts): eleven edits of all 100,000
        // notes hold 200,000 changed notes each, and ten stay.
        h.clear();
        for _ in 0..11 {
            h.push(change(200_000), 16, None);
        }
        assert_eq!(h.undo.len(), 10);
    }

    #[test]
    fn a_take_folds_only_its_own_fresh_additions() {
        let mut h = NoteHistory::default();
        h.record(add(1..3), 7, 16, None);
        h.record(add(3..5), 7, 16, None);
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.undo[0].change.after.len(), 4);
        assert_eq!(h.notes, 4);
        // An addition under an id the take already holds is not a fresh one.
        h.record(add(4..6), 7, 16, None);
        assert_eq!(h.undo.len(), 2);
        assert_eq!(h.undo[0].change.after.len(), 4);
        // Another take opens its own step.
        h.record(add(6..7), 8, 16, None);
        assert_eq!(h.undo.len(), 3);
        // A change that is not a pure addition never folds in: it opens its own step (and the
        // take goes on there).
        let mut update = add(1..2);
        update.before = vec![note(1)];
        h.record(update, 8, 16, None);
        assert_eq!(h.undo.len(), 4);
        h.record(add(9..10), 8, 16, None);
        assert_eq!(h.undo.len(), 4);
        // Any other change closes the take: its next commit opens a step of its own.
        h.push(add(10..11), 16, None);
        h.record(add(11..12), 8, 16, None);
        assert_eq!(h.undo.len(), 6);
    }

    #[test]
    fn the_length_is_the_log_played_with_the_steps_in_effect() {
        // Take A grows 16 -> 32, take B 32 -> 48, A's late note 48 -> 64, C 64 -> 80, A's
        // next late note 80 -> 96 (the review's interleaving).
        let mut h = NoteHistory::default();
        h.record(add(1..2), 1, 16, grow(16, 32));
        h.record(add(2..3), 2, 32, grow(32, 48));
        h.record(add(3..4), 1, 48, grow(48, 64));
        h.record(add(4..5), 3, 64, grow(64, 80));
        h.record(add(5..6), 1, 80, grow(80, 96));
        assert_eq!(h.undo.len(), 3);
        // Undoing C: A's growth on top of it still needs 96; then B; then A gives 16.
        let mut lengths = Vec::new();
        while h.can_undo() {
            lengths.push(h.length_after(true));
            h.undone();
        }
        assert_eq!(lengths, vec![96, 96, 16]);
        // Redo walks the same lengths back.
        let mut back = Vec::new();
        while h.can_redo() {
            back.push(h.length_after(false));
            h.redone();
        }
        assert_eq!(back, vec![96, 96, 96]);
    }

    #[test]
    fn a_growth_that_did_not_grow_counts_once_an_earlier_one_is_undone() {
        let mut h = NoteHistory::default();
        h.push(add(1..2), 16, grow(16, 32));
        // A note past 16 steps that did not grow the 32-step pattern.
        h.push(add(2..3), 32, grow(16, 32));
        h.undone();
        h.undone();
        // Redo only the second: its note still needs 32 steps.
        h.redo.swap(0, 1);
        assert_eq!(h.length_after(false), 32);
    }

    #[test]
    fn a_length_set_by_hand_stays_and_redo_plays_over_it() {
        let mut h = NoteHistory::default();
        h.record(add(1..2), 1, 16, grow(16, 32));
        h.set_length(64);
        h.record(add(2..3), 1, 64, grow(64, 80));
        assert_eq!(h.length_after(true), 64, "the length set by hand stays");
        h.undone();
        assert_eq!(h.length_after(false), 80);
        // Set by hand after the undo: redo keeps it (the redone note lies past the end,
        // stored and unplayed, as after any shortening).
        h.set_length(8);
        assert_eq!(h.length_after(false), 8);
    }

    #[test]
    fn dropped_steps_keep_their_growth_for_good() {
        let mut h = NoteHistory::default();
        h.push(add(0..1), 16, grow(16, 32));
        for i in 1..=MAX_UNDO as u32 {
            h.push(add(i..i + 1), 32, None);
        }
        assert_eq!(h.undo.len(), MAX_UNDO);
        assert_eq!(
            h.base, 32,
            "the first step's growth is folded into the start"
        );
        assert!(h.log.is_empty());
        while h.can_undo() {
            assert_eq!(h.length_after(true), 32);
            h.undone();
        }
    }

    #[test]
    fn lengths_set_by_hand_in_a_row_keep_only_the_last() {
        let mut h = NoteHistory::default();
        h.push(add(1..2), 16, grow(16, 32));
        for steps in 1..=100_000 {
            h.set_length(steps % 256 + 1);
        }
        assert_eq!(h.log.len(), 2);
        assert_eq!(h.log[1], Length::Set(100_000 % 256 + 1));
        h.undone();
        assert_eq!(h.length_after(false), 100_000 % 256 + 1);
    }

    #[test]
    fn a_steps_growths_in_a_row_with_one_bar_merge() {
        let mut h = NoteHistory::default();
        for (i, end) in [5_000, 4_000, 9_000, 7_000].into_iter().enumerate() {
            let growth = Growth {
                end,
                to: end.div_ceil(3_840) * 16,
                bar: 3_840,
            };
            h.record(add(i as u32..i as u32 + 1), 1, 16, Some(growth));
        }
        assert_eq!(h.log.len(), 1, "{:?}", h.log);
        // Played, the one growth still reaches the furthest end (9,000 ticks: 48 steps).
        assert_eq!(play(h.base, &h.log, |_| true), 48);
        assert_eq!(h.length_after(true), 16);
        // Another bar does not merge.
        let other = Growth {
            end: 10_000,
            to: 42,
            bar: 1_440,
        };
        h.record(add(9..10), 1, 48, Some(other));
        assert_eq!(h.log.len(), 2);
    }

    #[test]
    fn cleared_redo_takes_its_growth_out_of_the_log() {
        let mut h = NoteHistory::default();
        for i in 0..1_000 {
            h.push(add(i..i + 1), 16, grow(16, 32));
            h.set_length(16);
            h.undone();
        }
        // Each new step dropped the undone one's growth and folded the lengths set by hand.
        assert!(h.log.len() <= 2, "{:?}", h.log);
        assert_eq!(h.base, 16);
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
