//! The session: edits, the engine kept in step, dirty state, autosave timing, open and save.
//! The engine runs offline: its processor is driven by hand where a test needs to see what
//! reached it. Nothing is played.

use grooviq_dsp::sampler::SampleBuf;
use grooviq_engine::{
    AudioProcessor, ChannelId, Command, Controller, EngineConfig, STEP_TICKS, SeqNote, engine,
};
use grooviq_project::session::{AUTOSAVE_PERIOD, Edit, STRUCTURAL_DELAY, Session, SessionError};
use grooviq_project::{
    Channel, Loaded, Note, Project, SampleFile, SampleRef, decode_samples, load, read_sample_file,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RATE: f32 = 48_000.0;

fn rig() -> (Session, Arc<Mutex<Controller>>, AudioProcessor) {
    let (controller, processor) = engine(EngineConfig { sample_rate: RATE });
    let controller = Arc::new(Mutex::new(controller));
    (Session::new(Arc::clone(&controller)), controller, processor)
}

/// A tiny valid mono 16-bit WAV (`frames` frames of a ramp), built by hand.
fn wav_bytes(frames: u32) -> Vec<u8> {
    let data = frames * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&96_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for i in 0..frames {
        b.extend_from_slice(&(((i % 200) as i16) * 100 + 1000).to_le_bytes());
    }
    b
}

fn sample_file(name: &str) -> SampleFile {
    SampleFile {
        name: name.into(),
        bytes: Arc::new(wav_bytes(4_800)),
    }
}

fn buffer() -> Arc<SampleBuf> {
    Arc::new(SampleBuf {
        rate: RATE,
        l: vec![0.5; 4_800],
        r: vec![0.5; 4_800],
    })
}

/// Adds a channel named after `stem` and returns its id.
fn add(session: &mut Session, stem: &str, now: Instant) -> u32 {
    let path = PathBuf::from(format!("/sounds/{stem}.wav"));
    let view = session
        .add_channel(&path, sample_file(&format!("{stem}.wav")), buffer(), now)
        .unwrap();
    view.channels.last().unwrap().id
}

fn cells(session: &Session, channel: u32) -> Vec<Option<f32>> {
    session.view().pattern.cells[&channel].clone()
}

/// Renders `blocks` blocks of 64 frames, returns the peak.
fn render(processor: &mut AudioProcessor, blocks: usize) -> f32 {
    let mut out = vec![0.0f32; 128];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        processor.process(&mut out, 2);
        peak = out.iter().fold(peak, |p, x| p.max(x.abs()));
    }
    peak
}

#[test]
fn a_new_session_is_an_empty_untitled_project() {
    let (session, _, _) = rig();
    let view = session.view();
    assert_eq!(view.name, "Untitled");
    assert_eq!(view.path, None);
    assert!(!view.dirty);
    assert_eq!(view.bpm, 140.0);
    assert!(view.channels.is_empty());
    assert_eq!(view.pattern.steps, 16);
    assert!(!session.needs_confirmation());
}

#[test]
fn adding_a_channel_names_it_after_the_file_and_gives_it_the_next_ink() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "Kick 01", now);
    let snare = add(&mut session, "snare", now);
    let view = session.view();
    assert_eq!(view.channels[0].name, "Kick 01");
    assert_eq!(view.channels[0].sample_name.as_deref(), Some("Kick 01.wav"));
    assert!(!view.channels[0].missing);
    assert_eq!(view.channels[0].color, 0);
    assert_eq!(view.channels[1].color, 1);
    assert_ne!(kick, snare);
    assert_eq!(view.pattern.cells[&kick], vec![None; 16]);
    let state = controller.lock().unwrap().state().clone();
    let ids: Vec<u32> = state.channels.iter().map(|c| c.id.0).collect();
    assert_eq!(ids, vec![kick, snare]);
    assert!(state.channels[0].sample.is_some());
}

#[test]
fn toggling_a_step_adds_a_note_at_its_tick_and_sends_the_pattern() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let view = session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 5,
            },
            now,
        )
        .unwrap();
    assert_eq!(view.pattern.cells[&kick][5], Some(0.8));
    assert_eq!(
        session.project().patterns[0].notes,
        vec![Note {
            channel: kick,
            tick: 1_200,
            length: 240,
            key: 60,
            velocity: 0.8,
        }]
    );
    let pattern = controller.lock().unwrap().state().pattern.clone();
    assert_eq!(pattern.length, 16 * STEP_TICKS);
    assert_eq!(
        pattern.notes,
        vec![SeqNote {
            tick: 1_200,
            channel: ChannelId(kick),
            key: 60,
            velocity: 0.8,
            length: 240,
        }]
    );
}

#[test]
fn toggling_again_removes_it() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let toggle = Edit::ToggleStep {
        channel: kick,
        step: 3,
    };
    session.apply(toggle.clone(), now).unwrap();
    let view = session.apply(toggle, now).unwrap();
    assert_eq!(view.pattern.cells[&kick], vec![None; 16]);
    assert!(session.project().patterns[0].notes.is_empty());
    assert!(controller.lock().unwrap().state().pattern.notes.is_empty());
}

