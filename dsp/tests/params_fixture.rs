//! `tests/fixtures/device-params.json`: the one parameter table the UI editors check themselves
//! against, and `tests/fixtures/synth-presets.json`: every synth factory preset's 40 values. Both
//! are generated from the `dsp` tables; refresh them on purpose with
//! `GOLDEN_UPDATE=1 cargo test -p grooviq-dsp --test params_fixture`.

use grooviq_dsp::{sampler, synth};

fn table(ids: &[&str], spec: &[(f32, f32, f32)]) -> String {
    assert_eq!(ids.len(), spec.len(), "ids and SPEC differ in length");
    let rows: Vec<String> = ids
        .iter()
        .zip(spec)
        .map(|(id, (min, max, default))| {
            format!(r#"    {{ "id": "{id}", "min": {min}, "max": {max}, "default": {default} }}"#)
        })
        .collect();
    rows.join(",\n")
}

fn generated() -> String {
    format!(
        "{{\n  \"synth\": [\n{}\n  ],\n  \"sampler\": [\n{}\n  ]\n}}\n",
        table(&synth::PARAM_IDS, &synth::SPEC),
        table(&sampler::PARAM_IDS, &sampler::SPEC)
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
