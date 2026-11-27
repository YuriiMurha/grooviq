use grooviq_engine as engine;
use grooviq_project::{
    arrangement_edit::{self, ArrangementEdit as Op, HistoryEffect, parse_batch},
    session::Session,
    *,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

const CLIP: u32 = 3;
const CONFIG: engine::EngineConfig = engine::EngineConfig {
    sample_rate: 48_000.0,
};

fn volume() -> AutomationTarget {
    AutomationTarget::ChannelVolume { channel: 1 }
}

fn point(tick: u64, value: f32, mode: SegmentMode) -> AutomationPoint {
    AutomationPoint { tick, value, mode }
}

fn project() -> Project {
    let mut p = Project::new();
    p.bpm = 120.0;
    p.play_mode = PlayMode::Song;
    p.channels.push(
        serde_json::from_str(r#"{"id":1,"name":"Synth","color":0,"instrument":"synth"}"#).unwrap(),
    );
    p.patterns[0].notes.push(Note {
        id: 1,
        channel: 1,
        tick: 0,
        length: 240,
        key: 60,
        velocity: 0.8,
        slide: false,
    });
    p.patterns[0].next_note_id = 2;
    p.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "Track".into(),
        color: 0,
        mute: false,
    });
    p.arrangement.clips.extend([
        Clip {
            id: CLIP,
            track: 2,
            name: "Volume".into(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Automation {
                target: volume(),
                priority: 4,
                offset_tick: 10,
                length_ticks: 100,
                points: vec![point(10, 0.0, SegmentMode::Hold)].into(),
            },
        },
        Clip {
            id: 5,
            track: 2,
            name: "Notes".into(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Pattern {
                pattern: 0,
                offset_tick: 0,
                length_ticks: 960,
            },
        },
    ]);
    p.next_id = 6;
    p
}

fn session(p: Project, dir: &std::path::Path) -> Session {
    let (controller, _processor) = engine::engine(CONFIG);
    let mut session = Session::new(Arc::new(Mutex::new(controller)));
    session
        .open(
            &dir.join("source.grooviq"),
            Loaded {
                project: p,
                samples: Default::default(),
                warnings: vec![],
            },
            DecodedSamples::default(),
        )
        .unwrap();
    session
}

fn edit(session: &mut Session, ops: Vec<Op>) -> Result<Vec<u32>, String> {
    let at = session.arrangement_view();
    session
        .edit_arrangement(at.generation, at.revision, ops, Instant::now())
        .map(|ack| ack.created_ids)
        .map_err(|refusal| refusal.message)
}

fn set_points(clip: u32, points: Vec<AutomationPoint>) -> Op {
    Op::SetAutomationPoints { clip, points }
}

fn points(clip: &Clip) -> &CurvePoints {
    let ClipContent::Automation { points, .. } = &clip.content else {
        panic!("expected an automation clip");
    };
    points
}

#[test]
fn refuses_unsorted_points_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(project(), dir.path());
    let before = s.project().clone();
    let view = s.view();
    let batch = parse_batch(
        r#"[
          {"clip":3,"target":{"type":"channelPan","channel":1},"priority":6,"kind":"setAutomationClip"},
          {"points":[{"tick":30,"value":0.2,"mode":"linear"},{"tick":10,"value":0.1,"mode":"hold"}],
           "clip":3,"kind":"setAutomationPoints"}
        ]"#,
    )
    .unwrap();
    assert!(edit(&mut s, batch).is_err());
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
}

#[test]
fn refuses_values_outside_the_targets_native_range() {
    let mut p = project();
    for value in [-0.1, 2.01, f32::NAN] {
        assert!(
            arrangement_edit::apply(
                &mut p.clone(),
                vec![set_points(CLIP, vec![point(10, value, SegmentMode::Hold)])],
            )
            .is_err(),
            "volume accepted {value}"
        );
    }
    arrangement_edit::apply(
        &mut p,
        vec![set_points(CLIP, vec![point(10, 1.5, SegmentMode::Hold)])],
    )
    .unwrap();
    assert!(
        arrangement_edit::apply(
            &mut p,
            vec![Op::SetAutomationClip {
                clip: CLIP,
                target: AutomationTarget::ChannelPan { channel: 1 },
                priority: 4,
            }],
        )
        .is_err()
    );
}

