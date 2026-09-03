//! The live path: the same intelligence as `coach analyse`, fed one sample
//! at a time.
//!
//! * [`pipeline`] is the analysis itself — pure, deterministic, and golden-
//!   tested against the offline path on real captures.
//! * [`threads`] wraps it in the source/pipeline/consumer wiring a live
//!   session needs, with bounded channels so a slow consumer can never stall
//!   the sim.
//! * [`setup`] resolves the session a source discovered into the track model
//!   and personal best the coaching runs against.

pub mod pipeline;
pub mod setup;
pub mod threads;

pub use pipeline::{CoachPipeline, LiveLapTracker, RuntimeEvent, Stage, StreamingResampler};
pub use setup::{load_model_for_session, load_reference_for_session};
pub use threads::{LiveWiring, spawn};
