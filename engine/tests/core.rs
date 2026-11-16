//! Engine behaviour through its public API: offline renders and direct processor calls.

#[cfg(feature = "test-hooks")]
use grooviq_engine::HealthEvent;
use grooviq_engine::{BPM_RANGE, Command, EngineConfig, engine, render_offline};

const SR: f32 = 48_000.0;

fn cfg() -> EngineConfig {
    EngineConfig { sample_rate: SR }
}

fn tone(on: bool) -> Command {
    Command::TestTone {
        on,
        hz: 440.0,
        gain_db: -12.0,
    }
}

/// Counts `l[i-1] < 0 && l[i] >= 0`.
fn upward_crossings(left: &[f32]) -> usize {
    left.windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count()
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, x| m.max(x.abs()))
}

/// Index of the first sample that is not exactly zero.
fn onset(x: &[f32]) -> Option<usize> {
    x.iter().position(|v| *v != 0.0)
}

fn note_on(key: u8) -> Command {
    Command::Note {
        key,
        velocity: 1.0,
        on: true,
    }
}

/// Samples from a note-on at offset 0 to the synth's first non-zero sample, measured on the
/// device itself (outside the engine).
fn synth_onset_delay() -> usize {
    let mut synth = grooviq_dsp::create("synth", SR).unwrap();
    let (mut l, mut r) = ([0f32; 64], [0f32; 64]);
    let note = grooviq_dsp::NoteEvent {
        offset: 0,
        key: 60,
        velocity: 1.0,
        on: true,
        slide: false,
    };
    synth.process(&mut l, &mut r, None, &[note]);
    onset(&l).unwrap()
}

#[test]
fn silent_until_asked() {
    let out = render_offline(cfg(), &[], 48_000, 256);
    assert!(out.iter().all(|x| *x == 0.0));
}

#[test]
fn notes_are_sample_accurate() {
    let delay = synth_onset_delay();
    assert!(delay < 8, "delay {delay}");
    for frame in [1_000u64, 1_001, 1_023] {
        let l = left(&render_offline(cfg(), &[(frame, note_on(60))], 1_200, 37));
        assert_eq!(onset(&l), Some(frame as usize + delay), "note at {frame}");
    }
}

#[test]
fn events_on_a_block_edge_apply_in_the_block_they_name() {
    // Frame 1024 starts a block: 1023 is the last frame of the block before it.
    let delay = synth_onset_delay();
    for frame in [1_023u64, 1_024] {
        let l = left(&render_offline(cfg(), &[(frame, note_on(60))], 1_200, 37));
        assert_eq!(onset(&l), Some(frame as usize + delay), "note at {frame}");
    }
    // A command due at 1024 is silent through 1023 and plays in the block from 1024.
    let l = left(&render_offline(cfg(), &[(1_024, tone(true))], 1_200, 37));
    assert!(l[..1_024].iter().all(|x| *x == 0.0));
    assert!(peak(&l[1_024..1_088]) > 0.0);
}

#[test]
fn tone_settings_are_clamped_to_their_ranges() {
    let render = |hz, gain_db| {
        let ev = [(
            0,
            Command::TestTone {
                on: true,
                hz,
                gain_db,
            },
        )];
        left(&render_offline(cfg(), &ev, 48_000, 256))
    };
    // 1 Hz plays at the 20 Hz floor and 1 GHz at the 20 kHz ceiling (one second each).
    let c = upward_crossings(&render(1.0, -12.0));
    assert!((19..=21).contains(&c), "low: {c}");
    let c = upward_crossings(&render(1e9, -12.0));
    assert!((19_990..=20_010).contains(&c), "high: {c}");
    // -200 dB plays at the -60 dB floor (0.001), +40 dB at full scale.
    let p = peak(&render(440.0, -200.0));
    assert!((p - 0.001).abs() < 1e-4, "quiet: {p}");
    let p = peak(&render(440.0, 40.0));
    assert!((0.99..=1.0).contains(&p), "loud: {p}");
}