#[test]
fn refuses_ticks_outside_the_selected_clips_source_window() {
    for tick in [9, 110] {
        assert!(
            arrangement_edit::apply(
                &mut project(),
                vec![set_points(CLIP, vec![point(tick, 1.0, SegmentMode::Hold)])],
            )
            .is_err(),
            "accepted out-of-window tick {tick}"
        );
    }
    arrangement_edit::apply(
        &mut project(),
        vec![set_points(
            CLIP,
            vec![
                point(10, 0.0, SegmentMode::Linear),
                point(109, 1.0, SegmentMode::Hold),
            ],
        )],
    )
    .unwrap();
}

#[test]
fn refuses_per_clip_limit_in_parser_and_native_admission() {
    let oversized: Vec<_> = (10..=10 + MAX_AUTOMATION_POINTS as u64)
        .map(|tick| point(tick, 1.0, SegmentMode::Hold))
        .collect();
    let encoded = serde_json::to_string(&[set_points(CLIP, oversized.clone())]).unwrap();
    assert!(
        parse_batch(&encoded)
            .unwrap_err()
            .contains("arrangement list limit exceeded")
    );
    let mut p = project();
    let ClipContent::Automation { length_ticks, .. } = &mut p.arrangement.clips[0].content else {
        unreachable!();
    };
    *length_ticks = oversized.len() as u64;
    assert!(arrangement_edit::apply(&mut p, vec![set_points(CLIP, oversized)]).is_err());
}

#[test]
fn refuses_total_limit_in_parser_and_native_admission() {
    let limit = MAX_TOTAL_AUTOMATION_POINTS;
    let full = vec![point(10, 0.5, SegmentMode::Hold); MAX_AUTOMATION_POINTS];
    let remaining = limit % MAX_AUTOMATION_POINTS;
    let full_count = limit / MAX_AUTOMATION_POINTS;
    let mut ops = vec![set_points(CLIP, full.clone()); full_count];
    ops.push(set_points(CLIP, vec![full[0].clone(); remaining]));
    let encoded = serde_json::to_string(&ops).unwrap();
    assert_eq!(parse_batch(&encoded).unwrap().len(), ops.len());
    drop(encoded);
    ops.push(set_points(CLIP, vec![full[0].clone()]));
    let encoded = serde_json::to_string(&ops).unwrap();
    assert!(
        parse_batch(&encoded)
            .unwrap_err()
            .contains("total automation point limit exceeded")
    );

    let mut p = project();
    let mut template = p.arrangement.clips[0].clone();
    let ClipContent::Automation { length_ticks, .. } = &mut p.arrangement.clips[0].content else {
        unreachable!();
    };
    *length_ticks = remaining as u64 + 1;
    if let ClipContent::Automation { points, .. } = &mut template.content {
        *points = full.into();
    }
    for index in 0..full_count {
        let mut clip = template.clone();
        clip.id = 10 + index as u32;
        if let ClipContent::Automation { priority, .. } = &mut clip.content {
            *priority = 100 + index as u32;
        }
        p.arrangement.clips.push(clip);
    }
    p.next_id = 100 + full_count as u32;
    let fill = |count| {
        set_points(
            CLIP,
            (10..10 + count as u64)
                .map(|tick| point(tick, 0.5, SegmentMode::Hold))
                .collect(),
        )
    };
    arrangement_edit::apply(&mut p, vec![fill(remaining)]).unwrap();
    assert!(arrangement_edit::apply(&mut p, vec![fill(remaining + 1)]).is_err());
}

#[test]
fn refuses_duplicate_ticks_at_any_point_count() {
    for count in [2, MAX_AUTOMATION_POINTS] {
        let message = arrangement_edit::apply(
            &mut project(),
            vec![set_points(
                CLIP,
                vec![point(10, 0.5, SegmentMode::Hold); count],
            )],
        )
        .unwrap_err();
        assert!(message.contains("strictly increasing"), "{message}");
    }
}

#[test]
fn refuses_invalid_targets_priorities_and_non_automation_clips() {
    for (clip, target, priority) in [
        (CLIP, AutomationTarget::ChannelVolume { channel: 99 }, 4),
        (
            CLIP,
            AutomationTarget::InstrumentParam {
                channel: 1,
                instrument: "sampler".into(),
                param: "gain".into(),
            },
            4,
        ),
        (
            CLIP,
            AutomationTarget::InstrumentParam {
                channel: 1,
                instrument: "synth".into(),
                param: "missing".into(),
            },
            4,
        ),
        (CLIP, volume(), u32::MAX),
        (CLIP, volume(), 5),
        (5, volume(), 4),
    ] {
        assert!(
            arrangement_edit::apply(
                &mut project(),
                vec![Op::SetAutomationClip {
                    clip,
                    target,
                    priority,
                }]
            )
            .is_err()
        );
    }
    assert!(arrangement_edit::apply(&mut project(), vec![set_points(5, vec![])]).is_err());
}

