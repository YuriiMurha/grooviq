//! MIDI keyboards: the player (held keys, pedal, selected channel) and the hub (plug, unplug,
//! disabled inputs), with a fake set of ports and a sink that records the engine's commands.

use grooviq_app::midi::{CommandSink, MidiHub, MidiMessage, MidiPorts, NO_CHANNEL, Player};
use grooviq_engine::{ChannelId, Command};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const NOTE_ON: u8 = 0x90;
const NOTE_OFF: u8 = 0x80;
const CONTROL: u8 = 0xB0;

fn on(channel: u32, key: u8, velocity: f32) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity,
        on: true,
    }
}

fn off(channel: u32, key: u8) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.0,
        on: false,
    }
}

fn down(key: u8, velocity: u8) -> MidiMessage {
    MidiMessage::NoteOn { key, velocity }
}

fn up(key: u8) -> MidiMessage {
    MidiMessage::NoteOff { key }
}

/// A sorted copy: the order of releases within one message is not part of the contract.
fn sorted(mut commands: Vec<Command>) -> Vec<Command> {
    commands.sort_by_key(|c| match c {
        Command::ChannelNote { channel, key, .. } => (channel.0, *key),
        _ => (u32::MAX, 0),
    });
    commands
}

#[test]
fn velocity_maps_to_0_1_linearly() {
    let mut player = Player::new();
    let mut heard = Vec::new();
    for velocity in [1u8, 32, 64, 96, 127] {
        let commands = player.handle(down(60, velocity), Some(1));
        heard.push(commands);
        player.handle(up(60), Some(1));
    }
    let expected: Vec<Vec<Command>> = [1.0, 32.0, 64.0, 96.0, 127.0]
        .iter()
        .map(|v| vec![on(1, 60, v / 127.0)])
        .collect();
    assert_eq!(heard, expected);
}

#[test]
fn sustain_holds_offs_until_the_pedal_lifts() {
    let mut player = Player::new();
    assert_eq!(player.handle(MidiMessage::Sustain(true), Some(1)), []);
    assert_eq!(
        player.handle(down(60, 100), Some(1)),
        [on(1, 60, 100.0 / 127.0)]
    );
    assert_eq!(
        player.handle(down(64, 100), Some(1)),
        [on(1, 64, 100.0 / 127.0)]
    );
    // Both keys let go: nothing releases while the pedal is down.
    assert_eq!(player.handle(up(60), Some(1)), []);
    assert_eq!(player.handle(up(64), Some(1)), []);
    assert_eq!(
        sorted(player.handle(MidiMessage::Sustain(false), Some(1))),
        [off(1, 60), off(1, 64)]
    );
    // Without the pedal a release is immediate again, and the pedal lifting says nothing more.
    player.handle(down(67, 100), Some(1));
    assert_eq!(player.handle(up(67), Some(1)), [off(1, 67)]);
    assert_eq!(player.handle(MidiMessage::Sustain(false), Some(1)), []);
}

#[test]
fn a_key_pressed_again_under_the_pedal_stays_on_until_it_is_let_go() {
    let mut player = Player::new();
    player.handle(MidiMessage::Sustain(true), Some(1));
    player.handle(down(60, 100), Some(1));
    player.handle(up(60), Some(1));
    // Pressed again while the pedal still holds the first one.
    assert_eq!(
        player.handle(down(60, 90), Some(1)),
        [on(1, 60, 90.0 / 127.0)]
    );
    assert_eq!(player.handle(MidiMessage::Sustain(false), Some(1)), []);
    assert_eq!(player.handle(up(60), Some(1)), [off(1, 60)]);
}

