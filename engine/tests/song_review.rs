//! Independent review scenarios promoted to runtime regressions.
use grooviq_engine::*;
use std::sync::Arc;

const CFG: EngineConfig = EngineConfig {
    sample_rate: 48000.0,
};

fn sustained_channel(len: usize) -> ChannelSetup {
    let mut params: Vec<_> = grooviq_dsp::sampler::SPEC.iter().map(|p| p.2).collect();
    params[7] = 0.0;
    params[11] = 0.0;
    params[14] = 0.0;
    ChannelSetup::sampler(
        ChannelId(1),
        Some(Arc::new(SampleBuf {
            rate: 48000.0,
            l: vec![0.2; len],
            r: vec![0.2; len],
        })),
        params,
        ChannelMix::default(),
    )
}

fn note(tick: u64, length: u64) -> SongNote {
    SongNote {
        tick,
        length,
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        slide: false,
        clip_id: 1,
    }
}

fn song_state(notes: Vec<SongNote>, end_tick: u64) -> EngineState {
    EngineState {
        bpm: 120.0,
        channels: vec![sustained_channel(200_000)],
        playback_mode: PlaybackMode::Song,
        song: SongData {
            arrangement: ArrangementData {
                length: end_tick,
                notes,
            },
            end_tick,
            end_frame: 0,
            frame_rate: 48000.0,
            frame_extent: None,
            audio: AudioSchedule::default(),
            automation: Default::default(),
        },
        ..EngineState::default()
    }
}

fn last_loud(out: &[f32]) -> Option<usize> {
    out.chunks(2).rposition(|f| f[0].abs() > 0.05)
}

/// Lead candidate 1: a held Song note across a tempo change. 120 BPM: 1 beat = 24000 frames.
/// Two-beat note from tick 0; SetTempo 60 at frame 24000 (one beat in). Musically the note
/// ends one beat later at 60 BPM = frame 72000. Prints where it actually ends.
#[test]
fn probe_song_held_note_across_tempo_change() {
    let s = song_state(vec![note(0, 1920)], 3840);
    let events = [(0, Command::Play), (24000, Command::SetTempo { bpm: 60.0 })];
    let out = render_offline_with(CFG, s, &events, 100_000, 64);
    assert!((71999..72032).contains(&last_loud(&out).unwrap()));
    eprintln!(
        "SONG tempo probe: last loud frame = {:?} (musical end 72000)",
        last_loud(&out)
    );
}

/// Same for the existing Pattern path (inherited?).
#[test]
fn probe_pattern_held_note_across_tempo_change() {
    let mut s = song_state(vec![], 3840);
    s.playback_mode = PlaybackMode::Pattern;
    s.pattern = PatternData {
        length: 3840,
        notes: vec![SeqNote {
            tick: 0,
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            length: 1920,
            slide: false,
        }],
    };
    let events = [(0, Command::Play), (24000, Command::SetTempo { bpm: 60.0 })];
    let out = render_offline_with(CFG, s, &events, 100_000, 64);
    assert!((71999..72032).contains(&last_loud(&out).unwrap()));
    eprintln!(
        "PATTERN tempo probe: last loud frame = {:?} (musical end 72000)",
        last_loud(&out)
    );
}

