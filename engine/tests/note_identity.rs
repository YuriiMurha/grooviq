//! A note-off ends its own note: the one whose note-on carried its id. Every case plays a long
//! note A and a note B on the same key, and B's release comes first; it checks which note sounds
//! between the two releases, and that nothing sounds after both, in every voice mode of the
//! sampler and the synth (rendered offline).

use grooviq_dsp::{sampler, synth};
use grooviq_engine::{
    AudioProcessor, ChannelId, ChannelMix, ChannelSetup, Command, Controller, EngineConfig,
    EngineState, INJECTED_PANIC, NoteId, PatternData, SampleBuf, SeqNote, engine_with,
    render_offline_with,
};
use std::sync::{Arc, Once};

const SR: f32 = 48_000.0;
const KEY: u8 = 60;
/// Below this a window is silent.
const SILENT: f32 = 1e-3;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Inst {
    Sampler,
    Synth,
}

#[derive(Clone, Copy, Debug)]
struct Mode {
    inst: Inst,
    /// 0 poly, 1 mono, 2 legato.
    voice: f32,
}

fn modes() -> Vec<Mode> {
    let mut all = Vec::new();
    for inst in [Inst::Sampler, Inst::Synth] {
        for voice in [0.0, 1.0, 2.0] {
            all.push(Mode { inst, voice });
        }
    }
    all
}

fn param(spec: &[(f32, f32, f32)], ids: &[&str], set: &[(&str, f32)]) -> Vec<f32> {
    let mut p: Vec<f32> = spec.iter().map(|s| s.2).collect();
    for (id, value) in set {
        p[ids.iter().position(|i| i == id).unwrap()] = *value;
    }
    p
}

/// Channel 1: a gated sampler on a steady 0.5 (velocity 1 sounds 0.5 exactly), or a plain
/// synth sine with a flat envelope and fast attack and release.
fn channel(mode: Mode) -> ChannelSetup {
    match mode.inst {
        Inst::Sampler => {
            let n = 4 * SR as usize;
            let sample = Arc::new(SampleBuf {
                rate: SR,
                l: vec![0.5; n],
                r: vec![0.5; n],
            });
            let p = param(
                &sampler::SPEC,
                &sampler::PARAM_IDS,
                &[
                    ("one_shot", 0.0),
                    ("declick", 0.0),
                    ("release", 1.0),
                    ("voice_mode", mode.voice),
                ],
            );
            ChannelSetup::sampler(ChannelId(1), Some(sample), p, ChannelMix::default())
        }
        Inst::Synth => {
            let p = param(
                &synth::SPEC,
                &synth::PARAM_IDS,
                &[
                    ("osc1_shape", 0.0),
                    ("osc2_level", 0.0),
                    ("filter_cutoff", 20_000.0),
                    ("filter_env_amt", 0.0),
                    ("amp_attack", 0.5),
                    ("amp_sustain", 1.0),
                    ("amp_release", 2.0),
                    ("voice_mode", mode.voice),
                ],
            );
            ChannelSetup::synth(ChannelId(1), p, ChannelMix::default())
        }
    }
}

fn state(mode: Mode, notes: Vec<SeqNote>) -> EngineState {
    EngineState {
        bpm: 120.0,
        channels: vec![channel(mode)],
        pattern: PatternData {
            length: 3840 * 4,
            notes,
        },
        ..EngineState::default()
    }
}

/// A sequenced note on channel 1 (120 BPM: 25 frames a tick).
fn seq(tick: u32, length: u32, velocity: f32) -> SeqNote {
    SeqNote {
        tick,
        channel: ChannelId(1),
        key: KEY,
        velocity,
        length,
        slide: false,
    }
}

fn live(on: bool, velocity: f32, id: u32) -> Command {
    Command::ChannelNote {
        channel: ChannelId(1),
        key: KEY,
        velocity,
        on,
        id: NoteId(id),
    }
}

