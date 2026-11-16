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
        Err(SampleError::TooLong { seconds }) => {
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
        Err(SampleError::TooLong { seconds }) => {
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
        SampleError::TooLong { seconds: 845.2 },
        SampleError::BadRate(4_000),
    ];
    for e in cases {
        let s = e.to_string();
        assert!(s.ends_with('.'), "{s}");
        assert!(s.chars().next().unwrap().is_uppercase(), "{s}");
    }
    assert_eq!(
        SampleError::TooLong { seconds: 845.0 }.to_string(),
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
        Err(SampleError::TooLong { seconds }) => {
            assert!(
                (seconds - 0.1).abs() <= 0.001,
                "reports the full length: {seconds} s"
            )
        }
        other => panic!("expected TooLong, got {other:?}"),
    }
}
