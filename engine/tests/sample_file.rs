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
