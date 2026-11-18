use grooviq_project::{backup_folder, decode_samples, load, recovery::RecoveryDir};
use std::{fs, path::Path, process::Command};

fn run(home: &Path, cli: bool) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_grooviq-qa-fixture"));
    if cli {
        cmd.arg("--home").arg(home);
    } else {
        cmd.env("QA_HOME", home);
    }
    cmd.output().unwrap()
}
fn fixture(home: &Path, cli: bool) -> serde_json::Value {
    let out = run(home, cli);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn path(value: &serde_json::Value) -> &Path {
    Path::new(value.as_str().unwrap())
}

#[test]
fn cli_space_path_and_real_crash_recovery() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("QA home");
    fs::create_dir(&home).unwrap();
    let manifest = fixture(&home, false);
    let saved = load(path(&manifest["project"])).unwrap();
    assert!(saved.warnings.is_empty());
    assert_eq!(saved.project.channels.len(), 4);
    assert_eq!(saved.project.patterns[0].notes.len(), 17);
    let folder = backup_folder(path(&manifest["project"]), path(&manifest["backups"])).unwrap();
    let backups: Vec<_> = fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let old = load(&backups[0]).unwrap();
    assert_eq!(old.project.patterns[0].notes.len(), 16);
    assert_eq!(decode_samples(&old).samples.len(), 4);
    let (recovery, found) = RecoveryDir::start(path(&manifest["recovery"])).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].original.as_deref(),
        Some(path(&manifest["project"]))
    );
    let crash = load(&found[0].file).unwrap();
    assert_eq!(crash.project.patterns[0].notes.len(), 18);
    let first = saved.project.channels[0].id;
    assert!(
        !saved.project.patterns[0]
            .notes
            .iter()
            .any(|n| n.channel == first && n.tick == 2 * grooviq_engine::STEP_TICKS)
    );
    assert!(
        crash.project.patterns[0]
            .notes
            .iter()
            .any(|n| n.channel == first && n.tick == 2 * grooviq_engine::STEP_TICKS)
    );
    assert_eq!(decode_samples(&crash).samples.len(), 4);
    recovery.clean_exit().unwrap();
    for sound in manifest["sounds"].as_array().unwrap() {
        fs::remove_file(path(sound)).unwrap();
    }
    let embedded = load(path(&manifest["project"])).unwrap();
    assert!(embedded.warnings.is_empty());
    let decoded = decode_samples(&embedded);
    assert_eq!(decoded.samples.len(), 4);
    for sample in decoded.samples.values() {
        assert_eq!(sample.rate, 48_000.0);
        assert_eq!(sample.l.len(), 12_000);
        assert!(sample.l.iter().all(|x| x.is_finite()));
        assert!(sample.l.iter().any(|x| x.abs() > 0.01));
    }
}

#[test]
fn deterministic_wavs_across_homes_and_cli_argument() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let ma = fixture(a.path(), true);
    let mb = fixture(b.path(), false);
    let mut sounds = Vec::new();
    for (a, b) in ma["sounds"]
        .as_array()
        .unwrap()
        .iter()
        .zip(mb["sounds"].as_array().unwrap())
    {
        let bytes = fs::read(path(a)).unwrap();
        assert_eq!(bytes, fs::read(path(b)).unwrap());
        sounds.push(bytes);
    }
    for i in 0..4 {
        for j in i + 1..4 {
            assert_ne!(sounds[i], sounds[j]);
        }
    }
}

#[test]
fn nonempty_destination_is_preserved() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("owner data"), b"keep me").unwrap();
    assert!(!run(home.path(), false).status.success());
    assert_eq!(
        fs::read(home.path().join("owner data")).unwrap(),
        b"keep me"
    );
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn symlink_destination_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let actual = root.path().join("actual");
    let link = root.path().join("link");
    fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &link).unwrap();
    assert!(!run(&link, true).status.success());
    assert!(!run(&link.join("."), true).status.success());
    assert_eq!(fs::read_dir(&actual).unwrap().count(), 0);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn default_home_persists_and_second_run_refuses_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_grooviq-qa-fixture"))
        .env_remove("QA_HOME")
        .output()
        .unwrap();
    assert!(out.status.success());
    let manifest: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let home = path(&manifest["home"]);
    assert!(path(&manifest["project"]).is_file());
    let before = fs::read(path(&manifest["project"])).unwrap();
    assert!(!run(home, true).status.success());
    assert_eq!(before, fs::read(path(&manifest["project"])).unwrap());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn concurrent_generators_have_one_winner() {
    let home = tempfile::tempdir().unwrap();
    let mut a = Command::new(env!("CARGO_BIN_EXE_grooviq-qa-fixture"))
        .arg("--home")
        .arg(home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut b = Command::new(env!("CARGO_BIN_EXE_grooviq-qa-fixture"))
        .arg("--home")
        .arg(home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert_ne!(a.wait().unwrap().success(), b.wait().unwrap().success());
    assert!(load(&home.path().join("Projects/QA beat.grooviq")).is_ok());
}
