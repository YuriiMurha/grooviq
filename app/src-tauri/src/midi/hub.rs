//! The players (what each keyboard holds down) and the hub that opens and closes the inputs.

use super::{MidiMessage, MidiPorts, MidiSink, NO_CHANNEL, parse};
use grooviq_engine::{ChannelId, Command};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Where a keyboard's commands go: the engine's controller, never through the session's lock.
pub trait CommandSink: Send + Sync {
    /// Hands one command to the engine.
    fn send(&self, cmd: Command);

    /// Tries again what the engine refused earlier. Called after every message and by the poll.
    fn retry(&self) {}
}

/// Which keyboards own each key on each engine channel, counted across all inputs: the engine is
/// told to release a key only when the last keyboard holding or sustaining it lets go.
#[derive(Debug, Default)]
struct Ledger {
    owners: BTreeMap<(u32, u8), u32>,
}

impl Ledger {
    fn acquire(&mut self, channel: u32, key: u8) {
        *self.owners.entry((channel, key)).or_insert(0) += 1;
    }

    /// One owner lets go: whether it was the last.
    fn release(&mut self, channel: u32, key: u8) -> bool {
        match self.owners.get_mut(&(channel, key)) {
            Some(count) if *count > 1 => {
                *count -= 1;
                false
            }
            _ => {
                self.owners.remove(&(channel, key));
                true
            }
        }
    }
}

/// One keyboard's state: the keys down, the keys its sustain pedal holds, and the pedal.
///
/// It sounds only on one channel at a time, the one selected when the last message came. When
/// the selection changes, everything on the old channel is released first, so no note rings
/// forever on a channel the keyboard no longer plays. Runs on the MIDI thread, never the audio
/// thread.
///
/// Played on its own (`handle`, `select`) a player counts only its own keys. Under a hub every
/// input's keys are counted in one [`Ledger`], under the hub's lock (`handle_in`, ...).
#[derive(Debug, Default)]
pub struct Player {
    /// The channel the keys below sound on.
    channel: Option<u32>,
    /// Keys the person holds.
    down: BTreeSet<u8>,
    /// Keys let go while the pedal was down: still sounding.
    held: BTreeSet<u8>,
    pedal: bool,
    /// What a player played on its own counts its keys in.
    solo: Ledger,
}

impl Player {
    pub fn new() -> Self {
        Self::default()
    }

    /// The commands `msg` causes when `channel` is selected (`None`: no channel, nothing plays).
    /// A change of channel releases the old one's notes first.
    pub fn handle(&mut self, msg: MidiMessage, channel: Option<u32>) -> Vec<Command> {
        let mut ledger = std::mem::take(&mut self.solo);
        let commands = self.handle_in(&mut ledger, msg, channel);
        self.solo = ledger;
        commands
    }

    /// Moves the player to `channel`: if that is another channel, releases everything on the
    /// old one. Nothing otherwise.
    pub fn select(&mut self, channel: Option<u32>) -> Vec<Command> {
        let mut ledger = std::mem::take(&mut self.solo);
        let commands = self.select_in(&mut ledger, channel);
        self.solo = ledger;
        commands
    }

    fn handle_in(
        &mut self,
        ledger: &mut Ledger,
        msg: MidiMessage,
        channel: Option<u32>,
    ) -> Vec<Command> {
        let mut commands = self.select_in(ledger, channel);
        match msg {
            MidiMessage::NoteOn { key, velocity } => {
                if let Some(channel) = channel {
                    // Down or pedal-held already: still this player's key, only sounded again.
                    if !self.down.contains(&key) && !self.held.remove(&key) {
                        ledger.acquire(channel, key);
                    }
                    self.down.insert(key);
                    commands.push(note(channel, key, f32::from(velocity) / 127.0, true));
                }
            }
            MidiMessage::NoteOff { key } => {
                if self.down.remove(&key) {
                    if self.pedal {
                        self.held.insert(key);
                    } else if let Some(channel) = self.channel {
                        disown(ledger, channel, key, &mut commands);
                    }
                }
            }
            MidiMessage::Sustain(pressed) => {
                self.pedal = pressed;
                if !pressed {
                    self.release(ledger, &mut commands, false);
                }
            }
            MidiMessage::AllNotesOff => self.release(ledger, &mut commands, true),
        }
        commands
    }

    fn select_in(&mut self, ledger: &mut Ledger, channel: Option<u32>) -> Vec<Command> {
        let mut commands = Vec::new();
        if channel != self.channel {
            self.release(ledger, &mut commands, true);
            self.channel = channel;
        }
        commands
    }

