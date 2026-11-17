//! Notes as the UI gets them (the binary wire format) and note edits by id.

use grooviq_engine::{Controller, EngineConfig, engine};
use grooviq_project::notes::{NewNote, NotesEdit, encode_notes};
use grooviq_project::session::{Edit, Session, SessionError};
use grooviq_project::{Note, Pattern};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn rig() -> (Session, Arc<Mutex<Controller>>) {
    let (controller, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let controller = Arc::new(Mutex::new(controller));
    (Session::new(Arc::clone(&controller)), controller)
}

/// A session with one synth channel; returns it and the channel's id.
fn with_synth() -> (Session, Arc<Mutex<Controller>>, u32) {
    let (mut session, controller) = rig();
    let view = session.add_synth_channel("Init", Instant::now()).unwrap();
    let id = view.channels[0].id;
    (session, controller, id)
}

fn new_note(channel: u32, tick: u32, key: u8) -> NewNote {
    NewNote {
        channel,
        tick,
        length: 240,
        key,
        velocity: 0.8,
        slide: false,
    }
}

fn edit(session: &Session) -> NotesEdit {
    NotesEdit {
        base_revision: session.view().notes_revision,
        add: Vec::new(),
        update: Vec::new(),
        delete: Vec::new(),
    }
}

fn notes(session: &Session) -> Vec<Note> {
    let mut notes = session.project().patterns[0].notes.clone();
    notes.sort_by_key(|n| n.id);
    notes
}

#[test]
fn the_encoder_matches_the_wire_fixture() {
    let fixture =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/notes-v1.bin"))
            .unwrap();
    let mut pattern = Pattern::new(0, "P");
    pattern.revision = 5;
    // Stored out of order: the encoder sorts by (tick, channel, id).
    pattern.notes = vec![
        Note {
            id: 7,
            channel: 4,
            tick: 3600,
            length: 120,
            key: 36,
            velocity: 0.25,
            slide: false,
        },
        Note {
            id: 2,
            channel: 1,
            tick: 480,
            length: 960,
            key: 63,
            velocity: 1.0,
            slide: true,
        },
        Note {
            id: 1,
            channel: 1,
            tick: 0,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
        },
    ];
    assert_eq!(encode_notes(&pattern), fixture);
}

#[test]
fn the_session_sends_its_notes_with_their_revision() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 480, 64), new_note(ch, 0, 62)];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    let bytes = session.notes();
    assert_eq!(&bytes[0..4], b"GQN1");
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        ack.revision
    );
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
    assert_eq!(bytes.len(), 16 + 2 * 24);
    // Sorted by tick: the note at 0 (added second) comes first.
    assert_eq!(
        u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
        ack.added_ids[1]
    );
}

#[test]
fn adds_get_fresh_ids_in_order() {
    let (mut session, _, ch) = with_synth();
    let before = session.view().notes_revision;
    let mut e = edit(&session);
    e.add = vec![
        new_note(ch, 0, 60),
        new_note(ch, 240, 62),
        new_note(ch, 480, 64),
    ];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert_eq!(ack.added_ids.len(), 3);
    assert!(ack.added_ids.windows(2).all(|w| w[0] < w[1]), "{ack:?}");
    assert!(ack.ignored_ids.is_empty());
    assert!(!ack.reload);
    assert_eq!(ack.revision, before + 1);
    assert_eq!(ack.view.notes_revision, ack.revision);
    assert!(ack.view.dirty);
    let keys: Vec<(u32, u8)> = notes(&session).iter().map(|n| (n.id, n.key)).collect();
    assert_eq!(
        keys,
        vec![
            (ack.added_ids[0], 60),
            (ack.added_ids[1], 62),
            (ack.added_ids[2], 64)
        ]
    );

    // A later add never reuses an id, even one deleted.
    let mut e = edit(&session);
    e.delete = vec![ack.added_ids[2]];
    e.add = vec![new_note(ch, 720, 65)];
    let next = session.edit_notes(e, Instant::now()).unwrap();
    assert!(next.added_ids[0] > ack.added_ids[2]);
}

#[test]
fn updates_and_deletes_go_by_id() {
    let (mut session, controller, ch) = with_synth();
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 60), new_note(ch, 240, 62)];
    let ids = session.edit_notes(e, Instant::now()).unwrap().added_ids;

    let mut e = edit(&session);
    e.delete = vec![ids[0]];
    e.update = vec![Note {
        id: ids[1],
        channel: ch,
        tick: 960,
        length: 480,
        key: 67,
        velocity: 0.5,
        slide: true,
    }];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert!(ack.ignored_ids.is_empty() && !ack.reload, "{ack:?}");
    assert_eq!(
        notes(&session),
        vec![Note {
            id: ids[1],
            channel: ch,
            tick: 960,
            length: 480,
            key: 67,
            velocity: 0.5,
            slide: true,
        }]
    );
    // The engine plays the edited notes.
    let state = controller.lock().unwrap().state().clone();
    assert_eq!(state.pattern.notes.len(), 1);
    assert_eq!(
        (state.pattern.notes[0].tick, state.pattern.notes[0].key),
        (960, 67)
    );
    assert!(state.pattern.notes[0].slide);
}

