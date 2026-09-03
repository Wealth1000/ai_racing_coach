//! `coach record`: capture telemetry live from the running sim.
//!
//! This is the C# logger's job, done by the coach itself — one program on the
//! sim machine instead of two. The contract is interchange: a capture this
//! module writes is the same NDJSON the logger writes, key for key, so
//! `coach inspect`, `learn-track` and everything else cannot tell them apart
//! and no format fork ever opens.
//!
//! The recording loop mirrors the logger's (`Program.cs`): wait for the sim,
//! then poll the physics page at 10 ms, skip polls until the car is on track,
//! dedupe republished frames by packet id (once the id has been seen to
//! advance), re-read the static page about once a second, write every
//! surviving frame as one JSON line, gzip by default, flush every 200 lines,
//! and never overwrite an existing file. The skip and dedupe rules are not
//! re-implemented here — they live in [`FrameAssembler`], shared with the
//! live source, so a recorded capture and a live coaching session agree on
//! what counts as a frame.
//!
//! Unlike the logger, no `.meta.json` sidecar is written: the sidecar exists
//! to carry the logger's own probe verdicts, and this recorder *is* the
//! probe — the first frame passes the same plausibility guard a capture's
//! first line does, live ([`crate::sims::assetto_corsa::schema`]).

use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::Serialize;

use crate::core::{CoachError, Result};
use crate::sims::assetto_corsa::shared_memory::{
    FrameAssembler, GraphicsPage, PageStore, PhysicsPage, SkipReason, StaticPage, now_unix_ms,
    wchar_ascii, wchar_string, ATTACH_RETRY, PHYSICS_POLL, STATIC_POLL,
};
use crate::sims::{RecordOptions, RecordSummary};

