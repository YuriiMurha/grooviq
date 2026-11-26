//! WAV export: the file holds exactly what the offline engine renders, PCM 24 rounds and
//! clips as documented, the tail rings out and its trailing silence is cut, the tail is capped,
//! and a cancelled or failed export leaves no file behind.

use grooviq_dsp::sampler::SPEC;
use grooviq_engine::{
    ChannelId, ChannelMix, ChannelSetup, Command, EngineConfig, EngineState, ExportError,
    ExportOptions, Instrument, PatternData, STEP_TICKS, SampleBuf, SeqNote, WavFormat, export_wav,
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
    ChannelSetup::sampler(ChannelId(id), Some(Arc::clone(sample)), sampler, mix)
}

fn note(step: u32, channel: u32, velocity: f32) -> SeqNote {
    SeqNote {
        tick: step * STEP_TICKS,
        channel: ChannelId(channel),
        key: 60,
        velocity,
        length: STEP_TICKS,
        slide: false,
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
        ..EngineState::default()
    }
}

fn options(format: WavFormat, loops: u32) -> ExportOptions {
    ExportOptions {
        sample_rate: SR,
        format,
        loops,
        song_region: None,
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
    if let Instrument::Sampler { params, .. } = &mut state.channels[1].instrument {
        params[GAIN] = 12.0;
    }
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
        ..EngineState::default()
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
            ..EngineState::default()
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
        ..EngineState::default()
    };
    let options = ExportOptions {
        song_region: None,
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
fn eight_second_export_has_complete_pcm_frames() {
    eight_second_export();
}

#[test]
#[ignore = "4 s budget runs via make export-budget in release mode"]
fn export_is_faster_than_real_time() {
    #[allow(clippy::assertions_on_constants)]
    // Runtime failure if the ignored budget is invoked in debug.
    {
        assert!(!cfg!(debug_assertions), "export budget requires --release");
    }
    let took = eight_second_export();
    eprintln!("8 s PCM24 export: {took:?}; budget: 4 s");
    assert!(took.as_secs_f32() < 4.0, "an 8 s export took {took:?}");
}

fn eight_second_export() -> std::time::Duration {
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
    let (spec, samples) = read_i32(&path);
    assert_eq!(spec.sample_rate, SR);
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.bits_per_sample, 24);
    assert_eq!(samples.len() as u64, report.frames * 2);
    took
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
        ..EngineState::default()
    }
}

/// Holds the export at its second tail cancel check (the first one, at 0 frames, comes before
/// any tail is rendered) until the test has set `cancel`: the check that follows must see it.
/// The wait is bounded, so a broken barrier fails the test instead of hanging CI.
struct TailGate {
    reached: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    go: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    timed_out: AtomicBool,
}

const GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl TailGate {
    /// Lets a waiting test know the export is over (it never reached the checkpoint).
    fn close(&self) {
        self.reached.lock().unwrap().take();
    }
}

impl grooviq_engine::PartFiles for TailGate {
    fn sync(&self, file: &std::fs::File, path: &Path) -> std::io::Result<()> {
        grooviq_engine::RealFiles.sync(file, path)
    }

    fn rename(&self, part: &Path, to: &Path) -> std::io::Result<()> {
        grooviq_engine::RealFiles.rename(part, to)
    }

    fn tail_checkpoint(&self, tail_frames: u64) {
        if tail_frames != 4_096 {
            return;
        }
        if let Some(reached) = &*self.reached.lock().unwrap() {
            let _ = reached.send(());
        }
        if self.go.lock().unwrap().recv_timeout(GATE_TIMEOUT).is_err() {
            self.timed_out.store(true, Ordering::Relaxed);
        }
    }
}

