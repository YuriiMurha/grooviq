use grooviq_engine::{self as engine, mixer as live};
use grooviq_project::{
    AudioAsset, Clip, ClipContent, Loaded, PlayMode, PlaylistTrack, Project, SampleFile, SampleRef,
    arrangement_edit::ArrangementEdit,
    engine_state,
    mixer::{Assignment, Level, Mixer, Mode, SourceKey, Track},
    mixer_edit::{CreatedId, IdRef, MixerEdit as Op},
    session::Session,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

#[test]
fn routed_state_keeps_source_namespaces_and_expands_master_defaults() {
    let mut project = Project::new();
    for id in [7, 8] {
        project.channels.push(
            serde_json::from_value(serde_json::json!({
                "id": id, "name": "Synth", "color": 0, "instrument": "synth"
            }))
            .unwrap(),
        );
    }
    for id in [7, 9] {
        project
            .arrangement
            .tracks
            .push(grooviq_project::PlaylistTrack {
                id,
                name: "Playlist".into(),
                color: 0,
                mute: false,
            });
    }
    project.mixer = Mixer {
        mode: Mode::Routed,
        tracks: vec![
            Track::master(),
            Track {
                id: 1,
                name: "Bus".into(),
                output: Some(0),
                level: Level::Db(-6.0),
                pan: -1.0,
                ..Track::master()
            },
        ],
        sources: vec![Assignment {
            source: SourceKey::Channel(7),
            destination: 1,
        }],
        next_track_id: 2,
        ..Mixer::default()
    };
    let state = engine_state(&project, &HashMap::new()).unwrap();
    let live::MixSetup::Routed(mix) = state.mix else {
        panic!("routed input rendered in LegacyDirect");
    };
    let routes: HashMap<_, _> = mix
        .mixer
        .sources
        .iter()
        .map(|s| (s.id, s.destination))
        .collect();
    assert_eq!(routes[&live::SourceId::Channel(7)], live::MixerTrackId(1));
    assert_eq!(routes[&live::SourceId::Channel(8)], live::MASTER);
    assert_eq!(routes[&live::SourceId::AudioTrack(7)], live::MASTER);
    assert_eq!(routes[&live::SourceId::AudioTrack(9)], live::MASTER);
    let fader = mix.gains.iter().find(|g| g.track.0 == 1).unwrap();
    assert!((fader.left - 0.708_785_8).abs() < 1e-6);
    assert_eq!(fader.right, 0.0);
    engine::render_offline_with(
        engine::EngineConfig {
            sample_rate: 48000.0,
        },
        engine_state(&project, &HashMap::new()).unwrap(),
        &[],
        64,
        32,
    );
}

#[test]
fn one_slot_publishes_new_playlist_source_with_its_mixer_route() {
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut session = Session::new(c.clone());
    let mut loaded = audio_fixture(false);
    loaded.project.mixer.mode = Mode::Routed;
    loaded.project.mixer.tracks[0].level = Level::Db(-3.0);
    let decoded = grooviq_project::decode_samples(&loaded);
    session
        .open("routed.grooviq".as_ref(), loaded, decoded)
        .unwrap();
    processor.process(&mut [0.0; 128], 2);

    let stamp = session.arrangement_view();
    let edit = |s: &mut Session| {
        s.edit_arrangement(
            stamp.generation,
            stamp.revision,
            vec![
                ArrangementEdit::CreateTrack {
                    name: "Audio".into(),
                    color: 0,
                },
                ArrangementEdit::CreateClipOnCreatedTrack {
                    track_operation: 0,
                    name: "Recording".into(),
                    start_tick: 0,
                    muted: false,
                    content: audio_content(),
                },
            ],
            Instant::now(),
        )
    };
    let mut fill = 0;
    while c.lock().unwrap().send(engine::Command::Stop).is_ok() {
        fill += 1;
    }
    let before = (
        session.project().clone(),
        session.view(),
        session.history_usage(),
        c.lock().unwrap().state().clone(),
    );
    assert!(edit(&mut session).is_err());
    assert_eq!(session.project(), &before.0);
    assert_eq!(session.view(), before.1);
    assert_eq!(session.history_usage(), before.2);
    assert_eq!(c.lock().unwrap().state(), &before.3);

    processor.process(&mut [0.0; 128], 2);
    for _ in 0..fill - 1 {
        c.lock().unwrap().send(engine::Command::Stop).unwrap();
    }
    let ids = edit(&mut session).unwrap().created_ids;
    let [track, clip] = ids[..] else {
        panic!("expected a track and a clip")
    };
    let mut ctl = c.lock().unwrap();
    assert!(ctl.send(engine::Command::Stop).is_err());
    let live::MixSetup::Routed(mix) = &ctl.state().mix else {
        panic!("dry mix")
    };
    assert!(mix.mixer.sources.contains(&live::Source {
        id: live::SourceId::AudioTrack(track.into()),
        destination: live::MASTER,
    }));
    assert_eq!(ctl.state().song.audio.tracks(), vec![(clip, track.into())]);
    assert_eq!(session.history_usage().0, 1);
}

#[test]
fn mixer_admission_keeps_ids_and_shared_history_on_refusal_and_undo() {
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut session = Session::new(c.clone());
    session.add_synth_channel("Init", Instant::now()).unwrap();
    let channel = session.project().channels[0].id;
    processor.process(&mut [0.0; 128], 2);
    let mix_revision = c.lock().unwrap().mix_revision();
    let start = session.mixer_view();
    let start_view = session.view();
    let create = || {
        vec![
            Op::CreateTrack {
                name: "Bus".into(),
                output: IdRef::Id(0),
            },
            Op::SetFader {
                track: IdRef::Created(0),
                level: Level::Silence,
                pan: 0.0,
            },
        ]
    };
    let now = Instant::now();
    while c.lock().unwrap().send(engine::Command::Stop).is_ok() {}
    assert!(
        session
            .edit_mixer(start.generation, start.revision, create(), now)
            .is_err()
    );
    assert_eq!(session.mixer_view(), start);
    assert_eq!(session.history_usage().0, 0);
    assert_eq!(session.view(), start_view);
    assert_eq!(c.lock().unwrap().mix_revision(), mix_revision);
    processor.process(&mut [0.0; 128], 2);

    let first = session
        .edit_mixer(start.generation, start.revision, create(), now)
        .unwrap();
    assert_eq!(first.created_ids, vec![(0, CreatedId::Track(1))]);
    assert!(
        session
            .edit_mixer(start.generation, start.revision, create(), now)
            .is_err()
    );
    let arrangement = session
        .edit_arrangement(
            first.state.generation,
            first.state.revision,
            vec![ArrangementEdit::CreateTrack {
                name: "Playlist".into(),
                color: 0,
            }],
            now,
        )
        .unwrap();
    assert_eq!(arrangement.state.arrangement.tracks.len(), 1);
    session
        .apply(
            grooviq_project::session::Edit::ToggleStep { channel, step: 0 },
            now,
        )
        .unwrap();
    assert_eq!(session.project().patterns[0].notes.len(), 1);
    assert!(matches!(
        history(&mut session, true),
        grooviq_project::arrangement_edit::HistoryEffect::Notes { .. }
    ));
    assert!(session.project().patterns[0].notes.is_empty());
    assert_eq!(session.project().arrangement.tracks.len(), 1);
    history(&mut session, true);
    assert!(session.project().arrangement.tracks.is_empty());
    assert_eq!(session.mixer_view().mixer.tracks.len(), 2);

    let stamp = session.mixer_view();
    while c.lock().unwrap().send(engine::Command::Stop).is_ok() {}
    assert!(
        session
            .step_project_history(stamp.generation, stamp.revision, true, now)
            .is_err()
    );
    assert_eq!(session.mixer_view(), stamp);
    processor.process(&mut [0.0; 128], 2);
    session
        .step_project_history(stamp.generation, stamp.revision, true, now)
        .unwrap();
    assert_eq!(session.mixer_view().mixer.mode, Mode::LegacyDirect);
    let stamp = session.mixer_view();
    assert_eq!(stamp.mixer.next_track_id, 2);
    let branch = session
        .edit_mixer(stamp.generation, stamp.revision, create(), now)
        .unwrap();
    assert_eq!(branch.created_ids, vec![(0, CreatedId::Track(2))]);
}

const FRAMES: usize = 1024;

fn pcm_sample(frame: usize) -> i16 {
    (frame as i32 * 97 % 12001 - 6000) as i16
}

fn audio_content() -> ClipContent {
    ClipContent::Audio {
        asset: 4,
        source_start: 0,
        source_end: FRAMES as u64,
        gain: 1.0,
        reverse: false,
        fade_in: 0,
        fade_out: 0,
    }
}

fn audio_fixture(with_clip: bool) -> Loaded {
    let mut bytes = Vec::new();
    let payload = (FRAMES * 2) as u32;
    bytes.extend(b"RIFF");
    bytes.extend((36 + payload).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48_000_u32.to_le_bytes());
    bytes.extend(96_000_u32.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(payload.to_le_bytes());
    for frame in 0..FRAMES {
        bytes.extend(pcm_sample(frame).to_le_bytes());
    }
    let mut project = Project::new();
    project.play_mode = PlayMode::Song;
    project.bpm = 120.0;
    project.next_id = 6;
    for id in [2, 3] {
        project.arrangement.tracks.push(PlaylistTrack {
            id,
            name: format!("Track {id}"),
            color: 0,
            mute: false,
        });
    }
    project.arrangement.audio_assets.push(AudioAsset {
        id: 4,
        sample: SampleRef {
            path: "generated.wav".into(),
            file: None,
        },
        source_rate: 48_000,
        source_frames: FRAMES as u64,
        original_tempo: None,
        original_key: None,
    });
    if with_clip {
        project.arrangement.clips.push(Clip {
            id: 5,
            track: 2,
            name: "Generated".into(),
            start_tick: 0,
            muted: false,
            content: audio_content(),
        });
    }
    let samples = HashMap::from([(
        4,
        SampleFile {
            name: "generated.wav".into(),
            bytes: Arc::new(bytes),
        },
    )]);
    Loaded {
        project,
        samples,
        warnings: Vec::new(),
    }
}

fn sound(session: &Session) -> Vec<f32> {
    engine::render_offline_with(
        engine::EngineConfig {
            sample_rate: 48_000.0,
        },
        session.checked_export_state().unwrap(),
        &[(0, engine::Command::Play)],
        FRAMES,
        64,
    )
}

fn mixer(session: &mut Session, ops: Vec<Op>) -> grooviq_project::mixer_edit::MixerAck {
    let v = session.mixer_view();
    session
        .edit_mixer(v.generation, v.revision, ops, Instant::now())
        .unwrap()
}

fn arrangement(
    session: &mut Session,
    ops: Vec<ArrangementEdit>,
) -> grooviq_project::arrangement_edit::ArrangementAck {
    let v = session.arrangement_view();
    session
        .edit_arrangement(v.generation, v.revision, ops, Instant::now())
        .unwrap()
}

fn history(session: &mut Session, undo: bool) -> grooviq_project::arrangement_edit::HistoryEffect {
    let v = session.arrangement_view();
    session
        .step_project_history(v.generation, v.revision, undo, Instant::now())
        .unwrap()
        .effect
}

#[test]
fn an_audio_track_move_keeps_its_bank_and_follows_the_new_route() {
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut session = Session::new(c);
    let loaded = audio_fixture(true);
    let decoded = grooviq_project::decode_samples(&loaded);
    session
        .open("initial.grooviq".as_ref(), loaded, decoded)
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    let take_bus = vec![
        Op::CreateTrack {
            name: "Bus".into(),
            output: IdRef::Id(0),
        },
        Op::Assign {
            source: SourceKey::AudioTrack(3),
            destination: IdRef::Created(0),
        },
        Op::SetFader {
            track: IdRef::Created(0),
            level: Level::Db(-6.0),
            pan: -1.0,
        },
    ];
    let dry = sound(&session);
    mixer(&mut session, take_bus);
    assert_eq!(sound(&session), dry, "track 2 still belongs to master");
    session.play().unwrap();
    let mut prefix = [0.0_f32; 256];
    processor.process(&mut prefix, 2);
    assert_eq!(prefix.as_slice(), &dry[..prefix.len()]);
    let bank = session.checked_export_state().unwrap().song.audio;
    arrangement(
        &mut session,
        vec![ArrangementEdit::Move {
            clip: 5,
            track: 3,
            start_tick: 0,
        }],
    );
    let moved = session.checked_export_state().unwrap().song.audio;
    assert!(bank.shares_bank(&moved));
    assert_eq!(moved.tracks(), vec![(5, 3)]);
    let routed = sound(&session);
    for (frame, samples) in routed.as_chunks::<2>().0.iter().enumerate() {
        let expected = f32::from(pcm_sample(frame)) / 32_768.0 * 0.708_785_8;
        assert!((samples[0] - expected).abs() < 1e-6, "frame {frame}");
        assert_eq!(samples[1], 0.0, "frame {frame}");
    }

    let mut suffix = [0.0_f32; 256];
    processor.process(&mut suffix, 2);
    assert_eq!(
        suffix.as_slice(),
        &routed[prefix.len()..prefix.len() + suffix.len()]
    );

    history(&mut session, true);
    assert_eq!(sound(&session), dry);
    arrangement(
        &mut session,
        vec![ArrangementEdit::DeleteTrack { track: 3 }],
    );
    assert!(session.project().mixer.sources.is_empty());
    history(&mut session, true);
    assert_eq!(
        session
            .project()
            .mixer
            .destination(SourceKey::AudioTrack(3)),
        1
    );
    history(&mut session, false);
    assert!(session.project().mixer.sources.is_empty());
}

#[test]
fn removing_a_channel_preserves_other_notes_redo_and_the_bus_inverse() {
    use grooviq_project::{
        notes::{NewNote, NotesEdit},
        session::Edit,
    };
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut session = Session::new(c.clone());
    let now = Instant::now();
    session.add_synth_channel("Init", now).unwrap();
    session.add_synth_channel("Init", now).unwrap();
    let [removed, kept] = [
        session.project().channels[0].id,
        session.project().channels[1].id,
    ];
    mixer(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Voice bus".into(),
                output: IdRef::Id(0),
            },
            Op::Assign {
                source: SourceKey::Channel(removed),
                destination: IdRef::Created(0),
            },
        ],
    );
    for tick in [0, 240] {
        session
            .edit_notes(
                NotesEdit {
                    generation: session.generation(),
                    pattern_id: session.project().current_pattern,
                    base_revision: session.notes_revision(),
                    add: [removed, kept]
                        .map(|channel| NewNote {
                            channel,
                            tick,
                            length: 120,
                            key: 60,
                            velocity: 0.5,
                            slide: false,
                        })
                        .to_vec(),
                    update: Vec::new(),
                    delete: Vec::new(),
                },
                now,
            )
            .unwrap();
    }
    history(&mut session, true);
    processor.process(&mut [0.0; 128], 2);
    while c.lock().unwrap().send(engine::Command::Stop).is_ok() {}
    let before = (
        session.project().clone(),
        session.mixer_view(),
        session.history_usage(),
    );
    let remove = Edit::RemoveChannel { channel: removed };
    assert!(session.apply(remove.clone(), now).is_err());
    assert_eq!(session.project(), &before.0);
    assert_eq!(session.mixer_view(), before.1);
    assert_eq!(session.history_usage(), before.2);
    processor.process(&mut [0.0; 128], 2);
    session.apply(remove, now).unwrap();
    assert!(session.project().mixer.sources.is_empty());

    let notes = |s: &Session| {
        s.project().patterns[0]
            .notes
            .iter()
            .map(|n| (n.channel, n.tick))
            .collect::<Vec<_>>()
    };
    assert_eq!(notes(&session), [(kept, 0)]);
    history(&mut session, false);
    assert_eq!(notes(&session), [(kept, 0), (kept, 240)]);
    history(&mut session, true);
    history(&mut session, true);
    assert!(notes(&session).is_empty());

    let v = session.mixer_view();
    let error = session
        .step_project_history(v.generation, v.revision, true, now)
        .unwrap_err();
    assert!(error.message.contains("fading"), "{}", error.message);
    assert_eq!(session.mixer_view(), v);
    processor.process(&mut [0.0; 2048], 2);
    c.lock().unwrap().check_health();
    history(&mut session, true);
    assert_eq!(session.project().mixer.mode, Mode::LegacyDirect);
    assert_eq!(session.project().channels.len(), 1);
    assert_eq!(session.project().channels[0].id, kept);
}

