//! The canonical corner set for one track and layout, learned once, offline.
//!
//! # Why this exists
//!
//! [`corner::detect_corners`] answers *what did this lap do*, which is not the
//! same question as *what corners does this track have*. Measured on the two Red
//! Bull Ring captures, the detector disagrees with itself across clean laps of
//! the same car:
//!
//! | Capture | Clean laps | Corners per lap |
//! |---|---:|---|
//! | `ks_mazda_mx5_cup` | 3 | 13, 10, 13 |
//! | `ks_ferrari_f138` | 3 | 10, 10, 9 |
//!
//! Red Bull Ring GP has ten corners, so on this evidence any single lap is a
//! coin flip. What makes the disagreement tractable is that it is *not shared*.
//! The MX5's spurious detections sit at 241 m (7 deg of net turn), 1345 m (1 deg)
//! and 3905 m (13 deg), and no two laps produce the same one; the ten real
//! corners appear on every lap, with apexes within a few tens of metres of each
//! other.
//!
//! So: detect on every clean lap, keep what several laps independently agree on,
//! discard what only one lap saw. A 1-degree kink caught off a kerb does not
//! recur; Turn 3 does.
//!
//! # How a model is learned (format v2)
//!
//! Three stages, each documented in its own module:
//!
//! 1. **Segmentation** ([`segment`]). Each lap's cumulative rotation θ(s) is
//!    fitted with an MDL-optimal set of piecewise-linear segments. A circuit's
//!    θ(s) *is* piecewise linear — slope is curvature, vertical extent is turn
//!    angle — so this replaces every curvature threshold in the old detector
//!    with a fit whose only scale is the lap's own noise, and cannot cancel a
//!    chicane the way a smoothed |κ| peak-finder can.
//! 2. **Consensus** ([`consensus`]). The per-lap candidate arcs vote through a
//!    ring-aware alignment (a corner may sit anywhere relative to the
//!    start/finish line, including across it), and a corner enters the model
//!    when a Wilson lower bound on its match fraction clears ½. The bound —
//!    not a fixed fraction — is what makes 2-of-2 sufficient and 3-of-4 not:
//!    with few laps, unanimity is required; with many, one noisy lap cannot
//!    veto a corner every other lap drove.
//! 3. **Decision events** ([`decision`]). Brake onsets and releases, throttle
//!    dips and pickups, flat-out direction changes — extracted from each lap's
//!    own pedal trace, assigned to the corner they belong to, and confirmed by
//!    the same Wilson machinery one level down. These are the boundaries a
//!    driver actually decides at, and they are not where the geometric apex is.
//!
//! Geometry is per-field medians over the *representative* laps (those with a
//! typical lap time — an outlier lap still votes on existence but cannot drag
//! an apex). The medoid lap of the set is still computed and recorded, with its
//! spread numbers, purely as an audit handle on how tightly the driver repeated
//! themselves.
//!
//! # This is a per-car artefact, and says so
//!
//! Boundaries shift with speed, not just with the line. The F138 reaches Turn 3
//! carrying so much more speed that it registers as 204 m of corner where the
//! MX5's is 120 m, and the two cars genuinely disagree about whether the
//! 2150-2350 m stretch is one kink or two. Neither is wrong.
//!
//! The model therefore records the car it was learned from in [`Provenance`]
//! rather than pretending to a car-independent truth. Cross-car reconciliation
//! would need captures from several cars and is not attempted here.
//!
//! # Corners straddling the start/finish line
//!
//! The learner works on the ring, so a corner crossing the line is detected as
//! one corner. The on-disk axis is linear (`0..track_length`, and consumers
//! index into it), so such a corner is stored as **two rows**: one from its
//! start to the end of the track, one from zero to its end, the second carrying
//! [`ModelCorner::parent_id`] pointing at the first. The pair shares one
//! identity; the turn angle is apportioned by span.
//!
//! # What is deliberately not in the model
//!
//! * **The reference line itself.** 4,286 grid samples of 17 fields each is
//!   megabytes of JSON, and nothing downstream needs it: the live path needs to
//!   know which corner it is in, and the per-corner personal best is computed
//!   from the driver's own laps at runtime. Keeping the artefact to its corner
//!   list is what makes loading it O(1) against a lap.
//! * **Speeds.** A canonical corner that carried a speed would invite exactly
//!   the cross-car comparison the section above rules out.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::Result;
use crate::core::error::CoachError;
use crate::core::ids::{CornerId, LapId, TrackId};
use crate::core::sample::{SessionInfo, Sim};
use crate::features::consensus::{self, LapStanding};
use crate::features::corner::CornerDirection;
use crate::features::decision::{self, DecisionEvent, DecisionKind, EventWindow};
use crate::features::lap::Lap;
use crate::features::line;
use crate::features::resample::{self, ResampledLap};
use crate::features::segment;
use crate::features::stats;

/// On-disk format version. Bump on any incompatible change to the shape below;
/// [`TrackModel::load`] refuses anything else rather than guessing.
pub const MODEL_VERSION: u32 = 2;

/// How the corner set was produced, recorded in [`Provenance`].
///
/// A model is hand-editable JSON that outlives the binary that wrote it; when
/// the estimator changes what a corner *means*, this string is how an old
/// artefact explains itself.
pub const ESTIMATOR: &str = "theta-mdl + ring-consensus + decision-events";

/// Median sign agreement between the position- and heading-based curvature
/// estimators below which [`TrackModel::learn`] refuses to learn at all.
///
/// The two estimators are independent measurements of the same physical
/// quantity, so sustained disagreement means one channel is dead or lying, and
/// every corner learned after that point would be wrong with confidence. Refuse
/// loudly instead.
const ESTIMATOR_AGREEMENT_MIN: f32 = 0.5;

/// Floor on the lap-time spread, seconds, when classifying laps as
/// representative or atypical. Lap timers resolve finer than this, so a smaller
/// measured spread is timer quantisation, not a genuinely identical field.
const LAP_TIME_SIGMA_FLOOR_S: f32 = 0.1;

/// Fewest clean laps a model can be learned from.
///
/// Two, because the mechanism in the module docs is agreement between laps and
/// one lap cannot agree with anything. Two is the minimum that can vote, not a
/// recommendation — with two laps the Wilson bound demands both.
pub const MIN_LAPS: usize = 2;

/// How this artefact names itself in [`CoachError::BadArtefact`].
const ARTEFACT: &str = "track model";

/// Slack allowed when matching a model's track length against a live session.
///
/// Float noise only. `StaticInfo_TrackSPlineLength` is the same value from the
/// same field on both sides, so any real difference means a different layout,
/// and layouts differ by hundreds of metres rather than by centimetres.
const TRACK_LENGTH_TOLERANCE_M: f32 = 1.0;