/// The logger's float policy: a NaN or infinity (which AC's pages can carry
/// as 0x7F800000-pattern garbage during teardown) is written as 0 rather than
/// as JSON's `null`, because a number the pipeline can read beats a hole it
/// has to skip.
fn sf(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// The logger's NDJSON line: one flat object, 192 keys, exactly the key set
/// `Program.cs` writes (measured on the reference captures: 192 keys,
/// byte-identical on every frame). Field order follows the logger's emission
/// order so a line of ours and a line of its can be diffed by eye.
///
/// Only the fields the pipeline reads are shared with [`AcFrame`]; the rest
/// exist so a capture recorded by `coach record` is interchangeable with one
/// recorded by the C# logger. Values agree; the last byte-level difference
/// is float formatting on integral values (`1` vs `1.0`), which no reader of
/// these files distinguishes.
#[derive(Debug, Serialize)]
pub struct RecordFrame {
    #[serde(rename = "Timestamp")]
    pub timestamp: i64,
    #[serde(rename = "SequenceNumber")]
    pub sequence: i64,
    #[serde(rename = "PositionX")]
    pub position_x: f32,
    #[serde(rename = "PositionY")]
    pub position_y: f32,
    #[serde(rename = "PositionZ")]
    pub position_z: f32,
    #[serde(rename = "Physics_PacketId")]
    pub physics_packet_id: i32,
    #[serde(rename = "Physics_Gas")]
    pub physics_gas: f32,
    #[serde(rename = "Physics_Brake")]
    pub physics_brake: f32,
    #[serde(rename = "Physics_Fuel")]
    pub physics_fuel: f32,
    #[serde(rename = "Physics_Gear")]
    pub physics_gear: i32,
    #[serde(rename = "Physics_Rpms")]
    pub physics_rpms: i32,
    #[serde(rename = "Physics_SteerAngle")]
    pub physics_steer_angle: f32,
    #[serde(rename = "Physics_SpeedKmh")]
    pub physics_speed_kmh: f32,
    #[serde(rename = "Physics_Velocity0")]
    pub physics_velocity_0: f32,
    #[serde(rename = "Physics_Velocity1")]
    pub physics_velocity_1: f32,
    #[serde(rename = "Physics_Velocity2")]
    pub physics_velocity_2: f32,
    #[serde(rename = "Physics_AccG0")]
    pub physics_acc_g_0: f32,
    #[serde(rename = "Physics_AccG1")]
    pub physics_acc_g_1: f32,
    #[serde(rename = "Physics_AccG2")]
    pub physics_acc_g_2: f32,
    #[serde(rename = "Physics_WheelSlip0")]
    pub wheel_slip_0: f32,
    #[serde(rename = "Physics_WheelSlip1")]
    pub wheel_slip_1: f32,
    #[serde(rename = "Physics_WheelSlip2")]
    pub wheel_slip_2: f32,
    #[serde(rename = "Physics_WheelSlip3")]
    pub wheel_slip_3: f32,
    #[serde(rename = "Physics_WheelLoad0")]
    pub wheel_load_0: f32,
    #[serde(rename = "Physics_WheelLoad1")]
    pub wheel_load_1: f32,
    #[serde(rename = "Physics_WheelLoad2")]
    pub wheel_load_2: f32,
    #[serde(rename = "Physics_WheelLoad3")]
    pub wheel_load_3: f32,
    #[serde(rename = "Physics_WheelPressure0")]
    pub wheel_pressure_0: f32,
    #[serde(rename = "Physics_WheelPressure1")]
    pub wheel_pressure_1: f32,
    #[serde(rename = "Physics_WheelPressure2")]
    pub wheel_pressure_2: f32,
    #[serde(rename = "Physics_WheelPressure3")]
    pub wheel_pressure_3: f32,
    #[serde(rename = "Physics_WheelAngularSpeed0")]
    pub wheel_angular_speed_0: f32,
    #[serde(rename = "Physics_WheelAngularSpeed1")]
    pub wheel_angular_speed_1: f32,
    #[serde(rename = "Physics_WheelAngularSpeed2")]
    pub wheel_angular_speed_2: f32,
    #[serde(rename = "Physics_WheelAngularSpeed3")]
    pub wheel_angular_speed_3: f32,
    #[serde(rename = "Physics_TyreWear0")]
    pub tyre_wear_0: f32,
    #[serde(rename = "Physics_TyreWear1")]
    pub tyre_wear_1: f32,
    #[serde(rename = "Physics_TyreWear2")]
    pub tyre_wear_2: f32,
    #[serde(rename = "Physics_TyreWear3")]
    pub tyre_wear_3: f32,
    #[serde(rename = "Physics_TyreDirtyLevel0")]
    pub tyre_dirty_level_0: f32,
    #[serde(rename = "Physics_TyreDirtyLevel1")]
    pub tyre_dirty_level_1: f32,
    #[serde(rename = "Physics_TyreDirtyLevel2")]
    pub tyre_dirty_level_2: f32,
    #[serde(rename = "Physics_TyreDirtyLevel3")]
    pub tyre_dirty_level_3: f32,
    #[serde(rename = "Physics_TyreCoreTemp0")]
    pub tyre_core_temp_0: f32,
    #[serde(rename = "Physics_TyreCoreTemp1")]
    pub tyre_core_temp_1: f32,
    #[serde(rename = "Physics_TyreCoreTemp2")]
    pub tyre_core_temp_2: f32,
    #[serde(rename = "Physics_TyreCoreTemp3")]
    pub tyre_core_temp_3: f32,
    #[serde(rename = "Physics_CamberRad0")]
    pub camber_rad_0: f32,
    #[serde(rename = "Physics_CamberRad1")]
    pub camber_rad_1: f32,
    #[serde(rename = "Physics_CamberRad2")]
    pub camber_rad_2: f32,
    #[serde(rename = "Physics_CamberRad3")]
    pub camber_rad_3: f32,
    #[serde(rename = "Physics_SuspensionTravel0")]
    pub suspension_travel_0: f32,
    #[serde(rename = "Physics_SuspensionTravel1")]
    pub suspension_travel_1: f32,
    #[serde(rename = "Physics_SuspensionTravel2")]
    pub suspension_travel_2: f32,
    #[serde(rename = "Physics_SuspensionTravel3")]
    pub suspension_travel_3: f32,
    #[serde(rename = "Physics_BrakeTemp0")]
    pub brake_temp_0: f32,
    #[serde(rename = "Physics_BrakeTemp1")]
    pub brake_temp_1: f32,
    #[serde(rename = "Physics_BrakeTemp2")]
    pub brake_temp_2: f32,
    #[serde(rename = "Physics_BrakeTemp3")]
    pub brake_temp_3: f32,
    #[serde(rename = "Physics_TyreTempI0")]
    pub tyre_temp_i_0: f32,
    #[serde(rename = "Physics_TyreTempI1")]
    pub tyre_temp_i_1: f32,
    #[serde(rename = "Physics_TyreTempI2")]
    pub tyre_temp_i_2: f32,
    #[serde(rename = "Physics_TyreTempI3")]
    pub tyre_temp_i_3: f32,
    #[serde(rename = "Physics_TyreTempM0")]
    pub tyre_temp_m_0: f32,
    #[serde(rename = "Physics_TyreTempM1")]
    pub tyre_temp_m_1: f32,
    #[serde(rename = "Physics_TyreTempM2")]
    pub tyre_temp_m_2: f32,
    #[serde(rename = "Physics_TyreTempM3")]
    pub tyre_temp_m_3: f32,
    #[serde(rename = "Physics_TyreTempO0")]
    pub tyre_temp_o_0: f32,
    #[serde(rename = "Physics_TyreTempO1")]
    pub tyre_temp_o_1: f32,
    #[serde(rename = "Physics_TyreTempO2")]
    pub tyre_temp_o_2: f32,
    #[serde(rename = "Physics_TyreTempO3")]
    pub tyre_temp_o_3: f32,
    #[serde(rename = "Physics_Drs")]
    pub physics_drs: f32,
    #[serde(rename = "Physics_TC")]
    pub physics_tc: f32,
    #[serde(rename = "Physics_Heading")]
    pub physics_heading: f32,
    #[serde(rename = "Physics_Pitch")]
    pub physics_pitch: f32,
    #[serde(rename = "Physics_Roll")]
    pub physics_roll: f32,
    #[serde(rename = "Physics_CgHeight")]
    pub physics_cg_height: f32,
    #[serde(rename = "Physics_CarDamage0")]
    pub physics_car_damage_0: f32,
    #[serde(rename = "Physics_CarDamage1")]
    pub physics_car_damage_1: f32,
    #[serde(rename = "Physics_CarDamage2")]
    pub physics_car_damage_2: f32,
    #[serde(rename = "Physics_CarDamage3")]
    pub physics_car_damage_3: f32,
    #[serde(rename = "Physics_CarDamage4")]
    pub physics_car_damage_4: f32,
    #[serde(rename = "Physics_NumberOfTyresOut")]
    pub physics_number_of_tyres_out: i32,
    #[serde(rename = "Physics_PitLimiterOn")]
    pub physics_pit_limiter_on: i32,
    #[serde(rename = "Physics_Abs")]
    pub physics_abs: f32,
    #[serde(rename = "Physics_KersCharge")]
    pub physics_kers_charge: f32,
    #[serde(rename = "Physics_KersInput")]
    pub physics_kers_input: f32,
    #[serde(rename = "Physics_AutoShifterOn")]
    pub physics_auto_shifter_on: i32,
    #[serde(rename = "Physics_RideHeight0")]
    pub physics_ride_height_0: f32,
    #[serde(rename = "Physics_RideHeight1")]
    pub physics_ride_height_1: f32,
    #[serde(rename = "Physics_TurboBoost")]
    pub physics_turbo_boost: f32,
    #[serde(rename = "Physics_Ballast")]
    pub physics_ballast: f32,
    #[serde(rename = "Physics_AirDensity")]
    pub physics_air_density: f32,
    #[serde(rename = "Physics_AirTemp")]
    pub physics_air_temp: f32,
    #[serde(rename = "Physics_RoadTemp")]
    pub physics_road_temp: f32,
    #[serde(rename = "Physics_LocalAngularVelocity0")]
    pub physics_local_angular_velocity_0: f32,
    #[serde(rename = "Physics_LocalAngularVelocity1")]
    pub physics_local_angular_velocity_1: f32,
    #[serde(rename = "Physics_LocalAngularVelocity2")]
    pub physics_local_angular_velocity_2: f32,
    #[serde(rename = "Physics_FinalFF")]
    pub physics_final_ff: f32,
    #[serde(rename = "Physics_PerformanceMeter")]
    pub physics_performance_meter: f32,
    #[serde(rename = "Physics_EngineBrake")]
    pub physics_engine_brake: i32,
    #[serde(rename = "Physics_ErsRecoveryLevel")]
    pub physics_ers_recovery_level: i32,
    #[serde(rename = "Physics_ErsPowerLevel")]
    pub physics_ers_power_level: i32,
    #[serde(rename = "Physics_ErsHeatCharging")]
    pub physics_ers_heat_charging: i32,
    #[serde(rename = "Physics_ErsisCharging")]
    pub physics_ers_is_charging: i32,
    #[serde(rename = "Physics_KersCurrentKJ")]
    pub physics_kers_current_kj: f32,
    #[serde(rename = "Physics_DrsAvailable")]
    pub physics_drs_available: i32,
    #[serde(rename = "Physics_DrsEnabled")]
    pub physics_drs_enabled: i32,
    #[serde(rename = "Physics_Clutch")]
    pub physics_clutch: f32,
    #[serde(rename = "Physics_IsAIControlled")]
    pub physics_is_ai_controlled: i32,
    #[serde(rename = "Physics_BrakeBias")]
    pub physics_brake_bias: f32,
    #[serde(rename = "Physics_LocalVelocity0")]
    pub physics_local_velocity_0: f32,
    #[serde(rename = "Physics_LocalVelocity1")]
    pub physics_local_velocity_1: f32,
    #[serde(rename = "Physics_LocalVelocity2")]
    pub physics_local_velocity_2: f32,
    #[serde(rename = "Graphics_PacketId")]
    pub graphics_packet_id: i32,
    #[serde(rename = "Graphics_Status")]
    pub graphics_status: i32,
    #[serde(rename = "Graphics_Session")]
    pub graphics_session: i32,
    #[serde(rename = "Graphics_CurrentTime")]
    pub graphics_current_time: String,
    #[serde(rename = "Graphics_LastTime")]
    pub graphics_last_time: String,
    #[serde(rename = "Graphics_BestTime")]
    pub graphics_best_time: String,
    #[serde(rename = "Graphics_Split")]
    pub graphics_split: String,
    #[serde(rename = "Graphics_CompletedLaps")]
    pub graphics_completed_laps: i32,
    #[serde(rename = "Graphics_Position")]
    pub graphics_position: i32,
    #[serde(rename = "Graphics_iCurrentTime")]
    pub graphics_i_current_time: i32,
    #[serde(rename = "Graphics_iLastTime")]
    pub graphics_i_last_time: i32,
    #[serde(rename = "Graphics_iBestTime")]
    pub graphics_i_best_time: i32,
    #[serde(rename = "Graphics_SessionTimeLeft")]
    pub graphics_session_time_left: f32,
    #[serde(rename = "Graphics_DistanceTraveled")]
    pub graphics_distance_travelled: f32,
    #[serde(rename = "Graphics_IsInPit")]
    pub graphics_is_in_pit: i32,
    #[serde(rename = "Graphics_CurrentSectorIndex")]
    pub graphics_current_sector_index: i32,
    #[serde(rename = "Graphics_LastSectorTime")]
    pub graphics_last_sector_time: i32,
    #[serde(rename = "Graphics_NumberOfLaps")]
    pub graphics_number_of_laps: i32,
    #[serde(rename = "Graphics_TyreCompound")]
    pub graphics_tyre_compound: String,
    #[serde(rename = "Graphics_ReplayTimeMultiplier")]
    pub graphics_replay_time_multiplier: f32,
    #[serde(rename = "Graphics_NormalizedCarPosition")]
    pub graphics_normalized_car_position: f32,
    #[serde(rename = "Graphics_PenaltyTime")]
    pub graphics_penalty_time: f32,
    #[serde(rename = "Graphics_Flag")]
    pub graphics_flag: i32,
    #[serde(rename = "Graphics_IdealLineOn")]
    pub graphics_ideal_line_on: i32,
    #[serde(rename = "Graphics_IsInPitLane")]
    pub graphics_is_in_pit_lane: i32,
    #[serde(rename = "Graphics_SurfaceGrip")]
    pub graphics_surface_grip: f32,
    #[serde(rename = "Graphics_MandatoryPitDone")]
    pub graphics_mandatory_pit_done: i32,
    #[serde(rename = "Graphics_WindSpeed")]
    pub graphics_wind_speed: f32,
    #[serde(rename = "Graphics_WindDirection")]
    pub graphics_wind_direction: f32,
    #[serde(rename = "StaticInfo_SMVersion")]
    pub static_sm_version: String,
    #[serde(rename = "StaticInfo_ACVersion")]
    pub static_ac_version: String,
    #[serde(rename = "StaticInfo_NumberOfSessions")]
    pub static_number_of_sessions: i32,
    #[serde(rename = "StaticInfo_NumCars")]
    pub static_num_cars: i32,
    #[serde(rename = "StaticInfo_CarModel")]
    pub static_car_model: String,
    #[serde(rename = "StaticInfo_Track")]
    pub static_track: String,
    #[serde(rename = "StaticInfo_PlayerName")]
    pub static_player_name: String,
    #[serde(rename = "StaticInfo_PlayerSurname")]
    pub static_player_surname: String,
    #[serde(rename = "StaticInfo_PlayerNick")]
    pub static_player_nick: String,
    #[serde(rename = "StaticInfo_SectorCount")]
    pub static_sector_count: i32,
    #[serde(rename = "StaticInfo_MaxTorque")]
    pub static_max_torque: f32,
    #[serde(rename = "StaticInfo_MaxPower")]
    pub static_max_power: f32,
    #[serde(rename = "StaticInfo_MaxRpm")]
    pub static_max_rpm: i32,
    #[serde(rename = "StaticInfo_MaxFuel")]
    pub static_max_fuel: f32,
    #[serde(rename = "StaticInfo_SuspensionMaxTravel0")]
    pub static_suspension_max_travel_0: f32,
    #[serde(rename = "StaticInfo_SuspensionMaxTravel1")]
    pub static_suspension_max_travel_1: f32,
    #[serde(rename = "StaticInfo_SuspensionMaxTravel2")]
    pub static_suspension_max_travel_2: f32,
    #[serde(rename = "StaticInfo_SuspensionMaxTravel3")]
    pub static_suspension_max_travel_3: f32,
    #[serde(rename = "StaticInfo_TyreRadius0")]
    pub static_tyre_radius_0: f32,
    #[serde(rename = "StaticInfo_TyreRadius1")]
    pub static_tyre_radius_1: f32,
    #[serde(rename = "StaticInfo_TyreRadius2")]
    pub static_tyre_radius_2: f32,
    #[serde(rename = "StaticInfo_TyreRadius3")]
    pub static_tyre_radius_3: f32,
    #[serde(rename = "StaticInfo_MaxTurboBoost")]
    pub static_max_turbo_boost: f32,
    #[serde(rename = "StaticInfo_Deprecated1")]
    pub static_deprecated_1: f32,
    #[serde(rename = "StaticInfo_Deprecated2")]
    pub static_deprecated_2: f32,
    #[serde(rename = "StaticInfo_PenaltiesEnabled")]
    pub static_penalties_enabled: i32,
    #[serde(rename = "StaticInfo_AidFuelRate")]
    pub static_aid_fuel_rate: f32,
    #[serde(rename = "StaticInfo_AidTireRate")]
    pub static_aid_tire_rate: f32,
    #[serde(rename = "StaticInfo_AidMechanicalDamage")]
    pub static_aid_mechanical_damage: f32,
    #[serde(rename = "StaticInfo_AidAllowTyreBlankets")]
    pub static_aid_allow_tyre_blankets: f32,
    #[serde(rename = "StaticInfo_AidStability")]
    pub static_aid_stability: f32,
    #[serde(rename = "StaticInfo_AidAutoClutch")]
    pub static_aid_auto_clutch: i32,
    #[serde(rename = "StaticInfo_AidAutoBlip")]
    pub static_aid_auto_blip: i32,
    #[serde(rename = "StaticInfo_HasDRS")]
    pub static_has_drs: i32,
    #[serde(rename = "StaticInfo_HasERS")]
    pub static_has_ers: i32,
    #[serde(rename = "StaticInfo_HasKERS")]
    pub static_has_kers: i32,
    #[serde(rename = "StaticInfo_KersMaxJoules")]
    pub static_kers_max_joules: f32,
    #[serde(rename = "StaticInfo_EngineBrakeSettingsCount")]
    pub static_engine_brake_settings_count: i32,
    #[serde(rename = "StaticInfo_ErsPowerControllerCount")]
    pub static_ers_power_controller_count: i32,
    #[serde(rename = "StaticInfo_TrackSPlineLength")]
    pub static_track_spline_length: f32,
    #[serde(rename = "StaticInfo_TrackConfiguration")]
    pub static_track_configuration: String,
    #[serde(rename = "StaticInfo_ErsMaxJ")]
    pub static_ers_max_j: f32,
    #[serde(rename = "StaticInfo_IsTimedRace")]
    pub static_is_timed_race: i32,
    #[serde(rename = "StaticInfo_HasExtraLap")]
    pub static_has_extra_lap: i32,
    #[serde(rename = "StaticInfo_CarSkin")]
    pub static_car_skin: String,
    #[serde(rename = "StaticInfo_ReversedGridPositions")]
    pub static_reversed_grid_positions: i32,
    #[serde(rename = "StaticInfo_PitWindowStart")]
    pub static_pit_window_start: i32,
    #[serde(rename = "StaticInfo_PitWindowEnd")]
    pub static_pit_window_end: i32,
    #[serde(rename = "StaticInfo_IsOnline")]
    pub static_is_online: i32,
}

impl RecordFrame {
    /// Assemble a line from one poll of the three pages. `position` is the
    /// held non-zero position — the graphics page's own field reads zero
    /// until the car is placed, and briefly at session load, which is why
    /// the logger (and this) write the last good one.
    pub fn from_parts(
        physics: &PhysicsPage,
        graphics: &GraphicsPage,
        static_page: &StaticPage,
        position: [f32; 3],
        timestamp: i64,
        sequence: i64,
    ) -> Self {
        Self {
            timestamp,
            sequence,
            position_x: sf(position[0]),
            position_y: sf(position[1]),
            position_z: sf(position[2]),
            physics_packet_id: physics.packet_id,
            physics_gas: sf(physics.gas),
            physics_brake: sf(physics.brake),
            physics_fuel: sf(physics.fuel),
            physics_gear: physics.gear,
            physics_rpms: physics.rpms,
            physics_steer_angle: sf(physics.steer_angle),
            physics_speed_kmh: sf(physics.speed_kmh),
            physics_velocity_0: sf(physics.velocity[0]),
            physics_velocity_1: sf(physics.velocity[1]),
            physics_velocity_2: sf(physics.velocity[2]),
            physics_acc_g_0: sf(physics.acc_g[0]),
            physics_acc_g_1: sf(physics.acc_g[1]),
            physics_acc_g_2: sf(physics.acc_g[2]),
            wheel_slip_0: sf(physics.wheel_slip[0]),
            wheel_slip_1: sf(physics.wheel_slip[1]),
            wheel_slip_2: sf(physics.wheel_slip[2]),
            wheel_slip_3: sf(physics.wheel_slip[3]),
            wheel_load_0: sf(physics.wheel_load[0]),
            wheel_load_1: sf(physics.wheel_load[1]),
            wheel_load_2: sf(physics.wheel_load[2]),
            wheel_load_3: sf(physics.wheel_load[3]),
            wheel_pressure_0: sf(physics.wheel_pressure[0]),
            wheel_pressure_1: sf(physics.wheel_pressure[1]),
            wheel_pressure_2: sf(physics.wheel_pressure[2]),
            wheel_pressure_3: sf(physics.wheel_pressure[3]),
            wheel_angular_speed_0: sf(physics.wheel_angular_speed[0]),
            wheel_angular_speed_1: sf(physics.wheel_angular_speed[1]),
            wheel_angular_speed_2: sf(physics.wheel_angular_speed[2]),
            wheel_angular_speed_3: sf(physics.wheel_angular_speed[3]),
            tyre_wear_0: sf(physics.tyre_wear[0]),
            tyre_wear_1: sf(physics.tyre_wear[1]),
            tyre_wear_2: sf(physics.tyre_wear[2]),
            tyre_wear_3: sf(physics.tyre_wear[3]),
            tyre_dirty_level_0: sf(physics.tyre_dirty_level[0]),
            tyre_dirty_level_1: sf(physics.tyre_dirty_level[1]),
            tyre_dirty_level_2: sf(physics.tyre_dirty_level[2]),
            tyre_dirty_level_3: sf(physics.tyre_dirty_level[3]),
            tyre_core_temp_0: sf(physics.tyre_core_temp[0]),
            tyre_core_temp_1: sf(physics.tyre_core_temp[1]),
            tyre_core_temp_2: sf(physics.tyre_core_temp[2]),
            tyre_core_temp_3: sf(physics.tyre_core_temp[3]),
            camber_rad_0: sf(physics.camber_rad[0]),
            camber_rad_1: sf(physics.camber_rad[1]),
            camber_rad_2: sf(physics.camber_rad[2]),
            camber_rad_3: sf(physics.camber_rad[3]),
            suspension_travel_0: sf(physics.suspension_travel[0]),
            suspension_travel_1: sf(physics.suspension_travel[1]),
            suspension_travel_2: sf(physics.suspension_travel[2]),
            suspension_travel_3: sf(physics.suspension_travel[3]),
            brake_temp_0: sf(physics.brake_temp[0]),
            brake_temp_1: sf(physics.brake_temp[1]),
            brake_temp_2: sf(physics.brake_temp[2]),
            brake_temp_3: sf(physics.brake_temp[3]),
            tyre_temp_i_0: sf(physics.tyre_temp_i[0]),
            tyre_temp_i_1: sf(physics.tyre_temp_i[1]),
            tyre_temp_i_2: sf(physics.tyre_temp_i[2]),
            tyre_temp_i_3: sf(physics.tyre_temp_i[3]),
            tyre_temp_m_0: sf(physics.tyre_temp_m[0]),
            tyre_temp_m_1: sf(physics.tyre_temp_m[1]),
            tyre_temp_m_2: sf(physics.tyre_temp_m[2]),
            tyre_temp_m_3: sf(physics.tyre_temp_m[3]),
            tyre_temp_o_0: sf(physics.tyre_temp_o[0]),
            tyre_temp_o_1: sf(physics.tyre_temp_o[1]),
            tyre_temp_o_2: sf(physics.tyre_temp_o[2]),
            tyre_temp_o_3: sf(physics.tyre_temp_o[3]),
            physics_drs: sf(physics.drs),
            physics_tc: sf(physics.tc),
            physics_heading: sf(physics.heading),
            physics_pitch: sf(physics.pitch),
            physics_roll: sf(physics.roll),
            physics_cg_height: sf(physics.cg_height),
            physics_car_damage_0: sf(physics.car_damage[0]),
            physics_car_damage_1: sf(physics.car_damage[1]),
            physics_car_damage_2: sf(physics.car_damage[2]),
            physics_car_damage_3: sf(physics.car_damage[3]),
            physics_car_damage_4: sf(physics.car_damage[4]),
            physics_number_of_tyres_out: physics.number_of_tyres_out,
            physics_pit_limiter_on: physics.pit_limiter_on,
            physics_abs: sf(physics.abs),
            physics_kers_charge: sf(physics.kers_charge),
            physics_kers_input: sf(physics.kers_input),
            physics_auto_shifter_on: physics.auto_shifter_on,
            physics_ride_height_0: sf(physics.ride_height[0]),
            physics_ride_height_1: sf(physics.ride_height[1]),
            physics_turbo_boost: sf(physics.turbo_boost),
            physics_ballast: sf(physics.ballast),
            physics_air_density: sf(physics.air_density),
            physics_air_temp: sf(physics.air_temp),
            physics_road_temp: sf(physics.road_temp),
            physics_local_angular_velocity_0: sf(physics.local_angular_velocity[0]),
            physics_local_angular_velocity_1: sf(physics.local_angular_velocity[1]),
            physics_local_angular_velocity_2: sf(physics.local_angular_velocity[2]),
            physics_final_ff: sf(physics.final_ff),
            physics_performance_meter: sf(physics.performance_meter),
            physics_engine_brake: physics.engine_brake,
            physics_ers_recovery_level: physics.ers_recovery_level,
            physics_ers_power_level: physics.ers_power_level,
            physics_ers_heat_charging: physics.ers_heat_charging,
            physics_ers_is_charging: physics.ers_is_charging,
            physics_kers_current_kj: sf(physics.kers_current_kj),
            physics_drs_available: physics.drs_available,
            physics_drs_enabled: physics.drs_enabled,
            physics_clutch: sf(physics.clutch),
            physics_is_ai_controlled: physics.is_ai_controlled,
            physics_brake_bias: sf(physics.brake_bias),
            physics_local_velocity_0: sf(physics.local_velocity[0]),
            physics_local_velocity_1: sf(physics.local_velocity[1]),
            physics_local_velocity_2: sf(physics.local_velocity[2]),
            graphics_packet_id: graphics.packet_id,
            graphics_status: graphics.status,
            graphics_session: graphics.session,
            graphics_current_time: wchar_ascii(&graphics.current_time),
            graphics_last_time: wchar_ascii(&graphics.last_time),
            graphics_best_time: wchar_ascii(&graphics.best_time),
            graphics_split: wchar_ascii(&graphics.split),
            graphics_completed_laps: graphics.completed_laps,
            graphics_position: graphics.position,
            graphics_i_current_time: graphics.i_current_time,
            graphics_i_last_time: graphics.i_last_time,
            graphics_i_best_time: graphics.i_best_time,
            graphics_session_time_left: sf(graphics.session_time_left),
            graphics_distance_travelled: sf(graphics.distance_travelled),
            graphics_is_in_pit: graphics.is_in_pit,
            graphics_current_sector_index: graphics.current_sector_index,
            graphics_last_sector_time: graphics.last_sector_time,
            graphics_number_of_laps: graphics.number_of_laps,
            graphics_tyre_compound: wchar_ascii(&graphics.tyre_compound),
            graphics_replay_time_multiplier: sf(graphics.replay_time_multiplier),
            graphics_normalized_car_position: sf(graphics.normalized_car_position),
            graphics_penalty_time: sf(graphics.penalty_time),
            graphics_flag: graphics.flag,
            graphics_ideal_line_on: graphics.ideal_line_on,
            graphics_is_in_pit_lane: graphics.is_in_pit_lane,
            graphics_surface_grip: sf(graphics.surface_grip),
            graphics_mandatory_pit_done: graphics.mandatory_pit_done,
            graphics_wind_speed: sf(graphics.wind_speed),
            graphics_wind_direction: sf(graphics.wind_direction),
            static_sm_version: wchar_ascii(&static_page.sm_version),
            static_ac_version: wchar_ascii(&static_page.ac_version),
            static_number_of_sessions: static_page.number_of_sessions,
            static_num_cars: static_page.num_cars,
            static_car_model: wchar_ascii(&static_page.car_model),
            static_track: wchar_ascii(&static_page.track),
            static_player_name: wchar_string(&static_page.player_name),
            static_player_surname: wchar_string(&static_page.player_surname),
            static_player_nick: wchar_string(&static_page.player_nick),
            static_sector_count: static_page.sector_count,
            static_max_torque: sf(static_page.max_torque),
            static_max_power: sf(static_page.max_power),
            static_max_rpm: static_page.max_rpm,
            static_max_fuel: sf(static_page.max_fuel),
            static_suspension_max_travel_0: sf(static_page.suspension_max_travel[0]),
            static_suspension_max_travel_1: sf(static_page.suspension_max_travel[1]),
            static_suspension_max_travel_2: sf(static_page.suspension_max_travel[2]),
            static_suspension_max_travel_3: sf(static_page.suspension_max_travel[3]),
            static_tyre_radius_0: sf(static_page.tyre_radius[0]),
            static_tyre_radius_1: sf(static_page.tyre_radius[1]),
            static_tyre_radius_2: sf(static_page.tyre_radius[2]),
            static_tyre_radius_3: sf(static_page.tyre_radius[3]),
            static_max_turbo_boost: sf(static_page.max_turbo_boost),
            static_deprecated_1: sf(static_page.deprecated1),
            static_deprecated_2: sf(static_page.deprecated2),
            static_penalties_enabled: static_page.penalties_enabled,
            static_aid_fuel_rate: sf(static_page.aid_fuel_rate),
            static_aid_tire_rate: sf(static_page.aid_tire_rate),
            static_aid_mechanical_damage: sf(static_page.aid_mechanical_damage),
            static_aid_allow_tyre_blankets: sf(static_page.aid_allow_tyre_blankets),
            static_aid_stability: sf(static_page.aid_stability),
            static_aid_auto_clutch: static_page.aid_auto_clutch,
            static_aid_auto_blip: static_page.aid_auto_blip,
            static_has_drs: static_page.has_drs,
            static_has_ers: static_page.has_ers,
            static_has_kers: static_page.has_kers,
            static_kers_max_joules: sf(static_page.kers_max_joules),
            static_engine_brake_settings_count: static_page.engine_brake_settings_count,
            static_ers_power_controller_count: static_page.ers_power_controller_count,
            static_track_spline_length: sf(static_page.track_spline_length),
            static_track_configuration: wchar_ascii(&static_page.track_configuration),
            static_ers_max_j: sf(static_page.ers_max_j),
            static_is_timed_race: static_page.is_timed_race,
            static_has_extra_lap: static_page.has_extra_lap,
            static_car_skin: wchar_ascii(&static_page.car_skin),
            static_reversed_grid_positions: static_page.reversed_grid_positions,
            static_pit_window_start: static_page.pit_window_start,
            static_pit_window_end: static_page.pit_window_end,
            static_is_online: static_page.is_online,
        }
    }
}

// ======================================================================
// The output file
// ======================================================================

/// Lines between flushes — the logger's own cadence. Batching keeps the
/// write syscalls down; 200 lines (~2 s of driving) is the most a power cut
/// can cost.
const FLUSH_EVERY: usize = 200;

/// A capture being written: plain or gzipped NDJSON, created new.
struct LineWriter {
    path: PathBuf,
    inner: Inner,
}

enum Inner {
    Plain(io::BufWriter<fs::File>),
    Gzip(GzEncoder<io::BufWriter<fs::File>>),
}

impl LineWriter {
    /// Create the file, refusing to touch one that already exists — the
    /// logger's `FileMode.CreateNew`. A capture is a session record: silently
    /// appending to a previous one would lie about when it started, and
    /// silently replacing it would destroy data.
    fn create(path: &Path, plain: bool) -> Result<Self> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| CoachError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
        let buf = io::BufWriter::new(file);
        Ok(Self {
            path: path.to_path_buf(),
            inner: if plain {
                Inner::Plain(buf)
            } else {
                Inner::Gzip(GzEncoder::new(buf, Compression::default()))
            },
        })
    }

    fn write_line(&mut self, frame: &RecordFrame) -> Result<()> {
        let path = self.path.display().to_string();
        let r = (|| -> io::Result<()> {
            match &mut self.inner {
                Inner::Plain(w) => {
                    serde_json::to_writer(&mut *w, frame)?;
                    w.write_all(b"\n")?;
                }
                Inner::Gzip(w) => {
                    serde_json::to_writer(&mut *w, frame)?;
                    w.write_all(b"\n")?;
                }
            }
            Ok(())
        })();
        r.map_err(|e| CoachError::Io { path, source: e })
    }

    fn flush(&mut self) -> Result<()> {
        let path = self.path.display().to_string();
        let r = match &mut self.inner {
            Inner::Plain(w) => w.flush(),
            Inner::Gzip(w) => w.flush(),
        };
        r.map_err(|e| CoachError::Io { path, source: e })
    }

    /// Finish the stream: for gzip this writes the trailer, without which
    /// many readers refuse the file outright.
    fn finish(self) -> Result<()> {
        let path = self.path.display().to_string();
        let r = match self.inner {
            Inner::Plain(mut w) => w.flush(),
            Inner::Gzip(w) => w
                .finish()
                .and_then(|mut buf| buf.flush()),
        };
        r.map_err(|e| CoachError::Io { path, source: e })
    }
}

