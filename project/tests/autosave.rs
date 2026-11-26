//! The autosave clock between the session and the recovery folder.

use grooviq_engine::{EngineConfig, SampleBuf, engine};
use grooviq_project::autosave::{
    AutosaveTick, autosave_tick, autosave_tick_on, autosave_tick_with, clear_retired_autosaves,
};
use grooviq_project::file::{Disk, load_for_open};
use grooviq_project::recovery::RecoveryDir;
use grooviq_project::session::{AUTOSAVE_PERIOD, Edit, Session, decode_samples};
use grooviq_project::{SampleFile, load};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn session() -> Mutex<Session> {
    let (controller, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    Mutex::new(Session::new(Arc::new(Mutex::new(controller))))
}

fn edit(session: &Mutex<Session>, bpm: f32, at: Instant) {
    session
        .lock()
        .unwrap()
        .apply(Edit::SetTempo { bpm }, at)
        .unwrap();
}

/// What the next launch offers: the tempo of each pending project, newest first.
fn next_launch(root: &Path) -> Vec<f32> {
    let (dir, found) = RecoveryDir::start(root).unwrap();
    let tempos = found
        .iter()
        .map(|r| load(&r.file).unwrap().project.bpm)
        .collect();
    drop(dir);
    tempos
}

#[test]
fn a_due_autosave_is_written_and_recorded() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let t0 = Instant::now();
    assert!(matches!(
        autosave_tick(&session, &recovery, t0),
        AutosaveTick::Idle
    ));
    edit(&session, 120.0, t0);
    assert!(matches!(
        autosave_tick(&session, &recovery, t0),
        AutosaveTick::Idle
    ));
    let tick = autosave_tick(&session, &recovery, t0 + AUTOSAVE_PERIOD);
    assert!(matches!(tick, AutosaveTick::Written(_)), "{tick:?}");
    assert!(session.lock().unwrap().view().autosaved_at.is_some());
    drop(recovery);
    assert_eq!(next_launch(root.path()), [120.0]);
}

fn save_then_clear_note(s: &mut Session, saved: &Path, now: Instant) {
    let channel = s.add_synth_channel("Init", now).unwrap().channels[0].id;
    s.apply(Edit::ToggleStep { channel, step: 0 }, now).unwrap();
    s.save(Some(saved), saved.parent().unwrap()).unwrap();
    s.apply(Edit::ClearStep { channel, step: 0 }, now).unwrap();
    assert!(s.needs_confirmation());
}

fn replay_note(s: &mut Session, undo: bool, now: Instant) {
    let stamp = s.pattern_stamp();
    assert!(s.history_stamped(stamp, undo, now).unwrap().is_some());
}

fn assert_saved_state(s: &Session, saved: &Path) {
    assert!(!s.needs_confirmation());
    assert_eq!(
        serde_json::to_vec(s.project()).unwrap(),
        serde_json::to_vec(&load(saved).unwrap().project).unwrap()
    );
}

#[test]
fn a_saved_note_edit_is_offered_after_crash_only_if_it_remains_dirty() {
    for replay in [None, Some(true), Some(false)] {
        let root = tempfile::tempdir().unwrap();
        let recovery_path = root.path().join("recovery");
        let (dir, _) = RecoveryDir::start(&recovery_path).unwrap();
        let recovery = Mutex::new(Some(dir));
        let session = session();
        let now = Instant::now();
        let saved_path = root.path().join("song.grooviq");
        let mut s = session.lock().unwrap();
        save_then_clear_note(&mut s, &saved_path, now);
        if replay == Some(false) {
            s.save(Some(&saved_path), root.path()).unwrap();
            replay_note(&mut s, true, now);
            assert!(s.needs_confirmation());
        }
        drop(s);
        assert!(matches!(
            autosave_tick(&session, &recovery, now + AUTOSAVE_PERIOD),
            AutosaveTick::Written(_)
        ));
        if let Some(undo) = replay {
            {
                let mut s = session.lock().unwrap();
                replay_note(&mut s, undo, now + AUTOSAVE_PERIOD);
                assert_saved_state(&s, &saved_path);
            }
            clear_retired_autosaves(&session, &recovery).unwrap();
        }
        // Force-quit: leave the lock file, skip clean_exit and run no later autosave tick.
        drop(recovery);
        let (_restarted, found) = RecoveryDir::start(&recovery_path).unwrap();
        assert_eq!(
            found.len(),
            usize::from(replay.is_none()),
            "replay={replay:?}, found={found:?}"
        );
        if replay.is_none() {
            assert_eq!(found[0].original.as_deref(), Some(saved_path.as_path()));
            assert!(
                load(&found[0].file).unwrap().project.patterns[0]
                    .notes
                    .is_empty()
            );
            assert_eq!(
                load(&saved_path).unwrap().project.patterns[0].notes.len(),
                1
            );
        }
    }
}

