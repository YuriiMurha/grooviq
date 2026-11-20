#[path = "../benches/support/synth_workload.rs"]
mod workload;
use grooviq_dsp::{Device, synth::Synth};

fn next_note(s: &mut Synth) -> Vec<u32> {
    let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
    let mut output = Vec::new();
    let events = [workload::note()];
    for block in 0..32 {
        s.process(&mut l, &mut r, None, if block == 0 { &events } else { &[] });
        output.extend(l);
        output.extend(r);
    }
    assert!(
        output.iter().any(|x| x.abs() > 0.001),
        "next note must be audible"
    );
    output.into_iter().map(f32::to_bits).collect()
}

#[test]
fn idle_progress_and_next_note_match_per_frame_reference() {
    for freerunning in [false, true] {
        let mut reference = workload::setup(freerunning, false);
        let before = reference.meter(3);
        let mut measured = workload::render(workload::setup(freerunning, false));
        for _ in 0..workload::COUNTED_BLOCKS * workload::BLOCK {
            let (mut l, mut r) = ([1.0], [1.0]);
            reference.process(&mut l, &mut r, None, &[]);
            assert_eq!((l, r), ([0.0], [0.0]), "idle output must be zero");
        }
        assert_ne!(measured.meter(3), before, "idle LFO must advance");
        assert_eq!(measured.meter(3), reference.meter(3));
        let actual = next_note(&mut measured);
        assert_eq!(
            actual,
            next_note(&mut reference),
            "LFO and smoother continuation must match"
        );
        let mut frozen = workload::setup(freerunning, false);
        if freerunning {
            assert!(
                actual != next_note(&mut frozen),
                "skipping idle progression must change the witness"
            );
        }
    }
}

#[test]
fn active_control_matches_counted_frame_continuation() {
    let mut reference = workload::setup(true, true);
    let before = reference.meter(3).to_bits();
    let mut measured = workload::render(workload::setup(true, true));
    let mut audible = false;
    for _ in 0..workload::COUNTED_BLOCKS * workload::BLOCK {
        let (mut l, mut r) = ([0.0], [0.0]);
        reference.process(&mut l, &mut r, None, &[]);
        audible |= l.iter().chain(&r).any(|sample| sample.abs() > 0.001);
    }
    assert!(audible, "the counted active reference must be audible");
    assert_ne!(
        measured.meter(3).to_bits(),
        before,
        "active LFO must advance"
    );
    assert_eq!(measured.meter(3).to_bits(), reference.meter(3).to_bits());
    assert!(measured.meter(2) > 0.0);
    assert_eq!(measured.meter(2).to_bits(), reference.meter(2).to_bits());
    assert_eq!(next_note(&mut measured), next_note(&mut reference));
}
