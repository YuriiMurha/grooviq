//! A procedural static mixer graph rendered through the prepared serial kernel with real
//! built-in inserts: keyed compression, a tempo-synced delay on a pre-fader send bus,
//! saturation, EQ and a master utility. Compared byte for byte with `refs/mixer_static.wav`.
//! This is a kernel reference, not the M4 acceptance mix or a legacy-project identity claim.

use grooviq_dsp::{
    Device, HostDomain, HostTransport, comp::Comp, delay::Delay, eq::Eq, saturation::Saturation,
    utility::Utility,
};
use grooviq_engine::mixer::*;
use grooviq_golden::{compare_exact, read_wav, write_wav};

const SR: f32 = 48_000.0;
const FRAMES: usize = 24_064;

fn insert(id: u64) -> Insert {
    Insert {
        id: InsertId(id),
        latency_frames: 0,
        memory_bytes: 0,
    }
}

fn prepared() -> PreparedMixer {
    let track = |id, output: Option<MixerTrackId>, inserts: &[u64]| Track {
        id: MixerTrackId(id),
        output,
        inserts: inserts.iter().map(|&i| insert(i)).collect(),
    };
    let m = Mixer {
        tracks: vec![
            track(0, None, &[90]),
            track(1, Some(MASTER), &[10, 11]),
            track(2, Some(MASTER), &[20]),
            track(3, Some(MASTER), &[30]),
        ],
        sends: vec![
            Send {
                id: SendId(1),
                from: MixerTrackId(1),
                to: MixerTrackId(3),
                tap: Tap::PreFader,
                gain: 0.4,
            },
            Send {
                id: SendId(2),
                from: MixerTrackId(2),
                to: MixerTrackId(3),
                tap: Tap::PostFader,
                gain: 0.25,
            },
        ],
        sidechains: vec![Sidechain {
            from: MixerTrackId(2),
            to: InsertId(11),
            tap: Tap::PreFader,
        }],
        sources: vec![
            Source {
                id: SourceId::Channel(1),
                destination: MixerTrackId(1),
            },
            Source {
                id: SourceId::Channel(2),
                destination: MixerTrackId(2),
            },
        ],
    };
    let mut eq = Eq::new(SR);
    eq.set_param(15, 5.0); // band 3 (400 Hz bell) +5 dB
    let mut comp = Comp::new(SR);
    for (i, v) in [(0, -30.0), (1, 6.0), (2, 2.0), (3, 90.0), (7, 1.0)] {
        comp.set_param(i, v);
    }
    let mut sat = Saturation::new(SR);
    sat.set_param(1, 12.0);
    let mut util = Utility::new(SR);
    util.set_param(0, -3.0);
    util.set_param(2, 1.2);
    let devices = vec![
        PreparedInsert::new(InsertId(10), Box::new(eq)),
        PreparedInsert::new(InsertId(11), Box::new(comp)),
        PreparedInsert::new(InsertId(20), Box::new(sat)),
        PreparedInsert::new(InsertId(30), Box::new(Delay::new(SR))),
        PreparedInsert::new(InsertId(90), Box::new(util)),
    ];
    let gains = [
        TrackGain {
            track: MixerTrackId(1),
            left: 0.8,
            right: 0.7,
        },
        TrackGain {
            track: MixerTrackId(2),
            left: 0.5,
            right: 0.6,
        },
    ];
    let config = PrepareConfig {
        sample_rate: SR,
        max_frames: 128,
        max_bytes: 1 << 20,
    };
    PreparedMixer::prepare(
        compile(&m, Budget::default()).unwrap(),
        devices,
        &gains,
        config,
    )
    .unwrap()
}

/// Track 1: a slow sweep-like pad; track 2: short decaying clicks every 6000 frames.
fn sources() -> [(Vec<f32>, Vec<f32>); 2] {
    let pad_l = (0..FRAMES)
        .map(|i| libm::sinf(i as f32 * 0.021) * 0.4 + libm::sinf(i as f32 * 0.0031) * 0.2)
        .collect();
    let pad_r = (0..FRAMES)
        .map(|i| libm::sinf(i as f32 * 0.019) * 0.4)
        .collect();
    let click = |i: usize| {
        let t = (i % 6000) as f32;
        libm::expf(-t / 400.0) * libm::sinf(t * 0.09) * 0.9
    };
    let hit_l = (0..FRAMES).map(click).collect();
    let hit_r = (0..FRAMES).map(|i| click(i) * 0.8).collect();
    [(pad_l, pad_r), (hit_l, hit_r)]
}

fn render(block: usize) -> Vec<f32> {
    let mut mixer = prepared();
    let input = sources();
    let (mut l, mut r) = (vec![0.0f32; FRAMES], vec![0.0f32; FRAMES]);
    let mut at = 0;
    while at < FRAMES {
        let n = block.min(FRAMES - at);
        let blocks = [
            SourceBlock {
                id: SourceId::Channel(1),
                left: &input[0].0[at..at + n],
                right: &input[0].1[at..at + n],
            },
            SourceBlock {
                id: SourceId::Channel(2),
                left: &input[1].0[at..at + n],
                right: &input[1].1[at..at + n],
            },
        ];
        let transport = HostTransport {
            domain: HostDomain::Song,
            frame: at as u64,
            frames: n as u32,
            sample_rate: SR,
            bpm: 132.0,
            playing: true,
            anchor_frame: 0,
            anchor_tick: 0.0,
            shift: 0,
            loop_start: 0,
            loop_end: 0,
            stream_generation: 0,
            discontinuity: 0,
            changed: at == 0,
        };
        mixer
            .render(&transport, &blocks, &mut l[at..at + n], &mut r[at..at + n])
            .unwrap();
        at += n;
    }
    l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
}

#[test]
fn mixer_static_exact_reference() {
    let a = render(128);
    let path = format!("{}/refs/mixer_static.wav", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
        write_wav(&path, &a);
    } else {
        compare_exact(&a, &read_wav(&path)).unwrap();
    }
    assert_eq!(a, render(37), "partition-independent");
    assert!(a.iter().all(|x| x.is_finite()));
    assert!(a.iter().any(|x| x.abs() > 0.05));
}
