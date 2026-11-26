use super::*;

fn same(a: &Synth, b: &Synth) {
    for i in 0..6 {
        assert_eq!(a.meter(i).to_bits(), b.meter(i).to_bits(), "meter {i}");
    }
    assert_eq!(a.scope.map(f32::to_bits), b.scope.map(f32::to_bits));
    assert_eq!(a.scope_pos, b.scope_pos);
    assert_eq!(a.lfo.phase.to_bits(), b.lfo.phase.to_bits());
    assert_eq!(a.lfo.out.to_bits(), b.lfo.out.to_bits());
    assert_eq!(a.lfo.held.to_bits(), b.lfo.held.to_bits());
    assert_eq!(a.lfo.rng.0, b.lfo.rng.0);
    let smooth = |s: &Synth| {
        [
            s.s_shape1,
            s.s_shape2,
            s.s_lvl1,
            s.s_lvl2,
            s.s_sub,
            s.s_noise,
            s.s_res,
            s.s_drive,
            s.s_lfo_amt,
            s.s_master,
            s.s_env,
            s.s_kt,
        ]
    };
    for (x, y) in smooth(a).iter().zip(smooth(b)) {
        assert_eq!(x.cur.to_bits(), y.cur.to_bits());
    }
    assert_eq!(a.s_cut.cur.to_bits(), b.s_cut.cur.to_bits());
    let ramps = |s: &Synth| {
        s.route
            .into_iter()
            .chain(s.r_off)
            .chain(s.uxf)
            .chain(s.fw)
            .collect::<Vec<_>>()
    };
    for (x, y) in ramps(a).iter().zip(ramps(b)) {
        assert_eq!(x.cur.to_bits(), y.cur.to_bits());
        assert_eq!(x.left, y.left);
    }
    let caches = |s: &Synth| {
        [
            s.res_cache.0,
            s.res_cache.1,
            s.res_cache.2,
            s.res_cache.3,
            s.drive_cache.0,
            s.drive_cache.1,
            s.drive_cache.2,
        ]
        .map(f32::to_bits)
    };
    assert_eq!(caches(a), caches(b));
}

#[test]
fn advance_silent_matches_event_free_process_across_chunks() {
    let (mut skip, mut full) = (Synth::new(48_000.0), Synth::new(48_000.0));
    for synth in [&mut skip, &mut full] {
        // Reproduce a recent release: no voices left but old scope samples and decaying peaks.
        synth.scope.fill(0.25);
        synth.scope_pos = SCOPE_LEN - 6;
        synth.level_l.value = 0.73;
        synth.level_r.value = 0.61;
        synth.set_param(LFO_SHAPE as u32, 4.0);
        synth.set_param(LFO_RATE as u32, 20.0);
        synth.lfo.phase = 0.9999; // First nonempty span crosses a sample-and-hold boundary.
        // Idle setters snap, so seed moving state after them, as if a voice just stopped.
        synth.s_master.set(0.4);
        synth.s_res.set(0.5);
        synth.s_cut.set(10.0);
        synth.s_drive.set(0.4);
        synth.route[0].set(0.7);
        synth.fw[0].set(0.4);
        assert!(synth.is_silent());
    }

    for frames in [0, 13, MAX_BLOCK - 1, MAX_BLOCK, 3 * MAX_BLOCK + 7] {
        let (mut l, mut r) = (vec![1.0; frames], vec![1.0; frames]);
        skip.advance_silent(frames);
        full.process(&mut l, &mut r, None, &[]);
        assert!(!skip.fresh && !full.fresh);
        assert!(l.iter().chain(&r).all(|x| x.to_bits() == 0));
        same(&skip, &full);
    }
}

