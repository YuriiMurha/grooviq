//! The .grooviq file: save, load, migrations, validation, backups.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, EngineState, InstrumentKind, PatternData, SeqNote,
};
use grooviq_project::{
    Channel, FORMAT, LoadError, MAX_NOTES_PER_PATTERN, MAX_STEPS, Note, Pattern, Project,
    ProjectKey, SampleFile, SampleFiles, SampleRef, TimeSignature, VERSION, backup_before_save,
    decode_samples, engine_state, load, migrate, read_sample_file, save, save_with,
};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn channel(id: u32, name: &str, sample: Option<&str>) -> Channel {
    Channel {
        id,
        name: name.into(),
        color: id as u8,
        sample: sample.map(|p| SampleRef {
            path: PathBuf::from(p),
            file: None,
        }),
        instrument: InstrumentKind::Sampler,
        sampler: Default::default(),
        synth: Default::default(),
        preset: None,
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
    }
}

fn note(id: u32, channel: u32, tick: u32) -> Note {
    Note {
        id,
        channel,
        tick,
        length: 240,
        key: 60,
        velocity: 0.8,
        slide: false,
    }
}

fn file(name: &str, bytes: &[u8]) -> SampleFile {
    SampleFile {
        name: name.into(),
        bytes: Arc::new(bytes.to_vec()),
    }
}

/// Two channels with samples, a muted one without, and a few notes.
fn project() -> (Project, SampleFiles) {
    let mut p = Project::new();
    p.name = "Round trip".into();
    p.bpm = 123.5;
    p.time_signature = TimeSignature { beats: 7, unit: 8 };
    let mut kick = channel(1, "Kick", Some("/music/kick.wav"));
    kick.sampler.insert("tune".into(), 3.0);
    kick.volume = 0.7;
    kick.pan = -0.3;
    kick.solo = true;
    let mut snare = channel(2, "Snare", Some("/music/snare.wav"));
    snare.mute = true;
    let empty = channel(3, "Empty", None);
    p.channels = vec![kick, snare, empty];
    p.patterns[0].notes = vec![note(1, 1, 0), note(2, 2, 480), note(3, 1, 960)];
    p.patterns[0].next_note_id = 4;
    p.patterns[0].swing = 0.25;
    p.next_id = 10;
    let mut samples = SampleFiles::new();
    samples.insert(1, file("kick.wav", b"RIFF-kick-bytes"));
    samples.insert(2, file("snare.wav", b"RIFF-snare-bytes"));
    (p, samples)
}

/// A tiny valid mono 16-bit WAV (`frames` frames of a ramp), built by hand.
fn wav_bytes(frames: u32) -> Vec<u8> {
    let data = frames * 2;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&44_100u32.to_le_bytes());
    b.extend_from_slice(&88_200u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for i in 0..frames {
        b.extend_from_slice(&((i as i16) * 100).to_le_bytes());
    }
    b
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn json_zip(json: &str) -> Vec<u8> {
    zip_of(&[("project.json", json.as_bytes())])
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn names(path: &Path) -> Vec<String> {
    let mut zip = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().to_string())
        .collect()
}

#[test]
fn a_project_survives_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let (p, samples) = project();
    save(&p, &samples, &path).unwrap();

    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    // Saving records each sample's entry in the project; nothing else changes.
    let mut expected = p.clone();
    expected.channels[0].sample.as_mut().unwrap().file = Some("samples/1-kick.wav".into());
    expected.channels[1].sample.as_mut().unwrap().file = Some("samples/2-snare.wav".into());
    assert_eq!(loaded.project, expected);

    assert_eq!(loaded.samples.len(), 2);
    for id in [1, 2] {
        assert_eq!(loaded.samples[&id].name, samples[&id].name);
        assert_eq!(loaded.samples[&id].bytes, samples[&id].bytes);
    }
    assert!(!loaded.samples.contains_key(&3));
    // The caller's project is not touched.
    assert_eq!(p.channels[0].sample.as_ref().unwrap().file, None);
}

#[test]
fn saving_twice_gives_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (p, samples) = project();
    let (a, b) = (dir.path().join("a.grooviq"), dir.path().join("b.grooviq"));
    save(&p, &samples, &a).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // a clock-stamped entry would differ
    save(&p, &samples, &b).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());

    // Saving over an existing file replaces it (a rename over the target).
    let before = fs::read(&a).unwrap();
    save(&p, &samples, &a).unwrap();
    assert_eq!(fs::read(&a).unwrap(), before);
}

#[test]
fn a_failed_save_leaves_the_previous_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let (mut p, samples) = project();
    save(&p, &samples, &path).unwrap();
    let old = fs::read(&path).unwrap();

    p.name = "Changed".into();
    let err = save_with(&p, &samples, &path, true).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert_eq!(fs::read(&path).unwrap(), old);
    let left: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["a.grooviq".to_string()], "no temp file remains");

    // A real failure after the temp file is written (the target is a folder): same promise.
    let folder = dir.path().join("folder.grooviq");
    fs::create_dir(&folder).unwrap();
    assert!(save(&p, &samples, &folder).is_err());
    assert!(folder.is_dir());
    assert!(!dir.path().join("folder.grooviq.tmp").exists());

    // And the next save works.
    save(&p, &samples, &path).unwrap();
    assert_eq!(load(&path).unwrap().project.name, "Changed");
}

