use grooviq_dsp::{
    Device,
    algorithmic_reverb::{AlgorithmicReverb, PRESETS, preset},
};
use grooviq_golden::{SAMPLE_RATE, compare_exact, devices::test_input, read_wav, write_wav};
fn render(name: &str, block: usize) -> Vec<f32> {
    let (mut l, mut r) = test_input();
    let mut d = AlgorithmicReverb::new(SAMPLE_RATE as f32);
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
            "{}/refs/algorithmic_reverb_{}.wav",
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

#[test]
fn factory_wet_paths_are_exact_at_every_supported_rate() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        for name in PRESETS {
            let render = |block| {
                let mut d = AlgorithmicReverb::new(sr);
                for (i, v) in preset(name).unwrap() {
                    d.set_param(i, v);
                }
                d.set_param(10, 1.);
                let n = (sr * 0.45) as usize;
                let mut l: Vec<_> = (0..n)
                    .map(|i| {
                        if i < (sr * 0.1) as usize {
                            libm::sinf(i as f32 * 0.13) * 0.3
                        } else {
                            0.
                        }
                    })
                    .collect();
                let mut r: Vec<_> = l.iter().map(|v| v * -0.4).collect();
                for (l, r) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
                    d.process(l, r, None, &[]);
                }
                (l, r)
            };
            let a = render(128);
            assert!(a.0.iter().any(|v| v.abs() > 1e-6), "{name} at {sr}");
            assert_eq!(a, render(7));
        }
    }
}
