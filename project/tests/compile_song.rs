use grooviq_project::{compile_song::compile_song, *};
use std::collections::HashMap;

fn song() -> Project {
    let mut p = Project::new();
    p.bpm = 120.0;
    p.arrangement.tracks.push(PlaylistTrack {
        id: 10,
        name: "T".into(),
        color: 0,
        mute: false,
    });
    p.arrangement.audio_assets.push(AudioAsset {
        id: 11,
        sample: SampleRef {
            path: "missing.wav".into(),
            file: None,
        },
        source_rate: 48_000,
        source_frames: 48_001,
        original_tempo: None,
        original_key: None,
    });
    p.arrangement.clips.push(Clip {
        id: 12,
        track: 10,
        name: "A".into(),
        start_tick: 1,
        muted: false,
        content: ClipContent::Audio {
            asset: 11,
            source_start: 0,
            source_end: 48_001,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        },
    });
    p.next_id = 13;
    p
}
#[test]
fn audio_horizon_has_exact_frames_without_tick_duration_rounding() {
    let p = song();
    let s = compile_song(&p, &HashMap::new(), None, 10).unwrap();
    assert_eq!(s.end_tick, 1922);
    assert_eq!(s.end_frame, 48_026);
    let reused = compile_song(&p, &HashMap::new(), Some(&s.audio), 10).unwrap();
    assert_eq!(s.audio, reused.audio);
    let mut muted = p.clone();
    muted.arrangement.tracks[0].mute = true;
    let empty = compile_song(&muted, &HashMap::new(), None, 10).unwrap();
    assert_eq!((empty.end_tick, empty.end_frame), (1922, 48_026));
    assert!(empty.audio.is_empty());
}
#[test]
fn actual_tempo_horizon_refuses_instead_of_truncating() {
    let mut p = song();
    p.arrangement.clips[0].start_tick = MAX_SONG_TICKS - 1000;
    assert!(compile_song(&p, &HashMap::new(), None, 10).is_err());
}

#[test]
fn expansion_limit_is_inclusive_and_track_mute_removes_notes() {
    let mut p = song();
    p.arrangement.clips.clear();
    let ch: Channel =
        serde_json::from_str(r#"{"id":1,"name":"Synth","color":0,"instrument":"synth"}"#).unwrap();
    p.channels.push(ch);
    p.patterns[0].notes.push(Note {
        id: 1,
        channel: 1,
        tick: 0,
        length: 240,
        key: 60,
        velocity: 0.8,
        slide: false,
    });
    p.arrangement.clips.push(Clip {
        id: 12,
        track: 10,
        name: "P".into(),
        start_tick: 0,
        muted: false,
        content: ClipContent::Pattern {
            pattern: 0,
            offset_tick: 0,
            length_ticks: 7680,
        },
    });
    let s = compile_song(&p, &HashMap::new(), None, 2).unwrap();
    assert_eq!(s.arrangement.notes.len(), 2);
    assert_eq!(s.arrangement.notes[1].tick, 3840);
    assert!(compile_song(&p, &HashMap::new(), None, 1).is_err());
    p.arrangement.tracks[0].mute = true;
    assert!(
        compile_song(&p, &HashMap::new(), None, 0)
            .unwrap()
            .arrangement
            .notes
            .is_empty()
    );
}

#[test]
fn automation_only_song_has_musical_and_frame_horizon() {
    let mut p = song();
    p.channels.push(
        serde_json::from_str(r#"{"id":1,"name":"Synth","color":0,"instrument":"synth"}"#).unwrap(),
    );
    p.arrangement.clips[0].content = ClipContent::Automation {
        target: AutomationTarget::ChannelPan { channel: 1 },
        priority: 13,
        offset_tick: 0,
        length_ticks: 960,
        points: vec![AutomationPoint {
            tick: 0,
            value: 0.5,
            mode: SegmentMode::Hold,
        }],
    };
    p.next_id = 14;
    let s = compile_song(&p, &HashMap::new(), None, 0).unwrap();
    assert_eq!(s.end_tick, 961);
    assert_eq!(s.end_frame, 24_025);
}
