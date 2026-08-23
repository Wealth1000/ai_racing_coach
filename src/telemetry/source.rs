//! The seam between "where telemetry comes from" and everything that reads it.
//!
//! Two implementations are planned: replay from a capture file (here today) and
//! a live reader off AC's shared memory (Windows only, later). Keeping the
//! pipeline behind this trait is what lets every stage downstream be developed
//! and tested on recorded laps without the sim running.

use crate::core::{Result, SessionInfo};
use crate::telemetry::frame::AcFrame;

pub trait TelemetrySource {
    /// Next frame, or `Ok(None)` at end of stream.
    ///
    /// Returning `Result<Option<_>>` rather than an `Iterator` keeps the
    /// distinction between "stream finished" and "stream broke" — an iterator
    /// yielding `None` conflates them, and a truncated capture should not look
    /// like a complete one.
    fn next_frame(&mut self) -> Result<Option<AcFrame>>;

    /// Session facts, available once the first frame has been read.
    fn session(&self) -> Option<&SessionInfo>;

    /// Human-readable description for logs and CLI output.
    fn describe(&self) -> String;
}
