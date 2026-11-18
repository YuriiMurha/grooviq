//! The session: edits, the engine kept in step, dirty state, autosave timing, open and save.
//! The engine runs offline: its processor is driven by hand where a test needs to see what
//! reached it. Nothing is played.

use grooviq_dsp::sampler::SampleBuf;
use grooviq_engine::{
    AudioProcessor, ChannelId, Command, Controller, EngineConfig, STEP_TICKS, SeqNote, engine,
};
use grooviq_project::file::Disk;
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
    // The sampler editor keeps a region at least one frame long.
    assert_eq!(view.channels[0].sample_frames, Some(4_800));
    assert_eq!(view.channels[0].color, 0);
    assert_eq!(view.channels[1].color, 1);
    assert_ne!(kick, snare);
    assert_eq!(view.pattern.cells[&kick], vec![None; 16]);
    let state = controller.lock().unwrap().state().clone();
    let ids: Vec<u32> = state.channels.iter().map(|c| c.id.0).collect();
    assert_eq!(ids, vec![kick, snare]);
    assert!(state.channels[0].instrument.sample().is_some());
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
            id: 1,
            channel: kick,
            tick: 1_200,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
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
            slide: false,
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
    assert!(matches!(err, SessionError::UnknownParam { .. }), "{err}");
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
    assert_eq!(state.channels[0].instrument.params()[1], 24.0);
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

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
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

#[test]
fn a_failed_save_makes_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    session
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    let before = names(dir.path());
    for _ in 0..3 {
        // The disk fails while the new file is being staged (after the zip is in the temp file).
        let job = session.begin_save(None).unwrap();
        let mut disk = Ops::failing("written", &path);
        let err = job.write_on(&backups, &mut disk).unwrap_err();
        session.save_failed();
        assert!(matches!(err, SessionError::Save(_)), "{err}");
        assert!(err.to_string().contains("disk full"), "{err}");
    }
    assert_eq!(backup_tempos(&path, &backups), [100.0], "no backup rotated");
    assert_eq!(
        load(&path).unwrap().project.bpm,
        110.0,
        "the file is intact"
    );
    assert!(session.view().dirty, "the change is still unsaved");
    // And nothing of the failed saves is left beside the project.
    assert_eq!(names(dir.path()), before);
}

#[test]
fn a_project_too_big_to_reopen_is_not_saved_and_keeps_the_file_and_its_backups() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    // The same file, reopened and grown past the 64 MB `project.json` may hold.
    let mut project = load(&path).unwrap().project;
    project.patterns[0].name = "x".repeat(64 << 20);
    let loaded = Loaded {
        project,
        samples: Default::default(),
        warnings: vec![],
    };
    let decoded = decode_samples(&loaded);
    session.open(&path, loaded, decoded).unwrap();
    let before = names(dir.path());
    let folder = grooviq_project::backup_folder(&path, &backups).unwrap();
    let backed_up = names(&folder);
    let err = session.save(None, &backups).unwrap_err();
    assert!(matches!(err, SessionError::Save(_)), "{err}");
    assert!(
        err.to_string()
            .starts_with("The project is too big to save"),
        "{err}"
    );
    assert_eq!(
        load(&path).unwrap().project.bpm,
        110.0,
        "the file is intact"
    );
    assert_eq!(backup_tempos(&path, &backups), [100.0], "no backup rotated");
    assert_eq!(names(&folder), backed_up);
    assert_eq!(names(dir.path()), before, "nothing written");
}

/// A disk that records a save's file operations, checks the target is untouched until the
/// rename, and fails the operation named `fail` ("disk full").
struct Ops {
    ops: Vec<&'static str>,
    fail: Option<&'static str>,
    target: PathBuf,
    old: Vec<u8>,
}

impl Ops {
    fn failing(fail: &'static str, target: &Path) -> Self {
        Self {
            ops: Vec::new(),
            fail: Some(fail).filter(|f| !f.is_empty()),
            target: target.to_path_buf(),
            old: std::fs::read(target).unwrap(),
        }
    }

    fn step(&mut self, op: &'static str) -> std::io::Result<()> {
        self.ops.push(op);
        assert_eq!(
            std::fs::read(&self.target).unwrap(),
            self.old,
            "the target changed before the rename"
        );
        if self.fail == Some(op) {
            return Err(std::io::Error::other(format!(
                "injected: disk full at {op}"
            )));
        }
        Ok(())
    }
}

impl Disk for Ops {
    fn written(&mut self, _file: &mut std::fs::File) -> std::io::Result<()> {
        self.step("written")
    }

    fn sync(&mut self, file: &std::fs::File) -> std::io::Result<()> {
        if file.metadata()?.is_dir() {
            return file.sync_all();
        }
        // What is synced is the whole zip: written before, not after.
        use std::io::{Read, Seek};
        let mut bytes = Vec::new();
        let mut reader = file;
        reader.rewind()?;
        reader.read_to_end(&mut bytes)?;
        assert!(
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).is_ok(),
            "synced before the zip was complete"
        );
        #[cfg(unix)]
        let backup = {
            use std::os::unix::fs::MetadataExt;
            let m = file.metadata()?;
            let folder = grooviq_project::backup_folder(
                &self.target,
                &self.target.parent().unwrap().join("backups"),
            )?;
            std::fs::read_dir(folder).is_ok_and(|entries| {
                entries.flatten().any(|e| {
                    e.metadata()
                        .is_ok_and(|x| x.dev() == m.dev() && x.ino() == m.ino())
                })
            })
        };
        #[cfg(not(unix))]
        let backup = self.ops.contains(&"sync");
        self.step(if backup { "backup-sync" } else { "sync" })?;
        // The production sync, as a save makes it.
        grooviq_project::file::RealDisk.sync(file)
    }

    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.step("rename")?;
        std::fs::rename(from, to)
    }
}

#[test]
fn a_save_writes_then_syncs_then_renames() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    session
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    let job = session.begin_save(None).unwrap();
    let mut disk = Ops::failing("", &path);
    job.write_on(&backups, &mut disk).unwrap();
    session.finish_save(&job);
    assert_eq!(disk.ops, ["written", "sync", "backup-sync", "rename"]);
    assert_eq!(load(&path).unwrap().project.bpm, 120.0);
    assert_eq!(backup_tempos(&path, &backups), [100.0, 110.0]);
}

#[test]
fn a_failed_sync_keeps_the_file_and_its_backups() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    session
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    let before = names(dir.path());
    let folder = grooviq_project::backup_folder(&path, &backups).unwrap();
    let backed_up = names(&folder);
    let job = session.begin_save(None).unwrap();
    let mut disk = Ops::failing("sync", &path);
    let err = job.write_on(&backups, &mut disk).unwrap_err();
    session.save_failed();
    assert_eq!(disk.ops, ["written", "sync"], "never renamed");
    assert!(err.to_string().contains("disk full at sync"), "{err}");
    assert_eq!(
        load(&path).unwrap().project.bpm,
        110.0,
        "the file is intact"
    );
    assert_eq!(backup_tempos(&path, &backups), [100.0], "no backup rotated");
    assert_eq!(names(&folder), backed_up, "the backups are as they were");
    assert_eq!(names(dir.path()), before, "no temp file left");
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
        instrument: grooviq_engine::InstrumentKind::Sampler,
        sampler: BTreeMap::new(),
        synth: BTreeMap::new(),
        preset: None,
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
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
    for (id, (channel, tick)) in (1..).zip([(1, 0), (2, 0), (2, 960), (3, 0)]) {
        project.patterns[0].notes.push(Note {
            id,
            channel,
            tick,
            length: 240,
            key: 60,
            velocity: 0.8,
            slide: false,
        });
    }
    project.patterns[0].next_note_id = 5;
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
    // A missing sample has no length to show.
    let frames: Vec<Option<u64>> = view.channels.iter().map(|c| c.sample_frames).collect();
    assert_eq!(frames, vec![Some(4_800), None, None]);
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
    let has_sample: Vec<bool> = state
        .channels
        .iter()
        .map(|c| c.instrument.sample().is_some())
        .collect();
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
            .instrument
            .sample()
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
            .instrument
            .sample()
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
    let press1 = session.preview_on(kick).unwrap();
    assert!(render(&mut processor, 20) > 0.01);
    session.preview_off(kick, press1).unwrap();
    assert!(matches!(
        session.preview_on(42),
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
    for (id, (tick, velocity)) in (1..).zip([(100, 0.3), (480, 0.6), (16 * 240, 0.9)]) {
        notes.push(Note {
            id,
            channel: 1,
            tick,
            length: 240,
            key: 60,
            velocity,
            slide: false,
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
            assert_eq!(
                *value,
                engine.instrument.params()[index],
                "after {after}: {param}"
            );
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

/// A project with one sampler channel (id 1, no sample) whose only pattern holds `notes`
/// and counts note ids from `next_note_id`, opened in a fresh session.
fn opened_with_notes(notes: Vec<Note>, next_note_id: u32) -> Session {
    let mut project = Project::new();
    project.channels.push(Channel {
        sample: None,
        ..channel(1, "Kick", None)
    });
    project.next_id = 2;
    project.patterns[0].notes = notes;
    project.patterns[0].next_note_id = next_note_id;
    let loaded = Loaded {
        project,
        samples: Default::default(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    let (mut session, _, _) = rig();
    session
        .open(Path::new("/p/notes.grooviq"), loaded, decoded)
        .unwrap();
    session
}

fn plain(id: u32, tick: u32) -> Note {
    Note {
        id,
        channel: 1,
        tick,
        length: 240,
        key: 60,
        velocity: 0.8,
        slide: false,
    }
}

#[test]
fn a_pattern_out_of_note_ids_refuses_new_notes_and_never_repeats_one() {
    let now = Instant::now();
    let ids =
        |s: &Session| -> Vec<u32> { s.project().patterns[0].notes.iter().map(|n| n.id).collect() };

    // One id left: the first step gets it, the next additions are refused, by both paths.
    let mut s = opened_with_notes(Vec::new(), u32::MAX - 1);
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        now,
    )
    .unwrap();
    assert_eq!(ids(&s), vec![u32::MAX - 1]);
    for edit in [
        Edit::ToggleStep {
            channel: 1,
            step: 1,
        },
        Edit::SetStepVelocity {
            channel: 1,
            step: 2,
            velocity: 0.5,
        },
    ] {
        let what = format!("{edit:?}");
        let result = s.apply(edit, now);
        assert!(
            matches!(result, Err(SessionError::NoteIdsUsedUp)),
            "{what}: {result:?}"
        );
        assert_eq!(ids(&s), vec![u32::MAX - 1], "nothing added");
    }
    // Changing and removing notes still works.
    s.apply(
        Edit::SetStepVelocity {
            channel: 1,
            step: 0,
            velocity: 0.5,
        },
        now,
    )
    .unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        now,
    )
    .unwrap();
    assert!(ids(&s).is_empty());

    // Opening (which validates) renumbers a used-up pattern, so additions work again.
    let mut s = opened_with_notes(vec![plain(7, 0)], u32::MAX);
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 1,
        },
        now,
    )
    .unwrap();
    assert_eq!(ids(&s), vec![1, 2]);
    assert!(
        SessionError::NoteIdsUsedUp
            .to_string()
            .contains("open it again"),
        "the message says how to get ids back"
    );
}

#[test]
fn reopening_renumbers_a_pattern_out_of_note_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ids.grooviq");
    let mut project = Project::new();
    project.channels.push(Channel {
        sample: None,
        ..channel(1, "Kick", None)
    });
    project.next_id = 2;
    project.patterns[0].notes = vec![plain(9, 0), plain(u32::MAX - 1, 240)];
    project.patterns[0].next_note_id = u32::MAX;
    grooviq_project::save(&project, &Default::default(), &path).unwrap();

    let loaded = load(&path).unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            "Pattern \"Pattern 1\": its note ids left no room for new notes; 2 notes were renumbered."
                .to_string()
        ]
    );
    let decoded = decode_samples(&loaded);
    let (mut s, _, _) = rig();
    s.open(&path, loaded, decoded).unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 2,
        },
        Instant::now(),
    )
    .unwrap();
    let ids: Vec<u32> = s.project().patterns[0].notes.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn step_additions_stop_at_the_note_cap() {
    let now = Instant::now();
    let cap = grooviq_project::MAX_NOTES_PER_PATTERN as u32;
    let full: Vec<Note> = (1..=cap).map(|id| plain(id, 0)).collect();
    let mut s = opened_with_notes(full, cap + 1);
    for edit in [
        Edit::ToggleStep {
            channel: 1,
            step: 1,
        },
        Edit::SetStepVelocity {
            channel: 1,
            step: 2,
            velocity: 0.5,
        },
    ] {
        let what = format!("{edit:?}");
        let result = s.apply(edit, now);
        assert!(
            matches!(result, Err(SessionError::TooManyNotes)),
            "{what}: {result:?}"
        );
        assert_eq!(
            s.project().patterns[0].notes.len(),
            cap as usize,
            "nothing added"
        );
        assert!(!s.view().dirty, "a refused edit changes nothing");
    }
    assert!(SessionError::TooManyNotes.to_string().contains("100000"));
    // Removing makes room again.
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 0,
        },
        now,
    )
    .unwrap();
    s.apply(
        Edit::ToggleStep {
            channel: 1,
            step: 1,
        },
        now,
    )
    .unwrap();
    assert_eq!(s.project().patterns[0].notes.len(), 1);
}

