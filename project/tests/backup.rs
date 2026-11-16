//! Rolling backups of projects that share a file name.

use grooviq_project::backup_before_save;
use std::fs;
use std::path::{Path, PathBuf};

/// Writes `song.grooviq` in `folder` with `content`, backing up what was there.
fn save_song(folder: &Path, backups: &Path, content: &str) {
    let target = folder.join("song.grooviq");
    backup_before_save(&target, backups, 10).unwrap();
    fs::write(&target, content).unwrap();
}

/// The folders in `backups`, each with how many backups it holds.
fn folders(backups: &Path) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = fs::read_dir(backups)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let held = fs::read_dir(e.path()).unwrap().count();
            (e.file_name().to_string_lossy().into_owned(), held)
        })
        .collect();
    found.sort();
    found
}

#[test]
fn same_named_projects_in_different_folders_keep_ten_backups_each() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    let backups = dir.path().join("backups");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    // One more save than there is room for, in turns: neither may prune the other.
    for n in 0..13 {
        save_song(&a, &backups, &format!("a{n}"));
        save_song(&b, &backups, &format!("b{n}"));
    }
    let found = folders(&backups);
    assert_eq!(found.len(), 2, "one folder per project folder: {found:?}");
    for (name, held) in &found {
        assert_eq!(*held, 10, "{name}");
        // `<stem>-<8 hex>`.
        let hash = name.strip_prefix("song-").unwrap_or("");
        assert!(
            hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "{name}"
        );
    }
    assert_ne!(found[0].0, found[1].0);
}

#[test]
fn a_project_keeps_one_backup_folder_however_its_path_is_spelled() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("music");
    fs::create_dir_all(&folder).unwrap();
    let backups = dir.path().join("backups");
    save_song(&folder, &backups, "one");
    // `music/../music`: the same folder.
    let roundabout: PathBuf = folder.join("..").join("music");
    save_song(&roundabout, &backups, "two");
    save_song(&folder, &backups, "three");
    let found = folders(&backups);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, 2);
}

#[test]
fn an_old_style_backup_folder_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("music");
    fs::create_dir_all(&folder).unwrap();
    let backups = dir.path().join("backups");
    let old = backups.join("song");
    fs::create_dir_all(&old).unwrap();
    for n in 1..=10 {
        fs::write(old.join(format!("song-{n}.grooviq")), format!("old {n}")).unwrap();
    }
    save_song(&folder, &backups, "first");
    save_song(&folder, &backups, "second");
    // Never read, renamed, merged or pruned.
    assert_eq!(fs::read_dir(&old).unwrap().count(), 10);
    assert_eq!(fs::read(old.join("song-1.grooviq")).unwrap(), b"old 1");
    let found = folders(&backups);
    assert_eq!(found.len(), 2, "{found:?}");
}
