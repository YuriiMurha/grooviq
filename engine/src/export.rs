//! Offline WAV export: the pattern rendered by an engine of its own (never the live one, never
//! an audio device), faster than real time, then its tail until it falls silent.
//!
//! Determinism: the render is the same offline engine the golden tests use, and the file is
//! written with integer and basic IEEE arithmetic only, so one state gives the same bytes on
//! every platform.

use crate::leftover::{create_owned_temp, remove_stale_owned};
use crate::{
    AudioProcessor, BLOCK, Command, EngineConfig, EngineState, engine_with, ticks_to_frames,
};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::Path;
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
    /// Song region to render once; None renders the whole song, ignoring transport looping.
    pub song_region: Option<crate::SongLoop>,
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
            song_region: None,
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
/// ([`Command::PlayLoops`], the only command it gets: the metronome and recording stay off)
/// for `ticks_to_frames(loops * length)` frames. The tail follows in
/// 64-frame blocks until 0.1 s of blocks in a row peak at most 1e-5, or `max_tail_seconds`
/// have passed; the silent blocks at the end are cut. The file is written to a hidden part
/// file of its own beside `path` (`.<name>.grooviq-<pid>-<random>.part`, created new and locked
/// while this export runs), synced, and renamed to `path` when complete; on an error or a
/// cancel that part file is deleted.
///
/// Only part files of this naming, ten minutes old and whose writer is gone (nobody holds the
/// lock) are removed at the start; a file named `<path>.part` or anything else is never touched.
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
    export_wav_with(state, options, path, progress, cancel, &RealFiles)
}

/// The two file operations that make an export durable, plus a tail checkpoint, as a seam for
/// tests: the real ones ([`RealFiles`]) sync to disk and rename; a test's record the order they
/// are called in, or fail, or hold the export at the checkpoint. Not part of the API.
#[doc(hidden)]
pub trait PartFiles {
    /// Puts the part file's bytes on disk (`path` is its name).
    fn sync(&self, file: &File, path: &Path) -> io::Result<()>;
    /// Gives the part file its final name.
    fn rename(&self, part: &Path, to: &Path) -> io::Result<()>;
    /// Called in the tail right before each cancel check, with the tail frames rendered so far
    /// (a multiple of 4 096). A test blocks here until it has set `cancel`, so the check that
    /// follows sees it: no race with the renderer. Does nothing by default.
    fn tail_checkpoint(&self, _tail_frames: u64) {}
}

/// What the app uses.
#[doc(hidden)]
pub struct RealFiles;

impl PartFiles for RealFiles {
    fn sync(&self, file: &File, _path: &Path) -> io::Result<()> {
        file.sync_all()
    }

    fn rename(&self, part: &Path, to: &Path) -> io::Result<()> {
        fs::rename(part, to)
    }
}

/// [`export_wav`] through `files`: the test seam. Not part of the API.
#[doc(hidden)]
pub fn export_wav_with(
    state: EngineState,
    options: &ExportOptions,
    path: &Path,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
    files: &dyn PartFiles,
) -> Result<ExportReport, ExportError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportError::Cancelled);
    }
    // Leftovers of writers that died, never anything else (see `leftover`); best effort.
    let _ = remove_stale_owned(path, ".part");
    // A name of our own, locked while this export writes: no other export, and no cleanup, can
    // take it, and an old file of the same name in the folder is none of our business.
    let part = create_owned_temp(path, ".part")?;
    // From here the part file is ours, and goes on any error.
    let result = write_part(state, options, &part.file, progress, cancel, files)
        .and_then(|report| {
            // On disk before it takes the final name: a power cut must not leave a complete
            // name on a file whose bytes never landed.
            files.sync(&part.file, &part.path)?;
            Ok(report)
        })
        .and_then(|report| {
            files
                .rename(&part.path, path)
                .map(|()| report)
                .map_err(Into::into)
        });
    if result.is_err() {
        let _ = fs::remove_file(&part.path);
    }
    result
}

/// Renders and writes the whole file to `part` (the caller syncs it).
fn write_part(
    state: EngineState,
    options: &ExportOptions,
    part: &File,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
    files: &dyn PartFiles,
) -> Result<ExportReport, ExportError> {
    let config = EngineConfig {
        sample_rate: options.sample_rate as f32,
    };
    // The rate the engine really runs at, which the file must say.
    let rate = config.rate();
    let mut state = state;
    let is_song = state.playback_mode == crate::PlaybackMode::Song;
    let from = if is_song {
        options.song_region.map_or(0, |r| r.start_tick)
    } else {
        0
    };
    if is_song {
        if !state.song.valid_loop(options.song_region) {
            return Err(io::Error::other("invalid song export region").into());
        }
        state.song_loop = None;
    }
    let song_end = options
        .song_region
        .map_or(state.song.end_tick, |r| r.end_tick);
    let exact_end = if options.song_region.is_none() {
        state
            .song
            .end_frame_at(rate)
            .max(state.song.audio.end_frame(state.bpm, rate))
    } else {
        0
    };
    if is_song && options.song_region.is_some() {
        state.song.arrangement.notes.retain(|n| n.tick < song_end);
        for n in &mut state.song.arrangement.notes {
            n.length = n.length.min(song_end - n.tick);
        }
        state.song.arrangement.length = state.song.arrangement.length.min(song_end);
        state.song.end_tick = song_end;
        state.song.end_frame = 0;
    }
    let (mut controller, mut processor) = engine_with(config, state);
    let loops = options.loops.max(1);
    let command = if is_song {
        Command::Play
    } else {
        Command::PlayLoops { loops }
    };
    controller
        .send(command)
        .map_err(|_| io::Error::other("the engine refused to play"))?;
    if is_song && from > 0 {
        controller
            .send(Command::Seek { tick: from })
            .map_err(|_| io::Error::other("the engine refused to seek"))?;
    }
    let (bpm, length) = (controller.state().bpm, controller.state().pattern.length);
    let pattern_frames = if is_song {
        if options.song_region.is_some() {
            ticks_to_frames((song_end - from) as f64, bpm, rate)
        } else {
            ticks_to_frames(song_end as f64, bpm, rate).max(exact_end)
        }
    } else {
        ticks_to_frames(f64::from(loops) * f64::from(length), bpm, rate)
    };
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
        if tail.is_multiple_of(CHUNK) {
            files.tail_checkpoint(tail);
            if cancel.load(Ordering::Relaxed) {
                return Err(ExportError::Cancelled);
            }
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
}
