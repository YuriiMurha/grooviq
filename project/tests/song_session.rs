use grooviq_engine::{
    self as engine, AudioProcessor, Command, Controller, EngineConfig, SampleBuf,
};
use grooviq_project::{
    session::{Edit, Session},
    *,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

fn rig(
    limit: usize,
) -> (
    Session,
    Arc<Mutex<Controller>>,
    AudioProcessor,
    tempfile::TempDir,
) {
    let (c, p) = engine::engine(EngineConfig {
        sample_rate: 48000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    s.set_song_note_limit(limit);
    let d = tempfile::tempdir().unwrap();
    let mut project = Project::new();
    project.bpm = 120.0;
    project.play_mode = PlayMode::Song;
    project.channels.push(
        serde_json::from_str(r#"{"id":1,"name":"Synth","color":0,"instrument":"synth"}"#).unwrap(),
    );
    project.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "T".into(),
        color: 0,
        mute: false,
    });
    project.arrangement.clips.push(Clip {
        id: 3,
        track: 2,
        name: "P".into(),
        start_tick: 0,
        muted: false,
        content: ClipContent::Pattern {
            pattern: 0,
            offset_tick: 0,
            length_ticks: 7680,
        },
    });
    project.arrangement.audio_assets.push(AudioAsset {
        id: 4,
        sample: SampleRef {
            path: "generated.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 48001,
        original_tempo: None,
        original_key: None,
    });
    project.arrangement.clips.push(Clip {
        id: 5,
        track: 2,
        name: "A".into(),
        start_tick: 1,
        muted: false,
        content: ClipContent::Audio {
            asset: 4,
            source_start: 0,
            source_end: 48001,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        },
    });
    project.next_id = 6;
    let sample = Arc::new(SampleBuf {
        rate: 48000.0,
        l: (0..48001).map(|n| n as f32 / 480_010.0).collect(),
        r: (0..48001).map(|n| n as f32 / 480_010.0).collect(),
    });
    s.open(
        &d.path().join("song.grooviq"),
        Loaded {
            project,
            samples: Default::default(),
            warnings: vec![],
        },
        DecodedSamples {
            samples: HashMap::from([(4, sample)]),
            warnings: vec![],
            missing: Default::default(),
        },
    )
    .unwrap();
    (s, c, p, d)
}

#[test]
fn session_compilation_is_atomic_at_expansion_and_queue_limits_and_history() {
    let (mut s, c, mut p, _d) = rig(2);
    p.process(&mut [0.0; 128], 2);
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    assert_eq!(c.lock().unwrap().state().song.arrangement.notes.len(), 2);
    let before = s.project().clone();
    let view = s.view();
    let state = c.lock().unwrap().state().clone();
    assert!(
        s.apply(
            Edit::ToggleStep {
                channel: 1,
                step: 1
            },
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(c.lock().unwrap().state(), &state);
    while c
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    assert!(s.undo_notes(Instant::now()).is_err());
    assert_eq!(s.view(), view);
    assert_eq!(c.lock().unwrap().state(), &state);
    p.process(&mut [0.0; 128], 2);
    s.undo_notes(Instant::now()).unwrap();
    assert!(c.lock().unwrap().state().song.arrangement.notes.is_empty());
    s.redo_notes(Instant::now()).unwrap();
    assert_eq!(c.lock().unwrap().state().song.arrangement.notes.len(), 2);
}

#[test]
fn session_notes_and_tempo_reuse_audio_bank_and_export_actual_song() {
    let (mut s, c, mut p, _d) = rig(10);
    p.process(&mut [0.0; 128], 2);
    let bank = c.lock().unwrap().state().song.audio.clone();
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    s.apply(Edit::SetTempo { bpm: 180.0 }, Instant::now())
        .unwrap();
    assert!(bank.shares_bank(&c.lock().unwrap().state().song.audio));
    let state = s.checked_export_state().unwrap();
    assert_eq!(state.playback_mode, engine::PlaybackMode::Song);
    assert_eq!(state.bpm, 180.0);
    assert_eq!(state.song.arrangement.notes.len(), 2);
    let sound = engine::render_offline_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        state,
        &[(0, Command::Play)],
        2048,
        64,
    );
    assert!(sound.iter().any(|v| *v != 0.0));
}

#[test]
fn overexpansion_open_replaces_old_song_and_repair_restores_play_and_export() {
    let (mut s, c, mut p, d) = rig(0);
    s.play().unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut project = s.project().clone();
    project.patterns[0].notes.push(Note {
        id: 1,
        channel: 1,
        tick: 0,
        length: 240,
        key: 60,
        velocity: 0.8,
        slide: false,
    });
    project.patterns[0].next_note_id = 2;
    let generation = s.generation();
    s.open(
        &d.path().join("overflow.grooviq"),
        Loaded {
            project,
            samples: Default::default(),
            warnings: vec![],
        },
        DecodedSamples {
            samples: Default::default(),
            warnings: vec![],
            missing: Default::default(),
        },
    )
    .unwrap();
    assert_ne!(s.generation(), generation);
    assert!(s.song_error().is_some());
    assert!(s.play().is_err());
    assert!(s.checked_export_state().is_err());
    assert_eq!(c.lock().unwrap().state().song, engine::SongData::default());
    assert_eq!(
        c.lock().unwrap().state().playback_mode,
        engine::PlaybackMode::Pattern
    );
    let job = s
        .begin_save(Some(&d.path().join("editable.grooviq")))
        .unwrap();
    let result = job.write(&d.path().join("backups"));
    s.end_save(&job, result).unwrap();
    s.set_play_mode(PlayMode::Pattern, Instant::now()).unwrap();
    assert!(s.play().is_ok());
    assert!(s.set_play_mode(PlayMode::Song, Instant::now()).is_err());
    assert!(s.song_error().is_some());
    s.apply(
        Edit::ClearStep {
            channel: 1,
            step: 0,
        },
        Instant::now(),
    )
    .unwrap();
    assert!(s.song_error().is_none());
    assert!(!s.checked_export_state().unwrap().song.audio.is_empty());
    s.set_play_mode(PlayMode::Song, Instant::now()).unwrap();
    assert!(s.play().is_ok());
    assert!(s.checked_export_state().is_ok());
}

#[test]
fn production_compiler_retains_entire_record_poll_and_blocks_save_open_until_repaired() {
    use engine::{ChannelId, NoteId, RecordEvent};
    let (mut s, c, mut p, d) = rig(2);
    s.set_play_mode(PlayMode::Pattern, Instant::now()).unwrap();
    p.process(&mut [0.0; 128], 2);
    let mut marks = vec![];
    c.lock().unwrap().take_recorded(&mut marks);
    s.collect_recorded(&marks, Instant::now());
    let mut events = vec![];
    for take in [1, 2] {
        for on in [true, false] {
            events.push(RecordEvent::Note {
                channel: ChannelId(1),
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
    let before = s.project().clone();
    let view = s.view();
    let state = c.lock().unwrap().state().clone();
    s.collect_recorded(&events, Instant::now());
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(c.lock().unwrap().state(), &state);
    assert!(
        s.begin_save(Some(&d.path().join("refused.grooviq")))
            .is_err()
    );
    assert!(s.new_project().is_err());
    s.set_song_note_limit(4);
    s.collect_recorded(&[], Instant::now());
    assert_eq!(s.project().patterns[0].notes.len(), 2);
    assert_eq!(c.lock().unwrap().state().song.arrangement.notes.len(), 4);
    assert!(
        s.begin_save(Some(&d.path().join("accepted.grooviq")))
            .is_ok()
    );
}

#[test]
fn session_timeline_keeps_fixed_second_audio_progress_through_note_and_tempo_updates() {
    let (mut s, _c, mut p, _d) = rig(10);
    let (_reference, rc, mut rp, _rd) = rig(10);
    s.play().unwrap();
    rc.lock().unwrap().send(Command::Play).unwrap();
    for _ in 0..8 {
        let mut a = [0.0; 128];
        let mut b = [0.0; 128];
        p.process(&mut a, 2);
        rp.process(&mut b, 2);
        assert_eq!(a, b);
    }
    s.apply(
        Edit::SetStepVelocity {
            channel: 1,
            step: 0,
            velocity: 0.0,
        },
        Instant::now(),
    )
    .unwrap();
    s.apply(Edit::SetTempo { bpm: 180.0 }, Instant::now())
        .unwrap();
    rc.lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 180.0 })
        .unwrap();
    for _ in 0..32 {
        let mut a = [0.0; 128];
        let mut b = [0.0; 128];
        p.process(&mut a, 2);
        rp.process(&mut b, 2);
        assert_eq!(a, b);
    }
}

#[test]
fn full_queue_refuses_tempo_pattern_management_and_removal_without_mirror_changes() {
    use grooviq_project::pattern_edit::PatternEdit;
    let (mut s, c, mut p, _d) = rig(10);
    p.process(&mut [0.0; 128], 2);
    while c
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let before = s.project().clone();
    let view = s.view();
    let state = c.lock().unwrap().state().clone();
    let patterns = s.patterns();
    assert!(
        s.apply(Edit::SetTempo { bpm: 180.0 }, Instant::now())
            .is_err()
    );
    assert!(
        s.edit_patterns(
            s.generation(),
            patterns.revision,
            PatternEdit::Create { name: "B".into() },
            Instant::now()
        )
        .is_err()
    );
    assert!(
        s.apply(Edit::RemoveChannel { channel: 1 }, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(s.patterns().revision, patterns.revision);
    assert_eq!(s.patterns().selected, patterns.selected);
    assert_eq!(c.lock().unwrap().state(), &state);
}

#[test]
fn generated_all_clip_song_save_reopen_and_wav_export_preserve_sound_and_original_bytes() {
    let (mut s, _c, mut processor, d) = rig(10);
    processor.process(&mut [0.0; 128], 2);
    let mut project = s.project().clone();
    project.patterns[0].notes.push(Note {
        id: 1,
        channel: 1,
        tick: 0,
        length: 240,
        key: 60,
        velocity: 0.8,
        slide: false,
    });
    project.patterns[0].next_note_id = 2;
    project.arrangement.clips.push(Clip {
        id: 6,
        track: 2,
        name: "Pan".into(),
        start_tick: 0,
        muted: false,
        content: ClipContent::Automation {
            target: AutomationTarget::ChannelPan { channel: 1 },
            priority: 7,
            offset_tick: 0,
            length_ticks: 960,
            points: vec![AutomationPoint {
                tick: 0,
                value: 0.5,
                mode: SegmentMode::Hold,
            }],
        },
    });
    project.next_id = 8;
    project.arrangement.loop_region = Some(LoopRegion {
        start_tick: 240,
        end_tick: 960,
        enabled: true,
    });
    let data = 48001u32 * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&96000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for n in 0..48001 {
        bytes.extend_from_slice(&((n % 1000) as i16).to_le_bytes());
    }
    let bytes = Arc::new(bytes);
    let files = HashMap::from([(
        4,
        SampleFile {
            name: "generated.wav".into(),
            bytes: bytes.clone(),
        },
    )]);
    let path = d.path().join("all.grooviq");
    save(&project, &files, &path).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.samples[&4].bytes, bytes);
    let decoded = decode_samples(&loaded);
    assert!(decoded.samples.contains_key(&4));
    s.open(&path, loaded, decoded).unwrap();
    let state = s.checked_export_state().unwrap();
    assert_eq!(state.song.arrangement.notes.len(), 2);
    assert_eq!(state.song.automation.end_tick(), 960);
    let before = engine::render_offline_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        state.clone(),
        &[(0, Command::Play)],
        4096,
        64,
    );
    assert!(before.iter().any(|x| *x != 0.0));
    let options = engine::ExportOptions {
        max_tail_seconds: 0.0,
        ..Default::default()
    };
    let report = engine::export_wav(
        state,
        &options,
        &d.path().join("all.wav"),
        &mut |_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.frames, 192000);
    let (region_state, region) = s.export_selection(true).unwrap();
    let region_options = engine::ExportOptions {
        song_region: region,
        ..options
    };
    let region_report = engine::export_wav(
        region_state,
        &region_options,
        &d.path().join("region.wav"),
        &mut |_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(region_report.frames, 18000);
    let job = s.begin_save(Some(&path)).unwrap();
    let result = job.write(d.path());
    s.end_save(&job, result).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.samples[&4].bytes, bytes);
    let decoded = decode_samples(&loaded);
    s.open(&path, loaded, decoded).unwrap();
    let after = engine::render_offline_with(
        EngineConfig {
            sample_rate: 48000.0,
        },
        s.checked_export_state().unwrap(),
        &[(0, Command::Play)],
        4096,
        127,
    );
    assert_eq!(before, after);
}
