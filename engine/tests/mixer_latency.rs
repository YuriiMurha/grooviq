//! Compensation planner: every expectation is timed by hand from the topology, never
//! taken from the planner.
use grooviq_engine::mixer::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::mem::size_of;

struct AllocationMeter;
thread_local! {
    // Requested payload only, on the thread measuring the planner. Other tests stay parallel.
    static ALLOCATION: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

// SAFETY: forwards the allocator contract unchanged; the thread-local counters own no heap.
unsafe impl GlobalAlloc for AllocationMeter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's valid layout is forwarded to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let _ = ALLOCATION.try_with(|c| {
                if let Some((live, peak)) = c.get() {
                    let live = live + layout.size();
                    c.set(Some((live, peak.max(live))));
                }
            });
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = ALLOCATION.try_with(|c| {
            if let Some((live, peak)) = c.get() {
                c.set(Some((live.saturating_sub(layout.size()), peak)));
            }
        });
        // SAFETY: the original allocation and layout are forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: AllocationMeter = AllocationMeter;

fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATION.with(|c| {
        assert!(c.get().is_none());
        c.set(Some((0, 0)));
    });
    let result = f();
    let (_, peak) = ALLOCATION.with(|c| c.replace(None).unwrap());
    (result, peak)
}

fn track(id: u64, output: Option<u64>, inserts: &[(u64, u32)]) -> Track {
    Track {
        id: MixerTrackId(id),
        output: output.map(MixerTrackId),
        inserts: inserts
            .iter()
            .map(|&(id, latency_frames)| Insert {
                id: InsertId(id),
                latency_frames,
                memory_bytes: 0,
            })
            .collect(),
    }
}
fn source(channel: u64, to: u64) -> Source {
    Source {
        id: SourceId::Channel(channel),
        destination: MixerTrackId(to),
    }
}
fn send(id: u64, from: u64, to: u64, tap: Tap, gain: f32) -> Send {
    Send {
        id: SendId(id),
        from: MixerTrackId(from),
        to: MixerTrackId(to),
        tap,
        gain,
    }
}
fn sidechain(from: u64, to: u64, tap: Tap) -> Sidechain {
    Sidechain {
        from: MixerTrackId(from),
        to: InsertId(to),
        tap,
    }
}
fn actual(pairs: &[(u64, u32)]) -> Vec<ActualLatency> {
    pairs
        .iter()
        .map(|&(id, frames)| ActualLatency {
            insert: InsertId(id),
            frames,
        })
        .collect()
}
const ROOMY: PdcBudget = PdcBudget {
    descriptors: u64::MAX,
    ring_samples: u64::MAX,
    memory_bytes: u64::MAX,
};
fn plan(m: &Mixer, snapshot: &[(u64, u32)]) -> LatencyPlan {
    let compiled = compile(m, Budget::default()).unwrap();
    plan_latency(&compiled, &actual(snapshot), ROOMY).unwrap()
}
fn delay(p: &LatencyPlan, at: PdcPoint) -> u64 {
    let found: Vec<_> = p.delays.iter().filter(|d| d.at == at).collect();
    assert_eq!(found.len(), 1, "{at:?}");
    found[0].frames
}
fn main_in(track: u64, from: u64) -> PdcPoint {
    PdcPoint::Audible {
        track: MixerTrackId(track),
        input: InputId::Main(MixerTrackId(from)),
    }
}
fn stage(track: u64, insert: u64) -> PdcPoint {
    PdcPoint::Stage {
        track: MixerTrackId(track),
        insert: InsertId(insert),
    }
}
fn detector(track: u64, insert: u64, from: u64, tap: Tap) -> PdcPoint {
    PdcPoint::Detector {
        track: MixerTrackId(track),
        insert: InsertId(insert),
        input: InputId::Sidechain {
            from: MixerTrackId(from),
            insert: InsertId(insert),
            tap,
        },
    }
}
fn arrival(p: &LatencyPlan, track: u64) -> (u64, u64) {
    let t = p
        .tracks
        .iter()
        .find(|t| t.track == MixerTrackId(track))
        .unwrap();
    (t.gather, t.tap)
}
fn ring(p: &LatencyPlan) -> u64 {
    p.resources.ring_samples
}

/// 1: inserts 10 (5) then 11 (7) = 12. 2: insert 20 (30) = 30. Master aligns to 30.
fn parallel() -> Mixer {
    Mixer {
        tracks: vec![
            track(0, None, &[]),
            track(1, Some(0), &[(10, 8), (11, 7)]),
            track(2, Some(0), &[(20, 64)]),
        ],
        sources: vec![source(1, 1), source(2, 2)],
        ..Mixer::default()
    }
}

