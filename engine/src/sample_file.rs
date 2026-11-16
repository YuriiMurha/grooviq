//! Sample files: decodes WAV, AIFF, FLAC, MP3 and OGG Vorbis into a [`SampleBuf`] for the
//! sampler. Runs on a control thread, never the audio thread.
//!
//! Projects embed each sample's original file bytes, so decoding works from memory
//! ([`decode_sample`]); [`load_sample`] reads a file and decodes it the same way.

use grooviq_dsp::sampler::SampleBuf;
use std::fmt;
use std::io::{Cursor, ErrorKind};
use std::path::Path;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{
    MediaSource, MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource,
};
use symphonia::core::meta::MetadataOptions;

/// The longest sample [`load_sample`] accepts, in seconds.
pub const MAX_SAMPLE_SECONDS: f32 = 600.0;

/// Sample rates a sample may have (the engine's own range).
const RATE_RANGE: std::ops::RangeInclusive<u32> = 8_000..=384_000;

/// Why a sample could not be loaded. `Display` gives one sentence for the user.
#[derive(Clone, Debug, PartialEq)]
pub enum SampleError {
    /// No file at that path.
    NotFound,
    /// The file exists but could not be read (permissions, a folder, a disk error).
    Unreadable(String),
    /// Not a sound format Grooviq decodes.
    Unsupported,
    /// A known format, but damaged.
    Corrupt(String),
    /// Decodes to no frames.
    Empty,
    /// Longer than the limit.
    TooLong { seconds: f32 },
    /// A sample rate outside 8 kHz..=384 kHz.
    BadRate(u32),
}

impl fmt::Display for SampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "This sample file could not be found."),
            Self::Unreadable(why) => write!(f, "This sample file could not be read ({why})."),
            Self::Unsupported => write!(
                f,
                "This file is not a sound Grooviq can open; use WAV, AIFF, FLAC, MP3 or OGG."
            ),
            Self::Corrupt(why) => write!(f, "This sample file is damaged ({why})."),
            Self::Empty => write!(f, "This sample file has no sound in it."),
            Self::TooLong { seconds } => write!(
                f,
                "This sample is {} long; samples can be up to {} minutes.",
                clock(*seconds),
                MAX_SAMPLE_SECONDS / 60.0
            ),
            Self::BadRate(rate) => write!(
                f,
                "This sample's rate of {rate} Hz is not supported; samples can be 8 kHz to 384 kHz."
            ),
        }
    }
}

impl std::error::Error for SampleError {}

/// `m:ss` (or `h:mm:ss`), rounded up to the next whole second.
fn clock(seconds: f32) -> String {
    let s = if seconds.is_finite() {
        seconds.max(0.0).ceil() as u64
    } else {
        0
    };
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Reads and decodes the sample file at `path`, up to [`MAX_SAMPLE_SECONDS`] long.
pub fn load_sample(path: &Path) -> Result<SampleBuf, SampleError> {
    load_sample_with_limit(path, MAX_SAMPLE_SECONDS)
}

/// Reads and decodes the sample file at `path`, up to `max_seconds` long.
pub fn load_sample_with_limit(path: &Path, max_seconds: f32) -> Result<SampleBuf, SampleError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => SampleError::NotFound,
        _ => SampleError::Unreadable(e.to_string()),
    })?;
    let extension = path.extension().and_then(|e| e.to_str());
    decode_sample(&bytes, extension, max_seconds)
}