#[test]
fn refusing_a_huge_priority_keeps_project_ids_available() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(project(), dir.path());
    let next_id = s.project().next_id;
    let message = edit(
        &mut s,
        vec![Op::SetAutomationClip {
            clip: CLIP,
            target: volume(),
            priority: u32::MAX - 1,
        }],
    )
    .unwrap_err();
    assert!(message.contains("next available id"), "{message}");
    assert_eq!(s.project().next_id, next_id);
    let created = edit(
        &mut s,
        vec![Op::CreateTrack {
            name: "New track".into(),
            color: 0,
        }],
    )
    .unwrap();
    assert_eq!(created, [next_id]);
}

#[test]
fn refuses_retargeting_a_shared_curve_until_it_is_unique() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(project(), dir.path());
    let linked = edit(
        &mut s,
        vec![Op::Copy {
            clip: CLIP,
            track: 2,
            start_tick: 480,
        }],
    )
    .unwrap()[0];
    let retarget = Op::SetAutomationClip {
        clip: CLIP,
        target: AutomationTarget::ChannelPan { channel: 1 },
        priority: 4,
    };
    let before = s.project().clone();
    let message = edit(&mut s, vec![retarget.clone()]).unwrap_err();
    assert!(
        message.contains(&format!("clip {linked} (Volume)")),
        "{message}"
    );
    assert!(message.contains("make it unique first"), "{message}");
    assert_eq!(s.project(), &before);
    edit(&mut s, vec![Op::DeleteClip { clip: linked }, retarget]).unwrap();
    assert!(matches!(
        s.project().arrangement.clips[0].content,
        ClipContent::Automation {
            target: AutomationTarget::ChannelPan { channel: 1 },
            ..
        }
    ));
}

#[test]
fn linked_points_and_clip_settings_share_a_single_reversible_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project();
    let mut independent = p.arrangement.clips[0].clone();
    independent.id = 6;
    let ClipContent::Automation {
        points: independent_points,
        priority,
        ..
    } = &mut independent.content
    else {
        unreachable!();
    };
    *independent_points = independent_points.to_vec().into();
    *priority = 7;
    p.arrangement.clips.push(independent);
    p.next_id = 8;
    let mut s = session(p, dir.path());
    let linked = edit(
        &mut s,
        vec![Op::Copy {
            clip: CLIP,
            track: 2,
            start_tick: 480,
        }],
    )
    .unwrap()[0];
    let before = s.project().arrangement.clone();
    assert!(points(&before.clips[0]).shares_allocation(points(&before.clips[3])));
    let batch = parse_batch(
        r#"[
          {"kind":"setAutomationPoints","clip":3,"points":[{"tick":10,"value":0.4,"mode":"linear"}]},
          {"kind":"setAutomationClip","clip":3,"target":{"type":"channelVolume","channel":1},"priority":10}
        ]"#,
    )
    .unwrap();
    assert_eq!(linked, before.clips[3].id);
    assert!(edit(&mut s, batch).unwrap().is_empty());
    let after = s.project().arrangement.clone();
    let original = &after.clips[0];
    let copy = &after.clips[3];
    assert!(points(original).shares_allocation(points(copy)));
    assert_eq!(points(copy)[0].value, 0.4);
    assert_eq!(after.clips[2], before.clips[2]);
    assert!(matches!(
        original.content,
        ClipContent::Automation {
            target: AutomationTarget::ChannelVolume { channel: 1 },
            priority: 10,
            ..
        }
    ));
    assert!(matches!(
        copy.content,
        ClipContent::Automation {
            target: AutomationTarget::ChannelVolume { channel: 1 },
            priority: 9,
            ..
        }
    ));
    assert_eq!(s.project().next_id, 11);
    for (undo, expected) in [(true, &before), (false, &after)] {
        let at = s.arrangement_view();
        let ack = s
            .step_project_history(at.generation, at.revision, undo, Instant::now())
            .unwrap();
        assert!(matches!(ack.effect, HistoryEffect::Arrangement));
        let current = &s.project().arrangement;
        assert_eq!(current, expected);
        assert!(points(&current.clips[0]).shares_allocation(points(&current.clips[3])));
        assert_eq!(s.project().next_id, 11);
    }
    let path = dir.path().join("edited.grooviq");
    let backups = dir.path().join("backups");
    s.save(Some(&path), &backups).unwrap();
    let original_bytes = std::fs::read(&path).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.project.arrangement, s.project().arrangement);
    assert_eq!(loaded.project.next_id, 11);
    s.open(&path, loaded, DecodedSamples::default()).unwrap();
    let reopened_dir = dir.path().join("reopened");
    std::fs::create_dir(&reopened_dir).unwrap();
    let reopened_path = reopened_dir.join("edited.grooviq");
    s.save(Some(&reopened_path), &backups).unwrap();
    assert!(std::fs::read(reopened_path).unwrap() == original_bytes);
}

