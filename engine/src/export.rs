//! Offline WAV export: the pattern rendered by an engine of its own (never the live one, never
//! an audio device), faster than real time, then its tail until it falls silent.
//!
//! Determinism: the render is the same offline engine the golden tests use, and the file is
//! written with integer and basic IEEE arithmetic only, so one state gives the same bytes on
//! every platform.

use crate::leftover::{remove_stale_sibling, sibling};
use crate::{
    AudioProcessor, BLOCK, Command, EngineConfig, EngineState, engine_with, ticks_to_frames,
};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The largest 24-bit sample.
const PCM24_MAX: f64 = 8_388_607.0;
/// The smallest 24-bit sample.
const PCM24_MIN: f64 = -8_388_608.0;
/// A tail block whose peak is at most this is silent (-100 dBFS).
const SILENCE: f32 = 1e-5;
/// The tail ends after this much silence, in seconds.
const SILENCE_SECONDS: f64 = 0.1;
/// Frames rendered between progress reports and cancel checks.
const CHUNK: u64 = 4_096;

/// The sample format of an exported file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    /// 24-bit integer: `round(x * 8_388_607)`, clamped to the 24-bit range.
    Pcm24,
    /// 32-bit float: exactly what the engine rendered, nothing clipped.
    Float32,
}

/// What to export and how.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportOptions {
    /// Frames per second of the render and the file (the engine's range: 8 kHz..=384 kHz).
    pub sample_rate: u32,
    pub format: WavFormat,
    /// How many times the pattern plays (0 counts as 1).
    pub loops: u32,
    /// The longest tail after the last loop, in seconds.
    pub max_tail_seconds: f32,
}

impl Default for ExportOptions {
    /// 48 kHz, 24-bit, one loop, at most a 10 s tail.
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            format: WavFormat::Pcm24,
            loops: 1,
            max_tail_seconds: 10.0,
        }
    }
}

/// What an export wrote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportReport {
    /// Stereo frames in the file.
    pub frames: u64,
    /// The largest absolute sample the engine rendered (before any clipping).
    pub peak: f32,
    /// Samples (left and right counted apart) above full scale: clipped in a 24-bit file, kept
    /// as they are in a float one.
    pub clipped: u64,
}

/// Why an export did not finish. Either way no file (and no partial file) is left behind, and a
/// file that was already at the path is untouched.
#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    Cancelled,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "The WAV file could not be written: {e}."),
            Self::Cancelled => write!(f, "The export was cancelled."),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Cancelled => None,
        }
    }
}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<hound::Error> for ExportError {
    fn from(e: hound::Error) -> Self {
        match e {
            hound::Error::IoError(e) => Self::Io(e),
            other => Self::Io(io::Error::other(other)),
        }
    }
}

/// Renders `state`'s pattern `options.loops` times and writes it to `path` as a stereo WAV.
///
/// An engine of its own at `options.sample_rate` plays the pattern from frame 0
/// ([`Command::PlayLoops`]) for `ticks_to_frames(loops * length)` frames. The tail follows in
/// 64-frame blocks until 0.1 s of blocks in a row peak at most 1e-5, or `max_tail_seconds`
/// have passed; the silent blocks at the end are cut. The file is written to `<path>.part`,
/// synced, and renamed to `path` when complete; on an error or a cancel the `.part` file is
/// deleted.
///
/// A `.part` file already there is never truncated: one older than ten minutes is a crash's
/// leftover and is removed first, a younger one may be another export still writing it, and the
/// export then fails with `AlreadyExists` (and leaves that file alone).
///
/// `progress` gets 0..1 over the pattern part (never decreasing) and 1.0 once, when the file is
/// complete. `cancel` is checked between chunks of 4 096 frames.
///
/// # Panics
///
/// If `state` is one [`crate::Controller::load_state`] would refuse (see [`engine_with`]).
pub fn export_wav(
    state: EngineState,
    options: &ExportOptions,
    path: &Path,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<ExportReport, ExportError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    let part = part_path(path);
    // Best effort: if it cannot be removed, creating the part file below says so.
    let _ = remove_stale_sibling(path, ".part");
    let file = create_part(&part)?;
    // From here the part file is ours, and goes on any error.
    let result = write_part(state, options, &file, progress, cancel)
        .and_then(|report| {
            // On disk before it takes the final name: a power cut must not leave a complete
            // name on a file whose bytes never landed.
            file.sync_all()?;
            Ok(report)
        })
        .and_then(|report| fs::rename(&part, path).map(|()| report).map_err(Into::into));
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// `<path>.part`: the whole file name with `.part` added, next to the target.
fn part_path(path: &Path) -> PathBuf {
    sibling(path, ".part")
}

/// Creates the part file, failing if the name is taken.
fn create_part(part: &Path) -> Result<File, ExportError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(part)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                ExportError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} is already there: another export may be writing it. Try again in a few minutes",
                        part.display()
                    ),
                ))
            } else {
                e.into()
            }
        })
}

