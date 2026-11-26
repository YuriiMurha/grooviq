use grooviq_dsp::{Device, HostDomain, HostTransport, half_time::HalfTime};

const RATE: f32 = 48_000.0;
const QUARTER: usize = 7_200; // 1 beat at 400 BPM.

fn device(speed: f32, smoothing: f32) -> HalfTime {
    let mut device = HalfTime::new(RATE);
    for (index, value) in [(0, speed), (1, 0.0), (2, smoothing), (5, 400.0)] {
        device.set_param(index, value);
    }
    device
}

fn host(frames: usize) -> HostTransport {
    HostTransport {
        domain: HostDomain::Song,
        frame: 0,
        frames: frames as u32,
        sample_rate: RATE,
        bpm: 400.0,
        playing: true,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 0,
        loop_end: 0,
        stream_generation: 0,
        discontinuity: 0,
        changed: false,
    }
}

fn render(
    device: &mut HalfTime,
    left: &mut [f32],
    right: &mut [f32],
    block: usize,
    transport: Option<HostTransport>,
) {
    for (offset, (l, r)) in left
        .chunks_mut(block)
        .zip(right.chunks_mut(block))
        .enumerate()
    {
        if let Some(transport) = transport {
            device.set_transport(
                &transport
                    .span((offset * block) as u32, l.len() as u32)
                    .unwrap(),
            );
        }
        device.process(l, r, None, &[]);
    }
}

#[test]
fn a_ramp_reads_at_the_selected_fraction_and_restarts_on_grid() {
    let input: Vec<f32> = (0..QUARTER + 4).map(|i| i as f32 / 8192.0).collect();
    for (speed, ratio) in [(0.0, 0.5), (1.0, 0.25)] {
        let mut device = device(speed, 0.0);
        let mut l = input.clone();
        let mut r: Vec<f32> = input.iter().map(|x| -x).collect();
        render(&mut device, &mut l, &mut r, 7, Some(host(input.len())));
        for (i, (&l, &r)) in l.iter().zip(&r).enumerate() {
            let start = i / QUARTER * QUARTER;
            let expected = (start as f32 + (i - start) as f32 * ratio) / 8192.0;
            assert_eq!(l, expected, "speed {speed}, frame {i}");
            assert_eq!(r, -expected, "speed {speed}, right frame {i}");
        }
        assert_eq!(device.latency_frames(), 0);
        assert_eq!(device.meter(0), input[input.len() - 1]);
        assert_eq!(device.meter(1), l[l.len() - 1]);
        assert_eq!(device.meter(2), 3.0 / QUARTER as f32);
    }
}

#[test]
fn seek_and_stream_change_retrigger_inside_the_same_grid_window() {
    let mut device = device(0.0, 0.0);
    let mut transport = host(16);
    let mut l = [0.25; 16];
    let mut r = [-0.25; 16];
    render(&mut device, &mut l, &mut r, 7, Some(transport));
    for marker in 0..4 {
        transport.frame += 16;
        transport.anchor_frame = transport.frame;
        transport.anchor_tick = 480.0;
        transport.changed = marker == 1;
        if marker == 0 {
            transport.discontinuity = 1;
        }
        if marker == 2 {
            transport.stream_generation = 1;
        }
        if marker == 3 {
            transport.frame += 32;
            transport.anchor_frame = transport.frame;
        }
        let level = -0.5 + marker as f32 * 0.125;
        l.fill(level);
        r.fill(-level);
        render(&mut device, &mut l, &mut r, 7, Some(transport));
        assert_eq!(l, [level; 16], "marker {marker}");
        assert_eq!(r, [-level; 16], "marker {marker}");
        assert!((0.5..0.51).contains(&device.meter(2)));
    }
}

