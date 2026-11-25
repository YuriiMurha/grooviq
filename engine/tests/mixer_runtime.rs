//! The static serial mixer kernel against independent straight-line references: gather and
//! chain order, taps, detector-only sidechains, transport delivery, tails, reset, refusals
//! before state changes, and exact partition equivalence at every supported rate.

use grooviq_dsp::{Device, HostDomain, HostTransport, NoteEvent};
use grooviq_engine::mixer::*;
use std::sync::{Arc, Mutex};

const RATES: [f32; 5] = [22050.0, 44100.0, 48000.0, 96000.0, 192000.0];
const BLOCKS: [usize; 6] = [1, 7, 37, 64, 127, 128];

// ---------- minimal deterministic test processors ----------

/// `l*mul + add`, `r*mul - add`: order-sensitive and channel-distinct.
struct Affine(f32, f32);
impl Device for Affine {
    fn set_param(&mut self, index: u32, value: f32) {
        match index {
            0 => self.0 = value,
            _ => self.1 = value,
        }
    }
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        for x in l.iter_mut() {
            *x = *x * self.0 + self.1;
        }
        for x in r.iter_mut() {
            *x = *x * self.0 - self.1;
        }
    }
    fn reset(&mut self) {}
}

/// A leaky integrator: stateful, so a skipped, repeated or reset block is audible.
#[derive(Default)]
struct Leaky([f32; 2]);
impl Device for Leaky {
    fn set_param(&mut self, _: u32, _: f32) {}
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        for (c, buf) in [l, r].into_iter().enumerate() {
            for x in buf.iter_mut() {
                *x += self.0[c] * 0.5;
                self.0[c] = *x;
            }
        }
    }
    fn reset(&mut self) {
        self.0 = [0.0; 2];
    }
}

type Log = Arc<Mutex<Vec<Seen>>>;
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seen {
    Transport(u64, u32, f32),
    Tempo(f32),
    Process { frames: usize, detector: bool },
}

/// Adds a quarter of the detector left and an eighth of its right; logs what it was given.
struct Probe(Log);
impl Device for Probe {
    fn set_transport(&mut self, t: &HostTransport) {
        self.0
            .lock()
            .unwrap()
            .push(Seen::Transport(t.frame, t.frames, t.sample_rate));
    }
    fn set_tempo(&mut self, bpm: f32) {
        self.0.lock().unwrap().push(Seen::Tempo(bpm));
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        sc: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        self.0.lock().unwrap().push(Seen::Process {
            frames: l.len(),
            detector: sc.is_some(),
        });
        if let Some((a, b)) = sc {
            for i in 0..l.len() {
                l[i] += a[i] * 0.25;
                r[i] += b[i] * 0.125;
            }
        }
    }
    fn reset(&mut self) {}
}

/// Emits the absolute host frame of each sample (scaled): proves the facts match the span.
#[derive(Default)]
struct Clock {
    frame: u64,
    frames: u32,
}
impl Device for Clock {
    fn set_transport(&mut self, t: &HostTransport) {
        self.frame = t.frame;
        self.frames = t.frames;
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        assert_eq!(self.frames as usize, l.len());
        for i in 0..l.len() {
            let t = (self.frame + i as u64) as f32 * 1.0e-4;
            l[i] += t;
            r[i] -= t;
        }
    }
    fn reset(&mut self) {}
}

/// Writes NaN into its next block once armed through parameter 0.
#[derive(Default)]
struct Fault(bool);
impl Device for Fault {
    fn set_param(&mut self, _: u32, value: f32) {
        self.0 = value > 0.5;
    }
    fn process(
        &mut self,
        l: &mut [f32],
        _: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        if std::mem::take(&mut self.0) {
            l[0] = f32::NAN;
        }
    }
    fn reset(&mut self) {}
}

// ---------- fixtures ----------

