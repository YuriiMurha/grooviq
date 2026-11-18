//! Undo and redo of note changes: piano-roll edits, step edits and recorded takes. The engine
//! runs offline: its processor is driven by hand where a test needs to see what reached it.
//! Nothing is played.

use grooviq_engine::{AudioProcessor, Command, Controller, EngineConfig, RecordEvent, engine};
use grooviq_project::notes::{NewNote, NotesAck, NotesEdit};
use grooviq_project::session::{Edit, Recorded, Session, SessionError};
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
        pattern_id: 0,
        generation: session.generation(),
        base_revision: session.view().notes_revision,
        add,
        update,
        delete,
    };
    session.edit_notes(edit, Instant::now()).unwrap();
}

fn add_notes(session: &mut Session, add: Vec<NewNote>) -> Vec<u32> {
    let edit = NotesEdit {
        pattern_id: 0,
        generation: session.generation(),
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
    let presses: Vec<_> = keys
        .iter()
        .map(|&key| session.live_note_on(channel, key, 0.8).unwrap())
        .collect();
    render(processor, 6);
    for (&key, &press) in keys.iter().zip(&presses) {
        session.live_note_off(channel, key, press).unwrap();
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
    // The budget users are promised, written out: a test derived from the constant follows it.
    for i in 0..205 {
        add_notes(&mut session, vec![melodic(ch, 0, (i % 128) as u8)]);
    }
    let mut undone = 0;
    while session.undo_notes(Instant::now()).unwrap().is_some() {
        undone += 1;
    }
    assert_eq!(undone, 200);
    // The oldest five stay: they dropped out of the history.
    assert_eq!(notes(&session).len(), 5);
    let mut redone = 0;
    while session.redo_notes(Instant::now()).unwrap().is_some() {
        redone += 1;
    }
    assert_eq!(redone, 200);
    assert_eq!(notes(&session).len(), 205);
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
    let press2 = session.live_note_on(ch, 71, 0.8).unwrap();
    render(&mut processor, 4);
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    session.live_note_off(ch, 71, press2).unwrap();
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
    let press3 = session.live_note_on(ch, 71, 0.8).unwrap();
    render(&mut processor, 2);
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    session.live_note_off(ch, 71, press3).unwrap();
    session.set_recording(true).unwrap();
    let press4 = session.live_note_on(ch, 72, 0.8).unwrap();
    render(&mut processor, 2);
    session.live_note_off(ch, 72, press4).unwrap();
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
    assert!(matches!(
        session.settle_recording(),
        Err(SessionError::RecordingSettling)
    ));
    render(&mut processor, 4);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    session.collect_recorded(&events, Instant::now());
    session.settle_recording().unwrap();
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
    session.set_recording(true).unwrap();
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
    let presses = [71, 72].map(|key| session.live_note_on(ch, key, 0.8).unwrap());
    render(&mut processor, 4);
    for (key, press) in [71, 72].into_iter().zip(presses) {
        session.live_note_off(ch, key, press).unwrap();
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
    let press6 = session.live_note_on(ch, 71, 0.8).unwrap();
    render(&mut processor, 120);
    session.live_note_off(ch, 71, press6).unwrap();
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

/// Plays `key` for a few blocks without collecting.
fn play_key(session: &Session, processor: &mut AudioProcessor, channel: u32, key: u8) {
    let press7 = session.live_note_on(channel, key, 0.8).unwrap();
    render(processor, 2);
    session.live_note_off(channel, key, press7).unwrap();
    render(processor, 2);
}

/// Collects everything the engine recorded so far, in one poll.
fn collect(session: &mut Session, controller: &Mutex<Controller>) -> Recorded {
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    session.collect_recorded(&events, Instant::now())
}

#[test]
fn two_ended_takes_in_one_poll_are_two_steps() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    // Two whole takes, and the start of a third, before the recorder polls once.
    play_key(&session, &mut processor, ch, 70);
    session.set_recording(false).unwrap();
    session.set_recording(true).unwrap();
    play_key(&session, &mut processor, ch, 71);
    session.set_recording(false).unwrap();
    session.set_recording(true).unwrap();
    play_key(&session, &mut processor, ch, 72);
    render(&mut processor, 2);
    collect(&mut session, &controller).revision.unwrap();
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 71, 72]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70, 71]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64, 70]);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn a_take_whose_end_was_dropped_is_still_its_own_step() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    // Fill the engine's recording queue without a poll, so the take's End finds it full.
    let mut played = 0;
    while controller.lock().unwrap().record_dropped() == 0 {
        play_key(&session, &mut processor, ch, 30 + (played % 20) as u8);
        played += 1;
        assert!(played < 2_000, "the recording queue never filled");
    }
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(
        !events.iter().any(|e| matches!(e, RecordEvent::End { .. })),
        "the End was dropped"
    );
    // The next take, collected in the same poll as the first.
    session.set_recording(true).unwrap();
    play_key(&session, &mut processor, ch, 90);
    render(&mut processor, 2);
    controller.lock().unwrap().take_recorded(&mut events);
    session
        .collect_recorded(&events, Instant::now())
        .revision
        .unwrap();
    assert!(keys(&session).contains(&90));
    undo(&mut session);
    assert!(!keys(&session).contains(&90));
    assert!(keys(&session).len() > 3, "the first take stays");
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn a_take_cut_by_the_note_cap_under_a_busy_queue_is_one_step() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    // Two notes short of the cap.
    let fill: Vec<NewNote> = (0..(MAX_NOTES_PER_PATTERN - 5) as u32)
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
    let before = notes(&session).len();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    // B fits, C does not; the engine's queue is full when they are collected.
    play_key(&session, &mut processor, ch, 71);
    play_key(&session, &mut processor, ch, 72);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    while controller
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let got = session.collect_recorded(&events, Instant::now());
    assert!(
        got.stopped.is_none(),
        "unpublished capacity overflow waits too"
    );
    assert_eq!(notes(&session).len(), before + 1, "B waits for the engine");
    // The queue drains; B goes in, with A.
    render(&mut processor, 4);
    let admitted = collect(&mut session, &controller);
    admitted.revision.unwrap();
    assert_eq!(admitted.stopped.map(|s| s.reason), Some("noteCap"));
    assert_eq!(notes(&session).len(), MAX_NOTES_PER_PATTERN);
    undo(&mut session);
    assert_eq!(notes(&session).len(), before, "A and B are one take");
}

