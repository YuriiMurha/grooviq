//! MIDI keyboard input: keyboards are picked up as they are plugged in, play the selected
//! channel and are let go when unplugged or switched off.

mod hub;
mod parse;
mod sink;

use std::collections::HashMap;

pub use hub::{Action, CommandSink, MidiHub, POLL_PERIOD, Player, Poll, RETRY_PERIOD, Schedule};
pub use parse::{MidiMessage, parse};
pub use sink::ControllerSink;

/// The selected channel when none is selected: a MIDI keyboard then plays nothing.
pub const NO_CHANNEL: u32 = u32::MAX;

/// What an open input calls with each message's bytes.
pub type MidiSink = Box<dyn FnMut(&[u8]) + Send>;

/// The system's MIDI inputs, by name. A fake stands in for it in tests.
pub trait MidiPorts: Send {
    /// The names of the inputs plugged in now; empty if there are none or the system has no
    /// MIDI at all.
    fn list(&self) -> Vec<String>;
    /// Starts reading `name`: `sink` gets each message's bytes, on a thread of the system's.
    fn open(&mut self, name: &str, sink: MidiSink) -> Result<(), String>;
    /// Stops reading `name`.
    fn close(&mut self, name: &str);
}

/// The operating system's MIDI inputs, through `midir` (CoreMIDI on macOS, WinMM on Windows).
///
/// A machine with no MIDI devices, or none at all, lists nothing and says nothing: CI runners
/// are like that. Two devices with the same name are one input here.
#[derive(Default)]
pub struct MidirPorts {
    open: HashMap<String, midir::MidiInputConnection<()>>,
}

/// The name the OS shows for Grooviq's own MIDI client.
const CLIENT_NAME: &str = "Grooviq";

impl MidirPorts {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `input`'s port called `name`, if it is plugged in.
fn find_port(input: &midir::MidiInput, name: &str) -> Option<midir::MidiInputPort> {
    input
        .ports()
        .into_iter()
        .find(|port| input.port_name(port).is_ok_and(|n| n == name))
}

impl MidiPorts for MidirPorts {
    fn list(&self) -> Vec<String> {
        // `list` takes `&self`: a client made now is dropped after the listing. Cheap, and every
        // poll sees the devices as they are.
        let Ok(input) = midir::MidiInput::new(CLIENT_NAME) else {
            return Vec::new();
        };
        input
            .ports()
            .iter()
            .filter_map(|port| input.port_name(port).ok())
            .collect()
    }

    fn open(&mut self, name: &str, mut sink: MidiSink) -> Result<(), String> {
        let mut input = midir::MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
        // Notes, pedal and controllers only: no sysex, clock or active sensing.
        input.ignore(midir::Ignore::All);
        let port = find_port(&input, name).ok_or_else(|| "it was unplugged".to_owned())?;
        let connection = input
            .connect(&port, "grooviq-in", move |_, bytes, ()| sink(bytes), ())
            .map_err(|e| e.to_string())?;
        self.open.insert(name.to_owned(), connection);
        Ok(())
    }

    fn close(&mut self, name: &str) {
        if let Some(connection) = self.open.remove(name) {
            connection.close();
        }
    }
}