// --- Task 7: roll channels, synth channels, instrument parameters, recording, the key. ---

use grooviq_engine::RecordEvent;
use grooviq_project::ProjectKey;
use grooviq_project::notes::{NewNote, NotesEdit};

/// Adds a synth channel from `preset` and returns its id.
fn add_synth(session: &mut Session, preset: &str) -> u32 {
    let view = session.add_synth_channel(preset, Instant::now()).unwrap();
    view.channels.last().unwrap().id
}

fn add_notes(session: &mut Session, add: Vec<NewNote>) -> Vec<u32> {
    let edit = NotesEdit {
        pattern_id: 0,
        generation: session.generation(),
        base_revision: session.view().notes_revision,
        add,
        update: Vec::new(),
        delete: Vec::new(),
    };
    session.edit_notes(edit, Instant::now()).unwrap().added_ids
}

fn melodic(channel: u32, tick: u32, key: u8) -> NewNote {
    NewNote {
        channel,
        tick,
        length: 480,
        key,
        velocity: 0.8,
        slide: false,
    }
}

#[test]
fn step_edits_on_a_roll_channel_are_refused() {
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 0,
            },
            now,
        )
        .unwrap();
    assert!(!session.view().channels[0].roll, "plain steps only");
    add_notes(&mut session, vec![melodic(kick, 960, 64)]);
    assert!(session.view().channels[0].roll);
    let notes = session.project().patterns[0].notes.clone();
    let revision = session.view().notes_revision;
    for edit in [
        Edit::ToggleStep {
            channel: kick,
            step: 0,
        },
        Edit::SetStepVelocity {
            channel: kick,
            step: 0,
            velocity: 0.2,
        },
        Edit::ClearStep {
            channel: kick,
            step: 4,
        },
    ] {
        let err = session.apply(edit.clone(), now).unwrap_err();
        assert!(matches!(err, SessionError::RollChannel { .. }), "{edit:?}");
        assert_eq!(
            err.to_string(),
            "kick has piano-roll notes. Edit them in the piano roll (F7)."
        );
    }
    assert_eq!(session.project().patterns[0].notes, notes);
    assert_eq!(session.view().notes_revision, revision);
    // The grid shows plain steps only: the melodic note is not a cell.
    let row = cells(&session, kick);
    assert_eq!(row[0], Some(0.8));
    assert_eq!(row[4], None);
}

#[test]
fn step_edits_bump_the_notes_revision() {
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let before = session.view().notes_revision;
    session
        .apply(
            Edit::ToggleStep {
                channel: kick,
                step: 2,
            },
            now,
        )
        .unwrap();
    assert_eq!(session.view().notes_revision, before + 1);
    session.apply(Edit::SetSwing { swing: 0.5 }, now).unwrap();
    assert_eq!(
        session.view().notes_revision,
        before + 2,
        "swing advances the gesture revision too"
    );
}

#[test]
fn adding_a_synth_channel_uses_the_preset() {
    let (mut session, controller, _) = rig();
    let id = add_synth(&mut session, "808 Glide");
    let view = session.view();
    let c = &view.channels[0];
    assert_eq!(c.name, "808 Glide");
    assert_eq!(c.preset.as_deref(), Some("808 Glide"));
    assert_eq!(c.kind, grooviq_project::session::ChannelKind::Synth);
    assert!(c.sampler.is_empty());
    assert_eq!(c.sample_name, None);
    assert!(!c.missing);
    let preset = grooviq_dsp::synth::preset("808 Glide").unwrap();
    assert_eq!(c.synth.len(), preset.len());
    for (index, value) in &preset {
        assert_eq!(
            c.synth[grooviq_dsp::synth::PARAM_IDS[*index as usize]],
            *value
        );
    }
    let state = controller.lock().unwrap().state().clone();
    let setup = state.channels.iter().find(|s| s.id.0 == id).unwrap();
    let values: Vec<f32> = preset.iter().map(|(_, v)| *v).collect();
    assert_eq!(setup.instrument.params(), values.as_slice());
    assert!(view.dirty);
    assert!(session.autosave_due(Instant::now() + STRUCTURAL_DELAY));

    assert!(session.add_synth_channel("Nope", Instant::now()).is_err());
    assert_eq!(session.view().channels.len(), 1);

    // Loading another preset replaces all 40 values and names it.
    session
        .apply(
            Edit::LoadSynthPreset {
                channel: id,
                preset: "Reese".into(),
            },
            Instant::now(),
        )
        .unwrap();
    let reese = grooviq_dsp::synth::preset("Reese").unwrap();
    let values: Vec<f32> = reese.iter().map(|(_, v)| *v).collect();
    let state = controller.lock().unwrap().state().clone();
    assert_eq!(state.channels[0].instrument.params(), values.as_slice());
    assert_eq!(session.view().channels[0].preset.as_deref(), Some("Reese"));
    assert_eq!(
        session.view().channels[0].name,
        "808 Glide",
        "the name stays"
    );
    // A sampler channel takes no synth preset.
    let kick = add(&mut session, "kick", Instant::now());
    assert!(
        session
            .apply(
                Edit::LoadSynthPreset {
                    channel: kick,
                    preset: "Reese".into()
                },
                Instant::now()
            )
            .is_err()
    );
}

#[test]
fn instrument_params_go_by_the_channel_s_kind() {
    let (mut session, controller, _) = rig();
    let now = Instant::now();
    let synth = add_synth(&mut session, "Init");
    let kick = add(&mut session, "kick", now);
    session
        .apply(
            Edit::SetInstrumentParam {
                channel: synth,
                param: "filter_cutoff".into(),
                value: 1_234.0,
            },
            now,
        )
        .unwrap();
    session
        .apply(
            Edit::SetInstrumentParam {
                channel: kick,
                param: "tune".into(),
                value: 5.0,
            },
            now,
        )
        .unwrap();
    let view = session.view();
    assert_eq!(view.channels[0].synth["filter_cutoff"], 1_234.0);
    assert_eq!(view.channels[1].sampler["tune"], 5.0);
    let state = controller.lock().unwrap().state().clone();
    let cutoff = grooviq_dsp::synth::PARAM_IDS
        .iter()
        .position(|p| *p == "filter_cutoff")
        .unwrap();
    assert_eq!(state.channels[0].instrument.params()[cutoff], 1_234.0);
    let tune = grooviq_dsp::sampler::PARAM_IDS
        .iter()
        .position(|p| *p == "tune")
        .unwrap();
    assert_eq!(state.channels[1].instrument.params()[tune], 5.0);

    // A sampler id on a synth (and the other way round) is refused.
    for (channel, param) in [(synth, "tune"), (kick, "filter_cutoff")] {
        let err = session
            .apply(
                Edit::SetInstrumentParam {
                    channel,
                    param: param.into(),
                    value: 1.0,
                },
                now,
            )
            .unwrap_err();
        assert!(matches!(err, SessionError::UnknownParam { .. }), "{err}");
    }
    assert_eq!(
        session.view().channels[0].synth["filter_cutoff"],
        1_234.0,
        "unchanged"
    );

    // The choke group is clamped to 0..=16 and reaches the engine.
    session
        .apply(
            Edit::SetChoke {
                channel: kick,
                group: 99,
            },
            now,
        )
        .unwrap();
    assert_eq!(session.view().channels[1].choke, 16);
    assert_eq!(controller.lock().unwrap().state().channels[1].choke, 16);
}

