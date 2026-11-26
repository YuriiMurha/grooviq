//! The UI parameter and preset fixtures under `tests/fixtures` are generated from the DSP
//! tables. Refresh them on purpose with
//! `GOLDEN_UPDATE=1 cargo test -p grooviq-dsp --test params_fixture`.

use grooviq_dsp::{
    EFFECTS, is_stepped, param_ids, param_spec, preset, presets as effect_presets, sampler, synth,
};

/// One row per parameter, with `stepped` for switches and discrete choices.
fn table(ids: &[&str], spec: &[(f32, f32, f32)], stepped: impl Fn(usize) -> bool) -> String {
    assert_eq!(ids.len(), spec.len(), "ids and SPEC differ in length");
    let rows: Vec<String> = ids
        .iter()
        .zip(spec)
        .enumerate()
        .map(|(i, (id, (min, max, default)))| {
            let stepped = stepped(i);
            format!(
                r#"    {{ "id": "{id}", "min": {min}, "max": {max}, "default": {default}, "stepped": {stepped} }}"#
            )
        })
        .collect();
    rows.join(",\n")
}

fn generated() -> String {
    format!(
        "{{\n  \"synth\": [\n{}\n  ],\n  \"sampler\": [\n{}\n  ]\n}}\n",
        table(&synth::PARAM_IDS, &synth::SPEC, synth::is_stepped),
        table(&sampler::PARAM_IDS, &sampler::SPEC, sampler::is_stepped)
    )
}

/// `{ "<preset>": [40 values in SPEC order], ... }`, "Init" first, then `synth::PRESETS`.
fn presets() -> String {
    let rows: Vec<String> = std::iter::once("Init")
        .chain(synth::PRESETS)
        .map(|name| {
            let pairs = synth::preset(name).unwrap_or_else(|| panic!("no preset {name}"));
            let mut values = vec![f32::NAN; synth::SPEC.len()];
            for (index, value) in pairs {
                values[index as usize] = value;
            }
            assert!(
                values.iter().all(|v| v.is_finite()),
                "{name} does not set all {} parameters",
                values.len()
            );
            let list: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
            format!(r#"  "{name}": [{}]"#, list.join(", "))
        })
        .collect();
    format!("{{\n{}\n}}\n", rows.join(",\n"))
}

/// The committed fixture equals `expected`; `GOLDEN_UPDATE=1` rewrites it instead.
fn check_fixture(file: &str, expected: &str) {
    let path = format!("{}/../tests/fixtures/{file}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
        std::fs::write(&path, expected).expect("write the fixture");
        println!("{file} updated: explain why in the commit message");
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {path} ({e}); create it with GOLDEN_UPDATE=1"));
    assert_eq!(
        committed, expected,
        "{file} is stale; refresh it with GOLDEN_UPDATE=1"
    );
}

#[test]
fn the_device_params_fixture_matches_the_spec_tables() {
    check_fixture("device-params.json", &generated());
}

#[test]
fn the_synth_presets_fixture_matches_the_factory_presets() {
    check_fixture("synth-presets.json", &presets());
}

#[test]
fn effect_params_fixture_matches_the_registry() {
    let effects: Vec<String> = EFFECTS
        .iter()
        .map(|&id| {
            let rows = table(param_ids(id).unwrap(), param_spec(id).unwrap(), |i| {
                is_stepped(id, i as u32).unwrap()
            });
            format!("  \"{id}\": [\n{rows}\n  ]")
        })
        .collect();
    check_fixture(
        "effect-params.json",
        &format!("{{\n{}\n}}\n", effects.join(",\n")),
    );
}

#[test]
fn effect_presets_fixture_matches_the_registry() {
    let effects: Vec<String> = EFFECTS
        .iter()
        .map(|&id| {
            let names = effect_presets(id);
            if names.is_empty() {
                return format!("  \"{id}\": {{}}");
            }
            let spec = param_spec(id).unwrap();
            let rows: Vec<String> = names
                .iter()
                .map(|name| {
                    let mut values: Vec<_> = spec.iter().map(|s| s.2).collect();
                    for (index, value) in preset(id, name).unwrap() {
                        values[index as usize] = value;
                    }
                    let list: Vec<_> = values.iter().map(|v| v.to_string()).collect();
                    format!(r#"    "{name}": [{}]"#, list.join(", "))
                })
                .collect();
            format!("  \"{id}\": {{\n{}\n  }}", rows.join(",\n"))
        })
        .collect();
    check_fixture(
        "effect-presets.json",
        &format!("{{\n{}\n}}\n", effects.join(",\n")),
    );
}