#[test]
fn smoothing_bounds_grid_loop_and_seek_seams() {
    for (seam, mut transport) in [
        (QUARTER, host(QUARTER + 128)),
        (
            QUARTER / 2,
            HostTransport {
                loop_start: 240,
                loop_end: 480,
                ..host(QUARTER / 2 + 128)
            },
        ),
        (128, host(256)),
    ] {
        let mut device = device(0.0, 50.0);
        let mut l = vec![0.6; seam + 128];
        l[seam..].fill(-0.6);
        let mut r = l.clone();
        if seam == 128 {
            render(
                &mut device,
                &mut l[..seam],
                &mut r[..seam],
                7,
                transport.span(0, seam as u32),
            );
            transport.frame = seam as u64;
            transport.frames = 128;
            transport.discontinuity = 1;
            transport.anchor_frame = transport.frame;
            transport.anchor_tick = 240.0;
            render(
                &mut device,
                &mut l[seam..],
                &mut r[seam..],
                7,
                Some(transport),
            );
        } else {
            render(&mut device, &mut l, &mut r, 7, Some(transport));
        }
        assert_eq!(l[seam], l[seam - 1], "seam at {seam}");
        assert!(l[seam..].windows(2).all(|w| (w[1] - w[0]).abs() < 0.01));
        assert!(
            l[seam + 127] < 0.55,
            "crossfade must advance toward new input"
        );
    }
}

#[test]
fn loop_restarts_capture_even_when_wrapping_within_one_grid_cell() {
    let mut device = device(0.0, 0.0);
    let frames = QUARTER / 2 + 3;
    let mut l = vec![0.25; frames];
    l[QUARTER / 2..].fill(-0.75);
    let mut r = l.clone();
    render(
        &mut device,
        &mut l,
        &mut r,
        64,
        Some(HostTransport {
            loop_start: 240,
            loop_end: 480,
            ..host(frames)
        }),
    );
    assert_eq!(&l[QUARTER / 2 - 1..], &[0.25, -0.75, -0.75, -0.75]);
}

#[test]
fn transport_render_is_bit_identical_across_block_sizes_and_reset() {
    let frames = 22_000;
    let input: Vec<f32> = (0..frames)
        .map(|i| ((i * 7331 % 32768) as f32 - 16384.0) / 32768.0)
        .collect();
    let transport = HostTransport {
        bpm: 393.0,
        anchor_tick: 35.25,
        loop_start: 300,
        loop_end: 1313,
        ..host(frames)
    };
    let seek = 7_831;
    let audition = |device: &mut HalfTime, block| {
        let mut l = input.clone();
        let mut r = input.clone();
        render(
            device,
            &mut l[..seek],
            &mut r[..seek],
            block,
            transport.span(0, seek as u32),
        );
        render(
            device,
            &mut l[seek..],
            &mut r[seek..],
            block,
            Some(HostTransport {
                frame: seek as u64,
                frames: (frames - seek) as u32,
                anchor_frame: seek as u64,
                anchor_tick: 650.0,
                discontinuity: 1,
                ..transport
            }),
        );
        (l, r)
    };
    let mut device = device(1.0, 71.0);
    device.set_param(3, 0.62);
    device.set_param(4, -1.5);
    let (expected_l, expected_r) = audition(&mut device, 1);
    for size in [7, 64, 128] {
        device.reset();
        let (l, r) = audition(&mut device, size);
        assert!(
            l.iter()
                .zip(&expected_l)
                .chain(r.iter().zip(&expected_r))
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "block {size}"
        );
    }
}

#[test]
fn standalone_clock_runs_from_first_sample_at_tempo_extremes() {
    for (sample_rate, tempo, window, frames) in [
        (192_000.0, 20.0, 3.0, 4_608_000),
        (22_050.0, 400.0, 0.0, 3_308),
    ] {
        let mut device = HalfTime::new(sample_rate);
        device.set_param(1, window);
        device.set_param(2, 0.0);
        device.set_tempo(tempo);
        let mut frame = 0;
        while frame < frames {
            let len = (frames - frame).min(128);
            let mut l = [0.5; 128];
            let mut r = [-0.5; 128];
            // The last fractional read needs the upper tap at exactly half the window.
            let marker = frames / 2;
            if (frame..frame + len).contains(&marker) {
                l[marker - frame] = 1.0;
                r[marker - frame] = -1.0;
            }
            device.process(&mut l[..len], &mut r[..len], None, &[]);
            let mut expected = [0.5; 128];
            if frame + len == frames {
                expected[len - 1] = 0.75;
            }
            assert_eq!(l, expected, "frame {frame}");
            assert_eq!(r, expected.map(|x| -x), "frame {frame}");
            frame += len;
        }
        let mut l = [-0.25];
        let mut r = [0.25];
        device.process(&mut l, &mut r, None, &[]);
        assert_eq!((l, r), ([-0.25], [0.25]), "{sample_rate} Hz, {tempo} BPM");
        assert!(device.meter(2) < 0.001);
    }
}