/// Candidate 2: transport's pattern length goes stale when the pattern changes length while
/// Song mode is selected; Pattern recording then places notes modulo the old length.
#[test]
fn probe_pattern_length_change_in_song_mode_then_record() {
    let mut s = song_state(vec![], 3840);
    s.playback_mode = PlaybackMode::Song;
    s.pattern = PatternData {
        length: 3840,
        notes: vec![],
    };
    let (mut c, mut p) = engine_with(CFG, s);
    p.process(&mut [0.0; 128], 2);
    c.set_pattern(PatternData {
        length: 7680,
        notes: vec![],
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Pattern,
    })
    .unwrap();
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    // 120 BPM @ 48k: 25 frames per tick. Tick 5000 = frame 125000 after play.
    let mut buf = vec![0.0f32; 125_000 * 2];
    p.process(&mut buf, 2);
    let id = NoteId::live();
    c.send(Command::ChannelNote {
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        on: true,
        id,
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut rec = Vec::new();
    c.take_recorded(&mut rec);
    let pos = rec.iter().find_map(|e| match e {
        RecordEvent::Note {
            on: true,
            tick,
            position,
            ..
        } => Some((*tick, *position)),
        _ => None,
    });
    eprintln!(
        "RECORD probe after Song-mode length change: (tick, position) = {pos:?}; expected position ~5000 in 7680-tick pattern"
    );
    let (tick, position) = pos.unwrap();
    assert_eq!(tick, 5002);
    assert_eq!(position, 5002, "stale pattern length");
}

/// Control: same sequence without Song mode in between.
#[test]
fn probe_pattern_length_change_in_pattern_mode_then_record() {
    let mut s = song_state(vec![], 3840);
    s.playback_mode = PlaybackMode::Pattern;
    s.pattern = PatternData {
        length: 3840,
        notes: vec![],
    };
    let (mut c, mut p) = engine_with(CFG, s);
    p.process(&mut [0.0; 128], 2);
    c.set_pattern(PatternData {
        length: 7680,
        notes: vec![],
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    let mut buf = vec![0.0f32; 125_000 * 2];
    p.process(&mut buf, 2);
    c.send(Command::ChannelNote {
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        on: true,
        id: NoteId::live(),
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut rec = Vec::new();
    c.take_recorded(&mut rec);
    let (tick, position) = rec
        .iter()
        .find_map(|e| match e {
            RecordEvent::Note {
                on: true,
                tick,
                position,
                ..
            } => Some((*tick, *position)),
            _ => None,
        })
        .unwrap();
    assert_eq!(u64::from(position), tick % 7680);
}

/// Region export starts once at the region start and excludes notes before it (onset-only).
#[test]
fn probe_region_export_onset_only_and_exact() {
    let d = tempfile::tempdir().unwrap();
    // A note starting before the region and spanning into it, and one inside.
    let s = song_state(vec![note(0, 960), note(600, 120)], 960);
    let options = ExportOptions {
        format: WavFormat::Float32,
        max_tail_seconds: 0.0,
        song_region: Some(SongLoop {
            start_tick: 480,
            end_tick: 960,
        }),
        ..ExportOptions::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let path = d.path().join("r.wav");
    let report = export_wav(s, &options, &path, &mut |_| {}, &cancel).unwrap();
    assert_eq!(report.frames, 12000);
    let bytes = std::fs::read(&path).unwrap();
    let data: Vec<f32> = bytes[bytes.len() - 12000 * 8..]
        .chunks(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    // Before tick 600 (frame 3000 rel.) silent: the pre-region note must not play.
    assert!(
        data[..2900 * 2].iter().all(|x| x.abs() < 1e-6),
        "pre-region note resurrected"
    );
    assert!(data[3100 * 2].abs() > 0.1);
    // Note 600..720 ends at frame 6000 rel.
    assert!(data[6100 * 2].abs() < 1e-3);
}

/// Seeking while stopped is retained; Play starts there; Stop rewinds to 0.
#[test]
fn probe_stopped_seek_retained_then_stop_rewinds() {
    let s = song_state(vec![note(480, 120)], 960);
    let (mut c, mut p) = engine_with(CFG, s);
    c.send(Command::Seek { tick: 480 }).unwrap();
    p.process(&mut [0.0; 128], 2);
    assert_eq!(c.live().position_ticks, 480);
    assert!(!c.live().playing);
    c.send(Command::Play).unwrap();
    let mut buf = vec![0.0f32; 256 * 2];
    p.process(&mut buf, 2);
    assert!(
        buf[2..].iter().any(|x| x.abs() > 0.1),
        "note at seek target did not start"
    );
    c.send(Command::Stop).unwrap();
    p.process(&mut [0.0; 128], 2);
    assert_eq!(c.live().position_ticks, 0);
}

/// Loop change while playing outside the new region jumps into it; seek outside a loop clamps.
#[test]
fn probe_loop_change_while_playing_and_seek_clamp() {
    let s = song_state(vec![note(0, 60), note(600, 60)], 960);
    let (mut c, mut p) = engine_with(CFG, s);
    c.send(Command::Play).unwrap();
    p.process(&mut vec![0.0; 2000 * 2], 2);
    c.send(Command::SetSongLoop {
        region: Some(SongLoop {
            start_tick: 480,
            end_tick: 720,
        }),
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    let pos = c.live().position_ticks;
    assert!((480..720).contains(&pos), "{pos}");
    c.send(Command::Seek { tick: 10 }).unwrap();
    p.process(&mut [0.0; 128], 2);
    let pos = c.live().position_ticks;
    assert!(
        (480..490).contains(&pos),
        "seek not clamped into loop: {pos}"
    );
    // Play 5 loops: count onsets of the tick-600 note.
    let mut buf = vec![0.0f32; 6000 * 5 * 2];
    p.process(&mut buf, 2);
    let mut onsets = 0;
    let mut was = false;
    for f in buf.chunks(2) {
        let on = f[0].abs() > 0.1;
        if on && !was {
            onsets += 1;
        }
        was = on;
    }
    eprintln!("loop onsets over 5 loop lengths: {onsets}");
    assert!((4..=6).contains(&onsets));
    assert!(c.live().playing);
}

/// Mode change while a Song note is held releases it; a repeated mode command is a no-op.
#[test]
fn probe_mode_change_releases_song_note() {
    let s = song_state(vec![note(0, 1920)], 3840);
    let events = [
        (0, Command::Play),
        (
            6400,
            Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            },
        ),
        (
            12800,
            Command::SetPlaybackMode {
                mode: PlaybackMode::Pattern,
            },
        ),
    ];
    let out = render_offline_with(CFG, s, &events, 20000, 64);
    assert!(out[10000 * 2].abs() > 0.1, "same-mode command cut the note");
    assert!(
        out[14000 * 2].abs() < 1e-3,
        "mode change left the song note sounding"
    );
}

fn onsets(buf: &[f32]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut was = false;
    for (i, f) in buf.chunks(2).enumerate() {
        let on = f[0].abs() > 0.1;
        if on && !was {
            v.push(i);
        }
        was = on;
    }
    v
}

/// A song swap (and a pattern swap) mid-loop after several loop passes neither skips nor
/// repeats the loop's note.
#[test]
fn probe_swaps_mid_loop_after_many_passes() {
    let notes = vec![note(480, 30), note(600, 30)];
    let mut s = song_state(notes.clone(), 960);
    s.song_loop = Some(SongLoop {
        start_tick: 480,
        end_tick: 720,
    });
    let (mut c, mut p) = engine_with(CFG, s.clone());
    c.send(Command::Play).unwrap();
    // 240 ticks = 6000 frames per pass. Run 4.5 passes.
    let mut a = vec![0.0f32; 26880 * 2];
    p.process(&mut a, 2);
    c.set_song(s.song.clone()).unwrap();
    c.set_pattern(PatternData::default()).unwrap();
    let mut b = vec![0.0f32; 12000 * 2];
    p.process(&mut b, 2);
    let mut all = a.clone();
    all.extend_from_slice(&b);
    let on = onsets(&all);
    eprintln!("onsets across swap: {on:?}");
    // Expected: every 3000 frames (notes at 480 and 600 alternate), from frame 0.
    let expected: Vec<usize> = (0..13).map(|i| i * 3000).collect();
    assert_eq!(on, expected);
}

/// Exporting an empty Song (no content at all) with no tail.
#[test]
fn probe_empty_song_export() {
    let d = tempfile::tempdir().unwrap();
    let s = song_state(vec![], 0);
    let options = ExportOptions {
        format: WavFormat::Float32,
        max_tail_seconds: 0.0,
        ..ExportOptions::default()
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let r = export_wav(s, &options, &d.path().join("e.wav"), &mut |_| {}, &cancel);
    eprintln!("empty song export: {:?}", r.as_ref().map(|r| r.frames));
}

/// Whole-song export content ignores a live loop (content, not only length).
#[test]
fn probe_whole_export_content_ignores_live_loop() {
    let d = tempfile::tempdir().unwrap();
    let mut s = song_state(vec![note(240, 60), note(720, 60)], 960);
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
    let path = d.path().join("w.wav");
    export_wav(s, &options, &path, &mut |_| {}, &cancel).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let data: Vec<f32> = bytes[bytes.len() - 24000 * 8..]
        .chunks(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(onsets(&data), vec![6000, 18000]);
}

/// Seek while playing releases the sequenced note it leaves behind.
#[test]
fn probe_seek_releases_sequenced_note() {
    let s = song_state(vec![note(0, 960), note(900, 30)], 960);
    let events = [(0, Command::Play), (6400, Command::Seek { tick: 700 })];
    let out = render_offline_with(CFG, s, &events, 9000, 64);
    assert!(out[6000 * 2].abs() > 0.1);
    assert!(
        out[6500 * 2].abs() < 1e-3,
        "seek left the old sequenced note sounding"
    );
}

#[test]
fn validation_guards_have_independent_fixtures() {
    let base = song_state(vec![note(0, 1)], 960);
    let (mut c, _) = engine_with(CFG, base.clone());
    c.set_song(base.song.clone()).unwrap();
    let invalid: Vec<SongData> = [
        {
            let mut s = base.song.clone();
            s.arrangement.notes[0].channel = ChannelId(9);
            s
        },
        {
            let mut s = base.song.clone();
            s.arrangement.notes = vec![note(2, 1), note(1, 1)];
            s
        },
        {
            let mut s = base.song.clone();
            s.arrangement.notes[0].velocity = 1.1;
            s
        },
        {
            let mut s = base.song.clone();
            s.end_frame = 46_080_000_001;
            s
        },
        {
            let mut s = base.song.clone();
            s.frame_rate = 7999.0;
            s
        },
        {
            let mut s = base.song.clone();
            s.frame_rate = 384001.0;
            s
        },
        {
            let mut s = base.song.clone();
            s.arrangement.notes = vec![note(0, 1); MAX_SONG_NOTES + 1];
            s
        },
    ]
    .into();
    for bad in invalid {
        assert_eq!(c.set_song(bad), Err(ControlError::Rejected));
        assert_eq!(c.state().song, base.song);
    }
    for region in [
        SongLoop {
            start_tick: 1,
            end_tick: 1,
        },
        SongLoop {
            start_tick: 0,
            end_tick: 961,
        },
    ] {
        assert!(
            c.send(Command::SetSongLoop {
                region: Some(region)
            })
            .is_err()
        );
    }
    assert!(c.send(Command::Seek { tick: 961 }).is_err());
    assert!(c.send(Command::PlayLoops { loops: 1 }).is_err());
    c.send(Command::SetSongLoop {
        region: Some(SongLoop {
            start_tick: 10,
            end_tick: 20,
        }),
    })
    .unwrap();
    let mut short = base.song.clone();
    short.arrangement.length = 15;
    short.end_tick = 15;
    assert_eq!(c.set_song(short), Err(ControlError::Rejected));
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Pattern,
    })
    .unwrap();
    assert!(c.send(Command::Seek { tick: 10 }).is_err());
}
#[test]
fn disarm_is_retained_when_pattern_returns_without_rearming() {
    let mut s = song_state(vec![], 960);
    s.playback_mode = PlaybackMode::Pattern;
    let (mut c, mut p) = engine_with(CFG, s);
    for cmd in [Command::SetRecording { on: true }, Command::Play] {
        c.send(cmd).unwrap();
    }
    p.process(&mut [0.0; 128], 2);
    for cmd in [
        Command::SetPlaybackMode {
            mode: PlaybackMode::Song,
        },
        Command::SetPlaybackMode {
            mode: PlaybackMode::Pattern,
        },
        Command::Play,
        Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            on: true,
            id: NoteId::live(),
        },
    ] {
        c.send(cmd).unwrap();
    }
    p.process(&mut [0.0; 128], 2);
    let mut events = Vec::new();
    c.take_recorded(&mut events);
    assert!(!events.iter().any(|e| matches!(e, RecordEvent::Note { .. })));
}
#[test]
fn empty_song_zero_tail_is_a_legal_zero_frame_wav() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.wav");
    std::fs::write(&path, b"prior").unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let options = ExportOptions {
        max_tail_seconds: 0.0,
        ..ExportOptions::default()
    };
    assert!(export_wav(song_state(vec![], 0), &options, &path, &mut |_| {}, &cancel).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"prior");
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        export_wav(song_state(vec![], 0), &options, &path, &mut |_| {}, &cancel)
            .unwrap()
            .frames,
        0
    );
    assert_eq!(hound::WavReader::open(path).unwrap().duration(), 0);
}
#[test]
fn accepted_recording_mirror_observes_stopped_disarm() {
    let mut s = song_state(vec![], 960);
    s.playback_mode = PlaybackMode::Pattern;
    let (mut c, _) = engine_with(CFG, s);
    c.send(Command::SetRecording { on: true }).unwrap();
    assert!(c.recording_armed());
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Pattern,
    })
    .unwrap();
    assert!(c.recording_armed());
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Song,
    })
    .unwrap();
    assert!(!c.recording_armed());
    assert!(c.send(Command::SetRecording { on: true }).is_err());
    assert!(!c.recording_armed());
}
#[test]
fn playback_clamps_and_restart_are_rendered_not_only_mirrored() {
    let mut s = song_state(vec![note(480, 30)], 960);
    s.song_loop = Some(SongLoop {
        start_tick: 480,
        end_tick: 720,
    });
    let (mut c, mut p) = engine_with(CFG, s);
    c.send(Command::Play).unwrap();
    let mut out = vec![0.0; 6000 * 6 * 2];
    p.process(&mut out, 2);
    assert_eq!(onsets(&out), vec![0, 6000, 12000, 18000, 24000, 30000]);
    assert!((480..720).contains(&c.live().position_ticks));
    let s = song_state(vec![note(0, 30)], 960);
    let out = render_offline_with(
        CFG,
        s,
        &[(0, Command::Seek { tick: 960 }), (0, Command::Play)],
        1000,
        63,
    );
    assert_eq!(onsets(&out), vec![0]);
}
#[test]
fn exact_frame_conversion_and_export_region_trim_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut s = song_state(vec![], 0);
    s.song.end_frame = 1001;
    s.song.frame_rate = 44100.0;
    let options = ExportOptions {
        format: WavFormat::Float32,
        max_tail_seconds: 0.0,
        ..ExportOptions::default()
    };
    let path = dir.path().join("frames.wav");
    assert_eq!(
        export_wav(s, &options, &path, &mut |_| {}, &cancel)
            .unwrap()
            .frames,
        1090
    );
    let s = song_state(vec![note(600, 120), note(900, 30)], 960);
    let options = ExportOptions {
        song_region: Some(SongLoop {
            start_tick: 480,
            end_tick: 720,
        }),
        ..options
    };
    export_wav(s, &options, &path, &mut |_| {}, &cancel).unwrap();
    let samples: Vec<f32> = hound::WavReader::open(path)
        .unwrap()
        .into_samples::<f32>()
        .map(Result::unwrap)
        .collect();
    assert_eq!(onsets(&samples), vec![3000]);
    assert_eq!(samples.len(), 12000);
}
#[test]
fn replacement_releases_old_song_and_restores_loop_after_rate_change() {
    let mut s = song_state(vec![note(480, 480)], 960);
    s.song_loop = Some(SongLoop {
        start_tick: 480,
        end_tick: 720,
    });
    let (mut c, mut p) = engine_with(CFG, s.clone());
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    s.song.arrangement.notes.clear();
    c.set_song(s.song).unwrap();
    let mut out = vec![0.0; 1000 * 2];
    p.process(&mut out, 2);
    assert!(out[300 * 2].abs() < 0.001);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96000.0,
        },
    );
    p.process(&mut vec![0.0; 30000 * 2], 2);
    assert!((480..720).contains(&c.live().position_ticks));
}
#[test]
fn configuring_song_loop_cannot_change_pattern_audio_or_cursor() {
    let mut s = song_state(vec![], 3840);
    s.playback_mode = PlaybackMode::Pattern;
    s.pattern.notes = vec![SeqNote {
        tick: 0,
        length: 1920,
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        slide: false,
    }];
    let plain = render_offline_with(CFG, s.clone(), &[(0, Command::Play)], 12000, 63);
    let changed = render_offline_with(
        CFG,
        s,
        &[
            (0, Command::Play),
            (
                6400,
                Command::SetSongLoop {
                    region: Some(SongLoop {
                        start_tick: 480,
                        end_tick: 720,
                    }),
                },
            ),
        ],
        12000,
        127,
    );
    assert_eq!(plain, changed);
}
#[test]
fn leaving_pattern_flushes_its_pending_release_in_song_mode() {
    let mut s = song_state(vec![], 3840);
    s.playback_mode = PlaybackMode::Pattern;
    s.pattern.notes = vec![SeqNote {
        tick: 0,
        length: 1920,
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        slide: false,
    }];
    let out = render_offline_with(
        CFG,
        s,
        &[
            (0, Command::Play),
            (
                6400,
                Command::SetPlaybackMode {
                    mode: PlaybackMode::Song,
                },
            ),
        ],
        9000,
        511,
    );
    assert!(out[6000 * 2] > 0.1);
    assert!(out[6500 * 2].abs() < 0.001);
}
#[test]
fn changed_tempo_releases_are_identical_across_callback_partitions() {
    for mode in [PlaybackMode::Pattern, PlaybackMode::Song] {
        for (bpm, next, at) in [(120.0, 60.0, 24000), (60.0, 120.0, 48000)] {
            let mut s = song_state(vec![note(0, 1920)], 3840);
            s.bpm = bpm;
            s.playback_mode = mode;
            s.pattern.notes = vec![SeqNote {
                tick: 0,
                length: 1920,
                channel: ChannelId(1),
                key: 60,
                velocity: 1.0,
                slide: false,
            }];
            let events = [(0, Command::Play), (at, Command::SetTempo { bpm: next })];
            let expected = render_offline_with(CFG, s.clone(), &events, 80000, 64);
            assert!((71999..72032).contains(&last_loud(&expected).unwrap()));
            for callback in [1, 63, 64, 127, 128, 255, 256, 511] {
                assert_eq!(
                    render_offline_with(CFG, s.clone(), &events, 80000, callback),
                    expected,
                    "{mode:?} {bpm} {callback}"
                );
            }
        }
    }
}
#[test]
fn loading_song_disarms_recording_before_returning_to_pattern() {
    let mut s = song_state(vec![], 960);
    s.playback_mode = PlaybackMode::Pattern;
    let (mut c, mut p) = engine_with(CFG, s);
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.load_state(song_state(vec![], 960)).unwrap();
    p.process(&mut [0.0; 128], 2);
    assert!(!c.recording_armed());
    for cmd in [
        Command::SetPlaybackMode {
            mode: PlaybackMode::Pattern,
        },
        Command::Play,
        Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            on: true,
            id: NoteId::live(),
        },
    ] {
        c.send(cmd).unwrap();
    }
    p.process(&mut [0.0; 128], 2);
    let mut events = Vec::new();
    c.take_recorded(&mut events);
    assert!(!events.iter().any(|e| matches!(e, RecordEvent::Note { .. })));
}
#[test]
fn graph_replacement_cannot_leave_a_song_take_armed() {
    let mut s = song_state(vec![], 960);
    s.playback_mode = PlaybackMode::Pattern;
    let (mut c, mut p) = engine_with(CFG, s);
    c.send(Command::SetRecording { on: true }).unwrap();
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.load_state(song_state(vec![], 960)).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::Play).unwrap();
    p.process(&mut [0.0; 128], 2);
    c.send(Command::Stop).unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut events = Vec::new();
    c.take_recorded(&mut events);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, RecordEvent::End { .. }))
            .count(),
        1
    );
}
#[test]
fn removing_a_recompiled_channel_does_not_spend_its_replacement_event_budget() {
    let s = song_state(vec![note(0, 1920); 1024], 3840);
    let (mut c, mut p) = engine_with(CFG, s);
    c.send(Command::Play).unwrap();
    p.process(&mut vec![0.0; 512 * 2], 2);
    c.set_song(song_state(vec![], 3840).song).unwrap();
    c.remove_channel(ChannelId(1)).unwrap();
    c.add_channel(sustained_channel(200000)).unwrap();
    c.set_song(song_state(vec![note(24, 120)], 3840).song)
        .unwrap();
    let mut out = vec![0.0; 1000 * 2];
    p.process(&mut out, 2);
    assert!(
        out[400 * 2] > 0.1,
        "stale releases consumed replacement channel capacity"
    );
}
#[test]
fn song_pattern_resize_then_rate_change_keeps_recording_pattern_length() {
    let (mut c, mut p) = engine_with(CFG, song_state(vec![], 3840));
    c.set_pattern(PatternData {
        length: 7680,
        notes: vec![],
    })
    .unwrap();
    c.send(Command::SetPlaybackMode {
        mode: PlaybackMode::Song,
    })
    .unwrap();
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96000.0,
        },
    );
    for cmd in [
        Command::SetPlaybackMode {
            mode: PlaybackMode::Pattern,
        },
        Command::SetRecording { on: true },
        Command::Play,
    ] {
        c.send(cmd).unwrap();
    }
    p.process(&mut vec![0.0; 250000 * 2], 2);
    c.send(Command::ChannelNote {
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        on: true,
        id: NoteId::live(),
    })
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut events = Vec::new();
    c.take_recorded(&mut events);
    let (tick, position) = events
        .iter()
        .find_map(|e| {
            if let RecordEvent::Note {
                on: true,
                tick,
                position,
                ..
            } = e
            {
                Some((*tick, *position))
            } else {
                None
            }
        })
        .unwrap();
    assert!(tick >= 5000);
    assert_eq!(u64::from(position), tick % 7680);
}

