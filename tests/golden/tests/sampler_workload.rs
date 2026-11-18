// Exercise the exact benchmark setup on every platform without Valgrind or production hooks.
#[path = "../benches/support/sampler_workload.rs"]
mod sampler_workload;

use grooviq_dsp::{Device, sampler::Sampler};
use sampler_workload::{build, scenario};

fn output(mut s: Sampler) -> Vec<u32> {
    let mut bits = Vec::new();
    let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
    for _ in 0..100 {
        s.process(&mut l, &mut r, None, &[]);
        assert_eq!(s.active_voices(), 16);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        bits.extend(l.iter().chain(&r).map(|v| v.to_bits()));
    }
    bits
}

#[test]
fn measured_region_responds_to_envelope_attack_and_velocity_mapping() {
    let static_bits = output(scenario(0.0, false));
    let envelope = output(scenario(0.7, false));
    assert_ne!(static_bits, envelope);
    // Changing attack duration changes this region: the long attack is still moving,
    // rather than the envelope having already reached the same sustain plateau.
    assert_ne!(envelope, output(build(0.7, false, 4_000.0)));
    assert_ne!(envelope, output(scenario(0.7, true)));
    assert_eq!(envelope, output(scenario(0.7, false)));
}
