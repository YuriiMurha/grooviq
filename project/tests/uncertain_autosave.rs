use grooviq_engine::{EngineConfig, engine};
use grooviq_project::autosave::AutosaveTick;
use grooviq_project::load;
use grooviq_project::recovery::RecoveryDir;
use grooviq_project::session::{AUTOSAVE_PERIOD, Edit, Session};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn session() -> Mutex<Session> {
    let (controller, _p) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    Mutex::new(Session::new(Arc::new(Mutex::new(controller))))
}
fn edit(s: &Mutex<Session>, bpm: f32, at: Instant) {
    s.lock().unwrap().apply(Edit::SetTempo { bpm }, at).unwrap();
}
fn next_launch(root: &Path) -> Vec<(Option<std::path::PathBuf>, f32)> {
    let (dir, found) = RecoveryDir::start(root).unwrap();
    let v = found
        .iter()
        .map(|r| (r.original.clone(), load(&r.file).unwrap().project.bpm))
        .collect();
    drop(dir);
    v
}

struct Refuse {
    metadata: bool,
}
impl grooviq_project::file::Disk for Refuse {
    fn sync_folder(&mut self, path: &Path) -> std::io::Result<()> {
        if path.ends_with("autosave.json") == self.metadata {
            Err(std::io::Error::other("refused"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn an_uncertain_tick_keeps_the_open_projects_recovery() {
    for metadata in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (dir, _) = RecoveryDir::start(root.path()).unwrap();
        let recovery = Mutex::new(Some(dir));
        let s = session();
        let t0 = Instant::now();
        edit(&s, 121.0, t0);
        let tick = grooviq_project::autosave::autosave_tick_on(
            &s,
            &recovery,
            t0 + AUTOSAVE_PERIOD,
            || {},
            &mut Refuse { metadata },
        );
        assert!(
            matches!(&tick, AutosaveTick::Failed(_)),
            "{metadata}: {tick:?}"
        );
        drop(recovery);
        assert_eq!(next_launch(root.path()), [(None, 121.0)]);
    }
}

#[test]
fn a_replaced_tick_reports_cleanup_failure() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let s = session();
    let t0 = Instant::now();
    edit(&s, 121.0, t0);
    let metadata = root.path().join("autosave.json");
    let tick = grooviq_project::autosave::autosave_tick_on(
        &s,
        &recovery,
        t0 + AUTOSAVE_PERIOD,
        || {
            s.lock().unwrap().new_project().unwrap();
            // Force a real removal failure on every platform without changing permissions.
            std::fs::remove_file(&metadata).unwrap();
            std::fs::create_dir_all(metadata.join("occupied")).unwrap();
        },
        &mut Refuse { metadata: true },
    );
    assert!(matches!(&tick, AutosaveTick::Failed(_)), "{tick:?}");
}

#[test]
fn replaced_uncertain_snapshot_is_not_offered_after_restart() {
    for (open, metadata) in [(false, false), (false, true), (true, false), (true, true)] {
        let root = tempfile::tempdir().unwrap();
        let (dir, _) = RecoveryDir::start(root.path()).unwrap();
        let recovery = Mutex::new(Some(dir));
        let s = session();
        let t0 = Instant::now();
        edit(&s, 121.0, t0);
        let tick = grooviq_project::autosave::autosave_tick_on(
            &s,
            &recovery,
            t0 + AUTOSAVE_PERIOD,
            || {
                let mut s = s.lock().unwrap();
                if open {
                    s.open(
                        Path::new("other.grooviq"),
                        grooviq_project::Loaded {
                            project: grooviq_project::Project::new(),
                            samples: Default::default(),
                            warnings: vec![],
                        },
                        Default::default(),
                    )
                    .unwrap();
                } else {
                    s.new_project().unwrap();
                }
            },
            &mut Refuse { metadata },
        );
        assert!(matches!(tick, AutosaveTick::Replaced), "{tick:?}");
        drop(recovery);
        assert!(next_launch(root.path()).is_empty());
    }
}

#[test]
fn old_tick_cleanup_preserves_a_newer_projects_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = RecoveryDir::start(root.path()).unwrap();
    let recovery = Mutex::new(Some(dir));
    let s = session();
    let t0 = Instant::now();
    edit(&s, 121.0, t0);
    let tick = grooviq_project::autosave::autosave_tick_on(
        &s,
        &recovery,
        t0 + AUTOSAVE_PERIOD,
        || {
            s.lock().unwrap().new_project().unwrap();
            edit(&s, 145.0, t0);
            assert!(matches!(
                grooviq_project::autosave::autosave_tick(&s, &recovery, t0 + AUTOSAVE_PERIOD),
                AutosaveTick::Written(_)
            ));
        },
        &mut Refuse { metadata: true },
    );
    assert!(matches!(tick, AutosaveTick::Replaced));
    drop(recovery);
    let found = next_launch(root.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, 145.0);
}
