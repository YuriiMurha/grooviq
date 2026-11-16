//! Which backend the app plays on: the real one, or, when started with `GROOVIQ_AUDIO=null`,
//! the silent one for QA runs, which opens no audio device.

use crate::audio_log::Level;
use grooviq_engine::{OutputBackend, cpal_backend, null_backend};
use std::ffi::OsStr;

/// The variable that picks the backend.
pub(crate) const AUDIO_ENV: &str = "GROOVIQ_AUDIO";
/// Its one recognised value.
const SILENT: &str = "null";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputMode {
    /// The system's audio devices.
    Real,
    /// No device: the engine runs in real time and its sound is thrown away.
    Silent,
}

/// The mode `value` (the variable's value, if set) asks for, and a line for the audio log. Only
/// exactly `null` means silent; anything else plays on the real output, with a warning when
/// the variable is set to something it does not know.
pub(crate) fn output_mode(value: Option<&OsStr>) -> (OutputMode, Option<(Level, String)>) {
    match value {
        None => (OutputMode::Real, None),
        Some(value) if value == SILENT => (
            OutputMode::Silent,
            Some((
                Level::Warn,
                format!(
                    "Silent QA mode ({AUDIO_ENV}={SILENT}): no audio device is opened; the \
                     engine runs and its sound is discarded"
                ),
            )),
        ),
        Some(value) => (
            OutputMode::Real,
            Some((
                Level::Warn,
                format!(
                    "{AUDIO_ENV}={:?} is not a known value (only {SILENT:?} is): playing on \
                     the real output",
                    value.to_string_lossy()
                ),
            )),
        ),
    }
}

/// The backend for `mode`.
pub(crate) fn backend(mode: OutputMode) -> Box<dyn OutputBackend> {
    match mode {
        OutputMode::Real => cpal_backend(),
        OutputMode::Silent => null_backend(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grooviq_engine::{NULL_OUTPUT_ID, cpal_backends_created};

    fn mode(value: Option<&str>) -> OutputMode {
        output_mode(value.map(OsStr::new)).0
    }

    #[test]
    fn the_real_output_is_the_default() {
        assert_eq!(output_mode(None), (OutputMode::Real, None));
    }

    #[test]
    fn only_exactly_null_is_silent() {
        assert_eq!(mode(Some("null")), OutputMode::Silent);
        for other in ["", "NULL", "Null", " null", "null ", "cpal", "none", "0"] {
            assert_eq!(mode(Some(other)), OutputMode::Real, "{other:?}");
        }
    }

    #[test]
    fn silent_mode_and_unknown_values_are_logged() {
        let (_, note) = output_mode(Some(OsStr::new("null")));
        let (level, message) = note.expect("silent mode is logged");
        assert_eq!(level, Level::Warn);
        assert!(message.contains("GROOVIQ_AUDIO=null"), "{message}");
        let (_, note) = output_mode(Some(OsStr::new("loud")));
        let (level, message) = note.expect("an unknown value is logged");
        assert_eq!(level, Level::Warn);
        assert!(message.contains("loud"), "{message}");
    }

    /// One test, so no other test in this binary creates a cpal backend in between.
    #[test]
    fn silent_mode_never_creates_a_cpal_backend_and_the_real_mode_does() {
        let before = cpal_backends_created();
        let silent = backend(OutputMode::Silent);
        let ids: Vec<String> = silent.outputs().into_iter().map(|o| o.id).collect();
        assert_eq!(ids, [NULL_OUTPUT_ID]);
        drop(silent);
        assert_eq!(
            cpal_backends_created(),
            before,
            "the silent backend reached cpal"
        );
        // Creating the real backend lists and opens nothing.
        drop(backend(OutputMode::Real));
        assert_eq!(cpal_backends_created(), before + 1);
    }
}
