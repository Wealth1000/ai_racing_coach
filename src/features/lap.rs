//! Lap boundaries and lap quality.
//!
//! # Why not `Graphics_CompletedLaps`
//!
//! The obvious approach — group samples by AC's own lap counter — is what the
//! first implementation did, and it is the direct cause of the reported
//! "estimated track length: 29.9 m". Measured behaviour of that counter:
//!
//! * It increments 1-2 frames *after* the car crosses the line, so every group
//!   ends just past the spline wrap, at `lap_frac ~= 0.0006`. The last sample of
//!   each "lap" therefore sits about 2.6 m from the line rather than 4,286 m
//!   from it, and any code inferring track length from the final lap distance
//!   gets a two-orders-of-magnitude underestimate.
//! * It does not increment at all on the first crossing of a session joined
//!   mid-lap, because AC had already started the clock. In the MX5 capture the
//!   counter reads 0 across two separate crossings.
//!
//! So laps are delimited by the wrap in `lap_frac` itself, which is exact: the
//! fraction is monotonically increasing within a lap and drops from ~1.0 to ~0.0
//! at the line, and nowhere else.
//!
//! # Why lap validity needs its own detector
//!
//! `Graphics_IsValidLap` does not exist. It is an ACC field, and logger v3.0
//! removed the ACC-only columns precisely because AC never publishes those bytes
//! and they were being written out as zeros indistinguishable from real
//! readings. So the cut-track judgement has to be made here, from
//! `Physics_NumberOfTyresOut`.
//!
//! That still misses spins, which is not hypothetical: the MX5's third lap
//! contains one. A spin on a wide corner need not put a single wheel off the
//! track, so no tyres-out count catches it. What does catch it is total
//! rotation — a clean lap of a closed circuit nets exactly one revolution, and
//! that lap nets two (+4.0016*pi measured).

use crate::core::ids::LapId;
use crate::core::math::{TAU, angle_delta};
use crate::core::sample::Sample;
use crate::telemetry::frame::AcFrame;

/// A drop in `lap_frac` larger than this is a start/finish crossing.
///
/// Generous on purpose: a real wrap is a drop of very nearly 1.0, while the
/// largest spurious backward step measured in either capture is a few
/// centimetres, so anything in between separates them comfortably.
const WRAP_DROP: f32 = 0.5;

/// A lap must cover at least this fraction of the spline to count as complete.
const COMPLETE_FRACTION: f32 = 0.98;

/// Tyres off the track, per AC, before a lap is considered to have left it.
const OFF_TRACK_TYRES: u8 = 3;

/// How far the net rotation may deviate from one revolution.
///
/// A clean lap measures +2*pi to within a few hundredths. A spin adds a whole
/// further revolution. 0.35 rad (20 deg) sits far from both.
const SPIN_TOLERANCE: f32 = 0.35;

/// Frames to wait after a crossing before reading `Graphics_iLastTime`, which
/// latches 1-3 frames late.
const LAST_TIME_LATCH_FRAMES: u32 = 5;

/// How far AC's `iLastTime` may differ from the wall-clock duration before we
/// stop believing it.
///
/// The two measure the same interval by completely different means — AC's sim
/// clock versus `DateTimeOffset.UtcNow` deltas — so they should agree closely,
/// and a large disagreement means one of them is measuring something else. It
/// catches AC's inflated first lap directly: the MX5's first crossing reports
/// `iLastTime` = 207,150 ms for a lap that took 131,329 ms of wall clock,
/// because AC's clock already stood at 75,822 ms when logging started, and
/// 75,822 + 131,329 = 207,151.
const AC_TIME_AGREEMENT_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum LapQuality {
    /// Spans the whole lap, on track, no spin. Usable as a reference.
    Clean,
    /// An opening or closing fragment of the capture, not a whole lap.
    Partial,
    /// AC reported three or more wheels off the track.
    OffTrack,
    /// Total rotation is not one revolution — the car spun.
    Spun,
    /// The sim was paused, in replay, or the car was in the pits.
    NotLive,
}