#[test]
fn cancel_during_the_tail_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancelled_tail.wav");
    // A 60 s one-shot from the first step: it sounds through the whole tail. The tail is capped
    // at 4 800 frames, so the checkpoint at 4 096 is the LAST cancel check: a cancel set after
    // it (the race this test used to have) would let the export finish.
    let state = held_into_the_tail(&dc(60.0), 0);
    let options = ExportOptions {
        song_region: None,
        max_tail_seconds: 0.1,
        ..options(WavFormat::Float32, 1)
    };
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let gate = TailGate {
        reached: std::sync::Mutex::new(Some(reached_tx)),
        go: std::sync::Mutex::new(go_rx),
        timed_out: AtomicBool::new(false),
    };
    let cancel = AtomicBool::new(false);
    let (result, mid_export) = std::thread::scope(|s| {
        // Waits for the export to be in the tail, sets cancel, then lets it go on.
        let (cancel_ref, dir_path) = (&cancel, dir.path());
        let controller = s.spawn(move || {
            reached_rx.recv_timeout(GATE_TIMEOUT).ok()?;
            let mid_export = listing(dir_path);
            cancel_ref.store(true, Ordering::Relaxed);
            go_tx.send(()).unwrap();
            Some(mid_export)
        });
        let result = grooviq_engine::export_wav_with(
            state.clone(),
            &options,
            &path,
            &mut |_| {},
            &cancel,
            &gate,
        );
        gate.close();
        (result, controller.join().unwrap())
    });
    assert!(
        !gate.timed_out.load(Ordering::Relaxed),
        "the export waited at the checkpoint and cancel was never set"
    );
    let mid_export = mid_export.expect("the export never reached the tail");
    // The export's own hidden part file is there while it renders, so the empty folder below
    // means it was removed, not that it never existed.
    assert_eq!(mid_export.len(), 1, "{mid_export:?}");
    assert!(
        mid_export[0].starts_with(".cancelled_tail.wav.grooviq-"),
        "{mid_export:?}"
    );
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

/// Sets `path`'s modification time `age` in the past.
fn backdate(path: &Path, age: std::time::Duration) {
    // A folder cannot be opened for writing; a read handle sets its time on Unix.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() - age)
        .unwrap();
}

const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

/// The hidden part files an export of `mix.wav` leaves in `dir`.
fn owned_parts(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|n| n.starts_with(".mix.wav.grooviq-") && n.ends_with(".part"))
        .collect()
}

#[test]
fn a_part_file_that_is_not_ours_is_never_touched_however_old() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    // The person's own notes, named like a part file, an hour old: not ours to delete.
    let theirs = part_of(&path);
    std::fs::write(&theirs, b"my notes").unwrap();
    backdate(&theirs, HOUR);

    export(&groove(), &options(WavFormat::Pcm24, 1), &path).unwrap();
    assert_eq!(std::fs::read(&theirs).unwrap(), b"my notes");
    assert_eq!(
        listing(dir.path()),
        vec!["mix.wav".to_string(), "mix.wav.part".to_string()]
    );
    assert!(read_i32(&path).1.len() > 100, "the export is a real file");
}

#[test]
fn a_plain_part_file_does_not_stop_an_export() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    let part = part_of(&path);
    // Fresh: maybe someone else's, maybe a crashed older version's. Either way, left alone.
    std::fs::write(&part, b"in use?").unwrap();
    export(&groove(), &options(WavFormat::Pcm24, 1), &path).unwrap();
    assert_eq!(std::fs::read(&part).unwrap(), b"in use?");
    // And a failed export does not delete it either.
    let failed = export_wav(
        groove(),
        &options(WavFormat::Pcm24, 4),
        &path,
        &mut |_| {},
        &AtomicBool::new(true),
    );
    assert!(matches!(failed, Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read(&part).unwrap(), b"in use?");
}

#[test]
fn an_old_part_file_of_a_dead_export_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    // An export that died: its part file is still there, and nobody holds its lock.
    let dead = grooviq_engine::create_owned_temp(&path, ".part").unwrap();
    let dead_path = dead.path.clone();
    drop(dead.file);
    backdate(&dead_path, HOUR);
    // Younger than ten minutes: kept (it may be a writer between two syscalls).
    let young = grooviq_engine::create_owned_temp(&path, ".part").unwrap();
    let young_path = young.path.clone();
    drop(young.file);

    export(&groove(), &options(WavFormat::Pcm24, 1), &path).unwrap();
    assert!(!dead_path.exists());
    assert!(young_path.exists());
}

#[test]
fn an_old_part_file_whose_export_is_still_running_is_kept_and_not_written_into() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    // Another export, stalled for an hour but alive: it holds its lock.
    let live = grooviq_engine::create_owned_temp(&path, ".part").unwrap();
    backdate(&live.path, HOUR);

    export(&groove(), &options(WavFormat::Pcm24, 1), &path).unwrap();
    assert!(live.path.exists(), "a live writer's file was removed");
    assert_eq!(
        live.file.metadata().unwrap().len(),
        0,
        "it was written into"
    );
    // It can still finish its own work.
    drop(live.file);
    assert_eq!(owned_parts(dir.path()).len(), 1);
}