/// One corner of the track, as the model believes it to be.
///
/// Distances are metres along the spline from the start/finish line, on the same
/// axis as [`crate::core::sample::Sample::lap_distance`]. A corner straddling
/// the line is two rows joined by [`Self::parent_id`]; see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCorner {
    /// Ordinal from the start/finish line. Renumbered after the consensus vote, so
    /// these are always `0..corners.len()` with no gaps.
    pub id: CornerId,
    pub start_m: f32,
    pub end_m: f32,
    /// Point of highest curvature — the geometric apex.
    pub apex_m: f32,
    /// Point of fastest heading change, usually a little before the apex.
    pub heading_apex_m: f32,
    pub direction: CornerDirection,
    /// Net rotation through the corner, radians. Positive is right.
    pub turn_angle: f32,
    /// Peak smoothed curvature magnitude, 1/m.
    pub peak_curvature: f32,
    /// Laps that independently found this corner, atypical laps included.
    /// Compare against [`TrackModel::lap_count`].
    pub support: u32,
    /// Set on the second row of a corner split across the start/finish line,
    /// pointing at the first row's id. `None` everywhere else.
    #[serde(default)]
    pub parent_id: Option<CornerId>,
    /// `support / laps_seen`: the fraction of laps *this corner was exposed to*
    /// that found it. Differs from `support / lap_count` for corners a late lap
    /// introduced — the laps before it existed never had a chance to vote.
    #[serde(default)]
    pub match_fraction: f32,
    /// Confirmed decision boundaries inside this corner, distance-ordered.
    /// Empty where the pedals never spoke (or never agreed).
    #[serde(default)]
    pub decision_events: Vec<DecisionEvent>,
}

impl ModelCorner {
    pub fn length_m(&self) -> f32 {
        self.end_m - self.start_m
    }

    /// Radius at the apex in metres, or `None` where curvature is unusable.
    pub fn apex_radius_m(&self) -> Option<f32> {
        if self.peak_curvature > 1e-6 {
            Some(1.0 / self.peak_curvature)
        } else {
            None
        }
    }

    /// Turn angle in degrees, unsigned — how a driver would describe it.
    pub fn turn_degrees(&self) -> f32 {
        self.turn_angle.abs().to_degrees()
    }

    /// Whether a lap distance falls inside this corner.
    pub fn contains(&self, distance_m: f32) -> bool {
        distance_m >= self.start_m && distance_m <= self.end_m
    }

    /// Confirmed events of one kind, distance-ordered.
    pub fn events_of(&self, kind: DecisionKind) -> impl Iterator<Item = &DecisionEvent> {
        self.decision_events
            .iter()
            .filter(move |e| e.kind == kind)
    }
}

/// Where a model came from. Recorded so that a surprising corner set can be
/// traced back to the laps that produced it without re-running anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// Car the reference laps were driven in. See the module docs on why this
    /// is part of the artefact and not an incidental detail.
    pub car: String,
    /// File name of the capture, for the audit trail. Not a path: models are
    /// copied between machines and a stale absolute path is worse than none.
    pub capture: String,
    /// Which estimator produced the corner set — see [`ESTIMATOR`].
    #[serde(default)]
    pub estimator: String,
    /// Lap the audit spreads are measured against — the medoid of `lap_ids`.
    /// Geometry itself is per-field medians over the representative laps; the
    /// medoid is the handle for "how tightly did the driver repeat themselves".
    pub reference_lap: LapId,
    /// Every clean lap that voted, reference included.
    pub lap_ids: Vec<LapId>,
    /// Mean separation between the reference line and the other laps, metres,
    /// compared at equal track distance. See [`line`].
    ///
    /// How tightly the driver repeated themselves. A large value means the
    /// reference is representative of very little, and the corner boundaries
    /// should be read as one plausible line rather than *the* line.
    pub reference_spread_m: f32,
    /// Worst single-point separation between the reference and any other lap.
    ///
    /// Persisted alongside the mean because the two answer different questions
    /// and a model that looks wrong is usually wrong in one place, not
    /// everywhere. A 1 m mean with an 11 m maximum is a consistent driver who
    /// had one moment; both numbers are needed to tell that apart from a driver
    /// who wandered all lap.
    pub reference_spread_max_m: f32,
    /// Track distance at which [`Self::reference_spread_max_m`] occurred.
    ///
    /// The single most useful number for debugging a suspicious model on an
    /// unfamiliar circuit: it points at the metre to go and look at.
    pub reference_spread_max_at_m: f32,
    /// Distance-grid spacing the laps were resampled onto.
    pub step_m: f32,
    /// Per-lap curvature noise scale (σ_κ) from Stage 1, one entry per lap in
    /// [`Self::lap_ids`]. A lap with an unusually large value is a noisy
    /// capture segment, and this is the number that coarsened its fit.
    #[serde(default)]
    pub sigma_k_per_lap: Vec<f32>,
    /// Whether any voting lap had usable pedal channels. `false` means the
    /// capture (or sim) published none and every corner's `decision_events`
    /// is empty for that reason rather than because drivers never braked.
    #[serde(default)]
    pub pedal_events: bool,
}

/// Knobs for [`TrackModel::learn`].
///
/// Deliberately just the resampling grid. The old detector's corner thresholds
/// are gone: Stage 1's only scale is the lap's own noise, Stage 2's is the
/// Wilson bound, Stage 3's is each trace's own distribution. Nothing left here
/// to tune per circuit.
#[derive(Debug, Clone)]
pub struct LearnParams {
    /// Distance-grid spacing, metres.
    pub step_m: f32,
}

impl Default for LearnParams {
    fn default() -> Self {
        Self {
            step_m: resample::DEFAULT_STEP_M,
        }
    }
}

/// The canonical corner set for one track and layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackModel {
    pub version: u32,
    pub sim: Sim,
    pub track: TrackId,
    /// Metres, from `StaticInfo_TrackSPlineLength`.
    pub track_length_m: f32,
    /// Ordered by `start_m`, ids sequential from zero.
    pub corners: Vec<ModelCorner>,
    pub provenance: Provenance,
}

