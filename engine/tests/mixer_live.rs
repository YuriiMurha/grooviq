//! Live mixer installs against independent references: every source is rendered alone
//! through LegacyDirect (where the master is exactly that source), then summed and processed
//! by a hand-written chain of fresh devices in the documented canonical order.

use grooviq_dsp::Device;
use grooviq_engine::mixer::*;
use grooviq_engine::*;
use std::sync::Arc;

const SR: f32 = 48_000.0;
const BLOCKS: usize = 140;
const N: usize = BLOCK;

/// Which sources a render keeps. `All` is the full scene.
#[derive(Clone, Copy, PartialEq)]
enum Only {
    All,
    Channel(u32),
    Clip(u32),
    Synth,
}

fn asset() -> AudioAsset {
    // Asymmetric, non-periodic, deterministic.
    let l = (0u64..96_000)
        .map(|i| ((i * 7919 % 2003) as f32 / 2003.0 - 0.5) * 0.4)
        .collect();
    let r = (0u64..96_000)
        .map(|i| ((i * 104_729 % 1999) as f32 / 1999.0 - 0.3) * 0.25)
        .collect();
    AudioAsset {
        id: 1,
        source_rate: 48_000,
        source_frames: 96_000,
        decoded: Some(Arc::new(SampleBuf {
            rate: 48_000.0,
            l,
            r,
        })),
    }
}

fn clip(id: u32, start_tick: u64, source: (u64, u64), gain: f32, reverse: bool) -> AudioClip {
    AudioClip {
        id,
        asset: 1,
        start_tick,
        source_start: source.0,
        source_end: source.1,
        gain,
        reverse,
        fade_in: 64,
        fade_out: 256,
    }
}

/// Shuffled clip IDs on two playlist tracks, sharing one asset.
fn clips() -> Vec<(AudioClip, u64)> {
    vec![
        (clip(20, 0, (0, 40_000), 0.9, false), 5),
        (clip(11, 120, (1_000, 50_000), 0.7, true), 2),
        (clip(5, 200, (3_000, 30_000), 1.1, false), 5),
    ]
}

fn audio(only: Only, routes: &[(u32, u64)]) -> AudioSchedule {
    let kept: Vec<_> = clips()
        .into_iter()
        .filter(|(c, _)| only == Only::All || only == Only::Clip(c.id))
        .map(|(c, _)| c)
        .collect();
    AudioSchedule::build(vec![asset()], kept)
        .unwrap()
        .0
        .with_tracks(routes)
        .unwrap()
}

fn routes() -> Vec<(u32, u64)> {
    clips().iter().map(|(c, t)| (c.id, *t)).collect()
}

fn sample() -> Arc<SampleBuf> {
    Arc::new(SampleBuf {
        rate: 44_100.0,
        l: (0..30_000)
            .map(|i| ((i % 97) as f32 / 97.0 - 0.5) * (1.0 - i as f32 / 30_000.0))
            .collect(),
        r: (0..30_000)
            .map(|i| ((i % 61) as f32 / 61.0 - 0.4) * 0.6)
            .collect(),
    })
}

fn state(only: Only, mix: MixSetup) -> EngineState {
    let mut channels = Vec::new();
    if matches!(only, Only::All | Only::Channel(7)) {
        channels.push(ChannelSetup::synth(
            ChannelId(7),
            Vec::new(),
            ChannelMix::default(),
        ));
    }
    if matches!(only, Only::All | Only::Channel(3)) {
        channels.push(ChannelSetup::sampler(
            ChannelId(3),
            Some(sample()),
            Vec::new(),
            ChannelMix {
                volume: 0.8,
                pan: 0.3,
                ..ChannelMix::default()
            },
        ));
    }
    EngineState {
        bpm: 120.0,
        playback_mode: PlaybackMode::Song,
        song: SongData {
            audio: audio(only, &routes()),
            end_tick: 100_000,
            end_frame: 2_500_000,
            frame_rate: SR,
            frame_extent: None,
            ..SongData::default()
        },
        channels,
        mix,
        ..EngineState::default()
    }
}

/// The scene's live events at block `b`. Commands a filtered scene cannot take (an absent
/// channel) are skipped; the tone is only on in the full scene (and is never routed).
fn events(b: usize, c: &mut Controller, only: Only) {
    let note = |channel, key, on, id| Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.8,
        on,
        id: NoteId(id),
    };
    let _ = match b {
        0 => c.send(Command::Play).map_err(|_| ()),
        1 if only == Only::All => c
            .send(Command::TestTone {
                on: true,
                hz: 440.0,
                gain_db: -6.0,
            })
            .map_err(|_| ()),
        2 => c.send(note(7, 60, true, 1)).map_err(|_| ()),
        4 => c.send(note(3, 64, true, 2)).map_err(|_| ()),
        6 if matches!(only, Only::All | Only::Synth) => c
            .send(Command::Note {
                key: 67,
                velocity: 0.9,
                on: true,
                id: NoteId(3),
            })
            .map_err(|_| ()),
        // Held across every install below; released late.
        110 => c.send(note(7, 60, false, 1)).map_err(|_| ()),
        112 if matches!(only, Only::All | Only::Synth) => c
            .send(Command::Note {
                key: 67,
                velocity: 0.0,
                on: false,
                id: NoteId(3),
            })
            .map_err(|_| ()),
        _ => Ok(()),
    };
}

fn render(
    only: Only,
    mix: MixSetup,
    frames: impl Fn(usize) -> usize,
    mut hook: impl FnMut(usize, &mut Controller),
) -> Vec<f32> {
    let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state(only, mix));
    let mut out = Vec::with_capacity(BLOCKS * N * 2);
    let mut b = 0;
    let mut done = 0;
    while done < BLOCKS * N {
        if done >= b * N {
            events(b, &mut c, only);
            hook(b, &mut c);
            b += 1;
        }
        let want = frames(done).min(BLOCKS * N - done).min(b * N - done).max(1);
        let mut buf = vec![0.0; want * 2];
        p.process(&mut buf, 2);
        out.extend_from_slice(&buf);
        done += want;
        let _ = c.check_health();
    }
    out
}

fn blocks(only: Only, hook: impl FnMut(usize, &mut Controller)) -> Vec<f32> {
    render(only, MixSetup::LegacyDirect, |_| N, hook)
}

