use grooviq_engine::*;
use std::sync::Arc;
fn state() -> EngineState {
    let mut params: Vec<_> = grooviq_dsp::sampler::SPEC.iter().map(|s| s.2).collect();
    params[14] = 0.0;
    let sample = Arc::new(SampleBuf {
        rate: 48000.0,
        l: vec![1.0, 0.0, 0.0, 0.0],
        r: vec![1.0, 0.0, 0.0, 0.0],
    });
    EngineState {
        bpm: 120.0,
        channels: vec![ChannelSetup::sampler(
            ChannelId(1),
            Some(sample),
            params,
            ChannelMix::default(),
        )],
        playback_mode: PlaybackMode::Song,
        song: SongData {
            arrangement: ArrangementData {
                length: 960,
                notes: vec![SongNote {
                    tick: 240,
                    length: 120,
                    channel: ChannelId(1),
                    key: 60,
                    velocity: 1.0,
                    slide: false,
                    clip_id: 1,
                }],
            },
            end_tick: 960,
            end_frame: 0,
            frame_rate: 48000.0,
            audio: AudioSchedule::default(),
        },
        ..EngineState::default()
    }
}
#[test]
fn callback_partitions_produce_identical_samples_and_single_pass() {
    let cfg = EngineConfig {
        sample_rate: 48000.0,
    };
    let events = [(0, Command::Play)];
    let expected = render_offline_with(cfg, state(), &events, 50000, 64);
    assert!(expected.iter().any(|x| *x != 0.0));
    assert!(expected[24000 * 2..].iter().all(|x| *x == 0.0));
    for block in [1, 63, 64, 127, 128, 255, 256, 511] {
        assert_eq!(
            render_offline_with(cfg, state(), &events, 50000, block),
            expected,
            "{block}"
        );
    }
}
#[test]
fn atomic_validation_queue_refusal_and_recording_ruling() {
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        state(),
    );
    assert!(c.send(Command::SetRecording { on: true }).is_err());
    let before = c.state().clone();
    assert!(
        c.send(Command::SetSongLoop {
            region: Some(SongLoop {
                start_tick: 900,
                end_tick: 800
            })
        })
        .is_err()
    );
    let mut invalid = before.song.clone();
    invalid.arrangement.notes[0].velocity = f32::NAN;
    assert!(c.set_song(invalid).is_err());
    assert_eq!(c.state(), &before);
    assert!(c.remove_channel(ChannelId(1)).is_err());
    while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    assert_eq!(c.set_song(SongData::default()), Err(ControlError::Full));
    assert_eq!(c.state().song, before.song);
    p.process(&mut [0.0; 128], 2);
}
#[test]
fn mirror_reconfigure_seek_and_stop_keep_song_contract() {
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        state(),
    );
    c.send(Command::SetSongLoop {
        region: Some(SongLoop {
            start_tick: 240,
            end_tick: 480,
        }),
    })
    .unwrap();
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96000.0,
        },
    );
    assert_eq!(c.state().playback_mode, PlaybackMode::Song);
    assert_eq!(
        c.state().song_loop,
        Some(SongLoop {
            start_tick: 240,
            end_tick: 480
        })
    );
    p.process(&mut [0.0; 128], 2);
    assert!((242..245).contains(&c.live().position_ticks));
    c.send(Command::Seek { tick: 700 }).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::Stop).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Pattern,
    })
    .unwrap();
    c.send(Command::SetRecording { on: true }).unwrap();
    p.process(&mut [0.0; 128], 2);
}
#[test]
fn song_export_ignores_transport_loop_and_zero_tail_is_exact() {
    let d = tempfile::tempdir().unwrap();
    let mut s = state();
    s.song_loop = Some(SongLoop {
        start_tick: 240,
        end_tick: 480,
    });
    let options = ExportOptions {
        format: WavFormat::Float32,
        max_tail_seconds: 0.0,
        ..ExportOptions::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let report = export_wav(
        s.clone(),
        &options,
        &d.path().join("whole.wav"),
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    assert_eq!(report.frames, 24000);
    let options = ExportOptions {
        song_region: Some(SongLoop {
            start_tick: 240,
            end_tick: 480,
        }),
        ..options
    };
    let report = export_wav(
        s,
        &options,
        &d.path().join("region.wav"),
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    assert_eq!(report.frames, 6000);
}

#[test]
fn song_horizon_includes_non_note_content_and_take_ends_before_barrier() {
    let cfg = EngineConfig {
        sample_rate: 48000.0,
    };
    let mut s = state();
    s.song.arrangement.notes.clear();
    s.song.arrangement.length = 0;
    s.song.end_tick = 10;
    s.song.end_frame = 1000;
    let (mut c, mut p) = engine_with(cfg, s);
    c.send(Command::Play).unwrap();
    p.process(&mut vec![0.0; 1800], 2);
    assert!(c.live().playing);
    p.process(&mut vec![0.0; 400], 2);
    assert!(!c.live().playing);
    assert_eq!(c.live().position_ticks, 10);
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Pattern,
    })
    .unwrap();
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    let mark = c
        .send_marked(Command::SetPlaybackMode {
            mode: PlaybackMode::Song,
        })
        .unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut records = Vec::new();
    c.take_recorded(&mut records);
    let end = records
        .iter()
        .position(|e| matches!(e, RecordEvent::End { .. }))
        .unwrap();
    let barrier = records
        .iter()
        .position(|e| matches!(e,RecordEvent::Mark {id} if *id==mark))
        .unwrap();
    assert!(end < barrier);
}

#[test]
fn seek_and_loop_release_only_their_own_plain_notes() {
    let mut s = state();
    let mut params: Vec<_> = grooviq_dsp::sampler::SPEC.iter().map(|p| p.2).collect();
    params[7] = 0.0;
    params[11] = 0.0;
    params[14] = 0.0;
    s.channels = vec![ChannelSetup::sampler(
        ChannelId(1),
        Some(Arc::new(SampleBuf {
            rate: 48000.0,
            l: vec![0.2; 48000],
            r: vec![0.2; 48000],
        })),
        params,
        ChannelMix::default(),
    )];
    s.song.arrangement.notes[0].tick = 0;
    s.song.arrangement.notes[0].length = 960;
    let live = NoteId::live();
    let events = [
        (
            0,
            Command::ChannelNote {
                channel: ChannelId(1),
                key: 60,
                velocity: 1.0,
                on: true,
                id: live,
            },
        ),
        (0, Command::Play),
        (640, Command::Seek { tick: 480 }),
        (1280, Command::Stop),
        (
            3000,
            Command::ChannelNote {
                channel: ChannelId(1),
                key: 60,
                velocity: 0.0,
                on: false,
                id: live,
            },
        ),
    ];
    let out = render_offline_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        s,
        &events,
        4000,
        127,
    );
    assert!(out[2000 * 2] > 0.1, "sequenced release cut live identity");
    assert!(out[3500 * 2].abs() < 0.001, "live identity did not release");
}

