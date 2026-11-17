//! MIDI keyboards: the player (held keys, pedal, selected channel) and the hub (plug, unplug,
//! disabled inputs), with a fake set of ports and a sink that records the engine's commands.

use grooviq_app::midi::{CommandSink, MidiHub, MidiMessage, MidiPorts, NO_CHANNEL};
use grooviq_engine::{ChannelId, Command, NoteId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const NOTE_ON: u8 = 0x90;
const NOTE_OFF: u8 = 0x80;
const CONTROL: u8 = 0xB0;

/// A note command with no id: what [`plain`] makes of one, for tests that look past ids.
fn on(channel: u32, key: u8, velocity: f32) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity,
        on: true,
        id: NoteId(0),
    }
}

fn off(channel: u32, key: u8) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.0,
        on: false,
        id: NoteId(0),
    }
}

/// The note's id, or the command itself if it is no note.
fn id_of(cmd: Command) -> NoteId {
    match cmd {
        Command::ChannelNote { id, .. } => id,
        other => panic!("not a note: {other:?}"),
    }
}

/// The commands with their note ids cleared.
fn plain(commands: Vec<Command>) -> Vec<Command> {
    commands
        .into_iter()
        .map(|c| match c {
            Command::ChannelNote {
                channel,
                key,
                velocity,
                on,
                ..
            } => Command::ChannelNote {
                channel,
                key,
                velocity,
                on,
                id: NoteId(0),
            },
            other => other,
        })
        .collect()
}

/// A player whose commands come without ids, to compare with [`on`] and [`off`]; `raw` keeps
/// them.
#[derive(Default)]
struct Player(grooviq_app::midi::Player);

impl Player {
    fn new() -> Self {
        Self::default()
    }

    fn handle(&mut self, msg: MidiMessage, channel: Option<u32>) -> Vec<Command> {
        plain(self.raw(msg, channel))
    }

    fn raw(&mut self, msg: MidiMessage, channel: Option<u32>) -> Vec<Command> {
        self.0.handle(msg, channel)
    }

    fn select(&mut self, channel: Option<u32>) -> Vec<Command> {
        plain(self.0.select(channel))
    }
}

fn down(key: u8, velocity: u8) -> MidiMessage {
    MidiMessage::NoteOn { key, velocity }
}

fn up(key: u8) -> MidiMessage {
    MidiMessage::NoteOff { key }
}

/// The id these tests give a key's one press on the sink.
fn press_id(key: u8) -> NoteId {
    NoteId(u64::from(key))
}

/// Lets go of a key's press [`press_id`] on the channel as it is now.
fn release_now(
    sink: &grooviq_app::midi::ControllerSink,
    controller: &Mutex<grooviq_engine::Controller>,
    channel: u32,
    key: u8,
) {
    let instance = controller
        .lock()
        .unwrap()
        .channel_instance(ChannelId(channel))
        .expect("a channel");
    sink.release(channel, key, instance, &[press_id(key)]);
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
    // The engine took two presses of the key, so it is owed two releases.
    assert_eq!(player.handle(up(60), Some(1)), [off(1, 60), off(1, 60)]);
}

#[test]
fn every_release_names_the_press_it_ends() {
    // Pressed, let go under the pedal, pressed again: two notes on one key, each with an id of
    // its own, and the two releases name exactly those, oldest first.
    let mut player = Player::new();
    player.raw(MidiMessage::Sustain(true), Some(1));
    let first = player.raw(down(60, 100), Some(1));
    player.raw(up(60), Some(1));
    let second = player.raw(down(60, 90), Some(1));
    player.raw(MidiMessage::Sustain(false), Some(1));
    let offs = player.raw(up(60), Some(1));
    let ids = |c: &[Command]| c.iter().map(|&c| id_of(c)).collect::<Vec<_>>();
    let pressed = [ids(&first), ids(&second)].concat();
    assert_eq!(pressed.len(), 2);
    assert_ne!(pressed[0], pressed[1], "each press is a note of its own");
    assert_eq!(ids(&offs), pressed);
    // A new press is a new note again.
    let third = player.raw(down(60, 100), Some(1));
    assert!(!pressed.contains(&id_of(third[0])));
    assert_eq!(ids(&player.raw(up(60), Some(1))), ids(&third));
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
        plain(self.take_raw())
    }

    fn take_raw(&self) -> Vec<Command> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

impl Recorded {
    fn push(&self, cmd: Command) {
        self.0.lock().unwrap().push(cmd);
    }
}

impl CommandSink for Recorded {
    fn press(&self, channel: u32, key: u8, velocity: f32, id: NoteId) -> Option<u64> {
        self.push(with_id(on(channel, key, velocity), id));
        Some(0)
    }
    fn release(&self, channel: u32, key: u8, _instance: u64, ids: &[NoteId]) {
        for &id in ids {
            self.push(with_id(off(channel, key), id));
        }
    }
}

