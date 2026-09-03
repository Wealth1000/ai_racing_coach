//! Assetto Corsa's shared-memory pages, read live.
//!
//! AC publishes three Windows memory-mapped sections in the session
//! namespace — `Local\acpmf_physics`, `Local\acpmf_graphics`,
//! `Local\acpmf_static` — and refreshes them from its own simulation loop.
//! Reading them is the live counterpart of replaying the C# logger's
//! captures, and this module is deliberately structured so that everything
//! downstream of the pages is identical between the two:
//!
//! * the raw page layouts below are transcribed field-for-field from the
//!   logger (`Logger Programs/AcTelemetryLogger/{Physics,Graphics,StaticInfo}.cs`);
//! * [`FrameAssembler`] turns a poll of the three pages into an
//!   [`AcFrame`] — the same struct the NDJSON reader deserialises into —
//!   on the same skip rules the logger applies (`Program.cs`:
//!   no frame before a valid position, dedupe by packet id only once the
//!   id has been seen to advance);
//! * the conversion to [`Sample`] is the one shared with replay
//!   ([`crate::sims::assetto_corsa::convert`]).
//!
//! # Layout
//!
//! The C# structs are `LayoutKind.Sequential, Pack = 4`, which means each
//! field starts at a multiple of its own size (up to 4). `#[repr(C)]` gives
//! the same rule, including the two bytes of padding after every 66-byte
//! (`[u16; 33]`) string buffer that the next `i32`/`f32` needs — so the
//! Rust structs below list fields only, no explicit padding, and the size
//! and offset tests pin the result to the numbers the logger verified
//! against a live page probe:
//!
//! * physics ends at **580** bytes (`localVelocity` occupies 568..579);
//! * graphics places `normalizedCarPosition` at **248**, `carCoordinates`
//!   at **252** and `surfaceGrip` at **280**, ending at **296**;
//! * static places `pitWindowEnd` at **680** and `isOnline` at **684**,
//!   ending at **688**.
//!
//! AC's pages are fixed-layout per game version; if a future AC appends
//! fields, the page only grows at the tail and these offsets survive. A
//! *shifted* layout (a field inserted mid-page) cannot happen without a
//! new AC that also breaks every AC app ever written — and the
//! first-frame plausibility guard
//! ([`crate::sims::assetto_corsa::schema`]) is the backstop that catches it
//! anyway, exactly as it does for captures.
//!
//! [`Sample`]: crate::core::sample::Sample

use std::time::{SystemTime, UNIX_EPOCH};

use crate::sims::assetto_corsa::frame::AcFrame;

/// Wall-clock milliseconds since the Unix epoch — the logger's
/// `DateTimeOffset.UtcNow` timestamp, for frame ordering only (see
/// [`AcFrame::timestamp`]).
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One `Coordinates` triple in the physics page.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Coordinates {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// `SPageFilePhysics` — 580 bytes, no interior padding (every field is a
/// 4-byte scalar or array of them).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsPage {
    pub packet_id: i32,
    pub gas: f32,
    pub brake: f32,
    pub fuel: f32,
    pub gear: i32,
    /// Revolutions per minute. AC publishes an integer here; the frame
    /// schema carries it as a float, so the conversion happens in
    /// [`FrameAssembler`].
    pub rpms: i32,
    pub steer_angle: f32,
    pub speed_kmh: f32,
    pub velocity: [f32; 3],
    pub acc_g: [f32; 3],
    pub wheel_slip: [f32; 4],
    pub wheel_load: [f32; 4],
    pub wheel_pressure: [f32; 4],
    pub wheel_angular_speed: [f32; 4],
    pub tyre_wear: [f32; 4],
    pub tyre_dirty_level: [f32; 4],
    pub tyre_core_temp: [f32; 4],
    pub camber_rad: [f32; 4],
    pub suspension_travel: [f32; 4],
    pub drs: f32,
    pub tc: f32,
    pub heading: f32,
    pub pitch: f32,
    pub roll: f32,
    pub cg_height: f32,
    pub car_damage: [f32; 5],
    pub number_of_tyres_out: i32,
    pub pit_limiter_on: i32,
    pub abs: f32,
    pub kers_charge: f32,
    pub kers_input: f32,
    pub auto_shifter_on: i32,
    pub ride_height: [f32; 2],
    pub turbo_boost: f32,
    pub ballast: f32,
    pub air_density: f32,
    pub air_temp: f32,
    pub road_temp: f32,
    pub local_angular_velocity: [f32; 3],
    pub final_ff: f32,
    pub performance_meter: f32,
    pub engine_brake: i32,
    pub ers_recovery_level: i32,
    pub ers_power_level: i32,
    pub ers_heat_charging: i32,
    pub ers_is_charging: i32,
    pub kers_current_kj: f32,
    pub drs_available: i32,
    pub drs_enabled: i32,
    pub brake_temp: [f32; 4],
    pub clutch: f32,
    pub tyre_temp_i: [f32; 4],
    pub tyre_temp_m: [f32; 4],
    pub tyre_temp_o: [f32; 4],
    pub is_ai_controlled: i32,
    pub tyre_contact_point: [Coordinates; 4],
    pub tyre_contact_normal: [Coordinates; 4],
    pub tyre_contact_heading: [Coordinates; 4],
    pub brake_bias: f32,
    /// The last field AC publishes; the page is 580 bytes.
    pub local_velocity: [f32; 3],
}

/// `SPageFileGraphic` — 296 bytes. The time strings are fixed UTF-16
/// buffers; `tyre_compound` (66 bytes) is followed by two bytes of padding
/// before `replay_time_multiplier`, which `#[repr(C)]` inserts on its own.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphicsPage {
    pub packet_id: i32,
    /// `AC_OFF = 0`, `AC_REPLAY = 1`, `AC_LIVE = 2`, `AC_PAUSE = 3`.
    pub status: i32,
    /// `AC_SESSION_TYPE`: -1 unknown, 0 practice, 1 qualify, 2 race,
    /// 3 hotlap, 4 time attack, 5 drift, 6 drag.
    pub session: i32,
    pub current_time: [u16; 15],
    pub last_time: [u16; 15],
    pub best_time: [u16; 15],
    pub split: [u16; 15],
    pub completed_laps: i32,
    pub position: i32,
    pub i_current_time: i32,
    pub i_last_time: i32,
    pub i_best_time: i32,
    pub session_time_left: f32,
    pub distance_travelled: f32,
    pub is_in_pit: i32,
    pub current_sector_index: i32,
    pub last_sector_time: i32,
    pub number_of_laps: i32,
    pub tyre_compound: [u16; 33],
    pub replay_time_multiplier: f32,
    /// Spline position along the track, 0..1 — the canonical distance axis
    /// once multiplied by the static page's spline length.
    pub normalized_car_position: f32,
    /// World position; all zeros until the car is placed on track.
    pub car_coordinates: [f32; 3],
    pub penalty_time: f32,
    /// `AC_FLAG_TYPE`.
    pub flag: i32,
    pub ideal_line_on: i32,
    pub is_in_pit_lane: i32,
    pub surface_grip: f32,
    pub mandatory_pit_done: i32,
    pub wind_speed: f32,
    /// The last field AC publishes; the page is 296 bytes.
    pub wind_direction: f32,
}

