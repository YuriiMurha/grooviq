//! Turns the engine's recorded live events into notes.
//!
//! The engine's recording tap ([`RecordEvent`]) gives each live note-on and note-off with the
//! tick played since Play (`tick`, for lengths) and the loop position heard then (`position`,
//! where the note goes). A [`Recorder`] pairs a note-off with the held note-on of the same
//! press (channel, key and [`NoteId`]), so presses of one key held at once each keep their own
//! length whichever is let go first, and closes every note still held at a take's `End`.
//!
//! It can also wait for a barrier ([`RecordEvent::Mark`]) and drop everything before it:
//! silently after a new engine state (those events belong to the project before), or counting
//! every refused note while recording switches itself off ([`Recorder::refuse_until_mark`]).

use crate::notes::NewNote;
use grooviq_engine::{NoteId, RecordEvent};
use std::collections::{HashMap, VecDeque};

/// A note-on still waiting for its note-off.
#[derive(Clone, Copy, Debug)]
struct Held {
    tick: u64,
    position: u32,
    velocity: f32,
    /// The take of the note-on (see [`RecordEvent::Note`]).
    take: u32,
    /// The press its note-off names.
    id: NoteId,
}

/// What the recorder waits for while it drops events.
#[derive(Clone, Copy, Debug)]
struct Wait {
    /// The barrier that ends the wait; `None` until it is queued.
    mark: Option<u32>,
    /// Count every note dropped (note-ons, and notes held when the wait began).
    count: bool,
}

/// Pairs recorded note-ons and note-offs into notes. See the module docs.
#[derive(Debug, Default)]
pub struct Recorder {
    held: HashMap<(u32, u8), VecDeque<Held>>,
    awaiting: Option<Wait>,
    /// Notes refused by a counting wait, not yet taken ([`Self::take_refused`]).
    refused: usize,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes held now (note-ons without their note-off yet).
    pub fn open(&self) -> usize {
        self.held.values().map(VecDeque::len).sum()
    }

    /// Forgets every held note and drops every event up to and including the mark `id`
    /// (`grooviq_engine::Controller::record_mark` after a new state), counting nothing.
    pub fn reset_until_mark(&mut self, id: u32) {
        self.held.clear();
        self.awaiting = Some(Wait {
            mark: Some(id),
            count: false,
        });
    }

    /// Refuses, and counts, every note from now up to and including the mark `id` (`None`: a
    /// mark not queued yet, see [`Self::await_mark`]): the notes held now and every note-on
    /// that comes before the mark.
    pub fn refuse_until_mark(&mut self, id: Option<u32>) {
        self.refused += self.open();
        self.held.clear();
        self.awaiting = Some(Wait {
            mark: id,
            count: true,
        });
    }

    /// Names the barrier a counting wait whose mark was not queued yet ends at.
    pub fn await_mark(&mut self, id: u32) {
        if let Some(wait) = &mut self.awaiting
            && wait.count
        {
            wait.mark = Some(id);
        }
    }

    /// Whether notes are being refused and counted now.
    pub fn refusing(&self) -> bool {
        self.awaiting.is_some_and(|w| w.count)
    }

    /// The notes refused since the last call.
    pub fn take_refused(&mut self) -> usize {
        std::mem::take(&mut self.refused)
    }

    /// Takes `events` in order and returns the notes they completed, in the order they ended.
    /// A note starts at its note-on's `position`, lasts `off.tick - on.tick` ticks (at least 1)
    /// and has the note-on's velocity. A note-off ends the held note-on of its own press (the
    /// oldest, should one press be held twice); one whose press is not held is ignored.
    pub fn feed(&mut self, events: &[RecordEvent]) -> Vec<NewNote> {
        self.feed_takes(events)
            .into_iter()
            .map(|(_, note)| note)
            .collect()
    }

    /// [`Self::feed`], each note with the take it was played in (its note-on's).
    pub fn feed_takes(&mut self, events: &[RecordEvent]) -> Vec<(u32, NewNote)> {
        let mut notes = Vec::new();
        for event in events {
            if let Some(wait) = self.awaiting {
                match *event {
                    RecordEvent::Mark { id } if Some(id) == wait.mark => self.awaiting = None,
                    RecordEvent::Note { on: true, .. } if wait.count => self.refused += 1,
                    _ => {}
                }
                continue;
            }
            match *event {
                RecordEvent::Note {
                    channel,
                    key,
                    velocity,
                    on: true,
                    tick,
                    position,
                    take,
                    id,
                } => self
                    .held
                    .entry((channel.0, key))
                    .or_default()
                    .push_back(Held {
                        tick,
                        position,
                        velocity,
                        take,
                        id,
                    }),
                RecordEvent::Note {
                    channel,
                    key,
                    on: false,
                    tick,
                    id,
                    ..
                } => {
                    let held = self.held.get_mut(&(channel.0, key));
                    if let Some(on) = held.and_then(|q| {
                        let at = q.iter().position(|h| h.id == id)?;
                        q.remove(at)
                    }) {
                        notes.push((on.take, note(channel.0, key, on, tick)));
                    }
                }
                // Another state's barrier, already passed.
                RecordEvent::Mark { .. } => {}
                RecordEvent::End { tick, .. } => {
                    let mut ended: Vec<((u32, u8), Held)> = self
                        .held
                        .drain()
                        .flat_map(|(k, q)| q.into_iter().map(move |h| (k, h)))
                        .collect();
                    // In the order they started (the map has none).
                    ended.sort_by_key(|((channel, key), h)| (h.tick, *channel, *key));
                    notes.extend(
                        ended
                            .into_iter()
                            .map(|((channel, key), on)| (on.take, note(channel, key, on, tick))),
                    );
                }
            }
        }
        self.held.retain(|_, q| !q.is_empty());
        notes
    }
}

fn note(channel: u32, key: u8, on: Held, off_tick: u64) -> NewNote {
    let length = off_tick
        .saturating_sub(on.tick)
        .clamp(1, u64::from(u32::MAX));
    NewNote {
        channel,
        tick: on.position,
        length: length as u32,
        key,
        velocity: on.velocity,
        slide: false,
    }
}