fn track(id: u64, output: Option<u64>, inserts: &[u64]) -> Track {
    Track {
        id: MixerTrackId(id),
        output: output.map(MixerTrackId),
        inserts: inserts
            .iter()
            .map(|&i| Insert {
                id: InsertId(i),
                latency_frames: 0,
                memory_bytes: 0,
            })
            .collect(),
    }
}
fn source(id: u64, to: u64) -> Source {
    Source {
        id: SourceId::Channel(id),
        destination: MixerTrackId(to),
    }
}
fn config(sr: f32, max_frames: usize) -> PrepareConfig {
    PrepareConfig {
        sample_rate: sr,
        max_frames,
        max_bytes: 64 << 20,
    }
}
fn bind(devices: Vec<(u64, Box<dyn Device>)>) -> Vec<PreparedInsert> {
    devices
        .into_iter()
        .map(|(id, d)| PreparedInsert::new(InsertId(id), d))
        .collect()
}
fn gain(track: u64, left: f32, right: f32) -> TrackGain {
    TrackGain {
        track: MixerTrackId(track),
        left,
        right,
    }
}
fn prepare(
    m: &Mixer,
    devices: Vec<(u64, Box<dyn Device>)>,
    gains: &[TrackGain],
    cfg: PrepareConfig,
) -> PreparedMixer {
    let compiled = compile(m, Budget::default()).unwrap();
    PreparedMixer::prepare(compiled, bind(devices), gains, cfg).unwrap()
}
fn transport(sr: f32, frame: u64, frames: usize, bpm: f32) -> HostTransport {
    HostTransport {
        domain: HostDomain::Song,
        frame,
        frames: frames as u32,
        sample_rate: sr,
        bpm,
        playing: true,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 0,
        loop_end: 0,
        stream_generation: 0,
        discontinuity: 0,
        changed: frame == 0,
    }
}
/// Deterministic stereo signal `k`: bursts and tones, no randomness.
fn signal(k: u32, frames: usize) -> (Vec<f32>, Vec<f32>) {
    let f = 0.05 + k as f32 * 0.037;
    let l = (0..frames)
        .map(|i| {
            let burst = if (i / 300) % 2 == 0 { 0.9 } else { 0.2 };
            libm::sinf(i as f32 * f) * burst
        })
        .collect();
    let r = (0..frames)
        .map(|i| libm::cosf(i as f32 * f * 0.7) * 0.6)
        .collect();
    (l, r)
}
type Bank = Vec<(SourceId, Vec<f32>, Vec<f32>)>;
fn bank(m: &PreparedMixer, frames: usize) -> Bank {
    m.source_order()
        .iter()
        .enumerate()
        .map(|(k, id)| {
            let (l, r) = signal(k as u32, frames);
            (*id, l, r)
        })
        .collect()
}
fn blocks<'a>(bank: &'a Bank, at: usize, n: usize) -> Vec<SourceBlock<'a>> {
    bank.iter()
        .map(|(id, l, r)| SourceBlock {
            id: *id,
            left: &l[at..at + n],
            right: &r[at..at + n],
        })
        .collect()
}
/// Renders `frames` in spans of at most `block`, also splitting at every event frame, and
/// runs each event (caller-timed control) exactly at its boundary.
#[allow(
    clippy::too_many_arguments,
    reason = "test driver: every knob is a test axis"
)]
fn run(
    m: &mut PreparedMixer,
    bank: &Bank,
    sr: f32,
    frames: usize,
    block: usize,
    events: &mut dyn FnMut(&mut PreparedMixer, usize),
    bpm: f32,
    stops: &[usize],
) -> (Vec<f32>, Vec<f32>) {
    let (mut ol, mut or) = (vec![0.0; frames], vec![0.0; frames]);
    let mut at = 0;
    while at < frames {
        if stops.contains(&at) {
            events(m, at);
        }
        let next = stops
            .iter()
            .copied()
            .filter(|s| *s > at)
            .min()
            .unwrap_or(frames);
        let n = block.min(next - at).min(frames - at);
        let report = m
            .render(
                &transport(sr, at as u64, n, bpm),
                &blocks(bank, at, n),
                &mut ol[at..at + n],
                &mut or[at..at + n],
            )
            .unwrap();
        assert_eq!(report.frames as usize, n);
        at += n;
    }
    (ol, or)
}
fn plain(
    m: &mut PreparedMixer,
    bank: &Bank,
    sr: f32,
    frames: usize,
    block: usize,
) -> (Vec<f32>, Vec<f32>) {
    run(m, bank, sr, frames, block, &mut |_, _| {}, 120.0, &[])
}

// ---------- algebra ----------

/// Master 0 <- bus 3, track 1; bus 3 <- track 2 main, send 4 (2 post x0.25), send 8 (1 pre x0.5).
fn routed() -> Mixer {
    Mixer {
        tracks: vec![
            track(0, None, &[]),
            track(3, Some(0), &[31]),
            track(2, Some(3), &[21, 22]),
            track(1, Some(0), &[11]),
        ],
        sends: vec![
            Send {
                id: SendId(8),
                from: MixerTrackId(1),
                to: MixerTrackId(3),
                tap: Tap::PreFader,
                gain: 0.5,
            },
            Send {
                id: SendId(4),
                from: MixerTrackId(2),
                to: MixerTrackId(3),
                tap: Tap::PostFader,
                gain: 0.25,
            },
        ],
        sidechains: vec![],
        sources: vec![source(7, 1), source(2, 2)],
    }
}
fn routed_devices() -> Vec<(u64, Box<dyn Device>)> {
    vec![
        (11, Box::new(Affine(2.0, 0.0))),
        (21, Box::new(Affine(1.0, 0.1))),
        (22, Box::new(Affine(-0.5, 0.0))),
        (31, Box::new(Affine(0.5, 0.03))),
    ]
}
fn routed_gains() -> Vec<TrackGain> {
    vec![
        gain(1, 0.5, 0.25),
        gain(2, 0.75, -1.0),
        gain(3, 1.0, 0.5),
        gain(0, 0.9, 0.8),
    ]
}