/// The logger's default capture name:
/// `telemetry_ac_<track>_<car>_<stamp>.ndjson(.gz)`.
///
/// Resolved only when the first frame is about to be written, because the
/// track and car are session facts — the static page has to have published
/// before the name exists. The stamp is UTC `yyyyMMdd_HHmmss` (the logger
/// uses local time; both are only there to make names unique and roughly
/// sortable, and UTC needs no timezone tables).
fn default_path(static_page: &StaticPage, plain: bool) -> PathBuf {
    let track = wchar_ascii(&static_page.track);
    let car = wchar_ascii(&static_page.car_model);
    let extension = if plain { ".ndjson" } else { ".ndjson.gz" };
    PathBuf::from(format!(
        "telemetry_ac_{track}_{car}_{}{extension}",
        stamp_utc(now_unix_ms())
    ))
}

/// `yyyyMMdd_HHmmss` from Unix milliseconds.
fn stamp_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

// ======================================================================
// Recording while coaching
// ======================================================================

/// A capture written by a *coaching* session — the recorder behind
/// `SimProvider::live_with_recording`, so a live session leaves the same
/// file `coach record` would have.
///
/// Same writer, same frame, same flush cadence as [`record`], so the
/// interchange contract holds for coaching captures too: `coach learn-track`
/// cannot tell them from the C# logger's. The differences are all
/// lifecycle, and they come from the coaching session owning the loop:
///
/// * the file name resolves when the first frame with a published static
///   page arrives (the track and car that name the file are session facts),
///   in the caller's directory rather than the working directory —
/// * the recording ends when coaching ends, wherever [`Drop`] finds it, so
///   `Drop` finishes the stream: a session capture without its gzip trailer
///   is a file many readers refuse outright.
///
/// A failed write is the *caller's* verdict, never this type's: coaching is
/// the product and the capture is the byproduct, so the live source decides
/// whether a failed capture ends coaching (it does not — it says so and
/// carries on).
pub(crate) struct LiveRecorder {
    /// Where the capture lands; the file name inside it is the logger's own.
    out_dir: PathBuf,
    /// The capture, from the first frame that named it. `None` until the
    /// session resolved.
    writer: Option<LineWriter>,
    lines_since_flush: usize,
}

