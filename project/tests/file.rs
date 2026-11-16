//! The .grooviq file: save, load, migrations, validation, backups.

use grooviq_dsp::sampler::SPEC;
use grooviq_project::{
    Channel, FORMAT, LoadError, MAX_STEPS, Note, Pattern, Project, SampleFile, SampleFiles,
    SampleRef, TimeSignature, VERSION, backup_before_save, load, migrate, read_sample_file, save,
    save_with,
};
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
        sampler: Default::default(),
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
    }
}

fn note(channel: u32, tick: u32) -> Note {
    Note {
        channel,
        tick,
        length: 240,
        key: 60,
        velocity: 0.8,
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
    p.patterns[0].notes = vec![note(1, 0), note(2, 480), note(1, 960)];
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
        &json_zip(r#"{"format":"grooviq-project","version":2,"somethingNew":true}"#),
    ) {
        Err(LoadError::NewerVersion { found, supported }) => {
            assert_eq!((found, supported), (2, 1));
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
fn migrate_passes_version_1_through() {
    let json = serde_json::json!({"format": FORMAT, "version": VERSION, "name": "x"});
    assert_eq!(migrate(json.clone()).unwrap(), json);
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
            channel: 1,
            tick: 240,
            length: 120,
            key: 36,
            velocity: 0.5
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
    assert_eq!(v["version"], 1);
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
                channel: 1,
                tick: 0,
                length: 0,
                key: 200,
                velocity: 3.0,
            },
            note(9, 0),                   // channel 9 does not exist
            note(1, MAX_STEPS * 240),     // past the longest pattern
            note(2, MAX_STEPS * 240 - 1), // the last tick that stays
        ],
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
            channel: 1,
            tick: 0,
            length: 1,
            key: 127,
            velocity: 1.0
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
fn a_missing_entry_falls_back_to_the_original_path_then_to_no_sample() {
    let dir = tempfile::tempdir().unwrap();
    let on_disk = write(dir.path(), "kick.wav", &wav_bytes(32));
    let gone = dir.path().join("gone.wav");
    let json = format!(
        r#"{{"format":"grooviq-project","version":1,"name":"x","bpm":140,
            "timeSignature":{{"beats":4,"unit":4}},
            "channels":[
              {{"id":1,"name":"Kick","color":0,"sample":{{"path":{kick},"file":"samples/1-kick.wav"}},"volume":1,"pan":0,"mute":false,"solo":false}},
              {{"id":2,"name":"Snare","color":0,"sample":{{"path":{gone},"file":"samples/2-snare.wav"}},"volume":1,"pan":0,"mute":false,"solo":false}},
              {{"id":3,"name":"Never saved","color":0,"sample":{{"path":{kick},"file":null}},"volume":1,"pan":0,"mute":false,"solo":false}}
            ],
            "patterns":[{{"id":0,"name":"Pattern 1","steps":16,"swing":0,"notes":[]}}],
            "currentPattern":0,"nextId":4}}"#,
        kick = serde_json::to_string(on_disk.to_str().unwrap()).unwrap(),
        gone = serde_json::to_string(gone.to_str().unwrap()).unwrap(),
    );
    let path = write(dir.path(), "a.grooviq", &json_zip(&json));
    let loaded = load(&path).unwrap();

    assert_eq!(
        loaded.warnings,
        vec![
            format!(
                "Channel \"Kick\": its sample is not stored in the file; read \"{}\" from disk instead.",
                on_disk.display()
            ),
            format!(
                "Channel \"Snare\": its sample is not stored in the file and \"{}\" could not be read; the channel is silent until you relink it.",
                gone.display()
            ),
            format!(
                "Channel \"Never saved\": its sample is not stored in the file; read \"{}\" from disk instead.",
                on_disk.display()
            ),
        ]
    );
    assert_eq!(*loaded.samples[&1].bytes, wav_bytes(32));
    assert_eq!(loaded.samples[&1].name, "kick.wav");
    assert!(!loaded.samples.contains_key(&2));
    assert_eq!(*loaded.samples[&3].bytes, wav_bytes(32));
    // The channel keeps its reference, so the person can relink it.
    assert_eq!(
        loaded.project.channels[1].sample.as_ref().unwrap().path,
        gone
    );
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
    let folder = backups.join("Song");
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

    // Another project name has its own folder and its own count.
    let other = dir.path().join("Other.grooviq");
    fs::write(&other, [1]).unwrap();
    backup_before_save(&other, &backups, 10).unwrap();
    assert_eq!(fs::read_dir(backups.join("Other")).unwrap().count(), 1);
}

#[test]
fn the_disk_fallback_only_reads_audio_that_decodes() {
    let dir = tempfile::tempdir().unwrap();
    let text = write(dir.path(), "notes.txt", b"just some text, not a sound");
    let fake = write(dir.path(), "fake.wav", b"just some text, renamed to .wav");
    let real = write(dir.path(), "REAL.WAV", &wav_bytes(32)); // the extension's case does not matter
    let mut p = Project::new();
    p.channels = vec![
        channel(1, "Text", Some(text.to_str().unwrap())),
        channel(2, "Fake", Some(fake.to_str().unwrap())),
        channel(3, "Real", Some(real.to_str().unwrap())),
    ];
    p.next_id = 4;
    let path = dir.path().join("a.grooviq");
    save(&p, &SampleFiles::new(), &path).unwrap(); // no copies: every channel needs the fallback

    let loaded = load(&path).unwrap();
    assert_eq!(loaded.samples.keys().copied().collect::<Vec<_>>(), vec![3]);
    assert_eq!(*loaded.samples[&3].bytes, wav_bytes(32));
    assert_eq!(loaded.warnings.len(), 3, "{:#?}", loaded.warnings);
    assert_eq!(
        loaded.warnings[0],
        format!(
            "Channel \"Text\": its sample is not stored in the file and \"{}\" was not used: only WAV, AIFF, FLAC, MP3 and OGG files are read from a path; the channel is silent until you relink it.",
            text.display()
        )
    );
    assert!(
        loaded.warnings[1].starts_with(&format!(
            "Channel \"Fake\": its sample is not stored in the file and \"{}\" was not used: ",
            fake.display()
        )),
        "{}",
        loaded.warnings[1]
    );
    assert!(loaded.warnings[1].ends_with("; the channel is silent until you relink it."));
    assert!(
        loaded.warnings[2].contains("read") && loaded.warnings[2].contains("from disk instead")
    );

    // Nothing that failed can be written back: only the audio is embedded on the next save.
    let again = dir.path().join("again.grooviq");
    save(&loaded.project, &loaded.samples, &again).unwrap();
    assert_eq!(names(&again), vec!["project.json", "samples/1-REAL.WAV"]);
}
