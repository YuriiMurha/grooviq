use grooviq_golden::devices::{BLOCK, ott_cases, render_ott, test_input};
use grooviq_golden::{compare, read_wav, write_wav};

#[test]
fn each_ott_preset_matches_its_reference() {
    for (preset, stem) in ott_cases() {
        let actual = render_ott(preset, BLOCK);
        let path = format!("{}/refs/{stem}.wav", env!("CARGO_MANIFEST_DIR"));
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            write_wav(&path, &actual);
            println!("golden file updated: explain why in the commit message");
            continue;
        }
        if let Err(e) = compare(&actual, &read_wav(&path)) {
            panic!("preset {preset}: {e}");
        }
    }
}

#[test]
fn the_render_does_not_depend_on_the_block_size() {
    for (preset, _) in ott_cases() {
        let a = render_ott(preset, 1);
        let b = render_ott(preset, BLOCK);
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()) && a.len() == b.len(),
            "preset {preset} differs between block sizes"
        );
    }
}

#[test]
fn the_presets_sound_different_from_each_other_and_from_the_input() {
    let (l, r) = test_input();
    let input: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
    let renders: Vec<(&str, Vec<f32>)> = ott_cases()
        .into_iter()
        .map(|(p, _)| (p, render_ott(p, BLOCK)))
        .collect();
    for (i, (name, a)) in renders.iter().enumerate() {
        assert!(a.iter().all(|v| v.is_finite()), "{name}");
        assert!(
            compare(a, &input).is_err(),
            "{name} left the input unchanged"
        );
        for (other, b) in &renders[i + 1..] {
            assert!(compare(a, b).is_err(), "{name} and {other} render the same");
        }
    }
}

#[test]
fn the_test_input_is_deterministic_and_in_range() {
    let (a, b) = test_input();
    let (c, d) = test_input();
    assert!(a == c && b == d);
    assert!(a != b);
    assert!(a.iter().chain(&b).all(|v| v.abs() < 1.0));
    assert!(a.iter().any(|v| v.abs() > 0.1) && a.iter().any(|v| *v != 0.0));
}
