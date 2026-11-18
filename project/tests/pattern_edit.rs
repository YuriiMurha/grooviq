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
