//! Plausibility guard for the first frame of a capture.
//!
//! Serde catches a *renamed* key. It cannot catch the failure mode the logger's
//! own sidecar warns about at length: if the C# struct layout drifts from the
//! shared-memory page AC actually publishes, every offset past the drift moves,
//! and the reads come back finite, well-formed and completely wrong. The
//! sidecar's own note is blunt about it — a previous logger version read car
//! position from offset 256 instead of 252, so `position_x/y/z` in those files
//! are really `y/z/penalty_time` and z was always zero.
//!
//! Nothing downstream can detect that. A misaligned float is still a float. So
//! the only defence is to check, once, that the values are in ranges the sim can
//! physically produce.
//!
//! The bounds below are deliberately the *same* ones the logger checks in its
//! own `checks` array, so the C# and Rust sides agree on what "sane" means and
//! a capture cannot pass one and fail the other.

use crate::core::{CoachError, Result};
use crate::telemetry::frame::AcFrame;

/// AC's own heading bound, from the logger's `physics.Heading` check: pi with
/// about 1% slack, because the page occasionally reports a hair past pi.
const HEADING_LIMIT: f32 = 3.173_008_6;

/// Verify a frame is physically plausible.
///
/// Called on the first successfully parsed frame of a capture. Running it on
/// every frame would be wasted work: a layout drift is a property of the whole
/// file, not of one line.
pub fn validate_frame(frame: &AcFrame) -> Result<()> {
    // ---- The sim's own state must be one of the four enum values ----------
    if frame.ac_status().is_none() {
        return Err(CoachError::implausible(
            "Graphics_Status",
            frame.status,
            "0 (OFF), 1 (REPLAY), 2 (LIVE) or 3 (PAUSE)",
        ));
    }

    // ---- The distance axis. Everything spatial depends on these two. -----
    let ncp = frame.normalized_car_position;
    if !ncp.is_finite() || ncp < 0.0 || ncp > 1.0 {
        return Err(CoachError::implausible(
            "Graphics_NormalizedCarPosition",
            ncp,
            "a spline fraction in 0.0 ..= 1.0",
        ));
    }
    let len = frame.track_spline_length;
    if !len.is_finite() || len < 100.0 || len > 30_000.0 {
        return Err(CoachError::implausible(
            "StaticInfo_TrackSPlineLength",
            len,
            "a track length in metres, 100 ..= 30000 (0 means no session was loaded)",
        ));
    }

    // ---- Orientation ------------------------------------------------------
    if !frame.heading.is_finite() || frame.heading.abs() > HEADING_LIMIT {
        return Err(CoachError::implausible(
            "Physics_Heading",
            frame.heading,
            "radians within +/-3.173",
        ));
    }

    // ---- Driver inputs ----------------------------------------------------
    for (field, v) in [("Physics_Gas", frame.gas), ("Physics_Brake", frame.brake)] {
        if !v.is_finite() || v < 0.0 || v > 1.01 {
            return Err(CoachError::implausible(
                field,
                v,
                "a pedal fraction in 0.0 ..= 1.01",
            ));
        }
    }
    if !frame.steer_angle.is_finite() || frame.steer_angle.abs() > 2.0 {
        return Err(CoachError::implausible(
            "Physics_SteerAngle",
            frame.steer_angle,
            "normalised steering within +/-2.0 (it is not radians)",
        ));
    }

    // ---- Speed ------------------------------------------------------------
    if !frame.speed_kmh.is_finite() || frame.speed_kmh < -20.0 || frame.speed_kmh > 600.0 {
        return Err(CoachError::implausible(
            "Physics_SpeedKmh",
            frame.speed_kmh,
            "km/h in -20 ..= 600",
        ));
    }

    // ---- Small-range integers. These are the best drift canaries: a shifted
    // read turns a 0..4 counter into a bit pattern from a neighbouring float.
    if !(0..=4).contains(&frame.tyres_out) {
        return Err(CoachError::implausible(
            "Physics_NumberOfTyresOut",
            frame.tyres_out,
            "0 ..= 4",
        ));
    }
    if !(0..=10).contains(&frame.gear) {
        return Err(CoachError::implausible(
            "Physics_Gear",
            frame.gear,
            "0 ..= 10",
        ));
    }
    if !(0..=10_000).contains(&frame.completed_laps) {
        return Err(CoachError::implausible(
            "Graphics_CompletedLaps",
            frame.completed_laps,
            "0 ..= 10000",
        ));
    }

    // ---- Position ---------------------------------------------------------
    let p = frame.position();
    if !p.iter().all(|v| v.is_finite()) {
        return Err(CoachError::implausible(
            "PositionX/Y/Z",
            format!("[{}, {}, {}]", p[0], p[1], p[2]),
            "finite world coordinates",
        ));
    }
    // An all-zero position is the signature of reading the wrong offset, and
    // also of a car not yet placed on track.
    if p[0] == 0.0 && p[1] == 0.0 && p[2] == 0.0 {
        return Err(CoachError::implausible(
            "PositionX/Y/Z",
            "[0, 0, 0]",
            "a real track coordinate (all-zero means the car is not on track, \
             or the graphics page is being read at the wrong offset)",
        ));
    }

    // ---- Identity. Empty here means the static page never published, which
    // makes the capture unattributable to a track even if the rest is fine.
    if frame.track.as_deref().unwrap_or("").trim().is_empty() {
        return Err(CoachError::implausible(
            "StaticInfo_Track",
            "(empty)",
            "a track folder name such as ks_red_bull_ring",
        ));
    }
    if frame.car_model.as_deref().unwrap_or("").trim().is_empty() {
        return Err(CoachError::implausible(
            "StaticInfo_CarModel",
            "(empty)",
            "a car folder name such as ks_mazda_mx5_cup",
        ));
    }

    Ok(())
}
