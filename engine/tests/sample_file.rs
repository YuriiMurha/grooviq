//! Sample-file decoding. WAV files are written here with `hound`; the `data/blip.*` fixtures are
//! 0.1 s of a 1 kHz sine at -6 dBFS, 44.1 kHz, our own work (CC0), made once with:
//!
//! ```sh
//! ffmpeg -f lavfi -i "aevalsrc=0.5*sin(2*PI*1000*t):s=44100:d=0.1" -map_metadata -1 -fflags +bitexact -flags:a +bitexact -c:a pcm_s16be blip.aif
//! ffmpeg -f lavfi -i "aevalsrc=0.5*sin(2*PI*1000*t):s=44100:d=0.1" -map_metadata -1 -fflags +bitexact -flags:a +bitexact -c:a flac -sample_fmt s16 -compression_level 12 blip.flac
//! ffmpeg -f lavfi -i "aevalsrc=0.5*sin(2*PI*1000*t):s=44100:d=0.1" -map_metadata -1 -fflags +bitexact -flags:a +bitexact -c:a libmp3lame -b:a 64k -id3v2_version 0 -write_id3v1 0 blip.mp3
//! ffmpeg -f lavfi -i "aevalsrc=0.5*sin(2*PI*1000*t)|0.5*sin(2*PI*1000*t):s=44100:d=0.1" -map_metadata -1 -fflags +bitexact -flags:a +bitexact -c:a vorbis -strict experimental -q:a 5 blip.ogg
//! ```
//!
//! (`aevalsrc` rather than `sine`, whose amplitude is fixed at 1/8; the Vorbis source is stereo
//! because ffmpeg's mono-to-stereo upmix is 3 dB down and its Vorbis encoder needs two channels.)

use grooviq_engine::{SampleBuf, SampleError, decode_sample, load_sample, load_sample_with_limit};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "grooviq-sample-file-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Frame `i` of channel `c`: a different value per channel, inside -1..1.
fn value(i: usize, c: usize) -> f32 {
    let x = ((i * 7 + c * 13) % 101) as f32 / 101.0;
    (x * 2.0 - 1.0) * 0.9
}

fn write_wav(path: &Path, channels: u16, rate: u32, bits: u16, float: bool, frames: usize) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format: if float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    // Full scale is 2^(bits - 1), as decoders divide by it; values stay inside +-0.9.
    let scale = (1i64 << (bits - 1)) as f32;
    for i in 0..frames {
        for c in 0..channels as usize {
            let v = value(i, c);
            if float {
                w.write_sample(v).unwrap();
            } else {
                w.write_sample((v * scale).round() as i32).unwrap();
            }
        }
    }
    w.finalize().unwrap();
}