#[test]
fn velocity_edits_change_only_that_step() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let snare = add(&mut session, "snare", now);
    for (channel, step) in [(kick, 0), (kick, 4), (snare, 4)] {
        session
            .apply(Edit::ToggleStep { channel, step }, now)
            .unwrap();
    }
    session
        .apply(
            Edit::SetStepVelocity {
                channel: kick,
                step: 4,
                velocity: 0.25,
            },
            now,
        )
        .unwrap();
    let mut expected = vec![None; 16];
    expected[0] = Some(0.8);
    expected[4] = Some(0.25);
    assert_eq!(cells(&session, kick), expected);
    expected[0] = None;
    expected[4] = Some(0.8);
    assert_eq!(cells(&session, snare), expected);
    let notes = controller.lock().unwrap().state().pattern.notes.clone();
    let velocities: Vec<(u32, u32, f32)> = notes
        .iter()
        .map(|n| (n.tick, n.channel.0, n.velocity))
        .collect();
    assert_eq!(
        velocities,
        vec![(0, kick, 0.8), (960, kick, 0.25), (960, snare, 0.8)]
    );

    // Out of range is clamped; not a number is refused.
    session
        .apply(
            Edit::SetStepVelocity {
                channel: kick,
                step: 4,
                velocity: 3.0,
            },
            now,
        )
        .unwrap();
    assert_eq!(cells(&session, kick)[4], Some(1.0));
    let nan = session.apply(
        Edit::SetStepVelocity {
            channel: kick,
            step: 4,
            velocity: f32::NAN,
        },
        now,
    );
    assert!(nan.is_err());
    assert_eq!(cells(&session, kick)[4], Some(1.0));
}

#[test]
fn clearing_a_step_removes_its_hit_and_leaves_the_rest() {
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    for step in [1, 2] {
        session
            .apply(
                Edit::ToggleStep {
                    channel: kick,
                    step,
                },
                now,
            )
            .unwrap();
    }
    session
        .apply(
            Edit::ClearStep {
                channel: kick,
                step: 1,
            },
            now,
        )
        .unwrap();
    let mut expected = vec![None; 16];
    expected[2] = Some(0.8);
    assert_eq!(cells(&session, kick), expected);
}

#[test]
fn shortening_then_lengthening_the_pattern_keeps_its_steps() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    for step in [2, 12] {
        session
            .apply(
                Edit::ToggleStep {
                    channel: kick,
                    step,
                },
                now,
            )
            .unwrap();
    }
    let view = session
        .apply(Edit::SetPatternLength { steps: 8 }, now)
        .unwrap();
    assert_eq!(view.pattern.steps, 8);
    let mut short = vec![None; 8];
    short[2] = Some(0.8);
    assert_eq!(view.pattern.cells[&kick], short);
    let engine = controller.lock().unwrap().state().pattern.clone();
    assert_eq!(engine.length, 8 * STEP_TICKS);
    assert_eq!(engine.notes.len(), 1, "the note past the end is not played");

    let view = session
        .apply(Edit::SetPatternLength { steps: 16 }, now)
        .unwrap();
    let mut long = vec![None; 16];
    long[2] = Some(0.8);
    long[12] = Some(0.8);
    assert_eq!(view.pattern.cells[&kick], long);
    assert_eq!(controller.lock().unwrap().state().pattern.notes.len(), 2);
}

#[test]
fn a_step_past_the_end_is_refused() {
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let err = session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 16,
            },
            now,
        )
        .unwrap_err();
    assert!(matches!(err, SessionError::StepOutOfRange { .. }), "{err}");
}

#[test]
fn an_edit_for_an_unknown_channel_fails_and_changes_nothing() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let dir = tempfile::tempdir().unwrap();
    session
        .save(Some(&dir.path().join("u.grooviq")), dir.path())
        .unwrap();
    let before = session.project().clone();
    let engine_before = controller.lock().unwrap().state().clone();
    let edits = [
        Edit::ToggleStep {
            channel: 99,
            step: 0,
        },
        Edit::SetStepVelocity {
            channel: 99,
            step: 0,
            velocity: 0.5,
        },
        Edit::ClearStep {
            channel: 99,
            step: 0,
        },
        Edit::RemoveChannel { channel: 99 },
        Edit::SetChannelMix {
            channel: 99,
            volume: 0.5,
            pan: 0.0,
            mute: false,
            solo: false,
        },
        Edit::SetSamplerParam {
            channel: 99,
            param: "tune".into(),
            value: 2.0,
        },
    ];
    for edit in edits {
        let err = session.apply(edit.clone(), now).unwrap_err();
        assert!(
            matches!(err, SessionError::UnknownChannel(99)),
            "{edit:?}: {err}"
        );
        assert!(!err.to_string().is_empty());
    }
    let err = session
        .apply(
            Edit::SetSamplerParam {
                channel: kick,
                param: "wobble".into(),
                value: 1.0,
            },
            now,
        )
        .unwrap_err();
    assert!(matches!(err, SessionError::UnknownParam(_)), "{err}");
    assert_eq!(*session.project(), before);
    assert_eq!(*controller.lock().unwrap().state(), engine_before);
    assert!(!session.view().dirty);
}

#[test]
fn mix_sampler_tempo_swing_and_signature_reach_the_model_and_the_engine() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    session
        .apply(
            Edit::SetChannelMix {
                channel: kick,
                volume: 0.5,
                pan: -0.25,
                mute: true,
                solo: false,
            },
            now,
        )
        .unwrap();
    session
        .apply(
            Edit::SetSamplerParam {
                channel: kick,
                param: "tune".into(),
                value: 99.0,
            },
            now,
        )
        .unwrap();
    session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 1,
            },
            now,
        )
        .unwrap();
    session.apply(Edit::SetSwing { swing: 0.5 }, now).unwrap();
    session.apply(Edit::SetTempo { bpm: 1000.0 }, now).unwrap();
    let view = session
        .apply(Edit::SetTimeSignature { beats: 7, unit: 8 }, now)
        .unwrap();

    let channel = &view.channels[0];
    assert_eq!(
        (channel.volume, channel.pan, channel.mute, channel.solo),
        (0.5, -0.25, true, false)
    );
    assert_eq!(channel.sampler.get("tune"), Some(&24.0), "clamped");
    assert_eq!(view.pattern.swing, 0.5);
    assert_eq!(view.bpm, 400.0, "clamped");
    assert_eq!(
        (view.time_signature.beats, view.time_signature.unit),
        (7, 8)
    );

    let state = controller.lock().unwrap().state().clone();
    assert_eq!(state.bpm, 400.0);
    assert_eq!(state.channels[0].mix.volume, 0.5);
    assert_eq!(state.channels[0].mix.pan, -0.25);
    assert!(state.channels[0].mix.mute);
    assert_eq!(state.channels[0].sampler[1], 24.0);
    assert_eq!(
        state.pattern.notes[0].tick,
        240 + 60,
        "swing reached the engine"
    );

    let bad = session.apply(Edit::SetTimeSignature { beats: 4, unit: 3 }, now);
    assert!(matches!(bad, Err(SessionError::InvalidValue(_))));
    assert_eq!(session.view().time_signature.unit, 8);
}

