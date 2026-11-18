//! What the UI hears about the output and the engine: [`OutputEvent`] and [`HealthEvent`]
//! turned into `audio-event` payloads and audio-log lines.

use crate::audio_log::Level;
use grooviq_engine::{HealthEvent, OutputEvent, OutputInfo, OutputKind};
use serde::Serialize;

/// One output, as the `audio_outputs` command and `audio-event` describe it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDto {
    pub id: String,
    pub name: String,
    /// `"headphones"`, `"speakers"` or `"unknown"`.
    pub kind: &'static str,
    pub is_default: bool,
    /// The output plays now.
    pub current: bool,
    /// The rate it runs at (for the output that plays) or will be asked for, in Hz: the UI
    /// turns the live latency in frames into milliseconds with it.
    pub sample_rate: u32,
}

/// The `audio-event` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AudioEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputDto>,
    pub message: String,
}

/// `info` for the UI; `current` is the output that plays now ([`AudioOutput::current`]).
///
/// [`AudioOutput::current`]: grooviq_engine::AudioOutput::current
pub fn output_dto(info: &OutputInfo, current: Option<&OutputInfo>) -> OutputDto {
    let playing = current.filter(|c| c.id == info.id);
    OutputDto {
        id: info.id.clone(),
        name: info.name.clone(),
        kind: match info.kind {
            OutputKind::Headphones => "headphones",
            OutputKind::Speakers => "speakers",
            OutputKind::Unknown => "unknown",
        },
        is_default: info.is_default,
        current: playing.is_some(),
        // The open stream's rate is the true one.
        sample_rate: playing.map_or(info.sample_rate, |c| c.sample_rate),
    }
}

/// The payload and log level for an output event. `current` is read after the event, as the
/// truth about what plays (a `Switched` can be followed by a failed open).
pub fn from_output(event: &OutputEvent, current: Option<&OutputInfo>) -> (Level, AudioEvent) {
    let (level, kind, output, message) = match event {
        OutputEvent::Opened(o) => (
            Level::Info,
            "opened",
            Some(o),
            format!("Playing on {}", o.name),
        ),
        OutputEvent::Switched { from, to } => (
            Level::Info,
            "switched",
            Some(to),
            format!("Output moved from {} to {}", from.name, to.name),
        ),
        OutputEvent::GuardStopped { to } => (
            Level::Warn,
            "guard-stopped",
            Some(to),
            format!(
                "Playback stopped: sound would have moved to {}, which may be heard in the room",
                to.name
            ),
        ),
        OutputEvent::Lost {
            fallback: Some(fallback),
        } => (
            Level::Warn,
            "lost",
            Some(fallback),
            format!(
                "The chosen output is gone; using {} until it is back",
                fallback.name
            ),
        ),
        OutputEvent::Lost { fallback: None } => (
            Level::Warn,
            "lost",
            None,
            "The chosen output is gone, and there is no other".to_owned(),
        ),
        OutputEvent::NoDevice => (
            Level::Warn,
            "no-device",
            None,
            "No output device: nothing can play".to_owned(),
        ),
        OutputEvent::OpenFailed { output, error } => (
            Level::Warn,
            "open-failed",
            Some(output),
            format!("Could not open {} ({error}); trying again", output.name),
        ),
    };
    let event = AudioEvent {
        id: None,
        kind,
        output: output.map(|o| output_dto(o, current)),
        message,
    };
    (level, event)
}

