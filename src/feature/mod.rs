pub mod sample;
pub mod corner;
pub mod frechet;
pub use sample::{FrameSampler, FeatureSample, RawLapData};
pub use corner::{TrackCorner, CornerDirection, detect_corners, compute_heading_angle};