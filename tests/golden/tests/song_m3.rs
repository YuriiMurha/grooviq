use grooviq_engine as engine;
use grooviq_golden::{
    song::{self, storage},
    write_wav,
};
use grooviq_project as project;
use std::{fs, path::PathBuf, time::Instant};
fn refs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("refs/song_m3")
}
fn setup(d: &std::path::Path) -> project::session::Session {
    let (p, f) = song::fixture();
    let path = d.join("garden.grooviq");
    project::save(&p, &f, &path).unwrap();
    let loaded = project::load(&path).unwrap();
    let mut reopened = loaded.project.clone();
    for asset in &mut reopened.arrangement.audio_assets {
        asset.sample.file = None;
    }
    assert!(
        reopened == p,
        "project metadata/geometry changed across save/load"
    );
    for (id, file) in f {
        assert_eq!(loaded.samples[&id].bytes, file.bytes);
    }
    song::open(&path)
}
fn equal(a: &[u8], b: &[u8]) {
    assert_eq!(a.len(), b.len());
    if let Some(i) = a.iter().zip(b).position(|(a, b)| a != b) {
        panic!(
            "WAV differs at byte {i}; actual {}, expected {}",
            storage::hash(a),
            storage::hash(b)
        );
    }
}
fn update_reference(dir: &std::path::Path, bytes: &[u8], requested: Option<&std::ffi::OsStr>) {
    if requested == Some(std::ffi::OsStr::new("1")) {
        storage::store(dir, bytes);
    }
}
fn verify_update_guard(dir: &std::path::Path) {
    let frozen = dir.join("frozen-ref");
    fs::create_dir_all(&frozen).unwrap();
    let sentinel = b"read-only reference sentinel";
    fs::write(frozen.join("manifest.json"), sentinel).unwrap();
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("no"),
        Some("true"),
        Some("01"),
        Some(" 1"),
        Some("1 "),
    ] {
        update_reference(
            &frozen,
            b"candidate payload",
            value.map(std::ffi::OsStr::new),
        );
        assert_eq!(
            fs::read(frozen.join("manifest.json")).unwrap(),
            sentinel,
            "update value {value:?} wrote the reference"
        );
        assert!(!frozen.join("000.zlib").exists());
    }
    update_reference(
        &frozen,
        b"candidate payload",
        Some(std::ffi::OsStr::new("1")),
    );
    assert_ne!(fs::read(frozen.join("manifest.json")).unwrap(), sentinel);
    assert!(frozen.join("000.zlib").is_file());
}

#[test]
fn complete_song_bytes_roundtrip_and_full_callback_partitions() {
    let d = tempfile::tempdir().unwrap();
    verify_update_guard(d.path());
    let mut s = setup(d.path());
    let path = d.path().join("actual.wav");
    let report = song::export(&s, &path);
    let bytes = fs::read(&path).unwrap();
    let r = hound::WavReader::new(std::io::Cursor::new(&bytes)).unwrap();
    let actual_prefix: Vec<f32> = hound::WavReader::new(std::io::Cursor::new(&bytes))
        .unwrap()
        .samples::<f32>()
        .take(8192)
        .map(Result::unwrap)
        .collect();
    let expected_prefix = engine::render_offline_with(
        engine::EngineConfig {
            sample_rate: 48000.0,
        },
        s.checked_export_state().unwrap(),
        &[(0, engine::Command::Play)],
        4096,
        64,
    );
    grooviq_golden::compare_exact(&actual_prefix, &expected_prefix)
        .expect("production export must play Song samples, not Pattern fallback");
    assert_eq!(report.frames, song::FRAMES as u64);
    assert_eq!(r.duration() as usize, song::FRAMES);
    assert_eq!(r.spec().sample_format, hound::SampleFormat::Float);
    update_reference(
        &refs(),
        &bytes,
        std::env::var_os("SONG_M3_UPDATE").as_deref(),
    );
    equal(&bytes, &storage::load(&refs()).unwrap());
    s.save(
        Some(&d.path().join("saved.grooviq")),
        &d.path().join("backups"),
    )
    .unwrap();
    let mut reopened = song::open(&d.path().join("saved.grooviq"));
    let recovered = project::load(&d.path().join("saved.grooviq")).unwrap();
    let decoded = project::decode_samples(&recovered);
    reopened
        .open_recovered(None, recovered, decoded, Instant::now())
        .unwrap();
    song::export(&reopened, &path);
    equal(&fs::read(&path).unwrap(), &bytes);
    for callback in [64, 128, 256] {
        let samples = engine::render_offline_with(
            engine::EngineConfig {
                sample_rate: 48000.0,
            },
            reopened.checked_export_state().unwrap(),
            &[(0, engine::Command::Play)],
            song::FRAMES,
            callback,
        );
        write_wav(&path, &samples);
        equal(&fs::read(&path).unwrap(), &bytes);
    }
}
#[test]
fn bounded_partitions_cover_overlap_trim_fades_automation_and_seek() {
    let d = tempfile::tempdir().unwrap();
    let s = setup(d.path());
    for tick in [0, 1, 119, 239, 479, 959, 1919, 3839, 13439] {
        let events = [
            (0, engine::Command::Play),
            (0, engine::Command::Seek { tick }),
        ];
        let render = |callback| {
            engine::render_offline_with(
                engine::EngineConfig {
                    sample_rate: 48000.0,
                },
                s.checked_export_state().unwrap(),
                &events,
                52000,
                callback,
            )
        };
        let expected = render(64);
        assert!(expected.iter().any(|v| *v != 0.0));
        for callback in [1, 63, 64, 127, 128, 255, 256, 511] {
            grooviq_golden::compare_exact(&render(callback), &expected).unwrap();
        }
    }
}
#[test]
#[ignore = "five warmed release production exports; run explicitly for Phase A evidence"]
fn warmed_export_budget() {
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "use release profile"
    );
    let d = tempfile::tempdir().unwrap();
    let (p, files) = song::fixture();
    let project_path = d.path().join("garden.grooviq");
    let start = Instant::now();
    project::save(&p, &files, &project_path).unwrap();
    eprintln!(
        "save seconds={}; project bytes={}",
        start.elapsed().as_secs_f64(),
        fs::metadata(&project_path).unwrap().len()
    );
    let start = Instant::now();
    let s = song::open(&project_path);
    eprintln!(
        "load/decode/Session-open seconds={}",
        start.elapsed().as_secs_f64()
    );
    let path = d.path().join("budget.wav");
    song::export(&s, &path);
    let mut times = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        assert_eq!(song::export(&s, &path).frames, song::FRAMES as u64);
        times.push(start.elapsed().as_secs_f64());
    }
    eprintln!(
        "production export seconds={times:?}; wav bytes={}",
        fs::metadata(&path).unwrap().len()
    );
    if let Some(output) = std::env::var_os("SONG_M3_ARTIFACT_DIR") {
        let output = std::path::PathBuf::from(output);
        fs::create_dir_all(&output).unwrap();
        fs::copy(
            d.path().join("garden.grooviq"),
            output.join("garden.grooviq"),
        )
        .unwrap();
        fs::copy(&path, output.join("garden.wav")).unwrap();
    }
    times.sort_by(f64::total_cmp);
    assert!(times[2] <= 9.0, "median {} exceeds 9 seconds", times[2]);
}

