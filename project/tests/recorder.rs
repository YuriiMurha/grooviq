//! The recorder: live note events from the engine's recording tap become notes.

use grooviq_engine::{ChannelId, RecordEvent};
use grooviq_project::notes::NewNote;
use grooviq_project::recorder::Recorder;

fn on(channel: u32, key: u8, velocity: f32, tick: u64, position: u32) -> RecordEvent {
    RecordEvent::Note {
        channel: ChannelId(channel),
        key,
        velocity,
        on: true,
        tick,
        position,
        take: 0,
    }
}

fn off(channel: u32, key: u8, tick: u64, position: u32) -> RecordEvent {
    RecordEvent::Note {
        channel: ChannelId(channel),
        key,
        velocity: 0.0,
        on: false,
        tick,
        position,
        take: 0,
    }
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
fn overlapping_same_key_notes_pair_in_order() {
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
    // The same key on another channel is another note.
    let notes = r.feed(&[
        on(1, 60, 0.3, 0, 0),
        on(2, 60, 0.4, 10, 10),
        off(2, 60, 20, 20),
    ]);
    assert_eq!(notes, vec![note(2, 10, 10, 60, 0.4)]);
    assert_eq!(r.open(), 1);
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
