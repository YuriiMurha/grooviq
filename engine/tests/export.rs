//! WAV export: the file holds exactly what the offline engine renders, PCM 24 rounds and
//! clips as documented, the tail rings out and its trailing silence is cut, the tail is capped,
//! and a cancelled or failed export leaves no file behind.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig, EngineState, ExportError,
    ExportOptions, PatternData, STEP_TICKS, SampleBuf, SeqNote, WavFormat, export_wav,
    render_offline_with, ticks_to_frames,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const SR: u32 = 48_000;
/// Sampler parameter indices.
const ONE_SHOT: usize = 7;
const RELEASE: usize = 11;
const GAIN: usize = 12;
const DECLICK: usize = 14;

fn defaults() -> Vec<f32> {
    SPEC.iter().map(|&(_, _, d)| d).collect()
}

/// One 1.0, then zeros.
fn click() -> Arc<SampleBuf> {
    let mut l = vec![0.0; 16];
    l[0] = 1.0;
    Arc::new(SampleBuf {
        rate: SR as f32,
        r: l.clone(),
        l,
    })
}

/// `seconds` of a decaying 220 Hz sine (left) and 330 Hz (right), peak 0.6.
fn tone(seconds: f32) -> Arc<SampleBuf> {
    let n = (seconds * SR as f32) as usize;
    let wave = |hz: f32| -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                0.6 * libm::expf(-t * 3.0) * libm::sinf(std::f32::consts::TAU * hz * t)
            })
            .collect()
    };
    Arc::new(SampleBuf {
        rate: SR as f32,
        l: wave(220.0),
        r: wave(330.0),
    })
}

/// `seconds` of 0.5.
fn dc(seconds: f32) -> Arc<SampleBuf> {
    let n = (seconds * SR as f32) as usize;
    Arc::new(SampleBuf {
        rate: SR as f32,
        l: vec![0.5; n],
        r: vec![0.5; n],
    })
}

fn channel(id: u32, sample: &Arc<SampleBuf>, sampler: Vec<f32>, mix: ChannelMix) -> ChannelSetup {
    ChannelSetup {
        id: ChannelId(id),
        sample: Some(Arc::clone(sample)),
        sampler,
        mix,
    }
}

fn note(step: u32, channel: u32, velocity: f32) -> SeqNote {
    SeqNote {
        tick: step * STEP_TICKS,
        channel: ChannelId(channel),
        key: 60,
        velocity,
        length: STEP_TICKS,
    }
}

/// Two channels (a tone and a click) over a 16-step pattern at 140 BPM.
fn groove() -> EngineState {
    let mut click_params = defaults();
    click_params[DECLICK] = 0.0;
    EngineState {
        bpm: 140.0,
        channels: vec![
            channel(1, &tone(0.4), defaults(), ChannelMix::default()),
            channel(
                2,
                &click(),
                click_params,
                ChannelMix {
                    volume: 0.8,
                    pan: 0.3,
                    ..ChannelMix::default()
                },
            ),
        ],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![
                note(0, 1, 1.0),
                note(2, 2, 0.7),
                note(6, 1, 0.5),
                note(9, 2, 1.0),
                note(15, 1, 0.9),
            ],
        },
    }
}

fn options(format: WavFormat, loops: u32) -> ExportOptions {
    ExportOptions {
        sample_rate: SR,
        format,
        loops,
        max_tail_seconds: 10.0,
    }
}

/// Frames of `loops` loops of `state`'s pattern at 48 kHz.
fn pattern_frames(state: &EngineState, loops: u32) -> u64 {
    ticks_to_frames(
        f64::from(loops) * f64::from(state.pattern.length),
        state.bpm,
        SR as f32,
    )
}

fn render(state: &EngineState, loops: u32, frames: u64) -> Vec<f32> {
    render_offline_with(
        EngineConfig {
            sample_rate: SR as f32,
        },
        state.clone(),
        &[(0, Command::PlayLoops { loops })],
        frames as usize,
        64,
    )
}

fn export(
    state: &EngineState,
    options: &ExportOptions,
    path: &Path,
) -> Result<grooviq_engine::ExportReport, ExportError> {
    export_wav(
        state.clone(),
        options,
        path,
        &mut |_| {},
        &AtomicBool::new(false),
    )
}

fn read_f32(path: &Path) -> (hound::WavSpec, Vec<f32>) {
    let mut reader = hound::WavReader::open(path).expect("open the export");
    let spec = reader.spec();
    let samples = reader.samples::<f32>().map(|s| s.unwrap()).collect();
    (spec, samples)
}

