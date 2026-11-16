//! Writes the golden drum kit to `tests/golden/samples/`: four short sounds made entirely by this
//! code (CC0), in four different WAV flavours so the acceptance test also exercises the decoder:
//!
//! * `kick.wav`: 0.5 s sine sweep 150 → 45 Hz with an exponential decay, 24-bit stereo 48 kHz.
//! * `snare.wav`: 0.25 s noise plus a 180 Hz tone, 16-bit mono 44.1 kHz (mono and rate
//!   conversion).
//! * `hat.wav`: 0.08 s differenced noise, 32-bit float stereo 48 kHz.
//! * `clap.wav`: 0.3 s, three noise bursts and a decay, 16-bit stereo 48 kHz.
//!
//! Run with `cargo run -p grooviq-golden --example make_kit`. Every `exp`/`sin` goes through
//! `libm` and the noise is a seeded xorshift, so the files come out the same on every platform.
//! Regenerating them changes the M1 golden render: refresh `refs/drums_4ch.wav` too.

use std::f64::consts::TAU;
use std::path::Path;

/// xorshift64 (integer maths, so identical everywhere), uniform in -1..1.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// A 3 ms linear fade at the end, so no sound stops on a click.
fn fade_out(samples: &mut [f64], rate: u32) {
    let n = (0.003 * f64::from(rate)) as usize;
    let len = samples.len();
    for (i, s) in samples[len - n..].iter_mut().enumerate() {
        *s *= (n - 1 - i) as f64 / n as f64;
    }
}

fn kick(rate: u32) -> Vec<f64> {
    let n = (0.5 * f64::from(rate)) as usize;
    let mut phase = 0.0;
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let hz = 45.0 + 105.0 * libm::exp(-t / 0.05);
            let s = 0.6 * libm::exp(-t / 0.15) * libm::sin(phase);
            phase += TAU * hz / f64::from(rate);
            s
        })
        .collect();
    fade_out(&mut out, rate);
    out
}

fn snare(rate: u32) -> Vec<f64> {
    let n = (0.25 * f64::from(rate)) as usize;
    let mut noise = Noise(0x5EED_0002);
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let tone = 0.3 * libm::exp(-t / 0.04) * libm::sin(TAU * 180.0 * t);
            let hiss = 0.3 * libm::exp(-t / 0.06) * noise.next();
            tone + hiss
        })
        .collect();
    fade_out(&mut out, rate);
    out
}

fn hat(rate: u32, seed: u64) -> Vec<f64> {
    let n = (0.08 * f64::from(rate)) as usize;
    let mut noise = Noise(seed);
    let mut last = 0.0;
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let x = noise.next();
            // Differencing tilts white noise towards the highs.
            let d = 0.5 * (x - last);
            last = x;
            0.4 * libm::exp(-t / 0.015) * d
        })
        .collect();
    fade_out(&mut out, rate);
    out
}

fn clap(rate: u32, seed: u64) -> Vec<f64> {
    let n = (0.3 * f64::from(rate)) as usize;
    let mut noise = Noise(seed);
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            // Three short bursts 10 ms apart, then the room's decay from the third.
            let bursts: f64 = [0.0, 0.010, 0.020]
                .iter()
                .filter(|&&at| t >= at)
                .map(|&at| libm::exp(-(t - at) / 0.004))
                .sum();
            let decay = if t >= 0.020 {
                0.5 * libm::exp(-(t - 0.020) / 0.07)
            } else {
                0.0
            };
            0.35 * (bursts + decay) * noise.next()
        })
        .collect();
    fade_out(&mut out, rate);
    out
}

fn spec(channels: u16, rate: u32, bits: u16, float: bool) -> hound::WavSpec {
    hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format: if float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    }
}

/// Writes interleaved `channels` (each the same length) in `spec`'s format.
fn write(path: &Path, spec: hound::WavSpec, channels: &[Vec<f64>]) {
    let mut w = hound::WavWriter::create(path, spec).expect("create a kit file");
    let full = f64::from((1u32 << (spec.bits_per_sample - 1)) - 1);
    for i in 0..channels[0].len() {
        for ch in channels {
            let x = ch[i];
            match spec.sample_format {
                hound::SampleFormat::Float => w.write_sample(x as f32),
                hound::SampleFormat::Int => w.write_sample((x * full).round() as i32),
            }
            .expect("write a sample");
        }
    }
    w.finalize().expect("finish a kit file");
    println!("wrote {}", path.display());
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    std::fs::create_dir_all(&dir).expect("create samples/");
    let k = kick(48_000);
    write(
        &dir.join("kick.wav"),
        spec(2, 48_000, 24, false),
        &[k.clone(), k],
    );
    write(
        &dir.join("snare.wav"),
        spec(1, 44_100, 16, false),
        &[snare(44_100)],
    );
    write(
        &dir.join("hat.wav"),
        spec(2, 48_000, 32, true),
        &[hat(48_000, 0x5EED_0003), hat(48_000, 0x5EED_0013)],
    );
    write(
        &dir.join("clap.wav"),
        spec(2, 48_000, 16, false),
        &[clap(48_000, 0x5EED_0004), clap(48_000, 0x5EED_0014)],
    );
}