impl TrackModel {
    /// Learn a model from a capture's laps.
    ///
    /// Only clean laps are used — [`crate::features::lap::LapQuality::Clean`]
    /// already excludes partial laps, spins, off-track excursions and paused
    /// sim, all of which distort either the line or the geometry.
    pub fn learn(
        session: &SessionInfo,
        laps: &[Lap],
        capture: &str,
        params: &LearnParams,
    ) -> Result<Self> {
        if params.step_m <= 0.0 || !params.step_m.is_finite() {
            return Err(CoachError::implausible(
                "step_m",
                params.step_m,
                "a positive distance in metres",
            ));
        }
        let track_length = session.track_length;

        // Resample first, then count: a clean lap that cannot be put on the grid
        // cannot vote, so the "enough laps" test has to happen after this, not
        // before it.
        let mut grids: Vec<(LapId, f32, ResampledLap)> = Vec::new();
        for lap in laps.iter().filter(|l| l.quality.is_clean()) {
            if let Some(grid) = resample::resample_lap(&lap.samples, params.step_m) {
                grids.push((lap.id, lap.lap_time_s(), grid));
            }
        }

        if grids.len() < MIN_LAPS {
            let clean = laps.iter().filter(|l| l.quality.is_clean()).count();
            return Err(CoachError::NotEnoughData {
                action: "learn a track model",
                detail: format!(
                    "{} clean lap(s) of {} in the capture, {} resampled onto a {} m grid; \
                     at least {MIN_LAPS} are needed for laps to agree on a corner",
                    clean,
                    laps.len(),
                    grids.len(),
                    params.step_m,
                ),
            });
        }

        // --- Preprocessing contract: the channels must agree ------------------
        //
        // Signed curvature from positions and dθ/ds from headings measure the
        // same physical quantity through independent channels. Sustained sign
        // disagreement means one of them is dead or lying — the failure that
        // once produced a "9 Right / 0 Left" model — and no stage downstream
        // can detect it, so it is checked here, once, before anything believes
        // either channel.
        let mut agreements: Vec<f32> = grids
            .iter()
            .map(|(_, _, g)| segment::estimator_agreement(g))
            .collect();
        agreements.sort_by(|a, b| a.total_cmp(b));
        let median_agreement = agreements[agreements.len() / 2];
        if median_agreement < ESTIMATOR_AGREEMENT_MIN {
            return Err(CoachError::NotEnoughData {
                action: "learn a track model",
                detail: format!(
                    "position and heading channels disagree on the direction of turning \
                     (median sign agreement {:.0}%, floor {:.0}%): one channel is dead or \
                     lying, and a model learned from it would be wrong with confidence",
                    median_agreement * 100.0,
                    ESTIMATOR_AGREEMENT_MIN * 100.0,
                ),
            });
        }

        // --- Lap standing ------------------------------------------------------
        //
        // A lap with an atypical lap time still votes on whether a corner
        // exists — it drove the track — but cannot contribute geometry: an
        // outlier lap's boundaries are where its mistakes put them.
        let standings = representative_of(&grids.iter().map(|(_, t, _)| *t).collect::<Vec<_>>());

        // --- Stage 1: segment each lap's θ(s) -----------------------------------
        let segmentations: Vec<segment::Segmentation> = grids
            .iter()
            .map(|(_, _, g)| segment::segment_lap(g, track_length))
            .collect();
        let sigma_k_per_lap: Vec<f32> = segmentations.iter().map(|s| s.sigma_k).collect();

        // --- Stage 2: cross-lap consensus ---------------------------------------
        let mut learner = consensus::ConsensusLearner::new(track_length);
        for (seg, standing) in segmentations.iter().zip(&standings) {
            let observations: Vec<consensus::CornerObservation> =
                seg.arcs.iter().map(consensus::CornerObservation::from_arc).collect();
            learner.add_lap(
                &observations,
                if *standing {
                    LapStanding::Representative
                } else {
                    LapStanding::Atypical
                },
            );
        }
        let confirmed = learner.confirmed();

        // --- Confirmed corners to linear rows -----------------------------------
        let mut corners = corner_rows(&confirmed, track_length);
        if corners.is_empty() {
            return Err(CoachError::NotEnoughData {
                action: "learn a track model",
                detail: format!(
                    "no corner recurred across the {} clean laps with the confidence the \
                     Wilson bound demands; the track is either a straight or the capture \
                     does not describe it",
                    grids.len(),
                ),
            });
        }

        // --- Stage 3: decision events from the pedal traces ---------------------
        let pedal_events = attach_decision_events(&mut corners, &grids);

        // --- Provenance: the medoid lap and its spreads, for audit --------------
        let lines: Vec<&[_]> = grids.iter().map(|(_, _, g)| g.samples.as_slice()).collect();
        let reference_idx =
            line::medoid_lap(&lines, params.step_m).ok_or_else(|| CoachError::NotEnoughData {
                action: "learn a track model",
                detail: "no lap line could be compared against the others".to_string(),
            })?;
        let (spread, spread_max, spread_max_at) = spread_of(&lines, reference_idx, params.step_m);

        Ok(Self {
            version: MODEL_VERSION,
            sim: session.sim,
            track: session.track.clone(),
            track_length_m: track_length,
            corners,
            provenance: Provenance {
                car: session.car.clone(),
                capture: file_name_of(capture),
                estimator: ESTIMATOR.to_string(),
                reference_lap: grids[reference_idx].0,
                lap_ids: grids.iter().map(|(id, _, _)| *id).collect(),
                reference_spread_m: spread,
                reference_spread_max_m: spread_max,
                reference_spread_max_at_m: spread_max_at,
                step_m: params.step_m,
                sigma_k_per_lap,
                pedal_events,
            },
        })
    }

    /// Clean laps that voted on this model.
    pub fn lap_count(&self) -> u32 {
        self.provenance.lap_ids.len() as u32
    }

    /// Refuse a model that was not learned for the session being driven.
    ///
    /// Worth its own method because the failure it catches is silent and total.
    /// A model loaded for the wrong circuit puts every corner boundary in the
    /// wrong place, and nothing downstream can tell: [`Self::corner_at`] returns
    /// a corner, [`Self::next_corner`] returns a corner, and the coach then
    /// talks with complete confidence about Turn 4 while the driver is on a
    /// straight. Every other error in this crate is loud by design; this one
    /// would not be unless something checks.
    ///
    /// Length is checked as well as the identifier because Assetto Corsa ships
    /// several layouts inside one track folder and a mis-set `track_configuration`
    /// is an easy way to get the right name for the wrong circuit. The tolerance
    /// is float noise only — layouts of the same track differ by hundreds of
    /// metres, so anything looser would defeat the check.
    pub fn check_track(&self, track: &TrackId, track_length_m: f32) -> Result<()> {
        if &self.track != track {
            return Err(CoachError::BadArtefact {
                path: Self::file_name(&self.track),
                artefact: ARTEFACT,
                detail: format!("learned for {}, but the session is {track}", self.track),
            });
        }
        if (self.track_length_m - track_length_m).abs() > TRACK_LENGTH_TOLERANCE_M {
            return Err(CoachError::BadArtefact {
                path: Self::file_name(&self.track),
                artefact: ARTEFACT,
                detail: format!(
                    "learned on a {:.0} m track, but the session reports {track_length_m:.0} m \
                     — same name, different layout",
                    self.track_length_m
                ),
            });
        }
        Ok(())
    }

    /// The corner containing a lap distance, if any.
    ///
    /// Linear: a circuit has tens of corners, and a binary search over that is
    /// slower in practice than the scan while being easier to get wrong.
    pub fn corner_at(&self, distance_m: f32) -> Option<&ModelCorner> {
        self.corners.iter().find(|c| c.contains(distance_m))
    }

    /// The next corner at or after a lap distance, wrapping past the line.
    ///
    /// Wrapping matters for the live path: approaching the line on the last
    /// straight, the corner being approached is Turn 1 of the next lap, and a
    /// coach that says nothing there is silent exactly where it is most useful.
    /// `None` only when the model has no corners at all.
    pub fn next_corner(&self, distance_m: f32) -> Option<&ModelCorner> {
        self.corners
            .iter()
            .find(|c| c.start_m > distance_m)
            .or_else(|| self.corners.first())
    }

    /// Corner counts by direction, `(left, right)`.
    pub fn direction_counts(&self) -> (usize, usize) {
        let right = self
            .corners
            .iter()
            .filter(|c| c.direction == CornerDirection::Right)
            .count();
        (self.corners.len() - right, right)
    }

    /// Stable fingerprint of the canonical corner set.
    ///
    /// A reference lap's corner *k* is "corner k" only because this list says
    /// so: re-learning the model can insert, remove or merge detections and
    /// silently shift every ordinal. Anything that stores data keyed by
    /// [`CornerId`] — the personal best does — therefore pins itself to the
    /// exact geometry with this hash and refuses to merge against a model
    /// whose fingerprint differs.
    ///
    /// Hashes each corner's boundaries, apex and direction as raw bit patterns
    /// rather than rounded metres: `serde_json` round-trips `f32` exactly
    /// (shortest-representation output), so a saved-and-reloaded model
    /// fingerprints identically while a genuinely different corner list
    /// collides with probability ~0.
    pub fn fingerprint(&self) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