#[test]
fn a_held_instrument_survives_routing_and_another_source_addition() {
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut session = Session::new(c.clone());
    let now = Instant::now();
    session.add_synth_channel("Init", now).unwrap();
    let channel = session.project().channels[0].id;
    let instance = c
        .lock()
        .unwrap()
        .channel_instance(engine::ChannelId(channel));
    session.live_note_on(channel, 60, 0.5).unwrap();
    let mut before = [0.0_f32; 1024];
    processor.process(&mut before, 2);
    assert!(before.iter().any(|v| v.abs() > 0.001));
    mixer(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Voice bus".into(),
                output: IdRef::Id(0),
            },
            Op::Assign {
                source: SourceKey::Channel(channel),
                destination: IdRef::Created(0),
            },
            Op::SetFader {
                track: IdRef::Created(0),
                level: Level::Db(0.0),
                pan: 1.0,
            },
        ],
    );
    session.add_synth_channel("Init", now).unwrap();
    let new = session.project().channels[1].id;
    {
        let ctl = c.lock().unwrap();
        assert_eq!(ctl.channel_instance(engine::ChannelId(channel)), instance);
        let live::MixSetup::Routed(mix) = &ctl.state().mix else {
            panic!("dry mix")
        };
        assert!(mix.mixer.sources.contains(&live::Source {
            id: live::SourceId::Channel(new.into()),
            destination: live::MASTER,
        }));
    }
    let mut after = [0.0_f32; 1024];
    processor.process(&mut after, 2);
    assert!(
        after
            .as_chunks::<2>()
            .0
            .iter()
            .any(|frame| frame[1].abs() > 0.001)
    );
    assert!(after.as_chunks::<2>().0.iter().all(|frame| frame[0] == 0.0));
    assert!(
        session.project().patterns[0].notes.is_empty(),
        "a live voice supplied the sound"
    );
}

