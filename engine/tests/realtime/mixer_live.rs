//! Live mixer installs, survivor transfers, pulled sources, track-only audio moves, tail
//! resets and full-trash backpressure, all through the real callback.

use grooviq_engine::mixer::*;
use grooviq_engine::*;
use std::sync::Arc;

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

fn device(insert: u64, kind: &str, params: Vec<f32>) -> InsertDevice {
    InsertDevice {
        insert: InsertId(insert),
        kind: kind.into(),
        seed: 7,
        params,
    }
}

/// `variant` changes gains, parameters and the chain, so installs exercise survivors,
/// parameter deltas, fresh devices and displaced ones.
fn mix(variant: u32) -> MixSetup {
    let mut t2 = vec![(20, 0)];
    let mut devices = vec![
        device(10, "algorithmic_reverb", vec![]),
        device(20, "comp", vec![-20.0 - variant as f32, 4.0]),
        device(30, "delay", vec![]),
    ];
    if variant % 2 == 1 {
        t2.push((40, 0));
        devices.push(device(40, "eq", vec![]));
    }
    MixSetup::Routed(RoutedMix {
        mixer: Mixer {
            tracks: vec![
                track(0, None, &[]),
                track(1, Some(0), &[(10, 0)]),
                track(2, Some(0), &t2),
                track(3, Some(0), &[(30, 0)]),
            ],
            sends: vec![Send {
                id: SendId(1),
                from: MixerTrackId(1),
                to: MixerTrackId(3),
                tap: Tap::PreFader,
                gain: 0.3,
            }],
            sidechains: vec![Sidechain {
                from: MixerTrackId(1),
                to: InsertId(20),
                tap: Tap::PostFader,
            }],
            sources: vec![
                Source {
                    id: SourceId::Channel(3),
                    destination: MixerTrackId(1),
                },
                Source {
                    id: SourceId::Channel(7),
                    destination: MixerTrackId(2),
                },
                Source {
                    id: SourceId::AudioTrack(1),
                    destination: MixerTrackId(1),
                },
                Source {
                    id: SourceId::AudioTrack(2),
                    destination: MixerTrackId(2),
                },
                Source {
                    id: SourceId::System(SystemSource::Synth),
                    destination: MixerTrackId(3),
                },
            ],
        },
        gains: vec![TrackGain {
            track: MixerTrackId(0),
            left: 0.5 + variant as f32 * 0.01,
            right: 0.5,
        }],
        devices,
    })
}

