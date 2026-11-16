use grooviq_golden::{compare, render, scenes};

const BLOCK_SIZES: [usize; 2] = [64, 512];

#[test]
fn each_scene_matches_its_reference() {
    for scene in scenes() {
        let actual = render(&scene, 512);
        let path = format!("{}/refs/{}.wav", env!("CARGO_MANIFEST_DIR"), scene.name);
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            grooviq_golden::write_wav(&path, &actual);
            println!("golden file updated: explain why in the commit message");
            continue;
        }
        let reference = grooviq_golden::read_wav(&path);
        if let Err(e) = compare(&actual, &reference) {
            panic!("scene {}: {e}", scene.name);
        }
    }
}

#[test]
fn each_scene_is_identical_at_block_sizes_64_and_512() {
    for scene in scenes() {
        let a = render(&scene, BLOCK_SIZES[0]);
        let b = render(&scene, BLOCK_SIZES[1]);
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()) && a.len() == b.len(),
            "scene {} differs between callback sizes",
            scene.name
        );
    }
}

#[test]
fn compare_rejects_a_changed_render() {
    let scene = &scenes()[0];
    let good = render(scene, 512);
    let mut bad = good.clone();
    bad[1000] += 1e-3;
    assert!(compare(&good, &good).is_ok());
    assert!(compare(&bad, &good).is_err());
    assert!(compare(&good[1..], &good).is_err());
}

#[test]
fn compare_rejects_a_different_length() {
    let good = render(&scenes()[0], 512);
    // A clean prefix must not pass (zip would truncate), nor a longer render.
    assert!(compare(&good[..good.len() - 2], &good).is_err());
    assert!(compare(&good, &good[..good.len() - 2]).is_err());
    assert!(compare(&[], &good).is_err());
}

#[test]
fn compare_rejects_non_finite_samples() {
    let good = render(&scenes()[0], 512);
    for bad_value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut bad = good.clone();
        bad[1000] = bad_value;
        assert!(compare(&bad, &good).is_err(), "{bad_value} accepted");
    }
}