#[test]
fn edits_by_unknown_ids_are_ignored_and_reported() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 60)];
    let id = session.edit_notes(e, Instant::now()).unwrap().added_ids[0];
    let revision = session.view().notes_revision;

    let mut e = edit(&session);
    e.delete = vec![999];
    e.update = vec![Note {
        id: 998,
        channel: ch,
        tick: 0,
        length: 240,
        key: 61,
        velocity: 1.0,
        slide: false,
    }];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert_eq!(ack.ignored_ids, vec![999, 998]);
    assert!(ack.added_ids.is_empty());
    assert_eq!(ack.revision, revision, "nothing changed");
    assert_eq!(notes(&session).len(), 1);
    assert_eq!(notes(&session)[0].id, id);

    // A delete then an update of the same id: the update finds nothing.
    let mut e = edit(&session);
    e.delete = vec![id];
    e.update = vec![Note {
        id,
        channel: ch,
        tick: 0,
        length: 240,
        key: 61,
        velocity: 1.0,
        slide: false,
    }];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert_eq!(ack.ignored_ids, vec![id]);
    assert!(notes(&session).is_empty());
}

#[test]
fn an_edit_naming_an_unknown_channel_changes_nothing() {
    let (mut session, _, ch) = with_synth();
    let revision = session.view().notes_revision;
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 60), new_note(ch + 50, 240, 60)];
    let err = session.edit_notes(e, Instant::now()).unwrap_err();
    assert!(matches!(err, SessionError::UnknownChannel(_)), "{err}");
    assert!(notes(&session).is_empty());
    assert_eq!(session.view().notes_revision, revision);
}

#[test]
fn a_stale_base_revision_asks_for_a_reload() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.base_revision = e.base_revision.wrapping_sub(1);
    e.add = vec![new_note(ch, 0, 60)];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert!(ack.reload);
    assert_eq!(ack.added_ids.len(), 1, "the edit still applies");

    let mut e = edit(&session);
    e.add = vec![new_note(ch, 240, 60)];
    assert!(!session.edit_notes(e, Instant::now()).unwrap().reload);
}

#[test]
fn clamped_values_ask_for_a_reload() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = vec![NewNote {
        channel: ch,
        tick: 0,
        length: 0,
        key: 200,
        velocity: 3.0,
        slide: false,
    }];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert!(ack.reload);
    let n = notes(&session)[0];
    assert_eq!((n.length, n.key, n.velocity), (1, 127, 1.0));

    let mut e = edit(&session);
    e.update = vec![Note {
        velocity: -1.0,
        ..n
    }];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert!(ack.reload);
    assert_eq!(notes(&session)[0].velocity, 0.0);
}

#[test]
fn notes_past_the_end_grow_the_pattern_to_the_bar() {
    let (mut session, _, ch) = with_synth();
    assert_eq!(session.view().pattern.steps, 16);
    // 4/4: a bar is 3840 ticks (16 steps). A note ending one tick into the second bar.
    let mut e = edit(&session);
    e.add = vec![NewNote {
        length: 3841 - 3600,
        ..new_note(ch, 3600, 60)
    }];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert_eq!(ack.view.pattern.steps, 32);

    // Ending at tick 15 361 (one tick into bar 5) grows to 5 bars.
    let mut e = edit(&session);
    e.add = vec![NewNote {
        length: 15_361 - 15_000,
        ..new_note(ch, 15_000, 60)
    }];
    assert_eq!(
        session
            .edit_notes(e, Instant::now())
            .unwrap()
            .view
            .pattern
            .steps,
        80
    );

    // Never shrinks.
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 60)];
    assert_eq!(
        session
            .edit_notes(e, Instant::now())
            .unwrap()
            .view
            .pattern
            .steps,
        80
    );

    // Never past 256 steps.
    let mut e = edit(&session);
    e.add = vec![NewNote {
        length: 10_000,
        ..new_note(ch, 60_000, 60)
    }];
    assert_eq!(
        session
            .edit_notes(e, Instant::now())
            .unwrap()
            .view
            .pattern
            .steps,
        256
    );

    // In 3/4 a bar is 2880 ticks (12 steps).
    let (mut session, _, ch) = with_synth();
    session
        .apply(Edit::SetTimeSignature { beats: 3, unit: 4 }, Instant::now())
        .unwrap();
    let mut e = edit(&session);
    e.add = vec![NewNote {
        length: 400,
        ..new_note(ch, 3700, 60)
    }];
    assert_eq!(
        session
            .edit_notes(e, Instant::now())
            .unwrap()
            .view
            .pattern
            .steps,
        24
    );
}

