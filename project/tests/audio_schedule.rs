use grooviq_engine::{
    AudioAsset as RuntimeAsset, AudioClip as RuntimeClip, AudioSchedule, MAX_SONG_TICKS,
};
use grooviq_project::{
    AudioAsset, Clip, ClipContent, PlaylistTrack, Project, SampleFiles, SampleRef,
};

#[test]
fn valid_v3_low_tempo_clip_can_build_its_audio_bank() {
    let mut project = Project::new();
    project.bpm = 20.0;
    project.next_id = 4;
    project.arrangement.tracks.push(PlaylistTrack {
        id: 1,
        name: "Audio".into(),
        color: 0,
        mute: false,
    });
    project.arrangement.audio_assets.push(AudioAsset {
        id: 2,
        sample: SampleRef {
            path: "/qa/generated.wav".into(),
            file: None,
        },
        source_rate: 8000,
        source_frames: 8000,
        original_tempo: Some(20.0),
        original_key: None,
    });
    project.arrangement.clips.push(Clip {
        id: 3,
        track: 1,
        name: "Last second".into(),
        start_tick: MAX_SONG_TICKS - 320,
        muted: false,
        content: ClipContent::Audio {
            asset: 2,
            source_start: 0,
            source_end: 8000,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        },
    });
    grooviq_project::file::validate_save(&project, &SampleFiles::new()).unwrap();
    let asset = RuntimeAsset {
        id: 2,
        source_rate: 8000,
        source_frames: 8000,
        decoded: None,
    };
    let clip = RuntimeClip {
        id: 3,
        asset: 2,
        start_tick: MAX_SONG_TICKS - 320,
        source_start: 0,
        source_end: 8000,
        gain: 1.0,
        reverse: false,
        fade_in: 0,
        fade_out: 0,
    };
    let result = AudioSchedule::build(vec![asset.clone()], vec![clip]);
    assert!(
        result.is_ok(),
        "writer-valid 20 BPM placement was refused by a tempo-independent bank: {result:?}"
    );
    let (schedule, _) = result.unwrap();
    assert_eq!(
        schedule.end_frame(20.0, 48000.0),
        grooviq_engine::ticks_to_frames(MAX_SONG_TICKS as f64, 20.0, 48000.0)
    );
    project.bpm = 400.0;
    assert!(
        grooviq_project::file::validate_save(&project, &SampleFiles::new()).is_err(),
        "a later tempo change must still be refused at the timeline limit"
    );
    assert!(
        AudioSchedule::build(
            vec![asset],
            vec![RuntimeClip {
                start_tick: clip.start_tick + 1,
                ..clip
            }]
        )
        .is_err(),
        "a placement impossible even at the minimum tempo must be refused"
    );
}