/// Decodes a sample file held in memory. The format is detected from the content; `extension`
/// (without the dot) is only a hint. Mono is copied to both channels; channels past the second
/// are dropped. Undecodable packets are skipped; a stream that ends early ends the sample.
pub fn decode_sample(
    bytes: &[u8],
    extension: Option<&str>,
    max_seconds: f32,
) -> Result<SampleBuf, SampleError> {
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(&ext.to_ascii_lowercase());
    }
    let eof = |e: &Error| matches!(e, Error::IoError(io) if io.kind() == ErrorKind::UnexpectedEof);
    // Readers that may seek scan the whole file while opening (for trailing chunks, the last
    // page), so a cut-off file fails there. Opened as a stream, the same reader stops at the
    // sound data, and the part before the cut still plays.
    let mut format = match open(bytes, &hint, true) {
        Err(e) if eof(&e) => open(bytes, &hint, false),
        opened => opened,
    }
    .map_err(|e| match e {
        // No container marker anywhere: not a sound file.
        Error::Unsupported(_) => SampleError::Unsupported,
        // A container was recognised, but the file ends inside its header.
        e if eof(&e) => SampleError::Corrupt("it ends before its sound starts".into()),
        e => SampleError::Corrupt(e.to_string()),
    })?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or(SampleError::Unsupported)?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else {
        return Err(SampleError::Unsupported);
    };
    // Decide what the header can before decoding anything.
    let mut rate = params.sample_rate;
    if let Some(r) = rate
        && !RATE_RANGE.contains(&r)
    {
        return Err(SampleError::BadRate(r));
    }
    if let (Some(r), Some(frames)) = (rate, track.num_frames) {
        if frames == 0 {
            return Err(SampleError::Empty);
        }
        let seconds = frames as f64 / r as f64;
        if seconds > max_seconds as f64 {
            return Err(SampleError::TooLong {
                seconds: seconds as f32,
            });
        }
    }

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|_| SampleError::Unsupported)?;

    let (mut l, mut r) = (Vec::new(), Vec::new());
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut decoded = 0usize;
    let mut failed = 0usize;
    let mut last_error = String::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A truncated file ends where its data ends.
            Err(e) if eof(&e) => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(SampleError::Corrupt(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(Error::DecodeError(e)) => {
                failed += 1;
                last_error = e.to_string();
                continue;
            }
            Err(Error::IoError(e)) => {
                failed += 1;
                last_error = e.to_string();
                continue;
            }
            Err(e) => return Err(SampleError::Corrupt(e.to_string())),
        };
        decoded += 1;
        let spec_rate = buf.spec().rate();
        match rate {
            None if !RATE_RANGE.contains(&spec_rate) => {
                return Err(SampleError::BadRate(spec_rate));
            }
            None => rate = Some(spec_rate),
            Some(_) => {}
        }
        buf.copy_to_vecs_planar::<f32>(&mut planes);
        match planes.as_slice() {
            [] => {}
            [mono] => {
                l.extend_from_slice(mono);
                r.extend_from_slice(mono);
            }
            [left, right, ..] => {
                l.extend_from_slice(left);
                r.extend_from_slice(right);
            }
        }
        // Formats without a frame count in the header: stop as soon as it is too long.
        let r_hz = rate.unwrap_or(spec_rate) as f64;
        if l.len() as f64 / r_hz > max_seconds as f64 {
            let rest: u64 = remaining_frames(&mut *format, track_id);
            return Err(SampleError::TooLong {
                seconds: ((l.len() as u64 + rest) as f64 / r_hz) as f32,
            });
        }
    }

    if decoded == 0 && failed > 0 {
        return Err(SampleError::Corrupt(last_error));
    }
    let Some(rate) = rate else {
        return Err(SampleError::Empty);
    };
    if l.is_empty() {
        return Err(SampleError::Empty);
    }
    Ok(SampleBuf {
        rate: rate as f32,
        l,
        r,
    })
}

/// Opens a container reader over `bytes`, as a seekable source or as a plain stream.
fn open<'a>(
    bytes: &'a [u8],
    hint: &Hint,
    seekable: bool,
) -> Result<Box<dyn FormatReader + 'a>, Error> {
    let source: Box<dyn MediaSource + 'a> = if seekable {
        Box::new(Cursor::new(bytes))
    } else {
        Box::new(ReadOnlySource::new(bytes))
    };
    let stream = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
    symphonia::default::get_probe().probe(
        hint,
        stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    )
}

/// Sums the durations of the packets left in the stream (without decoding them), so a too-long
/// error can say how long the whole sample is.
fn remaining_frames(format: &mut dyn FormatReader, track: u32) -> u64 {
    let mut frames = 0u64;
    while let Ok(Some(p)) = format.next_packet() {
        if p.track_id == track {
            frames = frames.saturating_add(p.dur.get());
        }
    }
    frames
}