#[test]
fn reverting_points_inside_a_batch_preserves_slice_links_after_redo() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project();
    let ClipContent::Automation { points: source, .. } = &mut p.arrangement.clips[0].content else {
        unreachable!();
    };
    source.make_mut().push(point(60, 1.25, SegmentMode::Linear));
    let mut s = session(p, dir.path());
    let fragment = edit(
        &mut s,
        vec![Op::Slice {
            clip: CLIP,
            tick: 50,
            audio_first_frames: None,
        }],
    )
    .unwrap()[0];
    let original_window = points(&s.project().arrangement.clips[0])[..1].to_vec();
    let linked = edit(
        &mut s,
        vec![
            set_points(CLIP, vec![point(10, 0.75, SegmentMode::Hold)]),
            set_points(CLIP, original_window),
            Op::Copy {
                clip: CLIP,
                track: 2,
                start_tick: 480,
            },
        ],
    )
    .unwrap()[0];
    for undo in [true, true, false, false] {
        let at = s.arrangement_view();
        s.step_project_history(at.generation, at.revision, undo, Instant::now())
            .unwrap();
    }
    edit(
        &mut s,
        vec![set_points(
            fragment,
            vec![point(60, 0.4, SegmentMode::Hold)],
        )],
    )
    .unwrap();
    for id in [CLIP, fragment, linked] {
        let clip = s
            .project()
            .arrangement
            .clips
            .iter()
            .find(|clip| clip.id == id)
            .unwrap();
        assert_eq!(
            &**points(clip),
            &[
                point(10, 0.0, SegmentMode::Hold),
                point(60, 0.4, SegmentMode::Hold),
            ],
            "clip {id} lost points or its source link"
        );
        assert!(points(clip).shares_allocation(points(&s.project().arrangement.clips[0])));
    }
}

#[test]
fn editing_a_slice_preserves_its_siblings_points_through_undo_and_redo() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project();
    let original = vec![
        point(10, 0.0, SegmentMode::Linear),
        point(60, 2.0, SegmentMode::Hold),
        point(90, 1.5, SegmentMode::Linear),
    ];
    let ClipContent::Automation { points: source, .. } = &mut p.arrangement.clips[0].content else {
        unreachable!();
    };
    *source = original.clone().into();
    let mut s = session(p, dir.path());
    edit(
        &mut s,
        vec![Op::Slice {
            clip: CLIP,
            tick: 50,
            audio_first_frames: None,
        }],
    )
    .unwrap();
    let before = s.project().arrangement.clone();
    let window = vec![
        point(10, 0.5, SegmentMode::Linear),
        point(59, 1.0, SegmentMode::Hold),
    ];
    edit(&mut s, vec![set_points(CLIP, window.clone())]).unwrap();
    let after = s.project().arrangement.clone();
    let mut expected = window;
    expected.extend_from_slice(&original[1..]);
    assert_eq!(&**points(&after.clips[2]), expected.as_slice());
    assert!(points(&after.clips[0]).shares_allocation(points(&after.clips[2])));
    for (undo, expected) in [(true, before), (false, after)] {
        let at = s.arrangement_view();
        s.step_project_history(at.generation, at.revision, undo, Instant::now())
            .unwrap();
        let current = &s.project().arrangement;
        assert_eq!(current, &expected);
        assert!(points(&current.clips[0]).shares_allocation(points(&current.clips[2])));
    }
}

#[test]
fn extending_after_trim_and_edit_reveals_the_hidden_point() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = project();
    let ClipContent::Automation { points: source, .. } = &mut p.arrangement.clips[0].content else {
        unreachable!();
    };
    let hidden = point(90, 2.0, SegmentMode::Hold);
    source.make_mut().push(hidden.clone());
    let mut s = session(p, dir.path());
    let resize = |s: &Session, length| {
        let mut content = s.project().arrangement.clips[0].content.clone();
        let ClipContent::Automation { length_ticks, .. } = &mut content else {
            unreachable!();
        };
        *length_ticks = length;
        Op::Resize {
            clip: CLIP,
            start_tick: 0,
            content,
        }
    };
    let trim = resize(&s, 50);
    edit(&mut s, vec![trim]).unwrap();
    let replacement = point(10, 0.5, SegmentMode::Hold);
    edit(&mut s, vec![set_points(CLIP, vec![replacement.clone()])]).unwrap();
    let target = engine::AutomationTarget::ChannelVolume(engine::ChannelId(1));
    let trimmed = s.checked_export_state().unwrap();
    assert_eq!(trimmed.song.automation.value_at(target, 80.0), None);

    let extend = resize(&s, 100);
    edit(&mut s, vec![extend]).unwrap();
    assert_eq!(
        &**points(&s.project().arrangement.clips[0]),
        &[replacement, hidden]
    );
    let extended = s.checked_export_state().unwrap();
    assert_eq!(extended.song.automation.value_at(target, 80.0), Some(2.0));
}

