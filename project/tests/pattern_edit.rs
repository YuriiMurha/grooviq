use grooviq_project::{
    Project,
    pattern_edit::{PatternEdit, apply},
};

#[test]
fn duplicate_is_independent_and_allocator_never_rewinds() {
    let mut p = Project::new();
    let id = apply(&mut p, PatternEdit::Duplicate { pattern: 0 }).unwrap();
    assert_ne!(id, 0);
    p.patterns[1].steps = 32;
    assert_eq!(p.patterns[0].steps, 16);
    let floor = p.next_id;
    apply(&mut p, PatternEdit::Delete { pattern: id }).unwrap();
    assert_eq!(p.next_id, floor);
    assert!(apply(&mut p, PatternEdit::Delete { pattern: 0 }).is_err());
}

#[test]
fn exhausted_allocation_is_atomic() {
    let mut p = Project::new();
    p.next_id = u32::MAX;
    let before = p.clone();
    assert!(apply(&mut p, PatternEdit::Create { name: "P".into() }).is_err());
    assert_eq!(p, before);
}

use grooviq_engine::{EngineConfig, engine};
use grooviq_project::session::{Edit, Session};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
fn session() -> Session {
    let (c, _p) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    Session::new(Arc::new(Mutex::new(c)))
}
#[test]
fn admission_rejects_structural_and_note_changes_before_commit() {
    let mut s = session();
    s.set_precommit_validator(Some(Arc::new(|_| Err("compile budget".into()))));
    let before = s.view();
    assert!(
        s.edit_patterns(
            0,
            0,
            PatternEdit::Create { name: "P".into() },
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.view(), before);
    assert!(
        s.apply(Edit::SetPatternLength { steps: 32 }, Instant::now())
            .is_err()
    );
    assert_eq!(s.view(), before);
}

#[test]
fn referenced_delete_and_unique_only_change_the_chosen_reference() {
    use grooviq_project::{Clip, ClipContent};
    let mut p = Project::new();
    apply(
        &mut p,
        PatternEdit::Create {
            name: "Other".into(),
        },
    )
    .unwrap();
    for id in [10, 11] {
        p.arrangement.clips.push(Clip {
            id,
            track: 9,
            name: String::new(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 0,
                length_ticks: 960,
            },
        });
    }
    let before = p.clone();
    assert!(apply(&mut p, PatternEdit::Delete { pattern: 0 }).is_err());
    assert_eq!(p, before);
    let id = apply(&mut p, PatternEdit::MakeUnique { clip: 10 }).unwrap();
    assert!(
        matches!(p.arrangement.clips[0].content, ClipContent::Pattern { pattern, .. } if pattern == id)
    );
    assert_eq!(p.arrangement.clips[1], before.arrangement.clips[1]);
}

#[test]
fn selection_keeps_undo_with_its_pattern_and_late_notes_are_refused() {
    use grooviq_project::notes::NotesEdit;
    let mut s = session();
    let now = Instant::now();
    let channel = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.apply(Edit::ToggleStep { channel, step: 0 }, now).unwrap();
    s.edit_patterns(
        0,
        0,
        PatternEdit::Create {
            name: "Other".into(),
        },
        now,
    )
    .unwrap();
    let id = s.project().patterns[1].id;
    s.edit_patterns(0, 1, PatternEdit::Select { pattern: id }, now)
        .unwrap();
    assert!(s.undo_notes(now).unwrap().is_none());
    let before = s.project().clone();
    let ack = s
        .edit_notes(
            NotesEdit {
                generation: 0,
                pattern_id: 0,
                base_revision: 1,
                add: vec![],
                update: vec![],
                delete: vec![1],
            },
            now,
        )
        .unwrap();
    assert!(ack.reload);
    assert_eq!(s.project(), &before);
    s.edit_patterns(0, 2, PatternEdit::Select { pattern: 0 }, now)
        .unwrap();
    s.undo_notes(now).unwrap().unwrap();
    assert!(s.current_pattern().notes.is_empty());
    assert!(s.project().patterns[1].notes.is_empty());
}

#[test]
fn bounded_referenced_note_and_undo_admission_roll_back() {
    use grooviq_project::{Clip, ClipContent, DecodedSamples, Loaded, PlaylistTrack};
    let mut s = session();
    let now = Instant::now();
    let channel = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    let mut p = s.project().clone();
    p.arrangement.tracks.push(PlaylistTrack {
        id: 10,
        name: "Track".into(),
        color: 0,
        mute: false,
    });
    for id in [11, 12] {
        p.arrangement.clips.push(Clip {
            id,
            track: 10,
            name: String::new(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 0,
                length_ticks: 3840,
            },
        });
    }
    s.open(
        std::path::Path::new("test.grooviq"),
        Loaded {
            project: p,
            samples: Default::default(),
            warnings: vec![],
        },
        DecodedSamples::default(),
    )
    .unwrap();
    let validator = Arc::new(|p: &Project| {
        let expanded: usize = p
            .arrangement
            .clips
            .iter()
            .filter_map(|c| match c.content {
                ClipContent::Pattern { pattern, .. } => p
                    .patterns
                    .iter()
                    .find(|p| p.id == pattern)
                    .map(|p| p.notes.len()),
                _ => None,
            })
            .sum();
        if expanded > 1 {
            Err("Expanded note budget exceeded".into())
        } else {
            Ok(())
        }
    });
    s.set_precommit_validator(Some(validator.clone()));
    let before = s.view();
    assert!(
        s.edit_notes(
            grooviq_project::notes::NotesEdit {
                generation: s.generation(),
                pattern_id: 0,
                base_revision: s.notes_revision(),
                add: vec![grooviq_project::notes::NewNote {
                    channel,
                    tick: 0,
                    length: 240,
                    key: 60,
                    velocity: 0.8,
                    slide: false
                }],
                update: vec![],
                delete: vec![],
            },
            now
        )
        .is_err()
    );
    assert_eq!(s.view(), before);
    assert!(s.apply(Edit::ToggleStep { channel, step: 0 }, now).is_err());
    assert_eq!(s.view(), before);
    assert!(!s.view().can_undo);
    s.set_precommit_validator(None);
    s.apply(Edit::ToggleStep { channel, step: 0 }, now).unwrap();
    s.apply(Edit::ClearStep { channel, step: 0 }, now).unwrap();
    s.set_precommit_validator(Some(validator));
    let before = s.view();
    assert!(s.undo_notes(now).is_err());
    assert_eq!(s.view(), before);
    assert!(s.view().can_undo);
}

#[test]
fn editing_b_does_not_erase_as_history() {
    let mut s = session();
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.apply(
        Edit::ToggleStep {
            channel: ch,
            step: 0,
        },
        now,
    )
    .unwrap();
    s.edit_patterns(0, 0, PatternEdit::Create { name: "B".into() }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    s.edit_patterns(0, 1, PatternEdit::Select { pattern: b }, now)
        .unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: ch,
            step: 1,
        },
        now,
    )
    .unwrap();
    s.edit_patterns(0, 2, PatternEdit::Select { pattern: 0 }, now)
        .unwrap();
    assert!(s.undo_notes(now).unwrap().is_some());
    assert!(s.current_pattern().notes.is_empty());
    assert_eq!(s.project().patterns[1].notes.len(), 1);
}