#[test]
fn load_rejects_non_zip_truncated_missing_json_and_newer_versions() {
    let dir = tempfile::tempdir().unwrap();
    let check = |name: &str, bytes: &[u8]| load(&write(dir.path(), name, bytes));

    assert!(matches!(
        check("text.grooviq", b"this is not a zip file at all"),
        Err(LoadError::NotAProject)
    ));
    assert!(matches!(
        check("empty.grooviq", b""),
        Err(LoadError::NotAProject)
    ));

    let (p, samples) = project();
    let good = dir.path().join("good.grooviq");
    save(&p, &samples, &good).unwrap();
    let bytes = fs::read(&good).unwrap();
    let truncated = check("cut.grooviq", &bytes[..bytes.len() / 2]);
    assert!(
        matches!(truncated, Err(LoadError::Corrupt(_))),
        "{truncated:?}"
    );

    assert!(matches!(
        check("other.grooviq", &zip_of(&[("readme.txt", b"hi")])),
        Err(LoadError::NotAProject)
    ));
    assert!(matches!(
        check("badjson.grooviq", &json_zip("{ not json")),
        Err(LoadError::Corrupt(_))
    ));
    assert!(matches!(
        check(
            "foreign.grooviq",
            &json_zip(r#"{"format":"something-else","version":1}"#)
        ),
        Err(LoadError::NotAProject)
    ));
    assert!(matches!(
        check(
            "nover.grooviq",
            &json_zip(r#"{"format":"grooviq-project"}"#)
        ),
        Err(LoadError::Corrupt(_))
    ));
    assert!(matches!(
        check(
            "v0.grooviq",
            &json_zip(r#"{"format":"grooviq-project","version":0}"#)
        ),
        Err(LoadError::Corrupt(_))
    ));
    assert!(matches!(
        check(
            "badfield.grooviq",
            &json_zip(r#"{"format":"grooviq-project","version":1,"bpm":"fast"}"#)
        ),
        Err(LoadError::Corrupt(_))
    ));
    match check(
        "v2.grooviq",
        &json_zip(r#"{"format":"grooviq-project","version":3,"somethingNew":true}"#),
    ) {
        Err(LoadError::NewerVersion { found, supported }) => {
            assert_eq!((found, supported), (3, 2));
        }
        other => panic!("{other:?}"),
    }
    match load(&dir.path().join("missing.grooviq")) {
        Err(LoadError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }

    // Every message is a sentence for the person, not a debug dump.
    let msg = LoadError::NewerVersion {
        found: 2,
        supported: 1,
    }
    .to_string();
    assert!(
        msg.contains("newer") && msg.contains('2') && msg.contains('1'),
        "{msg}"
    );
    assert!(
        LoadError::NotAProject
            .to_string()
            .contains("not a Grooviq project")
    );
    assert!(
        LoadError::Corrupt("x".into())
            .to_string()
            .contains("damaged")
    );
}

#[test]
fn migrate_leaves_the_current_version_alone() {
    let (mut p, _) = project();
    p.patterns[0].notes[0].id = 42;
    let before = p.clone();
    migrate(&mut p, VERSION).unwrap();
    assert_eq!(p, before);
    assert!(matches!(migrate(&mut p, 0), Err(LoadError::Corrupt(_))));
}

#[test]
fn a_hand_written_v1_file_loads() {
    // Pins the format: field names, camelCase, unknown fields ignored.
    let json = r#"{
        "format": "grooviq-project",
        "version": 1,
        "futureField": {"a": 1},
        "name": "Hand written",
        "bpm": 120.0,
        "timeSignature": {"beats": 3, "unit": 4},
        "channels": [
            {"id": 1, "name": "Kick", "color": 2,
             "sample": {"path": "/nowhere/kick.wav", "file": "samples/1-kick.wav"},
             "sampler": {"tune": 3.0}, "volume": 0.9, "pan": 0.1,
             "mute": false, "solo": true, "extra": true}
        ],
        "patterns": [
            {"id": 2, "name": "Beat", "steps": 8, "swing": 0.25,
             "notes": [{"channel": 1, "tick": 240, "length": 120, "key": 36, "velocity": 0.5}]}
        ],
        "currentPattern": 2,
        "nextId": 3
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "hand.grooviq",
        &zip_of(&[
            ("project.json", json.as_bytes()),
            ("samples/1-kick.wav", &[1, 2, 3, 4]),
        ]),
    );
    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let p = loaded.project;
    assert_eq!(p.name, "Hand written");
    assert_eq!(p.bpm, 120.0);
    assert_eq!(p.time_signature, TimeSignature { beats: 3, unit: 4 });
    assert_eq!(p.current_pattern, 2);
    assert_eq!(p.next_id, 3);
    let c = &p.channels[0];
    assert_eq!((c.id, c.name.as_str(), c.color), (1, "Kick", 2));
    let s = c.sample.as_ref().unwrap();
    assert_eq!(s.path, PathBuf::from("/nowhere/kick.wav"));
    assert_eq!(s.file.as_deref(), Some("samples/1-kick.wav"));
    assert_eq!(c.sampler["tune"], 3.0);
    assert_eq!((c.volume, c.pan, c.mute, c.solo), (0.9, 0.1, false, true));
    let pat = &p.patterns[0];
    assert_eq!(
        (pat.id, pat.name.as_str(), pat.steps, pat.swing),
        (2, "Beat", 8, 0.25)
    );
    assert_eq!(
        pat.notes,
        vec![Note {
            id: 1,
            channel: 1,
            tick: 240,
            length: 120,
            key: 36,
            velocity: 0.5,
            slide: false,
        }]
    );
    assert_eq!(loaded.samples[&1].name, "kick.wav");
    assert_eq!(*loaded.samples[&1].bytes, vec![1, 2, 3, 4]);

    // The file written back carries the same field names.
    let again = dir.path().join("again.grooviq");
    save(&p, &loaded.samples, &again).unwrap();
    let mut zip = ZipArchive::new(fs::File::open(&again).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name("project.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["format"], "grooviq-project");
    assert_eq!(v["version"], 2);
    assert_eq!(v["timeSignature"]["beats"], 3);
    assert_eq!(v["currentPattern"], 2);
    assert_eq!(v["nextId"], 3);
    assert_eq!(v["channels"][0]["sample"]["file"], "samples/1-kick.wav");
    assert_eq!(v["patterns"][0]["notes"][0]["velocity"], 0.5);
}

#[test]
fn validate_clamps_out_of_range_values_and_drops_orphan_notes() {
    let mut p = Project::new();
    p.bpm = 999.0;
    p.time_signature = TimeSignature { beats: 20, unit: 3 };
    let mut kick = channel(1, "Kick", None);
    kick.volume = 5.0;
    kick.pan = -3.0;
    kick.sampler.insert("gain".into(), 99.0);
    kick.sampler.insert("tune".into(), 2.0);
    let dup = channel(1, "Dup", None);
    let snare = channel(2, "Snare", None);
    p.channels = vec![kick, dup, snare];
    p.patterns = vec![Pattern {
        id: 0,
        name: "P".into(),
        steps: 0,
        swing: 2.0,
        notes: vec![
            Note {
                id: 1,
                channel: 1,
                tick: 0,
                length: 0,
                key: 200,
                velocity: 3.0,
                slide: false,
            },
            note(2, 9, 0),                   // channel 9 does not exist
            note(3, 1, MAX_STEPS * 240),     // past the longest pattern
            note(4, 2, MAX_STEPS * 240 - 1), // the last tick that stays
        ],
        next_note_id: 5,
        revision: 1,
    }];
    p.current_pattern = 9;
    p.next_id = 0;

    let lines = p.validate();
    assert_eq!(lines.len(), 13, "{lines:#?}");
    let has = |s: &str| assert!(lines.iter().any(|l| l == s), "missing {s:?} in {lines:#?}");
    has("Tempo 999 is outside 20 to 400 BPM; changed to 400.");
    has("Time signature 20/3 is not valid; changed to 16/4.");
    has("Channel \"Kick\": volume 5 is outside 0 to 2; changed to 2.");
    has("Channel \"Kick\": pan -3 is outside -1 to 1; changed to -1.");
    has("Pattern \"P\": dropped 1 note of a channel that does not exist.");
    has("Pattern \"P\": dropped 1 note past step 256.");

    assert_eq!(p.bpm, 400.0);
    assert_eq!(p.time_signature, TimeSignature { beats: 16, unit: 4 });
    assert_eq!(p.channels.len(), 2, "the second channel 1 is dropped");
    assert_eq!(p.channels[0].name, "Kick");
    assert_eq!(p.channels[0].volume, 2.0);
    assert_eq!(p.channels[0].pan, -1.0);
    let gain = grooviq_dsp::sampler::PARAM_IDS
        .iter()
        .position(|&id| id == "gain")
        .unwrap();
    assert_eq!(p.channels[0].sampler["gain"], SPEC[gain].1);
    assert_eq!(p.channels[0].sampler["tune"], 2.0);
    let pat = &p.patterns[0];
    assert_eq!((pat.steps, pat.swing), (1, 1.0));
    assert_eq!(pat.notes.len(), 2);
    assert_eq!(
        pat.notes[0],
        Note {
            id: 1,
            channel: 1,
            tick: 0,
            length: 1,
            key: 127,
            velocity: 1.0,
            slide: false,
        }
    );
    assert_eq!(pat.notes[1].tick, MAX_STEPS * 240 - 1);
    assert_eq!(p.current_pattern, 0);
    assert_eq!(p.next_id, 3, "above every channel and pattern id");

    // Nothing left to fix.
    assert!(p.validate().is_empty());
}

#[test]
fn validate_repairs_patterns_and_caps_channels() {
    let mut p = Project::new();
    p.patterns.clear();
    let lines = p.validate();
    assert_eq!(p.patterns.len(), 1);
    assert_eq!(p.patterns[0].name, "Pattern 1");
    assert_eq!(p.current_pattern, p.patterns[0].id);
    assert!(lines.iter().any(|l| l.contains("no pattern")), "{lines:?}");

    // Two patterns with one id: the second is renumbered.
    let mut p = Project::new();
    let mut second = p.patterns[0].clone();
    second.name = "Second".into();
    p.patterns.push(second);
    p.validate();
    assert_ne!(p.patterns[0].id, p.patterns[1].id);
    assert!(p.next_id > p.patterns[1].id);

    // More channels than the engine holds.
    let mut p = Project::new();
    p.channels = (0..300).map(|i| channel(i, "c", None)).collect();
    let lines = p.validate();
    assert_eq!(p.channels.len(), 256);
    assert!(lines.iter().any(|l| l.contains("256")), "{lines:?}");
}

#[test]
fn samples_are_embedded_and_shared_files_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let mut p = Project::new();
    p.channels = vec![
        channel(1, "A", Some("/x/hat.wav")),
        channel(2, "B", Some("/y/hat.wav")),
        channel(3, "C", Some("/x/hat.wav")),
        channel(4, "D", Some("/x/other.wav")),
    ];
    let shared = Arc::new(b"same bytes".to_vec());
    let mut samples = SampleFiles::new();
    // 1 and 3 share an Arc, 2 holds an equal copy, 4 differs.
    samples.insert(
        1,
        SampleFile {
            name: "hat.wav".into(),
            bytes: shared.clone(),
        },
    );
    samples.insert(2, file("hat.wav", b"same bytes"));
    samples.insert(
        3,
        SampleFile {
            name: "hat.wav".into(),
            bytes: shared,
        },
    );
    samples.insert(4, file("other.wav", b"different"));
    save(&p, &samples, &path).unwrap();

    assert_eq!(
        names(&path),
        vec!["project.json", "samples/1-hat.wav", "samples/2-other.wav"]
    );
    let loaded = load(&path).unwrap();
    let file_of = |i: usize| {
        loaded.project.channels[i]
            .sample
            .as_ref()
            .unwrap()
            .file
            .clone()
    };
    assert_eq!(file_of(0).as_deref(), Some("samples/1-hat.wav"));
    assert_eq!(file_of(1), file_of(0));
    assert_eq!(file_of(2), file_of(0));
    assert_eq!(file_of(3).as_deref(), Some("samples/2-other.wav"));
    for id in 1..=3 {
        assert_eq!(*loaded.samples[&id].bytes, b"same bytes".to_vec());
    }
    assert_eq!(*loaded.samples[&4].bytes, b"different".to_vec());
}

#[test]
fn file_names_cannot_escape_the_samples_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let mut p = Project::new();
    p.channels = vec![
        channel(1, "A", Some("/x/a.wav")),
        channel(2, "B", Some("/x/b.wav")),
    ];
    let mut samples = SampleFiles::new();
    samples.insert(1, file("../../etc/pass\\wd.wav", b"1"));
    samples.insert(2, file("", b"2"));
    save(&p, &samples, &path).unwrap();
    for name in names(&path).iter().skip(1) {
        let rest = name.strip_prefix("samples/").expect(name);
        assert!(
            !rest.contains('/') && !rest.contains('\\') && !rest.contains(".."),
            "{name}"
        );
    }
    let loaded = load(&path).unwrap();
    assert_eq!(*loaded.samples[&1].bytes, b"1".to_vec());
    assert_eq!(*loaded.samples[&2].bytes, b"2".to_vec());
}

#[test]
fn a_project_opens_after_its_original_samples_are_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let original = write(dir.path(), "kick.wav", b"not really a wav, just bytes");
    let mut p = Project::new();
    p.channels = vec![channel(1, "Kick", Some(original.to_str().unwrap()))];
    p.next_id = 2;
    let mut samples = SampleFiles::new();
    samples.insert(1, read_sample_file(&original).unwrap());
    assert_eq!(samples[&1].name, "kick.wav");

    let path = dir.path().join("a.grooviq");
    save(&p, &samples, &path).unwrap();
    fs::remove_file(&original).unwrap();

    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(
        *loaded.samples[&1].bytes,
        b"not really a wav, just bytes".to_vec()
    );
    assert_eq!(loaded.samples[&1].name, "kick.wav");
}

#[test]
fn sample_entries_are_stored_uncompressed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let (p, mut samples) = project();
    samples.insert(1, file("kick.wav", &vec![0u8; 50_000])); // compressible on purpose
    save(&p, &samples, &path).unwrap();

    let mut zip = ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
    for i in 0..zip.len() {
        let entry = zip.by_index(i).unwrap();
        let name = entry.name().unwrap().to_string();
        let t = entry.last_modified().expect("a timestamp");
        assert_eq!((t.year(), t.month(), t.day()), (1980, 1, 1), "{name}");
        assert_eq!((t.hour(), t.minute(), t.second()), (0, 0, 0));
        if name == "project.json" {
            assert_eq!(entry.compression(), CompressionMethod::Deflated);
        } else {
            assert_eq!(entry.compression(), CompressionMethod::Stored, "{name}");
            assert_eq!(entry.size(), entry.compressed_size());
        }
    }
    assert_eq!(zip.by_name("samples/1-kick.wav").unwrap().size(), 50_000);
    // The host system is recorded explicitly, not taken from the platform the file is saved on.
    for i in 0..zip.len() {
        let entry = zip.by_index(i).unwrap();
        assert_eq!(entry.system(), zip::System::Unix, "{:?}", entry.name());
    }
}

#[test]
fn backups_keep_the_last_ten() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("Song.grooviq");
    let backups = dir.path().join("backups");

    // Nothing to back up before the first save.
    assert_eq!(backup_before_save(&target, &backups, 10).unwrap(), None);
    assert!(!backups.exists());

    let mut made = Vec::new();
    for i in 0..13u8 {
        fs::write(&target, [i]).unwrap();
        let copy = backup_before_save(&target, &backups, 10).unwrap().unwrap();
        assert_eq!(fs::read(&copy).unwrap(), vec![i]);
        made.push(copy);
    }
    let folder = grooviq_project::backup_folder(&target, &backups).unwrap();
    let mut kept: Vec<PathBuf> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 10);
    for path in &kept {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("Song-") && name.ends_with(".grooviq"),
            "{name}"
        );
        assert!(
            name["Song-".len()..name.len() - ".grooviq".len()]
                .chars()
                .all(|c| c.is_ascii_digit())
        );
    }
    // The three oldest are gone, the ten newest remain, newest last.
    for gone in &made[..3] {
        assert!(!gone.exists(), "{gone:?}");
    }
    for (i, keep) in made[3..].iter().enumerate() {
        assert_eq!(fs::read(keep).unwrap(), vec![i as u8 + 3]);
    }
    assert_eq!(made.last().unwrap(), kept.iter().max().unwrap());

    // Other files in the folder are left alone.
    fs::write(folder.join("notes.txt"), "keep me").unwrap();
    fs::write(&target, [99]).unwrap();
    backup_before_save(&target, &backups, 10).unwrap();
    assert!(folder.join("notes.txt").exists());
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 11); // ten backups and the note

    // The same bytes as the newest backup: no copy, nothing pruned.
    let before: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backup_before_save(&target, &backups, 10).unwrap(), None);
    let after: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(before.len(), after.len());
    // Same length, other bytes: a backup.
    fs::write(&target, [98]).unwrap();
    assert!(backup_before_save(&target, &backups, 10).unwrap().is_some());

    // Another project name has its own folder and its own count.
    let other = dir.path().join("Other.grooviq");
    fs::write(&other, [1]).unwrap();
    backup_before_save(&other, &backups, 10).unwrap();
    let other_folder = grooviq_project::backup_folder(&other, &backups).unwrap();
    assert_ne!(other_folder, folder);
    assert_eq!(fs::read_dir(other_folder).unwrap().count(), 1);
}

