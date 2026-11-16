//! The control-thread half: validates commands, mirrors the state they set, frees what the
//! processor hands back and rebuilds the graph after a panic. May allocate and block; never
//! call it from the audio thread.

use crate::EngineConfig;
use crate::command::{Command, Msg};
use crate::graph::Graph;
use crate::live::{LiveShared, LiveSnapshot};
use crate::processor::AudioProcessor;
use rtrb::{Consumer, Producer};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// What [`Controller::check_health`] found since its previous call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthEvent {
    /// The graph panicked and was rebuilt (by `check_health`, or by `reconfigure` if it came
    /// first) from the controller's state, or with default parameters if replaying that state
    /// panicked too; `faults` counts every panic so far.
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

impl Default for ToneState {
    fn default() -> Self {
        Self {
            on: false,
            hz: 440.0,
            gain_db: -12.0,
        }
    }
}

/// Every synth parameter at its default.
fn synth_defaults() -> Vec<f32> {
    grooviq_dsp::param_spec("synth")
        .unwrap_or_default()
        .iter()
        .map(|&(_, _, default)| default)
        .collect()
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
    /// Counts `reconfigure` calls. Every graph carries the generation it was built in, and the
    /// processor drops a replacement from an earlier one.
    generation: u64,
    /// Faults already answered with a restart (queued by `check_health`, or done by
    /// `reconfigure`).
    seen_faults: u32,
    /// A restart happened that `check_health` has not reported yet.
    restart_owed: bool,
    seen_guard_hits: u32,
    /// Test switch: replaying the state panics, as a device bug in `set_param` would.
    #[cfg(test)]
    replay_panics: bool,
}