#[test]
fn unequal_parallel_chains_align_at_master() {
    let p = plan(&parallel(), &[(10, 5), (11, 7), (20, 30)]);
    assert_eq!(delay(&p, main_in(0, 1)), 18);
    assert_eq!(delay(&p, main_in(0, 2)), 0);
    for (t, i) in [(1, 10), (1, 11), (2, 20)] {
        assert_eq!(delay(&p, stage(t, i)), 0);
    }
    assert_eq!(arrival(&p, 1), (0, 12));
    assert_eq!(arrival(&p, 2), (0, 30));
    assert_eq!(arrival(&p, 0), (30, 30));
    assert_eq!(p.master_frames, 30);
    // Stereo: 18 frames on two channels.
    assert_eq!(ring(&p), 36);
    assert_eq!(p.resources.ring_bytes, 36 * 4);
    // 2 sources + 2 mains audible, 3 stages, 0 detectors, 3 tracks.
    assert_eq!(p.resources.descriptors, 10);
    assert_eq!(p.delays.len(), 7);
}

#[test]
fn declared_bounds_are_not_timing() {
    // Declared 8, 7 and 64; actual 5, 7 and 30 time the plan.
    let p = plan(&parallel(), &[(10, 5), (11, 7), (20, 30)]);
    assert_ne!(p.master_frames, 64);
    let q = plan(&parallel(), &[(10, 8), (11, 7), (20, 64)]);
    assert_eq!(delay(&q, main_in(0, 1)), 49);
    assert_eq!(q.master_frames, 64);
}

/// Track 3 chain: 30 (4) -> 31 (6) -> 32 (2), detectors from 4 at 30, 1 at 31, 2 at 32.
/// Track 1 arrives at 20, 2 at 3, 4 at 0.
fn detectors() -> Mixer {
    Mixer {
        tracks: vec![
            track(0, None, &[]),
            track(1, Some(0), &[(10, 20)]),
            track(2, Some(0), &[(20, 3)]),
            track(3, Some(0), &[(30, 4), (31, 6), (32, 2)]),
            track(4, Some(0), &[]),
        ],
        sidechains: vec![
            sidechain(2, 32, Tap::PostFader),
            sidechain(1, 31, Tap::PreFader),
            sidechain(4, 30, Tap::PostFader),
        ],
        sources: vec![source(1, 1), source(2, 2), source(3, 3), source(4, 4)],
        ..Mixer::default()
    }
}
const DETECTOR_LATENCY: [(u64, u32); 5] = [(10, 20), (20, 3), (30, 4), (31, 6), (32, 2)];

#[test]
fn each_detector_aligns_with_its_named_insert_input() {
    let p = plan(&detectors(), &DETECTOR_LATENCY);
    // 30: chain 0, detector 0 -> 0; chain 4.
    assert_eq!(delay(&p, stage(3, 30)), 0);
    assert_eq!(delay(&p, detector(3, 30, 4, Tap::PostFader)), 0);
    // 31: chain 4, detector 20 -> 20; stage waits 16; chain 26.
    assert_eq!(delay(&p, stage(3, 31)), 16);
    assert_eq!(delay(&p, detector(3, 31, 1, Tap::PreFader)), 0);
    // 32: chain 26, detector 3 -> 26; detector waits 23; chain 28.
    assert_eq!(delay(&p, stage(3, 32)), 0);
    assert_eq!(delay(&p, detector(3, 32, 2, Tap::PostFader)), 23);
    assert_eq!(arrival(&p, 3), (0, 28));
    // Master: 1 at 20, 2 at 3, 3 at 28, 4 at 0.
    assert_eq!(delay(&p, main_in(0, 1)), 8);
    assert_eq!(delay(&p, main_in(0, 2)), 25);
    assert_eq!(delay(&p, main_in(0, 3)), 0);
    assert_eq!(delay(&p, main_in(0, 4)), 28);
    assert_eq!(p.master_frames, 28);
    assert_eq!(ring(&p), 2 * (16 + 23 + 8 + 25 + 28));
}

