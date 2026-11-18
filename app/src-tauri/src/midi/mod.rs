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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPort {
    pub id: String,
    pub name: String,
}

/// The system's MIDI inputs, by opaque ID. A fake stands in for it in tests.
pub trait MidiPorts: Send {
    /// The IDs and display names of the inputs plugged in now; empty if there are none or the system has no
    /// MIDI at all.
    fn list(&self) -> Vec<MidiPort>;
    /// Starts reading the opaque ID `name`: `sink` gets each message's bytes, on a thread of the system's.
    fn open(&mut self, name: &str, sink: MidiSink) -> Result<(), String>;
    /// Stops reading the opaque ID `name`.
    fn close(&mut self, name: &str);
}

/// The operating system's MIDI inputs, through `midir` (CoreMIDI on macOS, WinMM on Windows).
///
/// A machine with no MIDI devices, or none at all, lists nothing and says nothing: CI runners
/// are like that. Display names never select a connection.
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

// WinMM resolves by (interface ID, name); CoreMIDI resolves by stable unique ID.
// Always encode the full WinMM selector, even without siblings. JSON escapes arbitrary
// strings unambiguously; enumeration order and group membership never enter the key.
pub(crate) fn endpoint_key(id: &str, name: &str, winmm: bool) -> String {
    if winmm {
        serde_json::to_string(&("winmm", id, name)).expect("string tuple serializes")
    } else {
        id.to_owned()
    }
}

fn port_key(input: &midir::MidiInput, port: &midir::MidiInputPort) -> Option<String> {
    Some(endpoint_key(
        &port.id(),
        &input.port_name(port).ok()?,
        cfg!(target_os = "windows"),
    ))
}

/// Resolves the exact backend selector against the current enumeration.
fn resolve_port<P>(
    ports: impl IntoIterator<Item = P>,
    key: &str,
    descriptor: impl Fn(&P) -> Option<(String, String)>,
    winmm: bool,
) -> Option<P> {
    ports.into_iter().find(|port| {
        descriptor(port).is_some_and(|(id, name)| endpoint_key(&id, &name, winmm) == key)
    })
}

fn find_port(input: &midir::MidiInput, key: &str) -> Option<midir::MidiInputPort> {
    resolve_port(
        input.ports(),
        key,
        |port| Some((port.id(), input.port_name(port).ok()?)),
        cfg!(target_os = "windows"),
    )
}