fn with_id(cmd: Command, with: NoteId) -> Command {
    match cmd {
        Command::ChannelNote {
            channel,
            key,
            velocity,
            on,
            ..
        } => Command::ChannelNote {
            channel,
            key,
            velocity,
            on,
            id: with,
        },
        other => other,
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

// ---- keys several keyboards hold at once ----

/// A hub with two keyboards plugged in and the sink emptied.
fn two_keyboards() -> Rig {
    let mut rig = rig(&[]);
    rig.ports.plug("Keys");
    rig.ports.plug("Pads");
    rig.hub.poll();
    rig
}

#[test]
fn a_key_another_keyboard_sustains_is_not_released_by_the_first() {
    let rig = two_keyboards();
    // Keys sustains C4; Pads presses and lets go of the same key.
    rig.ports.send("Keys", &[CONTROL, 64, 127]);
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.ports.send("Keys", &[NOTE_OFF, 60, 0]);
    rig.ports.send("Pads", &[NOTE_ON, 60, 100]);
    rig.ports.send("Pads", &[NOTE_OFF, 60, 0]);
    let heard = rig.sent.take();
    assert!(
        !heard.contains(&off(1, 60)),
        "silenced Keys' note: {heard:?}"
    );
    // The pedal lifting is the last owner letting go: one release for each press.
    rig.ports.send("Keys", &[CONTROL, 64, 0]);
    assert_eq!(rig.sent.take(), [off(1, 60), off(1, 60)]);
}

#[test]
fn the_last_keyboard_to_let_go_releases_the_key_for_every_press() {
    let rig = two_keyboards();
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.ports.send("Pads", &[NOTE_ON, 60, 100]);
    let presses: Vec<NoteId> = rig.sent.take_raw().into_iter().map(id_of).collect();
    rig.ports.send("Pads", &[NOTE_OFF, 60, 0]);
    assert_eq!(rig.sent.take(), [], "Keys still holds it");
    rig.ports.send("Keys", &[NOTE_OFF, 60, 0]);
    // Two keyboards pressed it: the engine took two presses and is owed a release of each.
    let releases = rig.sent.take_raw();
    assert_eq!(plain(releases.clone()), [off(1, 60), off(1, 60)]);
    assert_eq!(releases.into_iter().map(id_of).collect::<Vec<_>>(), presses);
    assert_ne!(presses[0], presses[1]);
    // Different keys never share an owner.
    rig.ports.send("Keys", &[NOTE_ON, 62, 100]);
    rig.ports.send("Pads", &[NOTE_ON, 64, 100]);
    rig.sent.take();
    rig.ports.send("Pads", &[NOTE_OFF, 64, 0]);
    assert_eq!(rig.sent.take(), [off(1, 64)]);
}

#[test]
fn unplugging_or_disabling_one_keyboard_keeps_the_key_the_other_holds() {
    let mut rig = two_keyboards();
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.ports.send("Pads", &[NOTE_ON, 60, 100]);
    rig.ports.send("Pads", &[NOTE_ON, 64, 100]);
    rig.sent.take();

    rig.ports.unplug("Keys");
    rig.hub.poll();
    assert_eq!(rig.sent.take(), [], "Pads still holds both keys");
    rig.hub.set_enabled("Pads", false);
    assert_eq!(
        sorted(rig.sent.take()),
        [off(1, 60), off(1, 60), off(1, 64)]
    );
}

#[test]
fn a_channel_change_releases_a_shared_key_for_every_press() {
    let mut rig = two_keyboards();
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.ports.send("Pads", &[NOTE_ON, 60, 100]);
    rig.sent.take();
    rig.selected.store(5, Ordering::Release);
    rig.hub.release_stale();
    assert_eq!(rig.sent.take(), [off(1, 60), off(1, 60)]);
    // The same key on the new channel is its own key.
    rig.ports.send("Keys", &[NOTE_ON, 60, 100]);
    rig.ports.send("Keys", &[NOTE_OFF, 60, 0]);
    assert_eq!(rig.sent.take(), [on(5, 60, 100.0 / 127.0), off(5, 60)]);
}

// ---- the engine's queue, for real ----

mod engine_queue {
    use super::*;
    use grooviq_app::midi::ControllerSink;
    use grooviq_engine::{ChannelMix, ChannelSetup, Controller, EngineConfig, RecordEvent, engine};

    /// A real controller and processor with channel 1, recording while playing.
    fn recording_engine() -> (Arc<Mutex<Controller>>, grooviq_engine::AudioProcessor) {
        let (mut controller, mut processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        controller
            .add_channel(ChannelSetup::sampler(
                ChannelId(1),
                None,
                Vec::new(),
                ChannelMix::default(),
            ))
            .unwrap();
        controller.send(Command::SetRecording { on: true }).unwrap();
        controller.send(Command::Play).unwrap();
        run(&mut processor, 4);
        (Arc::new(Mutex::new(controller)), processor)
    }

    fn run(processor: &mut grooviq_engine::AudioProcessor, blocks: usize) {
        for _ in 0..blocks {
            processor.process(&mut [0.0; 512], 2);
        }
    }

    /// Everything the audio thread recorded so far, as (key, on).
    fn heard(controller: &Mutex<Controller>) -> Vec<(u8, bool)> {
        let mut events = Vec::new();
        controller.lock().unwrap().take_recorded(&mut events);
        events
            .into_iter()
            .filter_map(|e| match e {
                RecordEvent::Note { key, on, .. } => Some((key, on)),
                _ => None,
            })
            .collect()
    }

    /// Fills the command queue until the controller refuses.
    fn fill(controller: &Mutex<Controller>) {
        let mut c = controller.lock().unwrap();
        while c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {}
    }

    #[test]
    fn a_midi_press_and_a_typed_press_of_one_key_record_their_own_lengths() {
        let (controller, mut processor) = recording_engine();
        let sink = ControllerSink::new(Arc::clone(&controller));
        let typed = |on| Command::ChannelNote {
            channel: ChannelId(1),
            key: 60,
            velocity: 0.3,
            on,
            id: NoteId(1_060),
        };
        // The MIDI key is held; the typing keyboard plays the same key inside it.
        sink.press(1, 60, 0.9, press_id(60));
        run(&mut processor, 8);
        controller.lock().unwrap().send(typed(true)).unwrap();
        run(&mut processor, 8);
        controller.lock().unwrap().send(typed(false)).unwrap();
        run(&mut processor, 8);
        release_now(&sink, &controller, 1, 60);
        run(&mut processor, 2);
        let mut events = Vec::new();
        controller.lock().unwrap().take_recorded(&mut events);
        let mut notes = grooviq_project::recorder::Recorder::new().feed(&events);
        notes.sort_by_key(|n| n.tick);
        let got: Vec<(f32, u32)> = notes.iter().map(|n| (n.velocity, n.length)).collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0].0, got[1].0), (0.9, 0.3));
        // 24 blocks against 8: the MIDI note lasts three times the typed one.
        assert!(got[0].1 > 2 * got[1].1, "{got:?}");
        let end = |n: &grooviq_project::notes::NewNote| i64::from(n.tick + n.length);
        let gap = end(&notes[0]) - end(&notes[1]);
        assert!(
            (gap - i64::from(notes[1].length)).abs() <= 2,
            "the MIDI note ends 8 blocks after the typed one: {got:?}"
        );
    }

    #[test]
    fn a_release_the_full_queue_refused_is_delivered_when_it_has_room() {
        let (controller, mut processor) = recording_engine();
        let sink = ControllerSink::new(Arc::clone(&controller));
        sink.press(1, 60, 1.0, press_id(60));
        run(&mut processor, 2);
        assert_eq!(heard(&controller), [(60, true)]);

        // The audio thread stalls: the queue fills and the key is let go meanwhile.
        fill(&controller);
        release_now(&sink, &controller, 1, 60);
        assert_eq!(sink.owed(), 1, "kept, not dropped");
        // It is still refused while the queue is full: nothing is lost, nothing blocks.
        sink.retry();
        assert_eq!(sink.owed(), 1);

        run(&mut processor, 4);
        sink.retry();
        assert_eq!(sink.owed(), 0);
        run(&mut processor, 2);
        assert_eq!(heard(&controller), [(60, false)], "the note ends");
    }

    #[test]
    fn a_note_pressed_again_leaves_the_release_still_owed_to_end_the_older_note() {
        let (controller, mut processor) = recording_engine();
        let sink = ControllerSink::new(Arc::clone(&controller));
        sink.press(1, 60, 1.0, press_id(60));
        run(&mut processor, 2);
        heard(&controller);
        fill(&controller);
        release_now(&sink, &controller, 1, 60);
        assert_eq!(sink.owed(), 1);
        run(&mut processor, 4);
        // The key goes down again before the owed release was delivered. The release stays
        // owed: the engine pairs it with the older press, so it ends that note, not this one.
        assert!(sink.press(1, 60, 1.0, NoteId(1_060)).is_some());
        assert_eq!(sink.owed(), 1);
        sink.retry();
        assert_eq!(sink.owed(), 0);
        run(&mut processor, 2);
        assert_eq!(heard(&controller), [(60, true), (60, false)]);
    }

    #[test]
    fn a_release_for_a_removed_channel_is_dropped_not_kept() {
        let (controller, mut processor) = recording_engine();
        let sink = ControllerSink::new(Arc::clone(&controller));
        let instance = sink.press(1, 60, 1.0, press_id(60)).expect("pressed");
        run(&mut processor, 2);
        controller
            .lock()
            .unwrap()
            .remove_channel(ChannelId(1))
            .unwrap();
        run(&mut processor, 2);
        // Refused because the channel is gone, not because the queue is full.
        sink.release(1, 60, instance, &[press_id(60)]);
        assert_eq!(sink.owed(), 0);
    }

    #[test]
    fn an_owed_release_is_dropped_once_its_channel_is_removed() {
        let (controller, mut processor) = recording_engine();
        let sink = ControllerSink::new(Arc::clone(&controller));
        sink.press(1, 60, 1.0, press_id(60));
        run(&mut processor, 2);
        fill(&controller);
        release_now(&sink, &controller, 1, 60);
        assert_eq!(sink.owed(), 1);
        // The queue drains, then the channel goes before the release is retried.
        run(&mut processor, 4);
        controller
            .lock()
            .unwrap()
            .remove_channel(ChannelId(1))
            .unwrap();
        run(&mut processor, 2);
        sink.retry();
        assert_eq!(sink.owed(), 0, "nothing is owed to a channel that is gone");
    }
}

