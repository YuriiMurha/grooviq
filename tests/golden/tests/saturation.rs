use grooviq_dsp::{
    Device,
    saturation::{PRESETS, Saturation, preset},
};
use grooviq_golden::{compare_exact, read_wav, write_wav};
fn render(sr: f32, block: usize, patch: &str) -> Vec<f32> {
    let mut d = Saturation::new(sr);
    for (i, v) in preset(patch).unwrap() {
        d.set_param(i, v);
    }
    let mut seed = 819u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32 - 0.5) * 0.2
    };
    let mut l: Vec<_> = (0..4096)
        .map(|i| libm::sinf(i as f32 * 0.73) * 0.9 + next())
        .collect();
    let mut r: Vec<_> = (0..4096)
        .map(|i| libm::sinf(i as f32 * 0.17) * 0.7 + next())
        .collect();
    for (a, b) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(a, b, None, &[]);
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}
#[test]
fn saturation_exact_references() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        for (i, p) in PRESETS.iter().enumerate() {
            let a = render(sr, 1, p);
            let path = format!(
                "{}/refs/saturation_{i}_{}.wav",
                env!("CARGO_MANIFEST_DIR"),
                sr as u32
            );
            if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
                write_wav(&path, &a);
            } else {
                compare_exact(&a, &read_wav(&path)).unwrap();
            }
            for block in 1..=128 {
                assert_eq!(a, render(sr, block, p));
            }
        }
    }
}
