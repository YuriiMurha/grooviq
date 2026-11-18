use grooviq_engine::{self as e, AudioProcessor, Command, Controller, EngineConfig};
use grooviq_project::{
    arrangement_edit::{ArrangementEdit as Op, HistoryEffect},
    session::{Edit, Session},
    *,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
fn wave() -> Vec<u8> {
    let count = 4800u32;
    let mut b = vec![];
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + count * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48000u32.to_le_bytes());
    b.extend_from_slice(&96000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(count * 2).to_le_bytes());
    for i in 0..count {
        b.extend_from_slice(&((i % 300) as i16 * 50).to_le_bytes());
    }
    b
}
fn rig() -> (
    Session,
    Arc<Mutex<Controller>>,
    AudioProcessor,
    tempfile::TempDir,
) {
    let (c, mut processor) = e::engine(EngineConfig {
        sample_rate: 48000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let d = tempfile::tempdir().unwrap();
    let mut p = Project::new();
    p.bpm = 120.0;
    p.play_mode = PlayMode::Song;
    p.channels.push(
        serde_json::from_str(r#"{"id":1,"name":"Synth","color":0,"instrument":"synth"}"#).unwrap(),
    );
    p.patterns[0].notes.push(Note {
        id: 1,
        channel: 1,
        tick: 240,
        length: 240,
        key: 60,
        velocity: 0.5,
        slide: false,
    });
    p.patterns[0].next_note_id = 2;
    p.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "T".into(),
        color: 0,
        mute: false,
    });
    p.arrangement.audio_assets.push(AudioAsset {
        id: 3,
        sample: SampleRef {
            path: "tone.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 4800,
        original_tempo: Some(120.0),
        original_key: None,
    });
    p.next_id = 4;
    let loaded = Loaded {
        project: p,
        samples: HashMap::from([(
            3,
            SampleFile {
                name: "tone.wav".into(),
                bytes: Arc::new(wave()),
            },
        )]),
        warnings: vec![],
    };
    let decoded = decode_samples(&loaded);
    s.open(&d.path().join("song.grooviq"), loaded, decoded)
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    (s, c, processor, d)
}
fn edit(s: &mut Session, ops: Vec<Op>) -> arrangement_edit::ArrangementAck {
    let v = s.arrangement_view();
    s.edit_arrangement(v.generation, v.revision, ops, Instant::now())
        .unwrap()
}
fn history(s: &mut Session, undo: bool) -> arrangement_edit::HistoryAck {
    let v = s.arrangement_view();
    s.step_project_history(v.generation, v.revision, undo, Instant::now())
        .unwrap()
}
fn pattern() -> ClipContent {
    ClipContent::Pattern {
        pattern: 0,
        offset_tick: 0,
        length_ticks: 960,
    }
}
fn audio(reverse: bool) -> ClipContent {
    ClipContent::Audio {
        asset: 3,
        source_start: 0,
        source_end: 4800,
        gain: 0.7,
        reverse,
        fade_in: 100,
        fade_out: 200,
    }
}
fn curve() -> ClipContent {
    ClipContent::Automation {
        target: AutomationTarget::ChannelPan { channel: 1 },
        priority: 0,
        offset_tick: 0,
        length_ticks: 960,
        points: vec![
            AutomationPoint {
                tick: 0,
                value: -0.5,
                mode: SegmentMode::Linear,
            },
            AutomationPoint {
                tick: 960,
                value: 0.5,
                mode: SegmentMode::Hold,
            },
        ]
        .into(),
    }
}
fn draw(content: ClipContent, start: u64) -> Op {
    Op::CreateClip {
        track: 2,
        name: "C".into(),
        start_tick: start,
        muted: false,
        content,
    }
}
fn sound(s: &Session) -> Vec<f32> {
    e::render_offline_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        s.checked_export_state().unwrap(),
        &[(0, Command::Play)],
        30000,
        127,
    )
}
#[test]
fn mixed_batch_is_one_step_sound_exact_history_and_saved_originals() {
    let (mut s, _c, mut p, d) = rig();
    let before = sound(&s);
    let a = edit(
        &mut s,
        vec![
            draw(pattern(), 0),
            draw(audio(false), 0),
            draw(curve(), 0),
            Op::CreatePattern { name: "B".into() },
        ],
    );
    assert_eq!(a.created_ids.len(), 4);
    assert_eq!(s.history_usage().0, 1);
    let after = sound(&s);
    assert_ne!(before, after);
    let ids = s.project().next_id;
    assert!(matches!(
        history(&mut s, true).effect,
        HistoryEffect::Arrangement
    ));
    assert_eq!(sound(&s), before);
    assert_eq!(s.project().next_id, ids);
    history(&mut s, false);
    assert_eq!(sound(&s), after);
    p.process(&mut [0.0; 128], 2);
    let path = d.path().join("round.grooviq");
    let job = s.begin_save(Some(&path)).unwrap();
    let result = job.write(&d.path().join("backups"));
    s.end_save(&job, result).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(*loaded.samples[&3].bytes, wave());
    assert_eq!(
        loaded.project.arrangement.clips,
        s.project().arrangement.clips
    );
    assert_eq!(
        loaded.project.arrangement.audio_assets[0].source_rate,
        s.project().arrangement.audio_assets[0].source_rate
    );
    assert_eq!(
        loaded.project.arrangement.audio_assets[0].source_frames,
        s.project().arrangement.audio_assets[0].source_frames
    );
    let decoded = decode_samples(&loaded);
    s.open(&path, loaded, decoded).unwrap();
    assert_eq!(sound(&s), after);
}
#[test]
fn bad_last_operation_stamps_compiler_and_queue_refusal_keep_everything() {
    let (mut s, c, mut p, _) = rig();
    let view = s.view();
    let project = s.project().clone();
    let engine = c.lock().unwrap().state().clone();
    let stamp = s.arrangement_view();
    let usage = s.history_usage();
    for ops in [
        vec![draw(pattern(), 0), Op::DeleteClip { clip: 999 }],
        vec![draw(pattern(), MAX_SONG_TICKS)],
        vec![draw(pattern(), 0), Op::DeleteTrack { track: 2 }],
    ] {
        assert!(
            s.edit_arrangement(stamp.generation, stamp.revision, ops, Instant::now())
                .is_err()
        );
        assert_eq!(s.project(), &project);
        assert_eq!(s.view(), view);
        assert_eq!(s.history_usage(), usage);
        assert_eq!(c.lock().unwrap().state(), &engine);
    }
    assert!(
        s.edit_arrangement(stamp.generation + 1, stamp.revision, vec![], Instant::now())
            .is_err()
    );
    assert!(
        s.edit_arrangement(stamp.generation, stamp.revision + 1, vec![], Instant::now())
            .is_err()
    );
    s.set_song_note_limit(0);
    assert!(
        s.edit_arrangement(
            stamp.generation,
            stamp.revision,
            vec![draw(pattern(), 0)],
            Instant::now()
        )
        .is_err()
    );
    s.set_song_note_limit(10);
    while c
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    assert!(
        s.edit_arrangement(
            stamp.generation,
            stamp.revision,
            vec![draw(pattern(), 0)],
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &project);
    assert_eq!(s.history_usage(), usage);
    p.process(&mut [0.0; 128], 2);
    edit(&mut s, vec![draw(pattern(), 0)]);
    assert_eq!(s.history_usage().0, 1);
}
#[test]
fn musical_slice_source_phase_unique_and_priority_layers_are_stable() {
    let (mut s, _, _, _) = rig();
    let ids = edit(
        &mut s,
        vec![draw(pattern(), 0), draw(pattern(), 1920), draw(curve(), 0)],
    )
    .created_ids;
    let before = sound(&s);
    edit(
        &mut s,
        vec![Op::Slice {
            clip: ids[0],
            tick: 120,
            audio_first_frames: None,
        }],
    );
    assert_eq!(sound(&s), before);
    let right = s.project().arrangement.clips.last().unwrap().id;
    if let ClipContent::Pattern { offset_tick, .. } =
        s.project().arrangement.clips.last().unwrap().content
    {
        assert_eq!(offset_tick, 120);
    } else {
        panic!()
    }
    edit(&mut s, vec![Op::MakeUnique { clip: right }]);
    let p = s.project();
    let other = p.arrangement.clips.iter().find(|c| c.id == ids[1]).unwrap();
    assert!(matches!(
        other.content,
        ClipContent::Pattern { pattern: 0, .. }
    ));
    let copy = edit(
        &mut s,
        vec![Op::Copy {
            clip: ids[2],
            track: 2,
            start_tick: 0,
        }],
    )
    .created_ids[0];
    let priority = |s: &Session, id| {
        let c = s
            .project()
            .arrangement
            .clips
            .iter()
            .find(|c| c.id == id)
            .unwrap();
        if let ClipContent::Automation { priority, .. } = c.content {
            priority
        } else {
            panic!()
        }
    };
    assert!(priority(&s, copy) > priority(&s, ids[2]));
    let before = sound(&s);
    let split = edit(
        &mut s,
        vec![Op::Slice {
            clip: ids[2],
            tick: 480,
            audio_first_frames: None,
        }],
    )
    .created_ids[0];
    assert_eq!(priority(&s, split), priority(&s, ids[2]));
    assert_eq!(sound(&s), before);
    let v = s.arrangement_view();
    assert!(
        s.edit_arrangement(
            v.generation,
            v.revision,
            vec![Op::Move {
                clip: split,
                track: 2,
                start_tick: 0
            }],
            Instant::now()
        )
        .is_err()
    );
}
#[test]
fn reverse_audio_partition_and_fades_undo_and_redo_exact() {
    let (mut s, _, _, _) = rig();
    let clip = edit(&mut s, vec![draw(audio(true), 0)]).created_ids[0];
    let original = sound(&s);
    let right = edit(
        &mut s,
        vec![Op::Slice {
            clip,
            tick: 96,
            audio_first_frames: Some(2400),
        }],
    )
    .created_ids[0];
    let after = sound(&s);
    let a = &s.project().arrangement.clips;
    let first = a.iter().find(|c| c.id == clip).unwrap();
    let second = a.iter().find(|c| c.id == right).unwrap();
    assert!(matches!(
        first.content,
        ClipContent::Audio {
            source_start: 2400,
            source_end: 4800,
            fade_in: 100,
            fade_out: 0,
            ..
        }
    ));
    assert!(matches!(
        second.content,
        ClipContent::Audio {
            source_start: 0,
            source_end: 2400,
            fade_in: 0,
            fade_out: 200,
            ..
        }
    ));
    history(&mut s, true);
    assert_eq!(sound(&s), original);
    history(&mut s, false);
    assert_eq!(sound(&s), after);
}
#[test]
fn shared_curves_geometry_history_and_single_byte_step_refusal() {
    let (mut s, c, mut p, _) = rig();
    let mut content = curve();
    if let ClipContent::Automation { points, .. } = &mut content {
        *points = (0..16000)
            .map(|i| AutomationPoint {
                tick: i,
                value: 0.2,
                mode: SegmentMode::Hold,
            })
            .collect::<Vec<_>>()
            .into();
    }
    let clip = edit(&mut s, vec![draw(content, 0)]).created_ids[0];
    let points =
        if let ClipContent::Automation { points, .. } = &s.project().arrangement.clips[0].content {
            points.clone()
        } else {
            panic!()
        };
    let first = s.history_usage().2;
    for start in 1..12 {
        edit(
            &mut s,
            vec![Op::Move {
                clip,
                track: 2,
                start_tick: start,
            }],
        );
        p.process(&mut [0.0; 128], 2);
    }
    assert!(
        s.history_usage().2 < first + 30_000,
        "geometry must not duplicate curve payloads"
    );
    let current =
        if let ClipContent::Automation { points, .. } = &s.project().arrangement.clips[0].content {
            points
        } else {
            panic!()
        };
    assert!(points.shares_allocation(current));
    let before = s.project().clone();
    let view = s.view();
    let engine = c.lock().unwrap().state().clone();
    let usage = s.history_usage();
    s.set_history_limits(HistoryLimits {
        steps: 200,
        notes: 2_000_000,
        bytes: 1,
    });
    let v = s.arrangement_view();
    assert!(
        s.edit_arrangement(
            v.generation,
            v.revision,
            vec![Op::Move {
                clip,
                track: 2,
                start_tick: 100
            }],
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(s.history_usage(), usage);
    assert_eq!(c.lock().unwrap().state(), &engine);
}
#[test]
fn global_oldest_history_eviction_and_typed_pattern_identity() {
    let (mut s, _, mut p, _) = rig();
    s.set_history_limits(HistoryLimits {
        steps: 2,
        ..Default::default()
    });
    s.set_play_mode(PlayMode::Pattern, Instant::now()).unwrap();
    s.apply(
        Edit::SetStepVelocity {
            channel: 1,
            step: 1,
            velocity: 0.2,
        },
        Instant::now(),
    )
    .unwrap();
    edit(&mut s, vec![draw(pattern(), 0)]);
    s.apply(
        Edit::SetStepVelocity {
            channel: 1,
            step: 1,
            velocity: 0.3,
        },
        Instant::now(),
    )
    .unwrap();
    assert_eq!(s.history_usage().0, 2);
    assert!(matches!(
        history(&mut s, true).effect,
        HistoryEffect::Notes { pattern: 0 }
    ));
    assert!(matches!(
        history(&mut s, true).effect,
        HistoryEffect::Arrangement
    ));
    assert!(matches!(history(&mut s, true).effect, HistoryEffect::Empty));
    history(&mut s, false);
    history(&mut s, false);
    let v = s.patterns();
    s.edit_patterns(
        v.generation,
        v.revision,
        pattern_edit::PatternEdit::Create { name: "B".into() },
        Instant::now(),
    )
    .unwrap();
    let other = s.project().patterns[1].id;
    let v = s.patterns();
    s.edit_patterns(
        v.generation,
        v.revision,
        pattern_edit::PatternEdit::Select { pattern: other },
        Instant::now(),
    )
    .unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    let other_before = s.project().patterns[1].clone();
    history(&mut s, true);
    history(&mut s, true);
    assert_eq!(s.project().current_pattern, other);
    assert!(s.project().patterns[1].notes.is_empty());
    assert_ne!(s.project().patterns[1], other_before);
    p.process(&mut [0.0; 128], 2);
    s.new_project().unwrap();
    assert!(matches!(history(&mut s, true).effect, HistoryEffect::Empty));
}

#[test]
fn exact_history_byte_endpoint_and_eviction_plan_rollback() {
    let (mut s, c, mut processor, _) = rig();
    let id = edit(&mut s, vec![draw(curve(), 0)]).created_ids[0];
    let bytes = s.history_usage().2;
    let (mut exact, _, _, _) = rig();
    exact.set_history_limits(HistoryLimits {
        bytes,
        ..Default::default()
    });
    edit(&mut exact, vec![draw(curve(), 0)]);
    assert_eq!(exact.history_usage().2, bytes);
    let (mut too_small, _, _, _) = rig();
    too_small.set_history_limits(HistoryLimits {
        bytes: bytes - 1,
        ..Default::default()
    });
    let v = too_small.arrangement_view();
    assert!(
        too_small
            .edit_arrangement(
                v.generation,
                v.revision,
                vec![draw(curve(), 0)],
                Instant::now()
            )
            .is_err()
    );
    assert_eq!(too_small.history_usage().0, 0);
    edit(
        &mut s,
        vec![Op::Move {
            clip: id,
            track: 2,
            start_tick: 100,
        }],
    );
    history(&mut s, true);
    let before = s.project().clone();
    let usage = s.history_usage();
    s.set_history_limits(HistoryLimits {
        steps: 1,
        ..Default::default()
    });
    while c
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let v = s.arrangement_view();
    assert!(
        s.edit_arrangement(
            v.generation,
            v.revision,
            vec![Op::Move {
                clip: id,
                track: 2,
                start_tick: 200
            }],
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage(), usage);
    processor.process(&mut [0.0; 128], 2);
    history(&mut s, false);
    assert_eq!(s.project().arrangement.clips[0].start_tick, 100);
}
#[test]
fn note_single_step_limits_and_complete_take_refusal_leave_history_unchanged() {
    let (mut s, _, _, _) = rig();
    s.set_play_mode(PlayMode::Pattern, Instant::now()).unwrap();
    s.set_history_limits(HistoryLimits {
        notes: 1,
        ..Default::default()
    });
    let before = s.project().clone();
    let view = s.view();
    assert!(
        s.apply(
            Edit::SetStepVelocity {
                channel: 1,
                step: 1,
                velocity: 0.2
            },
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(s.history_usage().0, 0);
    s.set_history_limits(HistoryLimits {
        notes: 2,
        ..Default::default()
    });
    s.apply(
        Edit::SetStepVelocity {
            channel: 1,
            step: 1,
            velocity: 0.2,
        },
        Instant::now(),
    )
    .unwrap();
    assert_eq!(s.history_usage().1, 2);
}
#[test]
fn mute_resize_and_noop_keep_geometry_horizon_and_source_trim() {
    let (mut s, _, _, _) = rig();
    let ids = edit(
        &mut s,
        vec![
            draw(pattern(), 0),
            draw(audio(false), 100),
            draw(curve(), 0),
        ],
    )
    .created_ids;
    let horizon = s.export_state().song.end_tick;
    edit(
        &mut s,
        vec![Op::SetTrack {
            track: 2,
            name: "Muted".into(),
            color: 2,
            mute: true,
        }],
    );
    let state = s.export_state();
    assert_eq!(state.song.end_tick, horizon);
    assert!(state.song.audio.is_empty());
    assert!(sound(&s).iter().all(|v| *v == 0.0));
    let usage = s.history_usage();
    let revision = s.arrangement_view().revision;
    let a = edit(
        &mut s,
        vec![Op::SetTrack {
            track: 2,
            name: "Muted".into(),
            color: 2,
            mute: true,
        }],
    );
    assert!(!a.changed);
    assert_eq!(s.history_usage(), usage);
    assert_eq!(s.arrangement_view().revision, revision);
    edit(
        &mut s,
        vec![
            Op::Resize {
                clip: ids[0],
                start_tick: 300,
                content: ClipContent::Pattern {
                    pattern: 0,
                    offset_tick: 300,
                    length_ticks: 660,
                },
            },
            Op::Resize {
                clip: ids[1],
                start_tick: 100,
                content: ClipContent::Audio {
                    asset: 3,
                    source_start: 100,
                    source_end: 4000,
                    gain: 0.7,
                    reverse: false,
                    fade_in: 100,
                    fade_out: 200,
                },
            },
        ],
    );
    assert!(s.export_state().song.arrangement.notes.is_empty());
    let a = &s.project().arrangement.clips[1];
    assert!(matches!(
        a.content,
        ClipContent::Audio {
            source_start: 100,
            source_end: 4000,
            ..
        }
    ));
    let v = s.arrangement_view();
    assert!(
        s.edit_arrangement(
            v.generation,
            v.revision,
            vec![Op::Resize {
                clip: ids[1],
                start_tick: 100,
                content: ClipContent::Audio {
                    asset: 3,
                    source_start: 100,
                    source_end: 200,
                    gain: 0.7,
                    reverse: false,
                    fade_in: 101,
                    fade_out: 0
                }
            }],
            Instant::now()
        )
        .is_err()
    );
}

#[test]
fn create_track_draw_delete_and_canonical_operation_references() {
    let (mut s, _, _, _) = rig();
    let a = edit(
        &mut s,
        vec![
            Op::CreateTrack {
                name: "New".into(),
                color: 3,
            },
            Op::CreateClipOnCreatedTrack {
                track_operation: 0,
                name: "Linked".into(),
                start_tick: 0,
                muted: false,
                content: pattern(),
            },
        ],
    );
    let track = a.created_ids[0];
    let clip = a.created_ids[1];
    assert_eq!(s.project().arrangement.clips[0].track, track);
    edit(
        &mut s,
        vec![
            Op::SetClip {
                clip,
                name: "Renamed".into(),
                muted: true,
            },
            Op::SetTrack {
                track,
                name: "Updated".into(),
                color: 4,
                mute: true,
            },
        ],
    );
    edit(
        &mut s,
        vec![Op::DeleteClip { clip }, Op::DeleteTrack { track }],
    );
    assert!(s.project().arrangement.clips.is_empty());
    history(&mut s, true);
    assert_eq!(s.project().arrangement.clips[0].id, clip);
    assert_eq!(s.project().arrangement.tracks[1].id, track);
}
#[test]
fn history_shared_curve_distinct_allocations_and_redo_budget() {
    let (mut s, _, _, _) = rig();
    let a = edit(&mut s, vec![draw(curve(), 0)]).created_ids[0];
    let baseline = s.history_usage().2;
    let b = edit(
        &mut s,
        vec![Op::Copy {
            clip: a,
            track: 2,
            start_tick: 0,
        }],
    )
    .created_ids[0];
    let shared = s.history_usage().2;
    let c = edit(&mut s, vec![draw(curve(), 0)]).created_ids[0];
    let distinct = s.history_usage().2;
    assert!(distinct > shared);
    assert!(shared > baseline);
    let point = |id| {
        let clip = s
            .project()
            .arrangement
            .clips
            .iter()
            .find(|c| c.id == id)
            .unwrap();
        if let ClipContent::Automation { points, .. } = &clip.content {
            points.clone()
        } else {
            panic!()
        }
    };
    assert!(point(a).shares_allocation(&point(b)));
    assert!(!point(a).shares_allocation(&point(c)));
    history(&mut s, true);
    let redo = s.history_usage();
    assert_eq!(redo.0, 3);
    assert!(redo.2 >= baseline);
    edit(
        &mut s,
        vec![Op::Move {
            clip: a,
            track: 2,
            start_tick: 10,
        }],
    );
    assert!(matches!(
        history(&mut s, false).effect,
        HistoryEffect::Empty
    ));
}

#[test]
fn oversize_input_file_and_track_cap_refuse_before_any_publication() {
    let (mut s, c, _, _) = rig();
    let before = s.project().clone();
    let view = s.view();
    let engine = c.lock().unwrap().state().clone();
    let mut ops = vec![draw(pattern(), 0)];
    ops.extend((0..256).map(|_| Op::CreateTrack {
        name: "T".into(),
        color: 0,
    }));
    let v = s.arrangement_view();
    assert!(
        s.edit_arrangement(v.generation, v.revision, ops, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(c.lock().unwrap().state(), &engine);
    // Repeated shared curves exceed the exact serialized writer cap while retained
    // curve bytes remain small. This proves file admission independently of history.
    let points: CurvePoints = (0..16000)
        .map(|i| AutomationPoint {
            tick: i,
            value: 0.2,
            mode: SegmentMode::Hold,
        })
        .collect::<Vec<_>>()
        .into();
    let ops = (0..62)
        .map(|_| {
            draw(
                ClipContent::Automation {
                    target: AutomationTarget::ChannelPan { channel: 1 },
                    priority: 0,
                    offset_tick: 0,
                    length_ticks: 960,
                    points: points.clone(),
                },
                0,
            )
        })
        .chain(std::iter::once(Op::CreateTrack {
            name: "x".repeat(40 * 1024 * 1024),
            color: 0,
        }))
        .collect();
    let v = s.arrangement_view();
    let refused = s
        .edit_arrangement(v.generation, v.revision, ops, Instant::now())
        .unwrap_err();
    assert!(
        refused.message.contains("64 MB a project file"),
        "{}",
        refused.message
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage().0, 0);
}

#[test]
fn production_session_maximum_geometry_shape_retains_one_curve_allocation() {
    let (mut s, _, mut processor, d) = rig();
    let mut project = s.project().clone();
    project.arrangement.tracks[0].mute = true;
    let points: CurvePoints = (0..16000)
        .map(|tick| AutomationPoint {
            tick,
            value: 0.5,
            mode: SegmentMode::Hold,
        })
        .collect::<Vec<_>>()
        .into();
    for i in 0..100000 {
        project.arrangement.clips.push(Clip {
            id: 100 + i,
            track: 2,
            name: String::new(),
            start_tick: i as u64,
            muted: false,
            content: if i < 62 {
                ClipContent::Automation {
                    target: AutomationTarget::ChannelPan { channel: 1 },
                    priority: 200000 + i,
                    offset_tick: 0,
                    length_ticks: 1,
                    points: points.clone(),
                }
            } else {
                ClipContent::Pattern {
                    pattern: 0,
                    offset_tick: 0,
                    length_ticks: 1,
                }
            },
        });
    }
    project.next_id = 500000;
    let loaded = Loaded {
        project,
        samples: HashMap::from([(
            3,
            SampleFile {
                name: "tone.wav".into(),
                bytes: Arc::new(wave()),
            },
        )]),
        warnings: vec![],
    };
    let decoded = decode_samples(&loaded);
    s.open(&d.path().join("scale.grooviq"), loaded, decoded)
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    let begin = std::time::Instant::now();
    edit(
        &mut s,
        vec![Op::Move {
            clip: 100,
            track: 2,
            start_tick: 1,
        }],
    );
    let elapsed = begin.elapsed();
    assert_eq!(s.project().arrangement.clips.len(), 100000);
    assert_eq!(s.history_usage().0, 1);
    assert!(s.history_usage().2 < 400000);
    let ClipContent::Automation {
        points: current, ..
    } = &s.project().arrangement.clips[0].content
    else {
        panic!()
    };
    assert!(current.shares_allocation(&points));
    eprintln!(
        "production Session max-shape move: 100000 clips / 992000 points, {:?}, retained {} bytes",
        elapsed,
        s.history_usage().2
    );
}

#[test]
fn undo_created_selected_pattern_preserves_legacy_target_and_monotonic_note_identity() {
    let (mut s, _, _, _) = rig();
    let other = edit(
        &mut s,
        vec![Op::CreatePattern {
            name: "Other".into(),
        }],
    )
    .created_ids[0];
    let v = s.patterns();
    s.edit_patterns(
        v.generation,
        v.revision,
        pattern_edit::PatternEdit::Select { pattern: other },
        Instant::now(),
    )
    .unwrap();
    s.set_play_mode(PlayMode::Pattern, Instant::now()).unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    let floor = s
        .project()
        .patterns
        .iter()
        .find(|p| p.id == other)
        .unwrap()
        .next_note_id;
    history(&mut s, true);
    history(&mut s, true);
    assert_eq!(s.project().current_pattern, 0);
    assert!(s.undo_notes(Instant::now()).unwrap().is_none());
    history(&mut s, false);
    history(&mut s, false);
    assert_eq!(s.project().current_pattern, 0);
    let restored = s.project().patterns.iter().find(|p| p.id == other).unwrap();
    assert!(restored.next_note_id >= floor);
    assert_eq!(restored.notes.len(), 1);
    assert_eq!(s.project().patterns[0].notes.len(), 1);
}

#[test]
fn oversized_recorded_take_is_retained_whole_until_history_budget_repair() {
    use e::{ChannelId, NoteId, RecordEvent};
    let (mut s, c, mut processor, _) = rig();
    s.set_play_mode(PlayMode::Pattern, Instant::now()).unwrap();
    processor.process(&mut [0.0; 128], 2);
    let mut marks = vec![];
    c.lock().unwrap().take_recorded(&mut marks);
    s.collect_recorded(&marks, Instant::now());
    s.set_history_limits(HistoryLimits {
        notes: 1,
        ..Default::default()
    });
    let events = |id, key| {
        [
            RecordEvent::Note {
                channel: ChannelId(1),
                key,
                velocity: 0.8,
                on: true,
                tick: 0,
                position: 0,
                take: 55,
                id: NoteId(id),
            },
            RecordEvent::Note {
                channel: ChannelId(1),
                key,
                velocity: 0.8,
                on: false,
                tick: 240,
                position: 0,
                take: 55,
                id: NoteId(id),
            },
        ]
    };
    s.collect_recorded(&events(100, 70), Instant::now());
    assert_eq!(s.history_usage().1, 1);
    let before = s.project().clone();
    let usage = s.history_usage();
    s.collect_recorded(&events(101, 72), Instant::now());
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage(), usage);
    assert!(s.settle_recording().is_err());
    s.set_history_limits(HistoryLimits {
        notes: 2,
        ..Default::default()
    });
    s.collect_recorded(&[], Instant::now());
    assert_eq!(s.history_usage().0, 1);
    assert_eq!(s.history_usage().1, 2);
    s.undo_notes(Instant::now()).unwrap();
    assert_eq!(s.project().patterns[0].notes.len(), 1);
}

#[test]
fn arrangement_undo_full_queue_keeps_cursor_revision_and_redo_exact() {
    let (mut s, c, mut processor, _) = rig();
    let id = edit(&mut s, vec![draw(audio(false), 0)]).created_ids[0];
    edit(
        &mut s,
        vec![Op::Move {
            clip: id,
            track: 2,
            start_tick: 100,
        }],
    );
    let before = s.project().clone();
    let view = s.view();
    let usage = s.history_usage();
    while c
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let engine = c.lock().unwrap().state().clone();
    let v = s.arrangement_view();
    assert!(
        s.step_project_history(v.generation, v.revision, true, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(s.history_usage(), usage);
    assert_eq!(c.lock().unwrap().state(), &engine);
    processor.process(&mut [0.0; 128], 2);
    history(&mut s, true);
    assert_eq!(s.project().arrangement.clips[0].start_tick, 0);
    history(&mut s, false);
    assert_eq!(s.project().arrangement.clips[0].start_tick, 100);
}
#[test]
fn byte_limit_evicts_old_complete_large_curve_step_but_preserves_new_small_gesture() {
    let (mut s, _, _, _) = rig();
    let mut content = curve();
    if let ClipContent::Automation { points, .. } = &mut content {
        *points = (0..16000)
            .map(|tick| AutomationPoint {
                tick,
                value: 0.3,
                mode: SegmentMode::Hold,
            })
            .collect::<Vec<_>>()
            .into();
    }
    edit(&mut s, vec![draw(content, 0)]);
    let large = s.history_usage().2;
    s.set_history_limits(HistoryLimits {
        bytes: 10_000,
        ..Default::default()
    });
    edit(
        &mut s,
        vec![Op::CreateTrack {
            name: "Small".into(),
            color: 0,
        }],
    );
    assert_eq!(s.history_usage().0, 1);
    assert!(s.history_usage().2 < large);
    assert!(s.history_usage().2 <= 10_000);
    history(&mut s, true);
    assert_eq!(s.project().arrangement.clips.len(), 1);
    assert_eq!(s.project().arrangement.tracks.len(), 1);
}
