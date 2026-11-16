//! Output devices: which device the engine plays on, and moving it there safely.
//!
//! [`OutputMonitor`] decides from the device list alone. It follows the system default (or the
//! device the user chose, using the default while that one is gone), and its speaker guard stops
//! playback when sound would move from headphones to anything else. [`AudioOutput`] carries the
//! decisions out on an [`OutputBackend`]: it owns the processor, looks at the devices every
//! 500 ms and switches streams so that a guard stop never reaches the new device.
//! [`cpal_backend`] is the real backend.
//!
//! Only output devices are ever listed or opened: opening the AirPods' microphone would drop
//! them into their low-quality headset mode.

mod backend;
mod cpal_backend;
#[cfg(target_os = "macos")]
mod macos;

pub use backend::{AudioOutput, AudioSlot, OutputBackend};
pub use cpal_backend::cpal_backend;

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
    /// The rate the backend runs the device at when it opens it.
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

/// A change [`OutputMonitor::tick`] decided, in the order it happened.
#[derive(Clone, Debug, PartialEq)]
pub enum OutputEvent {
    /// An output was chosen while none was in use. [`AudioOutput`] also reports it when it
    /// reopens a device whose stream failed.
    Opened(OutputInfo),
    /// The output moved to another device.
    Switched { from: OutputInfo, to: OutputInfo },
    /// The speaker guard stopped playback, because sound would have moved from headphones to
    /// `to`. It comes before the `Switched` or `Opened` that moves there.
    GuardStopped { to: OutputInfo },
    /// The chosen device is gone. The output falls back to the system default, if there is
    /// one. Reported once until the device returns or the choice changes.
    Lost { fallback: Option<OutputInfo> },
    /// There is no device to play on. Reported once until there is one again.
    NoDevice,
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

/// Name words that mean speakers, for outputs whose transport does not tell.
const SPEAKER_WORDS: [&str; 5] = ["speaker", "display", "monitor", "hdmi", "tv"];

/// Classifies an output by headphone words in its name, then by its transport (Bluetooth means
/// headphones; built-in, HDMI and DisplayPort mean speakers), then by speaker words in its name.
/// Names match case-insensitively.
pub fn classify(name: &str, transport: Option<Transport>) -> OutputKind {
    let name = name.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| name.contains(word));
    if says(&HEADPHONE_WORDS) {
        return OutputKind::Headphones;
    }
    match transport {
        Some(Transport::Bluetooth) => OutputKind::Headphones,
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
    /// The kind of the device in use last. It outlives a moment with no device, so the guard
    /// also covers headphones, then nothing, then speakers.
    last_kind: Option<OutputKind>,
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
            last_kind: None,
            lost_reported: false,
            no_device_reported: false,
        }
    }

    /// Applies at the next tick.
    pub fn set_choice(&mut self, c: OutputChoice) {
        self.choice = c;
        self.lost_reported = false;
    }

    /// Turns the speaker guard on or off.
    pub fn set_guard(&mut self, on: bool) {
        self.guard = on;
    }

    /// Picks the device to use from `devices` (the full list) and reports what changed.
    ///
    /// The target is the chosen device if it is listed, otherwise the system default (with
    /// `Lost` when the chosen one is gone). If no device is flagged default, the current one
    /// stays while it is listed. Moving away from headphones to anything else emits
    /// `GuardStopped` first when the guard is on. `Opened` means nothing was in use;
    /// `NoDevice`, that there is nothing to use.
    pub fn tick(&mut self, devices: &[OutputInfo]) -> Vec<OutputEvent> {
        let mut events = Vec::new();
        let Some(target) = self.target(devices, &mut events).cloned() else {
            self.current = None;
            if !std::mem::replace(&mut self.no_device_reported, true) {
                events.push(OutputEvent::NoDevice);
            }
            return events;
        };
        self.no_device_reported = false;
        match self.current.take() {
            // The same device: keep it, with its fresh details.
            Some(current) if current.id == target.id => {}
            Some(current) => {
                if self.guard_stops(current.kind, &target) {
                    events.push(OutputEvent::GuardStopped { to: target.clone() });
                }
                events.push(OutputEvent::Switched {
                    from: current,
                    to: target.clone(),
                });
            }
            None => {
                if let Some(last) = self.last_kind
                    && self.guard_stops(last, &target)
                {
                    events.push(OutputEvent::GuardStopped { to: target.clone() });
                }
                events.push(OutputEvent::Opened(target.clone()));
            }
        }
        self.last_kind = Some(target.kind);
        self.current = Some(target);
        events
    }

    /// The device in use.
    pub fn current(&self) -> Option<&OutputInfo> {
        self.current.as_ref()
    }

    /// Drops the device in use because it failed to open. Its kind stays for the guard, and the
    /// next tick opens the target again.
    pub(crate) fn forget_current(&mut self) {
        self.current = None;
    }

    fn target<'a>(
        &mut self,
        devices: &'a [OutputInfo],
        events: &mut Vec<OutputEvent>,
    ) -> Option<&'a OutputInfo> {
        let default = devices.iter().find(|d| d.is_default);
        let wanted = match &self.choice {
            OutputChoice::SystemDefault => default,
            OutputChoice::Device(id) => match devices.iter().find(|d| d.id == *id) {
                Some(chosen) => {
                    self.lost_reported = false;
                    Some(chosen)
                }
                None => {
                    if !std::mem::replace(&mut self.lost_reported, true) {
                        events.push(OutputEvent::Lost {
                            fallback: default.cloned(),
                        });
                    }
                    default
                }
            },
        };
        // While the system moves its default, a list may have none flagged: stay put.
        wanted.or_else(|| {
            let current = self.current.as_ref()?;
            devices.iter().find(|d| d.id == current.id)
        })
    }

    /// Whether the guard stops playback when the output moves from a `from` device to `to`.
    fn guard_stops(&self, from: OutputKind, to: &OutputInfo) -> bool {
        self.guard && from == OutputKind::Headphones && to.kind != OutputKind::Headphones
    }
}
