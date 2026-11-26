//! Speed of the real engine on reference scenes, for `cargo bench -p grooviq-golden --bench
//! render`: offline render speed (times real time) and how long each `process` call takes
//! against its deadline. Every run also hashes what it rendered, so a run whose bits depend on
//! the block size or differ between repetitions is caught (determinism).

use std::time::Instant;

use grooviq_dsp::synth::preset;
use grooviq_engine::{
    Command, EngineConfig, EngineState, NoteId, PlaybackMode, STEP_TICKS, engine_with,
};
use grooviq_project::{SampleFiles, engine_state, session::Session};

use crate::{SAMPLE_RATE, decode_samples, drums_4ch_project, kit_dir};

pub mod export_budget;
pub mod report;

/// A scene to time: an engine state, and the commands sent before the first block.
pub struct PerfScene {
    pub name: &'static str,
    pub state: EngineState,
    pub start: Vec<Command>,
}

impl PerfScene {
    /// The start commands as `render_offline_with` events, all at frame 0.
    pub fn events(&self) -> Vec<(u64, Command)> {
        self.start.iter().map(|&cmd| (0, cmd)).collect()
    }
}

/// Every scene, smallest first. They play the transport in a loop, so any length works.
///
/// * `drums_4ch`: the M1 acceptance project.
/// * `beat_16ch`: the native-device part of spec 9's reference project, as far as the engine
///   goes today: 16 sampler channels (the kit four times, retuned, panned, each copy's pattern
///   shifted by a step) and a held four-note chord on the synth's "Dark Pad" preset (its
///   heaviest polyphonic one: 7 + 5 unison oscillators per voice).
pub fn perf_scenes() -> Vec<PerfScene> {
    let (project, files) = drums_4ch_project(&kit_dir());
    let drums = PerfScene {
        name: "drums_4ch",
        state: engine_state(&project, &decode_samples(&files)).unwrap(),
        start: vec![Command::Play],
    };

    let mut big = project.clone();
    big.channels.clear();
    big.patterns[0].notes.clear();
    let mut samples = SampleFiles::new();
    let pattern_ticks = big.patterns[0].steps * STEP_TICKS;
    for copy in 0..4u32 {
        let tune = [0.0, -5.0, 7.0, 12.0][copy as usize];
        for channel in &project.channels {
            let mut c = channel.clone();
            c.id = copy * 4 + channel.id;
            c.name = format!("{} {}", channel.name, copy + 1);
            *c.sampler.entry("tune".into()).or_insert(0.0) += tune;
            c.pan = (c.pan + 0.3 * (copy as f32 - 1.5)).clamp(-1.0, 1.0);
            samples.insert(c.id, files[&channel.id].clone());
            big.channels.push(c);
        }
        for note in &project.patterns[0].notes {
            let mut n = *note;
            n.channel += copy * 4;
            n.tick = (n.tick + copy * STEP_TICKS) % pattern_ticks;
            big.patterns[0].notes.push(n);
        }
    }
    big.next_id = 17;
    let mut start: Vec<Command> = preset("Dark Pad")
        .expect("the Dark Pad preset exists")
        .into_iter()
        .map(|(index, value)| Command::SetSynthParam { index, value })
        .collect();
    start.extend([36, 48, 55, 63].map(|key| Command::Note {
        key,
        velocity: 0.8,
        on: true,
        id: NoteId(u64::from(key)),
    }));
    start.push(Command::Play);
    let beat = PerfScene {
        name: "beat_16ch",
        state: engine_state(&big, &decode_samples(&samples)).unwrap(),
        start,
    };
    vec![drums, beat]
}

/// The reopened M3 golden session, played through the engine's Song transport.
pub fn song_m3(session: &Session) -> PerfScene {
    let state = session.checked_export_state().unwrap();
    assert_eq!(state.playback_mode, PlaybackMode::Song);
    assert!(state.song_loop.is_none(), "expected a single Song pass");
    PerfScene {
        name: "song_m3",
        state,
        start: vec![Command::Play],
    }
}

/// What [`measure`] saw for one scene at one block size.
pub struct Measurement {
    pub scene: &'static str,
    /// Frames per `process` call.
    pub block: usize,
    /// Frames each repetition rendered.
    pub frames: usize,
    /// Nanoseconds of every `process` call of every timed repetition.
    pub block_ns: Vec<u64>,
    /// Times real time of each timed repetition.
    pub speed: Vec<f64>,
    /// [`hash`] of the output (the same in every repetition).
    pub hash: u64,
}

/// Renders `scene` for `frames` frames (a multiple of `block`) in `block`-frame `process`
/// calls, once untimed to warm up and then `reps` times timed. Only the `process` calls are
/// timed, and nothing in the loop allocates.
///
/// # Errors
///
/// If two repetitions rendered different bits.
///
/// # Panics
///
/// If `frames` is not a whole number of blocks, or the start commands do not fit the queue.
pub fn measure(
    scene: &PerfScene,
    block: usize,
    frames: usize,
    reps: usize,
) -> Result<Measurement, String> {
    measure_with_timed_loop(scene, block, frames, reps, |render| render())
}