// ---- two keyboards at the same instant ----

/// Hands the callbacks of two plugged-in keyboards to the test, so each can run on its own
/// thread (the fake ports' own lock would serialize them).
fn callbacks(ports: &Fake, names: [&str; 2]) -> [Callback; 2] {
    let mut state = ports.0.lock().unwrap();
    names.map(|name| state.open.remove(name).unwrap())
}

#[test]
fn a_concurrent_last_release_cannot_cut_a_new_owner() {
    use std::sync::Condvar;
    use std::time::Duration;

    /// A sink that holds the first note-off back until another input's note-on reaches it
    /// (or, when the hub rightly keeps that note-on waiting, a moment passes).
    struct HoldOff {
        sent: Recorded,
        armed: Mutex<bool>,
        reached: Condvar,
        held: Mutex<bool>,
        on_seen: Condvar,
        on_arrived: Mutex<bool>,
    }
    impl CommandSink for HoldOff {
        fn press(&self, channel: u32, key: u8, velocity: f32, _: NoteId) -> Option<u64> {
            *self.on_arrived.lock().unwrap() = true;
            self.on_seen.notify_all();
            self.sent.push(on(channel, key, velocity));
            Some(0)
        }
        fn release(&self, channel: u32, key: u8, _instance: u64, ids: &[NoteId]) {
            assert_eq!(ids.len(), 1);
            if std::mem::take(&mut *self.armed.lock().unwrap()) {
                *self.held.lock().unwrap() = true;
                self.reached.notify_all();
                let arrived = self.on_arrived.lock().unwrap();
                let _ = self
                    .on_seen
                    .wait_timeout_while(arrived, Duration::from_millis(300), |seen| !*seen)
                    .unwrap();
            }
            self.sent.push(off(channel, key));
        }
    }

    let ports = Fake::default();
    ports.plug("Keys");
    ports.plug("Pads");
    let sink = Arc::new(HoldOff {
        sent: Recorded::default(),
        armed: Mutex::new(false),
        reached: Condvar::new(),
        held: Mutex::new(false),
        on_seen: Condvar::new(),
        on_arrived: Mutex::new(false),
    });
    let mut hub = MidiHub::new(
        Box::new(ports.clone()),
        Arc::clone(&sink) as Arc<dyn CommandSink>,
        Arc::new(AtomicU32::new(1)),
        Vec::new(),
    );
    hub.poll();
    let [mut keys, mut pads] = callbacks(&ports, ["Keys", "Pads"]);
    keys(&[NOTE_ON, 60, 100]);
    sink.sent.take();
    *sink.on_arrived.lock().unwrap() = false;
    *sink.armed.lock().unwrap() = true;

    // Keys lets go of C4 (the last owner) and stalls on its way to the engine; Pads presses C4
    // meanwhile.
    let release = std::thread::spawn(move || keys(&[NOTE_OFF, 60, 0]));
    {
        let held = sink.held.lock().unwrap();
        let _held = sink.reached.wait_while(held, |held| !*held).unwrap();
    }
    let press = std::thread::spawn(move || pads(&[NOTE_ON, 60, 100]));
    release.join().unwrap();
    press.join().unwrap();

    let heard = sink.sent.take();
    assert!(
        !matches!(heard.last(), Some(Command::ChannelNote { on: false, .. })),
        "the new owner's note was cut: {heard:?}"
    );
}