fn read_i32(path: &Path) -> (hound::WavSpec, Vec<i32>) {
    let mut reader = hound::WavReader::open(path).expect("open the export");
    let spec = reader.spec();
    let samples = reader.samples::<i32>().map(|s| s.unwrap()).collect();
    (spec, samples)
}

/// The names in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn part_of(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

#[test]
fn defaults_are_48k_pcm24_one_loop_and_a_ten_second_tail() {
    let d = ExportOptions::default();
    assert_eq!(d.sample_rate, 48_000);
    assert_eq!(d.format, WavFormat::Pcm24);
    assert_eq!(d.loops, 1);
    assert_eq!(d.max_tail_seconds, 10.0);
}

#[test]
fn float32_export_holds_the_render_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("groove.wav");
    let state = groove();
    let mut seen = Vec::new();
    let report = export_wav(
        state.clone(),
        &options(WavFormat::Float32, 2),
        &path,
        &mut |p| seen.push(p),
        &AtomicBool::new(false),
    )
    .unwrap();

    let (spec, samples) = read_f32(&path);
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, SR);
    assert_eq!(spec.bits_per_sample, 32);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(samples.len() as u64, report.frames * 2);
    assert!(report.frames > pattern_frames(&state, 2), "no tail");

    let reference = render(&state, 2, report.frames);
    let same = samples
        .iter()
        .zip(&reference)
        .all(|(a, b)| a.to_bits() == b.to_bits());
    assert!(same, "the file differs from the offline render");
    let peak = reference.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert_eq!(report.peak, peak);
    assert_eq!(report.clipped, 0);

    // Progress runs 0..=1, never backwards, and ends at exactly 1.
    assert!(seen.len() > 2, "progress reported {} times", seen.len());
    assert!(seen.windows(2).all(|w| w[0] <= w[1]));
    assert!(seen.iter().all(|p| (0.0..=1.0).contains(p)));
    assert_eq!(seen.last(), Some(&1.0));
    assert_eq!(
        seen.iter().filter(|&&p| p == 1.0).count(),
        1,
        "1.0 before the end"
    );
    assert!(!part_of(&path).exists());
}

#[test]
fn pcm24_rounds_clips_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loud.wav");
    let mut state = groove();
    // The click at +12 dB: about 3.98, far past full scale.
    state.channels[1].sampler[GAIN] = 12.0;
    let report = export(&state, &options(WavFormat::Pcm24, 1), &path).unwrap();

    let (spec, samples) = read_i32(&path);
    assert_eq!(spec.bits_per_sample, 24);
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    assert_eq!(spec.channels, 2);
    assert_eq!(samples.len() as u64, report.frames * 2);
    assert_eq!(samples.iter().max(), Some(&8_388_607));

    let reference = render(&state, 1, report.frames);
    let expected: Vec<i32> = reference
        .iter()
        .map(|&x| {
            (f64::from(x) * 8_388_607.0)
                .round()
                .clamp(-8_388_608.0, 8_388_607.0) as i32
        })
        .collect();
    let first_bad = samples.iter().zip(&expected).position(|(a, b)| a != b);
    assert_eq!(first_bad, None, "a sample is not round(x * 8_388_607)");

    let clipped = reference.iter().filter(|x| x.abs() > 1.0).count() as u64;
    assert!(clipped > 0);
    assert_eq!(report.clipped, clipped);
    assert!(report.peak > 1.0);
    assert_eq!(
        report.peak,
        reference.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    );
}

#[test]
fn the_tail_rings_out_and_trailing_silence_is_cut() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tail.wav");
    let mut gate = defaults();
    gate[ONE_SHOT] = 0.0;
    gate[RELEASE] = 500.0;
    // A held tone on the last step: its note-off lands at the pattern's end, then it releases.
    let state = EngineState {
        bpm: 140.0,
        channels: vec![channel(1, &dc(3.0), gate, ChannelMix::default())],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(15, 1, 1.0)],
        },
    };
    let report = export(&state, &options(WavFormat::Float32, 1), &path).unwrap();
    let pattern = pattern_frames(&state, 1);
    assert!(
        report.frames > pattern + 4_800,
        "{} frames: the release was cut",
        report.frames
    );
    // The tail is whole blocks after the pattern part.
    assert_eq!((report.frames - pattern) % 64, 0);
    assert!(
        report.frames < pattern + 10 * u64::from(SR),
        "never fell silent"
    );

    let (_, samples) = read_f32(&path);
    let last_block = &samples[samples.len() - 128..];
    assert!(
        last_block.iter().any(|x| x.abs() > 1e-5),
        "the file ends in a silent block"
    );
    // What was cut really is silence: the render goes quiet right after.
    let longer = render(&state, 1, report.frames + 4_800);
    let after = &longer[samples.len()..];
    assert!(after.iter().all(|x| x.abs() <= 1e-5));
}