#[test]
fn promotion_after_legacy_removal_waits_for_fade_collection() {
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let ctl = Arc::new(Mutex::new(c));
    let mut session = Session::new(ctl.clone());
    let now = Instant::now();
    session.add_synth_channel("Init", now).unwrap();
    let channel = session.project().channels[0].id;
    processor.process(&mut [0.0_f32; 128], 2);
    session
        .apply(
            grooviq_project::session::Edit::RemoveChannel { channel },
            now,
        )
        .unwrap();
    let create = || {
        vec![Op::CreateTrack {
            name: "Bus".into(),
            output: IdRef::Id(0),
        }]
    };
    let before = (
        session.mixer_view(),
        session.history_usage(),
        ctl.lock().unwrap().state().clone(),
    );
    let err = session
        .edit_mixer(before.0.generation, before.0.revision, create(), now)
        .unwrap_err();
    assert!(matches!(
        err,
        grooviq_project::session::SessionError::Engine(engine::ControlError::Busy)
    ));
    assert_eq!(
        (
            session.mixer_view(),
            session.history_usage(),
            ctl.lock().unwrap().state().clone()
        ),
        before
    );
    processor.process(&mut [0.0_f32; 2048], 2);
    ctl.lock().unwrap().check_health();
    let ack = session
        .edit_mixer(before.0.generation, before.0.revision, create(), now)
        .unwrap();
    assert_eq!(ack.created_ids, [(0, CreatedId::Track(1))]);
}