fn peak(b: &SampleBuf) -> f32 {
    b.l.iter().chain(&b.r).fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn decodes_wav_pcm16_mono_pcm24_stereo_and_float32() {
    let dir = TempDir::new();
    let frames = 1000;
    // (channels, rate, bits, float, one LSB)
    for (channels, rate, bits, float, lsb) in [
        (1u16, 44_100u32, 16u16, false, 1.0 / 32_768.0),
        (2, 48_000, 24, false, 1.0 / 8_388_608.0),
        (2, 96_000, 32, true, 1e-7),
    ] {
        let path = dir.path(&format!("{channels}-{bits}-{float}.wav"));
        write_wav(&path, channels, rate, bits, float, frames);
        let b = load_sample(&path).unwrap();
        assert_eq!(b.rate, rate as f32);
        assert_eq!(b.len(), frames);
        assert_eq!(b.r.len(), frames);
        for i in 0..frames {
            let r_src = if channels == 1 { 0 } else { 1 };
            assert!(
                (b.l[i] - value(i, 0)).abs() <= lsb * 1.01,
                "{bits}-bit left frame {i}"
            );
            assert!(
                (b.r[i] - value(i, r_src)).abs() <= lsb * 1.01,
                "{bits}-bit right frame {i}"
            );
        }
        if channels == 1 {
            assert_eq!(b.l, b.r, "mono is copied to both channels");
        }
    }
}

#[test]
fn decodes_aiff_flac_mp3_and_ogg() {
    for name in ["blip.aif", "blip.flac", "blip.mp3", "blip.ogg"] {
        let b = load_sample(&data(name)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(b.rate, 44_100.0, "{name}");
        let seconds = b.len() as f32 / b.rate;
        assert!((seconds - 0.1).abs() <= 0.06, "{name}: {seconds} s");
        let p = peak(&b);
        assert!((p - 0.5).abs() <= 0.05, "{name}: peak {p}");
        assert_eq!(b.l.len(), b.r.len(), "{name}");
    }
}

#[test]
fn extra_channels_are_dropped() {
    let dir = TempDir::new();
    let path = dir.path("quad.wav");
    write_wav(&path, 4, 48_000, 16, false, 500);
    let b = load_sample(&path).unwrap();
    for i in 0..500 {
        assert!((b.l[i] - value(i, 0)).abs() <= 1.0 / 32_000.0);
        assert!((b.r[i] - value(i, 1)).abs() <= 1.0 / 32_000.0);
    }
}

#[test]
fn decoding_from_memory_matches_decoding_the_file() {
    let dir = TempDir::new();
    let path = dir.path("memory.wav");
    write_wav(&path, 2, 44_100, 24, false, 2_000);
    let bytes = std::fs::read(&path).unwrap();
    let from_memory = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
    assert_eq!(from_memory, load_sample(&path).unwrap());
    // The extension is only a hint: content decides.
    assert_eq!(decode_sample(&bytes, None, 600.0).unwrap(), from_memory);
    assert_eq!(
        decode_sample(&bytes, Some("mp3"), 600.0).unwrap(),
        from_memory
    );
    for name in ["blip.aif", "blip.flac", "blip.mp3", "blip.ogg"] {
        let bytes = std::fs::read(data(name)).unwrap();
        let ext = name.rsplit('.').next();
        assert_eq!(
            decode_sample(&bytes, ext, 600.0).unwrap(),
            load_sample(&data(name)).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn rejects_missing_empty_corrupt_non_audio_and_too_long() {
    let dir = TempDir::new();

    assert_eq!(
        load_sample(&dir.path("missing.wav")),
        Err(SampleError::NotFound)
    );

    let empty = dir.path("empty.wav");
    write_wav(&empty, 1, 48_000, 16, false, 0);
    assert_eq!(load_sample(&empty), Err(SampleError::Empty));

    let full = dir.path("full.wav");
    write_wav(&full, 1, 48_000, 16, false, 4_800);
    let truncated = dir.path("truncated.wav");
    std::fs::write(&truncated, &std::fs::read(&full).unwrap()[..20]).unwrap();
    let e = load_sample(&truncated).unwrap_err();
    assert!(
        matches!(e, SampleError::Corrupt(_) | SampleError::Unsupported),
        "truncated: {e:?}"
    );

    let text = dir.path("notes.wav");
    std::fs::write(&text, "These are my notes, not a sound.\n".repeat(50)).unwrap();
    assert_eq!(load_sample(&text), Err(SampleError::Unsupported));

    let second = dir.path("second.wav");
    write_wav(&second, 1, 48_000, 16, false, 48_000);
    match load_sample_with_limit(&second, 0.5) {
        Err(SampleError::TooLong { seconds, .. }) => {
            assert!((seconds - 1.0).abs() <= 0.01, "{seconds} s")
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
    assert!(load_sample_with_limit(&second, 1.5).is_ok());

    // The header claims 100 s but the file holds 1 s: only a header check can say "too long".
    let mut claims = std::fs::read(&second).unwrap();
    assert_eq!(&claims[36..40], b"data", "plain 44-byte header");
    let data_bytes = 100u32 * 48_000 * 2;
    claims[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    claims[4..8].copy_from_slice(&(data_bytes + 36).to_le_bytes());
    let liar = dir.path("liar.wav");
    std::fs::write(&liar, &claims).unwrap();
    match load_sample_with_limit(&liar, 50.0) {
        Err(SampleError::TooLong { seconds, .. }) => {
            assert!((seconds - 100.0).abs() <= 0.01, "{seconds} s")
        }
        other => panic!("expected TooLong from the header, got {other:?}"),
    }
    // Without the limit, the truncated data simply ends the sample.
    assert_eq!(load_sample(&liar).unwrap().len(), 48_000);

    let low = dir.path("low.wav");
    write_wav(&low, 1, 4_000, 16, false, 400);
    assert_eq!(load_sample(&low), Err(SampleError::BadRate(4_000)));
}

#[test]
fn every_error_reads_as_one_sentence() {
    let cases = [
        SampleError::NotFound,
        SampleError::Unreadable("permission denied".into()),
        SampleError::Unsupported,
        SampleError::Corrupt("bad header".into()),
        SampleError::Empty,
        SampleError::TooLong {
            seconds: 845.2,
            limit: 600.0,
        },
        SampleError::Unfinished,
        SampleError::BadRate(4_000),
    ];
    for e in cases {
        let s = e.to_string();
        assert!(s.ends_with('.'), "{s}");
        assert!(s.chars().next().unwrap().is_uppercase(), "{s}");
    }
    assert_eq!(
        SampleError::TooLong {
            seconds: 845.0,
            limit: 600.0
        }
        .to_string(),
        "This sample is 14:05 long; samples can be up to 10 minutes."
    );
}

#[test]
fn a_cut_off_file_plays_up_to_the_cut() {
    let dir = TempDir::new();
    let path = dir.path("cut.wav");
    write_wav(&path, 1, 48_000, 16, false, 10_000);
    let bytes = std::fs::read(&path).unwrap();
    // 44-byte header, then 2 bytes per frame: keep 4000 frames.
    let b = decode_sample(&bytes[..44 + 8_000], Some("wav"), 600.0).unwrap();
    assert_eq!(b.len(), 4_000);
    assert_eq!(b.l[3_999], load_sample(&path).unwrap().l[3_999]);

    // The AIFF reader looks past the sound data while opening; a cut file still opens.
    let aif = std::fs::read(data("blip.aif")).unwrap();
    let full = decode_sample(&aif, Some("aif"), 600.0).unwrap();
    let half = decode_sample(&aif[..aif.len() / 2], Some("aif"), 600.0).unwrap();
    assert!(
        half.len() > 1_000 && half.len() < full.len(),
        "{}",
        half.len()
    );
    assert_eq!(half.l[..], full.l[..half.len()]);
}

#[test]
fn decodes_an_mp3_with_a_large_id3_tag() {
    // An ID3v2.3 tag of about 1.5 MB (one title frame, then zero padding), larger than the
    // probe's 1 MB byte scan: only an ID3v2 reader can skip it to find the audio.
    let title = b"\x00Blip";
    let mut frames = Vec::new();
    frames.extend_from_slice(b"TIT2");
    frames.extend_from_slice(&(title.len() as u32).to_be_bytes());
    frames.extend_from_slice(&[0, 0]);
    frames.extend_from_slice(title);
    frames.resize(1_500_000, 0); // padding
    let n = frames.len() as u32;
    let syncsafe = [
        (n >> 21) as u8 & 0x7f,
        (n >> 14) as u8 & 0x7f,
        (n >> 7) as u8 & 0x7f,
        n as u8 & 0x7f,
    ];
    let mut bytes = b"ID3\x03\x00\x00".to_vec();
    bytes.extend_from_slice(&syncsafe);
    bytes.extend_from_slice(&frames);
    bytes.extend_from_slice(&std::fs::read(data("blip.mp3")).unwrap());

    let plain = load_sample(&data("blip.mp3")).unwrap();
    let tagged = decode_sample(&bytes, Some("mp3"), 600.0).unwrap();
    assert_eq!(tagged.rate, plain.rate);
    assert_eq!(tagged.len(), plain.len());
    assert_eq!(peak(&tagged), peak(&plain));
}

/// `blip.mp3` without its first (Info) frame: five 1152-frame MPEG-1 Layer III frames at these
/// byte offsets. Copies of it concatenate into a longer stream with no gapless header.
fn mp3_frames() -> (Vec<u8>, [usize; 6]) {
    let mp3 = std::fs::read(data("blip.mp3")).unwrap();
    let audio = mp3[208..].to_vec();
    let bounds = [0, 208, 417, 626, 835, 1044];
    assert_eq!(audio.len(), bounds[5]);
    for b in &bounds[..5] {
        assert_eq!(&audio[*b..*b + 2], &[0xff, 0xfb], "frame sync at {b}");
    }
    (audio, bounds)
}

/// Garbles a frame's body, keeping its 4-byte header and the first side-info bytes so the
/// demuxer still delivers it as a packet; the decoder then fails on it.
fn garble(bytes: &mut [u8]) {
    for b in &mut bytes[6..] {
        *b ^= 0xa5;
    }
}

#[test]
fn a_packet_that_fails_to_decode_is_skipped() {
    let (audio, bounds) = mp3_frames();
    let long: Vec<u8> = (0..10).flat_map(|_| audio.clone()).collect();
    let clean = decode_sample(&long, Some("mp3"), 600.0).unwrap();
    assert_eq!(clean.len(), 50 * 1152);

    // Damage one frame in the middle (copy 5, frame 2).
    let mut damaged = long.clone();
    let at = 5 * audio.len();
    garble(&mut damaged[at + bounds[2]..at + bounds[3]]);
    let b = decode_sample(&damaged, Some("mp3"), 600.0).unwrap();
    assert_eq!(
        b.len(),
        clean.len() - 1152,
        "only the damaged packet is lost"
    );
}

#[test]
fn a_file_where_no_packet_decodes_is_corrupt() {
    let mp3 = std::fs::read(data("blip.mp3")).unwrap();
    let (_, bounds) = mp3_frames();
    let mut damaged = mp3.clone();
    for f in 0..5 {
        garble(&mut damaged[208 + bounds[f]..208 + bounds[f + 1]]);
    }
    let e = decode_sample(&damaged, Some("mp3"), 600.0).unwrap_err();
    assert!(matches!(e, SampleError::Corrupt(_)), "{e:?}");
}

#[test]
fn a_rate_above_384_khz_is_rejected() {
    let dir = TempDir::new();
    let path = dir.path("fast.wav");
    write_wav(&path, 1, 400_000, 16, false, 400);
    assert_eq!(load_sample(&path), Err(SampleError::BadRate(400_000)));
    let edge = dir.path("edge.wav");
    write_wav(&edge, 1, 384_000, 16, false, 400);
    assert_eq!(load_sample(&edge).unwrap().rate, 384_000.0);
}

#[test]
fn a_too_long_file_without_a_frame_count_is_caught_while_decoding() {
    // blip.flac with STREAMINFO's 36-bit total-samples field zeroed ("unknown"): the reader
    // reports no frame count, so only the check while decoding can stop it.
    let mut flac = std::fs::read(data("blip.flac")).unwrap();
    assert_eq!(&flac[..4], b"fLaC");
    flac[8 + 13] &= 0xf0;
    flac[8 + 14..8 + 18].fill(0);
    assert_eq!(
        decode_sample(&flac, Some("flac"), 1.0).unwrap().len(),
        4_410
    );
    match decode_sample(&flac, Some("flac"), 0.05) {
        Err(SampleError::TooLong { seconds, limit }) => {
            assert_eq!(limit, 0.05);
            assert!(
                (seconds - 0.1).abs() <= 0.001,
                "reports the full length: {seconds} s"
            )
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
}

// ---- Items from issue #73 ----

/// A mono 16-bit 48 kHz WAV with `frames` frames (the 44-byte header hound writes), its RIFF
/// and data sizes then overwritten with `riff` and `data`, as a recorder that never came back
/// to finish its header leaves them.
fn wav_with_sizes(frames: usize, riff: u32, data: u32) -> Vec<u8> {
    let dir = TempDir::new();
    let path = dir.path("unfinished.wav");
    write_wav(&path, 1, 48_000, 16, false, frames);
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[36..40], b"data", "plain 44-byte header");
    bytes[4..8].copy_from_slice(&riff.to_le_bytes());
    bytes[40..44].copy_from_slice(&data.to_le_bytes());
    bytes
}

#[test]
fn an_unfinished_wav_header_reads_to_the_end_of_the_file() {
    let finished = {
        let dir = TempDir::new();
        let path = dir.path("finished.wav");
        write_wav(&path, 1, 48_000, 16, false, 3_000);
        load_sample(&path).unwrap()
    };
    // (RIFF size, data size): a header never filled in, a streamed one, and only the data
    // size left unfinished.
    for (riff, data) in [
        (0, 0),
        (u32::MAX, u32::MAX),
        (u32::MAX, 0),
        (10_000_000, 0),
        // The file is two bytes shorter than its header says (cut off).
        (36 + 6_000 + 2, 0),
        (36 + 6_000, u32::MAX),
    ] {
        let bytes = wav_with_sizes(3_000, riff, data);
        let b = decode_sample(&bytes, Some("wav"), 600.0)
            .unwrap_or_else(|e| panic!("riff {riff:#x}, data {data:#x}: {e:?}"));
        assert_eq!(b, finished, "riff {riff:#x}, data {data:#x}");
    }
}

#[test]
fn an_unfinished_wav_with_no_audio_says_it_was_not_finished() {
    for (riff, data) in [(0, 0), (u32::MAX, u32::MAX), (0, u32::MAX)] {
        let bytes = wav_with_sizes(0, riff, data);
        assert_eq!(
            decode_sample(&bytes, Some("wav"), 600.0),
            Err(SampleError::Unfinished),
            "riff {riff:#x}, data {data:#x}"
        );
    }
    // Less than one frame of sound after an unfinished header is no sound at all.
    let mut stub = wav_with_sizes(0, 0, 0);
    stub.push(0x12);
    assert_eq!(
        decode_sample(&stub, Some("wav"), 600.0),
        Err(SampleError::Unfinished)
    );
    // A header that was finished, with no frames in it, is simply empty.
    assert_eq!(
        decode_sample(&wav_with_sizes(0, 36, 0), Some("wav"), 600.0),
        Err(SampleError::Empty)
    );
}

/// The little-endian pages of an Ogg stream as `(offset, length)`.
fn ogg_pages(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        assert_eq!(&bytes[at..at + 4], b"OggS");
        let segments = bytes[at + 26] as usize;
        let body: usize = bytes[at + 27..at + 27 + segments]
            .iter()
            .map(|&s| s as usize)
            .sum();
        pages.push((at, 27 + segments + body));
        at += 27 + segments + body;
    }
    pages
}

/// Ogg's CRC-32: polynomial 0x04c11db7, no reflection, zero start and no final xor.
fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in page {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// `blip.ogg` made longer: its two header pages, then its audio page repeated `copies` times
/// with fresh sequence numbers, granule positions and checksums. Returns the stream and the
/// pages' `(offset, length)`.
fn long_ogg(copies: usize) -> (Vec<u8>, Vec<(usize, usize)>) {
    let src = std::fs::read(data("blip.ogg")).unwrap();
    let pages = ogg_pages(&src);
    assert_eq!(pages.len(), 3, "two header pages, one audio page");
    let (at, len) = pages[2];
    let mut out = src[..at].to_vec();
    for n in 0..copies {
        let mut page = src[at..at + len].to_vec();
        // The blip's own page ends the stream (flag 4) at granule 4416.
        page[5] = if n + 1 == copies { 4 } else { 0 };
        page[6..14].copy_from_slice(&((4_416 * (n + 1)) as i64).to_le_bytes());
        page[18..22].copy_from_slice(&(2 + n as u32).to_le_bytes());
        page[22..26].fill(0);
        let crc = ogg_crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&page);
    }
    let pages = ogg_pages(&out);
    (out, pages)
}

#[test]
fn a_damaged_ogg_page_keeps_the_audio_around_it() {
    let (ogg, pages) = long_ogg(12);
    assert_eq!(pages.len(), 2 + 12);
    let clean = decode_sample(&ogg, Some("ogg"), 600.0).unwrap();
    assert!(clean.len() > 10 * 4_000, "{}", clean.len());

    // Flip one byte in the body of a page in the middle: its CRC no longer matches.
    let mut damaged = ogg.clone();
    let (at, len) = pages[2 + 6];
    damaged[at + len - 3] ^= 0xff;
    let b = decode_sample(&damaged, Some("ogg"), 600.0).expect("the audio around the damage");
    assert!(
        b.len() > clean.len() - 2 * 4_416 && b.len() < clean.len(),
        "only the damaged page's audio is lost: {} of {} frames",
        b.len(),
        clean.len()
    );
    // What decodes before the damage is what a clean file decodes there.
    let before = 5 * 4_416;
    assert_eq!(b.l[..before], clean.l[..before]);
    assert_eq!(b.rate, 44_100.0);

    // The same damage in the first audio page, right after the headers, which the reader
    // otherwise gives up on while still opening the file.
    let mut damaged = ogg.clone();
    let (at, len) = pages[2];
    damaged[at + len - 3] ^= 0xff;
    let b = decode_sample(&damaged, Some("ogg"), 600.0).expect("the audio after the damage");
    assert!(
        b.len() > clean.len() - 2 * 4_416 && b.len() < clean.len(),
        "{} of {} frames",
        b.len(),
        clean.len()
    );
}

#[test]
fn an_ogg_with_every_audio_page_damaged_is_corrupt() {
    let (ogg, pages) = long_ogg(2);
    let mut damaged = ogg.clone();
    for &(at, len) in &pages[2..] {
        damaged[at + len - 3] ^= 0xff;
    }
    let e = decode_sample(&damaged, Some("ogg"), 600.0).unwrap_err();
    assert!(
        matches!(e, SampleError::Corrupt(_) | SampleError::Empty),
        "{e:?}"
    );
}

#[test]
fn the_too_long_message_states_the_limit_it_was_given() {
    let limit_in = |seconds: f32| {
        SampleError::TooLong {
            seconds: 845.0,
            limit: seconds,
        }
        .to_string()
    };
    assert_ne!(
        limit_in(600.0),
        limit_in(90.0),
        "the message follows the limit"
    );

    // The variant carries the limit load_sample_with_limit was called with.
    let dir = TempDir::new();
    let path = dir.path("two-seconds.wav");
    write_wav(&path, 1, 48_000, 16, false, 96_000);
    match load_sample_with_limit(&path, 1.5) {
        Err(SampleError::TooLong { seconds, limit }) => {
            assert!((seconds - 2.0).abs() <= 0.01, "{seconds}");
            assert_eq!(limit, 1.5);
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
    match load_sample(&path) {
        Ok(_) => {}
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn a_decoded_buffer_holds_no_spare_capacity() {
    let dir = TempDir::new();
    let path = dir.path("long.wav");
    // Odd lengths: many packets, so growth by doubling would leave slack behind.
    write_wav(&path, 2, 44_100, 16, false, 100_003);
    let mut files = vec![load_sample(&path).unwrap()];
    for name in ["blip.aif", "blip.flac", "blip.mp3", "blip.ogg"] {
        files.push(load_sample(&data(name)).unwrap());
    }
    files.push(decode_sample(&long_ogg(12).0, Some("ogg"), 600.0).unwrap());
    for b in files {
        assert_eq!(b.l.capacity(), b.l.len());
        assert_eq!(b.r.capacity(), b.r.len());
    }
}

// ---- Fix round 1 ----

/// A RIFF `LIST`/`INFO` chunk holding one `INAM` title.
fn list_info(title: &[u8]) -> Vec<u8> {
    let mut inam = b"INAM".to_vec();
    inam.extend_from_slice(&(title.len() as u32).to_le_bytes());
    inam.extend_from_slice(title);
    if title.len() % 2 == 1 {
        inam.push(0);
    }
    let mut list = b"LIST".to_vec();
    list.extend_from_slice(&(4 + inam.len() as u32).to_le_bytes());
    list.extend_from_slice(b"INFO");
    list.extend_from_slice(&inam);
    list
}

#[test]
fn a_finished_empty_wav_with_trailing_metadata_is_still_empty() {
    // Hound's finished empty file (RIFF 36, data 0), then metadata, with the RIFF size updated.
    // "record" and "recorder" (odd and even sizes) must not be read as sound.
    for title in [&b"record"[..], b"recorder", b"x"] {
        let mut bytes = wav_with_sizes(0, 36, 0);
        let list = list_info(title);
        bytes.extend_from_slice(&list);
        let riff = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff.to_le_bytes());
        assert_eq!(
            decode_sample(&bytes, Some("wav"), 600.0),
            Err(SampleError::Empty),
            "{}",
            String::from_utf8_lossy(title)
        );
    }
    // Other metadata a finished file may carry: XMP (`_PMX`) and Broadcast Wave (`bext`), whose
    // tags and sizes need not look like text.
    for tag in [b"_PMX", b"bext", b"iXML", b"\x01\x02\x03\x04"] {
        let mut bytes = wav_with_sizes(0, 36, 0);
        let body = b"<x:xmpmeta>record</x:xmpmeta>";
        bytes.extend_from_slice(tag);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(body);
        bytes.push(0); // pad to an even size
        let riff = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff.to_le_bytes());
        assert_eq!(
            decode_sample(&bytes, Some("wav"), 600.0),
            Err(SampleError::Empty),
            "{}",
            String::from_utf8_lossy(tag)
        );
    }
}

#[test]
fn an_unfinished_payload_that_looks_like_chunks_is_still_sound() {
    // Unknown and malformed chunks after an unfinished header remain sound.
    // A second `data` chunk, and a `LIST` whose size (1000) runs past the end of the file.
    for (chunk, frames) in [
        (
            [b"data".as_slice(), &8u32.to_le_bytes(), &[0x10; 8]].concat(),
            8,
        ),
        (
            [b"LIST".as_slice(), &1000u32.to_le_bytes(), &[0x10; 8]].concat(),
            8,
        ),
    ] {
        let mut bytes = wav_with_sizes(0, 0, 0);
        bytes.extend_from_slice(&chunk);
        assert_eq!(
            decode_sample(&bytes, Some("wav"), 600.0).unwrap().len(),
            frames,
            "{}",
            String::from_utf8_lossy(&chunk[..4])
        );
    }
}

#[test]
fn a_wav_header_never_filled_in_still_recovers_sound_that_is_not_chunks() {
    // The same unfinished header (all sizes zero) with audio after it, including silence.
    for frames in [3_000usize, 7] {
        let b = decode_sample(&wav_with_sizes(frames, 0, 0), Some("wav"), 600.0).unwrap();
        assert_eq!(b.len(), frames);
    }
    let mut silent = wav_with_sizes(0, 0, 0);
    silent.extend_from_slice(&[0; 2_000]);
    assert_eq!(
        decode_sample(&silent, Some("wav"), 600.0).unwrap().len(),
        1_000
    );
}

#[test]
fn an_oversized_ogg_length_field_does_not_hide_the_good_pages_after_it() {
    let (ogg, pages) = long_ogg(12);
    let clean = decode_sample(&ogg, Some("ogg"), 600.0).unwrap();
    // The segment count of the first audio page says 255: its table and body would run past the
    // end of the file. Eleven intact pages follow.
    let mut damaged = ogg.clone();
    damaged[pages[2].0 + 26] = 255;
    let b = decode_sample(&damaged, Some("ogg"), 600.0).expect("the intact pages after it");
    assert!(
        b.len() > clean.len() - 2 * 4_416 && b.len() < clean.len(),
        "{} of {} frames",
        b.len(),
        clean.len()
    );

    // A false capture pattern inside a damaged body, claiming the same: "OggS", then a
    // segment count of 255 at its offset 26.
    let mut damaged = ogg.clone();
    let (at, len) = pages[2];
    let fake = at + len - 60;
    damaged[at + len - 3] ^= 0xff;
    damaged[fake..fake + 4].copy_from_slice(b"OggS");
    damaged[fake + 26] = 255;
    // (its table would start in the next page: make sure that page is long enough to matter)
    let b = decode_sample(&damaged, Some("ogg"), 600.0).expect("the intact pages after it");
    assert!(
        b.len() > clean.len() - 2 * 4_416 && b.len() < clean.len(),
        "{} of {} frames",
        b.len(),
        clean.len()
    );

    // A file genuinely cut off mid-page, after a damaged first page, still plays what is there.
    let mut cut = ogg.clone();
    cut[pages[2].0 + pages[2].1 - 3] ^= 0xff;
    cut.truncate(cut.len() - 100);
    let b = decode_sample(&cut, Some("ogg"), 600.0).expect("what is left");
    assert!(b.len() > 8 * 4_416, "{}", b.len());
}

#[test]
fn a_recovered_file_is_held_to_the_length_limit() {
    // WAV: both ways a header can be unfinished, one second of sound, limit half a second.
    for (riff, data) in [(0, 0), (u32::MAX, u32::MAX), (10_000_000, 0), (0, u32::MAX)] {
        let bytes = wav_with_sizes(48_000, riff, data);
        match decode_sample(&bytes, Some("wav"), 0.5) {
            Err(SampleError::TooLong { seconds, limit }) => {
                assert!(
                    (seconds - 1.0).abs() <= 0.01,
                    "riff {riff:#x} data {data:#x}: {seconds}"
                );
                assert_eq!(limit, 0.5);
            }
            other => panic!("riff {riff:#x} data {data:#x}: expected TooLong, got {other:?}"),
        }
        assert_eq!(
            decode_sample(&bytes, Some("wav"), 1.5).unwrap().len(),
            48_000
        );
    }

    // OGG reopened without its damaged page: about 1.2 s of sound against a 0.5 s limit.
    let (ogg, pages) = long_ogg(12);
    let mut damaged = ogg.clone();
    damaged[pages[2].0 + pages[2].1 - 3] ^= 0xff;
    match decode_sample(&damaged, Some("ogg"), 0.5) {
        Err(SampleError::TooLong { seconds, limit }) => {
            assert!(seconds > 1.0, "{seconds}");
            assert_eq!(limit, 0.5);
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
}

// ---- Fix round 3 ----

#[test]
fn a_riff_size_within_a_pad_byte_of_the_file_is_finished() {
    // An empty data chunk, then ten bytes (five frames' worth, if they were sound). The RIFF size
    // may be one byte off the file length minus 8 either way (a trailing pad byte too many, or a
    // final pad counted but not written): finished, so empty. Anything else is unfinished and
    // the ten bytes are sound.
    for (off, finished) in [(10i64, false), (9, true), (8, true), (7, true), (6, false)] {
        let mut bytes = wav_with_sizes(0, 0, 0);
        bytes.extend_from_slice(&[0x10; 10]);
        let riff = bytes.len() as i64 - off;
        bytes[4..8].copy_from_slice(&(riff as u32).to_le_bytes());
        let got = decode_sample(&bytes, Some("wav"), 600.0);
        if finished {
            assert_eq!(got, Err(SampleError::Empty), "RIFF = length - {off}");
        } else {
            assert_eq!(got.unwrap().len(), 5, "RIFF = length - {off}");
        }
    }
}

#[test]
fn a_final_metadata_chunk_whose_pad_byte_was_counted_but_not_written_is_empty() {
    // iXML of 3 bytes ("abc") needs a pad byte; this writer counted it in the RIFF size (48) but
    // did not write it, so the file is 55 bytes long.
    let mut bytes = wav_with_sizes(0, 36, 0);
    bytes.extend_from_slice(b"iXML");
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(b"abc");
    assert_eq!(bytes.len(), 55);
    bytes[4..8].copy_from_slice(&48u32.to_le_bytes());
    assert_eq!(
        decode_sample(&bytes, Some("wav"), 600.0),
        Err(SampleError::Empty)
    );
}

fn footer_chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut chunk = tag.to_vec();
    chunk.extend_from_slice(&(body.len() as u32).to_le_bytes());
    chunk.extend_from_slice(body);
    if body.len() % 2 == 1 {
        chunk.push(0);
    }
    chunk
}

#[test]
fn unfinished_known_metadata_preserves_exact_audio() {
    let mut failures = Vec::new();
    for size in [0, u32::MAX] {
        for footer in [
            list_info(b"title"),
            footer_chunk(b"_PMX", b"xml"),
            footer_chunk(b"bext", &[42; 602]),
            footer_chunk(b"iXML", b"<xml/>"),
        ] {
            let mut bytes = wav_with_sizes(3, 0, size);
            let expected = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
            bytes.extend_from_slice(&footer);
            let actual = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
            if actual.l != expected.l || actual.r != expected.r {
                failures.push((size, footer[..4].to_vec(), actual.len()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "metadata decoded as audio: {failures:?}"
    );
}

#[test]
fn unfinished_metadata_boundary_cases() {
    let known = footer_chunk(b"_PMX", b"odd");
    let mut multi = list_info(b"title");
    multi.extend_from_slice(&known);
    multi.extend_from_slice(&footer_chunk(b"iXML", b"xml"));
    for size in [0, u32::MAX] {
        let mut bytes = wav_with_sizes(3, 0, size);
        let expected = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
        bytes.extend_from_slice(&multi);
        assert_eq!(
            decode_sample(&bytes, Some("wav"), 600.0).unwrap().l,
            expected.l
        );
        let mut bad_info = list_info(b"title");
        bad_info[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let cases = [
            known[..7].to_vec(),               // short header
            known[..known.len() - 1].to_vec(), // missing odd pad
            [b"bext".as_slice(), &u32::MAX.to_le_bytes(), b"body"].concat(),
            footer_chunk(b"JUNK", b"unknown"),
            footer_chunk(b"data", b"second data"),
            footer_chunk(b"LIST", b"adtl"),
            bad_info,
            [known.clone(), vec![1, 2]].concat(), // valid prefix, not EOF
            [vec![1], known.clone()].concat(),    // unaligned mono frame
        ];
        for tail in cases {
            let mut bytes = wav_with_sizes(3, 0, size);
            bytes.extend_from_slice(&tail);
            let mut finished = bytes.clone();
            let riff = (finished.len() - 8) as u32;
            let data = (finished.len() - 44) as u32;
            finished[4..8].copy_from_slice(&riff.to_le_bytes());
            finished[40..44].copy_from_slice(&data.to_le_bytes());
            let expected = decode_sample(&finished, Some("wav"), 600.0).unwrap();
            let actual = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
            assert_eq!(actual.l, expected.l, "preserve {tail:?}, size {size}");
        }
        let mut empty = wav_with_sizes(0, 0, size);
        empty.extend_from_slice(&multi);
        assert_eq!(
            decode_sample(&empty, Some("wav"), 600.0),
            Err(SampleError::Unfinished)
        );
    }
}

#[test]
fn unfinished_metadata_uses_stereo_frames_and_finished_wavs_keep_their_data() {
    let dir = TempDir::new();
    let path = dir.path("stereo.wav");
    write_wav(&path, 2, 48_000, 16, false, 3);
    let original = std::fs::read(path).unwrap();
    let expected = decode_sample(&original, Some("wav"), 600.0).unwrap();
    for size in [0, u32::MAX] {
        let mut bytes = original.clone();
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        bytes[40..44].copy_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&footer_chunk(b"_PMX", b"four"));
        let actual = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
        assert_eq!(actual.l, expected.l);
        assert_eq!(actual.r, expected.r);
        let mut unaligned = original.clone();
        unaligned.extend_from_slice(&[1, 2]);
        unaligned.extend_from_slice(&footer_chunk(b"_PMX", b"xx"));
        let length = (unaligned.len() - 44) as u32;
        unaligned[40..44].copy_from_slice(&length.to_le_bytes());
        let riff = (unaligned.len() - 8) as u32;
        unaligned[4..8].copy_from_slice(&riff.to_le_bytes());
        let all = decode_sample(&unaligned, Some("wav"), 600.0).unwrap();
        unaligned[4..8].copy_from_slice(&0u32.to_le_bytes());
        unaligned[40..44].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            decode_sample(&unaligned, Some("wav"), 600.0).unwrap().l,
            all.l
        );
    }
    let mut finished = original;
    finished.extend_from_slice(&footer_chunk(b"_PMX", b"four"));
    let riff = (finished.len() - 8) as u32;
    finished[4..8].copy_from_slice(&riff.to_le_bytes());
    assert_eq!(
        decode_sample(&finished, Some("wav"), 600.0).unwrap().l,
        expected.l
    );
}

#[test]
fn finished_audio_that_spells_metadata_is_preserved() {
    let mut bytes = wav_with_sizes(3, 0, 0);
    bytes.extend_from_slice(&footer_chunk(b"_PMX", b"four"));
    let riff = (bytes.len() - 8) as u32;
    let data = (bytes.len() - 44) as u32;
    bytes[4..8].copy_from_slice(&riff.to_le_bytes());
    bytes[40..44].copy_from_slice(&data.to_le_bytes());
    let actual = decode_sample(&bytes, Some("wav"), 600.0).unwrap();
    let expected: Vec<f32> = bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| i16::from_le_bytes([v[0], v[1]]) as f32 / 32768.0)
        .collect();
    assert_eq!(actual.l, expected);
    assert_eq!(actual.r, expected);
}
