//! Control-side stereo peak tree. Never call from an audio callback.
use crate::SampleBuf;
use std::sync::Arc;

pub const MAX_BINS: usize = 16_384;
const BASE: usize = 64;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    pub l_min: f32,
    pub l_max: f32,
    pub r_min: f32,
    pub r_max: f32,
}
impl Peak {
    pub fn from_frames(l: &[f32], r: &[f32]) -> Self {
        let mut peak = Self {
            l_min: f32::INFINITY,
            l_max: f32::NEG_INFINITY,
            r_min: f32::INFINITY,
            r_max: f32::NEG_INFINITY,
        };
        for (&l, &r) in l.iter().zip(r) {
            peak = peak.merge(Self {
                l_min: l,
                l_max: l,
                r_min: r,
                r_max: r,
            });
        }
        peak
    }
    fn merge(self, other: Self) -> Self {
        Self {
            l_min: self.l_min.min(other.l_min),
            l_max: self.l_max.max(other.l_max),
            r_min: self.r_min.min(other.r_min),
            r_max: self.r_max.max(other.r_max),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidWaveform;
pub struct PeakPyramid {
    sample: Arc<SampleBuf>,
    levels: Vec<Vec<Peak>>,
}
impl PeakPyramid {
    pub fn new(sample: Arc<SampleBuf>) -> Result<Self, InvalidWaveform> {
        if sample.is_empty()
            || sample.len() != sample.r.len()
            || !sample.rate.is_finite()
            || !(8000.0..=384000.0).contains(&sample.rate)
            || sample.rate.fract() != 0.0
            || sample.len() as f64 > sample.rate as f64 * 600.0
            || sample.l.iter().chain(&sample.r).any(|v| !v.is_finite())
        {
            return Err(InvalidWaveform);
        }
        let mut levels = vec![
            sample
                .l
                .chunks(BASE)
                .zip(sample.r.chunks(BASE))
                .map(|(l, r)| Peak::from_frames(l, r))
                .collect::<Vec<_>>(),
        ];
        while levels.last().unwrap().len() > 1 {
            let next = levels
                .last()
                .unwrap()
                .chunks(2)
                .map(|p| if p.len() == 2 { p[0].merge(p[1]) } else { p[0] })
                .collect();
            levels.push(next);
        }
        Ok(Self { sample, levels })
    }
    pub fn allocated_peak_bytes(&self) -> usize {
        self.levels.capacity() * std::mem::size_of::<Vec<Peak>>()
            + self
                .levels
                .iter()
                .map(|l| l.capacity() * std::mem::size_of::<Peak>())
                .sum::<usize>()
    }
    pub fn budgeted(
        sample: Arc<SampleBuf>,
        budget: &crate::media_budget::MediaBudget,
    ) -> Result<Arc<Self>, String> {
        budget.sample(&sample).map_err(|e| e.to_string())?;
        let mut count = sample.len().div_ceil(BASE);
        let mut bins = 0usize;
        let mut levels = 0usize;
        loop {
            bins = bins.checked_add(count).ok_or("Peak size overflow")?;
            levels += 1;
            if count <= 1 {
                break;
            }
            count = count.div_ceil(2);
        }
        // Vec level-list growth doubles; per-level exact-size iterators allocate exact bins.
        let reserved = bins * 16
            + (if levels > 1 {
                levels.next_power_of_two().max(4)
            } else {
                1
            }) * std::mem::size_of::<Vec<Peak>>();
        let hold = budget.reserve(reserved).map_err(|e| e.to_string())?;
        let pyramid = Arc::new(Self::new(sample).map_err(|_| "Invalid waveform")?);
        hold.bind(2, &pyramid, pyramid.allocated_peak_bytes())
            .map_err(|e| e.to_string())?;
        Ok(pyramid)
    }
    pub fn sample(&self) -> &Arc<SampleBuf> {
        &self.sample
    }
    pub fn peak_bytes(&self) -> usize {
        self.levels.iter().map(|l| l.len() * 16).sum()
    }
    /// Exact half-open source range, equally partitioned into nonempty bins. Reverse is a
    /// presentation transform: reverse records, without swapping channel signs.
    pub fn query(
        &self,
        start: usize,
        end: usize,
        bins: usize,
    ) -> Result<Vec<Peak>, InvalidWaveform> {
        if start >= end
            || end > self.sample.len()
            || bins == 0
            || bins > MAX_BINS
            || bins > end - start
        {
            return Err(InvalidWaveform);
        }
        Ok((0..bins)
            .map(|i| {
                self.range(
                    start + ((end - start) as u64 * i as u64 / bins as u64) as usize,
                    start + ((end - start) as u64 * (i + 1) as u64 / bins as u64) as usize,
                )
            })
            .collect())
    }
    fn range(&self, mut start: usize, end: usize) -> Peak {
        let edge = end.min(start.next_multiple_of(BASE));
        let mut peak = Peak::from_frames(&self.sample.l[start..edge], &self.sample.r[start..edge]);
        start = edge;
        while start + BASE <= end {
            let index = start / BASE;
            let available = (end - start) / BASE;
            let level = (available.ilog2() as usize)
                .min(if index == 0 {
                    usize::MAX
                } else {
                    index.trailing_zeros() as usize
                })
                .min(self.levels.len() - 1);
            peak = peak.merge(self.levels[level][index >> level]);
            start += BASE << level;
        }
        if start < end {
            peak = peak.merge(Peak::from_frames(
                &self.sample.l[start..end],
                &self.sample.r[start..end],
            ));
        }
        peak
    }
    /// WFP1, little endian: 64-byte header then 16-byte stereo min/max records.
    pub fn encode(
        &self,
        generation: u64,
        request: u64,
        asset: u32,
        start: usize,
        end: usize,
        bins: usize,
    ) -> Result<Vec<u8>, InvalidWaveform> {
        let peaks = self.query(start, end, bins)?;
        let mut out = Vec::with_capacity(64 + 16 * bins);
        out.extend_from_slice(b"WFP1");
        out.extend_from_slice(&asset.to_le_bytes());
        for value in [
            generation,
            request,
            self.sample.len() as u64,
            start as u64,
            end as u64,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for value in [self.sample.rate as u32, 2, bins as u32, 0] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for p in peaks {
            for v in [p.l_min, p.l_max, p.r_min, p.r_max] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Ok(out)
    }
}