#[test]
fn refused_recording_admission_keeps_the_take_and_blocks_save_and_replacement() {
    use grooviq_engine::{ChannelId, NoteId, RecordEvent};
    let (c, mut processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let controller = Arc::new(Mutex::new(c));
    let mut s = Session::new(controller.clone());
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    let mut output = [0.0; 128];
    processor.process(&mut output, 2);
    // Two linked references amplify the candidate beyond a one-event compile budget.
    let mut linked = s.project().clone();
    linked
        .arrangement
        .tracks
        .push(grooviq_project::PlaylistTrack {
            id: 10,
            name: "Track".into(),
            color: 0,
            mute: false,
        });
    for id in [11, 12] {
        linked.arrangement.clips.push(grooviq_project::Clip {
            id,
            track: 10,
            name: String::new(),
            start_tick: 0,
            muted: false,
            content: grooviq_project::ClipContent::Pattern {
                pattern: 0,
                offset_tick: 0,
                length_ticks: 3840,
            },
        });
    }
    s.open(
        std::path::Path::new("linked.grooviq"),
        grooviq_project::Loaded {
            project: linked,
            samples: Default::default(),
            warnings: vec![],
        },
        grooviq_project::DecodedSamples::default(),
    )
    .unwrap();
    processor.process(&mut output, 2);
    let mark = controller.lock().unwrap().record_mark();
    s.collect_recorded(&[RecordEvent::Mark { id: mark }], now);
    s.set_precommit_validator(Some(Arc::new(|p| {
        if p.arrangement.clips.len() * p.patterns[0].notes.len() <= 1 {
            Ok(())
        } else {
            Err("expanded budget".into())
        }
    })));
    let before = s.view();
    let events = [
        RecordEvent::Note {
            channel: ChannelId(ch),
            key: 60,
            velocity: 0.8,
            on: true,
            tick: 0,
            position: 0,
            take: 1,
            id: NoteId(9),
        },
        RecordEvent::Note {
            channel: ChannelId(ch),
            key: 60,
            velocity: 0.8,
            on: false,
            tick: 240,
            position: 240,
            take: 1,
            id: NoteId(9),
        },
    ];
    let recorded = s.collect_recorded(&events, now);
    assert!(recorded.stopped.is_some());
    assert_eq!(s.view(), before);
    assert!(
        s.begin_save(Some(std::path::Path::new("take.grooviq")))
            .is_err()
    );
    assert!(s.new_project().is_err());
    assert_eq!(s.view(), before);
    s.set_precommit_validator(None);
    assert!(s.collect_recorded(&[], now).revision.is_some());
    assert_eq!(s.current_pattern().notes.len(), 1);
    s.undo_notes(now).unwrap().unwrap();
    assert!(s.current_pattern().notes.is_empty());
}

#[test]
fn a_record_poll_is_admitted_as_one_atomic_candidate_across_takes() {
    use grooviq_engine::{ChannelId, NoteId, RecordEvent};
    let mut s = session();
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.collect_recorded(&[RecordEvent::Mark { id: 1 }], now);
    s.set_precommit_validator(Some(Arc::new(|p| {
        if p.patterns[0].notes.len() > 1 {
            Err("expanded budget".into())
        } else {
            Ok(())
        }
    })));
    let mut events = vec![];
    for take in [1, 2] {
        for on in [true, false] {
            events.push(RecordEvent::Note {
                channel: ChannelId(ch),
                key: 60,
                velocity: 0.8,
                on,
                tick: if on { 0 } else { 240 },
                position: 0,
                take,
                id: NoteId(take as u64),
            });
        }
    }
    let before = s.view();
    s.collect_recorded(&events, now);
    assert_eq!(s.view(), before);
    assert!(s.current_pattern().notes.is_empty());
    s.set_precommit_validator(None);
    s.collect_recorded(&[], now);
    assert_eq!(s.current_pattern().notes.len(), 2);
    s.undo_notes(now).unwrap().unwrap();
    assert_eq!(s.current_pattern().notes.len(), 1);
    s.undo_notes(now).unwrap().unwrap();
    assert!(s.current_pattern().notes.is_empty());
}

#[test]
fn selection_disarms_then_waits_for_the_original_take_end_barrier() {
    use grooviq_engine::{Command, RecordEvent};
    use grooviq_project::pattern_edit::RefusalReason;
    use grooviq_project::session::PatternReply;
    let (c, mut processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let controller = Arc::new(Mutex::new(c));
    let mut s = Session::new(controller.clone());
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.edit_patterns(0, 0, PatternEdit::Create { name: "B".into() }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    s.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    s.live_note_on(ch, 60, 0.8).unwrap();
    let mut output = [0.0; 128];
    processor.process(&mut output, 2);
    let mut events = vec![];
    controller.lock().unwrap().take_recorded(&mut events);
    s.collect_recorded(&events, now);
    let reply = s.pattern_command(0, 1, PatternEdit::Select { pattern: b }, now);
    assert!(
        matches!(reply,PatternReply::Refused{reason:RefusalReason::RecordingSettling,settlement,..} if settlement.recording_disarmed)
    );
    assert_eq!(s.project().current_pattern, 0);
    processor.process(&mut output, 2);
    events.clear();
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(events.iter().any(|e| matches!(e, RecordEvent::End { .. })));
    s.collect_recorded(&events, now);
    assert_eq!(s.current_pattern().notes.len(), 1);
    assert!(matches!(
        s.pattern_command(0, 1, PatternEdit::Select { pattern: b }, now),
        PatternReply::Applied { .. }
    ));
    assert!(s.current_pattern().notes.is_empty());
    assert_eq!(s.project().patterns[0].notes.len(), 1);
}

#[test]
fn held_note_step_and_undo_stamps_cannot_cross_selection_or_replacement() {
    use grooviq_project::notes::NotesEdit;
    let mut s = session();
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.apply(
        Edit::ToggleStep {
            channel: ch,
            step: 0,
        },
        now,
    )
    .unwrap();
    let origin = s.pattern_stamp();
    s.edit_patterns(0, 0, PatternEdit::Create { name: "B".into() }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    s.edit_patterns(0, 1, PatternEdit::Select { pattern: b }, now)
        .unwrap();
    s.edit_patterns(0, 2, PatternEdit::Select { pattern: 0 }, now)
        .unwrap();
    let before = s.view();
    let history = s.retained_note_history();
    assert!(
        s.apply_stamped(
            origin,
            Edit::ClearStep {
                channel: ch,
                step: 0
            },
            now
        )
        .is_err()
    );
    assert!(s.history_stamped(origin, true, now).is_err());
    assert!(
        s.edit_notes(
            NotesEdit {
                generation: origin.generation,
                pattern_id: origin.pattern_id,
                base_revision: origin.revision,
                add: vec![],
                update: vec![],
                delete: vec![1]
            },
            now
        )
        .is_err()
    );
    assert_eq!(s.view(), before);
    assert_eq!(s.retained_note_history(), history);
    s.new_project().unwrap();
    let before = s.view();
    assert!(
        s.apply_stamped(
            origin,
            Edit::ClearStep {
                channel: ch,
                step: 0
            },
            now
        )
        .is_err()
    );
    assert_eq!(s.view(), before);
}

#[test]
fn full_engine_queue_refuses_selection_and_note_edits_without_commit() {
    use grooviq_engine::Command;
    let (c, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.edit_patterns(0, 0, PatternEdit::Create { name: "B".into() }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    while c
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 140.0 })
        .is_ok()
    {}
    let before = s.project().clone();
    let view = s.view();
    let history = s.retained_note_history();
    assert!(
        s.edit_patterns(0, 1, PatternEdit::Select { pattern: b }, now)
            .is_err()
    );
    assert!(
        s.apply(
            Edit::ToggleStep {
                channel: ch,
                step: 0
            },
            now
        )
        .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(s.retained_note_history(), history);
}

#[test]
fn histories_share_the_real_two_hundred_step_budget() {
    let mut s = session();
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    for _ in 0..120 {
        s.apply(
            Edit::ToggleStep {
                channel: ch,
                step: 0,
            },
            now,
        )
        .unwrap();
    }
    s.edit_patterns(0, 0, PatternEdit::Create { name: "B".into() }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    s.edit_patterns(0, 1, PatternEdit::Select { pattern: b }, now)
        .unwrap();
    for _ in 0..120 {
        s.apply(
            Edit::ToggleStep {
                channel: ch,
                step: 0,
            },
            now,
        )
        .unwrap();
    }
    assert_eq!(s.retained_note_history().0, 200);
    s.edit_patterns(0, 2, PatternEdit::Select { pattern: 0 }, now)
        .unwrap();
    let mut count = 0;
    while s.undo_notes(now).unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 80);
}

#[test]
fn canonical_refusals_leave_allocator_history_and_engine_unchanged() {
    use grooviq_project::pattern_edit::RefusalReason as R;
    use grooviq_project::session::PatternReply;
    use grooviq_project::{DecodedSamples, Loaded};
    let (c, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let now = Instant::now();
    for (generation, revision, edit, reason) in [
        (
            1,
            0,
            PatternEdit::Create { name: "X".into() },
            R::ProjectReplaced,
        ),
        (0, 1, PatternEdit::Select { pattern: 0 }, R::StaleRevision),
        (
            0,
            0,
            PatternEdit::Select { pattern: 999 },
            R::UnknownPattern,
        ),
        (0, 0, PatternEdit::Delete { pattern: 0 }, R::LastPattern),
        (0, 0, PatternEdit::MakeUnique { clip: 999 }, R::UnknownClip),
    ] {
        let before = s.project().clone();
        let view = s.view();
        let engine = format!("{:?}", c.lock().unwrap().state().pattern);
        assert!(
            matches!(s.pattern_command(generation,revision,edit,now),PatternReply::Refused{reason:r,..} if r==reason)
        );
        assert_eq!(s.project(), &before);
        assert_eq!(s.view(), view);
        assert_eq!(s.retained_note_history(), (0, 0));
        assert_eq!(format!("{:?}", c.lock().unwrap().state().pattern), engine);
    }
    let mut p = s.project().clone();
    p.next_id = u32::MAX;
    s.open(
        std::path::Path::new("ids.grooviq"),
        Loaded {
            project: p,
            samples: Default::default(),
            warnings: vec![],
        },
        DecodedSamples::default(),
    )
    .unwrap();
    let before = s.project().clone();
    assert!(matches!(
        s.pattern_command(
            s.generation(),
            0,
            PatternEdit::Create { name: "X".into() },
            now
        ),
        PatternReply::Refused {
            reason: R::IdExhausted,
            ..
        }
    ));
    assert_eq!(s.project(), &before);
    let before = s.project().clone();
    let view = s.view();
    assert!(matches!(
        s.pattern_command(
            s.generation(),
            0,
            PatternEdit::Rename {
                pattern: 0,
                name: "x".repeat(64 * 1024 * 1024)
            },
            now
        ),
        PatternReply::Refused {
            reason: R::FileBudget,
            ..
        }
    ));
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
}

#[test]
fn histories_share_the_real_two_million_retained_note_budget() {
    use grooviq_project::notes::NotesEdit;
    use grooviq_project::{DecodedSamples, Loaded, Note};
    let mut s = session();
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    let mut p = s.project().clone();
    p.patterns[0].notes = (1..=10000)
        .map(|id| Note {
            id,
            channel: ch,
            tick: 0,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
        })
        .collect();
    p.patterns[0].next_note_id = 10001;
    let b = apply(&mut p, PatternEdit::Duplicate { pattern: 0 }).unwrap();
    s.open(
        std::path::Path::new("history.grooviq"),
        Loaded {
            project: p,
            samples: Default::default(),
            warnings: vec![],
        },
        DecodedSamples::default(),
    )
    .unwrap();
    for pattern in [0, b] {
        if pattern != 0 {
            s.edit_patterns(s.generation(), 0, PatternEdit::Select { pattern }, now)
                .unwrap();
        }
        for edit in 0..60 {
            let mut update = s.current_pattern().notes;
            for n in &mut update {
                n.velocity = if edit % 2 == 0 { 0.7 } else { 0.8 };
            }
            s.edit_notes(
                NotesEdit {
                    generation: s.generation(),
                    pattern_id: pattern,
                    base_revision: s.notes_revision(),
                    add: vec![],
                    delete: vec![],
                    update,
                },
                now,
            )
            .unwrap();
        }
    }
    assert_eq!(s.retained_note_history(), (100, 2_000_000));
    s.edit_patterns(s.generation(), 1, PatternEdit::Select { pattern: 0 }, now)
        .unwrap();
    let mut count = 0;
    while s.undo_notes(now).unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 40);
    assert_eq!(s.project().patterns[1].notes[0].velocity, 0.8);
}

#[test]
fn channels_and_patterns_share_permanent_project_id_exhaustion() {
    use grooviq_project::{DecodedSamples, Loaded};
    let mut s = session();
    let mut p = s.project().clone();
    p.next_id = u32::MAX;
    s.open(
        std::path::Path::new("exhausted.grooviq"),
        Loaded {
            project: p,
            samples: Default::default(),
            warnings: vec![],
        },
        DecodedSamples::default(),
    )
    .unwrap();
    let before = s.project().clone();
    let view = s.view();
    assert!(s.add_synth_channel("Init", Instant::now()).is_err());
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
}

#[test]
fn even_an_empty_take_must_consume_record_off_before_switching() {
    use grooviq_project::{pattern_edit::RefusalReason, session::PatternReply};
    let mut s = session();
    let now = Instant::now();
    s.edit_patterns(0, 0, PatternEdit::Create { name: "B".into() }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    s.set_recording(true).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            s.pattern_command(0, 1, PatternEdit::Select { pattern: b }, now),
            PatternReply::Refused {
                reason: RefusalReason::RecordingSettling,
                ..
            }
        ));
        assert_eq!(s.project().current_pattern, 0);
    }
}

#[test]
fn compiler_refusals_never_publish_a_note_step_undo_or_recording_engine_pattern() {
    use grooviq_engine::{ChannelId, NoteId, RecordEvent};
    use grooviq_project::notes::{NewNote, NotesEdit};
    let (c, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.apply(
        Edit::ToggleStep {
            channel: ch,
            step: 0,
        },
        now,
    )
    .unwrap();
    s.apply(
        Edit::ClearStep {
            channel: ch,
            step: 0,
        },
        now,
    )
    .unwrap();
    let mark = c.lock().unwrap().record_mark();
    s.collect_recorded(&[RecordEvent::Mark { id: mark }], now);
    s.set_precommit_validator(Some(Arc::new(|p| {
        if p.patterns[0].notes.is_empty() {
            Ok(())
        } else {
            Err("expanded budget".into())
        }
    })));
    let before = s.view();
    let history = s.retained_note_history();
    let engine = format!("{:?}", c.lock().unwrap().state().pattern);
    assert!(
        s.edit_notes(
            NotesEdit {
                generation: 0,
                pattern_id: 0,
                base_revision: s.notes_revision(),
                add: vec![NewNote {
                    channel: ch,
                    tick: 0,
                    length: 240,
                    key: 60,
                    velocity: 0.8,
                    slide: false
                }],
                update: vec![],
                delete: vec![]
            },
            now
        )
        .is_err()
    );
    assert!(
        s.apply(
            Edit::ToggleStep {
                channel: ch,
                step: 0
            },
            now
        )
        .is_err()
    );
    assert!(s.undo_notes(now).is_err());
    s.collect_recorded(
        &[
            RecordEvent::Note {
                channel: ChannelId(ch),
                key: 60,
                velocity: 0.8,
                on: true,
                tick: 0,
                position: 0,
                take: 1,
                id: NoteId(8),
            },
            RecordEvent::Note {
                channel: ChannelId(ch),
                key: 60,
                velocity: 0.8,
                on: false,
                tick: 240,
                position: 0,
                take: 1,
                id: NoteId(8),
            },
        ],
        now,
    );
    assert_eq!(s.view(), before);
    assert_eq!(s.retained_note_history(), history);
    assert_eq!(format!("{:?}", c.lock().unwrap().state().pattern), engine);
}

#[test]
fn pattern_identity_is_checked_even_when_note_revisions_coincide() {
    use grooviq_project::notes::NotesEdit;
    let mut s = session();
    let now = Instant::now();
    let ch = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.apply(
        Edit::ToggleStep {
            channel: ch,
            step: 0,
        },
        now,
    )
    .unwrap();
    let origin = s.pattern_stamp();
    s.edit_patterns(0, 0, PatternEdit::Duplicate { pattern: 0 }, now)
        .unwrap();
    let b = s.project().patterns[1].id;
    s.edit_patterns(0, 1, PatternEdit::Select { pattern: b }, now)
        .unwrap();
    assert_eq!(s.notes_revision(), origin.revision);
    let before = s.view();
    assert!(
        s.apply_stamped(
            origin,
            Edit::ClearStep {
                channel: ch,
                step: 0
            },
            now
        )
        .is_err()
    );
    assert!(s.history_stamped(origin, true, now).is_err());
    let ack = s
        .edit_notes(
            NotesEdit {
                generation: origin.generation,
                pattern_id: origin.pattern_id,
                base_revision: origin.revision,
                add: vec![],
                update: vec![],
                delete: vec![1],
            },
            now,
        )
        .unwrap();
    assert!(ack.reload);
    assert!(ack.added_ids.is_empty());
    assert_eq!(s.view(), before);
}
