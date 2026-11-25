use grooviq_dsp::{
    Device, EFFECTS, InsertError, create, create_insert, db_to_gain, is_stepped,
    max_latency_frames, max_memory_bytes, param_ids, param_spec, preset, presets,
};

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn defaults(id: &str) -> Vec<f32> {
    param_spec(id).unwrap().iter().map(|spec| spec.2).collect()
}

#[test]
fn registry_matches_the_project_effect_list() {
    assert_eq!(
        EFFECTS,
        [
            "algorithmic_reverb",
            "bitcrusher",
            "character_delay",
            "comp",
            "delay",
            "drive",
            "drum_shaper",
            "eq",
            "fattener",
            "limiter",
            "lofi_colour",
            "ott",
            "reverb",
            "saturation",
            "shimmer_reverb",
            "utility",
        ]
    );
    for id in EFFECTS {
        let ids = param_ids(id).unwrap();
        let spec = param_spec(id).unwrap();
        assert_eq!(ids.len(), spec.len(), "{id}");
        for (i, name) in ids.iter().enumerate() {
            assert!(!name.is_empty() && !ids[..i].contains(name), "{id}.{name}");
            assert!(
                name.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                "{id}.{name}"
            );
            assert!(is_stepped(id, i as u32).is_some(), "{id}.{name}");
        }
        assert_eq!(is_stepped(id, spec.len() as u32), None, "{id}");
        assert!(create(id, 48000.).is_some(), "{id}");
        assert!(create_insert(id, 48000., 17, &defaults(id)).is_ok(), "{id}");
    }

    for id in ["synth", "sampler", "unknown"] {
        assert!(param_ids(id).is_none(), "{id}");
        assert!(is_stepped(id, 0).is_none(), "{id}");
        assert!(max_latency_frames(id, 48000.).is_none(), "{id}");
        assert!(presets(id).is_empty(), "{id}");
        assert!(preset(id, "Init").is_none(), "{id}");
        assert!(matches!(
            create_insert(id, 48000., 17, &[]),
            Err(InsertError::UnknownEffect)
        ));
    }
    assert!(param_spec("unknown").is_none());
    assert!(create("unknown", 48000.).is_none());
}

#[test]
fn stepped_controls_match_switches_and_rounded_values() {
    for id in EFFECTS {
        let stepped: Vec<u32> = (0..param_spec(id).unwrap().len() as u32)
            .filter(|&i| is_stepped(id, i).unwrap())
            .collect();
        let expected: &[u32] = match id {
            "character_delay" | "delay" => &[0, 1, 4],
            "comp" => &[7],
            "drive" => &[0, 5],
            "eq" => &[
                0, 1, 5, 6, 7, 11, 12, 13, 17, 18, 19, 23, 24, 25, 29, 30, 31, 35, 36, 37, 41, 42,
                43, 47,
            ],
            "saturation" => &[0],
            "utility" => &[4],
            _ => &[],
        };
        assert_eq!(stepped, expected, "{id}");
    }
}

#[test]
fn every_preset_assigns_valid_indices_at_most_once() {
    for id in EFFECTS {
        let spec = param_spec(id).unwrap();
        let names = presets(id);
        assert!(preset(id, "Unknown preset").is_none(), "{id}");
        for (i, &name) in names.iter().enumerate() {
            assert!(
                !name.is_empty() && !names[..i].contains(&name),
                "{id} {name}"
            );
            let assignments = preset(id, name).unwrap_or_else(|| panic!("{id} {name}"));
            let mut seen = vec![false; spec.len()];
            for (index, value) in assignments {
                let i = index as usize;
                let &(min, max, _) = spec
                    .get(i)
                    .unwrap_or_else(|| panic!("{id} {name} index {i}"));
                assert!(!seen[i], "{id} {name} assigns index {i} twice");
                seen[i] = true;
                assert!(
                    value.is_finite() && (min..=max).contains(&value),
                    "{id} {name}[{i}] = {value}"
                );
            }
        }
    }
}

// Nondefault settings, with modulation/noise pathways active in the seeded effects.
fn render_params(id: &str) -> Vec<f32> {
    match id {
        "algorithmic_reverb" => vec![
            1., 0.5, 0., 1.2, 0.1, 0.7, 0.8, 100., 12000., 1.2, 0.8, -3., 0.2,
        ],
        "bitcrusher" => vec![6.5, 4.5, 0.5, 0.35, 0.8, 0.9],
        "character_delay" => vec![
            0., 4., 7., 0.5, 0., 100., 9000., 0.6, 140., 2., 0.9, 0.2, 0.8, 2., -3.,
        ],
        "delay" => vec![0., 4., 7., 0.5, 0., 100., 9000., 0.6, 140.],
        "lofi_colour" => vec![0.9, 0.8, 0.8, 0.3, 0.4, 0.2, 0.2, 0.5],
        "shimmer_reverb" => vec![0.6, 2., 1.5, 0.7, 0.5, 0.8],
        _ => param_spec(id)
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, &(min, max, _))| min + (max - min) * if i % 2 == 0 { 0.65 } else { 0.35 })
            .collect(),
    }
}

