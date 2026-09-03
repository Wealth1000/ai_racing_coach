//! Turning [`Advice`] into something the driver perceives while driving.
//!
//! The intelligence stack ends at [`crate::coaching::Advice`] — a complete,
//! self-contained sentence with the measured numbers in it. This module is
//! the layer that makes it audible (or, for tests and CI, records it
//! instead). It knows nothing about how the advice was produced and nothing
//! about where it came from; a sink is handed finished advice and decides
//! what to do with it.
//!
//! # The one rule: advice is perishable
//!
//! A braking tip delivered three corners late is worse than silence — the
//! driver has either braked by now or gone off. So a sink must never block
//! the consumer and never queue: [`TtsSink`] drops a line that arrives while
//! the previous one is still being spoken, counts it as skipped, and moves
//! on. The drop counters of the live wiring and the skip counters of the
//! sink together are the honest account of *everything the coach decided*
//! versus *everything the driver heard*.

pub mod sink;

pub use sink::{FeedbackSink, NullSink, Speech, TtsSink, UnavailableSpeech};
#[cfg(feature = "voice")]
pub use sink::SystemSpeech;
