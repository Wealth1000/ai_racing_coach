//! Strict Assetto Corsa frame.
//!
//! # Why every field is required
//!
//! The previous version of this file annotated all ~30 fields with
//! `#[serde(default)]` and added `#[serde(flatten)] extra: Value` as a
//! catch-all, so that one deserializer could handle both AC and Automobilista 2
//! layouts. The cost was that *any* key it failed to find became `0.0` — a
//! finite, plausible, silently wrong number. A key rename in the logger would
//! not produce an error; it would produce a lap of telemetry where the car
//! never moved, and the first visible symptom appeared four stages downstream
//! as "0 corners detected".
//!
//! So: no `default`, no `flatten`. A missing key is a hard error naming the
//! key. This is safe to do because the logger emits a fixed key set — measured
//! at 192 keys, byte-identical on all 79,406 frames of both reference captures.
//! The ~157 keys not listed below are simply ignored by serde, which is the
//! intended behaviour: the contract is over the fields we actually read.
//!
//! The `StaticInfo_*` string fields are `Option<String>` rather than `String`
//! because the logger writes JSON `null` for them when AC's static page has not
//! been published yet (`ACProgram.cs:482-530`). The *key* is always present;
//! only the value is nullable, and `Option<T>` without `default` still requires
//! the key. `Graphics_CurrentTime`/`LastTime` are deliberately not read — those
//! are display strings; the authoritative values are the `i`-prefixed integers.

use serde::Deserialize;

/// One line of the logger's NDJSON output.
///
/// Field order follows the logger's own emission order so the two can be diffed
/// against each other by eye.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AcFrame {
    // ---- Timestamp & position -------------------------------------------
    /// Wall-clock milliseconds (`DateTimeOffset.UtcNow`, `ACProgram.cs:322`).
    ///
    /// This is *not* a sim clock: it does not pause when the sim pauses and it
    /// is subject to NTP steps. Use it for frame ordering and dt only; use
    /// distance for anything spatial and `Graphics_iLastTime` for lap times.
    #[serde(rename = "Timestamp")]
    pub timestamp: i64,
    #[serde(rename = "SequenceNumber")]
    pub sequence: i64,

    /// World position, from AC's graphics page. Left-handed: X and Z span the
    /// ground plane, Y is up. Note this updates at ~38 Hz while the frame rate
    /// is ~62 Hz, so consecutive frames often repeat it verbatim.
    #[serde(rename = "PositionX")]
    pub pos_x: f32,
    #[serde(rename = "PositionY")]
    pub pos_y: f32,
    #[serde(rename = "PositionZ")]
    pub pos_z: f32,

    // ---- Physics page ----------------------------------------------------
    #[serde(rename = "Physics_PacketId")]
    pub physics_packet_id: i64,
    #[serde(rename = "Physics_Gas")]
    pub gas: f32,
    #[serde(rename = "Physics_Brake")]
    pub brake: f32,
    /// AC's raw gear index: 0 = reverse, 1 = neutral, 2 = first. Subtract 1 for
    /// the number on the dash.
    #[serde(rename = "Physics_Gear")]
    pub gear: i32,
    #[serde(rename = "Physics_Rpms")]
    pub rpms: f32,
    /// Normalised steering, about -1..1 — *not* radians. Measured range over
    /// both captures: -0.91 .. 1.00. Positive is right.
    #[serde(rename = "Physics_SteerAngle")]
    pub steer_angle: f32,
    #[serde(rename = "Physics_SpeedKmh")]
    pub speed_kmh: f32,

    /// Yaw, radians. Measured to satisfy `heading == -atan2(dx, dz)` with a
    /// median error of 0.20-0.24 deg across both cars.
    #[serde(rename = "Physics_Heading")]
    pub heading: f32,
    #[serde(rename = "Physics_Pitch")]
    pub pitch: f32,
    #[serde(rename = "Physics_Roll")]
    pub roll: f32,

    #[serde(rename = "Physics_NumberOfTyresOut")]
    pub tyres_out: i32,
    #[serde(rename = "Physics_PitLimiterOn")]
    pub pit_limiter_on: i32,

    /// Body-frame angular velocity. Index 1 is yaw rate, but with the *opposite*
    /// sign to d(heading)/dt — see [`crate::core::sample::Sample::yaw_rate`].
    #[serde(rename = "Physics_LocalAngularVelocity0")]
    pub local_ang_vel_0: f32,
    #[serde(rename = "Physics_LocalAngularVelocity1")]
    pub local_ang_vel_1: f32,
    #[serde(rename = "Physics_LocalAngularVelocity2")]
    pub local_ang_vel_2: f32,

    /// Body-frame velocity. Index 2 is *longitudinal* and index 0 is lateral —
    /// measured `|LocalVelocity2| / speed == 1.0000` (median) against
    /// `|LocalVelocity0| / speed == 0.003`.
    #[serde(rename = "Physics_LocalVelocity0")]
    pub local_vel_0: f32,
    #[serde(rename = "Physics_LocalVelocity1")]
    pub local_vel_1: f32,
    #[serde(rename = "Physics_LocalVelocity2")]
    pub local_vel_2: f32,

    // ---- Graphics page ---------------------------------------------------
    #[serde(rename = "Graphics_PacketId")]
    pub graphics_packet_id: i64,
    /// `AC_OFF = 0`, `AC_REPLAY = 1`, `AC_LIVE = 2`, `AC_PAUSE = 3`.
    #[serde(rename = "Graphics_Status")]
    pub status: i32,
    /// AC's own lap counter. **Do not use this to delimit laps** — it lags the
    /// start/finish crossing by 1-2 frames and never increments on the first
    /// crossing of a session joined mid-lap. `features::lap` uses the
    /// `NormalizedCarPosition` wrap instead.
    #[serde(rename = "Graphics_CompletedLaps")]
    pub completed_laps: i32,
    /// Milliseconds elapsed on the current lap, per AC.
    #[serde(rename = "Graphics_iCurrentTime")]
    pub i_current_time: i32,
    /// AC's authoritative time for the last completed lap, in milliseconds.
    /// Latches 1-3 frames *after* the line crossing. `0` means "no lap yet".
    #[serde(rename = "Graphics_iLastTime")]
    pub i_last_time: i32,
    #[serde(rename = "Graphics_DistanceTraveled")]
    pub distance_travelled: f32,
    #[serde(rename = "Graphics_IsInPit")]
    pub is_in_pit: i32,
    #[serde(rename = "Graphics_IsInPitLane")]
    pub is_in_pit_lane: i32,
    #[serde(rename = "Graphics_CurrentSectorIndex")]
    pub sector_index: i32,
    /// Position along the track spline, 0..1. Multiplied by
    /// `StaticInfo_TrackSPlineLength` this is the canonical distance axis:
    /// measured to track true travelled distance to within +/-2 cm per frame,
    /// and being spline-based it is independent of the racing line, so the same
    /// value means the same place on the track across laps and across cars.
    #[serde(rename = "Graphics_NormalizedCarPosition")]
    pub normalized_car_position: f32,
    #[serde(rename = "Graphics_SurfaceGrip")]
    pub surface_grip: f32,

    // ---- Static page -----------------------------------------------------
    #[serde(rename = "StaticInfo_SMVersion")]
    pub sm_version: Option<String>,
    #[serde(rename = "StaticInfo_ACVersion")]
    pub ac_version: Option<String>,
    #[serde(rename = "StaticInfo_CarModel")]
    pub car_model: Option<String>,
    #[serde(rename = "StaticInfo_Track")]
    pub track: Option<String>,
    #[serde(rename = "StaticInfo_TrackConfiguration")]
    pub track_configuration: Option<String>,
    /// Track length in metres — authoritative, constant, and present on every
    /// frame (4286.7896 for Red Bull Ring GP). `0` before a session loads.
    #[serde(rename = "StaticInfo_TrackSPlineLength")]
    pub track_spline_length: f32,
    #[serde(rename = "StaticInfo_SectorCount")]
    pub sector_count: i32,
}