#[test]
fn removing_a_channel_drops_it_and_its_notes_everywhere() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let snare = add(&mut session, "snare", now);
    for channel in [kick, snare] {
        session
            .apply(Edit::ToggleStep { channel, step: 0 }, now)
            .unwrap();
    }
    let view = session
        .apply(Edit::RemoveChannel { channel: kick }, now)
        .unwrap();
    assert_eq!(view.channels.len(), 1);
    assert!(!view.pattern.cells.contains_key(&kick));
    assert!(
        session.project().patterns[0]
            .notes
            .iter()
            .all(|n| n.channel == snare)
    );
    let state = controller.lock().unwrap().state().clone();
    assert_eq!(state.channels.len(), 1);
    assert!(state.pattern.notes.iter().all(|n| n.channel.0 == snare));
}

#[test]
fn edits_mark_dirty_and_saving_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let now = Instant::now();
    session.apply(Edit::SetTempo { bpm: 120.0 }, now).unwrap();
    assert!(session.view().dirty);
    let path = dir.path().join("beat.grooviq");
    let saved = session
        .save(Some(&path), &dir.path().join("backups"))
        .unwrap();
    assert_eq!(saved, path);
    let view = session.view();
    assert!(!view.dirty);
    assert_eq!(view.path.as_deref(), Some(path.to_str().unwrap()));
    assert_eq!(view.name, "beat");
    assert_eq!(load(&path).unwrap().project.bpm, 120.0);

    // An edit that changes nothing does not make it dirty.
    session.apply(Edit::SetTempo { bpm: 120.0 }, now).unwrap();
    assert!(!session.view().dirty);
}

#[test]
fn save_as_adds_the_extension() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let saved = session
        .save(Some(&dir.path().join("my.beat")), &dir.path().join("b"))
        .unwrap();
    assert_eq!(saved, dir.path().join("my.beat.grooviq"));
    assert!(saved.is_file());
}

#[test]
fn an_edit_during_a_save_keeps_the_project_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let now = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, now).unwrap();
    let job = session
        .begin_save(Some(&dir.path().join("a.grooviq")))
        .unwrap();
    // The session lock is released while the file is written; an edit lands meanwhile.
    session.apply(Edit::SetTempo { bpm: 110.0 }, now).unwrap();
    job.write(&dir.path().join("backups")).unwrap();
    session.finish_save(&job);
    assert!(session.view().dirty, "the 110 BPM edit is not in the file");
    assert!(session.view().path.is_some());
    assert!(
        session.autosave_due(now + AUTOSAVE_PERIOD),
        "the unsaved edit still gets autosaved"
    );
}

#[test]
fn saving_an_untitled_project_without_a_path_asks_for_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    session
        .apply(Edit::SetTempo { bpm: 90.0 }, Instant::now())
        .unwrap();
    let err = session.save(None, dir.path()).unwrap_err();
    assert!(matches!(err, SessionError::NoPath));
    assert!(session.view().dirty);
}

#[test]
fn saving_over_a_file_keeps_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");
    let path = dir.path().join("song.grooviq");
    let (mut session, _, _) = rig();
    let now = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, now).unwrap();
    session.save(Some(&path), &backups).unwrap();
    let folder = grooviq_project::backup_folder(&path, &backups).unwrap();
    assert!(!folder.exists(), "nothing to back up yet");

    session.apply(Edit::SetTempo { bpm: 101.0 }, now).unwrap();
    session.save(None, &backups).unwrap();
    let copies: Vec<PathBuf> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(load(&copies[0]).unwrap().project.bpm, 100.0);
    assert_eq!(load(&path).unwrap().project.bpm, 101.0);
}

/// The tempos of the backups of the project at `project`, oldest first.
fn backup_tempos(project: &Path, backups: &Path) -> Vec<f32> {
    let folder = grooviq_project::backup_folder(project, backups).unwrap();
    let mut copies: Vec<PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    copies.sort();
    copies
        .iter()
        .map(|c| load(c).unwrap().project.bpm)
        .collect()
}

/// A session whose `song.grooviq` was saved at 100, then 110 BPM: one backup, of 100.
fn saved_twice(dir: &Path) -> (Session, PathBuf, PathBuf) {
    let backups = dir.join("backups");
    let path = dir.join("song.grooviq");
    let (mut session, _, _) = rig();
    let now = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, now).unwrap();
    session.save(Some(&path), &backups).unwrap();
    session.apply(Edit::SetTempo { bpm: 110.0 }, now).unwrap();
    session.save(None, &backups).unwrap();
    assert_eq!(backup_tempos(&path, &backups), [100.0]);
    (session, path, backups)
}