#[test]
fn routed_graph_matches_a_straight_line_reference() {
    let sr = 48000.0;
    let mut m = prepare(
        &routed(),
        routed_devices(),
        &routed_gains(),
        config(sr, 128),
    );
    assert_eq!(
        m.source_order(),
        &[SourceId::Channel(2), SourceId::Channel(7)],
        "identity order, not destination or declaration order"
    );
    let frames = 600;
    let bank = bank(&m, frames);
    let (ol, or) = plain(&mut m, &bank, sr, frames, 64);

    let (s2, s7) = (&bank[0], &bank[1]);
    for i in 0..frames {
        for (c, sign, out) in [(0, 1.0f32, ol[i]), (1, -1.0, or[i])] {
            let pick = |s: &(SourceId, Vec<f32>, Vec<f32>)| if c == 0 { s.1[i] } else { s.2[i] };
            let g = |l: f32, r: f32| if c == 0 { l } else { r };
            let t1_pre = (0.0 + pick(s7) * 1.0) * 2.0 + 0.0 * sign;
            let t1_post = t1_pre * g(0.5, 0.25);
            let t2_pre = ((0.0 + pick(s2) * 1.0) * 1.0 + 0.1 * sign) * -0.5 + 0.0 * sign;
            let t2_post = t2_pre * g(0.75, -1.0);
            let t3_in = 0.0 + t2_post * 1.0 + t2_post * 0.25 + t1_pre * 0.5;
            let t3_post = (t3_in * 0.5 + 0.03 * sign) * g(1.0, 0.5);
            let master = (0.0 + t1_post * 1.0 + t3_post * 1.0) * g(0.9, 0.8);
            assert_eq!(out, master, "frame {i} channel {c}");
        }
    }
    let dry: Vec<f32> = (0..frames).map(|i| s2.1[i] + s7.1[i]).collect();
    assert_ne!(ol, dry);
    assert!(ol.iter().any(|x| x.abs() > 0.05));
}

#[test]
fn inserts_run_in_vector_order_not_identity_order() {
    let sr = 44100.0;
    let m = Mixer {
        tracks: vec![track(0, None, &[]), track(1, Some(0), &[30, 10])],
        sources: vec![source(1, 1)],
        ..Mixer::default()
    };
    let make = || -> Vec<(u64, Box<dyn Device>)> {
        vec![
            (30, Box::new(Affine(3.0, 0.0))),
            (10, Box::new(Affine(1.0, 0.2))),
        ]
    };
    let mut k = prepare(&m, make(), &[], config(sr, 64));
    let bank = bank(&k, 128);
    let (ol, _) = plain(&mut k, &bank, sr, 128, 64);
    for i in 0..128 {
        let x = bank[0].1[i];
        assert_eq!(ol[i], (x * 3.0 + 0.0) * 1.0 + 0.2);
        assert_ne!(ol[i], (x * 1.0 + 0.2) * 3.0 + 0.0);
    }
}

#[test]
fn sidechains_feed_only_their_insert_detector_with_the_named_tap() {
    let sr = 48000.0;
    // Track 1: probe 11 (no sidechain), probe 12 (sidechain from 2 post, from 3 pre).
    let m = Mixer {
        tracks: vec![
            track(0, None, &[]),
            track(1, Some(0), &[11, 12]),
            track(2, Some(0), &[21]),
            track(3, Some(0), &[]),
        ],
        sidechains: vec![
            Sidechain {
                from: MixerTrackId(3),
                to: InsertId(12),
                tap: Tap::PreFader,
            },
            Sidechain {
                from: MixerTrackId(2),
                to: InsertId(12),
                tap: Tap::PostFader,
            },
        ],
        sources: vec![source(1, 1), source(2, 2), source(3, 3)],
        ..Mixer::default()
    };
    let (a, b) = (Log::default(), Log::default());
    let devices: Vec<(u64, Box<dyn Device>)> = vec![
        (11, Box::new(Probe(a.clone()))),
        (12, Box::new(Probe(b.clone()))),
        (21, Box::new(Affine(0.5, 0.0))),
    ];
    let gains = [gain(2, 0.25, 0.5), gain(3, 2.0, 3.0)];
    let mut k = prepare(&m, devices, &gains, config(sr, 128));
    let frames = 256;
    let bank = bank(&k, frames);
    let (ol, or) = plain(&mut k, &bank, sr, frames, 128);
    for i in 0..frames {
        let (s1, s2, s3) = (&bank[0], &bank[1], &bank[2]);
        // Detector feeds in compiled order: Sidechain{from 2} before Sidechain{from 3}.
        let det_l = 0.0 + (s2.1[i] * 0.5) * 0.25 * 1.0 + s3.1[i] * 1.0;
        let det_r = 0.0 + (s2.2[i] * 0.5 - 0.0) * 0.5 * 1.0 + s3.2[i] * 1.0;
        let t1_l = (0.0 + s1.1[i] * 1.0) + det_l * 0.25;
        let t1_r = (0.0 + s1.2[i] * 1.0) + det_r * 0.125;
        let t2_l = (0.0 + s2.1[i] * 1.0) * 0.5 + 0.0;
        let t2_r = (0.0 + s2.2[i] * 1.0) * 0.5 - 0.0;
        let want_l = 0.0 + t1_l * 1.0 + (t2_l * 0.25) * 1.0 + (s3.1[i] * 2.0) * 1.0;
        let want_r = 0.0 + t1_r * 1.0 + (t2_r * 0.5) * 1.0 + (s3.2[i] * 3.0) * 1.0;
        assert_eq!((ol[i], or[i]), (want_l, want_r), "frame {i}");
    }
    let a = a.lock().unwrap();
    let b = b.lock().unwrap();
    assert!(
        a.iter()
            .all(|s| !matches!(s, Seen::Process { detector: true, .. }))
    );
    assert!(
        b.iter()
            .filter(|s| matches!(s, Seen::Process { .. }))
            .all(|s| matches!(s, Seen::Process { detector: true, .. }))
    );
}