#[test]
fn param_live_changes_the_engine_not_the_project() {
    let (mut session, controller, _) = rig();
    let synth = add_synth(&mut session, "Init");
    let dir = tempfile::tempdir().unwrap();
    session
        .save(Some(&dir.path().join("p.grooviq")), dir.path())
        .unwrap();
    let project = session.project().clone();
    session.param_live(synth, "filter_cutoff", 2_000.0).unwrap();
    let cutoff = grooviq_dsp::synth::PARAM_IDS
        .iter()
        .position(|p| *p == "filter_cutoff")
        .unwrap();
    assert_eq!(
        controller.lock().unwrap().state().channels[0]
            .instrument
            .params()[cutoff],
        2_000.0
    );
    assert_eq!(*session.project(), project);
    assert!(!session.view().dirty);
    assert!(session.param_live(synth, "tune", 1.0).is_err());
    assert!(session.param_live(99, "filter_cutoff", 1.0).is_err());
}

/// Starts the transport for a take; returns what was recorded so far (nothing).
fn start_take(controller: &Mutex<Controller>, processor: &mut AudioProcessor) -> Vec<RecordEvent> {
    let mut events = Vec::new();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(processor, 2);
    controller.lock().unwrap().take_recorded(&mut events);
    events
}

/// A looping kick channel (each note sounds until its release) and its processor, warmed up.
fn looping_kick() -> (Session, Arc<Mutex<Controller>>, AudioProcessor, u32) {
    let (mut session, controller, mut processor) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    session
        .apply(
            Edit::SetInstrumentParam {
                channel: kick,
                param: "loop".into(),
                value: 1.0,
            },
            now,
        )
        .unwrap();
    render(&mut processor, 4);
    (session, controller, processor, kick)
}

/// The steady level once the last change has settled.
fn settled(processor: &mut AudioProcessor) -> f32 {
    render(processor, 200);
    render(processor, 10)
}

#[test]
fn a_preview_and_a_typing_note_on_one_key_each_end_their_own() {
    // The typing keyboard holds key 60 (velocity 1) and the row's preview plays key 60 too
    // (velocity 0.8): whichever lets go first, the other sounds on until its own release.
    for preview_first in [true, false] {
        let (session, _controller, mut processor, kick) = looping_kick();
        let typing = session.live_note_on(kick, 60, 1.0).unwrap();
        let typing_level = settled(&mut processor);
        let preview = session.preview_on(kick).unwrap();
        let both = settled(&mut processor);
        assert!(
            (both - 1.8 * typing_level).abs() < 1e-3,
            "{both} vs {typing_level}"
        );
        let alone = if preview_first {
            session.preview_off(kick, preview).unwrap();
            typing_level
        } else {
            session.live_note_off(kick, 60, typing).unwrap();
            0.8 * typing_level
        };
        let left = settled(&mut processor);
        assert!(
            (left - alone).abs() < 1e-3,
            "preview first {preview_first}: {left}, expected {alone}"
        );
        if preview_first {
            session.live_note_off(kick, 60, typing).unwrap();
        } else {
            session.preview_off(kick, preview).unwrap();
        }
        assert!(settled(&mut processor) < 1e-4, "both ended");
    }
}

#[test]
fn a_refused_preview_leaves_the_typing_note_to_its_own_release() {
    let (session, controller, mut processor, kick) = looping_kick();
    let typing = session.live_note_on(kick, 60, 1.0).unwrap();
    let typing_level = settled(&mut processor);
    {
        let mut c = controller.lock().unwrap();
        while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    }
    assert!(
        session.preview_on(kick).is_err(),
        "the full queue refuses it"
    );
    // No press was made, so the preview has nothing to release; the typing note sounds on.
    let left = settled(&mut processor);
    assert!(
        (left - typing_level).abs() < 1e-3,
        "{left} vs {typing_level}"
    );
    session.live_note_off(kick, 60, typing).unwrap();
    assert!(settled(&mut processor) < 1e-4);
}

#[test]
fn recording_stop_sends_end_and_adds_the_notes() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    assert!(session.view().recording);
    let mut events = start_take(&controller, &mut processor);
    let press3 = session.live_note_on(synth, 64, 0.9).unwrap();
    render(&mut processor, 40);
    // Stopped while the key is held: the take ends there.
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    session.live_note_off(synth, 64, press3).unwrap();
    render(&mut processor, 2);
    controller.lock().unwrap().take_recorded(&mut events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RecordEvent::End { .. })),
        "{events:?}"
    );
    let before = session.view().notes_revision;
    let revision = session.collect_recorded(&events, Instant::now()).revision;
    assert_eq!(revision, Some(before + 1));
    let notes = &session.project().patterns[0].notes;
    assert_eq!(notes.len(), 1);
    let n = notes[0];
    assert_eq!(
        (n.channel, n.key, n.velocity, n.slide),
        (synth, 64, 0.9, false)
    );
    // 40 blocks of 64 frames at 48 kHz and 140 BPM (2240 ticks a second): about 119 ticks,
    // starting where the note-on was heard, two blocks after Play.
    assert!((115..=125).contains(&n.length), "{n:?}");
    assert_eq!(n.tick, 128 * 2240 / 48_000, "{n:?}");
    assert!(session.view().dirty);
    assert!(!session.view().recording);
    // Nothing more to collect.
    assert_eq!(session.collect_recorded(&[], Instant::now()).revision, None);
}

#[test]
fn editing_while_recording_keeps_both() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    let mut events = start_take(&controller, &mut processor);
    let start = session.view().notes_revision;
    let press4 = session.live_note_on(synth, 60, 0.8).unwrap();
    render(&mut processor, 10);
    // The person draws a note while the take runs.
    let drawn = add_notes(&mut session, vec![melodic(synth, 1_920, 72)]);
    session.live_note_off(synth, 60, press4).unwrap();
    render(&mut processor, 2);
    controller.lock().unwrap().take_recorded(&mut events);
    session
        .collect_recorded(&events, Instant::now())
        .revision
        .unwrap();
    let notes = &session.project().patterns[0].notes;
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().any(|n| n.id == drawn[0] && n.key == 72));
    assert!(notes.iter().any(|n| n.key == 60 && n.id != drawn[0]));
    assert_eq!(session.view().notes_revision, start + 2);
}

#[test]
fn recorded_notes_of_a_removed_channel_are_dropped() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    let mut events = start_take(&controller, &mut processor);
    let press5 = session.live_note_on(synth, 60, 0.8).unwrap();
    render(&mut processor, 4);
    session.live_note_off(synth, 60, press5).unwrap();
    render(&mut processor, 4);
    controller.lock().unwrap().take_recorded(&mut events);
    session
        .apply(Edit::RemoveChannel { channel: synth }, Instant::now())
        .unwrap();
    assert_eq!(
        session.collect_recorded(&events, Instant::now()).revision,
        None
    );
    assert!(session.project().patterns[0].notes.is_empty());
}

#[test]
fn opening_a_project_mid_take_records_nothing_into_it() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    let mut events = start_take(&controller, &mut processor);
    let press6 = session.live_note_on(synth, 60, 0.8).unwrap();
    render(&mut processor, 4);
    session.live_note_off(synth, 60, press6).unwrap();
    render(&mut processor, 1);
    // A whole note of the old take is still queued when the next project opens.
    controller.lock().unwrap().take_recorded(&mut events);
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    let generation = session.generation();
    assert!(
        session
            .open(Path::new("/p/beat.grooviq"), loaded, decoded)
            .is_err()
    );
    assert_eq!(session.generation(), generation);
    session.collect_recorded(&events, Instant::now());
    events.clear();
    open_fixture(&mut session, &controller, &mut processor);
    let notes = session.project().patterns[0].notes.clone();
    render(&mut processor, 2);
    controller.lock().unwrap().take_recorded(&mut events);
    assert_eq!(
        session.collect_recorded(&events, Instant::now()).revision,
        None
    );
    assert_eq!(session.project().patterns[0].notes, notes);
}

#[test]
fn removing_a_channel_drops_its_notes_and_bumps_the_revision() {
    let (mut session, controller, _) = rig();
    let synth = add_synth(&mut session, "Init");
    let other = add_synth(&mut session, "Pluck");
    add_notes(
        &mut session,
        vec![melodic(synth, 0, 60), melodic(other, 0, 48)],
    );
    let before = session.view().notes_revision;
    let view = session
        .apply(Edit::RemoveChannel { channel: synth }, Instant::now())
        .unwrap();
    assert_eq!(view.notes_revision, before + 1);
    assert!(
        session.project().patterns[0]
            .notes
            .iter()
            .all(|n| n.channel == other)
    );
    assert!(
        controller
            .lock()
            .unwrap()
            .state()
            .pattern
            .notes
            .iter()
            .all(|n| n.channel.0 == other)
    );
}

#[test]
fn opening_a_project_moves_the_notes_revision_on() {
    let (mut session, _, _) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(&mut session, vec![melodic(synth, 0, 60)]);
    let before = session.view().notes_revision;
    let view = session.new_project().unwrap();
    assert!(
        view.notes_revision > before,
        "an edit made against the old project must read as stale"
    );
}