#[test]
fn zero_mix_preserves_dry_bits_at_any_output_gain_and_flushes_subnormals() {
    let mut device = device(0.0, 0.0);
    device.set_param(3, 0.0);
    device.set_param(4, 12.0);
    let mut l = [0.125, -0.0, 0.5, -1.0, 27.0, f32::from_bits(1)];
    let mut r = [-0.125, 0.0, -0.5, 1.0, -27.0, f32::from_bits(0x8000_0001)];
    let expected_l = [0.125f32, -0.0, 0.5, -1.0, 27.0, 0.0].map(f32::to_bits);
    let expected_r = [-0.125f32, 0.0, -0.5, 1.0, -27.0, -0.0].map(f32::to_bits);
    device.process(&mut l, &mut r, None, &[]);
    assert_eq!(l.map(f32::to_bits), expected_l);
    assert_eq!(r.map(f32::to_bits), expected_r);
    device.set_param(3, 1.0);
    device.reset();
    let mut l = [f32::MIN_POSITIVE, 0.0, 0.0, 0.0];
    let mut r = l;
    device.set_param(4, -24.0);
    device.process(&mut l, &mut r, None, &[]);
    assert!(l.iter().chain(&r).all(|&sample| sample == 0.0));
}

#[test]
fn stopped_transport_uses_host_tempo_then_restarts_on_play() {
    let mut device = device(0.0, 0.0);
    device.set_tempo(20.0);
    let mut l = vec![0.25; QUARTER + 3];
    l[QUARTER..].fill(-0.25);
    let mut r = l.clone();
    let stopped = HostTransport {
        playing: false,
        ..host(l.len())
    };
    render(&mut device, &mut l, &mut r, 64, Some(stopped));
    assert_eq!(&l[QUARTER - 1..], &[0.25, -0.25, -0.25, -0.25]);

    let playing = HostTransport {
        playing: true,
        frame: l.len() as u64,
        anchor_frame: l.len() as u64,
        anchor_tick: 480.0,
        ..host(4)
    };
    let mut l = [0.5; 4];
    let mut r = l;
    render(&mut device, &mut l, &mut r, 4, Some(playing));
    assert_eq!(l, [0.5; 4]);
    assert!((0.5..0.51).contains(&device.meter(2)));
}

#[test]
fn smoothing_reaches_new_input_before_a_short_loop_wraps_again() {
    let mut device = device(0.0, 100.0);
    let samples = 90;
    let transport = HostTransport {
        loop_start: 0,
        loop_end: 1,
        ..host(samples)
    };
    let mut l = vec![0.0; samples];
    let first_wrap = 8;
    let second_wrap = 15;
    l[first_wrap..second_wrap].fill(1.0);
    let mut r = l.clone();
    render(&mut device, &mut l, &mut r, 7, Some(transport));
    assert_eq!(l[first_wrap], 0.0);
    assert_eq!(l[second_wrap - 1], 1.0);
    assert_eq!(l[second_wrap], 1.0);
}

#[test]
fn grid_uses_forward_rounding_at_an_exact_scheduled_frame() {
    let mut device = device(0.0, 0.0);
    let boundary = 64_000;
    let mut l = [0.5, -0.5, -0.5];
    let mut r = l;
    let transport = HostTransport {
        bpm: 45.0,
        frame: boundary - 1,
        ..host(l.len())
    };
    // At 45 BPM the inverse clock reports slightly under 960 ticks at frame 64,000.
    // The scheduler's forward map still places the beat exactly on that frame.
    render(&mut device, &mut l, &mut r, 3, Some(transport));
    assert_eq!(l, [0.5, -0.5, -0.5]);
}