#[test]
fn live_mixer_installs_and_pulled_sources_never_allocate() {
    let buf = Arc::new(SampleBuf {
        rate: 48_000.0,
        l: (0..48_000)
            .map(|i| ((i % 89) as f32 - 44.0) / 100.0)
            .collect(),
        r: (0..48_000)
            .map(|i| ((i % 53) as f32 - 26.0) / 90.0)
            .collect(),
    });
    let clip = |id, start_tick| AudioClip {
        id,
        asset: 1,
        start_tick,
        source_start: 0,
        source_end: 40_000,
        gain: 0.8,
        reverse: id % 2 == 0,
        fade_in: 32,
        fade_out: 32,
    };
    let audio = AudioSchedule::build(
        vec![AudioAsset {
            id: 1,
            source_rate: 48_000,
            source_frames: 48_000,
            decoded: Some(buf.clone()),
        }],
        vec![clip(9, 0), clip(4, 30), clip(6, 90)],
    )
    .unwrap()
    .0
    .with_tracks(&[(9, 1), (4, 2), (6, 1)])
    .unwrap();
    let state = EngineState {
        bpm: 120.0,
        playback_mode: PlaybackMode::Song,
        channels: vec![
            ChannelSetup::sampler(ChannelId(3), Some(buf), vec![], ChannelMix::default()),
            ChannelSetup::synth(ChannelId(7), vec![], ChannelMix::default()),
        ],
        song: SongData {
            audio,
            end_tick: 100_000,
            end_frame: 2_500_000,
            frame_rate: 48_000.0,
            frame_extent: None,
            ..SongData::default()
        },
        mix: mix(0),
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48_000.0,
        },
        state,
    );
    let note = |channel, on| Command::ChannelNote {
        channel: ChannelId(channel),
        key: 60,
        velocity: 0.9,
        on,
        id: NoteId(u64::from(channel)),
    };
    c.send(Command::Play).unwrap();
    c.send(note(3, true)).unwrap();
    c.send(note(7, true)).unwrap();
    c.send(Command::Note {
        key: 50,
        velocity: 1.0,
        on: true,
        id: NoteId(1),
    })
    .unwrap();
    let mut out = [0.0; 128];
    let mut odd = [0.0; 74];
    for block in 0..400u32 {
        match block % 50 {
            5 => {
                c.set_mix(mix(block / 50 + 1)).unwrap();
            }
            15 => {
                let mut song = c.state().song.clone();
                let moved = if block % 100 == 15 {
                    [(9, 2), (4, 1), (6, 2)]
                } else {
                    [(9, 1), (4, 2), (6, 1)]
                };
                song.audio = song.audio.with_tracks(&moved).unwrap();
                c.set_song(song).unwrap();
            }
            20 => {
                c.remove_channel(ChannelId(3)).unwrap();
                c.add_channel(ChannelSetup::sampler(
                    ChannelId(3),
                    None,
                    vec![],
                    ChannelMix::default(),
                ))
                .unwrap();
            }
            25 => c.send(Command::Seek { tick: 40 }).unwrap(),
            30 if block == 230 => c.set_mix(MixSetup::LegacyDirect).map(|_| ()).unwrap(),
            31 if block == 231 => c.set_mix(mix(9)).map(|_| ()).unwrap(),
            35 => c.send(Command::AllSoundOff).unwrap(),
            // Replace a sounding channel, move clips and change the mix in one transaction.
            40 | 41 => {
                let s = c.state();
                let mut song = s.song.clone();
                let flip = block % 2 == 0;
                song.audio = song
                    .audio
                    .with_tracks(&if flip {
                        [(9, 2), (4, 1), (6, 2)]
                    } else {
                        [(9, 1), (4, 2), (6, 1)]
                    })
                    .unwrap();
                let edit = SourceEdit {
                    remove: vec![ChannelId(7)],
                    add: vec![ChannelSetup::synth(
                        ChannelId(7),
                        vec![],
                        ChannelMix::default(),
                    )],
                    timeline: Some(TimelineUpdate {
                        pattern: Box::new(s.pattern.clone()),
                        song: Box::new(song),
                        bpm: s.bpm,
                        mode: s.playback_mode,
                        region: s.song_loop,
                        remove_channel: None,
                    }),
                    mix: mix(block / 50 + 20),
                };
                c.set_sources_and_mix(edit).unwrap();
                c.send(note(7, true)).unwrap();
            }
            36 => {
                c.send(Command::Play).unwrap();
                c.send(note(7, true)).unwrap();
            }
            _ => {}
        }
        if block % 3 == 0 {
            realtime_process(&mut p, &mut odd);
        }
        realtime_process(&mut p, &mut out);
        // Retirement is freed here, on the control thread, except in a stretch where the
        // trash queue fills and installs must wait in the command queue.
        if !(300..340).contains(&block) {
            let _ = c.check_health();
        }
    }
    // A burst with the trash never freed: installs park and wait, nothing is dropped live.
    for v in 0..80 {
        let _ = c.set_mix(mix(v));
        realtime_process(&mut p, &mut out);
    }
    for _ in 0..200 {
        realtime_process(&mut p, &mut out);
        let _ = c.check_health();
    }
    assert!(out.iter().all(|x| x.is_finite()));
}

fn realtime_process(p: &mut AudioProcessor, out: &mut [f32]) {
    super::realtime(|| p.process(out, 2));
}