#[test]
fn a_length_set_by_hand_mid_take_keeps_the_take_whole() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    session
        .apply(Edit::SetPatternLength { steps: 64 }, Instant::now())
        .unwrap();
    record_chord(&mut session, &controller, &mut processor, ch, &[71]).unwrap();
    let recorded = notes(&session);

    let ack = undo(&mut session);
    assert_eq!(notes(&session), before, "the whole take");
    assert_eq!(ack.view.pattern.steps, 64, "the length set by hand stays");
    let ack = redo(&mut session);
    assert_eq!(notes(&session), recorded);
    assert_eq!(ack.view.pattern.steps, 64);
}

/// Fills the engine's recording queue (without a poll) with one-tick notes of key 20 on
/// `channel`, which never grow the pattern, until it drops an event.
fn fill_the_recording_queue(
    session: &Session,
    controller: &Mutex<Controller>,
    processor: &mut AudioProcessor,
    channel: u32,
) {
    let mut sent = 0;
    while controller.lock().unwrap().record_dropped() == 0 {
        for _ in 0..50 {
            let press8 = session.live_note_on(channel, 20, 0.5).unwrap();
            session.live_note_off(channel, 20, press8).unwrap();
        }
        render(processor, 1);
        sent += 1;
        assert!(sent < 100, "the recording queue never filled");
    }
}

/// Holds key 71 from early in take A and explicitly omits A's End to model a legacy/lost
/// event stream (the engine now retains End under pressure), then records take B (key 72, held for `b_blocks`) and collects it before 71 lets go.
/// Returns the session after 71's late completion is collected.
fn late_completion(b_blocks: usize) -> (Session, Arc<Mutex<Controller>>, AudioProcessor, u32) {
    late_completion_after(b_blocks, |_| {})
}

/// [`late_completion`], with `between` run on the session after take B, before 71 lets go.
fn late_completion_after(
    b_blocks: usize,
    between: impl FnOnce(&mut Session),
) -> (Session, Arc<Mutex<Controller>>, AudioProcessor, u32) {
    // 16 steps at 140 BPM last about 1286 blocks: 71, held from early in A to after B, runs
    // past the loop's end and grows the pattern when it completes.
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    let press9 = session.live_note_on(ch, 71, 0.8).unwrap();
    render(&mut processor, 1_300);
    fill_the_recording_queue(&session, &controller, &mut processor, ch);
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(
        !events.iter().any(|e| matches!(e, RecordEvent::End { .. })),
        "A's End is still waiting behind the full queue"
    );
    session.collect_recorded(&events, Instant::now());
    // Take B, collected while 71 (of take A) is still down.
    session.set_recording(true).unwrap();
    let press10 = session.live_note_on(ch, 72, 0.8).unwrap();
    render(&mut processor, b_blocks);
    session.live_note_off(ch, 72, press10).unwrap();
    render(&mut processor, 2);
    // Preserve this fixture's missing-End premise explicitly; it tests Session's legacy
    // late-take bookkeeping, independently of the engine's reliable boundary delivery.
    let mut later = Vec::new();
    controller.lock().unwrap().take_recorded(&mut later);
    assert_eq!(
        later
            .iter()
            .filter(|e| matches!(e, RecordEvent::End { .. }))
            .count(),
        1
    );
    later.retain(|e| !matches!(e, RecordEvent::End { .. }));
    session
        .collect_recorded(&later, Instant::now())
        .revision
        .unwrap();
    assert!(keys(&session).contains(&72) && !keys(&session).contains(&71));
    let b_steps = session.view().pattern.steps;
    assert_eq!(
        b_steps,
        if b_blocks > 1_000 { 32 } else { 16 },
        "after take B"
    );
    between(&mut session);
    // 71 lets go: it completes now, in take A.
    session.live_note_off(ch, 71, press9).unwrap();
    render(&mut processor, 2);
    collect(&mut session, &controller).revision.unwrap();
    assert!(keys(&session).contains(&71));
    (session, controller, processor, ch)
}

