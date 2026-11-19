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
            frame_extent: None,
            audio: AudioSchedule::default(),
            automation: Default::default(),
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

#[test]
fn compound_timeline_refusal_and_rebuild_preserve_complete_candidate() {
    let cfg = EngineConfig {
        sample_rate: 48000.0,
    };
    let (mut c, mut p) = engine_with(cfg, state());
    let update = TimelineUpdate {
        pattern: Box::new(PatternData {
            length: 1920,
            notes: vec![],
        }),
        song: Box::new(state().song),
        bpm: 180.0,
        mode: PlaybackMode::Song,
        region: None,
        remove_channel: None,
    };
    while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    let before = c.state().clone();
    assert_eq!(c.set_timeline(update.clone()), Err(ControlError::Full));
    assert_eq!(c.state(), &before);
    p.process(&mut [0.0; 128], 2);
    c.set_timeline(update).unwrap();
    assert_eq!(c.state().bpm, 180.0);
    assert_eq!(c.state().pattern.length, 1920);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44100.0,
        },
    );
    c.send(Command::Play).unwrap();
    let mut actual = vec![0.0; 10_000];
    p.process(&mut actual, 2);
    let expected = render_offline_with(
        EngineConfig {
            sample_rate: 44100.0,
        },
        c.state().clone(),
        &[(0, Command::Play)],
        5000,
        64,
    );
    assert!(expected.iter().any(|x| *x != 0.0));
    assert_eq!(actual.as_slice(), expected.as_slice());
}

#[test]
fn queued_compound_mode_change_still_stops_transport_after_reconfigure() {
    let cfg = EngineConfig {
        sample_rate: 48000.0,
    };
    let mut initial = state();
    initial.playback_mode = PlaybackMode::Pattern;
    let (mut c, mut p) = engine_with(cfg, initial);
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    assert!(c.live().playing);
    c.set_timeline(TimelineUpdate {
        pattern: Box::new(PatternData::default()),
        song: Box::new(state().song),
        bpm: 180.0,
        mode: PlaybackMode::Song,
        region: None,
        remove_channel: None,
    })
    .unwrap();
    c.reconfigure(&mut p, cfg);
    p.process(&mut [0.0; 128], 2);
    assert!(!c.live().playing);
    assert_eq!(c.state().playback_mode, PlaybackMode::Song);
}

