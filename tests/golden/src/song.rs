//! Generated M3 arrangement, compiled and exported exclusively by production APIs.
use grooviq_engine as engine;
use grooviq_project::{session::Session, *};
use std::{
    collections::HashMap,
    io::Cursor,
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
pub mod storage;
pub const FRAMES: usize = 8_640_000;
pub const END_TICK: u64 = 345_600;

pub fn fixture() -> (Project, HashMap<u32, SampleFile>) {
    let mut p = Project::new();
    p.name = "M3 original — clockwork garden".into();
    p.bpm = 120.0;
    p.play_mode = PlayMode::Song;
    p.key = Some(ProjectKey {
        root: 0,
        scale: "minor".into(),
    });
    for id in [1, 2] {
        p.channels.push(serde_json::from_value(serde_json::json!({"id":id,"name":format!("Garden {id}"),"color":id,"instrument":"synth"})).unwrap());
    }
    p.patterns[0].name = "Interlocking seeds".into();
    for (i, (channel, tick, length, key, slide)) in [
        (1, 0, 960, 48, false),
        (1, 240, 120, 48, false),
        (1, 480, 240, 55, true),
        (2, 120, 600, 60, false),
        (2, 1200, 480, 64, false),
    ]
    .into_iter()
    .enumerate()
    {
        p.patterns[0].notes.push(Note {
            id: i as u32 + 1,
            channel,
            tick,
            length,
            key,
            velocity: 0.3,
            slide,
        });
    }
    p.patterns[0].next_note_id = 6;
    let mut other = p.patterns[0].clone();
    other.id = 3;
    other.name = "Second growth".into();
    for n in &mut other.notes {
        n.key += 7;
    }
    p.patterns.push(other);
    for id in [10, 11, 12] {
        p.arrangement.tracks.push(PlaylistTrack {
            id,
            name: format!("Bed {id}"),
            color: 2,
            mute: false,
        });
    }
    let mut id = 100;
    for bar in 0..90u64 {
        // Gaps, overlaps, linked repetitions and source offsets; last clip defines 180 seconds.
        if bar % 7 == 3 {
            continue;
        }
        p.arrangement.clips.push(Clip {
            id,
            track: 10,
            name: format!("Seed {bar}"),
            start_tick: bar * 3840,
            muted: false,
            content: ClipContent::Pattern {
                pattern: if bar % 3 == 0 { 3 } else { 0 },
                offset_tick: if bar % 4 == 0 { 120 } else { 0 },
                length_ticks: 3840,
            },
        });
        id += 1;
    }
    p.next_id = 1000;
    crate::song::make_unique(&mut p, 101);
    let mut files = HashMap::new();
    for (asset, rate) in [(20, 22050u32), (21, 44100), (22, 48000)] {
        let mut bytes = Cursor::new(Vec::new());
        let mut w = hound::WavWriter::new(
            &mut bytes,
            hound::WavSpec {
                channels: 2,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for n in 0..rate {
            let v = ((n % 251) as f32 - 125.0) / 1000.0;
            w.write_sample(v).unwrap();
            w.write_sample(-v * 0.5).unwrap();
        }
        w.finalize().unwrap();
        files.insert(
            asset,
            SampleFile {
                name: format!("generated-{rate}.wav"),
                bytes: Arc::new(bytes.into_inner()),
            },
        );
        p.arrangement.audio_assets.push(AudioAsset {
            id: asset,
            sample: SampleRef {
                path: format!("generated-{rate}.wav").into(),
                file: None,
            },
            source_rate: rate,
            source_frames: rate as u64,
            original_tempo: Some(120.0),
            original_key: p.key.clone(),
        });
        for j in 0..24u64 {
            p.arrangement.clips.push(Clip {
                id,
                track: 11,
                name: "Water".into(),
                start_tick: j * 13440 + 1 + (asset as u64 - 20) * 97,
                muted: false,
                content: ClipContent::Audio {
                    asset,
                    source_start: 3,
                    source_end: rate as u64 - 7,
                    gain: 0.4,
                    reverse: j % 2 == 0,
                    fade_in: 137,
                    fade_out: 259,
                },
            });
            id += 1;
        }
    }
    for j in 0..36u64 {
        p.arrangement.clips.push(Clip {
            id,
            track: 12,
            name: "Light".into(),
            start_tick: j * 9600,
            muted: false,
            content: ClipContent::Automation {
                target: AutomationTarget::ChannelPan { channel: 1 },
                priority: 2000 + j as u32,
                offset_tick: 120,
                length_ticks: 4800,
                points: vec![
                    AutomationPoint {
                        tick: 0,
                        value: -0.7,
                        mode: SegmentMode::Linear,
                    },
                    AutomationPoint {
                        tick: 2400,
                        value: 0.7,
                        mode: SegmentMode::Hold,
                    },
                ]
                .into(),
            },
        });
        id += 1;
        p.arrangement.clips.push(Clip {
            id,
            track: 12,
            name: "Shade".into(),
            start_tick: j * 9600 + 960,
            muted: false,
            content: ClipContent::Automation {
                target: AutomationTarget::ChannelPan { channel: 1 },
                priority: 3000 + j as u32,
                offset_tick: 0,
                length_ticks: 960,
                points: vec![AutomationPoint {
                    tick: 0,
                    value: 0.2,
                    mode: SegmentMode::Hold,
                }]
                .into(),
            },
        });
        id += 1;
    }
    p.next_id = 4000;
    p.validate_arrangement().unwrap();
    (p, files)
}
fn make_unique(p: &mut Project, clip: u32) {
    pattern_edit::apply(p, pattern_edit::PatternEdit::MakeUnique { clip }).unwrap();
}

pub fn open(path: &Path) -> Session {
    let loaded = load(path).unwrap();
    let decoded = decode_samples(&loaded);
    assert!(decoded.warnings.is_empty());
    let (c, _processor) = engine::engine(engine::EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut s = Session::new(Arc::new(Mutex::new(c)));
    s.open(path, loaded, decoded).unwrap();
    s
}
pub fn options() -> engine::ExportOptions {
    engine::ExportOptions {
        sample_rate: 48000,
        format: engine::WavFormat::Float32,
        loops: 1,
        song_region: None,
        max_tail_seconds: 0.0,
    }
}
pub fn export(s: &Session, path: &Path) -> engine::ExportReport {
    let (state, region) = s.export_selection(false).unwrap();
    assert!(region.is_none());
    engine::export_wav(
        state,
        &options(),
        path,
        &mut |_| {},
        &AtomicBool::new(false),
    )
    .unwrap()
}
