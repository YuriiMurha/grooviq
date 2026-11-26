//! Causal, grid-locked slowdown. Every wet read is at or behind the input just captured.
use crate::util::{Smooth, clamp_param, flush_output_subnormal, sane};
use crate::{Device, HostDomain, HostTransport, NoteEvent, db_to_gain};

pub const NAME: &str = "Half-time";
pub const SAMPLE_RATE_RANGE: (f32, f32) = (22_050.0, 192_000.0);

/// Speed (1/2, 1/4), window (1/4, 1/2, 1, 2 bars), smoothing %, mix, wet output dB and BPM.
pub const SPEC: [(f32, f32, f32); 6] = [
    (0.0, 1.0, 0.0),
    (0.0, 3.0, 2.0),
    (0.0, 100.0, 25.0),
    (0.0, 1.0, 1.0),
    (-24.0, 12.0, 0.0),
    (20.0, 400.0, 120.0),
];
pub const PARAM_IDS: [&str; 6] = ["speed", "window", "smoothing", "mix", "output_db", "bpm"];
pub const PRESETS: [&str; 2] = ["Pocket Shift", "Low Orbit"];

pub fn is_stepped(index: u32) -> bool {
    matches!(index, 0 | 1)
}

pub fn preset(name: &str) -> Option<Vec<(u32, f32)>> {
    let values = match name {
        "Init" | "Default" => SPEC.map(|s| s.2),
        "Pocket Shift" => [0.0, 1.0, 20.0, 0.7, 0.0, 120.0],
        "Low Orbit" => [1.0, 3.0, 45.0, 1.0, -2.0, 120.0],
        _ => return None,
    };
    Some(
        values
            .into_iter()
            .enumerate()
            .map(|(i, value)| (i as u32, value))
            .collect(),
    )
}

const PPQ: f64 = 960.0;
const SECONDS_PER_WINDOW_AT_MIN_BPM: f64 = 24.0;

pub struct HalfTime {
    sr: f32,
    capture: Box<[[f32; 2]]>,
    elapsed: usize,
    speed: usize,
    window: usize,
    smoothing_frames: u32,
    mix: Smooth,
    gain: Smooth,
    bpm: f32,
    settings_changed: bool,
    pending: Option<HostTransport>,
    previous: Option<HostTransport>,
    local_frame: u64,
    grid: Option<(i64, u64)>,
    wet: [f32; 2],
    fade_from: [f32; 2],
    fade_length: u32,
    fade_elapsed: u32,
    meters: [f32; 3],
}

impl HalfTime {
    pub fn new(sample_rate: f32) -> Self {
        let (min, max) = SAMPLE_RATE_RANGE;
        let sr = if sample_rate.is_finite() {
            sample_rate.clamp(min, max)
        } else {
            48_000.0
        };
        // At 1/2 speed a 24-second window can only read the first 12 seconds.
        // The extra frame allows the upper tap of the fractional read.
        let capacity = libm::ceil(f64::from(sr) * SECONDS_PER_WINDOW_AT_MIN_BPM * 0.5) as usize + 1;
        Self {
            sr,
            capture: vec![[0.0; 2]; capacity].into_boxed_slice(),
            elapsed: 0,
            speed: SPEC[0].2 as usize,
            window: SPEC[1].2 as usize,
            smoothing_frames: smoothing_frames(sr, SPEC[2].2),
            mix: Smooth::new(SPEC[3].2, 5.0, sr),
            gain: Smooth::new(db_to_gain(SPEC[4].2), 5.0, sr),
            bpm: SPEC[5].2,
            settings_changed: false,
            pending: None,
            previous: None,
            local_frame: 0,
            grid: None,
            wet: [0.0; 2],
            fade_from: [0.0; 2],
            fade_length: 0,
            fade_elapsed: 0,
            meters: [0.0; 3],
        }
    }

