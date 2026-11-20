use grooviq_engine::*;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Update {
    None,
    SameBank,
    EmptyBank,
}

fn render(update: Update) -> (Vec<f32>, bool) {
    let sample = Arc::new(SampleBuf {
        rate: 48_000.0,
        l: (0..96_000)
            .map(|frame| 0.1 + frame as f32 / 1_000_000.0)
            .collect(),
        r: vec![0.0; 96_000],
    });
    let (audio, _) = AudioSchedule::build(
        vec![AudioAsset {
            id: 1,
            source_rate: 48_000,
            source_frames: 96_000,
            decoded: Some(sample),
        }],
        vec![AudioClip {
            id: 1,
            asset: 1,
            start_tick: 0,
            source_start: 0,
            source_end: 96_000,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        }],
    )
    .unwrap();
    let state = EngineState {
        bpm: 120.0,
        playback_mode: PlaybackMode::Song,
        song: SongData {
            audio,
            end_tick: 3840,
            end_frame: 96_000,
            frame_rate: 48_000.0,
            frame_extent: None,
            ..SongData::default()
        },
        ..EngineState::default()
    };
    let (mut control, mut processor) = engine_with(
        EngineConfig {
            sample_rate: 48_000.0,
        },
        state,
    );
    control.send(Command::Play).unwrap();
    let mut output = Vec::with_capacity(192_000);
    for frame in (0..96_000).step_by(64) {
        if frame == 24_000 {
            control.send(Command::SetTempo { bpm: 60.0 }).unwrap();
        }
        if !matches!(update, Update::None) && frame == 48_000 {
            let mut song = control.state().song.clone();
            song.end_tick = 1920;
            if matches!(update, Update::EmptyBank) {
                song.audio = AudioSchedule::default();
                song.end_tick = 1440;
                song.end_frame = 48_000;
            }
            control.set_song(song).unwrap();
        }
        let mut block = [0.0; 128];
        processor.process(&mut block, 2);
        output.extend_from_slice(&block);
    }
    (output, control.live().playing)
}

#[test]
fn compatible_song_update_after_tempo_change_preserves_active_audio() {
    let (baseline, _) = render(Update::None);
    let (updated, _) = render(Update::SameBank);
    let mismatch = baseline
        .iter()
        .zip(&updated)
        .position(|(a, b)| a.to_bits() != b.to_bits());
    assert!(
        mismatch.is_none(),
        "same-bank update changed audio at frame {:?}",
        mismatch.map(|sample| sample / 2)
    );
}

#[test]
fn replacing_the_audio_bank_stops_old_voices_and_can_finish() {
    let (baseline, _) = render(Update::None);
    let (updated, playing) = render(Update::EmptyBank);
    assert_eq!(&baseline[..96_000], &updated[..96_000]);
    assert!(updated[96_000..].iter().all(|sample| *sample == 0.0));
    assert!(
        !playing,
        "old voice state must not prevent the new song from finishing"
    );
}