fn same_bits(a: &[f32], b: &[f32]) -> Option<usize> {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .position(|(x, y)| x.to_bits() != y.to_bits())
}

fn track(id: u64, output: Option<u64>, inserts: &[(u64, u32)]) -> Track {
    Track {
        id: MixerTrackId(id),
        output: output.map(MixerTrackId),
        inserts: inserts
            .iter()
            .map(|&(i, latency)| Insert {
                id: InsertId(i),
                latency_frames: latency,
                memory_bytes: 0,
            })
            .collect(),
    }
}

fn defaults(kind: &str) -> Vec<f32> {
    grooviq_dsp::param_spec(kind)
        .unwrap()
        .iter()
        .map(|p| p.2)
        .collect()
}

fn comp_params(threshold: f32) -> Vec<f32> {
    let mut p = defaults("comp");
    p[0] = threshold;
    p[7] = 1.0; // external sidechain
    p
}

/// T1: channel 3 + playlist track 2, seeded algorithmic reverb.
/// T2: channel 7 + playlist track 5 + the synth, compressor keyed from T1 pre-fader.
/// T3: reverb return fed by a post-fader send from T2.
/// The tone and channel/track IDs the mix does not name are never heard.
fn routed(master: f32, threshold: f32, eq: bool) -> MixSetup {
    let mut t3 = vec![(30, 0)];
    if eq {
        t3.push((40, 0));
    }
    let mut devices = vec![
        InsertDevice {
            insert: InsertId(10),
            kind: "algorithmic_reverb".into(),
            seed: 16_384,
            params: Vec::new(),
        },
        InsertDevice {
            insert: InsertId(20),
            kind: "comp".into(),
            seed: 0,
            params: comp_params(threshold),
        },
        InsertDevice {
            insert: InsertId(30),
            kind: "reverb".into(),
            seed: 0,
            params: Vec::new(),
        },
    ];
    if eq {
        devices.push(InsertDevice {
            insert: InsertId(40),
            kind: "eq".into(),
            seed: 0,
            params: Vec::new(),
        });
    }
    MixSetup::Routed(RoutedMix {
        mixer: Mixer {
            tracks: vec![
                track(0, None, &[]),
                track(1, Some(0), &[(10, 0)]),
                track(2, Some(0), &[(20, 0)]),
                track(3, Some(0), &t3),
            ],
            sends: vec![Send {
                id: SendId(1),
                from: MixerTrackId(2),
                to: MixerTrackId(3),
                tap: Tap::PostFader,
                gain: 0.5,
            }],
            sidechains: vec![Sidechain {
                from: MixerTrackId(1),
                to: InsertId(20),
                tap: Tap::PreFader,
            }],
            sources: vec![
                Source {
                    id: SourceId::AudioTrack(5),
                    destination: MixerTrackId(2),
                },
                Source {
                    id: SourceId::System(SystemSource::Synth),
                    destination: MixerTrackId(2),
                },
                Source {
                    id: SourceId::Channel(3),
                    destination: MixerTrackId(1),
                },
                Source {
                    id: SourceId::Channel(7),
                    destination: MixerTrackId(2),
                },
                Source {
                    id: SourceId::AudioTrack(2),
                    destination: MixerTrackId(1),
                },
                // Named but never present: silent, harmless.
                Source {
                    id: SourceId::Channel(99),
                    destination: MixerTrackId(3),
                },
            ],
        },
        gains: vec![
            TrackGain {
                track: MixerTrackId(0),
                left: master,
                right: master,
            },
            TrackGain {
                track: MixerTrackId(1),
                left: 0.5,
                right: 0.75,
            },
            TrackGain {
                track: MixerTrackId(2),
                left: 1.25,
                right: 0.25,
            },
        ],
        devices,
    })
}

/// Every source alone, deinterleaved, rendered with the same per-block `hook`.
struct Isolated {
    ch3: Vec<f32>,
    ch7: Vec<f32>,
    synth: Vec<f32>,
    clips: [(u32, Vec<f32>); 3],
}

fn isolated(hook: impl Fn(usize, &mut Controller, Only)) -> Isolated {
    let one = |only| blocks(only, |b, c| hook(b, c, only));
    let clips = [5, 11, 20].map(|id| (id, one(Only::Clip(id))));
    Isolated {
        ch3: one(Only::Channel(3)),
        ch7: one(Only::Channel(7)),
        synth: one(Only::Synth),
        clips,
    }
}

fn acc(w: &mut [f32], x: &[f32], gain: f32) {
    for (a, x) in w.iter_mut().zip(x) {
        *a += *x * gain;
    }
}

fn fresh(kind: &str, seed: u64, params: &[f32]) -> Box<dyn Device> {
    let mut d: Box<dyn Device> = if kind == "algorithmic_reverb" {
        Box::new(grooviq_dsp::algorithmic_reverb::AlgorithmicReverb::with_seed(SR, seed))
    } else {
        grooviq_dsp::create(kind, SR).unwrap()
    };
    let mut padded = defaults(kind);
    padded[..params.len()].copy_from_slice(params);
    for (i, v) in padded.iter().enumerate() {
        d.set_param(i as u32, *v);
    }
    d
}

/// What changes at a block in the manual chain.
struct Change {
    at: usize,
    master: f32,
    threshold: Option<f32>,
    eq: bool,
    /// Clip -> playlist track from this block on.
    routes: Vec<(u32, u64)>,
    /// From this block on the mix also routes playlist track 9 into T1.
    track9: bool,
}