/// `SPageFileStatic` — 688 bytes. Written once per session (track, car,
/// versions); the reader holds the latest copy and re-reads it slowly.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaticPage {
    pub sm_version: [u16; 15],
    pub ac_version: [u16; 15],
    pub number_of_sessions: i32,
    pub num_cars: i32,
    pub car_model: [u16; 33],
    pub track: [u16; 33],
    pub player_name: [u16; 33],
    pub player_surname: [u16; 33],
    pub player_nick: [u16; 33],
    pub sector_count: i32,
    pub max_torque: f32,
    pub max_power: f32,
    pub max_rpm: i32,
    pub max_fuel: f32,
    pub suspension_max_travel: [f32; 4],
    pub tyre_radius: [f32; 4],
    pub max_turbo_boost: f32,
    pub deprecated1: f32,
    pub deprecated2: f32,
    pub penalties_enabled: i32,
    pub aid_fuel_rate: f32,
    pub aid_tire_rate: f32,
    pub aid_mechanical_damage: f32,
    pub aid_allow_tyre_blankets: f32,
    pub aid_stability: f32,
    pub aid_auto_clutch: i32,
    pub aid_auto_blip: i32,
    pub has_drs: i32,
    pub has_ers: i32,
    pub has_kers: i32,
    pub kers_max_joules: f32,
    pub engine_brake_settings_count: i32,
    pub ers_power_controller_count: i32,
    pub track_spline_length: f32,
    pub track_configuration: [u16; 33],
    pub ers_max_j: f32,
    pub is_timed_race: i32,
    pub has_extra_lap: i32,
    pub car_skin: [u16; 33],
    pub reversed_grid_positions: i32,
    pub pit_window_start: i32,
    /// At offset 680 — the logger verified this against the live page.
    pub pit_window_end: i32,
    /// At offset 684; the page is 688 bytes.
    pub is_online: i32,
}

// ======================================================================
// Fixed UTF-16 string buffers → Rust strings
// ======================================================================

/// The longest string the logger will ever write out (its `MaxStringLength`).
const MAX_STRING_CHARS: usize = 96;

