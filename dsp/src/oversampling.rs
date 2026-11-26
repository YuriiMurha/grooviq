//! Private Drive/Saturation FIR machinery. Arithmetic retained for legacy parity.

/// Count an up/down FIR pair in each stereo channel. `k` is a fixed stage size.
pub(crate) fn add_stereo_stage_payload(k: usize, payload: &mut crate::insert_memory::Payload) {
    payload.array::<f32>(2 * k, 4); // Coefficients for Up and Down.
    payload.array::<f32>(4 * k, 4); // Doubled Up and even Down histories.
    payload.array::<f32>(2 * (k + 1), 2); // Doubled odd Down history.
    payload.array::<f64>(4 * k - 1, 4); // Temporary halfband tables.
}
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// Causal halfband impulse response of length 4K-1 (centre tap 0.5, even offsets zero).
fn halfband(k: usize, beta: f64) -> Vec<f64> {
    let taps = 4 * k - 1;
    let m = (taps - 1) / 2;
    let mut h = vec![0.0; taps];
    let i0b = bessel_i0(beta);
    for (i, hv) in h.iter_mut().enumerate() {
        let n = i as i64 - m as i64;
        *hv = if n == 0 {
            0.5
        } else if n % 2 == 0 {
            0.0
        } else {
            let x = n as f64 / m as f64;
            let w = bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / i0b;
            libm::sin(std::f64::consts::FRAC_PI_2 * n as f64) / (std::f64::consts::PI * n as f64)
                * w
        };
    }
    // make the polyphase branch sum exactly 0.5 so DC gain is exactly 1
    let odd: f64 = h
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, v)| v)
        .sum();
    for (i, hv) in h.iter_mut().enumerate() {
        if i % 2 == 0 {
            *hv *= 0.5 / odd;
        }
    }
    h
}

/// History buffer with doubled storage so the newest `len` samples are one contiguous slice.
#[derive(Clone)]
pub(super) struct Hist {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
}

impl Hist {
    pub(super) fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; 2 * len],
            len,
            pos: 0,
        }
    }
    #[inline(always)]
    fn push(&mut self, x: f32) {
        self.pos = if self.pos == 0 {
            self.len - 1
        } else {
            self.pos - 1
        };
        self.buf[self.pos] = x;
        self.buf[self.pos + self.len] = x;
    }
    /// s[0] = newest, s[j] = j samples ago.
    #[inline(always)]
    fn slice(&self) -> &[f32] {
        &self.buf[self.pos..self.pos + self.len]
    }
    pub(super) fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

#[inline(always)]
fn dot(c: &[f32], s: &[f32]) -> f32 {
    c.iter().zip(s).map(|(a, b)| a * b).sum()
}

/// Polyphase 2x upsampler: one input -> two outputs.
#[derive(Clone)]
pub(super) struct Up {
    pub(super) h: Hist,
    c: Vec<f32>,
    k: usize,
}

impl Up {
    pub(super) fn new(k: usize, beta: f64) -> Self {
        let h = halfband(k, beta);
        Self {
            h: Hist::new(2 * k),
            c: (0..2 * k).map(|j| (2.0 * h[2 * j]) as f32).collect(),
            k,
        }
    }
    #[inline(always)]
    pub(super) fn run(&mut self, x: f32) -> (f32, f32) {
        self.h.push(x);
        let s = self.h.slice();
        (dot(&self.c, s), s[self.k - 1])
    }
}

/// Polyphase 2x downsampler: two inputs -> one output.
#[derive(Clone)]
pub(super) struct Down {
    pub(super) e: Hist,
    pub(super) o: Hist,
    c: Vec<f32>,
    k: usize,
}

impl Down {
    pub(super) fn new(k: usize, beta: f64) -> Self {
        let h = halfband(k, beta);
        Self {
            e: Hist::new(2 * k),
            o: Hist::new(k + 1),
            c: (0..2 * k).map(|j| h[2 * j] as f32).collect(),
            k,
        }
    }
    #[inline(always)]
    pub(super) fn run(&mut self, a: f32, b: f32) -> f32 {
        self.e.push(a);
        self.o.push(b);
        dot(&self.c, self.e.slice()) + 0.5 * self.o.slice()[self.k]
    }
}
