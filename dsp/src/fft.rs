//! Iterative radix-2 complex FFT (f64) with precomputed twiddles and bit reversal.
//! Used for the EQ analyser and for building the synth's band-limited wavetables.

use std::f64::consts::PI;

pub struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    rev: Vec<u32>,
}

impl Fft {
    pub(crate) fn add_payload(n: usize, payload: &mut crate::insert_memory::Payload) {
        payload.array::<f64>(n / 2, 2); // Cosine and sine tables.
        payload.array::<u32>(n, 1); // Bit reversal.
    }

    /// `n` must be a power of two.
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let cos = (0..n / 2)
            .map(|k| libm::cos(2.0 * PI * k as f64 / n as f64))
            .collect();
        let sin = (0..n / 2)
            .map(|k| libm::sin(2.0 * PI * k as f64 / n as f64))
            .collect();
        let rev = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        Self { n, cos, sin, rev }
    }

    /// In place. Forward: X[k] = sum x[n] e^{-2 pi i k n / N}. Inverse uses e^{+...} and is
    /// NOT scaled by 1/N.
    pub fn run(&self, re: &mut [f64], im: &mut [f64], inverse: bool) {
        let n = self.n;
        debug_assert!(re.len() >= n && im.len() >= n);
        for (i, &r) in self.rev.iter().enumerate().take(n) {
            let j = r as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let sign = if inverse { 1.0 } else { -1.0 };
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.cos[k * step];
                    let wi = sign * self.sin[k * step];
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
                start += len;
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_finds_a_bin() {
        let n = 64;
        let f = Fft::new(n);
        let mut re: Vec<f64> = (0..n)
            .map(|i| libm::cos(2.0 * PI * 5.0 * i as f64 / n as f64))
            .collect();
        let mut im = vec![0.0; n];
        f.run(&mut re, &mut im, false);
        assert!((re[5] - n as f64 / 2.0).abs() < 1e-9);
        assert!(re[6].abs() < 1e-9 && im[5].abs() < 1e-9);
        f.run(&mut re, &mut im, true);
        assert!((re[0] / n as f64 - 1.0).abs() < 1e-9);
    }
}
