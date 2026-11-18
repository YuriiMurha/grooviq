use grooviq_engine::*;
use std::sync::Arc;
fn clip() -> AudioClip {
    AudioClip {
        id: 1,
        asset: 1,
        start_tick: 0,
        source_start: 1,
        source_end: 5,
        gain: 1.0,
        reverse: false,
        fade_in: 0,
        fade_out: 0,
    }
}
fn schedule(c: Vec<AudioClip>) -> AudioSchedule {
    AudioSchedule::build(
        vec![AudioAsset {
            id: 1,
            source_rate: 48000,
            source_frames: 6,
            decoded: Some(Arc::new(SampleBuf {
                rate: 48000.0,
                l: vec![0., 1., 2., 3., 4., 5.],
                r: vec![0., -1., -2., -3., -4., -5.],
            })),
        }],
        c,
    )
    .unwrap()
    .0
}
fn render(c: Vec<AudioClip>, partition: usize) -> Vec<f32> {
    let mut s = EngineState {
        playback_mode: PlaybackMode::Song,
        ..EngineState::default()
    };
    s.song.audio = schedule(c);
    s.song.end_tick = 960;
    render_offline_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        s,
        &[(0, Command::Play)],
        128,
        partition,
    )
}
#[test]
fn trim_reverse_and_partitions() {
    let out = render(vec![clip()], 64);
    assert_eq!(&out[..10], &[1., -1., 2., -2., 3., -3., 4., -4., 0., 0.]);
    for n in [1, 63, 127, 128, 255, 256, 511] {
        assert_eq!(render(vec![clip()], n), out);
    }
    let mut c = clip();
    c.reverse = true;
    assert_eq!(
        &render(vec![c], 64)[..10],
        &[4., -4., 3., -3., 2., -2., 1., -1., 0., 0.]
    );
}
#[test]
fn fades_multiply_and_gain() {
    let mut c = clip();
    c.fade_in = 4;
    c.fade_out = 4;
    c.gain = 2.;
    assert_eq!(
        &render(vec![c], 64)[..8],
        &[0., 0., 0.75, -0.75, 1.5, -1.5, 1.5, -1.5]
    );
}
#[test]
fn invalid_and_missing_assets() {
    let mut c = clip();
    c.gain = f32::NAN;
    assert!(AudioSchedule::build(vec![], vec![c]).is_err());
    let (s, w) = AudioSchedule::build(
        vec![AudioAsset {
            id: 1,
            source_rate: 48000,
            source_frames: 6,
            decoded: None,
        }],
        vec![clip()],
    )
    .unwrap();
    assert_eq!(w.len(), 1);
    assert!(s.validate_loop(None).is_ok());
    let (mut controller, _) = engine(EngineConfig {
        sample_rate: 48000.,
    });
    let before = controller.state().clone();
    let song = SongData {
        audio: s,
        ..SongData::default()
    };
    assert_eq!(controller.set_song(song), Err(ControlError::Rejected));
    assert_eq!(controller.state(), &before);
}
fn long_state(rate: u32, frames: usize) -> EngineState {
    let asset = AudioAsset {
        id: 1,
        source_rate: rate,
        source_frames: frames as u64,
        decoded: Some(Arc::new(SampleBuf {
            rate: rate as f32,
            l: (0..frames).map(|i| i as f32 / frames as f32).collect(),
            r: vec![0.; frames],
        })),
    };
    let mut c = clip();
    c.source_start = 0;
    c.source_end = frames as u64;
    let mut s = EngineState {
        bpm: 60.,
        playback_mode: PlaybackMode::Song,
        ..EngineState::default()
    };
    s.song.audio = AudioSchedule::build(vec![asset], vec![c]).unwrap().0;
    s.song.end_tick = 1920;
    s
}
#[test]
fn tempo_and_rate_preserve_seconds_and_remaining_progress() {
    let s = long_state(48000, 48000);
    let baseline = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s.clone(),
        &[(0, Command::Play)],
        60000,
        64,
    );
    let changed = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s.clone(),
        &[(0, Command::Play), (12032, Command::SetTempo { bpm: 180. })],
        60000,
        127,
    );
    assert_eq!(baseline, changed);
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
    );
    c.send(Command::Play).unwrap();
    let mut out = vec![0.; 24000 * 2];
    p.process(&mut out, 2);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96000.,
        },
    );
    let mut next = [0.; 128];
    p.process(&mut next, 2);
    assert!((next[0] - 0.5).abs() < 1e-6, "{}", next[0]);
}
#[test]
fn seek_and_loop_carry_and_atomic_refusal() {
    let mut s = long_state(48000, 48000);
    s.song.end_tick = 2000;
    let out = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s.clone(),
        &[(0, Command::Seek { tick: 480 }), (0, Command::Play)],
        64,
        64,
    );
    assert_eq!(out[0], 0.5);
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
    );
    let before = c.state().clone();
    assert!(
        c.send(Command::SetSongLoop {
            region: Some(SongLoop {
                start_tick: 0,
                end_tick: 1
            })
        })
        .is_err()
    );
    assert_eq!(c.state(), &before);
    c.send(Command::SetSongLoop {
        region: Some(SongLoop {
            start_tick: 0,
            end_tick: 960,
        }),
    })
    .unwrap();
    c.send(Command::SetTempo { bpm: 400. }).unwrap();
    c.send(Command::Play).unwrap();
    let mut out = vec![0.; 20000 * 2];
    p.process(&mut out, 2);
    assert!(out[16000 * 2] > 0.5);
}
#[test]
fn provenance_time_mapping_and_one_frame() {
    let mut c = clip();
    c.source_start = 0;
    c.source_end = 4;
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 4,
        decoded: Some(Arc::new(SampleBuf {
            rate: 96000.,
            l: vec![1., 2., 3., 4.],
            r: vec![1., 2., 3., 4.],
        })),
    };
    let (audio, w) = AudioSchedule::build(vec![a], vec![c]).unwrap();
    assert_eq!(w.len(), 1);
    let mut s = long_state(48000, 48000);
    s.song.audio = audio;
    let out = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
        &[(0, Command::Play)],
        8,
        1,
    );
    assert_eq!(
        out,
        [
            1., 1., 3., 3., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0.
        ]
    );
    c.source_start = 2;
    c.source_end = 3;
    c.reverse = true;
    assert_eq!(&render(vec![c], 1)[..4], &[2., -2., 0., 0.]);
}
#[test]
fn mixed_rates_have_exact_end_exclusive_duration_and_nonzero_onset() {
    for rate in [44100, 48000, 96000] {
        let mut s = long_state(rate, rate as usize);
        s.bpm = 120.;
        let out = render_offline_with(
            EngineConfig {
                sample_rate: 48000.,
            },
            s,
            &[(0, Command::Play)],
            48100,
            63,
        );
        let pos = 47999.0 * f64::from(rate) / 48000.0;
        let i = pos as usize;
        let a = i as f32 / rate as f32;
        let b = if i + 1 < rate as usize {
            (i + 1) as f32 / rate as f32
        } else {
            0.
        };
        assert_eq!(out[47999 * 2], a + (b - a) * (pos - i as f64) as f32);
        assert_eq!(out[48000 * 2], 0.);
    }
    let mut c = clip();
    c.start_tick = 1;
    let out = render(vec![c], 1); // default 60? EngineState default140: ceil(48000*60/(140*960))=22
    assert!(out[..44].iter().all(|v| *v == 0.));
    assert_eq!(out[44], 1.);
}
#[test]
fn stable_clip_order_and_capacity_refusal() {
    let c = clip();
    let mut second = c;
    second.id = 2;
    assert_eq!(render(vec![second, c], 64), render(vec![c, second], 127));
    let clips = (0..257).map(|id| AudioClip { id, ..c }).collect();
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 6,
        decoded: None,
    };
    assert!(AudioSchedule::build(vec![a.clone()], clips).is_err());
    assert!(
        AudioSchedule::build(
            vec![a],
            vec![AudioClip {
                source_end: u64::MAX,
                ..c
            }]
        )
        .is_err()
    );
}
#[test]
fn hundred_thousand_clips_control_and_sparse_callback_probe() {
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 1,
        decoded: None,
    };
    let c = AudioClip {
        source_start: 0,
        source_end: 1,
        ..clip()
    };
    let start = std::time::Instant::now();
    let (audio, _) = AudioSchedule::build(
        vec![a],
        (0..100000)
            .map(|i| AudioClip {
                id: i,
                start_tick: u64::from(i) * 10,
                ..c
            })
            .collect(),
    )
    .unwrap();
    eprintln!("100k clip build {:?}", start.elapsed());
    let mut s = EngineState {
        playback_mode: PlaybackMode::Song,
        ..EngineState::default()
    };
    s.song.audio = audio;
    s.song.end_tick = 1000000;
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
    );
    c.send(Command::Play).unwrap();
    p.process(&mut [], 2);
    let mut out = [0.; 256];
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        p.process(&mut out, 2);
    }
    eprintln!("1000 sparse callbacks {:?}", start.elapsed());
    assert_eq!(out, [0.; 256]);
}
#[test]
fn capacity_at_adjacent_half_open_endpoints_never_drops_onsets() {
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 15,
        decoded: Some(Arc::new(SampleBuf {
            rate: 48000.,
            l: vec![0.001; 15],
            r: vec![0.; 15],
        })),
    };
    // At 400 BPM a tick is 7.5 frames; two ticks are exactly 15 frames.
    let base = AudioClip {
        source_start: 0,
        source_end: 15,
        ..clip()
    };
    let clips = (0..256)
        .map(|i| AudioClip {
            id: i,
            start_tick: if i < 128 { 0 } else { 2 },
            ..base
        })
        .collect();
    let audio = AudioSchedule::build(vec![a], clips).unwrap().0;
    let mut s = EngineState {
        bpm: 400.,
        playback_mode: PlaybackMode::Song,
        ..EngineState::default()
    };
    s.song.audio = audio;
    s.song.end_tick = 4;
    let out = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
        &[(0, Command::Play)],
        32,
        64,
    );
    assert_eq!(out[0], out[15 * 2]);
    assert!(out[15 * 2] > 0.12);
    assert_eq!(out[30 * 2], 0.);
}
#[test]
fn generated_sine_resampling_and_slice_reference() {
    for rate in [44100, 48000, 96000] {
        let l: Vec<f32> = (0..1000)
            .map(|i| libm::sin(i as f64 * 0.03) as f32)
            .collect();
        let a = AudioAsset {
            id: 1,
            source_rate: rate,
            source_frames: 1000,
            decoded: Some(Arc::new(SampleBuf {
                rate: rate as f32,
                l: l.clone(),
                r: l.clone(),
            })),
        };
        let c = AudioClip {
            source_start: 10,
            source_end: 900,
            ..clip()
        };
        let mut s = EngineState {
            playback_mode: PlaybackMode::Song,
            ..EngineState::default()
        };
        s.song.end_tick = 960;
        s.song.audio = AudioSchedule::build(vec![a], vec![c]).unwrap().0;
        let out = render_offline_with(
            EngineConfig {
                sample_rate: 48000.,
            },
            s,
            &[(0, Command::Play)],
            400,
            127,
        );
        for i in 0..400 {
            let pos = 10.0 + i as f64 / 48000.0 * f64::from(rate);
            let k = pos as usize;
            let expected = l[k] + (l[k + 1] - l[k]) * (pos - k as f64) as f32;
            assert_eq!(out[i * 2], expected);
            assert_eq!(out[i * 2 + 1], expected);
        }
    }
    let mut a = clip();
    a.source_end = 3;
    let mut b = clip();
    b.source_start = 3;
    let whole = render(vec![clip()], 1);
    let first = render(vec![a], 1);
    let second = render(vec![b], 1);
    assert_eq!(&whole[..8], [&first[..4], &second[..4]].concat());
}
#[test]
fn nonzero_loop_start_and_carry_are_fixed_seconds() {
    let mut s = long_state(48000, 48000);
    s.bpm = 120.;
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 48000,
        decoded: Some(Arc::new(SampleBuf {
            rate: 48000.,
            l: (0..48000).map(|i| i as f32 / 48000.).collect(),
            r: vec![0.; 48000],
        })),
    };
    let c = AudioClip {
        start_tick: 240,
        source_start: 0,
        source_end: 48000,
        ..clip()
    };
    s.song.audio = AudioSchedule::build(vec![a], vec![c]).unwrap().0;
    s.song_loop = Some(SongLoop {
        start_tick: 240,
        end_tick: 480,
    });
    let out = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
        &[(0, Command::Play)],
        13000,
        63,
    );
    assert_eq!(out[6000 * 2], 0.125);
    assert_eq!(out[12000 * 2], 0.375);
}
#[test]
fn audio_only_export_has_exact_frame_horizon() {
    let mut s = long_state(48000, 48000);
    s.song.end_tick = 1;
    let d = tempfile::tempdir().unwrap();
    let options = ExportOptions {
        format: WavFormat::Float32,
        max_tail_seconds: 0.,
        ..ExportOptions::default()
    };
    let report = export_wav(
        s,
        &options,
        &d.path().join("audio.wav"),
        &mut |_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.frames, 48000);
}
#[test]
fn malformed_decoded_buffers_and_provenance_are_refused() {
    for b in [
        SampleBuf {
            rate: f32::NAN,
            l: vec![0.],
            r: vec![0.],
        },
        SampleBuf {
            rate: 48000.,
            l: vec![0.],
            r: vec![],
        },
        SampleBuf {
            rate: 48000.,
            l: vec![f32::NAN],
            r: vec![0.],
        },
    ] {
        let a = AudioAsset {
            id: 1,
            source_rate: 48000,
            source_frames: 6,
            decoded: Some(Arc::new(b)),
        };
        assert!(AudioSchedule::build(vec![a], vec![clip()]).is_err());
    }
    for rate in [0, 7999, 384001, u32::MAX] {
        assert!(
            AudioSchedule::build(
                vec![AudioAsset {
                    id: 1,
                    source_rate: rate,
                    source_frames: 6,
                    decoded: None
                }],
                vec![clip()]
            )
            .is_err()
        );
    }
}
#[test]
fn saturated_audio_callback_probe() {
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 48000,
        decoded: Some(Arc::new(SampleBuf {
            rate: 48000.,
            l: vec![0.001; 48000],
            r: vec![-0.001; 48000],
        })),
    };
    let c = AudioClip {
        source_start: 0,
        source_end: 48000,
        ..clip()
    };
    let audio = AudioSchedule::build(vec![a], (0..256).map(|id| AudioClip { id, ..c }).collect())
        .unwrap()
        .0;
    let mut s = EngineState {
        playback_mode: PlaybackMode::Song,
        ..EngineState::default()
    };
    s.song.audio = audio;
    s.song.end_tick = 9600;
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
    );
    c.send(Command::Play).unwrap();
    let mut out = [0.; 256];
    let start = std::time::Instant::now();
    for _ in 0..300 {
        p.process(&mut out, 2);
    }
    eprintln!("300 saturated callbacks {:?}", start.elapsed());
    assert!(out[0] > 0.25);
    assert_eq!(out[0], -out[1]);
}
#[test]
fn one_frame_reverse_and_forward_resampling_match() {
    let a = AudioAsset {
        id: 1,
        source_rate: 8000,
        source_frames: 1,
        decoded: Some(Arc::new(SampleBuf {
            rate: 8000.,
            l: vec![1.],
            r: vec![1.],
        })),
    };
    let render_one = |reverse| {
        let c = AudioClip {
            source_start: 0,
            source_end: 1,
            reverse,
            ..clip()
        };
        let mut s = EngineState {
            playback_mode: PlaybackMode::Song,
            ..EngineState::default()
        };
        s.song.end_tick = 960;
        s.song.audio = AudioSchedule::build(vec![a.clone()], vec![c]).unwrap().0;
        render_offline_with(
            EngineConfig {
                sample_rate: 48000.,
            },
            s,
            &[(0, Command::Play)],
            8,
            1,
        )
    };
    let forward = render_one(false);
    let reverse = render_one(true);
    for (a, b) in forward.iter().zip(reverse.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
    assert_eq!(forward[12], 0.);
}
#[test]
fn trimmed_reverse_progress_survives_rate_and_tempo_change() {
    let mut s = long_state(48000, 48000);
    let a = AudioAsset {
        id: 1,
        source_rate: 48000,
        source_frames: 48000,
        decoded: Some(Arc::new(SampleBuf {
            rate: 48000.,
            l: (0..48000).map(|i| i as f32 / 48000.).collect(),
            r: vec![0.; 48000],
        })),
    };
    let c = AudioClip {
        source_start: 12000,
        source_end: 36000,
        reverse: true,
        ..clip()
    };
    s.song.audio = AudioSchedule::build(vec![a], vec![c]).unwrap().0;
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
    );
    c.send(Command::Play).unwrap();
    let mut out = vec![0.; 6144 * 2];
    p.process(&mut out, 2);
    c.send(Command::SetTempo { bpm: 400. }).unwrap();
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96000.,
        },
    );
    let mut next = [0.; 128];
    p.process(&mut next, 2);
    assert_eq!(next[0], (35999 - 6144) as f32 / 48000.);
    let pos = (35999 - 6144) as f64 - 0.5;
    let k = pos as usize;
    let expected = k as f32 / 48000. + ((k + 1) as f32 / 48000. - k as f32 / 48000.) * 0.5;
    assert_eq!(next[2], expected);
}
#[test]
fn loop_boundary_inside_clip_restarts_at_source_time_with_bounded_carry() {
    let mut s = long_state(48000, 48000);
    s.bpm = 120.;
    s.song_loop = Some(SongLoop {
        start_tick: 240,
        end_tick: 480,
    });
    let out = render_offline_with(
        EngineConfig {
            sample_rate: 48000.,
        },
        s,
        &[(0, Command::Play)],
        13000,
        63,
    );
    assert_eq!(out[0], 0.125);
    assert_eq!(out[6000 * 2], 0.375);
}