#[test]
fn saves_that_change_nothing_keep_the_older_backups() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    // Ten more saves of the same project (a held Cmd+S): one backup of 110 at most, and 100
    // is still there.
    for _ in 0..10 {
        session.save(None, &backups).unwrap();
    }
    assert_eq!(backup_tempos(&path, &backups), [100.0, 110.0]);
    assert_eq!(load(&path).unwrap().project.bpm, 110.0);
}

#[cfg(unix)]
#[test]
fn a_failed_save_makes_no_backup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    session
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    // The temp file cannot be created: the project's folder takes no new files.
    let open = std::fs::metadata(dir.path()).unwrap().permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut errors = Vec::new();
    for _ in 0..3 {
        errors.push(session.save(None, &backups));
    }
    std::fs::set_permissions(dir.path(), open).unwrap();
    for result in errors {
        // (A process allowed to write anywhere, such as root, would save: not a failure case.)
        if let Err(err) = result {
            assert!(matches!(err, SessionError::Save(_)), "{err}");
        } else {
            return;
        }
    }
    assert_eq!(backup_tempos(&path, &backups), [100.0], "no backup rotated");
    assert_eq!(load(&path).unwrap().project.bpm, 110.0);
    assert!(session.view().dirty);
}

#[test]
fn a_refused_save_makes_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let (_, path, backups) = saved_twice(dir.path());
    // The same file, opened with a channel whose sample bytes were never loaded: `save`
    // refuses before writing anything.
    let mut project = load(&path).unwrap().project;
    project
        .channels
        .push(channel(1, "Kick", Some("samples/1-kick.wav")));
    let loaded = Loaded {
        project,
        samples: Default::default(),
        warnings: vec![],
    };
    let decoded = decode_samples(&loaded);
    let (mut session, _, _) = rig();
    session.open(&path, loaded, decoded).unwrap();
    session
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    for _ in 0..3 {
        let err = session.save(None, &backups).unwrap_err();
        assert!(err.to_string().contains("saving would lose it"), "{err}");
    }
    assert_eq!(backup_tempos(&path, &backups), [100.0], "no backup rotated");
    assert_eq!(load(&path).unwrap().project.bpm, 110.0);
}

#[test]
fn a_save_error_is_surfaced_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let mut project = Project::new();
    project
        .channels
        .push(channel(1, "Kick", Some("samples/1-kick.wav")));
    // The channel names an entry, but its bytes were never loaded: `save` refuses.
    let loaded = Loaded {
        project,
        samples: Default::default(),
        warnings: vec![],
    };
    let decoded = decode_samples(&loaded);
    session
        .open(&dir.path().join("x.grooviq"), loaded, decoded)
        .unwrap();
    session.apply(Edit::SetTempo { bpm: 99.0 }, now).unwrap();
    let err = session.save(None, &dir.path().join("b")).unwrap_err();
    assert!(
        err.to_string().contains("saving would lose it"),
        "save's own message: {err}"
    );
    assert!(session.view().dirty);
}

#[test]
fn structural_edits_autosave_after_two_seconds() {
    let (mut session, _, _) = rig();
    let t0 = Instant::now();
    add(&mut session, "kick", t0);
    assert!(!session.autosave_due(t0));
    assert!(!session.autosave_due(t0 + STRUCTURAL_DELAY - Duration::from_millis(1)));
    assert!(session.autosave_due(t0 + STRUCTURAL_DELAY));
    let (project, samples, path) = session.autosave_snapshot(t0 + STRUCTURAL_DELAY).unwrap();
    assert_eq!(project.channels.len(), 1);
    assert_eq!(samples.len(), 1);
    assert_eq!(path, None);
    assert!(
        !session.autosave_due(t0 + AUTOSAVE_PERIOD * 2),
        "nothing new"
    );
    assert!(session.view().dirty, "an autosave is not a save");

    let t1 = t0 + Duration::from_secs(10);
    session
        .apply(Edit::SetPatternLength { steps: 32 }, t1)
        .unwrap();
    assert!(!session.autosave_due(t1 + Duration::from_secs(1)));
    assert!(session.autosave_due(t1 + STRUCTURAL_DELAY));
    session.autosave_snapshot(t1 + STRUCTURAL_DELAY).unwrap();

    let kick = session.view().channels[0].id;
    let t2 = t1 + Duration::from_secs(10);
    session
        .apply(Edit::RemoveChannel { channel: kick }, t2)
        .unwrap();
    assert!(!session.autosave_due(t2 + Duration::from_secs(1)));
    assert!(
        session.autosave_due(t2 + STRUCTURAL_DELAY),
        "removing is structural"
    );
}

#[test]
fn other_edits_wait_one_minute() {
    let (mut session, _, _) = rig();
    let t0 = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, t0).unwrap();
    assert!(!session.autosave_due(t0 + STRUCTURAL_DELAY));
    assert!(!session.autosave_due(t0 + AUTOSAVE_PERIOD - Duration::from_millis(1)));
    // Later edits do not push the deadline back.
    session
        .apply(Edit::SetTempo { bpm: 101.0 }, t0 + Duration::from_secs(50))
        .unwrap();
    assert!(session.autosave_due(t0 + AUTOSAVE_PERIOD));
    assert!(session.autosave_snapshot(t0 + AUTOSAVE_PERIOD).is_some());
    assert!(!session.autosave_due(t0 + AUTOSAVE_PERIOD * 3));
}

#[test]
fn a_failed_autosave_is_tried_again() {
    let (mut session, _, _) = rig();
    let t0 = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, t0).unwrap();
    let t1 = t0 + AUTOSAVE_PERIOD;
    assert!(session.autosave_snapshot(t1).is_some());
    session.autosave_failed(t1);
    assert!(!session.autosave_due(t1 + Duration::from_secs(1)));
    assert!(session.autosave_due(t1 + AUTOSAVE_PERIOD));
}

