use grooviq_engine::*;

fn note(tick: u32, length: u32) -> SeqNote {
    SeqNote {
        tick,
        length,
        channel: ChannelId(1),
        key: 60,
        velocity: 0.7,
        slide: false,
    }
}
fn pattern(notes: Vec<SeqNote>) -> Vec<(u32, PatternData)> {
    vec![(7, PatternData { length: 16, notes })]
}
fn clip(id: u32, start: u64, length: u64, offset: u32) -> PatternClip {
    PatternClip {
        id,
        pattern: 7,
        start,
        length,
        offset,
        muted: false,
    }
}
#[test]
fn repeats_offset_and_clips_release_without_resurrecting_trimmed_notes() {
    let p = pattern(vec![note(0, 8), note(6, 12)]);
    let song = compile_pattern_clips(&p, &[clip(2, 10, 20, 4)], 10).unwrap();
    assert_eq!(song.length, 30);
    assert_eq!(
        song.notes
            .iter()
            .map(|n| (n.tick, n.length))
            .collect::<Vec<_>>(),
        vec![(12, 12), (22, 8), (28, 2)]
    );
}
#[test]
fn split_recombines_onsets_and_release_at_slice_boundary() {
    let p = pattern(vec![note(0, 2), note(6, 2)]);
    let whole = compile_pattern_clips(&p, &[clip(1, 0, 48, 3)], 100).unwrap();
    let split = compile_pattern_clips(&p, &[clip(1, 0, 16, 3), clip(2, 16, 32, 19)], 100).unwrap();
    let events = |s: ArrangementData| {
        s.notes
            .into_iter()
            .map(|n| (n.tick, n.length, n.key))
            .collect::<Vec<_>>()
    };
    assert_eq!(events(whole), events(split));
}
#[test]
fn overlapping_instances_and_stable_ties_keep_identity() {
    let mut a = note(0, 8);
    a.velocity = 0.2;
    let mut b = a;
    b.velocity = 0.9;
    let mut slide = a;
    slide.slide = true;
    let p = pattern(vec![slide, a, b]);
    let s = compile_pattern_clips(&p, &[clip(9, 0, 16, 0), clip(2, 0, 16, 0)], 20).unwrap();
    assert_eq!(
        s.notes
            .iter()
            .map(|n| (n.clip_id, n.slide, n.velocity))
            .collect::<Vec<_>>(),
        vec![
            (2, false, 0.2),
            (2, false, 0.9),
            (2, true, 0.2),
            (9, false, 0.2),
            (9, false, 0.9),
            (9, true, 0.2)
        ]
    );
}
#[test]
fn muted_and_empty_preserve_extent_and_still_validate() {
    assert_eq!(
        compile_pattern_clips(&[], &[], 0).unwrap(),
        ArrangementData {
            length: 0,
            notes: vec![]
        }
    );
    let mut c = clip(1, 20, 10, 0);
    c.muted = true;
    assert_eq!(
        compile_pattern_clips(&pattern(vec![note(0, 1)]), &[c], 0)
            .unwrap()
            .length,
        30
    );
    assert!(
        compile_pattern_clips(&pattern(vec![note(0, 1)]), &[c], 0)
            .unwrap()
            .notes
            .is_empty()
    );
    c.pattern = 99;
    assert!(matches!(
        compile_pattern_clips(&[], &[c], 0),
        Err(ArrangementError::UnknownPattern(99))
    ));
}
#[test]
fn refuses_invalid_data_even_unused_or_muted() {
    let c = clip(1, 0, 16, 0);
    assert_eq!(
        compile_pattern_clips(&pattern(vec![]), &[c, c], 10),
        Err(ArrangementError::DuplicateClip(1))
    );
    let mut p = pattern(vec![note(0, 1)]);
    assert_eq!(
        compile_pattern_clips(&[p[0].clone(), p[0].clone()], &[], 0),
        Err(ArrangementError::DuplicatePattern(7))
    );
    for velocity in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        p[0].1.notes[0].velocity = velocity;
        assert_eq!(
            compile_pattern_clips(&p, &[], 0),
            Err(ArrangementError::InvalidNote {
                pattern: 7,
                index: 0
            })
        );
    }
    for n in [
        note(16, 1),
        note(0, 0),
        note(0, u32::MAX),
        SeqNote {
            key: 128,
            ..note(0, 1)
        },
    ] {
        assert_eq!(
            compile_pattern_clips(&pattern(vec![n]), &[], 0),
            Err(ArrangementError::InvalidNote {
                pattern: 7,
                index: 0
            })
        );
    }
    for length in [0, 256 * STEP_TICKS + 1] {
        assert_eq!(
            compile_pattern_clips(
                &[(
                    7,
                    PatternData {
                        length,
                        notes: vec![]
                    }
                )],
                &[],
                0
            ),
            Err(ArrangementError::InvalidPattern(7))
        );
    }
}
#[test]
fn tick_clip_and_expansion_limits_are_atomic() {
    let p = pattern(vec![note(0, 1)]);
    for c in [
        clip(1, u64::MAX, 2, 0),
        clip(1, MAX_SONG_TICKS, 1, 0),
        clip(1, 0, 0, 0),
    ] {
        assert_eq!(
            compile_pattern_clips(&p, &[c], 10),
            Err(ArrangementError::InvalidClip(1))
        );
    }
    let clips: Vec<_> = (0..100_001).map(|id| clip(id, 0, 1, 1)).collect();
    assert_eq!(
        compile_pattern_clips(&p, &clips, 0),
        Err(ArrangementError::TooManyClips)
    );
    assert_eq!(
        compile_pattern_clips(&p, &clips[..100_000], 0).unwrap(),
        ArrangementData {
            length: 1,
            notes: vec![]
        }
    );
    assert_eq!(
        compile_pattern_clips(&p, &[clip(1, 0, 48, 0)], 2),
        Err(ArrangementError::TooManyNotes { limit: 2 })
    );
    assert_eq!(
        compile_pattern_clips(&p, &[clip(1, 0, 48, 0)], 3)
            .unwrap()
            .notes
            .len(),
        3
    );
    assert_eq!(
        compile_pattern_clips(&p, &[clip(1, 0, MAX_SONG_TICKS, 0)], usize::MAX),
        Err(ArrangementError::TooManyNotes {
            limit: MAX_SONG_NOTES
        })
    );
    assert_eq!(
        compile_pattern_clips(&pattern(vec![]), &[clip(1, 0, MAX_SONG_TICKS, u32::MAX)], 0)
            .unwrap()
            .length,
        MAX_SONG_TICKS
    );
}