/// The hand-written chain: fresh devices that persist across `change` (as survivors must),
/// sources summed per track in canonical identity order, levels in topological order.
fn manual(iso: &Isolated, change: Option<Change>, start: usize) -> Vec<f32> {
    let mut r1 = fresh("algorithmic_reverb", 16_384, &[]);
    let mut c2 = fresh("comp", 0, &comp_params(-18.0));
    let mut r3 = fresh("reverb", 0, &[]);
    let mut eq: Option<Box<dyn Device>> = None;
    let mut master = 0.8;
    let mut routes = routes();
    let mut track9 = false;
    let mut out = Vec::with_capacity(BLOCKS * N * 2);
    let side = |v: &[f32], b: usize, ch: usize| -> Vec<f32> {
        (0..N).map(|i| v[(b * N + i) * 2 + ch]).collect()
    };
    for b in 0..BLOCKS {
        if b < start {
            out.extend(std::iter::repeat_n(0.0, 2 * N));
            continue;
        }
        if let Some(ch) = &change
            && ch.at == b
        {
            master = ch.master;
            if let Some(t) = ch.threshold {
                c2.set_param(0, t);
            }
            if ch.eq {
                eq = Some(fresh("eq", 0, &[]));
            }
            routes = ch.routes.clone();
            track9 = ch.track9;
        }
        let audio_track = |track: u64, ch: usize| -> Vec<f32> {
            let mut w = vec![0.0f32; N];
            for (id, signal) in &iso.clips {
                if routes.iter().any(|&(c, t)| c == *id && t == track) {
                    let s = side(signal, b, ch);
                    for (a, x) in w.iter_mut().zip(&s) {
                        *a += x;
                    }
                }
            }
            w
        };
        let mut lr = [[0.0f32; N]; 2];
        let mut t1 = [vec![0.0f32; N], vec![0.0f32; N]];
        let mut t2 = [vec![0.0f32; N], vec![0.0f32; N]];
        for ch in 0..2 {
            acc(&mut t1[ch], &side(&iso.ch3, b, ch), 1.0);
            acc(&mut t1[ch], &audio_track(2, ch), 1.0);
            if track9 {
                acc(&mut t1[ch], &audio_track(9, ch), 1.0);
            }
            acc(&mut t2[ch], &side(&iso.ch7, b, ch), 1.0);
            acc(&mut t2[ch], &audio_track(5, ch), 1.0);
            acc(&mut t2[ch], &side(&iso.synth, b, ch), 1.0);
        }
        {
            let [l, r] = &mut t1;
            r1.process(l, r, None, &[]);
        }
        let pre1 = t1.clone();
        let post1 = [
            t1[0].iter().map(|x| x * 0.5).collect::<Vec<_>>(),
            t1[1].iter().map(|x| x * 0.75).collect::<Vec<_>>(),
        ];
        let mut det = [vec![0.0f32; N], vec![0.0f32; N]];
        acc(&mut det[0], &pre1[0], 1.0);
        acc(&mut det[1], &pre1[1], 1.0);
        {
            let [l, r] = &mut t2;
            c2.process(l, r, Some((&det[0], &det[1])), &[]);
        }
        let post2 = [
            t2[0].iter().map(|x| x * 1.25).collect::<Vec<_>>(),
            t2[1].iter().map(|x| x * 0.25).collect::<Vec<_>>(),
        ];
        let mut t3 = [vec![0.0f32; N], vec![0.0f32; N]];
        acc(&mut t3[0], &post2[0], 0.5);
        acc(&mut t3[1], &post2[1], 0.5);
        {
            let [l, r] = &mut t3;
            r3.process(l, r, None, &[]);
            if let Some(eq) = &mut eq {
                eq.process(l, r, None, &[]);
            }
        }
        for ch in 0..2 {
            let mut w = vec![0.0f32; N];
            acc(&mut w, &post1[ch], 1.0);
            acc(&mut w, &post2[ch], 1.0);
            acc(&mut w, &t3[ch], 1.0);
            for (o, x) in lr[ch].iter_mut().zip(&w) {
                *o = *x * master;
            }
        }
        for i in 0..N {
            out.push(lr[0][i]);
            out.push(lr[1][i]);
        }
    }
    out
}

fn audible(v: &[f32]) -> bool {
    v.iter().any(|x| x.abs() > 1e-4)
}

#[test]
fn explicit_legacy_direct_installs_leave_every_byte_unchanged() {
    let plain = blocks(Only::All, |_, _| {});
    let reinstalled = blocks(Only::All, |b, c| {
        if b == 9 || b == 55 {
            let before = c.mix_revision();
            assert_eq!(c.set_mix(MixSetup::LegacyDirect), Ok(before + 1));
        }
    });
    assert!(audible(&plain));
    assert_eq!(same_bits(&plain, &reinstalled), None);
}

#[test]
fn routed_mix_matches_an_independent_manual_chain() {
    let iso = isolated(|_, _, _| {});
    for (i, x) in [
        &iso.ch3,
        &iso.ch7,
        &iso.synth,
        &iso.clips[0].1,
        &iso.clips[1].1,
        &iso.clips[2].1,
    ]
    .iter()
    .enumerate()
    {
        assert!(
            audible(x),
            "every source sounds: {i} {:?}",
            x.iter().fold(0f32, |m, v| m.max(v.abs()))
        );
    }
    let expected = manual(&iso, None, 0);
    let got = render(Only::All, routed(0.8, -18.0, false), |_| N, |_, _| {});
    assert!(audible(&got));
    assert_eq!(same_bits(&expected, &got), None);
}

#[test]
fn odd_callbacks_render_the_same_routed_bytes() {
    let aligned = render(Only::All, routed(0.8, -18.0, false), |_| N, |_, _| {});
    let odd = render(
        Only::All,
        routed(0.8, -18.0, false),
        |done| [37, 1, 91, 64, 13][done % 5],
        |_, _| {},
    );
    assert_eq!(same_bits(&aligned, &odd), None);
}

#[test]
fn a_live_install_keeps_survivor_state_and_held_voices() {
    let iso = isolated(|_, _, _| {});
    let expected = manual(
        &iso,
        Some(Change {
            at: 30,
            master: 0.6,
            threshold: Some(-30.0),
            eq: true,
            routes: routes(),
            track9: false,
        }),
        0,
    );
    let got = render(
        Only::All,
        routed(0.8, -18.0, false),
        |_| N,
        |b, c| {
            if b == 30 {
                c.set_mix(routed(0.6, -30.0, true)).unwrap();
            }
        },
    );
    assert_eq!(same_bits(&expected, &got), None);
}