#[test]
fn a_late_note_joins_its_own_take_below_a_newer_one() {
    let (mut session, _, _, _) = late_completion(4);
    // 71 was held so long that it grew the pattern, under take B's step.
    let grown = session.view().pattern.steps;
    assert!(grown > 16, "{grown}");
    undo(&mut session);
    assert!(!keys(&session).contains(&72), "take B goes first");
    assert!(keys(&session).contains(&71) && keys(&session).contains(&70));
    assert_eq!(session.view().pattern.steps, grown);
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64], "then all of take A");
    assert_eq!(session.view().pattern.steps, 16);
    assert!(!session.view().can_undo);
    redo(&mut session);
    redo(&mut session);
    assert!(keys(&session).contains(&71) && keys(&session).contains(&72));
    assert_eq!(session.view().pattern.steps, grown);
}

#[test]
fn a_late_growth_on_top_of_a_newer_takes_growth_undoes_to_the_start() {
    // Take B's note is held across the loop's end: B grows the pattern first, then A's late
    // note grows it further.
    let (mut session, _, _, _) = late_completion(1_400);
    let grown = session.view().pattern.steps;
    assert!(grown > 32, "{grown}");
    undo(&mut session);
    assert!(!keys(&session).contains(&72));
    assert_eq!(
        session.view().pattern.steps,
        grown,
        "71 still needs the length"
    );
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
    assert_eq!(session.view().pattern.steps, 16, "the length before take A");
    redo(&mut session);
    redo(&mut session);
    assert_eq!(session.view().pattern.steps, grown);
}

#[test]
fn a_late_note_whose_take_was_undone_is_a_new_step() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    let press11 = session.live_note_on(ch, 71, 0.8).unwrap();
    render(&mut processor, 2);
    undo(&mut session);
    assert!(session.view().can_redo);
    session.live_note_off(ch, 71, press11).unwrap();
    render(&mut processor, 2);
    collect(&mut session, &controller).revision.unwrap();
    assert!(!session.view().can_redo, "a note change clears redo");
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
    assert!(!session.view().can_undo);
}

#[test]
fn a_rearm_right_after_the_self_switch_off_starts_a_new_take() {
    let (mut session, controller, mut processor, ch, _) = three_notes();
    let fill: Vec<NewNote> = (0..(MAX_NOTES_PER_PATTERN - 5) as u32)
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
    // A, then B (fits) and C (does not) with the engine's queue full: neither is lost yet.
    record_chord(&mut session, &controller, &mut processor, ch, &[70]).unwrap();
    play_key(&session, &mut processor, ch, 71);
    play_key(&session, &mut processor, ch, 72);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    while controller
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let got = session.collect_recorded(&events, Instant::now());
    assert!(got.stopped.is_none());
    // Admission after the queue drains commits B and stops at the cap. Rearm before its
    // record-off reaches the callback, making room without undoing the admitted old take.
    render(&mut processor, 4);
    let admitted = collect(&mut session, &controller);
    assert!(admitted.revision.is_some());
    assert_eq!(admitted.stopped.map(|s| s.reason), Some("noteCap"));
    let filler = notes(&session).iter().find(|n| n.key == 20).unwrap().id;
    edit_notes(&mut session, vec![], vec![], vec![filler]);
    session.set_recording(true).unwrap();
    render(&mut processor, 2);
    assert_eq!(collect(&mut session, &controller).revision, None);
    assert!(keys(&session).contains(&71), "B went in");
    record_chord(&mut session, &controller, &mut processor, ch, &[73]).unwrap();
    undo(&mut session);
    assert!(!keys(&session).contains(&73));
    assert!(keys(&session).contains(&71), "B is a step of its own");
}

#[test]
fn an_undo_closes_an_older_take_to_its_late_notes() {
    // Take B is undone while 71 of take A is still down: undo ends every take, so 71 is a step
    // of its own (it clears redo), and take A's step keeps 70.
    let (mut session, _, _, _) = late_completion_after(4, |s| {
        undo(s);
        assert!(!keys(s).contains(&72));
    });
    assert!(!session.view().can_redo);
    undo(&mut session);
    assert!(!keys(&session).contains(&71));
    assert!(keys(&session).contains(&70), "take A's step is still there");
    undo(&mut session);
    assert_eq!(keys(&session), vec![60, 62, 64]);
}

#[test]
fn a_redo_closes_an_older_take_to_its_late_notes() {
    // Take B undone and redone while 71 of take A is still down: 71 is a step of its own.
    let (mut session, _, _, _) = late_completion_after(4, |s| {
        undo(s);
        redo(s);
    });
    undo(&mut session);
    assert!(!keys(&session).contains(&71));
    assert!(keys(&session).contains(&72) && keys(&session).contains(&70));
}

