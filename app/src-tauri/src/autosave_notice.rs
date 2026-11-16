//! What the person is told about autosave. Pure of Tauri, so the rules are tested here; the
//! autosave thread and `start_audio` only emit what these return.

use std::io;

/// Emitted with a sentence when an autosave could not be written.
pub(crate) const FAILED_EVENT: &str = "autosave-failed";
/// Emitted once, with a sentence, when this copy of the app runs without autosave because
/// another one holds the recovery folder.
pub(crate) const DISABLED_EVENT: &str = "autosave-disabled";

/// Tells of a failing autosave once per distinct error: a disk that stays full fails every
/// minute, and one toast a minute is noise. A success (the trouble is over) re-arms it, so the
/// next failure is told again.
#[derive(Debug, Default)]
pub(crate) struct FailureNotice {
    told: Option<String>,
}

impl FailureNotice {
    /// The sentence to show for this failure, or `None` if the person was already told of this
    /// very error since the last success.
    pub(crate) fn failed(&mut self, error: &io::Error) -> Option<String> {
        let reason = error.to_string();
        if self.told.as_deref() == Some(reason.as_str()) {
            return None;
        }
        let message = format!(
            "Autosave is failing: {reason}. Your changes are not being backed up; save the project to keep them."
        );
        self.told = Some(reason);
        Some(message)
    }

    /// An autosave was written.
    pub(crate) fn succeeded(&mut self) {
        self.told = None;
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

    #[test]
    fn a_repeating_error_is_told_once() {
        let mut notice = FailureNotice::default();
        assert!(notice.failed(&error("no space left on device")).is_some());
        for _ in 0..5 {
            assert_eq!(notice.failed(&error("no space left on device")), None);
        }
    }

    #[test]
    fn a_different_error_is_told_again() {
        let mut notice = FailureNotice::default();
        assert!(notice.failed(&error("no space left on device")).is_some());
        assert!(notice.failed(&error("permission denied")).is_some());
        // And back to the first: it is a change too.
        assert!(notice.failed(&error("no space left on device")).is_some());
    }

    #[test]
    fn a_success_re_arms_the_notice() {
        let mut notice = FailureNotice::default();
        assert!(notice.failed(&error("no space left on device")).is_some());
        notice.succeeded();
        assert!(notice.failed(&error("no space left on device")).is_some());
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
