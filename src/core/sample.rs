//! The canonical telemetry sample.
//!
//! # Sign and unit conventions
//!
//! Everything downstream of this module assumes the conventions below. They
//! were not chosen; they were measured, and each sim provider is responsible
//! for producing them — the AC derivations are recorded in
//! [`crate::sims::assetto_corsa::convert`] so nobody has to re-derive them
//! from the raw pages again.
//!
//! **Turning right is positive.** `yaw_rate`, the change in `heading`, and
//! curvature all share this sign. This is worth stating loudly because the
//! first implementation had it backwards: it mapped a positive heading change
//! to a left-hand corner, which reported all eight of Red Bull Ring's
//! right-handers as lefts.

use crate::core::ids::TrackId;

/// Which simulator a capture came from. One variant today; the enum exists so
/// that adding a second sim is a compile error everywhere it matters rather
/// than a silently-wrong parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Sim {
    AssettoCorsa,
}

impl Sim {
    /// The sim's short key, used in on-disk paths (`data/tracks/ac/`) and the
    /// `--sim` flag. Stable once chosen: files on disk carry it.
    pub fn key(self) -> &'static str {
        match self {
            Sim::AssettoCorsa => "ac",
        }
    }

    /// The sim's human name, for messages the driver reads.
    pub fn name(self) -> &'static str {
        match self {
            Sim::AssettoCorsa => "Assetto Corsa",
        }
    }
}

/// Facts that hold for a whole capture.
///
/// Read once from the first frame (the static page in AC, the equivalent
/// block in any other sim) and passed alongside the sample stream, so the
/// per-sample stream does not carry identical strings on every frame.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub sim: Sim,
    pub track: TrackId,
    pub car: String,
    /// Metres. Authoritative, from the sim's own track length.
    ///
    /// The first implementation instead *estimated* track length as the largest
    /// final lap distance across laps, and got 29.9 m for a 4.3 km circuit,
    /// because its lap grouping put every boundary just after the spline wrap.
    pub track_length: f32,
    pub sector_count: i32,
    /// The sim's own version string(s), as the provider formats them (AC's
    /// is `"AC 1.16.4, SM 1.7"`). Opaque to everything downstream: it exists
    /// so captures can be described, not parsed.
    pub sim_version: String,
}

/// One sample of driving, in canonical units.
///
/// Sim-agnostic by construction: nothing here mentions any simulator, and the
/// conversions that make that true live in each provider (AC's is
/// [`crate::sims::assetto_corsa::convert`]).
///
/// `PartialEq` is exact, not approximate: the offline and streaming resamplers
/// share their arithmetic, so the golden test can demand bit-for-bit equality
/// rather than a tolerance.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Sample {
    /// Wall-clock milliseconds. Fine for ordering and dt; not a sim clock.
    pub t_ms: i64,
    /// Metres along the track spline from the start/finish line.
    pub lap_distance: f32,
    /// The same quantity as a 0..1 fraction.
    pub lap_frac: f32,

    /// World position. Ground plane is `(pos[0], pos[2])`; `pos[1]` is up.
    pub pos: [f32; 3],
    /// Yaw in radians, wrapped to `(-pi, pi]`. Increasing = turning right.
    pub heading: f32,

    /// Metres per second.
    pub speed: f32,
    /// 0..1.
    pub throttle: f32,
    /// 0..1.
    pub brake: f32,
    /// Normalised, about -1..1. Positive is right.
    pub steer: f32,

    /// Radians per second, positive to the right.
    pub yaw_rate: f32,
    /// Radians. Positive means the car's velocity points right of where it is
    /// pointing.
    pub slip_angle: f32,

    /// -1 reverse, 0 neutral, 1.. forward.
    pub gear: i8,
    pub rpm: f32,
    /// 0..4. Three or more is off-track by AC's own reckoning.
    pub tyres_out: u8,
    /// True when the sim is running a live session and the car is out of the
    /// pits — i.e. this sample describes real driving. False for paused,
    /// replayed and pit-lane frames, none of which may count toward a
    /// reference (see `features::lap`).
    pub live: bool,
    /// Grip multiplier, ~0.98-1.0 on a dry track.
    pub surface_grip: f32,
    /// AC's own elapsed time on the current lap, in ms. Unlike `t_ms` this is a
    /// sim clock, so it is the right thing to compare lap times with.
    pub lap_time_ms: i32,
    /// The sim's authoritative time for the lap that just ended, in ms. Sims
    /// latch this a few frames *after* the start/finish crossing, which is why
    /// it rides every sample: the lap tracker needs it only after a boundary,
    /// and never knows in advance which sample carries it. `0` means "no lap
    /// completed yet" (or the sim does not report one).
    pub last_lap_time_ms: i32,
}
