use grooviq_engine::mixer::*;

fn track(id: u64, output: Option<u64>) -> Track {
    Track {
        id: MixerTrackId(id),
        output: output.map(MixerTrackId),
        inserts: vec![],
    }
}
fn fixture() -> Mixer {
    Mixer {
        tracks: vec![
            track(0, None),
            track(3, Some(0)),
            track(2, Some(3)),
            track(1, Some(0)),
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
        sources: vec![
            Source {
                id: SourceId::AudioTrack(2),
                destination: MixerTrackId(2),
            },
            Source {
                id: SourceId::Channel(7),
                destination: MixerTrackId(1),
            },
        ],
        sidechains: vec![],
    }
}
fn checked(m: &Mixer) -> CompiledMixer {
    compile(m, Budget::default()).unwrap()
}

#[test]
fn levels_and_gather_order_are_canonical() {
    let m = fixture();
    let expected = checked(&m);
    assert_eq!(
        expected.levels,
        vec![
            vec![MixerTrackId(1), MixerTrackId(2)],
            vec![MixerTrackId(3)],
            vec![MASTER]
        ]
    );
    assert_eq!(
        expected.tracks[2].inputs[0].id,
        InputId::Source(SourceId::AudioTrack(2))
    );
    assert_eq!(
        expected.tracks[3]
            .inputs
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![
            InputId::Main(MixerTrackId(2)),
            InputId::Send(SendId(4)),
            InputId::Send(SendId(8))
        ]
    );
    for rotation in 0..m.tracks.len() {
        let mut shuffled = m.clone();
        shuffled.tracks.rotate_left(rotation);
        shuffled.sends.reverse();
        shuffled.sources.reverse();
        assert_eq!(checked(&shuffled), expected);
    }
    assert_eq!(expected.tracks[3].inputs[2].tap, Tap::PreFader);
    assert_eq!(expected.tracks[3].inputs[2].gain, 0.5);
}

#[test]
fn insert_sequence_and_sidechain_dependency_survive_compile() {
    let mut m = fixture();
    m.tracks[1].inserts = vec![
        Insert {
            id: InsertId(9),
            latency_frames: 7,
            memory_bytes: 16,
        },
        Insert {
            id: InsertId(2),
            latency_frames: 11,
            memory_bytes: 32,
        },
    ];
    m.sidechains.push(Sidechain {
        from: MixerTrackId(1),
        to: InsertId(2),
        tap: Tap::PostFader,
    });
    let c = checked(&m);
    assert_eq!(c.tracks[3].track.inserts, m.tracks[1].inserts);
    assert_eq!(c.resources.latency_frames, 18);
    assert!(matches!(
        c.tracks[3].inputs[3].id,
        InputId::Sidechain {
            insert: InsertId(2),
            ..
        }
    ));
    // A sidechain is a scheduling dependency even though it is not audible.
    m.sidechains[0].from = MASTER;
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::MasterOutgoing)
    );
}

#[test]
fn cycles_including_silent_sends_and_self_edges_are_refused() {
    let mut m = fixture();
    m.sends.push(Send {
        id: SendId(99),
        from: MixerTrackId(3),
        to: MixerTrackId(2),
        tap: Tap::PostFader,
        gain: 0.0,
    });
    assert_eq!(compile(&m, Budget::default()), Err(CompileError::Cycle));
    m.sends.last_mut().unwrap().to = MixerTrackId(3);
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::SelfEdge(MixerTrackId(3)))
    );
}

#[test]
fn invalid_references_and_identities_are_refused() {
    let base = fixture();
    let mut m = base.clone();
    m.tracks.push(m.tracks[1].clone());
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DuplicateTrack(MixerTrackId(3)))
    );
    let mut m = base.clone();
    m.sends.push(m.sends[0].clone());
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DuplicateSend(SendId(8)))
    );
    let mut m = base.clone();
    m.sources.push(m.sources[0].clone());
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DuplicateSource(SourceId::AudioTrack(2)))
    );
    let mut m = base.clone();
    m.tracks[1].output = Some(MixerTrackId(999));
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DanglingTrack(MixerTrackId(999)))
    );
    let mut m = base.clone();
    m.sidechains.push(Sidechain {
        from: MixerTrackId(1),
        to: InsertId(999),
        tap: Tap::PreFader,
    });
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DanglingInsert(InsertId(999)))
    );
    let mut m = base.clone();
    m.tracks.remove(0);
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::MissingMaster)
    );
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 4.1] {
        let mut m = base.clone();
        m.sends[0].gain = value;
        assert_eq!(
            compile(&m, Budget::default()),
            Err(CompileError::InvalidGain(SendId(8)))
        );
    }
}