    /// The input is gone or switched off: releases everything.
    fn retire_in(&mut self, ledger: &mut Ledger) -> Vec<Command> {
        let mut commands = Vec::new();
        self.release(ledger, &mut commands, true);
        commands
    }

    /// Releases the notes the pedal holds, and with `keys` the ones still down as well.
    fn release(&mut self, ledger: &mut Ledger, commands: &mut Vec<Command>, keys: bool) {
        let Some(channel) = self.channel else { return };
        let mut gone = std::mem::take(&mut self.held);
        if keys {
            gone.append(&mut self.down);
        }
        for key in gone {
            disown(ledger, channel, key, commands);
        }
    }
}

/// One owner no longer holds `key`: the engine hears it only if no other input does.
fn disown(ledger: &mut Ledger, channel: u32, key: u8, commands: &mut Vec<Command>) {
    if ledger.release(channel, key) {
        commands.push(note(channel, key, 0.0, false));
    }
}

fn note(channel: u32, key: u8, velocity: f32, on: bool) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity,
        on,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The selected channel, `None` if there is none.
fn selected(selected: &AtomicU32) -> Option<u32> {
    match selected.load(Ordering::Acquire) {
        NO_CHANNEL => None,
        id => Some(id),
    }
}

/// What one poll (or one switch) did, for the log and the UI.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Poll {
    /// Inputs opened.
    pub opened: Vec<String>,
    /// Inputs closed: unplugged or switched off.
    pub closed: Vec<String>,
    /// Inputs that would not open, with why: each is reported once until it opens or vanishes.
    pub failed: Vec<(String, String)>,
    /// The list of inputs the settings show, or one's switch, changed.
    pub changed: bool,
}

/// What every open input shares, under one lock: who holds which key, and each input's player.
/// A callback holds the lock from the change in ownership to the command it causes reaching the
/// engine, so no input's release can land after another's press of the same key.
#[derive(Default)]
struct Bus {
    ledger: Ledger,
    players: BTreeMap<String, Player>,
}

/// What the hub's thread does next, and how long to wait after it (see [`Schedule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Look for keyboards plugged in or out.
    Poll,
    /// Hand the engine the releases it refused.
    Retry,
}

/// How often the hub looks for keyboards plugged in or out.
pub const POLL_PERIOD: Duration = Duration::from_secs(2);
/// How often the hub hands the engine the releases it refused, between polls.
pub const RETRY_PERIOD: Duration = Duration::from_millis(100);

/// The hub thread's timetable: a poll every [`POLL_PERIOD`], a retry every [`RETRY_PERIOD`] in
/// between. Takes the time as an argument so it can be driven by a clock a test controls.
#[derive(Debug, Default)]
pub struct Schedule {
    polled: Option<Instant>,
}

impl Schedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// What to do at `now`, and how long to wait before asking again.
    pub fn step(&mut self, now: Instant) -> (Action, Duration) {
        let due = self
            .polled
            .is_none_or(|at| now.saturating_duration_since(at) >= POLL_PERIOD);
        if due {
            self.polled = Some(now);
            (Action::Poll, RETRY_PERIOD)
        } else {
            (Action::Retry, RETRY_PERIOD)
        }
    }
}

/// Owns the MIDI inputs: every enabled input that is plugged in is open, each with its own
/// [`Player`]. [`MidiHub::poll`] runs every 2 s on the `grooviq-midi` thread.
pub struct MidiHub {
    ports: Box<dyn MidiPorts>,
    sink: Arc<dyn CommandSink>,
    selected: Arc<AtomicU32>,
    /// Inputs the person switched off, by name (also those not plugged in now).
    disabled: BTreeSet<String>,
    /// Inputs plugged in at the last poll, sorted.
    listed: Vec<String>,
    /// The open inputs' players and the keys they hold.
    bus: Arc<Mutex<Bus>>,
    /// Inputs whose last open failed and was reported.
    failed: BTreeSet<String>,
}

impl MidiHub {
    pub fn new(
        ports: Box<dyn MidiPorts>,
        sink: Arc<dyn CommandSink>,
        selected: Arc<AtomicU32>,
        disabled: Vec<String>,
    ) -> Self {
        Self {
            ports,
            sink,
            selected,
            disabled: disabled.into_iter().collect(),
            listed: Vec::new(),
            bus: Arc::default(),
            failed: BTreeSet::new(),
        }
    }