fn render(id: &str, sr: f32, seed: u64, params: &[f32], block: usize) -> Vec<[u32; 2]> {
    let mut device = create_insert(id, sr, seed, params).unwrap();
    render_device(id, sr, device.as_mut(), block)
}

fn render_device(id: &str, sr: f32, device: &mut dyn Device, block: usize) -> Vec<[u32; 2]> {
    // Short, nonrepeating stereo excitation followed by room for delay and reverb tails.
    let (mut left, mut right): (Vec<_>, Vec<_>) = (0..8192)
        .map(|i| {
            if i < 4096 {
                (
                    ((i * 17 % 257) as f32 - 128.) / 192.,
                    ((i * 37 % 251) as f32 - 125.) / 256.,
                )
            } else {
                (0., 0.)
            }
        })
        .unzip();
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        assert_no_alloc::assert_no_alloc(|| device.process(l, r, None, &[]));
    }
    left.into_iter()
        .zip(right)
        .map(|(l, r)| {
            assert!(l.is_finite() && r.is_finite(), "{id} at {sr} Hz");
            [l.to_bits(), r.to_bits()]
        })
        .collect()
}

#[test]
fn insert_render_is_bit_identical_at_all_block_sizes() {
    for sr in [44100., 48000.] {
        for id in EFFECTS {
            let params = render_params(id);
            let expected = render(id, sr, 0x1234_5678_9abc_def0, &params, 1);
            for block in [7, 64, 128] {
                let actual = render(id, sr, 0x1234_5678_9abc_def0, &params, block);
                assert!(actual == expected, "{id} at {sr} Hz, block {block}");
            }
        }
    }
}

#[test]
fn seeded_effects_respond_to_the_project_seed_and_obey_the_xor_fold() {
    let seed = 0x0123_4567_89ab_cdef;
    for sr in [44100., 48000.] {
        for id in [
            "algorithmic_reverb",
            "bitcrusher",
            "character_delay",
            "lofi_colour",
            "shimmer_reverb",
        ] {
            let params = render_params(id);
            let base = render(id, sr, seed, &params, 64);
            let folded = matches!(id, "bitcrusher" | "lofi_colour");
            // Algorithmic Reverb currently maps its phase from the low 16 bits (#552).
            let changed_bit = if id == "algorithmic_reverb" { 12 } else { 50 };
            assert!(
                base != render(id, sr, seed ^ (1 << changed_bit), &params, 64),
                "{id} at {sr} Hz"
            );
            if folded {
                // XORing the same bit into each half keeps their XOR constant.
                let collision = render(id, sr, seed ^ 0x4000_0000_4000_0000, &params, 64);
                assert!(base == collision, "{id} at {sr} Hz");
                let mut expected: Box<dyn Device> = if id == "bitcrusher" {
                    Box::new(grooviq_dsp::bitcrusher::Bitcrusher::with_seed(
                        sr,
                        0x8888_8888,
                    ))
                } else {
                    Box::new(grooviq_dsp::lofi_colour::LofiColour::with_seed(
                        sr,
                        0x8888_8888,
                    ))
                };
                for (i, &value) in params.iter().enumerate() {
                    expected.set_param(i as u32, value);
                }
                assert!(
                    base == render_device(id, sr, expected.as_mut(), 64),
                    "{id} at {sr} Hz"
                );
            }
        }
    }
}

#[test]
fn utility_starts_at_the_requested_gain_and_polarity() {
    let mut device = create_insert("utility", 48000., 0, &[-18., 0., 1., 0., 1.]).unwrap();
    let mut left = [0.5; 128];
    let mut right = [-0.25; 128];
    device.process(&mut left, &mut right, None, &[]);
    let gain = db_to_gain(-18.);
    for (l, r) in left.into_iter().zip(right) {
        assert!((l + 0.5 * gain).abs() < 1e-7 && (r - 0.25 * gain).abs() < 1e-7);
    }
}

#[test]
fn declared_latency_bounds_the_allocation_free_query() {
    for sr in [22050., 44100., 48000., 96000., 192000.] {
        for id in EFFECTS {
            let spec = param_spec(id).unwrap();
            let bound = max_latency_frames(id, sr).unwrap();
            let mut device = create_insert(id, sr, 17, &defaults(id)).unwrap();
            let assert_bound = |device: &dyn Device| {
                let latency = assert_no_alloc::assert_no_alloc(|| {
                    std::hint::black_box(device).latency_frames()
                });
                assert!(latency <= bound, "{id} at {sr} Hz: {latency} > {bound}");
            };
            assert_bound(&*device);
            for (i, &(min, max, default)) in spec.iter().enumerate() {
                let steps = if is_stepped(id, i as u32).unwrap() {
                    (max - min) as u32
                } else {
                    4
                };
                for step in 0..=steps {
                    let value = min + (max - min) * (step as f32 / steps as f32);
                    assert_no_alloc::assert_no_alloc(|| device.set_param(i as u32, value));
                    assert_bound(&*device);
                }
                device.set_param(i as u32, default);
            }
        }
    }
}