#[test]
fn nothing_to_autosave_when_clean() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let t0 = Instant::now();
    assert!(!session.autosave_due(t0 + AUTOSAVE_PERIOD * 10));
    assert!(
        session
            .autosave_snapshot(t0 + AUTOSAVE_PERIOD * 10)
            .is_none()
    );

    // Saved: nothing is left to autosave either.
    let kick = add(&mut session, "kick", t0);
    session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 0,
            },
            t0,
        )
        .unwrap();
    session
        .save(Some(&dir.path().join("s.grooviq")), dir.path())
        .unwrap();
    assert!(!session.autosave_due(t0 + AUTOSAVE_PERIOD * 10));
    assert!(
        session
            .autosave_snapshot(t0 + AUTOSAVE_PERIOD * 10)
            .is_none()
    );
}

#[test]
fn the_view_shows_the_last_autosave_time() {
    let (mut session, _, _) = rig();
    let t0 = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, t0).unwrap();
    assert_eq!(session.view().autosaved_at, None);
    session.autosave_snapshot(t0 + AUTOSAVE_PERIOD).unwrap();
    assert!(session.autosaved(session.generation(), 1_700_000_000));
    assert_eq!(session.view().autosaved_at, Some(1_700_000_000));
}

fn channel(id: u32, name: &str, entry: Option<&str>) -> Channel {
    Channel {
        id,
        name: name.into(),
        color: 0,
        sample: Some(SampleRef {
            path: PathBuf::from(format!("/gone/{}.wav", name.to_lowercase())),
            file: entry.map(Into::into),
        }),
        sampler: BTreeMap::new(),
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
    }
}

/// Kick (bytes present), Snare (its sample was not in the file), Hat (bytes that do not
/// decode); a hit for each on step 0, and one on step 4 for the snare.
fn loaded_with_a_missing_sample() -> Loaded {
    let mut project = Project::new();
    project.next_id = 4;
    project
        .channels
        .push(channel(1, "Kick", Some("samples/1-kick.wav")));
    project.channels.push(channel(2, "Snare", None));
    project
        .channels
        .push(channel(3, "Hat", Some("samples/2-hat.wav")));
    for (channel, tick) in [(1, 0), (2, 0), (2, 960), (3, 0)] {
        project.patterns[0].notes.push(Note {
            channel,
            tick,
            length: 240,
            key: 60,
            velocity: 0.8,
        });
    }
    let mut samples = std::collections::HashMap::new();
    samples.insert(1, sample_file("kick.wav"));
    samples.insert(
        3,
        SampleFile {
            name: "hat.wav".into(),
            bytes: Arc::new(b"not a sound".to_vec()),
        },
    );
    Loaded {
        project,
        samples,
        warnings: vec!["Channel \"Snare\": its sample is not in the project file.".into()],
    }
}

#[test]
fn a_sample_shared_by_channels_is_decoded_once_and_shared() {
    let mut project = Project::new();
    project.next_id = 6;
    for (id, name) in [(1, "Kick"), (2, "Kick 2"), (3, "Kick 3"), (4, "Snare")] {
        project
            .channels
            .push(channel(id, name, Some(&format!("samples/{id}-x.wav"))));
    }
    // Two copies of the same bytes (not the same allocation), one more through the same
    // allocation, and a different sound.
    let shared = Arc::new(wav_bytes(4_800));
    let file = |bytes: &Arc<Vec<u8>>| SampleFile {
        name: "kick.wav".into(),
        bytes: Arc::clone(bytes),
    };
    let mut samples = std::collections::HashMap::new();
    samples.insert(1, file(&shared));
    samples.insert(2, file(&Arc::new(wav_bytes(4_800))));
    samples.insert(3, file(&shared));
    samples.insert(4, file(&Arc::new(wav_bytes(2_400))));
    let loaded = Loaded {
        project,
        samples,
        warnings: Vec::new(),
    };

    let decoded = decode_samples(&loaded);
    assert!(decoded.missing.is_empty() && decoded.warnings.is_empty());
    let (a, b, c, d) = (
        &decoded.samples[&1],
        &decoded.samples[&2],
        &decoded.samples[&3],
        &decoded.samples[&4],
    );
    assert!(Arc::ptr_eq(a, b), "equal bytes: one buffer");
    assert!(Arc::ptr_eq(a, c), "the same bytes: one buffer");
    assert!(!Arc::ptr_eq(a, d), "different bytes: their own buffer");
    assert_eq!(a.l.len() * 2, d.l.len() * 4);
}

