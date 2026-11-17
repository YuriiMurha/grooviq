//! Undo and redo of note changes: piano-roll edits, step edits and recorded takes. The engine
//! runs offline: its processor is driven by hand where a test needs to see what reached it.
//! Nothing is played.

use grooviq_engine::{AudioProcessor, Command, Controller, EngineConfig, RecordEvent, engine};
use grooviq_project::notes::{NewNote, NotesAck, NotesEdit};
use grooviq_project::session::{Edit, Session, SessionError};
use grooviq_project::undo::MAX_UNDO;
use grooviq_project::{Loaded, Note, Project, compile_pattern, decode_samples};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn rig() -> (Session, Arc<Mutex<Controller>>, AudioProcessor) {
    let (controller, processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let controller = Arc::new(Mutex::new(controller));
    (Session::new(Arc::clone(&controller)), controller, processor)
}

fn render(processor: &mut AudioProcessor, blocks: usize) {
    let mut out = vec![0.0f32; 128];
    for _ in 0..blocks {
        processor.process(&mut out, 2);
    }
}

fn add_synth(session: &mut Session) -> u32 {
    let view = session.add_synth_channel("Init", Instant::now()).unwrap();
    view.channels.last().unwrap().id
}

fn melodic(channel: u32, tick: u32, key: u8) -> NewNote {
    NewNote {
        channel,
        tick,
        length: 480,
        key,
        velocity: 0.8,
        slide: false,
    }
}

fn edit_notes(session: &mut Session, add: Vec<NewNote>, update: Vec<Note>, delete: Vec<u32>) {
    let edit = NotesEdit {
        base_revision: session.view().notes_revision,
        add,
        update,
        delete,
    };
    session.edit_notes(edit, Instant::now()).unwrap();
}

fn add_notes(session: &mut Session, add: Vec<NewNote>) -> Vec<u32> {
    let edit = NotesEdit {
        base_revision: session.view().notes_revision,
        add,
        update: Vec::new(),
        delete: Vec::new(),
    };
    session.edit_notes(edit, Instant::now()).unwrap().added_ids
}

/// The current pattern's notes, by id.
fn notes(session: &Session) -> Vec<Note> {
    let mut notes = session.project().patterns[0].notes.clone();
    notes.sort_by_key(|n| n.id);
    notes
}

fn undo(session: &mut Session) -> NotesAck {
    session
        .undo_notes(Instant::now())
        .unwrap()
        .expect("something to undo")
}

fn redo(session: &mut Session) -> NotesAck {
    session
        .redo_notes(Instant::now())
        .unwrap()
        .expect("something to redo")
}

/// A session with one synth channel holding three notes, saved (clean), with nothing to undo.
fn three_notes() -> (
    Session,
    Arc<Mutex<Controller>>,
    AudioProcessor,
    u32,
    Vec<Note>,
) {
    let (mut session, controller, processor) = rig();
    let ch = add_synth(&mut session);
    add_notes(
        &mut session,
        vec![
            melodic(ch, 0, 60),
            melodic(ch, 480, 62),
            melodic(ch, 960, 64),
        ],
    );
    let project = session.project().clone();
    let loaded = Loaded {
        project,
        samples: HashMap::new(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/three.grooviq"), loaded, decoded)
        .unwrap();
    let view = session.view();
    assert!(!view.can_undo && !view.can_redo && !view.dirty);
    let before = notes(&session);
    (session, controller, processor, ch, before)
}

#[test]
fn undo_restores_deleted_notes_with_their_ids() {
    let (mut session, _, _, _, before) = three_notes();
    let gone = vec![before[0].id, before[2].id];
    edit_notes(&mut session, Vec::new(), Vec::new(), gone);
    assert_eq!(notes(&session).len(), 1);
    assert!(session.view().can_undo);

    let revision = session.view().notes_revision;
    let ack = undo(&mut session);
    assert_eq!(notes(&session), before, "the same notes, with the same ids");
    assert!(ack.reload, "the UI fetches the notes again");
    assert_eq!(ack.revision, revision + 1);
    assert_eq!(ack.view.notes_revision, ack.revision);
    assert!(ack.view.dirty);
    assert!(!ack.view.can_undo && ack.view.can_redo);

    // Redo deletes them again; undo brings back the same ids once more.
    redo(&mut session);
    assert_eq!(notes(&session), vec![before[1]]);
    undo(&mut session);
    assert_eq!(notes(&session), before);
}

#[test]
fn undo_reverts_moves_resizes_and_velocity() {
    let (mut session, _, _, _, before) = three_notes();
    let changed = vec![
        Note {
            tick: 1_440,
            key: 67,
            ..before[0]
        },
        Note {
            length: 1_920,
            ..before[1]
        },
        Note {
            velocity: 0.25,
            slide: true,
            ..before[2]
        },
    ];
    edit_notes(&mut session, Vec::new(), changed.clone(), Vec::new());
    assert_eq!(notes(&session), changed);
    undo(&mut session);
    assert_eq!(notes(&session), before);
    redo(&mut session);
    assert_eq!(notes(&session), changed);
}

#[test]
fn undo_removes_added_notes_and_restores_the_pattern_length() {
    let (mut session, _, _, ch, before) = three_notes();
    assert_eq!(session.view().pattern.steps, 16);
    // A note past the end grows the pattern to the end of its bar (32 steps).
    let added = add_notes(
        &mut session,
        vec![melodic(ch, 4_000, 72), melodic(ch, 0, 48)],
    );
    assert_eq!(session.view().pattern.steps, 32);

    let ack = undo(&mut session);
    assert_eq!(notes(&session), before);
    assert_eq!(ack.view.pattern.steps, 16);

    // Redo puts back the same notes under the same ids, and the length.
    let ack = redo(&mut session);
    let ids: Vec<u32> = notes(&session).iter().map(|n| n.id).collect();
    assert!(added.iter().all(|id| ids.contains(id)), "{ids:?}");
    assert_eq!(notes(&session).len(), 5);
    assert_eq!(ack.view.pattern.steps, 32);

    // Ids are never reused: a note added after an undone add gets a fresh id.
    undo(&mut session);
    let fresh = add_notes(&mut session, vec![melodic(ch, 0, 50)]);
    assert!(
        fresh[0] > *added.iter().max().unwrap(),
        "{fresh:?} {added:?}"
    );
}

#[test]
fn a_length_set_by_hand_is_kept_by_undo() {
    let (mut session, _, _, ch, _) = three_notes();
    add_notes(&mut session, vec![melodic(ch, 4_000, 72)]);
    assert_eq!(session.view().pattern.steps, 32);
    session
        .apply(Edit::SetPatternLength { steps: 64 }, Instant::now())
        .unwrap();
    // The length is not a note change: undo takes the note away and leaves the length alone.
    undo(&mut session);
    assert_eq!(session.view().pattern.steps, 64);
}

/// Starts the transport for a take; the session reads the engine's barrier from it.
fn start_take(
    session: &mut Session,
    controller: &Mutex<Controller>,
    processor: &mut AudioProcessor,
) {
    let mut events: Vec<RecordEvent> = Vec::new();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(processor, 2);
    controller.lock().unwrap().take_recorded(&mut events);
    assert_eq!(
        session.collect_recorded(&events, Instant::now()).revision,
        None
    );
}

/// Plays `keys` together for a few blocks and collects what was recorded.
fn record_chord(
    session: &mut Session,
    controller: &Mutex<Controller>,
    processor: &mut AudioProcessor,
    channel: u32,
    keys: &[u8],
) -> Option<u32> {
    for &key in keys {
        session.live_note(channel, key, 0.8, true).unwrap();
    }
    render(processor, 6);
    for &key in keys {
        session.live_note(channel, key, 0.0, false).unwrap();
    }
    render(processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    session.collect_recorded(&events, Instant::now()).revision
}

#[test]
fn a_recorded_take_undoes_in_one_step() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[60, 64, 67]).unwrap();
    assert_eq!(notes(&session).len(), 6);

    undo(&mut session);
    assert_eq!(notes(&session), before, "the whole take goes in one step");
    // Undo leaves the recorder alone: recording is still on and the next take lands.
    assert!(session.view().recording);
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();
    let after = notes(&session);
    assert_eq!(after.len(), 4, "{after:?}");
    assert!(after.iter().any(|n| n.key == 72));
    // The new take cleared redo.
    assert!(!session.view().can_redo);
    assert_eq!(session.redo_notes(Instant::now()).unwrap(), None);
}

#[test]
fn step_edits_undo_one_by_one() {
    let (mut session, _, _) = rig();
    let ch = add_synth(&mut session);
    let now = Instant::now();
    let cells = |s: &Session| s.view().pattern.cells[&ch].clone();
    let empty = cells(&session);
    session
        .apply(
            Edit::ToggleStep {
                channel: ch,
                step: 0,
            },
            now,
        )
        .unwrap();
    session
        .apply(
            Edit::ToggleStep {
                channel: ch,
                step: 4,
            },
            now,
        )
        .unwrap();
    session
        .apply(
            Edit::SetStepVelocity {
                channel: ch,
                step: 4,
                velocity: 0.3,
            },
            now,
        )
        .unwrap();
    session
        .apply(
            Edit::ClearStep {
                channel: ch,
                step: 0,
            },
            now,
        )
        .unwrap();
    let last = cells(&session);
    assert_eq!(last[4], Some(0.3));
    assert_eq!(last[0], None);

    undo(&mut session);
    assert_eq!(cells(&session)[0], Some(0.8), "the cleared step is back");
    assert_eq!(cells(&session)[4], Some(0.3));
    undo(&mut session);
    assert_eq!(
        cells(&session)[4],
        Some(0.8),
        "the velocity before the change"
    );
    undo(&mut session);
    assert_eq!(cells(&session)[4], None);
    assert_eq!(cells(&session)[0], Some(0.8));
    undo(&mut session);
    assert_eq!(cells(&session), empty);
    assert_eq!(session.undo_notes(Instant::now()).unwrap(), None);
    // Mix, tempo, swing and length are not note changes: they leave nothing to undo.
    session.apply(Edit::SetSwing { swing: 0.5 }, now).unwrap();
    session.apply(Edit::SetTempo { bpm: 99.0 }, now).unwrap();
    assert!(!session.view().can_undo);
    // Redo walks forward through all four.
    for _ in 0..4 {
        redo(&mut session);
    }
    assert_eq!(cells(&session), last);
    assert_eq!(session.redo_notes(Instant::now()).unwrap(), None);
}

#[test]
fn a_new_edit_clears_redo() {
    let (mut session, _, _, ch, _) = three_notes();
    add_notes(&mut session, vec![melodic(ch, 0, 70)]);
    undo(&mut session);
    assert!(session.view().can_redo);
    add_notes(&mut session, vec![melodic(ch, 0, 71)]);
    let view = session.view();
    assert!(view.can_undo && !view.can_redo);
    assert_eq!(session.redo_notes(Instant::now()).unwrap(), None);
    // An edit that changes nothing (an unknown id) is no note change: redo stays.
    undo(&mut session);
    edit_notes(&mut session, Vec::new(), Vec::new(), vec![9_999]);
    assert!(session.view().can_redo);
}

#[test]
fn history_is_capped_at_200() {
    let (mut session, _, _) = rig();
    let ch = add_synth(&mut session);
    for i in 0..(MAX_UNDO + 5) {
        add_notes(&mut session, vec![melodic(ch, 0, (i % 128) as u8)]);
    }
    let mut undone = 0;
    while session.undo_notes(Instant::now()).unwrap().is_some() {
        undone += 1;
    }
    assert_eq!(undone, MAX_UNDO);
    // The oldest five stay: they dropped out of the history.
    assert_eq!(notes(&session).len(), 5);
    let mut redone = 0;
    while session.redo_notes(Instant::now()).unwrap().is_some() {
        redone += 1;
    }
    assert_eq!(redone, MAX_UNDO);
    assert_eq!(notes(&session).len(), MAX_UNDO + 5);
}

#[test]
fn open_new_and_channel_removal_clear_the_history() {
    // Open.
    let (mut session, _, _, ch, _) = three_notes();
    add_notes(&mut session, vec![melodic(ch, 0, 70)]);
    let loaded = Loaded {
        project: Project::new(),
        samples: HashMap::new(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/other.grooviq"), loaded, decoded)
        .unwrap();
    assert!(!session.view().can_undo);
    assert_eq!(session.undo_notes(Instant::now()).unwrap(), None);

    // New, with something to redo too.
    let (mut session, _, _, ch, _) = three_notes();
    add_notes(&mut session, vec![melodic(ch, 0, 70)]);
    add_notes(&mut session, vec![melodic(ch, 0, 71)]);
    undo(&mut session);
    session.new_project().unwrap();
    let view = session.view();
    assert!(!view.can_undo && !view.can_redo);

    // A recovered autosave.
    let (mut session, _, _, ch, _) = three_notes();
    add_notes(&mut session, vec![melodic(ch, 0, 70)]);
    let loaded = Loaded {
        project: session.project().clone(),
        samples: HashMap::new(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    session
        .open_recovered(None, loaded, decoded, Instant::now())
        .unwrap();
    assert!(!session.view().can_undo);

    // Removing a channel (even another one).
    let (mut session, _, _, ch, _) = three_notes();
    let other = add_synth(&mut session);
    add_notes(&mut session, vec![melodic(ch, 0, 70)]);
    session
        .apply(Edit::RemoveChannel { channel: other }, Instant::now())
        .unwrap();
    assert!(!session.view().can_undo);
    assert_eq!(session.undo_notes(Instant::now()).unwrap(), None);
}

#[test]
fn undo_while_playing_updates_the_engine_pattern() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 4);
    assert!(controller.lock().unwrap().live().playing);
    add_notes(&mut session, vec![melodic(ch, 240, 75)]);
    render(&mut processor, 2);

    undo(&mut session);
    let project = session.project().clone();
    assert_eq!(notes(&session), before);
    assert_eq!(
        controller.lock().unwrap().state().pattern,
        compile_pattern(&project, &project.patterns[0]),
        "the engine holds the undone pattern"
    );
    render(&mut processor, 4);
    assert!(
        controller.lock().unwrap().live().playing,
        "the transport keeps playing"
    );

    redo(&mut session);
    let project = session.project().clone();
    assert_eq!(
        controller.lock().unwrap().state().pattern,
        compile_pattern(&project, &project.patterns[0])
    );
    render(&mut processor, 4);
    assert!(controller.lock().unwrap().live().playing);
}

#[test]
fn a_refused_undo_keeps_its_place() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    add_notes(&mut session, vec![melodic(ch, 0, 70)]);
    let after = notes(&session);
    // The engine's command queue is full: the undo is refused and changes nothing.
    while controller
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let revision = session.view().notes_revision;
    assert!(matches!(
        session.undo_notes(Instant::now()),
        Err(SessionError::Engine(_))
    ));
    assert_eq!(notes(&session), after);
    assert_eq!(session.view().notes_revision, revision);
    assert!(session.view().can_undo && !session.view().can_redo);
    // Once the queue drains, the same undo goes through.
    render(&mut processor, 4);
    undo(&mut session);
    assert_eq!(notes(&session), before);
}

#[test]
fn undo_and_redo_never_pass_the_note_cap_or_reuse_ids() {
    // Undoing restores a state the pattern held before, and redo one it held after: the count
    // is the one it had then, and every id was issued before. A replay checks both anyway.
    let (mut session, _, _, ch, before) = three_notes();
    let ids = add_notes(&mut session, vec![melodic(ch, 0, 70), melodic(ch, 0, 71)]);
    edit_notes(
        &mut session,
        Vec::new(),
        Vec::new(),
        vec![before[1].id, ids[0]],
    );
    undo(&mut session);
    undo(&mut session);
    redo(&mut session);
    redo(&mut session);
    let now: Vec<u32> = notes(&session).iter().map(|n| n.id).collect();
    assert_eq!(now, vec![before[0].id, before[2].id, ids[1]]);
    let next = session.project().patterns[0].next_note_id;
    assert!(now.iter().all(|&id| id < next), "{now:?} next {next}");
    let fresh = add_notes(&mut session, vec![melodic(ch, 0, 72)]);
    assert!(!now.contains(&fresh[0]));
}

#[test]
fn a_recorded_note_of_a_channel_is_kept_by_another_channels_undo() {
    // Two takes on two channels are two entries; undoing the newer keeps the older.
    let (mut session, controller, mut processor, ch, _) = three_notes();
    let other = add_synth(&mut session);
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[50]).unwrap();
    record_chord(&mut session, &controller, &mut processor, other, &[51]).unwrap();
    undo(&mut session);
    let keys: Vec<(u32, u8)> = notes(&session).iter().map(|n| (n.channel, n.key)).collect();
    assert!(keys.contains(&(ch, 50)), "{keys:?}");
    assert!(!keys.iter().any(|&(c, _)| c == other), "{keys:?}");
}