/// Read a fixed UTF-16 buffer as a C string: cut at the first NUL, drop
/// unencodable and non-printable characters, trim.
///
/// This is the logger's `Sanitize`/`SanitizeCore` (`Program.cs`): a fixed
/// char buffer is a C string, so it ends at the first NUL — anything after
/// is adjacent page memory, not this field. Surrogates (the signature of
/// non-text bytes decoded as UTF-16) and the C0/C1 controls drop out rather
/// than corrupt the line.
pub fn wchar_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let mut out = String::new();
    for c in String::from_utf16_lossy(&buf[..end]).chars() {
        if out.chars().count() >= MAX_STRING_CHARS {
            break;
        }
        if c < '\u{20}' || c == '\u{7F}' {
            continue; // C0 controls + DEL
        }
        if ('\u{80}'..='\u{9F}').contains(&c) {
            continue; // C1 controls
        }
        if c == '\u{FFFD}' || c >= '\u{FFFE}' {
            continue; // the lossy-decode marker, and noncharacters
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// The ASCII variant for fields AC only ever fills with ASCII: lap times,
/// track and car identifiers, versions. A non-ASCII character proves the
/// bytes were not this field, so the result is empty — obviously missing
/// beats plausibly wrong (the logger's `SanitizeAscii`).
pub fn wchar_ascii(buf: &[u16]) -> String {
    let clean = wchar_string(buf);
    if clean.chars().any(|c| c > '\u{7E}') {
        String::new()
    } else {
        clean
    }
}

// ======================================================================
// The frame assembler
// ======================================================================

/// Why a poll produced no frame. The counters matter as much as the frames:
/// "attached but silent" must be diagnosable as *which* silence it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The graphics page has not reported a car position yet — the car is
    /// not on track, so there is nothing worth writing (the logger's
    /// `skipped_no_position`).
    NoPosition,
    /// The physics packet id has not advanced since the previous poll: AC
    /// republished the same frame because we polled faster than it updates.
    /// Only ever emitted once the id has been *seen* to advance — on builds
    /// where it stays 0 forever, this never fires (the logger's
    /// `skipped_duplicate` with the dedupe probe ON).
    DuplicatePacket,
}

/// The logger's frame-emission state machine, without any of the
/// Windows-specific plumbing: feed it polls of the three pages, it decides
/// which polls become frames, on exactly the rules `Program.cs` applies.
///
/// Pure data in and out, so the skip and dedupe rules are testable on any
/// platform — the mapping layer beneath is the only Windows-only part.
#[derive(Debug, Default)]
pub struct FrameAssembler {
    /// The number of frames emitted so far; each frame's `sequence` is
    /// this plus one, matching the logger's `SequenceNumber`.
    sequence: i64,
    /// The physics packet id of the previous poll.
    last_packet_id: Option<i32>,
    /// True once the packet id has been observed to change. Until then a
    /// repeat cannot be told apart from "this build never advances the id",
    /// and deduping on that would drop every frame (the exact bug the
    /// logger's probe exists to avoid).
    seen_packet_advance: bool,
    /// The last non-zero world position. The graphics page reports zeros
    /// until the car is placed, and briefly at session load; the logger
    /// keeps the last good one and writes frames with it.
    last_position: Option<[f32; 3]>,
    /// The most recent static page, held between its slow re-reads.
    static_page: Option<StaticPage>,
    /// The most recently emitted frame, for `session()` and `describe()`.
    last_frame: Option<AcFrame>,
}

impl FrameAssembler {
    /// Feed one poll of the physics and graphics pages; `now_ms` is the
    /// wall-clock timestamp stamped on any frame emitted.
    pub fn on_poll(
        &mut self,
        physics: &PhysicsPage,
        graphics: &GraphicsPage,
        now_ms: i64,
    ) -> Result<AcFrame, SkipReason> {
        // Position first: the world coordinate lives on the graphics page
        // and only updates when the car is out, so before it there is
        // nothing worth a frame.
        if graphics.car_coordinates != [0.0, 0.0, 0.0] {
            self.last_position = Some(graphics.car_coordinates);
        }
        let position = match self.last_position {
            Some(p) => p,
            None => return Err(SkipReason::NoPosition),
        };

        // Dedupe: a repeat is only a repeat if the id has ever moved.
        if let Some(prev) = self.last_packet_id {
            if physics.packet_id == prev {
                if self.seen_packet_advance {
                    return Err(SkipReason::DuplicatePacket);
                }
            } else {
                self.seen_packet_advance = true;
            }
        }
        self.last_packet_id = Some(physics.packet_id);

        self.sequence += 1;
        let frame = AcFrame {
            timestamp: now_ms,
            sequence: self.sequence,
            pos_x: position[0],
            pos_y: position[1],
            pos_z: position[2],
            physics_packet_id: physics.packet_id as i64,
            gas: physics.gas,
            brake: physics.brake,
            gear: physics.gear,
            rpms: physics.rpms as f32,
            steer_angle: physics.steer_angle,
            speed_kmh: physics.speed_kmh,
            heading: physics.heading,
            pitch: physics.pitch,
            roll: physics.roll,
            tyres_out: physics.number_of_tyres_out,
            pit_limiter_on: physics.pit_limiter_on,
            local_ang_vel_0: physics.local_angular_velocity[0],
            local_ang_vel_1: physics.local_angular_velocity[1],
            local_ang_vel_2: physics.local_angular_velocity[2],
            local_vel_0: physics.local_velocity[0],
            local_vel_1: physics.local_velocity[1],
            local_vel_2: physics.local_velocity[2],
            graphics_packet_id: graphics.packet_id as i64,
            status: graphics.status,
            completed_laps: graphics.completed_laps,
            i_current_time: graphics.i_current_time,
            i_last_time: graphics.i_last_time,
            distance_travelled: graphics.distance_travelled,
            is_in_pit: graphics.is_in_pit,
            is_in_pit_lane: graphics.is_in_pit_lane,
            sector_index: graphics.current_sector_index,
            normalized_car_position: graphics.normalized_car_position,
            surface_grip: graphics.surface_grip,
            sm_version: self
                .static_page
                .as_ref()
                .map(|s| wchar_ascii(&s.sm_version)),
            ac_version: self
                .static_page
                .as_ref()
                .map(|s| wchar_ascii(&s.ac_version)),
            car_model: self
                .static_page
                .as_ref()
                .map(|s| wchar_ascii(&s.car_model)),
            track: self.static_page.as_ref().map(|s| wchar_ascii(&s.track)),
            track_configuration: self
                .static_page
                .as_ref()
                .map(|s| wchar_ascii(&s.track_configuration)),
            track_spline_length: self
                .static_page
                .as_ref()
                .map(|s| s.track_spline_length)
                .unwrap_or(0.0),
            sector_count: self
                .static_page
                .as_ref()
                .map(|s| s.sector_count)
                .unwrap_or(0),
        };
        self.last_frame = Some(frame.clone());
        Ok(frame)
    }

    /// Take the latest static page (the caller re-reads it slowly, ~1 Hz,
    /// because it changes once per session).
    pub fn update_static(&mut self, page: &StaticPage) {
        self.static_page = Some(*page);
    }

    /// The most recently emitted frame — the pipeline's samples and the
    /// session facts both derive from it.
    pub fn last_frame(&self) -> Option<&AcFrame> {
        self.last_frame.as_ref()
    }

    /// The last non-zero world position, for the recorder's lines.
    pub fn last_position(&self) -> Option<[f32; 3]> {
        self.last_position
    }

    /// The held static page, for the recorder's lines.
    pub fn static_page(&self) -> Option<&StaticPage> {
        self.static_page.as_ref()
    }

    /// Frames emitted so far.
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
}

// ======================================================================
// The live source
// ======================================================================

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::core::sample::{Sample, SessionInfo};
use crate::telemetry::source::{SourceStats, TelemetrySource};

/// Where the three pages come from. A trait so that the source's behaviour —
/// the attach-retry wait, the poll loop, the shutdown path — is identical
/// code on Windows (the real mapping) and in tests (scripted pages): only
/// the ~80 lines of Win32 calls are Windows-only, and everything that can
/// be reasoned about is testable anywhere.
pub trait PageStore: Send {
    /// Attach to the sim's pages. An `Err` means "not available right now"
    /// (the sim is not running, or has not created the pages yet) and
    /// carries the reason for the waiting message; the source will retry.
    fn attach() -> Result<Self, String>
    where
        Self: Sized;

    /// A copy of the physics page as it stands right now.
    fn read_physics(&mut self) -> PhysicsPage;
    /// A copy of the graphics page as it stands right now.
    fn read_graphics(&mut self) -> GraphicsPage;
    /// A copy of the static page as it stands right now.
    fn read_static(&mut self) -> StaticPage;
}

/// How fast the source polls the physics page — the logger's own cadence
/// (`Program.cs`: 10 ms), which comfortably exceeds AC's update rate.
pub(crate) const PHYSICS_POLL: Duration = Duration::from_millis(10);
/// The static page changes once per session, so it is re-read slowly.
pub(crate) const STATIC_POLL: Duration = Duration::from_secs(1);
/// How often a failed attach is retried — the logger's 2 s.
pub(crate) const ATTACH_RETRY: Duration = Duration::from_secs(2);

/// `TelemetrySource` over AC's shared-memory pages.
///
/// Construction never fails and never touches the sim — "not running yet" is
/// a *state*, not an error, and it is the whole UX of the pick-a-sim flow:
/// the source is created the moment the driver picks Assetto Corsa, and
/// [`TelemetrySource::next_sample`] sits in an attach-retry loop, saying
/// once why it is waiting, until AC publishes its pages. The first frame
/// after that is validated by the same plausibility guard a capture's first
/// line passes ([`crate::sims::assetto_corsa::schema`]) — a shared-memory
/// layout drift is exactly the failure mode that guard exists for, and live
/// telemetry deserves the same refusal as a bad capture.
pub struct SharedMemorySource<R: PageStore> {
    /// The mapped pages, once attached. `None` is the waiting state.
    store: Option<R>,
    assembler: FrameAssembler,
    /// The live wiring's stop flag ([`TelemetrySource::set_stop_flag`]).
    /// Checked inside every wait, so shutdown does not depend on the sim
    /// publishing anything ever again.
    stop: Option<Arc<AtomicBool>>,
    /// The session capture, when coaching was asked to keep recording (the
    /// record-while-coaching setting). Writing it is best-effort: a failed
    /// capture must never end the coaching — see [`Self::with_recording`].
    recorder: Option<super::record::LiveRecorder>,
    /// The last attach failure, so the waiting message prints once per
    /// *distinct* reason rather than once per retry.
    last_wait_message: Option<String>,
    /// Session facts, derived from the first validated frame.
    session: Option<SessionInfo>,
    samples: usize,
    last_static_poll: Option<Instant>,
    poll_every: Duration,
    retry_every: Duration,
}

/// One trip around the main loop, with its outcomes told apart: "waiting"
/// (retry with a message) is a state, while the plausibility guard refusing
/// the first frame is a hard error — the source must not sit and retry a
/// layout it cannot read.
enum StepOutcome {
    Sample(Sample),
    /// Polled, nothing worth emitting yet.
    Nothing,
    /// Not attached; the string says why, for the waiting message.
    Waiting(String),
    /// The first frame failed the plausibility guard.
    Fatal(crate::core::error::CoachError),
}

impl<R: PageStore> SharedMemorySource<R> {
    /// A source in the waiting state — not attached, nothing touched.
    pub fn new() -> Self {
        Self {
            store: None,
            assembler: FrameAssembler::default(),
            stop: None,
            recorder: None,
            last_wait_message: None,
            session: None,
            samples: 0,
            last_static_poll: None,
            poll_every: PHYSICS_POLL,
            retry_every: ATTACH_RETRY,
        }
    }

    /// Coach live *and* record: every frame the coach sees is also written to
    /// a capture in `out_dir`, in the logger's format, so the session's laps
    /// can refine the track model later (`learn-track` accepts the capture
    /// alongside the originals).
    ///
    /// The recording is a byproduct, not a promise: if writing it fails — a
    /// full disk, a read-only directory — the source says so once and keeps
    /// coaching without it. A broken capture is lost laps; broken coaching
    /// is a lost session.
    pub fn with_recording(out_dir: impl Into<std::path::PathBuf>) -> Self {
        let mut source = Self::new();
        source.recorder = Some(super::record::LiveRecorder::new(out_dir.into()));
        source
    }

    fn stopped(&self) -> bool {
        self.stop.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
    }

    /// Sleep for `d`, waking early if the stop flag is set. Returns false
    /// when the wait ended because the session is over.
    fn sleep_or_stop(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        loop {
            if self.stopped() {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    fn step(&mut self) -> StepOutcome {
        // ---- The waiting state --------------------------------------------
        if self.store.is_none() {
            match R::attach() {
                Ok(store) => {
                    self.store = Some(store);
                    self.last_static_poll = None;
                }
                Err(reason) => return StepOutcome::Waiting(reason),
            }
        }

        // ---- Attached: poll ------------------------------------------------
        let store = self.store.as_mut().expect("just attached");
        if self
            .last_static_poll
            .is_none_or(|t| t.elapsed() >= STATIC_POLL)
        {
            self.assembler.update_static(&store.read_static());
            self.last_static_poll = Some(Instant::now());
        }
        let physics = store.read_physics();
        let graphics = store.read_graphics();
        match self.assembler.on_poll(&physics, &graphics, now_unix_ms()) {
            Ok(frame) => {
                // The plausibility guard runs on the first frame that
                // carries a session (the static page must have published,
                // or "empty track" would be a false alarm).
                if self.session.is_none() && frame.track.is_some() {
                    if let Err(e) =
                        crate::sims::assetto_corsa::schema::validate_frame(&frame)
                    {
                        return StepOutcome::Fatal(e);
                    }
                    self.session = Some(SessionInfo::from_ac_frame(&frame));
                }
                self.record_frame(&physics, &graphics);
                self.samples += 1;
                let track_length = self
                    .session
                    .as_ref()
                    .map(|s| s.track_length)
                    .unwrap_or(frame.track_spline_length);
                StepOutcome::Sample(Sample::from_ac_frame(&frame, track_length))
            }
            // Not a frame worth emitting — wait one poll interval and try
            // again. Same handling for both reasons: they are both "the
            // state has not changed enough to matter yet".
            Err(_) => StepOutcome::Nothing,
        }
    }

    /// Hand one emitted frame to the session recorder, if there is one.
    ///
    /// Frames that predate the static page are not recorded: the recorder
    /// needs the page to name the file (track and car), and a frame without
    /// it has no session to belong to. A write failure is warned about once
    /// and drops the recorder — coaching is the product, the capture the
    /// byproduct.
    fn record_frame(
        &mut self,
        physics: &PhysicsPage,
        graphics: &GraphicsPage,
    ) {
        let Some(recorder) = self.recorder.as_mut() else {
            return;
        };
        let Some(static_page) = self.assembler.static_page() else {
            return;
        };
        let position = self
            .assembler
            .last_position()
            .expect("an emitted frame always has a position");
        let timestamp = self
            .assembler
            .last_frame()
            .map(|f| f.timestamp)
            .expect("an emitted frame is the last frame");
        let sequence = self.assembler.sequence();
        if let Err(e) = recorder.on_frame(
            physics,
            graphics,
            static_page,
            position,
            timestamp,
            sequence,
        ) {
            eprintln!("warning: session recording stopped — {e}");
            // Drop, don't retry: whatever made the write fail (full disk,
            // read-only directory) is not something polling fixes, and every
            // further frame would fail the same way.
            self.recorder = None;
        }
    }
}

impl<R: PageStore> Default for SharedMemorySource<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: PageStore> TelemetrySource for SharedMemorySource<R> {
    fn next_sample(&mut self) -> crate::core::Result<Option<Sample>> {
        loop {
            if self.stopped() {
                return Ok(None);
            }
            match self.step() {
                StepOutcome::Sample(sample) => return Ok(Some(sample)),
                StepOutcome::Nothing => {
                    if !self.sleep_or_stop(self.poll_every) {
                        return Ok(None);
                    }
                }
                StepOutcome::Waiting(reason) => {
                    // Waiting, not broken: say why once per distinct reason,
                    // then retry. The logger retries on the same 2 s cadence;
                    // the message here is quieter because it repeats per
                    // *reason*, not per attempt.
                    if self.last_wait_message.as_deref() != Some(reason.as_str()) {
                        eprintln!("waiting for Assetto Corsa: {reason}");
                        self.last_wait_message = Some(reason);
                    }
                    self.store = None;
                    if !self.sleep_or_stop(self.retry_every) {
                        return Ok(None);
                    }
                }
                // The plausibility guard refused the first frame: the pages
                // are mapped but do not mean what this build thinks they
                // mean. Retry cannot fix that, so it propagates — the same
                // refusal a corrupt capture gets.
                StepOutcome::Fatal(e) => return Err(e),
            }
        }
    }

    fn set_stop_flag(&mut self, stop: Arc<AtomicBool>) {
        self.stop = Some(stop);
    }

    fn session(&self) -> Option<&SessionInfo> {
        self.session.as_ref()
    }

    fn describe(&self) -> String {
        let recording = self
            .recorder
            .as_ref()
            .and_then(|r| r.path())
            .map(|p| format!(", recording to {}", p.display()))
            .unwrap_or_default();
        match &self.session {
            None => "Assetto Corsa, waiting for the sim (not attached yet)".to_string(),
            Some(s) => format!(
                "Assetto Corsa live — {} in {} ({} m, {}){}",
                s.car, s.track, s.track_length, s.sim_version, recording
            ),
        }
    }

    fn stats(&self) -> SourceStats {
        SourceStats {
            samples: self.samples,
            ..SourceStats::default()
        }
    }
}

// ======================================================================
// The Windows mapping
// ======================================================================

#[cfg(windows)]
mod windows {
    use super::{GraphicsPage, PageStore, PhysicsPage, StaticPage};

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows_sys::Win32::System::Memory::{
        MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
        MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
    };

    /// One open mapping: the section handle plus the mapped view.
    ///
    /// Raw pointers are `!Send`, so `Send` is asserted here — the view is
    /// owned by exactly one thread (the source thread) for its whole life,
    /// which is what `Send` needs to mean for it to be sound.
    struct MappedView {
        /// The mapped view itself, as `MapViewOfFile` returns it (windows-sys
        /// wraps the pointer; `.Value` is the address).
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        handle: HANDLE,
    }

    // SAFETY: see the struct docs.
    unsafe impl Send for MappedView {}

    impl MappedView {
        /// Open `name` read-only and map it whole.
        ///
        /// Read-only is not a preference: the C# logger documents that the
        /// `ReadWrite` overload is *denied* on AC's pages — AC creates them
        /// without granting write access to other processes.
        ///
        /// `expect_bytes` is checked against the actual region size via
        /// `VirtualQuery`, so a page that comes up short — the signature of a
        /// layout this build does not know — fails here with both numbers,
        /// instead of handing back a struct read past the end of the mapping.
        fn open(name: &str, expect_bytes: usize) -> Result<Self, String> {
            let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, wide.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "cannot open {name} (is Assetto Corsa running? error {})",
                    unsafe { GetLastError() }
                ));
            }
            // SAFETY: the handle is a valid section handle we own.
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
            if view.Value.is_null() {
                let err = unsafe { GetLastError() };
                unsafe { CloseHandle(handle) };
                return Err(format!("cannot map {name} (error {err})"));
            }

            // Size check: how many bytes are actually mapped at the view.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a valid, correctly-sized out pointer.
            let queried = unsafe {
                VirtualQuery(view.Value, &mut info, std::mem::size_of_val(&info))
            };
            if queried == 0 {
                let err = unsafe { GetLastError() };
                unsafe { UnmapViewOfFile(view) };
                unsafe { CloseHandle(handle) };
                return Err(format!("cannot query {name} (error {err})"));
            }
            if info.RegionSize < expect_bytes {
                unsafe { UnmapViewOfFile(view) };
                unsafe { CloseHandle(handle) };
                return Err(format!(
                    "{name} is {} bytes, expected {expect_bytes} — the page \
                     layout does not match this build",
                    info.RegionSize
                ));
            }
            Ok(Self { view, handle })
        }

        /// A copy of the page as it stands right now.
        ///
        /// The read can tear: AC refreshes these pages from its own loop and
        /// offers no reader/writer protocol — the C# logger's
        /// `Marshal.PtrToStructure` has the same property. A torn read is a
        /// frame with a handful of fields from one tick and the rest from the
        /// next, which the plausibility guard (first frame) and the
        /// distance-grid resampler (everything else) are built to shrug off.
        fn read<T: Copy>(&self) -> T {
            // SAFETY: the view is at least `size_of::<T>()` bytes — checked
            // at open — and aligned to the page size, which exceeds any
            // alignment `T` needs.
            unsafe { std::ptr::read(self.view.Value as *const T) }
        }
    }

    impl Drop for MappedView {
        fn drop(&mut self) {
            // SAFETY: both handles are the ones `open` acquired and are
            // still valid — nothing else closes them.
            unsafe {
                UnmapViewOfFile(self.view);
                CloseHandle(self.handle);
            }
        }
    }

    /// The three AC pages, mapped. The real [`PageStore`].
    pub struct AcPages {
        physics: MappedView,
        graphics: MappedView,
        r#static: MappedView,
    }

    impl PageStore for AcPages {
        fn attach() -> Result<Self, String> {
            Ok(Self {
                physics: MappedView::open("acpmf_physics", std::mem::size_of::<PhysicsPage>())?,
                graphics: MappedView::open(
                    "acpmf_graphics",
                    std::mem::size_of::<GraphicsPage>(),
                )?,
                r#static: MappedView::open("acpmf_static", std::mem::size_of::<StaticPage>())?,
            })
        }

        fn read_physics(&mut self) -> PhysicsPage {
            self.physics.read()
        }

        fn read_graphics(&mut self) -> GraphicsPage {
            self.graphics.read()
        }

        fn read_static(&mut self) -> StaticPage {
            self.r#static.read()
        }
    }
}

#[cfg(windows)]
pub use windows::AcPages;

/// The AC live source, as the provider hands it over on Windows.
#[cfg(windows)]
pub type AcSharedMemorySource = SharedMemorySource<AcPages>;


// ---- The live source, over a scripted fake sim -----------------------
//
// Everything the Windows mapping does is copy bytes out of three pages;
// everything *interesting* — the waiting state, the poll loop, the stop
// paths — is here, testable on any platform. The test harness runs each
// test on its own thread, so the script lives in a thread-local and
// needs no plumbing between tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;

    use super::{GraphicsPage, PageStore, PhysicsPage, StaticPage};

    /// What the fake sim does: which attach attempts fail (in order),
    /// then which page pair each poll publishes.
    pub struct Script {
        pub attach_errors: Vec<String>,
        pub pages: Vec<(PhysicsPage, GraphicsPage)>,
        pub static_page: StaticPage,
        pub polls: usize,
    }

    thread_local! {
        pub static SCRIPT: RefCell<Script> = RefCell::new(Script {
            attach_errors: Vec::new(),
            pages: Vec::new(),
            static_page: super::pages().2,
            polls: 0,
        });
    }

    /// The scripted stand-in for the Windows mapping.
    pub struct FakeStore;

    impl PageStore for FakeStore {
        fn attach() -> Result<Self, String> {
            SCRIPT.with(|s| {
                let mut s = s.borrow_mut();
                if !s.attach_errors.is_empty() {
                    return Err(s.attach_errors.remove(0));
                }
                if s.pages.is_empty() {
                    return Err("sim not running".to_string());
                }
                Ok(FakeStore)
            })
        }

        fn read_physics(&mut self) -> PhysicsPage {
            SCRIPT.with(|s| {
                let mut s = s.borrow_mut();
                let (p, _) = s.pages[s.polls % s.pages.len()];
                s.polls += 1;
                p
            })
        }

        fn read_graphics(&mut self) -> GraphicsPage {
            SCRIPT.with(|s| {
                let s = s.borrow();
                s.pages[s.polls.saturating_sub(1) % s.pages.len()].1
            })
        }

        fn read_static(&mut self) -> StaticPage {
            SCRIPT.with(|s| s.borrow().static_page)
        }
    }
}

