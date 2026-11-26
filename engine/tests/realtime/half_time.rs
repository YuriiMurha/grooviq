use super::realtime;
use grooviq_dsp::{
    Device, HostDomain, HostTransport,
    half_time::{HalfTime, SPEC},
};

#[test]
fn controls_capture_retrigger_and_reset_are_realtime() {
    let mut effect = HalfTime::new(48_000.0);
    let mut host = HostTransport {
        domain: HostDomain::Song,
        frame: 0,
        frames: 128,
        sample_rate: 48_000.0,
        bpm: 400.0,
        playing: true,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 2,
        loop_end: 8,
        stream_generation: 1,
        discontinuity: 0,
        changed: true,
    };
    let dry: [f32; 128] = std::array::from_fn(|i| (i as f32 - 64.0) / 128.0);
    let mut l = dry;
    let mut r = dry;
    realtime(|| {
        effect.set_transport(&host);
        effect.process(&mut l, &mut r, None, &[]);
    });
    assert_ne!(l, dry);
    assert!(effect.meter(1) > 0.0);

    realtime(|| {
        for endpoint in [false, true] {
            for (i, &(lo, hi, _)) in SPEC.iter().enumerate() {
                effect.set_param(i as u32, if endpoint { hi } else { lo });
            }
            effect.set_tempo(20.0);
            host.frame += u64::from(host.frames);
            host.anchor_frame = host.frame;
            host.anchor_tick = 4.0;
            host.discontinuity += 1;
            effect.set_transport(&host);
            effect.process(&mut l, &mut r, None, &[]);
        }
        effect.reset();
        effect.set_tempo(400.0);
        effect.process(&mut l, &mut r, None, &[]);
    });
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
}
