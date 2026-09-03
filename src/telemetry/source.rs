//! The seam between "where telemetry comes from" and everything that reads it.
//!
//! Sources yield canonical [`Sample`]s — never a sim's raw frame — so the
//! whole pipeline downstream is sim-agnostic and can be developed and tested
//! on recorded laps without the sim running. Implementations live with their
//! providers in [`crate::sims`]: replay from a capture file today, and (per
//! sim) live readers off shared memory or sockets.
//!
//! `Send` is a supertrait because a source always ends up owned by the live
//! pipeline's source thread ([`crate::runtime::spawn`]); a source that cannot
//! move across threads is not a live source.
//!
//! # Shutdown
//!
//! The source thread's loop checks a stop flag *between* calls to
//! [`TelemetrySource::next_sample`] — which is enough for a capture file,
//! where each call returns promptly. A live source is different: it can sit
//! inside `next_sample` for as long as the sim publishes nothing. Such a
//! source observes the flag through [`TelemetrySource::set_stop_flag`]:
//! [`crate::runtime::spawn`] hands the flag over before the loop starts, and
//! a blocking poll checks it and returns `Ok(None)` to end the stream.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::core::sample::{Sample, SessionInfo};
use crate::core::Result;

pub trait TelemetrySource: Send {
    /// Next sample, or `Ok(None)` at end of stream.
    ///
    /// Returning `Result<Option<_>>` rather than an `Iterator` keeps the
    /// distinction between "stream finished" and "stream broke" — an iterator
    /// yielding `None` conflates them, and a truncated capture should not look
    /// like a complete one.
    fn next_sample(&mut self) -> Result<Option<Sample>>;

    /// Hand the live wiring's stop flag to a source that can block.
    ///
    /// Sources whose `next_sample` always returns promptly — a capture
    /// replay, a probe wrapper — do not need it, and the default no-op is
    /// for them. A source that *waits* inside `next_sample` (a shared-memory
    /// poll loop, a socket read) stores the flag, checks it inside the wait,
    /// and returns `Ok(None)` when it is set, so that shutdown does not hang
    /// on a sim that has gone quiet.
    fn set_stop_flag(&mut self, _stop: Arc<AtomicBool>) {}

    /// Session facts, available once the first sample has been read.
    fn session(&self) -> Option<&SessionInfo>;

    /// Human-readable description for logs and CLI output.
    fn describe(&self) -> String;

    /// Read counters for `coach inspect` and the GUI's connection panel.
    ///
    /// A default rather than a required method: a source that cannot count
    /// (a socket with no framing stats) still implements the trait, and the
    /// counters it genuinely lacks read as zero rather than breaking the
    /// panel that displays them.
    fn stats(&self) -> SourceStats {
        SourceStats::default()
    }
}

/// How many samples a source has handed over, and how many lines it could
/// not use. The three counters mirror what a file-backed source knows about
/// its input; a live source reports samples only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// Samples successfully parsed and yielded.
    pub samples: usize,
    /// Lines that did not parse. Nonzero only for file-backed sources.
    pub bad_lines: usize,
    /// Blank lines skipped. Nonzero only for file-backed sources.
    pub blank_lines: usize,
}

/// Yields one sample read ahead of opening, then everything the inner source
/// produces.
///
/// This is how a probe hands back what it consumed: the provider registry
/// claims a capture by reading its first sample, and this wrapper puts that
/// sample back at the head of the stream so the session still sees every
/// sample exactly once.
pub struct PrefixedSource {
    pending: Option<Sample>,
    inner: Box<dyn TelemetrySource>,
}

impl PrefixedSource {
    pub fn new(pending: Sample, inner: Box<dyn TelemetrySource>) -> Self {
        Self {
            pending: Some(pending),
            inner,
        }
    }
}

impl TelemetrySource for PrefixedSource {
    fn next_sample(&mut self) -> Result<Option<Sample>> {
        match self.pending.take() {
            Some(s) => Ok(Some(s)),
            None => self.inner.next_sample(),
        }
    }

    // The prefix is one already-read sample; anything that blocks blocks in
    // the inner source, so the flag belongs there.
    fn set_stop_flag(&mut self, stop: Arc<AtomicBool>) {
        self.inner.set_stop_flag(stop);
    }

    // Delegated, not forwarded: the inner source has already read the sample
    // the prefix holds, so its session and description are current.
    fn session(&self) -> Option<&SessionInfo> {
        self.inner.session()
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }

    fn stats(&self) -> SourceStats {
        self.inner.stats()
    }
}
