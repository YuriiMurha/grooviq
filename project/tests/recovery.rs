//! Crash recovery: a lock file marks a running session, the autosave sits beside it, and the
//! next launch offers it back if the lock was left behind.

use grooviq_project::recovery::RecoveryDir;
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
    assert!(found.is_none());
    session
        .write_autosave(&project(111.0), &SampleFiles::new(), Some(&original))
        .unwrap();
    // The app dies: no clean exit.
    drop(session);

    let (session, found) = RecoveryDir::start(&root).unwrap();
    let recovery = found.expect("the autosave is offered back");
    assert_eq!(recovery.original.as_deref(), Some(original.as_path()));
    assert!(recovery.saved_at > 1_600_000_000, "{}", recovery.saved_at);
    let name = recovery
        .file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("recovered-") && name.ends_with(".grooviq"),
        "{name}"
    );
    assert_eq!(load(&recovery.file).unwrap().project.bpm, 111.0);
    assert!(!root.join("autosave.grooviq").exists(), "moved, not copied");

    // This session ends cleanly: the next launch offers nothing.
    session
        .write_autosave(&project(112.0), &SampleFiles::new(), None)
        .unwrap();
    session.clean_exit().unwrap();
    assert!(!root.join("session.lock").exists());
    assert!(!root.join("autosave.grooviq").exists());
    let (session, found) = RecoveryDir::start(&root).unwrap();
    assert!(found.is_none());
    session.clean_exit().unwrap();
}

#[test]
fn a_crash_before_any_autosave_offers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    drop(session);
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert!(found.is_none());
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
    let recovery = found.unwrap();
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
    let recovery = found.unwrap();
    let file = recovery.file.clone();
    assert!(file.is_file());
    session.discard(recovery).unwrap();
    assert!(!file.exists());
    assert_eq!(names(dir.path()), vec!["session.lock".to_string()]);
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
    assert!(found.is_none());
}

#[test]
fn old_recovered_files_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..13 {
        let (session, _) = RecoveryDir::start(dir.path()).unwrap();
        session
            .write_autosave(&project(100.0), &SampleFiles::new(), None)
            .unwrap();
        drop(session);
    }
    let (_session, found) = RecoveryDir::start(dir.path()).unwrap();
    let newest = found.unwrap().file;
    let recovered: Vec<String> = names(dir.path())
        .into_iter()
        .filter(|n| n.starts_with("recovered-"))
        .collect();
    assert_eq!(recovered.len(), grooviq_project::session::MAX_BACKUPS);
    assert!(newest.is_file(), "the one just offered is kept");
}
