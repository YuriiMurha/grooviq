//! Parameter-defined sampler workloads; setup and witnesses are outside counted rendering.
use grooviq_dsp::sampler::{PARAM_IDS, SampleBuf, Sampler};
use grooviq_dsp::{Device, NoteEvent};
use std::sync::Arc;

const SR: f32 = 48_000.0;

pub fn scenario(envelope: f32, velocity_mapping: bool) -> Sampler {
    build(envelope, velocity_mapping, 5_000.0)
}

pub fn build(envelope: f32, velocity_mapping: bool, attack: f32) -> Sampler {
    let mut x = 0x2545_f491_u32;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x as f32 / u32::MAX as f32) - 0.5
    };
    let mut s = Sampler::new(SR);
    s.set_sample(Some(Arc::new(SampleBuf {
        rate: SR,
        l: (0..96_000).map(|_| next()).collect(),
        r: (0..96_000).map(|_| next()).collect(),
    })));
    for (name, value) in [
        ("filter_type", 1.0),
        ("cutoff", 4_000.0),
        ("res", 0.5),
        ("filter_env", envelope),
        ("flt_attack", attack),
        ("vel_filter", if velocity_mapping { 0.75 } else { 0.0 }),
    ] {
        let index = PARAM_IDS.iter().position(|id| *id == name).unwrap();
        s.set_param(index as u32, value);
    }
    let notes: Vec<_> = (0..16)
        .map(|i| NoteEvent {
            offset: 0,
            key: 55 + i,
            velocity: if velocity_mapping {
                0.2 + i as f32 * 0.04
            } else {
                0.8
            },
            on: true,
            slide: false,
            id: u64::from(55 + i),
        })
        .collect();
    let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
    s.process(&mut l, &mut r, None, &notes);
    for _ in 1..16 {
        s.process(&mut l, &mut r, None, &[]);
    }
    assert_eq!(s.active_voices(), 16);
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    assert!(l.iter().chain(&r).any(|v| v.abs() > 1e-6));
    s
}