/// A project of channels whose zip has no sample entries: each is `(name, path)`, and says it was
/// embedded (`"file":"samples/<id>-x.wav"`) except where `embedded` is false.
fn project_zip_without_entries(channels: &[(&str, &str, bool)]) -> Vec<u8> {
    let list: Vec<String> = channels
        .iter()
        .enumerate()
        .map(|(i, (name, path, embedded))| {
            let file = if *embedded {
                format!("\"samples/{}-x.wav\"", i + 1)
            } else {
                "null".to_string()
            };
            format!(
                r#"{{"id":{id},"name":{name},"color":0,"sample":{{"path":{path},"file":{file}}},"volume":1,"pan":0,"mute":false,"solo":false}}"#,
                id = i + 1,
                name = serde_json::to_string(name).unwrap(),
                path = serde_json::to_string(path).unwrap(),
            )
        })
        .collect();
    json_zip(&format!(
        r#"{{"format":"grooviq-project","version":1,"name":"x","bpm":140,
            "timeSignature":{{"beats":4,"unit":4}},"channels":[{}],
            "patterns":[{{"id":0,"name":"Pattern 1","steps":16,"swing":0,"notes":[]}}],
            "currentPattern":0,"nextId":{}}}"#,
        list.join(","),
        channels.len() + 1
    ))
}