#[test]
fn a_shared_sample_that_does_not_decode_warns_for_each_channel() {
    let mut project = Project::new();
    project.next_id = 3;
    project
        .channels
        .push(channel(1, "Hat", Some("samples/1-h")));
    project
        .channels
        .push(channel(2, "Hat 2", Some("samples/1-h")));
    let bad = || SampleFile {
        name: "hat.wav".into(),
        bytes: Arc::new(b"not a sound".to_vec()),
    };
    let mut samples = std::collections::HashMap::new();
    samples.insert(1, bad());
    samples.insert(2, bad());
    let loaded = Loaded {
        project,
        samples,
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    assert_eq!(decoded.missing.len(), 2);
    assert_eq!(decoded.warnings.len(), 2);
    assert!(decoded.warnings[0].contains("\"Hat\""));
    assert!(decoded.warnings[1].contains("\"Hat 2\""));
}

#[test]
fn opening_a_project_with_a_missing_sample_keeps_the_channel_and_warns() {
    let (mut session, controller, mut processor) = rig();
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    assert_eq!(
        decoded.missing.iter().copied().collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(decoded.samples.contains_key(&1));
    assert_eq!(decoded.warnings.len(), 1, "{:?}", decoded.warnings);
    assert!(
        decoded.warnings[0].contains("Hat"),
        "{:?}",
        decoded.warnings
    );

    let report = session
        .open(Path::new("/projects/beat.grooviq"), loaded, decoded)
        .unwrap();
    assert!(report.warnings.iter().any(|w| w.contains("Snare")));
    assert!(report.warnings.iter().any(|w| w.contains("Hat")));
    let view = report.view;
    let missing: Vec<(String, bool)> = view
        .channels
        .iter()
        .map(|c| (c.name.clone(), c.missing))
        .collect();
    assert_eq!(
        missing,
        vec![
            ("Kick".into(), false),
            ("Snare".into(), true),
            ("Hat".into(), true)
        ]
    );
    assert_eq!(view.channels[1].sample_name.as_deref(), Some("snare.wav"));
    let snare = &view.pattern.cells[&2];
    assert_eq!((snare[0], snare[4]), (Some(0.8), Some(0.8)));
    assert!(!view.dirty);

    let state = controller.lock().unwrap().state().clone();
    let has_sample: Vec<bool> = state.channels.iter().map(|c| c.sample.is_some()).collect();
    assert_eq!(has_sample, vec![true, false, false]);
    assert_eq!(
        state.pattern.notes.len(),
        4,
        "the missing channels keep their hits"
    );

    // The kick still plays.
    controller.lock().unwrap().send(Command::Play).unwrap();
    assert!(render(&mut processor, 40) > 0.01);
}

#[test]
fn a_hat_that_does_not_decode_is_saved_with_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    session
        .open(&dir.path().join("in.grooviq"), loaded, decoded)
        .unwrap();
    let out = dir.path().join("out.grooviq");
    session.save(Some(&out), dir.path()).unwrap();
    let reloaded = load(&out).unwrap();
    assert_eq!(&**reloaded.samples[&3].bytes, b"not a sound");
}

#[test]
fn relinking_gives_the_channel_its_sample() {
    let (mut session, controller, _) = rig();
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/beat.grooviq"), loaded, decoded)
        .unwrap();
    let now = Instant::now();
    let view = session
        .set_channel_sample(
            2,
            Path::new("/new/snare 2.wav"),
            sample_file("snare 2.wav"),
            buffer(),
            now,
        )
        .unwrap();
    assert!(!view.channels[1].missing);
    assert_eq!(view.channels[1].name, "Snare", "the name stays");
    assert_eq!(view.channels[1].sample_name.as_deref(), Some("snare 2.wav"));
    assert!(view.dirty);
    assert!(
        controller.lock().unwrap().state().channels[1]
            .sample
            .is_some()
    );
    assert!(session.autosave_due(now + STRUCTURAL_DELAY));
    let err = session
        .set_channel_sample(9, Path::new("/x.wav"), sample_file("x.wav"), buffer(), now)
        .unwrap_err();
    assert!(matches!(err, SessionError::UnknownChannel(9)));
}

#[test]
fn a_saved_project_keeps_samples_whose_originals_are_gone() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("kick.wav");
    std::fs::write(&original, wav_bytes(2_400)).unwrap();
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let file = read_sample_file(&original).unwrap();
    let buf = grooviq_engine::decode_sample(&file.bytes, Some("wav"), 600.0).unwrap();
    let view = session
        .add_channel(&original, file, Arc::new(buf), now)
        .unwrap();
    session
        .apply(
            Edit::ToggleStep {
                channel: view.channels[0].id,
                step: 0,
            },
            now,
        )
        .unwrap();
    let path = dir.path().join("beat.grooviq");
    session.save(Some(&path), &dir.path().join("b")).unwrap();
    std::fs::remove_file(&original).unwrap();

    let (mut other, controller, _) = rig();
    let loaded = load(&path).unwrap();
    let decoded = decode_samples(&loaded);
    assert!(decoded.missing.is_empty());
    let report = other.open(&path, loaded, decoded).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(!report.view.channels[0].missing);
    assert!(
        controller.lock().unwrap().state().channels[0]
            .sample
            .is_some()
    );
}

#[test]
fn opening_stops_the_transport_first() {
    let (mut session, controller, mut processor) = rig();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 4);
    assert!(controller.lock().unwrap().live().playing);

    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/beat.grooviq"), loaded, decoded)
        .unwrap();
    render(&mut processor, 1);
    let live = controller.lock().unwrap().live();
    assert!(!live.playing);
    assert_eq!(live.position_ticks, 0);
}

#[test]
fn opening_never_starts_playback() {
    let (mut session, controller, mut processor) = rig();
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/beat.grooviq"), loaded, decoded)
        .unwrap();
    let peak = render(&mut processor, 200);
    assert!(!controller.lock().unwrap().live().playing);
    assert_eq!(peak, 0.0, "not a sound after opening");
}

#[test]
fn preview_sends_a_note_on_the_channel_only_when_asked() {
    let (mut session, _, mut processor) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    assert_eq!(render(&mut processor, 20), 0.0);
    let dir = tempfile::tempdir().unwrap();
    session
        .save(Some(&dir.path().join("p.grooviq")), dir.path())
        .unwrap();
    session.preview(kick, true).unwrap();
    assert!(render(&mut processor, 20) > 0.01);
    session.preview(kick, false).unwrap();
    assert!(matches!(
        session.preview(42, true),
        Err(SessionError::UnknownChannel(42))
    ));
    assert!(!session.view().dirty, "a preview is not an edit");
}

