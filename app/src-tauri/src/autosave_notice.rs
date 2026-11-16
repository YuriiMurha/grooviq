//! What the person is told about autosave. Pure of Tauri, so the rules are tested here; the
//! autosave thread and `start_audio` only record and emit what these return.
//!
//! The state is retained ([`AutosaveStatus`]) and the UI asks for it with `autosave_status`
//! once its listeners are attached, so a notice raised before the UI was ready is not lost. The
//! `autosave-failed` event only carries later changes.

use serde::Serialize;
use std::collections::HashSet;
use std::io;

/// Emitted with a sentence when an autosave fails with an error not announced since the last
/// success.
pub(crate) const FAILED_EVENT: &str = "autosave-failed";

/// What the UI is owed about autosave. `{ disabled, failure }` as JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct AutosaveStatus {
    /// Why autosave is off in this copy for good (another Grooviq holds the recovery folder).
    pub(crate) disabled: Option<String>,
    /// The latest failing autosave's sentence, until an autosave succeeds.
    pub(crate) failure: Option<String>,
    /// The distinct errors already announced since the last success.
    #[serde(skip)]
    told: HashSet<String>,
}

impl AutosaveStatus {
    /// Autosave never started here: `disabled` is the sentence for the person.
    pub(crate) fn off(disabled: Option<String>) -> Self {
        Self {
            disabled,
            ..Self::default()
        }
    }

    /// An autosave failed. The status keeps the sentence; the return value is `Some` only the
    /// first time this error comes up since the last success: a disk that stays full fails every
    /// minute, and one toast a minute is noise (and an error that comes back after another is
    /// still the same trouble).
    pub(crate) fn failed(&mut self, error: &io::Error) -> Option<String> {
        let reason = error.to_string();
        let message = format!(
            "Autosave is failing: {reason}. Your changes are not being backed up; save the project to keep them."
        );
        self.failure = Some(message.clone());
        self.told.insert(reason).then_some(message)
    }

    /// An autosave was written: the trouble is over, and the next failure is told again.
    pub(crate) fn succeeded(&mut self) {
        self.failure = None;
        self.told.clear();
    }
}

/// The sentence for a recovery folder that could not be taken, if the person should hear of it:
/// another running Grooviq holds it. (Other failures stay in the audio log.)
pub(crate) fn disabled_notice(error: &io::Error) -> Option<String> {
    (error.kind() == io::ErrorKind::WouldBlock).then(|| {
        "Another Grooviq is already running, so autosave is off in this window. Unsaved changes here are lost if it closes unexpectedly; save often."
            .to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> io::Error {
        io::Error::other(text)
    }

    const FULL: &str = "no space left on device";
    const DENIED: &str = "permission denied";

    #[test]
    fn a_repeating_error_is_told_once() {
        let mut status = AutosaveStatus::default();
        assert!(status.failed(&error(FULL)).is_some());
        for _ in 0..5 {
            assert_eq!(status.failed(&error(FULL)), None);
        }
    }

    #[test]
    fn a_new_error_is_told_and_an_old_one_coming_back_is_not() {
        let mut status = AutosaveStatus::default();
        assert!(status.failed(&error(FULL)).is_some());
        assert!(status.failed(&error(DENIED)).is_some());
        // Back and forth with no autosave in between: both already told.
        assert_eq!(status.failed(&error(FULL)), None);
        assert_eq!(status.failed(&error(DENIED)), None);
        assert_eq!(status.failed(&error(FULL)), None);
    }

    #[test]
    fn a_success_re_arms_every_error() {
        let mut status = AutosaveStatus::default();
        assert!(status.failed(&error(FULL)).is_some());
        assert!(status.failed(&error(DENIED)).is_some());
        status.succeeded();
        assert!(status.failed(&error(FULL)).is_some());
        assert!(status.failed(&error(DENIED)).is_some());
    }

    #[test]
    fn the_status_keeps_the_latest_failure_even_when_it_is_not_told_again() {
        let mut status = AutosaveStatus::default();
        let first = status.failed(&error(FULL)).unwrap();
        assert_eq!(status.failure.as_deref(), Some(first.as_str()));
        let second = status.failed(&error(DENIED)).unwrap();
        // The UI that asks now is owed the failure that is going on now.
        assert_eq!(status.failure.as_deref(), Some(second.as_str()));
        assert_eq!(status.failed(&error(FULL)), None);
        assert_eq!(status.failure.as_deref(), Some(first.as_str()));
        status.succeeded();
        assert_eq!(status.failure, None);
    }

    #[test]
    fn a_disabled_copy_keeps_saying_so_however_often_it_is_asked() {
        let held = io::Error::new(io::ErrorKind::WouldBlock, "another Grooviq is running");
        let status = AutosaveStatus::off(disabled_notice(&held));
        assert!(status.disabled.is_some());
        // Asking is reading: nothing is consumed.
        assert_eq!(status.clone().disabled, status.disabled);
        assert_eq!(AutosaveStatus::off(None).disabled, None);
    }

    #[test]
    fn only_a_folder_held_by_another_copy_is_announced() {
        let held = io::Error::new(io::ErrorKind::WouldBlock, "another Grooviq is running");
        assert!(disabled_notice(&held).is_some());
        assert_eq!(disabled_notice(&error("read-only file system")), None);
        assert_eq!(
            disabled_notice(&io::Error::from(io::ErrorKind::PermissionDenied)),
            None
        );
    }
}
