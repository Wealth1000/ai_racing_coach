//! AC frames → the canonical [`Sample`] and [`SessionInfo`].
//!
//! # Where every sign convention comes from
//!
//! These were not chosen; they were measured, from both reference captures,
//! and they are recorded here so nobody has to re-derive them from AC's raw
//! pages again.
//!
//! **Turning right is positive** (the canonical rule, set in
//! [`crate::core::sample`]). The evidence: every clean lap of Red Bull Ring
//! accumulates exactly +2*pi of heading change, and Red Bull Ring is driven
//! clockwise. Corroborated independently by the sign of the cross product of
//! successive position deltas, which agrees with the sign of d(heading) on
//! 99.2% / 98.8% of samples, and by `Physics_SteerAngle`, which agrees on
//! 95.6% / 97.2%.
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

use crate::core::math::wrap_pi;
use crate::core::sample::{Sample, SessionInfo, Sim};
use crate::core::ids::TrackId;
use crate::sims::assetto_corsa::frame::AcFrame;

impl SessionInfo {
    /// The session facts of an AC capture, from its first frame's
    /// `StaticInfo_*` block.
    pub fn from_ac_frame(frame: &AcFrame) -> Self {
        let s = |o: &Option<String>| o.as_deref().unwrap_or("").trim().to_string();
        Self {
            sim: Sim::AssettoCorsa,
            track: TrackId::new(s(&frame.track), s(&frame.track_configuration)),
            car: s(&frame.car_model),
            track_length: frame.track_spline_length,
            sector_count: frame.sector_count,
            sim_version: format!(
                "AC {}, SM {}",
                s(&frame.ac_version),
                s(&frame.sm_version)
            ),
        }
    }
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
            live: frame.is_live() && !frame.in_pits(),
            surface_grip: frame.surface_grip,
            lap_time_ms: frame.i_current_time,
            last_lap_time_ms: frame.i_last_time,
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

    #[test]
    fn the_latched_lap_time_rides_the_sample() {
        let frame = AcFrame {
            i_last_time: 91_234,
            ..crate::sims::assetto_corsa::frame::test_frame()
        };
        let sample = Sample::from_ac_frame(&frame, 4286.7896);
        assert_eq!(sample.last_lap_time_ms, 91_234);
    }
}
