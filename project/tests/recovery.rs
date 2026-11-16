//! Crash recovery: a lock file marks a running session, the autosave sits beside it, and the
//! next launch offers it back if the lock was left behind.

use grooviq_project::recovery::{Recovery, RecoveryDir};
use grooviq_project::{Project, SampleFile, SampleFiles, load};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn project(bpm: f32) -> Project {
    let mut p = Project::new();
    p.bpm = bpm;
    p
}

fn names(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn recovery_is_offered_after_an_unclean_exit_and_not_after_a_clean_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("recovery");
    let original = PathBuf::from("/music/beat.grooviq");

    // First launch: nothing to recover.
    let (session, found) = RecoveryDir::start(&root).unwrap();
    assert!(found.is_empty());
    session
        .write_autosave(&project(111.0), &SampleFiles::new(), Some(&original))
        .unwrap();
    // The app dies: no clean exit.
    drop(session);

    let (session, found) = RecoveryDir::start(&root).unwrap();
    assert_eq!(found.len(), 1, "the autosave is offered back");
    let recovery = found[0].clone();
    assert_eq!(recovery.original.as_deref(), Some(original.as_path()));
    assert!(recovery.saved_at > 1_600_000_000, "{}", recovery.saved_at);
    let name = recovery
        .file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("pending-") && name.ends_with(".grooviq"),
        "{name}"
    );
    assert_eq!(load(&recovery.file).unwrap().project.bpm, 111.0);
    assert!(!root.join("autosave.grooviq").exists(), "moved, not copied");
    session.restored(&recovery).unwrap();

    // This session ends cleanly: the next launch offers nothing.
    session
        .write_autosave(&project(112.0), &SampleFiles::new(), None)
        .unwrap();
    session.clean_exit().unwrap();
    assert!(!root.join("session.lock").exists());
    assert!(!root.join("autosave.grooviq").exists());
    let (session, found) = RecoveryDir::start(&root).unwrap();
    assert!(found.is_empty());
    session.clean_exit().unwrap();
}

#[test]
fn a_crash_before_any_autosave_offers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    drop(session);
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert!(found.is_empty());
}

#[test]
fn an_untitled_project_recovers_without_a_path_and_with_its_samples() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    let mut p = project(90.0);
    p.channels.push(grooviq_project::Channel {
        id: 1,
        name: "Kick".into(),
        color: 0,
        sample: Some(grooviq_project::SampleRef {
            path: "/s/kick.wav".into(),
            file: None,
        }),
        sampler: Default::default(),
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
    });
    p.next_id = 2;
    let mut samples = SampleFiles::new();
    samples.insert(
        1,
        SampleFile {
            name: "kick.wav".into(),
            bytes: Arc::new(b"RIFF-kick".to_vec()),
        },
    );
    session.write_autosave(&p, &samples, None).unwrap();
    drop(session);
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    let recovery = found[0].clone();
    assert_eq!(recovery.original, None);
    let loaded = load(&recovery.file).unwrap();
    assert_eq!(&**loaded.samples[&1].bytes, b"RIFF-kick");
}

#[test]
fn discarding_removes_the_recovered_file() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    session
        .write_autosave(&project(100.0), &SampleFiles::new(), None)
        .unwrap();
    drop(session);
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    let recovery = found[0].clone();
    let file = recovery.file.clone();
    assert!(file.is_file());
    session.discard(recovery).unwrap();
    assert!(!file.exists());
    assert_eq!(names(dir.path()), vec!["session.lock".to_string()]);
    // Gone for good: the next launch offers nothing.
    session.clean_exit().unwrap();
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert!(found.is_empty());
}