impl LapQuality {
    pub fn is_clean(self) -> bool {
        self == LapQuality::Clean
    }

    pub fn reason(self) -> &'static str {
        match self {
            LapQuality::Clean => "clean",
            LapQuality::Partial => "partial lap",
            LapQuality::OffTrack => "3+ tyres off track",
            LapQuality::Spun => "spun",
            LapQuality::NotLive => "not live (paused/replay/pits)",
        }
    }
}

/// One lap's worth of samples, plus what we make of it.
#[derive(Debug, Clone)]
pub struct Lap {
    pub id: LapId,
    pub samples: Vec<Sample>,
    pub quality: LapQuality,

    /// Fraction of the spline the samples actually cover.
    pub coverage: f32,
    /// Total heading change over the lap, in radians. ~+2*pi when clean.
    pub net_rotation: f32,
    /// Frames with 3+ tyres off the track.
    pub off_track_frames: u32,
    /// Frames where the sim was not live, or the car was in the pits.
    pub not_live_frames: u32,

    /// AC's own time for this lap, in ms, when it can be trusted.
    ///
    /// `None` when logging began mid-lap, because AC's figure then includes time
    /// before the capture started.
    pub ac_lap_time_ms: Option<i32>,
    /// Wall-clock duration from the first to the last sample. Always available,
    /// but a wall clock: it keeps running while the sim is paused.
    pub wall_duration_ms: i64,
}

impl Lap {
    /// Best available lap time in seconds: AC's if trustworthy, else wall clock.
    pub fn lap_time_s(&self) -> f32 {
        match self.ac_lap_time_ms {
            Some(ms) => ms as f32 / 1000.0,
            None => self.wall_duration_ms as f32 / 1000.0,
        }
    }
}

/// Splits a frame stream into laps.
///
/// Streaming in the sense that matters — it never holds more than the lap
/// currently being built — but a lap is inherently the unit of comparison here,
/// so one lap's samples are buffered.
pub struct LapTracker {
    track_length: f32,
    current: Vec<Sample>,
    prev_frac: Option<f32>,
    /// Highest `lap_frac` seen so far this lap, used to clamp out the small
    /// backward steps (58 of them in the MX5 capture, none in the F138) that
    /// would otherwise make the distance axis non-monotone.
    max_frac: f32,
    next_id: u32,

    off_track_frames: u32,
    not_live_frames: u32,
    rotation: f32,
    prev_heading: Option<f32>,

    /// Set at a crossing; counts down frames until `iLastTime` has latched.
    pending: Option<PendingLap>,
    /// False only for the opening fragment of a capture, which we joined mid-lap.
    started_at_wrap: bool,
}

/// A lap awaiting AC's `iLastTime`, which arrives a few frames after the line.
struct PendingLap {
    lap: Lap,
    frames_waited: u32,
    /// Whether this lap began at a crossing we actually saw. The opening segment
    /// of a capture did not, so AC's time for it counts laps we never logged.
    started_at_wrap: bool,
}

impl LapTracker {
    pub fn new(track_length: f32) -> Self {
        Self {
            track_length,
            current: Vec::new(),
            prev_frac: None,
            max_frac: 0.0,
            next_id: 0,
            off_track_frames: 0,
            not_live_frames: 0,
            rotation: 0.0,
            prev_heading: None,
            pending: None,
            started_at_wrap: false,
        }
    }