#[test]
fn invalid_inserts_return_typed_errors() {
    assert!(matches!(
        create_insert("missing", 48000., 0, &[]),
        Err(InsertError::UnknownEffect)
    ));
    let mut params = defaults("utility");
    for values in [&params[..4], &[0.; 6][..]] {
        assert!(matches!(
            create_insert("utility", 48000., 0, values),
            Err(InsertError::ParameterCount { expected: 5, actual }) if actual == values.len()
        ));
    }
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        params[4] = value;
        assert!(matches!(
            create_insert("utility", 48000., 0, &params),
            Err(InsertError::NonFiniteParameter { index: 4 })
        ));
    }
    for value in [-f32::EPSILON, 1. + f32::EPSILON] {
        params[4] = value;
        assert!(matches!(
            create_insert("utility", 48000., 0, &params),
            Err(InsertError::ParameterOutOfRange { index: 4 })
        ));
    }
    for value in [0., 0.4, 1.] {
        params[4] = value;
        assert!(create_insert("utility", 48000., 0, &params).is_ok());
    }
}

/// 128 blocks of 128 frames of deterministic noise, as bits: long enough for 15 ms smoothers,
/// 50 ms tap fades and Shimmer's slower pitch glide (which first diverges near frame 3,730).
fn first_blocks(device: &mut dyn Device) -> Vec<u32> {
    let mut state = 0x1234_5678u32;
    let mut out = Vec::new();
    for _ in 0..128 {
        let (mut left, mut right) = ([0f32; 128], [0f32; 128]);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = (state >> 8) as f32 / 16_777_216.0 * 2.0 - 1.0;
            (*l, *r) = (0.5 * v, -0.3 * v);
        }
        device.process(&mut left, &mut right, None, &[]);
        out.extend(left.iter().chain(&right).map(|x| x.to_bits()));
    }
    out
}

/// `create_insert` must not glide from the defaults: its first blocks equal the same device
/// after `reset()`, which snaps every smoother, at the minimum, maximum and middle vectors
/// (plus a short wet tap for the delays).
#[test]
fn every_effect_starts_at_the_requested_values() {
    for id in EFFECTS {
        let spec = param_spec(id).unwrap();
        let mut vectors = vec![
            spec.iter().map(|s| s.0).collect::<Vec<f32>>(),
            spec.iter().map(|s| s.1).collect(),
            spec.iter().map(|s| 0.5 * (s.0 + s.1)).collect(),
        ];
        if matches!(id, "delay" | "character_delay") {
            // A short free-time tap, fully wet: a tap that fades in from the default time
            // shows within the first milliseconds. Indices: 0 sync, 2 time ms, 7 mix.
            let mut tap = defaults(id);
            (tap[0], tap[2], tap[7]) = (0., 1., 1.);
            vectors.push(tap);
        }
        for params in &vectors {
            let mut fresh = create_insert(id, 48000., 9, params).unwrap();
            let mut snapped = create_insert(id, 48000., 9, params).unwrap();
            snapped.reset();
            assert!(
                first_blocks(fresh.as_mut()) == first_blocks(snapped.as_mut()),
                "{id} glides from its defaults at {params:?}"
            );
        }
    }
}

/// Construction, latency and memory admission refuse exactly the same rates.
#[test]
fn rates_outside_the_native_range_are_refused_everywhere() {
    for id in EFFECTS {
        let params = defaults(id);
        for sr in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.,
            -48000.,
            7999.,
            384_001.,
        ] {
            assert_eq!(
                create_insert(id, sr, 1, &params).err(),
                Some(InsertError::InvalidSampleRate),
                "{id} at {sr}"
            );
            assert_eq!(max_latency_frames(id, sr), None, "{id} at {sr}");
            assert_eq!(
                max_memory_bytes(id, sr),
                Err(InsertError::InvalidSampleRate),
                "{id} at {sr}"
            );
        }
        for sr in [
            8000., 16000., 22049., 22050., 44100., 192000., 192001., 384000.,
        ] {
            let admitted = max_memory_bytes(id, sr).is_ok();
            assert_eq!(
                create_insert(id, sr, 1, &params).is_ok(),
                admitted,
                "{id} at {sr}"
            );
            assert_eq!(
                max_latency_frames(id, sr).is_some(),
                admitted,
                "{id} at {sr}"
            );
        }
    }
}