#[test]
fn installing_routed_from_legacy_keeps_every_source_running() {
    let iso = isolated(|_, _, _| {});
    // From block 30 the manual chain runs with fresh devices; before it, LegacyDirect.
    let legacy = blocks(Only::All, |_, _| {});
    let got = render(
        Only::All,
        MixSetup::LegacyDirect,
        |_| N,
        |b, c| {
            if b == 30 {
                c.set_mix(routed(0.8, -18.0, false)).unwrap();
            }
        },
    );
    assert_eq!(same_bits(&legacy[..30 * N * 2], &got[..30 * N * 2]), None);
    // Sources keep their state, so the routed half equals a chain that starts at block 30.
    let expected = manual(&iso, None, 30);
    assert_eq!(same_bits(&expected[30 * N * 2..], &got[30 * N * 2..]), None);
}

#[test]
fn a_track_only_move_reroutes_sounding_clips_without_restarting_them() {
    let moved = vec![(20, 2), (11, 2), (5, 5)];
    // The same song update reaches every render, so only routing differs.
    let update = |b: usize, c: &mut Controller, moved: &[(u32, u64)]| {
        if b == 25 {
            let mut song = c.state().song.clone();
            let before = song.audio.clone();
            song.audio = song.audio.with_tracks(moved).unwrap();
            assert!(song.audio.shares_bank(&before));
            c.set_song(song).unwrap();
        }
    };
    let iso = isolated(|b, c, only| {
        let keep: Vec<_> = moved
            .iter()
            .copied()
            .filter(|(id, _)| only == Only::Clip(*id))
            .collect();
        update(b, c, &keep);
    });
    let expected = manual(
        &iso,
        Some(Change {
            at: 25,
            master: 0.8,
            threshold: None,
            eq: false,
            routes: moved.clone(),
            track9: false,
        }),
        0,
    );
    let got = render(
        Only::All,
        routed(0.8, -18.0, false),
        |_| N,
        |b, c| update(b, c, &moved),
    );
    assert_eq!(same_bits(&expected, &got), None);
}

#[test]
fn all_sound_off_clears_insert_tails_but_a_seek_keeps_them() {
    let tail = |cmd: Command| {
        render(
            Only::All,
            routed(0.8, -18.0, false),
            |_| N,
            move |b, c| {
                if b == 60 {
                    c.send(cmd).unwrap();
                }
            },
        )
    };
    let off = tail(Command::AllSoundOff);
    let seek = tail(Command::Seek { tick: 0 });
    // Sources fade within 5 ms (4 blocks); afterwards a cleared mix is exactly silent.
    let after = |v: &[f32]| v[66 * N * 2..80 * N * 2].to_vec();
    assert!(after(&off).iter().all(|x| *x == 0.0));
    assert!(audible(&after(&seek)));
}

#[test]
fn refused_mixes_change_nothing() {
    let (mut c, _p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::All, routed(0.8, -18.0, false)),
    );
    let before = (c.state().clone(), c.mix_revision());
    let mut bad = routed(0.8, -18.0, false);
    if let MixSetup::Routed(m) = &mut bad {
        m.devices.pop();
    }
    assert_eq!(c.set_mix(bad), Err(ControlError::Rejected));
    let mut instrument = routed(0.8, -18.0, false);
    if let MixSetup::Routed(m) = &mut instrument {
        m.devices[0].kind = "synth".into();
    }
    assert_eq!(c.set_mix(instrument), Err(ControlError::Rejected));
    // A device whose actual latency exceeds its declaration.
    let mut late = routed(0.8, -18.0, false);
    if let MixSetup::Routed(m) = &mut late {
        m.mixer.tracks[3].inserts[0].latency_frames = 0;
        m.devices[2] = InsertDevice {
            insert: InsertId(30),
            kind: "limiter".into(),
            seed: 0,
            params: Vec::new(),
        };
    }
    assert_eq!(c.set_mix(late), Err(ControlError::Rejected));
    while c.send(Command::Stop).is_ok() {}
    assert_eq!(
        c.set_mix(routed(0.5, -18.0, false)),
        Err(ControlError::Full)
    );
    assert_eq!((c.state().clone(), c.mix_revision()), before);
}

#[test]
fn system_sources_cannot_collide_with_user_ids() {
    let ids = [
        SourceId::Channel(u64::MAX),
        SourceId::AudioTrack(0),
        SourceId::System(SystemSource::Tone),
        SourceId::System(SystemSource::Synth),
    ];
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert_ne!(a, b);
            assert!(a < b, "canonical order is user sources, then system ones");
        }
    }
}

/// Two channel sources, each on its own unity track into a unity master, in insertion and
/// identity order: here the routed sum is `(0 + a) + b`, the very additions LegacyDirect
/// makes after a silent tone and synth. This equality is arithmetic for this shape only,
/// not a claim about arbitrary routed sums. It lets chokes, automation, a song loop and a
/// seek run through the routed graph against the legacy one.
#[test]
fn routed_chokes_automation_loop_and_seek_match_legacy_for_a_two_source_shape() {
    let hits = |n: u32| -> Vec<SeqNote> {
        (0..n)
            .map(|i| SeqNote {
                tick: i * 120,
                channel: ChannelId(1 + i % 2),
                key: 60 + (i % 5) as u8,
                velocity: 0.9,
                length: 200,
                slide: false,
            })
            .collect()
    };
    let channel = |id, pan| {
        let mut c = ChannelSetup::sampler(
            ChannelId(id),
            Some(sample()),
            Vec::new(),
            ChannelMix {
                pan,
                ..ChannelMix::default()
            },
        );
        c.choke = 1;
        c
    };
    let channels = vec![channel(1, -0.4), channel(2, 0.6)];
    let arrangement = compile_pattern_clips(
        &[(
            1,
            PatternData {
                length: 3_840,
                notes: hits(32),
            },
        )],
        &[PatternClip {
            id: 1,
            pattern: 1,
            start: 0,
            length: 3_840,
            offset: 0,
            muted: false,
        }],
        10_000,
    )
    .unwrap();
    let automation = AutomationSchedule::build(
        vec![AutomationClip {
            target: AutomationTarget::ChannelVolume(ChannelId(1)),
            priority: 1,
            start_tick: 0,
            offset_tick: 0,
            length_ticks: 1_920,
            points: vec![
                AutomationPoint {
                    tick: 0,
                    value: 0.2,
                    segment: AutomationSegment::Linear,
                },
                AutomationPoint {
                    tick: 1_920,
                    value: 1.6,
                    segment: AutomationSegment::Hold,
                },
            ],
        }],
        &channels,
    )
    .unwrap();
    let to = |id| Track {
        id: MixerTrackId(id),
        output: Some(MASTER),
        inserts: Vec::new(),
    };
    let mix = MixSetup::Routed(RoutedMix {
        mixer: Mixer {
            tracks: vec![
                Track {
                    id: MASTER,
                    output: None,
                    inserts: Vec::new(),
                },
                to(1),
                to(2),
            ],
            sources: vec![
                Source {
                    id: SourceId::Channel(2),
                    destination: MixerTrackId(2),
                },
                Source {
                    id: SourceId::Channel(1),
                    destination: MixerTrackId(1),
                },
            ],
            ..Mixer::default()
        },
        gains: Vec::new(),
        devices: Vec::new(),
    });
    let run = |mix: MixSetup| {
        let state = EngineState {
            bpm: 120.0,
            playback_mode: PlaybackMode::Song,
            song_loop: Some(SongLoop {
                start_tick: 240,
                end_tick: 1_200,
            }),
            channels: channels.clone(),
            song: SongData {
                arrangement: arrangement.clone(),
                automation: automation.clone(),
                end_tick: 3_840,
                end_frame: 96_000,
                frame_rate: SR,
                frame_extent: None,
                ..SongData::default()
            },
            mix,
            ..EngineState::default()
        };
        let (mut c, mut p) = engine_with(EngineConfig { sample_rate: SR }, state);
        c.send(Command::Play).unwrap();
        let mut out = Vec::new();
        for b in 0..600 {
            if b == 333 {
                c.send(Command::Seek { tick: 500 }).unwrap();
            }
            let mut buf = [0.0; 2 * N];
            p.process(&mut buf, 2);
            out.extend_from_slice(&buf);
            let _ = c.check_health();
        }
        out
    };
    let legacy = run(MixSetup::LegacyDirect);
    assert!(audible(&legacy));
    assert_eq!(same_bits(&legacy, &run(mix)), None);
}