#[test]
fn quitting_with_unsaved_changes_waits_for_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let now = Instant::now();
    assert!(!session.needs_confirmation());
    session.apply(Edit::SetSwing { swing: 0.3 }, now).unwrap();
    assert!(session.needs_confirmation());
    session
        .save(Some(&dir.path().join("q.grooviq")), dir.path())
        .unwrap();
    assert!(!session.needs_confirmation());
    session.apply(Edit::SetSwing { swing: 0.4 }, now).unwrap();
    assert!(session.needs_confirmation());
    let view = session.new_project().unwrap();
    assert!(!session.needs_confirmation());
    assert_eq!(view.path, None);
    assert!(view.channels.is_empty());
}

#[test]
fn restoring_keeps_the_original_path_and_stays_dirty() {
    let (mut session, _, _) = rig();
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    let original = PathBuf::from("/music/beat.grooviq");
    let now = Instant::now();
    let report = session
        .open_recovered(Some(&original), loaded, decoded, now)
        .unwrap();
    assert_eq!(
        report.view.path.as_deref(),
        Some(original.to_str().unwrap())
    );
    assert!(report.view.dirty);
    assert!(session.needs_confirmation());
    assert!(session.autosave_due(now + STRUCTURAL_DELAY));
}

#[test]
fn the_export_state_is_the_engine_state_of_the_project() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 2,
            },
            now,
        )
        .unwrap();
    assert_eq!(
        session.export_state(),
        controller.lock().unwrap().state().clone()
    );
}

#[test]
fn edits_and_views_have_the_json_shapes_the_ui_uses() {
    let edit: Edit = serde_json::from_str(
        r#"{"kind":"setChannelMix","channel":3,"volume":0.5,"pan":0,"mute":false,"solo":true}"#,
    )
    .unwrap();
    assert_eq!(
        edit,
        Edit::SetChannelMix {
            channel: 3,
            volume: 0.5,
            pan: 0.0,
            mute: false,
            solo: true
        }
    );
    let edit: Edit =
        serde_json::from_str(r#"{"kind":"setTimeSignature","beats":3,"unit":4}"#).unwrap();
    assert_eq!(edit, Edit::SetTimeSignature { beats: 3, unit: 4 });
    let edit: Edit = serde_json::from_str(r#"{"kind":"toggleStep","channel":1,"step":2}"#).unwrap();
    assert_eq!(
        edit,
        Edit::ToggleStep {
            channel: 1,
            step: 2
        }
    );

    let (mut session, _, _) = rig();
    let kick = add(&mut session, "kick", Instant::now());
    session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 1,
            },
            Instant::now(),
        )
        .unwrap();
    let json = serde_json::to_value(session.view()).unwrap();
    for key in [
        "name",
        "path",
        "dirty",
        "bpm",
        "timeSignature",
        "channels",
        "pattern",
        "autosavedAt",
    ] {
        assert!(json.get(key).is_some(), "{key} in {json}");
    }
    let channel = &json["channels"][0];
    for key in [
        "id",
        "name",
        "color",
        "sampleName",
        "missing",
        "volume",
        "pan",
        "mute",
        "solo",
        "sampler",
    ] {
        assert!(channel.get(key).is_some(), "{key} in {channel}");
    }
    let pattern = &json["pattern"];
    for key in ["id", "name", "steps", "swing", "cells"] {
        assert!(pattern.get(key).is_some(), "{key} in {pattern}");
    }
    assert_eq!(pattern["cells"][kick.to_string()][1], 0.800000011920929);
    assert!(pattern["cells"][kick.to_string()][0].is_null());
    assert_eq!(json["timeSignature"]["beats"], 4);
}

#[test]
fn the_grid_shows_only_hits_that_start_on_a_step() {
    let (mut session, _, _) = rig();
    let mut loaded = loaded_with_a_missing_sample();
    let notes = &mut loaded.project.patterns[0].notes;
    notes.clear();
    for (tick, velocity) in [(100, 0.3), (480, 0.6), (16 * 240, 0.9)] {
        notes.push(Note {
            channel: 1,
            tick,
            length: 240,
            key: 60,
            velocity,
        });
    }
    let decoded = decode_samples(&loaded);
    let view = session
        .open(Path::new("/p/grid.grooviq"), loaded, decoded)
        .unwrap()
        .view;
    let mut expected = vec![None; 16];
    expected[2] = Some(0.6);
    assert_eq!(view.pattern.cells[&1], expected);
}

#[test]
fn edits_that_change_nothing_leave_the_project_clean() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    session
        .save(Some(&dir.path().join("c.grooviq")), dir.path())
        .unwrap();
    let noops = [
        Edit::ClearStep {
            channel: kick,
            step: 3,
        },
        Edit::SetSwing { swing: 0.0 },
        Edit::SetPatternLength { steps: 16 },
        Edit::SetChannelMix {
            channel: kick,
            volume: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
        },
        Edit::SetTimeSignature { beats: 4, unit: 4 },
    ];
    for edit in noops {
        session.apply(edit.clone(), now).unwrap();
        assert!(!session.view().dirty, "{edit:?}");
    }
    assert!(!session.autosave_due(now + AUTOSAVE_PERIOD));
}

#[test]
fn a_velocity_on_an_empty_step_adds_a_hit_there() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    session
        .apply(
            Edit::SetStepVelocity {
                channel: kick,
                step: 7,
                velocity: 0.4,
            },
            now,
        )
        .unwrap();
    assert_eq!(cells(&session, kick)[7], Some(0.4));
    assert_eq!(controller.lock().unwrap().state().pattern.notes.len(), 1);
}

