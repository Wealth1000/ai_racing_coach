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

    /// The window could not be opened or the event loop failed. The session
    /// itself is fine; only the surface the driver watches it on is not.
    #[error("the GUI failed: {detail}")]
    Ui { detail: String },

    /// No provider in the registry recognised a capture. Carries each
    /// provider's reason for declining, so the message says what was tried
    /// rather than just that it failed — a foreign format is a diagnosis, not
    /// a mystery.
    #[error("{path} is not a recognised telemetry capture. Tried {attempts}")]
    UnrecognisedCapture {
        path: String,
        attempts: CaptureAttempts,
    },

    /// `--sim` named a key no provider registered. Lists the keys that exist
    /// so the fix is obvious.
    #[error("unknown sim '{key}' — registered sims: {known}")]
    UnknownSim { key: String, known: String },

    /// The provider has no live reader in this build. Distinct from a live
    /// reader that fails to attach: this sim's telemetry cannot be read live
    /// here at all.
    #[error("live telemetry from {sim} is not supported in this build")]
    LiveAttachUnsupported { sim: String },

    /// The provider can coach live but cannot record while doing so (no live
    /// reader in this build, or one that has no recorder). The caller falls
    /// back to plain live coaching — the recording is a byproduct, and its
    /// absence must not cost the session.
    #[error("recording while coaching is not supported for {sim} in this build")]
    LiveRecordUnsupported { sim: String },
}

/// Every provider's verdict on a capture none of them would open, formatted
/// for the [`CoachError::UnrecognisedCapture`] message.
#[derive(Debug, Clone)]
pub struct CaptureAttempts(pub Vec<(String, String)>);

impl fmt::Display for CaptureAttempts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (name, reason) in &self.0 {
            if !first {
                write!(f, "; ")?;
            }
            first = false;
            write!(f, "{name} ({reason})")?;
        }
        Ok(())
    }
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