#[test]
fn load_never_reads_a_path_named_by_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let real = write(dir.path(), "kick.wav", &wav_bytes(32)); // (a) a valid WAV that exists
    let text = write(dir.path(), "notes.txt", b"just some text, not a sound"); // (b)
    let channels = [
        ("Kick", real.to_str().unwrap(), true),
        ("Text", text.to_str().unwrap(), true),
        // (c) relative paths resolve against the working directory: "Cargo.toml" exists there
        // when the tests run, "kick.wav" does not.
        ("Relative", "Cargo.toml", true),
        ("Relative wav", "kick.wav", true),
        // (d) a network share
        ("Share", "//server/share/x.wav", true),
        // Never embedded at all: the path is no more used than for a missing entry.
        ("Never saved", real.to_str().unwrap(), false),
    ];
    let path = write(
        dir.path(),
        "a.grooviq",
        &project_zip_without_entries(&channels),
    );

    let loaded = load(&path).unwrap();

    assert!(loaded.samples.is_empty(), "{:?}", loaded.samples.keys());
    assert_eq!(
        loaded.warnings,
        channels
            .iter()
            .map(|(name, ..)| format!(
                "Channel \"{name}\": its sample is missing from the project file. Relink it to hear it."
            ))
            .collect::<Vec<_>>()
    );
    // Each channel keeps its path (to show and to relink) but has no entry name.
    for (channel, (_, path, _)) in loaded.project.channels.iter().zip(channels) {
        let sample = channel.sample.as_ref().unwrap();
        assert_eq!(sample.path, Path::new(path), "{}", channel.name);
        assert_eq!(sample.file, None, "{}", channel.name);
    }
    // Nothing was found, so a save has nothing to lose and embeds nothing.
    let again = dir.path().join("again.grooviq");
    save(&loaded.project, &loaded.samples, &again).unwrap();
    assert_eq!(names(&again), vec!["project.json"]);
}

/// Opening a FIFO for reading blocks until a writer appears, and a writer's `open` blocks until a
/// reader does: so a writer thread parked on the FIFO learns that something opened it.
#[cfg(unix)]
#[test]
fn load_does_not_even_open_the_path_the_project_names() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("kick.wav");
    if !std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success())
    {
        return; // no mkfifo here
    }
    let channels = [("Kick", fifo.to_str().unwrap(), true)];
    let path = write(
        dir.path(),
        "a.grooviq",
        &project_zip_without_entries(&channels),
    );

    let (tx, rx) = std::sync::mpsc::channel();
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        let end = fs::OpenOptions::new().write(true).open(&writer_path);
        tx.send(end.is_ok()).unwrap();
    });

    let loaded = load(&path).unwrap();
    assert!(loaded.samples.is_empty());
    assert_eq!(
        loaded.warnings,
        vec![
            "Channel \"Kick\": its sample is missing from the project file. Relink it to hear it."
        ]
    );
    let opened = rx.recv_timeout(std::time::Duration::from_millis(500));
    // Release the parked writer whatever happened, so the test never hangs.
    if opened.is_err() {
        let _ = fs::File::open(&fifo);
    }
    writer.join().unwrap();
    assert!(
        opened.is_err(),
        "load opened the path named by the project file"
    );
}

#[test]
fn load_validates_and_reports_what_it_fixed() {
    let json = r#"{
        "format": "grooviq-project", "version": 1, "name": "x", "bpm": 999.0,
        "timeSignature": {"beats": 4, "unit": 4},
        "channels": [{"id": 1, "name": "Kick"}],
        "patterns": [{"id": 2, "name": "Beat", "steps": 16, "swing": 0.0, "notes": [
            {"channel": 1, "tick": 0}, {"channel": 9, "tick": 240}]}],
        "currentPattern": 2, "nextId": 3
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(&write(dir.path(), "a.grooviq", &json_zip(json))).unwrap();
    assert_eq!(loaded.project.bpm, 400.0, "clamped");
    assert_eq!(
        loaded.project.patterns[0].notes.len(),
        1,
        "orphan note dropped"
    );
    assert_eq!(
        loaded.warnings,
        vec![
            "Tempo 999 is outside 20 to 400 BPM; changed to 400.".to_string(),
            "Pattern \"Beat\": dropped 1 note of a channel that does not exist.".to_string(),
        ]
    );
}

#[test]
fn validate_replaces_non_finite_values_with_their_defaults() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut p = Project::new();
        p.bpm = bad;
        let mut c = channel(1, "Kick", None);
        c.volume = bad;
        c.pan = bad;
        c.sampler.insert("gain".into(), bad);
        p.channels = vec![c];
        p.next_id = 2;
        p.patterns[0].swing = bad;
        p.patterns[0].notes = vec![Note {
            velocity: bad,
            ..note(1, 1, 0)
        }];
        p.patterns[0].next_note_id = 2;
        let lines = p.validate();
        assert_eq!(p.bpm, 140.0, "{bad}");
        assert_eq!(p.channels[0].volume, 1.0, "{bad}");
        assert_eq!(p.channels[0].pan, 0.0, "{bad}");
        let gain = grooviq_dsp::sampler::PARAM_IDS
            .iter()
            .position(|&id| id == "gain")
            .unwrap();
        assert_eq!(p.channels[0].sampler["gain"], SPEC[gain].2, "{bad}");
        assert_eq!(p.patterns[0].swing, 0.0, "{bad}");
        assert_eq!(p.patterns[0].notes[0].velocity, 0.8, "{bad}");
        assert_eq!(lines.len(), 6, "{bad}: {lines:#?}");
        assert!(p.validate().is_empty(), "{bad}");
    }
}

#[test]
fn saving_never_overwrites_the_old_file_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let link = dir.path().join("old.link");
    let (mut p, samples) = project();
    save(&p, &samples, &path).unwrap();
    let old = fs::read(&path).unwrap();
    // A second name for the same file: it sees any in-place write, and none made to a new file.
    fs::hard_link(&path, &link).unwrap();

    p.name = "A different name".into();
    save(&p, &samples, &path).unwrap();
    assert_eq!(
        fs::read(&link).unwrap(),
        old,
        "the old file was written into"
    );
    assert_eq!(load(&path).unwrap().project.name, "A different name");
}

