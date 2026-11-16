//! The audio graph: a sine test tone and the synth, mixed into the master bus. After each node,
//! a NaN guard silences and resets a node whose block holds NaN or infinity, so one bad device
//! cannot poison the mix. Everything here runs on the audio thread except `Graph::new`.

use crate::BLOCK;
use grooviq_dsp::{Device, NoteEvent, db_to_gain};
use std::f64::consts::TAU;

/// Length of every de-click ramp: tone changes and the synth fade on `AllSoundOff`.
const RAMP_SECONDS: f32 = 0.005;

pub(crate) struct Graph {
    /// The controller's `reconfigure` count when the graph was built.
    generation: u64,
    tone: ToneNode,
    synth: SynthNode,
    /// The block of the node being rendered, checked by the guard before it joins the master.
    node_l: [f32; BLOCK],
    node_r: [f32; BLOCK],
}

impl Graph {
    /// Builds every node for `sample_rate` (allocates: call it off the audio thread).
    pub(crate) fn new(sample_rate: f32, generation: u64) -> Self {
        let ramp_len = (RAMP_SECONDS * sample_rate).round().max(1.0) as u32;
        Self {
            generation,
            tone: ToneNode::new(sample_rate, ramp_len),
            synth: SynthNode::new(sample_rate, ramp_len),
            node_l: [0.0; BLOCK],
            node_r: [0.0; BLOCK],
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn set_tone(&mut self, on: bool, hz: f32, gain_db: f32) {
        self.tone.set(on, hz, gain_db);
    }

    pub(crate) fn set_synth_param(&mut self, index: u32, value: f32) {
        self.synth.device.set_param(index, value);
    }

    /// Fades the tone and the synth out over 5 ms; the synth resets once silent.
    pub(crate) fn all_sound_off(&mut self) {
        self.tone.switch_off();
        self.synth.fade_out();
    }

    pub(crate) fn tone_on(&self) -> bool {
        self.tone.on
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn inject_nan(&mut self) {
        self.synth.nan_next = true;
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn inject_panic(&mut self) {
        self.synth.panic_next = true;
    }

    /// Renders one block into the master bus `l`/`r` (overwritten); `events` are sorted by
    /// offset. Returns how many nodes the NaN guard silenced.
    pub(crate) fn render(
        &mut self,
        l: &mut [f32; BLOCK],
        r: &mut [f32; BLOCK],
        events: &[NoteEvent],
    ) -> u32 {
        let mut silenced = 0;
        let (node_l, node_r) = (&mut self.node_l, &mut self.node_r);

        self.tone.render(node_l, node_r);
        if guard(node_l, node_r) {
            self.tone.reset();
            silenced += 1;
        }
        *l = *node_l;
        *r = *node_r;

        self.synth.render(node_l, node_r, events);
        if guard(node_l, node_r) {
            self.synth.reset();
            silenced += 1;
        }
        for (master, node) in l.iter_mut().zip(node_l.iter()) {
            *master += node;
        }
        for (master, node) in r.iter_mut().zip(node_r.iter()) {
            *master += node;
        }
        silenced
    }
}

/// Zeroes a block that holds NaN or infinity. Returns true if it had to.
fn guard(l: &mut [f32], r: &mut [f32]) -> bool {
    if l.iter().chain(r.iter()).all(|x| x.is_finite()) {
        return false;
    }
    l.fill(0.0);
    r.fill(0.0);
    true
}

/// A linear ramp that reaches its target after a fixed number of samples.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Ramp {
    fn at(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            left: 0,
        }
    }

    /// Heads for `target`, arriving after `samples` samples.
    fn start(&mut self, target: f32, samples: u32) {
        self.target = target;
        self.left = samples.max(1);
        self.step = (target - self.value) / self.left as f32;
    }

    /// The value for the next sample.
    fn tick(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }

    fn done(&self) -> bool {
        self.left == 0
    }
}

/// A sine test tone. The phase is f64 so the pitch stays exact; every change of level ramps.
struct ToneNode {
    sample_rate: f64,
    ramp_len: u32,
    on: bool,
    /// Linear gain while on.
    gain: f32,
    /// Cycles, 0..1.
    phase: f64,
    /// Cycles per sample.
    step: f64,
    level: Ramp,
}

impl ToneNode {
    fn new(sample_rate: f32, ramp_len: u32) -> Self {
        Self {
            sample_rate: f64::from(sample_rate),
            ramp_len,
            on: false,
            gain: 0.0,
            phase: 0.0,
            step: 0.0,
            level: Ramp::at(0.0),
        }
    }

    fn target(&self) -> f32 {
        if self.on { self.gain } else { 0.0 }
    }

    fn set(&mut self, on: bool, hz: f32, gain_db: f32) {
        self.on = on;
        self.step = f64::from(hz) / self.sample_rate;
        self.gain = db_to_gain(gain_db);
        self.level.start(self.target(), self.ramp_len);
    }

    fn switch_off(&mut self) {
        self.on = false;
        self.level.start(0.0, self.ramp_len);
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let level = self.level.tick();
            let s = if level == 0.0 {
                // Silent: the next start begins at phase 0.
                self.phase = 0.0;
                0.0
            } else {
                let s = (self.phase * TAU).sin() as f32 * level;
                self.phase = (self.phase + self.step).fract();
                s
            };
            *a = s;
            *b = s;
        }
    }

    /// Clears the phase and ramps back in from silence, keeping the settings.
    fn reset(&mut self) {
        self.phase = 0.0;
        self.level = Ramp::at(0.0);
        self.level.start(self.target(), self.ramp_len);
    }
}

/// The synth, plus the fade `AllSoundOff` starts.
struct SynthNode {
    device: Box<dyn Device>,
    ramp_len: u32,
    /// Output gain while fading out after `AllSoundOff`; the synth resets when it reaches 0.
    fade: Option<Ramp>,
    #[cfg(feature = "test-hooks")]
    nan_next: bool,
    #[cfg(feature = "test-hooks")]
    panic_next: bool,
}

impl SynthNode {
    fn new(sample_rate: f32, ramp_len: u32) -> Self {
        Self {
            device: grooviq_dsp::create("synth", sample_rate)
                .expect("grooviq-dsp always provides the synth"),
            ramp_len,
            fade: None,
            #[cfg(feature = "test-hooks")]
            nan_next: false,
            #[cfg(feature = "test-hooks")]
            panic_next: false,
        }
    }

    fn fade_out(&mut self) {
        if self.fade.is_none() {
            let mut fade = Ramp::at(1.0);
            fade.start(0.0, self.ramp_len);
            self.fade = Some(fade);
        }
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32], events: &[NoteEvent]) {
        #[cfg(feature = "test-hooks")]
        if std::mem::take(&mut self.panic_next) {
            panic!("{}", crate::INJECTED_PANIC);
        }
        self.device.process(l, r, None, events);
        if let Some(fade) = &mut self.fade {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let gain = fade.tick();
                *a *= gain;
                *b *= gain;
            }
            let silent = fade.done();
            if silent {
                // Every voice is released at once now that nothing can be heard.
                self.reset();
            }
        }
        #[cfg(feature = "test-hooks")]
        if std::mem::take(&mut self.nan_next) {
            l[0] = f32::NAN;
        }
    }

    fn reset(&mut self) {
        self.device.reset();
        self.fade = None;
    }
}