#[test]
fn synth_values_beyond_their_range_play_as_the_limit() {
    let render = |ev: &[(u64, Command)]| render_offline(cfg(), ev, 4_800, 256);
    let spec = grooviq_dsp::param_spec("synth").unwrap();
    // Cutoff, resonance and master.
    for index in [20u32, 21, 39] {
        let (_, max, default) = spec[index as usize];
        let at = |value| {
            render(&[
                (0, Command::SetSynthParam { index, value }),
                (0, note_on(48)),
            ])
        };
        let at_max = at(max);
        assert!(at_max != at(default), "index {index} is audible");
        assert!(at(max * 1_000.0 + 1_000.0) == at_max, "index {index}");
    }
    let loud = |velocity| Command::Note {
        key: 48,
        velocity,
        on: true,
    };
    assert!(render(&[(0, loud(5.0))]) == render(&[(0, loud(1.0))]));
    let (mut c, _p) = engine(cfg());
    assert!(c.send(note_on(128)).is_err());
    assert!(c.send(note_on(127)).is_ok());
}

#[test]
fn play_and_stop_drive_the_transport_flag() {
    let (mut c, mut p) = engine(cfg());
    let mut buf = vec![0f32; 64 * 2];
    c.send(Command::Play).unwrap();
    p.process(&mut buf, 2);
    assert!(c.live().playing);
    c.send(Command::Stop).unwrap();
    p.process(&mut buf, 2);
    assert!(!c.live().playing);
    c.send(Command::PlayLoops { loops: 1 }).unwrap();
    p.process(&mut buf, 2);
    assert!(c.live().playing);
    c.send(Command::AllSoundOff).unwrap();
    p.process(&mut buf, 2);
    assert!(!c.live().playing);
}

#[test]
fn tempo_and_loop_counts_are_checked_and_mirrored() {
    let (mut c, _p) = engine(cfg());
    assert_eq!(c.state().bpm, 140.0, "the default tempo");
    assert!(c.send(Command::SetTempo { bpm: f32::NAN }).is_err());
    assert!(c.send(Command::SetTempo { bpm: f32::INFINITY }).is_err());
    assert!(c.send(Command::PlayLoops { loops: 0 }).is_err());
    assert_eq!(c.state().bpm, 140.0, "a refused tempo changes nothing");
    c.send(Command::SetTempo { bpm: 1_000.0 }).unwrap();
    assert_eq!(c.state().bpm, BPM_RANGE.1);
    c.send(Command::SetTempo { bpm: 1.0 }).unwrap();
    assert_eq!(c.state().bpm, BPM_RANGE.0);
    c.send(Command::SetTempo { bpm: 97.5 }).unwrap();
    assert_eq!(c.state().bpm, 97.5);
    c.send(Command::PlayLoops { loops: 1 }).unwrap();
}

#[test]
fn frames_count_the_rendered_blocks() {
    let (c, mut p) = engine(cfg());
    let mut buf = vec![0f32; 640 * 2];
    p.process(&mut buf, 2);
    assert_eq!(c.live().frames, 10 * 64);
    // One more frame needs one more block.
    p.process(&mut [0f32; 2], 2);
    assert_eq!(c.live().frames, 11 * 64);
}

#[test]
fn every_channel_layout_carries_the_same_signal() {
    // Each output starts as 9.0 and ends in a partial frame, so any sample not written shows.
    let run = |channels: usize| {
        let (mut c, mut p) = engine(cfg());
        assert!(c.synth_preset("Reese")); // wide unison: left and right differ
        c.send(note_on(40)).unwrap();
        let mut out = vec![9.0; 1_000 * channels + channels - 1];
        p.process(&mut out, channels);
        out
    };
    let stereo = run(2);
    let mono = run(1);
    let three = run(3);
    let six = run(6);
    assert!(stereo.chunks(2).any(|f| f[0] != f[1]));
    for i in 0..1_000 {
        let (l, r) = (stereo[2 * i], stereo[2 * i + 1]);
        assert_eq!(mono[i], 0.5 * (l + r), "mono frame {i}");
        assert_eq!(three[3 * i..3 * i + 3], [l, r, 0.0], "frame {i}");
        assert_eq!(
            six[6 * i..6 * i + 6],
            [l, r, 0.0, 0.0, 0.0, 0.0],
            "frame {i}"
        );
    }
    for partial in [&stereo[2_000..], &three[3_000..], &six[6_000..]] {
        assert!(partial.iter().all(|x| *x == 0.0));
    }
}

#[test]
fn tone_is_440hz_at_minus_12_dbfs() {
    let l = left(&render_offline(cfg(), &[(0, tone(true))], 48_000, 256));
    let peak = l.iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!((peak - 0.2512).abs() < 0.002, "peak {peak}");
    let c = upward_crossings(&l[24_000..]);
    assert!((219..=221).contains(&c), "crossings {c}");
}

