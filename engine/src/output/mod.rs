//! Output devices: which device the engine plays on, and moving it there safely.
//!
//! [`OutputMonitor`] decides from the device list alone. It follows the system default (or the
//! device the user chose, using the default while that one is gone). Its speaker guard stops
//! playback whenever the output moves by itself (a new default, a lost device, a moment with no
//! device) to anything but headphones; a move the user picks plays on. [`AudioOutput`] carries
//! the decisions out on an [`OutputBackend`]: it owns the processor, looks at the devices every
//! 500 ms and switches streams so that a guard stop never reaches the new device.
//! [`cpal_backend`] is the real backend; [`null_backend`] a silent one for QA runs of the app,
//! which opens no device.
//!
//! Only output devices are ever listed or opened: opening the AirPods' microphone would drop
//! them into their low-quality headset mode.

mod backend;
mod cpal_backend;
#[cfg(target_os = "macos")]
mod macos;
mod null_backend;

pub use backend::{AudioOutput, AudioSlot, OutputBackend};
pub use cpal_backend::{cpal_backend, cpal_backends_created};
pub use null_backend::{NULL_OUTPUT_ID, null_backend, run_silent};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// What an output is, as far as the speaker guard is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutputKind {
    /// Only the listener hears it.
    Headphones,
    /// The room hears it.
    Speakers,
    /// Neither its name nor its transport tells.
    Unknown,
}

/// How an output is connected. Only macOS reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transport {
    BuiltIn,
    Bluetooth,
    Usb,
    Hdmi,
    DisplayPort,
    Other,
}

/// One output device, as a backend lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    /// Stable across launches (the CoreAudio UID or the WASAPI endpoint id).
    pub id: String,
    pub name: String,
    pub kind: OutputKind,
    /// The system's default output.
    pub is_default: bool,
    /// The rate the backend asks the device for when it opens it. [`AudioOutput::current`] and
    /// the events [`AudioOutput`] delivers carry the rate the stream really runs at.
    pub sample_rate: u32,
}

/// Which output the user wants.
#[derive(Clone, Debug, PartialEq)]
pub enum OutputChoice {
    /// Whatever the system uses, following it when it changes.
    SystemDefault,
    /// This device (by [`OutputInfo::id`]) while it is connected, the system default while not.
    Device(String),
}

/// A change [`OutputMonitor::tick`] decided (or, for `OpenFailed`, a failure [`AudioOutput`]
/// met), in the order it happened.
#[derive(Clone, Debug, PartialEq)]
pub enum OutputEvent {
    /// An output was chosen while none was in use. [`AudioOutput`] also reports it when a
    /// device it could not open opens at last, and once when it reopens a failed stream.
    Opened(OutputInfo),
    /// The output moved to another device.
    Switched { from: OutputInfo, to: OutputInfo },
    /// The speaker guard stopped playback, because the output moved by itself to `to`, which
    /// is not headphones. It comes before the `Switched` or `Opened` that moves there.
    GuardStopped { to: OutputInfo },
    /// The chosen device is gone. The output falls back to the system default, if there is
    /// one. Reported again once the device has come back, or the choice has changed.
    Lost { fallback: Option<OutputInfo> },
    /// There is no device to play on. Reported again once there has been one.
    NoDevice,
    /// [`AudioOutput`] could not open `output`, so nothing plays. It keeps trying the same
    /// device (after 0.5 s, then doubling up to 8 s) and never falls back to another one.
    /// Reported once until the output opens, the target changes, or the choice changes.
    OpenFailed { output: OutputInfo, error: String },
}

/// Name words that mean headphones. They win over the transport: "External Headphones" is a
/// built-in jack.
const HEADPHONE_WORDS: [&str; 11] = [
    "airpods",
    "headphone",
    "headset",
    "earbud",
    "earphone",
    "buds",
    "beats",
    "bose",
    "jabra",
    "wh-",
    "wf-",
];

/// Name words that mean speakers.
const SPEAKER_WORDS: [&str; 5] = ["speaker", "display", "monitor", "hdmi", "tv"];

/// Classifies an output: headphone words in its name mean headphones; then a built-in, HDMI or
/// DisplayPort transport, or speaker words in its name, mean speakers. Anything else is
/// unknown, Bluetooth included (Bluetooth speakers exist). Names match case-insensitively.
pub fn classify(name: &str, transport: Option<Transport>) -> OutputKind {
    let name = name.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| name.contains(word));
    if says(&HEADPHONE_WORDS) {
        return OutputKind::Headphones;
    }
    match transport {
        Some(Transport::BuiltIn | Transport::Hdmi | Transport::DisplayPort) => OutputKind::Speakers,
        _ if says(&SPEAKER_WORDS) => OutputKind::Speakers,
        _ => OutputKind::Unknown,
    }
}

