//! The control-thread half: validates commands, mirrors the state they set, frees what the
//! processor hands back and rebuilds the graph after a panic. May allocate and block; never
//! call it from the audio thread.

use crate::EngineConfig;
use crate::command::{Command, Msg};
use crate::graph::Graph;
use crate::live::{LiveShared, LiveSnapshot};
use crate::processor::AudioProcessor;
use rtrb::{Consumer, Producer};
use std::sync::Arc;

/// What [`Controller::check_health`] found since its previous call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthEvent {
    /// The graph panicked and was rebuilt from the controller's state; `faults` counts every
    /// panic so far.
    Restarted { faults: u32 },
    /// The NaN guard silenced and reset a node; `total` counts every such block so far.
    DeviceSilenced { total: u32 },
}

/// Why a command was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendError {
    /// A value was not finite, or a key or parameter index does not exist.
    Rejected,
    /// The queue had no room.
    Full,
}

/// The test tone as last set.
#[derive(Clone, Copy, Debug)]
struct ToneState {
    on: bool,
    hz: f32,
    gain_db: f32,
}

/// Sends commands to the [`AudioProcessor`] and watches its health. Created by
/// [`crate::engine`].
pub struct Controller {
    commands: Producer<Msg>,
    trash: Consumer<Box<Graph>>,
    live: Arc<LiveShared>,
    sample_rate: f32,
    /// The state the queued commands lead to, to rebuild the graph from.
    tone: ToneState,
    synth: Vec<f32>,
    seen_faults: u32,
    seen_guard_hits: u32,
}

impl Controller {
    pub(crate) fn new(
        sample_rate: f32,
        commands: Producer<Msg>,
        trash: Consumer<Box<Graph>>,
        live: Arc<LiveShared>,
    ) -> Self {
        let synth = grooviq_dsp::param_spec("synth")
            .unwrap_or_default()
            .iter()
            .map(|&(_, _, default)| default)
            .collect();
        Self {
            commands,
            trash,
            live,
            sample_rate,
            tone: ToneState {
                on: false,
                hz: 440.0,
                gain_db: -12.0,
            },
            synth,
            seen_faults: 0,
            seen_guard_hits: 0,
        }
    }

    /// Queues `cmd` for the next block, with its values clamped into range. Gives it back if
    /// it is rejected (a value not finite, an unknown key or parameter index) or the queue is
    /// full.
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        self.send_at(0, cmd).map_err(|_| cmd)
    }

    /// Queues `cmd` for the block that contains frame `due` (0: the next block). The processor
    /// holds back everything behind a command that is not due yet, so send in frame order.
    pub(crate) fn send_at(&mut self, due: u64, cmd: Command) -> Result<(), SendError> {
        let cmd = cmd.clamped().ok_or(SendError::Rejected)?;
        self.commands
            .push(Msg::Command { due, cmd })
            .map_err(|_| SendError::Full)?;
        self.mirror(cmd);
        Ok(())
    }

    /// Sends a factory preset as `SetSynthParam` for every synth parameter. False if the name
    /// is unknown or the queue has no room for the whole preset (then nothing is sent).
    pub fn synth_preset(&mut self, name: &str) -> bool {
        let Some(params) = grooviq_dsp::synth::preset(name) else {
            return false;
        };
        // The queue only gains room while we push, so this check holds for the whole loop.
        if self.commands.slots() < params.len() {
            return false;
        }
        params
            .into_iter()
            .all(|(index, value)| self.send(Command::SetSynthParam { index, value }).is_ok())
    }

    /// The latest live state.
    pub fn live(&self) -> LiveSnapshot {
        self.live.snapshot()
    }

    /// The live state's atomics, for readers on other threads.
    pub fn live_shared(&self) -> Arc<LiveShared> {
        Arc::clone(&self.live)
    }

    /// Frees graphs the processor replaced, restarts the graph after a panic and reports what
    /// happened since the previous call. Call it every 250 ms from a control thread.
    pub fn check_health(&mut self) -> Vec<HealthEvent> {
        self.free_trash();
        let live = self.live.snapshot();
        let mut events = Vec::new();
        if live.faults != self.seen_faults {
            // The processor stays silent until it gets a fresh graph. If the queue is full,
            // the next call tries again.
            if self
                .commands
                .push(Msg::ReplaceGraph(self.build_graph()))
                .is_ok()
            {
                self.seen_faults = live.faults;
                events.push(HealthEvent::Restarted {
                    faults: live.faults,
                });
            }
        }
        if live.guard_hits != self.seen_guard_hits {
            self.seen_guard_hits = live.guard_hits;
            events.push(HealthEvent::DeviceSilenced {
                total: live.guard_hits,
            });
        }
        events
    }

    /// Switches the engine to a new sample rate, rebuilding the graph from the controller's
    /// state. Call it with the audio callback stopped (it needs the processor itself), never
    /// on the audio thread.
    pub fn reconfigure(&mut self, processor: &mut AudioProcessor, config: EngineConfig) {
        self.sample_rate = config.rate();
        processor.restart(self.sample_rate, self.build_graph());
        self.free_trash();
    }

    /// A graph for the current sample rate in the state the queued commands lead to.
    pub(crate) fn build_graph(&self) -> Box<Graph> {
        let mut graph = Box::new(Graph::new(self.sample_rate));
        let ToneState { on, hz, gain_db } = self.tone;
        graph.set_tone(on, hz, gain_db);
        for (index, &value) in (0u32..).zip(&self.synth) {
            graph.set_synth_param(index, value);
        }
        graph
    }

    fn mirror(&mut self, cmd: Command) {
        match cmd {
            Command::TestTone { on, hz, gain_db } => self.tone = ToneState { on, hz, gain_db },
            Command::AllSoundOff => self.tone.on = false,
            Command::SetSynthParam { index, value } => {
                if let Some(param) = self.synth.get_mut(index as usize) {
                    *param = value;
                }
            }
            _ => {}
        }
    }

    fn free_trash(&mut self) {
        while self.trash.pop().is_ok() {}
    }
}
