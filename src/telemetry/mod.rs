//! The sim-agnostic seam between "where telemetry comes from" and everything
//! that reads it.
//!
//! Sources yield canonical [`crate::core::sample::Sample`]s — never a sim's
//! raw frame — so no stage downstream knows or cares which simulator is
//! driving. The sim-specific reading and conversion lives in
//! [`crate::sims`], one provider per simulator.

pub mod ndjson;
pub mod source;

pub use ndjson::NdjsonLines;
pub use source::{PrefixedSource, SourceStats, TelemetrySource};
