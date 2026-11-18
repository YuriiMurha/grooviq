use grooviq_dsp::*;
fn context() -> HostTransport {
    HostTransport {
        domain: HostDomain::Song,
        frame: 0,
        frames: 128,
        sample_rate: 8000.0,
        bpm: 400.0,
        playing: true,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 0,
        loop_end: 1,
        stream_generation: 1,
        discontinuity: 2,
        changed: true,
    }
}
#[test]
fn canonical_splits_edges_and_zero_span() {
    let c = context();
    let expected: Vec<_> = (0..128).map(|i| c.position_at(i).unwrap()).collect();
    let edges: Vec<_> = c.edges().collect();
    assert!(edges.len() > 90);
    assert!(edges.len() <= MAX_BLOCK);
    for split in 0..=128 {
        let a = c.span(0, split).unwrap();
        let b = c.span(split, 128 - split).unwrap();
        let actual: Vec<_> = (0..a.frames)
            .map(|i| a.position_at(i).unwrap())
            .chain((0..b.frames).map(|i| b.position_at(i).unwrap()))
            .collect();
        assert_eq!(expected, actual);
        let actual: Vec<_> = a
            .edges()
            .chain(b.edges().map(|mut e| {
                e.offset += split;
                e
            }))
            .collect();
        assert_eq!(edges, actual);
    }
    assert_eq!(c.span(0, 0).unwrap().edges().count(), 0);
    assert!(c.position_at(129).is_none());
    assert!(c.position_at(128).is_some());
}
#[test]
fn fractional_anchor_and_stopped_time() {
    let mut c = context();
    c.loop_end = 0;
    c.frame = 12345;
    c.anchor_frame = 12000;
    c.anchor_tick = 19.125;
    c.bpm = 137.0;
    c.sample_rate = 44100.0;
    assert_eq!(
        c.position_at(7).unwrap().tick,
        19.125 + 352.0 / (44100.0 * 60.0 / (137.0 * 960.0))
    );
    c.playing = false;
    assert_eq!(c.position_at(0), c.position_at(128));
}
#[test]
fn endpoint_rates_tempos_and_nonzero_loop_bounds() {
    for rate in [8000.0, 44100.0, 48000.0, 384000.0] {
        for bpm in [20.0, 137.0, 400.0] {
            let mut h = context();
            h.sample_rate = rate;
            h.bpm = bpm;
            h.loop_start = 17;
            h.loop_end = 18;
            h.anchor_tick = 17.0;
            let mut previous = None;
            for frame in 0..1024 {
                h.frame = frame;
                h.frames = 1;
                h.changed = frame == 0;
                let position = h.position_at(0).unwrap();
                assert!(position.tick >= 17.0 && position.tick < 18.0);
                if let Some(old) = previous {
                    assert!(position.iteration >= old);
                    assert!(position.iteration - old <= 1);
                }
                assert!(h.edges().count() <= 1);
                previous = Some(position.iteration);
            }
        }
    }
}
