use grooviq_golden::{compare, compare_exact, render, scenes, synth_preset_scene};

const BLOCK_SIZES: [usize; 2] = [64, 512];

#[test]
fn each_scene_matches_its_reference() {
    for scene in scenes() {
        let actual = render(&scene, 512);
        let path = format!("{}/refs/{}.wav", env!("CARGO_MANIFEST_DIR"), scene.name);
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            grooviq_golden::write_wav(&path, &actual);
            println!("golden file updated: explain why in the commit message");
            continue;
        }
        let reference = grooviq_golden::read_wav(&path);
        if let Err(e) = compare_exact(&actual, &reference) {
            panic!("scene {}: {e}", scene.name);
        }
    }
}

#[test]
fn each_scene_is_identical_at_block_sizes_64_and_512() {
    for scene in scenes() {
        let a = render(&scene, BLOCK_SIZES[0]);
        let b = render(&scene, BLOCK_SIZES[1]);
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()) && a.len() == b.len(),
            "scene {} differs between callback sizes",
            scene.name
        );
    }
}

/// "Init" and every factory preset, straight from the synth: the audio tests below do not go
/// through the library's own `preset_names()`, so a helper that drops a preset cannot drop coverage.
fn every_preset() -> impl Iterator<Item = &'static str> {
    std::iter::once("Init").chain(grooviq_dsp::synth::PRESETS)
}

#[test]
fn the_preset_list_covers_init_and_every_factory_preset() {
    let expected: Vec<&str> = every_preset().collect();
    assert_eq!(expected.len(), 1 + grooviq_dsp::synth::PRESETS.len());
    assert_eq!(grooviq_golden::synth_presets::preset_names(), expected);
}

#[test]
fn each_synth_preset_matches_its_reference_exactly() {
    use grooviq_golden::synth_presets::stem;
    for name in every_preset() {
        let scene = synth_preset_scene(name);
        let actual = render(&scene, 512);
        assert!(
            actual.iter().any(|v| v.abs() > 1e-3) && actual.iter().all(|v| v.is_finite()),
            "preset {name} renders silence or NaN"
        );
        let path = format!("{}/refs/{}.wav", env!("CARGO_MANIFEST_DIR"), stem(name));
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            grooviq_golden::write_wav(&path, &actual);
            println!("golden file updated: explain why in the commit message");
            continue;
        }
        let reference = grooviq_golden::read_wav(&path);
        if let Err(e) = compare_exact(&actual, &reference) {
            panic!("synth preset {name}: {e}");
        }
    }
}

#[test]
fn no_two_scenes_share_a_reference_file() {
    use grooviq_golden::synth_presets::stem;
    let mut stems: Vec<String> = scenes().iter().map(|s| s.name.to_string()).collect();
    stems.extend(every_preset().map(stem));
    let mut unique = stems.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        stems.len(),
        "two scenes write one file: {stems:?}"
    );
}

#[test]
fn the_synth_presets_are_identical_at_block_sizes_64_and_512() {
    for name in every_preset() {
        let scene = synth_preset_scene(name);
        let (a, b) = (render(&scene, 64), render(&scene, 512));
        assert!(
            compare_exact(&a, &b).is_ok(),
            "preset {name} differs between callback sizes"
        );
    }
}

#[test]
fn compare_exact_reports_the_first_differing_frame() {
    let good = vec![0.5f32, -0.5, 0.25, 0.125, 0.0, 0.0];
    assert!(compare_exact(&good, &good).is_ok());
    // A 1-ulp change is far inside `compare`'s tolerance but must fail here.
    let mut ulp = good.clone();
    ulp[3] = f32::from_bits(ulp[3].to_bits() + 1);
    assert!(compare(&ulp, &good).is_ok());
    let e = compare_exact(&ulp, &good).unwrap_err();
    assert!(e.contains("frame 1, right"), "{e}");
    // -0.0 is a different bit pattern; a NaN is not in the reference; lengths must agree.
    let mut neg_zero = good.clone();
    neg_zero[4] = -0.0;
    assert!(compare_exact(&neg_zero, &good).is_err());
    let mut nan = good.clone();
    nan[0] = f32::NAN;
    assert!(compare_exact(&nan, &good).is_err());
    assert!(compare_exact(&good[..4], &good).is_err());
}

