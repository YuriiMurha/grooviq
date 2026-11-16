//! The user's audio settings, kept in `settings.json` in the app config dir.

use crate::audio_log::Level;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::Path;

/// What the app remembers between launches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The chosen output's id; `None` follows the system default.
    pub output: Option<String>,
    /// Stop playback rather than let sound move to speakers by itself.
    pub guard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output: None,
            guard: true,
        }
    }
}

/// What falling back to the defaults means, for the audio log.
const DEFAULTS: &str = "using the system default output with the speaker guard on";

/// Reads the settings at `path`. A missing file gives the defaults (system default output,
/// guard on) with an info line for the audio log; an unreadable or corrupt one, the defaults
/// with a warning.
pub fn load(path: &Path) -> (Settings, Option<(Level, String)>) {
    let problem = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(settings) => return (settings, None),
            Err(e) => (
                Level::Warn,
                format!("Saved settings could not be read ({e}); {DEFAULTS}"),
            ),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            (Level::Info, format!("No saved settings yet; {DEFAULTS}"))
        }
        Err(e) => (
            Level::Warn,
            format!("Saved settings could not be opened ({e}); {DEFAULTS}"),
        ),
    };
    (Settings::default(), Some(problem))
}

/// Writes `settings` to `path` atomically: a temporary file next to it, flushed to disk, then
/// a rename, so a crash or power loss mid-write never leaves a half-written or empty file.
/// Creates the directory if needed.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&json)?;
    // Without this a power loss after the rename can leave an empty file behind.
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fresh, empty directory for one test.
    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("grooviq-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_gives_the_defaults_with_a_note() {
        let path = dir("missing").join("settings.json");
        let (settings, note) = load(&path);
        assert_eq!(
            settings,
            Settings {
                output: None,
                guard: true
            }
        );
        let (level, message) = note.unwrap();
        assert_eq!(level, Level::Info);
        assert!(message.contains("No saved settings"), "{message}");
    }

    #[test]
    fn a_corrupt_file_gives_the_defaults_with_a_warning() {
        let path = dir("corrupt").join("settings.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let (settings, note) = load(&path);
        assert_eq!(settings, Settings::default());
        let (level, message) = note.unwrap();
        assert_eq!(level, Level::Warn);
        assert!(message.contains("could not be read"), "{message}");
    }

    #[test]
    fn saved_settings_load_back_and_leave_no_temporary_file() {
        let dir = dir("roundtrip");
        // A directory that does not exist yet is created.
        let path = dir.join("nested").join("settings.json");
        let saved = Settings {
            output: Some("BuiltInSpeakerDevice".into()),
            guard: false,
        };
        save(&path, &saved).unwrap();
        assert_eq!(load(&path), (saved.clone(), None));
        // Overwriting goes through the same rename.
        save(&path, &Settings::default()).unwrap();
        assert_eq!(load(&path).0, Settings::default());
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.json"]);
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let path = dir("partial").join("settings.json");
        std::fs::write(&path, br#"{ "output": "usb-dac" }"#).unwrap();
        assert_eq!(
            load(&path),
            (
                Settings {
                    output: Some("usb-dac".into()),
                    guard: true
                },
                None
            )
        );
    }
}