/// A whole note of `take` (key `key`, `steps` steps long from tick 0), as the engine stamps it.
fn inject(session: &mut Session, ch: u32, take: u32, key: u8, steps: u32) {
    let note = |on: bool, tick: u64| RecordEvent::Note {
        channel: grooviq_engine::ChannelId(ch),
        key,
        velocity: 0.8,
        on,
        tick,
        position: 0,
        take,
        id: grooviq_engine::NoteId(u64::from(key)),
    };
    let events = [note(true, 0), note(false, u64::from(steps * 240))];
    session
        .collect_recorded(&events, Instant::now())
        .revision
        .unwrap();
}

/// The pattern as undo and redo must restore it: its notes, length and id counter.
fn state(session: &Session) -> (Vec<Note>, u32, u32) {
    let p = &session.project().patterns[0];
    (p.notes.clone(), p.steps, p.next_note_id)
}

#[test]
fn repeated_late_growth_under_newer_takes_undoes_to_the_start() {
    // The review's stamped case: A grows to 32, B to 48, A again to 64, C to 80, A to 96.
    let (mut s, c, mut p, ch, before) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    inject(&mut s, ch, 101, 70, 32);
    inject(&mut s, ch, 102, 71, 48);
    inject(&mut s, ch, 101, 72, 64);
    inject(&mut s, ch, 103, 73, 80);
    inject(&mut s, ch, 101, 74, 96);
    let last = state(&s);
    assert_eq!(s.view().pattern.steps, 96);
    let mut lengths = Vec::new();
    for _ in 0..3 {
        undo(&mut s);
        lengths.push(s.view().pattern.steps);
    }
    assert_eq!(notes(&s), before);
    assert_eq!(
        lengths,
        vec![96, 96, 16],
        "A's late notes need 96 until A goes"
    );
    for _ in 0..3 {
        redo(&mut s);
    }
    assert_eq!(state(&s), last);
}

#[test]
fn three_takes_round_trip_byte_for_byte() {
    let (mut s, c, mut p, ch, _) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    inject(&mut s, ch, 101, 70, 1);
    inject(&mut s, ch, 102, 71, 32);
    inject(&mut s, ch, 103, 72, 48);
    inject(&mut s, ch, 101, 73, 64);
    let mut states = vec![state(&s)];
    for _ in 0..3 {
        undo(&mut s);
        states.push(state(&s));
    }
    assert_eq!(s.view().pattern.steps, 16);
    for i in (0..3).rev() {
        redo(&mut s);
        assert_eq!(state(&s), states[i], "redo to state {i}");
    }
}

#[test]
fn a_length_set_by_hand_between_takes_stays_through_undo_and_redo() {
    let (mut s, c, mut p, ch, before) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    inject(&mut s, ch, 101, 70, 32);
    inject(&mut s, ch, 102, 71, 48);
    s.apply(Edit::SetPatternLength { steps: 64 }, Instant::now())
        .unwrap();
    inject(&mut s, ch, 103, 72, 80);
    inject(&mut s, ch, 101, 73, 96);
    let mut states = vec![state(&s)];
    for _ in 0..3 {
        undo(&mut s);
        states.push(state(&s));
    }
    assert_eq!(notes(&s), before);
    assert_eq!(s.view().pattern.steps, 64, "the length set by hand");
    for i in (0..3).rev() {
        redo(&mut s);
        assert_eq!(state(&s), states[i], "redo to state {i}");
    }
}

#[test]
fn a_shorter_length_set_by_hand_after_a_take_stays() {
    let (mut s, controller, mut processor, ch, before) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &controller, &mut processor);
    render(&mut processor, 1_240);
    let press = s.live_note_on(ch, 80, 0.8).unwrap();
    render(&mut processor, 120);
    s.live_note_off(ch, 80, press).unwrap();
    render(&mut processor, 2);
    collect(&mut s, &controller).revision.unwrap();
    let after = notes(&s);
    s.apply(Edit::SetPatternLength { steps: 1 }, Instant::now())
        .unwrap();
    undo(&mut s);
    assert_eq!(notes(&s), before);
    assert_eq!(s.view().pattern.steps, 1);
    redo(&mut s);
    assert_eq!(notes(&s), after);
    assert_eq!(s.view().pattern.steps, 1);
    // Notes past the end stay stored and unplayed.
    assert!(
        compile_pattern(s.project(), &s.project().patterns[0])
            .notes
            .iter()
            .all(|n| n.tick < 240)
    );
}

#[test]
fn an_edit_round_trips_exactly() {
    let (mut s, _, _, ch, before) = three_notes();
    let mut n = before[1];
    n.tick = 5_000;
    n.length = 719;
    n.key = 77;
    n.slide = true;
    n.velocity = f32::from_bits(0x3f00_0001);
    edit_notes(
        &mut s,
        vec![melodic(ch, 6_000, 80)],
        vec![n],
        vec![before[0].id],
    );
    let after = state(&s);
    undo(&mut s);
    assert_eq!(notes(&s), before);
    assert_eq!(s.view().pattern.steps, 16);
    redo(&mut s);
    assert_eq!(state(&s), after);
}