#[test]
fn a_tenth_of_a_second_of_silence_ends_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gap.wav");
    // A one-shot on the last step: 50 ms of sound, `gap` s of silence, 50 ms of sound. The
    // pattern ends 107 ms after the step, so the tail starts inside the gap.
    let with_gap = |gap: f32| {
        let n = |s: f32| (s * SR as f32) as usize;
        let mut l = vec![0.5; n(0.05)];
        l.extend(vec![0.0; n(gap)]);
        l.extend(vec![0.5; n(0.05)]);
        let sample = Arc::new(SampleBuf {
            rate: SR as f32,
            r: l.clone(),
            l,
        });
        EngineState {
            bpm: 140.0,
            channels: vec![channel(1, &sample, defaults(), ChannelMix::default())],
            pattern: PatternData {
                length: 16 * STEP_TICKS,
                notes: vec![note(15, 1, 1.0)],
            },
        }
    };

    // 300 ms: the tail is silent for well over 0.1 s, so it ends there and is cut whole.
    let state = with_gap(0.3);
    let pattern = pattern_frames(&state, 1);
    let report = export(&state, &options(WavFormat::Float32, 1), &path).unwrap();
    assert_eq!(report.frames, pattern);
    // The second burst is really there, past the gap.
    let longer = render(&state, 1, report.frames + u64::from(SR) / 2);
    assert!(
        longer[report.frames as usize * 2..]
            .iter()
            .any(|x| x.abs() > 0.1)
    );

    // 120 ms: about 63 ms of it falls in the tail, under 0.1 s, so the second burst is kept.
    let state = with_gap(0.12);
    let report = export(&state, &options(WavFormat::Float32, 1), &path).unwrap();
    assert!(
        report.frames > pattern + 3_000 + 2_000,
        "{} frames: the tail stopped in the gap",
        report.frames - pattern
    );
}

#[test]
fn the_tail_is_capped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.wav");
    // A 30 s one-shot that starts on the first step.
    let state = EngineState {
        bpm: 140.0,
        channels: vec![channel(1, &dc(30.0), defaults(), ChannelMix::default())],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(0, 1, 1.0)],
        },
    };
    let options = ExportOptions {
        max_tail_seconds: 1.0,
        ..options(WavFormat::Float32, 1)
    };
    let report = export(&state, &options, &path).unwrap();
    let pattern = pattern_frames(&state, 1);
    assert!(report.frames >= pattern + 48_000, "{}", report.frames);
    assert!(report.frames <= pattern + 48_000 + 64, "{}", report.frames);
    let (_, samples) = read_f32(&path);
    assert_eq!(samples.len() as u64, report.frames * 2);
}