#[test]
fn compare_exact_compares_nan_payloads_bit_for_bit() {
    let nan = |bits: u32| {
        let mut v = vec![0.5f32, -0.5, 0.25, 0.125];
        v[2] = f32::from_bits(bits);
        v
    };
    let quiet = nan(0x7fc0_0000);
    // The same NaN bits are equal (a NaN never equals itself under `==`, bits do).
    assert!(compare_exact(&quiet, &nan(0x7fc0_0000)).is_ok());
    // A different payload or sign bit is a different render.
    assert!(compare_exact(&quiet, &nan(0x7fc0_0001)).is_err());
    assert!(compare_exact(&nan(0x7fc0_0001), &nan(0x7fc0_0002)).is_err());
    assert!(compare_exact(&quiet, &nan(0xffc0_0000)).is_err());
    // A NaN against a finite sample, both ways.
    assert!(compare_exact(&quiet, &[0.5, -0.5, 0.25, 0.125]).is_err());
    assert!(compare_exact(&[0.5, -0.5, 0.25, 0.125], &quiet).is_err());
}

#[test]
fn compare_rejects_a_changed_render() {
    let scene = &scenes()[0];
    let good = render(scene, 512);
    let mut bad = good.clone();
    bad[1000] += 1e-3;
    assert!(compare(&good, &good).is_ok());
    assert!(compare(&bad, &good).is_err());
    assert!(compare(&good[1..], &good).is_err());
}

#[test]
fn compare_rejects_a_different_length() {
    let good = render(&scenes()[0], 512);
    // A clean prefix must not pass (zip would truncate), nor a longer render.
    assert!(compare(&good[..good.len() - 2], &good).is_err());
    assert!(compare(&good, &good[..good.len() - 2]).is_err());
    assert!(compare(&[], &good).is_err());
}

#[test]
fn compare_rejects_non_finite_samples() {
    let good = render(&scenes()[0], 512);
    for bad_value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut bad = good.clone();
        bad[1000] = bad_value;
        assert!(compare(&bad, &good).is_err(), "{bad_value} accepted");
    }
}

mod drums_4ch {
    use grooviq_engine::{
        Command, EngineConfig, ExportOptions, WavFormat, export_wav, render_offline_with,
        ticks_to_frames,
    };
    use grooviq_golden::{LOOPS, SAMPLE_RATE, decode_samples, drums_4ch_project, kit_dir};
    use grooviq_project::{engine_state, load, save};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    pub(super) fn golden_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    pub(super) fn read_f32(path: &Path) -> Vec<f32> {
        hound::WavReader::open(path)
            .unwrap_or_else(|e| panic!("cannot open {} ({e})", path.display()))
            .samples::<f32>()
            .map(|s| s.unwrap())
            .collect()
    }

    /// The first sample that differs, as "frame N, left/right: a vs b".
    pub(super) fn first_difference(actual: &[f32], reference: &[f32]) -> String {
        match actual
            .iter()
            .zip(reference)
            .position(|(a, b)| a.to_bits() != b.to_bits())
        {
            Some(i) => format!(
                "frame {}, {}: {:e} exported vs {:e} in the reference",
                i / 2,
                if i % 2 == 0 { "left" } else { "right" },
                actual[i],
                reference[i]
            ),
            None => format!(
                "no sample differs in the first {} ({} vs {} samples)",
                actual.len().min(reference.len()),
                actual.len(),
                reference.len()
            ),
        }
    }