#[test]
fn the_project_key_round_trips() {
    let (mut session, _, _) = rig();
    let key = ProjectKey {
        root: 9,
        scale: "minor".into(),
    };
    let view = session
        .apply(
            Edit::SetProjectKey {
                key: Some(key.clone()),
            },
            Instant::now(),
        )
        .unwrap();
    assert_eq!(view.key, Some(key.clone()));
    assert!(view.dirty);
    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(
        json["key"],
        serde_json::json!({"root": 9, "scale": "minor"})
    );
    let edit: Edit = serde_json::from_str(r#"{"kind":"setProjectKey","key":null}"#).unwrap();
    assert_eq!(edit, Edit::SetProjectKey { key: None });

    let dir = tempfile::tempdir().unwrap();
    let path = session
        .save(Some(&dir.path().join("k.grooviq")), dir.path())
        .unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.project.key, Some(key.clone()));

    for bad in [
        ProjectKey {
            root: 12,
            scale: "minor".into(),
        },
        ProjectKey {
            root: 0,
            scale: "mystery".into(),
        },
    ] {
        let err = session
            .apply(Edit::SetProjectKey { key: Some(bad) }, Instant::now())
            .unwrap_err();
        assert!(matches!(err, SessionError::InvalidValue(_)), "{err}");
    }
    assert_eq!(session.view().key, Some(key));
    session
        .apply(Edit::SetProjectKey { key: None }, Instant::now())
        .unwrap();
    assert_eq!(session.view().key, None);
}

#[test]
fn the_time_signature_reaches_the_engine() {
    let (mut session, controller, _) = rig();
    session
        .apply(Edit::SetTimeSignature { beats: 7, unit: 8 }, Instant::now())
        .unwrap();
    assert_eq!(controller.lock().unwrap().state().time_signature, (7, 8));
    assert_eq!(session.export_state().time_signature, (7, 8));
}

#[test]
fn waveform_peaks_cover_the_sample() {
    let (mut session, _, _) = rig();
    let path = PathBuf::from("/sounds/ramp.wav");
    // Left climbs 0..1, right at half of it: the mono mix climbs, each quarter from its first frame
    // (min) to its last (max).
    let l: Vec<f32> = (0..1_000).map(|i| i as f32 / 1_000.0).collect();
    let r: Vec<f32> = l.iter().map(|x| x * 0.5).collect();
    let buf = Arc::new(SampleBuf {
        rate: RATE,
        l: l.clone(),
        r: r.clone(),
    });
    let view = session
        .add_channel(&path, sample_file("ramp.wav"), buf, Instant::now())
        .unwrap();
    let id = view.channels[0].id;
    let bytes = session.waveform(id, 4).unwrap();
    assert_eq!(bytes.len(), 4 * 8);
    let pairs: Vec<(f32, f32)> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect::<Vec<f32>>()
        .chunks(2)
        .map(|p| (p[0], p[1]))
        .collect();
    let mono = |i: usize| (l[i] + r[i]) * 0.5;
    for (q, (min, max)) in pairs.iter().enumerate() {
        assert_eq!(*min, mono(q * 250), "quarter {q}");
        assert_eq!(*max, mono(q * 250 + 249), "quarter {q}");
    }
    // More points than frames: every point still holds a frame.
    let many = session.waveform(id, 4_000).unwrap();
    assert_eq!(many.len(), 4_000 * 8);
    let synth = add_synth(&mut session, "Init");
    assert_eq!(session.waveform(synth, 4), None);
    assert_eq!(session.waveform(99, 4), None);
}

#[test]
fn channel_requests_from_a_replaced_project_change_nothing() {
    let (mut session, _, _) = rig();
    let now = Instant::now();
    let kick = add(&mut session, "kick", now);
    let a = session.generation();
    // B is opened over A (the same channel ids), clean.
    let loaded = Loaded {
        project: session.project().clone(),
        samples: Default::default(),
        warnings: vec![],
    };
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/music/b.grooviq"), loaded, decoded)
        .unwrap();
    let before = session.view();
    let refused = |r: Result<_, SessionError>| {
        assert!(matches!(r, Err(SessionError::ProjectReplaced)), "{r:?}");
    };
    // Requests made in A, arriving after their files were decoded: refused, B as it was.
    refused(session.add_channel_in(
        a,
        Path::new("/s/late.wav"),
        sample_file("late.wav"),
        buffer(),
        now,
    ));
    refused(session.add_synth_channel_in(a, "Init", now));
    refused(session.set_channel_sample_in(
        a,
        kick,
        Path::new("/s/new.wav"),
        sample_file("new.wav"),
        buffer(),
        now,
    ));
    assert_eq!(session.view(), before);
    assert!(!session.view().dirty);
    assert_eq!(
        SessionError::ProjectReplaced.to_string(),
        "The project changed before this finished, so it was not applied."
    );
    // B's own go through.
    let b = session.generation();
    session
        .add_channel_in(
            b,
            Path::new("/s/late.wav"),
            sample_file("late.wav"),
            buffer(),
            now,
        )
        .unwrap();
    session.add_synth_channel_in(b, "Init", now).unwrap();
    let view = session
        .set_channel_sample_in(
            b,
            kick,
            Path::new("/s/new.wav"),
            sample_file("new.wav"),
            buffer(),
            now,
        )
        .unwrap();
    assert_eq!(view.channels.len(), 3);
    assert!(view.dirty);
}

#[test]
fn waveform_requests_are_held_to_the_point_budget() {
    use grooviq_project::session::{MAX_WAVEFORM_POINTS, waveform_peaks};
    // Three frames whose mono mix is -0.5, 0.25 and 1.0.
    let sample = SampleBuf {
        rate: RATE,
        l: vec![-1.0, 0.5, 1.0],
        r: vec![0.0, 0.0, 1.0],
    };
    let pairs = |bytes: &[u8]| -> Vec<(f32, f32)> {
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                let (min, max) = c.split_at(4);
                (
                    f32::from_le_bytes(min.try_into().unwrap()),
                    f32::from_le_bytes(max.try_into().unwrap()),
                )
            })
            .collect()
    };
    // None asked for: one point, the whole sample.
    assert_eq!(pairs(&waveform_peaks(&sample, 0)), [(-0.5, 1.0)]);
    let limit = MAX_WAVEFORM_POINTS as usize;
    for asked in [MAX_WAVEFORM_POINTS, MAX_WAVEFORM_POINTS + 1, u32::MAX] {
        let got = pairs(&waveform_peaks(&sample, asked));
        assert_eq!(got.len(), limit, "{asked} points asked for");
        // Each point holds one of the three frames, in order, and together they hold all three.
        assert_eq!(got[0], (-0.5, -0.5), "{asked}");
        assert_eq!(got[limit - 1], (1.0, 1.0), "{asked}");
        assert!(got.contains(&(0.25, 0.25)), "{asked}");
        assert!(
            got.iter()
                .all(|&(min, max)| min == max && [-0.5, 0.25, 1.0].contains(&min))
        );
        assert!(
            got.windows(2).all(|w| w[0].0 <= w[1].0),
            "{asked}: out of order"
        );
    }
}

// --- Fix round 1: recording across project replacement, engine back-pressure, the note cap. ---

use grooviq_project::session::Recorded;

/// Opens the missing-sample fixture (channel ids 1 to 3) and returns its notes.
fn open_fixture(
    session: &mut Session,
    controller: &Mutex<Controller>,
    processor: &mut AudioProcessor,
) -> Vec<Note> {
    // Replacement now settles the originating take rather than dropping it unobserved.
    for _ in 0..8 {
        if session.settle_recording().is_ok() {
            break;
        }
        poll(session, controller, processor);
    }
    session
        .settle_recording()
        .expect("the original take settled");
    let loaded = loaded_with_a_missing_sample();
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/beat.grooviq"), loaded, decoded)
        .unwrap();
    session.project().patterns[0].notes.clone()
}

/// Renders a few blocks and hands everything recorded to the session.
fn poll(
    session: &mut Session,
    controller: &Mutex<Controller>,
    processor: &mut AudioProcessor,
) -> Recorded {
    render(processor, 4);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    session.collect_recorded(&events, Instant::now())
}

/// A whole note played and released during a take, not yet collected.
fn play_a_note(session: &Session, processor: &mut AudioProcessor, channel: u32) {
    let press7 = session.live_note_on(channel, 60, 0.8).unwrap();
    render(processor, 4);
    session.live_note_off(channel, 60, press7).unwrap();
    render(processor, 2);
}

#[test]
fn record_off_then_open_records_nothing_into_the_new_project() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    assert_eq!(synth, 1, "the fixture's Kick has the same id");
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 2);
    play_a_note(&session, &mut processor, synth);
    // Recording is off before the recorder thread polls.
    session.set_recording(false).unwrap();
    render(&mut processor, 2);
    let notes = open_fixture(&mut session, &controller, &mut processor);
    assert_eq!(
        poll(&mut session, &controller, &mut processor).revision,
        None
    );
    assert_eq!(session.project().patterns[0].notes, notes);
}

#[test]
fn stop_then_open_records_nothing_into_the_new_project() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 2);
    play_a_note(&session, &mut processor, synth);
    controller.lock().unwrap().send(Command::Stop).unwrap();
    render(&mut processor, 2);
    let notes = open_fixture(&mut session, &controller, &mut processor);
    assert_eq!(
        poll(&mut session, &controller, &mut processor).revision,
        None
    );
    assert_eq!(session.project().patterns[0].notes, notes);
}

#[test]
fn a_queued_play_then_open_records_nothing_into_the_new_project_but_the_next_take_does() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    render(&mut processor, 2);
    // Play and a whole note are still queued when the project opens.
    controller.lock().unwrap().send(Command::Play).unwrap();
    let press8 = session.live_note_on(synth, 60, 0.8).unwrap();
    session.live_note_off(synth, 60, press8).unwrap();
    let notes = open_fixture(&mut session, &controller, &mut processor);
    assert_eq!(
        poll(&mut session, &controller, &mut processor).revision,
        None
    );
    assert_eq!(session.project().patterns[0].notes, notes);

    // Selection/replacement settled and disarmed the original take. Arm a new one explicitly.
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 2);
    play_a_note(&session, &mut processor, 1);
    assert!(
        poll(&mut session, &controller, &mut processor)
            .revision
            .is_some()
    );
    assert_eq!(session.project().patterns[0].notes.len(), notes.len() + 1);
}

