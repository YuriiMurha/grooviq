//! The players (what each keyboard holds down) and the hub that opens and closes the inputs.

use super::{MidiMessage, MidiPort, MidiPorts, MidiSink, NO_CHANNEL, parse};
use grooviq_engine::{ChannelId, Command, NoteId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Where a keyboard's notes go: the engine's controller, never through the session's lock.
///
/// A channel id is not a channel: a project opened meanwhile makes a new channel with the same
/// id. So a key is pressed on a channel *instance*, and a release names the instance its press
/// reached; the sink sends it only if the channel is still that instance.
pub trait CommandSink: Send + Sync {
    /// Starts `key` on `channel` now, as note `id`. The channel instance it reached; `None` if
    /// it was not sent (no such channel, or the engine did not take it).
    fn press(&self, channel: u32, key: u8, velocity: f32, id: NoteId) -> Option<u64>;

    /// Releases the presses `ids` of `key` on `channel`, made on `instance`: the engine ends the
    /// note each id names, so every press it took needs its own. Nothing if the channel is
    /// another one by now (the engine cut those notes when it went).
    fn release(&self, channel: u32, key: u8, instance: u64, ids: &[NoteId]);

    /// Tries again what the engine refused earlier. Called after every message and by the poll.
    fn retry(&self) {}
}

/// What a player does to the engine; the hub's goes to a sink, a lone player's is collected.
trait Out {
    fn press(&mut self, channel: u32, key: u8, velocity: f32, id: NoteId) -> Option<u64>;
    fn release(&mut self, channel: u32, key: u8, instance: u64, ids: &[NoteId]);
}

struct ToSink<'a>(&'a dyn CommandSink);

impl Out for ToSink<'_> {
    fn press(&mut self, channel: u32, key: u8, velocity: f32, id: NoteId) -> Option<u64> {
        self.0.press(channel, key, velocity, id)
    }
    fn release(&mut self, channel: u32, key: u8, instance: u64, ids: &[NoteId]) {
        self.0.release(channel, key, instance, ids);
    }
}

/// Collects a lone player's commands (one instance of every channel, for ever).
#[derive(Default)]
struct Collect(Vec<Command>);

impl Out for Collect {
    fn press(&mut self, channel: u32, key: u8, velocity: f32, id: NoteId) -> Option<u64> {
        self.0.push(note(channel, key, velocity, true, id));
        Some(0)
    }
    fn release(&mut self, channel: u32, key: u8, _instance: u64, ids: &[NoteId]) {
        for &id in ids {
            self.0.push(note(channel, key, 0.0, false, id));
        }
    }
}

/// Which keyboards own each key on each engine channel, counted across all inputs, and the
/// presses the engine took of it: the engine is told to release a key only when the last
/// keyboard holding or sustaining it lets go, and then once per press it took, naming that
/// press (the engine ends the note a release names). Each key is counted for the channel
/// instance it was pressed on; owners of an earlier instance are stale and count for nothing.
#[derive(Debug, Default)]
struct Ledger {
    owners: BTreeMap<(u32, u8), Held>,
}

#[derive(Debug)]
struct Held {
    instance: u64,
    /// Inputs holding or sustaining the key.
    owners: u32,
    /// Presses the engine took that no release was sent for yet, oldest first.
    presses: Vec<NoteId>,
}

impl Ledger {
    /// The engine took press `id` of `key` on `instance`; `new_owner` if an input that did not
    /// hold it presses it.
    fn press(&mut self, channel: u32, key: u8, instance: u64, new_owner: bool, id: NoteId) {
        let held = self.owners.entry((channel, key)).or_insert(Held {
            instance,
            owners: 0,
            presses: Vec::new(),
        });
        if held.instance != instance {
            // Whoever held it before held another channel's key.
            *held = Held {
                instance,
                owners: 0,
                presses: Vec::new(),
            };
        }
        held.presses.push(id);
        held.owners += u32::from(new_owner);
    }

    /// One owner of `key` on `instance` lets go: the presses the engine is owed releases for,
    /// which is none until the last owner lets go. A stale owner (the key is now another
    /// instance's) owes nothing.
    fn release(&mut self, channel: u32, key: u8, instance: u64) -> Vec<NoteId> {
        let Some(held) = self.owners.get_mut(&(channel, key)) else {
            return Vec::new();
        };
        if held.instance != instance {
            return Vec::new();
        }
        held.owners = held.owners.saturating_sub(1);
        if held.owners > 0 {
            return Vec::new();
        }
        self.owners
            .remove(&(channel, key))
            .map(|held| held.presses)
            .unwrap_or_default()
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
    /// Keys the person holds, with the channel instance each was pressed on.
    down: BTreeMap<u8, u64>,
    /// Keys let go while the pedal was down: still sounding.
    held: BTreeMap<u8, u64>,
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
        let mut out = Collect::default();
        self.handle_in(&mut ledger, &mut out, msg, channel);
        self.solo = ledger;
        out.0
    }