#[test]
fn every_aggregate_budget_has_an_exact_boundary() {
    let mut m = fixture();
    m.tracks[1].inserts.push(Insert {
        id: InsertId(1),
        latency_frames: 10,
        memory_bytes: 128,
    });
    let r = checked(&m).resources;
    assert_eq!(
        r,
        Resources {
            nodes: 7,
            edges: 7,
            buffers: 28,
            latency_frames: 10,
            // 14 compiler records * 1024 + 28 stereo blocks * 64 * 8
            // + 7 compensation lines * 10 * 8 + 128 device bytes.
            memory_bytes: 29_360,
        }
    );
    let exact = Budget {
        nodes: r.nodes,
        edges: r.edges,
        buffers: r.buffers,
        latency_frames: r.latency_frames,
        memory_bytes: r.memory_bytes,
        block_frames: 64,
    };
    assert_eq!(compile(&m, exact).unwrap().resources, r);
    for resource in [
        Limit::Nodes,
        Limit::Edges,
        Limit::Buffers,
        Limit::Latency,
        Limit::Memory,
    ] {
        let mut b = exact;
        match resource {
            Limit::Nodes => b.nodes -= 1,
            Limit::Edges => b.edges -= 1,
            Limit::Buffers => b.buffers -= 1,
            Limit::Latency => b.latency_frames -= 1,
            Limit::Memory => b.memory_bytes -= 1,
        }
        assert!(
            matches!(compile(&m, b), Err(CompileError::Budget { resource: actual, .. }) if actual == resource)
        );
    }
    m.tracks[1].inserts[0].memory_bytes = u64::MAX;
    assert_eq!(compile(&m, Budget::default()), Err(CompileError::Overflow));
}

#[test]
fn long_chains_are_iterative_and_insert_counts_are_aggregate() {
    let mut m = Mixer {
        tracks: vec![track(0, None)],
        ..Mixer::default()
    };
    for id in 1..=2000 {
        m.tracks.push(track(id, Some(id - 1)));
    }
    m.tracks[1].inserts = (0..100)
        .map(|id| Insert {
            id: InsertId(id),
            latency_frames: 0,
            memory_bytes: 0,
        })
        .collect();
    let c = checked(&m);
    assert_eq!(c.levels.len(), 2001);
    assert_eq!(c.tracks[1].track.inserts.len(), 100);
    m.tracks[1].output = Some(MixerTrackId(2000));
    assert_eq!(compile(&m, Budget::default()), Err(CompileError::Cycle));
}

#[test]
fn insert_and_sidechain_duplicates_and_output_rules_are_refused() {
    let mut m = fixture();
    let insert = Insert {
        id: InsertId(1),
        latency_frames: 0,
        memory_bytes: 0,
    };
    m.tracks[1].inserts = vec![insert.clone(), insert];
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DuplicateInsert(InsertId(1)))
    );
    m.tracks[1].inserts.pop();
    let sidechain = Sidechain {
        from: MixerTrackId(1),
        to: InsertId(1),
        tap: Tap::PreFader,
    };
    m.sidechains = vec![sidechain.clone(), sidechain];
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::DuplicateSidechain)
    );
    m.sidechains.clear();
    m.tracks[0].output = Some(MixerTrackId(1));
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::InvalidOutput(MASTER))
    );
    m.tracks[0].output = None;
    m.tracks[1].output = None;
    assert_eq!(
        compile(&m, Budget::default()),
        Err(CompileError::InvalidOutput(MixerTrackId(3)))
    );
    assert_eq!(
        compile(
            &fixture(),
            Budget {
                block_frames: 0,
                ..Budget::default()
            }
        ),
        Err(CompileError::InvalidBlockSize)
    );
}

#[test]
fn sidechains_alone_can_create_a_cycle() {
    let mut m = fixture();
    m.tracks[2].inserts.push(Insert {
        id: InsertId(1),
        latency_frames: 0,
        memory_bytes: 0,
    });
    m.sidechains.push(Sidechain {
        from: MixerTrackId(3),
        to: InsertId(1),
        tap: Tap::PreFader,
    });
    assert_eq!(compile(&m, Budget::default()), Err(CompileError::Cycle));
}

#[test]
fn source_namespaces_and_gather_order_are_stable() {
    let mut m = fixture();
    m.sources = vec![
        Source {
            id: SourceId::AudioTrack(1),
            destination: MixerTrackId(1),
        },
        Source {
            id: SourceId::Channel(8),
            destination: MixerTrackId(1),
        },
        Source {
            id: SourceId::Channel(1),
            destination: MixerTrackId(1),
        },
    ];
    let c = checked(&m);
    assert_eq!(
        c.tracks[1].inputs.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![
            InputId::Source(SourceId::Channel(1)),
            InputId::Source(SourceId::Channel(8)),
            InputId::Source(SourceId::AudioTrack(1)),
        ]
    );
    m.sources.reverse();
    assert_eq!(checked(&m), c);
}

#[test]
fn admission_precedes_graph_copy_and_validation() {
    let mut m = fixture();
    // Duplicate identities would be rejected during graph construction. Node
    // admission takes precedence, without copying these descriptors into maps.
    m.tracks.push(m.tracks[1].clone());
    assert!(matches!(
        compile(
            &m,
            Budget {
                nodes: 0,
                ..Budget::default()
            }
        ),
        Err(CompileError::Budget {
            resource: Limit::Nodes,
            ..
        })
    ));
}