// ---- debts the engine owes, at their edges ----

mod engine_queue_edges {
    use super::*;
    use grooviq_app::midi::ControllerSink;
    use grooviq_engine::{
        AudioProcessor, ChannelMix, ChannelSetup, Controller, EngineConfig, EngineState,
        RecordEvent, engine_with,
    };

    /// `n` synth channels (ids 1..=n), recording while playing.
    fn setup(n: u32) -> (Arc<Mutex<Controller>>, AudioProcessor) {
        let state = EngineState {
            channels: (1..=n)
                .map(|id| ChannelSetup::synth(ChannelId(id), Vec::new(), ChannelMix::default()))
                .collect(),
            ..EngineState::default()
        };
        let (controller, mut processor) = engine_with(
            EngineConfig {
                sample_rate: 48_000.0,
            },
            state,
        );
        let controller = Arc::new(Mutex::new(controller));
        controller.lock().unwrap().send(Command::Play).unwrap();
        controller
            .lock()
            .unwrap()
            .send(Command::SetRecording { on: true })
            .unwrap();
        run(&mut processor);
        (controller, processor)
    }

    fn run(processor: &mut AudioProcessor) {
        for _ in 0..4 {
            processor.process(&mut [0.0; 512], 2);
        }
    }

    fn fill(controller: &Mutex<Controller>) {
        while controller
            .lock()
            .unwrap()
            .send(Command::SetTempo { bpm: 120.0 })
            .is_ok()
        {}
    }

    fn heard(controller: &Mutex<Controller>) -> Vec<RecordEvent> {
        let mut events = Vec::new();
        controller.lock().unwrap().take_recorded(&mut events);
        events
    }

    #[test]
    fn the_1025th_release_is_not_forgotten() {
        let (controller, mut processor) = setup(9);
        let sink = ControllerSink::new(Arc::clone(&controller));
        sink.press(9, 127, 1.0, press_id(127));
        run(&mut processor);
        heard(&controller);
        fill(&controller);
        // 8 channels x 128 keys of debt, then one more on a ninth channel.
        for channel in 1..=8 {
            for key in 0..128 {
                release_now(&sink, &controller, channel, key);
            }
        }
        assert_eq!(sink.owed(), 1024);
        release_now(&sink, &controller, 9, 127);
        // The queue holds 1,024 commands, so the debt is paid over several retries.
        let mut released = false;
        for _ in 0..4 {
            run(&mut processor);
            sink.retry();
            run(&mut processor);
            released |= heard(&controller).iter().any(|e| {
                matches!(
                    e,
                    RecordEvent::Note {
                        channel: ChannelId(9),
                        key: 127,
                        on: false,
                        ..
                    }
                )
            });
        }
        assert!(released, "the release past 1,024 was lost");
        assert_eq!(sink.owed(), 0);
    }

    #[test]
    fn an_owed_release_does_not_cross_to_a_channel_made_afterwards() {
        let (controller, mut processor) = setup(1);
        let sink = ControllerSink::new(Arc::clone(&controller));
        sink.press(1, 60, 1.0, press_id(60));
        run(&mut processor);
        heard(&controller);
        fill(&controller);
        release_now(&sink, &controller, 1, 60);
        run(&mut processor);
        // Channel 1 is removed and made again before the release is retried.
        controller
            .lock()
            .unwrap()
            .remove_channel(ChannelId(1))
            .unwrap();
        run(&mut processor);
        controller
            .lock()
            .unwrap()
            .add_channel(ChannelSetup::synth(
                ChannelId(1),
                Vec::new(),
                ChannelMix::default(),
            ))
            .unwrap();
        run(&mut processor);
        // The new channel's note comes from somewhere else (the roll), not from this sink.
        controller.lock().unwrap().send(on(1, 60, 1.0)).unwrap();
        run(&mut processor);
        heard(&controller);
        sink.retry();
        run(&mut processor);
        assert!(
            !heard(&controller).iter().any(|e| matches!(
                e,
                RecordEvent::Note {
                    key: 60,
                    on: false,
                    ..
                }
            )),
            "the old channel's owed release cut its replacement"
        );
    }

