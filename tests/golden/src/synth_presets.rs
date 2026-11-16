//! One scene per synth preset (plus "Init"): load the patch, play three overlapping keys, render
//! 0.75 s. Each scene has a byte-exact reference in `refs/synth_preset_<snake_case name>.wav`
//! (the `preset_` infix keeps "808 Glide" apart from the older `synth_808_glide` scene).

use grooviq_dsp::synth::preset;
use grooviq_engine::Command;

use crate::Scene;

/// Every scene name, in order: "Init" first, then `synth::PRESETS`.
pub fn preset_names() -> Vec<&'static str> {
    let mut names = vec!["Init"];
    names.extend(grooviq_dsp::synth::PRESETS);
    names
}

/// "808 Glide" -> "808_glide".
pub fn snake_case(name: &str) -> String {
    name.to_lowercase().replace(' ', "_")
}

/// `synth_preset_<snake_case preset name>`: the reference file's stem.
pub fn stem(preset: &str) -> String {
    format!("synth_preset_{}", snake_case(preset))
}

/// Keys 48, 55 and 60 on at frames 0, 2 400 and 4 800 (so they overlap; legato presets glide
/// between them), all off at frame 24 000, rendered for 0.75 s.
pub fn synth_preset_scene(preset_name: &str) -> Scene {
    let mut events: Vec<(u64, Command)> = preset(preset_name)
        .unwrap_or_else(|| panic!("no synth preset named {preset_name:?}"))
        .into_iter()
        .map(|(index, value)| (0, Command::SetSynthParam { index, value }))
        .collect();
    let note = |key, on| Command::Note {
        key,
        velocity: 1.0,
        on,
    };
    events.extend([
        (0, note(48, true)),
        (2_400, note(55, true)),
        (4_800, note(60, true)),
        (24_000, note(48, false)),
        (24_000, note(55, false)),
        (24_000, note(60, false)),
    ]);
    Scene {
        name: "synth_preset",
        seconds: 0.75,
        events,
    }
}
