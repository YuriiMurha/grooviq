//! Sample files: decodes WAV, AIFF, FLAC, MP3 and OGG Vorbis into a [`SampleBuf`] for the
//! sampler. Runs on a control thread, never the audio thread.
//!
//! Projects embed each sample's original file bytes, so decoding works from memory
//! ([`decode_sample`]); [`load_sample`] reads a file and decodes it the same way.

use grooviq_dsp::sampler::SampleBuf;
use std::fmt;
use std::io::{Cursor, ErrorKind};
use std::path::Path;
use symphonia::core::checksum::Crc32;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{
    MediaSource, MediaSourceStream, MediaSourceStreamOptions, Monitor, ReadOnlySource,
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
    /// Longer than the limit; `limit` is the limit that was applied, in seconds.
    TooLong { seconds: f32, limit: f32 },
    /// A WAV whose header was never filled in (the recorder stopped before finishing it) and
    /// that holds no sound after it.
    Unfinished,
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
            Self::TooLong { seconds, limit } => write!(
                f,
                "This sample is {} long; samples can be up to {}.",
                clock(*seconds),
                limit_text(*limit)
            ),
            Self::Unfinished => write!(
                f,
                "This WAV file was not finished by the program that recorded it."
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

/// A length limit in words: "10 minutes" for whole minutes, otherwise `m:ss`.
fn limit_text(seconds: f32) -> String {
    let minutes = seconds / 60.0;
    if seconds.is_finite() && minutes >= 1.0 && minutes.fract() == 0.0 {
        format!("{minutes} minute{}", if minutes == 1.0 { "" } else { "s" })
    } else {
        clock(seconds)
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
    // A WAV whose recorder never came back to fill in its header sizes: read to the file's end.
    let header = unfinished_wav_header(bytes);
    let unfinished = header.is_some();
    let wav_fixed;
    let bytes = match header {
        Some(UnfinishedWav::NoSound) => return Err(SampleError::Unfinished),
        Some(UnfinishedWav::Sound(fixed)) => {
            wav_fixed = fixed;
            wav_fixed.as_slice()
        }
        None => bytes,
    };
    let eof = |e: &Error| matches!(e, Error::IoError(io) if io.kind() == ErrorKind::UnexpectedEof);
    let opened = open_any(bytes, &hint);
    // An Ogg page that fails its checksum next to the headers, or whose length field runs past
    // the end of the file, stops the reader from opening the file at all, though the pages after
    // it are fine: open it again without them.
    let ogg_fixed;
    let opened = match opened {
        Err(Error::DecodeError(_) | Error::IoError(_)) if bytes.starts_with(b"OggS") => {
            match without_damaged_ogg_pages(bytes) {
                Some(fixed) => {
                    ogg_fixed = fixed;
                    open_any(&ogg_fixed, &hint)
                }
                None => opened,
            }
        }
        opened => opened,
    };
    let mut format = opened.map_err(|e| match e {
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
            // A WAV's header sizes were filled in by us: no frames there means no sound written.
            return Err(if unfinished {
                SampleError::Unfinished
            } else {
                SampleError::Empty
            });
        }
        let seconds = frames as f64 / r as f64;
        if seconds > max_seconds as f64 {
            return Err(SampleError::TooLong {
                seconds: seconds as f32,
                limit: max_seconds,
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
                limit: max_seconds,
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
    // The vectors grew by doubling; hand back exactly what was decoded.
    l.shrink_to_fit();
    r.shrink_to_fit();
    Ok(SampleBuf {
        rate: rate as f32,
        l,
        r,
    })
}

/// Opens `bytes` as a seekable source and, if that hits the end of the data, as a stream.
fn open_any<'a>(bytes: &'a [u8], hint: &Hint) -> Result<Box<dyn FormatReader + 'a>, Error> {
    match open(bytes, hint, true) {
        Err(Error::IoError(io)) if io.kind() == ErrorKind::UnexpectedEof => {
            open(bytes, hint, false)
        }
        opened => opened,
    }
}

/// What an unfinished WAV header turned out to hold.
enum UnfinishedWav {
    /// Nothing follows the header.
    NoSound,
    /// A copy of the file with the RIFF and data sizes filled in for what follows the header.
    Sound(Vec<u8>),
}

/// A WAV recorder that stops before it can come back to fill in its header leaves a RIFF size
/// of 0 (never filled in), 0xFFFF_FFFF (streamed) or one that does not match the file, and a
/// data chunk size of 0 or 0xFFFF_FFFF. Other tools read such a file to its end; this returns the file with
/// the sizes made true so the reader does the same. `None` when this is not a WAV, or its header
/// is finished: a writer that backpatched the RIFF size also backpatched the data size, so a
/// data size of 0 there is an empty recording, whatever metadata follows it.
fn unfinished_wav_header(bytes: &[u8]) -> Option<UnfinishedWav> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    // Finished: the RIFF size is the file length minus 8, give or take a pad byte (one too many
    // written after the last chunk, or the last chunk's pad counted but not written). 0,
    // 0xFFFF_FFFF (streamed) and anything else further off is an unfinished header.
    let off = bytes.len() as i64 - 8 - i64::from(riff_size);
    let riff_unfinished = !(-1..=1).contains(&off);
    let mut at = 12usize;
    while let Some(chunk) = bytes.get(at..at + 8) {
        let size = u32::from_le_bytes(chunk[4..8].try_into().ok()?);
        if &chunk[..4] != b"data" {
            // Chunks are padded to an even size.
            at = at.checked_add(8 + size as usize + (size as usize & 1))?;
            continue;
        }
        let sound = at + 8;
        let after = bytes.len() - sound;
        return match size {
            0 if !riff_unfinished => None,
            0 | u32::MAX if after == 0 => Some(UnfinishedWav::NoSound),
            0 | u32::MAX => {
                let mut fixed = bytes.to_vec();
                let end = |n: usize| u32::try_from(n).unwrap_or(u32::MAX - 1).min(u32::MAX - 1);
                fixed[4..8].copy_from_slice(&end(bytes.len() - 8).to_le_bytes());
                fixed[at + 4..at + 8].copy_from_slice(&end(after).to_le_bytes());
                Some(UnfinishedWav::Sound(fixed))
            }
            _ => None,
        };
    }
    None
}

/// `bytes` (an Ogg stream) with the pages whose checksum fails removed, or `None` when every
/// page is sound or the repair would cost too much (see [`scan_ogg`]).
fn without_damaged_ogg_pages(bytes: &[u8]) -> Option<Vec<u8>> {
    scan_ogg(bytes, bytes.len().saturating_mul(4)).repaired
}

/// The result of [`scan_ogg`].
struct OggScan {
    /// The stream without its damaged pages; `None` if nothing was damaged, or the scan gave up.
    repaired: Option<Vec<u8>>,
    /// Bytes checksummed (the tests pin the cost on it).
    #[cfg_attr(not(test), allow(dead_code))]
    hashed: usize,
}

/// Walks `bytes` page by page, keeping the pages whose checksum holds. Anything else (a damaged
/// page, bytes between pages, a capture pattern that is not a page) is dropped. Every length
/// field here is untrusted: a candidate whose table or body runs past the end of the data is
/// skipped like any other false match, and kept (as a cut-off tail) only if no sound page
/// follows it. A page whose checksum fails resumes the search just after its capture pattern,
/// since the damage may be in its length fields. That could rehash the same bytes for every
/// false match, so the scan gives up (`repaired: None`) after `max_hashed` bytes.
fn scan_ogg(bytes: &[u8], max_hashed: usize) -> OggScan {
    const HEADER: usize = 27;
    let mut out = Vec::with_capacity(bytes.len());
    let (mut at, mut dropped, mut hashed) = (0usize, false, 0usize);
    // Where the first candidate that ran past the end of the data began.
    let mut tail: Option<usize> = None;
    while at < bytes.len() {
        let Some(page_at) = bytes[at..].windows(4).position(|w| w == b"OggS") else {
            dropped = true;
            break;
        };
        dropped |= page_at > 0;
        at += page_at;
        let page = bytes
            .get(at + HEADER - 1)
            .map(|&n| at + HEADER..at + HEADER + n as usize)
            .and_then(|table| {
                let lacing = bytes.get(table.clone())?;
                let end = table.end + lacing.iter().map(|&s| s as usize).sum::<usize>();
                bytes.get(at..end)
            });
        let Some(page) = page else {
            tail.get_or_insert(at);
            at += 4;
            continue;
        };
        if hashed.saturating_add(page.len()) > max_hashed {
            return OggScan {
                repaired: None,
                hashed,
            };
        }
        hashed += page.len();
        // The checksum is taken with its own four bytes zeroed.
        let mut crc = Crc32::new(0);
        crc.process_buf_bytes(&page[..22]);
        crc.process_buf_bytes(&[0; 4]);
        crc.process_buf_bytes(&page[26..]);
        if crc.crc().to_le_bytes() == page[22..26] {
            // A sound page after an overrunning candidate: that candidate was false.
            dropped |= tail.take().is_some();
            out.extend_from_slice(page);
            at += page.len();
        } else {
            dropped = true;
            at += 4;
        }
    }
    if let Some(tail) = tail {
        out.extend_from_slice(&bytes[tail..]);
    }
    OggScan {
        repaired: dropped.then_some(out),
        hashed,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture patterns back to back: at every candidate the segment table and body that its
    /// bytes claim fit inside the file, so each one is checksummed in full and fails.
    fn dense_capture_patterns(len: usize) -> Vec<u8> {
        b"OggS".iter().copied().cycle().take(len).collect()
    }

    #[test]
    fn the_ogg_repair_scan_checksums_at_most_four_times_the_input() {
        let bytes = dense_capture_patterns(1 << 20);
        let cap = bytes.len() * 4;
        let scan = scan_ogg(&bytes, cap);
        assert!(scan.repaired.is_none(), "gives up rather than repair");
        assert!(scan.hashed <= cap, "{} bytes checksummed", scan.hashed);
        // It really was the cap that stopped it: unlimited, the same input costs far more.
        let all = scan_ogg(&bytes[..1 << 16], usize::MAX);
        assert!(all.hashed > 100 * (1 << 16), "{} bytes", all.hashed);
    }

    #[test]
    fn the_repair_budget_is_four_times_the_input() {
        // Through the entry point the decoder uses: 64 KiB of dense markers (about 150 MB of
        // checksumming, far past 4x the input) padded to 1 MiB. With a budget of 1000x the scan
        // would finish and return a repair.
        let mut bytes = dense_capture_patterns(1 << 16);
        bytes.resize(1 << 20, 0);
        assert!(without_damaged_ogg_pages(&bytes).is_none());
    }

    #[test]
    fn a_sound_ogg_stream_is_checksummed_once() {
        let src =
            std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/blip.ogg")).unwrap();
        let scan = scan_ogg(&src, src.len() * 4);
        assert!(scan.repaired.is_none());
        assert_eq!(scan.hashed, src.len());
    }
}
