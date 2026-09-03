//! AI Sim Racing Coach — telemetry interpreter.
//!
//! Pipeline shape, raw bytes to corners:
//!
//! ```text
//!   .ndjson.gz  ──▶ sims::assetto_corsa       (strict AC schema, AC-only)
//!               ──▶ core::Sample              (canonical units + signs)
//!               ──▶ features::lap             (lap boundaries + quality)
//!               ──▶ features::resample            (fixed 1 m distance grid)
//!               ──▶ features::curvature/corner    (corner geometry)
//!               ──▶ features::track_model         (the canonical corner set)
//!               ──▶ features::corner_features     (what a lap did in each)
//!               ──▶ features::reference           (your best pass per corner)
//!               ──▶ models::DrivingModel          (what went wrong, by rule)
//!               ──▶ coaching::Phraser             (issue → words)
//!               ──▶ coaching::Advice              (ready for delivery)
//!               ──▶ coaching::DecisionEngine      (when to speak, what to suppress)
//! ```
//!
//! [`runtime`] is the same pipeline run live: one sample at a time, no lap
//! buffered whole, advice the moment a corner pass completes — golden-tested
//! to agree with the offline path above on real captures. [`audio`] is where
//! finished advice goes: the OS synthesiser behind a never-block, never-queue
//! gate (advice is perishable — a busy synth means a skipped line, not a
//! delayed one). [`storage`] is what a session leaves behind: an NDJSON
//! record of everything that happened, and the CSV export that turns recorded
//! sessions into a corpus. [`ui`] is the driver's window: the live feed and
//! counters on screen, owning nothing but the consumer's end of the wiring.
//!
//! The resampling stage is not cosmetic. Assetto Corsa publishes car position
//! on its *graphics* page, which updates at roughly 38 Hz while physics runs at
//! ~62 Hz, so ~39% of frames repeat the previous position verbatim. Menger
//! curvature needs three *distinct* consecutive points, so on raw frames it
//! collapses to exactly zero on 76-81% of samples. Putting the data on an
//! even distance grid first takes that to ~0-2%.

pub mod audio;
pub mod coaching;
pub mod core;
pub mod features;
pub mod models;
pub mod runtime;
pub mod sims;
pub mod storage;
pub mod telemetry;
pub mod ui;

pub use crate::core::{CaptureAttempts, CoachError, Result};