/// Like [`measure`], with a caller-supplied runner for each timed repetition's block loop.
/// The runner calls the loop once and returns after it finishes.
///
/// # Errors
/// If two repetitions rendered different bits.
/// # Panics
/// If `frames` is not a whole number of blocks, or the start commands do not fit the queue.
pub fn measure_with_timed_loop(
    scene: &PerfScene,
    block: usize,
    frames: usize,
    reps: usize,
    mut run_timed: impl FnMut(&mut (dyn FnMut() + Send)),
) -> Result<Measurement, String> {
    assert!(
        block > 0 && frames.is_multiple_of(block),
        "{frames} frames is not a whole number of {block}-frame blocks"
    );
    let blocks = frames / block;
    let config = EngineConfig {
        sample_rate: SAMPLE_RATE as f32,
    };
    let mut buf = vec![0.0f32; block * 2];
    let mut block_ns = Vec::with_capacity(blocks * reps);
    let mut speed = Vec::with_capacity(reps);
    let mut first_hash = None;
    for rep in 0..=reps {
        let (mut controller, mut processor) = engine_with(config, scene.state.clone());
        for &cmd in &scene.start {
            controller
                .send(cmd)
                .expect("the start commands fit the queue");
        }
        let timed = rep > 0;
        let mut hash = Fnv::new();
        let mut total = 0u64;
        let mut render = || {
            for _ in 0..blocks {
                let t = Instant::now();
                processor.process(&mut buf, 2);
                let ns = u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX);
                total += ns;
                if timed {
                    block_ns.push(ns);
                }
                hash.write(&buf);
            }
        };
        if timed {
            run_timed(&mut render);
        } else {
            render();
        }
        // Frees whatever the processor handed back; nothing is expected while it plays.
        controller.check_health();
        let hash = hash.finish();
        match first_hash {
            None => first_hash = Some(hash),
            Some(first) if first != hash => {
                return Err(format!(
                    "{} at {block} frames: repetition {rep} rendered different bits \
                     ({hash:016x} vs {first:016x})",
                    scene.name
                ));
            }
            Some(_) => {}
        }
        if timed {
            let audio = frames as f64 / f64::from(SAMPLE_RATE);
            speed.push(audio / (total.max(1) as f64 * 1e-9));
        }
    }
    Ok(Measurement {
        scene: scene.name,
        block,
        frames,
        block_ns,
        speed,
        hash: first_hash.expect("at least one repetition"),
    })
}

/// The numbers a report shows for one [`Measurement`].
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub scene: &'static str,
    pub block: usize,
    /// The real-time budget of one block, in nanoseconds.
    pub deadline_ns: f64,
    /// Median and slowest repetition, in times real time.
    pub speed_median: f64,
    pub speed_worst: f64,
    /// Block times in nanoseconds.
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub max_ns: u64,
    pub preemption_count: usize,
    pub hash: u64,
}

pub const B2_TARGET_PCT: f64 = 50.0;

impl Summary {
    pub fn deadline_pct(&self, ns: u64) -> f64 {
        ns as f64 / self.deadline_ns * 100.0
    }

    pub fn under_b2_target(&self) -> Option<bool> {
        (self.block == 128).then(|| self.deadline_pct(self.max_ns) < B2_TARGET_PCT)
    }
}

impl Measurement {
    pub fn summary(&self) -> Summary {
        let mut times = self.block_ns.clone();
        times.sort_unstable();
        let mut speed = self.speed.clone();
        speed.sort_by(f64::total_cmp);
        let p50_ns = percentile(&times, 0.5);
        // Count blocks strictly above 10× p50 so a cluster of slow blocks won't raise a tail cutoff.
        let preemption_cutoff_ns = p50_ns.saturating_mul(10);
        let preemption_count =
            times.len() - times.partition_point(|&ns| ns <= preemption_cutoff_ns);
        Summary {
            scene: self.scene,
            block: self.block,
            deadline_ns: self.block as f64 / f64::from(SAMPLE_RATE) * 1e9,
            speed_median: median(&speed),
            speed_worst: speed.first().copied().unwrap_or(0.0),
            p50_ns,
            p99_ns: percentile(&times, 0.99),
            p999_ns: percentile(&times, 0.999),
            max_ns: times.last().copied().unwrap_or(0),
            preemption_count,
            hash: self.hash,
        }
    }
}

/// The median of `sorted` (ascending): the middle value, or the mean of the two middle ones;
/// 0 for an empty slice.
pub fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => sorted[n / 2],
        _ => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
    }
}

/// The nearest-rank `q` quantile (0..=1) of `sorted` (ascending); 0 for an empty slice.
pub fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (q.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// FNV-1a over the bits of `samples` (little-endian), for comparing renders.
pub fn hash(samples: &[f32]) -> u64 {
    let mut h = Fnv::new();
    h.write(samples);
    h.finish()
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, samples: &[f32]) {
        for s in samples {
            for byte in s.to_bits().to_le_bytes() {
                self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let v: Vec<u64> = (1..=1000).collect();
        assert_eq!(percentile(&v, 0.5), 500);
        assert_eq!(percentile(&v, 0.99), 990);
        assert_eq!(percentile(&v, 0.999), 999);
        assert_eq!(percentile(&v, 1.0), 1000);
        assert_eq!(percentile(&v, 0.0), 1);
        assert_eq!(percentile(&[7], 0.999), 7);
        assert_eq!(percentile(&[], 0.5), 0);
    }

    #[test]
    fn median_averages_the_middle_pair_of_an_even_count() {
        assert_eq!(median(&[10.0, 100.0]), 55.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(median(&[1.0, 2.0, 9.0]), 2.0);
        assert_eq!(median(&[7.0]), 7.0);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn the_hash_sees_every_bit() {
        assert_ne!(hash(&[0.0]), hash(&[-0.0]));
        assert_ne!(hash(&[1.0, 2.0]), hash(&[2.0, 1.0]));
        assert_eq!(hash(&[0.5, 0.25]), hash(&[0.5, 0.25]));
    }
}