#[test]
fn a_built_in_compressor_hears_its_sidechain_but_the_mix_does_not() {
    use grooviq_dsp::comp::Comp;
    let sr = 48000.0;
    let m = Mixer {
        tracks: vec![
            track(0, None, &[]),
            track(1, Some(0), &[11]),
            track(2, Some(0), &[]),
        ],
        sidechains: vec![Sidechain {
            from: MixerTrackId(2),
            to: InsertId(11),
            tap: Tap::PreFader,
        }],
        sources: vec![source(1, 1), source(2, 2)],
        ..Mixer::default()
    };
    let comp = || {
        let mut c = Comp::new(sr);
        for (i, v) in [(0, -36.0), (1, 10.0), (2, 1.0), (3, 60.0), (7, 1.0)] {
            c.set_param(i, v);
        }
        c
    };
    let mut k = prepare(
        &m,
        vec![(11, Box::new(comp()))],
        &[gain(2, 0.0, 0.0)],
        config(sr, 128),
    );
    let frames = 4096;
    let bank = bank(&k, frames);
    let (ol, or) = plain(&mut k, &bank, sr, frames, 128);
    let (mut keyed, mut internal) = (comp(), comp());
    let (mut rl, mut rr) = (bank[0].1.clone(), bank[0].2.clone());
    let (mut il, mut ir) = (rl.clone(), rr.clone());
    for at in (0..frames).step_by(128) {
        let s = at..at + 128;
        keyed.process(
            &mut rl[s.clone()],
            &mut rr[s.clone()],
            Some((&bank[1].1[s.clone()], &bank[1].2[s.clone()])),
            &[],
        );
        internal.process(&mut il[s.clone()], &mut ir[s], None, &[]);
    }
    // Track 2 is fully faded: only track 1 (keyed by track 2's pre-fader tap) is audible.
    let want_l: Vec<f32> = rl.iter().map(|x| 0.0 + x * 1.0 + 0.0).collect();
    let want_r: Vec<f32> = rr.iter().map(|x| 0.0 + x * 1.0 + 0.0).collect();
    assert_eq!(ol, want_l);
    assert_eq!(or, want_r);
    assert_ne!(ol, il, "the detector input changed the gain reduction");
    assert_ne!(ol, bank[0].1, "the compressor processed audio");
}

// ---------- transport, tails, reset ----------

#[test]
fn every_insert_gets_the_exact_span_facts_before_processing() {
    let sr = 96000.0;
    let m = Mixer {
        tracks: vec![track(0, None, &[2]), track(1, Some(0), &[1])],
        sources: vec![source(1, 1)],
        ..Mixer::default()
    };
    let (a, b) = (Log::default(), Log::default());
    let mut k = prepare(
        &m,
        vec![
            (1, Box::new(Probe(a.clone()))),
            (2, Box::new(Probe(b.clone()))),
        ],
        &[],
        config(sr, 128),
    );
    let bank = bank(&k, 200);
    let empty = blocks(&bank, 0, 0);
    let zero = k.render(&transport(sr, 0, 0, 97.5), &empty, &mut [], &mut []);
    assert_eq!(zero, Ok(RenderReport::default()));
    assert!(
        a.lock().unwrap().is_empty(),
        "a zero-frame call reaches no device"
    );
    run(&mut k, &bank, sr, 200, 128, &mut |_, _| {}, 97.5, &[37]);
    let want = vec![
        Seen::Transport(0, 37, sr),
        Seen::Tempo(97.5),
        Seen::Process {
            frames: 37,
            detector: false,
        },
        Seen::Transport(37, 128, sr),
        Seen::Tempo(97.5),
        Seen::Process {
            frames: 128,
            detector: false,
        },
        Seen::Transport(165, 35, sr),
        Seen::Tempo(97.5),
        Seen::Process {
            frames: 35,
            detector: false,
        },
    ];
    assert_eq!(*a.lock().unwrap(), want);
    assert_eq!(*b.lock().unwrap(), want);

    // The facts are the render's own span, sample by sample.
    let mut c = prepare(
        &m,
        vec![
            (1, Box::new(Clock::default())),
            (2, Box::new(Affine(1.0, 0.0))),
        ],
        &[],
        config(sr, 128),
    );
    let (ol, _) = run(
        &mut c,
        &bank,
        sr,
        200,
        128,
        &mut |_, _| {},
        120.0,
        &[37, 50],
    );
    for i in 0..200 {
        assert_eq!(
            ol[i],
            (0.0 + bank[0].1[i] * 1.0 + i as f32 * 1.0e-4) * 1.0 + 0.0
        );
    }
}