#[test]
fn two_exports_of_one_file_do_not_delete_each_others_part_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    let results: Vec<_> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| export(&groove(), &options(WavFormat::Pcm24, 8), &path)))
            .collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    // Each one wrote a whole file of its own and renamed it: both finish.
    for result in &results {
        assert!(result.is_ok(), "{results:?}");
    }
    assert_eq!(listing(dir.path()), vec!["mix.wav".to_string()]);
    let (_, samples) = read_i32(&path);
    assert_eq!(
        samples.len() as u64,
        results[0].as_ref().unwrap().frames * 2
    );
}

#[cfg(unix)]
#[test]
fn a_part_symlink_is_never_followed_or_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    let target = dir.path().join("precious.txt");
    std::fs::write(&target, b"precious").unwrap();
    backdate(&target, HOUR);
    std::os::unix::fs::symlink(&target, part_of(&path)).unwrap();
    export(&groove(), &options(WavFormat::Pcm24, 1), &path).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"precious");
    assert!(
        std::fs::symlink_metadata(part_of(&path))
            .unwrap()
            .is_symlink()
    );
}

/// Records what an export does to its part file, and checks the file at each step.
struct Recorder {
    events: std::sync::Mutex<Vec<&'static str>>,
    fail_sync: bool,
}

impl Recorder {
    fn new(fail_sync: bool) -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            fail_sync,
        }
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}

impl grooviq_engine::PartFiles for Recorder {
    fn sync(&self, file: &std::fs::File, _path: &Path) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        self.events.lock().unwrap().push("sync");
        // Everything is already in the file: flushed out of the buffer, header finalized
        // (24-bit stereo: hound's 68-byte extensible header and 3 bytes a sample). Syncing
        // earlier would put only part of it on disk. Read it through the export's own handle:
        // the file is locked, and Windows refuses a second handle even to read.
        let mut handle = file;
        let at = handle.stream_position()?;
        handle.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        handle.read_to_end(&mut bytes)?;
        handle.seek(SeekFrom::Start(at))?;
        let reader =
            hound::WavReader::new(std::io::Cursor::new(&bytes)).expect("a whole WAV at sync time");
        assert!(reader.len() > 0, "the header was not finalized");
        assert_eq!(
            bytes.len() as u64,
            68 + u64::from(reader.len()) * 3,
            "bytes were still buffered"
        );
        if self.fail_sync {
            return Err(std::io::Error::other("injected: the disk refused to sync"));
        }
        file.sync_all()
    }

    fn rename(&self, part: &Path, to: &Path) -> std::io::Result<()> {
        let events = self.events();
        assert_eq!(events, ["sync"], "renamed before the bytes were synced");
        self.events.lock().unwrap().push("rename");
        std::fs::rename(part, to)
    }
}

fn export_through(
    files: &Recorder,
    path: &Path,
) -> Result<grooviq_engine::ExportReport, ExportError> {
    grooviq_engine::export_wav_with(
        groove(),
        &options(WavFormat::Pcm24, 1),
        path,
        &mut |_| {},
        &AtomicBool::new(false),
        files,
    )
}

#[test]
fn an_export_syncs_the_finished_file_and_only_then_renames_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    let files = Recorder::new(false);
    export_through(&files, &path).unwrap();
    assert_eq!(files.events(), ["sync", "rename"]);
    assert_eq!(listing(dir.path()), vec!["mix.wav".to_string()]);
}

#[test]
fn a_failed_sync_keeps_the_destination_and_removes_only_its_own_part_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mix.wav");
    std::fs::write(&path, b"the previous export").unwrap();
    // Neighbours that are not this export's: a plain part file, and another running export's.
    std::fs::write(part_of(&path), b"someone's notes").unwrap();
    let other = grooviq_engine::create_owned_temp(&path, ".part").unwrap();
    let other_name = other
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let files = Recorder::new(true);
    let result = export_through(&files, &path);
    let Err(ExportError::Io(e)) = result else {
        panic!("expected an Io error, got {result:?}");
    };
    assert!(e.to_string().contains("refused to sync"), "{e}");
    assert_eq!(files.events(), ["sync"], "it renamed after a failed sync");
    assert_eq!(std::fs::read(&path).unwrap(), b"the previous export");
    assert_eq!(std::fs::read(part_of(&path)).unwrap(), b"someone's notes");
    // Sorted, and a leading dot sorts first.
    assert_eq!(
        listing(dir.path()),
        vec![
            other_name,
            "mix.wav".to_string(),
            "mix.wav.part".to_string()
        ],
        "only this export's own part file goes"
    );
    drop(other);
}