#[test]
fn a_recorded_note_survives_a_full_engine_queue_and_is_added_once() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    render(&mut processor, 2);
    play_a_note(&session, &mut processor, synth);
    let mut events = Vec::new();
    controller.lock().unwrap().take_recorded(&mut events);
    // The engine's command queue is full when the note is committed.
    while controller
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let got = session.collect_recorded(&events, Instant::now());
    assert_eq!(got, Recorded::default(), "nothing yet, nothing lost");
    assert!(session.project().patterns[0].notes.is_empty());
    // The queue drains; the next poll commits the note, once.
    render(&mut processor, 4);
    let got = session.collect_recorded(&[], Instant::now());
    assert!(got.revision.is_some(), "{got:?}");
    assert_eq!(session.project().patterns[0].notes.len(), 1);
    assert_eq!(
        session.collect_recorded(&[], Instant::now()),
        Recorded::default()
    );
    assert_eq!(session.project().patterns[0].notes.len(), 1);
}

/// `n` whole notes on `channel`, as the engine records them, after the session's barrier.
fn whole_notes(channel: u32, n: u32) -> Vec<RecordEvent> {
    (0..n)
        .flat_map(|i| {
            let note = |on, tick: u64| RecordEvent::Note {
                channel: ChannelId(channel),
                key: 40 + i as u8,
                velocity: 0.8,
                on,
                tick,
                position: tick as u32,
                take: 0,
                id: grooviq_engine::NoteId(u64::from(40 + i)),
            };
            [
                note(true, u64::from(i) * 10),
                note(false, u64::from(i) * 10 + 5),
            ]
        })
        .collect()
}

/// Lets the session see its barrier (the mark of its last new state).
fn settle(session: &mut Session, controller: &Mutex<Controller>, processor: &mut AudioProcessor) {
    poll(session, controller, processor);
}

#[test]
fn recording_at_the_note_cap_keeps_what_fits_stops_and_says_so() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(
        &mut session,
        (0..99_999u32)
            .map(|i| NewNote {
                length: 10,
                ..melodic(synth, (i % 64) * 240, (i % 128) as u8)
            })
            .collect(),
    );
    session.set_recording(true).unwrap();
    settle(&mut session, &controller, &mut processor);
    let before = session.view().notes_revision;

    // Two notes, room for one.
    let got = session.collect_recorded(&whole_notes(synth, 2), Instant::now());
    assert_eq!(got.revision, Some(before + 1));
    let stopped = got.stopped.expect("the person is told");
    assert_eq!(stopped.dropped, 1);
    assert_eq!(
        stopped.message,
        "Recording stopped: this pattern holds 100000 notes, the most a pattern can hold. 1 recorded note was not added."
    );
    assert_eq!(session.project().patterns[0].notes.len(), 100_000);
    assert!(!session.view().recording, "recording is switched off");
    render(&mut processor, 2);

    // Full: every note of a later batch is reported.
    let got = session.collect_recorded(&whole_notes(synth, 3), Instant::now());
    assert_eq!(got.revision, None);
    assert_eq!(got.stopped.map(|s| s.dropped), Some(3));
    assert_eq!(session.project().patterns[0].notes.len(), 100_000);
}

#[test]
fn recording_when_note_ids_run_out_keeps_what_fits_stops_and_says_so() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    let mut project = session.project().clone();
    // Two ids left.
    project.patterns[0].next_note_id = u32::MAX - 2;
    let loaded = Loaded {
        project,
        samples: Default::default(),
        warnings: Vec::new(),
    };
    let decoded = decode_samples(&loaded);
    session
        .open(Path::new("/p/ids.grooviq"), loaded, decoded)
        .unwrap();
    session.set_recording(true).unwrap();
    settle(&mut session, &controller, &mut processor);
    let got = session.collect_recorded(&whole_notes(synth, 3), Instant::now());
    assert!(got.revision.is_some());
    assert_eq!(got.stopped.as_ref().map(|s| s.dropped), Some(1), "{got:?}");
    assert_eq!(session.project().patterns[0].notes.len(), 2);
    assert!(!session.view().recording);
}

// --- Fix round 2: the automatic record-off is owed until the engine takes it. ---

/// Leave one slot for the admitted note prefix; its following record-off must retry.
fn leave_one_command_slot(controller: &Mutex<Controller>) {
    let (mut probe, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut capacity = 0;
    while probe.send(Command::SetTempo { bpm: 120.0 }).is_ok() {
        capacity += 1;
    }
    assert!(capacity > 1);
    for _ in 0..capacity - 1 {
        controller
            .lock()
            .unwrap()
            .send(Command::SetTempo { bpm: 120.0 })
            .unwrap();
    }
}

#[test]
fn overflow_disarm_is_retried_until_the_engine_takes_it() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(
        &mut session,
        (0..99_999u32)
            .map(|i| NewNote {
                length: 10,
                ..melodic(synth, (i % 64) * 240, (i % 128) as u8)
            })
            .collect(),
    );
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    settle(&mut session, &controller, &mut processor);
    // The prefix consumes the last slot, so only its automatic record-off must retry.
    leave_one_command_slot(&controller);
    let got = session.collect_recorded(&whole_notes(synth, 2), Instant::now());
    assert!(
        got.revision.is_some(),
        "the prefix is admitted before counting overflow"
    );
    assert_eq!(session.project().patterns[0].notes.len(), 100_000);
    assert_eq!(got.stopped.map(|s| s.dropped), Some(1));
    assert!(!session.view().recording);
    // Still full: the engine records on, and what it completes is refused and reported.
    let got = session.collect_recorded(&whole_notes(synth, 1), Instant::now());
    assert_eq!(got.revision, None);
    let stopped = got.stopped.expect("reported");
    assert_eq!((stopped.reason, stopped.dropped), ("engineBusy", 1));
    // The queue drains: an empty poll retries record-off without committing the prefix twice.
    render(&mut processor, 4);
    assert_eq!(session.collect_recorded(&[], Instant::now()).revision, None);
    assert_eq!(session.project().patterns[0].notes.len(), 100_000);
    poll(&mut session, &controller, &mut processor);
    // Room again, and the person plays while Record shows off: nothing is recorded.
    let edit = NotesEdit {
        pattern_id: 0,
        generation: session.generation(),
        base_revision: session.view().notes_revision,
        add: Vec::new(),
        update: Vec::new(),
        delete: vec![1],
    };
    session.edit_notes(edit, Instant::now()).unwrap();
    play_a_note(&session, &mut processor, synth);
    // The engine took the switch at the empty poll: it recorded nothing to refuse.
    assert_eq!(
        poll(&mut session, &controller, &mut processor),
        Recorded::default()
    );
    assert_eq!(session.project().patterns[0].notes.len(), 99_999);
}

#[test]
fn turning_record_on_again_cancels_an_owed_disarm() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(
        &mut session,
        (0..100_000u32)
            .map(|i| NewNote {
                length: 10,
                ..melodic(synth, (i % 64) * 240, (i % 128) as u8)
            })
            .collect(),
    );
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    settle(&mut session, &controller, &mut processor);
    while controller
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let got = session.collect_recorded(&whole_notes(synth, 1), Instant::now());
    assert_eq!(got.stopped.map(|s| s.dropped), Some(1));
    render(&mut processor, 4);
    // The person makes room and turns Record on again before the next poll.
    let edit = NotesEdit {
        pattern_id: 0,
        generation: session.generation(),
        base_revision: session.view().notes_revision,
        add: Vec::new(),
        update: Vec::new(),
        delete: vec![1],
    };
    session.edit_notes(edit, Instant::now()).unwrap();
    session.set_recording(true).unwrap();
    assert_eq!(
        session.collect_recorded(&[], Instant::now()),
        Recorded::default()
    );
    assert!(
        session.view().recording,
        "the old disarm does not switch it off"
    );
    play_a_note(&session, &mut processor, synth);
    assert!(
        poll(&mut session, &controller, &mut processor)
            .revision
            .is_some(),
        "the new take records"
    );
}

// --- Fix round 3: refusal lasts until the automatic record-off reaches the engine. ---

/// A session whose admitted prefix filled the command queue before automatic record-off.
/// The queue is drained again and the person makes room by deleting one committed note.
fn overflowed_with_an_owed_disarm() -> (Session, Arc<Mutex<Controller>>, AudioProcessor, u32) {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(
        &mut session,
        (0..99_999u32)
            .map(|i| NewNote {
                length: 10,
                ..melodic(synth, (i % 64) * 240, (i % 128) as u8)
            })
            .collect(),
    );
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    settle(&mut session, &controller, &mut processor);
    leave_one_command_slot(&controller);
    let got = session.collect_recorded(&whole_notes(synth, 2), Instant::now());
    assert!(
        got.revision.is_some(),
        "the prefix committed before the owed disarm"
    );
    assert_eq!(session.project().patterns[0].notes.len(), 100_000);
    assert_eq!(got.stopped.map(|s| s.dropped), Some(1));
    render(&mut processor, 4);
    let edit = NotesEdit {
        pattern_id: 0,
        generation: session.generation(),
        base_revision: session.view().notes_revision,
        add: Vec::new(),
        update: Vec::new(),
        delete: vec![1],
    };
    session.edit_notes(edit, Instant::now()).unwrap();
    (session, controller, processor, synth)
}