#[test]
fn host_tempo_reaches_a_synced_built_in_delay() {
    use grooviq_dsp::delay::Delay;
    let sr = 48000.0;
    let m = Mixer {
        tracks: vec![track(0, None, &[]), track(1, Some(0), &[1])],
        sources: vec![source(1, 1)],
        ..Mixer::default()
    };
    let render = |bpm: f32| {
        let mut k = prepare(
            &m,
            vec![(1, Box::new(Delay::new(sr)))],
            &[],
            config(sr, 128),
        );
        let bank = bank(&k, 24064);
        let out = run(&mut k, &bank, sr, 24064, 128, &mut |_, _| {}, bpm, &[]).0;
        let mut d = Delay::new(sr);
        let (mut l, mut r) = (bank[0].1.clone(), bank[0].2.clone());
        for at in (0..24064).step_by(128) {
            d.set_tempo(bpm);
            d.process(&mut l[at..at + 128], &mut r[at..at + 128], None, &[]);
        }
        (out, l)
    };
    let (fast, fast_ref) = render(180.0);
    let (slow, slow_ref) = render(70.0);
    assert_eq!(fast, fast_ref);
    assert_eq!(slow, slow_ref);
    assert_ne!(fast, slow);
}

#[test]
fn zero_input_keeps_effect_tails_running() {
    use grooviq_dsp::reverb::Reverb;
    let sr = 44100.0;
    let m = Mixer {
        tracks: vec![track(0, None, &[]), track(1, Some(0), &[1])],
        sources: vec![source(1, 1)],
        ..Mixer::default()
    };
    let mut k = prepare(
        &m,
        vec![(1, Box::new(Reverb::new(sr)))],
        &[],
        config(sr, 64),
    );
    let frames = 8192;
    let mut b = bank(&k, frames);
    b[0].1[512..].fill(0.0);
    b[0].2[512..].fill(0.0);
    let (ol, _) = plain(&mut k, &b, sr, frames, 64);
    let mut d = Reverb::new(sr);
    let (mut l, mut r) = (b[0].1.clone(), b[0].2.clone());
    for at in (0..frames).step_by(64) {
        d.process(&mut l[at..at + 64], &mut r[at..at + 64], None, &[]);
    }
    assert_eq!(ol, l);
    assert!(
        ol[4096..].iter().any(|x| x.abs() > 1.0e-4),
        "the tail kept sounding"
    );
}

#[test]
fn reset_returns_devices_and_taps_to_their_prepared_state() {
    let sr = 22050.0;
    let make = || {
        prepare(
            &routed_leaky(),
            leaky_devices(),
            &routed_gains(),
            config(sr, 128),
        )
    };
    let mut used = make();
    let bank = bank(&used, 900);
    plain(&mut used, &bank, sr, 900, 37);
    used.reset();
    let after = plain(&mut used, &bank, sr, 900, 64);
    let fresh = plain(&mut make(), &bank, sr, 900, 64);
    assert_eq!(after, fresh);
}

fn routed_leaky() -> Mixer {
    routed()
}
fn leaky_devices() -> Vec<(u64, Box<dyn Device>)> {
    vec![
        (11, Box::new(Leaky::default())),
        (21, Box::new(Affine(1.0, 0.1))),
        (22, Box::new(Leaky::default())),
        (31, Box::new(Leaky::default())),
    ]
}

// ---------- refusals ----------