#[test]
fn structural_pattern_edit_clears_mixer_undo_and_redo_without_changing_the_mix() {
    use grooviq_project::{arrangement_edit::HistoryEffect, pattern_edit::PatternEdit};
    for make_unique in [false, true] {
        let (c, _p) = engine::engine(engine::EngineConfig {
            sample_rate: 48_000.0,
        });
        let mut session = Session::new(Arc::new(Mutex::new(c)));
        let loaded = audio_fixture(false);
        let decoded = grooviq_project::decode_samples(&loaded);
        session
            .open("initial.grooviq".as_ref(), loaded, decoded)
            .unwrap();
        let edit = |s: &mut Session, edit| {
            let v = s.patterns();
            s.edit_patterns(v.generation, v.revision, edit, Instant::now())
                .unwrap()
        };
        let structural = if make_unique {
            let ack = arrangement(
                &mut session,
                vec![ArrangementEdit::CreateClip {
                    track: 2,
                    name: "Pattern".into(),
                    start_tick: 0,
                    muted: false,
                    content: ClipContent::Pattern {
                        pattern: 0,
                        offset_tick: 0,
                        length_ticks: 960,
                    },
                }],
            );
            PatternEdit::MakeUnique {
                clip: ack.created_ids[0],
            }
        } else {
            let v = edit(
                &mut session,
                PatternEdit::Create {
                    name: "Spare".into(),
                },
            );
            PatternEdit::Delete {
                pattern: v.patterns[1].id,
            }
        };
        mixer(
            &mut session,
            vec![Op::CreateTrack {
                name: "Bus".into(),
                output: IdRef::Id(0),
            }],
        );
        mixer(
            &mut session,
            vec![Op::SetFader {
                track: IdRef::Id(1),
                level: Level::Silence,
                pan: 1.0,
            }],
        );
        history(&mut session, true);
        let before = session.project().mixer.clone();
        edit(&mut session, structural);
        assert!(matches!(history(&mut session, true), HistoryEffect::Empty));
        assert!(matches!(history(&mut session, false), HistoryEffect::Empty));
        assert_eq!(session.project().mixer, before);
    }
}

#[test]
fn mixer_mute_and_solo_each_refuse_compile_and_save() {
    for (mute, solo) in [(true, false), (false, true)] {
        let mut project = Project::new();
        project.mixer.mode = Mode::Routed;
        project.mixer.tracks[0].mute = mute;
        project.mixer.tracks[0].solo = solo;
        let samples = HashMap::new();
        assert!(matches!(
            engine_state(&project, &samples),
            Err(grooviq_project::mixer::MixerError::UnsupportedTrackControls(0))
        ));
        assert!(grooviq_project::validate_save(&project, &HashMap::new()).is_err());
    }
}