#[test]
fn pattern_loop_configuration_and_repeated_mode_command_do_not_move_transport() {
    let mut s = state();
    s.playback_mode = PlaybackMode::Pattern;
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        s,
    );
    c.send(Command::Play).unwrap();
    p.process(&mut vec![0.0; 12800], 2);
    let before = c.live().position_ticks;
    c.send(Command::SetSongLoop {
        region: Some(SongLoop {
            start_tick: 240,
            end_tick: 480,
        }),
    })
    .unwrap();
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Pattern,
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    assert!(c.live().position_ticks >= before);
    assert!(c.live().playing);
}

#[cfg(feature = "test-hooks")]
#[test]
fn fault_recovery_retains_song_region_and_musical_cursor() {
    let mut s = state();
    s.song_loop = Some(SongLoop {
        start_tick: 240,
        end_tick: 480,
    });
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        s,
    );
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::InjectPanic).unwrap();
    p.process(&mut [0.0; 128], 2);
    let before = c.live().position_ticks;
    let events = c.check_health();
    assert!(!events.is_empty());
    p.process(&mut [0.0; 128], 2);
    assert!(c.live().playing);
    assert!(c.live().position_ticks >= before);
    assert_eq!(
        c.state().song_loop,
        Some(SongLoop {
            start_tick: 240,
            end_tick: 480
        })
    );
    assert_eq!(c.state().playback_mode, PlaybackMode::Song);
}