    fn clock(&mut self, frames: u32) -> (HostTransport, bool) {
        let host = self.pending.take().filter(|h| {
            h.frames >= frames
                && h.sample_rate == self.sr
                && (SPEC[5].0..=SPEC[5].1).contains(&h.bpm)
                && h.anchor_tick.is_finite()
                && h.frame.checked_add(u64::from(frames)).is_some()
        });
        let restart = match (self.previous, host) {
            (Some(previous), Some(host)) => {
                host.changed
                    || host.discontinuity != previous.discontinuity
                    || host.stream_generation != previous.stream_generation
                    || host.domain != previous.domain
                    || host.playing != previous.playing
                    || (host.playing && host.frame != previous.frame)
                    || (!host.playing && host.bpm != previous.bpm)
            }
            (None, None) => false,
            _ => true,
        };
        self.previous = host.map(|h| HostTransport {
            frame: h.frame + u64::from(frames),
            ..h
        });
        if let Some(host) = host.filter(|h| h.playing) {
            return (host, restart);
        }
        if restart {
            self.local_frame = 0;
        }
        // A stopped host supplies tempo but has no moving song position. The local clock
        // keeps the effect working on live input until playback starts.
        let clock = HostTransport {
            domain: HostDomain::Pattern,
            frame: self.local_frame,
            frames,
            sample_rate: self.sr,
            bpm: host.map_or(self.bpm, |h| h.bpm),
            playing: true,
            anchor_frame: 0,
            anchor_tick: 0.0,
            shift: 0,
            loop_start: 0,
            loop_end: 0,
            stream_generation: 0,
            discontinuity: 0,
            changed: false,
        };
        self.local_frame += u64::from(frames);
        (clock, restart)
    }

    fn begin_capture(&mut self, grid: (i64, u64), available_frames: u64) {
        self.fade_from = self.wet;
        // A very short loop/fragment must reach the new signal before its next restart.
        self.fade_length = if self.grid.is_some() {
            u64::from(self.smoothing_frames).min(available_frames.saturating_sub(1)) as u32
        } else {
            0
        };
        self.fade_elapsed = 0;
        self.elapsed = 0;
        self.grid = Some(grid);
    }

    fn read(&mut self, input: [f32; 2], ratio: f64) -> [f32; 2] {
        if self.elapsed < self.capture.len() {
            self.capture[self.elapsed] = input.map(|v| v.clamp(-16.0, 16.0));
        }
        let last = self.elapsed.min(self.capture.len() - 1);
        let read = (self.elapsed as f64 * ratio).min(last as f64);
        let lower = read as usize;
        let upper = (lower + 1).min(last);
        let fraction = (read - lower as f64) as f32;
        self.elapsed += 1;

        let wet = std::array::from_fn(|ch| {
            let a = self.capture[lower][ch];
            let b = self.capture[upper][ch];
            a + fraction * (b - a)
        });
        if self.fade_elapsed < self.fade_length {
            let t = self.fade_elapsed as f32 / self.fade_length as f32;
            let weight = 0.5 - 0.5 * libm::cosf(std::f32::consts::PI * t);
            self.fade_elapsed += 1;
            // Hold the actual outgoing wet sample so even a second seek during a fade is
            // continuous, without keeping a second window alive.
            std::array::from_fn(|ch| self.fade_from[ch] + weight * (wet[ch] - self.fade_from[ch]))
        } else {
            wet
        }
    }
}

fn smoothing_frames(sr: f32, percent: f32) -> u32 {
    libm::round(f64::from(sr) * f64::from(percent) * 0.0003) as u32
}

// Use the host's forward-map operation order and rounding to classify grid samples.
// An inverse division can put an exact scheduled boundary just below its tick.
fn frame_at(clock: &HostTransport, unwrapped_tick: f64) -> u64 {
    let played = unwrapped_tick - clock.shift as f64;
    let offset = libm::ceil(
        (played - clock.anchor_tick) * f64::from(clock.sample_rate) * 60.0
            / (f64::from(clock.bpm) * PPQ),
    );
    if offset >= 0.0 {
        clock.anchor_frame.saturating_add(offset as u64)
    } else {
        clock.anchor_frame.saturating_sub((-offset) as u64)
    }
}

impl Device for HalfTime {
    fn set_transport(&mut self, transport: &HostTransport) {
        self.pending = Some(*transport);
    }

    fn set_tempo(&mut self, bpm: f32) {
        if let Some(bpm) = clamp_param(&SPEC, 5, bpm)
            && self.bpm != bpm
        {
            self.bpm = bpm;
            if self.previous.is_none() {
                self.local_frame = 0;
                self.settings_changed = true;
            }
        }
    }

