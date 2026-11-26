use grooviq_engine::{self as engine, mixer as live};
use grooviq_project::{
    AudioAsset, Clip, ClipContent, Loaded, PlayMode, PlaylistTrack, Project, SampleFile, SampleRef,
    arrangement_edit::ArrangementEdit,
    engine_state,
    mixer::{Assignment, Level, Mixer, Mode, Send, SourceKey, Tap, Track},
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
        sends: vec![
            Send {
                id: 1,
                from: 1,
                to: 0,
                tap: Tap::Pre,
                gain: 0.5,
            },
            Send {
                id: 2,
                from: 1,
                to: 0,
                tap: Tap::Post,
                gain: 0.25,
            },
        ],
        next_track_id: 2,
        next_send_id: 3,
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
    assert_eq!(
        mix.mixer.sends,
        [
            live::Send {
                id: live::SendId(1),
                from: live::MixerTrackId(1),
                to: live::MASTER,
                tap: live::Tap::PreFader,
                gain: 0.5,
            },
            live::Send {
                id: live::SendId(2),
                from: live::MixerTrackId(1),
                to: live::MASTER,
                tap: live::Tap::PostFader,
                gain: 0.25,
            },
        ]
    );

    let note_id = engine::NoteId::live();
    let render = |project: &Project| {
        engine::render_offline_with(
            engine::EngineConfig {
                sample_rate: 48_000.0,
            },
            engine_state(project, &HashMap::new()).unwrap(),
            &[(
                0,
                engine::Command::ChannelNote {
                    channel: engine::ChannelId(7),
                    key: 60,
                    velocity: 0.5,
                    on: true,
                    id: note_id,
                },
            )],
            512,
            64,
        )
    };
    let with_both = render(&project);
    project.mixer.sends.pop();
    let with_pre = render(&project);
    project.mixer.sends.clear();
    let dry = render(&project);
    let pre = with_pre.as_chunks::<2>().0;
    let both = with_both.as_chunks::<2>().0;
    assert!(pre.iter().any(|f| f[1].abs() > 0.001));
    assert!(dry.as_chunks::<2>().0.iter().all(|f| f[1] == 0.0));
    assert!(
        both.iter()
            .zip(pre)
            .any(|(both, pre)| (both[0] - pre[0]).abs() > 0.001)
    );
    assert!(both.iter().zip(pre).all(|(both, pre)| both[1] == pre[1]));
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
fn mixer_history_budget_refuses_a_large_track_delta_before_engine_admission() {
    let make = || {
        let (c, p) = engine::engine(engine::EngineConfig {
            sample_rate: 48_000.0,
        });
        let c = Arc::new(Mutex::new(c));
        (Session::new(c.clone()), c, p)
    };
    let tracks = |name: &str| {
        (0..12)
            .map(|_| Op::CreateTrack {
                name: name.into(),
                output: IdRef::Id(0),
            })
            .collect()
    };

    // Both gestures retain the same number of tracks and deltas. Only the nested names differ.
    let (mut small, _, _small_processor) = make();
    mixer(&mut small, tracks("Bus"));
    let budget = small.history_usage().2;

    let (mut session, ctl, _processor) = make();
    session.set_history_limits(grooviq_project::HistoryLimits {
        bytes: budget,
        ..Default::default()
    });
    let stamp = session.mixer_view();
    let before = (
        session.project().clone(),
        session.history_usage(),
        session.view(),
    );
    let engine_before = {
        let ctl = ctl.lock().unwrap();
        (ctl.mix_revision(), ctl.state().clone())
    };
    let message = session
        .edit_mixer(
            stamp.generation,
            stamp.revision,
            tracks(&"x".repeat(256)),
            Instant::now(),
        )
        .unwrap_err()
        .to_string();
    assert!(message.contains("history budget"), "{message}");
    {
        let ctl = ctl.lock().unwrap();
        assert_eq!(ctl.mix_revision(), engine_before.0);
        assert_eq!(ctl.state(), &engine_before.1);
    }
    assert_eq!(session.project(), &before.0);
    assert_eq!(session.history_usage(), before.1);
    assert_eq!(session.view(), before.2);
    assert_eq!(session.mixer_view(), stamp);
}

#[test]
fn routed_bus_mix_is_installed_from_save_and_recovery() {
    let (c, _processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let ctl = Arc::new(Mutex::new(c));
    let mut session = Session::new(ctl.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("routed.grooviq");
    session.add_synth_channel("Init", Instant::now()).unwrap();
    let channel = session.project().channels[0].id;
    mixer(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Voice".into(),
                output: IdRef::Id(0),
            },
            Op::CreateTrack {
                name: "Return".into(),
                output: IdRef::Id(0),
            },
            Op::Assign {
                source: SourceKey::Channel(channel),
                destination: IdRef::Created(0),
            },
            Op::SetFader {
                track: IdRef::Created(0),
                level: Level::Db(-7.0),
                pan: 0.4,
            },
            Op::CreateSend {
                from: IdRef::Created(0),
                to: IdRef::Created(1),
                tap: Tap::Pre,
                gain: 0.3,
            },
        ],
    );
    let stored_mix = session.project().mixer.clone();
    let live_mix = ctl.lock().unwrap().state().mix.clone();
    assert!(matches!(live_mix, live::MixSetup::Routed(_)));

    let job = session.begin_save(Some(&path)).unwrap();
    let result = job.write(&dir.path().join("backups"));
    session.end_save(&job, result).unwrap();
    let loaded = grooviq_project::load(&path).unwrap();
    let decoded = grooviq_project::decode_samples(&loaded);
    assert_eq!(loaded.project.mixer, stored_mix);
    assert_eq!(
        engine_state(&loaded.project, &decoded.samples).unwrap().mix,
        live_mix
    );
    session.new_project().unwrap();
    session.open(&path, loaded, decoded).unwrap();
    assert_eq!(session.project().mixer, stored_mix);
    assert_eq!(ctl.lock().unwrap().state().mix, live_mix);
    assert_eq!(session.export_state().unwrap().mix, live_mix);

    let root = dir.path().join("recovery");
    let (recovery, _) = grooviq_project::recovery::RecoveryDir::start(&root).unwrap();
    recovery
        .write_autosave(session.project(), &job.samples, Some(&path))
        .unwrap();
    drop(recovery);
    let (_recovery, entries) = grooviq_project::recovery::RecoveryDir::start(&root).unwrap();
    let [entry] = entries.as_slice() else {
        panic!("expected one autosave")
    };
    let loaded = grooviq_project::load(&entry.file).unwrap();
    let decoded = grooviq_project::decode_samples(&loaded);
    assert_eq!(loaded.project.mixer, stored_mix);
    session.new_project().unwrap();
    session
        .open_recovered(entry.original.as_deref(), loaded, decoded, Instant::now())
        .unwrap();
    assert_eq!(session.project().mixer, stored_mix);
    assert_eq!(ctl.lock().unwrap().state().mix, live_mix);
    assert_eq!(session.export_state().unwrap().mix, live_mix);
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
fn structural_pattern_edit_preserves_interleaved_mixer_undo_and_redo() {
    use grooviq_project::{arrangement_edit::HistoryEffect, pattern_edit::PatternEdit};
    for make_unique in [false, true] {
        let (c, _processor) = engine::engine(engine::EngineConfig {
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
        let set_pan = |s: &mut Session, pan| {
            mixer(
                s,
                vec![Op::SetFader {
                    track: IdRef::Id(0),
                    level: Level::Db(-6.0),
                    pan,
                }],
            );
        };
        let pan = |s: &Session| s.project().mixer.tracks[0].pan;
        let pattern = if make_unique {
            0
        } else {
            edit(
                &mut session,
                PatternEdit::Create {
                    name: "Spare".into(),
                },
            )
            .patterns[1]
                .id
        };
        set_pan(&mut session, -0.5);
        let clip = arrangement(
            &mut session,
            vec![ArrangementEdit::CreateClip {
                track: 2,
                name: "Pattern".into(),
                start_tick: 0,
                muted: false,
                content: ClipContent::Pattern {
                    pattern,
                    offset_tick: 0,
                    length_ticks: 960,
                },
            }],
        )
        .created_ids[0];
        set_pan(&mut session, 0.5);
        let structural = if make_unique {
            arrangement(
                &mut session,
                vec![ArrangementEdit::Move {
                    clip,
                    track: 2,
                    start_tick: 960,
                }],
            );
            history(&mut session, true);
            PatternEdit::MakeUnique { clip }
        } else {
            arrangement(&mut session, vec![ArrangementEdit::DeleteClip { clip }]);
            history(&mut session, true);
            PatternEdit::Delete { pattern }
        };
        // Leave the second fader in redo. In the Delete case the clip must be
        // absent, so undo its creation and put that step in redo as well.
        history(&mut session, true);
        if !make_unique {
            history(&mut session, true);
        }
        assert_eq!(pan(&session), -0.5);
        edit(&mut session, structural);
        let patterns = session.project().patterns.clone();
        let arrangement = session.project().arrangement.clone();
        assert_eq!(session.history_usage().0, 2);

        history(&mut session, true);
        assert_eq!(pan(&session), 0.0);
        assert!(matches!(history(&mut session, true), HistoryEffect::Empty));
        history(&mut session, false);
        assert_eq!(pan(&session), -0.5);
        history(&mut session, false);
        assert_eq!(pan(&session), 0.5);
        assert!(matches!(history(&mut session, false), HistoryEffect::Empty));
        assert_eq!(session.project().patterns, patterns);
        assert_eq!(session.project().arrangement, arrangement);
    }
}

#[test]
fn deleting_pattern_drops_redo_for_routes_to_a_discarded_playlist_track() {
    use grooviq_project::{arrangement_edit::HistoryEffect, pattern_edit::PatternEdit};
    let (c, _processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut session = Session::new(Arc::new(Mutex::new(c)));
    let now = Instant::now();
    let stamp = session.patterns();
    let created = session
        .edit_patterns(
            stamp.generation,
            stamp.revision,
            PatternEdit::Create { name: "P".into() },
            now,
        )
        .unwrap();
    let pattern = created.patterns[1].id;
    let created = arrangement(
        &mut session,
        vec![
            ArrangementEdit::CreateTrack {
                name: "Playlist".into(),
                color: 0,
            },
            ArrangementEdit::CreateClipOnCreatedTrack {
                track_operation: 0,
                name: "P clip".into(),
                start_tick: 0,
                muted: false,
                content: ClipContent::Pattern {
                    pattern,
                    offset_tick: 0,
                    length_ticks: 960,
                },
            },
        ],
    );
    let source = SourceKey::AudioTrack(created.created_ids[0]);
    mixer(
        &mut session,
        vec![
            Op::CreateTrack {
                name: "Bus".into(),
                output: IdRef::Id(0),
            },
            Op::Assign {
                source,
                destination: IdRef::Created(0),
            },
        ],
    );
    assert!(
        session
            .project()
            .mixer
            .sources
            .iter()
            .any(|a| a.source == source)
    );

    for _ in 0..2 {
        assert!(matches!(
            history(&mut session, true),
            HistoryEffect::Arrangement
        ));
    }
    let stamp = session.patterns();
    session
        .edit_patterns(
            stamp.generation,
            stamp.revision,
            PatternEdit::Delete { pattern },
            now,
        )
        .unwrap();

    let before = session.project().clone();
    assert!(matches!(history(&mut session, false), HistoryEffect::Empty));
    assert_eq!(session.project(), &before);
    assert!(session.project().mixer.sources.is_empty());
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

const BIT_SEED: u64 = 0x9758_35de_1c97_56ce;
const BIT_PARAMS: [f32; 6] = [4.0, 11.0, 0.35, 0.4, 1.0, 0.8];
const UTIL_PARAMS: [f32; 5] = [-7.0, -0.25, 1.25, 0.0, 1.0];
const COMP_PARAMS: [f32; 9] = [-35.0, 8.0, 0.1, 25.0, 0.0, 0.0, 1.0, 1.0, 25.0];

fn set_params(device: &mut impl grooviq_dsp::Device, params: &[f32]) {
    for (index, &value) in (0..).zip(params) {
        device.set_param(index, value);
    }
}

fn manual_chain(seed: u64, params: [f32; 6]) -> Vec<f32> {
    use grooviq_dsp::{Device, bitcrusher::Bitcrusher, comp::Comp, utility::Utility};
    let folded = (seed % 4_294_967_296) as u32 ^ (seed / 4_294_967_296) as u32;
    let mut bitcrusher = Bitcrusher::with_seed(48_000.0, folded);
    let mut utility = Utility::new(48_000.0);
    let mut compressor = Comp::new(48_000.0);
    set_params(&mut bitcrusher, &params);
    set_params(&mut utility, &UTIL_PARAMS);
    set_params(&mut compressor, &COMP_PARAMS);

    let mut expected = Vec::with_capacity(FRAMES * 2);
    for from in (0..FRAMES).step_by(64) {
        let mut left =
            std::array::from_fn::<_, 64, _>(|i| f32::from(pcm_sample(from + i)) / 32768.0);
        let mut right = left;
        bitcrusher.process(&mut left, &mut right, None, &[]);
        utility.process(&mut left, &mut right, None, &[]);
        // The main bus is silent. Its pre-fader send feeds the return at 0.75, while the
        // pre-fader sidechain drives the compressor at the bus's full signal level.
        let mut audio_l = left.map(|v| v * 0.75);
        let mut audio_r = right.map(|v| v * 0.75);
        compressor.process(&mut audio_l, &mut audio_r, Some((&left, &right)), &[]);
        for (l, r) in audio_l.into_iter().zip(audio_r) {
            expected.extend([l, r]);
        }
    }
    expected
}

fn configure_chain(session: &mut Session) {
    use grooviq_project::mixer::Tap;
    let result = mixer(
        session,
        vec![
            Op::CreateTrack {
                name: "Chain".into(),
                output: IdRef::Id(0),
            },
            Op::CreateTrack {
                name: "Return".into(),
                output: IdRef::Id(0),
            },
            Op::Assign {
                source: SourceKey::AudioTrack(2),
                destination: IdRef::Created(0),
            },
            Op::SetFader {
                track: IdRef::Created(0),
                level: Level::Silence,
                pan: 0.0,
            },
            Op::AddInsert {
                track: IdRef::Created(0),
                index: 0,
                device: "utility".into(),
                params: UTIL_PARAMS.to_vec(),
                preset: None,
            },
            Op::AddInsert {
                track: IdRef::Created(0),
                index: 0,
                device: "bitcrusher".into(),
                params: BIT_PARAMS.to_vec(),
                preset: Some("Label only".into()),
            },
            Op::AddInsert {
                track: IdRef::Created(1),
                index: 0,
                device: "comp".into(),
                params: COMP_PARAMS.to_vec(),
                preset: None,
            },
            Op::CreateSend {
                from: IdRef::Created(0),
                to: IdRef::Created(1),
                tap: Tap::Pre,
                gain: 0.75,
            },
            Op::SetSidechain {
                previous: None,
                from: IdRef::Created(0),
                to: IdRef::Created(6),
                tap: Tap::Pre,
            },
        ],
    );
    assert_eq!(
        result.created_ids,
        [
            (0, CreatedId::Track(1)),
            (1, CreatedId::Track(2)),
            (4, CreatedId::Insert(1)),
            (5, CreatedId::Insert(2)),
            (6, CreatedId::Insert(3)),
            (7, CreatedId::Send(1)),
        ]
    );
}

#[test]
fn a_seeded_chain_matches_its_manual_render_through_edit_history_save_and_recovery() {
    let (c, mut processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut session = Session::new(c.clone());
    let loaded = audio_fixture(true);
    let decoded = grooviq_project::decode_samples(&loaded);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Song.grooviq");
    session.open(&path, loaded, decoded).unwrap();
    processor.process(&mut [0.0; 128], 2);
    let dry = sound(&session);
    configure_chain(&mut session);
    let first = manual_chain(BIT_SEED, BIT_PARAMS);
    assert!(first.iter().any(|x| x.abs() > 1e-4));
    assert_ne!(first, dry);
    assert_eq!(sound(&session), first);

    let mut params = BIT_PARAMS;
    params[0] = 8.0;
    mixer(
        &mut session,
        vec![Op::SetInsert {
            insert: IdRef::Id(2),
            params: params.to_vec(),
            preset: Some("Name only".into()),
        }],
    );
    let edited = manual_chain(BIT_SEED, params);
    assert_ne!(first, edited);
    assert_ne!(manual_chain(0, params), edited);
    assert_eq!(sound(&session), edited);
    history(&mut session, true);
    assert_eq!(sound(&session), first);
    history(&mut session, true);
    assert_eq!(sound(&session), dry);
    let m = &session.project().mixer;
    assert_eq!(
        (m.next_track_id, m.next_insert_id, m.next_send_id),
        (3, 4, 2)
    );
    history(&mut session, false);
    history(&mut session, false);
    assert_eq!(sound(&session), edited);

    let mix = session.project().mixer.clone();
    let job = session.begin_save(Some(&path)).unwrap();
    let written = job.write(&dir.path().join("backups"));
    session.end_save(&job, written).unwrap();
    let loaded = grooviq_project::load(&path).unwrap();
    assert_eq!(loaded.project.mixer, mix);
    let decoded = grooviq_project::decode_samples(&loaded);
    session.open(&path, loaded, decoded).unwrap();
    assert_eq!(sound(&session), edited);

    let recovery_root = dir.path().join("recovery");
    let (recovery, _) = grooviq_project::recovery::RecoveryDir::start(&recovery_root).unwrap();
    recovery
        .write_autosave(session.project(), &job.samples, Some(&path))
        .unwrap();
    drop(recovery);
    let (_recovery, entries) =
        grooviq_project::recovery::RecoveryDir::start(&recovery_root).unwrap();
    let [entry] = entries.as_slice() else {
        panic!("expected one recovery file")
    };
    let recovered = grooviq_project::load(&entry.file).unwrap();
    assert_eq!(recovered.project.mixer, mix);
    let decoded = grooviq_project::decode_samples(&recovered);
    session
        .open_recovered(
            entry.original.as_deref(),
            recovered,
            decoded,
            Instant::now(),
        )
        .unwrap();
    assert_eq!(sound(&session), edited);

    let options = engine::ExportOptions {
        sample_rate: 48_000,
        format: engine::WavFormat::Float32,
        max_tail_seconds: 0.0,
        ..Default::default()
    };
    let before = (
        session.view(),
        session.mixer_view(),
        c.lock().unwrap().state().clone(),
    );
    let output = dir.path().join("Song.wav");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    assert!(
        engine::export_wav(
            session.checked_export_state().unwrap(),
            &engine::ExportOptions {
                sample_rate: 384_000,
                ..options
            },
            &output,
            &mut |_| {},
            &cancel,
        )
        .is_err()
    );
    assert!(!output.exists());
    assert_eq!(
        (
            session.view(),
            session.mixer_view(),
            c.lock().unwrap().state().clone()
        ),
        before
    );
    let report = engine::export_wav(
        session.checked_export_state().unwrap(),
        &options,
        &output,
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    assert_eq!(report.frames, FRAMES as u64);
    let bytes = std::fs::read(&output).unwrap();
    let data = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
    let exported: Vec<_> = bytes[data..]
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .map(f32::from_le_bytes)
        .collect();
    assert_eq!(exported, edited);
}

#[test]
fn replacing_the_mix_preserves_the_delay_line_of_a_surviving_insert() {
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
    mixer(
        &mut session,
        vec![Op::AddInsert {
            track: IdRef::Id(0),
            index: 0,
            device: "delay".into(),
            params: vec![0.0, 4.0, 40.0, 0.35, 0.0, 20.0, 7000.0, 1.0, 120.0],
            preset: None,
        }],
    );
    session.play().unwrap();
    let mut before = vec![0.0; FRAMES * 2];
    processor.process(&mut before, 2);
    assert!(
        before.iter().all(|x| x.abs() < 1e-6),
        "wet-only delay starts beyond the input"
    );
    mixer(
        &mut session,
        vec![Op::SetFader {
            track: IdRef::Id(0),
            level: Level::Db(-3.0),
            pan: -1.0,
        }],
    );
    let mut tail = vec![0.0; 2048 * 2];
    processor.process(&mut tail, 2);
    assert!(tail.as_chunks::<2>().0.iter().any(|f| f[0].abs() > 1e-3));
    assert!(tail.as_chunks::<2>().0.iter().all(|f| f[1] == 0.0));
}

#[test]
fn undo_budget_charges_nested_insert_metadata_before_engine_admission() {
    let make = || {
        let (c, _) = engine::engine(engine::EngineConfig {
            sample_rate: 48_000.0,
        });
        let c = Arc::new(Mutex::new(c));
        (Session::new(c.clone()), c)
    };
    let ops = |label: &str| {
        (0..16)
            .map(|index| Op::AddInsert {
                track: IdRef::Id(0),
                index,
                device: "utility".into(),
                params: UTIL_PARAMS.to_vec(),
                preset: Some(label.into()),
            })
            .collect()
    };
    let (mut short, _) = make();
    mixer(&mut short, ops("small"));
    let minimal_bytes = short.history_usage().2;

    let (mut long, ctl) = make();
    long.set_history_limits(grooviq_project::HistoryLimits {
        bytes: minimal_bytes,
        ..Default::default()
    });
    let v = long.mixer_view();
    let state = ctl.lock().unwrap().state().clone();
    let long_label = "x".repeat(256);
    let error = long
        .edit_mixer(v.generation, v.revision, ops(&long_label), Instant::now())
        .unwrap_err();
    assert!(error.to_string().contains("history budget"), "{error}");
    assert_eq!(long.mixer_view(), v);
    assert_eq!(long.history_usage().0, 0);
    assert_eq!(ctl.lock().unwrap().state(), &state);
}

#[test]
fn a_handwritten_v4_file_opens_only_at_a_supported_rate() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Manual.grooviq");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("project.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(
        br#"{
        "format": "grooviq-project", "version": 4, "name": "Recorded session",
        "mixer": {
            "mode": "routed",
            "tracks": [{
                "id": 0, "name": "Main", "level": {"db": -2.5}, "pan": -0.25,
                "inserts": [{
                    "id": 8, "device": "bitcrusher", "seed": "18446744073709551614",
                    "params": [4.0, 11.0, 0.35, 0.4, 1.0, 0.8]
                }]
            }],
            "nextTrackId": 1, "nextInsertId": 9, "nextSendId": 1
        }
    }"#,
    )
    .unwrap();
    zip.finish().unwrap();

    let open = |s: &mut Session| {
        let loaded = grooviq_project::load(&path).unwrap();
        let decoded = grooviq_project::decode_samples(&loaded);
        s.open(&path, loaded, decoded)
    };
    let (c, _p) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut native = Session::new(Arc::new(Mutex::new(c)));
    open(&mut native).unwrap();
    let v = native.mixer_view();
    assert_eq!(v.mixer.tracks[0].inserts[0].seed, u64::MAX - 1);
    assert_eq!(v.mixer.tracks[0].inserts[0].params, BIT_PARAMS);
    let mut changed_params = BIT_PARAMS;
    changed_params[0] = 8.0;
    mixer(
        &mut native,
        vec![Op::SetInsert {
            insert: IdRef::Id(8),
            params: changed_params.to_vec(),
            preset: Some("Keep the imported seed".into()),
        }],
    );
    assert_eq!(
        native.project().mixer.tracks[0].inserts[0].seed,
        u64::MAX - 1
    );
    history(&mut native, true);
    history(&mut native, false);
    assert_eq!(
        native.project().mixer.tracks[0].inserts[0].seed,
        u64::MAX - 1
    );

    let (c, _p) = engine::engine(engine::EngineConfig {
        sample_rate: 8_000.0,
    });
    let ctl = Arc::new(Mutex::new(c));
    let mut limited = Session::new(ctl.clone());
    limited.add_synth_channel("Init", Instant::now()).unwrap();
    limited.set_recording(true).unwrap();
    let before = (
        limited.view(),
        limited.mixer_view(),
        ctl.lock().unwrap().state().clone(),
    );
    for recover in [false, true] {
        let loaded = grooviq_project::load(&path).unwrap();
        let decoded = grooviq_project::decode_samples(&loaded);
        let result = if recover {
            limited.open_recovered(Some(&path), loaded, decoded, Instant::now())
        } else {
            limited.open(&path, loaded, decoded)
        };
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("bitcrusher") && error.contains("8000"),
            "{error}"
        );
        assert_eq!(
            (
                limited.view(),
                limited.mixer_view(),
                ctl.lock().unwrap().state().clone()
            ),
            before
        );
    }
    let stamp = limited.mixer_view();
    let err = limited
        .edit_mixer(
            stamp.generation,
            stamp.revision,
            vec![Op::AddInsert {
                track: IdRef::Id(0),
                index: 0,
                device: "bitcrusher".into(),
                params: BIT_PARAMS.to_vec(),
                preset: None,
            }],
            Instant::now(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("bitcrusher"));
    assert_eq!(
        (
            limited.view(),
            limited.mixer_view(),
            ctl.lock().unwrap().state().clone()
        ),
        before
    );
}

#[test]
fn saving_a_mix_refuses_inserts_whose_full_rate_payload_exceeds_the_graph_budget() {
    use grooviq_project::mixer::{Insert, MixerError};
    let mut project = Project::new();
    project.mixer.mode = Mode::Routed;
    project.mixer.next_insert_id = 34;
    // Even the legacy 384 kHz Delay has two 2^21-sample float rings; 33 are over 512 MiB.
    // No DSP should be constructed to reject this shape.
    let params = vec![0.0, 4.0, 40.0, 0.35, 0.0, 20.0, 7000.0, 1.0, 120.0];
    project.mixer.tracks[0].inserts = (1..=33)
        .map(|id| Insert {
            id,
            device: "delay".into(),
            params: params.clone(),
            preset: None,
            seed: u64::from(id),
            bypass: false,
        })
        .collect();
    project.validate_mixer().unwrap();
    assert!(matches!(
        grooviq_project::mixer_compile::compile_mix(&project),
        Err(MixerError::Graph(_))
    ));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.grooviq");
    assert!(grooviq_project::save(&project, &HashMap::new(), &path).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn reverb_seeds_survive_insert_undo_redo_and_exact_reopen_export() {
    let (c, _processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut session = Session::new(Arc::new(Mutex::new(c)));
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("reverbs.grooviq");
    let loaded = audio_fixture(true);
    let decoded = grooviq_project::decode_samples(&loaded);
    session.open(&project_path, loaded, decoded).unwrap();

    let params: Vec<_> = grooviq_dsp::algorithmic_reverb::SPEC
        .iter()
        .map(|&(_, _, default)| default)
        .collect();
    for index in 0..2 {
        mixer(
            &mut session,
            vec![Op::AddInsert {
                track: IdRef::Id(0),
                index,
                device: "algorithmic_reverb".into(),
                params: params.clone(),
                preset: None,
            }],
        );
    }
    let seeds = |s: &Session| {
        s.project().mixer.tracks[0]
            .inserts
            .iter()
            .map(|insert| insert.seed)
            .collect::<Vec<_>>()
    };
    let expected = seeds(&session);
    assert_ne!(expected[0] & 0xffff, expected[1] & 0xffff);
    assert!(expected.iter().all(|&seed| seed > 1_u64 << 53));
    history(&mut session, true);
    assert_eq!(seeds(&session), [expected[0]]);
    history(&mut session, false);
    assert_eq!(seeds(&session), expected);

    let export = |s: &Session, filename: &str| {
        let path = dir.path().join(filename);
        let options = engine::ExportOptions {
            sample_rate: 48_000,
            format: engine::WavFormat::Float32,
            max_tail_seconds: 0.25,
            ..Default::default()
        };
        let report = engine::export_wav(
            s.checked_export_state().unwrap(),
            &options,
            &path,
            &mut |_| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert!(report.frames > FRAMES as u64, "rendered reverb tail");
        std::fs::read(path).unwrap()
    };
    let before = export(&session, "before.wav");
    let job = session.begin_save(Some(&project_path)).unwrap();
    let result = job.write(&dir.path().join("backups"));
    session.end_save(&job, result).unwrap();

    let loaded = grooviq_project::load(&project_path).unwrap();
    let decoded = grooviq_project::decode_samples(&loaded);
    session.open(&project_path, loaded, decoded).unwrap();
    assert_eq!(seeds(&session), expected);
    assert!(
        export(&session, "after.wav") == before,
        "reopened export bytes differ"
    );
}