        fn mix(mut h: u64, bytes: &[u8]) -> u64 {
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(FNV_PRIME);
            }
            h
        }

        let mut hash = mix(FNV_OFFSET, &(self.corners.len() as u64).to_le_bytes());
        for c in &self.corners {
            for value in [
                c.start_m.to_bits(),
                c.end_m.to_bits(),
                c.apex_m.to_bits(),
                match c.direction {
                    CornerDirection::Right => 1,
                    CornerDirection::Left => 0,
                },
            ] {
                hash = mix(hash, &value.to_le_bytes());
            }
        }
        hash
    }

    /// Conventional file name for a track's model: `<track>_<layout>.json`.
    pub fn file_name(track: &TrackId) -> String {
        if track.layout.is_empty() {
            format!("{}.json", track.track)
        } else {
            format!("{}_{}.json", track.track, track.layout)
        }
    }

    /// Path this model belongs at inside a directory of models.
    pub fn path_in(dir: impl AsRef<Path>) -> impl Fn(&TrackId) -> PathBuf {
        let dir = dir.as_ref().to_path_buf();
        move |track| dir.join(Self::file_name(track))
    }

    /// Write the model as JSON, creating parent directories as needed.
    ///
    /// Writes a sibling temporary file and renames it over the target, so an
    /// interrupted save leaves the previous model intact rather than a truncated
    /// file that [`Self::load`] would reject.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let io = |source: std::io::Error| CoachError::Io {
            path: path.display().to_string(),
            source,
        };

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(io)?;
        }

        // `to_string_pretty` on a struct of numbers and short strings cannot
        // fail, but unwrapping here would be the one panic in a save path.
        let json = serde_json::to_string_pretty(self).map_err(|e| CoachError::BadArtefact {
            path: path.display().to_string(),
            artefact: ARTEFACT,
            detail: format!("could not serialise: {e}"),
        })?;

        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json.as_bytes()).map_err(|source| CoachError::Io {
            path: tmp.display().to_string(),
            source,
        })?;
        fs::rename(&tmp, path).map_err(io)
    }

    /// Read a model, refusing anything that violates an invariant the rest of
    /// the pipeline relies on.
    ///
    /// Validation is not defensive padding. A model is hand-editable JSON that
    /// outlives the binary that wrote it, and every check below corresponds to
    /// something a consumer would otherwise discover as nonsense output several
    /// stages later.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| CoachError::Io {
            path: path.display().to_string(),
            source,
        })?;

        let bad = |detail: String| CoachError::BadArtefact {
            path: path.display().to_string(),
            artefact: ARTEFACT,
            detail,
        };

        let model: Self =
            serde_json::from_str(&text).map_err(|e| bad(format!("malformed JSON: {e}")))?;
        model.validate().map_err(bad)?;
        Ok(model)
    }

    /// Check the invariants. `Err` carries the reason, for the caller to wrap.
    fn validate(&self) -> std::result::Result<(), String> {
        if self.version != MODEL_VERSION {
            return Err(format!(
                "format version {}, but this build reads version {MODEL_VERSION}",
                self.version
            ));
        }
        if !self.track_length_m.is_finite() || self.track_length_m <= 0.0 {
            return Err(format!("track length {} m", self.track_length_m));
        }
        if self.track.track.is_empty() {
            return Err("no track name".to_string());
        }
        if self.provenance.lap_ids.is_empty() {
            return Err("no reference laps recorded".to_string());
        }
        if self.provenance.estimator.is_empty() {
            return Err("no estimator recorded".to_string());
        }
        if self.provenance.sigma_k_per_lap.len() != self.provenance.lap_ids.len() {
            return Err(format!(
                "{} per-lap noise entries for {} laps",
                self.provenance.sigma_k_per_lap.len(),
                self.provenance.lap_ids.len()
            ));
        }
        for (i, sigma) in self.provenance.sigma_k_per_lap.iter().enumerate() {
            if !sigma.is_finite() || *sigma <= 0.0 {
                return Err(format!("lap {i} records noise scale {sigma}"));
            }
        }

        let lap_count = self.provenance.lap_ids.len() as u32;
        let mut prev_end = f32::NEG_INFINITY;
        for (i, c) in self.corners.iter().enumerate() {
            let at = format!("corner {} ({})", i, c.id);

            if c.id != CornerId(i as u32) {
                return Err(format!("{at} is out of order: ids must run 0..n"));
            }
            for (name, v) in [
                ("start_m", c.start_m),
                ("end_m", c.end_m),
                ("apex_m", c.apex_m),
                ("heading_apex_m", c.heading_apex_m),
                ("turn_angle", c.turn_angle),
                ("peak_curvature", c.peak_curvature),
                ("match_fraction", c.match_fraction),
            ] {
                if !v.is_finite() {
                    return Err(format!("{at} has non-finite {name}: {v}"));
                }
            }
            if c.start_m >= c.end_m {
                return Err(format!(
                    "{at} spans {} m to {} m, which is empty or reversed",
                    c.start_m, c.end_m
                ));
            }
            if c.start_m < 0.0 || c.end_m > self.track_length_m {
                return Err(format!(
                    "{at} spans {} m to {} m, outside a {} m track",
                    c.start_m, c.end_m, self.track_length_m
                ));
            }
            if !c.contains(c.apex_m) {
                return Err(format!(
                    "{at} has its apex at {} m, outside its own {} m to {} m span",
                    c.apex_m, c.start_m, c.end_m
                ));
            }
            // Overlap would make `corner_at` ambiguous, and the row clamping in
            // `corner_rows` guarantees it cannot happen, so an overlap in a file
            // means the file was edited into an inconsistent state.
            if c.start_m < prev_end {
                return Err(format!(
                    "{at} starts at {} m, inside the previous corner which ends at {prev_end} m",
                    c.start_m
                ));
            }
            if c.support == 0 {
                return Err(format!("{at} claims no supporting laps"));
            }
            if c.support > lap_count {
                return Err(format!(
                    "{at} claims support from {support} laps but only {lap_count} voted",
                    support = c.support
                ));
            }
            if c.match_fraction <= 0.0 || c.match_fraction > 1.0 {
                return Err(format!(
                    "{at} has match fraction {}, which is not a (0, 1] fraction",
                    c.match_fraction
                ));
            }
            // A parent link points at the other half of the same corner. The
            // tail half (late on the axis) carries the *lower* id in practice —
            // the head half starts at 0 m and sorts first — so all that can be
            // checked structurally is that the link resolves and is not self
            // reference.
            if let Some(parent) = c.parent_id {
                if parent == c.id || parent.0 as usize >= self.corners.len() {
                    return Err(format!(
                        "{at} references parent {parent}, which is not another corner in the model"
                    ));
                }
            }
            for e in &c.decision_events {
                if !e.distance_m.is_finite() {
                    return Err(format!("{at} has a non-finite event distance"));
                }
                if e.distance_m < 0.0 || e.distance_m > self.track_length_m {
                    return Err(format!(
                        "{at} has an event at {} m, outside a {} m track",
                        e.distance_m, self.track_length_m
                    ));
                }
                if e.support == 0 || e.support > lap_count {
                    return Err(format!(
                        "{at} has a {:?} event with support {}, but {lap_count} laps voted",
                        e.kind, e.support
                    ));
                }
            }
            prev_end = c.end_m;
        }
        Ok(())
    }
}