#[test]
fn delays_follow_execution_and_chain_order() {
    let p = plan(&detectors(), &DETECTOR_LATENCY);
    let at: Vec<_> = p
        .delays
        .iter()
        .filter(|d| matches!(d.at, PdcPoint::Stage { track, .. } | PdcPoint::Detector { track, .. } if track == MixerTrackId(3)))
        .map(|d| d.at)
        .collect();
    assert_eq!(
        at,
        vec![
            stage(3, 30),
            detector(3, 30, 4, Tap::PostFader),
            stage(3, 31),
            detector(3, 31, 1, Tap::PreFader),
            stage(3, 32),
            detector(3, 32, 2, Tap::PostFader),
        ]
    );
    let order: Vec<_> = p.tracks.iter().map(|t| t.track.0).collect();
    // 4 feeds 3's detector, so 3 runs in the second level.
    assert_eq!(order, vec![1, 2, 4, 3, 0]);
}

#[test]
fn two_detectors_on_one_insert_align_to_the_later() {
    let mut m = detectors();
    m.sidechains.push(sidechain(2, 31, Tap::PostFader));
    let p = plan(&m, &DETECTOR_LATENCY);
    assert_eq!(delay(&p, detector(3, 31, 1, Tap::PreFader)), 0);
    assert_eq!(delay(&p, detector(3, 31, 2, Tap::PostFader)), 17);
    assert_eq!(delay(&p, stage(3, 31)), 16);
}

#[test]
fn late_detector_on_first_insert_delays_the_whole_chain() {
    let mut m = detectors();
    m.sidechains = vec![sidechain(1, 30, Tap::PreFader)];
    let p = plan(&m, &DETECTOR_LATENCY);
    // 30 waits for 20, then 4 + 6 + 2.
    assert_eq!(delay(&p, stage(3, 30)), 20);
    assert_eq!(arrival(&p, 3), (0, 32));
    assert_eq!(p.master_frames, 32);
}

/// 1 (10) pre-fader sends silently to bus 5; 2 (none) post-fader sends to 5.
/// Bus 5 (3) and master (7).
fn buses(gain: f32) -> Mixer {
    Mixer {
        tracks: vec![
            track(0, None, &[(1, 7)]),
            track(1, Some(0), &[(10, 10)]),
            track(2, Some(0), &[]),
            track(5, Some(0), &[(50, 3)]),
        ],
        sends: vec![
            send(8, 1, 5, Tap::PreFader, gain),
            send(9, 2, 5, Tap::PostFader, 1.0),
        ],
        sources: vec![source(1, 1), source(2, 2), source(5, 5)],
        ..Mixer::default()
    }
}

#[test]
fn sends_buses_and_master_share_tap_arrivals() {
    let p = plan(&buses(0.0), &[(1, 7), (10, 10), (50, 3)]);
    let send_in = |id| PdcPoint::Audible {
        track: MixerTrackId(5),
        input: InputId::Send(SendId(id)),
    };
    let source_in = PdcPoint::Audible {
        track: MixerTrackId(5),
        input: InputId::Source(SourceId::Channel(5)),
    };
    // Bus gathers pre-fader 1 at 10, post-fader 2 at 0, its own source at 0.
    assert_eq!(delay(&p, send_in(8)), 0);
    assert_eq!(delay(&p, send_in(9)), 10);
    assert_eq!(delay(&p, source_in), 10);
    assert_eq!(arrival(&p, 5), (10, 13));
    // Master: 1 at 10, 2 at 0, 5 at 13; then master insert 7.
    assert_eq!(delay(&p, main_in(0, 1)), 3);
    assert_eq!(delay(&p, main_in(0, 2)), 13);
    assert_eq!(delay(&p, main_in(0, 5)), 0);
    assert_eq!(arrival(&p, 0), (13, 20));
    assert_eq!(p.master_frames, 20);
    // A silent send is still a dependency: gain never changes timing.
    assert_eq!(p, plan(&buses(1.0), &[(1, 7), (10, 10), (50, 3)]));
}

#[test]
fn long_delays_span_many_blocks() {
    let p = plan(&parallel(), &[(10, 0), (11, 0), (20, 0)]);
    assert_eq!(ring(&p), 0);
    let mut m = parallel();
    m.tracks[2].inserts[0].latency_frames = 1_000_000;
    let p = plan(&m, &[(10, 5), (11, 7), (20, 1_000_000)]);
    assert_eq!(delay(&p, main_in(0, 1)), 999_988);
    assert_eq!(p.master_frames, 1_000_000);
    assert_eq!(ring(&p), 1_999_976);
}