#[test]
fn retry_success_must_still_refuse_already_recorded_notes() {
    let (mut session, controller, mut processor, synth) = overflowed_with_an_owed_disarm();
    // Played while Record shows off, before the poll that sends the record-off.
    play_a_note(&session, &mut processor, synth);
    let got = poll(&mut session, &controller, &mut processor);
    assert_eq!(
        got.revision, None,
        "the admitted prefix must not be committed twice"
    );
    let stopped = got.stopped.expect("the late note is reported");
    assert_eq!((stopped.reason, stopped.dropped), ("engineBusy", 1));
    assert_eq!(session.project().patterns[0].notes.len(), 99_999);
    // The engine is off now: nothing more.
    assert_eq!(
        poll(&mut session, &controller, &mut processor),
        Recorded::default()
    );
    assert_eq!(session.project().patterns[0].notes.len(), 99_999);
}

#[test]
fn retry_success_must_refuse_held_notes_closed_by_disarm() {
    let (mut session, controller, mut processor, synth) = overflowed_with_an_owed_disarm();
    // A key goes down while Record shows off and is still held when the record-off lands.
    let press9 = session.live_note_on(synth, 70, 0.8).unwrap();
    render(&mut processor, 4);
    let first = poll(&mut session, &controller, &mut processor);
    // The record-off lands and ends the take: its End closes the held note.
    let second = poll(&mut session, &controller, &mut processor);
    let refused: u32 = [&first, &second]
        .iter()
        .filter_map(|g| g.stopped.as_ref())
        .map(|s| s.dropped)
        .sum();
    assert_eq!(refused, 1, "{first:?} {second:?}");
    assert_eq!(second.revision, None, "{second:?}");
    assert_eq!(session.project().patterns[0].notes.len(), 99_999);
    session.live_note_off(synth, 70, press9).unwrap();
    assert_eq!(
        poll(&mut session, &controller, &mut processor),
        Recorded::default()
    );
}

#[test]
fn a_refusal_running_survives_a_project_replacement() {
    let (mut session, controller, mut processor, _synth) = overflowed_with_an_owed_disarm();
    // The record-off is still owed when another project opens: the engine records on.
    open_fixture(&mut session, &controller, &mut processor);
    // The next collect sends the switch; what the engine recorded meanwhile is refused and
    // reported, not taken into the new project.
    let notes = session.project().patterns[0].notes.clone();
    let got = session.collect_recorded(&whole_notes(1, 1), Instant::now());
    assert_eq!(got.revision, None);
    // The replacement's barrier drops old events; its original refusal was settled first.
    assert!(got.stopped.is_none());
    assert_eq!(session.project().patterns[0].notes, notes);
}

#[test]
fn a_note_held_when_recording_stops_itself_is_refused_and_counted() {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(
        &mut session,
        (0..100_000u32)
            .map(|i| NewNote {
                length: 10,
                ..melodic(synth, (i % 64) * 240, (i % 128) as u8)
            })
            .collect(),
    );
    session.set_recording(true).unwrap();
    settle(&mut session, &controller, &mut processor);
    // One whole note does not fit, and another key is still down when recording stops.
    let mut events = whole_notes(synth, 1);
    let held = |on| RecordEvent::Note {
        channel: ChannelId(synth),
        key: 90,
        velocity: 0.8,
        on,
        tick: 50,
        position: 50,
        take: 0,
        id: grooviq_engine::NoteId(90),
    };
    events.push(held(true));
    let got = session.collect_recorded(&events, Instant::now());
    assert_eq!(got.stopped.map(|s| s.dropped), Some(2), "the held note too");
    // Its release later adds nothing and is not counted twice.
    assert_eq!(
        session.collect_recorded(&[held(false)], Instant::now()),
        Recorded::default()
    );
}

// --- Fix round 4: a replacement moves a running refusal to its own barrier. ---

/// A session at the note cap whose automatic record-off is owed (the engine's command queue
/// was full), with recording shown off. (The re-review's setup.)
fn owed_at_the_cap() -> (Session, Arc<Mutex<Controller>>, AudioProcessor, u32) {
    let (mut session, controller, mut processor) = rig();
    let synth = add_synth(&mut session, "Init");
    add_notes(
        &mut session,
        (0..100_000u32)
            .map(|i| NewNote {
                length: 10,
                ..melodic(synth, (i % 64) * 240, (i % 128) as u8)
            })
            .collect(),
    );
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    settle(&mut session, &controller, &mut processor);
    while controller
        .lock()
        .unwrap()
        .send(Command::SetTempo { bpm: 120.0 })
        .is_ok()
    {}
    let got = session.collect_recorded(&whole_notes(synth, 1), Instant::now());
    assert_eq!(got.stopped.unwrap().dropped, 1);
    assert!(!session.view().recording);
    (session, controller, processor, synth)
}

#[test]
fn record_on_then_open_imports_no_note_of_the_old_project() {
    let (mut session, controller, mut processor, synth) = owed_at_the_cap();
    render(&mut processor, 4);
    session.set_recording(true).unwrap();
    play_a_note(&session, &mut processor, synth);
    // Record-on's barrier and a whole note of the old project wait, uncollected.
    let notes = open_fixture(&mut session, &controller, &mut processor);
    let got = poll(&mut session, &controller, &mut processor);
    assert_eq!(got.revision, None, "an old project's note leaked: {got:?}");
    assert_eq!(session.project().patterns[0].notes, notes);
}

#[test]
fn a_replacement_never_leaves_the_refusal_waiting_for_a_superseded_barrier() {
    let (mut session, controller, mut processor, synth) = owed_at_the_cap();
    render(&mut processor, 4);
    // The recording queue fills without a poll, so a waiting barrier can be replaced by a
    // newer one before it goes out.
    for _ in 0..512 {
        play_a_note(&session, &mut processor, synth);
    }
    session.set_recording(true).unwrap();
    let notes = open_fixture(&mut session, &controller, &mut processor);
    poll(&mut session, &controller, &mut processor);
    poll(&mut session, &controller, &mut processor);
    session.set_recording(true).unwrap();
    controller.lock().unwrap().send(Command::Play).unwrap();
    play_a_note(&session, &mut processor, 1);
    let got = poll(&mut session, &controller, &mut processor);
    assert!(
        got.revision.is_some(),
        "the new project records, but the refusal never ended: {got:?}"
    );
    assert_eq!(session.project().patterns[0].notes.len(), notes.len() + 1);
}

#[test]
fn an_owed_record_off_blocks_replacement_until_its_original_barrier_settles() {
    let (mut session, controller, mut processor, _) = owed_at_the_cap();
    let generation = session.generation();
    assert!(session.new_project().is_err());
    assert_eq!(session.generation(), generation);
    let notes = open_fixture(&mut session, &controller, &mut processor);
    let mut old = whole_notes(1, 1);
    render(&mut processor, 2);
    controller.lock().unwrap().take_recorded(&mut old);
    let got = session.collect_recorded(&old, Instant::now());
    assert_eq!(got.revision, None);
    assert_eq!(session.project().patterns[0].notes, notes);
}

