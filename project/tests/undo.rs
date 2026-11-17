//! Undo and redo of note changes: piano-roll edits, step edits and recorded takes. The engine
//! runs offline: its processor is driven by hand where a test needs to see what reached it.
//! Nothing is played.

use grooviq_engine::{AudioProcessor, Command, Controller, EngineConfig, RecordEvent, engine};
use grooviq_project::notes::{NewNote, NotesAck, NotesEdit};
use grooviq_project::session::{Edit, Session, SessionError};
use grooviq_project::undo::MAX_UNDO;
use grooviq_project::{
    Loaded, MAX_NOTES_PER_PATTERN, Note, Project, compile_pattern, decode_samples,
};
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

/// The keys of the current pattern's notes, sorted (the three-note fixture holds 60, 62, 64).
fn keys(session: &Session) -> Vec<u8> {
    let mut keys: Vec<u8> = notes(session).iter().map(|n| n.key).collect();
    keys.sort_unstable();
    keys
}

/// Undoes until nothing is left, counting the steps.
fn undo_all(session: &mut Session) -> usize {
    let mut steps = 0;
    while session.undo_notes(Instant::now()).unwrap().is_some() {
        steps += 1;
    }
    steps
}

#[test]
fn a_take_over_several_polls_undoes_in_one_step() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    // Three polls of the recorder, each adding notes: one take.
    record_chord(&mut session, &controller, &mut processor, ch, &[70, 74]).unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[71]).unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[72, 76, 79]).unwrap();
    let recorded = notes(&session);
    assert_eq!(recorded.len(), 9);

    let ack = undo(&mut session);
    assert_eq!(notes(&session), before, "the whole take goes in one step");
    assert_eq!(ack.view.pattern.steps, 16);
    assert!(!session.view().can_undo);
    redo(&mut session);
    assert_eq!(notes(&session), recorded, "and comes back with its ids");
    assert!(session.view().recording, "recording is left alone");
}

#[test]
fn recording_off_ends_the_take() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    // A key still held when Record goes off ends there, in the take it started in.
    session.live_note(ch, 71, 0.8, true).unwrap();
    render(&mut processor, 4);
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    session.live_note(ch, 71, 0.0, false).unwrap();
    render(&mut processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    session
        .collect_recorded(&events, Instant::now())
        .revision
        .unwrap();
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 71]);
    // The next take is a step of its own.
    session.set_recording(true).unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 71]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn notes_after_the_end_in_the_same_poll_start_the_next_take() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    // Off and on again, and a note played, all before the recorder's next poll: the poll
    // holds the first take's end, then the second take's note.
    session.live_note(ch, 71, 0.8, true).unwrap();
    render(&mut processor, 2);
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    session.live_note(ch, 71, 0.0, false).unwrap();
    session.set_recording(true).unwrap();
    session.live_note(ch, 72, 0.8, true).unwrap();
    render(&mut processor, 2);
    session.live_note(ch, 72, 0.0, false).unwrap();
    render(&mut processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(
        events.iter().any(|e| matches!(e, RecordEvent::End { .. })),
        "{events:?}"
    );
    session
        .collect_recorded(&events, Instant::now())
        .revision
        .unwrap();
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 71, 72]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 71]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn stopping_the_transport_ends_the_take() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    controller.lock().unwrap().send(Command::Stop).unwrap();
    render(&mut processor, 2);
    // Recording stays on; the next Play starts the next take.
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();
    assert!(session.view().recording);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn an_edit_ends_the_take() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    // A piano-roll edit while the take runs.
    add_notes(&mut session, vec![melodic(ch, 0, 90)]);
    record_chord(&mut session, &controller, &mut processor, ch, &[71]).unwrap();
    // A step edit on another channel while the take runs.
    let drums = add_synth(&mut session);
    session
        .apply(
            Edit::ToggleStep {
                channel: drums,
                step: 3,
            },
            Instant::now(),
        )
        .unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();

    undo(&mut session);
    assert!(!keys(&session).contains(&72));
    assert!(keys(&session).contains(&71));
    undo(&mut session);
    assert_eq!(
        session.view().pattern.cells[&drums][3],
        None,
        "the step edit"
    );
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 90]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70], "the roll edit");
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn undo_during_a_take_undoes_what_was_recorded_and_closes_it() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    // An older step, so a take that stayed open could only fold into it.
    add_notes(&mut session, vec![melodic(ch, 0, 90)]);
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[71]).unwrap();
    undo(&mut session);
    assert_eq!(
        keys(&session),
        vec![60, 62, 64, 90],
        "what was recorded so far"
    );
    // The take records on: its next notes are a new step, and redo is gone.
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();
    assert!(!session.view().can_redo);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 90]);
    undo(&mut session);
    assert_eq!(
        keys(&session),
        vec![60, 62, 64],
        "the older step is its own"
    );
    assert!(!session.view().can_undo);
}

