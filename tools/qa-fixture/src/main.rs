use grooviq_engine::{EngineConfig, engine, load_sample};
use grooviq_project::{
    read_sample_file,
    recovery::RecoveryDir,
    session::{Edit, Session},
};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn wav(kind: u32) -> Vec<u8> {
    let frames = 12_000u32;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + frames * 2).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(48_000u32.to_le_bytes());
    bytes.extend(96_000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((frames * 2).to_le_bytes());
    let mut seed = 0x1234_abcd_u32;
    for i in 0..frames {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let triangle = ((i % 240) as i32 - 120).abs() * 200 - 12_000;
        let noise = (seed % 24_001) as i32 - 12_000;
        let pulse = if i % 96 < 48 { 10_000 } else { -10_000 };
        let value = match kind {
            0 => triangle,
            1 => noise,
            2 => pulse,
            _ => (noise + triangle) / 2,
        };
        let value = (value * (frames - i) as i32 / frames as i32) as i16;
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

fn identifier() -> Result<String> {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../../app/src-tauri/tauri.conf.json"))?;
    let id = config["identifier"]
        .as_str()
        .ok_or("missing application identifier")?;
    if id.is_empty()
        || id == "."
        || id == ".."
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    {
        return Err("unsafe application identifier".into());
    }
    Ok(id.into())
}

fn generate(home: &Path) -> Result<serde_json::Value> {
    // Claim before writing fixture data. A competing generator sees a nonempty directory or
    // loses create_new; neither can replace the other's files. Failed runs remain for inspection.
    // Strip trailing separators and `.` before lstat, which otherwise follows a leaf symlink.
    let normalized: PathBuf = home.components().collect();
    let home = normalized.as_path();
    let meta = fs::symlink_metadata(home)?;
    if meta.file_type().is_symlink() || !meta.is_dir() || fs::read_dir(home)?.next().is_some() {
        return Err("QA home must be an empty, nonsymlink directory".into());
    }
    let _claim = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(home.join(".qa-fixture-claim"))?;
    let home = fs::canonicalize(home)?;
    let data = home.join("Library/Application Support").join(identifier()?);
    let backups = data.join("backups");
    let recovery = data.join("recovery");
    let sounds = home.join("Sounds");
    let projects = home.join("Projects");
    fs::create_dir_all(&sounds)?;
    fs::create_dir_all(&projects)?;
    let project = projects.join("QA beat.grooviq");
    let (controller, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut session = Session::new(Arc::new(Mutex::new(controller)));
    let now = Instant::now();
    let mut paths = Vec::new();
    for (kind, name) in ["Triangle drum", "Noise drum", "Pulse drum", "Mixed drum"]
        .iter()
        .enumerate()
    {
        let path = sounds.join(format!("{name}.wav"));
        fs::write(&path, wav(kind as u32))?;
        session.add_channel(
            &path,
            read_sample_file(&path)?,
            Arc::new(load_sample(&path)?),
            now,
        )?;
        let channel = session
            .project()
            .channels
            .last()
            .ok_or("missing channel")?
            .id;
        for step in (kind as u32..16).step_by(4) {
            session.apply(Edit::ToggleStep { channel, step }, now)?;
        }
        paths.push(path);
    }
    session.save(Some(&project), &backups)?;
    let channel = session.project().channels[0].id;
    session.apply(Edit::ToggleStep { channel, step: 1 }, now)?;
    session.save(None, &backups)?;
    session.apply(Edit::ToggleStep { channel, step: 2 }, now)?;
    let (snapshot, files, original) = session
        .autosave_snapshot(now)
        .ok_or("missing dirty snapshot")?;
    let (dir, found) = RecoveryDir::start(&recovery)?;
    if !found.is_empty() {
        return Err("unexpected recovery in fresh home".into());
    }
    dir.write_autosave(&snapshot, &files, original.as_deref())?;
    drop(dir); // Deliberately omit clean_exit: the next launch must offer Restore.
    Ok(
        serde_json::json!({"home": home, "project": project, "sounds": paths,
        "backups": backups, "recovery": recovery, "layout": "macos-qa-app",
        "savedExtraStep": 1, "unsavedExtraStep": 2}),
    )
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let home = match args.next() {
        Some(flag) if flag == "--home" => Some(PathBuf::from(
            args.next().ok_or("--home requires a directory")?,
        )),
        Some(_) => return Err("usage: grooviq-qa-fixture [--home EMPTY_DIRECTORY]".into()),
        None => std::env::var_os("QA_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
    };
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let home = match home {
        Some(p) => p,
        None => tempfile::Builder::new()
            .prefix("grooviq-qa-")
            .tempdir()?
            .keep(),
    };
    println!("{}", generate(&home)?);
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("qa-fixture: {error}");
        std::process::exit(1);
    }
}