#[test]
fn discard_and_restore_only_touch_pending_files_of_their_own_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("recovery");
    let (session, _) = RecoveryDir::start(&root).unwrap();
    // A project somewhere else, and a file in the folder that is not a pending project.
    let outside = dir.path().join("beat.grooviq");
    std::fs::write(&outside, b"mine").unwrap();
    let lock = root.join("session.lock");
    for file in [
        outside.clone(),
        lock.clone(),
        root.join("..").join("beat.grooviq"),
    ] {
        let foreign = Recovery {
            file: file.clone(),
            original: None,
            saved_at: 0,
        };
        let err = session.discard(foreign.clone()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{file:?}");
        let err = session.restored(&foreign).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{file:?}");
    }
    assert_eq!(std::fs::read(&outside).unwrap(), b"mine");
    assert!(lock.is_file());
}

#[test]
fn a_second_running_instance_cannot_take_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (first, _) = RecoveryDir::start(dir.path()).unwrap();
    first
        .write_autosave(&project(100.0), &SampleFiles::new(), None)
        .unwrap();
    // The first instance is alive: its autosave is not a crash to recover.
    assert!(RecoveryDir::start(dir.path()).is_err());
    assert!(dir.path().join("autosave.grooviq").is_file());
    first.clean_exit().unwrap();
}

#[test]
fn clearing_the_autosave_after_a_save_leaves_nothing_to_recover() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    session
        .write_autosave(&project(100.0), &SampleFiles::new(), None)
        .unwrap();
    session.clear_autosave().unwrap();
    drop(session);
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert!(found.is_empty());
}

/// A crash that leaves an autosave of `bpm` behind.
fn crash_with(root: &Path, bpm: f32) {
    let (session, _) = RecoveryDir::start(root).unwrap();
    session
        .write_autosave(&project(bpm), &SampleFiles::new(), None)
        .unwrap();
    drop(session);
}

fn tempos(found: &[Recovery]) -> Vec<f32> {
    found
        .iter()
        .map(|r| load(&r.file).unwrap().project.bpm)
        .collect()
}

#[test]
fn old_restored_files_are_pruned_but_pending_ones_never_are() {
    let dir = tempfile::tempdir().unwrap();
    // One crash nobody decides on, then 13 that are restored.
    crash_with(dir.path(), 77.0);
    for _ in 0..13 {
        crash_with(dir.path(), 100.0);
        let (session, found) = RecoveryDir::start(dir.path()).unwrap();
        session.restored(&found[0]).unwrap();
    }
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    let all = names(dir.path());
    let restored = all.iter().filter(|n| n.starts_with("recovered-")).count();
    assert_eq!(restored, grooviq_project::session::MAX_BACKUPS);
    assert_eq!(tempos(&found), [77.0], "the undecided one is still offered");
}

#[test]
fn an_undecided_recovery_is_offered_again_after_a_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let original = PathBuf::from("/music/beat.grooviq");
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    session
        .write_autosave(&project(101.0), &SampleFiles::new(), Some(&original))
        .unwrap();
    drop(session);
    // Offered; the person quits without choosing.
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(found.len(), 1);
    let first = found[0].clone();
    session.clean_exit().unwrap();
    // Offered again, the same file with its metadata, until restored.
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(found, std::slice::from_ref(&first));
    session.restored(&found[0]).unwrap();
    session.clean_exit().unwrap();
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert!(found.is_empty(), "restored: no longer offered");
    assert!(!first.file.exists());
}

#[test]
fn an_undecided_recovery_survives_another_crash_and_newer_ones_come_first() {
    let dir = tempfile::tempdir().unwrap();
    crash_with(dir.path(), 101.0);
    // Offered, then this launch crashes before an autosave: still offered.
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(tempos(&found), [101.0]);
    drop(session);
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(tempos(&found), [101.0]);
    // This one crashes after an autosave: both are offered, the newest first.
    session
        .write_autosave(&project(102.0), &SampleFiles::new(), None)
        .unwrap();
    drop(session);
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(tempos(&found), [102.0, 101.0]);
    // Discarding the newest leaves the older one offered.
    session.discard(found[0].clone()).unwrap();
    session.clean_exit().unwrap();
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(tempos(&found), [101.0]);
}