#[test]
fn direct_mode_transition_survives_rebuild_with_take_and_barrier() {
    for rebuild in [false, true] {
        let mut s = song_state(vec![], 96000);
        s.playback_mode = PlaybackMode::Pattern;
        s.pattern = PatternData {
            length: 3840,
            notes: vec![SeqNote {
                tick: 0,
                length: 1920,
                channel: ChannelId(1),
                key: 60,
                velocity: 1.0,
                slide: false,
            }],
        };
        let (mut c, mut p) = engine_with(CFG, s);
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        c.send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            on: true,
            id: NoteId(999),
        })
        .unwrap();
        let mut out = [0.0; 128];
        p.process(&mut out, 2);
        assert!(out.iter().any(|x| x.abs() > 0.05));
        assert!(c.live().playing);
        c.send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            on: false,
            id: NoteId(999),
        })
        .unwrap();
        p.process(&mut out, 2);
        c.send_marked(Command::SetPlaybackMode {
            mode: PlaybackMode::Song,
        })
        .unwrap();
        let mark = c.record_mark();
        if rebuild {
            c.reconfigure(
                &mut p,
                EngineConfig {
                    sample_rate: 44100.0,
                },
            );
        }
        p.process(&mut out, 2);
        assert!(!c.live().playing, "rebuild={rebuild}");
        assert!(!c.recording_armed());
        for _ in 0..100 {
            p.process(&mut out, 2);
        }
        assert!(out.iter().all(|x| x.abs() < 0.001));
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, RecordEvent::Note { on: true, .. }))
        );
        let end = events
            .iter()
            .position(|e| matches!(e, RecordEvent::End { .. }))
            .unwrap();
        let barrier = events
            .iter()
            .position(|e| matches!(e, RecordEvent::Mark { id } if *id == mark))
            .unwrap();
        assert!(end < barrier);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, RecordEvent::End { .. }))
                .count(),
            1
        );
    }
}