#[test]
fn malformed_render_calls_change_nothing() {
    let sr = 48000.0;
    let make = || {
        prepare(
            &routed_leaky(),
            leaky_devices(),
            &routed_gains(),
            config(sr, 64),
        )
    };
    let mut k = make();
    let bank = bank(&k, 65);
    let good = blocks(&bank, 0, 64);
    let mut long = good.clone();
    long[0].left = &bank[0].1[..65];
    let (mut ol, mut or) = ([7.0f32; 64], [7.0f32; 64]);
    let t = transport(sr, 0, 64, 120.0);
    let short = blocks(&bank, 0, 63);
    let mut swapped = good.clone();
    swapped.swap(0, 1);
    let mut uneven = good.clone();
    uneven[1].right = &bank[1].2[..63];

    let cases: [(HostTransport, &[SourceBlock], usize, RenderError); 8] = [
        (
            transport(sr, 0, 65, 120.0),
            &good,
            64,
            RenderError::TooManyFrames {
                frames: 65,
                max: 64,
            },
        ),
        (
            transport(44100.0, 0, 64, 120.0),
            &good,
            64,
            RenderError::SampleRate,
        ),
        (t, &good, 63, RenderError::OutputCapacity),
        (
            t,
            &good[..1],
            64,
            RenderError::SourceCount {
                expected: 2,
                got: 1,
            },
        ),
        (t, &swapped, 64, RenderError::SourceOrder { index: 0 }),
        (t, &short, 64, RenderError::SourceLength { index: 0 }),
        (t, &uneven, 64, RenderError::SourceLength { index: 1 }),
        (t, &long, 64, RenderError::SourceLength { index: 0 }),
    ];
    for (t, sources, cap, want) in cases {
        let got = k.render(&t, sources, &mut ol[..cap], &mut or[..cap]);
        assert_eq!(got, Err(want));
        assert_eq!(ol, [7.0; 64], "refused output untouched");
    }
    let empty: [SourceBlock; 2] = [
        SourceBlock {
            id: SourceId::Channel(2),
            left: &[],
            right: &[],
        },
        SourceBlock {
            id: SourceId::Channel(7),
            left: &[],
            right: &[],
        },
    ];
    assert_eq!(
        k.render(&transport(sr, 0, 0, 120.0), &empty, &mut [], &mut []),
        Ok(RenderReport::default())
    );
    assert_eq!(
        k.render(&transport(sr, 0, 0, 120.0), &empty[..1], &mut [], &mut []),
        Err(RenderError::SourceCount {
            expected: 2,
            got: 1
        })
    );

    let report = k.render(&t, &good, &mut ol, &mut or).unwrap();
    assert_eq!(
        report,
        RenderReport {
            frames: 64,
            silenced: 0
        }
    );
    let (mut fl, mut fr) = ([0.0f32; 64], [0.0f32; 64]);
    make().render(&t, &good, &mut fl, &mut fr).unwrap();
    assert_eq!(
        (ol, or),
        (fl, fr),
        "no refusal or empty call advanced any state"
    );
    assert_ne!(ol, [7.0; 64]);
}

