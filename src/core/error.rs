//! Errors for the whole crate.
//!
//! The design rule here comes from the one thing that hurt most on the first
//! pass: a permissive parser that silently substituted `0.0` for every field it
//! could not find. That turned a schema mismatch into plausible-looking
//! telemetry, and the failure only surfaced 4 stages downstream as "0 corners
//! detected". Every variant below therefore names the *field* that went wrong,
//! so the error message points at the cause rather than the symptom.

use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum CoachError {
    #[error("i/o error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A line failed to deserialize. Carries the 1-based line number because a
    /// capture is ~50k lines and "invalid JSON" alone is not actionable.
    #[error("{path}:{line}: could not parse telemetry frame: {source}")]
    Json {
        path: String,
        line: usize,
        #[source]
        source: serde_json::Error,
    },

    /// The frame parsed, but a value is outside anything the sim can produce.
    /// This is the guard against the *silent* failure mode: a shared-memory
    /// struct-layout drift reads the right number of bytes at the wrong offsets
    /// and yields finite, well-formed, completely wrong numbers.
    #[error(
        "implausible value for {field}: {value} (expected {expected}) \
         — the capture is corrupt, or the logger's struct layout no longer \
         matches the sim's shared memory"
    )]
    ImplausibleValue {
        field: &'static str,
        value: String,
        expected: &'static str,
    },

    #[error("{path} does not look like Assetto Corsa telemetry: {detail}")]
    SchemaMismatch { path: String, detail: String },

    /// The logger's own sidecar reported that the capture is untrustworthy.
    /// Better to refuse than to analyse known-bad data.
    #[error("{path} was flagged by its own .meta.json sidecar: {detail}")]
    BadCapture { path: String, detail: String },

    #[error("{path} contains no usable telemetry frames")]
    EmptyCapture { path: String },

    /// The input parsed and was plausible, there was simply not enough of it to
    /// compute what was asked for. Distinct from [`Self::EmptyCapture`]: a
    /// capture of two laps is a perfectly good capture and still cannot yield a
    /// corner set that several laps agree on.
    #[error("not enough data to {action}: {detail}")]
    NotEnoughData {
        action: &'static str,
        detail: String,
    },

    /// A persisted artefact on disk was written by an incompatible version, or
    /// violates an invariant the loader relies on.
    #[error("{path} is not a usable {artefact}: {detail}")]
    BadArtefact {
        path: String,
        artefact: &'static str,
        detail: String,
    },
}

impl CoachError {
    /// Build an [`CoachError::ImplausibleValue`] from anything printable.
    pub fn implausible<T: fmt::Display>(
        field: &'static str,
        value: T,
        expected: &'static str,
    ) -> Self {
        Self::ImplausibleValue {
            field,
            value: value.to_string(),
            expected,
        }
    }
}

pub type Result<T> = std::result::Result<T, CoachError>;
