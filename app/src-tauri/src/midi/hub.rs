//! The players (what each keyboard holds down) and the hub that opens and closes the inputs.

use super::{MidiMessage, MidiPorts, MidiSink, NO_CHANNEL, parse};
use grooviq_engine::{ChannelId, Command};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Where a keyboard's commands go: the engine's controller, never through the session's lock.
pub trait CommandSink: Send + Sync {
    /// Hands one command to the engine.
    fn send(&self, cmd: Command);
}

/// One keyboard's state: the keys down, the keys its sustain pedal holds, and the pedal.
///
/// It sounds only on one channel at a time, the one selected when the last message came. When
/// the selection changes, everything on the old channel is released first, so no note rings
/// forever on a channel the keyboard no longer plays. Runs on the MIDI thread, never the audio
/// thread.
#[derive(Debug, Default)]
pub struct Player {
    /// The channel the keys below sound on.
    channel: Option<u32>,
    /// Keys the person holds.
    down: BTreeSet<u8>,
    /// Keys let go while the pedal was down: still sounding.
    held: BTreeSet<u8>,
    pedal: bool,
    /// Set once the input is closed: a late message from it plays nothing.
    retired: bool,
}

impl Player {
    pub fn new() -> Self {
        Self::default()
    }

    /// The commands `msg` causes when `channel` is selected (`None`: no channel, nothing plays).
    /// A change of channel releases the old one's notes first.
    pub fn handle(&mut self, msg: MidiMessage, channel: Option<u32>) -> Vec<Command> {
        if self.retired {
            return Vec::new();
        }
        let mut commands = self.select(channel);
        match msg {
            MidiMessage::NoteOn { key, velocity } => {
                if let Some(channel) = channel {
                    self.held.remove(&key);
                    self.down.insert(key);
                    commands.push(note(channel, key, f32::from(velocity) / 127.0, true));
                }
            }
            MidiMessage::NoteOff { key } => {
                if self.down.remove(&key) {
                    if self.pedal {
                        self.held.insert(key);
                    } else if let Some(channel) = self.channel {
                        commands.push(note(channel, key, 0.0, false));
                    }
                }
            }
            MidiMessage::Sustain(pressed) => {
                self.pedal = pressed;
                if !pressed {
                    self.release(&mut commands, false);
                }
            }
            MidiMessage::AllNotesOff => self.release(&mut commands, true),
        }
        commands
    }

    /// Moves the player to `channel`: if that is another channel, releases everything on the
    /// old one. Nothing otherwise.
    pub fn select(&mut self, channel: Option<u32>) -> Vec<Command> {
        let mut commands = Vec::new();
        if channel != self.channel {
            self.release(&mut commands, true);
            self.channel = channel;
        }
        commands
    }

    /// The input is gone or switched off: releases everything and ignores what still arrives.
    pub fn retire(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        self.release(&mut commands, true);
        self.retired = true;
        commands
    }

    /// Releases the notes the pedal holds, and with `keys` the ones still down as well.
    fn release(&mut self, commands: &mut Vec<Command>, keys: bool) {
        let Some(channel) = self.channel else { return };
        if keys {
            commands.extend(self.down.iter().map(|&k| note(channel, k, 0.0, false)));
            self.down.clear();
        }
        commands.extend(self.held.iter().map(|&k| note(channel, k, 0.0, false)));
        self.held.clear();
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
    open: BTreeMap<String, Arc<Mutex<Player>>>,
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
            open: BTreeMap::new(),
            failed: BTreeSet::new(),
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
        for player in self.open.values() {
            let mut player = lock(player);
            for cmd in player.select(channel) {
                self.sink.send(cmd);
            }
        }
    }

    /// Closes what vanished or was switched off, opens what is plugged in and on.
    fn apply(&mut self, poll: &mut Poll) {
        let gone: Vec<String> = self
            .open
            .keys()
            .filter(|name| !self.listed.contains(name) || self.disabled.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            self.ports.close(&name);
            if let Some(player) = self.open.remove(&name) {
                // Under the player's lock, so nothing it sends interleaves with the releases.
                let mut player = lock(&player);
                for cmd in player.retire() {
                    self.sink.send(cmd);
                }
            }
            poll.closed.push(name);
        }
        let wanted: Vec<String> = self
            .listed
            .iter()
            .filter(|name| !self.disabled.contains(*name) && !self.open.contains_key(*name))
            .cloned()
            .collect();
        for name in wanted {
            let player = Arc::new(Mutex::new(Player::new()));
            match self.ports.open(&name, self.callback(&player)) {
                Ok(()) => {
                    self.failed.remove(&name);
                    self.open.insert(name.clone(), player);
                    poll.opened.push(name);
                }
                // Tried again at every poll; said once.
                Err(why) => {
                    if self.failed.insert(name.clone()) {
                        poll.failed.push((name, why));
                    }
                }
            }
        }
    }

    /// What an input does with each message it sends.
    fn callback(&self, player: &Arc<Mutex<Player>>) -> MidiSink {
        let (player, sink, selected_channel) = (
            Arc::clone(player),
            Arc::clone(&self.sink),
            Arc::clone(&self.selected),
        );
        Box::new(move |bytes| {
            let Some(msg) = parse(bytes) else { return };
            let channel = selected(&selected_channel);
            let mut player = lock(&player);
            for cmd in player.handle(msg, channel) {
                sink.send(cmd);
            }
        })
    }
}
