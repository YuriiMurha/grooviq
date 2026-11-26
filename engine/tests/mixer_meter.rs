use grooviq_engine::mixer::*;
use grooviq_engine::*;
use std::sync::Arc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000.0,
};
const AUDIBLE: MixerTrackId = MixerTrackId(7);
const SILENT: MixerTrackId = MixerTrackId(2);

fn state() -> EngineState {
    let audio = AudioSchedule::build(
        vec![AudioAsset {
            id: 1,
            source_rate: 48_000,
            source_frames: 4,
            decoded: Some(Arc::new(SampleBuf {
                rate: CONFIG.sample_rate,
                l: vec![0.25, -2.0, 1.0, 0.0],
                r: vec![-0.75, 0.125, 0.5, 0.0],
            })),
        }],
        vec![AudioClip {
            id: 1,
            asset: 1,
            start_tick: 0,
            source_start: 0,
            source_end: 4,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        }],
    )
    .unwrap()
    .0
    .with_tracks(&[(1, 10)])
    .unwrap();

    EngineState {
        playback_mode: PlaybackMode::Song,
        song: SongData {
            audio,
            end_tick: 960,
            ..SongData::default()
        },
        mix: MixSetup::Routed(RoutedMix {
            mixer: Mixer {
                tracks: (0..=70)
                    .rev()
                    .map(|id| Track {
                        id: MixerTrackId(id),
                        output: (id != 0).then_some(MASTER),
                        inserts: vec![],
                    })
                    .collect(),
                sends: vec![],
                sidechains: vec![],
                sources: vec![Source {
                    id: SourceId::AudioTrack(10),
                    destination: AUDIBLE,
                }],
            },
            gains: vec![
                TrackGain {
                    track: AUDIBLE,
                    left: 0.75,
                    right: 0.5,
                },
                TrackGain {
                    track: MASTER,
                    left: 0.25,
                    right: 0.25,
                },
            ],
            devices: vec![],
        }),
        ..EngineState::default()
    }
}

fn render(processor: &mut AudioProcessor) -> [f32; 2 * BLOCK] {
    let mut out = [0.0; 2 * BLOCK];
    processor.process(&mut out, 2);
    out
}

#[test]
fn mixer_meter_captures_guarded_stereo_post_fader_without_changing_audio() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    let (mut reference, mut unmetered) = engine_with(CONFIG, state());
    c.send(Command::Play).unwrap();
    reference.send(Command::Play).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE, SILENT]).is_none());
    assert!(reference.mixer_meter(&[]).is_none());
    let out = render(&mut p);
    assert_eq!(
        &out[..8],
        &[
            0.046875, -0.09375, -0.375, 0.015625, 0.1875, 0.0625, 0.0, 0.0
        ]
    );
    assert_eq!(
        out.map(f32::to_bits),
        render(&mut unmetered).map(f32::to_bits)
    );
    let meter = c.mixer_meter(&[AUDIBLE, SILENT]).unwrap();
    assert_eq!(meter.mix_revision, c.mix_revision());
    assert_eq!(meter.frames, BLOCK as u64);
    assert_eq!(meter.tracks[0].track, SILENT);
    assert_eq!(meter.tracks[0].peak, [0.0, 0.0]);
    assert_eq!(meter.tracks[0].clip_frame, None);
    assert_eq!(meter.tracks[1].track, AUDIBLE);
    assert_eq!(meter.tracks[1].peak, [1.5, 0.375]);
    assert_eq!(meter.tracks[1].clip_frame, Some(BLOCK as u64));

    for _ in 0..24 {
        assert_eq!(
            render(&mut p).map(f32::to_bits),
            render(&mut unmetered).map(f32::to_bits)
        );
    }
    let meter = c.mixer_meter(&[SILENT, AUDIBLE]).unwrap();
    assert_eq!(meter.tracks[1].peak, [0.0, 0.0]);
    assert_eq!(meter.tracks[1].clip_frame, Some(BLOCK as u64));
}

