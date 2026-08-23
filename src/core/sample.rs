//! The canonical telemetry sample.
//!
//! # Sign and unit conventions
//!
//! Everything downstream of this module assumes the conventions below. They were
//! not chosen; they were measured, from both reference captures, and they are
//! recorded here so nobody has to re-derive them from the raw pages again.
//!
//! **Turning right is positive.** `yaw_rate`, the change in `heading`, and
//! curvature all share this sign. The evidence: every clean lap of Red Bull Ring
//! accumulates exactly +2*pi of heading change, and Red Bull Ring is driven
//! clockwise. Corroborated independently by the sign of the cross product of
//! successive position deltas, which agrees with the sign of d(heading) on
//! 99.2% / 98.8% of samples, and by `Physics_SteerAngle`, which agrees on
//! 95.6% / 97.2%.
//!
//! This is worth stating loudly because the first implementation had it
//! backwards: it mapped a positive heading change to a left-hand corner, which
//! reported all eight of Red Bull Ring's right-handers as lefts.
//!
//! **Heading is `-atan2(dx, dz)`.** AC's world is left-handed: X and Z span the
//! ground plane and Y is up. Of the four sign/argument-order candidates, this
//! one reproduces `Physics_Heading` with a median error of 0.20-0.24 deg; the
//! runner-up was out by 86 deg.
//!
//! **Yaw rate is the negation of `Physics_LocalAngularVelocity1`.** Measured
//! sign agreement with d(heading)/dt is 0.0% / 0.4% — that is, they disagree
//! essentially always — and the least-squares fit is k = -1.003 / -1.0005.
//!
//! **Slip angle is `atan2(lateral, longitudinal)` = `atan2(LocalVelocity0,
//! LocalVelocity2)`.** Index 2 is the longitudinal axis, not index 0:
//! `|LocalVelocity2| / speed` has a median of exactly 1.0000 while
//! `|LocalVelocity0| / speed` sits at 0.003.

use crate::core::ids::TrackId;
use crate::core::math::wrap_pi;
use crate::telemetry::frame::AcFrame;

/// Which simulator a capture came from. One variant today; the enum exists so
/// that adding a second sim is a compile error everywhere it matters rather
/// than a silently-wrong parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Sim {
    AssettoCorsa,
}

/// Facts that hold for a whole capture.
///
/// These come from the `StaticInfo_*` block, which the logger repeats on every
/// single frame. Reading them once and passing them alongside the sample stream
/// avoids carrying ~40 bytes of identical strings on all 51,383 frames.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub sim: Sim,
    pub track: TrackId,
    pub car: String,
    /// Metres. Authoritative, straight from `StaticInfo_TrackSPlineLength`.
    ///
    /// The first implementation instead *estimated* track length as the largest
    /// final lap distance across laps, and got 29.9 m for a 4.3 km circuit,
    /// because its lap grouping put every boundary just after the spline wrap.
    pub track_length: f32,
    pub sector_count: i32,
    pub ac_version: String,
    pub sm_version: String,
}

impl SessionInfo {
    pub fn from_frame(frame: &AcFrame) -> Self {
        let s = |o: &Option<String>| o.as_deref().unwrap_or("").trim().to_string();
        Self {
            sim: Sim::AssettoCorsa,
            track: TrackId::new(s(&frame.track), s(&frame.track_configuration)),
            car: s(&frame.car_model),
            track_length: frame.track_spline_length,
            sector_count: frame.sector_count,
            ac_version: s(&frame.ac_version),
            sm_version: s(&frame.sm_version),
        }
    }
}

/// One sample of driving, in canonical units.
///
/// Sim-agnostic by construction: nothing here mentions AC, and the conversions
/// that make that true live in [`Sample::from_ac_frame`].
#[derive(Debug, Clone, Copy, serde::Serialize)]
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
    /// Grip multiplier, ~0.98-1.0 on a dry track.
    pub surface_grip: f32,
    /// AC's own elapsed time on the current lap, in ms. Unlike `t_ms` this is a
    /// sim clock, so it is the right thing to compare lap times with.
    pub lap_time_ms: i32,
}

impl Sample {
    /// Convert a raw AC frame, applying every convention documented above.
    ///
    /// `track_length` comes from [`SessionInfo`] rather than from the frame so
    /// that one authoritative value is used for the whole capture.
    pub fn from_ac_frame(frame: &AcFrame, track_length: f32) -> Self {
        Self {
            t_ms: frame.timestamp,
            lap_frac: frame.normalized_car_position,
            lap_distance: frame.normalized_car_position * track_length,

            pos: frame.position(),
            heading: wrap_pi(frame.heading),

            speed: frame.speed_ms(),
            throttle: frame.gas,
            brake: frame.brake,
            steer: frame.steer_angle,

            // Negated: see the module docs. AC's body-frame yaw rate runs
            // opposite to d(heading)/dt.
            yaw_rate: -frame.local_ang_vel_1,

            // Index 2 is longitudinal, index 0 lateral.
            slip_angle: slip_angle(frame.local_vel_0, frame.local_vel_2),

            gear: frame.display_gear(),
            rpm: frame.rpms,
            tyres_out: frame.tyres_out.clamp(0, 4) as u8,
            surface_grip: frame.surface_grip,
            lap_time_ms: frame.i_current_time,
        }
    }
}

/// Slip angle from body-frame lateral and longitudinal velocity.
///
/// Returns 0 when the car is essentially stationary, where the angle is
/// meaningless and `atan2` would amplify sensor noise into a full-scale value.
fn slip_angle(lateral: f32, longitudinal: f32) -> f32 {
    if lateral.hypot(longitudinal) < 0.5 {
        return 0.0;
    }
    lateral.atan2(longitudinal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slip_angle_is_zero_at_a_standstill() {
        assert_eq!(slip_angle(0.001, 0.002), 0.0);
    }

    #[test]
    fn slip_angle_is_positive_when_sliding_right() {
        // Travelling forward at 30 m/s with 1 m/s of rightward drift.
        let a = slip_angle(1.0, 30.0);
        assert!(a > 0.0 && a < 0.1, "got {a}");
    }
}
