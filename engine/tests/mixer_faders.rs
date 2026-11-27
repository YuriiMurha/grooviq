//! Live fader edits measured against generated, constant stereo input.

use grooviq_dsp::Device;
use grooviq_engine::mixer::*;
use grooviq_engine::*;
use std::sync::Arc;

const INPUT: [f32; 2] = [0.25, 0.125];
const STRIP: MixerTrackId = MixerTrackId(10);
const UNITY: [f32; 2] = [1.0, 1.0];
const BUS: MixerTrackId = MixerTrackId(5);
const RATE: f32 = 48_000.0;

fn track(id: MixerTrackId, output: Option<MixerTrackId>) -> Track {
    Track {
        id,
        output,
        inserts: vec![],
    }
}

fn strip_mix(left: f32, right: f32) -> RoutedMix {
    RoutedMix {
        mixer: Mixer {
            tracks: vec![track(MASTER, None), track(STRIP, Some(MASTER))],
            sources: vec![Source {
                id: SourceId::AudioTrack(1),
                destination: STRIP,
            }],
            ..Mixer::default()
        },
        gains: vec![TrackGain {
            track: STRIP,
            left,
            right,
        }],
        devices: vec![],
    }
}

fn fixture(rate: f32, mix: RoutedMix) -> (Controller, AudioProcessor) {
    fixture_with_inputs(rate, mix, &[INPUT])
}

fn fixture_with_inputs(
    rate: f32,
    mix: RoutedMix,
    inputs: &[[f32; 2]],
) -> (Controller, AudioProcessor) {
    let frames = 32_768;
    let mut assets = Vec::new();
    let mut clips = Vec::new();
    let mut routes = Vec::new();
    for (id, &[l, r]) in (1..).zip(inputs) {
        assets.push(AudioAsset {
            id,
            source_rate: rate as u32,
            source_frames: frames,
            decoded: Some(Arc::new(SampleBuf {
                rate,
                l: vec![l; frames as usize],
                r: vec![r; frames as usize],
            })),
        });
        clips.push(AudioClip {
            id,
            asset: id,
            start_tick: 0,
            source_start: 0,
            source_end: frames,
            gain: 1.0,
            reverse: false,
            fade_in: 0,
            fade_out: 0,
        });
        routes.push((id, u64::from(id)));
    }
    let (audio, diagnostics) = AudioSchedule::build(assets, clips).unwrap();
    assert!(diagnostics.is_empty());
    let (mut controller, processor) = engine_with(
        EngineConfig { sample_rate: rate },
        EngineState {
            playback_mode: PlaybackMode::Song,
            song: SongData {
                audio: audio.with_tracks(&routes).unwrap(),
                frame_rate: rate,
                end_tick: 100_000,
                end_frame: 2_500_000,
                ..SongData::default()
            },
            mix: MixSetup::Routed(mix),
            ..EngineState::default()
        },
    );
    controller.send(Command::Play).unwrap();
    (controller, processor)
}

fn install(controller: &mut Controller, mix: RoutedMix) {
    controller.set_mix(MixSetup::Routed(mix)).unwrap();
}

fn render(processor: &mut AudioProcessor, frames: usize, partition: usize) -> Vec<[f32; 2]> {
    let mut out = vec![0.0; frames * 2];
    for part in out.chunks_mut(partition * 2) {
        processor.process(part, 2);
    }
    out.as_chunks::<2>().0.to_vec()
}

fn ramp(from: [f32; 2], to: [f32; 2], sample: usize, duration: usize) -> [f32; 2] {
    if sample >= duration {
        return to;
    }
    let t = sample as f64 / duration as f64;
    std::array::from_fn(|i| (f64::from(from[i]) * (1.0 - t) + f64::from(to[i]) * t) as f32)
}

fn stereo(signal: [f32; 2], gain: [f32; 2]) -> [f32; 2] {
    [signal[0] * gain[0], signal[1] * gain[1]]
}

