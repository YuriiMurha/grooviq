//! The recorder: live note events from the engine's recording tap become notes.

use grooviq_engine::{ChannelId, NoteId, RecordEvent};
use grooviq_project::notes::NewNote;
use grooviq_project::recorder::Recorder;

/// A note-on of the press numbered after its key (one press per key at a time).
fn on(channel: u32, key: u8, velocity: f32, tick: u64, position: u32) -> RecordEvent {
    pressed(u64::from(key), channel, key, velocity, tick, position)
}

/// The note-off of [`on`]'s press.
fn off(channel: u32, key: u8, tick: u64, position: u32) -> RecordEvent {
    released(u64::from(key), channel, key, tick, position)
}

/// The note-on of press `id`, in take 0.
fn pressed(id: u64, channel: u32, key: u8, velocity: f32, tick: u64, position: u32) -> RecordEvent {
    RecordEvent::Note {
        channel: ChannelId(channel),
        key,
        velocity,
        on: true,
        tick,
        position,
        take: 0,
        id: NoteId(id),
    }
}

/// The note-off of press `id`, in take 0.
fn released(id: u64, channel: u32, key: u8, tick: u64, position: u32) -> RecordEvent {
    RecordEvent::Note {
        channel: ChannelId(channel),
        key,
        velocity: 0.0,
        on: false,
        tick,
        position,
        take: 0,
        id: NoteId(id),
    }
}

/// `event` in take `take`.
fn in_take(event: RecordEvent, take: u32) -> RecordEvent {
    match event {
        RecordEvent::Note {
            channel,
            key,
            velocity,
            on,
            tick,
            position,
            id,
            ..
        } => RecordEvent::Note {
            channel,
            key,
            velocity,
            on,
            tick,
            position,
            take,
            id,
        },
        other => other,
    }
}

/// (start, length, velocity) of `notes`, by start.
fn spans(notes: &[NewNote]) -> Vec<(u32, u32, f32)> {
    let mut got: Vec<_> = notes
        .iter()
        .map(|n| (n.tick, n.length, n.velocity))
        .collect();
    got.sort_by_key(|n| n.0);
    got
}

fn note(channel: u32, tick: u32, length: u32, key: u8, velocity: f32) -> NewNote {
    NewNote {
        channel,
        tick,
        length,
        key,
        velocity,
        slide: false,
    }
}

#[test]
fn on_and_off_pair_into_a_note() {
    let mut r = Recorder::new();
    assert!(
        r.feed(&[on(1, 60, 0.7, 4_000, 160)]).is_empty(),
        "still held"
    );
    let notes = r.feed(&[off(1, 60, 4_480, 640)]);
    assert_eq!(notes, vec![note(1, 160, 480, 60, 0.7)]);
    assert_eq!(r.open(), 0);
}

#[test]
fn a_note_held_across_the_loop_end_keeps_its_length() {
    // 16 steps = 3840 ticks: on at 3700, off at 4100 (position 260 in the next loop).
    let mut r = Recorder::new();
    let notes = r.feed(&[on(1, 48, 0.8, 3_700, 3_700), off(1, 48, 4_100, 260)]);
    assert_eq!(notes, vec![note(1, 3_700, 400, 48, 0.8)]);
}

#[test]
fn end_closes_open_notes() {
    let mut r = Recorder::new();
    let notes = r.feed(&[
        on(1, 60, 0.5, 100, 100),
        on(2, 36, 1.0, 200, 200),
        off(1, 60, 300, 300),
        on(1, 62, 0.6, 400, 400),
        RecordEvent::End {
            tick: 1_000,
            position: 1_000,
        },
    ]);
    assert_eq!(
        notes,
        vec![
            note(1, 100, 200, 60, 0.5),
            note(2, 200, 800, 36, 1.0),
            note(1, 400, 600, 62, 0.6),
        ]
    );
    assert_eq!(r.open(), 0);
    // A note-off after the end belongs to no take.
    assert!(r.feed(&[off(1, 62, 1_100, 1_100)]).is_empty());
}

#[test]
fn a_note_lasts_at_least_one_tick() {
    let mut r = Recorder::new();
    let notes = r.feed(&[on(1, 60, 0.5, 100, 100), off(1, 60, 100, 100)]);
    assert_eq!(notes, vec![note(1, 100, 1, 60, 0.5)]);
    let notes = r.feed(&[
        on(1, 61, 0.5, 200, 200),
        RecordEvent::End {
            tick: 200,
            position: 200,
        },
    ]);
    assert_eq!(notes, vec![note(1, 200, 1, 61, 0.5)]);
}

#[test]
fn a_release_ends_its_own_press_inner_released_first() {
    // A held typing C4 (press 1); a short roll-key C4 (press 2) inside it.
    let mut r = Recorder::new();
    let notes = r.feed(&[
        pressed(1, 1, 60, 0.8, 0, 0),
        pressed(2, 1, 60, 0.4, 120, 120),
        released(2, 1, 60, 240, 240),
        released(1, 1, 60, 480, 480),
    ]);
    assert_eq!(spans(&notes), vec![(0, 480, 0.8), (120, 120, 0.4)]);
    assert_eq!(r.open(), 0);
}