#[test]
fn repeated_late_growth_on_the_real_engine_undoes_to_the_start() {
    // The review's engine case: take A's notes complete across B's and C's growth.
    let (mut s, c, mut p, ch, before) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    let [p70, p71, p74] = [70, 71, 74].map(|key| s.live_note_on(ch, key, 0.8).unwrap());
    render(&mut p, 1_300);
    s.live_note_off(ch, 70, p70).unwrap();
    render(&mut p, 2);
    collect(&mut s, &c).revision.unwrap();
    assert_eq!(s.view().pattern.steps, 32);
    s.set_recording(true).unwrap();
    let p72 = s.live_note_on(ch, 72, 0.8).unwrap();
    render(&mut p, 2_600);
    s.live_note_off(ch, 72, p72).unwrap();
    render(&mut p, 2);
    collect(&mut s, &c).revision.unwrap();
    s.live_note_off(ch, 71, p71).unwrap();
    render(&mut p, 2);
    collect(&mut s, &c).revision.unwrap();
    s.set_recording(true).unwrap();
    let p73 = s.live_note_on(ch, 73, 0.8).unwrap();
    render(&mut p, 2_600);
    s.live_note_off(ch, 73, p73).unwrap();
    render(&mut p, 2);
    collect(&mut s, &c).revision.unwrap();
    render(&mut p, 1_300);
    s.live_note_off(ch, 74, p74).unwrap();
    render(&mut p, 2);
    collect(&mut s, &c).revision.unwrap();
    let last = state(&s);
    let mut lengths = vec![last.1];
    for _ in 0..3 {
        undo(&mut s);
        lengths.push(s.view().pattern.steps);
    }
    assert_eq!(notes(&s), before);
    assert_eq!(lengths[3], 16, "all takes undone; lengths {lengths:?}");
    for _ in 0..3 {
        redo(&mut s);
    }
    assert_eq!(state(&s), last);
}

/// One block, then the control thread's upkeep (the app runs it every 250 ms): the patterns
/// the engine replaced are freed, so a long run never fills its queues.
fn tick(controller: &Mutex<Controller>, processor: &mut AudioProcessor) {
    render(processor, 1);
    controller.lock().unwrap().check_health();
}

/// A small seeded generator (xorshift64*), so the sequence test is repeatable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A seeded mix of takes, late notes, roll edits, lengths set by hand, undo and redo. After
/// every step: undoing everything leaves the first notes and the length from before the
/// history (or the last one set by hand), and redoing everything is the pattern byte for
/// byte; undo then redo of the newest step is the pattern byte for byte.
#[test]
fn a_generated_sequence_keeps_the_undo_invariants() {
    for seed in [1u64, 7, 42, 1_234, 99_991] {
        let (mut s, c, mut p, ch, before) = three_notes();
        s.set_recording(true).unwrap();
        start_take(&mut s, &c, &mut p);
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut take = 1u32;
        let mut now = 0u64;
        let mut held: Vec<(u32, u8, u64)> = Vec::new();
        let mut set_by_hand: Option<u32> = None;
        for op in 0..160 {
            now += 1 + rng.below(500);
            let event =
                |key: u8, on: bool, tick: u64, take: u32, position: u32| RecordEvent::Note {
                    channel: grooviq_engine::ChannelId(ch),
                    key,
                    velocity: 0.8,
                    on,
                    tick,
                    position,
                    take,
                    // One press per key at a time.
                    id: grooviq_engine::NoteId(u64::from(key)),
                };
            match rng.below(10) {
                // A key goes down in the running take.
                0..=2 if held.len() < 8 => {
                    let key = 30 + rng.below(60) as u8;
                    if !held.iter().any(|h| h.1 == key) {
                        let at = rng.below(3_840) as u32;
                        held.push((take, key, now));
                        let e = [event(key, true, now, take, at)];
                        s.collect_recorded(&e, Instant::now());
                    }
                }
                // A held key lets go: maybe long after, maybe in an older take (a late note).
                0..=4 if !held.is_empty() => {
                    let (t, key, on) = held.remove(rng.below(held.len() as u64) as usize);
                    let off = on + 1 + rng.below(12_000);
                    now = now.max(off);
                    s.collect_recorded(&[event(key, false, off, t, 0)], Instant::now());
                }
                // The take ends; the next one starts.
                5 => take += 1,
                6 => {
                    let steps = 1 + rng.below(64) as u32;
                    s.apply(Edit::SetPatternLength { steps }, Instant::now())
                        .unwrap();
                    set_by_hand = Some(steps);
                }
                7 => {
                    let at = rng.below(12_000) as u32;
                    add_notes(&mut s, vec![melodic(ch, at, 100 + rng.below(20) as u8)]);
                }
                8 => {
                    let _ = s.undo_notes(Instant::now()).unwrap();
                }
                _ => {
                    let _ = s.redo_notes(Instant::now()).unwrap();
                }
            }
            tick(&c, &mut p);
            // Most steps check from the side (the order of takes stays as it was); every
            // fifth undoes and redoes, which closes the open takes.
            if op % 5 != 4 {
                continue;
            }
            let here = state(&s);
            if s.view().can_undo {
                undo(&mut s);
                tick(&c, &mut p);
                redo(&mut s);
                tick(&c, &mut p);
                assert_eq!(state(&s), here, "seed {seed} op {op}: undo then redo");
            }
            let redos = s.view().can_redo;
            let mut undone = 0;
            while s.undo_notes(Instant::now()).unwrap().is_some() {
                tick(&c, &mut p);
                undone += 1;
            }
            assert_eq!(notes(&s), before, "seed {seed} op {op}: all undone");
            assert_eq!(
                s.view().pattern.steps,
                set_by_hand.unwrap_or(16),
                "seed {seed} op {op}: the length from before the history"
            );
            for _ in 0..undone {
                redo(&mut s);
                tick(&c, &mut p);
            }
            if !redos {
                assert_eq!(state(&s), here, "seed {seed} op {op}: all redone");
            }
        }
    }
}