impl LiveRecorder {
    pub(crate) fn new(out_dir: PathBuf) -> Self {
        Self {
            out_dir,
            writer: None,
            lines_since_flush: 0,
        }
    }

    /// The capture's path, once the session named it — for the connection
    /// line that tells the driver where their laps are going.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.writer.as_ref().map(|w| w.path.as_path())
    }

    /// Write one poll of the pages, in the logger's format.
    pub(crate) fn on_frame(
        &mut self,
        physics: &PhysicsPage,
        graphics: &GraphicsPage,
        static_page: &StaticPage,
        position: [f32; 3],
        timestamp: i64,
        sequence: i64,
    ) -> Result<()> {
        if self.writer.is_none() {
            let path = self.out_dir.join(default_path(static_page, false));
            fs::create_dir_all(&self.out_dir).map_err(|e| CoachError::Io {
                path: self.out_dir.display().to_string(),
                source: e,
            })?;
            self.writer = Some(LineWriter::create(&path, false)?);
        }
        let writer = self.writer.as_mut().expect("created above");
        writer.write_line(&RecordFrame::from_parts(
            physics,
            graphics,
            static_page,
            position,
            timestamp,
            sequence,
        ))?;
        self.lines_since_flush += 1;
        if self.lines_since_flush >= FLUSH_EVERY {
            writer.flush()?;
            self.lines_since_flush = 0;
        }
        Ok(())
    }
}