/// A device that plays a constant, to see which kernel holds it.
struct Marker(f32);
impl Device for Marker {
    fn set_param(&mut self, _: u32, _: f32) {}
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[grooviq_dsp::NoteEvent],
    ) {
        l.fill(self.0);
        r.fill(self.0);
    }
    fn reset(&mut self) {}
}

fn marker_kernel(rate: f32, value: f32, seed: u64) -> PreparedMixer {
    let mixer = Mixer {
        tracks: vec![Track {
            id: MASTER,
            output: None,
            inserts: vec![Insert {
                id: InsertId(1),
                latency_frames: 0,
                memory_bytes: 0,
            }],
        }],
        ..Mixer::default()
    };
    PreparedMixer::prepare(
        compile(&mixer, Budget::default()).unwrap(),
        vec![PreparedInsert::with_identity(
            InsertId(1),
            Box::new(Marker(value)),
            InsertIdentity { kind: 3, seed },
        )],
        &[],
        PrepareConfig {
            sample_rate: rate,
            max_frames: N,
            max_bytes: 1 << 20,
        },
    )
    .unwrap()
}

fn plays(k: &mut PreparedMixer) -> f32 {
    let host = grooviq_dsp::HostTransport {
        domain: grooviq_dsp::HostDomain::Pattern,
        frame: 0,
        frames: N as u32,
        sample_rate: k.sample_rate(),
        bpm: 120.0,
        playing: false,
        anchor_frame: 0,
        anchor_tick: 0.0,
        shift: 0,
        loop_start: 0,
        loop_end: 0,
        stream_generation: 0,
        discontinuity: 0,
        changed: false,
    };
    let sources = k.source_buffers().unwrap();
    let (mut l, mut r) = ([0.0; N], [0.0; N]);
    k.render_pulled(&host, &sources, &mut l, &mut r).unwrap();
    l[0]
}

#[test]
fn survivor_transfers_check_identity_rate_and_order_before_moving_anything() {
    let id = [InsertId(1)];
    let mut old = marker_kernel(SR, 1.0, 5);
    let mut other_seed = marker_kernel(SR, 2.0, 6);
    assert_eq!(
        other_seed.transfer_from(&mut old, &id),
        Err(TransferError::Identity(InsertId(1)))
    );
    let mut other_rate = marker_kernel(44_100.0, 2.0, 5);
    assert_eq!(
        other_rate.transfer_from(&mut old, &id),
        Err(TransferError::SampleRate)
    );
    let mut new = marker_kernel(SR, 2.0, 5);
    assert_eq!(
        new.transfer_from(&mut old, &[InsertId(1), InsertId(1)]),
        Err(TransferError::Unordered)
    );
    assert_eq!(
        new.transfer_from(&mut old, &[InsertId(9)]),
        Err(TransferError::Missing(InsertId(9)))
    );
    // Every refusal left each device where it was.
    assert_eq!(
        (plays(&mut old), plays(&mut other_seed), plays(&mut new)),
        (1.0, 2.0, 2.0)
    );
    assert_eq!(new.transfer_from(&mut old, &id), Ok(()));
    assert_eq!((plays(&mut new), plays(&mut old)), (1.0, 2.0));
}

// ---- R2/R3 repair: instance routes and one source-set transaction ----

/// Channel 3 through unity track 1; track 2 is silent (zero gain).
fn one_channel_mix(destination: u64) -> MixSetup {
    MixSetup::Routed(RoutedMix {
        mixer: Mixer {
            tracks: vec![
                Track {
                    id: MASTER,
                    output: None,
                    inserts: vec![],
                },
                Track {
                    id: MixerTrackId(1),
                    output: Some(MASTER),
                    inserts: vec![],
                },
                Track {
                    id: MixerTrackId(2),
                    output: Some(MASTER),
                    inserts: vec![],
                },
            ],
            sources: vec![Source {
                id: SourceId::Channel(3),
                destination: MixerTrackId(destination),
            }],
            ..Mixer::default()
        },
        gains: vec![TrackGain {
            track: MixerTrackId(2),
            left: 0.0,
            right: 0.0,
        }],
        devices: vec![],
    })
}

