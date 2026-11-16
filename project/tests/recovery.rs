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

/// The autosave files (not the metadata) in `root`.
fn autosave_files(root: &Path) -> Vec<String> {
    names(root)
        .into_iter()
        .filter(|n| n.starts_with("autosave-") && n.ends_with(".grooviq"))
        .collect()
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
    assert!(autosave_files(&root).is_empty(), "moved, not copied");
    session.restored(&recovery).unwrap();

    // This session ends cleanly: the next launch offers nothing.
    session
        .write_autosave(&project(112.0), &SampleFiles::new(), None)
        .unwrap();
    session.clean_exit().unwrap();
    assert!(!root.join("session.lock").exists());
    assert!(autosave_files(&root).is_empty());
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
    assert_eq!(autosave_files(dir.path()).len(), 1);
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

/// What the next launch offers, as `(tempo, original)` of each pending project.
fn offered(root: &Path) -> Vec<(f32, Option<PathBuf>)> {
    let (session, found) = RecoveryDir::start(root).unwrap();
    let pairs = found
        .iter()
        .map(|r| (load(&r.file).unwrap().project.bpm, r.original.clone()))
        .collect();
    drop(session);
    pairs
}

fn write(session: &RecoveryDir, bpm: f32, original: Option<&Path>) {
    session
        .write_autosave(&project(bpm), &SampleFiles::new(), original)
        .unwrap();
}

/// Writes an autosave of `bpm` for `original` and "crashes" (panics) between the autosave
/// file and its metadata: nothing after the hook runs, as in a killed process.
fn crash_between_the_two_writes(session: &RecoveryDir, bpm: f32, original: Option<&Path>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = session.write_autosave_with(&project(bpm), &SampleFiles::new(), original, || {
            panic!("the process dies here")
        });
    }));
    assert!(result.is_err(), "the hook did not run");
}

#[test]
fn a_crash_between_the_autosave_and_its_metadata_never_mixes_the_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (
        PathBuf::from("/music/a.grooviq"),
        PathBuf::from("/music/b.grooviq"),
    );
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    write(&session, 100.0, Some(&a));
    // Another project's autosave dies half way.
    crash_between_the_two_writes(&session, 140.0, Some(&b));
    drop(session);
    // The old pair, whole: project A with A's path. Never 140 BPM labelled A, or 100 labelled B.
    assert_eq!(offered(dir.path()), [(100.0, Some(a))]);
}

#[test]
fn a_failed_metadata_write_leaves_the_old_pair_and_no_stray_file() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (
        PathBuf::from("/music/a.grooviq"),
        PathBuf::from("/music/b.grooviq"),
    );
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    write(&session, 100.0, Some(&a));
    let before = names(dir.path());
    let err = session
        .write_autosave_with(&project(140.0), &SampleFiles::new(), Some(&b), || {
            Err(std::io::Error::other("injected"))
        })
        .unwrap_err();
    assert_eq!(err.to_string(), "injected");
    assert_eq!(names(dir.path()), before, "the failed write left files");
    drop(session);
    assert_eq!(offered(dir.path()), [(100.0, Some(a))]);
}

#[test]
fn a_finished_write_replaces_the_pair_as_a_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (
        PathBuf::from("/music/a.grooviq"),
        PathBuf::from("/music/b.grooviq"),
    );
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    write(&session, 100.0, Some(&a));
    write(&session, 140.0, Some(&b));
    write(&session, 150.0, None);
    drop(session);
    assert_eq!(offered(dir.path()), [(150.0, None)]);
}

#[test]
fn a_first_autosave_that_dies_before_its_metadata_is_offered_untitled() {
    let dir = tempfile::tempdir().unwrap();
    let a = PathBuf::from("/music/a.grooviq");
    let (session, _) = RecoveryDir::start(dir.path()).unwrap();
    crash_between_the_two_writes(&session, 100.0, Some(&a));
    drop(session);
    // No pair was ever complete: the work is kept, without a path it cannot vouch for.
    assert_eq!(offered(dir.path()), [(100.0, None)]);
}

/// A folder as a crashed session of the first release left it: the lock, `autosave.grooviq`
/// and an `autosave.json` that has no `file`.
fn earlier_version_crash(root: &Path, bpm: f32, original: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("session.lock"), b"1\n").unwrap();
    grooviq_project::save(
        &project(bpm),
        &SampleFiles::new(),
        &root.join("autosave.grooviq"),
    )
    .unwrap();
    let meta = format!(
        r#"{{"original":{},"savedAt":1700000000}}"#,
        serde_json::to_string(original).unwrap()
    );
    std::fs::write(root.join("autosave.json"), meta).unwrap();
}

#[test]
fn an_autosave_an_earlier_version_left_is_still_offered_with_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let a = PathBuf::from("/music/a.grooviq");
    earlier_version_crash(dir.path(), 123.0, &a);
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].original.as_deref(), Some(a.as_path()));
    assert_eq!(found[0].saved_at, 1_700_000_000);
    assert_eq!(load(&found[0].file).unwrap().project.bpm, 123.0);
    assert!(!dir.path().join("autosave.grooviq").exists());
    drop(session);
}

#[test]
fn the_files_of_writes_that_died_are_removed_at_launch_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    crash_with(dir.path(), 100.0);
    for dead in [
        "autosave.json.tmp",
        "autosave-7.grooviq.tmp",
        "autosave.grooviq.tmp",
        "pending-3.json.tmp",
    ] {
        std::fs::write(dir.path().join(dead), b"half").unwrap();
    }
    // Not ours: left as they are.
    for other in ["notes.tmp", "autosave-x.grooviq.tmp", "other.json.tmp"] {
        std::fs::write(dir.path().join(other), b"mine").unwrap();
    }
    let (session, found) = RecoveryDir::start(dir.path()).unwrap();
    assert_eq!(found.len(), 1);
    let left: Vec<String> = names(dir.path())
        .into_iter()
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert_eq!(
        left,
        ["autosave-x.grooviq.tmp", "notes.tmp", "other.json.tmp"]
    );
    drop(session);
}

#[test]
fn a_metadata_file_cannot_name_a_file_outside_the_autosaves() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("recovery");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("session.lock"), b"1\n").unwrap();
    let victim = dir.path().join("victim.grooviq");
    std::fs::write(&victim, b"the person's project").unwrap();
    for named in ["../victim.grooviq", "session.lock"] {
        let meta = format!(r#"{{"original":null,"savedAt":5,"file":"{named}"}}"#);
        std::fs::write(root.join("autosave.json"), meta).unwrap();
        let (session, found) = RecoveryDir::start(&root).unwrap();
        assert!(found.is_empty(), "{named}");
        drop(session);
    }
    assert_eq!(std::fs::read(&victim).unwrap(), b"the person's project");
}
