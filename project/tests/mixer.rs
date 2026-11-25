//! Project v4 mixer persistence: the lossless legacy upgrade, the strict model and its bounds,
//! and the guard that keeps a routed mixer out of every path that would play or write it dry.

use grooviq_engine as e;
use grooviq_engine::InstrumentKind;
use grooviq_project::compile::compile_channels;
use grooviq_project::compile_song::compile_song;
use grooviq_project::mixer::{
    Assignment, Insert, Level, Limits, Mixer, MixerError, Mode, Send, Sidechain, SourceKey, Tap,
    Track, read_with_limits,
};
use grooviq_project::recovery::RecoveryDir;
use grooviq_project::session::{Session, SessionError};
use grooviq_project::{
    LoadError, Loaded, PlayMode, Project, decode_samples, engine_state, load, save, validate_save,
};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Renders and data of the fixtures as the last version 3 build (90ec26d) produced them.
const BASELINE: [(&str, u64, u64); 3] = [
    ("v1", 0x0b4d_3305_2ae3_f1ae, 0x728d_f32f_82f0_4ddf),
    ("v2", 0xc10f_35b6_d9fa_1725, 0xb3e3_e049_4b7a_3e6e),
    ("v3", 0x864b_0d92_c75b_b1ef, 0x6f47_398f_c689_746c),
];

// ---- Independent legacy fixtures (hand-written, never produced by this build's writer) ----

/// A mono 16-bit WAV, integer samples only (identical on every platform).
fn wav(frames: u32, period: u32, scale: i16) -> Vec<u8> {
    let mut b = vec![];
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48000u32.to_le_bytes());
    b.extend_from_slice(&96000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(frames * 2).to_le_bytes());
    for i in 0..frames {
        let v = (i % period) as i32 - (period / 2) as i32;
        b.extend_from_slice(&((v as i16).wrapping_mul(scale)).to_le_bytes());
    }
    b
}

fn zip_of(json: &str, entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    z.start_file("project.json", options).unwrap();
    z.write_all(json.as_bytes()).unwrap();
    for (name, bytes) in entries {
        z.start_file(*name, options).unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap().into_inner()
}

/// Version 1: channel 7 before channel 2, instruments and note ids that migration replaces,
/// two embedded samples and a malformed `mixer` that was an unknown field then.
fn v1() -> Vec<u8> {
    zip_of(
        r#"{"format":"grooviq-project","version":1,"name":"One","bpm":128,
        "channels":[
          {"id":7,"name":"Kick","instrument":["not","read"],"sample":{"path":"/gone/kick.wav","file":"samples/1-kick.wav"},"volume":0.9,"pan":-0.3},
          {"id":2,"name":"Snare","instrument":"synth","sample":{"path":"/gone/snare.wav","file":"samples/2-snare.wav"},"volume":0.7,"pan":0.4,"choke":1}],
        "patterns":[{"id":0,"name":"Beat","steps":16,"swing":0.3,"nextNoteId":"x",
          "notes":[{"id":"x","channel":7,"tick":0,"key":60,"velocity":1.0},
                   {"channel":2,"tick":960,"key":62,"velocity":0.6},
                   {"id":99,"channel":7,"tick":1920,"length":480,"key":55,"velocity":0.8},
                   {"channel":2,"tick":2880,"key":64,"velocity":0.5}]}],
        "currentPattern":0,"nextId":9,
        "mixer":{"tracks":[[1,2],{"bad":null}],"mode":42}}"#,
        &[
            ("samples/1-kick.wav", wav(9000, 120, 200)),
            ("samples/2-snare.wav", wav(6000, 37, 700)),
        ],
    )
}

/// Version 2: a synth and a sampler (channel 4 before channel 1), parameters, a slide, a key
/// and a time signature, and a `mixer` of the wrong type.
fn v2() -> Vec<u8> {
    zip_of(
        r#"{"format":"grooviq-project","version":2,"name":"Two","bpm":96,
        "timeSignature":{"beats":3,"unit":4},"key":{"root":9,"scale":"minor"},
        "channels":[
          {"id":4,"name":"Bass","instrument":"synth","synth":{"cutoff":0.4,"glide":0.2},"volume":1.2,"pan":0.1},
          {"id":1,"name":"Hat","instrument":"sampler","sample":{"path":"/gone/hat.wav","file":"samples/1-hat.wav"},"sampler":{"tune":3},"volume":0.5,"pan":-0.6}],
        "patterns":[{"id":3,"name":"Line","steps":12,"swing":0.0,"nextNoteId":5,
          "notes":[{"id":1,"channel":4,"tick":0,"length":720,"key":45,"velocity":0.9},
                   {"id":2,"channel":4,"tick":480,"length":240,"key":52,"velocity":0.9,"slide":true},
                   {"id":3,"channel":1,"tick":240,"key":60,"velocity":0.7},
                   {"id":4,"channel":1,"tick":1200,"key":60,"velocity":0.4}]}],
        "currentPattern":3,"nextId":5,
        "mixer":"legacy builds never wrote this"}"#,
        &[("samples/1-hat.wav", wav(3000, 11, 1500))],
    )
}