#[test]
fn directory_failure_keeps_new_bytes_and_reports_uncertainty() {
    struct Refuse;
    impl Disk for Refuse {
        fn sync_folder(&mut self, path: &Path) -> std::io::Result<()> {
            if path.extension().is_none_or(|ext| ext != "grooviq") {
                return grooviq_project::file::RealDisk.sync_folder(path);
            }
            Err(std::io::Error::other("folder open/sync refused"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.grooviq");
    let backups = dir.path().join("backups");
    let (mut session, _, _processor) = rig();
    session.save(Some(&path), &backups).unwrap();
    let old_bytes = std::fs::read(&path).unwrap();
    session
        .apply(Edit::SetTempo { bpm: 137.0 }, Instant::now())
        .unwrap();
    let job = session.begin_save(None).unwrap();
    let error = job.write_on(&backups, &mut Refuse).unwrap_err();
    assert!(error.to_string().contains("written"));
    assert_eq!(load(&path).unwrap().project.bpm, 137.0);
    assert!(error.directory_unconfirmed());
    assert!(session.end_save(&job, Err(error)).is_err());
    assert!(session.view().dirty);
    assert_eq!(session.path(), Some(path.as_path()));
    let folder = grooviq_project::backup::backup_folder(&path, &backups).unwrap();
    let copies: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(std::fs::read(&copies[0]).unwrap(), old_bytes);
    session.save(None, &backups).unwrap();
    assert!(!session.view().dirty);
}

#[test]
fn a_clean_save_as_warning_records_the_new_path_and_keeps_close_protection() {
    struct Refuse;
    impl Disk for Refuse {
        fn sync_folder(&mut self, path: &Path) -> std::io::Result<()> {
            if path.extension().is_none_or(|ext| ext != "grooviq") {
                return grooviq_project::file::RealDisk.sync_folder(path);
            }
            Err(std::io::Error::other("directory sync refused"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old.grooviq");
    let new = root.path().join("new.grooviq");
    let backups = root.path().join("backups");
    let (mut session, _, _processor) = rig();
    session.save(Some(&old), &backups).unwrap();
    assert!(
        session
            .save_on(Some(&new), &backups, &mut Refuse)
            .unwrap_err()
            .directory_unconfirmed()
    );
    assert_eq!(session.path(), Some(new.as_path()));
    assert!(session.needs_confirmation());
    let due = Instant::now() + AUTOSAVE_PERIOD;
    assert!(session.autosave_due(due));
    assert!(session.autosave_snapshot(due).is_some());
    assert!(load(&new).is_ok());
    session.save(None, &backups).unwrap();
    assert!(!session.needs_confirmation());
}

fn saturate_ui_queue(controller: &Arc<Mutex<Controller>>) {
    let mut c = controller.lock().unwrap();
    while c.send(Command::SetMetronome { on: false }).is_ok() {}
}

#[test]
fn ui_release_retries_without_input_after_queue_drains() {
    let (mut s, c, mut p) = rig();
    let ch = add_synth(&mut s, "Init");
    let press = s.live_note_on(ch, 60, 0.8).unwrap();
    assert!(render(&mut p, 30) > 0.001);
    saturate_ui_queue(&c);
    let _ = s.live_note_off(ch, 60, press);
    render(&mut p, 30);
    s.collect_recorded(&[], Instant::now());
    render(&mut p, 2000);
    assert_eq!(render(&mut p, 10), 0.0);
}

#[test]
fn ui_release_retry_preserves_a_new_same_key_press_in_every_voice_mode() {
    for sampler in [false, true] {
        for mode in 0..3 {
            let (mut s, c, mut p) = rig();
            let ch = if sampler {
                let ch = add(&mut s, "loop", Instant::now());
                s.param_live(ch, "loop", 1.0).unwrap();
                ch
            } else {
                add_synth(&mut s, "Init")
            };
            s.param_live(ch, "voice_mode", mode as f32).unwrap();
            let old = s.live_note_on(ch, 60, 0.8).unwrap();
            render(&mut p, 30);
            saturate_ui_queue(&c);
            let _ = s.live_note_off(ch, 60, old);
            render(&mut p, 30);
            let new = s.live_note_on(ch, 60, 0.8).unwrap();
            s.collect_recorded(&[], Instant::now());
            assert!(
                render(&mut p, 100) > 0.001,
                "sampler={sampler}, mode={mode}"
            );
            s.live_note_off(ch, 60, new).unwrap();
            render(&mut p, 2000);
            assert_eq!(render(&mut p, 10), 0.0, "sampler={sampler}, mode={mode}");
        }
    }
}

#[test]
fn ui_release_debt_survives_engine_rebuild_and_duplicate_release() {
    let (mut s, c, mut p) = rig();
    let ch = add_synth(&mut s, "Init");
    let press = s.live_note_on(ch, 60, 0.8).unwrap();
    saturate_ui_queue(&c);
    let _ = s.live_note_off(ch, 60, press);
    let _ = s.live_note_off(ch, 60, press);
    c.lock()
        .unwrap()
        .reconfigure(&mut p, EngineConfig { sample_rate: RATE });
    render(&mut p, 30);
    s.collect_recorded(&[], Instant::now());
    render(&mut p, 2000);
    assert_eq!(render(&mut p, 10), 0.0);
}

#[test]
fn ui_release_retry_records_delivery_and_keeps_take_barriers() {
    let (mut s, c, mut p) = rig();
    let ch = add_synth(&mut s, "Init");
    s.set_recording(true).unwrap();
    let mut events = start_take(&c, &mut p);
    let old = s.live_note_on(ch, 60, 0.8).unwrap();
    render(&mut p, 40);
    saturate_ui_queue(&c);
    let _ = s.live_note_off(ch, 60, old);
    render(&mut p, 40);
    c.lock().unwrap().take_recorded(&mut events);
    s.collect_recorded(&events, Instant::now());
    render(&mut p, 2);
    events.clear();
    c.lock().unwrap().take_recorded(&mut events);
    s.collect_recorded(&events, Instant::now());
    let notes = &s.project().patterns[0].notes;
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].length > 200,
        "release records delivery, not physical key-up"
    );
    // A release owed by the previous take must not close this take's same-key press.
    let old = s.live_note_on(ch, 60, 0.8).unwrap();
    render(&mut p, 2);
    saturate_ui_queue(&c);
    let _ = s.live_note_off(ch, 60, old);
    render(&mut p, 2);
    s.set_recording(false).unwrap();
    s.set_recording(true).unwrap();
    let new = s.live_note_on(ch, 60, 0.8).unwrap();
    s.collect_recorded(&[], Instant::now());
    render(&mut p, 40);
    s.live_note_off(ch, 60, new).unwrap();
    render(&mut p, 2);
    events.clear();
    c.lock().unwrap().take_recorded(&mut events);
    s.collect_recorded(&events, Instant::now());
    assert!(
        s.project().patterns[0]
            .notes
            .iter()
            .any(|n| n.length >= 115 && n.length <= 125)
    );
}

#[test]
fn ui_release_debt_does_not_cross_project_or_channel_replacement() {
    for open in [false, true] {
        let (mut s, c, mut p) = rig();
        let ch = add_synth(&mut s, "Init");
        let old = s.live_note_on(ch, 60, 0.8).unwrap();
        saturate_ui_queue(&c);
        s.live_note_off(ch, 60, old).unwrap();
        render(&mut p, 10);
        let ch = if open {
            let loaded = Loaded {
                project: s.project().clone(),
                samples: Default::default(),
                warnings: vec![],
            };
            s.open(
                Path::new("/generated/replacement.grooviq"),
                loaded,
                Default::default(),
            )
            .unwrap();
            ch
        } else {
            s.apply(Edit::RemoveChannel { channel: ch }, Instant::now())
                .unwrap();
            add_synth(&mut s, "Init")
        };
        let new = s.live_note_on(ch, 60, 0.8).unwrap();
        s.live_note_off(ch, 60, old).unwrap();
        s.collect_recorded(&[], Instant::now());
        assert!(render(&mut p, 100) > 0.001);
        s.live_note_off(ch, 60, new).unwrap();
        render(&mut p, 2000);
        assert_eq!(render(&mut p, 10), 0.0);
    }
}
#[test]
fn stale_uncertain_save_cannot_adopt_path_after_new_or_open() {
    struct Refuse;
    impl Disk for Refuse {
        fn sync_folder(&mut self, path: &Path) -> std::io::Result<()> {
            if path.extension().is_none_or(|ext| ext != "grooviq") {
                return grooviq_project::file::RealDisk.sync_folder(path);
            }
            Err(std::io::Error::other("directory sync refused"))
        }
    }
    for open in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old.grooviq");
        let (mut session, _, _processor) = rig();
        let job = session.begin_save(Some(&old)).unwrap();
        let result = job.write_on(&root.path().join("backups"), &mut Refuse);
        assert!(result.as_ref().unwrap_err().directory_unconfirmed());
        if open {
            session
                .open(
                    &root.path().join("other.grooviq"),
                    Loaded {
                        project: Project::new(),
                        samples: Default::default(),
                        warnings: vec![],
                    },
                    Default::default(),
                )
                .unwrap();
        } else {
            session.new_project().unwrap();
        }
        let before = session.view();
        assert!(session.end_save(&job, result).is_err());
        assert_eq!(session.view(), before);
        assert_ne!(session.path(), Some(old.as_path()));
        let old_bytes = std::fs::read(&old).unwrap();
        let retry = session.save(None, &root.path().join("backups"));
        if open {
            retry.unwrap();
        } else {
            assert!(matches!(retry, Err(SessionError::NoPath)));
        }
        assert_eq!(std::fs::read(&old).unwrap(), old_bytes);
    }
}

#[test]
fn pre_rename_save_failure_keeps_the_sessions_written_facts() {
    struct Refuse;
    impl Disk for Refuse {
        fn sync(&mut self, _: &std::fs::File) -> std::io::Result<()> {
            Err(std::io::Error::other("file sync refused"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let backups = root.path().join("backups");
    let old = root.path().join("old.grooviq");
    let new = root.path().join("new.grooviq");
    let (mut session, _, _processor) = rig();
    session.save(Some(&old), &backups).unwrap();
    let before = session.view();
    let error = session
        .save_on(Some(&new), &backups, &mut Refuse)
        .unwrap_err();
    assert!(!error.directory_unconfirmed());
    assert_eq!(session.view(), before);
    assert!(!new.exists());
    session.save(None, &backups).unwrap();
    assert_eq!(session.path(), Some(old.as_path()));
}

#[test]
fn failed_publication_at_capacity_preserves_every_backup_byte() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    for bpm in 111..125 {
        session
            .apply(Edit::SetTempo { bpm: bpm as f32 }, Instant::now())
            .unwrap();
        session.save(None, &backups).unwrap();
    }
    let folder = grooviq_project::backup_folder(&path, &backups).unwrap();
    let snapshot = || {
        names(&folder)
            .into_iter()
            .map(|n| {
                let bytes = std::fs::read(folder.join(&n)).unwrap();
                (n, bytes)
            })
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    assert_eq!(before.len(), 10);
    let target = std::fs::read(&path).unwrap();
    session
        .apply(Edit::SetTempo { bpm: 130.0 }, Instant::now())
        .unwrap();
    for _ in 0..3 {
        let job = session.begin_save(None).unwrap();
        let error = job
            .write_on(&backups, &mut Ops::failing("rename", &path))
            .unwrap_err();
        assert!(!error.directory_unconfirmed());
        assert!(session.end_save(&job, Err(error)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), target);
        assert_eq!(snapshot(), before);
        assert_eq!(load(&path).unwrap().project.bpm, 124.0);
        assert!(session.view().dirty);
    }
    session.save(None, &backups).unwrap();
    assert_eq!(snapshot().len(), 10);
    assert_eq!(load(&path).unwrap().project.bpm, 130.0);
    assert!(!session.view().dirty);
}

#[test]
fn uncertain_publication_at_capacity_retains_history_until_confirmed_retry() {
    struct Refuse;
    impl Disk for Refuse {
        fn sync_folder(&mut self, path: &Path) -> std::io::Result<()> {
            if path.extension().is_none_or(|ext| ext != "grooviq") {
                return grooviq_project::file::RealDisk.sync_folder(path);
            }
            Err(std::io::Error::other("injected directory uncertainty"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    for bpm in 111..125 {
        session
            .apply(Edit::SetTempo { bpm: bpm as f32 }, Instant::now())
            .unwrap();
        session.save(None, &backups).unwrap();
    }
    let folder = grooviq_project::backup_folder(&path, &backups).unwrap();
    let before: Vec<_> = names(&folder)
        .into_iter()
        .map(|n| {
            let bytes = std::fs::read(folder.join(&n)).unwrap();
            (n, bytes)
        })
        .collect();
    let old = std::fs::read(&path).unwrap();
    session
        .apply(Edit::SetTempo { bpm: 130.0 }, Instant::now())
        .unwrap();
    let error = session.save_on(None, &backups, &mut Refuse).unwrap_err();
    assert!(error.directory_unconfirmed());
    assert!(session.view().dirty);
    assert_eq!(load(&path).unwrap().project.bpm, 130.0);
    assert_eq!(names(&folder).len(), 11);
    for (name, bytes) in &before {
        assert_eq!(&std::fs::read(folder.join(name)).unwrap(), bytes);
    }
    assert!(
        names(&folder)
            .iter()
            .any(|n| std::fs::read(folder.join(n)).unwrap() == old)
    );
    session.save(None, &backups).unwrap();
    assert_eq!(names(&folder).len(), 10);
    assert!(!session.view().dirty);
    assert_eq!(load(&path).unwrap().project.bpm, 130.0);
}

#[test]
fn backup_preparation_failure_leaves_target_and_session_unsaved() {
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, _) = saved_twice(dir.path());
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"unowned").unwrap();
    let old = std::fs::read(&path).unwrap();
    session
        .apply(Edit::SetTempo { bpm: 130.0 }, Instant::now())
        .unwrap();
    let error = session.save(None, &blocked).unwrap_err();
    assert!(matches!(error, SessionError::Backup(_)));
    assert_eq!(std::fs::read(&blocked).unwrap(), b"unowned");
    assert_eq!(std::fs::read(&path).unwrap(), old);
    assert!(session.view().dirty);
    assert_eq!(load(&path).unwrap().project.bpm, 110.0);
}

#[test]
fn overlapping_save_cannot_borrow_a_backup_that_may_roll_back() {
    struct Interleave<'a> {
        other: &'a mut Session,
        backups: &'a Path,
    }
    impl Disk for Interleave<'_> {
        fn rename(&mut self, _: &Path, _: &Path) -> std::io::Result<()> {
            self.other.save(None, self.backups).unwrap();
            Err(std::io::Error::other("first publisher refused"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut first, path, backups) = saved_twice(dir.path());
    let (mut second, _, _) = rig();
    let loaded = load(&path).unwrap();
    let decoded = decode_samples(&loaded);
    second.open(&path, loaded, decoded).unwrap();
    first
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    second
        .apply(Edit::SetTempo { bpm: 130.0 }, Instant::now())
        .unwrap();
    let error = first
        .save_on(
            None,
            &backups,
            &mut Interleave {
                other: &mut second,
                backups: &backups,
            },
        )
        .unwrap_err();
    assert!(!error.directory_unconfirmed());
    assert!(first.view().dirty);
    assert!(!second.view().dirty);
    assert_eq!(load(&path).unwrap().project.bpm, 130.0);
    assert_eq!(backup_tempos(&path, &backups), [100.0, 110.0]);
}

#[test]
fn overlapping_failed_save_does_not_spend_retention_history() {
    struct Interleave<'a> {
        other: &'a mut Session,
        backups: &'a Path,
    }
    impl Disk for Interleave<'_> {
        fn rename(&mut self, _: &Path, _: &Path) -> std::io::Result<()> {
            self.other.save(None, self.backups).unwrap();
            Err(std::io::Error::other("first publisher refused"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut first, path, backups) = saved_twice(dir.path());
    for bpm in 111..125 {
        first
            .apply(Edit::SetTempo { bpm: bpm as f32 }, Instant::now())
            .unwrap();
        first.save(None, &backups).unwrap();
    }
    let before = backup_tempos(&path, &backups);
    assert_eq!(before.len(), 10);
    let (mut second, _, _) = rig();
    let loaded = load(&path).unwrap();
    let decoded = decode_samples(&loaded);
    second.open(&path, loaded, decoded).unwrap();
    first
        .apply(Edit::SetTempo { bpm: 125.0 }, Instant::now())
        .unwrap();
    second
        .apply(Edit::SetTempo { bpm: 130.0 }, Instant::now())
        .unwrap();
    let error = first
        .save_on(
            None,
            &backups,
            &mut Interleave {
                other: &mut second,
                backups: &backups,
            },
        )
        .unwrap_err();
    assert!(!error.directory_unconfirmed());
    assert!(first.view().dirty);
    assert!(!second.view().dirty);
    assert_eq!(load(&path).unwrap().project.bpm, 130.0);
    let mut expected = before[1..].to_vec();
    expected.push(124.0);
    assert_eq!(backup_tempos(&path, &backups), expected);
}

#[test]
fn repeated_saves_and_retained_duplicate_retry_keep_backup_bytes() {
    struct RefuseDirectory(bool);
    impl Disk for RefuseDirectory {
        fn sync_folder(&mut self, path: &Path) -> std::io::Result<()> {
            if self.0 {
                Err(std::io::Error::other(
                    "injected backup confirmation refusal",
                ))
            } else {
                grooviq_project::file::RealDisk.sync_folder(path)
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut session, path, backups) = saved_twice(dir.path());
    for _ in 0..3 {
        session.save(None, &backups).unwrap();
    }
    let folder = grooviq_project::backup_folder(&path, &backups).unwrap();
    let before: Vec<_> = names(&folder)
        .into_iter()
        .map(|name| {
            let bytes = std::fs::read(folder.join(&name)).unwrap();
            (name, bytes)
        })
        .collect();
    session
        .apply(Edit::SetTempo { bpm: 120.0 }, Instant::now())
        .unwrap();
    let mut disk = RefuseDirectory(true);
    for _ in 0..2 {
        let error = session.save_on(None, &backups, &mut disk).unwrap_err();
        assert!(matches!(error, SessionError::Backup(_)));
        assert!(!error.directory_unconfirmed());
        assert_eq!(load(&path).unwrap().project.bpm, 110.0);
        assert!(session.view().dirty);
    }
    disk.0 = false;
    session.save_on(None, &backups, &mut disk).unwrap();
    assert_eq!(load(&path).unwrap().project.bpm, 120.0);
    assert!(!session.view().dirty);
    for (name, bytes) in before {
        assert_eq!(std::fs::read(folder.join(name)).unwrap(), bytes);
    }
    assert_eq!(backup_tempos(&path, &backups), [100.0, 110.0]);
}

#[cfg(unix)]
#[test]
fn backup_barriers_fail_before_target_and_retry_duplicate() {
    use std::os::unix::fs::MetadataExt;
    struct Barrier {
        target: std::path::PathBuf,
        leaf: std::path::PathBuf,
        fail: String,
        seen: Vec<String>,
    }
    impl Disk for Barrier {
        fn sync(&mut self, file: &std::fs::File) -> std::io::Result<()> {
            let m = file.metadata()?;
            let identity = |p: &Path| {
                std::fs::metadata(p).is_ok_and(|x| x.dev() == m.dev() && x.ino() == m.ino())
            };
            let label = if m.is_file() {
                if std::fs::read_dir(&self.leaf)
                    .is_ok_and(|entries| entries.flatten().any(|e| identity(&e.path())))
                {
                    "backup-file".to_owned()
                } else {
                    "target-file".to_owned()
                }
            } else {
                let mut p = self.leaf.as_path();
                let mut found = None;
                loop {
                    if identity(p) {
                        found = Some(format!("dir:{}", p.display()));
                        break;
                    }
                    match p.parent() {
                        Some(parent) => p = parent,
                        None => break,
                    }
                }
                found.unwrap_or_else(|| "target-dir".into())
            };
            self.seen.push(label.clone());
            if label == self.fail {
                return Err(std::io::Error::other("backup barrier refused"));
            }
            file.sync_all()
        }
        fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
            assert_eq!(to, self.target);
            self.seen.push("rename".into());
            std::fs::rename(from, to)
        }
    }
    for failure in 0..3 {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("song.grooviq");
        let backups = root.path().join("new/app/backups");
        let (mut s, _, _processor) = rig();
        let original = root.path().join("original.grooviq");
        s.save(Some(&original), &backups).unwrap();
        std::fs::copy(&original, &target).unwrap();
        let old = std::fs::read(&target).unwrap();
        s.apply(Edit::SetTempo { bpm: 141.0 }, Instant::now())
            .unwrap();
        let leaf = grooviq_project::backup_folder(&target, &backups).unwrap();
        // Canonical spelling matters on macOS (/tmp versus /private/tmp).
        std::fs::create_dir_all(root.path()).unwrap();
        let leaf = std::fs::canonicalize(root.path())
            .unwrap()
            .join(leaf.strip_prefix(root.path()).unwrap());
        let fail = match failure {
            0 => "backup-file".into(),
            1 => format!("dir:{}", leaf.display()),
            _ => format!("dir:{}", leaf.parent().unwrap().display()),
        };
        let mut disk = Barrier {
            target: target.clone(),
            leaf: leaf.clone(),
            fail,
            seen: vec![],
        };
        let error = s.save_on(Some(&target), &backups, &mut disk).unwrap_err();
        assert!(matches!(error, SessionError::Backup(_)), "{error}");
        assert!(!error.directory_unconfirmed());
        assert_eq!(std::fs::read(&target).unwrap(), old);
        assert!(s.view().dirty);
        assert_eq!(s.path(), Some(original.as_path()));
        assert!(!disk.seen.iter().any(|x| x == "rename"));
        // Fail again on the same directory: a retained duplicate must not bypass it.
        if failure != 0 {
            disk.seen.clear();
            assert!(matches!(
                s.save_on(Some(&target), &backups, &mut disk),
                Err(SessionError::Backup(_))
            ));
            assert_eq!(names(&leaf).len(), 1);
            assert_eq!(std::fs::read(leaf.join(&names(&leaf)[0])).unwrap(), old);
        }
        disk.fail.clear();
        disk.seen.clear();
        s.save_on(Some(&target), &backups, &mut disk).unwrap();
        let rename = disk.seen.iter().position(|x| x == "rename").unwrap();
        let file = disk.seen.iter().position(|x| x == "backup-file").unwrap();
        let leaf_sync = disk
            .seen
            .iter()
            .position(|x| x == &format!("dir:{}", leaf.display()))
            .unwrap();
        let parent_sync = disk
            .seen
            .iter()
            .position(|x| x == &format!("dir:{}", leaf.parent().unwrap().display()))
            .unwrap();
        assert!(
            file < leaf_sync && leaf_sync < parent_sync && parent_sync < rename,
            "{:?}",
            disk.seen
        );
        assert_eq!(load(&target).unwrap().project.bpm, 141.0);
        assert!(!s.view().dirty);
    }
}