#[test]
fn mixer_meter_canonicalizes_and_bounds_the_selection() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    let mut ids = vec![MASTER, MixerTrackId(u64::MAX), SILENT, SILENT];
    ids.extend((1..=70).map(MixerTrackId));
    assert!(c.mixer_meter(&ids).is_none());
    render(&mut p);
    let meter = c.mixer_meter(&ids).unwrap();
    let captured: Vec<_> = meter.tracks.iter().map(|row| row.track).collect();
    assert_eq!(captured, (1..=64).map(MixerTrackId).collect::<Vec<_>>());

    // A selection change is effective immediately, including closing the display.
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    assert_eq!(c.mixer_meter(&[AUDIBLE]).unwrap().tracks.len(), 1);
    assert!(c.mixer_meter(&[]).is_none());
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
}

#[test]
fn mixer_meter_lease_expires_and_renewal_restarts_clip_history() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    c.send(Command::Play).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    for _ in 0..375 {
        render(&mut p);
    }
    assert_eq!(c.live().frames, 24_000);
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    let meter = c.mixer_meter(&[AUDIBLE]).unwrap();
    assert_eq!(meter.frames, 24_064);
    assert_eq!(meter.tracks[0].clip_frame, None);
}

#[test]
fn mixer_meter_rejects_pending_retired_and_legacy_mixes() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    c.send(Command::Play).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    assert!(
        c.mixer_meter(&[AUDIBLE]).unwrap().tracks[0]
            .clip_frame
            .is_some()
    );

    let second = c.set_mix(state().mix).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    let new_mix = c.mixer_meter(&[AUDIBLE]).unwrap();
    assert_eq!(new_mix.mix_revision, second);
    assert_eq!(new_mix.tracks[0].clip_frame, None);

    c.load_state(state()).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    let new_project = c.mixer_meter(&[AUDIBLE]).unwrap();
    assert_eq!(new_project.mix_revision, c.mix_revision());
    assert_eq!(new_project.tracks[0].clip_frame, None);

    c.set_mix(MixSetup::LegacyDirect).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
}

#[test]
fn mixer_meter_clears_at_same_revision_rate_restart_and_processor_drop() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    assert!(c.mixer_meter(&[AUDIBLE]).is_some());
    let revision = c.mix_revision();
    assert!(c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 96_000.0
        }
    ));
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    let meter = c.mixer_meter(&[AUDIBLE]).unwrap();
    assert_eq!(meter.mix_revision, revision);
    assert_eq!(meter.frames, 2 * BLOCK as u64);
    drop(p);
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
}

#[cfg(feature = "test-hooks")]
#[test]
fn mixer_meter_clears_on_fault_and_only_resumes_on_replacement() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    assert!(c.mixer_meter(&[AUDIBLE]).is_some());
    c.send(Command::InjectPanic).unwrap();
    render(&mut p);
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p); // Retires the faulted mix.
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    assert!(
        c.check_health()
            .iter()
            .any(|event| matches!(event, HealthEvent::Restarted { .. }))
    );
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    render(&mut p);
    assert!(c.mixer_meter(&[AUDIBLE]).is_some());
}

#[test]
fn mixer_meter_uses_the_silenced_tap_after_non_finite_post_fader_output() {
    let mut scene = state();
    let MixSetup::Routed(mix) = &mut scene.mix else {
        unreachable!()
    };
    mix.gains[0].left = f32::MAX;
    let (mut c, mut p) = engine_with(CONFIG, scene);
    c.send(Command::Play).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    assert_eq!(render(&mut p), [0.0; 2 * BLOCK]);
    assert_eq!(c.live().guard_hits, 1);
    let meter = c.mixer_meter(&[AUDIBLE]).unwrap();
    assert_eq!(meter.tracks[0].peak, [0.0, 0.0]);
    assert_eq!(meter.tracks[0].clip_frame, None);
}

#[test]
fn mixer_meter_renewal_extends_the_lease_and_keeps_prior_clips() {
    let (mut c, mut p) = engine_with(CONFIG, state());
    c.send(Command::Play).unwrap();
    assert!(c.mixer_meter(&[AUDIBLE]).is_none());
    for _ in 0..250 {
        render(&mut p);
    }
    assert_eq!(
        c.mixer_meter(&[AUDIBLE]).unwrap().tracks[0].clip_frame,
        Some(BLOCK as u64)
    );
    for _ in 0..250 {
        render(&mut p);
    }
    let renewed = c.mixer_meter(&[AUDIBLE]).unwrap();
    assert_eq!(renewed.frames, 32_000);
    assert_eq!(renewed.tracks[0].clip_frame, Some(BLOCK as u64));
}