    #[test]
    fn a_new_debt_on_a_reused_id_forgets_the_old_channels_keys() {
        let (controller, mut processor) = setup(1);
        let sink = ControllerSink::new(Arc::clone(&controller));
        sink.press(1, 60, 1.0, press_id(60));
        run(&mut processor);
        heard(&controller);
        fill(&controller);
        release_now(&sink, &controller, 1, 60);
        run(&mut processor);
        // Channel 1 is made again; C4 sounds on it from the roll, and a key played through
        // this sink is let go while the queue is full again.
        controller
            .lock()
            .unwrap()
            .remove_channel(ChannelId(1))
            .unwrap();
        run(&mut processor);
        controller
            .lock()
            .unwrap()
            .add_channel(ChannelSetup::synth(
                ChannelId(1),
                Vec::new(),
                ChannelMix::default(),
            ))
            .unwrap();
        run(&mut processor);
        controller.lock().unwrap().send(on(1, 60, 1.0)).unwrap();
        sink.press(1, 62, 1.0, press_id(62));
        run(&mut processor);
        heard(&controller);
        fill(&controller);
        release_now(&sink, &controller, 1, 62);
        assert_eq!(sink.owed(), 1, "only the new channel's key is owed");
        run(&mut processor);
        sink.retry();
        run(&mut processor);
        let ended: Vec<u8> = heard(&controller)
            .iter()
            .filter_map(|e| match e {
                RecordEvent::Note { key, on: false, .. } => Some(*key),
                _ => None,
            })
            .collect();
        assert_eq!(ended, [62], "C4 from the roll must keep sounding");
    }

    // ---- every way a refused release is tried again ----

    /// A keyboard "Keys" under a hub on a real engine, whose C4 was let go while the engine's
    /// queue was full: the release is owed, the queue has drained since, nothing retried yet.
    struct Stalled {
        hub: MidiHub,
        ports: Fake,
        controller: Arc<Mutex<Controller>>,
        processor: AudioProcessor,
        sink: Arc<ControllerSink>,
    }

    fn stalled() -> Stalled {
        let (controller, mut processor) = setup(1);
        let sink = Arc::new(ControllerSink::new(Arc::clone(&controller)));
        let ports = Fake::default();
        ports.plug("Keys");
        let mut hub = MidiHub::new(
            Box::new(ports.clone()),
            Arc::clone(&sink) as Arc<dyn CommandSink>,
            Arc::new(AtomicU32::new(1)),
            Vec::new(),
        );
        hub.poll();
        ports.send("Keys", &[NOTE_ON, 60, 100]);
        run(&mut processor);
        heard(&controller);
        fill(&controller);
        ports.send("Keys", &[NOTE_OFF, 60, 0]);
        assert_eq!(sink.owed(), 1, "the engine owes the release");
        run(&mut processor);
        Stalled {
            hub,
            ports,
            controller,
            processor,
            sink,
        }
    }

    impl Stalled {
        /// Whether the audio thread has heard C4's release by now.
        fn released(&mut self) -> bool {
            run(&mut self.processor);
            heard(&self.controller).iter().any(|e| {
                matches!(
                    e,
                    RecordEvent::Note {
                        key: 60,
                        on: false,
                        ..
                    }
                )
            })
        }
    }

    #[test]
    fn the_hub_hands_the_engine_its_owed_release_when_asked() {
        let mut stall = stalled();
        assert!(!stall.released(), "nothing retries by itself");
        stall.hub.retry_owed();
        assert!(stall.released());
        assert_eq!(stall.sink.owed(), 0);
    }

    #[test]
    fn the_next_message_from_any_keyboard_hands_over_the_owed_release() {
        let mut stall = stalled();
        stall.ports.send("Keys", &[NOTE_ON, 62, 100]);
        assert!(stall.released());
    }

    #[test]
    fn a_poll_hands_over_the_owed_release() {
        let mut stall = stalled();
        stall.hub.poll();
        assert!(stall.released());
    }

    #[test]
    fn switching_an_input_hands_over_the_owed_release() {
        let mut stall = stalled();
        stall.hub.set_enabled("Keys", false);
        assert!(stall.released());
    }

    #[test]
    fn the_timetable_hands_over_the_owed_release_within_a_quarter_second() {
        use grooviq_app::midi::Schedule;
        use std::time::{Duration, Instant};

        let mut stall = stalled();
        // A clock the test moves: no sleeping.
        let start = Instant::now();
        let mut schedule = Schedule::new();
        // The first step is the poll at start-up, which pays the debt itself: skip it, and see
        // what the steps after it do.
        let (_, first_wait) = schedule.step(start);
        let mut now = start + first_wait;
        let mut delivered = None;
        while now.duration_since(start) < Duration::from_secs(3) {
            let (action, wait) = schedule.step(now);
            stall.hub.run(action);
            if stall.released() {
                delivered = Some(now.duration_since(start));
                break;
            }
            now += wait;
        }
        let delivered = delivered.expect("the timetable never delivered the release");
        assert!(
            delivered <= Duration::from_millis(250),
            "after {delivered:?}"
        );
    }

    #[test]
    fn the_timetable_polls_every_two_seconds_and_retries_in_between() {
        use grooviq_app::midi::{Action, POLL_PERIOD, Schedule};
        use std::time::{Duration, Instant};

        let mut schedule = Schedule::new();
        let start = Instant::now();
        let (first, wait) = schedule.step(start);
        assert_eq!(first, Action::Poll, "at once");
        let mut now = start;
        let mut polls = 0;
        while now.duration_since(start) < POLL_PERIOD * 3 {
            now += wait;
            if schedule.step(now).0 == Action::Poll {
                polls += 1;
                // A poll comes no sooner than it is due.
                assert!(now.duration_since(start) >= POLL_PERIOD * polls);
            }
        }
        assert_eq!(polls, 3);
        assert!(wait > Duration::ZERO);
    }