/// The engine mirrors the project exactly: the state the project compiles to is the state the
/// controller holds, the model keeps every value in range (as the engine clamps it), and each
/// mix, sampler value and the tempo are the very values the engine got.
fn assert_mirror(session: &Session, controller: &Mutex<Controller>, after: &str) {
    let state = controller.lock().unwrap().state().clone();
    assert_eq!(session.export_state(), state, "after {after}");
    let project = session.project();
    assert_eq!(
        project.clone().validate(),
        Vec::<String>::new(),
        "after {after}: the model holds a value the engine clamped"
    );
    assert_eq!(project.bpm, state.bpm, "after {after}");
    for (channel, engine) in project.channels.iter().zip(&state.channels) {
        assert_eq!(channel.id, engine.id.0, "after {after}");
        assert_eq!(
            (channel.volume, channel.pan, channel.mute, channel.solo),
            (
                engine.mix.volume,
                engine.mix.pan,
                engine.mix.mute,
                engine.mix.solo
            ),
            "after {after}"
        );
        for (param, value) in &channel.sampler {
            let index = grooviq_dsp::sampler::PARAM_IDS
                .iter()
                .position(|p| p == param)
                .unwrap();
            assert_eq!(*value, engine.sampler[index], "after {after}: {param}");
        }
    }
    let pattern = &project.patterns[0];
    assert_eq!(
        pattern.steps * STEP_TICKS,
        state.pattern.length,
        "after {after}"
    );
}

#[test]
fn out_of_range_values_reach_the_model_as_the_engine_clamps_them() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let snare = add(&mut session, "snare", now);
    assert_mirror(&session, &controller, "adding channels");
    let edits = [
        Edit::ToggleStep {
            channel: kick,
            step: 1,
        },
        Edit::ToggleStep {
            channel: snare,
            step: 3,
        },
        Edit::SetStepVelocity {
            channel: kick,
            step: 1,
            velocity: 7.0,
        },
        Edit::SetStepVelocity {
            channel: snare,
            step: 5,
            velocity: -3.0,
        },
        Edit::SetChannelMix {
            channel: kick,
            volume: 5.0,
            pan: -9.0,
            mute: false,
            solo: true,
        },
        Edit::SetChannelMix {
            channel: snare,
            volume: -1.0,
            pan: 4.0,
            mute: true,
            solo: false,
        },
        Edit::SetSamplerParam {
            channel: kick,
            param: "tune".into(),
            value: 99.0,
        },
        Edit::SetSamplerParam {
            channel: snare,
            param: "root".into(),
            value: -40.0,
        },
        Edit::SetSwing { swing: 5.0 },
        Edit::SetSwing { swing: -2.0 },
        Edit::SetSwing { swing: 0.5 },
        Edit::SetTempo { bpm: 9_000.0 },
        Edit::SetTempo { bpm: 1.0 },
        Edit::SetPatternLength { steps: 999 },
        Edit::SetPatternLength { steps: 0 },
        Edit::SetPatternLength { steps: 16 },
        Edit::SetTimeSignature { beats: 3, unit: 8 },
        Edit::ClearStep {
            channel: snare,
            step: 3,
        },
        Edit::RemoveChannel { channel: snare },
    ];
    for edit in edits {
        session.apply(edit.clone(), now).unwrap();
        assert_mirror(&session, &controller, &format!("{edit:?}"));
    }
    let view = session.view();
    assert_eq!((view.bpm, view.pattern.swing), (20.0, 0.5));
    assert_eq!((view.channels[0].volume, view.channels[0].pan), (2.0, -1.0));
}

#[test]
fn opening_or_starting_a_project_during_a_save_does_not_take_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let backups = dir.path().join("backups");
    let old = dir.path().join("old.grooviq");
    let (mut session, _, _) = rig();
    let now = Instant::now();
    session.apply(Edit::SetTempo { bpm: 100.0 }, now).unwrap();

    // The app writes the file without the session's lock: a new project lands meanwhile.
    let job = session.begin_save(Some(&old)).unwrap();
    session.new_project().unwrap();
    session.apply(Edit::SetTempo { bpm: 150.0 }, now).unwrap();
    job.write(&backups).unwrap();
    assert!(
        !session.finish_save(&job),
        "the save belongs to another project"
    );
    let view = session.view();
    assert_eq!(view.path, None);
    assert_eq!(view.name, "Untitled");
    assert!(view.dirty);
    assert!(matches!(
        session.save(None, &backups),
        Err(SessionError::NoPath)
    ));
    assert_eq!(
        load(&old).unwrap().project.bpm,
        100.0,
        "the old file is untouched"
    );

    // The same with a project opened during the save.
    let job = session.begin_save(Some(&old)).unwrap();
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    let other = dir.path().join("other.grooviq");
    session.open(&other, loaded, decoded).unwrap();
    job.write(&backups).unwrap();
    assert!(!session.finish_save(&job));
    assert_eq!(
        session.view().path.as_deref(),
        Some(other.to_str().unwrap())
    );
}

#[test]
fn only_one_save_runs_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, _, _) = rig();
    let a = dir.path().join("a.grooviq");
    let b = dir.path().join("b.grooviq");
    let job = session.begin_save(Some(&a)).unwrap();
    let err = session.begin_save(Some(&b)).unwrap_err();
    assert!(matches!(err, SessionError::SaveRunning), "{err}");
    assert!(err.to_string().contains("already being saved"), "{err}");
    job.write(dir.path()).unwrap();
    assert!(session.finish_save(&job));
    assert_eq!(session.view().path.as_deref(), Some(a.to_str().unwrap()));

    // A failed write frees the next save too.
    let job = session.begin_save(Some(&b)).unwrap();
    drop(job);
    session.save_failed();
    assert!(session.begin_save(Some(&b)).is_ok());
}