#[test]
fn a_release_ends_its_own_press_outer_released_first() {
    let mut r = Recorder::new();
    let notes = r.feed(&[
        pressed(1, 1, 60, 0.8, 0, 0),
        pressed(2, 1, 60, 0.4, 120, 120),
        released(1, 1, 60, 240, 240),
        released(2, 1, 60, 480, 480),
    ]);
    assert_eq!(spans(&notes), vec![(0, 240, 0.8), (120, 360, 0.4)]);
    assert_eq!(r.open(), 0);
}

#[test]
fn a_release_of_a_press_the_take_never_heard_ends_nothing() {
    // Press 1 started before the take; press 2, on the same key, inside it.
    let mut r = Recorder::new();
    let notes = r.feed(&[
        pressed(2, 1, 60, 0.4, 120, 120),
        released(1, 1, 60, 240, 240),
    ]);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(r.open(), 1, "press 2 is still held");
    let notes = r.feed(&[released(2, 1, 60, 480, 480)]);
    assert_eq!(spans(&notes), vec![(120, 360, 0.4)]);
}

#[test]
fn a_release_names_its_channel_and_key_too() {
    let mut r = Recorder::new();
    let notes = r.feed(&[
        pressed(1, 1, 60, 0.3, 0, 0),
        pressed(2, 2, 60, 0.4, 10, 10),
        released(2, 2, 60, 20, 20),
        // Press 1's id on another key or channel ends nothing.
        released(1, 1, 61, 30, 30),
        released(1, 2, 60, 30, 30),
    ]);
    assert_eq!(notes, vec![note(2, 10, 10, 60, 0.4)]);
    assert_eq!(r.open(), 1);
}

#[test]
fn a_release_after_its_take_ended_leaves_the_next_takes_press_alone() {
    let mut r = Recorder::new();
    let notes = r.feed_takes(&[
        in_take(pressed(1, 1, 60, 0.8, 0, 0), 1),
        RecordEvent::End {
            tick: 300,
            position: 300,
        },
        in_take(pressed(2, 1, 60, 0.4, 400, 400), 2),
        // Press 1, closed by the End, is let go in take 2.
        in_take(released(1, 1, 60, 500, 500), 2),
        in_take(released(2, 1, 60, 600, 600), 2),
    ]);
    assert_eq!(
        notes,
        vec![
            (1, note(1, 0, 300, 60, 0.8)),
            (2, note(1, 400, 200, 60, 0.4))
        ]
    );
}

#[test]
fn a_press_refused_before_the_barrier_does_not_end_one_after_it() {
    let mut r = Recorder::new();
    r.refuse_until_mark(Some(5));
    let notes = r.feed(&[
        pressed(1, 1, 60, 0.8, 0, 0),
        RecordEvent::Mark { id: 5 },
        pressed(2, 1, 60, 0.4, 200, 200),
        released(1, 1, 60, 300, 300),
        released(2, 1, 60, 600, 600),
    ]);
    assert_eq!(spans(&notes), vec![(200, 400, 0.4)]);
    assert_eq!(r.take_refused(), 1, "press 1, refused once");
    // The same after a silent reset (a new project's barrier).
    r.reset_until_mark(6);
    let notes = r.feed(&[
        pressed(3, 1, 60, 0.8, 0, 0),
        RecordEvent::Mark { id: 6 },
        pressed(4, 1, 60, 0.4, 200, 200),
        released(3, 1, 60, 300, 300),
        released(4, 1, 60, 600, 600),
    ]);
    assert_eq!(spans(&notes), vec![(200, 400, 0.4)]);
    assert_eq!(r.take_refused(), 0);
}

#[test]
fn one_press_id_held_twice_pairs_oldest_first() {
    // Never sent by the app (ids are not reused), but a repeated id must not lose a note.
    let mut r = Recorder::new();
    let notes = r.feed(&[
        on(1, 60, 0.3, 0, 0),
        on(1, 60, 0.9, 100, 100),
        off(1, 60, 150, 150),
        off(1, 60, 400, 400),
    ]);
    assert_eq!(
        notes,
        vec![note(1, 0, 150, 60, 0.3), note(1, 100, 300, 60, 0.9)]
    );
}

#[test]
fn an_off_without_an_on_is_ignored() {
    let mut r = Recorder::new();
    assert!(r.feed(&[off(1, 60, 100, 100)]).is_empty());
    assert_eq!(r.open(), 0);
}

#[test]
fn events_before_the_awaited_mark_are_dropped() {
    let mut r = Recorder::new();
    r.reset_until_mark(7);
    let notes = r.feed(&[
        on(1, 60, 0.5, 0, 0),
        RecordEvent::Mark { id: 6 },
        off(1, 60, 100, 100),
        on(1, 62, 0.5, 120, 120),
        RecordEvent::Mark { id: 7 },
        off(1, 62, 200, 200),
        on(1, 64, 0.9, 300, 300),
        RecordEvent::Mark { id: 3 },
        off(1, 64, 400, 400),
    ]);
    assert_eq!(
        notes,
        vec![note(1, 300, 100, 64, 0.9)],
        "only the take after mark 7"
    );
    assert_eq!(r.open(), 0);
}
