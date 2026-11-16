//! From a project to what the engine plays: instrument parameter lists, a pattern with swing
//! applied, and the whole [`EngineState`].

use crate::model::{
    BPM, Channel, DEFAULT_STEPS, DEFAULT_VELOCITY, MAX_STEPS, Pattern, Project, VOLUME,
};
use grooviq_dsp::sampler::{self, SampleBuf};
use grooviq_dsp::synth;
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, EngineState, InstrumentKind, MAX_CHANNELS,
    MAX_CHOKE_GROUP, PatternData, STEP_TICKS, SeqNote,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// The channel's sampler parameters as the engine takes them: the full `SPEC` length, in
/// `PARAM_IDS` order, each clamped to its range. Missing (or non-finite) values are the
/// defaults; names that are not parameters are ignored.
pub fn sampler_values(channel: &Channel) -> Vec<f32> {
    values(&channel.sampler, &sampler::PARAM_IDS, &sampler::SPEC)
}

/// The channel's synth parameters as the engine takes them, like [`sampler_values`]: the full
/// `grooviq_dsp::synth::SPEC` length, in `PARAM_IDS` order, clamped, defaults for the rest.
pub fn synth_values(channel: &Channel) -> Vec<f32> {
    values(&channel.synth, &synth::PARAM_IDS, &synth::SPEC)
}

fn values(by_name: &BTreeMap<String, f32>, ids: &[&str], spec: &[(f32, f32, f32)]) -> Vec<f32> {
    ids.iter()
        .zip(spec)
        .map(|(id, &(min, max, default))| {
            by_name
                .get(*id)
                .filter(|v| v.is_finite())
                .map_or(default, |v| v.clamp(min, max))
        })
        .collect()
}

/// The pattern as the engine plays it. Swing moves a note that starts exactly on an odd step
/// (`tick % 480 == 240`) later by `round(swing * 120)` ticks. Notes of channels the project does
/// not have, and notes that start at or after the pattern's end, are dropped; the rest are
/// sorted by tick, channel, then slide (a slide follows the plain note it bends).
pub fn compile_pattern(project: &Project, pattern: &Pattern) -> PatternData {
    let steps = pattern.steps.clamp(1, MAX_STEPS);
    let length = steps * STEP_TICKS;
    let swing = if pattern.swing.is_finite() {
        pattern.swing.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let delay = (swing * (STEP_TICKS / 2) as f32).round() as u32;
    let channels: HashSet<u32> = project.channels.iter().map(|c| c.id).collect();

    let mut notes: Vec<SeqNote> = pattern
        .notes
        .iter()
        .filter(|n| n.tick < length && channels.contains(&n.channel))
        .map(|n| SeqNote {
            tick: if n.tick % (2 * STEP_TICKS) == STEP_TICKS {
                n.tick + delay
            } else {
                n.tick
            },
            channel: ChannelId(n.channel),
            key: n.key.min(127),
            velocity: if n.velocity.is_finite() {
                n.velocity.clamp(0.0, 1.0)
            } else {
                DEFAULT_VELOCITY
            },
            length: n.length.max(1),
            slide: n.slide,
        })
        .collect();
    // A stable sort: notes that tie keep the order they have in the project.
    notes.sort_by_key(|n| (n.tick, n.channel, n.slide));
    PatternData { length, notes }
}

/// The engine's state for the project's current pattern. `samples` holds each sampler channel's
/// decoded sample by channel id; a sampler without one stays silent, and a synth channel never
/// takes one. Values are clamped so the engine never refuses the state, even for a project
/// nobody validated.
pub fn engine_state(project: &Project, samples: &HashMap<u32, Arc<SampleBuf>>) -> EngineState {
    let mut seen = HashSet::new();
    let channels = project
        .channels
        .iter()
        .filter(|c| seen.insert(c.id))
        .take(MAX_CHANNELS)
        .map(|c| {
            let id = ChannelId(c.id);
            let mix = ChannelMix {
                volume: finite_or(c.volume, 1.0).clamp(VOLUME.0, VOLUME.1),
                pan: finite_or(c.pan, 0.0).clamp(-1.0, 1.0),
                mute: c.mute,
                solo: c.solo,
            };
            let setup = match c.instrument {
                InstrumentKind::Sampler => {
                    ChannelSetup::sampler(id, samples.get(&c.id).cloned(), sampler_values(c), mix)
                }
                InstrumentKind::Synth => ChannelSetup::synth(id, synth_values(c), mix),
            };
            ChannelSetup {
                choke: c.choke.min(MAX_CHOKE_GROUP),
                ..setup
            }
        })
        .collect();

    let pattern = project
        .patterns
        .iter()
        .find(|p| p.id == project.current_pattern)
        .or_else(|| project.patterns.first())
        .map_or_else(
            || PatternData {
                length: DEFAULT_STEPS * STEP_TICKS,
                notes: Vec::new(),
            },
            |p| compile_pattern(project, p),
        );

    EngineState {
        bpm: finite_or(project.bpm, BPM.2).clamp(BPM.0, BPM.1),
        channels,
        pattern,
    }
}

fn finite_or(value: f32, default: f32) -> f32 {
    if value.is_finite() { value } else { default }
}
