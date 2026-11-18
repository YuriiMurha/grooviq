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
                        // Also move ongoing recurrences directly: public idle parameter writes snap.
                        s.s_cut.set(8.0 + (step % 5) as f32);
                        s.s_lfo_amt.set(0.2 + (step % 3) as f32 * 0.3);
                        s.route[1].set((step % 2) as f32);
                        s.s_res.set((step % 7) as f32 / 7.0);
                        s.s_drive.set((step % 9) as f32 / 9.0);
                        s.set_param(OSC1_FINE as u32, step as f32 % 90.0);
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