    #[test]
    fn a_key_held_across_a_project_replacement_does_not_cut_the_new_note() {
        let (controller, mut processor) = setup(1);
        let sink = Arc::new(ControllerSink::new(Arc::clone(&controller)));
        let ports = Fake::default();
        ports.plug("Keys");
        let mut hub = MidiHub::new(
            Box::new(ports.clone()),
            Arc::clone(&sink) as Arc<dyn CommandSink>,
            Arc::new(AtomicU32::new(1)),
            Vec::new(),
        );
        hub.poll();
        ports.send("Keys", &[NOTE_ON, 60, 100]);
        run(&mut processor);
        heard(&controller);
        // Another project is opened: channel 1 is a new channel. C4 sounds on it from the roll.
        let state = EngineState {
            channels: vec![ChannelSetup::synth(
                ChannelId(1),
                Vec::new(),
                ChannelMix::default(),
            )],
            ..EngineState::default()
        };
        controller.lock().unwrap().load_state(state).unwrap();
        run(&mut processor);
        controller.lock().unwrap().send(Command::Play).unwrap();
        controller
            .lock()
            .unwrap()
            .send(Command::SetRecording { on: true })
            .unwrap();
        controller.lock().unwrap().send(on(1, 60, 1.0)).unwrap();
        run(&mut processor);
        heard(&controller);

        ports.send("Keys", &[NOTE_OFF, 60, 0]);
        run(&mut processor);
        assert!(
            !heard(&controller).iter().any(|e| matches!(
                e,
                RecordEvent::Note {
                    key: 60,
                    on: false,
                    ..
                }
            )),
            "the old project's key cut the new project's note"
        );
    }

    #[test]
    fn a_key_held_across_a_project_replacement_owes_nothing_when_the_queue_is_full() {
        let (controller, mut processor) = setup(1);
        let sink = Arc::new(ControllerSink::new(Arc::clone(&controller)));
        let ports = Fake::default();
        ports.plug("Keys");
        let mut hub = MidiHub::new(
            Box::new(ports.clone()),
            Arc::clone(&sink) as Arc<dyn CommandSink>,
            Arc::new(AtomicU32::new(1)),
            Vec::new(),
        );
        hub.poll();
        ports.send("Keys", &[NOTE_ON, 60, 100]);
        run(&mut processor);
        let state = EngineState {
            channels: vec![ChannelSetup::synth(
                ChannelId(1),
                Vec::new(),
                ChannelMix::default(),
            )],
            ..EngineState::default()
        };
        controller.lock().unwrap().load_state(state).unwrap();
        run(&mut processor);
        fill(&controller);
        ports.send("Keys", &[NOTE_OFF, 60, 0]);
        assert_eq!(
            sink.owed(),
            0,
            "a release of the old channel is not a debt of the new one"
        );
    }

    #[test]
    fn a_keyboard_holding_the_old_projects_key_does_not_disown_the_new_projects_key() {
        let (controller, mut processor) = setup(1);
        let sink = Arc::new(ControllerSink::new(Arc::clone(&controller)));
        let ports = Fake::default();
        ports.plug("Keys");
        ports.plug("Pads");
        let mut hub = MidiHub::new(
            Box::new(ports.clone()),
            Arc::clone(&sink) as Arc<dyn CommandSink>,
            Arc::new(AtomicU32::new(1)),
            Vec::new(),
        );
        hub.poll();
        ports.send("Keys", &[NOTE_ON, 60, 100]);
        run(&mut processor);
        // Another project opens; Pads presses C4 on its channel 1.
        let state = EngineState {
            channels: vec![ChannelSetup::synth(
                ChannelId(1),
                Vec::new(),
                ChannelMix::default(),
            )],
            ..EngineState::default()
        };
        controller.lock().unwrap().load_state(state).unwrap();
        run(&mut processor);
        controller.lock().unwrap().send(Command::Play).unwrap();
        controller
            .lock()
            .unwrap()
            .send(Command::SetRecording { on: true })
            .unwrap();
        ports.send("Pads", &[NOTE_ON, 60, 100]);
        run(&mut processor);
        heard(&controller);
        // Keys lets go of what it pressed in the old project: the new C4 sounds on.
        ports.send("Keys", &[NOTE_OFF, 60, 0]);
        run(&mut processor);
        assert!(
            heard(&controller).is_empty(),
            "the old key cut the new note"
        );
        // Pads is still the new C4's owner, and its release reaches the engine.
        ports.send("Pads", &[NOTE_OFF, 60, 0]);
        run(&mut processor);
        assert!(
            heard(&controller).iter().any(|e| matches!(
                e,
                RecordEvent::Note {
                    key: 60,
                    on: false,
                    ..
                }
            )),
            "the new note was left ringing"
        );
    }
}

// ---- what a refused release does to the voices, in every voice mode ----

/// The instruments pair the n-th note-off of a key with the n-th note-on of that key (a ticket
/// each), so a key that was pressed twice needs two releases, and a late release ends the older
/// note, never the newer one. These tests play press, refused release, re-press and release
/// through a hub on a real engine, rendered offline, and listen to what is left sounding.
mod voices {
    use super::*;
    use grooviq_app::midi::ControllerSink;
    use grooviq_engine::{
        AudioProcessor, ChannelMix, ChannelSetup, Controller, EngineConfig, SampleBuf, engine,
    };

    #[derive(Clone, Copy, Debug)]
    enum Kind {
        Sampler,
        Synth,
    }