/// Which laps are representative by lap time: a robust |z| ≤ 3 against the
/// field's own median and MAD.
///
/// Robust, not parametric, because a short session routinely contains one lap
/// that caught a tow or a moment — exactly the outlier a mean-and-σ test would
/// let drag the threshold onto itself. A field of identical lap times (a
/// synthetic capture) is all-representative: the MAD floors at
/// [`LAP_TIME_SIGMA_FLOOR_S`] rather than dividing by zero.
fn representative_of(times: &[f32]) -> Vec<bool> {
    let centre = stats::median(times);
    let sigma = stats::sigma_from_mad(times, LAP_TIME_SIGMA_FLOOR_S);
    times
        .iter()
        .map(|&t| ((t - centre) / sigma).abs() <= segment::MODIFIED_Z)
        .collect()
}

/// Turn the consensus corners into `ModelCorner` rows on the linear axis.
///
/// The learner works on the ring; this file's consumers (and the on-disk
/// format) work on `0..track_length`. A corner that crosses the start/finish
/// line — `end_m < start_m` — becomes two rows, the second pointing at the
/// first through [`ModelCorner::parent_id`], with the turn angle apportioned
/// by span. Each apex lands in the half that contains it; the other half
/// carries its own midpoint, because a row must still say where its sharpest
/// point is.
///
/// Medians from independent laps can disagree by a few metres at a boundary;
/// rows are clamped apart so the no-overlap invariant [`TrackModel::load`]
/// enforces always holds. A row that clamping squeezes to nothing is dropped —
/// it describes a corner the rows around it already cover.
fn corner_rows(confirmed: &[consensus::ConsensusCorner], track_length_m: f32) -> Vec<ModelCorner> {
    #[derive(Debug, Clone)]
    struct RowDraft {
        start_m: f32,
        end_m: f32,
        apex_m: f32,
        heading_apex_m: f32,
        direction: CornerDirection,
        turn_angle: f32,
        peak_curvature: f32,
        support: u32,
        match_fraction: f32,
        /// Index into the pre-sort row list of the first half of a
        /// line-straddling corner.
        parent: Option<usize>,
    }

    let mut rows: Vec<RowDraft> = Vec::new();
    let contains = |v: f32, lo: f32, hi: f32| v >= lo && v <= hi;

    for c in confirmed {
        if c.end_m >= c.start_m {
            rows.push(RowDraft {
                start_m: c.start_m,
                end_m: c.end_m,
                apex_m: c.apex_m,
                heading_apex_m: c.heading_apex_m,
                direction: c.direction,
                turn_angle: c.turn_angle,
                peak_curvature: c.peak_curvature,
                support: c.support,
                match_fraction: c.match_fraction,
                parent: None,
            });
            continue;
        }

        // Straddles the line: [start, L] then [0, end], turn apportioned by
        // how much of the span sits in each half.
        let span = (c.end_m - c.start_m).rem_euclid(track_length_m);
        let tail = (track_length_m - c.start_m).max(0.0);
        let head = c.end_m.max(0.0);
        let tail_share = if span > 0.0 { tail / span } else { 0.5 };
        let tail_mid = c.start_m + tail / 2.0;
        let head_mid = head / 2.0;

        let parent = rows.len();
        rows.push(RowDraft {
            start_m: c.start_m,
            end_m: track_length_m,
            apex_m: if contains(c.apex_m, c.start_m, track_length_m) {
                c.apex_m
            } else {
                tail_mid
            },
            heading_apex_m: if contains(c.heading_apex_m, c.start_m, track_length_m) {
                c.heading_apex_m
            } else {
                tail_mid
            },
            direction: c.direction,
            turn_angle: c.turn_angle * tail_share,
            peak_curvature: c.peak_curvature,
            support: c.support,
            match_fraction: c.match_fraction,
            parent: None,
        });
        rows.push(RowDraft {
            start_m: 0.0,
            end_m: head,
            apex_m: if contains(c.apex_m, 0.0, head) {
                c.apex_m
            } else {
                head_mid
            },
            heading_apex_m: if contains(c.heading_apex_m, 0.0, head) {
                c.heading_apex_m
            } else {
                head_mid
            },
            direction: c.direction,
            turn_angle: c.turn_angle * (1.0 - tail_share),
            peak_curvature: c.peak_curvature,
            support: c.support,
            match_fraction: c.match_fraction,
            parent: Some(parent),
        });
    }

    // Sort by start, clamp overlaps away, drop rows squeezed to nothing.
    let total = rows.len();
    let mut indexed: Vec<(usize, RowDraft)> = rows.into_iter().enumerate().collect();
    indexed.sort_by(|(_, a), (_, b)| a.start_m.total_cmp(&b.start_m));

    let mut kept: Vec<(usize, RowDraft)> = Vec::new();
    let mut prev_end = f32::NEG_INFINITY;
    for (orig, mut row) in indexed {
        if row.start_m < prev_end {
            row.start_m = prev_end;
        }
        if row.end_m - row.start_m <= 0.0 {
            continue;
        }
        // Clamping may have pushed the apexes out of the row; a corner's
        // identity is its position, so they follow the row rather than
        // dangling outside it.
        row.apex_m = row.apex_m.clamp(row.start_m, row.end_m);
        row.heading_apex_m = row.heading_apex_m.clamp(row.start_m, row.end_m);
        prev_end = row.end_m;
        kept.push((orig, row));
    }

    // Original row index -> final position, so parent links survive the sort
    // and any drops. `usize::MAX` marks a dropped row, and a parent that was
    // squeezed away has nothing left to reference.
    let mut new_index = vec![usize::MAX; total];
    for (new, (orig, _)) in kept.iter().enumerate() {
        new_index[*orig] = new;
    }

    let mut corners = Vec::with_capacity(kept.len());
    for (_, row) in kept {
        let parent_id = row
            .parent
            .filter(|p| new_index[*p] != usize::MAX)
            .map(|p| CornerId(new_index[p] as u32));
        corners.push(ModelCorner {
            id: CornerId(0), // renumbered below
            start_m: row.start_m,
            end_m: row.end_m,
            apex_m: row.apex_m,
            heading_apex_m: row.heading_apex_m,
            direction: row.direction,
            turn_angle: row.turn_angle,
            peak_curvature: row.peak_curvature,
            support: row.support,
            parent_id,
            match_fraction: row.match_fraction,
            decision_events: Vec::new(),
        });
    }
    // The vote can remove corners from the middle of the list, so the ordinals
    // must run 0..n with no gaps — drivers count turns from the line that way,
    // and so does everything downstream.
    for (i, c) in corners.iter_mut().enumerate() {
        c.id = CornerId(i as u32);
    }
    corners
}