#[test]
fn a_reused_channel_id_leaves_the_old_fade_on_its_old_route() {
    let run = |reroute: bool| {
        render(
            Only::Channel(3),
            one_channel_mix(1),
            |_| N,
            |b, c| {
                if b == 30 {
                    c.remove_channel(ChannelId(3)).unwrap();
                    c.add_channel(ChannelSetup::sampler(
                        ChannelId(3),
                        None,
                        vec![],
                        ChannelMix::default(),
                    ))
                    .unwrap();
                    if reroute {
                        c.set_mix(one_channel_mix(2)).unwrap();
                    }
                }
            },
        )
    };
    let original = run(false);
    let rerouted = run(true);
    let fade = 30 * N * 2..34 * N * 2;
    assert!(
        audible(&original[fade.clone()]),
        "the old instance fades out audibly"
    );
    assert_eq!(same_bits(&original, &rerouted), None);
}

#[test]
fn a_song_naming_an_unrouted_track_is_refused_whole_in_routed_mode() {
    let (mut c, _p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::All, routed(0.8, -18.0, false)),
    );
    let before = c.state().clone();
    let mut song = before.song.clone();
    song.audio = song.audio.with_tracks(&[(20, 9), (11, 2), (5, 5)]).unwrap();
    assert_eq!(c.set_song(song.clone()), Err(ControlError::Rejected));
    let update = TimelineUpdate {
        pattern: Box::new(before.pattern.clone()),
        song: Box::new(song),
        bpm: before.bpm,
        mode: before.playback_mode,
        region: before.song_loop,
        remove_channel: None,
    };
    assert_eq!(c.set_timeline(update), Err(ControlError::Rejected));
    assert_eq!(c.state(), &before);
}

fn with_track9(mut mix: MixSetup) -> MixSetup {
    if let MixSetup::Routed(m) = &mut mix {
        m.mixer.sources.push(Source {
            id: SourceId::AudioTrack(9),
            destination: MixerTrackId(1),
        });
    }
    mix
}

fn moved_song(c: &Controller, routes: &[(u32, u64)]) -> TimelineUpdate {
    let s = c.state();
    let mut song = s.song.clone();
    let before = song.audio.clone();
    song.audio = song.audio.with_tracks(routes).unwrap();
    assert!(song.audio.shares_bank(&before));
    TimelineUpdate {
        pattern: Box::new(s.pattern.clone()),
        song: Box::new(song),
        bpm: s.bpm,
        mode: s.playback_mode,
        region: s.song_loop,
        remove_channel: None,
    }
}

#[test]
fn one_queue_slot_admits_a_whole_source_and_mix_edit_and_a_full_queue_none() {
    let moved = [(20, 9), (11, 2), (5, 5)];
    let (mut c, _p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::All, routed(0.8, -18.0, false)),
    );
    let edit = |c: &Controller| SourceEdit {
        remove: vec![ChannelId(3)],
        add: vec![ChannelSetup::synth(
            ChannelId(3),
            vec![],
            ChannelMix::default(),
        )],
        timeline: Some(moved_song(c, &moved)),
        mix: with_track9(routed(0.8, -18.0, false)),
    };
    // The unsafe half on its own is refused: the song would name an unrouted track.
    assert_eq!(
        c.set_song(*moved_song(&c, &moved).song),
        Err(ControlError::Rejected)
    );
    let mut fill = 0;
    while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {
        fill += 1;
    }
    assert!(fill > 0);
    let before = (
        c.state().clone(),
        c.mix_revision(),
        c.channel_instance(ChannelId(3)),
    );
    assert_eq!(c.set_sources_and_mix(edit(&c)), Err(ControlError::Full));
    assert_eq!(
        (
            c.state().clone(),
            c.mix_revision(),
            c.channel_instance(ChannelId(3))
        ),
        before
    );
    // Exactly one slot.
    let (mut c, mut p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::All, routed(0.8, -18.0, false)),
    );
    for _ in 0..fill - 1 {
        c.send(Command::SetTempo { bpm: 120.0 }).unwrap();
    }
    let old_instance = c.channel_instance(ChannelId(3));
    let revision = c.set_sources_and_mix(edit(&c)).unwrap();
    assert_eq!(revision, c.mix_revision());
    assert_eq!(c.state().song.audio.tracks(), moved.to_vec());
    assert_eq!(c.state().mix, with_track9(routed(0.8, -18.0, false)));
    assert_ne!(c.channel_instance(ChannelId(3)), old_instance);
    assert!(c.send(Command::Stop).is_err(), "it took the single slot");
    p.process(&mut [0.0; 2 * N], 2);
    assert!(c.send(Command::Stop).is_ok());
}

#[test]
fn a_compound_edit_moves_a_sounding_clip_to_a_new_track_without_restarting_anything() {
    let moved = vec![(20, 9), (11, 2), (5, 5)];
    // Isolated sources see the same song update, so only routing differs.
    let iso = isolated(|b, c, only| {
        if b == 25 {
            let keep: Vec<_> = moved
                .iter()
                .copied()
                .filter(|(id, _)| only == Only::Clip(*id))
                .collect();
            c.set_timeline(moved_song(c, &keep)).unwrap();
        }
    });
    let expected = manual(
        &iso,
        Some(Change {
            at: 25,
            master: 0.6,
            threshold: Some(-30.0),
            eq: false,
            routes: moved.clone(),
            track9: true,
        }),
        0,
    );
    let got = render(
        Only::All,
        routed(0.8, -18.0, false),
        |_| N,
        |b, c| {
            if b == 25 {
                let edit = SourceEdit {
                    remove: vec![],
                    add: vec![],
                    timeline: Some(moved_song(c, &moved)),
                    mix: with_track9(routed(0.6, -30.0, false)),
                };
                c.set_sources_and_mix(edit).unwrap();
            }
        },
    );
    assert_eq!(same_bits(&expected, &got), None);
}

#[test]
fn repeated_reuse_and_reroute_keep_every_fade_on_its_own_route() {
    let silent = || ChannelSetup::sampler(ChannelId(3), None, vec![], ChannelMix::default());
    let original = render(
        Only::Channel(3),
        one_channel_mix(1),
        |_| N,
        |b, c| {
            if b == 30 {
                c.remove_channel(ChannelId(3)).unwrap();
                c.add_channel(silent()).unwrap();
            }
        },
    );
    let churned = render(
        Only::Channel(3),
        one_channel_mix(1),
        |_| N,
        |b, c| {
            if (30..34).contains(&b) {
                // Each block: replace the channel and flip its route, in one admission.
                let edit = SourceEdit {
                    remove: vec![ChannelId(3)],
                    add: vec![silent()],
                    timeline: None,
                    mix: one_channel_mix(if b % 2 == 0 { 2 } else { 1 }),
                };
                c.set_sources_and_mix(edit).unwrap();
            }
        },
    );
    assert!(audible(&original[30 * N * 2..34 * N * 2]));
    assert_eq!(same_bits(&original, &churned), None);
}