    /// Does what the timetable says: a poll (and its result), or a retry of refused releases.
    pub fn run(&mut self, action: Action) -> Option<Poll> {
        match action {
            Action::Poll => Some(self.poll()),
            Action::Retry => {
                self.retry_owed();
                None
            }
        }
    }

    /// Opens the enabled inputs that appeared and closes the ones that vanished (their held
    /// notes are released).
    pub fn poll(&mut self) -> Poll {
        let mut listed = self.ports.list();
        listed.sort();
        listed.dedup();
        let mut poll = Poll {
            changed: listed != self.listed,
            ..Poll::default()
        };
        self.listed = listed;
        self.failed.retain(|name| self.listed.contains(name));
        self.apply(&mut poll);
        self.release_stale();
        poll
    }

    /// Tries again the releases the engine refused (a stalled audio thread).
    pub fn retry_owed(&self) {
        // Under the bus lock, so no callback's note-on slips in between the debt being read and
        // being paid.
        let _bus = lock(&self.bus);
        self.sink.retry();
    }

    /// Switches one input on or off, taking effect at once.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Poll {
        let was_enabled = !self.disabled.contains(name);
        if enabled {
            self.disabled.remove(name);
        } else {
            self.disabled.insert(name.to_owned());
        }
        let mut poll = Poll {
            changed: was_enabled != enabled,
            ..Poll::default()
        };
        self.apply(&mut poll);
        poll
    }

    /// The inputs plugged in at the last poll, with whether each is on.
    pub fn inputs(&self) -> Vec<(String, bool)> {
        self.listed
            .iter()
            .map(|name| (name.clone(), !self.disabled.contains(name)))
            .collect()
    }

    /// Lets go of the notes on a channel that is no longer the selected one, without waiting for
    /// the keyboard to send anything.
    pub fn release_stale(&mut self) {
        let channel = selected(&self.selected);
        let mut bus = lock(&self.bus);
        let Bus { ledger, players } = &mut *bus;
        for player in players.values_mut() {
            for cmd in player.select_in(ledger, channel) {
                self.sink.send(cmd);
            }
        }
    }

    /// Closes what vanished or was switched off, opens what is plugged in and on.
    fn apply(&mut self, poll: &mut Poll) {
        let gone: Vec<String> = lock(&self.bus)
            .players
            .keys()
            .filter(|name| !self.listed.contains(name) || self.disabled.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            self.ports.close(&name);
            {
                // The player leaves and its releases go out in one step: nothing it sends can
                // come after another input's press.
                let mut bus = lock(&self.bus);
                let Bus { ledger, players } = &mut *bus;
                if let Some(mut player) = players.remove(&name) {
                    for cmd in player.retire_in(ledger) {
                        self.sink.send(cmd);
                    }
                }
            }
            poll.closed.push(name);
        }
        let wanted: Vec<String> = {
            let bus = lock(&self.bus);
            self.listed
                .iter()
                .filter(|name| !self.disabled.contains(*name) && !bus.players.contains_key(*name))
                .cloned()
                .collect()
        };
        for name in wanted {
            // Registered first: a message that arrives while the port is still being opened
            // finds its player.
            lock(&self.bus).players.insert(name.clone(), Player::new());
            match self.ports.open(&name, self.callback(&name)) {
                Ok(()) => {
                    self.failed.remove(&name);
                    poll.opened.push(name);
                }
                // Tried again at every poll; said once.
                Err(why) => {
                    lock(&self.bus).players.remove(&name);
                    if self.failed.insert(name.clone()) {
                        poll.failed.push((name, why));
                    }
                }
            }
        }
        self.retry_owed();
    }

    /// What an input does with each message it sends.
    fn callback(&self, name: &str) -> MidiSink {
        let (name, bus, sink, selected_channel) = (
            name.to_owned(),
            Arc::clone(&self.bus),
            Arc::clone(&self.sink),
            Arc::clone(&self.selected),
        );
        Box::new(move |bytes| {
            let Some(msg) = parse(bytes) else { return };
            let channel = selected(&selected_channel);
            let mut bus = lock(&bus);
            let Bus { ledger, players } = &mut *bus;
            // An input that was closed has no player: a late message plays nothing.
            let Some(player) = players.get_mut(&name) else {
                return;
            };
            for cmd in player.handle_in(ledger, msg, channel) {
                sink.send(cmd);
            }
            sink.retry();
        })
    }
}
