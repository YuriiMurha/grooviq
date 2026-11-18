use grooviq_dsp::{
    Device,
    lofi_colour::{LofiColour, PRESETS, preset},
};
use grooviq_golden::{compare_exact, devices::test_input, read_wav, write_wav};
fn render(name: &str, rate: u32, block: usize) -> Vec<f32> {
    // Use the same deterministic source fixture at every device rate so changes
    // in device coefficients are covered independently of source generation.
    let (mut l, mut r) = test_input();
    let mut d = LofiColour::new(rate as f32);
    for (i, v) in preset(name).unwrap() {
        d.set_param(i, v);
    }
    let at = 15000;
    let mut i = 0;
    while i < l.len() {
        if i == at {
            for (p, v) in [
                (0, 0.5),
                (1, 0.8),
                (2, 0.8),
                (3, 0.2),
                (4, 1.0),
                (5, 0.6),
                (6, 0.7),
                (7, 2.0),
            ] {
                d.set_param(p, v);
            }
        }
        let e = (i + block)
            .min(l.len())
            .min(if i < at { at } else { l.len() });
        d.process(&mut l[i..e], &mut r[i..e], None, &[]);
        i = e;
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}
#[test]
fn exact_presets_all_rates_and_automated_block_parity() {
    for rate in [22050, 44100, 48000, 96000, 192000] {
        for name in PRESETS {
            let a = render(name, rate, 1);
            for block in [7, 64, 127, 128] {
                let b = render(name, rate, block);
                assert!(
                    a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
                    "{name} {rate} block {block}"
                );
            }
            let suffix = if rate == 48000 {
                String::new()
            } else {
                format!("_{rate}")
            };
            let path = format!(
                "{}/refs/lofi_colour_{}{suffix}.wav",
                env!("CARGO_MANIFEST_DIR"),
                name.to_lowercase().replace(' ', "_")
            );
            if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
                write_wav(&path, &a);
            } else {
                compare_exact(&a, &read_wav(&path)).unwrap();
            }
        }
    }
}