    /// Moves the player to `channel`: if that is another channel, releases everything on the
    /// old one. Nothing otherwise.
    pub fn select(&mut self, channel: Option<u32>) -> Vec<Command> {
        let mut ledger = std::mem::take(&mut self.solo);
        let mut out = Collect::default();
        self.select_in(&mut ledger, &mut out, channel);
        self.solo = ledger;
        out.0
    }

    fn handle_in(
        &mut self,
        ledger: &mut Ledger,
        out: &mut dyn Out,
        msg: MidiMessage,
        channel: Option<u32>,
    ) {
        self.select_in(ledger, out, channel);
        match msg {
            MidiMessage::NoteOn { key, velocity } => {
                let id = NoteId::live();
                if let Some(channel) = channel
                    && let Some(instance) = out.press(channel, key, f32::from(velocity) / 127.0, id)
                {
                    let before = self.held.remove(&key).or(self.down.get(&key).copied());
                    self.down.insert(key, instance);
                    // Down or pedal-held on this instance already: still this player's key,
                    // only sounded again (and owed a release of this press too).
                    ledger.press(channel, key, instance, before != Some(instance), id);
                }
            }
            MidiMessage::NoteOff { key } => {
                if let Some(instance) = self.down.remove(&key) {
                    if self.pedal {
                        self.held.insert(key, instance);
                    } else if let Some(channel) = self.channel {
                        disown(ledger, out, channel, key, instance);
                    }
                }
            }
            MidiMessage::Sustain(pressed) => {
                self.pedal = pressed;
                if !pressed {
                    self.release(ledger, out, false);
                }
            }
            MidiMessage::AllNotesOff => self.release(ledger, out, true),
        }
    }

    fn select_in(&mut self, ledger: &mut Ledger, out: &mut dyn Out, channel: Option<u32>) {
        if channel != self.channel {
            self.release(ledger, out, true);
            self.channel = channel;
        }
    }

    /// The input is gone or switched off: releases everything.
    fn retire_in(&mut self, ledger: &mut Ledger, out: &mut dyn Out) {
        self.release(ledger, out, true);
    }

    /// Releases the notes the pedal holds, and with `keys` the ones still down as well.
    fn release(&mut self, ledger: &mut Ledger, out: &mut dyn Out, keys: bool) {
        let Some(channel) = self.channel else { return };
        let mut gone = std::mem::take(&mut self.held);
        if keys {
            gone.append(&mut self.down);
        }
        for (key, instance) in gone {
            disown(ledger, out, channel, key, instance);
        }
    }
}

/// One owner no longer holds `key`: the engine hears it only if no other input does, and then
/// once for every press it took.
fn disown(ledger: &mut Ledger, out: &mut dyn Out, channel: u32, key: u8, instance: u64) {
    let ids = ledger.release(channel, key, instance);
    if !ids.is_empty() {
        out.release(channel, key, instance, &ids);
    }
}