/// UTF-16 encode a string, for building page fixtures.
#[cfg(test)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Write `s` into a fixed buffer as the page would: UTF-16, then the
/// NUL terminator, then untouched padding.
#[cfg(test)]
fn fill_wide(buf: &mut [u16], s: &str) {
    let chars = wide(s);
    buf[..chars.len()].copy_from_slice(&chars);
    buf[chars.len()] = 0;
}

/// A plausible set of pages — Monza, the SF70H on track — for tests in this
/// module and in `record`'s, which share it through [`fake`].
#[cfg(test)]
pub(crate) fn pages() -> (PhysicsPage, GraphicsPage, StaticPage) {
    let mut graphics = GraphicsPage {
        packet_id: 0,
        status: 2, // AC_LIVE
        session: 0,
        current_time: [0; 15],
        last_time: [0; 15],
        best_time: [0; 15],
        split: [0; 15],
        completed_laps: 0,
        position: 1,
        i_current_time: 1000,
        i_last_time: 0,
        i_best_time: 0,
        session_time_left: 0.0,
        distance_travelled: 0.0,
        is_in_pit: 0,
        current_sector_index: 0,
        last_sector_time: 0,
        number_of_laps: 0,
        tyre_compound: [0; 33],
        replay_time_multiplier: 0.0,
        normalized_car_position: 0.123,
        car_coordinates: [10.0, 0.5, 20.0],
        penalty_time: 0.0,
        flag: 0,
        ideal_line_on: 0,
        is_in_pit_lane: 0,
        surface_grip: 0.98,
        mandatory_pit_done: 0,
        wind_speed: 0.0,
        wind_direction: 0.0,
    };
    graphics.current_time[..4].copy_from_slice(&wide("1:23"));
    graphics.last_time[..4].copy_from_slice(&wide("1:25"));

    let mut static_page = StaticPage {
        sm_version: [0; 15],
        ac_version: [0; 15],
        number_of_sessions: 1,
        num_cars: 1,
        car_model: [0; 33],
        track: [0; 33],
        player_name: [0; 33],
        player_surname: [0; 33],
        player_nick: [0; 33],
        sector_count: 3,
        max_torque: 0.0,
        max_power: 0.0,
        max_rpm: 15000,
        max_fuel: 105.0,
        suspension_max_travel: [0.0; 4],
        tyre_radius: [0.0; 4],
        max_turbo_boost: 0.0,
        deprecated1: 0.0,
        deprecated2: 0.0,
        penalties_enabled: 0,
        aid_fuel_rate: 0.0,
        aid_tire_rate: 0.0,
        aid_mechanical_damage: 0.0,
        aid_allow_tyre_blankets: 0.0,
        aid_stability: 0.0,
        aid_auto_clutch: 0,
        aid_auto_blip: 0,
        has_drs: 0,
        has_ers: 0,
        has_kers: 0,
        kers_max_joules: 0.0,
        engine_brake_settings_count: 0,
        ers_power_controller_count: 0,
        track_spline_length: 5758.6606,
        track_configuration: [0; 33],
        ers_max_j: 0.0,
        is_timed_race: 0,
        has_extra_lap: 0,
        car_skin: [0; 33],
        reversed_grid_positions: 0,
        pit_window_start: 0,
        pit_window_end: 0,
        is_online: 0,
    };
    fill_wide(&mut static_page.sm_version, "1.7");
    fill_wide(&mut static_page.ac_version, "1.16.4");
    fill_wide(&mut static_page.car_model, "ks_ferrari_sf70h");
    fill_wide(&mut static_page.track, "monza");

    let physics = PhysicsPage {
        packet_id: 7,
        gas: 0.5,
        brake: 0.0,
        fuel: 90.0,
        gear: 3,
        rpms: 7200,
        steer_angle: -0.2,
        speed_kmh: 250.0,
        velocity: [0.0; 3],
        acc_g: [0.0; 3],
        wheel_slip: [0.0; 4],
        wheel_load: [0.0; 4],
        wheel_pressure: [0.0; 4],
        wheel_angular_speed: [0.0; 4],
        tyre_wear: [0.0; 4],
        tyre_dirty_level: [0.0; 4],
        tyre_core_temp: [90.0; 4],
        camber_rad: [0.0; 4],
        suspension_travel: [0.0; 4],
        drs: 0.0,
        tc: 0.0,
        heading: 1.25,
        pitch: 0.0,
        roll: 0.0,
        cg_height: 0.3,
        car_damage: [0.0; 5],
        number_of_tyres_out: 0,
        pit_limiter_on: 0,
        abs: 0.0,
        kers_charge: 0.0,
        kers_input: 0.0,
        auto_shifter_on: 0,
        ride_height: [0.0; 2],
        turbo_boost: 0.0,
        ballast: 0.0,
        air_density: 1.2,
        air_temp: 25.0,
        road_temp: 30.0,
        local_angular_velocity: [0.0; 3],
        final_ff: 0.0,
        performance_meter: 0.0,
        engine_brake: 0,
        ers_recovery_level: 0,
        ers_power_level: 0,
        ers_heat_charging: 0,
        ers_is_charging: 0,
        kers_current_kj: 0.0,
        drs_available: 0,
        drs_enabled: 0,
        brake_temp: [400.0; 4],
        clutch: 0.0,
        tyre_temp_i: [80.0; 4],
        tyre_temp_m: [85.0; 4],
        tyre_temp_o: [90.0; 4],
        is_ai_controlled: 0,
        tyre_contact_point: [Coordinates::default(); 4],
        tyre_contact_normal: [Coordinates::default(); 4],
        tyre_contact_heading: [Coordinates::default(); 4],
        brake_bias: 0.54,
        local_velocity: [0.5, 0.0, 69.4],
    };

    (physics, graphics, static_page)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Layout: the numbers the logger verified against a live page ----

    #[test]
    fn the_physics_page_is_580_bytes_with_local_velocity_last() {
        assert_eq!(std::mem::size_of::<PhysicsPage>(), 580);
        assert_eq!(std::mem::offset_of!(PhysicsPage, heading), 208);
        assert_eq!(std::mem::offset_of!(PhysicsPage, local_velocity), 568);
        assert_eq!(std::mem::offset_of!(PhysicsPage, brake), 8);
        assert_eq!(std::mem::offset_of!(PhysicsPage, speed_kmh), 28);
        assert_eq!(std::mem::offset_of!(PhysicsPage, number_of_tyres_out), 244);
        assert_eq!(std::mem::offset_of!(PhysicsPage, local_angular_velocity), 296);
    }

    #[test]
    fn the_graphics_page_is_296_bytes_with_the_probed_offsets() {
        assert_eq!(std::mem::size_of::<GraphicsPage>(), 296);
        // The logger's live-probe anchors (Graphics.cs): the 66-byte tyre
        // compound buffer pads to 244, so these land where the probe saw them.
        assert_eq!(std::mem::offset_of!(GraphicsPage, normalized_car_position), 248);
        assert_eq!(std::mem::offset_of!(GraphicsPage, car_coordinates), 252);
        assert_eq!(std::mem::offset_of!(GraphicsPage, penalty_time), 264);
        assert_eq!(std::mem::offset_of!(GraphicsPage, surface_grip), 280);
        assert_eq!(std::mem::offset_of!(GraphicsPage, wind_direction), 292);
        // The time strings are 15 UTF-16 chars each, back to back from 12.
        assert_eq!(std::mem::offset_of!(GraphicsPage, current_time), 12);
        assert_eq!(std::mem::offset_of!(GraphicsPage, split), 102);
        assert_eq!(std::mem::offset_of!(GraphicsPage, tyre_compound), 176);
    }

    #[test]
    fn the_static_page_is_688_bytes_with_the_probed_offsets() {
        assert_eq!(std::mem::size_of::<StaticPage>(), 688);
        // The logger's live-probe anchors (StaticInfo.cs).
        assert_eq!(std::mem::offset_of!(StaticPage, pit_window_end), 680);
        assert_eq!(std::mem::offset_of!(StaticPage, is_online), 684);
        assert_eq!(std::mem::offset_of!(StaticPage, sector_count), 400);
        assert_eq!(std::mem::offset_of!(StaticPage, track_spline_length), 520);
        assert_eq!(std::mem::offset_of!(StaticPage, track), 134);
        assert_eq!(std::mem::offset_of!(StaticPage, track_configuration), 524);
        assert_eq!(std::mem::offset_of!(StaticPage, car_model), 68);
    }

    /// The one test that would catch a mis-transcribed *field order*
    /// rather than a wrong size: write known bytes at a probed offset,
    /// cast, and read them back as the documented field.
    #[test]
    fn page_fields_read_back_at_the_probed_byte_offsets() {
        let mut bytes = [0u8; 580];
        // Physics_Heading at 208, as raw f32 bits.
        bytes[208..212].copy_from_slice(&7.5f32.to_le_bytes());
        // LocalVelocity[0] at 568.
        bytes[568..572].copy_from_slice(&(-2.25f32).to_le_bytes());
        let physics: &PhysicsPage = unsafe { &*(&bytes as *const [u8; 580] as *const PhysicsPage) };
        assert_eq!(physics.heading, 7.5);
        assert_eq!(physics.local_velocity[0], -2.25);

        let mut bytes = [0u8; 296];
        // NormalizedCarPosition at 248, CarCoordinates[0] at 252.
        bytes[248..252].copy_from_slice(&0.5f32.to_le_bytes());
        bytes[252..256].copy_from_slice(&249.745f32.to_le_bytes());
        // SurfaceGrip at 280.
        bytes[280..284].copy_from_slice(&0.98f32.to_le_bytes());
        let graphics: &GraphicsPage = unsafe { &*(&bytes as *const [u8; 296] as *const GraphicsPage) };
        assert_eq!(graphics.normalized_car_position, 0.5);
        assert_eq!(graphics.car_coordinates[0], 249.745);
        assert_eq!(graphics.surface_grip, 0.98);

        let mut bytes = [0u8; 688];
        // TrackSPlineLength at 520.
        bytes[520..524].copy_from_slice(&4286.7896f32.to_le_bytes());
        let static_page: &StaticPage = unsafe { &*(&bytes as *const [u8; 688] as *const StaticPage) };
        assert_eq!(static_page.track_spline_length, 4286.7896);
    }

    // ---- String sanitisation --------------------------------------------


    #[test]
    fn fixed_buffers_cut_at_the_first_nul() {
        let mut buf = [0u16; 15];
        buf[..5].copy_from_slice(&wide("monza"));
        buf[7] = b'X' as u16; // past the terminator: adjacent memory, not us
        assert_eq!(wchar_string(&buf), "monza");
    }

    #[test]
    fn control_characters_and_surrogates_drop_out() {
        let mut buf = [0u16; 15];
        let poisoned: Vec<u16> = vec![b'a' as u16, 0x01, 0x7F, 0xD800, b'b' as u16, 0];
        buf[..poisoned.len()].copy_from_slice(&poisoned);
        // The unpaired surrogate decodes lossily to U+FFFD and is dropped
        // with the controls; "a" and "b" survive.
        assert_eq!(wchar_string(&buf), "ab");
    }

    #[test]
    fn ascii_fields_go_empty_rather_than_pass_mojibake() {
        let mut buf = [0u16; 15];
        buf[..3].copy_from_slice(&wide("ac "));
        buf[3] = 0x65E5; // CJK: legal Unicode, impossible in an AC version string
        buf[4] = 0;
        assert_eq!(wchar_ascii(&buf), "");
        assert_eq!(wchar_string(&buf), "ac 日");
    }

    #[test]
    fn unwritten_buffers_read_as_empty() {
        let buf = [0u16; 33];
        assert_eq!(wchar_ascii(&buf), "");
        assert_eq!(wchar_string(&buf), "");
    }

    // ---- The assembler ---------------------------------------------------

    #[test]
    fn no_frame_before_the_car_is_on_track() {
        let (physics, mut graphics, _static) = pages();
        graphics.car_coordinates = [0.0; 3];
        let mut asm = FrameAssembler::default();
        assert_eq!(
            asm.on_poll(&physics, &graphics, 1000),
            Err(SkipReason::NoPosition)
        );
        assert_eq!(asm.sequence(), 0, "nothing was emitted");

        // On track: the first frame arrives, carrying the fresh position.
        graphics.car_coordinates = [10.0, 0.5, 20.0];
        let frame = asm
            .on_poll(&physics, &graphics, 1001)
            .expect("emit");
        assert_eq!(frame.pos_x, 10.0);
        assert_eq!(frame.sequence, 1);

        // Back to zeros (a session transition): the last good position is
        // kept, exactly as the logger keeps `_lastPosition`.
        graphics.car_coordinates = [0.0; 3];
        let frame = asm
            .on_poll(&physics, &graphics, 1002)
            .expect("emit");
        assert_eq!(frame.pos_x, 10.0, "the held position is written");
    }

    #[test]
    fn duplicate_packet_ids_are_skipped_only_once_the_id_has_advanced() {
        let (mut physics, graphics, _static) = pages();
        let mut asm = FrameAssembler::default();

        // The id never changes: every poll emits, because a repeat cannot
        // be told from "this build never advances the id" (the logger's
        // dedupe-probe rule).
        for i in 0..3 {
            asm.on_poll(&physics, &graphics, 1000 + i)
                .expect("a static packet id must not be deduped");
        }

        // The id advances once — now repeats are recognisable as repeats.
        physics.packet_id = 8;
        asm.on_poll(&physics, &graphics, 1100).expect("emit");
        assert_eq!(
            asm.on_poll(&physics, &graphics, 1101),
            Err(SkipReason::DuplicatePacket),
            "the republished frame is dropped"
        );
        physics.packet_id = 9;
        asm.on_poll(&physics, &graphics, 1102).expect("emit");
        // Sequences count emissions, not polls.
        assert_eq!(asm.sequence(), 5);
    }

    #[test]
    fn an_assembled_frame_carries_the_session_and_the_fields_the_pipeline_reads() {
        let (physics, graphics, static_page) = pages();
        let mut asm = FrameAssembler::default();
        asm.update_static(&static_page);

        let frame = asm
            .on_poll(&physics, &graphics, 1_759_000_000_000)
            .expect("emit");

        assert_eq!(frame.timestamp, 1_759_000_000_000);
        assert_eq!(frame.sequence, 1);
        assert_eq!(frame.physics_packet_id, 7);
        assert_eq!(frame.rpms, 7200.0, "the page's integer rpm becomes f32");
        assert_eq!(frame.heading, 1.25);
        assert_eq!(frame.speed_kmh, 250.0);
        assert_eq!(frame.status, 2);
        assert!((frame.lap_distance() - 0.123 * 5758.6606).abs() < 1e-3);
        assert_eq!(frame.track.as_deref(), Some("monza"));
        assert_eq!(frame.car_model.as_deref(), Some("ks_ferrari_sf70h"));
        assert_eq!(frame.track_configuration.as_deref(), Some(""));
        assert_eq!(frame.sector_count, 3);
        assert_eq!(frame.track_spline_length, 5758.6606);
        // The static page is held between its slow re-reads: a poll with
        // no fresh static copy still carries the session.
        let frame2 = asm
            .on_poll(&physics, &graphics, 1_759_000_000_010)
            .expect("emit");
        assert_eq!(frame2.track.as_deref(), Some("monza"));
    }

    #[test]
    fn without_a_static_page_the_session_fields_are_absent_but_the_frame_flows() {
        let (physics, graphics, _static) = pages();
        let mut asm = FrameAssembler::default();
        let frame = asm
            .on_poll(&physics, &graphics, 0)
            .expect("emit");
        assert_eq!(frame.track, None);
        assert_eq!(frame.track_spline_length, 0.0);
    }

    use std::sync::atomic::AtomicBool;
    use super::fake::{FakeStore, SCRIPT};
    use super::pages;

    /// A source with sub-millisecond waits, so tests do not sleep for real.
    fn fast_source() -> SharedMemorySource<FakeStore> {
        let mut source = SharedMemorySource::new();
        source.poll_every = Duration::from_micros(100);
        source.retry_every = Duration::from_micros(100);
        source
    }

    /// Point the fake sim at one stable page pair: attach succeeds, every
    /// poll publishes the same pages (a packet id that never advances, so
    /// every poll emits a frame — the assembler's dedupe-probe rule).
    fn script_stable_session() {
        let (physics, graphics, static_page) = pages();
        SCRIPT.with(|s| {
            *s.borrow_mut() = fake::Script {
                attach_errors: Vec::new(),
                pages: vec![(physics, graphics)],
                static_page,
                polls: 0,
            }
        });
    }

    #[test]
    fn the_source_waits_for_the_sim_then_streams_and_derives_the_session() {
        script_stable_session();
        // The first attach fails — the sim is still starting. The source
        // must say so once and retry, not error out.
        SCRIPT.with(|s| {
            s.borrow_mut()
                .attach_errors
                .push("cannot open acpmf_physics".to_string())
        });

        let mut source = fast_source();
        assert!(
            source.session().is_none(),
            "no session before the first sample is read"
        );
        assert!(
            source.describe().contains("waiting"),
            "{}",
            source.describe()
        );

        let sample = source
            .next_sample()
            .expect("the retry after the failed attach succeeds")
            .expect("a sample");
        // The conversion is the shared one, so spot-check one convention:
        // speed in m/s, from the page's 250 km/h.
        assert!((sample.speed - 250.0 / 3.6).abs() < 1e-3);
        assert_eq!(sample.rpm, 7200.0);

        let session = source.session().expect("session after first sample");
        assert_eq!(session.track.track, "monza");
        assert_eq!(session.car, "ks_ferrari_sf70h");
        assert_eq!(session.track_length, 5758.6606);
        assert_eq!(session.sim_version, "AC 1.16.4, SM 1.7");
        assert!(
            source.describe().contains("monza"),
            "{}",
            source.describe()
        );
        assert_eq!(source.stats().samples, 1);

        // And it keeps streaming.
        assert!(source.next_sample().unwrap().is_some());
        assert_eq!(source.stats().samples, 2);
    }

    #[test]
    fn a_set_stop_flag_ends_the_attach_wait() {
        // The sim never appears. Without the stop flag this source would
        // retry forever — by design, "not running" is a state — so shutdown
        // has to come from the flag.
        SCRIPT.with(|s| {
            *s.borrow_mut() = fake::Script {
                attach_errors: vec!["sim not running".to_string()],
                ..fake_default()
            }
        });

        let mut source = fast_source();
        let stop = Arc::new(AtomicBool::new(false));
        source.set_stop_flag(Arc::clone(&stop));

        // The wait is ended from outside, the way the live wiring ends it.
        let setter = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let start = std::time::Instant::now();
        assert_eq!(source.next_sample().unwrap(), None);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the stop flag must cut the retry wait short"
        );
    }

    /// `fake::Script` defaults (attach ok, no pages) for the `..` spread.
    fn fake_default() -> super::fake::Script {
        fake::Script {
            attach_errors: Vec::new(),
            pages: Vec::new(),
            static_page: pages().2,
            polls: 0,
        }
    }

    #[test]
    fn a_car_not_yet_on_track_produces_no_samples_and_the_stop_flag_ends_the_poll() {
        // Attached, but the graphics page reports no position: the poll
        // loop must spin quietly, and the stop flag must break it.
        let (mut physics, mut graphics, static_page) = pages();
        graphics.car_coordinates = [0.0; 3];
        physics.packet_id = 0;
        SCRIPT.with(|s| {
            *s.borrow_mut() = fake::Script {
                attach_errors: Vec::new(),
                pages: vec![(physics, graphics)],
                static_page,
                polls: 0,
            }
        });

        let mut source = fast_source();
        let stop = Arc::new(AtomicBool::new(false));
        source.set_stop_flag(Arc::clone(&stop));
        let setter = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        assert_eq!(source.next_sample().unwrap(), None);
        assert!(source.session().is_none(), "no frame was ever emitted");
        assert_eq!(source.stats().samples, 0);
    }

    #[test]
    fn an_implausible_first_frame_is_refused_not_coached() {
        // The pages map and frames flow, but the values cannot be real —
        // here, a track length no circuit has. This is the shared-memory
        // layout-drift failure mode, and it must refuse loudly exactly as a
        // corrupt capture would, not stream plausible-looking garbage.
        script_stable_session();
        SCRIPT.with(|s| {
            s.borrow_mut().static_page.track_spline_length = 99_999.0;
        });

        let mut source = fast_source();
        let err = source.next_sample().expect_err("the guard must refuse");
        let msg = err.to_string();
        assert!(
            msg.contains("implausible value for StaticInfo_TrackSPlineLength"),
            "{msg}"
        );
        assert_eq!(source.stats().samples, 0, "the frame was not handed on");
    }

    /// The record-while-coaching setting, end to end: a source built with
    /// [`SharedMemorySource::with_recording`] coaches off the pages and
    /// leaves behind a capture the pipeline's own replay reader accepts —
    /// the interchange contract holding for coaching captures exactly as it
    /// does for the logger's.
    #[test]
    fn a_recording_source_leaves_a_replayable_capture_of_the_coached_frames() {
        script_stable_session();
        let dir = std::env::temp_dir().join("coach_live_record_tests/round_trip");
        let _ = std::fs::remove_dir_all(&dir);

        let mut source =
            SharedMemorySource::<FakeStore>::with_recording(dir.clone());
        source.poll_every = Duration::from_micros(100);
        source.retry_every = Duration::from_micros(100);
        for _ in 0..5 {
            assert!(source.next_sample().unwrap().is_some(), "coaching streams");
        }
        assert!(
            source.describe().contains("recording to"),
            "the connection line says where the laps are going: {}",
            source.describe()
        );
        drop(source); // the session ends: the recorder finishes the gzip stream

        let path = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .expect("one capture in the directory")
            .unwrap()
            .path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("telemetry_ac_monza_ks_ferrari_sf70h_")
                && name.ends_with(".ndjson.gz"),
            "the capture is named for the session it coached: {name}"
        );

        let mut replay =
            crate::sims::assetto_corsa::NdjsonReplaySource::open(&path)
                .expect("the coaching capture reopens like the logger's");
        assert!(replay.next_sample().unwrap().is_some(), "a first frame");
        let session = replay.session().expect("the session is in the capture");
        assert_eq!(session.track.track, "monza");
        assert_eq!(session.car, "ks_ferrari_sf70h");
        let mut frames = 1;
        while replay.next_sample().unwrap().is_some() {
            frames += 1;
        }
        assert_eq!(frames, 5, "every coached frame was recorded");
    }
}