/// AC's `AC_STATUS` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcStatus {
    Off,
    Replay,
    Live,
    Pause,
}

impl AcFrame {
    pub fn ac_status(&self) -> Option<AcStatus> {
        match self.status {
            0 => Some(AcStatus::Off),
            1 => Some(AcStatus::Replay),
            2 => Some(AcStatus::Live),
            3 => Some(AcStatus::Pause),
            _ => None,
        }
    }

    /// True when the sim is actually running a session the driver is driving.
    ///
    /// The old gate tested a key called `GameState`, which does not exist
    /// anywhere in AC's schema, so it silently never fired and paused frames
    /// flowed straight through.
    pub fn is_live(&self) -> bool {
        self.ac_status() == Some(AcStatus::Live)
    }

    pub fn in_pits(&self) -> bool {
        self.is_in_pit != 0 || self.is_in_pit_lane != 0
    }

    /// Gear as shown on the dash: -1 reverse, 0 neutral, 1.. forward.
    pub fn display_gear(&self) -> i8 {
        (self.gear - 1) as i8
    }

    pub fn speed_ms(&self) -> f32 {
        self.speed_kmh / 3.6
    }

    /// Distance along the track spline, in metres.
    pub fn lap_distance(&self) -> f32 {
        self.normalized_car_position * self.track_spline_length
    }

    pub fn position(&self) -> [f32; 3] {
        [self.pos_x, self.pos_y, self.pos_z]
    }
}

/// A plausible live frame, for tests that need to drive the pipeline without a
/// capture file. Red Bull Ring GP in the MX5, stationary on the line.
///
/// Every field is set explicitly, so adding a field to `AcFrame` makes this fail
/// to compile rather than silently defaulting — which is the whole point of the
/// struct having no `#[serde(default)]`.
#[cfg(test)]
pub fn test_frame() -> AcFrame {
    AcFrame {
        timestamp: 0,
        sequence: 0,
        pos_x: 0.0,
        pos_y: 0.0,
        pos_z: 0.0,
        physics_packet_id: 0,
        gas: 0.0,
        brake: 0.0,
        gear: 1,
        rpms: 1000.0,
        steer_angle: 0.0,
        speed_kmh: 0.0,
        heading: 0.0,
        pitch: 0.0,
        roll: 0.0,
        tyres_out: 0,
        pit_limiter_on: 0,
        local_ang_vel_0: 0.0,
        local_ang_vel_1: 0.0,
        local_ang_vel_2: 0.0,
        local_vel_0: 0.0,
        local_vel_1: 0.0,
        local_vel_2: 0.0,
        graphics_packet_id: 0,
        status: 2,
        completed_laps: 0,
        i_current_time: 0,
        i_last_time: 0,
        distance_travelled: 0.0,
        is_in_pit: 0,
        is_in_pit_lane: 0,
        sector_index: 0,
        normalized_car_position: 0.0,
        surface_grip: 1.0,
        sm_version: Some("1.7".to_string()),
        ac_version: Some("1.14.1".to_string()),
        car_model: Some("ks_mazda_mx5_cup".to_string()),
        track: Some("ks_red_bull_ring".to_string()),
        track_configuration: Some("layout_gp".to_string()),
        track_spline_length: 4286.7896,
        sector_count: 3,
    }
}