#[test]
fn idle_matches_original_preparation_and_next_note() {
    for mode in 0..3 {
        for shape in 0..5 {
            let mut a = Synth::new(48_000.0);
            let mut b = Synth::new(48_000.0);
            b.force_full_preparation = true;
            let mut seed = 719u32;
            for step in 0..440 {
                for s in [&mut a, &mut b] {
                    if step == 0 {
                        s.set_param(VOICE_MODE as u32, mode as f32);
                        s.set_param(LFO_SHAPE as u32, shape as f32);
                        s.set_param(LFO_RATE as u32, 20.0);
                        s.set_param(LFO_AMOUNT as u32, 0.8);
                        s.set_param(AMP_R as u32, 1.0);
                    }
                    if step % 23 == 0 {
                        s.set_param(OSC1_FINE as u32, step as f32 % 90.0);
                        // Idle public setters snap, so move every recurrence after the setter.
                        for smoother in [
                            &mut s.s_shape1,
                            &mut s.s_shape2,
                            &mut s.s_lvl1,
                            &mut s.s_lvl2,
                            &mut s.s_sub,
                            &mut s.s_noise,
                            &mut s.s_res,
                            &mut s.s_drive,
                            &mut s.s_lfo_amt,
                            &mut s.s_master,
                            &mut s.s_env,
                            &mut s.s_kt,
                        ] {
                            smoother.set(smoother.cur * 0.37 + 0.01);
                        }
                        s.s_cut.set(8.0 + (step % 5) as f32);
                        for ramp in s
                            .route
                            .iter_mut()
                            .chain(&mut s.r_off)
                            .chain(&mut s.uxf)
                            .chain(&mut s.fw)
                        {
                            ramp.set(ramp.cur * 0.27 + 0.13);
                        }
                    }
                    if step == 240 {
                        s.choke();
                    }
                    if step == 320 {
                        s.reset();
                    }
                }
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = if step % 4 == 0 {
                    193
                } else {
                    1 + seed as usize % 128
                };
                let mut ev = Vec::new();
                if matches!(step, 60 | 61 | 62 | 180 | 241 | 360) {
                    ev.push(NoteEvent {
                        offset: [0, 1, 63][step % 3].min(n - 1) as u32,
                        key: 60 + (step % 3) as u8,
                        velocity: 0.8,
                        on: true,
                        slide: false,
                        id: step as u64,
                    });
                }
                if step == 100 || step == 200 {
                    for id in [60, 61, 62, 180] {
                        ev.push(NoteEvent {
                            offset: 0,
                            key: 60 + (id % 3) as u8,
                            velocity: 0.0,
                            on: false,
                            slide: false,
                            id,
                        });
                    }
                }
                let (mut al, mut ar, mut bl, mut br) =
                    (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
                a.process(&mut al, &mut ar, None, &ev);
                b.process(&mut bl, &mut br, None, &ev);
                assert_eq!(
                    al.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    bl.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    "step {step}"
                );
                assert_eq!(
                    ar.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                    br.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                );
                same(&a, &b);
            }
        }
    }
}

#[test]
fn idle_recurrences_are_audible_on_the_next_note() {
    for offset in [0, 1, 63, 64, 127] {
        let mut a = Synth::new(48_000.0);
        let mut b = Synth::new(48_000.0);
        b.force_full_preparation = true;
        for s in [&mut a, &mut b] {
            s.set_param(LFO_AMOUNT as u32, 0.9);
            s.set_param(LFO_RATE as u32, 17.0);
            s.set_param(LFO_TARGET as u32, 1.0);
            s.s_lvl1.set(0.1);
        }
        let (mut l, mut r) = ([0.0; 193], [0.0; 193]);
        a.process(&mut l, &mut r, None, &[]);
        b.process(&mut l, &mut r, None, &[]);
        let note = NoteEvent {
            offset,
            key: 60,
            velocity: 1.0,
            on: true,
            slide: false,
            id: 1,
        };
        let (mut al, mut ar, mut bl, mut br) = ([0.0; 193], [0.0; 193], [0.0; 193], [0.0; 193]);
        a.process(&mut al, &mut ar, None, &[note]);
        b.process(&mut bl, &mut br, None, &[note]);
        assert!(bl.iter().any(|x| x.abs() > 0.001));
        assert_eq!(al.map(f32::to_bits), bl.map(f32::to_bits), "onset {offset}");
        assert_eq!(ar.map(f32::to_bits), br.map(f32::to_bits));
    }
}

fn compare_audio_and_state(a: &mut Synth, b: &mut Synth, n: usize, events: &[NoteEvent]) -> bool {
    let (mut al, mut ar, mut bl, mut br) = (vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    a.process(&mut al, &mut ar, None, events);
    b.process(&mut bl, &mut br, None, events);
    assert_eq!(
        al.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        bl.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
    assert_eq!(
        ar.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        br.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
    same(a, b);
    bl.iter().chain(&br).any(|sample| sample.abs() > 0.001)
}

#[test]
fn every_idle_recurrence_and_pending_boundary_matches_original() {
    for sr in [8000.0, 44100.0, 48000.0, 96000.0, 384000.0] {
        for shape in 0..5 {
            let (mut a, mut b) = (Synth::new(sr), Synth::new(sr));
            b.force_full_preparation = true;
            for s in [&mut a, &mut b] {
                s.set_param(LFO_SHAPE as u32, shape as f32);
                s.set_param(LFO_RATE as u32, 20.0);
                // Deliberately exercise every recurrence; public idle setters snap them.
                for sm in [
                    &mut s.s_shape1,
                    &mut s.s_shape2,
                    &mut s.s_lvl1,
                    &mut s.s_lvl2,
                    &mut s.s_sub,
                    &mut s.s_noise,
                    &mut s.s_res,
                    &mut s.s_drive,
                    &mut s.s_lfo_amt,
                    &mut s.s_master,
                    &mut s.s_env,
                    &mut s.s_kt,
                ] {
                    sm.set(sm.cur * 0.37 + 0.01);
                }
                s.s_cut.set(9.0);
                for r in s
                    .route
                    .iter_mut()
                    .chain(&mut s.r_off)
                    .chain(&mut s.uxf)
                    .chain(&mut s.fw)
                {
                    r.set(r.cur * 0.27 + 0.13);
                }
            }
            for n in [1, 63, 64, 127, 128, 129, 257, 4096] {
                compare_audio_and_state(&mut a, &mut b, n, &[]);
            }
            // Changes to shape/route and signed-zero public values while idle.
            for (parameter, spec) in SPEC.iter().enumerate() {
                let value = if spec.0 <= 0.0 && spec.1 >= 0.0 {
                    -0.0
                } else {
                    spec.0
                };
                a.set_param(parameter as u32, value);
                b.set_param(parameter as u32, value);
            }
            compare_audio_and_state(&mut a, &mut b, 129, &[]);
            for offset in [0, 1, 63, 64, 127, 128, 129, 256, 257, 999] {
                // Reset preserves the zeroed parameters above; each onset needs an audible patch.
                let (mut a, mut b) = (Synth::new(sr), Synth::new(sr));
                b.force_full_preparation = true;
                for synth in [&mut a, &mut b] {
                    synth.set_param(LFO_SHAPE as u32, shape as f32);
                    synth.set_param(LFO_RATE as u32, 17.0);
                    synth.set_param(LFO_AMOUNT as u32, 0.8);
                    synth.set_param(LFO_TARGET as u32, 1.0);
                }
                let onset_audible = compare_audio_and_state(
                    &mut a,
                    &mut b,
                    257,
                    &[NoteEvent {
                        offset,
                        key: 60,
                        velocity: 0.9,
                        on: true,
                        slide: true,
                        id: 3,
                    }],
                );
                let next_block_audible = compare_audio_and_state(&mut a, &mut b, 257, &[]);
                assert!(
                    onset_audible || next_block_audible,
                    "silent oracle at {sr}/{shape}/{offset}"
                );
            }
        }
    }
}

#[test]
fn choke_preserves_audio_with_only_fade_slots_active() {
    let (mut a, mut b) = (Synth::new(48000.0), Synth::new(48000.0));
    b.force_full_preparation = true;
    let note = NoteEvent {
        offset: 0,
        key: 60,
        velocity: 1.0,
        on: true,
        slide: false,
        id: 1,
    };
    compare_audio_and_state(&mut a, &mut b, 4096, &[note]);
    for s in [&mut a, &mut b] {
        s.set_param(MASTER as u32, -24.0);
        s.set_param(LFO_AMOUNT as u32, 0.9);
        s.choke();
        assert!(s.voices[..NUM_VOICES].iter().all(|v| !v.active));
        assert!(s.voices[NUM_VOICES..].iter().any(|v| v.active));
    }
    for n in [1, 63, 64, 127, 128, 129, 257] {
        compare_audio_and_state(&mut a, &mut b, n, &[]);
    }
    assert!(a.voices.iter().all(|v| !v.active));
    compare_audio_and_state(&mut a, &mut b, 257, &[note]);
}

#[test]
fn master_smoothing_after_choke_matches_the_next_note() {
    let (mut a, mut b) = (Synth::new(48000.0), Synth::new(48000.0));
    b.force_full_preparation = true;
    let note = NoteEvent {
        offset: 0,
        key: 60,
        velocity: 1.0,
        on: true,
        slide: false,
        id: 1,
    };
    compare_audio_and_state(&mut a, &mut b, 4096, &[note]);
    for s in [&mut a, &mut b] {
        s.set_param(MASTER as u32, -24.0);
        s.choke();
    }
    let (mut l, mut r) = ([0.0; 64], [0.0; 64]);
    for _ in 0..5 {
        a.process(&mut l, &mut r, None, &[]);
        b.process(&mut l, &mut r, None, &[]);
    }
    assert!(a.voices.iter().all(|v| !v.active));
    let (mut al, mut ar, mut bl, mut br) = ([0.0; 128], [0.0; 128], [0.0; 128], [0.0; 128]);
    a.process(&mut al, &mut ar, None, &[note]);
    b.process(&mut bl, &mut br, None, &[note]);
    assert!(bl.iter().any(|x| x.abs() > 0.001));
    assert_eq!(al.map(f32::to_bits), bl.map(f32::to_bits));
    assert_eq!(ar.map(f32::to_bits), br.map(f32::to_bits));
}