#[test]
fn zero_latency_keeps_every_descriptor_at_zero() {
    let p = plan(&detectors(), &[(10, 0), (20, 0), (30, 0), (31, 0), (32, 0)]);
    // 4 sources + 4 mains, 5 stages, 3 detectors.
    assert_eq!(p.delays.len(), 16);
    assert!(p.delays.iter().all(|d| d.frames == 0));
    assert!(p.tracks.iter().all(|t| t.gather == 0 && t.tap == 0));
    assert_eq!(
        (p.master_frames, ring(&p), p.resources.ring_bytes),
        (0, 0, 0)
    );
}

#[test]
fn master_only_plans_empty() {
    let m = Mixer {
        tracks: vec![track(0, None, &[])],
        ..Mixer::default()
    };
    let p = plan(&m, &[]);
    assert!(p.delays.is_empty());
    assert_eq!(
        p.tracks,
        vec![TrackArrival {
            track: MASTER,
            gather: 0,
            tap: 0
        }]
    );
    assert_eq!(p.master_frames, 0);
}

#[test]
fn snapshot_order_never_changes_the_plan_or_refusal() {
    let compiled = compile(&detectors(), Budget::default()).unwrap();
    let base = actual(&DETECTOR_LATENCY);
    let expected = plan_latency(&compiled, &base, ROOMY).unwrap();
    let mut bad = base.clone();
    bad.push(ActualLatency {
        insert: InsertId(99),
        frames: 0,
    });
    bad.push(ActualLatency {
        insert: InsertId(31),
        frames: 1,
    });
    for r in 0..bad.len() {
        let mut s = base.clone();
        let n = r % s.len();
        s.rotate_left(n);
        if r % 2 == 1 {
            s.reverse();
        }
        assert_eq!(plan_latency(&compiled, &s, ROOMY).unwrap(), expected);
        let mut b = bad.clone();
        b.rotate_left(r);
        assert_eq!(
            plan_latency(&compiled, &b, ROOMY),
            Err(LatencyError::DuplicateLatency(InsertId(31)))
        );
    }
}

#[test]
fn wrong_snapshots_are_refused() {
    let compiled = compile(&parallel(), Budget::default()).unwrap();
    let run = |s: &[(u64, u32)]| plan_latency(&compiled, &actual(s), ROOMY);
    assert_eq!(run(&[]), Err(LatencyError::MissingLatency(InsertId(10))));
    assert_eq!(
        run(&[(10, 5), (20, 30)]),
        Err(LatencyError::MissingLatency(InsertId(11)))
    );
    assert_eq!(
        run(&[(10, 5), (11, 7), (20, 30), (10, 5)]),
        Err(LatencyError::DuplicateLatency(InsertId(10)))
    );
    assert_eq!(
        run(&[(10, 5), (11, 7), (20, 30), (21, 0)]),
        Err(LatencyError::UnknownLatency(InsertId(21)))
    );
    assert_eq!(
        run(&[(10, 9), (11, 7), (20, 30)]),
        Err(LatencyError::AboveDeclared {
            insert: InsertId(10),
            actual: 9,
            declared: 8
        })
    );
    // The declaration itself is admitted.
    assert!(run(&[(10, 8), (11, 7), (20, 64)]).is_ok());
}

