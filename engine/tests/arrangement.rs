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
    c.pattern = 99;
    assert!(matches!(
        compile_pattern_clips(&[], &[c], 0),
        Err(ArrangementError::UnknownPattern(99))
    ));
}
#[test]
fn refuses_invalid_data_even_unused_or_muted() {
    let c = clip(1, 0, 16, 0);
    assert!(compile_pattern_clips(&pattern(vec![]), &[c, c], 10).is_err());
    let mut p = pattern(vec![note(0, 1)]);
    assert!(compile_pattern_clips(&[p[0].clone(), p[0].clone()], &[], 0).is_err());
    for velocity in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        p[0].1.notes[0].velocity = velocity;
        assert!(compile_pattern_clips(&p, &[], 0).is_err());
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
        assert!(compile_pattern_clips(&pattern(vec![n]), &[], 0).is_err());
    }
    for length in [0, 256 * STEP_TICKS + 1] {
        assert!(
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
            )
            .is_err()
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
        assert!(compile_pattern_clips(&p, &[c], 10).is_err());
    }
    assert!(compile_pattern_clips(&p, &vec![clip(1, 0, 1, 0); 100_001], 0).is_err());
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
    assert!(compile_pattern_clips(&p, &[clip(1, 0, MAX_SONG_TICKS, 0)], usize::MAX).is_err());
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
