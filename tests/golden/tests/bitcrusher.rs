use grooviq_dsp::{
    Device,
    bitcrusher::{Bitcrusher, PRESETS, preset},
};
use grooviq_golden::{SAMPLE_RATE, compare_exact, devices::test_input, read_wav, write_wav};
fn render(name: &str, block: usize) -> Vec<f32> {
    let (mut l, mut r) = test_input();
    let mut d = Bitcrusher::new(SAMPLE_RATE as f32);
    for (i, v) in preset(name).unwrap() {
        d.set_param(i, v);
    }
    for (a, b) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(a, b, None, &[]);
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}
#[test]
fn exact_presets_and_block_parity() {
    for name in PRESETS {
        let a = render(name, 1);
        for block in [7, 64, 127, 128] {
            assert_eq!(a, render(name, block));
        }
        let path = format!(
            "{}/refs/bitcrusher_{}.wav",
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
