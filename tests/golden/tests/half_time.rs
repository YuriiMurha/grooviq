use grooviq_dsp::{Device, HostDomain, HostTransport, half_time::HalfTime};
use grooviq_golden::{compare_exact, read_wav, write_wav};

fn render() -> Vec<f32> {
    let mut effect = HalfTime::new(48_000.0);
    for (id, value) in [(0, 1.0), (1, 0.0), (2, 65.0), (3, 0.8), (4, -2.0)] {
        effect.set_param(id, value);
    }
    let mut samples = Vec::new();
    let mut host = HostTransport {
        domain: HostDomain::Song,
        frame: 0,
        frames: 9_216,
        sample_rate: 48_000.0,
        bpm: 397.0,
        playing: true,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 155,
        loop_end: 1_129,
        stream_generation: 0,
        discontinuity: 0,
        changed: true,
    };
    for segment in 0..2 {
        if segment == 1 {
            host.frame = u64::from(host.frames);
            host.anchor_frame = host.frame;
            host.anchor_tick = 441.25;
            host.discontinuity = 1;
        }
        for offset in (0..host.frames).step_by(128) {
            let mut l = [0.0; 128];
            let mut r = [0.0; 128];
            for (i, (l, r)) in l.iter_mut().zip(&mut r).enumerate() {
                let frame = host.frame as u32 + offset + i as u32;
                *l = (frame.wrapping_mul(7_331) % 32_768) as f32 / 32_768.0 - 0.5;
                *r = (frame.wrapping_mul(1_997) % 8_192) as f32 / 8_192.0 - 0.5;
            }
            effect.set_transport(&host.span(offset, 128).unwrap());
            effect.process(&mut l, &mut r, None, &[]);
            samples.extend(l.into_iter().zip(r).flat_map(|(l, r)| [l, r]));
        }
    }
    samples
}

#[test]
fn grid_wrap_and_seek_render_matches_reference_exactly() {
    let actual = render();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/refs/half_time_transport.wav");
    if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
        write_wav(path, &actual);
    } else {
        compare_exact(&actual, &read_wav(path)).unwrap();
    }
}