fn add_gain(mix: &mut RoutedMix, track: MixerTrackId, [left, right]: [f32; 2]) {
    mix.gains.push(TrackGain { track, left, right });
}

#[test]
fn first_live_frame_moves_one_sample_toward_each_endpoint() {
    let (mut controller, mut processor) = fixture(RATE, strip_mix(1.0, 1.0));
    let mut block = [0.0; BLOCK * 2];
    processor.process(&mut block, 2);
    assert_eq!(block.as_chunks::<2>().0, &[INPUT; BLOCK]);

    controller
        .set_mix(MixSetup::Routed(strip_mix(0.0, 1.5)))
        .unwrap();
    processor.process(&mut block, 2);

    let expected = [INPUT[0] * (239.0 / 240.0), INPUT[1] * (1.0 + 0.5 / 240.0)];
    assert_eq!(&block[..2], &expected, "first live fader frame");
}

#[test]
fn level_pan_and_zero_reach_their_endpoints_in_five_ms_at_every_partition() {
    let moves = [(1.25, 0.75), (0.75, -0.75), (0.0, 0.0), (0.5, 0.0)];
    for (rate, duration) in [
        (8_000.0, 40),
        (44_100.0, 221),
        (RATE, 240),
        (96_000.0, 480),
        (192_000.0, 960),
        (384_000.0, 1920),
    ] {
        let span = (duration + BLOCK).div_ceil(BLOCK) * BLOCK;
        let mut baseline = None;
        for partition in [1, 7, 37, 64, 127, 128] {
            let (mut c, mut p) = fixture(rate, strip_mix(0.5, 0.5));
            assert_eq!(
                render(&mut p, BLOCK, partition),
                [stereo(INPUT, [0.5; 2]); BLOCK]
            );
            let mut from = [0.5; 2];
            let mut receipt = Vec::new();
            for (volume, pan) in moves {
                let [left, right] = ChannelMix {
                    volume,
                    pan,
                    ..ChannelMix::default()
                }
                .gains(true);
                let to = [left, right];
                install(&mut c, strip_mix(left, right));
                p.process(&mut [], 2);
                let actual = render(&mut p, span, partition);
                for (frame, &sample) in actual.iter().enumerate() {
                    let expected = stereo(INPUT, ramp(from, to, frame + 1, duration));
                    // The independent f64 formula can land on the other side of an f32
                    // midpoint. The endpoint and partition comparison still require exact bits.
                    let tolerance = u32::from(frame + 1 < duration);
                    for (got, want) in sample.into_iter().zip(expected) {
                        assert!(
                            got.to_bits().abs_diff(want.to_bits()) <= tolerance,
                            "{rate} Hz, partition {partition}, frame {frame}, target {to:?}: {got} != {want}"
                        );
                    }
                }
                receipt.extend(actual.iter().map(|frame| frame.map(f32::to_bits)));
                from = to;
            }
            match &baseline {
                Some(bits) => assert_eq!(&receipt, bits, "{rate} Hz, partition {partition}"),
                None => baseline = Some(receipt),
            }
        }
    }
}