#[test]
fn a_mix_that_drops_a_fading_route_waits_for_the_fade() {
    let (mut c, mut p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::Channel(3), one_channel_mix(1)),
    );
    c.send(Command::Play).unwrap();
    c.send(Command::ChannelNote {
        channel: ChannelId(3),
        key: 60,
        velocity: 1.0,
        on: true,
        id: NoteId(1),
    })
    .unwrap();
    p.process(&mut [0.0; 8 * N], 2);
    c.remove_channel(ChannelId(3)).unwrap();
    // Without track 1 the fading instance would have nowhere to go; so would LegacyDirect.
    let mut no_track1 = one_channel_mix(2);
    if let MixSetup::Routed(m) = &mut no_track1 {
        m.mixer.tracks.retain(|t| t.id != MixerTrackId(1));
        m.mixer.sources.clear();
    }
    let before = (c.state().clone(), c.mix_revision());
    assert_eq!(c.set_mix(no_track1.clone()), Err(ControlError::Busy));
    assert_eq!(c.set_mix(MixSetup::LegacyDirect), Err(ControlError::Busy));
    assert_eq!((c.state().clone(), c.mix_revision()), before);
    // Once the fade is collected and freed, the same edit goes through.
    for _ in 0..16 {
        p.process(&mut [0.0; 2 * N], 2);
        let _ = c.check_health();
    }
    assert!(c.set_mix(no_track1).is_ok());
    assert!(c.set_mix(MixSetup::LegacyDirect).is_ok());
}

#[test]
fn compound_edits_refuse_whole_on_any_invalid_part() {
    let (mut c, _p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::All, routed(0.8, -18.0, false)),
    );
    let before = (
        c.state().clone(),
        c.mix_revision(),
        c.channel_instance(ChannelId(7)),
    );
    let base = || SourceEdit {
        remove: vec![],
        add: vec![],
        timeline: None,
        mix: routed(0.8, -18.0, false),
    };
    let cases = [
        // A new channel the mix does not route.
        (
            SourceEdit {
                add: vec![ChannelSetup::synth(
                    ChannelId(8),
                    vec![],
                    ChannelMix::default(),
                )],
                ..base()
            },
            ControlError::Rejected,
        ),
        (
            SourceEdit {
                remove: vec![ChannelId(4)],
                ..base()
            },
            ControlError::UnknownChannel,
        ),
        (
            SourceEdit {
                remove: vec![ChannelId(3), ChannelId(3)],
                ..base()
            },
            ControlError::Rejected,
        ),
        (
            SourceEdit {
                add: vec![ChannelSetup::synth(
                    ChannelId(7),
                    vec![],
                    ChannelMix::default(),
                )],
                ..base()
            },
            ControlError::ChannelExists,
        ),
        (
            SourceEdit {
                timeline: Some(TimelineUpdate {
                    remove_channel: Some(ChannelId(7)),
                    ..moved_song(&c, &routes())
                }),
                ..base()
            },
            ControlError::Rejected,
        ),
        // A song routing clip 20 to a track the mix does not name.
        (
            SourceEdit {
                timeline: Some(moved_song(&c, &[(20, 9), (11, 2), (5, 5)])),
                ..base()
            },
            ControlError::Rejected,
        ),
        // The mix itself invalid (a device missing), after everything else checks out.
        (
            SourceEdit {
                add: vec![ChannelSetup::synth(
                    ChannelId(99),
                    vec![],
                    ChannelMix::default(),
                )],
                mix: {
                    let mut m = routed(0.8, -18.0, false);
                    if let MixSetup::Routed(r) = &mut m {
                        r.devices.pop();
                    }
                    m
                },
                ..base()
            },
            ControlError::Rejected,
        ),
    ];
    for (edit, err) in cases {
        assert_eq!(c.set_sources_and_mix(edit), Err(err));
        assert_eq!(
            (
                c.state().clone(),
                c.mix_revision(),
                c.channel_instance(ChannelId(7))
            ),
            before
        );
    }
    // A mix that stops naming a track the song uses is refused like the song itself.
    let mut drops5 = routed(0.8, -18.0, false);
    if let MixSetup::Routed(m) = &mut drops5 {
        m.mixer.sources.retain(|s| s.id != SourceId::AudioTrack(5));
    }
    assert_eq!(c.set_mix(drops5), Err(ControlError::Rejected));
    assert_eq!(
        (
            c.state().clone(),
            c.mix_revision(),
            c.channel_instance(ChannelId(7))
        ),
        before
    );
}

#[test]
fn a_plainly_added_routed_channel_gets_its_fade_route_and_must_be_routed() {
    // Channel 3 added live (not in the initial state), played, then removed: its fade
    // must stay audible on track 1 exactly as when it is removed without a reroute.
    let run = |reroute: bool| {
        let (mut c, mut p) = engine_with(
            EngineConfig { sample_rate: SR },
            EngineState {
                mix: one_channel_mix(1),
                ..EngineState::default()
            },
        );
        c.add_channel(ChannelSetup::sampler(
            ChannelId(3),
            Some(sample()),
            vec![],
            ChannelMix::default(),
        ))
        .unwrap();
        c.send(Command::ChannelNote {
            channel: ChannelId(3),
            key: 60,
            velocity: 1.0,
            on: true,
            id: NoteId(1),
        })
        .unwrap();
        let mut out = Vec::new();
        for b in 0..20 {
            if b == 10 {
                c.remove_channel(ChannelId(3)).unwrap();
                if reroute {
                    c.add_channel(ChannelSetup::sampler(
                        ChannelId(3),
                        None,
                        vec![],
                        ChannelMix::default(),
                    ))
                    .unwrap();
                    c.set_mix(one_channel_mix(2)).unwrap();
                }
            }
            let mut buf = [0.0; 2 * N];
            p.process(&mut buf, 2);
            out.extend_from_slice(&buf);
            let _ = c.check_health();
        }
        out
    };
    let kept = run(false);
    assert!(audible(&kept[10 * N * 2..14 * N * 2]), "the fade is heard");
    assert_eq!(same_bits(&kept, &run(true)), None);
    // A channel the routed mix does not name would never be heard: refused.
    let (mut c, _p) = engine_with(
        EngineConfig { sample_rate: SR },
        EngineState {
            mix: one_channel_mix(1),
            ..EngineState::default()
        },
    );
    let unnamed = ChannelSetup::synth(ChannelId(8), vec![], ChannelMix::default());
    assert_eq!(c.add_channel(unnamed), Err(ControlError::Rejected));
    assert!(c.state().channels.is_empty());
}