#[test]
fn the_live_fixtures_length_case_matches() {
    // app/ui/e2e/m2-undo.e2e.ts `follows_the_shells_length_log` runs the same steps on the mock.
    let (mut s, _, _, ch, _) = three_notes();
    let note = |tick| NewNote {
        length: 240,
        ..melodic(ch, tick, 60)
    };
    let mut lengths = Vec::new();
    add_notes(&mut s, vec![note(5_000)]);
    lengths.push(s.view().pattern.steps);
    add_notes(&mut s, vec![note(10_000)]);
    lengths.push(s.view().pattern.steps);
    s.apply(Edit::SetPatternLength { steps: 40 }, Instant::now())
        .unwrap();
    add_notes(&mut s, vec![note(12_000)]);
    lengths.push(s.view().pattern.steps);
    for _ in 0..3 {
        lengths.push(undo(&mut s).view.pattern.steps);
    }
    for _ in 0..3 {
        lengths.push(redo(&mut s).view.pattern.steps);
    }
    assert_eq!(lengths, vec![32, 48, 64, 40, 40, 40, 40, 40, 64]);
}

#[test]
fn a_note_that_did_not_grow_the_pattern_keeps_it_long_once_the_growth_goes() {
    // Take B opens, take A grows 16 -> 32, then B's late note ends at 20 steps: inside 32, so
    // it grows nothing then. Undoing A must leave the 32 steps B's note needs.
    let (mut s, c, mut p, ch, before) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    inject(&mut s, ch, 201, 70, 1);
    inject(&mut s, ch, 202, 71, 32);
    assert_eq!(s.view().pattern.steps, 32);
    inject(&mut s, ch, 201, 72, 20);
    assert_eq!(s.view().pattern.steps, 32);
    undo(&mut s);
    assert!(!keys(&s).contains(&71));
    assert_eq!(s.view().pattern.steps, 32, "B's late note still needs it");
    undo(&mut s);
    assert_eq!(notes(&s), before);
    assert_eq!(s.view().pattern.steps, 16);
}

#[test]
fn an_identical_update_that_grows_the_pattern_round_trips() {
    // The review's R8 case: the note is unchanged, but resubmitting it grows the pattern
    // after the length was shortened by hand. That growth is a step of its own.
    let (mut s, _, _, ch, _) = three_notes();
    add_notes(&mut s, vec![melodic(ch, 5_000, 75)]);
    s.apply(Edit::SetPatternLength { steps: 16 }, Instant::now())
        .unwrap();
    let n = *notes(&s).iter().find(|n| n.key == 75).unwrap();
    edit_notes(&mut s, vec![], vec![n], vec![]);
    assert_eq!(s.view().pattern.steps, 32);
    let grown = state(&s);
    undo(&mut s);
    assert_eq!(s.view().pattern.steps, 16, "the growth alone is undone");
    assert!(notes(&s).iter().any(|n| n.key == 75));
    redo(&mut s);
    assert_eq!(state(&s), grown);
    // Undoing everything: the length set by hand.
    undo(&mut s);
    undo(&mut s);
    assert_eq!(s.view().pattern.steps, 16);
    assert!(!s.view().can_undo);
}

#[test]
fn growth_does_not_round_a_note_already_inside_a_length_set_by_hand() {
    let (mut s, _, _, ch, _) = three_notes();
    s.apply(Edit::SetPatternLength { steps: 25 }, Instant::now())
        .unwrap();
    add_notes(
        &mut s,
        vec![NewNote {
            tick: 0,
            length: 20 * 240,
            ..melodic(ch, 0, 75)
        }],
    );
    assert_eq!(s.view().pattern.steps, 25);
    undo(&mut s);
    redo(&mut s);
    assert_eq!(s.view().pattern.steps, 25);
}