#[test]
fn malformed_compiled_plans_are_refused() {
    let good = compile(&detectors(), Budget::default()).unwrap();
    let snapshot = actual(&DETECTOR_LATENCY);
    let refuse = |c: &CompiledMixer| match plan_latency(c, &snapshot, ROOMY) {
        Err(LatencyError::Plan(e)) => e,
        other => panic!("{other:?}"),
    };
    let at = |c: &CompiledMixer, id: u64| {
        c.tracks
            .iter()
            .position(|t| t.track.id == MixerTrackId(id))
            .unwrap()
    };

    let mut c = good.clone();
    let dup = c.tracks[1].clone();
    c.tracks.push(dup);
    assert_eq!(refuse(&c), PlanError::DuplicateTrack(MixerTrackId(1)));

    let mut c = good.clone();
    c.levels.pop();
    assert_eq!(refuse(&c), PlanError::UnscheduledTrack(MASTER));

    let mut c = good.clone();
    c.levels.push(vec![MixerTrackId(77)]);
    assert_eq!(refuse(&c), PlanError::UnknownTrack(MixerTrackId(77)));

    let mut c = good.clone();
    c.levels[0].push(MixerTrackId(1));
    assert_eq!(refuse(&c), PlanError::DuplicateTrack(MixerTrackId(1)));

    let mut c = good.clone();
    let master = c.levels.pop().unwrap();
    c.levels.insert(0, master);
    assert_eq!(refuse(&c), PlanError::MasterNotLast);

    let mut c = good.clone();
    let t = at(&c, 3);
    c.tracks[t].inputs.reverse();
    assert_eq!(refuse(&c), PlanError::UnsortedInputs(MixerTrackId(3)));

    let mut c = good.clone();
    let t = at(&c, 0);
    c.tracks[t].inputs[0].gain = f32::NAN;
    assert_eq!(refuse(&c), PlanError::MalformedInput(MASTER));

    let mut c = good.clone();
    let t = at(&c, 0);
    c.tracks[t].inputs[0].from = Some(MixerTrackId(2));
    assert_eq!(refuse(&c), PlanError::MalformedInput(MASTER));

    let mut c = good.clone();
    let t = at(&c, 1);
    c.tracks[t].inputs[0].from = Some(MixerTrackId(2));
    assert_eq!(refuse(&c), PlanError::MalformedInput(MixerTrackId(1)));

    // Master feeding track 3 is not earlier.
    let mut c = good.clone();
    let t = at(&c, 3);
    c.tracks[t].inputs.insert(
        0,
        Input {
            id: InputId::Main(MASTER),
            from: Some(MASTER),
            tap: Tap::PostFader,
            gain: 1.0,
        },
    );
    assert_eq!(refuse(&c), PlanError::NotEarlier(MASTER));

    // A sidechain on track 3 naming track 1's insert.
    let mut c = good.clone();
    let t = at(&c, 3);
    for i in &mut c.tracks[t].inputs {
        if let InputId::Sidechain { insert, .. } = &mut i.id
            && *insert == InsertId(30)
        {
            *insert = InsertId(10);
        }
    }
    c.tracks[t].inputs.sort_by_key(|i| i.id);
    assert_eq!(refuse(&c), PlanError::ForeignInsert(InsertId(10)));

    let mut c = good.clone();
    let (a, b) = (at(&c, 1), at(&c, 2));
    c.tracks[b].inputs[0].id = c.tracks[a].inputs[0].id;
    assert_eq!(refuse(&c), PlanError::DuplicateSource(SourceId::Channel(1)));

    let mut c = good.clone();
    let (a, b) = (at(&c, 1), at(&c, 2));
    c.tracks[b].track.inserts[0].id = c.tracks[a].track.inserts[0].id;
    assert_eq!(refuse(&c), PlanError::DuplicateInsert(InsertId(10)));
}

#[test]
fn budgets_admit_the_exact_boundary_and_refuse_one_less() {
    let compiled = compile(&detectors(), Budget::default()).unwrap();
    let snapshot = actual(&DETECTOR_LATENCY);
    let p = plan_latency(&compiled, &snapshot, ROOMY).unwrap();
    // 4 sources + 4 mains + 5 stages + 3 detectors + 5 tracks.
    let descriptors = 21;
    let ring_samples = 2 * (16 + 23 + 8 + 25 + 28);
    let descriptor_bytes = (16 * size_of::<PdcDelay>() + 5 * size_of::<TrackArrival>()) as u64;
    assert_eq!(p.resources.descriptors, descriptors);
    assert_eq!(p.resources.ring_samples, ring_samples);
    assert_eq!(p.resources.ring_bytes, ring_samples * 4);
    assert_eq!(p.resources.descriptor_bytes, descriptor_bytes);
    assert!(p.resources.work_bytes > 0);
    let memory = descriptor_bytes + p.resources.work_bytes + ring_samples * 4;

    let exact = PdcBudget {
        descriptors,
        ring_samples,
        memory_bytes: memory,
    };
    assert_eq!(plan_latency(&compiled, &snapshot, exact).unwrap(), p);
    for (budget, resource, required, limit) in [
        (
            PdcBudget {
                descriptors: descriptors - 1,
                ..exact
            },
            PdcLimit::Descriptors,
            descriptors,
            descriptors - 1,
        ),
        (
            PdcBudget {
                ring_samples: ring_samples - 1,
                ..exact
            },
            PdcLimit::RingSamples,
            ring_samples,
            ring_samples - 1,
        ),
        (
            PdcBudget {
                memory_bytes: memory - 1,
                ..exact
            },
            PdcLimit::Memory,
            memory,
            memory - 1,
        ),
    ] {
        assert_eq!(
            plan_latency(&compiled, &snapshot, budget),
            Err(LatencyError::Budget {
                resource,
                required,
                limit
            })
        );
    }
    // Admission refuses before work storage when even descriptors and work cannot fit.
    let tiny = PdcBudget {
        memory_bytes: descriptor_bytes,
        ..exact
    };
    assert!(matches!(
        plan_latency(&compiled, &snapshot, tiny),
        Err(LatencyError::Budget {
            resource: PdcLimit::Memory,
            ..
        })
    ));
}