    /// Kit → project → `.grooviq` → load → decode → engine state → WAV, exactly as the app does.
    #[test]
    fn drums_4ch_exports_bit_identical_to_its_golden_file() {
        let out = golden_dir().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (project, files) = drums_4ch_project(&kit_dir());
        let project_path = out.join("drums_4ch.grooviq");
        save(&project, &files, &project_path).expect("save the project");
        let loaded = load(&project_path).expect("load the project");
        assert_eq!(loaded.warnings, Vec::<String>::new());
        assert_eq!(loaded.samples.len(), 4, "every sample is embedded");
        let state = engine_state(&loaded.project, &decode_samples(&loaded.samples));

        let wav = out.join("drums_4ch.wav");
        let options = ExportOptions {
            sample_rate: SAMPLE_RATE,
            format: WavFormat::Float32,
            loops: LOOPS,
            max_tail_seconds: 10.0,
        };
        let report = export_wav(state, &options, &wav, &mut |_| {}, &AtomicBool::new(false))
            .expect("export");
        assert!(
            report.peak < 1.0,
            "the reference clips: peak {}",
            report.peak
        );

        let reference = golden_dir().join("refs").join("drums_4ch.wav");
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            std::fs::copy(&wav, &reference).unwrap();
            println!("golden file updated: explain why in the commit message");
            return;
        }
        let actual_bytes = std::fs::read(&wav).unwrap();
        let reference_bytes = std::fs::read(&reference).unwrap_or_else(|e| {
            panic!(
                "cannot read {} ({e}); create it with GOLDEN_UPDATE=1",
                reference.display()
            )
        });
        if actual_bytes != reference_bytes {
            panic!(
                "the export is not byte-identical to {} ({} vs {} bytes); first difference: {}",
                reference.display(),
                actual_bytes.len(),
                reference_bytes.len(),
                first_difference(&read_f32(&wav), &read_f32(&reference))
            );
        }
    }

    /// The reference itself, checked without listening: the pattern plus a tail of whole blocks,
    /// below full scale, and a kick onset on every kick step.
    #[test]
    fn drums_4ch_reference_is_sane() {
        let reference = read_f32(&golden_dir().join("refs").join("drums_4ch.wav"));
        let (project, _) = drums_4ch_project(&kit_dir());
        let pattern = ticks_to_frames(
            f64::from(LOOPS * 16 * grooviq_engine::STEP_TICKS),
            project.bpm,
            SAMPLE_RATE as f32,
        ) as usize;
        let frames = reference.len() / 2;
        // The last hat ends before the pattern does, so its tail is all silence and is cut.
        assert!(frames >= pattern, "{frames} frames, pattern {pattern}");
        assert_eq!((frames - pattern) % 64, 0, "the tail is not whole blocks");
        assert!(frames < pattern + SAMPLE_RATE as usize, "a tail over 1 s");
        let peak = reference.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.3 && peak < 1.0, "peak {peak}");

        // Kick steps 0, 3 (swung by 60 ticks), 8 and 10 in both loops: the 10 ms after each
        // is much louder than the 10 ms before.
        let window = SAMPLE_RATE as usize / 100;
        let level = |from: usize, to: usize| {
            reference[from * 2..to * 2]
                .iter()
                .fold(0.0f32, |m, x| m.max(x.abs()))
        };
        for lp in 0..LOOPS {
            for tick in [0, 3 * 240 + 60, 8 * 240, 10 * 240] {
                let at = ticks_to_frames(
                    f64::from(lp * 16 * 240 + tick),
                    project.bpm,
                    SAMPLE_RATE as f32,
                ) as usize;
                let before = level(at.saturating_sub(window), at);
                let after = level(at, at + window);
                assert!(
                    after > 1.5 * before && after > 0.3,
                    "no kick onset at frame {at}: {before} before, {after} after"
                );
            }
        }
    }

    #[test]
    fn drums_4ch_is_identical_at_callback_sizes_37_64_512() {
        let (project, files) = drums_4ch_project(&kit_dir());
        let state = engine_state(&project, &decode_samples(&files));
        let config = EngineConfig {
            sample_rate: SAMPLE_RATE as f32,
        };
        let frames = 3 * SAMPLE_RATE as usize;
        let events = [(0, Command::PlayLoops { loops: LOOPS })];
        let render =
            |callback| render_offline_with(config, state.clone(), &events, frames, callback);
        let reference = render(64);
        assert!(reference.iter().any(|x| x.abs() > 0.1), "silent render");
        for callback in [37, 512] {
            let other = render(callback);
            assert!(
                other.len() == reference.len()
                    && other
                        .iter()
                        .zip(&reference)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                "callback size {callback} differs: {}",
                first_difference(&other, &reference)
            );
        }
    }
}

mod notes_m2 {
    use super::drums_4ch::{first_difference, golden_dir, read_f32};
    use grooviq_engine::{
        Command, EngineConfig, ExportOptions, WavFormat, export_wav, render_offline_with,
        ticks_to_frames,
    };
    use grooviq_golden::notes::{
        CLOSED_HAT, KEYS, LEAD, LOOPS, OPEN_HAT, STEPS, SUB, SYNTH_808, notes_m2_project,
    };
    use grooviq_golden::{SAMPLE_RATE, decode_samples, kit_dir};
    use grooviq_project::{engine_state, load, save};
    use std::sync::atomic::AtomicBool;

