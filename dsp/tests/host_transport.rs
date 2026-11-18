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

#[test]
fn loop_edges_follow_forward_frame_rounding() {
    for rate in [8000.0, 44100.0, 48000.0, 384000.0] {
        for bpm in [20.0, 120.0, 137.0, 400.0] {
            for (start, length, anchor_frame, anchor_tick, shift) in [
                (0, 30, 0, 0.0, 0),
                (0, 3836, 0, 0.0, 0),
                (17, 30, 4096, 18.125, 11),
            ] {
                for iteration in 1..=4 {
                    let boundary = start + iteration * length;
                    // The scheduler rounds this forward mapping, left to right.
                    let delta =
                        ((boundary as f64 - shift as f64 - anchor_tick) * rate as f64 * 60.0
                            / (bpm as f64 * 960.0))
                            .ceil() as u64;
                    let frame = anchor_frame + delta;
                    let h = HostTransport {
                        frame: frame - 1,
                        frames: 3,
                        sample_rate: rate,
                        bpm,
                        anchor_frame,
                        anchor_tick,
                        shift,
                        loop_start: start,
                        loop_end: start + length,
                        changed: false,
                        ..context()
                    };
                    let edges: Vec<_> = h.edges().collect();
                    assert_eq!(edges.len(), 1, "rate={rate} bpm={bpm} boundary={boundary}");
                    assert_eq!(
                        edges[0].offset, 1,
                        "rate={rate} bpm={bpm} boundary={boundary}"
                    );
                    let before = h.position_at(0).unwrap();
                    let at = h.position_at(1).unwrap();
                    assert_eq!(before.iteration, iteration - 1);
                    assert_eq!(at.iteration, iteration);
                    assert!(at.tick >= start as f64 && at.tick < (start + length) as f64);
                    for split in 0..=3 {
                        let a = h.span(0, split).unwrap();
                        let b = h.span(split, 3 - split).unwrap();
                        let split_edges: Vec<_> = a
                            .edges()
                            .chain(b.edges().map(|mut e| {
                                e.offset += split;
                                e
                            }))
                            .collect();
                        assert_eq!(split_edges, edges);
                        for offset in 0..b.frames {
                            assert_eq!(b.position_at(offset), h.position_at(split + offset));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn fractional_anchors_keep_every_wrap_on_its_scheduled_frame() {
    let mut random = 0x346347_u64;
    for _ in 0..512 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let rate = [8000.0, 44100.0, 48000.0, 96000.0, 384000.0][random as usize % 5];
        let bpm = 20.0 + ((random >> 8) % 381) as f32;
        let start = (random >> 17) % 2000;
        let length = 1 + (random >> 29) % 4000;
        let iteration = 1 + (random >> 41) % 1000;
        let shift = (random >> 21) % length;
        let anchor_tick = start as f64 + 0.125 - shift as f64;
        let anchor_frame = 32768;
        let boundary = start + iteration * length;
        let offset = ((boundary as f64 - shift as f64 - anchor_tick) * rate as f64 * 60.0
            / (bpm as f64 * 960.0))
            .ceil() as u64;
        let frame = anchor_frame + offset;
        let h = HostTransport {
            frame: frame - 1,
            frames: 2,
            sample_rate: rate,
            bpm,
            anchor_frame,
            anchor_tick,
            shift,
            loop_start: start,
            loop_end: start + length,
            changed: false,
            ..context()
        };
        assert_eq!(h.position_at(0).unwrap().iteration, iteration - 1);
        assert_eq!(h.position_at(1).unwrap().iteration, iteration);
        assert_eq!(h.edges().last().unwrap().offset, 1);
    }
}