/// A handle the OS cannot sync: a pipe on Unix (`fsync` refuses special files), a read-only file
/// on Windows (`FlushFileBuffers` needs write access).
fn unsyncable_handle(dir: &Path) -> std::fs::File {
    #[cfg(unix)]
    {
        let _ = dir;
        let (_reader, writer) = std::io::pipe().unwrap();
        // Keeping only the write end open is fine for `fsync`.
        std::fs::File::from(std::os::fd::OwnedFd::from(writer))
    }
    #[cfg(not(unix))]
    {
        let path = dir.join("read-only.bin");
        std::fs::write(&path, b"x").unwrap();
        std::fs::File::open(&path).unwrap()
    }
}

#[test]
fn the_real_adapter_reports_a_sync_the_os_refuses() {
    use grooviq_engine::PartFiles;
    let dir = tempfile::tempdir().unwrap();
    let file = unsyncable_handle(dir.path());
    // Proves the handle is one `sync_all` refuses; otherwise this test proves nothing.
    assert!(file.sync_all().is_err(), "the fixture can be synced");
    // The production adapter hands the file to the OS and passes its answer on. An adapter that
    // returned Ok without syncing would hide a disk that never got the bytes.
    let result = grooviq_engine::RealFiles.sync(&file, &dir.path().join("mix.wav.part"));
    assert!(result.is_err(), "the real adapter did not sync");
}

fn routed_export_fixture(kind: &str) -> EngineState {
    use grooviq_engine::mixer::*;
    let insert = InsertId(1);
    EngineState {
        pattern: PatternData {
            length: STEP_TICKS,
            notes: vec![],
        },
        mix: MixSetup::Routed(RoutedMix {
            mixer: Mixer {
                tracks: vec![Track {
                    id: MASTER,
                    output: None,
                    inserts: vec![Insert {
                        id: insert,
                        latency_frames: 0,
                        memory_bytes: 0,
                    }],
                }],
                ..Default::default()
            },
            devices: vec![InsertDevice {
                insert,
                kind: kind.into(),
                seed: 123,
                params: Vec::new(),
            }],
            gains: Vec::new(),
        }),
        ..EngineState::default()
    }
}

#[test]
fn unsupported_insert_rate_refuses_before_export_touches_files_or_reports_progress() {
    use std::fs::{self, File, FileTimes};
    use std::time::{Duration, SystemTime};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("export.wav");
    let stale = dir
        .path()
        .join(".export.wav.grooviq-1-1234567890abcdef.part");
    fs::write(&path, b"old export").unwrap();
    fs::write(&stale, b"old partial").unwrap();
    File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(1200)))
        .unwrap();
    let list = || {
        let mut names = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let before = list();
    for sample_rate in [8_000, 384_000] {
        let options = ExportOptions {
            sample_rate,
            max_tail_seconds: 0.0,
            ..Default::default()
        };
        let mut progress = Vec::new();
        let error = export_wav(
            routed_export_fixture("bitcrusher"),
            &options,
            &path,
            &mut |p| progress.push(p),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(&error, ExportError::InsertRate(_)), "{error}");
        assert!(error.to_string().contains("bitcrusher"), "{error}");
        assert!(progress.is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"old export");
        assert_eq!(fs::read(&stale).unwrap(), b"old partial");
        assert_eq!(list(), before);
        assert!(matches!(
            export_wav(
                routed_export_fixture("bitcrusher"),
                &options,
                &path,
                &mut |_| panic!("cancelled export reported progress"),
                &AtomicBool::new(true)
            ),
            Err(ExportError::Cancelled)
        ));
    }
}

#[test]
fn exports_accept_the_inclusive_endpoints_of_each_native_rate_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("export.wav");
    for (state, rates) in [
        (EngineState::default(), [8_000, 384_000]),
        (routed_export_fixture("utility"), [8_000, 384_000]),
        (routed_export_fixture("bitcrusher"), [22_050, 192_000]),
    ] {
        for sample_rate in rates {
            let options = ExportOptions {
                sample_rate,
                max_tail_seconds: 0.0,
                ..Default::default()
            };
            export_wav(
                state.clone(),
                &options,
                &path,
                &mut |_| {},
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                hound::WavReader::open(&path).unwrap().spec().sample_rate,
                sample_rate
            );
        }
    }
}