#[test]
fn direct_mode_round_trip_and_refusal_keep_take_order() {
    for rebuild in [false, true] {
        let mut s = song_state(vec![], 96000);
        s.playback_mode = PlaybackMode::Pattern;
        let (mut c, mut p) = engine_with(CFG, s);
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        p.process(&mut [0.0; 128], 2);
        let before = c.live().position_ticks;
        c.send(Command::SetPlaybackMode {
            mode: PlaybackMode::Pattern,
        })
        .unwrap();
        if rebuild {
            c.reconfigure(
                &mut p,
                EngineConfig {
                    sample_rate: 44100.0,
                },
            );
        }
        p.process(&mut [0.0; 128], 2);
        assert!(c.live().playing);
        assert!(c.recording_armed());
        assert!(c.live().position_ticks >= before);
        while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
        assert_eq!(
            c.send_at(
                0,
                Command::SetPlaybackMode {
                    mode: PlaybackMode::Song
                }
            ),
            Err(ControlError::Full)
        );
        assert_eq!(c.state().playback_mode, PlaybackMode::Pattern);
        assert!(c.recording_armed());
        p.process(&mut [0.0; 128], 2);
        let first = c
            .send_marked(Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            })
            .unwrap();
        c.send(Command::SetPlaybackMode {
            mode: PlaybackMode::Song,
        })
        .unwrap();
        c.send(Command::SetPlaybackMode {
            mode: PlaybackMode::Pattern,
        })
        .unwrap();
        c.send(Command::SetRecording { on: true }).unwrap();
        c.send(Command::Play).unwrap();
        c.send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 61,
            velocity: 1.0,
            on: true,
            id: NoteId(998),
        })
        .unwrap();
        let last = c
            .send_marked(Command::SetPlaybackMode {
                mode: PlaybackMode::Song,
            })
            .unwrap();
        c.send(Command::SetPlaybackMode {
            mode: PlaybackMode::Pattern,
        })
        .unwrap();
        if rebuild {
            c.reconfigure(
                &mut p,
                EngineConfig {
                    sample_rate: 44100.0,
                },
            );
        }
        p.process(&mut [0.0; 128], 2);
        assert!(!c.live().playing);
        assert!(!c.recording_armed());
        let mut events = Vec::new();
        c.take_recorded(&mut events);
        assert!(
            matches!(events.as_slice(), [RecordEvent::End { .. }, RecordEvent::Mark { id: a }, RecordEvent::Note { on: true, .. }, RecordEvent::End { .. }, RecordEvent::Mark { id: b }] if *a == first && *b == last),
            "{events:?}"
        );
    }
}