#[test]
fn prepare_refuses_bad_bindings_gains_configs_and_plans() {
    let sr = 48000.0;
    let compiled = || compile(&routed(), Budget::default()).unwrap();
    let try_prepare = |c: CompiledMixer, d: Vec<(u64, Box<dyn Device>)>, g: &[TrackGain], cfg| {
        PreparedMixer::prepare(c, bind(d), g, cfg).err()
    };
    let mut missing = routed_devices();
    missing.pop();
    assert_eq!(
        try_prepare(compiled(), missing, &[], config(sr, 64)),
        Some(PrepareError::MissingInsert(InsertId(31)))
    );
    let mut extra = routed_devices();
    extra.push((99, Box::new(Leaky::default())));
    assert_eq!(
        try_prepare(compiled(), extra, &[], config(sr, 64)),
        Some(PrepareError::UnknownInsert(InsertId(99)))
    );
    let mut twice = routed_devices();
    twice.push((21, Box::new(Leaky::default())));
    assert_eq!(
        try_prepare(compiled(), twice, &[], config(sr, 64)),
        Some(PrepareError::DuplicateBinding(InsertId(21)))
    );
    for (g, want) in [
        (
            vec![gain(9, 1.0, 1.0)],
            PrepareError::UnknownGain(MixerTrackId(9)),
        ),
        (
            vec![gain(1, 1.0, 1.0), gain(1, 0.5, 0.5)],
            PrepareError::DuplicateGain(MixerTrackId(1)),
        ),
        (
            vec![gain(2, f32::NAN, 1.0)],
            PrepareError::InvalidGain(MixerTrackId(2)),
        ),
        (
            vec![gain(2, 1.0, f32::INFINITY)],
            PrepareError::InvalidGain(MixerTrackId(2)),
        ),
    ] {
        assert_eq!(
            try_prepare(compiled(), routed_devices(), &g, config(sr, 64)),
            Some(want)
        );
    }
    for (cfg, want) in [
        (config(sr, 0), PrepareError::InvalidMaxFrames),
        (config(sr, 129), PrepareError::InvalidMaxFrames),
        (config(0.0, 64), PrepareError::InvalidSampleRate),
        (config(f32::NAN, 64), PrepareError::InvalidSampleRate),
    ] {
        assert_eq!(
            try_prepare(compiled(), routed_devices(), &[], cfg),
            Some(want)
        );
    }
    let tight = PrepareConfig {
        max_bytes: 1024,
        ..config(sr, 64)
    };
    assert!(matches!(
        try_prepare(compiled(), routed_devices(), &[], tight),
        Some(PrepareError::Budget { limit: 1024, .. })
    ));

    // Hand-edited plans are checked, not trusted.
    let mut c = compiled();
    c.levels.reverse();
    assert_eq!(
        try_prepare(c, routed_devices(), &[], config(sr, 64)),
        Some(PrepareError::Plan(PlanError::MasterNotLast))
    );
    let mut c = compiled();
    let flat: Vec<_> = c.levels.concat();
    c.levels = vec![flat[..flat.len() - 1].to_vec(), vec![MASTER]];
    assert!(matches!(
        try_prepare(c, routed_devices(), &[], config(sr, 64)),
        Some(PrepareError::Plan(PlanError::NotEarlier(_)))
    ));
    let mut c = compiled();
    let bus = c
        .tracks
        .iter_mut()
        .find(|t| t.track.id == MixerTrackId(3))
        .unwrap();
    bus.inputs.reverse();
    assert_eq!(
        try_prepare(c, routed_devices(), &[], config(sr, 64)),
        Some(PrepareError::Plan(PlanError::UnsortedInputs(MixerTrackId(
            3
        ))))
    );
    let mut c = compiled();
    c.levels[0].pop();
    assert!(matches!(
        try_prepare(c, routed_devices(), &[], config(sr, 64)),
        Some(PrepareError::Plan(PlanError::UnscheduledTrack(_)))
    ));
    let mut c = compiled();
    let dup = c
        .tracks
        .iter()
        .find(|t| t.track.id == MixerTrackId(1))
        .unwrap()
        .inputs[0]
        .clone();
    c.tracks
        .iter_mut()
        .find(|t| t.track.id == MixerTrackId(2))
        .unwrap()
        .inputs
        .push(dup);
    assert_eq!(
        try_prepare(c, routed_devices(), &[], config(sr, 64)),
        Some(PrepareError::Plan(PlanError::DuplicateSource(
            SourceId::Channel(7)
        )))
    );
    let mut c = compiled();
    let master = c.tracks.iter_mut().find(|t| t.track.id == MASTER).unwrap();
    master.inputs.push(Input {
        id: InputId::Sidechain {
            from: MixerTrackId(1),
            insert: InsertId(21),
            tap: Tap::PreFader,
        },
        from: Some(MixerTrackId(1)),
        tap: Tap::PreFader,
        gain: 1.0,
    });
    assert_eq!(
        try_prepare(c, routed_devices(), &[], config(sr, 64)),
        Some(PrepareError::Plan(PlanError::ForeignInsert(InsertId(21))))
    );

    let k = prepare(&routed(), routed_devices(), &[], config(sr, 64));
    let res = k.resources();
    assert_eq!(
        res.audio_bytes,
        (4 * 4 + 4) * 64 * 4,
        "four taps per node plus work and detector pairs"
    );
    assert!(res.table_bytes > 0);
}

#[test]
fn keyed_parameters_reach_only_their_insert() {
    let sr = 48000.0;
    let mut k = prepare(&routed(), routed_devices(), &[], config(sr, 64));
    assert_eq!(
        k.set_param(InsertId(5), 0, 1.0),
        Err(ParamError::UnknownInsert(InsertId(5)))
    );
    let mut r = prepare(&routed(), routed_devices(), &[], config(sr, 64));
    k.set_param(InsertId(31), 1, 0.25).unwrap();
    let bank = bank(&k, 64);
    let a = plain(&mut k, &bank, sr, 64, 64);
    let b = plain(&mut r, &bank, sr, 64, 64);
    assert_ne!(a, b);
    // Bus 3 post (gain 1) reaches the master directly: the offset shifts left by exactly 0.22.
    for i in 0..64 {
        assert!((a.0[i] - b.0[i] - 0.22).abs() < 1.0e-6);
    }
}

// ---------- faults ----------