/// Renders and writes the whole file to `part` (the caller syncs it).
fn write_part(
    state: EngineState,
    options: &ExportOptions,
    part: &File,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<ExportReport, ExportError> {
    let config = EngineConfig {
        sample_rate: options.sample_rate as f32,
    };
    // The rate the engine really runs at, which the file must say.
    let rate = config.rate();
    let (mut controller, mut processor) = engine_with(config, state);
    let loops = options.loops.max(1);
    controller
        .send(Command::PlayLoops { loops })
        .map_err(|_| io::Error::other("the engine refused to play"))?;
    let (bpm, length) = (controller.state().bpm, controller.state().pattern.length);
    let pattern_frames = ticks_to_frames(f64::from(loops) * f64::from(length), bpm, rate);
    let max_tail = f64::from(options.max_tail_seconds);
    let max_tail = if max_tail.is_finite() && max_tail > 0.0 {
        (max_tail * f64::from(rate)).ceil() as u64
    } else {
        0
    };
    let silent_blocks = (SILENCE_SECONDS * f64::from(rate) / BLOCK as f64).ceil() as usize;

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate as u32,
        bits_per_sample: match options.format {
            WavFormat::Pcm24 => 24,
            WavFormat::Float32 => 32,
        },
        sample_format: match options.format {
            WavFormat::Pcm24 => hound::SampleFormat::Int,
            WavFormat::Float32 => hound::SampleFormat::Float,
        },
    };
    let mut out = Output {
        writer: hound::WavWriter::new(BufWriter::new(part), spec)?,
        format: options.format,
        report: ExportReport {
            frames: 0,
            peak: 0.0,
            clipped: 0,
        },
    };

    // The pattern part.
    let mut buf = vec![0.0f32; CHUNK as usize * 2];
    let mut done = 0u64;
    progress(0.0);
    while done < pattern_frames {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = (pattern_frames - done).min(CHUNK) as usize;
        render(&mut processor, &mut buf[..n * 2]);
        out.write(&buf[..n * 2])?;
        done += n as u64;
        // 1.0 is kept for the end, after the tail and the file are done.
        if done < pattern_frames {
            progress((done as f64 / pattern_frames as f64) as f32);
        }
    }

    // The tail: whole blocks, holding back silent ones until something louder follows.
    let mut held: Vec<f32> = Vec::with_capacity(silent_blocks * BLOCK * 2);
    let mut block = [0.0f32; BLOCK * 2];
    let mut tail = 0u64;
    let mut quiet = 0usize;
    while tail < max_tail && quiet < silent_blocks {
        if tail.is_multiple_of(CHUNK) && cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        render(&mut processor, &mut block);
        tail += BLOCK as u64;
        if block.iter().all(|x| x.abs() <= SILENCE) {
            held.extend_from_slice(&block);
            quiet += 1;
        } else {
            out.write(&held)?;
            out.write(&block)?;
            held.clear();
            quiet = 0;
        }
    }

    out.writer.finalize()?;
    progress(1.0);
    Ok(out.report)
}

/// Renders `out.len() / 2` frames of interleaved stereo.
fn render(processor: &mut AudioProcessor, out: &mut [f32]) {
    processor.process(out, 2);
}

/// The WAV being written and what has gone into it.
struct Output<'a> {
    writer: hound::WavWriter<BufWriter<&'a File>>,
    format: WavFormat,
    report: ExportReport,
}

impl Output<'_> {
    /// Appends interleaved stereo samples.
    fn write(&mut self, samples: &[f32]) -> Result<(), hound::Error> {
        for &x in samples {
            let level = x.abs();
            // A NaN never reaches here (the engine's guard), and would compare false anyway.
            if level > self.report.peak {
                self.report.peak = level;
            }
            if level > 1.0 {
                self.report.clipped += 1;
            }
            match self.format {
                WavFormat::Float32 => self.writer.write_sample(x)?,
                WavFormat::Pcm24 => self.writer.write_sample(pcm24(x))?,
            }
        }
        self.report.frames += (samples.len() / 2) as u64;
        Ok(())
    }
}

/// `round(x * 8_388_607)`, clamped to the 24-bit range (half away from zero; NaN gives 0).
fn pcm24(x: f32) -> i32 {
    // f64 holds the product exactly, so the only rounding is `round`'s own.
    (f64::from(x) * PCM24_MAX)
        .round()
        .clamp(PCM24_MIN, PCM24_MAX) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm24_rounds_half_away_from_zero_and_clamps() {
        assert_eq!(pcm24(0.0), 0);
        assert_eq!(pcm24(1.0), 8_388_607);
        assert_eq!(pcm24(-1.0), -8_388_607);
        assert_eq!(pcm24(4.0), 8_388_607);
        assert_eq!(pcm24(-4.0), -8_388_608);
        // 0.5 * 8_388_607 = 4_194_303.5: rounds up, never truncates.
        assert_eq!(pcm24(0.5), 4_194_304);
        assert_eq!(pcm24(-0.5), -4_194_304);
        assert_eq!(pcm24(f32::NAN), 0);
    }

    #[test]
    fn the_part_file_sits_next_to_the_target() {
        assert_eq!(
            part_path(Path::new("out/beat.wav")),
            Path::new("out/beat.wav.part")
        );
    }
}