#[test]
fn scrambled_detector_sort_stays_within_admitted_peak_memory() {
    let (_, calibration) = peak_during(|| {
        let bytes = std::hint::black_box(vec![0u8; 4096]);
        std::hint::black_box(&bytes);
    });
    assert_eq!(
        calibration, 4096,
        "allocation meter must observe real payload"
    );

    const N: u64 = 64;
    const RECEIVER: u64 = 10_000;
    let inserts: Vec<_> = (1..=N).rev().map(|id| (id, 0)).collect();
    let mut m = Mixer {
        tracks: vec![track(0, None, &[]), track(RECEIVER, Some(0), &inserts)],
        ..Mixer::default()
    };
    for from in 1..=N {
        m.tracks.push(track(from, Some(0), &[]));
        for to in 1..=N {
            m.sidechains.push(sidechain(from, to, Tap::PreFader));
        }
    }
    let compiled = compile(&m, Budget::default()).unwrap();
    let snapshot = actual(&inserts);
    let initial = plan_latency(&compiled, &snapshot, ROOMY).unwrap();
    // No future rings can mask temporary sorting payload in this graph.
    assert_eq!(initial.resources.ring_samples, 0);
    let memory = initial.resources.descriptor_bytes + initial.resources.work_bytes;
    let budget = PdcBudget {
        memory_bytes: memory,
        ..ROOMY
    };
    let (result, peak) = peak_during(|| plan_latency(&compiled, &snapshot, budget));
    let p = result.unwrap();
    assert!(
        peak as u64 <= memory,
        "planner exceeded admitted requested payload: {peak} > {memory}"
    );
    assert_eq!(
        peak as u64, memory,
        "all reported storage was really allocated"
    );

    // Scrambling chain positions must not scramble equal-position detector gather order.
    let mut feeds = p.delays.iter().filter_map(|d| match d.at {
        PdcPoint::Detector {
            insert,
            input: InputId::Sidechain { from, .. },
            ..
        } => Some((insert, from)),
        _ => None,
    });
    for insert in (1..=N).rev() {
        for from in 1..=N {
            assert_eq!(feeds.next(), Some((InsertId(insert), MixerTrackId(from))));
        }
    }
    assert_eq!(feeds.next(), None);
}

#[test]
fn actual_device_latency_after_parameter_setup_times_the_plan() {
    use grooviq_dsp::create;
    for (rate, limiter) in [(44100., 65), (48000., 71), (96000., 143)] {
        let mut drive = create("drive", rate).unwrap();
        drive.set_param(3, 0.);
        drive.set_param(5, 2.); // mode with 49 frames
        let mut saturation = create("saturation", rate).unwrap();
        saturation.set_param(7, 0.);
        let limiter_device = create("limiter", rate).unwrap();
        // Musical delay timing is not compensable processing latency.
        let delay_device = create("delay", rate).unwrap();
        let measured = [
            (10, drive.latency_frames()),
            (11, delay_device.latency_frames()),
            (20, saturation.latency_frames()),
            (1, limiter_device.latency_frames()),
        ];
        assert_eq!(measured, [(10, 49), (11, 0), (20, 49), (1, limiter)]);

        // 1: drive then delay = 49. 2: saturation = 49. 3: nothing = 0. Master limiter.
        let mut m = Mixer {
            tracks: vec![
                track(0, None, &[(1, 1024)]),
                track(1, Some(0), &[(10, 1024), (11, 1024)]),
                track(2, Some(0), &[(20, 1024)]),
                track(3, Some(0), &[]),
            ],
            sources: vec![source(1, 1), source(2, 2), source(3, 3)],
            ..Mixer::default()
        };
        m.sidechains.push(sidechain(3, 1, Tap::PreFader));
        let p = plan(&m, &measured);
        assert_eq!(delay(&p, main_in(0, 1)), 0);
        assert_eq!(delay(&p, main_in(0, 2)), 0);
        assert_eq!(delay(&p, main_in(0, 3)), 49);
        assert_eq!(delay(&p, detector(0, 1, 3, Tap::PreFader)), 49);
        assert_eq!(p.master_frames, 49 + u64::from(limiter));
    }
}