#[test]
fn reconfigure_keeps_the_pitch_at_a_new_rate() {
    let (mut c, mut p) = engine(cfg());
    c.send(tone(true)).unwrap();
    let mut buf = vec![0f32; 441 * 2];
    p.process(&mut buf, 2);
    c.reconfigure(
        &mut p,
        EngineConfig {
            sample_rate: 44_100.0,
        },
    );
    let mut out = Vec::new();
    for _ in 0..100 {
        p.process(&mut buf, 2); // 1 s at 44.1 kHz
        out.extend_from_slice(&buf);
    }
    let l = left(&out);
    let c = upward_crossings(&l[22_050..]);
    assert!((219..=221).contains(&c), "crossings {c}");
    assert!((peak(&l[22_050..]) - 0.2512).abs() < 0.002);
}

#[test]
fn output_is_identical_for_any_callback_size() {
    let ev = vec![
        (0, tone(true)),
        (
            100,
            Command::Note {
                key: 36,
                velocity: 0.9,
                on: true,
            },
        ),
        (
            20_000,
            Command::Note {
                key: 36,
                velocity: 0.0,
                on: false,
            },
        ),
    ];
    let reference = render_offline(cfg(), &ev, 30_000, 64);
    for cb in [1, 37, 441, 512, 4096] {
        assert_eq!(
            render_offline(cfg(), &ev, 30_000, cb),
            reference,
            "callback {cb}"
        );
    }
}

#[test]
fn any_callback_value_renders_the_same() {
    let ev = [(0, tone(true))];
    let reference = render_offline(cfg(), &ev, 1_000, 64);
    for cb in [0, usize::MAX] {
        assert!(
            render_offline(cfg(), &ev, 1_000, cb) == reference,
            "callback {cb}"
        );
    }
}

#[test]
fn notes_start_at_their_frame_and_other_commands_at_their_block() {
    // Frame 1000 lies in the block 960..1024.
    let note = Command::Note {
        key: 60,
        velocity: 1.0,
        on: true,
    };
    let l = left(&render_offline(cfg(), &[(1_000, note)], 2_000, 37));
    assert!(l[..1_000].iter().all(|x| *x == 0.0));
    assert!(peak(&l[1_000..1_064]) > 0.0);
    let l = left(&render_offline(cfg(), &[(1_000, tone(true))], 2_000, 37));
    assert!(l[..960].iter().all(|x| *x == 0.0));
    assert!(l[961] != 0.0);
}