impl MidiPorts for MidirPorts {
    fn list(&self) -> Vec<MidiPort> {
        // `list` takes `&self`: a client made now is dropped after the listing. Cheap, and every
        // poll sees the devices as they are.
        let Ok(input) = midir::MidiInput::new(CLIENT_NAME) else {
            return Vec::new();
        };
        input
            .ports()
            .iter()
            .filter_map(|port| {
                Some(MidiPort {
                    id: port_key(&input, port)?,
                    name: input.port_name(port).ok()?,
                })
            })
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

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn winmm_siblings_route_and_persist_without_membership_dependent_keys() {
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex, atomic::AtomicU32};
        type Callbacks = BTreeMap<String, MidiSink>;
        #[derive(Clone, Default)]
        struct Ports {
            raw: Arc<Mutex<Vec<(String, String)>>>,
            callbacks: Arc<Mutex<Callbacks>>,
        }
        impl MidiPorts for Ports {
            fn list(&self) -> Vec<MidiPort> {
                self.raw
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, name)| MidiPort {
                        id: endpoint_key(id, name, true),
                        name: name.clone(),
                    })
                    .collect()
            }
            fn open(&mut self, key: &str, sink: MidiSink) -> Result<(), String> {
                let raw = self.raw.lock().unwrap();
                let resolved = resolve_port(raw.iter(), key, |p| Some((*p).clone()), true)
                    .expect("exact selector");
                assert_eq!(endpoint_key(&resolved.0, &resolved.1, true), key);
                drop(raw);
                self.callbacks.lock().unwrap().insert(key.into(), sink);
                Ok(())
            }
            fn close(&mut self, key: &str) {
                self.callbacks.lock().unwrap().remove(key);
            }
        }
        #[derive(Default)]
        struct Sink(Mutex<Vec<bool>>);
        impl CommandSink for Sink {
            fn press(&self, _: u32, _: u8, _: f32, _: grooviq_engine::NoteId) -> Option<u64> {
                self.0.lock().unwrap().push(true);
                Some(1)
            }
            fn release(&self, _: u32, _: u8, _: u64, ids: &[grooviq_engine::NoteId]) {
                self.0.lock().unwrap().extend(ids.iter().map(|_| false));
            }
        }
        let ports = Ports::default();
        let a = ("interface\u{1f}\0".to_owned(), "Keys".to_owned());
        let b = (a.0.clone(), "MIDIIN2 (Keys)".to_owned());
        let key = |p: &(String, String)| endpoint_key(&p.0, &p.1, true);
        let ka = key(&a);
        let kb = key(&b);
        *ports.raw.lock().unwrap() = vec![a.clone(), b.clone(), a.clone()];
        let sink = Arc::new(Sink::default());
        let make = |settings: &crate::settings::Settings| {
            MidiHub::from_settings(
                Box::new(ports.clone()),
                sink.clone(),
                Arc::new(AtomicU32::new(1)),
                settings,
            )
        };
        let mut settings = crate::settings::Settings::default();
        let mut hub = make(&settings);
        hub.poll();
        assert_eq!(hub.inputs().len(), 2);
        assert_eq!(hub.input_views().len(), 2);
        assert_eq!(ports.callbacks.lock().unwrap().len(), 2);
        for k in [&ka, &kb] {
            ports.callbacks.lock().unwrap().get_mut(k).unwrap()(&[0x90, 60, 100]);
        }
        assert_eq!(*sink.0.lock().unwrap(), [true, true]);
        *ports.raw.lock().unwrap() = vec![b.clone()];
        hub.poll();
        assert_eq!(*sink.0.lock().unwrap(), [true, true]);
        ports.callbacks.lock().unwrap().get_mut(&kb).unwrap()(&[0x80, 60, 0]);
        assert_eq!(*sink.0.lock().unwrap(), [true, true, false, false]);
        hub.switch_configured(&mut settings, &kb, false);
        *ports.raw.lock().unwrap() = vec![b.clone(), a.clone()];
        hub.poll();
        assert!(ports.callbacks.lock().unwrap().contains_key(&ka));
        assert!(!ports.callbacks.lock().unwrap().contains_key(&kb));
        hub.switch_configured(&mut settings, &ka, false);
        let path =
            std::env::temp_dir().join(format!("grooviq-midi-selector-{}.json", std::process::id()));
        crate::settings::save(&path, &settings).unwrap();
        drop(hub);
        *ports.raw.lock().unwrap() = vec![a.clone()];
        let mut restored = crate::settings::load(&path).0;
        let mut hub = make(&restored);
        hub.poll();
        assert_eq!(hub.inputs(), [(ka.clone(), false)]);
        hub.switch_configured(&mut restored, &ka, true);
        crate::settings::save(&path, &restored).unwrap();
        drop(hub);
        *ports.raw.lock().unwrap() = vec![b, a];
        let mut hub = make(&crate::settings::load(&path).0);
        hub.poll();
        let choices: BTreeMap<_, _> = hub.inputs().into_iter().collect();
        assert!(choices[&ka]);
        assert!(!choices[&kb]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn selectors_are_stable_unambiguous_and_backend_specific() {
        let a = endpoint_key("interface", "Keys", true);
        let b = endpoint_key("interface", "MIDIIN2 (Keys)", true);
        assert_ne!(a, b);
        assert_eq!(a, endpoint_key("interface", "Keys", true));
        assert_ne!(
            endpoint_key("a\u{1f}b", "c", true),
            endpoint_key("a", "b\u{1f}c", true)
        );
        assert_ne!(
            endpoint_key("a", "\0\n\"", true),
            endpoint_key("a\0", "\n\"", true)
        );
        assert_eq!(
            endpoint_key("42", "Old", false),
            endpoint_key("42", "Renamed", false)
        );
        assert_ne!(
            endpoint_key("42", "Keys", false),
            endpoint_key("43", "Keys", false)
        );
    }
}
