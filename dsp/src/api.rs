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
}

pub trait Device: Send {
    /// Sets a parameter by its `index` from devices.json, in real units (min..max).
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

    /// Host tempo, for tempo-synced devices.
    fn set_tempo(&mut self, _bpm: f32) {}

    /// Scalar meter by its position in the device's "meters" list in devices.json (0-based).
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
    10f32.powf(db / 20.0)
}

/// Linear gain to decibels (floored at -120 dB).
#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-6 { -120.0 } else { 20.0 * g.log10() }
}

/// MIDI key to frequency in Hz (A4 = 69 = 440 Hz).
#[inline]
pub fn key_to_hz(key: f32) -> f32 {
    440.0 * 2f32.powf((key - 69.0) / 12.0)
}