#[test]
fn strip_meter_reports_the_ramped_post_tap_at_the_end_of_the_rendered_block() {
    let (mut c, mut p) = fixture(RATE, strip_mix(1.0, 1.0));
    assert!(c.mixer_meter(&[STRIP]).is_none());
    render(&mut p, BLOCK, BLOCK);
    let before = c.mixer_meter(&[STRIP]).unwrap();
    assert_eq!(before.frames, BLOCK as u64);
    assert_eq!(before.tracks[0].peak, INPUT);

    let target = [0.0, 1.5];
    let mut edit = strip_mix(target[0], target[1]);
    edit.mixer.tracks.insert(0, track(BUS, Some(MASTER)));
    add_gain(&mut edit, MASTER, [0.5; 2]);
    let revision = c.set_mix(MixSetup::Routed(edit)).unwrap();
    let tracks = &[STRIP, MASTER, BUS];
    assert!(c.mixer_meter(tracks).is_none());
    render(&mut p, 1, 1);
    let after = c.mixer_meter(tracks).unwrap();
    assert_eq!(after.mix_revision, revision);
    assert_eq!(after.frames, (2 * BLOCK) as u64);
    assert_eq!(after.tracks.len(), 2);
    assert_eq!(after.tracks[0].track, BUS);
    assert_eq!(after.tracks[0].peak, [0.0; 2]);
    assert_eq!(after.tracks[1].track, STRIP);
    let first = stereo(INPUT, ramp(UNITY, target, 1, 240));
    let last = stereo(INPUT, ramp(UNITY, target, BLOCK, 240));
    assert_eq!(after.tracks[1].peak, [first[0], last[1]]);
}

#[test]
fn repeats_and_reordered_tracks_preserve_independent_channel_and_master_histories() {
    let (mut c, mut p) = fixture(RATE, strip_mix(1.0, 1.0));
    render(&mut p, BLOCK, BLOCK);
    let to = [0.0, 2.0];
    let master = [0.5, 1.5];
    let mut mix = strip_mix(to[0], to[1]);
    add_gain(&mut mix, MASTER, master);
    install(&mut c, mix.clone());
    let first = render(&mut p, BLOCK, 7);

    // A lower persistent ID enters before the running strip in execution order.
    mix.mixer.tracks.insert(0, track(BUS, Some(MASTER)));
    add_gain(&mut mix, BUS, [4.0, 3.0]);
    add_insert(&mut mix, BUS, InsertId(1), "utility", vec![]);
    let mut invalid = mix.clone();
    invalid.gains[0].left = f32::NAN;
    let revision = c.mix_revision();
    assert!(matches!(
        c.set_mix(MixSetup::Routed(invalid)),
        Err(ControlError::Rejected)
    ));
    assert_eq!(c.mix_revision(), revision);
    let after_refusal = render(&mut p, BLOCK, 37);
    install(&mut c, mix.clone());
    let after_repeat = render(&mut p, BLOCK, 37);
    for (frame, sample) in first
        .into_iter()
        .chain(after_refusal)
        .chain(after_repeat)
        .enumerate()
    {
        let expected = stereo(
            stereo(INPUT, ramp(UNITY, to, frame + 1, 240)),
            ramp(UNITY, master, frame + 1, 240),
        );
        assert_eq!(sample, expected, "frame {frame}");
    }

    // Only the left coefficient changes. The right and Master have 48 frames left.
    let retarget = 0.75;
    mix.gains[0].left = retarget;
    install(&mut c, mix);
    let left_at_install = ramp(UNITY, to, BLOCK * 3, 240)[0];
    for (i, sample) in render(&mut p, BLOCK * 4, 127).into_iter().enumerate() {
        let frame = i + 1;
        let left = ramp([left_at_install; 2], [retarget; 2], frame, 240)[0];
        let right = ramp(UNITY, to, BLOCK * 3 + frame, 240)[1];
        let expected = stereo(
            stereo(INPUT, [left, right]),
            ramp(UNITY, master, BLOCK * 3 + frame, 240),
        );
        assert_eq!(sample, expected, "retarget frame {frame}");
    }
}

#[test]
fn replacement_track_starts_at_its_own_target() {
    let (mut c, mut p) = fixture(RATE, strip_mix(1.0, 1.0));
    render(&mut p, BLOCK, BLOCK);
    install(&mut c, strip_mix(0.0, 2.0));
    render(&mut p, BLOCK, BLOCK);
    let mut replacement = strip_mix(0.5, 0.25);
    replacement.mixer.tracks[1].id = BUS;
    replacement.mixer.sources[0].destination = BUS;
    replacement.gains[0].track = BUS;
    install(&mut c, replacement);
    assert_eq!(
        render(&mut p, BLOCK, 7),
        [stereo(INPUT, [0.5, 0.25]); BLOCK]
    );
}