#[test]
fn cancel_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancelled.wav");
    let cancel = AtomicBool::new(false);
    let mut calls = 0;
    let result = export_wav(
        groove(),
        &options(WavFormat::Pcm24, 4),
        &path,
        // Cancel once the export is under way.
        &mut |p| {
            calls += 1;
            if p > 0.0 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    );
    assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
    // 0.0, then the first chunk sets the flag: the next chunk never starts.
    assert_eq!(calls, 2, "the export ran on after the cancel");
    assert_eq!(listing(dir.path()), Vec::<String>::new());

    // A file already there is left as it was.
    std::fs::write(&path, b"keep me").unwrap();
    let result = export_wav(
        groove(),
        &options(WavFormat::Pcm24, 1),
        &path,
        &mut |_| {},
        &AtomicBool::new(true),
    );
    assert!(matches!(result, Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    assert_eq!(listing(dir.path()), vec!["cancelled.wav".to_string()]);
}

#[test]
fn an_unwritable_path_reports_io_and_leaves_no_partial_file() {
    let dir = tempfile::tempdir().unwrap();

    // A folder that does not exist: nothing can be created.
    let missing = dir.path().join("no such folder").join("out.wav");
    let result = export(&groove(), &options(WavFormat::Pcm24, 1), &missing);
    assert!(matches!(result, Err(ExportError::Io(_))), "{result:?}");
    assert_eq!(listing(dir.path()), Vec::<String>::new());

    // A folder in the way: the render is written, then the rename fails.
    let folder = dir.path().join("taken.wav");
    std::fs::create_dir(&folder).unwrap();
    let result = export(&groove(), &options(WavFormat::Pcm24, 1), &folder);
    assert!(matches!(result, Err(ExportError::Io(_))), "{result:?}");
    assert_eq!(listing(dir.path()), vec!["taken.wav".to_string()]);
    assert!(folder.is_dir());
}

#[test]
fn export_is_faster_than_real_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("eight_seconds.wav");
    // Four channels, 16 steps at 120 BPM (2 s), four loops: 8 s.
    let mut state = groove();
    state.bpm = 120.0;
    let mut extra = defaults();
    extra[ONE_SHOT] = 0.0;
    state
        .channels
        .push(channel(3, &tone(0.3), extra, ChannelMix::default()));
    state
        .channels
        .push(channel(4, &dc(0.1), defaults(), ChannelMix::default()));
    state.pattern.notes = (0..16)
        .flat_map(|step| (1..=4).map(move |ch| note(step, ch, 0.8)))
        .collect();
    let started = Instant::now();
    let report = export(&state, &options(WavFormat::Pcm24, 4), &path).unwrap();
    let took = started.elapsed();
    assert!(report.frames >= 8 * u64::from(SR));
    assert!(took.as_secs_f32() < 4.0, "an 8 s export took {took:?}");
}

/// `seconds` of the constant `value` on both sides.
fn dc_at(seconds: f32, value: f32) -> Arc<SampleBuf> {
    let n = (seconds * SR as f32) as usize;
    Arc::new(SampleBuf {
        rate: SR as f32,
        l: vec![value; n],
        r: vec![value; n],
    })
}

/// One note of `sample` on the last step, at unity gain with declick off, so the sample's
/// values come out unchanged and carry on into the tail.
fn held_into_the_tail(sample: &Arc<SampleBuf>, step: u32) -> EngineState {
    let mut exact = defaults();
    exact[DECLICK] = 0.0;
    EngineState {
        bpm: 140.0,
        channels: vec![channel(1, sample, exact, ChannelMix::default())],
        pattern: PatternData {
            length: 16 * STEP_TICKS,
            notes: vec![note(step, 1, 1.0)],
        },
    }
}

#[test]
fn cancel_during_the_tail_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancelled_tail.wav");
    let part = part_of(&path);
    // A 60 s one-shot from the first step: the tail runs for up to a minute of audio.
    let state = held_into_the_tail(&dc(60.0), 0);
    let options = ExportOptions {
        max_tail_seconds: 60.0,
        ..options(WavFormat::Float32, 1)
    };
    // Past the pattern part's bytes (8 per frame, plus the header and the write buffer's lag):
    // the export is in the tail.
    let threshold = pattern_frames(&state, 1) * 8 + 64 * 1024;
    let cancel = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    let mut seen_at = 0;
    let result = std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                let len = std::fs::metadata(&part).map_or(0, |m| m.len());
                if len > threshold {
                    cancel.store(true, Ordering::Relaxed);
                    return len;
                }
                std::thread::yield_now();
            }
            0
        });
        let result = export_wav(state.clone(), &options, &path, &mut |_| {}, &cancel);
        finished.store(true, Ordering::Relaxed);
        seen_at = watcher.join().unwrap();
        result
    });
    assert!(seen_at > threshold, "the watcher never saw the tail start");
    assert!(matches!(result, Err(ExportError::Cancelled)), "{result:?}");
    assert_eq!(listing(dir.path()), Vec::<String>::new());
}

#[test]
fn a_tail_exactly_at_the_silence_threshold_counts_as_silent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threshold.wav");
    // 2 s of exactly 1e-5 from the last step: it sounds on through the whole tail window.
    let state = held_into_the_tail(&dc_at(2.0, 1e-5), 15);
    let pattern = pattern_frames(&state, 1);
    // The engine passes the value through unchanged, so every tail frame is exactly 1e-5.
    let longer = render(&state, 1, pattern + 9_600);
    let tail = &longer[pattern as usize * 2..];
    assert!(
        tail.iter().all(|&x| x == 1e-5),
        "the tail is not exactly 1e-5"
    );

    let report = export(&state, &options(WavFormat::Float32, 1), &path).unwrap();
    assert_eq!(report.frames, pattern, "a tail at exactly 1e-5 was kept");
}

#[test]
fn full_scale_is_not_clipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("full_scale.wav");
    // The click at unity gain: one frame of exactly 1.0 on each side.
    let state = held_into_the_tail(&click(), 0);
    let longer = render(&state, 1, pattern_frames(&state, 1));
    assert_eq!(longer.iter().fold(0.0f32, |m, x| m.max(x.abs())), 1.0);

    let report = export(&state, &options(WavFormat::Pcm24, 1), &path).unwrap();
    assert_eq!(report.clipped, 0, "1.0 is full scale, not a clip");
    assert_eq!(report.peak, 1.0);
    let (_, samples) = read_i32(&path);
    assert_eq!(samples.iter().max(), Some(&8_388_607));
}
