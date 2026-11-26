use grooviq_dsp::{
    Device,
    character_delay::{CharacterDelay, PRESETS, preset},
};
use grooviq_golden::{compare_exact, read_wav, write_wav};
fn render(name: &str, sr: f32, block: usize) -> Vec<f32> {
    let mut d = CharacterDelay::new(sr);
    for (i, v) in preset(name).unwrap() {
        d.set_param(i, v);
    }
    let mut l = vec![0.0; (sr * 1.5) as usize];
    let mut r = l.clone();
    for i in 0..(sr * 0.025) as usize {
        l[i] = libm::sinf(i as f32 * 0.27) * 0.95;
        r[i] = libm::sinf(i as f32 * 0.19) * 0.8;
    }
    for (a, b) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        d.process(a, b, None, &[]);
    }
    l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect()
}
#[test]
fn character_delay_exact_references_and_partitions() {
    for (i, name) in PRESETS.iter().enumerate() {
        for sr in [22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
            let reference = render(name, sr, 128);
            for block in 1..=128 {
                assert!(
                    compare_exact(&reference, &render(name, sr, block)).is_ok(),
                    "{name} {sr} {block}"
                );
            }
            if sr == 48000.0 {
                let path = format!(
                    "{}/refs/character_delay_{i}.wav",
                    env!("CARGO_MANIFEST_DIR")
                );
                if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
                    write_wav(&path, &reference);
                } else {
                    let mut expected = read_wav(&path);
                    // The original Digital and Tape WAVs contain subnormal tails.
                    // Keep those files intact and expect signed zero for just those samples.
                    for sample in &mut expected {
                        if sample.is_subnormal() {
                            *sample = 0.0f32.copysign(*sample);
                        }
                    }
                    compare_exact(&reference, &expected).unwrap();
                }
            }
        }
    }
}