#[test]
fn ramps_advance_on_silent_blocks_and_reset_and_rate_rebuild_snap_to_targets() {
    let target = [0.25, 0.75];
    for ending in ["silent", "reset", "new rate"] {
        let (mut c, mut p) = fixture(RATE, strip_mix(1.0, 1.0));
        render(&mut p, BLOCK, BLOCK);
        install(&mut c, strip_mix(target[0], target[1]));
        if ending == "silent" {
            c.send(Command::Stop).unwrap();
            assert_eq!(render(&mut p, 4 * BLOCK, 37), [[0.0; 2]; BLOCK * 4]);
        } else {
            assert_eq!(
                render(&mut p, BLOCK, 37)[0],
                stereo(INPUT, ramp(UNITY, target, 1, 240))
            );
            if ending == "reset" {
                c.send(Command::AllSoundOff).unwrap();
            } else {
                assert!(c.reconfigure(
                    &mut p,
                    EngineConfig {
                        sample_rate: 96_000.0
                    }
                ));
            }
        }
        c.send(Command::Play).unwrap();
        for sample in render(&mut p, BLOCK, 37) {
            for (got, expected) in sample.into_iter().zip(stereo(INPUT, target)) {
                // The rate-rebuilt clip uses the resampler, including its edge.
                if ending == "new rate" {
                    assert!(
                        (got - expected).abs() < 1e-6,
                        "{ending}: {got} != {expected}"
                    );
                } else {
                    assert_eq!(got, expected, "{ending}");
                }
            }
        }
    }
}

#[test]
fn opposite_finite_extremes_cross_zero_and_arrive_without_faults() {
    let from = [f32::MAX, -f32::MAX];
    let to = [-f32::MAX, f32::MAX];
    let (mut c, mut p) = fixture(RATE, strip_mix(from[0], from[1]));
    render(&mut p, BLOCK, BLOCK);
    install(&mut c, strip_mix(to[0], to[1]));
    for (frame, sample) in render(&mut p, BLOCK * 4, 37).into_iter().enumerate() {
        let expected = stereo(INPUT, ramp(from, to, frame + 1, 240));
        for (got, want) in sample.into_iter().zip(expected) {
            assert!(got.is_finite());
            assert!(
                ((got - want) / f32::MAX).abs() < 1e-7,
                "frame {frame}: {got} != {want}"
            );
            if frame + 1 >= 240 {
                assert_eq!(got.to_bits(), want.to_bits());
            }
        }
    }
    assert!(c.check_health().is_empty());
}

fn suppress_strip_main(mix: &mut RoutedMix) {
    mix.mixer.tracks[1].output = Some(BUS);
    mix.mixer.tracks.push(track(BUS, Some(MASTER)));
    add_gain(mix, BUS, [0.0; 2]);
}

fn add_insert(
    mix: &mut RoutedMix,
    track: MixerTrackId,
    id: InsertId,
    kind: &str,
    params: Vec<f32>,
) {
    let node = mix
        .mixer
        .tracks
        .iter_mut()
        .find(|node| node.id == track)
        .unwrap();
    node.inserts
        .push(describe_builtin_insert(id, kind).unwrap());
    mix.devices.push(InsertDevice {
        insert: id,
        kind: kind.into(),
        seed: 0,
        params,
    });
}