#[test]
fn region_loop_mode_and_tempo_have_independent_sample_controls() {
    use std::sync::atomic::AtomicBool;
    let d = tempfile::tempdir().unwrap();
    let s = setup(d.path());
    let state = s.checked_export_state().unwrap();
    assert_eq!(state.playback_mode, engine::PlaybackMode::Song);
    assert_eq!(state.song.end_tick, song::END_TICK);
    assert!(state.song_loop.is_none());
    assert_eq!(state.bpm, 120.0);
    let render = |state, events: &[_]| {
        engine::render_offline_with(
            engine::EngineConfig {
                sample_rate: 48000.0,
            },
            state,
            events,
            100000,
            127,
        )
    };
    let whole = render(state.clone(), &[(0, engine::Command::Play)]);
    let region = engine::SongLoop {
        start_tick: 0,
        end_tick: 960,
    };
    let path = d.path().join("region.wav");
    let options = engine::ExportOptions {
        song_region: Some(region),
        ..song::options()
    };
    let report = engine::export_wav(
        state.clone(),
        &options,
        &path,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(report.frames, 24000);
    let samples = grooviq_golden::read_wav(&path);
    grooviq_golden::compare_exact(&samples[..128], &whole[..128]).unwrap();
    // Isolate the last 120 ticks of a loop: an early scheduler end loses this onset.
    let (mut edge, files) = song::fixture();
    edge.patterns[0].notes = vec![project::Note {
        id: 1,
        channel: 1,
        tick: 900,
        length: 30,
        key: 60,
        velocity: 0.5,
        slide: false,
    }];
    edge.patterns[0].next_note_id = 2;
    edge.arrangement.clips = vec![project::Clip {
        id: 100,
        track: 10,
        name: "Loop edge".into(),
        start_tick: 0,
        muted: false,
        content: project::ClipContent::Pattern {
            pattern: 0,
            offset_tick: 0,
            length_ticks: 3840,
        },
    }];
    project::save(&edge, &files, &d.path().join("edge.grooviq")).unwrap();
    let mut edge_state = song::open(&d.path().join("edge.grooviq"))
        .checked_export_state()
        .unwrap();
    edge_state.song_loop = Some(region);
    let edge_sound = render(edge_state, &[(0, engine::Command::Play)]);
    assert!(edge_sound[..45000].iter().all(|v| *v == 0.0));
    assert!(
        edge_sound[45000..48000].iter().any(|v| *v != 0.0),
        "loop-end onset at tick 900 was dropped"
    );
    let mut looped = state.clone();
    looped.song_loop = Some(region);
    let cycling = render(looped.clone(), &[(0, engine::Command::Play)]);
    assert!(
        cycling
            .iter()
            .zip(&whole)
            .any(|(a, b)| a.to_bits() != b.to_bits())
    );
    let once = engine::export_wav(
        looped,
        &song::options(),
        &path,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(once.frames, song::FRAMES as u64);
    let mut pattern = state.clone();
    pattern.playback_mode = engine::PlaybackMode::Pattern;
    let alternate = render(pattern, &[(0, engine::Command::Play)]);
    assert!(
        alternate
            .iter()
            .zip(&whole)
            .any(|(a, b)| a.to_bits() != b.to_bits())
    );
    let mut fast = state;
    fast.bpm = 180.0;
    let alternate = render(fast, &[(0, engine::Command::Play)]);
    assert!(
        alternate
            .iter()
            .zip(&whole)
            .any(|(a, b)| a.to_bits() != b.to_bits())
    );
}
