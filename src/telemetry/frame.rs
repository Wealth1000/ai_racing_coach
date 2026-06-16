use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct TelemetryFrame {
    pub Timestamp: i64,
    #[serde(rename = "SequenceNumber")]
    pub SequenceNumber: u32,

    pub Version: u32,
    #[serde(rename = "BuildVersion")]
    pub BuildVersion: u32,

    #[serde(rename = "GameState")]
    pub GameState: u32,
    #[serde(rename = "SessionState")]
    pub SessionState: u32,
    #[serde(rename = "RaceState")]
    pub RaceState: u32,
    #[serde(rename = "PitMode")]
    pub PitMode: u32,
    #[serde(rename = "PitSchedule")]
    pub PitSchedule: u32,
    #[serde(rename = "LapInvalidated")]
    pub LapInvalidated: bool,
    #[serde(rename = "YellowFlagState")]
    pub YellowFlagState: i32,

    #[serde(rename = "TrackLocation")]
    pub TrackLocation: String,
    #[serde(rename = "TrackVariation")]
    pub TrackVariation: String,
    #[serde(rename = "TrackLength")]
    pub TrackLength: f32,
    #[serde(rename = "NumSectors")]
    pub NumSectors: i32,
    #[serde(rename = "CarName")]
    pub CarName: String,
    #[serde(rename = "CarClassName")]
    pub CarClassName: String,

    pub Viewed: ViewedParticipant,

    #[serde(rename = "BestLapTime")]
    pub BestLapTime: f32,
    #[serde(rename = "LastLapTime")]
    pub LastLapTime: f32,
    #[serde(rename = "CurrentTime")]
    pub CurrentTime: f32,
    #[serde(rename = "SplitTime")]
    pub SplitTime: f32,
    #[serde(rename = "CurrentSector1Time")]
    pub CurrentSector1Time: f32,
    #[serde(rename = "CurrentSector2Time")]
    pub CurrentSector2Time: f32,
    #[serde(rename = "CurrentSector3Time")]
    pub CurrentSector3Time: f32,

    pub Throttle: f32,
    pub Brake: f32,
    pub Steering: f32,
    #[serde(rename = "UnfilteredThrottle")]
    pub UnfilteredThrottle: f32,
    #[serde(rename = "UnfilteredBrake")]
    pub UnfilteredBrake: f32,
    #[serde(rename = "UnfilteredSteering")]
    pub UnfilteredSteering: f32,

    pub Speed: f32,
    pub Rpm: f32,
    #[serde(rename = "MaxRPM")]
    pub MaxRPM: f32,
    pub Gear: i32,
    #[serde(rename = "NumGears")]
    pub NumGears: i32,

    #[serde(rename = "AntiLockActive")]
    pub AntiLockActive: bool,
    #[serde(rename = "AntiLockSetting")]
    pub AntiLockSetting: i32,
    #[serde(rename = "TractionControlSetting")]
    pub TractionControlSetting: i32,
    #[serde(rename = "BrakeBias")]
    pub BrakeBias: f32,

    pub Orientation: [f32; 3],
    #[serde(rename = "AngularVelocity")]
    pub AngularVelocity: [f32; 3],
    #[serde(rename = "LocalVelocity")]
    pub LocalVelocity: [f32; 3],
    #[serde(rename = "WorldVelocity")]
    pub WorldVelocity: [f32; 3],
    #[serde(rename = "LocalAcceleration")]
    pub LocalAcceleration: [f32; 3],
    #[serde(rename = "WorldAcceleration")]
    pub WorldAcceleration: [f32; 3],

    #[serde(rename = "TyreTempLeft")]
    pub TyreTempLeft: [f32; 4],
    #[serde(rename = "TyreTempCenter")]
    pub TyreTempCenter: [f32; 4],
    #[serde(rename = "TyreTempRight")]
    pub TyreTempRight: [f32; 4],
    #[serde(rename = "AirPressure")]
    pub AirPressure: [f32; 4],
    #[serde(rename = "TyreWear")]
    pub TyreWear: [f32; 4],
    #[serde(rename = "TyreRPS")]
    pub TyreRPS: [f32; 4],
    #[serde(rename = "BrakeTempCelsius")]
    pub BrakeTempCelsius: [f32; 4],

    #[serde(rename = "SuspensionTravel")]
    pub SuspensionTravel: [f32; 4],
    #[serde(rename = "SuspensionVelocity")]
    pub SuspensionVelocity: [f32; 4],
    #[serde(rename = "RideHeight")]
    pub RideHeight: [f32; 4],

    #[serde(rename = "CrashState")]
    pub CrashState: u32,
    #[serde(rename = "AeroDamage")]
    pub AeroDamage: f32,
    #[serde(rename = "EngineDamage")]
    pub EngineDamage: f32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ViewedParticipant {
    pub Index: i32,
    #[serde(rename = "CurrentLap")]
    pub CurrentLap: u32,
    #[serde(rename = "LapsCompleted")]
    pub LapsCompleted: u32,
    #[serde(rename = "CurrentSector")]
    pub CurrentSector: i32,
    #[serde(rename = "CurrentLapDistance")]
    pub CurrentLapDistance: f32,
    #[serde(rename = "WorldPosition")]
    pub WorldPosition: [f32; 3],
}