/// Stage 3: extract each lap's pedal events, assign them to the corner rows,
/// and keep the ones that recur across laps with Wilson confidence.
///
/// Laps whose pedal channels never move are excluded from Stage 3 entirely —
/// they never braked *on the record*, which is not the same statement as
/// braked, and must not count as votes against. Returns whether any voting lap
/// had usable pedal channels at all, for [`Provenance::pedal_events`].
fn attach_decision_events(
    corners: &mut [ModelCorner],
    grids: &[(LapId, f32, ResampledLap)],
) -> bool {
    let windows: Vec<EventWindow> = corners
        .iter()
        .map(|c| EventWindow {
            start_m: c.start_m,
            end_m: c.end_m,
        })
        .collect();

    // Per voting lap: that lap's events, split per corner row.
    let mut assigned: Vec<Vec<Vec<decision::LapEvent>>> = Vec::new();
    let mut any_live = false;
    for (_, _, grid) in grids {
        let levels = decision::pedal_levels(grid);
        if !levels.pedals_live(grid) {
            continue;
        }
        any_live = true;
        let events = decision::lap_events(grid, &levels);
        assigned.push(decision::assign_events(&events, &windows));
    }

    let kinds = [
        DecisionKind::BrakeOnset,
        DecisionKind::BrakeRelease,
        DecisionKind::ThrottleDip,
        DecisionKind::ThrottlePickup,
        DecisionKind::FlatDirectionChange,
    ];

    for (ci, corner) in corners.iter_mut().enumerate() {
        for kind in kinds {
            let per_lap: Vec<Vec<f32>> = assigned
                .iter()
                .map(|per_arc| {
                    per_arc[ci]
                        .iter()
                        .filter(|e| e.kind == kind)
                        .map(|e| e.distance_m)
                        .collect()
                })
                .collect();
            corner.decision_events.extend(decision::confirm_events(kind, &per_lap));
        }
        corner
            .decision_events
            .sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    }
    any_live
}

/// Mean separation from one line to all the others, metres.
///
/// Recomputed rather than threaded out of [`line::medoid_lap`]: the pairwise
/// pass is `O(n)` per pair now, so running it twice is cheaper than the plumbing
/// needed to avoid it.
fn spread_of(lines: &[&[crate::core::sample::Sample]], idx: usize, step_m: f32) -> (f32, f32, f32) {
    let mean = line::mean_separations(lines, step_m)
        .get(idx)
        .copied()
        .unwrap_or(f32::INFINITY);

    // Worst single point across the pairs this line takes part in, and where.
    let mut max_m = 0.0f32;
    let mut max_at_m = 0.0f32;
    for (other, l) in lines.iter().enumerate() {
        if other == idx {
            continue;
        }
        if let Some(s) = line::separation(lines[idx], l, step_m) {
            if s.max_m > max_m {
                max_m = s.max_m;
                max_at_m = s.max_at_m;
            }
        }
    }
    (mean, max_m, max_at_m)
}