#[test]
fn all_notes_off_and_pedal_release_everything() {
    let mut player = Player::new();
    player.handle(down(60, 100), Some(2));
    player.handle(down(64, 100), Some(2));
    assert_eq!(
        sorted(player.handle(MidiMessage::AllNotesOff, Some(2))),
        [off(2, 60), off(2, 64)]
    );
    // Nothing left to release.
    assert_eq!(player.handle(MidiMessage::AllNotesOff, Some(2)), []);
    assert_eq!(player.handle(up(60), Some(2)), []);

    // Notes the pedal holds are released too, and the pedal stays down afterwards.
    player.handle(MidiMessage::Sustain(true), Some(2));
    player.handle(down(60, 100), Some(2));
    player.handle(up(60), Some(2));
    player.handle(down(62, 100), Some(2));
    assert_eq!(
        sorted(player.handle(MidiMessage::AllNotesOff, Some(2))),
        [off(2, 60), off(2, 62)]
    );
    player.handle(down(65, 100), Some(2));
    assert_eq!(
        player.handle(up(65), Some(2)),
        [],
        "the pedal is still down"
    );
    assert_eq!(
        player.handle(MidiMessage::Sustain(false), Some(2)),
        [off(2, 65)]
    );
}

#[test]
fn notes_follow_the_selected_channel_and_a_change_releases_the_old_one() {
    let mut player = Player::new();
    player.handle(down(60, 100), Some(1));
    player.handle(MidiMessage::Sustain(true), Some(1));
    player.handle(down(62, 100), Some(1));
    player.handle(up(62), Some(1));
    // The person selects channel 4 while one key is down and one is held by the pedal.
    assert_eq!(
        sorted(player.handle(down(65, 100), Some(4))),
        [off(1, 60), off(1, 62), on(4, 65, 100.0 / 127.0)]
    );
    // The key that was down on channel 1 is let go: the new channel hears nothing about it.
    assert_eq!(player.handle(up(60), Some(4)), []);
    // The pedal still counts: the new note is held by it.
    assert_eq!(player.handle(up(65), Some(4)), []);
    assert_eq!(
        player.handle(MidiMessage::Sustain(false), Some(4)),
        [off(4, 65)]
    );
}

#[test]
fn selecting_without_a_message_releases_the_old_channel_too() {
    let mut player = Player::new();
    player.handle(down(60, 100), Some(1));
    assert_eq!(player.select(Some(1)), []);
    assert_eq!(player.select(Some(3)), [off(1, 60)]);
    assert_eq!(player.select(Some(3)), []);
}

#[test]
fn no_channel_selected_plays_nothing() {
    let mut player = Player::new();
    assert_eq!(player.handle(down(60, 100), None), []);
    assert_eq!(player.handle(up(60), None), []);
    // Selecting none lets go of what was held.
    player.handle(down(62, 100), Some(1));
    assert_eq!(player.handle(down(64, 100), None), [off(1, 62)]);
    assert_eq!(player.handle(up(64), None), []);
}

// ---- the hub, with fake ports ----

type Callback = Box<dyn FnMut(&[u8]) + Send>;

#[derive(Default)]
struct FakeState {
    present: Vec<String>,
    open: HashMap<String, Callback>,
    opens: Vec<String>,
    refuse: Vec<String>,
    /// Callbacks of closed ports: a system can still run one that was mid-delivery.
    closed: HashMap<String, Callback>,
}

/// The ports a test plugs devices into; the hub gets a clone.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn plug(&self, name: &str) {
        self.0.lock().unwrap().present.push(name.to_owned());
    }
    fn unplug(&self, name: &str) {
        let mut state = self.0.lock().unwrap();
        state.present.retain(|n| n != name);
        // The system closes a vanished port by itself: nothing arrives from it any more.
        state.open.remove(name);
    }
    fn refuse(&self, name: &str) {
        self.0.lock().unwrap().refuse.push(name.to_owned());
    }
    /// A message the system delivers to a port after it was closed.
    fn send_late(&self, name: &str, bytes: &[u8]) {
        let mut state = self.0.lock().unwrap();
        state.closed.get_mut(name).expect("a closed port")(bytes);
    }
    fn is_open(&self, name: &str) -> bool {
        self.0.lock().unwrap().open.contains_key(name)
    }
    fn opens(&self) -> Vec<String> {
        self.0.lock().unwrap().opens.clone()
    }
    /// The keyboard sends `bytes`; `false` if the port is not open.
    fn send(&self, name: &str, bytes: &[u8]) -> bool {
        let mut state = self.0.lock().unwrap();
        match state.open.get_mut(name) {
            Some(sink) => {
                sink(bytes);
                true
            }
            None => false,
        }
    }
}

