//! AI Sim Racing Coach — telemetry interpreter.
//!
//! Pipeline shape, raw bytes to corners:
//!
//! ```text
//!   .ndjson.gz  ──▶ telemetry::replay ──▶ AcFrame  (strict, AC-only)
//!               ──▶ core::Sample                  (canonical units + signs)
//!               ──▶ features::lap                 (lap boundaries + quality)
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
//! The resampling stage is not cosmetic. Assetto Corsa publishes car position
//! on its *graphics* page, which updates at roughly 38 Hz while physics runs at
//! ~62 Hz, so ~39% of frames repeat the previous position verbatim. Menger
//! curvature needs three *distinct* consecutive points, so on raw frames it
//! collapses to exactly zero on 76-81% of samples. Putting the data on an
//! even distance grid first takes that to ~0-2%.

pub mod coaching;
pub mod core;
pub mod features;
pub mod models;
pub mod telemetry;

pub use crate::core::{CoachError, Result};
