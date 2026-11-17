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
fn zip_json(path: &std::path::Path, json: &str, entries: &[(&str, &[u8])]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    z.start_file("project.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(json.as_bytes()).unwrap();
    for (name, bytes) in entries {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap();
}
fn audio_song() -> Project {
    let mut p = song();
    p.arrangement.audio_assets.push(AudioAsset {
        id: 5,
        sample: SampleRef {
            path: "original.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 48000,
        original_tempo: Some(120.0),
        original_key: Some(ProjectKey {
            root: 0,
            scale: "minor".into(),
        }),
    });
    p.arrangement.clips[0].content = ClipContent::Audio {
        asset: 5,
        source_start: 0,
        source_end: 48000,
        gain: 1.0,
        reverse: false,
        fade_in: 0,
        fade_out: 0,
    };
    p.next_id = 6;
    p
}
fn disk_json(p: &Project) -> String {
    let mut v = serde_json::to_value(p).unwrap();
    v["format"] = FORMAT.into();
    v["version"] = VERSION.into();
    serde_json::to_string(&v).unwrap()
}
#[test]
fn absent_asset_zip_entry_can_be_saved_after_loading() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("missing.grooviq");
    let mut p = audio_song();
    p.arrangement.audio_assets[0].sample.file = Some("samples/absent.wav".into());
    zip_json(&path, &disk_json(&p), &[]);
    let l = load(&path).unwrap();
    assert_eq!(l.project.arrangement.audio_assets[0].sample.file, None);
    assert_eq!(
        l.project.arrangement.audio_assets[0].sample.path,
        p.arrangement.audio_assets[0].sample.path
    );
    assert_eq!(l.project.arrangement.clips, p.arrangement.clips);
    validate_save(&l.project, &l.samples).unwrap();
    save(&l.project, &l.samples, &path).unwrap();
    assert_eq!(load(&path).unwrap().project, l.project);
}
#[test]
fn structural_audio_rate_and_duration_endpoints() {
    for rate in [8000, 384000] {
        for frames in [1, u64::from(rate) * 600] {
            let mut p = audio_song();
            p.arrangement.audio_assets[0].source_rate = rate;
            p.arrangement.audio_assets[0].source_frames = frames;
            if let ClipContent::Audio { source_end, .. } = &mut p.arrangement.clips[0].content {
                *source_end = frames;
            }
            assert!(p.validate_arrangement().is_ok());
        }
    }
    for (rate, frames) in [
        (7999, 48000),
        (384001, 48000),
        (48000, 0),
        (48000, 28_800_001),
    ] {
        let mut p = audio_song();
        p.arrangement.audio_assets[0].source_rate = rate;
        p.arrangement.audio_assets[0].source_frames = frames;
        assert!(
            p.validate_arrangement().is_err(),
            "rate={rate},frames={frames}"
        );
    }
}
fn auto_song() -> Project {
    let mut p: Project =
        serde_json::from_str(r#"{"channels":[{"id":10,"instrument":"sampler"}]}"#).unwrap();
    p.arrangement = song().arrangement;
    p.arrangement.clips[0].content = ClipContent::Automation {
        target: AutomationTarget::InstrumentParam {
            channel: 10,
            instrument: "sampler".into(),
            param: grooviq_dsp::sampler::PARAM_IDS[0].into(),
        },
        priority: 30,
        offset_tick: 0,
        length_ticks: 960,
        points: vec![
            AutomationPoint {
                tick: 0,
                value: grooviq_dsp::sampler::SPEC[0].2,
                mode: SegmentMode::Hold,
            },
            AutomationPoint {
                tick: 480,
                value: grooviq_dsp::sampler::SPEC[0].2,
                mode: SegmentMode::Linear,
            },
        ],
    };
    p.validate();
    p
}
fn refuses_only_change(base: &Project, invalid: &Project) {
    assert!(base.validate_arrangement().is_ok());
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("last-good.grooviq");
    save(base, &SampleFiles::new(), &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(invalid.validate_arrangement().is_err());
    assert!(validate_save(invalid, &SampleFiles::new()).is_err());
    assert!(save(invalid, &SampleFiles::new(), &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let disk = d.path().join("invalid.grooviq");
    zip_json(&disk, &disk_json(invalid), &[]);
    assert!(matches!(load(&disk), Err(LoadError::Corrupt(_))));
}
#[test]
fn isolated_audio_guards_refuse_without_destroying_last_good() {
    let base = audio_song();
    for field in [
        "end",
        "start",
        "fadeIn",
        "fadeOut",
        "gainLow",
        "gainHigh",
        "rateLow",
        "rateHigh",
        "framesZero",
        "framesLong",
        "tempoLow",
        "tempoHigh",
        "keyRoot",
        "keyScale",
        "extent",
    ] {
        let mut p = base.clone();
        match field {
            "rateLow" => p.arrangement.audio_assets[0].source_rate = 7999,
            "rateHigh" => p.arrangement.audio_assets[0].source_rate = 384001,
            "framesZero" => {
                p.arrangement.audio_assets[0].source_frames = 0;
                p.arrangement.clips.clear();
            }
            "framesLong" => p.arrangement.audio_assets[0].source_frames = 28_800_001,
            "tempoLow" => p.arrangement.audio_assets[0].original_tempo = Some(19.9),
            "tempoHigh" => p.arrangement.audio_assets[0].original_tempo = Some(400.1),
            "keyRoot" => {
                p.arrangement.audio_assets[0]
                    .original_key
                    .as_mut()
                    .unwrap()
                    .root = 12
            }
            "keyScale" => {
                p.arrangement.audio_assets[0]
                    .original_key
                    .as_mut()
                    .unwrap()
                    .scale = "unknown".into()
            }
            "extent" => p.arrangement.clips[0].start_tick = MAX_SONG_TICKS - 1,
            _ => {
                if let ClipContent::Audio {
                    source_start,
                    source_end,
                    fade_in,
                    fade_out,
                    gain,
                    ..
                } = &mut p.arrangement.clips[0].content
                {
                    match field {
                        "end" => *source_end = 48001,
                        "start" => *source_start = 48000,
                        "fadeIn" => *fade_in = 48001,
                        "fadeOut" => *fade_out = 48001,
                        "gainLow" => *gain = -0.001,
                        "gainHigh" => *gain = 2.001,
                        _ => unreachable!(),
                    }
                }
            }
        }
        refuses_only_change(&base, &p);
    }
}
#[test]
fn isolated_automation_guards_refuse_without_destroying_last_good() {
    let base = auto_song();
    for field in ["kind", "order", "tick", "end", "collision", "target"] {
        let mut p = base.clone();
        if field == "target" {
            let mut c = p.arrangement.clips[0].clone();
            c.id = 40;
            c.start_tick = 960;
            if let ClipContent::Automation { target, points, .. } = &mut c.content {
                *target = AutomationTarget::InstrumentParam {
                    channel: 10,
                    instrument: "sampler".into(),
                    param: "start".into(),
                };
                for p in points {
                    p.value = 0.0;
                }
            }
            p.arrangement.clips.push(c);
            let mut disjoint = p.clone();
            if let ClipContent::Automation { target, .. } =
                &mut disjoint.arrangement.clips[1].content
            {
                *target = AutomationTarget::InstrumentParam {
                    channel: 10,
                    instrument: "sampler".into(),
                    param: "root".into(),
                };
            }
            assert!(disjoint.validate_arrangement().is_ok());
        } else if let ClipContent::Automation {
            target: _,
            points,
            offset_tick,
            priority,
            ..
        } = &mut p.arrangement.clips[0].content
        {
            match field {
                "kind" => p.channels[0].instrument = grooviq_engine::InstrumentKind::Synth,
                "order" => points.swap(0, 1),
                "tick" => points[1].tick = MAX_SONG_TICKS + 1,
                "end" => *offset_tick = MAX_SONG_TICKS as u32,
                "collision" => *priority = 2,
                _ => unreachable!(),
            }
        }
        refuses_only_change(&base, &p);
    }
}
#[test]
fn isolated_pattern_and_loop_guards() {
    let base = song();
    let mut bad = base.clone();
    if let ClipContent::Pattern { pattern, .. } = &mut bad.arrangement.clips[0].content {
        *pattern = 99;
    }
    refuses_only_change(&base, &bad);
    for (start, end) in [(960, 960), (961, 960), (0, MAX_SONG_TICKS + 1)] {
        let mut bad = base.clone();
        bad.arrangement.loop_region = Some(LoopRegion {
            start_tick: start,
            end_tick: end,
            enabled: true,
        });
        refuses_only_change(&base, &bad);
    }
}
#[test]
fn unsafe_zip_entry_is_missing_and_old_version_arrangement_is_reset() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("unsafe.grooviq");
    for name in ["outside.wav", "samples/../outside.wav", "samples/sub/x.wav"] {
        let mut p = audio_song();
        p.arrangement.audio_assets[0].sample.file = Some(name.into());
        zip_json(&path, &disk_json(&p), &[(name, &[1, 2, 3])]);
        let l = load(&path).unwrap();
        assert!(!l.samples.contains_key(&5));
        assert_eq!(l.project.arrangement.audio_assets[0].sample.file, None);
    }
    let mut v = serde_json::to_value(audio_song()).unwrap();
    v["format"] = FORMAT.into();
    v["version"] = 2.into();
    zip_json(&path, &v.to_string(), &[]);
    let l = load(&path).unwrap();
    assert_eq!(l.project.arrangement, Arrangement::default());
    assert_eq!(l.project.play_mode, PlayMode::Pattern);
}
struct ResourceAllocator;
thread_local! {static ALLOCATION:std::cell::Cell<Option<(usize,usize)>>=const {std::cell::Cell::new(None)};}
unsafe impl std::alloc::GlobalAlloc for ResourceAllocator {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        ALLOCATION
            .try_with(|c| {
                if let Some((n, p)) = c.get() {
                    let n = n + l.size();
                    c.set(Some((n, p.max(n))));
                }
            })
            .ok();
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        ALLOCATION
            .try_with(|c| {
                if let Some((n, peak)) = c.get() {
                    c.set(Some((n.saturating_sub(l.size()), peak)));
                }
            })
            .ok();
        unsafe { std::alloc::System.dealloc(p, l) }
    }
}
#[global_allocator]
static RESOURCE_ALLOCATOR: ResourceAllocator = ResourceAllocator;
fn automation_zip(
    path: &std::path::Path,
    clips: usize,
    points: usize,
    last: bool,
    unknown_target: bool,
) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    z.start_file(
        "project.json",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    z.write_all(br#"{"format":"grooviq-project","version":3,"channels":[{"id":10}],"arrangement":{"tracks":[{"id":2,"name":"T","color":0,"mute":false}],"clips":["#).unwrap();
    for c in 0..clips {
        if c > 0 {
            z.write_all(b",").unwrap();
        }
        write!(
            z,
            r#"{{"id":{},"track":2,"name":"A","startTick":0,"muted":false,"content":{{"#,
            100 + c
        )
        .unwrap();
        if !last {
            z.write_all(br#""type":"automation","#).unwrap();
        }
        z.write_all(br#""target":{"channel":10,"#).unwrap();
        if unknown_target {
            z.write_all(br#""unknown":["#).unwrap();
            for n in 0..100_000 {
                if n > 0 {
                    z.write_all(b",").unwrap();
                }
                z.write_all(br#"{"nested":"ignored"}"#).unwrap();
            }
            z.write_all(b"],").unwrap();
        }
        write!(z, "\"type\":\"channelVolume\"}},\"priority\":{},\"offsetTick\":0,\"lengthTicks\":960,\"points\":[",10_000+c).unwrap();
        let point = br#"{"tick":0,"value":1.0,"mode":"hold"}"#;
        if points > 0 {
            z.write_all(point).unwrap();
            let mut chunk = Vec::new();
            for _ in 0..1024 {
                chunk.push(b',');
                chunk.extend_from_slice(point);
            }
            let mut left = points - 1;
            while left >= 1024 {
                z.write_all(&chunk).unwrap();
                left -= 1024;
            }
            z.write_all(&chunk[..left * (point.len() + 1)]).unwrap();
        }
        z.write_all(b"]").unwrap();
        if last {
            z.write_all(br#", "type":"automation""#).unwrap();
        }
        z.write_all(b"}}").unwrap();
    }
    z.write_all(b"]}}").unwrap();
    z.finish().unwrap();
}
#[test]
fn point_caps_stream_through_actual_load_with_bounded_allocations() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("points.grooviq");
    for last in [false, true] {
        automation_zip(&path, 1, 100_000, last, false);
        ALLOCATION.with(|c| c.set(Some((0, 0))));
        let result = load(&path);
        let peak = ALLOCATION.with(|c| c.replace(None).unwrap().1);
        let err = result.unwrap_err();
        assert!(matches!(err, LoadError::Corrupt(_)));
        let message = err.to_string();
        assert!(message.contains("arrangement list limit"), "{message}");
        let col: usize = message
            .split("column ")
            .nth(1)
            .unwrap()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(col < 1_000_000, "late point refusal at {col}");
        assert!(peak < 8 * 1024 * 1024, "point peak {peak}");
        eprintln!("point resource last={last} peak={peak} error_column={col}");
    }
    automation_zip(&path, 61, MAX_AUTOMATION_POINTS, true, false);
    assert_eq!(load(&path).unwrap().project.arrangement.clips.len(), 61);
    automation_zip(&path, 62, MAX_AUTOMATION_POINTS, true, false);
    let error = load(&path).unwrap_err().to_string();
    assert!(error.contains("total automation point limit"), "{error}");
    automation_zip(&path, 1, 2, true, true);
    ALLOCATION.with(|c| c.set(Some((0, 0))));
    let l = load(&path).unwrap();
    let peak = ALLOCATION.with(|c| c.replace(None).unwrap().1);
    assert_eq!(l.project.arrangement.clips.len(), 1);
    assert!(peak < 8 * 1024 * 1024, "target peak {peak}");
    eprintln!("target unknown resource peak={peak}");
}
#[test]
fn absent_channel_entry_is_saveable_control() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("channel.grooviq");
    let mut p = auto_song();
    p.channels[0].sample = Some(SampleRef {
        path: "channel.wav".into(),
        file: Some("samples/absent.wav".into()),
    });
    zip_json(&path, &disk_json(&p), &[]);
    let l = load(&path).unwrap();
    assert_eq!(l.project.channels[0].sample.as_ref().unwrap().file, None);
    validate_save(&l.project, &l.samples).unwrap();
    save(&l.project, &l.samples, &path).unwrap();
    assert_eq!(load(&path).unwrap().project, l.project);
}