/// Version 3: Song mode with a pattern clip, an audio clip on its own playlist track and a
/// volume automation clip; channel 5 before channel 3; a `mixer` that looks almost valid.
fn v3() -> Vec<u8> {
    zip_of(
        r#"{"format":"grooviq-project","version":3,"name":"Three","bpm":120,"playMode":"song",
        "channels":[
          {"id":5,"name":"Lead","instrument":"synth","synth":{"cutoff":0.7},"volume":0.8,"pan":0.2},
          {"id":3,"name":"Hit","instrument":"sampler","sample":{"path":"/gone/hit.wav","file":"samples/1-hit.wav"},"volume":1.0}],
        "patterns":[{"id":1,"name":"Riff","steps":16,"nextNoteId":4,
          "notes":[{"id":1,"channel":5,"tick":0,"length":480,"key":67,"velocity":0.8},
                   {"id":2,"channel":3,"tick":960,"key":60,"velocity":1.0},
                   {"id":3,"channel":5,"tick":1920,"length":960,"key":71,"velocity":0.6}]}],
        "currentPattern":1,"nextId":41,
        "arrangement":{
          "tracks":[{"id":10,"name":"Notes","color":1,"mute":false},{"id":11,"name":"Audio","color":2,"mute":false}],
          "clips":[
            {"id":20,"track":10,"name":"Riff","startTick":0,"muted":false,
             "content":{"type":"pattern","pattern":1,"offsetTick":0,"lengthTicks":3840}},
            {"id":21,"track":11,"name":"Tone","startTick":960,"muted":false,
             "content":{"type":"audio","asset":30,"sourceStart":0,"sourceEnd":4800,"gain":0.7,"reverse":false,"fadeIn":100,"fadeOut":200}},
            {"id":22,"track":10,"name":"Vol","startTick":0,"muted":false,
             "content":{"type":"automation","target":{"type":"channelVolume","channel":5},"priority":40,"offsetTick":0,"lengthTicks":3840,
               "points":[{"tick":0,"value":0.2,"mode":"linear"},{"tick":3840,"value":1.0,"mode":"hold"}]}}],
          "audioAssets":[{"id":30,"sample":{"path":"/gone/tone.wav","file":"samples/2-tone.wav"},"sourceRate":48000,"sourceFrames":4800,"originalTempo":120.0,"originalKey":null}]},
        "mixer":{"mode":"routed","tracks":[{"id":0}],"nextTrackId":1,"nextInsertId":1,"nextSendId":1,"extra":true}}"#,
        &[
            ("samples/1-hit.wav", wav(4000, 50, 600)),
            ("samples/2-tone.wav", wav(4800, 300, 50)),
        ],
    )
}

