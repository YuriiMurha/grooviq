use std::path::Path;

use grooviq_golden::song;
use grooviq_project::{self as project, session::Session};

pub fn reopen(dir: &Path) -> Session {
    let (fixture, files) = song::fixture();
    let path = dir.join("garden.grooviq");
    project::save(&fixture, &files, &path).unwrap();
    song::open(&path)
}