/// The payload and log level for a health event.
pub fn from_health(event: HealthEvent) -> (Level, AudioEvent) {
    let (kind, message) = match event {
        HealthEvent::Restarted { faults } => (
            "restarted",
            format!("The sound engine hit an error and restarted ({faults} so far)"),
        ),
        HealthEvent::DeviceSilenced { total } => (
            "silenced",
            format!("A device produced invalid audio and was muted for a moment ({total} so far)"),
        ),
    };
    let event = AudioEvent {
        id: None,
        kind,
        output: None,
        message,
    };
    (Level::Warn, event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(id: &str, name: &str, kind: OutputKind) -> OutputInfo {
        OutputInfo {
            id: id.into(),
            name: name.into(),
            kind,
            is_default: false,
            sample_rate: 48_000,
        }
    }

    fn airpods() -> OutputInfo {
        OutputInfo {
            is_default: true,
            ..info("ap", "AirPods Pro", OutputKind::Headphones)
        }
    }

    fn speakers() -> OutputInfo {
        info("spk", "MacBook Pro Speakers", OutputKind::Speakers)
    }

    #[test]
    fn an_output_serialises_in_camel_case_with_its_kind_and_current_flag() {
        let ap = airpods();
        let dto = output_dto(&ap, Some(&ap));
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            json!({ "id": "ap", "name": "AirPods Pro", "kind": "headphones", "isDefault": true, "current": true, "sampleRate": 48000 })
        );
        assert!(!output_dto(&ap, None).current);
        assert!(!output_dto(&ap, Some(&speakers())).current);
        assert_eq!(output_dto(&speakers(), None).kind, "speakers");
        let usb = info("usb", "DAC", OutputKind::Unknown);
        assert_eq!(output_dto(&usb, None).kind, "unknown");
    }

    #[test]
    fn every_output_event_has_its_kind_level_and_output() {
        let (ap, spk) = (airpods(), speakers());
        let cases = [
            (
                OutputEvent::Opened(ap.clone()),
                "opened",
                Level::Info,
                Some("ap"),
            ),
            (
                OutputEvent::Switched {
                    from: ap.clone(),
                    to: spk.clone(),
                },
                "switched",
                Level::Info,
                Some("spk"),
            ),
            (
                OutputEvent::GuardStopped { to: spk.clone() },
                "guard-stopped",
                Level::Warn,
                Some("spk"),
            ),
            (
                OutputEvent::Lost {
                    fallback: Some(spk.clone()),
                },
                "lost",
                Level::Warn,
                Some("spk"),
            ),
            (
                OutputEvent::Lost { fallback: None },
                "lost",
                Level::Warn,
                None,
            ),
            (OutputEvent::NoDevice, "no-device", Level::Warn, None),
            (
                OutputEvent::OpenFailed {
                    output: spk.clone(),
                    error: "busy".into(),
                },
                "open-failed",
                Level::Warn,
                Some("spk"),
            ),
        ];
        for (event, kind, level, output) in cases {
            let (got_level, payload) = from_output(&event, Some(&ap));
            assert_eq!(payload.kind, kind, "{event:?}");
            assert_eq!(got_level, level, "{event:?}");
            assert_eq!(
                payload.output.as_ref().map(|o| o.id.as_str()),
                output,
                "{event:?}"
            );
            assert!(!payload.message.is_empty());
        }
    }

    #[test]
    fn messages_name_the_devices_and_the_error() {
        let (ap, spk) = (airpods(), speakers());
        let (_, opened) = from_output(&OutputEvent::Opened(ap.clone()), Some(&ap));
        assert!(opened.message.contains("AirPods Pro"));
        let (_, guard) = from_output(&OutputEvent::GuardStopped { to: spk.clone() }, Some(&ap));
        assert!(guard.message.contains("MacBook Pro Speakers"));
        let failed = OutputEvent::OpenFailed {
            output: spk,
            error: "device busy".into(),
        };
        let (_, failed) = from_output(&failed, None);
        assert!(
            failed.message.contains("MacBook Pro Speakers")
                && failed.message.contains("device busy")
        );
    }

    #[test]
    fn the_current_flag_comes_from_what_plays_not_from_the_event() {
        // The switch to the speakers was announced, but their stream failed to open: the
        // AirPods still play.
        let (ap, spk) = (airpods(), speakers());
        let switched = OutputEvent::Switched {
            from: ap.clone(),
            to: spk,
        };
        let (_, payload) = from_output(&switched, Some(&ap));
        assert!(!payload.output.unwrap().current);
    }

    #[test]
    fn health_events_warn_and_serialise_without_an_output() {
        let (level, restarted) = from_health(HealthEvent::Restarted { faults: 2 });
        assert_eq!((level, restarted.kind), (Level::Warn, "restarted"));
        assert!(restarted.message.contains('2'));
        assert_eq!(
            serde_json::to_value(&restarted).unwrap(),
            json!({ "kind": "restarted", "message": restarted.message })
        );
        let (level, silenced) = from_health(HealthEvent::DeviceSilenced { total: 5 });
        assert_eq!((level, silenced.kind), (Level::Warn, "silenced"));
    }
}