#[test]
fn growth_keeps_the_bar_of_its_time_through_signature_changes() {
    let (mut s, c, mut p, ch, _) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    inject(&mut s, ch, 201, 70, 1);
    s.apply(Edit::SetTimeSignature { beats: 3, unit: 4 }, Instant::now())
        .unwrap();
    s.apply(Edit::SetTempo { bpm: 77.0 }, Instant::now())
        .unwrap();
    inject(&mut s, ch, 202, 71, 25); // rounds to 36 steps in 3/4
    assert_eq!(s.view().pattern.steps, 36);
    s.apply(Edit::SetTimeSignature { beats: 5, unit: 8 }, Instant::now())
        .unwrap();
    inject(&mut s, ch, 201, 72, 25); // grows nothing now; 30 steps in 5/8
    undo(&mut s);
    assert_eq!(s.view().pattern.steps, 30);
    redo(&mut s);
    assert_eq!(s.view().pattern.steps, 36);
    undo(&mut s);
    undo(&mut s);
    assert_eq!(s.view().pattern.steps, 16);
}

#[test]
fn a_length_set_by_hand_to_the_length_it_has_does_not_dirty() {
    let (mut s, _, _, ch, _) = three_notes();
    add_notes(&mut s, vec![melodic(ch, 4_000, 75)]);
    let loaded = Loaded {
        project: s.project().clone(),
        samples: HashMap::new(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    s.open(Path::new("/p/x.grooviq"), loaded, decoded).unwrap();
    s.apply(Edit::SetPatternLength { steps: 32 }, Instant::now())
        .unwrap();
    assert!(!s.view().dirty);
}

#[test]
fn a_hundred_thousand_lengths_set_by_hand_keep_the_log_small() {
    let (mut s, c, mut p, ch, _) = three_notes();
    add_notes(&mut s, vec![melodic(ch, 5_000, 75)]);
    for i in 0..100_000u32 {
        s.apply(
            Edit::SetPatternLength { steps: 1 + i % 200 },
            Instant::now(),
        )
        .unwrap();
        if i % 50 == 0 {
            tick(&c, &mut p);
        }
    }
    assert!(
        s.history_log_len() <= 2,
        "the log holds {} entries for one step",
        s.history_log_len()
    );
    // The last length set by hand is what undo leaves.
    let last = s.view().pattern.steps;
    undo(&mut s);
    assert_eq!(s.view().pattern.steps, last);
}

#[test]
fn undo_and_new_edit_cycles_keep_the_log_small() {
    // Each new edit forgets the undone one (redo is cleared): its growth leaves the log, and
    // the lengths set by hand before it fold into the start.
    let (mut s, c, mut p, ch, before) = three_notes();
    for i in 0..1_000u32 {
        add_notes(&mut s, vec![melodic(ch, 5_000 + i % 100, 75)]);
        s.apply(Edit::SetPatternLength { steps: 16 }, Instant::now())
            .unwrap();
        undo(&mut s);
        tick(&c, &mut p);
    }
    assert!(s.history_log_len() <= 2, "{}", s.history_log_len());
    assert_eq!(notes(&s), before);
    assert_eq!(s.view().pattern.steps, 16);
}

#[test]
fn a_takes_growths_with_different_bars_stay_apart() {
    // The live fixture's `keeps_each_growths_bar_within_one_take`: one take grows to 32 in
    // 4/4; its next note, after a switch to 3/4, ends inside 32 and grows nothing (in 3/4 it
    // would round to 36). Redo must give 32 again.
    let (mut s, c, mut p, ch, _) = three_notes();
    s.set_recording(true).unwrap();
    start_take(&mut s, &c, &mut p);
    inject(&mut s, ch, 301, 70, 25);
    s.apply(Edit::SetTimeSignature { beats: 3, unit: 4 }, Instant::now())
        .unwrap();
    inject(&mut s, ch, 301, 71, 26);
    let mut lengths = vec![s.view().pattern.steps];
    lengths.push(undo(&mut s).view.pattern.steps);
    lengths.push(redo(&mut s).view.pattern.steps);
    assert_eq!(lengths, vec![32, 16, 32]);
}

/// One take of two presses of key 60 on `ch`: the rack's preview (velocity 0.8) for `outer`
/// blocks, and a typed press (velocity 0.4) that starts `before` blocks in and is let go after
/// `inner` blocks. Both releases come before recording is switched off.
fn nested_take(
    session: &mut Session,
    processor: &mut AudioProcessor,
    ch: u32,
    (before, inner, outer): (usize, usize, usize),
) {
    session.set_recording(true).unwrap();
    render(processor, 2);
    let preview = session.preview_on(ch).unwrap();
    render(processor, before);
    let typed = session.live_note_on(ch, 60, 0.4).unwrap();
    // Each release when its own press is due, whichever comes first.
    let mut releases = [(before + inner, Some(typed)), (outer, None)];
    releases.sort_by_key(|r| r.0);
    let mut at = before;
    for (due, press) in releases {
        render(processor, due - at);
        at = due;
        match press {
            Some(press) => session.live_note_off(ch, 60, press).unwrap(),
            None => session.preview_off(ch, preview).unwrap(),
        }
    }
    render(processor, 2);
    session.set_recording(false).unwrap();
    render(processor, 2);
}

#[test]
fn nested_presses_of_one_key_record_their_own_lengths_and_each_take_undoes_in_one_step() {
    let (mut session, controller, mut processor) = rig();
    let ch = add_synth(&mut session);
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 2);
    // Take 1: the typed press is let go first. Take 2: the preview is.
    nested_take(&mut session, &mut processor, ch, (40, 20, 120));
    nested_take(&mut session, &mut processor, ch, (40, 80, 60));
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(
        session
            .collect_recorded(&events, Instant::now())
            .revision
            .is_some()
    );
    // 64 frames a block at 48 kHz, 960 ticks a beat: the ticks `blocks` last.
    let bpm = f64::from(session.project().bpm);
    let ticks = |blocks: usize| (blocks as f64 * 64.0 * bpm * 960.0 / (48_000.0 * 60.0)) as i64;
    let mut got: Vec<(f32, u32, u32)> = notes(&session)
        .iter()
        .map(|n| (n.velocity, n.tick, n.length))
        .collect();
    got.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.total_cmp(&b.0)));
    assert_eq!(got.len(), 4, "{got:?}");
    let want = [
        (0.8, ticks(120)),
        (0.4, ticks(20)),
        (0.8, ticks(60)),
        (0.4, ticks(80)),
    ];
    let starts = |i: usize| got[i].1;
    assert!(starts(0) < starts(1) && starts(1) < starts(2) && starts(2) < starts(3));
    for (i, (velocity, length)) in want.into_iter().enumerate() {
        assert_eq!(got[i].0, velocity, "{got:?}");
        assert!(
            (i64::from(got[i].2) - length).abs() <= 3,
            "note {i}: length {} for {length}: {got:?}",
            got[i].2
        );
    }
    // Each take is one step.
    assert_eq!(undo_all(&mut session), 2);
    assert!(notes(&session).is_empty());
}

