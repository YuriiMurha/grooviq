use grooviq_engine::{EngineConfig, engine};
use grooviq_project::{
    session::{Edit, Session},
    *,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

#[test]
fn targeted_channel_removal_preserves_session_engine_and_last_good_file() {
    let (c, _processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("automation.grooviq");
    let mut p = Project::new();
    let channel = p.take_id().unwrap();
    let mut ch =
        serde_json::from_str::<Channel>(&format!(r#"{{"id":{channel},"name":"Synth","color":0}}"#))
            .unwrap();
    ch.instrument = grooviq_engine::InstrumentKind::Synth;
    p.channels.push(ch);
    let track = p.take_id().unwrap();
    let clip = p.take_id().unwrap();
    let priority = p.take_id().unwrap();
    p.arrangement.tracks.push(PlaylistTrack {
        id: track,
        name: "T".into(),
        color: 0,
        mute: false,
    });
    p.arrangement.clips.push(Clip {
        id: clip,
        track,
        name: "Volume".into(),
        start_tick: 0,
        muted: false,
        content: ClipContent::Automation {
            target: AutomationTarget::ChannelVolume { channel },
            priority,
            offset_tick: 0,
            length_ticks: 960,
            points: vec![AutomationPoint {
                tick: 0,
                value: 0.5,
                mode: SegmentMode::Hold,
            }],
        },
    });
    save(&p, &SampleFiles::new(), &path).unwrap();
    s.open(
        &path,
        load(&path).unwrap(),
        DecodedSamples {
            samples: Default::default(),
            warnings: vec![],
            missing: Default::default(),
        },
    )
    .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let reopened_before = load(&path).unwrap().project;
    let before = s.project().clone();
    let view = s.view();
    let engine_before = c.lock().unwrap().state().clone();
    assert!(
        s.apply(Edit::RemoveChannel { channel }, Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.view(), view);
    assert_eq!(c.lock().unwrap().state(), &engine_before);
    save(s.project(), &SampleFiles::new(), &path).unwrap();
    assert_eq!(load(&path).unwrap().project, reopened_before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
