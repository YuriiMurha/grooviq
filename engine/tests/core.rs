//! Engine behaviour through its public API: offline renders and direct processor calls.

#[cfg(feature = "test-hooks")]
use grooviq_engine::HealthEvent;
use grooviq_engine::{Command, EngineConfig, engine, render_offline};

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

#[test]
fn silent_until_asked() {
    let out = render_offline(cfg(), &[], 48_000, 256);
    assert!(out.iter().all(|x| *x == 0.0));
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
    assert!((ratio - (-1f32).exp()).abs() < 0.05, "ratio {ratio}");
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
    let mut buf = vec![0f32; 4096 * 2];
    p.process(&mut buf, 2);
    assert!(peak(&buf) > 0.2, "peak {}", peak(&buf));
}
