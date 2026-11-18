//! The device interface shared by the engine, the DSP crate and the wasm glue. Do not change
//! existing signatures; additions are fine.

/// Largest block the engine passes to `process` (the Web Audio render quantum is 128).
pub const MAX_BLOCK: usize = 128;

/// A note event inside the current block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEvent {
    /// Sample offset inside the block (0..len).
    pub offset: u32,
    /// MIDI key 0..127.
    pub key: u8,
    /// 0..1.
    pub velocity: f32,
    /// true = note on, false = note off.
    pub on: bool,
    /// FL-style slide note: glide from the previous note's pitch to this one (mono/legato voices).
    pub slide: bool,
    /// The note's identity, the same on its note-on and its note-off: a note-off ends the note
    /// whose note-on carried its id (and key), and nothing if that note never started or has
    /// already ended. Notes held at once need distinct ids.
    pub id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDomain {
    Pattern,
    Song,
}

/// Canonical host time map. Equality tokens carry no ordering guarantee.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostTransport {
    pub domain: HostDomain,
    pub frame: u64,
    pub frames: u32,
    pub sample_rate: f32,
    pub bpm: f32,
    pub playing: bool,
    pub anchor_frame: u64,
    pub anchor_tick: f64,
    pub shift: u64,
    pub loop_start: u64,
    pub loop_end: u64,
    pub stream_generation: u64,
    pub discontinuity: u64,
    pub changed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostPosition {
    pub unwrapped_tick: f64,
    pub tick: f64,
    pub iteration: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostEdge {
    pub offset: u32,
    pub iteration: u64,
    pub discontinuity: u64,
}
impl HostTransport {
    /// Queries sample positions and the exclusive end boundary; no event is consumed.
    pub fn position_at(&self, offset: u32) -> Option<HostPosition> {
        if offset > self.frames {
            return None;
        }
        let frames_per_tick = f64::from(self.sample_rate) * 60.0 / (f64::from(self.bpm) * 960.0);
        let elapsed = if self.playing {
            (self.frame + u64::from(offset)) as f64 - self.anchor_frame as f64
        } else {
            0.0
        };
        let unwrapped_tick = self.anchor_tick + elapsed / frames_per_tick + self.shift as f64;
        let (tick, iteration) =
            if self.loop_end > self.loop_start && unwrapped_tick >= self.loop_end as f64 {
                let length = (self.loop_end - self.loop_start) as f64;
                let iteration = ((unwrapped_tick - self.loop_start as f64) / length).floor() as u64;
                (
                    self.loop_start as f64 + (unwrapped_tick - self.loop_start as f64) % length,
                    iteration,
                )
            } else {
                (unwrapped_tick, 0)
            };
        Some(HostPosition {
            unwrapped_tick,
            tick,
            iteration,
        })
    }
    /// Every wrap is observable. At 400 BPM/8 kHz a one-tick loop is 1.25 frames,
    /// so at most one edge falls on a sample. Iteration changes and the initial
    /// discontinuity share one marker when coincident. MAX_BLOCK markers suffice.
    pub fn edges(&self) -> impl Iterator<Item = HostEdge> + '_ {
        (0..self.frames).filter_map(|offset| {
            let position = self.position_at(offset)?;
            let previous = if offset > 0 {
                self.position_at(offset - 1)?.iteration
            } else if self.frame > 0 {
                let mut prior = *self;
                prior.frame -= 1;
                prior.position_at(0)?.iteration
            } else {
                0
            };
            (position.iteration != previous || (offset == 0 && self.changed)).then_some(HostEdge {
                offset,
                iteration: position.iteration,
                discontinuity: self.discontinuity,
            })
        })
    }
    /// A split preserves the canonical anchor and absolute arithmetic.
    pub fn span(&self, offset: u32, frames: u32) -> Option<Self> {
        if offset.checked_add(frames)? > self.frames {
            return None;
        }
        Some(Self {
            frame: self.frame + u64::from(offset),
            frames,
            changed: self.changed && offset == 0,
            ..*self
        })
    }
}

pub trait Device: Send {
    /// Facts for the immediately following actual process span; default preserves old kernels.
    fn set_transport(&mut self, _transport: &HostTransport) {}

    /// Sets a parameter by its `index` in the device's `SPEC` table, in real units (min..max).
    fn set_param(&mut self, index: u32, value: f32);

    /// Processes one block of `l.len()` frames (<= MAX_BLOCK).
    /// Effects read `l`/`r` and overwrite them with output.
    /// Instruments ignore the input and overwrite `l`/`r` with their output.
    /// `sidechain` is the external sidechain signal when routed (compressor), else None.
    /// `events` are sorted by offset; effects ignore them.
    fn process(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        sidechain: Option<(&[f32], &[f32])>,
        events: &[NoteEvent],
    );

    /// Clears all internal state (voices, delay lines, envelopes).
    fn reset(&mut self);

    /// Cuts every sounding note (a choke group): each fades out linearly over 3 ms, then frees
    /// its voice. Notes that arrive after the call are unaffected. Effects ignore it.
    fn choke(&mut self) {}

    /// Host tempo, for tempo-synced devices.
    fn set_tempo(&mut self, _bpm: f32) {}

    /// Scalar meter by its position in the device's meter list (0-based).
    fn meter(&self, _index: u32) -> f32 {
        0.0
    }

    /// Array meter (scope, spectrum, playheads) by its position in "meters"; writes into `out`
    /// and returns the number of values written.
    fn meter_array(&self, _index: u32, _out: &mut [f32]) -> usize {
        0
    }
}

/// Decibels to linear gain.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    libm::powf(10.0, db / 20.0)
}

/// Linear gain to decibels (floored at -120 dB).
#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-6 {
        -120.0
    } else {
        20.0 * libm::log10f(g)
    }
}

/// MIDI key to frequency in Hz (A4 = 69 = 440 Hz).
#[inline]
pub fn key_to_hz(key: f32) -> f32 {
    440.0 * libm::powf(2.0, (key - 69.0) / 12.0)
}