/// An M1 project as M1 Grooviq wrote it: two sampler channels, swing, an odd-step note and a
/// note off the sampler's root.
const V1_JSON: &str = r#"{
    "format": "grooviq-project",
    "version": 1,
    "name": "Old beat",
    "bpm": 96.0,
    "timeSignature": {"beats": 4, "unit": 4},
    "channels": [
        {"id": 1, "name": "Kick", "color": 0,
         "sample": {"path": "/m1/kick.wav", "file": "samples/1-kick.wav"},
         "sampler": {"tune": -2.0, "release": 40.0}, "volume": 0.8, "pan": -0.5,
         "mute": false, "solo": false},
        {"id": 2, "name": "Hat", "color": 1,
         "sample": {"path": "/m1/hat.wav", "file": "samples/2-hat.wav"},
         "sampler": {}, "volume": 1.0, "pan": 0.25, "mute": true, "solo": false}
    ],
    "patterns": [
        {"id": 3, "name": "Beat", "steps": 8, "swing": 0.5, "notes": [
            {"channel": 2, "tick": 240, "length": 120, "key": 60, "velocity": 0.5},
            {"channel": 1, "tick": 0, "length": 240, "key": 60, "velocity": 1.0},
            {"channel": 1, "tick": 960, "length": 240, "key": 48, "velocity": 0.9}
        ]}
    ],
    "currentPattern": 3,
    "nextId": 4
}"#;

/// `project.json` of a saved file, parsed.
fn saved_json(path: &Path) -> serde_json::Value {
    let mut zip = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name("project.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn a_v1_file_opens_and_sounds_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let (kick, hat) = (wav_bytes(64), wav_bytes(32));
    let path = write(
        dir.path(),
        "m1.grooviq",
        &zip_of(&[
            ("project.json", V1_JSON.as_bytes()),
            ("samples/1-kick.wav", &kick),
            ("samples/2-hat.wav", &hat),
        ]),
    );
    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    // The migration: every channel is a sampler in no choke group, and the notes are numbered
    // 1..=n in the order they were stored.
    let p = &loaded.project;
    assert_eq!(p.key, None);
    for c in &p.channels {
        assert_eq!(c.instrument, InstrumentKind::Sampler);
        assert_eq!((c.choke, c.preset.as_deref()), (0, None));
        assert!(c.synth.is_empty());
    }
    let pat = &p.patterns[0];
    let ids: Vec<(u32, u32, u32)> = pat
        .notes
        .iter()
        .map(|n| (n.id, n.channel, n.tick))
        .collect();
    assert_eq!(ids, vec![(1, 2, 240), (2, 1, 0), (3, 1, 960)]);
    assert!(pat.notes.iter().all(|n| !n.slide));
    assert_eq!((pat.next_note_id, pat.revision), (4, 1));

    // The engine gets exactly what M1 compiled from this file, written out by hand here.
    let decoded = decode_samples(&loaded);
    assert!(decoded.warnings.is_empty(), "{:?}", decoded.warnings);
    let sampler = |overrides: &[(&str, f32)]| -> Vec<f32> {
        grooviq_dsp::sampler::PARAM_IDS
            .iter()
            .zip(SPEC.iter())
            .map(|(id, s)| {
                overrides
                    .iter()
                    .find(|(name, _)| name == id)
                    .map_or(s.2, |o| o.1)
            })
            .collect()
    };
    let seq = |tick, channel, key, velocity, length| SeqNote {
        tick,
        channel: ChannelId(channel),
        key,
        velocity,
        length,
        slide: false,
    };
    let m1 = EngineState {
        bpm: 96.0,
        channels: vec![
            ChannelSetup::sampler(
                ChannelId(1),
                Some(decoded.samples[&1].clone()),
                sampler(&[("tune", -2.0), ("release", 40.0)]),
                ChannelMix {
                    volume: 0.8,
                    pan: -0.5,
                    mute: false,
                    solo: false,
                },
            ),
            ChannelSetup::sampler(
                ChannelId(2),
                Some(decoded.samples[&2].clone()),
                sampler(&[]),
                ChannelMix {
                    volume: 1.0,
                    pan: 0.25,
                    mute: true,
                    solo: false,
                },
            ),
        ],
        pattern: PatternData {
            length: 8 * 240,
            // Swing 0.5 moves the odd-step hat 60 ticks later.
            notes: vec![
                seq(0, 1, 60, 1.0, 240),
                seq(300, 2, 60, 0.5, 120),
                seq(960, 1, 48, 0.9, 240),
            ],
        },
        // The file's 4/4 (M1's engine had no time signature: it counted 4/4).
        time_signature: (p.time_signature.beats, p.time_signature.unit),
    };
    assert_eq!(p.time_signature, TimeSignature { beats: 4, unit: 4 });
    assert_eq!(engine_state(p, &decoded.samples), m1);

    // Saving writes version 2, with the new fields.
    let again = dir.path().join("again.grooviq");
    save(p, &loaded.samples, &again).unwrap();
    let v = saved_json(&again);
    assert_eq!(v["version"], 2);
    assert_eq!(v["channels"][1]["instrument"], "sampler");
    assert_eq!(v["channels"][1]["choke"], 0);
    assert_eq!(v["patterns"][0]["notes"][0]["id"], 1);
    assert_eq!(v["patterns"][0]["notes"][2]["slide"], false);
    assert_eq!(v["patterns"][0]["nextNoteId"], 4);
    assert!(v["patterns"][0].get("revision").is_none(), "never saved");
    let reloaded = load(&again).unwrap();
    assert!(reloaded.warnings.is_empty(), "{:?}", reloaded.warnings);
    assert_eq!(reloaded.project, *p);

    // The migration step on its own.
    // A v1 file with v2 fields in it (no M1 Grooviq wrote one) is still read as v1.
    let mut v1: Project = serde_json::from_str(V1_JSON).unwrap();
    v1.channels[0].instrument = InstrumentKind::Synth;
    v1.patterns[0].notes[0].id = 77;
    migrate(&mut v1, 1).unwrap();
    assert_eq!(v1.channels[0].instrument, InstrumentKind::Sampler);
    let ids: Vec<u32> = v1.patterns[0].notes.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert_eq!(v1.patterns[0].next_note_id, 4);
}

#[test]
fn a_v2_project_survives_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.grooviq");
    let mut p = Project::new();
    p.key = Some(ProjectKey {
        root: 9,
        scale: "minor".into(),
    });
    let mut kick = channel(1, "Kick", Some("/music/kick.wav"));
    kick.choke = 3;
    let mut bass = channel(2, "Bass", None);
    bass.instrument = InstrumentKind::Synth;
    bass.preset = Some("808 Glide".into());
    bass.synth.insert("filter_cutoff".into(), 650.0);
    bass.synth.insert("glide".into(), 90.0);
    bass.synth.insert("voice_mode".into(), 2.0);
    p.channels = vec![kick, bass];
    p.next_id = 3;
    let mut slide = note(8, 2, 480);
    slide.key = 43;
    slide.length = 960;
    slide.slide = true;
    p.patterns[0].notes = vec![note(1, 1, 0), note(5, 2, 0), slide];
    p.patterns[0].next_note_id = 9;
    p.patterns[0].revision = 7;
    let mut samples = SampleFiles::new();
    samples.insert(1, file("kick.wav", b"RIFF-kick-bytes"));
    save(&p, &samples, &path).unwrap();

    let v = saved_json(&path);
    assert_eq!(v["version"], 2);
    assert_eq!(v["key"], serde_json::json!({"root": 9, "scale": "minor"}));
    assert_eq!(v["channels"][1]["instrument"], "synth");
    assert_eq!(v["channels"][1]["preset"], "808 Glide");
    assert_eq!(v["channels"][1]["synth"]["glide"], 90.0);
    assert_eq!(v["channels"][0]["choke"], 3);
    assert_eq!(v["patterns"][0]["notes"][2]["slide"], true);
    assert_eq!(v["patterns"][0]["nextNoteId"], 9);

    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let mut expected = p.clone();
    expected.channels[0].sample.as_mut().unwrap().file = Some("samples/1-kick.wav".into());
    // The revision is the session's edit counter: it is not saved and starts at 1.
    expected.patterns[0].revision = 1;
    assert_eq!(loaded.project, expected);
}