const FADING_CAPACITY: u32 = MAX_CHANNELS as u32;

fn synth(id: u32) -> ChannelSetup {
    ChannelSetup::synth(ChannelId(id), vec![], ChannelMix::default())
}

fn edit(remove: Vec<u32>, add: Vec<u32>) -> SourceEdit {
    SourceEdit {
        remove: remove.into_iter().map(ChannelId).collect(),
        add: add.into_iter().map(synth).collect(),
        timeline: None,
        mix: MixSetup::LegacyDirect,
    }
}

fn start_note(channel: u32, id: u64) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key: 60,
        velocity: 0.8,
        on: true,
        id: NoteId(id),
    }
}

/// MAX_LEAVING channels fading, then one more removal: the node with no fading room comes
/// back through the transaction's reserved `removed` capacity inside the callback.
#[test]
fn full_fading_room_returns_removed_nodes_under_guard() {
    let state = EngineState {
        channels: (0..FADING_CAPACITY).map(synth).collect(),
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48_000.0,
        },
        state,
    );
    let mut out = [0.0; 128];
    super::realtime(|| p.process(&mut out, 2));
    // Replace all 256 (256 leaving), then remove one more before any fade can finish.
    c.set_sources_and_mix(edit(
        (0..FADING_CAPACITY).collect(),
        (1000..1000 + FADING_CAPACITY).collect(),
    ))
    .unwrap();
    // This queued voice must disappear with the extra immediately removed node.
    c.send(start_note(1000, 1)).unwrap();
    c.set_sources_and_mix(edit(vec![1000], vec![])).unwrap();
    c.send(start_note(1001, 2)).unwrap();
    super::realtime(|| p.process(&mut out, 2));

    // An isolated source proves the new nodes actually reached the graph and that the
    // extra removed node contributes no sound, rather than only checking the mirror.
    let (mut reference, mut renderer) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let mut expected = [0.0; 128];
    super::realtime(|| renderer.process(&mut expected, 2));
    reference.add_channel(synth(1001)).unwrap();
    reference.send(start_note(1001, 2)).unwrap();
    super::realtime(|| renderer.process(&mut expected, 2));
    assert!(expected.iter().any(|x| x.abs() > 1e-6));
    assert_eq!(out, expected);
    for _ in 0..50 {
        super::realtime(|| p.process(&mut out, 2));
        let _ = c.check_health();
    }
    assert_eq!(c.state().channels.len(), FADING_CAPACITY as usize - 1);
    assert!(out.iter().all(|x| x.is_finite()));
}

/// A source transaction reaching a faulted graph is refused whole inside the callback and
/// returns every owned node in its envelope.
#[test]
fn faulted_graph_returns_refused_sources_under_guard() {
    let state = EngineState {
        channels: vec![synth(1)],
        ..EngineState::default()
    };
    let (mut c, mut p) = engine_with(
        EngineConfig {
            sample_rate: 48_000.0,
        },
        state,
    );
    let mut out = [0.0; 128];
    super::realtime(|| p.process(&mut out, 2));
    c.send(Command::InjectPanic).unwrap();
    // The panic itself allocates its payload: outside the guard, as a fault is.
    p.process(&mut out, 2);
    // The controller has not collected the fault yet: it admits the edit.
    c.set_sources_and_mix(edit(vec![1], vec![1, 2, 3])).unwrap();
    c.set_mix(MixSetup::LegacyDirect).unwrap();
    super::realtime(|| p.process(&mut out, 2));
    super::realtime(|| p.process(&mut out, 2));
    let events = c.check_health();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HealthEvent::Restarted { .. })),
        "{events:?}"
    );
    for _ in 0..20 {
        super::realtime(|| p.process(&mut out, 2));
        let _ = c.check_health();
    }
    assert_eq!(c.state().channels.len(), 3);
    assert!(out.iter().all(|x| x.is_finite()));
}