#[test]
#[cfg(feature = "test-hooks")]
fn retiring_an_older_graph_keeps_fades_of_the_newer_one() {
    let (mut c, mut p) = engine_with(
        EngineConfig { sample_rate: SR },
        state(Only::Channel(3), one_channel_mix(1)),
    );
    p.process(&mut [0.0; 2 * N], 2);
    // A panic: the next health check queues a graph rebuilt from the mirror.
    c.send(Command::InjectPanic).unwrap();
    p.process(&mut [0.0; 2 * N], 2);
    let _ = c.check_health();
    // Removed after that rebuild: it fades in the new graph.
    c.remove_channel(ChannelId(3)).unwrap();
    let mut no_track1 = one_channel_mix(2);
    if let MixSetup::Routed(m) = &mut no_track1 {
        m.mixer.tracks.retain(|t| t.id != MixerTrackId(1));
        m.mixer.sources.clear();
    }
    // One block installs the new graph and starts the fade; the old graph retires.
    p.process(&mut [0.0; 2 * N], 2);
    let _ = c.check_health();
    assert_eq!(c.set_mix(no_track1.clone()), Err(ControlError::Busy));
    for _ in 0..16 {
        p.process(&mut [0.0; 2 * N], 2);
        let _ = c.check_health();
    }
    assert!(c.set_mix(no_track1).is_ok());
}

#[test]
fn admission_counts_the_fade_routes_a_build_adds() {
    // Exactly at the node budget without fade routes; one channel's route tips it over.
    let mut mix = one_channel_mix(1);
    if let MixSetup::Routed(m) = &mut mix {
        m.mixer.tracks.truncate(2);
        m.gains.clear();
        let room = Budget::default().nodes as usize - m.mixer.tracks.len() - 1;
        m.mixer.sources.extend((0..room as u64).map(|t| Source {
            id: SourceId::AudioTrack(t),
            destination: MASTER,
        }));
    }
    let routed = EngineState {
        channels: vec![ChannelSetup::synth(
            ChannelId(3),
            vec![],
            ChannelMix::default(),
        )],
        mix: mix.clone(),
        ..EngineState::default()
    };
    let (mut c, _p) = engine(EngineConfig { sample_rate: SR });
    assert_eq!(c.load_state(routed), Err(ControlError::Rejected));
    assert_eq!(c.state(), &EngineState::default());
}

#[test]
fn legacy_fades_refuse_routed_installs_until_retired_across_all_removal_apis() {
    for path in 0..3 {
        let run = |switch| {
            render(
                Only::Channel(3),
                MixSetup::LegacyDirect,
                |_| N,
                |b, c| {
                    let replacement =
                        || ChannelSetup::sampler(ChannelId(3), None, vec![], ChannelMix::default());
                    let snapshot = |c: &Controller| {
                        (
                            c.state().clone(),
                            c.mix_revision(),
                            c.channel_instance(ChannelId(3)),
                        )
                    };
                    if b == 30 {
                        if switch && path == 1 {
                            let before = snapshot(c);
                            assert_eq!(
                                c.set_sources_and_mix(SourceEdit {
                                    remove: vec![ChannelId(3)],
                                    add: vec![replacement()],
                                    timeline: None,
                                    mix: one_channel_mix(1),
                                }),
                                Err(ControlError::Busy),
                                "compound mode change must retain the legacy sink"
                            );
                            assert_eq!(snapshot(c), before, "refused compound edit is atomic");
                        }
                        match path {
                            0 => {
                                c.remove_channel(ChannelId(3)).unwrap();
                                c.add_channel(replacement()).unwrap();
                            }
                            1 => {
                                // A legacy-only compound edit is safe while a legacy fade rings.
                                c.set_sources_and_mix(SourceEdit {
                                    remove: vec![ChannelId(3)],
                                    add: vec![replacement()],
                                    timeline: None,
                                    mix: MixSetup::LegacyDirect,
                                })
                                .unwrap();
                            }
                            _ => {
                                let state = c.state();
                                c.set_timeline(TimelineUpdate {
                                    pattern: Box::new(state.pattern.clone()),
                                    song: Box::new(state.song.clone()),
                                    bpm: state.bpm,
                                    mode: state.playback_mode,
                                    region: state.song_loop,
                                    remove_channel: Some(ChannelId(3)),
                                })
                                .unwrap();
                                c.add_channel(replacement()).unwrap();
                            }
                        }
                        if switch {
                            let before = snapshot(c);
                            assert_eq!(c.set_mix(one_channel_mix(1)), Err(ControlError::Busy));
                            assert_eq!(snapshot(c), before, "refused mix leaves the mirror intact");
                            // Do not block same-mode legacy edits merely because it has a fade.
                            c.set_mix(MixSetup::LegacyDirect).unwrap();
                        }
                    }
                    if b == 40 && switch {
                        c.set_mix(one_channel_mix(1))
                            .expect("the collected legacy fade releases Busy");
                    }
                },
            )
        };
        let control = run(false);
        let changed = run(true);
        let fade = 30 * N * 2..34 * N * 2;
        assert!(
            audible(&control[fade.clone()]),
            "path {path} has an audible old-instance fade"
        );
        assert_eq!(
            same_bits(&control[fade.clone()], &changed[fade]),
            None,
            "path {path} preserved the fade"
        );
        assert!(changed[40 * N * 2..].iter().all(|v| v.abs() < 1e-10));
    }
}