#[test]
fn direct_sampler_trim_undo_retires_recovery_and_leaves_pending_earlier_files() {
    let root = tempfile::tempdir().unwrap();
    let recovery_path = root.path().join("recovery");
    let (dir, _) = RecoveryDir::start(&recovery_path).unwrap();
    let previous_recovery = Mutex::new(Some(dir));
    let previous = session();
    let now = Instant::now();
    edit(&previous, 123.0, now);
    let due = now + AUTOSAVE_PERIOD;
    assert!(matches!(
        autosave_tick(&previous, &previous_recovery, due),
        AutosaveTick::Written(_)
    ));
    drop(previous_recovery);
    let (dir, earlier) = RecoveryDir::start(&recovery_path).unwrap();
    assert_eq!(earlier.len(), 1);
    let earlier_bytes = std::fs::read(&earlier[0].file).unwrap();

    let recovery = Mutex::new(Some(dir));
    let session = session();
    let saved = root.path().join("song.grooviq");
    let mut s = session.lock().unwrap();
    // Four frames of mono 48 kHz, 16-bit PCM.
    let bytes = b"RIFF\x2c\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0\x77\x01\0\x02\0\x10\0data\x08\0\0\0\0\0\x0a\0\xf6\xff\0\0";
    let channel = s
        .add_channel(
            Path::new("unit.wav"),
            SampleFile {
                name: "unit.wav".into(),
                bytes: Arc::new(bytes.to_vec()),
            },
            Arc::new(SampleBuf {
                rate: 48_000.0,
                l: vec![0.0; 4],
                r: vec![0.0; 4],
            }),
            now,
        )
        .unwrap()
        .channels[0]
        .id;
    s.apply(
        Edit::SetSamplerParam {
            channel,
            param: "start".into(),
            value: 0.25,
        },
        now,
    )
    .unwrap();
    s.save(Some(&saved), root.path()).unwrap();
    let loaded = load_for_open(&saved, &s.media_budget()).unwrap();
    let decoded = decode_samples(loaded.loaded());
    s.open(&saved, loaded, decoded).unwrap();
    assert_saved_state(&s, &saved);
    let trim = s.sampler_trim_view(channel).unwrap();
    let reset = s.sampler_trim_reset(channel, &trim.stamp, now).unwrap();
    assert!(reset.changed);
    assert!(s.needs_confirmation());
    drop(s);
    assert!(matches!(
        autosave_tick(&session, &recovery, due),
        AutosaveTick::Written(_)
    ));
    {
        let mut s = session.lock().unwrap();
        let trim = &reset.state;
        assert!(
            s.sampler_trim_undo(channel, &trim.stamp, trim.undo.as_deref().unwrap(), due)
                .unwrap()
                .changed
        );
        assert_saved_state(&s, &saved);
    }
    clear_retired_autosaves(&session, &recovery).unwrap();
    drop(recovery);
    let (_dir, found) = RecoveryDir::start(&recovery_path).unwrap();
    assert_eq!(found, earlier);
    assert_eq!(std::fs::read(&found[0].file).unwrap(), earlier_bytes);
}

struct DuringWrite<F>(Option<F>);

impl<F: FnOnce()> Disk for DuringWrite<F> {
    fn written(&mut self, _: &mut std::fs::File) -> io::Result<()> {
        self.0.take().unwrap()();
        Ok(())
    }
}

#[test]
fn clean_history_during_an_autosave_rejects_its_metadata_commit_without_locking_out_edits() {
    let root = tempfile::tempdir().unwrap();
    let recovery_path = root.path().join("recovery");
    let (dir, _) = RecoveryDir::start(&recovery_path).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let now = Instant::now();
    let saved = root.path().join("song.grooviq");
    save_then_clear_note(&mut session.lock().unwrap(), &saved, now);
    let due = now + AUTOSAVE_PERIOD;
    let tick = autosave_tick_on(
        &session,
        &recovery,
        due,
        || assert!(!recovery_path.join("autosave.json").exists()),
        &mut DuringWrite(Some(|| {
            let mut s = session
                .try_lock()
                .expect("autosave I/O held the Session lock");
            replay_note(&mut s, true, due);
            assert_saved_state(&s, &saved);
        })),
    );
    assert!(matches!(tick, AutosaveTick::Replaced), "{tick:?}");
    assert_eq!(session.lock().unwrap().view().autosaved_at, None);
    drop(recovery);
    assert!(next_launch(&recovery_path).is_empty());
}

