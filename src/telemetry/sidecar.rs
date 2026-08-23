//! The logger's `.meta.json` sidecar.
//!
//! The logger probes AC's shared-memory pages at startup, records whether each
//! offset it reads produces a sane value, and counts the things it had to paper
//! over while writing (strings with non-text bytes, NaN/Infinity clamped to
//! zero). All of that lands in a sidecar next to the capture.
//!
//! Reading it is the cheapest quality gate available: the logger has already
//! decided whether its own output can be trusted, and a capture it flagged as
//! fatally broken should be refused rather than analysed. The sidecar's own
//! notes spell out why the soft counters matter too:
//!
//! > `strings_sanitized > 0` means the struct layout does not match the page
//! > this AC build publishes; fields at or past the first failing offset are not
//! > trustworthy.
//!
//! > `non_finite_floats` counts NaN/Infinity clamped to 0 on write; a zero in
//! > the data may therefore be a clamp, not a reading.
//!
//! Only the fields used for gating are deserialized. The sidecar is a
//! human-facing diagnostic document and its shape will keep changing, so
//! everything here is optional and a sidecar that fails to parse is treated as
//! absent rather than as an error.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::core::{CoachError, Result};

#[derive(Debug, Clone, Deserialize)]
pub struct Sidecar {
    #[serde(default)]
    pub logger_version: Option<String>,
    #[serde(default)]
    pub ac_running: Option<bool>,
    #[serde(default)]
    pub any_fatal_failure: Option<bool>,
    #[serde(default)]
    pub track: Option<String>,
    #[serde(default)]
    pub car_model: Option<String>,
    #[serde(default)]
    pub counters: Option<Counters>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Counters {
    #[serde(default)]
    pub frames_written: Option<u64>,
    #[serde(default)]
    pub skipped_duplicate: Option<u64>,
    #[serde(default)]
    pub skipped_no_position: Option<u64>,
    #[serde(default)]
    pub serialization_errors: Option<u64>,
    #[serde(default)]
    pub strings_sanitized: Option<u64>,
    #[serde(default)]
    pub non_finite_floats: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    pub name: String,
    #[serde(default)]
    pub pass: Option<bool>,
    #[serde(default)]
    pub fatal: Option<bool>,
    #[serde(default)]
    pub value: Option<String>,
}

impl Sidecar {
    /// Look for `<capture>.meta.json` beside the capture. Absent or unparseable
    /// returns `None`: captures without a sidecar are still usable.
    pub fn for_capture(capture: &Path) -> Option<Self> {
        let mut name = capture.file_name()?.to_os_string();
        name.push(".meta.json");
        let path: PathBuf = capture.with_file_name(name);
        let text = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Refuse captures the logger itself flagged as fatally broken.
    pub fn check(&self, capture: &Path) -> Result<()> {
        let bad = |detail: String| CoachError::BadCapture {
            path: capture.display().to_string(),
            detail,
        };

        if self.any_fatal_failure == Some(true) {
            let failed: Vec<&str> = self
                .checks
                .iter()
                .filter(|c| c.pass == Some(false) && c.fatal == Some(true))
                .map(|c| c.name.as_str())
                .collect();
            return Err(bad(format!(
                "the logger recorded a fatal probe failure ({}). \
                 Shared-memory offsets did not match this AC build, so the \
                 values in this capture are not trustworthy.",
                if failed.is_empty() {
                    "no specific check named".to_string()
                } else {
                    failed.join(", ")
                }
            )));
        }

        if self.ac_running == Some(false) {
            return Err(bad(
                "the logger recorded that Assetto Corsa was not running, so no \
                 telemetry was captured."
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Soft concerns worth telling the user about, but not worth refusing over.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Some(c) = &self.counters else {
            return out;
        };

        if c.strings_sanitized.unwrap_or(0) > 0 {
            out.push(format!(
                "{} string(s) contained non-text bytes — per the logger's own \
                 notes this means the struct layout does not match this AC \
                 build, and fields at or past the first bad offset are suspect",
                c.strings_sanitized.unwrap_or(0)
            ));
        }
        if c.non_finite_floats.unwrap_or(0) > 0 {
            out.push(format!(
                "{} NaN/Infinity value(s) were clamped to 0 on write — a zero \
                 in this capture may be a clamp rather than a reading",
                c.non_finite_floats.unwrap_or(0)
            ));
        }
        if c.serialization_errors.unwrap_or(0) > 0 {
            out.push(format!(
                "{} frame(s) failed to serialize and are missing from the capture",
                c.serialization_errors.unwrap_or(0)
            ));
        }
        out
    }
}
