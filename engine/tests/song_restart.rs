use grooviq_engine::*;
use std::sync::Arc;

const CFG: EngineConfig = EngineConfig {
    sample_rate: 48000.0,
};

fn sustained_channel(len: usize) -> ChannelSetup {
    let mut params: Vec<_> = grooviq_dsp::sampler::SPEC.iter().map(|p| p.2).collect();
    params[7] = 0.0;
    params[11] = 0.0;
    params[14] = 0.0;
    ChannelSetup::sampler(
        ChannelId(1),
        Some(Arc::new(SampleBuf {
            rate: 48000.0,
            l: vec![0.2; len],
            r: vec![0.2; len],
        })),
        params,
        ChannelMix::default(),
    )
}

fn note(tick: u64, length: u64) -> SongNote {
    SongNote {
        tick,
        length,
        channel: ChannelId(1),
        key: 60,
        velocity: 1.0,
        slide: false,
        clip_id: 1,
    }
}

fn song_state(notes: Vec<SongNote>, end_tick: u64) -> EngineState {
    EngineState {
        bpm: 120.0,
        channels: vec![sustained_channel(400_000)],
        playback_mode: PlaybackMode::Song,
        song: SongData {
            arrangement: ArrangementData {
                length: end_tick,
                notes,
            },
            end_tick,
            end_frame: 0,
            frame_rate: 48000.0,
            audio: AudioSchedule::default(),
            automation: Default::default(),
        },
        ..EngineState::default()
    }
}

fn level(out: &[f32], frame: usize) -> f32 {
    out[frame * 2].abs()
}

#[test]
fn replay_at_a_block_aligned_song_end_releases_the_previous_instance() {
    let state = song_state(vec![note(0, 1920)], 1920);
    for callback in [1, 63, 64, 127, 256, 511] {
        for play_at in [48000u64, 48064, 60000] {
            let events = [(0, Command::Play), (play_at, Command::Play)];
            let out = render_offline_with(CFG, state.clone(), &events, 150000, callback);
            let mid = play_at as usize + 24000;
            assert!(
                (level(&out, mid) - 0.2).abs() < 0.00001,
                "callback={callback}, play_at={play_at}, level={}",
                level(&out, mid)
            );
            assert!(level(&out, play_at as usize + 49000) < 0.00001);
        }
    }
}

#[test]
fn loading_a_pattern_keeps_recording_armed_and_records_the_next_note() {
    let mut state = song_state(vec![], 960);
    state.playback_mode = PlaybackMode::Pattern;
    let (mut controller, mut processor) = engine_with(CFG, state.clone());
    controller.send(Command::SetRecording { on: true }).unwrap();
    processor.process(&mut [0.0; 128], 2);
    controller.load_state(state).unwrap();
    assert!(controller.recording_armed());
    processor.process(&mut [0.0; 128], 2);
    controller.send(Command::Play).unwrap();
    let id = NoteId::live();
    controller
        .send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            on: true,
            id,
        })
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    controller
        .send(Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 1.0,
            on: false,
            id,
        })
        .unwrap();
    processor.process(&mut [0.0; 128], 2);
    let mut recorded = Vec::new();
    controller.take_recorded(&mut recorded);
    assert!(
        recorded
            .iter()
            .any(|event| matches!(event, RecordEvent::Note { .. }))
    );
}