#[test]
fn ids_are_repaired_and_note_count_capped() {
    let note_json = |id: u32, tick: u32| {
        format!(
            r#"{{"id": {id}, "channel": 1, "tick": {tick}, "length": 240, "key": 60, "velocity": 0.8}}"#
        )
    };
    let dups = [(5, 0), (5, 240), (0, 480), (7, 720)]
        .iter()
        .map(|&(id, tick)| note_json(id, tick))
        .collect::<Vec<_>>()
        .join(",");
    let total = MAX_NOTES_PER_PATTERN + 5;
    let huge = (1..=total as u32)
        .map(|id| note_json(id, (id * 7) % (MAX_STEPS * 240)))
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"format": "grooviq-project", "version": 2, "name": "x",
        "channels": [{{"id": 1, "name": "Kick", "instrument": "sampler"}}],
        "patterns": [
            {{"id": 2, "name": "Dups", "nextNoteId": 3, "notes": [{dups}]}},
            {{"id": 3, "name": "Huge", "nextNoteId": {next}, "notes": [{huge}]}}
        ],
        "currentPattern": 2, "nextId": 4}}"#,
        next = total + 1
    );
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(&write(dir.path(), "ids.grooviq", &json_zip(&json))).unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            "Pattern \"Dups\": 2 notes had a missing or repeated id and got new ones.".to_string(),
            format!(
                "Pattern \"Huge\": dropped 5 notes past the limit of {MAX_NOTES_PER_PATTERN} notes per pattern."
            ),
        ]
    );

    // The first of two equal ids keeps it; the repaired ones count up from above the largest.
    let d = &loaded.project.patterns[0];
    let ids: Vec<(u32, u32)> = d.notes.iter().map(|n| (n.id, n.tick)).collect();
    assert_eq!(ids, vec![(5, 0), (8, 240), (9, 480), (7, 720)]);
    assert_eq!(d.next_note_id, 10, "above every id");

    // The cap keeps the notes stored first.
    let h = &loaded.project.patterns[1];
    assert_eq!(h.notes.len(), MAX_NOTES_PER_PATTERN);
    assert_eq!(h.notes.last().unwrap().id, MAX_NOTES_PER_PATTERN as u32);
    let unique: HashSet<u32> = h.notes.iter().map(|n| n.id).collect();
    assert_eq!(unique.len(), MAX_NOTES_PER_PATTERN);
    assert_eq!(h.next_note_id, total as u32 + 1);

    // An id at the top of the range leaves no room above it: the pattern is renumbered, and
    // a note added next still gets an unused id.
    let mut p = Project::new();
    p.channels = vec![channel(1, "Kick", None)];
    p.next_id = 2;
    p.patterns[0].name = "Top".into();
    p.patterns[0].notes = vec![
        note(u32::MAX, 1, 0),
        note(u32::MAX, 1, 240),
        note(3, 1, 480),
    ];
    p.patterns[0].next_note_id = 1;
    assert_eq!(
        p.validate(),
        vec![
            "Pattern \"Top\": its note ids left no room for new notes; 3 notes were renumbered."
                .to_string()
        ]
    );
    let ids: Vec<u32> = p.patterns[0].notes.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert_eq!(p.patterns[0].next_note_id, 4);
    assert!(p.validate().is_empty());

    // A counter at or below the largest id is moved above it without a warning: it is
    // bookkeeping the person cannot act on.
    p.patterns[0].next_note_id = 2;
    assert!(p.validate().is_empty());
    assert_eq!(p.patterns[0].next_note_id, 4);
}

#[test]
fn out_of_range_v2_values_are_clamped_with_warnings() {
    let json = |key: &str| {
        format!(
            r#"{{"format": "grooviq-project", "version": 2, "name": "x", {key}
            "channels": [{{"id": 1, "name": "Bass", "instrument": "synth", "choke": 99,
                "synth": {{"filter_cutoff": 1000000000.0, "glide": 50.0, "lydian_wobble": 3.0}}}}],
            "patterns": [{{"id": 2, "name": "P", "notes": []}}],
            "currentPattern": 2, "nextId": 3}}"#
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(&write(
        dir.path(),
        "a.grooviq",
        &json_zip(&json(r#""key": {"root": 14, "scale": "minor"},"#)),
    ))
    .unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            "The project's key (root 14, scale \"minor\") is not valid; it was cleared."
                .to_string(),
            "Channel \"Bass\": choke group 99 is outside 0 to 16; changed to 16.".to_string(),
            "Channel \"Bass\": synth value \"filter_cutoff\" 1000000000 is outside 20 to 20000; changed to 20000.".to_string(),
        ]
    );
    let p = &loaded.project;
    assert_eq!(p.key, None);
    let c = &p.channels[0];
    assert_eq!(c.instrument, InstrumentKind::Synth);
    assert_eq!(c.choke, 16);
    // An unknown synth parameter is dropped without a word, as unknown fields are.
    let names: Vec<&str> = c.synth.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["filter_cutoff", "glide"]);
    assert_eq!(
        (c.synth["filter_cutoff"], c.synth["glide"]),
        (20_000.0, 50.0)
    );

    // An unknown scale clears the key too; a known one is kept.
    let loaded = load(&write(
        dir.path(),
        "b.grooviq",
        &json_zip(&json(r#""key": {"root": 2, "scale": "lydianish"},"#)),
    ))
    .unwrap();
    assert_eq!(loaded.project.key, None);
    assert_eq!(
        loaded.warnings[0],
        "The project's key (root 2, scale \"lydianish\") is not valid; it was cleared."
    );
    for scale in [
        "major",
        "minor",
        "harmonic_minor",
        "melodic_minor",
        "dorian",
        "phrygian",
        "lydian",
        "mixolydian",
        "locrian",
        "major_pentatonic",
        "minor_pentatonic",
        "blues",
    ] {
        let mut p = Project::new();
        p.key = Some(ProjectKey {
            root: 11,
            scale: scale.into(),
        });
        assert!(p.validate().is_empty(), "{scale}");
        assert!(p.key.is_some(), "{scale}");
    }
}

#[test]
fn a_v3_file_is_refused_as_newer() {
    let json = serde_json::json!({"format": FORMAT, "version": 3, "channels": "whatever"});
    match migrate(&mut Project::new(), 3) {
        Err(LoadError::NewerVersion { found, supported }) => {
            assert_eq!((found, supported), (3, 2));
        }
        other => panic!("{other:?}"),
    }
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "v3.grooviq",
        &json_zip(&serde_json::to_string(&json).unwrap()),
    );
    let err = load(&path).unwrap_err();
    assert!(matches!(
        err,
        LoadError::NewerVersion {
            found: 3,
            supported: 2
        }
    ));
    assert!(
        err.to_string().contains("newer version of Grooviq"),
        "{err}"
    );
}

