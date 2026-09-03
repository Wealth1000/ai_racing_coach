//! Persistent user settings: the one file, `data/settings.json`.
//!
//! The CLI's knobs are flags because a terminal session is exploratory and
//! per-invocation. The GUI is the product, and a product remembers its
//! settings: the toggle that decides whether live coaching keeps recording
//! the session's telemetry is chosen once in the window and applies to every
//! session after — including `coach live` from a terminal, because the file
//! is the setting, not any one surface's memory of it.
//!
//! The shape is deliberately one flat struct with serde defaults on every
//! field, so a settings file from an older build (or a field removed later)
//! loads rather than bricks the GUI: unknown fields are ignored by serde,
//! missing ones fall back to their defaults. A corrupt file warns and loads
//! defaults rather than refusing to start — settings are conveniences, not
//! artefacts the pipeline depends on (unlike the models in `data/tracks`,
//! which are refused loudly when wrong; see [`CoachError::BadArtefact`]).

use serde::{Deserialize, Serialize};

use crate::core::{CoachError, Result};

/// Where captures recorded during live coaching are written, and where the
/// Coach Learn screen looks first when refining a model. Sits beside
/// `data/tracks` — models and the captures that grow them are one workflow.
pub const CAPTURES_DIR: &str = "data/captures";

/// The user's persisted choices. See the module docs for the shape's rules.
///
/// Not `Copy` — it owns the install id — so it is passed by reference and
/// cloned where a copy used to be enough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Keep writing the raw telemetry to a capture file while coaching live.
    ///
    /// The capture is the refinement loop's fuel: re-learning the model from
    /// the original capture plus the ones later sessions recorded makes the
    /// corner set a model of everything the driver has driven, not just the
    /// first session. Defaults to true — the disk cost is a few MB per
    /// session and the alternative is silently forgetting laps the driver
    /// might have wanted.
    #[serde(default = "default_record_while_coaching")]
    pub record_while_coaching: bool,

    /// Send the exported dataset to the author when the driver asks, to grow
    /// the corpus the neural coach will train on.
    ///
    /// Defaults to false and only turns on through the consent dialog: the
    /// toggle must never be on because a default said so. See
    /// [`crate::storage::share`] for what a share bundle does and does not
    /// contain.
    #[serde(default)]
    pub share_telemetry: bool,

    /// The random id that marks this install's share bundles. Generated at
    /// the first consent — the author needs to link one stranger's sessions
    /// across uploads (to split a pooled corpus by session honestly) without
    /// ever knowing who they are. Absent until the driver opts in: an id for
    /// a machine that never shared is a file entry for nothing.
    #[serde(default)]
    pub install_id: Option<String>,
}

fn default_record_while_coaching() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            record_while_coaching: default_record_while_coaching(),
            share_telemetry: false,
            install_id: None,
        }
    }
}

impl Settings {
    /// A fresh install id, at the moment the driver first consents to
    /// sharing. Wall-clock nanoseconds plus the process id: unique across
    /// machines without coordination (the same trick `SessionId` uses, one
    /// tick finer) and carrying nothing about the machine it came from.
    pub fn generate_install_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("install_{nanos:x}_{:x}", std::process::id())
    }

    /// Where the one settings file lives. Relative, like every data path in
    /// this crate (`data/tracks`), so a portable install keeps its settings
    /// beside its models.
    pub const PATH: &str = "data/settings.json";

    /// Load the settings, or the defaults when there is nothing usable.
    ///
    /// A missing file is not an error — first run, or a build that predates
    /// settings. A file that exists but does not parse *warns*: the driver
    /// edited the file by hand (there is no other way to make it invalid)
    /// and deserves to hear that the edit did not take.
    pub fn load() -> Self {
        Self::load_from(std::path::Path::new(Self::PATH))
    }

    /// [`Self::load`] against an explicit path — the seam the tests use, so
    /// they never have to touch the process's working directory.
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!(
                    "warning: {} could not be read as settings ({e}) — using defaults",
                    path.display()
                );
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Persist the settings, creating the directory like the model and
    /// personal-best writers do.
    pub fn save(&self) -> Result<()> {
        self.save_to(std::path::Path::new(Self::PATH))
    }

    /// [`Self::save`] against an explicit path.
    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| CoachError::Io {
                path: parent.display().to_string(),
                source: e,
            })?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| CoachError::Io {
            path: path.display().to_string(),
            source: std::io::Error::other(e),
        })?;
        std::fs::write(path, text).map_err(|e| CoachError::Io {
            path: path.display().to_string(),
            source: e,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("coach_settings_tests").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn a_missing_file_loads_the_defaults_and_recording_is_on() {
        let settings = Settings::load_from(&temp_path("missing/settings.json"));
        assert!(settings.record_while_coaching);
        // Sharing is a consent decision, never a default.
        assert!(!settings.share_telemetry);
        assert_eq!(settings.install_id, None);
    }

    #[test]
    fn a_saved_file_round_trips() {
        let path = temp_path("round_trip");
        Settings {
            record_while_coaching: false,
            share_telemetry: true,
            install_id: Some("install_deadbeef_1a2b".to_string()),
        }
        .save_to(&path)
        .expect("save writes the file");
        let loaded = Settings::load_from(&path);
        assert!(!loaded.record_while_coaching);
        assert!(loaded.share_telemetry);
        assert_eq!(loaded.install_id.as_deref(), Some("install_deadbeef_1a2b"));
    }

    /// The forward-compatibility rule: a file from a newer build (extra
    /// fields) or an older one (fields missing) still loads — the defaults
    /// fill the gaps rather than refusing to start the GUI.
    #[test]
    fn a_file_with_unknown_or_missing_fields_still_loads() {
        let path = temp_path("tolerant");
        std::fs::write(
            &path,
            "{\"record_while_coaching\": false, \"future_knob\": 3}",
        )
        .unwrap();
        let loaded = Settings::load_from(&path);
        assert!(!loaded.record_while_coaching);
        assert!(!loaded.share_telemetry, "consent never arrives by default");

        std::fs::write(&path, "{}").unwrap();
        assert!(Settings::load_from(&path).record_while_coaching);
    }

    /// Install ids are unique across the same process's two consecutive
    /// consents (two machines, same moment) — the nanosecond tick is the
    /// whole point of the scheme.
    #[test]
    fn generated_install_ids_do_not_collide() {
        assert_ne!(Settings::generate_install_id(), Settings::generate_install_id());
    }
}