#[test]
fn out_of_range_and_non_finite_values_are_clamped_or_rejected() {
    let (mut c, mut p) = engine(cfg());
    assert!(
        c.send(Command::SetSynthParam {
            index: 25,
            value: f32::NAN
        })
        .is_err()
    );
    assert!(
        c.send(Command::SetSynthParam {
            index: 999,
            value: 1.0
        })
        .is_err()
    );
    c.send(Command::TestTone {
        on: true,
        hz: 1e9,
        gain_db: 40.0,
    })
    .unwrap(); // clamped to 20000 Hz and 0 dB
    let mut buf = vec![0f32; 4096 * 2];
    p.process(&mut buf, 2);
    assert!(buf.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
    // Clamped, not rejected: the tone plays at full scale.
    assert!(peak(&buf) > 0.99, "peak {}", peak(&buf));
}

#[test]
fn all_sound_off_is_silent_within_10_ms() {
    let ev = vec![
        (0, tone(true)),
        (
            0,
            Command::Note {
                key: 48,
                velocity: 1.0,
                on: true,
            },
        ),
        (24_000, Command::AllSoundOff),
    ];
    let l = left(&render_offline(cfg(), &ev, 30_000, 256));
    // Loud right up to the command, so the silence below is the command's doing.
    assert!(peak(&l[23_000..24_000]) > 0.2);
    assert!(l[24_480..].iter().all(|x| x.abs() < 1e-4));
}

#[test]
fn all_sound_off_stops_the_transport_and_the_tone_and_later_notes_still_play() {
    let (mut c, mut p) = engine(cfg());
    c.send(Command::Play).unwrap();
    c.send(tone(true)).unwrap();
    let mut buf = vec![0f32; 1024 * 2];
    p.process(&mut buf, 2);
    assert!(c.live().playing && c.live().tone);
    c.send(Command::AllSoundOff).unwrap();
    p.process(&mut buf, 2);
    assert!(!c.live().playing && !c.live().tone);
    let note = Command::Note {
        key: 60,
        velocity: 1.0,
        on: true,
    };
    c.send(note).unwrap();
    p.process(&mut buf, 2);
    assert!(peak(&buf) > 0.05, "synth after AllSoundOff: {}", peak(&buf));
}

#[test]
fn the_tone_ramps_on_and_off_over_5_ms() {
    let ev = vec![(0, tone(true)), (24_000, tone(false))];
    let l = left(&render_offline(cfg(), &ev, 30_000, 256));
    // On: after 1 ms the level is still at a fifth of 0.2512; after 5 ms it is all there.
    assert!(peak(&l[..48]) < 0.06, "start {}", peak(&l[..48]));
    assert!(peak(&l[240..480]) > 0.24);
    // Off: not cut at once, silent from 5 ms on.
    assert!(peak(&l[24_000..24_120]) > 0.1);
    assert!(l[24_240..].iter().all(|x| *x == 0.0));
}

#[test]
fn the_peak_meter_attacks_instantly_and_releases_over_about_300_ms() {
    let (mut c, mut p) = engine(cfg());
    c.live_shared().set_output_latency_frames(7_680);
    c.send(tone(true)).unwrap();
    let mut buf = vec![0f32; 4_800 * 2]; // 100 ms
    p.process(&mut buf, 2);
    let held = c.live().peak;
    assert!(
        (held[0] - 0.2512).abs() < 0.002 && held[1] == held[0],
        "{held:?}"
    );
    c.send(tone(false)).unwrap();
    let mut buf = vec![0f32; 14_400 * 2]; // 300 ms of silence after a 5 ms fade
    p.process(&mut buf, 2);
    let ratio = c.live().peak[0] / held[0];
    assert!((ratio - libm::expf(-1.0)).abs() < 0.05, "ratio {ratio}");
    // The processor's writes leave the output layer's field alone.
    assert_eq!(c.live().output_latency_frames, 7_680);
}

#[test]
fn more_events_than_the_queue_holds_render_identically_for_any_callback_size() {
    // 1500 notes in the first 1500 frames (the queue holds 1024), then 300 at one frame (more
    // than one block takes): timing still must not depend on the callback size.
    let mut ev: Vec<(u64, Command)> = (0..1500u64)
        .map(|i| {
            let note = Command::Note {
                key: 36 + (i % 48) as u8,
                velocity: 0.8,
                on: i % 3 != 2,
            };
            (i, note)
        })
        .collect();
    ev.extend((0..300u64).map(|i| {
        let note = Command::Note {
            key: 40 + (i % 40) as u8,
            velocity: 0.5,
            on: i % 2 == 0,
        };
        (5_000, note)
    }));
    let reference = render_offline(cfg(), &ev, 12_000, 64);
    assert!(peak(&reference) > 0.01);
    for cb in [1, 37, 441, 4096] {
        let out = render_offline(cfg(), &ev, 12_000, cb);
        assert!(out == reference, "callback {cb}");
    }
}

#[cfg(feature = "test-hooks")]
#[test]
fn a_node_that_outputs_nan_is_silenced_and_reported() {
    let (mut c, mut p) = engine(cfg());
    c.send(tone(true)).unwrap();
    c.send(Command::InjectNaN).unwrap();
    let mut buf = vec![0f32; 1024 * 2];
    p.process(&mut buf, 2);
    assert!(buf.iter().all(|x| x.is_finite()));
    assert_eq!(c.live().guard_hits, 1);
    assert!(
        c.check_health()
            .contains(&HealthEvent::DeviceSilenced { total: 1 })
    );
    // Reported once.
    assert!(c.check_health().is_empty());
    let mut buf = vec![0f32; 4096 * 2];
    p.process(&mut buf, 2);
    assert!(peak(&buf) > 0.2, "peak {}", peak(&buf));
}

#[cfg(feature = "test-hooks")]
#[test]
fn the_nan_guard_resets_the_device() {
    // A held note; then either a NaN in one synth block or nothing.
    let run = |nan: bool| {
        let (mut c, mut p) = engine(cfg());
        let note = Command::Note {
            key: 48,
            velocity: 1.0,
            on: true,
        };
        c.send(note).unwrap();
        let mut buf = vec![0f32; 1024 * 2];
        p.process(&mut buf, 2);
        if nan {
            c.send(Command::InjectNaN).unwrap();
        }
        p.process(&mut buf, 2);
        let mut after = vec![0f32; 4096 * 2];
        p.process(&mut after, 2);
        after
    };
    let held = run(false);
    assert!(peak(&held) > 0.05, "without the NaN the note still sounds");
    // The reset dropped the voice: no tail after the silenced block.
    assert!(run(true).iter().all(|x| *x == 0.0));
}