#[test]
fn a_save_over_the_note_cap_is_refused_and_keeps_the_old_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.grooviq");
    let (p, samples) = project();
    save(&p, &samples, &path).unwrap();
    let before = fs::read(&path).unwrap();

    // One note more than `load` keeps: saving it would lose that note on the next open.
    let mut over = p.clone();
    over.patterns[0].name = "Full".into();
    over.patterns[0].notes = (1..=MAX_NOTES_PER_PATTERN as u32 + 1)
        .map(|id| note(id, 1, 0))
        .collect();
    over.patterns[0].next_note_id = MAX_NOTES_PER_PATTERN as u32 + 2;
    let err = save(&over, &samples, &path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        format!(
            "Pattern \"Full\" has 100001 notes, more than the {MAX_NOTES_PER_PATTERN} a pattern can hold, so the project was not saved."
        )
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "the old file is untouched"
    );
    assert_eq!(
        names_in(dir.path()),
        vec!["cap.grooviq".to_string()],
        "no temp file left"
    );

    // Exactly at the cap saves and loads whole.
    over.patterns[0].notes.pop();
    save(&over, &samples, &path).unwrap();
    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(
        loaded.project.patterns[0].notes.len(),
        MAX_NOTES_PER_PATTERN
    );
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_cap_counts_only_notes_that_can_play() {
    // 100,000 notes of a channel that does not exist, then 100,000 past step 256, then two
    // playable ones: the cap must not spend itself on notes cleanup drops anyway. `patterns`
    // comes before `channels`, so the reader cannot rely on field order.
    let orphans = std::iter::repeat_n(r#"{"channel":99,"tick":0}"#, MAX_NOTES_PER_PATTERN)
        .collect::<Vec<_>>()
        .join(",");
    let far = format!(r#"{{"channel":1,"tick":{}}}"#, MAX_STEPS * 240);
    let far = std::iter::repeat_n(far.as_str(), MAX_NOTES_PER_PATTERN)
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"format":"grooviq-project","version":1,
        "patterns":[{{"id":0,"name":"Beat","notes":[{orphans},{far},
            {{"channel":1,"tick":0}},{{"channel":2,"tick":240}}]}}],
        "channels":[{{"id":1,"name":"Kick"}},{{"id":2,"name":"Hat"}}],
        "nextId":3}}"#
    );
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(&write(dir.path(), "orphans.grooviq", &json_zip(&json))).unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            "Pattern \"Beat\": dropped 100000 notes of channels that do not exist.".to_string(),
            "Pattern \"Beat\": dropped 100000 notes past step 256.".to_string(),
        ]
    );
    let kept: Vec<(u32, u32, u32)> = loaded.project.patterns[0]
        .notes
        .iter()
        .map(|n| (n.id, n.channel, n.tick))
        .collect();
    assert_eq!(
        kept,
        vec![(1, 1, 0), (2, 2, 240)],
        "the playable notes survive"
    );

    // The cap still applies to playable notes, counted apart from the cleanup.
    let playable = (0..MAX_NOTES_PER_PATTERN + 3)
        .map(|_| r#"{"channel":1,"tick":0}"#)
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        r#"{{"format":"grooviq-project","version":2,
        "channels":[{{"id":1}}],"nextId":2,
        "patterns":[{{"id":0,"name":"Full","notes":[{{"channel":7,"tick":0}},{playable}]}}]}}"#
    );
    let loaded = load(&write(dir.path(), "full.grooviq", &json_zip(&json))).unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            "Pattern \"Full\": dropped 1 note of a channel that does not exist.".to_string(),
            format!(
                "Pattern \"Full\": dropped 3 notes past the limit of {MAX_NOTES_PER_PATTERN} notes per pattern."
            ),
            format!(
                "Pattern \"Full\": {MAX_NOTES_PER_PATTERN} notes had a missing or repeated id and got new ones."
            ),
        ]
    );
    assert_eq!(
        loaded.project.patterns[0].notes.len(),
        MAX_NOTES_PER_PATTERN
    );
}

