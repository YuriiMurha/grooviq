use grooviq_dsp::synth::{Synth, preset};
use grooviq_dsp::{Device, NoteEvent};
use std::hint::black_box;

pub const BLOCK: usize = 64;
pub const COUNTED_BLOCKS: usize = 100;

pub fn note() -> NoteEvent {
    NoteEvent {
        offset: 0,
        key: 48,
        velocity: 0.8,
        on: true,
        slide: false,
        id: 48,
    }
}

pub fn setup(freerunning: bool, active: bool) -> Synth {
    let mut s = Synth::new(48_000.0);
    for (index, value) in preset("Init").unwrap() {
        s.set_param(index, value);
    }
    if freerunning {
        s.set_param(34, 7.0);
        s.set_param(35, 0.8);
        s.set_param(36, 2.0);
    }
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    let events = if active || freerunning {
        vec![note()]
    } else {
        vec![]
    };
    s.process(&mut l, &mut r, None, &events);
    for _ in 1..16 {
        s.process(&mut l, &mut r, None, &[]);
    }
    // Start smoothing during the measured interval as well as advancing the free LFO.
    s.set_param(20, 900.0);
    s.set_param(35, if freerunning { 0.6 } else { 0.0 });
    if freerunning && !active {
        s.choke();
        // Drain the choke fade before counting a genuinely silent device.
        for _ in 0..16 {
            s.process(&mut l, &mut r, None, &[]);
        }
    }
    s
}

pub fn render(mut s: Synth) -> Synth {
    let (mut l, mut r) = ([0.0; BLOCK], [0.0; BLOCK]);
    for _ in 0..COUNTED_BLOCKS {
        s.process(black_box(&mut l), black_box(&mut r), None, &[]);
    }
    // Make all mutable state observable, including silent per-frame progression.
    black_box(s)
}
