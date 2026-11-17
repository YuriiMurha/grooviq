//! Turns the engine's recorded live events into notes.
//!
//! The engine's recording tap ([`RecordEvent`]) gives each live note-on and note-off with the
//! tick played since Play (`tick`, for lengths) and the loop position heard then (`position`,
//! where the note goes). A [`Recorder`] pairs them per (channel, key), oldest note-on first,
//! and closes every note still held at a take's `End`.

use crate::notes::NewNote;
use grooviq_engine::RecordEvent;
use std::collections::{HashMap, VecDeque};

/// A note-on still waiting for its note-off.
#[derive(Clone, Copy, Debug)]
struct Held {
    tick: u64,
    position: u32,
    velocity: f32,
}

/// Pairs recorded note-ons and note-offs into notes. See the module docs.
#[derive(Debug, Default)]
pub struct Recorder {
    held: HashMap<(u32, u8), VecDeque<Held>>,
    /// Events of a take that belongs to a replaced project are dropped up to its `End`.
    skip_to_end: bool,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes held now (note-ons without their note-off yet).
    pub fn open(&self) -> usize {
        self.held.values().map(VecDeque::len).sum()
    }

    /// Forgets every held note; with `skip_to_end`, also drops every event up to and including
    /// the next `End` (the rest of a take that is no longer wanted).
    pub fn reset(&mut self, skip_to_end: bool) {
        self.held.clear();
        self.skip_to_end = skip_to_end;
    }

    /// Takes `events` in order and returns the notes they completed, in the order they ended.
    /// A note starts at its note-on's `position`, lasts `off.tick - on.tick` ticks (at least 1)
    /// and has the note-on's velocity. A note-off without a note-on is ignored.
    pub fn feed(&mut self, events: &[RecordEvent]) -> Vec<NewNote> {
        let mut notes = Vec::new();
        for event in events {
            if self.skip_to_end {
                self.skip_to_end = !matches!(event, RecordEvent::End { .. });
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
                } => self
                    .held
                    .entry((channel.0, key))
                    .or_default()
                    .push_back(Held {
                        tick,
                        position,
                        velocity,
                    }),
                RecordEvent::Note {
                    channel,
                    key,
                    on: false,
                    tick,
                    ..
                } => {
                    let held = self.held.get_mut(&(channel.0, key));
                    if let Some(on) = held.and_then(VecDeque::pop_front) {
                        notes.push(note(channel.0, key, on, tick));
                    }
                }
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
                            .map(|((channel, key), on)| note(channel, key, on, tick)),
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