#[test]
fn exact_million_note_budget_is_accepted() {
    let p = pattern(vec![note(0, 1)]);
    let song = compile_pattern_clips(&p, &[clip(1, 0, 16_000_000, 0)], usize::MAX).unwrap();
    assert_eq!(song.notes.len(), 1_000_000);
    assert_eq!(song.notes.last().unwrap().tick, 15_999_984);
    assert_eq!(
        compile_pattern_clips(&p, &[clip(1, 0, 16_000_001, 0)], usize::MAX),
        Err(ArrangementError::TooManyNotes { limit: 1_000_000 })
    );
}

#[test]
fn full_sort_contract_conflicts_at_every_level() {
    let make = |channel, key, slide, velocity| SeqNote {
        channel: ChannelId(channel),
        key,
        slide,
        velocity,
        ..note(0, 1)
    };
    // Deliberately opposite key/channel order, with a slide's key below a plain key.
    let p = pattern(vec![
        make(2, 10, false, 0.1),
        make(1, 20, true, 0.2),
        make(1, 80, false, 0.3),
        make(1, 40, false, 0.4),
        make(1, 40, false, 0.5),
    ]);
    let s = compile_pattern_clips(&p, &[clip(9, 0, 17, 0), clip(2, 1, 16, 0)], 20).unwrap();
    let signatures: Vec<_> = s
        .notes
        .iter()
        .map(|n| (n.tick, n.clip_id, n.channel.0, n.slide, n.key, n.velocity))
        .collect();
    let group = |tick, id| {
        vec![
            (tick, id, 1, false, 40, 0.4),
            (tick, id, 1, false, 40, 0.5),
            (tick, id, 1, false, 80, 0.3),
            (tick, id, 1, true, 20, 0.2),
            (tick, id, 2, false, 10, 0.1),
        ]
    };
    let mut expected = group(0, 9);
    expected.extend(group(1, 2));
    expected.extend(group(16, 9));
    assert_eq!(signatures, expected);
    // Same tick, conflicting clip IDs: clip ID precedes all source-note keys.
    let same = compile_pattern_clips(&p, &[clip(9, 0, 1, 0), clip(2, 0, 1, 0)], 20).unwrap();
    assert_eq!(
        same.notes.iter().map(|n| n.clip_id).collect::<Vec<_>>(),
        vec![2, 2, 2, 2, 2, 9, 9, 9, 9, 9]
    );
}