    /// Feed one frame. Returns any lap that just became complete.
    pub fn push(&mut self, frame: &AcFrame) -> Option<Lap> {
        let mut sample = Sample::from_ac_frame(frame, self.track_length);

        // Clamp the distance axis monotone within a lap. Left alone, a
        // centimetre-scale backward step puts a zero or negative denominator
        // into the resampler and a spurious spike into the curvature.
        if sample.lap_frac < self.max_frac && self.max_frac - sample.lap_frac < WRAP_DROP {
            sample.lap_frac = self.max_frac;
            sample.lap_distance = self.max_frac * self.track_length;
        }

        let wrapped = self
            .prev_frac
            .is_some_and(|prev| prev - sample.lap_frac > WRAP_DROP);

        let mut emitted = None;

        if wrapped {
            let lap = self.close_lap();
            self.pending = Some(PendingLap {
                lap,
                frames_waited: 0,
                started_at_wrap: self.started_at_wrap,
            });
            self.reset_lap_state();
            // Every lap after the first was entered through a crossing we saw.
            self.started_at_wrap = true;
        }

        // Resolve a lap that was waiting for AC's authoritative time.
        if let Some(p) = &mut self.pending {
            p.frames_waited += 1;
            if p.frames_waited >= LAST_TIME_LATCH_FRAMES {
                let mut p = self.pending.take().expect("checked above");
                let agrees = (frame.i_last_time as i64 - p.lap.wall_duration_ms).abs()
                    < AC_TIME_AGREEMENT_MS;
                if p.started_at_wrap && frame.i_last_time > 0 && agrees {
                    p.lap.ac_lap_time_ms = Some(frame.i_last_time);
                }
                emitted = Some(p.lap);
            }
        }

        // Accumulate this frame into the lap being built.
        self.max_frac = self.max_frac.max(sample.lap_frac);
        self.prev_frac = Some(sample.lap_frac);

        if sample.tyres_out >= OFF_TRACK_TYRES {
            self.off_track_frames += 1;
        }
        if !frame.is_live() || frame.in_pits() {
            self.not_live_frames += 1;
        }
        if let Some(prev) = self.prev_heading {
            self.rotation += angle_delta(prev, sample.heading);
        }
        self.prev_heading = Some(sample.heading);
        self.current.push(sample);

        emitted
    }

    /// Flush what is left at end of stream: any lap still waiting for AC's
    /// `iLastTime`, then the trailing fragment.
    ///
    /// Returns **only** laps that [`Self::push`] has not already handed back, so
    /// a caller that collects both never sees a lap twice.
    pub fn finish(mut self) -> Vec<Lap> {
        let mut out = Vec::new();
        // A lap still pending at EOF never got its authoritative time; it falls
        // back to the wall clock, which the lap table flags.
        if let Some(p) = self.pending.take() {
            out.push(p.lap);
        }
        if !self.current.is_empty() {
            out.push(self.close_lap());
        }
        out
    }

    fn close_lap(&mut self) -> Lap {
        let samples = std::mem::take(&mut self.current);
        let id = LapId(self.next_id);
        self.next_id += 1;

        let coverage = match (samples.first(), samples.last()) {
            (Some(f), Some(l)) => (l.lap_frac - f.lap_frac).abs(),
            _ => 0.0,
        };
        let wall_duration_ms = match (samples.first(), samples.last()) {
            (Some(f), Some(l)) => l.t_ms - f.t_ms,
            _ => 0,
        };

        // Order matters. Completeness first: a partial lap never had a full
        // revolution to make, so it cannot be judged for rotation. Then
        // not-live, whose geometry may be junk. Then the spin, ahead of
        // off-track, because a spin is the more specific finding and usually
        // *causes* the off-track frames — reporting the symptom would bury it.
        let quality = if coverage < COMPLETE_FRACTION {
            LapQuality::Partial
        } else if self.not_live_frames > 0 {
            LapQuality::NotLive
        } else if (self.rotation.abs() - TAU).abs() > SPIN_TOLERANCE {
            LapQuality::Spun
        } else if self.off_track_frames > 0 {
            LapQuality::OffTrack
        } else {
            LapQuality::Clean
        };

        Lap {
            id,
            samples,
            quality,
            coverage,
            net_rotation: self.rotation,
            off_track_frames: self.off_track_frames,
            not_live_frames: self.not_live_frames,
            ac_lap_time_ms: None,
            wall_duration_ms,
        }
    }

    fn reset_lap_state(&mut self) {
        self.max_frac = 0.0;
        self.prev_frac = None;
        self.off_track_frames = 0;
        self.not_live_frames = 0;
        self.rotation = 0.0;
        self.prev_heading = None;
    }
}