/// Last path component, for the provenance record.
fn file_name_of(capture: &str) -> String {
    Path::new(capture)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| capture.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::math::wrap_pi;
    use crate::core::sample::Sample;
    use crate::features::lap::LapQuality;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique-within-this-process id for tests that share a temp directory.
    /// The directory key (`coach_track_model_validate`) is fixed, so without
    /// this counter parallel tests race on the same file.
    fn next_id() -> u32 {
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// Build a lap from a curvature programme of `(length_m, signed_curvature)`
    /// segments, integrated into a path on a 1 m grid.
    ///
    /// The path is built to agree with the crate's sign convention — positive
    /// curvature is right *and* yields a positive ground-plane cross product —
    /// so the dual-estimator canary in [`segment::estimator_agreement`] passes
    /// on it. (A naive `x += heading.sin()` integration produces a mirror-image
    /// path whose Menger curvature has the opposite sign; the canary exists
    /// precisely to catch channels that disagree like that.)
    fn lap_from_curvature(id: u32, program: &[(f32, f32)]) -> Lap {
        let mut samples = Vec::new();
        let (mut heading, mut x, mut z, mut d) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);

        for (length, k) in program {
            for _ in 0..(*length as usize) {
                // Left-handed X/Z: positive curvature turns right.
                heading = wrap_pi(heading + k);
                x -= heading.sin();
                z += heading.cos();
                d += 1.0;
                samples.push(Sample {
                    t_ms: (d * 33.0) as i64,
                    lap_distance: d,
                    lap_frac: d / 4286.0,
                    pos: [x, 0.0, z],
                    heading,
                    speed: if k.abs() > 0.001 { 25.0 } else { 60.0 },
                    throttle: 1.0,
                    brake: 0.0,
                    steer: 0.0,
                    yaw_rate: 0.0,
                    slip_angle: 0.0,
                    gear: 4,
                    rpm: 6000.0,
                    tyres_out: 0,
                    live: true,
                    surface_grip: 1.0,
                    lap_time_ms: (d * 33.0) as i32,
                });
            }
        }

        Lap {
            id: LapId(id),
            samples,
            quality: LapQuality::Clean,
            coverage: 1.0,
            net_rotation: std::f32::consts::TAU,
            off_track_frames: 0,
            not_live_frames: 0,
            ac_lap_time_ms: Some((d * 33.0) as i32),
            wall_duration_ms: (d * 33.0) as i64,
        }
    }

    fn right_90() -> (f32, f32) {
        (78.5, 1.0 / 50.0)
    }

    fn left_90() -> (f32, f32) {
        (78.5, -1.0 / 50.0)
    }

    /// A weak, short kink — the shape of the MX5's spurious 1-degree detections.
    fn kink() -> (f32, f32) {
        (40.0, 1.0 / 180.0)
    }

    /// A session for the synthetic laps below.
    ///
    /// Deliberately not named after a real circuit. The laps in these tests are
    /// hand-built curvature programmes a few hundred metres long and share no
    /// geometry with any track that exists, so borrowing a real track's name
    /// would suggest these assertions were measured somewhere they were not.
    fn session() -> SessionInfo {
        SessionInfo {
            sim: Sim::AssettoCorsa,
            track: TrackId::new("test_circuit", "layout_a"),
            car: "test_car".to_string(),
            // Longer than every synthetic lap below (including the 1312 m
            // closed ring in the seam test), so the `next_corner` wrap test
            // has straight to sit on past the last corner.
            track_length: 1500.0,
            sector_count: 3,
            ac_version: "1.16.3".to_string(),
            sm_version: "1.7".to_string(),
        }
    }

    #[test]
    fn a_corner_only_one_lap_saw_is_not_in_the_model() {
        // Three laps of the same circuit. Only the first has the kink, and it is
        // the shape of the MX5's 1-degree phantom at 1345 m.
        let plain = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let with_kink = &[
            (300.0, 0.0),
            right_90(),
            (150.0, 0.0),
            kink(),
            (150.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];

        let laps = vec![
            lap_from_curvature(0, with_kink),
            lap_from_curvature(1, plain),
            lap_from_curvature(2, plain),
        ];

        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        assert_eq!(
            model.corners.len(),
            2,
            "the kink only one lap saw should have been voted out, got {:#?}",
            model.corners
        );
        assert!(model.corners.iter().all(|c| c.support >= 2));
    }

    #[test]
    fn corners_every_lap_agrees_on_survive_with_full_support() {
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();

        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        assert_eq!(model.corners.len(), 2);
        assert_eq!(model.direction_counts(), (1, 1), "one left, one right");
        for c in &model.corners {
            assert_eq!(c.support, 3, "every lap drove this corner");
            assert!(
                (c.match_fraction - 1.0).abs() < 1e-6,
                "unanimous corners match every lap they were exposed to"
            );
            assert_eq!(c.parent_id, None);
        }
        // Ids are renumbered from the line with no gaps.
        assert_eq!(model.corners[0].id, CornerId(0));
        assert_eq!(model.corners[1].id, CornerId(1));
    }

    #[test]
    fn ids_are_renumbered_after_the_vote_removes_a_middle_corner() {
        // The phantom sits *between* two real corners, so if renumbering were
        // skipped the surviving ids would be 0 and 2. Four of five laps carry
        // it — enough for the Wilson bound (4/5 lower bound 0.51 > 0.5), while
        // the fifth lap's miss is what the third lap of three could not
        // survive (2/3 lower bound 0.32 < 0.5).
        let with_kink = &[
            (300.0, 0.0),
            right_90(),
            (150.0, 0.0),
            kink(),
            (150.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let plain = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];

        let laps: Vec<Lap> = (0..5)
            .map(|i| lap_from_curvature(i, if i < 4 { with_kink } else { plain }))
            .collect();
        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("five clean laps");

        assert_eq!(
            model.corners.len(),
            3,
            "the kink survived 4-of-5: {:#?}",
            model.corners
        );
        for (i, c) in model.corners.iter().enumerate() {
            assert_eq!(c.id, CornerId(i as u32));
        }
    }

    #[test]
    fn one_lap_is_refused_rather_than_believed() {
        let laps = vec![lap_from_curvature(
            0,
            &[(300.0, 0.0), right_90(), (300.0, 0.0)],
        )];
        let err = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect_err("one lap cannot agree with anything");
        assert!(
            matches!(err, CoachError::NotEnoughData { .. }),
            "expected NotEnoughData, got {err}"
        );
    }

    #[test]
    fn unclean_laps_do_not_vote() {
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0)];
        let mut laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();
        laps[1].quality = LapQuality::Spun;
        laps[2].quality = LapQuality::OffTrack;

        let err = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect_err("only one clean lap remains");
        assert!(matches!(err, CoachError::NotEnoughData { .. }), "got {err}");
    }

    /// A closed 1500 m ring whose corner halves straddle the sample boundary:
    /// integer segment lengths with k = 2π/312, so the ring closes exactly and
    /// the start/finish gap carries exactly one grid step of rotation. See the
    /// seam test in `segment` for why the lengths must be grid multiples.
    fn seam_program() -> Vec<(f32, f32)> {
        let k = std::f32::consts::TAU / 312.0;
        vec![
            (39.0, k),   // A: second half of the seam corner
            (297.0, 0.0),
            (78.0, k),
            (297.0, 0.0),
            (78.0, k),
            (297.0, 0.0),
            (78.0, k),
            (297.0, 0.0),
            (39.0, k),   // G: first half of the seam corner
        ]
    }

    #[test]
    fn a_corner_straddling_the_line_is_two_rows_sharing_a_parent() {
        // Total 1500 m exactly — the session's length, so the ring closes on
        // the linear axis with no gap.
        let program = seam_program();
        assert_eq!(program.iter().map(|(l, _)| *l).sum::<f32>(), 1500.0);
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, &program)).collect();

        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        // Three 90s plus the seam corner, which must be two rows.
        let parents: Vec<&ModelCorner> = model
            .corners
            .iter()
            .filter(|c| c.parent_id.is_some())
            .collect();
        assert_eq!(
            parents.len(),
            1,
            "exactly one row is the second half of a seam corner: {:#?}",
            model.corners
        );
        let head = parents[0];
        let tail = model
            .corners
            .iter()
            .find(|c| Some(c.id) == head.parent_id)
            .expect("the parent link resolves");

        // The two halves partition the line: the tail runs to the end of the
        // track, the head starts at zero.
        assert_eq!(tail.end_m, model.track_length_m);
        assert_eq!(head.start_m, 0.0);
        assert!(head.end_m > 0.0 && head.end_m < 100.0);
        assert!(tail.start_m > 1400.0 && tail.start_m < model.track_length_m);
        // Turn angle is apportioned by span, and the two halves sum to the
        // corner's ~90°.
        let total = tail.turn_angle + head.turn_angle;
        assert!(
            (total - std::f32::consts::FRAC_PI_2).abs() < 0.15,
            "halves sum to {total} rad, expected ~π/2"
        );
        // And the model still has exactly four corners' worth of rows.
        assert_eq!(model.corners.len(), 5);
    }

    #[test]
    fn decision_events_come_from_the_pedal_trace() {
        // Three laps of the right-then-left circuit, all braking from 240 m —
        // 60 m before the right-hander — and back on the throttle at 320 m.
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let mut laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();
        for lap in &mut laps {
            for s in &mut lap.samples {
                if s.lap_distance >= 240.0 && s.lap_distance < 320.0 {
                    s.brake = 0.8;
                    s.throttle = 0.1;
                }
            }
        }

        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");
        assert!(model.provenance.pedal_events, "the pedals were live");

        let right = &model.corners[0];
        let onset: Vec<&crate::features::decision::DecisionEvent> = right
            .events_of(crate::features::decision::DecisionKind::BrakeOnset)
            .collect();
        assert_eq!(onset.len(), 1, "one braking zone, one onset");
        assert_eq!(onset[0].support, 3, "every lap braked here");
        assert!(
            (onset[0].distance_m - 240.0).abs() < 5.0,
            "onset at {} m, expected ~240",
            onset[0].distance_m
        );
        assert!(
            onset[0].distance_m < right.start_m,
            "a brake onset belongs to the corner but happens before it"
        );

        // The release is confirmed too, still inside the same corner's events.
        assert_eq!(
            right
                .events_of(crate::features::decision::DecisionKind::BrakeRelease)
                .count(),
            1
        );
        // The left-hander was driven flat: no events of its own.
        assert!(
            model.corners[1].decision_events.is_empty(),
            "a corner driven flat has no decision boundaries"
        );
    }

    #[test]
    fn dead_pedal_channels_are_reported_not_guessed_from() {
        // Same circuit, but the capture publishes constant pedals: no events
        // may be invented, and the model must say why.
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();

        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");
        assert!(!model.provenance.pedal_events);
        assert!(
            model
                .corners
                .iter()
                .all(|c| c.decision_events.is_empty()),
            "no pedal trace, no decision events"
        );
    }

    #[test]
    fn a_lying_heading_channel_is_refused_loudly() {
        // Same three laps, but the heading channel is mirrored: positions say
        // right where headings say left. The estimator canary must refuse the
        // whole learn rather than emit a model with every direction flipped.
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let mut laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();
        for lap in &mut laps {
            for s in &mut lap.samples {
                s.heading = wrap_pi(-s.heading);
            }
        }

        let err = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect_err("the channels disagree about turning");
        assert!(
            matches!(err, CoachError::NotEnoughData { .. }),
            "expected a loud refusal, got {err}"
        );
    }

    #[test]
    fn representative_laps_are_classified_robustly() {
        // Identical times: all representative (the MAD floors rather than
        // dividing by zero).
        assert!(representative_of(&[90.0, 90.0, 90.0])
            .iter()
            .all(|&r| r));

        // One lap 12 s off a 90 s field: the MAD ignores it, so it alone is
        // atypical.
        let flags = representative_of(&[90.0, 90.2, 90.1, 102.0]);
        assert_eq!(flags, vec![true, true, true, false]);

        // A wild field (MAD ~ 8 s) swallows a 12 s deviation: still
        // representative, because relative to that field it is.
        let flags = representative_of(&[90.0, 98.0, 82.0, 102.0]);
        assert!(flags.iter().all(|&r| r), "got {flags:?}");
    }

    #[test]
    fn corner_lookup_finds_the_containing_corner_and_the_next_one() {
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();
        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        let first = &model.corners[0];
        let second = &model.corners[1];

        assert_eq!(
            model.corner_at(first.apex_m).map(|c| c.id),
            Some(first.id),
            "the apex is inside its own corner"
        );
        assert!(
            model.corner_at(10.0).is_none(),
            "the first 10 m is a straight"
        );

        assert_eq!(model.next_corner(0.0).map(|c| c.id), Some(first.id));
        assert_eq!(
            model.next_corner(first.end_m).map(|c| c.id),
            Some(second.id)
        );
        // Past the last corner, the next one is the first of the following lap.
        assert_eq!(
            model.next_corner(model.track_length_m - 1.0).map(|c| c.id),
            Some(first.id),
            "next_corner must wrap past the start/finish line"
        );
    }

    #[test]
    fn a_saved_model_reloads_identically() {
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();
        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        let dir = std::env::temp_dir().join("coach_track_model_roundtrip");
        let path = dir.join(TrackModel::file_name(&model.track));
        let _ = fs::remove_dir_all(&dir);

        model.save(&path).expect("save");
        let back = TrackModel::load(&path).expect("load");

        assert_eq!(back.track, model.track);
        assert_eq!(back.corners.len(), model.corners.len());
        assert_eq!(
            back.provenance.reference_lap,
            model.provenance.reference_lap
        );
        assert_eq!(back.provenance.car, model.provenance.car);
        assert_eq!(back.provenance.estimator, model.provenance.estimator);
        assert_eq!(
            back.provenance.sigma_k_per_lap,
            model.provenance.sigma_k_per_lap
        );
        for (a, b) in back.corners.iter().zip(&model.corners) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.start_m, b.start_m);
            assert_eq!(a.apex_m, b.apex_m);
            assert_eq!(a.direction, b.direction);
            assert_eq!(a.support, b.support);
            assert_eq!(a.parent_id, b.parent_id);
            assert_eq!(a.match_fraction, b.match_fraction);
            assert_eq!(a.decision_events.len(), b.decision_events.len());
        }
        // Saving into a directory that does not exist yet must work.
        assert!(path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_name_follows_the_track_and_layout() {
        assert_eq!(
            TrackModel::file_name(&TrackId::new("ks_red_bull_ring", "layout_gp")),
            "ks_red_bull_ring_layout_gp.json"
        );
        // AC leaves the configuration empty for tracks that ship one layout.
        assert_eq!(
            TrackModel::file_name(&TrackId::new("magione", "")),
            "magione.json"
        );
    }

    #[test]
    fn the_fingerprint_survives_a_save_load_roundtrip() {
        let model = learned();

        let dir = std::env::temp_dir().join("coach_track_model_fingerprint");
        let path = dir.join(TrackModel::file_name(&model.track));
        let _ = fs::remove_dir_all(&dir);

        model.save(&path).expect("save");
        let back = TrackModel::load(&path).expect("load");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(
            model.fingerprint(),
            back.fingerprint(),
            "serde_json must round-trip f32 exactly or every stored reference goes stale"
        );
    }

    #[test]
    fn the_fingerprint_changes_when_the_corner_set_changes() {
        let model = learned();
        let same = model.clone();
        assert_eq!(model.fingerprint(), same.fingerprint());

        // One boundary moved by a metre: a different corner set, and any
        // reference keyed to the old ordinals is now describing somewhere
        // else. The hash must notice.
        let mut shifted = model.clone();
        shifted.corners[0].end_m += 1.0;
        assert_ne!(model.fingerprint(), shifted.fingerprint());

        // A corner removed entirely: same.
        let mut shorter = model.clone();
        shorter.corners.remove(0);
        for (i, c) in shorter.corners.iter_mut().enumerate() {
            c.id = CornerId(i as u32);
        }
        assert_ne!(model.fingerprint(), shorter.fingerprint());
    }

    /// Round-trip a model through JSON after mutating it, to check `load`'s
    /// validation rather than `save`'s output.
    /// A model learned from three identical laps of a right-then-left circuit.
    fn learned() -> TrackModel {
        let program = &[
            (300.0, 0.0),
            right_90(),
            (300.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();
        TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps")
    }

    fn load_mutated(mutate: impl FnOnce(&mut serde_json::Value)) -> Result<TrackModel> {
        let model = learned();

        let mut json = serde_json::to_value(&model).expect("serialise");
        mutate(&mut json);

        let dir = std::env::temp_dir().join("coach_track_model_validate");
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("m{}_{}.json", std::process::id(), next_id()));
        fs::write(&path, serde_json::to_string(&json).unwrap()).expect("write");
        let result = TrackModel::load(&path);
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn load_refuses_a_model_from_a_future_version() {
        let err = load_mutated(|j| j["version"] = serde_json::json!(MODEL_VERSION + 1))
            .expect_err("version mismatch");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_a_corner_outside_the_track() {
        let err = load_mutated(|j| j["corners"][0]["end_m"] = serde_json::json!(99_999.0))
            .expect_err("corner past the end of the track");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_an_apex_outside_its_own_corner() {
        let err = load_mutated(|j| j["corners"][0]["apex_m"] = serde_json::json!(0.0))
            .expect_err("apex outside the span");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_a_non_finite_distance() {
        let err = load_mutated(|j| j["corners"][0]["start_m"] = serde_json::json!(f32::NAN))
            .expect_err("NaN start");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_corners_that_overlap() {
        // Drag the second corner's start back behind the first one's end.
        let err = load_mutated(|j| {
            let first_end = j["corners"][0]["end_m"].as_f64().unwrap();
            j["corners"][1]["start_m"] = serde_json::json!(first_end - 10.0);
        })
        .expect_err("overlapping corners make corner_at ambiguous");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_gappy_corner_ids() {
        let err = load_mutated(|j| j["corners"][1]["id"] = serde_json::json!(7))
            .expect_err("ids must run 0..n");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn check_track_accepts_the_track_it_was_learned_on() {
        let s = session();
        let model = learned();
        model
            .check_track(&s.track, s.track_length)
            .expect("the model was learned from this very session");
    }

    #[test]
    fn check_track_refuses_a_different_track() {
        let model = learned();
        let err = model
            .check_track(&TrackId::new("other_circuit", "layout_a"), 1500.0)
            .expect_err("a model for one circuit must not be used on another");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn check_track_refuses_a_different_layout_of_the_same_track() {
        // The trap this guards: AC ships several layouts in one track folder, so
        // the name can match while the circuit does not.
        let model = learned();
        let err = model
            .check_track(&TrackId::new("test_circuit", "layout_b"), 1500.0)
            .expect_err("layout is part of the identity");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn check_track_refuses_a_length_that_disagrees() {
        let s = session();
        let model = learned();
        let err = model
            .check_track(&s.track, s.track_length + 400.0)
            .expect_err("same name, different length means a different layout");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn check_track_tolerates_float_noise_in_the_length() {
        let s = session();
        let model = learned();
        model
            .check_track(&s.track, s.track_length + 0.25)
            .expect("a quarter-metre is noise, not a different circuit");
    }

    #[test]
    fn load_refuses_malformed_json() {
        let dir = std::env::temp_dir().join("coach_track_model_malformed");
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("broken.json");
        fs::write(&path, b"{not json").expect("write");
        let err = TrackModel::load(&path).expect_err("malformed");
        let _ = fs::remove_file(&path);
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }
}