fn note(channel: u32, key: u8, velocity: f32, on: bool, id: NoteId) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity,
        on,
        id,
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
    descriptors: Vec<MidiPort>,
    overrides: BTreeMap<String, bool>,
    labels: BTreeMap<String, String>,
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
            descriptors: Vec::new(),
            overrides: BTreeMap::new(),
            labels: BTreeMap::new(),
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
        let mut descriptors = self.ports.list();
        descriptors.sort_by(|a, b| a.id.cmp(&b.id).then(a.name.cmp(&b.name)));
        descriptors.dedup_by(|a, b| a.id == b.id);
        let descriptor_changed = descriptors != self.descriptors;
        for port in &descriptors {
            if self.disabled.contains(&port.name) {
                self.overrides.entry(port.id.clone()).or_insert(false);
            }
        }
        self.descriptors = descriptors;
        let mut listed: Vec<String> = self.descriptors.iter().map(|p| p.id.clone()).collect();
        listed.sort();
        listed.dedup();
        let mut poll = Poll {
            changed: descriptor_changed || listed != self.listed,
            ..Poll::default()
        };
        self.listed = listed;
        self.labels.extend(
            self.input_views()
                .into_iter()
                .map(|(id, name, _)| (id, name)),
        );
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
        let was_enabled = self.enabled(name);
        self.overrides.insert(name.to_owned(), enabled);
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
            .map(|name| (name.clone(), self.enabled(name)))
            .collect()
    }

    pub fn with_overrides(mut self, overrides: BTreeMap<String, bool>) -> Self {
        self.overrides = overrides;
        self
    }

    pub fn input_overrides(&self) -> BTreeMap<String, bool> {
        self.overrides.clone()
    }

    fn label(&self, id: &str) -> String {
        self.input_views()
            .into_iter()
            .find(|p| p.0 == id)
            .map(|p| p.1)
            .or_else(|| self.labels.get(id).cloned())
            .unwrap_or_else(|| "Keyboard".into())
    }

    fn enabled(&self, id: &str) -> bool {
        self.overrides.get(id).copied().unwrap_or_else(|| {
            !self
                .descriptors
                .iter()
                .any(|p| p.id == id && self.disabled.contains(&p.name))
        })
    }

    /// Human labels reserve genuine names before generating suffixes.
    pub fn input_views(&self) -> Vec<(String, String, bool)> {
        let mut used: BTreeSet<String> = self.descriptors.iter().map(|p| p.name.clone()).collect();
        let mut seen = BTreeSet::new();
        self.descriptors
            .iter()
            .map(|p| {
                let mut label = p.name.clone();
                if !seen.insert(p.name.clone()) {
                    let mut n = 2;
                    loop {
                        label = format!("{} ({n})", p.name);
                        if used.insert(label.clone()) {
                            break;
                        }
                        n += 1;
                    }
                }
                (p.id.clone(), label, self.enabled(&p.id))
            })
            .collect()
    }

    /// Lets go of the notes on a channel that is no longer the selected one, without waiting for
    /// the keyboard to send anything.
    pub fn release_stale(&mut self) {
        let channel = selected(&self.selected);
        let mut bus = lock(&self.bus);
        let Bus { ledger, players } = &mut *bus;
        let mut out = ToSink(&*self.sink);
        for player in players.values_mut() {
            player.select_in(ledger, &mut out, channel);
        }
    }

    /// Closes what vanished or was switched off, opens what is plugged in and on.
    fn apply(&mut self, poll: &mut Poll) {
        let gone: Vec<String> = lock(&self.bus)
            .players
            .keys()
            .filter(|name| !self.listed.contains(name) || !self.enabled(name))
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
                    player.retire_in(ledger, &mut ToSink(&*self.sink));
                }
            }
            poll.closed.push(self.label(&name));
        }
        let wanted: Vec<String> = {
            let bus = lock(&self.bus);
            self.listed
                .iter()
                .filter(|name| self.enabled(name) && !bus.players.contains_key(*name))
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
                    poll.opened.push(self.label(&name));
                }
                // Tried again at every poll; said once.
                Err(why) => {
                    lock(&self.bus).players.remove(&name);
                    if self.failed.insert(name.clone()) {
                        poll.failed.push((self.label(&name), why));
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
            player.handle_in(ledger, &mut ToSink(&*sink), msg, channel);
            sink.retry();
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink that checks, from inside every call, that the ownership lock is still held: a
    /// timeout cannot show mutual exclusion, this does.
    #[derive(Default)]
    struct HoldsTheLock {
        bus: Mutex<Option<Arc<Mutex<Bus>>>>,
        calls: Mutex<u32>,
    }

    impl HoldsTheLock {
        fn check(&self) {
            let bus = self.bus.lock().unwrap();
            assert!(
                bus.as_ref().unwrap().try_lock().is_err(),
                "the ownership lock was released before the engine was told"
            );
            *self.calls.lock().unwrap() += 1;
        }
    }

    impl CommandSink for HoldsTheLock {
        fn press(&self, _: u32, _: u8, _: f32, _: NoteId) -> Option<u64> {
            self.check();
            Some(0)
        }
        fn release(&self, _: u32, _: u8, _: u64, _: &[NoteId]) {
            self.check();
        }
        fn retry(&self) {
            self.check();
        }
    }

    struct TwoPorts;

    impl MidiPorts for TwoPorts {
        fn list(&self) -> Vec<MidiPort> {
            vec!["Keys", "Pads"]
                .into_iter()
                .map(|name| MidiPort {
                    id: name.into(),
                    name: name.into(),
                })
                .collect()
        }
        fn open(&mut self, _: &str, _: MidiSink) -> Result<(), String> {
            Ok(())
        }
        fn close(&mut self, _: &str) {}
    }

    #[test]
    fn every_command_reaches_the_engine_with_the_ownership_lock_held() {
        let sink = Arc::new(HoldsTheLock::default());
        let selected = Arc::new(AtomicU32::new(1));
        let mut hub = MidiHub::new(
            Box::new(TwoPorts),
            Arc::clone(&sink) as Arc<dyn CommandSink>,
            Arc::clone(&selected),
            Vec::new(),
        );
        *sink.bus.lock().unwrap() = Some(Arc::clone(&hub.bus));
        hub.poll();
        let (mut keys, mut pads) = (hub.callback("Keys"), hub.callback("Pads"));
        // A press, a release and the pedal, through the callbacks.
        keys(&[0x90, 60, 100]);
        keys(&[0x80, 60, 0]);
        pads(&[0xB0, 64, 127]);
        pads(&[0x90, 62, 100]);
        pads(&[0x80, 62, 0]);
        pads(&[0xB0, 64, 0]);
        // A release from a change of channel, from unplugging (switching off) and a retry.
        keys(&[0x90, 64, 100]);
        selected.store(2, Ordering::Release);
        hub.release_stale();
        pads(&[0x90, 65, 100]);
        hub.set_enabled("Pads", false);
        hub.retry_owed();
        // Every one of them reached the sink: presses, releases and retries.
        assert!(*sink.calls.lock().unwrap() >= 12);
    }
}