struct UnconfirmedSnapshot;

impl Disk for UnconfirmedSnapshot {
    fn sync_folder(&mut self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("snapshot directory sync unconfirmed"))
    }
}

#[test]
fn late_clean_replay_cleanup_preserves_a_newer_uncertain_dirty_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let recovery_path = root.path().join("recovery");
    let (dir, _) = RecoveryDir::start(&recovery_path).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let now = Instant::now();
    let saved = root.path().join("song.grooviq");
    save_then_clear_note(&mut session.lock().unwrap(), &saved, now);
    let due = now + AUTOSAVE_PERIOD;
    let old = autosave_tick_with(&session, &recovery, due, || {
        {
            let mut s = session.lock().unwrap();
            replay_note(&mut s, true, due);
            assert_saved_state(&s, &saved);
        }
        edit(&session, 145.0, due);
        let newer = autosave_tick_on(
            &session,
            &recovery,
            due + AUTOSAVE_PERIOD,
            || {},
            &mut UnconfirmedSnapshot,
        );
        let AutosaveTick::Failed(error) = newer else {
            panic!("{newer:?}")
        };
        assert!(grooviq_project::file::directory_unconfirmed(&error));
    });
    assert!(matches!(old, AutosaveTick::Replaced), "{old:?}");
    assert_eq!(session.lock().unwrap().view().autosaved_at, None);
    drop(recovery);
    assert_eq!(next_launch(&recovery_path), [145.0]);
}

#[test]
fn the_autosave_clock_retries_failed_cleanup_even_when_the_project_is_clean() {
    let root = tempfile::tempdir().unwrap();
    let recovery_path = root.path().join("recovery");
    let (dir, _) = RecoveryDir::start(&recovery_path).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let now = Instant::now();
    let saved = root.path().join("song.grooviq");
    save_then_clear_note(&mut session.lock().unwrap(), &saved, now);
    let due = now + AUTOSAVE_PERIOD;
    assert!(matches!(
        autosave_tick(&session, &recovery, due),
        AutosaveTick::Written(_)
    ));
    let metadata = recovery_path.join("autosave.json");
    std::fs::remove_file(&metadata).unwrap();
    std::fs::create_dir(&metadata).unwrap();
    replay_note(&mut session.lock().unwrap(), true, due);
    assert!(clear_retired_autosaves(&session, &recovery).is_err());
    assert!(session.lock().unwrap().autosave_cleanup_pending());
    std::fs::remove_dir(&metadata).unwrap();
    assert!(matches!(
        autosave_tick(&session, &recovery, due),
        AutosaveTick::Idle
    ));
    assert!(!session.lock().unwrap().autosave_cleanup_pending());
    drop(recovery);
    assert!(next_launch(&recovery_path).is_empty());
}

#[test]
fn an_autosave_written_while_a_new_project_starts_is_removed_again() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let t0 = Instant::now();
    edit(&session, 120.0, t0);

    // New is chosen while the old project's autosave is being written.
    let tick = autosave_tick_with(&session, &recovery, t0 + AUTOSAVE_PERIOD, || {
        session.lock().unwrap().new_project().unwrap();
    });
    assert!(matches!(tick, AutosaveTick::Replaced), "{tick:?}");
    assert_eq!(session.lock().unwrap().view().autosaved_at, None);
    // The app then crashes: the old project must not be offered back.
    drop(recovery);
    assert_eq!(next_launch(root.path()), Vec::<f32>::new());
}

#[test]
fn a_failed_autosave_is_reported_and_retried() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    // The folder vanishes under the running session.
    std::fs::remove_dir_all(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let session = session();
    let t0 = Instant::now();
    edit(&session, 120.0, t0);
    let tick = autosave_tick(&session, &recovery, t0 + AUTOSAVE_PERIOD);
    assert!(matches!(tick, AutosaveTick::Failed(_)), "{tick:?}");
    assert!(
        session
            .lock()
            .unwrap()
            .autosave_due(t0 + AUTOSAVE_PERIOD * 3)
    );
}

#[test]
fn a_taken_recovery_folder_stops_the_clock() {
    let session = session();
    let recovery = Mutex::new(None);
    let t0 = Instant::now();
    edit(&session, 120.0, t0);
    assert!(matches!(
        autosave_tick(&session, &recovery, t0 + AUTOSAVE_PERIOD),
        AutosaveTick::Stopped
    ));
}