fn left(inter: &[f32]) -> Vec<f32> {
    inter.iter().step_by(2).copied().collect()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

/// The loudest sample of one note held alone at `velocity`, at the same distance from its
/// start as `at` is from B's: the level that tells the notes apart.
fn solo(mode: Mode, velocity: f32) -> f32 {
    let out = render_offline_with(
        EngineConfig { sample_rate: SR },
        state(mode, vec![seq(0, 3000, velocity)]),
        &[(0, Command::Play)],
        30_000,
        64,
    );
    peak(&left(&out)[20_000..30_000])
}

#[derive(Clone, Copy, Debug)]
enum Hear {
    /// The note at this velocity sounds.
    Note(f32),
    Nothing,
}

/// Checks `window` against what `mode` should hear there; a message if it does not.
fn check(mode: Mode, what: &str, window: &[f32], hear: Hear) -> Option<String> {
    let got = peak(window);
    let ok = match hear {
        Hear::Nothing => got < SILENT,
        Hear::Note(velocity) => {
            let want = solo(mode, velocity);
            match mode.inst {
                // A steady sample: the exact level.
                Inst::Sampler => (got - want).abs() < 1e-3,
                Inst::Synth => want > 0.01 && (got - want).abs() < 0.05 * want,
            }
        }
    };
    (!ok).then(|| format!("{mode:?} {what}: expected {hear:?}, peak {got}"))
}

fn report(failures: Vec<String>) {
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// What sounds after B's release while A is still held, per mode: the sampler plays both
/// notes in poly mode and keeps the held A in mono and legato (on the sounding voice, so at
/// B's level in mono, A's in legato); the synth's re-struck key ends A in poly mode (B's
/// release then leaves nothing), while mono and legato go back to the held A.
fn after_inner_release(mode: Mode) -> Hear {
    match (mode.inst, mode.voice as u8) {
        (Inst::Sampler, 0) => Hear::Note(1.0),
        (Inst::Sampler, 1) => Hear::Note(0.2),
        (Inst::Synth, 0) => Hear::Nothing,
        (Inst::Synth, 1) => Hear::Note(0.2),
        (_, _) => Hear::Note(1.0),
    }
}

#[test]
fn nested_same_key_notes_in_a_pattern_release_their_own_voice() {
    let mut failures = Vec::new();
    for mode in modes() {
        // A: ticks 0..1440 (frames 0..36 000) at velocity 1; B: 240..480 (6 000..12 000) at 0.2.
        let notes = vec![seq(0, 1440, 1.0), seq(240, 240, 0.2)];
        let out = render_offline_with(
            EngineConfig { sample_rate: SR },
            state(mode, notes),
            &[(0, Command::Play)],
            48_000,
            64,
        );
        let l = left(&out);
        failures.extend(check(
            mode,
            "between the releases",
            &l[20_000..30_000],
            after_inner_release(mode),
        ));
        failures.extend(check(mode, "after both", &l[40_000..48_000], Hear::Nothing));
    }
    report(failures);
}

#[test]
fn a_live_release_does_not_end_a_sequenced_hold_on_its_key() {
    let mut failures = Vec::new();
    for mode in modes() {
        let out = render_offline_with(
            EngineConfig { sample_rate: SR },
            state(mode, vec![seq(0, 1440, 1.0)]),
            &[
                (0, Command::Play),
                (6_400, live(true, 0.2, 1)),
                (12_800, live(false, 0.0, 1)),
            ],
            48_000,
            64,
        );
        let l = left(&out);
        failures.extend(check(
            mode,
            "between the releases",
            &l[20_000..30_000],
            after_inner_release(mode),
        ));
        failures.extend(check(mode, "after both", &l[40_000..48_000], Hear::Nothing));
    }
    report(failures);
}

/// Silences the injected panic's message (any other panic still prints).
fn quiet_injected_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload_as_str() != Some(INJECTED_PANIC) {
                default_hook(info);
            }
        }));
    });
}

fn run(p: &mut AudioProcessor, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * 2];
    p.process(&mut out, 2);
    left(&out)
}

/// Plays B (velocity 1) after `setup`, then A's late release, then B's: B must sound between
/// them and nothing after.
fn b_outlives_a_late_release(
    what: &str,
    setup: impl Fn(&mut Controller, &mut AudioProcessor),
) -> Vec<String> {
    let mut failures = Vec::new();
    for mode in modes() {
        let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state(mode, Vec::new()));
        run(&mut p, 64);
        setup(&mut c, &mut p);
        c.send(live(true, 1.0, 2)).unwrap();
        run(&mut p, 6_400);
        c.send(live(false, 0.0, 1)).unwrap();
        let between = run(&mut p, 16_000);
        c.send(live(false, 0.0, 2)).unwrap();
        run(&mut p, 8_000);
        let after = run(&mut p, 8_000);
        failures.extend(check(
            mode,
            &format!("{what}, between"),
            &between[8_000..],
            Hear::Note(1.0),
        ));
        failures.extend(check(
            mode,
            &format!("{what}, after"),
            &after,
            Hear::Nothing,
        ));
    }
    failures
}

#[test]
fn a_release_from_before_a_panic_recovery_does_not_end_a_note_after_it() {
    quiet_injected_panics();
    report(b_outlives_a_late_release("recovery", |c, p| {
        let instance = c.channel_instance(ChannelId(1));
        c.send(live(true, 1.0, 1)).unwrap();
        run(p, 640);
        c.send(Command::InjectPanic).unwrap();
        run(p, 64);
        assert_eq!(c.live().faults, 1);
        c.check_health();
        run(p, 64);
        // MIDI sends the release only to the instance its press reached: still this one.
        assert_eq!(c.channel_instance(ChannelId(1)), instance);
    }));
}

#[test]
fn a_release_from_before_a_rate_change_does_not_end_a_note_after_it() {
    report(b_outlives_a_late_release("rate change", |c, p| {
        c.send(live(true, 1.0, 1)).unwrap();
        run(p, 640);
        c.reconfigure(p, EngineConfig { sample_rate: SR });
        run(p, 64);
    }));
}

#[test]
fn the_release_of_a_note_on_a_full_channel_dropped_does_not_end_a_later_note() {
    report(b_outlives_a_late_release("dropped note-on", |c, p| {
        // 128 releases of notes that never started fill the block's buffer, so A's note-on
        // behind them is dropped.
        for i in 0..128 {
            c.send(Command::ChannelNote {
                channel: ChannelId(1),
                key: 61,
                velocity: 0.0,
                on: false,
                id: NoteId(1_000 + i),
            })
            .unwrap();
        }
        c.send(live(true, 1.0, 1)).unwrap();
        run(p, 64);
    }));
}
