//! The live path: the same intelligence as `coach analyse`, fed one sample
//! at a time.
//!
//! * [`pipeline`] is the analysis itself — pure, deterministic, and golden-
//!   tested against the offline path on real captures.
//! * [`threads`] wraps it in the source/pipeline/consumer wiring a live
//!   session needs, with bounded channels so a slow consumer can never stall
//!   the sim.

pub mod pipeline;
pub mod threads;

pub use pipeline::{CoachPipeline, LiveLapTracker, RuntimeEvent, Stage, StreamingResampler};
pub use threads::{LiveWiring, spawn};