#[test]
fn recovery_exhaustion_finishes_the_actual_take_and_preserves_undo() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    record_chord(&mut session, &controller, &mut processor, ch, &[70, 74]);
    let _held = session.live_note_on(ch, 76, 0.8).unwrap();
    render(&mut processor, 6);
    for delay in [0, 250, 500, 1000] {
        controller
            .lock()
            .unwrap()
            .send(Command::InjectPanic)
            .unwrap();
        render(&mut processor, 1);
        std::thread::sleep(std::time::Duration::from_millis(delay));
        controller.lock().unwrap().check_health();
        render(&mut processor, 1);
    }
    let stopped = collect(&mut session, &controller).stopped.unwrap();
    assert_eq!(stopped.reason, "engineRecovery");
    assert_eq!(stopped.dropped, 0);
    render(&mut processor, 2);
    assert!(collect(&mut session, &controller).stopped.is_none());
    assert!(!controller.lock().unwrap().live().playing);
    assert!(!session.view().recording);
    assert!(
        session.settle_recording().is_ok(),
        "the End barrier must settle the take"
    );
    assert_eq!(notes(&session).len(), before.len() + 3);
    assert!(session.view().dirty);
    undo(&mut session);
    assert_eq!(notes(&session), before);
    assert!(controller.lock().unwrap().reconfigure(
        &mut processor,
        EngineConfig {
            sample_rate: 48_000.0
        }
    ));
    render(&mut processor, 2);
    assert!(!controller.lock().unwrap().live().playing);
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 2);
    session.live_note_on(ch, 79, 0.8).unwrap();
    render(&mut processor, 3);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(!events.iter().any(|e| matches!(e, RecordEvent::Note { .. })));
}

#[test]
fn recovery_exhaustion_preserves_accepted_notes_with_a_full_recording_fifo() {
    let (mut session, controller, mut processor, ch, before) = three_notes();
    session.set_recording(true).unwrap();
    start_take(&mut session, &controller, &mut processor);
    let _held = session.live_note_on(ch, 76, 0.8).unwrap();
    render(&mut processor, 6);
    collect(&mut session, &controller);
    // Exactly fill the recording FIFO with 512 completed notes. No events are refused.
    for _ in 0..512 {
        play_key(&session, &mut processor, ch, 70);
    }
    assert_eq!(controller.lock().unwrap().record_dropped(), 0);
    for delay in [0, 250, 500, 1000] {
        controller
            .lock()
            .unwrap()
            .send(Command::InjectPanic)
            .unwrap();
        render(&mut processor, 1);
        std::thread::sleep(std::time::Duration::from_millis(delay));
        controller.lock().unwrap().check_health();
        render(&mut processor, 1);
    }
    let stopped = collect(&mut session, &controller).stopped.unwrap();
    assert_eq!(stopped.reason, "engineRecovery");
    render(&mut processor, 2);
    collect(&mut session, &controller);
    assert!(!controller.lock().unwrap().live().playing);
    assert!(!session.view().recording);
    assert_eq!(
        notes(&session).len(),
        before.len() + 513,
        "R5: recovery End must retain the already accepted held note through a full recorder"
    );
    assert!(session.settle_recording().is_ok(), "R5: take must settle");
    undo(&mut session);
    assert_eq!(notes(&session), before);
}