#[test]
fn a_non_finite_node_is_silenced_and_reset_while_healthy_nodes_continue() {
    let sr = 48000.0;
    let m = Mixer {
        tracks: vec![
            track(0, None, &[]),
            track(1, Some(0), &[11, 12]),
            track(2, Some(0), &[21]),
        ],
        sources: vec![source(1, 1), source(2, 2)],
        ..Mixer::default()
    };
    let make = || {
        let d: Vec<(u64, Box<dyn Device>)> = vec![
            (11, Box::new(Leaky::default())),
            (12, Box::new(Fault::default())),
            (21, Box::new(Leaky::default())),
        ];
        prepare(&m, d, &[], config(sr, 64))
    };
    let mut k = make();
    let bank = bank(&k, 256);
    let mut out = (vec![0.0; 256], vec![0.0; 256]);
    let mut reports = vec![];
    for (b, at) in (0..256).step_by(64).enumerate() {
        if b == 2 {
            k.set_param(InsertId(12), 0, 1.0).unwrap();
        }
        let t = transport(sr, at as u64, 64, 120.0);
        reports.push(
            k.render(
                &t,
                &blocks(&bank, at, 64),
                &mut out.0[at..at + 64],
                &mut out.1[at..at + 64],
            )
            .unwrap()
            .silenced,
        );
    }
    assert_eq!(reports, [0, 0, 1, 0]);
    // Track 2 alone (unity) is the healthy reference for the faulted block.
    let mut t2 = Leaky::default();
    let (mut l2, mut r2) = (bank[1].1.clone(), bank[1].2.clone());
    for at in (0..256).step_by(64) {
        t2.process(&mut l2[at..at + 64], &mut r2[at..at + 64], None, &[]);
    }
    for i in 128..192 {
        assert_eq!(out.0[i], 0.0 + 0.0 * 1.0 + l2[i] * 1.0, "frame {i}");
    }
    // After the fault, track 1's integrator restarted from zero.
    let mut t1 = Leaky::default();
    let (mut l1, mut r1) = (bank[0].1[192..].to_vec(), bank[0].2[192..].to_vec());
    t1.process(&mut l1, &mut r1, None, &[]);
    for i in 0..64 {
        assert_eq!(out.0[192 + i], 0.0 + l1[i] * 1.0 + l2[192 + i] * 1.0);
    }
    let healthy = plain(&mut make(), &bank, sr, 256, 64);
    assert_eq!(out.0[..128], healthy.0[..128]);
    assert_ne!(out.0[128..], healthy.0[128..]);
}

// ---------- partitions ----------

type Devices = Vec<(u64, Box<dyn Device>)>;
fn built_in_graph(sr: f32) -> (Mixer, Devices, Vec<TrackGain>) {
    use grooviq_dsp::{comp::Comp, delay::Delay, saturation::Saturation, utility::Utility};
    let m = Mixer {
        tracks: vec![
            track(0, None, &[4]),
            track(1, Some(3), &[1]),
            track(2, Some(0), &[2, 5]),
            track(3, Some(0), &[3]),
        ],
        sends: vec![Send {
            id: SendId(1),
            from: MixerTrackId(2),
            to: MixerTrackId(3),
            tap: Tap::PreFader,
            gain: 0.35,
        }],
        sidechains: vec![Sidechain {
            from: MixerTrackId(2),
            to: InsertId(1),
            tap: Tap::PostFader,
        }],
        sources: vec![source(1, 1), source(2, 2), source(3, 3)],
    };
    let mut comp = Comp::new(sr);
    for (i, v) in [(0, -30.0), (1, 6.0), (7, 1.0)] {
        comp.set_param(i, v);
    }
    let mut sat = Saturation::new(sr);
    for (i, v) in [0., 12., 0.2, 3., 45., 16000., 1., 0.85, 0.]
        .into_iter()
        .enumerate()
    {
        sat.set_param(i as u32, v);
    }
    let mut util = Utility::new(sr);
    util.set_param(0, -3.0);
    util.set_param(2, 1.4);
    let devices: Vec<(u64, Box<dyn Device>)> = vec![
        (1, Box::new(comp)),
        (2, Box::new(Delay::new(sr))),
        (5, Box::new(sat)),
        (3, Box::new(Leaky::default())),
        (4, Box::new(util)),
    ];
    (m, devices, vec![gain(2, 0.7, 0.6), gain(3, 0.5, 0.5)])
}

#[test]
fn healthy_partitions_are_exact_at_every_rate_and_block_with_caller_timed_control() {
    let frames = 2400;
    let stops = [700, 1100, 1500];
    for sr in RATES {
        let mut events = |m: &mut PreparedMixer, at: usize| match at {
            700 => m.set_param(InsertId(1), 0, -12.0).unwrap(),
            1100 => m.reset(),
            1500 => m.set_param(InsertId(5), 1, 6.0).unwrap(),
            _ => unreachable!(),
        };
        let mut outputs = vec![];
        for block in BLOCKS {
            let (m, d, g) = built_in_graph(sr);
            let mut k = prepare(&m, d, &g, config(sr, 128));
            let bank = bank(&k, frames);
            outputs.push(run(
                &mut k,
                &bank,
                sr,
                frames,
                block,
                &mut events,
                128.0,
                &stops,
            ));
        }
        for (b, o) in BLOCKS.iter().zip(&outputs) {
            assert_eq!(o, &outputs[0], "{sr} Hz, block {b} differs from block 1");
        }
        let (m, d, g) = built_in_graph(sr);
        let bank = bank(&prepare(&m, d, &g, config(sr, 128)), frames);
        let dry: Vec<f32> = (0..frames)
            .map(|i| bank.iter().map(|s| s.1[i]).sum())
            .collect();
        assert_ne!(outputs[0].0, dry);
        assert!(outputs[0].0.iter().all(|x| x.is_finite()));
        assert!(outputs[0].0[1100..].iter().any(|x| x.abs() > 0.01));
    }
}