#[test]
fn a_v1_file_ignores_the_fields_its_migration_supplies() {
    // M1 never wrote these, but a v1 file that carries them (from an extension, or by hand)
    // opened before v2 existed: migration replaces them, whatever their type.
    let json = r#"{"format":"grooviq-project","version":1,
        "channels":[{"id":1,"name":"Kick","instrument":"plugin"},{"id":2,"instrument":7}],
        "patterns":[{"id":0,"name":"Beat","nextNoteId":"extension","notes":[
            {"channel":1,"tick":0,"id":"extension"},{"channel":2,"tick":240,"id":-3}]}],
        "nextId":3}"#;
    let dir = tempfile::tempdir().unwrap();
    let loaded = load(&write(dir.path(), "ext.grooviq", &json_zip(json))).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let p = &loaded.project;
    assert!(
        p.channels
            .iter()
            .all(|c| c.instrument == InstrumentKind::Sampler)
    );
    let ids: Vec<u32> = p.patterns[0].notes.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(p.patterns[0].next_note_id, 3);
    // The version is only known for the length of a load: afterwards ids read as ids again.
    let note: Note = serde_json::from_str(r#"{"id": 5, "channel": 1, "tick": 0}"#).unwrap();
    assert_eq!(note.id, 5);

    // In a v2 file the same fields are the format's own, and a wrong one is damage.
    for bad in [
        r#""channels":[{"id":1,"instrument":"plugin"}],"patterns":[{"id":0,"notes":[]}]"#,
        r#""channels":[{"id":1}],"patterns":[{"id":0,"nextNoteId":"x","notes":[]}]"#,
        r#""channels":[{"id":1}],"patterns":[{"id":0,"notes":[{"channel":1,"tick":0,"id":"x"}]}]"#,
    ] {
        let json = format!(r#"{{"format":"grooviq-project","version":2,{bad},"nextId":2}}"#);
        let path = write(dir.path(), "v2.grooviq", &json_zip(&json));
        assert!(matches!(load(&path), Err(LoadError::Corrupt(_))), "{bad}");
    }
}

/// A project document of `version` with these `channels` and `patterns` (JSON) and `extra`
/// top-level fields (each ending in a comma).
fn doc(version: u32, channels: &str, patterns: &str, extra: &str) -> String {
    format!(
        r#"{{"format":"grooviq-project","version":{version},{extra}"channels":{channels},"patterns":{patterns},"nextId":2}}"#
    )
}

#[test]
fn structs_written_as_arrays_are_refused_as_damage() {
    // Grooviq writes every struct with named fields. serde would also read a struct as an array
    // of its fields in order; a file that does so is refused, never half read (a channel kept
    // while its notes are dropped as orphans, or a pattern whose notes go unread).
    let note = r#"{"id":1,"channel":1,"tick":0}"#;
    let channels = r#"[{"id":1}]"#;
    let patterns = format!(r#"[{{"id":0,"notes":[{note}]}}]"#);
    let cases = [
        ("channel", "[[1]]".to_string(), patterns.clone(), ""),
        (
            "pattern",
            channels.to_string(),
            format!(r#"[[0,"P",16,0,[{note}]]]"#),
            "",
        ),
        (
            "note",
            channels.to_string(),
            r#"[{"id":0,"notes":[[1,1,0]]}]"#.to_string(),
            "",
        ),
        (
            "key",
            channels.to_string(),
            patterns.clone(),
            r#""key":[9,"minor"],"#,
        ),
        (
            "time signature",
            channels.to_string(),
            patterns.clone(),
            r#""timeSignature":[3,4],"#,
        ),
        (
            "sample",
            r#"[{"id":1,"sample":["/a.wav",null]}]"#.to_string(),
            patterns.clone(),
            "",
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (what, channels, patterns, extra) in &cases {
        for v in [1, 2] {
            let path = write(
                dir.path(),
                "array.grooviq",
                &json_zip(&doc(v, channels, patterns, extra)),
            );
            match load(&path) {
                Err(LoadError::Corrupt(why)) => assert!(
                    why.contains("an object with named fields"),
                    "{what}, version {v}: {why}"
                ),
                other => panic!("{what}, version {v}: {other:?}"),
            }
        }
    }

    // The same file with named fields loads whole, note included.
    let path = write(
        dir.path(),
        "named.grooviq",
        &json_zip(&doc(
            2,
            channels,
            &patterns,
            r#""key":{"root":9,"scale":"minor"},"timeSignature":{"beats":3,"unit":4},"#,
        )),
    );
    let loaded = load(&path).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(loaded.project.patterns[0].notes.len(), 1);
    // A whole document that is an array is not a project at all, as before.
    let path = write(dir.path(), "top.grooviq", &json_zip("[1, 2]"));
    assert!(matches!(load(&path), Err(LoadError::NotAProject)));
}

#[test]
fn the_typed_read_refuses_structs_written_as_arrays_on_its_own() {
    // Without the first pass (a plain serde read), each struct still needs named fields.
    let base = |channels: &str, patterns: &str, extra: &str| {
        format!(r#"{{{extra}"channels":{channels},"patterns":{patterns}}}"#)
    };
    let ok = r#"[{"id":0,"notes":[{"channel":1,"tick":0}]}]"#;
    for json in [
        base("[[1]]", ok, ""),
        base(r#"[{"id":1}]"#, r#"[[0,"P",16,0,[]]]"#, ""),
        base(r#"[{"id":1}]"#, r#"[{"id":0,"notes":[[1,1,0]]}]"#, ""),
        base(r#"[{"id":1}]"#, ok, r#""key":[9,"minor"],"#),
        base(r#"[{"id":1}]"#, ok, r#""timeSignature":[3,4],"#),
        base(r#"[{"id":1,"sample":["/a.wav",null]}]"#, ok, ""),
    ] {
        let err = serde_json::from_str::<Project>(&json).unwrap_err();
        assert!(
            err.to_string().contains("an object with named fields"),
            "{json}: {err}"
        );
    }
    let named = base(r#"[{"id":1}]"#, ok, r#""key":{"root":9,"scale":"minor"},"#);
    assert_eq!(
        serde_json::from_str::<Project>(&named).unwrap().patterns[0]
            .notes
            .len(),
        1
    );
}

/// A pattern holding the most notes a pattern can, every field of every note in its widest
/// range, so its JSON is as long as a real pattern's can be.
fn full_pattern(id: u32, channel: u32) -> Pattern {
    let mut p = Pattern::new(id, format!("Full {id}"));
    p.steps = MAX_STEPS;
    let ticks = MAX_STEPS * 240;
    p.notes = (0..MAX_NOTES_PER_PATTERN as u32)
        .map(|i| Note {
            id: i + 1,
            channel,
            tick: ticks - 1 - (i * 7919) % 10_000,
            length: ticks - (i % 997),
            key: (i % 128) as u8,
            velocity: (i % 997) as f32 / 997.0,
            slide: i % 2 == 0,
        })
        .collect();
    p.next_note_id = MAX_NOTES_PER_PATTERN as u32 + 1;
    p
}

/// A project of `patterns` full patterns on one channel.
fn full_project(patterns: u32) -> Project {
    let mut p = Project::new();
    p.channels = vec![channel(1, "Lead", None)];
    p.patterns = (2..patterns + 2).map(|id| full_pattern(id, 1)).collect();
    p.current_pattern = 2;
    p.next_id = patterns + 2;
    p
}

/// Bytes of `project.json` in a saved file.
fn json_bytes(path: &Path) -> u64 {
    let mut zip = ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    zip.by_name("project.json").unwrap().size()
}

#[test]
fn several_full_patterns_survive_load_save_load() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.grooviq");
    let b = dir.path().join("b.grooviq");
    let p = full_project(4);
    save(&p, &SampleFiles::new(), &a).unwrap();
    let first = load(&a).unwrap_or_else(|e| panic!("the saved file does not open: {e}"));
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    assert_eq!(first.project.patterns, p.patterns);

    save(&first.project, &first.samples, &b).unwrap();
    let second = load(&b).unwrap_or_else(|e| panic!("the file saved again does not open: {e}"));
    assert_eq!(second.project.patterns, p.patterns);
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
    // Compact: well inside the reader's 64 MB for project.json.
    assert!(json_bytes(&a) < 48 << 20, "{} bytes", json_bytes(&a));
}

#[test]
fn a_save_the_loader_would_refuse_is_refused_and_keeps_the_old_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.grooviq");
    let (old_project, samples) = project();
    save(&old_project, &samples, &path).unwrap();
    let old = fs::read(&path).unwrap();

    // Eight full patterns: about 80 MB of JSON, more than `load` reads.
    let err = save(&full_project(8), &SampleFiles::new(), &path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        "The project is too big to save: its notes and settings would take more than the 64 MB a project file can hold. Remove some notes, then save again."
    );
    assert_eq!(fs::read(&path).unwrap(), old, "the old file is intact");
    assert_eq!(names_in(dir.path()), vec!["a.grooviq"], "nothing written");
}

fn preset_project(label: Option<String>) -> Project {
    let mut p = Project::new();
    let mut c = channel(1, "Bass", None);
    c.instrument = InstrumentKind::Synth;
    c.preset = label;
    c.synth.insert("filter_cutoff".into(), 731.25);
    c.synth.insert("glide".into(), 93.5);
    c.synth.insert("voice_mode".into(), 2.0);
    p.channels.push(c);
    p.next_id = 2;
    p
}

#[test]
fn preset_metadata_validation_preserves_known_labels_and_sound() {
    for label in [None, Some("Init")]
        .into_iter()
        .chain(grooviq_dsp::synth::PRESETS.into_iter().map(Some))
    {
        let mut p = preset_project(label.map(str::to_owned));
        let before = serde_json::to_value(&p).unwrap();
        assert!(p.validate().is_empty());
        assert_eq!(serde_json::to_value(&p).unwrap(), before);
    }
}

#[test]
fn preset_metadata_validation_clears_unknown_labels_without_changing_sound() {
    for label in [
        "Future Patch",
        "",
        " Init",
        "Init ",
        "init",
        "sub bass",
        "Sub Bass\n",
    ] {
        let mut p = preset_project(Some(label.into()));
        let mut expected = serde_json::to_value(&p).unwrap();
        expected["channels"][0]["preset"] = serde_json::Value::Null;
        let warnings = p.validate();
        assert_eq!(p.channels[0].preset, None);
        assert_eq!(serde_json::to_value(&p).unwrap(), expected);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].len() < 160);
        assert!(p.validate().is_empty());
    }
}

#[test]
fn preset_metadata_file_roundtrip_normalizes_large_label_preserving_all_other_data() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.grooviq");
    let output = dir.path().join("normalized.grooviq");
    let p = preset_project(Some("UNTRUSTED_LABEL".repeat(750_000)));
    let mut expected = serde_json::to_value(&p).unwrap();
    expected["channels"][0]["preset"] = serde_json::Value::Null;
    save(&p, &SampleFiles::new(), &input).unwrap();
    let mut loaded = load(&input).unwrap();
    assert!(loaded.project.channels[0].preset.is_none());
    assert_eq!(serde_json::to_value(&loaded.project).unwrap(), expected);
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].len() < 160);
    assert!(!loaded.warnings[0].contains("UNTRUSTED_LABEL"));
    assert!(loaded.project.validate().is_empty());
    save(&loaded.project, &loaded.samples, &output).unwrap();
    let reloaded = load(&output).unwrap();
    assert!(reloaded.warnings.is_empty());
    assert_eq!(serde_json::to_value(&reloaded.project).unwrap(), expected);
    let before = saved_json(&input);
    let after = saved_json(&output);
    assert_eq!(
        before["channels"][0]["synth"],
        after["channels"][0]["synth"]
    );
    assert_eq!(before["version"], after["version"]);
}

#[test]
fn preset_metadata_validation_leaves_sampler_metadata_alone() {
    let mut p = preset_project(Some("Sampler display metadata".into()));
    p.channels[0].instrument = InstrumentKind::Sampler;
    let before = serde_json::to_value(&p).unwrap();
    assert!(p.validate().is_empty());
    assert_eq!(serde_json::to_value(&p).unwrap(), before);
}
