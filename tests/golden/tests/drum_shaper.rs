use grooviq_dsp::{
    Device,
    drum_shaper::{DrumShaper, PRESETS, preset},
};
use grooviq_golden::{SAMPLE_RATE, compare_exact, devices::test_input, read_wav, write_wav};
fn render(name: &str, block: usize) -> Vec<f32> {
    let (mut l, mut r) = test_input();
    let mut d = DrumShaper::new(SAMPLE_RATE as f32);
    for (i, v) in preset(name).unwrap() {
        d.set_param(i, v);
    }
    for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(l, r, None, &[]);
    }
    l.into_iter().zip(r).flat_map(|(l, r)| [l, r]).collect()
}
#[test]
fn presets_exact_and_block_independent() {
    for name in PRESETS {
        let a = render(name, 128);
        let path = format!(
            "{}/refs/drum_shaper_{}.wav",
            env!("CARGO_MANIFEST_DIR"),
            name.to_lowercase().replace(' ', "_")
        );
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            write_wav(&path, &a);
        }
        compare_exact(&a, &read_wav(&path)).unwrap();
        for block in [1, 7, 64, 127] {
            assert_eq!(a, render(name, block));
        }
    }
}
