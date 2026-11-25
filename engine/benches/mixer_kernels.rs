//! The static serial mixer kernel at 64/128 frames: six source tracks with built-in inserts,
//! one keyed compressor, a pre-fader send bus and a master chain. Construction, input
//! synthesis and warmup are outside the timed closure; each timed call renders the next
//! block of an immutable precomputed input bank with continuous device state. Divan also
//! reports allocations inside the timed closure, which must stay at zero. Smoke timings on a
//! shared host: no deadline or dropout claim.

use divan::{Bencher, black_box};
use grooviq_dsp::{
    Device, HostDomain, HostTransport, comp::Comp, eq::Eq, saturation::Saturation, utility::Utility,
};
use grooviq_engine::mixer::*;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

const SR: f32 = 48_000.0;
const TRACKS: u64 = 6;
const BANK_BLOCKS: usize = 32;
const WARMUP_BLOCKS: usize = 64;

fn main() {
    divan::main();
}

fn insert(id: u64) -> Insert {
    Insert {
        id: InsertId(id),
        latency_frames: 0,
        memory_bytes: 0,
    }
}

fn prepared(block: usize) -> PreparedMixer {
    // Tracks 1..=6 -> master 0; bus 7 -> master, fed by pre-fader sends from every track.
    let mut tracks = vec![
        Track {
            id: MASTER,
            output: None,
            inserts: vec![insert(900), insert(901)],
        },
        Track {
            id: MixerTrackId(7),
            output: Some(MASTER),
            inserts: vec![insert(700)],
        },
    ];
    let mut sends = vec![];
    let mut sources = vec![];
    let mut devices: Vec<PreparedInsert> = vec![];
    for t in 1..=TRACKS {
        tracks.push(Track {
            id: MixerTrackId(t),
            output: Some(MASTER),
            inserts: vec![insert(t * 10), insert(t * 10 + 1)],
        });
        sends.push(Send {
            id: SendId(t),
            from: MixerTrackId(t),
            to: MixerTrackId(7),
            tap: Tap::PreFader,
            gain: 0.3,
        });
        sources.push(Source {
            id: SourceId::Channel(t),
            destination: MixerTrackId(t),
        });
        let mut sat = Saturation::new(SR);
        sat.set_param(1, 6.0 + t as f32);
        devices.push(PreparedInsert::new(InsertId(t * 10), Box::new(Eq::new(SR))));
        devices.push(PreparedInsert::new(InsertId(t * 10 + 1), Box::new(sat)));
    }
    let mut keyed = Comp::new(SR);
    for (i, v) in [(0, -24.0), (1, 4.0), (7, 1.0)] {
        keyed.set_param(i, v);
    }
    devices.push(PreparedInsert::new(InsertId(700), Box::new(keyed)));
    devices.push(PreparedInsert::new(InsertId(900), Box::new(Comp::new(SR))));
    let mut util = Utility::new(SR);
    util.set_param(0, -6.0);
    devices.push(PreparedInsert::new(InsertId(901), Box::new(util)));
    let m = Mixer {
        tracks,
        sends,
        sidechains: vec![Sidechain {
            from: MixerTrackId(1),
            to: InsertId(700),
            tap: Tap::PreFader,
        }],
        sources,
    };
    let compiled = compile(&m, Budget::default()).expect("a valid fixture");
    let config = PrepareConfig {
        sample_rate: SR,
        max_frames: block,
        max_bytes: 1 << 20,
    };
    PreparedMixer::prepare(compiled, devices, &[], config).expect("prepared")
}

fn transport(frame: u64, frames: usize) -> HostTransport {
    HostTransport {
        domain: HostDomain::Song,
        frame,
        frames: frames as u32,
        sample_rate: SR,
        bpm: 120.0,
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

#[divan::bench(args = [64, 128])]
fn static_mixer_block(bencher: Bencher, block: usize) {
    let mut mixer = prepared(block);
    let ids: Vec<SourceId> = mixer.source_order().to_vec();
    let frames = block * BANK_BLOCKS;
    let bank: Vec<(Vec<f32>, Vec<f32>)> = (0..ids.len())
        .map(|k| {
            let f = 0.03 + k as f32 * 0.011;
            let l = (0..frames)
                .map(|i| libm::sinf(i as f32 * f) * 0.7)
                .collect();
            let r = (0..frames)
                .map(|i| libm::cosf(i as f32 * f * 0.9) * 0.6)
                .collect();
            (l, r)
        })
        .collect();
    let (mut out_l, mut out_r) = (vec![0.0f32; block], vec![0.0f32; block]);
    let mut frame = 0u64;
    // The per-call source table is a stack array: selecting the next bank block allocates nothing.
    let mut render = |mixer: &mut PreparedMixer, n: usize| {
        let at = (n % BANK_BLOCKS) * block;
        let sources: [SourceBlock; TRACKS as usize] = std::array::from_fn(|k| SourceBlock {
            id: ids[k],
            left: &bank[k].0[at..at + block],
            right: &bank[k].1[at..at + block],
        });
        let report = mixer
            .render(
                &transport(frame, block),
                black_box(&sources),
                &mut out_l,
                &mut out_r,
            )
            .expect("valid render");
        frame += block as u64;
        black_box((report, &out_l, &out_r));
    };
    for n in 0..WARMUP_BLOCKS {
        render(&mut mixer, n);
    }
    let mut n = WARMUP_BLOCKS;
    bencher.bench_local(|| {
        render(&mut mixer, n);
        n += 1;
    });
}