#[test]
fn region_export_crops_future_audio_and_automation_without_invalid_horizon() {
    let mut state = state();
    state.song.end_tick = 1920;
    state.song.audio = AudioSchedule::build(
        vec![AudioAsset {
            id: 2,
            source_rate: 48000,
            source_frames: 128,
            decoded: Some(Arc::new(SampleBuf {
                rate: 48000.0,
                l: vec![0.25; 128],
                r: vec![0.25; 128],
            })),
        }],
        vec![AudioClip {
            id: 3,
            asset: 2,
            start_tick: 1440,
            source_start: 0,
            source_end: 128,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        }],
    )
    .unwrap()
    .0;
    state.song.automation = AutomationSchedule::build(
        vec![AutomationClip {
            target: AutomationTarget::ChannelVolume(ChannelId(1)),
            priority: 4,
            start_tick: 0,
            offset_tick: 0,
            length_ticks: 1920,
            points: vec![AutomationPoint {
                tick: 0,
                value: 0.5,
                segment: AutomationSegment::Hold,
            }],
        }],
        &state.channels,
    )
    .unwrap();
    let d = tempfile::tempdir().unwrap();
    let options = ExportOptions {
        song_region: Some(SongLoop {
            start_tick: 240,
            end_tick: 720,
        }),
        max_tail_seconds: 0.0,
        ..Default::default()
    };
    let result = std::panic::catch_unwind(|| {
        export_wav(
            state,
            &options,
            &d.path().join("region.wav"),
            &mut |_| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
    });
    assert!(
        result.is_ok(),
        "region preparation must never construct an invalid engine state"
    );
    assert_eq!(result.unwrap().unwrap().frames, 12000);
}
#[test]
fn region_export_excludes_onset_at_exact_end() {
    let mut state = state();
    state.song.end_tick = 1920;
    state.song.audio = AudioSchedule::build(
        vec![AudioAsset {
            id: 2,
            source_rate: 48000,
            source_frames: 128,
            decoded: Some(Arc::new(SampleBuf {
                rate: 48000.0,
                l: vec![0.25; 128],
                r: vec![0.25; 128],
            })),
        }],
        vec![AudioClip {
            id: 3,
            asset: 2,
            start_tick: 720,
            source_start: 0,
            source_end: 128,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        }],
    )
    .unwrap()
    .0;
    state.song.automation = AutomationSchedule::build(
        vec![AutomationClip {
            target: AutomationTarget::ChannelVolume(ChannelId(1)),
            priority: 4,
            start_tick: 0,
            offset_tick: 0,
            length_ticks: 1920,
            points: vec![AutomationPoint {
                tick: 0,
                value: 0.5,
                segment: AutomationSegment::Hold,
            }],
        }],
        &state.channels,
    )
    .unwrap();
    let d = tempfile::tempdir().unwrap();
    let options = ExportOptions {
        song_region: Some(SongLoop {
            start_tick: 240,
            end_tick: 720,
        }),
        max_tail_seconds: 0.0,
        ..Default::default()
    };
    let result = std::panic::catch_unwind(|| {
        export_wav(
            state,
            &options,
            &d.path().join("region.wav"),
            &mut |_| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
    });
    assert!(
        result.is_ok(),
        "region preparation must never construct an invalid engine state"
    );
    assert_eq!(result.unwrap().unwrap().frames, 12000);
}

#[test]
fn unrounded_extent_admission_refuses_invalid_geometry() {
    let mut c = engine_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        state(),
    )
    .0;
    let base = state().song;
    let good = SongFrameExtent {
        musical_end_tick: 960,
        audio: vec![(0, 48001, 48000)],
    };
    let mut song = base.clone();
    song.frame_extent = Some(Arc::new(good.clone()));
    c.set_song(song).unwrap();
    for extent in [
        SongFrameExtent {
            musical_end_tick: 961,
            ..good.clone()
        },
        SongFrameExtent {
            audio: vec![(960, 1, 48000)],
            ..good.clone()
        },
        SongFrameExtent {
            audio: vec![(0, 1, 7999)],
            ..good.clone()
        },
        SongFrameExtent {
            audio: vec![(0, 0, 48000)],
            ..good.clone()
        },
        SongFrameExtent {
            audio: vec![(0, 48000 * 600 + 1, 48000)],
            ..good.clone()
        },
        SongFrameExtent {
            audio: vec![(0, 1, 48000); 100001],
            ..good
        },
    ] {
        let mut song = base.clone();
        song.frame_extent = Some(Arc::new(extent));
        assert!(c.set_song(song).is_err());
    }
}

#[test]
fn explicit_extent_cannot_shorten_admitted_musical_content() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for source in ["arrangement", "note", "automation"] {
        let mut initial = state();
        initial.song.arrangement.length = 0;
        initial.song.arrangement.notes.clear();
        let minimum = match source {
            "arrangement" => {
                initial.song.arrangement.length = 960;
                960
            }
            "note" => {
                // Engine callers may provide a note beyond arrangement.length. The note's
                // release is independently mandatory; the UI end_tick is not its substitute.
                initial.song.arrangement.notes = state().song.arrangement.notes;
                360
            }
            _ => {
                initial.song.automation = AutomationSchedule::build(
                    vec![AutomationClip {
                        target: AutomationTarget::ChannelVolume(ChannelId(1)),
                        priority: 1,
                        start_tick: 240,
                        offset_tick: 0,
                        length_ticks: 720,
                        points: vec![AutomationPoint {
                            tick: 0,
                            value: 0.5,
                            segment: AutomationSegment::Hold,
                        }],
                    }],
                    &initial.channels,
                )
                .unwrap();
                960
            }
        };
        initial.song.frame_extent = Some(Arc::new(SongFrameExtent {
            musical_end_tick: minimum,
            audio: Vec::new(),
        }));
        let (mut c, mut p) = engine_with(
            EngineConfig {
                sample_rate: 48000.0,
            },
            initial.clone(),
        );
        let options = ExportOptions {
            format: WavFormat::Float32,
            max_tail_seconds: 0.0,
            ..Default::default()
        };
        let file = dir.path().join("valid.wav");
        let expected = ticks_to_frames(minimum as f64, 120.0, 48000.0);
        assert_eq!(
            export_wav(c.state().clone(), &options, &file, &mut |_| {}, &cancel)
                .unwrap()
                .frames,
            expected
        );
        if source == "note" {
            assert!(
                hound::WavReader::open(&file)
                    .unwrap()
                    .into_samples::<f32>()
                    .any(|s| s.unwrap() != 0.0)
            );
        }
        for endpoint in [0, minimum - 1] {
            let mut contradictory = initial.song.clone();
            contradictory.frame_extent = Some(Arc::new(SongFrameExtent {
                musical_end_tick: endpoint,
                audio: Vec::new(),
            }));
            let before = c.state().clone();
            assert_eq!(
                c.set_song(contradictory.clone()),
                Err(ControlError::Rejected),
                "{source} endpoint {endpoint}"
            );
            assert_eq!(c.state(), &before);
            assert_eq!(
                c.set_timeline(TimelineUpdate {
                    pattern: Box::new(before.pattern.clone()),
                    song: Box::new(contradictory),
                    bpm: before.bpm,
                    mode: before.playback_mode,
                    region: None,
                    remove_channel: None,
                }),
                Err(ControlError::Rejected)
            );
            assert_eq!(c.state(), &before);
            assert_eq!(
                export_wav(c.state().clone(), &options, &file, &mut |_| {}, &cancel)
                    .unwrap()
                    .frames,
                expected
            );
        }
        c.set_song(initial.song).unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; 128], 2);
        let mut reader = hound::WavReader::open(file).unwrap();
        assert_eq!(reader.samples::<f32>().count() as u64, expected * 2);
    }
}
