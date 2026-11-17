use grooviq_project::*;
use std::io::Write;
#[test]
fn v3_arrangement_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.grooviq");
    let json = r#"{"format":"grooviq-project","version":3,"playMode":"song","arrangement":{"tracks":[{"id":2,"name":"Track","color":0,"mute":false}],"clips":[],"audioAssets":[],"loopRegion":{"startTick":0,"endTick":960,"enabled":true}}}"#;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("project.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(json.as_bytes()).unwrap();
    zip.finish().unwrap();
    let loaded = load(&path).unwrap();
    save(&loaded.project, &loaded.samples, &path).unwrap();
    assert_eq!(load(&path).unwrap().project, loaded.project);
}
fn song() -> Project {
    let mut p = Project::new();
    p.play_mode = PlayMode::Song;
    p.arrangement.tracks.push(PlaylistTrack {
        id: 2,
        name: "Track".into(),
        color: 1,
        mute: false,
    });
    p.arrangement.clips.push(Clip {
        id: 3,
        track: 2,
        name: "Pattern".into(),
        start_tick: 0,
        muted: false,
        content: ClipContent::Pattern {
            pattern: 0,
            offset_tick: 0,
            length_ticks: 960,
        },
    });
    p.next_id = 4;
    p
}
#[test]
fn invalid_bounds_and_identity_cannot_replace_good_file() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("s.grooviq");
    let p = song();
    save(&p, &SampleFiles::new(), &path).unwrap();
    let good = std::fs::read(&path).unwrap();
    for kind in 0..4 {
        let mut bad = p.clone();
        match kind {
            0 => bad.arrangement.clips[0].start_tick = u64::MAX,
            1 => bad.arrangement.clips[0].id = 0,
            2 => bad.arrangement.clips[0].track = 99,
            _ => {
                bad.arrangement.clips[0].content = ClipContent::Pattern {
                    pattern: 0,
                    offset_tick: 0,
                    length_ticks: 0,
                }
            }
        };
        assert!(save(&bad, &SampleFiles::new(), &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), good);
    }
    assert!(save_with(&p, &SampleFiles::new(), &path, true).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), good);
}
#[test]
fn allocator_skips_all_domains_and_never_rewinds() {
    let mut p = song();
    p.next_id = 0;
    assert_eq!(p.take_id(), Some(1));
    assert_eq!(p.take_id(), Some(4));
    p.next_id = u32::MAX;
    assert_eq!(p.take_id(), None);
}
#[test]
fn original_asset_bytes_deduplicate_and_missing_clips_survive() {
    let mut p = song();
    p.arrangement.audio_assets.push(AudioAsset {
        id: 5,
        sample: SampleRef {
            path: "original.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 8,
        original_tempo: Some(120.0),
        original_key: Some(ProjectKey {
            root: 2,
            scale: "minor".into(),
        }),
    });
    let mut other = p.arrangement.audio_assets[0].clone();
    other.id = 6;
    p.arrangement.audio_assets.push(other);
    p.arrangement.clips.push(Clip {
        id: 7,
        track: 2,
        name: "Audio".into(),
        start_tick: 960,
        muted: false,
        content: ClipContent::Audio {
            asset: 5,
            source_start: 1,
            source_end: 7,
            gain: 0.5,
            reverse: true,
            fade_in: 2,
            fade_out: 3,
        },
    });
    for (start, end, gain, fade) in [
        (7, 1, 1.0, 0),
        (0, u64::MAX, 1.0, 0),
        (0, 8, f32::NAN, 0),
        (0, 8, 1.0, 9),
    ] {
        let mut invalid = p.clone();
        invalid.arrangement.clips[1].content = ClipContent::Audio {
            asset: 5,
            source_start: start,
            source_end: end,
            gain,
            reverse: false,
            fade_in: fade,
            fade_out: 0,
        };
        assert!(invalid.validate_arrangement().is_err());
    }
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("a.grooviq");
    let bytes = std::sync::Arc::new(vec![1, 2, 3, 4]);
    let mut files = SampleFiles::new();
    for id in [5, 6] {
        files.insert(
            id,
            SampleFile {
                name: "../../a.wav".into(),
                bytes: bytes.clone(),
            },
        );
    }
    save(&p, &files, &path).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(*loaded.samples[&5].bytes, *bytes);
    assert!(std::sync::Arc::ptr_eq(
        &loaded.samples[&5].bytes,
        &loaded.samples[&6].bytes
    ));
    let zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(zip.len(), 2);
    save(&p, &SampleFiles::new(), &path).unwrap();
    let missing = load(&path).unwrap();
    assert_eq!(missing.project.arrangement, p.arrangement);
    assert_eq!(
        missing
            .warnings
            .iter()
            .filter(|w| w.contains("missing"))
            .count(),
        2
    );
}
#[test]
fn native_automation_range_and_instrument_are_checked() {
    let mut p = song();
    p.arrangement.clips[0].content = ClipContent::Automation {
        priority: 30,
        target: AutomationTarget::ChannelVolume { channel: 99 },
        offset_tick: 0,
        length_ticks: 960,
        points: vec![AutomationPoint {
            tick: 0,
            value: f32::NAN,
            mode: SegmentMode::Hold,
        }],
    };
    assert!(p.validate_arrangement().is_err());
}
#[test]
fn v2_independent_fixture_preserves_many_patterns_and_source() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("v2.grooviq");
    let patterns:Vec<_>=(0..600).map(|id|serde_json::json!({"id":id,"name":format!("P{id}"),"steps":16,"swing":0.3,"notes":[{"id":42,"channel":800,"tick":0,"length":800,"key":48,"velocity":0.7,"slide":false},{"id":71,"channel":800,"tick":120,"length":240,"key":48,"velocity":0.4,"slide":true}],"nextNoteId":73})).collect();
    let json = serde_json::json!({"format":"grooviq-project","version":2,"channels":[{"id":800,"name":"Shared","sample":{"path":"original.wav","file":"samples/1-original.wav"},"sampler":{"attack":0.1}}],"patterns":patterns,"nextId":900,"key":{"root":3,"scale":"minor"}});
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("project.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(&json).unwrap()).unwrap();
    zip.start_file(
        "samples/1-original.wav",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(&[9, 8, 7, 6]).unwrap();
    zip.finish().unwrap();
    let original = std::fs::read(&path).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.project.patterns.len(), 600);
    assert_eq!(*loaded.samples[&800].bytes, vec![9, 8, 7, 6]);
    assert_eq!(loaded.project.patterns[599].notes[1].id, 71);
    assert!(loaded.project.patterns[599].notes[1].slide);
    assert_eq!(loaded.project.play_mode, PlayMode::Pattern);
    assert_eq!(loaded.project.arrangement, Arrangement::default());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(loaded.project.patterns[599].next_note_id, 73);
    assert!(save_with(&loaded.project, &loaded.samples, &path, true).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let dest = d.path().join("v3.grooviq");
    save(&loaded.project, &loaded.samples, &dest).unwrap();
    assert_eq!(load(&dest).unwrap().project, loaded.project);
}
#[test]
fn bounded_tracks_refused_during_read() {
    let json = serde_json::json!({"arrangement":{"tracks":(0..257).map(|id|serde_json::json!({"id":id,"name":"T","color":0,"mute":false})).collect::<Vec<_>>()}});
    assert!(serde_json::from_value::<Project>(json).is_err());
}
#[test]
fn all_automation_targets_and_modes_roundtrip() {
    let mut p = song();
    p.channels.push(Channel {
        id: 10,
        name: "Synth".into(),
        color: 0,
        instrument: grooviq_engine::InstrumentKind::Synth,
        sample: None,
        sampler: Default::default(),
        synth: Default::default(),
        preset: None,
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
    });
    let targets = [
        AutomationTarget::ChannelVolume { channel: 10 },
        AutomationTarget::ChannelPan { channel: 10 },
        AutomationTarget::InstrumentParam {
            channel: 10,
            instrument: "synth".into(),
            param: grooviq_dsp::synth::PARAM_IDS[0].into(),
        },
    ];
    for (i, target) in targets.into_iter().enumerate() {
        let value = if i == 2 {
            grooviq_dsp::synth::SPEC[0].2
        } else {
            0.5
        };
        p.arrangement.clips.push(Clip {
            id: 20 + i as u32,
            track: 2,
            name: "Automation".into(),
            start_tick: 0,
            muted: false,
            content: ClipContent::Automation {
                priority: 40 + i as u32,
                target,
                offset_tick: 0,
                length_ticks: 960,
                points: vec![
                    AutomationPoint {
                        tick: 0,
                        value,
                        mode: SegmentMode::Hold,
                    },
                    AutomationPoint {
                        tick: 480,
                        value,
                        mode: SegmentMode::Linear,
                    },
                ],
            },
        });
    }
    p.arrangement.audio_assets.push(AudioAsset {
        id: 5,
        sample: SampleRef {
            path: "missing.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 8,
        original_tempo: None,
        original_key: None,
    });
    p.arrangement.clips.push(Clip {
        id: 6,
        track: 2,
        name: "Audio".into(),
        start_tick: 0,
        muted: false,
        content: ClipContent::Audio {
            asset: 5,
            source_start: 0,
            source_end: 8,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        },
    });
    p.validate();
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("automation.grooviq");
    save(&p, &SampleFiles::new(), &path).unwrap();
    assert_eq!(load(&path).unwrap().project, p);
    if let ClipContent::Automation { points, .. } = &mut p.arrangement.clips[1].content {
        points[0].value = 3.0;
    }
    assert!(save(&p, &SampleFiles::new(), &path).is_err());
}
#[test]
fn sliced_priority_is_saved_and_siblings_must_be_disjoint() {
    let mut p = song();
    p.channels.push(Channel {
        id: 10,
        name: "C".into(),
        color: 0,
        instrument: grooviq_engine::InstrumentKind::Sampler,
        sample: None,
        sampler: Default::default(),
        synth: Default::default(),
        preset: None,
        volume: 1.0,
        pan: 0.0,
        mute: false,
        solo: false,
        choke: 0,
    });
    p.arrangement.clips[0].content = ClipContent::Automation {
        target: AutomationTarget::ChannelVolume { channel: 10 },
        priority: 30,
        offset_tick: 0,
        length_ticks: 960,
        points: vec![],
    };
    let mut sibling = p.arrangement.clips[0].clone();
    sibling.id = 31;
    sibling.start_tick = 960;
    p.arrangement.clips.push(sibling);
    p.validate();
    assert_eq!(p.next_id, 32);
    validate_save(&p, &SampleFiles::new()).unwrap();
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("priority.grooviq");
    save(&p, &SampleFiles::new(), &path).unwrap();
    assert_eq!(load(&path).unwrap().project, p);
    p.arrangement.clips[1].start_tick = 959;
    assert!(validate_save(&p, &SampleFiles::new()).is_err());
    p.arrangement.clips[1].start_tick = 960;
    p.next_id = 30;
    assert_eq!(p.take_id(), Some(32));
}
#[test]
fn future_version_wins_before_any_musical_schema_is_inspected() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("future.grooviq");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    z.start_file("project.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(br#"{"channels":[[true]],"patterns":[[false]],"arrangement":false,"format":"grooviq-project","version":4}"#).unwrap();
    z.finish().unwrap();
    assert!(matches!(
        load(&path),
        Err(LoadError::NewerVersion {
            found: 4,
            supported: 3
        })
    ));
}
