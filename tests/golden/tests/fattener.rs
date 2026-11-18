use grooviq_dsp::{
    Device,
    fattener::{Fattener, PRESETS, preset},
};
use grooviq_golden::{compare_exact, devices::test_input, read_wav, write_wav};

fn render_fixture(name: &str, block: usize, hot: bool) -> Vec<f32> {
    let (mut l, mut r) = test_input();
    if hot {
        // Original low-frequency square pulses: enough sustained level to engage
        // every factory compressor, including nonzero Colour settings.
        for (i, (l, r)) in l.iter_mut().zip(&mut r).enumerate() {
            *l = if i % 1200 < 600 { 0.98 } else { -0.98 };
            *r = if i % 900 < 450 { -0.91 } else { 0.91 };
        }
    }
    let mut d = Fattener::new(48000.);
    for (i, v) in preset(name).unwrap() {
        d.set_param(i, v);
    }
    for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(l, r, None, &[]);
    }
    l.into_iter().zip(r).flat_map(|(l, r)| [l, r]).collect()
}
#[test]
fn factory_renders_match_exact_wavs_at_every_block_size() {
    for name in PRESETS {
        let actual = render_fixture(name, 128, false);
        let stem = name.to_lowercase().replace(' ', "_");
        let path = format!("{}/refs/fattener_{stem}.wav", env!("CARGO_MANIFEST_DIR"));
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            write_wav(&path, &actual);
        }
        compare_exact(&actual, &read_wav(&path)).unwrap();
        for block in [1, 7, 64, 127] {
            assert_eq!(
                actual,
                render_fixture(name, block, false),
                "{name}, {block}"
            );
        }
    }
}

#[test]
fn hot_factory_renders_pin_post_tilt_compression() {
    for name in PRESETS {
        let actual = render_fixture(name, 128, true);
        let stem = name.to_lowercase().replace(' ', "_");
        let path = format!(
            "{}/refs/fattener_hot_{stem}.wav",
            env!("CARGO_MANIFEST_DIR")
        );
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            write_wav(&path, &actual);
        }
        compare_exact(&actual, &read_wav(&path)).unwrap();
        for block in [1, 7, 64, 127] {
            assert_eq!(actual, render_fixture(name, block, true));
        }
    }
}