#[test]
fn noop_timeline_restores_mode_without_ending_take_before_its_barrier() {
    for rebuilds in 0..=2 {
        for transition_is_timeline in [false, true] {
            let mut s = song_state(vec![], 96000);
            s.playback_mode = PlaybackMode::Pattern;
            let (mut c, mut p) = engine_with(CFG, s);
            c.send(Command::SetRecording { on: true }).unwrap();
            c.send(Command::Play).unwrap();
            p.process(&mut [0.0; 128], 2);
            let snapshot = c.state().clone();
            c.set_timeline(TimelineUpdate {
                pattern: Box::new(snapshot.pattern.clone()),
                song: Box::new(snapshot.song.clone()),
                bpm: snapshot.bpm,
                mode: PlaybackMode::Pattern,
                region: snapshot.song_loop,
                remove_channel: None,
            })
            .unwrap();
            for on in [true, false] {
                c.send(Command::ChannelNote {
                    channel: ChannelId(1),
                    key: 60,
                    velocity: 1.0,
                    on,
                    id: NoteId(408),
                })
                .unwrap();
            }
            let before = c.send_marked(Command::SetTempo { bpm: 120.0 }).unwrap();
            let after = if transition_is_timeline {
                c.set_timeline(TimelineUpdate {
                    pattern: Box::new(snapshot.pattern),
                    song: Box::new(snapshot.song),
                    bpm: snapshot.bpm,
                    mode: PlaybackMode::Song,
                    region: snapshot.song_loop,
                    remove_channel: None,
                })
                .unwrap();
                c.send_marked(Command::SetTempo { bpm: 120.0 }).unwrap()
            } else {
                c.send_marked(Command::SetPlaybackMode {
                    mode: PlaybackMode::Song,
                })
                .unwrap()
            };
            for rate in [44100.0, 96000.0].into_iter().take(rebuilds) {
                c.reconfigure(&mut p, EngineConfig { sample_rate: rate });
            }
            p.process(&mut [0.0; 128], 2);
            let mut events = Vec::new();
            c.take_recorded(&mut events);
            assert!(
                matches!(events.as_slice(), [
                RecordEvent::Note { on: true, .. }, RecordEvent::Note { on: false, .. },
                RecordEvent::Mark { id: a }, RecordEvent::End { .. }, RecordEvent::Mark { id: b }
            ] if *a==before && *b==after),
                "rebuilds={rebuilds},timeline={transition_is_timeline}: {events:?}"
            );
            assert!(!c.live().playing);
            assert!(!c.recording_armed());
        }
    }
}