    fn set_param(&mut self, index: u32, value: f32) {
        let Some(value) = clamp_param(&SPEC, index, value) else {
            return;
        };
        match index {
            0 | 1 => {
                let stepped = if index == 0 {
                    &mut self.speed
                } else {
                    &mut self.window
                };
                let value = libm::roundf(value) as usize;
                self.settings_changed |= *stepped != value;
                *stepped = value;
            }
            2 => self.smoothing_frames = smoothing_frames(self.sr, value),
            3 => self.mix.set(value),
            4 => self.gain.set(db_to_gain(value)),
            5 => self.set_tempo(value),
            _ => unreachable!("checked against SPEC"),
        }
        if self.grid.is_none() {
            self.mix.snap();
            self.gain.snap();
        }
    }

    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        _: Option<(&[f32], &[f32])>,
        _: &[NoteEvent],
    ) {
        let frames = l.len().min(r.len()) as u32;
        let (clock, restart) = self.clock(frames);
        let mut retrigger = restart || std::mem::take(&mut self.settings_changed);
        self.settings_changed = retrigger && frames == 0;
        let ratio = [0.5, 0.25][self.speed];
        let window = PPQ * f64::from([1, 2, 4, 8][self.window]);
        let loop_ticks = clock.loop_end.saturating_sub(clock.loop_start);

        self.meters[..2].fill(0.0);
        for (offset, (l, r)) in l.iter_mut().zip(r).enumerate() {
            let pos = clock
                .position_at(offset as u32)
                .expect("position within span");
            let frame = clock.frame + offset as u64;
            let cycle_ticks = pos.iteration as f64 * loop_ticks as f64;
            let mut cell = libm::floor(pos.tick / window) as i64;
            let grid_tick = cell as f64 * window;
            let next_tick = grid_tick + window;
            if (pos.iteration == 0 || grid_tick > clock.loop_start as f64)
                && frame_at(&clock, grid_tick + cycle_ticks) > frame
            {
                cell = cell.saturating_sub(1);
            }
            if (loop_ticks == 0 || next_tick < clock.loop_end as f64)
                && frame_at(&clock, next_tick + cycle_ticks) <= frame
            {
                cell = cell.saturating_add(1);
            }

            let grid = (cell, pos.iteration);
            let grid_tick = cell as f64 * window;
            if retrigger || self.grid != Some(grid) {
                let mut next_tick = grid_tick + window;
                if loop_ticks != 0 {
                    next_tick = next_tick.min(clock.loop_end as f64);
                }
                let available = frame_at(&clock, next_tick + cycle_ticks).saturating_sub(frame);
                self.begin_capture(grid, available);
                retrigger = false;
            }

            let dry = [sane(*l), sane(*r)];
            self.wet = self.read(dry, ratio);
            let mix = self.mix.next();
            let gain = self.gain.next();
            let out = std::array::from_fn::<_, 2, _>(|ch| {
                let wet = gain * self.wet[ch];
                let sample = match mix {
                    0.0 => dry[ch],
                    1.0 => wet,
                    _ => (1.0 - mix) * dry[ch] + mix * wet,
                };
                flush_output_subnormal(sample)
            });
            [*l, *r] = out;
            self.meters[0] = self.meters[0].max(dry[0].abs().max(dry[1].abs()));
            self.meters[1] = self.meters[1].max(out[0].abs().max(out[1].abs()));
            self.meters[2] = (((pos.tick - grid_tick) / window).max(0.0) as f32).min(1.0);
        }
    }

    fn reset(&mut self) {
        // A fresh capture only reads its written prefix; the old buffer needn't be cleared.
        self.elapsed = 0;
        self.pending = None;
        self.previous = None;
        self.local_frame = 0;
        self.grid = None;
        self.wet = [0.0; 2];
        self.fade_from = [0.0; 2];
        self.fade_length = 0;
        self.fade_elapsed = 0;
        self.meters = [0.0; 3];
        self.settings_changed = false;
        self.mix.snap();
        self.gain.snap();
    }

    /// Block input peak, block output peak, and final sample's grid phase.
    fn meter(&self, index: u32) -> f32 {
        self.meters.get(index as usize).copied().unwrap_or(0.0)
    }
}
