//! `tests/fixtures/device-params.json`: the one parameter table the UI editors check themselves
//! against. It is generated from the `SPEC` and `PARAM_IDS` tables; refresh it on purpose with
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

#[test]
fn the_device_params_fixture_matches_the_spec_tables() {
    let path = format!(
        "{}/../tests/fixtures/device-params.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let expected = generated();
    if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
        std::fs::write(&path, &expected).expect("write the fixture");
        println!("fixture updated: explain why in the commit message");
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {path} ({e}); create it with GOLDEN_UPDATE=1"));
    assert_eq!(
        committed, expected,
        "device-params.json is stale; refresh it with GOLDEN_UPDATE=1"
    );
}
