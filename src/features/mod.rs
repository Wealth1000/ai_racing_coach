//! Turning a frame stream into features: laps, an even distance grid, curvature,
//! corners.
//!
//! The order here is the pipeline order, and each stage depends on the one above
//! it being correct. In particular [`resample`] must run before [`curvature`]:
//! on raw Assetto Corsa frames the curvature is degenerate on 76-81% of samples,
//! and no amount of threshold tuning recovers a corner from that.

pub mod consensus;
pub mod corner;
pub mod corner_features;
pub mod curvature;
pub mod decision;
pub mod frechet;
pub mod lap;
pub mod line;
pub mod reference;
pub mod resample;
pub mod segment;
pub mod stats;
pub mod track_model;

pub use corner::{CornerDirection, CornerParams, TrackCorner, detect_corners};
pub use corner_features::{CornerFeatures, FeatureParams, extract_all};
pub use curvature::{CornerProfiles, corner_profiles, signed_curvature};
pub use lap::{Lap, LapBoundaryDetector, LapQuality, LapScore, LapScorer, LapTracker};
pub use reference::{CornerReference, MergeReport, ReferenceStore};
pub use resample::{DEFAULT_STEP_M, ResampledLap, resample_lap};
pub use track_model::{LearnParams, ModelCorner, TrackModel};