impl Controller {
    pub(crate) fn new(
        sample_rate: f32,
        commands: Producer<Msg>,
        trash: Consumer<Box<Graph>>,
        live: Arc<LiveShared>,
    ) -> Self {
        Self {
            commands,
            trash,
            live,
            sample_rate,
            tone: ToneState::default(),
            synth: synth_defaults(),
            generation: 0,
            seen_faults: 0,
            restart_owed: false,
            seen_guard_hits: 0,
            #[cfg(test)]
            replay_panics: false,
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

    /// The rate the engine runs at (after clamping): the output layer compares it with a
    /// device's before deciding to reconfigure.
    pub(crate) fn sample_rate(&self) -> f32 {
        self.sample_rate
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
            let graph = self.build_graph();
            if self.commands.push(Msg::ReplaceGraph(graph)).is_ok() {
                self.seen_faults = live.faults;
                self.restart_owed = true;
            }
        }
        if std::mem::take(&mut self.restart_owed) {
            events.push(HealthEvent::Restarted {
                faults: self.seen_faults,
            });
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
    /// state (which also recovers a graph that panicked; the next `check_health` still reports
    /// the panic). Call it with the audio callback stopped (it needs the processor itself),
    /// never on the audio thread.
    pub fn reconfigure(&mut self, processor: &mut AudioProcessor, config: EngineConfig) {
        self.sample_rate = config.rate();
        // Replacements queued before now are stale: the processor drops them.
        self.generation += 1;
        // The fresh graph also recovers from every fault so far, so `check_health` must not
        // restart it again; a fault it has not reported yet is still owed. (The processor
        // publishes its count after every block, and its callback is stopped now.)
        let faults = self.live.snapshot().faults;
        if faults != self.seen_faults {
            self.seen_faults = faults;
            self.restart_owed = true;
        }
        let graph = self.build_graph();
        processor.restart(self.sample_rate, graph);
        self.free_trash();
    }

    /// A graph for the current sample rate and generation, in the state the queued commands
    /// lead to. If replaying that state panics (a device bug in `set_param`), the graph keeps
    /// its default parameters and the state is reset to match, so a restart still happens, the
    /// panic stays off the caller (and any mutex it holds), and it cannot recur.
    pub(crate) fn build_graph(&mut self) -> Box<Graph> {
        match catch_unwind(AssertUnwindSafe(|| self.replay())) {
            Ok(graph) => graph,
            Err(_) => {
                self.tone = ToneState::default();
                self.synth = synth_defaults();
                Box::new(Graph::new(self.sample_rate, self.generation))
            }
        }
    }

    fn replay(&self) -> Box<Graph> {
        let mut graph = Box::new(Graph::new(self.sample_rate, self.generation));
        #[cfg(test)]
        if self.replay_panics {
            panic!("replaying the controller's state panicked (test switch)");
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine;

    fn controller() -> (Controller, AudioProcessor) {
        engine(EngineConfig {
            sample_rate: 48_000.0,
        })
    }

    /// Publishes what the processor would after `faults` panics and `guard_hits` NaN blocks
    /// (the processor itself is never run, so nothing overwrites it).
    fn pretend(c: &Controller, faults: u32, guard_hits: u32) {
        c.live
            .publish([0.0; 2], false, false, 0, faults, guard_hits);
    }

    #[test]
    fn each_health_event_is_reported_once_with_one_restart_per_fault() {
        let (mut c, _p) = controller();
        let free = c.commands.slots();
        pretend(&c, 1, 2);
        assert_eq!(
            c.check_health(),
            [
                HealthEvent::Restarted { faults: 1 },
                HealthEvent::DeviceSilenced { total: 2 }
            ]
        );
        assert_eq!(c.commands.slots(), free - 1, "one replacement queued");
        assert!(c.check_health().is_empty());
        assert_eq!(c.commands.slots(), free - 1, "and no second one");
    }

    #[test]
    fn a_rebuild_that_panics_falls_back_to_defaults_and_still_restarts() {
        let (mut c, mut p) = controller();
        let tone = Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        };
        c.send(tone).unwrap();
        let cutoff = Command::SetSynthParam {
            index: 20,
            value: 300.0,
        };
        c.send(cutoff).unwrap();
        c.replay_panics = true;
        pretend(&c, 1, 0);
        assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
        // The replacement plays with default parameters, and the state now says so too.
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        assert!(!c.live().tone);
        assert!(!c.tone.on);
        let default_cutoff = grooviq_dsp::param_spec("synth").unwrap()[20].2;
        assert_eq!(c.synth[20], default_cutoff);
    }

    #[test]
    fn reconfigure_makes_restarts_queued_before_it_stale() {
        let (mut c, mut p) = controller();
        pretend(&c, 1, 0);
        // Queues a replacement built with the tone off.
        assert_eq!(c.check_health(), [HealthEvent::Restarted { faults: 1 }]);
        let tone = Command::TestTone {
            on: true,
            hz: 440.0,
            gain_db: -12.0,
        };
        c.send(tone).unwrap();
        // Same rate: only the generation tells the queued graph is stale.
        c.reconfigure(
            &mut p,
            EngineConfig {
                sample_rate: 48_000.0,
            },
        );
        p.process(&mut [0.0; crate::BLOCK * 2], 2);
        // The queued graph came back unused; the one reconfigure built (tone on) plays.
        let returned = c.trash.pop().expect("a graph handed back");
        assert!(!returned.tone_on());
        assert!(c.live().tone);
    }

    #[test]
    fn an_unknown_preset_or_a_full_queue_sends_nothing() {
        let (mut c, _p) = controller();
        let free = c.commands.slots();
        assert!(!c.synth_preset("nope"));
        assert_eq!(c.commands.slots(), free);
        // Room for 39 commands, one short of a preset: all or nothing.
        while c.commands.slots() > 39 {
            c.send(Command::Play).unwrap();
        }
        let before = c.synth.clone();
        assert!(!c.synth_preset("Reese"));
        assert_eq!(c.commands.slots(), 39);
        assert_eq!(c.synth, before);
    }
}
