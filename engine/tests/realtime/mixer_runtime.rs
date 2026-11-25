use grooviq_dsp::{
    Device, HostDomain, HostTransport, NoteEvent, comp::Comp, saturation::Saturation,
    utility::Utility,
};
use grooviq_engine::mixer::*;

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
fn transport(sr: f32, frame: u64, frames: usize) -> HostTransport {
    HostTransport {
        domain: HostDomain::Song,
        frame,
        frames: frames as u32,
        sample_rate: sr,
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
fn comp(sr: f32) -> Comp {
    let mut c = Comp::new(sr);
    for (i, v) in [(0, -30.0), (1, 8.0), (2, 1.0), (7, 1.0)] {
        c.set_param(i, v);
    }
    c
}
fn sat(sr: f32) -> Saturation {
    let mut s = Saturation::new(sr);
    s.set_param(1, 14.0);
    s
}
fn util(sr: f32) -> Utility {
    let mut u = Utility::new(sr);
    u.set_param(0, -4.0);
    u.set_param(2, 1.3);
    u
}

/// Every `render`, refusal, empty call, `reset`, keyed parameter and fault path runs inside
/// the guard. The guarded output is checked outside it against a manual device chain.
#[test]
fn mixer_runtime_realtime() {
    const SPANS: [usize; 6] = [1, 7, 37, 64, 127, 128];
    let frames: usize = SPANS.iter().sum::<usize>() * 3;
    for sr in [22050.0, 48000.0, 192000.0] {
        // Master <- track 1 [comp keyed by track 2 pre], track 2 [saturation] x0.5; master [utility].
        let m = Mixer {
            tracks: vec![
                track(0, None, &[3]),
                track(1, Some(0), &[1]),
                track(2, Some(0), &[2]),
            ],
            sidechains: vec![Sidechain {
                from: MixerTrackId(2),
                to: InsertId(1),
                tap: Tap::PreFader,
            }],
            sources: vec![source(1, 1), source(2, 2)],
            ..Mixer::default()
        };
        let devices = vec![
            PreparedInsert::new(InsertId(1), Box::new(comp(sr))),
            PreparedInsert::new(InsertId(2), Box::new(sat(sr))),
            PreparedInsert::new(InsertId(3), Box::new(util(sr))),
        ];
        let gains = [TrackGain {
            track: MixerTrackId(2),
            left: 0.5,
            right: 0.5,
        }];
        let cfg = PrepareConfig {
            sample_rate: sr,
            max_frames: 128,
            max_bytes: 1 << 20,
        };
        let mut k = PreparedMixer::prepare(
            compile(&m, Budget::default()).unwrap(),
            devices,
            &gains,
            cfg,
        )
        .unwrap();
        let input = |f: f32, a: f32| -> Vec<f32> {
            (0..frames)
                .map(|i| libm::sinf(i as f32 * f) * if (i / 200) % 2 == 0 { a } else { 0.1 })
                .collect()
        };
        let (s1l, s1r, s2l, s2r) = (
            input(0.07, 0.6),
            input(0.05, 0.5),
            input(0.21, 0.9),
            input(0.13, 0.8),
        );
        let (mut ol, mut or) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let mut refusals = [None; 4];
        let mut reports = [RenderReport::default(); 18];
        let mut unknown = Ok(());
        let reset_at = SPANS.iter().sum::<usize>();
        let param_at = reset_at * 2;

        super::realtime(|| {
            let mut at = 0;
            let mut call = 0;
            for _ in 0..3 {
                for n in SPANS {
                    if at == reset_at {
                        k.reset();
                    }
                    if at == param_at {
                        k.set_param(InsertId(1), 0, -18.0).unwrap();
                        unknown = k.set_param(InsertId(42), 0, 0.0);
                    }
                    let blocks = [
                        SourceBlock {
                            id: SourceId::Channel(1),
                            left: &s1l[at..at + n],
                            right: &s1r[at..at + n],
                        },
                        SourceBlock {
                            id: SourceId::Channel(2),
                            left: &s2l[at..at + n],
                            right: &s2r[at..at + n],
                        },
                    ];
                    let t = transport(sr, at as u64, n);
                    if call == 0 {
                        let swapped = [blocks[1], blocks[0]];
                        let none: [SourceBlock; 0] = [];
                        refusals = [
                            k.render(&t, &swapped, &mut ol[at..at + n], &mut or[at..at + n])
                                .err(),
                            k.render(&t, &none, &mut ol[at..at + n], &mut or[at..at + n])
                                .err(),
                            k.render(&transport(sr, 0, 129), &blocks, &mut ol, &mut or)
                                .err(),
                            k.render(
                                &transport(sr * 2.0, at as u64, n),
                                &blocks,
                                &mut ol[at..at + n],
                                &mut or[at..at + n],
                            )
                            .err(),
                        ];
                        let empty = [
                            SourceBlock {
                                id: SourceId::Channel(1),
                                left: &[],
                                right: &[],
                            },
                            SourceBlock {
                                id: SourceId::Channel(2),
                                left: &[],
                                right: &[],
                            },
                        ];
                        k.render(&transport(sr, 0, 0), &empty, &mut [], &mut [])
                            .unwrap();
                    }
                    reports[call] = k
                        .render(&t, &blocks, &mut ol[at..at + n], &mut or[at..at + n])
                        .unwrap();
                    at += n;
                    call += 1;
                }
            }
        });

        assert!(refusals.iter().all(Option::is_some), "{refusals:?}");
        assert_eq!(unknown, Err(ParamError::UnknownInsert(InsertId(42))));
        assert!(reports.iter().all(|r| r.silenced == 0 && r.frames > 0));

        // Independent manual chain over the same spans, outside the guard.
        let (mut c, mut s, mut u) = (comp(sr), sat(sr), util(sr));
        let (mut rl, mut rr) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        let mut at = 0;
        for _ in 0..3 {
            for n in SPANS {
                if at == reset_at {
                    c.reset();
                    s.reset();
                    u.reset();
                }
                if at == param_at {
                    c.set_param(0, -18.0);
                }
                let span = at..at + n;
                let (mut l2, mut r2) = (s2l[span.clone()].to_vec(), s2r[span.clone()].to_vec());
                s.process(&mut l2, &mut r2, None, &[]);
                let (mut l1, mut r1) = (s1l[span.clone()].to_vec(), s1r[span.clone()].to_vec());
                c.process(&mut l1, &mut r1, Some((&l2, &r2)), &[]);
                let mut ml: Vec<f32> = (0..n)
                    .map(|i| 0.0 + l1[i] * 1.0 + l2[i] * 0.5 * 1.0)
                    .collect();
                let mut mr: Vec<f32> = (0..n)
                    .map(|i| 0.0 + r1[i] * 1.0 + r2[i] * 0.5 * 1.0)
                    .collect();
                u.process(&mut ml, &mut mr, None, &[]);
                rl[span.clone()].copy_from_slice(&ml);
                rr[span].copy_from_slice(&mr);
                at += n;
            }
        }
        assert_eq!(ol, rl, "{sr} Hz");
        assert_eq!(or, rr, "{sr} Hz");
        let dry: Vec<f32> = s1l.iter().zip(&s2l).map(|(a, b)| a + b).collect();
        assert_ne!(ol, dry);
        assert!(ol.iter().any(|x| x.abs() > 0.05));

        // Fault path: a NaN block on track 1 is zeroed and its chain reset, inside the guard.
        let m = Mixer {
            tracks: vec![track(0, None, &[]), track(1, Some(0), &[1, 2])],
            sources: vec![source(1, 1)],
            ..Mixer::default()
        };
        let devices = vec![
            PreparedInsert::new(InsertId(1), Box::new(sat(sr))),
            PreparedInsert::new(InsertId(2), Box::new(Fault::default())),
        ];
        let mut k =
            PreparedMixer::prepare(compile(&m, Budget::default()).unwrap(), devices, &[], cfg)
                .unwrap();
        let mut silenced = [0; 3];
        let (mut fl, mut fr) = ([1.0f32; 128], [1.0f32; 128]);
        let mut after = [0.0f32; 128];
        super::realtime(|| {
            for (b, slot) in silenced.iter_mut().enumerate() {
                if b == 1 {
                    k.set_param(InsertId(2), 0, 1.0).unwrap();
                }
                let blocks = [SourceBlock {
                    id: SourceId::Channel(1),
                    left: &s1l[..128],
                    right: &s1r[..128],
                }];
                *slot = k
                    .render(
                        &transport(sr, b as u64 * 128, 128),
                        &blocks,
                        &mut fl,
                        &mut fr,
                    )
                    .unwrap()
                    .silenced;
                if b == 1 {
                    after = fl;
                }
            }
        });
        assert_eq!(silenced, [0, 1, 0]);
        assert_eq!(after, [0.0; 128]);
        // The block after the fault is a fresh saturation of the same input.
        let mut fresh = sat(sr);
        let (mut l, mut r) = (s1l[..128].to_vec(), s1r[..128].to_vec());
        fresh.process(&mut l, &mut r, None, &[]);
        assert_eq!(fl.to_vec(), l);
        assert_ne!(fl.to_vec(), s1l[..128].to_vec());
    }
}