#[test]
fn notes_left_past_a_shortened_end_do_not_grow_it_back() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 3600, 60)];
    session.edit_notes(e, Instant::now()).unwrap();
    session
        .apply(Edit::SetPatternLength { steps: 8 }, Instant::now())
        .unwrap();
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 60)];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    assert_eq!(ack.view.pattern.steps, 8);
}

#[test]
fn additions_stop_at_the_note_cap() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = (0..100_000u32)
        .map(|i| new_note(ch, (i % 64) * 240, (i % 128) as u8))
        .collect();
    session.edit_notes(e, Instant::now()).unwrap();
    let revision = session.view().notes_revision;
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 1)];
    let err = session.edit_notes(e, Instant::now()).unwrap_err();
    assert!(matches!(err, SessionError::TooManyNotes), "{err}");
    assert_eq!(session.view().notes_revision, revision);
    // A delete in the same edit makes room.
    let mut e = edit(&session);
    e.delete = vec![1];
    e.add = vec![new_note(ch, 0, 1)];
    assert_eq!(
        session
            .edit_notes(e, Instant::now())
            .unwrap()
            .added_ids
            .len(),
        1
    );
}

#[test]
fn edits_have_the_json_shapes_the_ui_sends_and_reads() {
    let e: NotesEdit = serde_json::from_str(
        r#"{"baseRevision":3,
            "add":[{"channel":1,"tick":0,"length":240,"key":60,"velocity":0.8,"slide":false}],
            "update":[{"id":4,"channel":1,"tick":480,"length":960,"key":63,"velocity":1,"slide":true}],
            "delete":[7]}"#,
    )
    .unwrap();
    assert_eq!(e.base_revision, 3);
    assert_eq!(e.add, vec![new_note(1, 0, 60)]);
    assert_eq!(e.update[0].id, 4);
    assert!(e.update[0].slide);
    assert_eq!(e.delete, vec![7]);

    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = vec![new_note(ch, 0, 60)];
    let ack = session.edit_notes(e, Instant::now()).unwrap();
    let json = serde_json::to_value(&ack).unwrap();
    for key in ["revision", "addedIds", "ignoredIds", "reload", "view"] {
        assert!(json.get(key).is_some(), "{key} in {json}");
    }
    for key in ["key", "notesRevision", "recording"] {
        assert!(json["view"].get(key).is_some(), "{key} in view");
    }
    let channel = &json["view"]["channels"][0];
    assert_eq!(channel["kind"], "synth");
    assert_eq!(channel["roll"], false);
    assert_eq!(channel["choke"], 0);
    assert_eq!(channel["preset"], "Init");
    assert_eq!(channel["sampler"], serde_json::json!({}));
    assert_eq!(channel["synth"].as_object().unwrap().len(), 40);
}

/// Median of `runs` timings of `f`.
fn median(runs: usize, mut f: impl FnMut() -> Duration) -> Duration {
    let mut times: Vec<Duration> = (0..runs).map(|_| f()).collect();
    times.sort();
    times[runs / 2]
}

#[test]
fn ten_thousand_notes_edit_in_under_50_ms() {
    let (mut session, _, ch) = with_synth();
    let mut e = edit(&session);
    e.add = (0..10_000u32)
        .map(|i| new_note(ch, (i % 64) * 240, (i % 128) as u8))
        .collect();
    session.edit_notes(e, Instant::now()).unwrap();
    let mut shift = 0;
    let took = median(5, || {
        // Move every note one tick later: one edit, as a drag of a select-all commits.
        shift += 1;
        let mut e = edit(&session);
        e.update = notes(&session)
            .into_iter()
            .map(|n| Note {
                tick: n.tick + 1,
                ..n
            })
            .collect();
        let start = Instant::now();
        let ack = session.edit_notes(e, Instant::now()).unwrap();
        let took = start.elapsed();
        assert!(ack.ignored_ids.is_empty());
        took
    });
    assert_eq!(notes(&session)[0].tick, shift);
    assert!(took < Duration::from_millis(50), "{took:?}");
}

/// Notes that start together go out by channel, then by id: the fixture's ticks all differ,
/// so it cannot check either tie on its own.
#[test]
fn simultaneous_notes_go_out_by_channel_then_id() {
    let mut pattern = Pattern::new(0, "ties");
    for (id, channel) in [(7, 4), (2, 1), (1, 1)] {
        pattern.notes.push(Note {
            id,
            channel,
            tick: 0,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
        });
    }
    let bytes = encode_notes(&pattern);
    let ids: Vec<u32> = bytes[16..]
        .as_chunks::<24>()
        .0
        .iter()
        .map(|n| u32::from_le_bytes([n[0], n[1], n[2], n[3]]))
        .collect();
    assert_eq!(ids, vec![1, 2, 7]);
}