fn fnv(samples: &[f32]) -> u64 {
    samples.iter().fold(0xcbf2_9ce4_8422_2325, |h, s| {
        s.to_bits()
            .to_le_bytes()
            .iter()
            .fold(h, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
    })
}

/// Opens `loaded` in a fresh session and renders what it plays, offline.
fn play(loaded: Loaded) -> Vec<f32> {
    let (controller, _processor) = e::engine(e::EngineConfig {
        sample_rate: 48000.0,
    });
    let mut s = Session::new(Arc::new(Mutex::new(controller)));
    let decoded = decode_samples(&loaded);
    s.open(Path::new("fixture.grooviq"), loaded, decoded)
        .unwrap();
    e::render_offline_with(
        e::EngineConfig {
            sample_rate: 48000.0,
        },
        s.checked_export_state().unwrap(),
        &[(0, e::Command::Play)],
        100_000,
        64,
    )
}

fn open_bytes(dir: &Path, name: &str, bytes: &[u8]) -> Loaded {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    load(&path).unwrap()
}
// ---- End of fixtures ----

/// The project's data without its mixer, keys sorted: what a version 3 build kept.
fn data_hash(p: &Project) -> u64 {
    let mut v = serde_json::to_value(p).unwrap();
    v.as_object_mut().unwrap().remove("mixer");
    let s = serde_json::to_string(&v).unwrap();
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn fixture(name: &str) -> Vec<u8> {
    match name {
        "v1" => v1(),
        "v2" => v2(),
        _ => v3(),
    }
}

fn reloaded(loaded: &Loaded) -> Loaded {
    Loaded {
        project: loaded.project.clone(),
        samples: loaded.samples.clone(),
        warnings: vec![],
    }
}

#[test]
fn legacy_files_upgrade_to_legacy_direct_with_data_and_sound_unchanged() {
    let d = tempfile::tempdir().unwrap();
    for (name, pcm, data) in BASELINE {
        let loaded = open_bytes(d.path(), &format!("{name}.grooviq"), &fixture(name));
        assert!(loaded.warnings.is_empty(), "{name}: {:?}", loaded.warnings);
        // Whatever the old file's `mixer` held, the upgrade is the legacy identity.
        assert_eq!(loaded.project.mixer, Mixer::default(), "{name}");
        assert_eq!(loaded.project.mixer.mode, Mode::LegacyDirect, "{name}");
        assert_eq!(data_hash(&loaded.project), data, "{name}: data changed");
        assert_eq!(fnv(&play(reloaded(&loaded))), pcm, "{name}: sound changed");

        // Saved as version 4, reopened: the same project and the same sound.
        let path = d.path().join(format!("{name}-v4.grooviq"));
        save(&loaded.project, &loaded.samples, &path).unwrap();
        let json = project_json(&path);
        assert_eq!(json["version"], 4, "{name}");
        assert_eq!(json["mixer"]["mode"], "legacyDirect", "{name}");
        let again = load(&path).unwrap();
        assert_eq!(again.project, loaded.project, "{name}");
        assert_eq!(fnv(&play(again)), pcm, "{name}: sound changed after reopen");

        // The same through an autosave recovered after a crash.
        let root = d.path().join(format!("recovery-{name}"));
        let (recovery, _) = RecoveryDir::start(&root).unwrap();
        recovery
            .write_autosave(&loaded.project, &loaded.samples, None)
            .unwrap();
        drop(recovery);
        let (_recovery, found) = RecoveryDir::start(&root).unwrap();
        let recovered = load(&found[0].file).unwrap();
        assert_eq!(recovered.project, loaded.project, "{name}");
        assert_eq!(
            fnv(&play(recovered)),
            pcm,
            "{name}: sound changed after recovery"
        );
    }
}

#[test]
fn legacy_data_is_what_the_old_files_say() {
    let d = tempfile::tempdir().unwrap();
    let one = open_bytes(d.path(), "1.grooviq", &v1()).project;
    let ids: Vec<u32> = one.channels.iter().map(|c| c.id).collect();
    assert_eq!(ids, [7, 2], "file order, not id order");
    assert!(
        one.channels
            .iter()
            .all(|c| c.instrument == InstrumentKind::Sampler)
    );
    let notes: Vec<(u32, u32, u8)> = one.patterns[0]
        .notes
        .iter()
        .map(|n| (n.id, n.tick, n.key))
        .collect();
    assert_eq!(
        notes,
        [(1, 0, 60), (2, 960, 62), (3, 1920, 55), (4, 2880, 64)]
    );
    assert_eq!(one.patterns[0].next_note_id, 5);

    let two = open_bytes(d.path(), "2.grooviq", &v2()).project;
    let kinds: Vec<(u32, InstrumentKind)> =
        two.channels.iter().map(|c| (c.id, c.instrument)).collect();
    assert_eq!(
        kinds,
        [(4, InstrumentKind::Synth), (1, InstrumentKind::Sampler)]
    );
    assert!(two.patterns[0].notes[1].slide);
    assert_eq!(two.play_mode, PlayMode::Pattern);

    let three = open_bytes(d.path(), "3.grooviq", &v3()).project;
    assert_eq!(three.play_mode, PlayMode::Song);
    let ids: Vec<u32> = three.channels.iter().map(|c| c.id).collect();
    assert_eq!(ids, [5, 3]);
    let clips: Vec<u32> = three.arrangement.clips.iter().map(|c| c.id).collect();
    assert_eq!(clips, [20, 21, 22]);
    assert_eq!(three.arrangement.audio_assets[0].id, 30);

    let fresh = Project::new();
    assert_eq!(fresh.mixer.mode, Mode::LegacyDirect);
    assert!(fresh.mixer.is_identity());
}

fn project_json(path: &Path) -> serde_json::Value {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    serde_json::from_reader(zip.by_name("project.json").unwrap()).unwrap()
}

fn json_file(dir: &Path, name: &str, json: &str) -> Result<Loaded, LoadError> {
    let path = dir.join(name);
    std::fs::write(&path, zip_of(json, &[])).unwrap();
    load(&path)
}

const IDENTITY: &str = r#"{"mode":"legacyDirect","tracks":[{"id":0,"name":"Master"}],"nextTrackId":1,"nextInsertId":1,"nextSendId":1}"#;

#[test]
fn only_version_1_skips_its_migrated_fields() {
    let d = tempfile::tempdir().unwrap();
    // The mixer skip is its own rule: a version 2 file still types `instrument` and note ids.
    for (name, json) in [
        (
            "instrument",
            r#"{"format":"grooviq-project","version":2,"channels":[{"id":1,"instrument":["x"]}]}"#,
        ),
        (
            "note",
            r#"{"format":"grooviq-project","version":3,"channels":[{"id":1}],"patterns":[{"id":0,"notes":[{"id":"x","channel":1,"tick":0}]}]}"#,
        ),
    ] {
        assert!(
            matches!(json_file(d.path(), name, json), Err(LoadError::Corrupt(_))),
            "{name}"
        );
    }
}

#[test]
fn a_version_4_file_needs_a_strict_mixer() {
    let d = tempfile::tempdir().unwrap();
    let file = |mixer: &str| {
        format!(r#"{{"format":"grooviq-project","version":4,"channels":[{{"id":1}}]{mixer}}}"#)
    };
    let ok = json_file(d.path(), "ok", &file(&format!(r#","mixer":{IDENTITY}"#))).unwrap();
    assert_eq!(ok.project.mixer, Mixer::default());
    for (name, mixer) in [
        ("missing", String::new()),
        ("scalar", r#","mixer":7"#.into()),
        ("array", r#","mixer":["legacyDirect"]"#.into()),
        (
            "unknown field",
            format!(
                r#","mixer":{}"#,
                IDENTITY.replace("\"mode\"", "\"x\":1,\"mode\"")
            ),
        ),
        (
            "track as array",
            format!(
                r#","mixer":{}"#,
                IDENTITY.replace(r#"{"id":0,"name":"Master"}"#, "[0]")
            ),
        ),
        (
            "twice",
            format!(r#","mixer":{IDENTITY},"mixer":{IDENTITY}"#),
        ),
        (
            "bad mode",
            format!(r#","mixer":{}"#, IDENTITY.replace("legacyDirect", "flat")),
        ),
        (
            "no master",
            format!(
                r#","mixer":{}"#,
                IDENTITY.replace(r#""id":0"#, r#""id":1,"output":0"#)
            ),
        ),
        (
            "unknown channel",
            format!(
                r#","mixer":{}"#,
                IDENTITY.replace(
                    "\"nextTrackId\"",
                    r#""sources":[{"source":{"channel":9},"destination":0}],"nextTrackId""#
                )
            ),
        ),
    ] {
        assert!(
            matches!(
                json_file(d.path(), name, &file(&mixer)),
                Err(LoadError::Corrupt(_))
            ),
            "{name}"
        );
    }
    // A newer version is refused as newer whatever its mixer holds.
    assert!(matches!(
        json_file(
            d.path(),
            "v5",
            r#"{"format":"grooviq-project","version":5,"mixer":[[]]}"#
        ),
        Err(LoadError::NewerVersion {
            found: 5,
            supported: 4
        })
    ));
}

fn defaults(device: &str) -> Vec<f32> {
    grooviq_dsp::param_spec(device)
        .unwrap()
        .iter()
        .map(|s| s.2)
        .collect()
}

fn track(id: u32, output: Option<u32>, inserts: Vec<Insert>) -> Track {
    Track {
        id,
        name: format!("T{id}"),
        output,
        inserts,
        level: Level::default(),
        pan: 0.0,
        mute: false,
        solo: false,
    }
}

fn insert(id: u32, device: &str) -> Insert {
    Insert {
        id,
        device: device.into(),
        params: defaults(device),
        preset: None,
        seed: u64::from(id),
        bypass: false,
    }
}

/// A valid routed mixer for the v3 fixture: channel 5 to a bus with an EQ, playlist track 11
/// to a bus with a compressor fed by a pre-fader send and a sidechain.
fn routed() -> Mixer {
    let mut eq = insert(1, "eq");
    eq.preset = Some("Warm".into());
    eq.seed = u64::MAX;
    let mut comp = insert(2, "comp");
    comp.bypass = true;
    let mut drums = track(1, Some(0), vec![eq]);
    drums.level = Level::Db(-3.0);
    drums.pan = 0.25;
    let mut verb = track(2, Some(0), vec![comp]);
    verb.level = Level::Silence;
    Mixer {
        mode: Mode::Routed,
        tracks: vec![Track::master(), drums, verb],
        sends: vec![Send {
            id: 1,
            from: 1,
            to: 2,
            tap: Tap::Pre,
            gain: 0.5,
        }],
        sidechains: vec![Sidechain {
            from: 1,
            to: 2,
            tap: Tap::Post,
        }],
        sources: vec![
            Assignment {
                source: SourceKey::Channel(5),
                destination: 1,
            },
            Assignment {
                source: SourceKey::AudioTrack(11),
                destination: 2,
            },
        ],
        next_track_id: 3,
        next_insert_id: 3,
        next_send_id: 2,
    }
}

fn sets(channels: &[u32], tracks: &[u32]) -> (HashSet<u32>, HashSet<u32>) {
    (
        channels.iter().copied().collect(),
        tracks.iter().copied().collect(),
    )
}

fn check(m: &Mixer) -> Result<(), MixerError> {
    let (c, t) = sets(&[5, 3], &[10, 11]);
    m.validate(&c, &t)
}

#[test]
fn a_routed_mixer_validates_and_round_trips_exactly() {
    let m = routed();
    check(&m).unwrap();
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains(r#""seed":"18446744073709551615""#), "{json}");
    assert_eq!(serde_json::from_str::<Mixer>(&json).unwrap(), m);
    let compiled = e::mixer::compile(&m.descriptors(), e::mixer::Budget::default()).unwrap();
    assert_eq!(compiled.levels.last().unwrap(), &[e::mixer::MASTER]);
    assert_eq!(m.destination(SourceKey::Channel(5)), 1);
    assert_eq!(
        m.destination(SourceKey::Channel(3)),
        0,
        "unassigned goes to the master"
    );
}

#[test]
fn sources_of_two_kinds_never_share_a_key() {
    let mut m = routed();
    m.sources = vec![
        Assignment {
            source: SourceKey::Channel(5),
            destination: 1,
        },
        Assignment {
            source: SourceKey::AudioTrack(5),
            destination: 2,
        },
    ];
    let (c, t) = sets(&[5], &[5]);
    m.validate(&c, &t).unwrap();
    assert_eq!(m.destination(SourceKey::Channel(5)), 1);
    assert_eq!(m.destination(SourceKey::AudioTrack(5)), 2);
    // The same key twice is refused, never resolved by the last one winning.
    m.sources[1].source = SourceKey::Channel(5);
    assert_eq!(
        m.validate(&c, &t),
        Err(MixerError::DuplicateSource(SourceKey::Channel(5)))
    );
}

#[test]
fn invalid_mixers_are_refused_with_their_reason() {
    type Edit = fn(&mut Mixer);
    type Expected = fn(&MixerError) -> bool;
    let cases: [(&str, Edit, Expected); 24] = [
        (
            "duplicate track",
            |m| m.tracks[2].id = 1,
            |e| matches!(e, MixerError::DuplicateTrack(1)),
        ),
        (
            "duplicate insert",
            |m| m.tracks[2].inserts[0].id = 1,
            |e| matches!(e, MixerError::DuplicateInsert(1)),
        ),
        (
            "duplicate send",
            |m| {
                let s = m.sends[0].clone();
                m.sends.push(s)
            },
            |e| matches!(e, MixerError::DuplicateSend(1)),
        ),
        (
            "cycle",
            |m| m.tracks[2].output = Some(1),
            |e| matches!(e, MixerError::Graph(_)),
        ),
        (
            "dangling output",
            |m| m.tracks[1].output = Some(9),
            |e| matches!(e, MixerError::Graph(_)),
        ),
        (
            "dangling sidechain",
            |m| m.sidechains[0].to = 9,
            |e| matches!(e, MixerError::Graph(_)),
        ),
        (
            "dangling destination",
            |m| m.sources[0].destination = 9,
            |e| matches!(e, MixerError::Graph(_)),
        ),
        (
            "master output",
            |m| m.tracks[0].output = Some(1),
            |e| matches!(e, MixerError::InvalidOutput(0)),
        ),
        (
            "no output",
            |m| m.tracks[1].output = None,
            |e| matches!(e, MixerError::InvalidOutput(1)),
        ),
        (
            "master send",
            |m| m.sends[0].from = 0,
            |e| matches!(e, MixerError::InvalidOutput(0)),
        ),
        (
            "no master",
            |m| {
                m.tracks.remove(0);
            },
            |e| matches!(e, MixerError::MissingMaster),
        ),
        (
            "level at floor",
            |m| m.tracks[1].level = Level::Db(-60.0),
            |e| matches!(e, MixerError::InvalidLevel(1)),
        ),
        (
            "level over",
            |m| m.tracks[1].level = Level::Db(6.001),
            |e| matches!(e, MixerError::InvalidLevel(1)),
        ),
        (
            "level nan",
            |m| m.tracks[1].level = Level::Db(f32::NAN),
            |e| matches!(e, MixerError::InvalidLevel(1)),
        ),
        (
            "pan",
            |m| m.tracks[1].pan = 1.001,
            |e| matches!(e, MixerError::InvalidPan(1)),
        ),
        (
            "gain",
            |m| m.sends[0].gain = f32::INFINITY,
            |e| matches!(e, MixerError::InvalidGain(1)),
        ),
        (
            "instrument insert",
            |m| m.tracks[1].inserts[0] = insert(1, "synth"),
            |e| matches!(e, MixerError::UnknownDevice(1)),
        ),
        (
            "short params",
            |m| {
                m.tracks[1].inserts[0].params.pop();
            },
            |e| matches!(e, MixerError::InvalidParams(1)),
        ),
        (
            "param range",
            |m| m.tracks[1].inserts[0].params[0] = 1e9,
            |e| matches!(e, MixerError::InvalidParams(1)),
        ),
        (
            "param nan",
            |m| m.tracks[1].inserts[0].params[0] = f32::NAN,
            |e| matches!(e, MixerError::InvalidParams(1)),
        ),
        (
            "unknown source",
            |m| m.sources[0].source = SourceKey::Channel(99),
            |e| matches!(e, MixerError::UnknownSource(SourceKey::Channel(99))),
        ),
        (
            "track counter",
            |m| m.next_track_id = 2,
            |e| matches!(e, MixerError::InvalidId("track", 2)),
        ),
        (
            "insert zero",
            |m| m.tracks[1].inserts[0].id = 0,
            |e| matches!(e, MixerError::InvalidId("insert", 0)),
        ),
        (
            "send counter",
            |m| m.next_send_id = 1,
            |e| matches!(e, MixerError::InvalidId("send", 1)),
        ),
    ];
    for (name, edit, expected) in cases {
        let mut m = routed();
        edit(&mut m);
        let err = check(&m).expect_err(name);
        assert!(expected(&err), "{name}: {err:?}");
    }
    // The accepted edges of each range.
    for edit in [
        (|m: &mut Mixer| m.tracks[1].level = Level::Db(-59.999)) as fn(&mut Mixer),
        |m| m.tracks[1].level = Level::Db(6.0),
        |m| m.tracks[1].pan = -1.0,
        |m| m.sends[0].gain = 4.0,
        |m| m.sends[0].gain = 0.0,
    ] {
        let mut m = routed();
        edit(&mut m);
        check(&m).unwrap();
    }
    // Modes say what the graph is.
    let flat = Mixer {
        mode: Mode::Routed,
        ..Mixer::default()
    };
    assert_eq!(check(&flat), Err(MixerError::IdentityRouted));
    let mut panned = Mixer::default();
    panned.tracks[0].pan = 0.5;
    assert_eq!(check(&panned), Err(MixerError::NotIdentity));
    let mut assigned = Mixer::default();
    assigned.sources.push(Assignment {
        source: SourceKey::Channel(5),
        destination: 0,
    });
    assert_eq!(check(&assigned), Err(MixerError::NotIdentity));
    let mut named = Mixer::default();
    named.tracks[0].name = "Main".into();
    check(&named).unwrap();
}

#[test]
fn id_counters_never_wrap_or_reset() {
    let mut m = Mixer::default();
    assert_eq!(m.take_track_id(), Ok(1));
    m.next_insert_id = u32::MAX - 1;
    assert_eq!(m.take_insert_id(), Ok(u32::MAX - 1));
    assert_eq!(m.take_insert_id(), Err(MixerError::IdsUsedUp("insert")));
    assert_eq!(m.next_insert_id, u32::MAX);
    m.next_send_id = 0;
    assert_eq!(m.take_send_id(), Ok(1), "0 is reserved");
    let zero = Mixer {
        next_track_id: 0,
        ..Mixer::default()
    };
    assert!(matches!(check(&zero), Err(MixerError::InvalidId(_, 0))));
}

// ---- Bounds, checked while reading ----

fn mixer_json(tracks: &str, extra: &str) -> String {
    format!(
        r#"{{"mode":"routed","tracks":[{tracks}],"nextTrackId":9,"nextInsertId":99999,"nextSendId":99999{extra}}}"#
    )
}

fn list(n: usize, item: impl Fn(usize) -> String) -> String {
    (0..n).map(item).collect::<Vec<_>>().join(",")
}

fn read(json: &str) -> Result<Mixer, String> {
    read_with_limits(json, Limits::DEFAULT).map_err(|e| e.to_string())
}

fn refused(json: &str, why: &str) {
    let err = read(json).unwrap_err();
    assert!(err.contains(why), "{err}");
}

#[test]
fn every_list_is_bounded_at_its_exact_limit_while_read() {
    let l = Limits::DEFAULT;
    assert_eq!(
        (
            l.tracks,
            l.inserts,
            l.sends,
            l.sidechains,
            l.assignments,
            l.entries,
            l.bytes
        ),
        (16_384, 16_384, 65_536, 65_536, 65_536, 131_072, 32 << 20)
    );
    let tracks = |n| list(n, |i| format!(r#"{{"id":{i}}}"#));
    assert_eq!(
        read(&mixer_json(&tracks(16_384), "")).unwrap().tracks.len(),
        16_384
    );
    refused(&mixer_json(&tracks(16_385), ""), "track limit");

    // No track has its own insert ceiling: every insert sits on the master here.
    let inserts = |n| {
        let list = list(n, |i| {
            format!(
                r#"{{"id":{},"device":"fattener","params":[0,0],"seed":"{i}"}}"#,
                i + 1
            )
        });
        format!(r#"{{"id":0,"inserts":[{list}]}}"#)
    };
    assert_eq!(
        read(&mixer_json(&inserts(16_384), "")).unwrap().tracks[0]
            .inserts
            .len(),
        16_384
    );
    refused(&mixer_json(&inserts(16_385), ""), "insert limit");

    let one = r#"{"id":0}"#;
    let sends = |n| {
        list(n, |i| {
            format!(r#"{{"id":{},"from":1,"to":0,"gain":0}}"#, i + 1)
        })
    };
    let sidechains = |n| list(n, |_| r#"{"from":1,"to":1}"#.to_string());
    let sources = |n| {
        list(n, |i| {
            format!(r#"{{"source":{{"channel":{i}}},"destination":0}}"#)
        })
    };
    for (key, items, why) in [
        ("sends", &sends as &dyn Fn(usize) -> String, "send limit"),
        ("sidechains", &sidechains, "sidechain limit"),
        ("sources", &sources, "assignment limit"),
    ] {
        read(&mixer_json(
            one,
            &format!(r#","{key}":[{}]"#, items(65_536)),
        ))
        .unwrap();
        refused(
            &mixer_json(one, &format!(r#","{key}":[{}]"#, items(65_537))),
            why,
        );
    }

    // All entries together: 1 track + 65 536 sends + 65 535 sources is exactly the limit.
    let both = |n| format!(r#","sends":[{}],"sources":[{}]"#, sends(65_536), sources(n));
    read(&mixer_json(one, &both(65_535))).unwrap();
    refused(&mixer_json(one, &both(65_536)), "entry limit");

    // Insert parameters count as entries too, and no list is longer than the longest effect's.
    let eq = |n: usize| {
        let params = list(n, |_| "0".into());
        format!(r#"{{"id":0,"inserts":[{{"id":1,"device":"eq","params":[{params}],"seed":"1"}}]}}"#)
    };
    let longest = grooviq_dsp::param_spec("eq").unwrap().len();
    assert_eq!(
        read(&mixer_json(&eq(longest), "")).unwrap().tracks[0].inserts[0]
            .params
            .len(),
        longest
    );
    refused(
        &mixer_json(&eq(longest + 1), ""),
        "too many insert parameters",
    );
}

#[test]
fn strings_seeds_and_bytes_are_bounded_exactly() {
    let named = |name: &str| mixer_json(&format!(r#"{{"id":0,"name":"{name}"}}"#), "");
    read(&named(&"é".repeat(128))).unwrap();
    refused(&named(&format!("{}a", "é".repeat(128))), "too long");
    let device = |id: &str| {
        mixer_json(
            &format!(r#"{{"id":0,"inserts":[{{"id":1,"device":"{id}","params":[],"seed":"1"}}]}}"#),
            "",
        )
    };
    read(&device(&"d".repeat(32))).unwrap();
    refused(&device(&"d".repeat(33)), "too long");
    let preset = |label: &str| {
        mixer_json(
            &format!(
                r#"{{"id":0,"inserts":[{{"id":1,"device":"utility","params":[],"preset":"{label}","seed":"1"}}]}}"#
            ),
            "",
        )
    };
    read(&preset(&"p".repeat(256))).unwrap();
    refused(&preset(&"p".repeat(257)), "too long");

    let seed = |s: &str| {
        mixer_json(
            &format!(
                r#"{{"id":0,"inserts":[{{"id":1,"device":"utility","params":[],"seed":{s}}}]}}"#
            ),
            "",
        )
    };
    let read_seed = |s: &str| read(&seed(s)).map(|m| m.tracks[0].inserts[0].seed);
    assert_eq!(read_seed(r#""18446744073709551615""#), Ok(u64::MAX));
    assert_eq!(read_seed(r#""0""#), Ok(0));
    for bad in [
        r#""18446744073709551616""#,
        r#""007""#,
        r#""""#,
        r#""+1""#,
        "12",
        r#""1e3""#,
    ] {
        assert!(read_seed(bad).is_err(), "{bad}");
    }

    // Kept bytes: each entry's size plus its strings.
    let exact = std::mem::size_of::<Track>() + 2;
    let json = named("ab");
    let limits = |bytes| Limits {
        bytes,
        ..Limits::DEFAULT
    };
    read_with_limits(&json, limits(exact)).unwrap();
    let err = read_with_limits(&json, limits(exact - 1)).unwrap_err();
    assert!(err.to_string().contains("byte limit"), "{err}");
    let m = read_with_limits(&json, limits(exact)).unwrap();
    let (c, t) = sets(&[], &[]);
    let mut legacy = m.clone();
    legacy.mode = Mode::LegacyDirect;
    legacy.next_track_id = 1;
    legacy.validate_with(&c, &t, limits(exact)).unwrap();
    assert_eq!(
        legacy.validate_with(&c, &t, limits(exact - 1)),
        Err(MixerError::TooBig("bytes"))
    );
}

#[test]
fn a_mixer_built_in_memory_is_held_to_the_same_bounds() {
    let mut m = routed();
    for id in 3..16_384 {
        m.tracks.push(track(id, Some(0), vec![]));
    }
    m.next_track_id = 16_384;
    assert_eq!(m.tracks.len(), 16_384);
    check(&m).unwrap();
    m.tracks.push(track(16_384, Some(0), vec![]));
    m.next_track_id = 16_385;
    assert_eq!(check(&m), Err(MixerError::TooBig("tracks")));
}

// ---- Unsupported controls are refused wherever the project would play or be written ----

/// The v3 fixture with [`routed`], as a version 4 file: readable, valid, unsupported.
fn routed_file(dir: &Path) -> (std::path::PathBuf, Loaded) {
    let legacy = open_bytes(dir, "base.grooviq", &v3());
    let mut project = legacy.project.clone();
    project.mixer = routed();
    project.validate_mixer().unwrap();
    let mut json = serde_json::to_value(&project).unwrap();
    let doc = json.as_object_mut().unwrap();
    doc.insert("format".into(), "grooviq-project".into());
    doc.insert("version".into(), 4.into());
    let path = dir.join("routed.grooviq");
    std::fs::write(
        &path,
        zip_of(
            &json.to_string(),
            &[
                ("samples/1-hit.wav", wav(4000, 50, 600)),
                ("samples/2-tone.wav", wav(4800, 300, 50)),
            ],
        ),
    )
    .unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(
        loaded.project.mixer,
        routed(),
        "read strictly, for inspection"
    );
    (path, loaded)
}

#[test]
fn compiling_a_bypassed_insert_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let (_, loaded) = routed_file(d.path());
    let decoded = decode_samples(&loaded).samples;
    let p = &loaded.project;
    assert_eq!(
        engine_state(p, &decoded),
        Err(MixerError::UnsupportedBypass(2))
    );
    assert_eq!(
        compile_channels(p, &decoded),
        Err(MixerError::UnsupportedBypass(2))
    );
    assert!(compile_song(p, &decoded, None, 1_000_000).is_err());
    // The same project with the legacy mixer compiles: the guard is the only refusal.
    let mut legacy = p.clone();
    legacy.mixer = Mixer::default();
    assert!(engine_state(&legacy, &decoded).is_ok());
    assert!(compile_channels(&legacy, &decoded).is_ok());
    assert!(compile_song(&legacy, &decoded, None, 1_000_000).is_ok());
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn writing_a_bypassed_insert_is_refused_before_any_file_changes() {
    let d = tempfile::tempdir().unwrap();
    let (_, loaded) = routed_file(d.path());
    let out = d.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let dest = out.join("song.grooviq");
    let mut legacy = loaded.project.clone();
    legacy.mixer = Mixer::default();
    save(&legacy, &loaded.samples, &dest).unwrap();
    let before = std::fs::read(&dest).unwrap();
    let names = listing(&out);

    assert!(validate_save(&loaded.project, &loaded.samples).is_err());
    assert!(save(&loaded.project, &loaded.samples, &dest).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), before);
    assert_eq!(listing(&out), names, "no temp file");

    let root = d.path().join("recovery");
    let (recovery, _) = RecoveryDir::start(&root).unwrap();
    let names = listing(&root);
    assert!(
        recovery
            .write_autosave(&loaded.project, &loaded.samples, None)
            .is_err()
    );
    assert_eq!(listing(&root), names, "no autosave file");
}

#[test]
fn opening_a_bypassed_insert_changes_nothing_in_the_session() {
    let d = tempfile::tempdir().unwrap();
    let (path, routed) = routed_file(d.path());
    let (controller, mut processor) = e::engine(e::EngineConfig {
        sample_rate: 48000.0,
    });
    let controller = Arc::new(Mutex::new(controller));
    let mut s = Session::new(controller.clone());
    let legacy = open_bytes(d.path(), "legacy.grooviq", &v2());
    let decoded = decode_samples(&legacy);
    s.open(&d.path().join("legacy.grooviq"), legacy, decoded)
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    let now = Instant::now();
    s.add_synth_channel("Init", now).unwrap();
    s.set_recording(true).unwrap();
    controller.lock().unwrap().send(e::Command::Play).unwrap();

    let view = s.view();
    assert_eq!((view.dirty, view.recording), (true, true));
    let project = s.project().clone();
    let state = controller.lock().unwrap().state().clone();
    let generation = s.generation();

    type Attempt = fn(&mut Session, &Path, Loaded) -> Result<(), SessionError>;
    let attempts: [Attempt; 2] = [
        |s, path, loaded| {
            let decoded = decode_samples(&loaded);
            s.open(path, loaded, decoded).map(|_| ())
        },
        |s, path, loaded| {
            let decoded = decode_samples(&loaded);
            s.open_recovered(Some(path), loaded, decoded, Instant::now())
                .map(|_| ())
        },
    ];
    for attempt in attempts {
        let err = attempt(&mut s, &path, reloaded(&routed)).unwrap_err();
        assert!(matches!(err, SessionError::InvalidValue(_)), "{err:?}");
        assert_eq!(s.view(), view);
        assert_eq!(s.project(), &project);
        assert_eq!(s.generation(), generation);
        assert_eq!(s.path(), Some(d.path().join("legacy.grooviq").as_path()));
        assert_eq!(controller.lock().unwrap().state(), &state);
    }
    // The session still plays and exports its own project.
    assert!(s.checked_export_state().is_ok());
}