#[test]
fn pre_and_post_sends_independently_read_the_correct_tap_after_inserts() {
    let to = [0.0, 1.75];
    let params = [0.0, 0.0, 1.0, 0.0, 1.0];
    let mut utility = grooviq_dsp::utility::Utility::new(RATE);
    for (index, param) in (0..).zip(params) {
        utility.set_param(index, param);
    }
    let [mut left, mut right] = INPUT.map(|sample| [sample]);
    utility.process(&mut left, &mut right, None, &[]);
    let inserted = [left[0], right[0]];
    assert!(inserted.iter().all(|&sample| sample < 0.0));
    for tap in [Tap::PreFader, Tap::PostFader] {
        let mut mix = strip_mix(1.0, 1.0);
        suppress_strip_main(&mut mix);
        // A polarity flip proves that both taps are after the insert.
        add_insert(&mut mix, STRIP, InsertId(5), "utility", params.to_vec());
        mix.mixer.sends.push(Send {
            id: SendId(1),
            from: STRIP,
            to: MASTER,
            tap,
            gain: 0.5,
        });
        let (mut c, mut p) = fixture(RATE, mix.clone());
        let before = stereo(inserted, [0.5; 2]);
        assert_eq!(render(&mut p, BLOCK, 7), [before; BLOCK]);
        mix.gains[0].left = to[0];
        mix.gains[0].right = to[1];
        install(&mut c, mix);
        for (frame, sample) in render(&mut p, BLOCK * 4, 37).into_iter().enumerate() {
            let expected = match tap {
                Tap::PreFader => before,
                Tap::PostFader => {
                    stereo(stereo(inserted, ramp(UNITY, to, frame + 1, 240)), [0.5; 2])
                }
            };
            assert_eq!(sample, expected, "{tap:?} send, frame {frame}");
        }
    }
}

#[test]
fn detector_taps_match_separately_fed_compressors() {
    let to = [0.0, 4.0];
    let program = [0.125, 0.0625];
    let receiver = MixerTrackId(20);
    let insert = InsertId(5);
    let params = [-24.0, 8.0, 0.05, 20.0, 0.0, 0.0, 1.0, 1.0, 20.0];
    let mut traces = Vec::new();
    for tap in [Tap::PreFader, Tap::PostFader] {
        let mut mix = strip_mix(1.0, 1.0);
        suppress_strip_main(&mut mix);
        mix.mixer.tracks.push(track(receiver, Some(MASTER)));
        mix.mixer.sources.push(Source {
            id: SourceId::AudioTrack(2),
            destination: receiver,
        });
        add_insert(&mut mix, receiver, insert, "comp", params.to_vec());
        mix.mixer.sidechains.push(Sidechain {
            from: STRIP,
            to: insert,
            tap,
        });
        let mut compressor = grooviq_dsp::comp::Comp::new(RATE);
        for (index, param) in (0..).zip(params) {
            compressor.set_param(index, param);
        }
        let (mut c, mut p) = fixture_with_inputs(RATE, mix.clone(), &[INPUT, program]);
        let mut trace = Vec::new();
        for block in 0..5 {
            if block == 1 {
                mix.gains[0].left = to[0];
                mix.gains[0].right = to[1];
                install(&mut c, mix.clone());
            }
            let mut left = [program[0]; BLOCK];
            let mut right = [program[1]; BLOCK];
            let mut key = [[0.0; BLOCK]; 2];
            for frame in 0..BLOCK {
                let gain = if block > 0 && tap == Tap::PostFader {
                    ramp(UNITY, to, (block - 1) * BLOCK + frame + 1, 240)
                } else {
                    UNITY
                };
                [key[0][frame], key[1][frame]] = stereo(INPUT, gain);
            }
            compressor.process(&mut left, &mut right, Some((&key[0], &key[1])), &[]);
            let actual = render(&mut p, BLOCK, 37);
            for (frame, (&sample, (left, right))) in
                actual.iter().zip(left.into_iter().zip(right)).enumerate()
            {
                assert_eq!(
                    sample,
                    [left, right],
                    "{tap:?} detector, block {block}, frame {frame}"
                );
            }
            trace.extend(actual);
        }
        traces.push(trace);
    }
    assert!((traces[0][BLOCK * 4][0] - traces[1][BLOCK * 4][0]).abs() > 0.001);
}