impl MidiPorts for Fake {
    fn list(&self) -> Vec<String> {
        self.0.lock().unwrap().present.clone()
    }
    fn open(&mut self, name: &str, sink: Callback) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        if state.refuse.iter().any(|n| n == name) {
            return Err("busy".to_owned());
        }
        state.opens.push(name.to_owned());
        state.open.insert(name.to_owned(), sink);
        Ok(())
    }
    fn close(&mut self, name: &str) {
        let mut state = self.0.lock().unwrap();
        if let Some(sink) = state.open.remove(name) {
            state.closed.insert(name.to_owned(), sink);
        }
    }
}

/// The engine's side: every command the keyboards sent, in order.
#[derive(Default)]
struct Recorded(Mutex<Vec<Command>>);

impl Recorded {
    fn take(&self) -> Vec<Command> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

impl CommandSink for Recorded {
    fn send(&self, cmd: Command) {
        self.0.lock().unwrap().push(cmd);
    }
}

struct Rig {
    ports: Fake,
    sent: Arc<Recorded>,
    selected: Arc<AtomicU32>,
    hub: MidiHub,
}

fn rig(disabled: &[&str]) -> Rig {
    let ports = Fake::default();
    let sent = Arc::new(Recorded::default());
    let selected = Arc::new(AtomicU32::new(1));
    let hub = MidiHub::new(
        Box::new(ports.clone()),
        Arc::clone(&sent) as Arc<dyn CommandSink>,
        Arc::clone(&selected),
        disabled.iter().map(|n| (*n).to_owned()).collect(),
    );
    Rig {
        ports,
        sent,
        selected,
        hub,
    }
}

#[test]
fn a_new_device_is_opened_on_the_next_poll() {
    let mut rig = rig(&[]);
    assert_eq!(rig.hub.poll().opened, Vec::<String>::new());
    assert!(!rig.hub.poll().changed, "nothing plugged: nothing changed");

    rig.ports.plug("Keys");
    assert!(!rig.ports.is_open("Keys"), "not before the next poll");
    let poll = rig.hub.poll();
    assert_eq!(poll.opened, ["Keys"]);
    assert!(poll.changed);
    assert!(rig.ports.is_open("Keys"));

    // It plays the selected channel, with its velocity, and a second poll opens nothing again.
    rig.ports.send("Keys", &[NOTE_ON, 60, 127]);
    assert_eq!(rig.sent.take(), [on(1, 60, 1.0)]);
    assert!(!rig.hub.poll().changed);
    assert_eq!(rig.ports.opens(), ["Keys"]);
    assert_eq!(rig.hub.inputs(), [("Keys".to_owned(), true)]);
}

#[test]
fn unplugging_a_device_releases_its_held_notes() {
    let mut rig = rig(&[]);
    rig.ports.plug("Keys");
    rig.ports.plug("Pads");
    rig.hub.poll();
    rig.ports.send("Keys", &[CONTROL, 64, 127]);
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.ports.send("Keys", &[NOTE_ON, 64, 100]);
    rig.ports.send("Keys", &[NOTE_OFF, 64, 0]);
    rig.ports.send("Pads", &[NOTE_ON, 36, 100]);
    rig.sent.take();

    rig.ports.unplug("Keys");
    let poll = rig.hub.poll();
    assert_eq!(poll.closed, ["Keys"]);
    assert!(poll.changed);
    // The key still down and the one the pedal held are both let go; the other device is not
    // touched.
    assert_eq!(sorted(rig.sent.take()), [off(1, 60), off(1, 64)]);
    assert_eq!(rig.hub.inputs(), [("Pads".to_owned(), true)]);

    // Plugged back in, it starts clean: its pedal is up again.
    rig.ports.plug("Keys");
    rig.hub.poll();
    rig.ports.send("Keys", &[NOTE_ON, 62, 100]);
    rig.ports.send("Keys", &[NOTE_OFF, 62, 0]);
    assert_eq!(rig.sent.take(), [on(1, 62, 100.0 / 127.0), off(1, 62)]);
}

#[test]
fn disabled_inputs_stay_closed() {
    let mut rig = rig(&["Keys"]);
    rig.ports.plug("Keys");
    rig.ports.plug("Pads");
    let poll = rig.hub.poll();
    assert_eq!(poll.opened, ["Pads"]);
    assert!(!rig.ports.is_open("Keys"));
    // Listed for the settings, switched off.
    assert_eq!(
        rig.hub.inputs(),
        [("Keys".to_owned(), false), ("Pads".to_owned(), true)]
    );

    // Switching one on opens it at once; switching it off closes it and lets go of its notes.
    let poll = rig.hub.set_enabled("Keys", true);
    assert_eq!(poll.opened, ["Keys"]);
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.sent.take();
    let poll = rig.hub.set_enabled("Keys", false);
    assert_eq!(poll.closed, ["Keys"]);
    assert!(!rig.ports.is_open("Keys"));
    assert_eq!(rig.sent.take(), [off(1, 60)]);
    // And it stays closed on later polls.
    rig.hub.poll();
    assert!(!rig.ports.is_open("Keys"));
}

#[test]
fn a_message_that_arrives_after_the_port_closed_plays_nothing() {
    let mut rig = rig(&[]);
    rig.ports.plug("Keys");
    rig.hub.poll();
    rig.hub.set_enabled("Keys", false);
    rig.sent.take();
    // A note-on caught mid-delivery would ring forever: nobody is left to release it.
    rig.ports.send_late("Keys", &[NOTE_ON, 60, 100]);
    assert_eq!(rig.sent.take(), []);
}

#[test]
fn a_port_that_will_not_open_is_reported_once_and_tried_again() {
    let mut rig = rig(&[]);
    rig.ports.plug("Keys");
    rig.ports.refuse("Keys");
    let first = rig.hub.poll();
    assert_eq!(first.failed, [("Keys".to_owned(), "busy".to_owned())]);
    // Still listed (and on), but not repeated on every poll.
    assert_eq!(rig.hub.poll().failed, []);
    assert_eq!(rig.hub.inputs(), [("Keys".to_owned(), true)]);
    rig.ports.0.lock().unwrap().refuse.clear();
    assert_eq!(rig.hub.poll().opened, ["Keys"]);
}

#[test]
fn a_selected_channel_change_is_let_go_by_the_next_poll_without_a_message() {
    let mut rig = rig(&[]);
    rig.ports.plug("Keys");
    rig.hub.poll();
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.sent.take();
    rig.selected.store(5, Ordering::Release);
    rig.hub.release_stale();
    assert_eq!(rig.sent.take(), [off(1, 60)]);
    // And one with no channel selected plays nothing.
    rig.selected.store(NO_CHANNEL, Ordering::Release);
    rig.ports.send("Keys", &[NOTE_ON, 62, 100]);
    assert_eq!(rig.sent.take(), []);
}

#[test]
fn the_system_ports_list_without_error_and_refuse_a_name_that_is_not_there() {
    // A runner has no MIDI devices (this machine may have some): listing must neither fail nor
    // panic, and a port that is not plugged in is refused rather than opened.
    let mut ports = grooviq_app::midi::MidirPorts::new();
    let listed = ports.list();
    let missing = "Grooviq test: no such MIDI input";
    assert!(!listed.iter().any(|name| name == missing));
    assert!(ports.open(missing, Box::new(|_| {})).is_err());
    // Closing what was never opened is not an error either.
    ports.close(missing);
}