    /// The index of the parameter `id` in the fixture the Rust tests keep in step with the
    /// devices.
    fn index(kind: Kind, id: &str) -> u32 {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/device-params.json"
        );
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let list = match kind {
            Kind::Sampler => "sampler",
            Kind::Synth => "synth",
        };
        fixture[list]
            .as_array()
            .unwrap()
            .iter()
            .position(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("no parameter {id}")) as u32
    }

    struct Rig {
        controller: Arc<Mutex<Controller>>,
        processor: AudioProcessor,
        sink: Arc<ControllerSink>,
        ports: Fake,
        hub: MidiHub,
        /// How many commands the queue holds.
        capacity: usize,
    }

    /// Frames in a rendered block.
    const BLOCK: usize = 512;
    const RATE: usize = 48_000;

    fn rig(kind: Kind, mode: f32) -> Rig {
        let (mut controller, processor) = engine(EngineConfig {
            sample_rate: RATE as f32,
        });
        let channel = ChannelId(1);
        let setup = match kind {
            Kind::Sampler => {
                // A long steady sample: nothing ends by itself while a note is held.
                let sample = SampleBuf {
                    rate: RATE as f32,
                    l: vec![0.5; RATE * 20],
                    r: vec![0.5; RATE * 20],
                };
                ChannelSetup::sampler(
                    channel,
                    Some(Arc::new(sample)),
                    Vec::new(),
                    ChannelMix::default(),
                )
            }
            Kind::Synth => ChannelSetup::synth(channel, Vec::new(), ChannelMix::default()),
        };
        controller.add_channel(setup).unwrap();
        match kind {
            Kind::Sampler => {
                // Gate mode: a note-off releases the note (the default plays one-shot).
                for (id, value) in [("one_shot", 0.0), ("voice_mode", mode)] {
                    controller
                        .send(Command::SetSamplerParam {
                            channel,
                            index: index(kind, id),
                            value,
                        })
                        .unwrap();
                }
            }
            Kind::Synth => controller
                .send(Command::SetChannelParam {
                    channel,
                    index: index(kind, "voice_mode"),
                    value: mode,
                })
                .unwrap(),
        }
        let controller = Arc::new(Mutex::new(controller));
        let sink = Arc::new(ControllerSink::new(Arc::clone(&controller)));
        let ports = Fake::default();
        ports.plug("Keys");
        let mut hub = MidiHub::new(
            Box::new(ports.clone()),
            Arc::clone(&sink) as Arc<dyn CommandSink>,
            Arc::new(AtomicU32::new(1)),
            Vec::new(),
        );
        hub.poll();
        let mut rig = Rig {
            controller,
            processor,
            sink,
            ports,
            hub,
            capacity: 0,
        };
        // The setup commands go first; then what an empty queue holds.
        rig.render(4);
        rig.capacity = rig.fill(usize::MAX);
        rig.render(4);
        rig
    }

    impl Rig {
        /// Renders `blocks` blocks; the loudest sample of the last one.
        fn render(&mut self, blocks: usize) -> f32 {
            let mut peak = 0.0f32;
            for _ in 0..blocks {
                let mut out = [0.0f32; BLOCK * 2];
                self.processor.process(&mut out, 2);
                peak = out.iter().fold(0.0, |peak, v| peak.max(v.abs()));
            }
            peak
        }

        /// Renders `seconds` and gives the loudest sample of the last 200 ms.
        fn listen(&mut self, seconds: f32) -> f32 {
            let blocks = (seconds * RATE as f32) as usize / BLOCK;
            let mut peak = 0.0f32;
            for i in 0..blocks {
                let block = self.render(1);
                if i + 200 * RATE / 1000 / BLOCK >= blocks {
                    peak = peak.max(block);
                }
            }
            peak
        }

        /// Fills the queue with harmless commands until `n` are in (or it is full); how many.
        fn fill(&mut self, n: usize) -> usize {
            let mut c = self.controller.lock().unwrap();
            let mut sent = 0;
            while sent < n && c.send(Command::SetTempo { bpm: 120.0 }).is_ok() {
                sent += 1;
            }
            sent
        }

        fn key(&self, on: bool) {
            let status = if on { NOTE_ON } else { NOTE_OFF };
            self.ports.send("Keys", &[status, 60, 100]);
        }
    }

    const AUDIBLE: f32 = 1e-3;
    const SILENT: f32 = 1e-4;

    /// Notes a failure instead of stopping at it, so one run names every mode that breaks.
    fn check(failures: &mut Vec<String>, ok: bool, what: String) {
        if !ok {
            failures.push(what);
        }
    }

    fn modes() -> Vec<(Kind, f32)> {
        [Kind::Sampler, Kind::Synth]
            .into_iter()
            .flat_map(|kind| [0.0, 1.0, 2.0].map(move |mode| (kind, mode)))
            .collect()
    }

    /// The release is refused (queue full), the key is pressed again once there is room, and let
    /// go: while it is held the new note sounds, afterwards nothing does.
    #[test]
    fn a_re_press_after_a_refused_release_keeps_the_newest_note_until_it_is_let_go() {
        let mut failures = Vec::new();
        for (kind, mode) in modes() {
            let mut rig = rig(kind, mode);
            rig.key(true);
            assert!(
                rig.listen(0.4) > AUDIBLE,
                "{kind:?} {mode}: the first note sounds"
            );
            rig.fill(usize::MAX);
            rig.key(false);
            check(
                &mut failures,
                rig.sink.owed() == 1,
                format!("{kind:?} {mode}: the release is owed"),
            );
            rig.render(4);
            rig.key(true);
            check(
                &mut failures,
                rig.listen(1.5) > AUDIBLE,
                format!("{kind:?} {mode}: the older release cut the newer note"),
            );
            rig.key(false);
            check(
                &mut failures,
                rig.listen(3.0) < SILENT,
                format!("{kind:?} {mode}: a voice is left sounding"),
            );
            check(
                &mut failures,
                rig.sink.owed() == 0,
                format!("{kind:?} {mode}: a release is still owed"),
            );
        }
        assert_eq!(failures, Vec::<String>::new());
    }

    /// The release is refused (queue full), then the graph panics and is rebuilt, which keeps
    /// the channel instance, so the release is still owed. The key is pressed again; the old
    /// release, delivered after that press, names a note the rebuilt instrument never started:
    /// the new note sounds until it is let go.
    #[test]
    fn a_release_owed_across_a_panic_recovery_does_not_cut_the_next_note() {
        let mut failures = Vec::new();
        for (kind, mode) in modes() {
            let mut rig = rig(kind, mode);
            rig.key(true);
            rig.render(4);
            rig.fill(usize::MAX);
            rig.key(false);
            rig.render(4);
            rig.controller
                .lock()
                .unwrap()
                .send(Command::InjectPanic)
                .unwrap();
            rig.render(1);
            rig.controller.lock().unwrap().check_health();
            rig.render(1);
            check(
                &mut failures,
                rig.sink.owed() == 1,
                format!("{kind:?} {mode}: the release is still owed to this instance"),
            );
            rig.key(true);
            check(
                &mut failures,
                rig.sink.owed() == 0,
                format!("{kind:?} {mode}: the press handed over the owed release"),
            );
            check(
                &mut failures,
                rig.listen(1.5) > AUDIBLE,
                format!("{kind:?} {mode}: the pre-recovery release cut the new note"),
            );
            rig.key(false);
            check(
                &mut failures,
                rig.listen(3.0) < SILENT,
                format!("{kind:?} {mode}: a voice is left sounding"),
            );
        }
        assert_eq!(failures, Vec::<String>::new());
    }

    /// Two releases of one key are owed (a loud older press, a quiet newer one) and the queue
    /// has room for exactly one: the older debt is paid first, so the loud note ends and the
    /// quiet one sounds on until the next retry.
    #[test]
    fn review_partial_debt_payment_is_oldest_first() {
        let mut rig = rig(Kind::Sampler, 0.0);
        let instance = rig.sink.press(1, 60, 1.0, NoteId(1)).expect("pressed");
        rig.sink.press(1, 60, 0.2, NoteId(2)).expect("pressed");
        let both = rig.listen(0.4);
        rig.fill(usize::MAX);
        rig.sink.release(1, 60, instance, &[NoteId(1), NoteId(2)]);
        assert_eq!(rig.sink.owed(), 2);
        rig.render(4);
        let room = rig.capacity - 1;
        assert_eq!(rig.fill(room), room);
        rig.sink.retry();
        assert_eq!(rig.sink.owed(), 1, "one release fit");
        let left = rig.listen(1.0);
        assert!(
            (left - both / 6.0).abs() < 1e-3,
            "the quiet newer note sounds on: {left} (both: {both})"
        );
        rig.render(8);
        rig.sink.retry();
        assert_eq!(rig.sink.owed(), 0);
        assert!(rig.listen(1.0) < SILENT);
    }

    /// The re-press is refused too (the queue is still full): the old note must still end.
    #[test]
    fn a_re_press_the_full_queue_refused_does_not_lose_the_owed_release() {
        let mut failures = Vec::new();
        for (kind, mode) in modes() {
            let mut rig = rig(kind, mode);
            rig.key(true);
            rig.render(4);
            rig.fill(usize::MAX);
            rig.key(false);
            rig.key(true);
            rig.key(false);
            rig.render(4);
            rig.hub.retry_owed();
            check(
                &mut failures,
                rig.listen(3.0) < SILENT,
                format!("{kind:?} {mode}: the first note rings for ever"),
            );
            check(
                &mut failures,
                rig.sink.owed() == 0,
                format!("{kind:?} {mode}: a release is still owed"),
            );
        }
        assert_eq!(failures, Vec::<String>::new());
    }

    /// A refused re-press owes nothing: once the queue drains, the next accepted hold is not cut
    /// by a release invented for the press the engine never took.
    #[test]
    fn a_refused_re_press_owes_no_release_that_cuts_the_next_hold() {
        let mut failures = Vec::new();
        for (kind, mode) in modes() {
            let mut rig = rig(kind, mode);
            rig.key(true);
            rig.render(4);
            rig.fill(usize::MAX);
            rig.key(false);
            rig.key(true);
            rig.key(false);
            rig.render(4);
            rig.key(true);
            check(
                &mut failures,
                rig.listen(1.5) > AUDIBLE,
                format!("{kind:?} {mode}: the next hold was cut"),
            );
            rig.key(false);
            check(
                &mut failures,
                rig.listen(3.0) < SILENT,
                format!("{kind:?} {mode}: a note rings for ever"),
            );
        }
        assert_eq!(failures, Vec::<String>::new());
    }

    /// There is room for the re-press but not for the release the debt needs: both releases
    /// are owed, and both arrive.
    #[test]
    fn a_re_press_with_one_free_slot_and_two_owed_releases_ends_both_notes() {
        let mut failures = Vec::new();
        for (kind, mode) in modes() {
            let mut rig = rig(kind, mode);
            rig.key(true);
            rig.render(4);
            rig.fill(usize::MAX);
            rig.key(false);
            rig.render(4);
            // One slot: the second press gets it, the debt and the next release do not.
            let capacity = rig.capacity;
            rig.fill(capacity - 1);
            rig.key(true);
            rig.key(false);
            check(
                &mut failures,
                rig.sink.owed() == 2,
                format!("{kind:?} {mode}: two releases are owed"),
            );
            rig.render(4);
            rig.hub.retry_owed();
            check(
                &mut failures,
                rig.listen(3.0) < SILENT,
                format!("{kind:?} {mode}: a voice is left sounding"),
            );
            check(
                &mut failures,
                rig.sink.owed() == 0,
                format!("{kind:?} {mode}: a release is still owed"),
            );
        }
        assert_eq!(failures, Vec::<String>::new());
    }

    /// A key two keyboards hold is two notes in the engine: letting go of the last takes two
    /// releases.
    #[test]
    fn a_key_two_keyboards_pressed_ends_both_notes_when_the_last_lets_go() {
        let mut failures = Vec::new();
        for (kind, mode) in modes() {
            let mut rig = rig(kind, mode);
            rig.ports.plug("Pads");
            rig.hub.poll();
            rig.key(true);
            rig.ports.send("Pads", &[NOTE_ON, 60, 100]);
            check(
                &mut failures,
                rig.listen(0.4) > AUDIBLE,
                format!("{kind:?} {mode}: the notes sound"),
            );
            rig.ports.send("Pads", &[NOTE_OFF, 60, 0]);
            check(
                &mut failures,
                rig.listen(1.0) > AUDIBLE,
                format!("{kind:?} {mode}: Keys still holds the key"),
            );
            rig.key(false);
            check(
                &mut failures,
                rig.listen(3.0) < SILENT,
                format!("{kind:?} {mode}: a voice is left sounding"),
            );
        }
        assert_eq!(failures, Vec::<String>::new());
    }
}