/// Decides which output to use from each fresh device list. Pure logic: it opens nothing.
#[derive(Debug)]
pub struct OutputMonitor {
    choice: OutputChoice,
    guard: bool,
    current: Option<OutputInfo>,
    /// A device has been in use: from now on a move may need the guard. The very first open
    /// never does.
    used_before: bool,
    /// The choice changed since the last tick, so the move it causes is the user's own.
    chosen_by_user: bool,
    /// `Lost` was reported, and the chosen device has not come back since.
    lost_reported: bool,
    /// `NoDevice` was reported, and there has been no device since.
    no_device_reported: bool,
}

impl OutputMonitor {
    pub fn new(choice: OutputChoice, guard: bool) -> Self {
        Self {
            choice,
            guard,
            current: None,
            used_before: false,
            chosen_by_user: false,
            lost_reported: false,
            no_device_reported: false,
        }
    }

    /// Applies at the next tick. The move it causes there, to the device the choice names, is
    /// the user's own: the guard lets it play on. A fallback (the chosen device is missing)
    /// still counts as a move by itself.
    pub fn set_choice(&mut self, c: OutputChoice) {
        self.choice = c;
        self.lost_reported = false;
        self.chosen_by_user = true;
    }

    /// Turns the speaker guard on or off.
    pub fn set_guard(&mut self, on: bool) {
        self.guard = on;
    }

    /// Picks the device to use from `devices` (the full list) and reports what changed.
    ///
    /// The target is the chosen device if it is listed, otherwise the system default (with
    /// `Lost` when the chosen one is gone). If no device is flagged default, the current one
    /// stays while it is listed. When the guard is on and the output moves by itself to
    /// anything but headphones, `GuardStopped` comes first. `Opened` means nothing was in use;
    /// `NoDevice`, that there is nothing to use.
    pub fn tick(&mut self, devices: &[OutputInfo]) -> Vec<OutputEvent> {
        let mut events = Vec::new();
        let by_user = std::mem::take(&mut self.chosen_by_user);
        let (target, as_chosen) = self.target(devices, &mut events);
        let Some(target) = target.cloned() else {
            self.current = None;
            if !std::mem::replace(&mut self.no_device_reported, true) {
                events.push(OutputEvent::NoDevice);
            }
            return events;
        };
        self.no_device_reported = false;
        let by_itself = !(by_user && as_chosen);
        match self.current.take() {
            // The same device: keep it, with its fresh details.
            Some(current) if current.id == target.id => {
                if current.kind == OutputKind::Headphones && self.guard_stops(by_itself, &target) {
                    events.push(OutputEvent::GuardStopped { to: target.clone() });
                }
            }
            Some(current) => {
                if self.guard_stops(by_itself, &target) {
                    events.push(OutputEvent::GuardStopped { to: target.clone() });
                }
                events.push(OutputEvent::Switched {
                    from: current,
                    to: target.clone(),
                });
            }
            None => {
                if self.used_before && self.guard_stops(by_itself, &target) {
                    events.push(OutputEvent::GuardStopped { to: target.clone() });
                }
                events.push(OutputEvent::Opened(target.clone()));
            }
        }
        self.used_before = true;
        self.current = Some(target);
        events
    }

    /// The device in use.
    pub fn current(&self) -> Option<&OutputInfo> {
        self.current.as_ref()
    }

    /// The device to use, and whether it is the one the choice names (not a fallback).
    fn target<'a>(
        &mut self,
        devices: &'a [OutputInfo],
        events: &mut Vec<OutputEvent>,
    ) -> (Option<&'a OutputInfo>, bool) {
        let default = devices.iter().find(|d| d.is_default);
        let (wanted, named) = match &self.choice {
            OutputChoice::SystemDefault => (default, true),
            OutputChoice::Device(id) => match devices.iter().find(|d| d.id == *id) {
                Some(chosen) => {
                    self.lost_reported = false;
                    (Some(chosen), true)
                }
                None => {
                    if !std::mem::replace(&mut self.lost_reported, true) {
                        events.push(OutputEvent::Lost {
                            fallback: default.cloned(),
                        });
                    }
                    (default, false)
                }
            },
        };
        if wanted.is_some() {
            return (wanted, named);
        }
        // While the system moves its default, a list may have none flagged: stay put.
        let current = self.current.as_ref();
        let stay = current.and_then(|current| devices.iter().find(|d| d.id == current.id));
        (stay, false)
    }

    /// Whether the guard stops playback for a move to `to`.
    fn guard_stops(&self, by_itself: bool, to: &OutputInfo) -> bool {
        self.guard && by_itself && to.kind != OutputKind::Headphones
    }
}

/// Locks `mutex` even if a thread panicked while holding it: the output layer must still be
/// able to stop the sound.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