#[test]
fn edited_point_modes_reach_offline_render() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(project(), dir.path());
    let before = s.checked_export_state().unwrap();
    edit(
        &mut s,
        vec![set_points(
            CLIP,
            vec![
                point(10, 0.0, SegmentMode::Linear),
                point(50, 1.0, SegmentMode::Hold),
                point(90, 0.5, SegmentMode::Linear),
            ],
        )],
    )
    .unwrap();
    let after = s.checked_export_state().unwrap();
    let target = engine::AutomationTarget::ChannelVolume(engine::ChannelId(1));
    assert_eq!(before.song.automation.value_at(target, 20.0), Some(0.0));
    for (tick, value) in [(20.0, 0.5), (40.0, 1.0), (60.0, 1.0), (80.0, 0.5)] {
        assert_eq!(after.song.automation.value_at(target, tick), Some(value));
    }
    assert_eq!(after.song.automation.value_at(target, 100.0), None);

    let render =
        |state| engine::render_offline_with(CONFIG, state, &[(0, engine::Command::Play)], 2496, 64);
    let before = render(before);
    let after = render(after);
    assert!(before[2048..].iter().all(|sample| *sample == 0.0));
    assert!(after[2048..].iter().any(|sample| sample.abs() > 0.001));
}

#[test]
fn instrument_created_through_arrangement_restores_manual_output_on_stop() {
    let dir = tempfile::tempdir().unwrap();
    let mut project = project();
    project.channels[0].synth.insert("master".into(), 0.0);
    project.arrangement.clips.clear();
    let (control, mut processor) = engine::engine(CONFIG);
    let control = Arc::new(Mutex::new(control));
    let mut session = Session::new(Arc::clone(&control));
    session
        .open(
            &dir.path().join("instrument.grooviq"),
            Loaded {
                project,
                samples: Default::default(),
                warnings: vec![],
            },
            DecodedSamples::default(),
        )
        .unwrap();
    let created = edit(
        &mut session,
        vec![Op::CreateClip {
            track: 2,
            name: "Master".into(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Automation {
                target: AutomationTarget::InstrumentParam {
                    channel: 1,
                    instrument: "synth".into(),
                    param: "master".into(),
                },
                priority: 0,
                offset_tick: 0,
                length_ticks: 7680,
                points: vec![point(0, -36.0, SegmentMode::Hold)].into(),
            },
        }],
    )
    .unwrap();
    let send = |command| control.lock().unwrap().send(command).unwrap();
    let note = |id, on| engine::Command::ChannelNote {
        channel: engine::ChannelId(1),
        key: 60,
        velocity: 0.8,
        on,
        id: engine::NoteId(id),
    };
    let mut measure = |blocks: usize| {
        let mut block = [0.0_f32; 128];
        let mut peak = 0.0_f32;
        for index in 0..blocks {
            processor.process(&mut block, 2);
            if index >= blocks / 2 {
                peak = block.iter().map(|sample| sample.abs()).fold(peak, f32::max);
            }
        }
        peak
    };
    session.play().unwrap();
    measure(96); // Let the automated gain settle before the first fresh note.
    send(note(60, true));
    let automated = measure(96);
    assert!(control.lock().unwrap().live().playing);
    assert!(automated > 0.0001, "the automated note must sound");

    send(note(60, false));
    measure(576);
    assert!(control.lock().unwrap().live().playing);
    send(engine::Command::Stop);
    measure(96);
    assert!(!control.lock().unwrap().live().playing);
    send(note(61, true));
    let restored = measure(96);
    assert!(
        restored > 0.02 && restored > automated * 20.0,
        "Stop must restore manual synth gain: automated {automated}, restored {restored}"
    );
    assert_eq!(session.project().channels[0].synth["master"], 0.0);
    let clip = &session.project().arrangement.clips[0];
    assert_eq!(clip.id, created[0]);
    assert_eq!(&**points(clip), &[point(0, -36.0, SegmentMode::Hold)]);
}