impl Drop for LiveRecorder {
    fn drop(&mut self) {
        // Finish the stream — for gzip this writes the trailer. A coaching
        // session ends by its own rules (the window closed, the sim quit),
        // and every one of them must still leave a readable capture.
        if let Some(writer) = self.writer.take()
            && let Err(e) = writer.finish()
        {
            eprintln!("warning: could not finish the session capture: {e}");
        }
    }
}

/// Days since 1970-01-01 to (year, month, day) — Howard Hinnant's
/// `civil_from_days`, the standard era-based calendar arithmetic.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

// ======================================================================
// The recording loop
// ======================================================================

/// Record a capture from the running sim.
///
/// Waits for the sim to publish its pages (retrying every 2 s, saying why
/// once per distinct reason), then writes frames until `--laps` laps have
/// completed. Without `--laps` it runs until the process is stopped — a
/// killed recording costs the gzip trailer and any unflushed line, and the
/// frames before it are still readable; use `--laps` for a clean file.
pub fn record<R: PageStore>(opts: &RecordOptions) -> Result<RecordSummary> {
    let mut assembler = FrameAssembler::default();
    let mut store: Option<R> = None;
    let mut last_wait_message: Option<String> = None;
    let mut last_static_poll: Option<Instant> = None;
    let mut next_poll = Instant::now();
    let mut writer: Option<LineWriter> = None;
    let mut summary = RecordSummary::default();
    // `--laps` counts from the lap the recording started on, not from zero:
    // a session joined mid-stint should record three laps *from now*.
    let mut baseline_lap: Option<i32> = None;
    let mut lines_since_flush: usize = 0;

    loop {
        // `--laps` also bounds the whole loop: the target met means done.
        if opts
            .laps
            .is_some_and(|target| summary.laps_completed >= target as i32)
        {
            break;
        }

        // The caller's stop flag, same role as `--laps` from the other end:
        // the GUI's record screen has a Stop button rather than a kill signal,
        // and a stopped recording must still flush and finish its file. Like
        // the laps check, this sits at the top of the loop so both the waiting
        // and the polling phases honour it — worst case one attach-retry
        // (2 s) after the button is pressed.
        if opts
            .stop
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
        {
            break;
        }

        // ---- Waiting for the sim ----------------------------------------
        if store.is_none() {
            match R::attach() {
                Ok(s) => {
                    store = Some(s);
                    last_static_poll = None;
                }
                Err(reason) => {
                    if last_wait_message.as_deref() != Some(reason.as_str()) {
                        eprintln!("waiting for Assetto Corsa: {reason}");
                        last_wait_message = Some(reason);
                    }
                    std::thread::sleep(ATTACH_RETRY);
                    continue;
                }
            }
        }

        // ---- One poll ------------------------------------------------------
        let now = Instant::now();
        if now < next_poll {
            std::thread::sleep(next_poll - now);
        }
        next_poll = Instant::now() + PHYSICS_POLL;

        let store = store.as_mut().expect("attached above");
        if last_static_poll
            .is_none_or(|t| t.elapsed() >= STATIC_POLL)
        {
            assembler.update_static(&store.read_static());
            last_static_poll = Some(Instant::now());
        }
        let physics = store.read_physics();
        let graphics = store.read_graphics();
        let frame = match assembler.on_poll(&physics, &graphics, now_unix_ms()) {
            Ok(f) => f,
            Err(SkipReason::NoPosition) => {
                summary.skipped_no_position += 1;
                continue;
            }
            Err(SkipReason::DuplicatePacket) => {
                summary.skipped_duplicate += 1;
                continue;
            }
        };

        // A session means track and car are populated; without one there is
        // nothing to attribute the capture to, so the frame is held back
        // rather than written into a file that cannot be named.
        let static_page = match assembler.static_page() {
            Some(s)
                if !wchar_ascii(&s.track).is_empty()
                    && !wchar_ascii(&s.car_model).is_empty() =>
            {
                s
            }
            _ => {
                summary.skipped_no_session += 1;
                continue;
            }
        };

        // ---- Lap counting (the graphics page's own counter, which is
        // exactly what it is for — display counters, not lap delimiting)
        if let Some(target) = opts.laps {
            let baseline = *baseline_lap.get_or_insert(graphics.completed_laps);
            let done = (graphics.completed_laps - baseline).max(0);
            if done > summary.laps_completed {
                summary.laps_completed = done;
                println!("lap {done} of {target} complete");
            }
        }

        // ---- The output file, resolved once the session is known ----------
        if writer.is_none() {
            let path = match &opts.out {
                Some(p) => p.clone(),
                None => default_path(static_page, opts.plain),
            };
            if let Some(parent) = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                fs::create_dir_all(parent).map_err(|e| CoachError::Io {
                    path: path.display().to_string(),
                    source: e,
                })?;
            }
            println!("recording to {}", path.display());
            writer = Some(LineWriter::create(&path, opts.plain)?);
            summary.path = Some(path);
        }

        let position = assembler.last_position().unwrap_or([0.0, 0.0, 0.0]);
        let line = RecordFrame::from_parts(
            &physics,
            &graphics,
            static_page,
            position,
            frame.timestamp,
            frame.sequence,
        );
        if let Some(w) = writer.as_mut() {
            w.write_line(&line)?;
        }
        summary.frames += 1;
        lines_since_flush += 1;
        if lines_since_flush >= FLUSH_EVERY {
            if let Some(w) = writer.as_mut() {
                w.flush()?;
            }
            lines_since_flush = 0;
        }
    }

    if let Some(w) = writer.take() {
        w.finish()?;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sims::assetto_corsa::frame::AcFrame;
    use crate::sims::assetto_corsa::shared_memory::fake::{FakeStore, SCRIPT};
    use crate::sims::assetto_corsa::shared_memory::pages;
    use crate::telemetry::TelemetrySource;
    use std::io::Read;

    const MONZA_CAPTURE: &str =
        "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";

    /// The first line of a real C#-logger capture, decompressed.
    fn logger_first_line() -> Option<String> {
        if !Path::new(MONZA_CAPTURE).exists() {
            return None;
        }
        let raw = fs::read(MONZA_CAPTURE).ok()?;
        let mut text = String::new();
        flate2::read::GzDecoder::new(&raw[..])
            .take(1 << 20)
            .read_to_string(&mut text)
            .ok()?;
        text.lines().next().map(|l| l.to_string())
    }

    fn sample_line() -> (String, AcFrame) {
        let (physics, graphics, static_page) = pages();
        let mut assembler = FrameAssembler::default();
        assembler.update_static(&static_page);
        let frame = assembler
            .on_poll(&physics, &graphics, 1_788_365_559_824)
            .expect("emit");
        let line = RecordFrame::from_parts(
            &physics,
            &graphics,
            &static_page,
            assembler.last_position().expect("held position"),
            frame.timestamp,
            frame.sequence,
        );
        (serde_json::to_string(&line).unwrap(), frame)
    }

    /// The contract of this whole module: our line carries exactly the key
    /// set the C# logger writes — same keys, same order — checked against a
    /// real capture rather than against a hand-copied list.
    #[test]
    fn the_key_set_is_the_loggers() {
        let Some(logger_line) = logger_first_line() else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };
        let logger: serde_json::Value = serde_json::from_str(&logger_line).unwrap();
        let ours: serde_json::Value = serde_json::from_str(&sample_line().0).unwrap();
        let logger_keys: Vec<&str> = logger.as_object().unwrap().keys().map(String::as_str).collect();
        let our_keys: Vec<&str> = ours.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(our_keys, logger_keys);
        assert_eq!(our_keys.len(), 192);
    }

    /// And the values parse back through the reader's schema into the frame
    /// the assembler emitted — recorder and reader agree on both ends.
    #[test]
    fn a_written_line_round_trips_into_the_frame_schema() {
        let (line, frame) = sample_line();
        let parsed: AcFrame = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.timestamp, frame.timestamp);
        assert_eq!(parsed.sequence, frame.sequence);
        assert_eq!(parsed.pos_x, frame.pos_x);
        assert_eq!(parsed.pos_y, frame.pos_y);
        assert_eq!(parsed.pos_z, frame.pos_z);
        assert_eq!(parsed.physics_packet_id, frame.physics_packet_id);
        assert_eq!(parsed.gas, frame.gas);
        assert_eq!(parsed.rpms, frame.rpms);
        assert_eq!(parsed.speed_kmh, frame.speed_kmh);
        assert_eq!(parsed.heading, frame.heading);
        assert_eq!(parsed.normalized_car_position, frame.normalized_car_position);
        assert_eq!(parsed.track.as_deref(), Some("monza"));
        assert_eq!(parsed.car_model.as_deref(), Some("ks_ferrari_sf70h"));
        assert_eq!(parsed.track_spline_length, frame.track_spline_length);
        assert_eq!(parsed, frame, "the whole frame must survive the round trip");
    }

    #[test]
    fn non_finite_floats_are_written_as_zero_not_null() {
        let (mut physics, graphics, static_page) = pages();
        physics.gas = f32::NAN;
        physics.heading = f32::INFINITY;
        let line = RecordFrame::from_parts(
            &physics,
            &graphics,
            &static_page,
            [1.0, 2.0, 3.0],
            0,
            1,
        );
        let v = serde_json::to_value(&line).unwrap();
        assert_eq!(v["Physics_Gas"], 0.0);
        assert_eq!(v["Physics_Heading"], 0.0);
    }

    #[test]
    fn the_default_name_is_the_loggers() {
        let (_, _, static_page) = pages();
        let gz = default_path(&static_page, false);
        let name = gz.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("telemetry_ac_monza_ks_ferrari_sf70h_"),
            "{name}"
        );
        assert!(name.ends_with(".ndjson.gz"), "{name}");
        assert!(default_path(&static_page, true)
            .to_string_lossy()
            .ends_with(".ndjson"));
    }

    #[test]
    fn stamps_are_calendar_correct() {
        assert_eq!(stamp_utc(0), "19700101_000000");
        assert_eq!(stamp_utc(1_788_365_559_824), "20260902_161239");
        assert_eq!(stamp_utc(4_102_444_800_000), "21000101_000000");
    }

    #[test]
    fn recording_writes_logger_lines_until_the_lap_count() {
        // Two polls of scripted pages: the second reports one completed lap,
        // so `--laps 1` finishes the recording after it.
        let (physics, graphics, static_page) = pages();
        let mut next_graphics = graphics;
        next_graphics.completed_laps = 1;
        SCRIPT.with(|s| {
            *s.borrow_mut() = crate::sims::assetto_corsa::shared_memory::fake::Script {
                attach_errors: Vec::new(),
                pages: vec![(physics, graphics), (physics, next_graphics)],
                static_page,
                polls: 0,
            }
        });

        let dir = std::env::temp_dir().join("coach_record_tests");
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("laps.ndjson");
        let _ = fs::remove_file(&out);

        let summary = record::<FakeStore>(&RecordOptions {
            out: Some(out.clone()),
            laps: Some(1),
            plain: true,
            stop: None,
        })
        .unwrap();

        assert_eq!(summary.laps_completed, 1);
        assert_eq!(summary.path.as_deref(), Some(out.as_path()));
        assert!(summary.frames >= 1, "at least one frame was written");

        let content = fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), summary.frames, "one JSON object per line");
        for (n, line) in lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {n} does not parse: {e}"));
            assert_eq!(v.as_object().unwrap().len(), 192);
            assert_eq!(v["SequenceNumber"], (n + 1) as u64);
        }
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let dir = std::env::temp_dir().join("coach_record_tests");
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("exists.ndjson");
        fs::write(&out, "precious").unwrap();

        let (physics, graphics, static_page) = pages();
        SCRIPT.with(|s| {
            *s.borrow_mut() = crate::sims::assetto_corsa::shared_memory::fake::Script {
                attach_errors: vec![],
                pages: vec![(physics, graphics)],
                static_page,
                polls: 0,
            }
        });

        let err = record::<FakeStore>(&RecordOptions {
            out: Some(out.clone()),
            laps: Some(1),
            plain: true,
            stop: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("i/o error"), "{err}");
        assert_eq!(fs::read_to_string(&out).unwrap(), "precious");
    }

    /// The GUI's Stop button: a pre-set stop flag ends an open-ended
    /// recording before any frame is written, cleanly — no file left behind,
    /// no partial capture pretending to be a session.
    #[test]
    fn a_set_stop_flag_ends_the_recording_before_any_frame() {
        let (physics, graphics, static_page) = pages();
        SCRIPT.with(|s| {
            *s.borrow_mut() = crate::sims::assetto_corsa::shared_memory::fake::Script {
                attach_errors: vec![],
                pages: vec![(physics, graphics)],
                static_page,
                polls: 0,
            }
        });

        let dir = std::env::temp_dir().join("coach_record_tests");
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("stopped.ndjson");
        let _ = fs::remove_file(&out);

        let summary = record::<FakeStore>(&RecordOptions {
            out: Some(out.clone()),
            laps: None,
            plain: true,
            stop: Some(std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(true),
            )),
        })
        .unwrap();

        assert_eq!(summary.frames, 0, "nothing was written");
        assert!(summary.path.is_none(), "the file was never named");
        assert!(!out.exists(), "no partial capture was left behind");
    }

    /// The recorder behind record-while-coaching: same writer, same frame,
    /// same name as `record` — proven by reopening the file it left through
    /// the replay source, the pipeline's own reader. This is the unit half
    /// of the shared-memory round-trip test (that one proves the wiring;
    /// this one proves the recorder itself).
    #[test]
    fn a_live_recorder_writes_a_replayable_logger_capture() {
        let dir = std::env::temp_dir().join("coach_record_tests/live_recorder");
        let _ = fs::remove_dir_all(&dir);

        let (physics, graphics, static_page) = pages();
        let mut recorder = LiveRecorder::new(dir.clone());
        assert!(
            recorder.path().is_none(),
            "no file until the session names it"
        );
        let position = [10.0, 1.0, 20.0];
        for sequence in 1..=3 {
            recorder
                .on_frame(
                    &physics,
                    &graphics,
                    &static_page,
                    position,
                    1_788_365_559_824 + sequence,
                    sequence,
                )
                .expect("frames write");
        }
        assert!(recorder.path().is_some(), "the first frame named it");
        drop(recorder); // the session ends: the trailer is written

        let path = fs::read_dir(&dir)
            .unwrap()
            .next()
            .expect("one capture")
            .unwrap()
            .path();
        assert!(
            path.extension().is_some_and(|e| e == "gz"),
            "gzip by default, like the logger: {}",
            path.display()
        );

        let mut replay =
            crate::sims::assetto_corsa::NdjsonReplaySource::open(&path)
                .expect("the coaching capture reopens");
        let mut frames = 0;
        while replay.next_sample().unwrap().is_some() {
            frames += 1;
        }
        assert_eq!(frames, 3, "every handed frame is in the capture");
        assert_eq!(
            replay.session().expect("session").car,
            "ks_ferrari_sf70h"
        );
    }
}