#[test]
fn phase_windows_exact_budget_and_one_lower() {
    let p = pattern(vec![note(0, 2), note(3, 2), note(7, 2), note(15, 2)]);
    // Source [6,26): 7,15,16,19,23. Source [14,19): 15,16 (19 excluded).
    for (offset, length, expected) in [
        (6, 20, vec![1, 9, 10, 13, 17]),
        (38, 20, vec![1, 9, 10, 13, 17]),
        (30, 5, vec![1, 2]),
        (3, 1, vec![0]),
    ] {
        let n = expected.len();
        let s = compile_pattern_clips(&p, &[clip(1, 0, length, offset)], n).unwrap();
        assert_eq!(s.notes.iter().map(|n| n.tick).collect::<Vec<_>>(), expected);
        assert_eq!(
            compile_pattern_clips(&p, &[clip(1, 0, length, offset)], n - 1),
            Err(ArrangementError::TooManyNotes { limit: n - 1 })
        );
    }
}

#[test]
fn inclusive_source_and_note_length_maxima() {
    let max = 256 * STEP_TICKS;
    let p = vec![(
        7,
        PatternData {
            length: max,
            notes: vec![note(0, max)],
        },
    )];
    let s = compile_pattern_clips(&p, &[clip(0, 0, u64::from(max), 0)], 1).unwrap();
    assert_eq!((s.notes[0].tick, s.notes[0].length), (0, u64::from(max)));
    let mut invalid = p.clone();
    invalid[0].1.notes[0].length = max + 1;
    assert_eq!(
        compile_pattern_clips(&invalid, &[], 0),
        Err(ArrangementError::InvalidNote {
            pattern: 7,
            index: 0
        })
    );
}

#[test]
fn sparse_source_many_empty_clips() {
    let p = vec![(
        7,
        PatternData {
            length: 256 * STEP_TICKS,
            notes: vec![note(60_000, 1); 100_000],
        },
    )];
    let clips: Vec<_> = (0..10_000).map(|id| clip(id, 0, 1, 0)).collect();
    assert_eq!(
        compile_pattern_clips(&p, &clips, 0).unwrap(),
        ArrangementData {
            length: 1,
            notes: vec![]
        }
    );
}

#[test]
fn unsorted_source_partial_period_has_exact_events_and_budget() {
    let p = pattern(vec![note(14, 9), note(6, 8), note(0, 3)]);
    for offset in [5, 37] {
        let clips = [clip(1, 10, 22, offset)];
        let song = compile_pattern_clips(&p, &clips, 4).unwrap();
        assert_eq!(song.length, 32);
        assert_eq!(
            song.notes
                .iter()
                .map(|n| (n.tick, n.length))
                .collect::<Vec<_>>(),
            vec![(11, 8), (19, 9), (21, 3), (27, 5)]
        );
        assert_eq!(
            compile_pattern_clips(&p, &clips, 3),
            Err(ArrangementError::TooManyNotes { limit: 3 })
        );
    }
}

#[test]
fn empty_one_tick_source_many_maximum_length_clips() {
    let p = vec![(
        7,
        PatternData {
            length: 1,
            notes: vec![],
        },
    )];
    let clips: Vec<_> = (0..MAX_PATTERN_CLIPS as u32)
        .map(|id| clip(id, 0, MAX_SONG_TICKS, u32::MAX))
        .collect();
    assert_eq!(
        compile_pattern_clips(&p, &clips, 0).unwrap(),
        ArrangementData {
            length: MAX_SONG_TICKS,
            notes: vec![]
        }
    );
}