    /// Synth channels, a sampler with loop/filter/legato/glide, a choke group → project →
    /// `.grooviq` (v2) → load → decode → engine state → WAV, exactly as the app does. This is
    /// M2's acceptance file: it must match on macOS and Windows.
    #[test]
    fn notes_m2_exports_bit_identical_to_its_golden_file() {
        let out = golden_dir().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let (project, files) = notes_m2_project(&kit_dir());
        let project_path = out.join("notes_m2.grooviq");
        save(&project, &files, &project_path).expect("save the project");
        let loaded = load(&project_path).expect("load the project");
        assert_eq!(loaded.warnings, Vec::<String>::new());
        // Saving fills in each sample's entry in the file; nothing else may change.
        let mut reloaded = loaded.project.clone();
        for sample in reloaded
            .channels
            .iter_mut()
            .filter_map(|c| c.sample.as_mut())
        {
            sample.file = None;
        }
        assert!(
            reloaded == project,
            "the file round trip changed the project"
        );
        assert_eq!(loaded.samples.len(), 3, "every sample is embedded");
        let state = engine_state(&loaded.project, &decode_samples(&loaded.samples));

        let wav = out.join("notes_m2.wav");
        let options = ExportOptions {
            sample_rate: SAMPLE_RATE,
            format: WavFormat::Float32,
            loops: LOOPS,
            max_tail_seconds: 10.0,
        };
        let report = export_wav(state, &options, &wav, &mut |_| {}, &AtomicBool::new(false))
            .expect("export");
        assert!(
            report.peak < 1.0,
            "the reference clips: peak {}",
            report.peak
        );

        let reference = golden_dir().join("refs").join("notes_m2.wav");
        if std::env::var("GOLDEN_UPDATE").as_deref() == Ok("1") {
            std::fs::copy(&wav, &reference).unwrap();
            println!("golden file updated: explain why in the commit message");
            return;
        }
        let actual_bytes = std::fs::read(&wav).unwrap();
        let reference_bytes = std::fs::read(&reference).unwrap_or_else(|e| {
            panic!(
                "cannot read {} ({e}); create it with GOLDEN_UPDATE=1",
                reference.display()
            )
        });
        if actual_bytes != reference_bytes {
            panic!(
                "the export is not byte-identical to {} ({} vs {} bytes); first difference: {}",
                reference.display(),
                actual_bytes.len(),
                reference_bytes.len(),
                first_difference(&read_f32(&wav), &read_f32(&reference))
            );
        }
    }

    fn render(state: grooviq_engine::EngineState, callback: usize) -> Vec<f32> {
        let config = EngineConfig {
            sample_rate: SAMPLE_RATE as f32,
        };
        let frames = 4 * SAMPLE_RATE as usize;
        let events = [(0, Command::PlayLoops { loops: LOOPS })];
        render_offline_with(config, state, &events, frames, callback)
    }

    #[test]
    fn notes_m2_is_identical_at_callback_sizes_37_64_512() {
        let (project, files) = notes_m2_project(&kit_dir());
        let state = engine_state(&project, &decode_samples(&files));
        let reference = render(state.clone(), 64);
        assert!(reference.iter().any(|x| x.abs() > 0.1), "silent render");
        for callback in [37, 512] {
            let other = render(state.clone(), callback);
            assert!(
                other.len() == reference.len()
                    && other
                        .iter()
                        .zip(&reference)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                "callback size {callback} differs: {}",
                first_difference(&other, &reference)
            );
        }
    }

    /// The reference itself, checked without listening: the pattern plus a tail of whole blocks,
    /// a sane level, and every channel contributes.
    #[test]
    fn notes_m2_reference_is_sane() {
        let reference = read_f32(&golden_dir().join("refs").join("notes_m2.wav"));
        let (project, files) = notes_m2_project(&kit_dir());
        let pattern = ticks_to_frames(
            f64::from(LOOPS * STEPS * grooviq_engine::STEP_TICKS),
            project.bpm,
            SAMPLE_RATE as f32,
        ) as usize;
        let frames = reference.len() / 2;
        assert!(frames >= pattern, "{frames} frames, pattern {pattern}");
        assert_eq!((frames - pattern) % 64, 0, "the tail is not whole blocks");
        assert!(
            frames < pattern + 2 * SAMPLE_RATE as usize,
            "a tail over 2 s"
        );
        let peak = reference.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.2 && peak < 1.0, "peak {peak}");

        // Muting any one channel changes the render: every channel is heard.
        let samples = decode_samples(&files);
        let all = render(engine_state(&project, &samples), 512);
        for (id, name) in [
            (SYNTH_808, "808"),
            (KEYS, "Keys"),
            (LEAD, "Lead"),
            (SUB, "Sub"),
            (CLOSED_HAT, "Closed hat"),
            (OPEN_HAT, "Open hat"),
        ] {
            let mut muted = project.clone();
            muted.channels.iter_mut().find(|c| c.id == id).unwrap().mute = true;
            let without = render(engine_state(&muted, &samples), 512);
            assert!(
                without.iter().zip(&all).any(|(a, b)| (a - b).abs() > 1e-3),
                "muting {name} changes nothing: it is not heard"
            );
        }
    }
}