#[test]
fn opening_a_project_mid_take_starts_afresh() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    // The same project again, as a new one: the history is gone, and so is the take.
    let loaded = Loaded {
        project: session.project().clone(),
        samples: HashMap::new(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/again.grooviq"), loaded, decoded)
        .unwrap();
    assert!(!session.view().can_undo);
    let opened = keys(&session);
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();
    assert_eq!(undo_all(&mut session), 1);
    assert_eq!(keys(&session), opened);
}

#[test]
fn removing_a_channel_mid_take_starts_afresh() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    let other = add_synth(&mut session);
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    session
        .apply(Edit::RemoveChannel { channel: other }, Instant::now())
        .unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();
    assert_eq!(undo_all(&mut session), 1);
    assert_eq!(keys(&session), vec![60, 62, 64, 70]);
}

#[test]
fn recording_switching_itself_off_ends_the_take() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    // One note short of the cap.
    let fill: Vec<NewNote> = (0..(MAX_NOTES_PER_PATTERN - 4) as u32)
        .map(|i| NewNote {
            channel: ch,
            tick: (i % 3_840) + 10,
            length: 1,
            key: 20,
            velocity: 0.5,
            slide: false,
        })
        .collect();
    add_notes(&mut session, fill);
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    // Two more: one fits, recording switches itself off.
    for key in [71, 72] {
        session.live_note(ch, key, 0.8, true).unwrap();
    }
    render(&mut processor, 4);
    for key in [71, 72] {
        session.live_note(ch, key, 0.0, false).unwrap();
    }
    render(&mut processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    let got = session.collect_recorded(&events, Instant::now());
    assert_eq!(got.stopped.map(|s| s.reason), Some("noteCap"));
    assert!(!session.view().recording);
    // The take, with the note that fit, is one step; the fill another.
    undo(&mut session);
    assert_eq!(notes(&session).len(), MAX_NOTES_PER_PATTERN - 1);
    assert!(!keys(&session).iter().any(|&k| k >= 70));
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn a_take_that_grew_the_pattern_undoes_to_its_starting_length() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    assert_eq!(session.view().pattern.steps, 16);
    // 16 steps at 140 BPM last about 1286 blocks: a note held across the loop's end runs past
    // it and grows the pattern to the next bar, in a later poll of the same take.
    render(&mut processor, 1_240);
    session.live_note(ch, 71, 0.8, true).unwrap();
    render(&mut processor, 120);
    session.live_note(ch, 71, 0.0, false).unwrap();
    render(&mut processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    session
        .collect_recorded(&events, Instant::now())
        .revision
        .unwrap();
    assert_eq!(session.view().pattern.steps, 32);
    // And one more poll after the growth.
    record_chord(&mut session, &controller, &mut processor, ch, &[72]).unwrap();

    let ack = undo(&mut session);
    assert_eq!(notes(&session), before);
    assert_eq!(
        ack.view.pattern.steps, 16,
        "the length the take started from"
    );
    let ack = redo(&mut session);
    assert_eq!(ack.view.pattern.steps, 32);
}
