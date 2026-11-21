#![cfg(feature = "test-hooks")]

use grooviq_engine::*;
use std::sync::Arc;

fn config(rate: f32) -> EngineConfig {
    EngineConfig { sample_rate: rate }
}

fn state(frames: usize) -> EngineState {
    let sample = Arc::new(SampleBuf {
        rate: 48_000.0,
        l: (0..frames).map(|i| 0.1 + i as f32 / 100_000.0).collect(),
        r: vec![0.0; frames],
    });
    let audio = AudioSchedule::build(
        vec![AudioAsset {
            id: 1,
            source_rate: 48_000,
            source_frames: frames as u64,
            decoded: Some(sample),
        }],
        vec![AudioClip {
            id: 1,
            asset: 1,
            start_tick: 0,
            source_start: 0,
            source_end: frames as u64,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        }],
    )
    .unwrap()
    .0;
    EngineState {
        bpm: 120.0,
        playback_mode: PlaybackMode::Song,
        song: SongData {
            audio,
            end_tick: (frames as u64).div_ceil(25),
            end_frame: frames as u64,
            frame_rate: 48_000.0,
            frame_extent: None,
            ..SongData::default()
        },
        ..EngineState::default()
    }
}

fn recover(explicit: bool, frames: usize, new_rate: f32) -> Vec<f32> {
    let (mut control, mut processor) = engine_with(config(48_000.0), state(frames));
    control.send(Command::Play).unwrap();
    let mut before = [0.0; 512];
    processor.process(&mut before, 2);
    assert!(before.iter().any(|v| *v != 0.0));
    control.send(Command::InjectPanic).unwrap();
    let mut gap = [1.0; 4096];
    processor.process(&mut gap, 2);
    assert!(gap.iter().all(|v| *v == 0.0));
    assert_eq!(control.live().faults, 1);
    if explicit {
        control.reconfigure(&mut processor, config(new_rate));
    } else {
        assert!(
            control
                .check_health()
                .contains(&HealthEvent::Restarted { faults: 1 })
        );
    }
    let mut output = vec![0.0; 128];
    processor.process(&mut output, 2);
    output
}

#[test]
fn expired_clips_stay_silent_after_automatic_recovery() {
    assert!(recover(false, 1024, 48_000.0).iter().all(|v| *v == 0.0));
}

#[test]
fn expired_clips_stay_silent_after_explicit_rate_rebuild() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        assert!(
            recover(true, 1024, rate).iter().all(|v| *v == 0.0),
            "rate {rate}"
        );
    }
}

#[test]
fn active_clips_resume_at_current_transport_after_fault() {
    for explicit in [false, true] {
        let output = recover(explicit, 48_000, 48_000.0);
        // 256 audible + 2048 silent frames elapsed; the old source runtime stopped at 320.
        assert_eq!(output[0], 0.1 + 2304.0 / 100_000.0, "explicit {explicit}");
        assert_eq!(output[1], 0.0);
    }
}

#[test]
fn healthy_rebuilds_keep_fixed_second_progress_across_tempo_and_rate_changes() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let (mut control, mut processor) = engine_with(config(48_000.0), state(48_000));
        control.send(Command::Play).unwrap();
        processor.process(&mut [0.0; 512], 2);
        control.send(Command::SetTempo { bpm: 400.0 }).unwrap();
        control.reconfigure(&mut processor, config(rate));
        let mut output = [0.0; 128];
        processor.process(&mut output, 2);
        assert_eq!(output[0], 0.1 + 256.0 / 100_000.0, "rate {rate}");
    }
}
