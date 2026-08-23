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
//! # Why the reference lap is the medoid, and what it is used for
//!
//! Corner *existence* is decided by the vote above. Corner *geometry* — the
//! boundaries, the apex, the radius — is taken wholesale from one lap, the
//! [`line`] medoid of the clean set: the lap with the lowest mean separation
//! from all the others, compared at equal track distance.
//!
//! Taking geometry from a single coherent lap rather than averaging across laps
//! is deliberate. Boundaries wander: the MX5 places the fast kink after Turn 3
//! at 2164 m, 2205 m and 2235 m on its three laps, a 71 m spread. A median of
//! those is a number no lap actually drove, and averaging start against end
//! independently can produce a corner shorter than either input or one that
//! overlaps its neighbour. One lap's numbers are at least mutually consistent.
//!
//! The medoid rather than the fastest lap because the fastest lap of a short
//! session is routinely an outlier that caught a tow or clipped a kerb, whereas
//! the medoid is by construction the lap closest to all the others. See the
//! [`line`] module docs for why that comparison is a single distance-aligned
//! pass rather than the Fréchet distance this originally used.
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
//! # What is deliberately not in the model
//!
//! * **The reference line itself.** 4,286 grid samples of 17 fields each is
//!   megabytes of JSON, and nothing downstream needs it: the live path needs to
//!   know which corner it is in, and the per-corner personal best is computed
//!   from the driver's own laps at runtime. Keeping the artefact to its corner
//!   list is what makes loading it O(1) against a lap.
//! * **Speeds.** [`TrackCorner::min_speed`] is a fact about a car on a lap, not
//!   about a corner, and the MX5/F138 figures for the same corner differ by 40%.
//!   A canonical corner that carried a speed would invite exactly the
//!   cross-car comparison the section above rules out.
//!
//! # Known limitation: corners straddling the start/finish line
//!
//! Detection runs over one lap's distance axis, `0..track_length`, so a corner
//! that crosses the line is seen as two — one ending at the axis end, one
//! starting at its beginning. Neither Red Bull Ring layout has one, so this is
//! untested rather than handled. [`TrackModel::next_corner`] wraps correctly
//! regardless, because the live path needs that whether or not a corner does.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::Result;
use crate::core::error::CoachError;
use crate::core::ids::{CornerId, LapId, TrackId};
use crate::core::sample::{SessionInfo, Sim};
use crate::features::corner::{self, CornerDirection, CornerParams, TrackCorner};
use crate::features::line;
use crate::features::lap::Lap;
use crate::features::resample::{self, ResampledLap};

/// On-disk format version. Bump on any incompatible change to the shape below;
/// [`TrackModel::load`] refuses anything else rather than guessing.
pub const MODEL_VERSION: u32 = 1;

/// Fewest clean laps a model can be learned from.
///
/// Two, because the mechanism in the module docs is agreement between laps and
/// one lap cannot agree with anything. Two is the minimum that can vote, not a
/// recommendation — with two laps [`LearnParams::min_support`] demands both.
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
/// axis as [`crate::core::sample::Sample::lap_distance`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCorner {
    /// Ordinal from the start/finish line. Renumbered after the support vote, so
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
    /// Clean laps that independently detected this corner, including the
    /// reference lap. Compare against [`TrackModel::lap_count`].
    pub support: u32,
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

    /// Build from a reference-lap detection, dropping the per-lap fields.
    fn from_detection(detected: &TrackCorner, support: u32) -> Self {
        Self {
            id: detected.id,
            start_m: detected.start_m,
            end_m: detected.end_m,
            apex_m: detected.apex_m,
            heading_apex_m: detected.heading_apex_m,
            direction: detected.direction,
            turn_angle: detected.turn_angle,
            peak_curvature: detected.peak_curvature,
            support,
        }
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
    /// Lap the geometry was taken from — the medoid of `lap_ids`.
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
}

/// Knobs for [`TrackModel::learn`].
#[derive(Debug, Clone)]
pub struct LearnParams {
    /// Distance-grid spacing, metres.
    pub step_m: f32,
    /// Passed through to the per-lap detector.
    pub corner: CornerParams,
    /// Fraction of clean laps that must independently detect a corner for it to
    /// enter the model.
    ///
    /// A fraction rather than a count, so it means the same thing on a three-lap
    /// capture as on a thirty-lap one. The floor is two laps regardless: one lap
    /// agreeing with itself is what this whole module exists to stop.
    ///
    /// Note what this can and cannot do. A vote removes detections that only one
    /// lap made — on the reference captures it dropped four such, two per car.
    /// It cannot recover a corner the detector never proposed on any lap, because
    /// every lap runs the same detector with the same thresholds and will miss it
    /// identically. Corners lost to [`CornerParams`] being wrong for a circuit
    /// have to be fixed there, not here.
    pub min_support: f32,
    /// How far from a candidate corner's span another lap's apex may fall and
    /// still count as the same corner. **Tuning knob**, and an absolute distance
    /// — the one thing in this module that does not scale with the circuit.
    ///
    /// Set to the same order as [`CornerParams::merge_gap_m`]: beyond that
    /// distance the two detections would have been separate corners even within
    /// one lap, so calling them the same corner across laps would be
    /// inconsistent. That gap was itself tuned on Red Bull Ring, so treat the
    /// default as a starting value on a circuit whose corners are packed
    /// tighter — Monaco and the Nordschleife both have corners closer together
    /// than 25 m, where a generous window makes neighbouring corners compete
    /// for the same votes.
    ///
    /// Making it relative to corner length was tried and rejected: the measured
    /// apex wander runs the wrong way for that. On the reference captures a 30 m
    /// corner wandered 46 m across laps while a 236 m corner wandered 12 m, so a
    /// length-proportional rule would be a worse guess wearing the costume of a
    /// principle. It stays absolute and visible instead — `--apex-tolerance` on
    /// the command line, so a tight circuit needs a flag rather than a rebuild.
    ///
    /// The damage is bounded either way. [`nearest_candidate`] gives each
    /// detection to at most one candidate, so an over-wide window costs a corner
    /// some support rather than inventing support it never had, and the `laps`
    /// column in `coach learn-track` shows it happening.
    pub apex_tolerance_m: f32,
}

impl Default for LearnParams {
    fn default() -> Self {
        Self {
            step_m: resample::DEFAULT_STEP_M,
            corner: CornerParams::default(),
            min_support: 0.5,
            apex_tolerance_m: 25.0,
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

        // Resample first, then count: a clean lap that cannot be put on the grid
        // cannot vote, so the "enough laps" test has to happen after this, not
        // before it.
        let mut grids: Vec<(LapId, ResampledLap)> = Vec::new();
        for lap in laps.iter().filter(|l| l.quality.is_clean()) {
            if let Some(grid) = resample::resample_lap(&lap.samples, params.step_m) {
                grids.push((lap.id, grid));
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

        // Medoid over the lines, which fixes the reference lap.
        let lines: Vec<&[_]> = grids.iter().map(|(_, g)| g.samples.as_slice()).collect();
        let reference_idx = line::medoid_lap(&lines, params.step_m).ok_or_else(|| {
            CoachError::NotEnoughData {
                action: "learn a track model",
                detail: "no lap line could be compared against the others".to_string(),
            }
        })?;

        let detections: Vec<Vec<TrackCorner>> = grids
            .iter()
            .map(|(_, g)| corner::detect_corners_with(g, &params.corner))
            .collect();

        let lap_count = grids.len() as u32;
        let required = required_support(lap_count, params.min_support);
        let candidates = &detections[reference_idx];
        let support = support_counts(candidates, &detections, params.apex_tolerance_m);

        let mut corners: Vec<ModelCorner> = candidates
            .iter()
            .zip(&support)
            .map(|(candidate, support)| ModelCorner::from_detection(candidate, *support))
            .filter(|c| c.support >= required)
            .collect();

        // The vote removes corners from the middle of the list, so the ordinals
        // the detector assigned no longer run 0..n. Drivers count turns from the
        // line with no gaps, and so does everything downstream.
        for (i, c) in corners.iter_mut().enumerate() {
            c.id = CornerId(i as u32);
        }

        let (spread, spread_max, spread_max_at) = spread_of(&lines, reference_idx, params.step_m);

        Ok(Self {
            version: MODEL_VERSION,
            sim: session.sim,
            track: session.track.clone(),
            track_length_m: session.track_length,
            corners,
            provenance: Provenance {
                car: session.car.clone(),
                capture: file_name_of(capture),
                reference_lap: grids[reference_idx].0,
                lap_ids: grids.iter().map(|(id, _)| *id).collect(),
                reference_spread_m: spread,
                reference_spread_max_m: spread_max,
                reference_spread_max_at_m: spread_max_at,
                step_m: params.step_m,
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
            // Overlap would make `corner_at` ambiguous, and the detector's merge
            // step guarantees it cannot happen, so an overlap in a file means
            // the file was edited into an inconsistent state.
            if c.start_m < prev_end {
                return Err(format!("{at} starts at {} m, inside the previous corner which ends at {prev_end} m", c.start_m));
            }
            if c.support == 0 {
                return Err(format!("{at} claims no supporting laps"));
            }
            prev_end = c.end_m;
        }
        Ok(())
    }
}

/// Laps that must detect a corner for it to enter the model.
///
/// Clamped to at least two: the failure this module exists to prevent is a
/// single lap's noise becoming canonical, and a threshold of one would let every
/// one of it through.
fn required_support(lap_count: u32, min_support: f32) -> u32 {
    let fraction = if min_support.is_finite() {
        min_support.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let wanted = (fraction * lap_count as f32).ceil() as u32;
    wanted.clamp(2, lap_count.max(2))
}

/// How many laps independently found each of the reference lap's corners.
///
/// Every detection in every lap is assigned to **at most one** candidate — the
/// nearest one it plausibly belongs to — and each lap then votes at most once
/// per candidate. Both halves of that matter, and both are cases this data
/// actually produces:
///
/// * *One detection, two candidates.* The reference lap's Turn 4 and Turn 5 at
///   Red Bull Ring are 26 m apart, so with a 25 m tolerance their windows
///   overlap and another lap's single apex at 2263 m sits inside both. Counting
///   it twice would let one detection manufacture agreement for two corners.
/// * *Two detections, one candidate.* Where a lap splits what the reference
///   merged, both halves land in the same window; that is one lap agreeing, not
///   two.
///
/// Matching is by apex rather than by span overlap because an apex is a point:
/// where one lap merges what another splits, the merged detection *overlaps*
/// both halves and would vouch for each of them, while its apex falls in one or
/// neither.
fn support_counts(
    candidates: &[TrackCorner],
    detections: &[Vec<TrackCorner>],
    tolerance_m: f32,
) -> Vec<u32> {
    let mut counts = vec![0u32; candidates.len()];

    for lap in detections {
        let mut voted = vec![false; candidates.len()];
        for d in lap {
            if let Some(best) = nearest_candidate(d, candidates, tolerance_m) {
                voted[best] = true;
            }
        }
        for (count, voted) in counts.iter_mut().zip(&voted) {
            if *voted {
                *count += 1;
            }
        }
    }

    counts
}

/// The candidate a detection belongs to, if any: same direction, apex inside the
/// candidate's span widened by `tolerance_m`, and nearest by apex of those.
fn nearest_candidate(
    detection: &TrackCorner,
    candidates: &[TrackCorner],
    tolerance_m: f32,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.direction == detection.direction
                && detection.apex_m >= c.start_m - tolerance_m
                && detection.apex_m <= c.end_m + tolerance_m
        })
        .min_by(|(_, a), (_, b)| {
            let da = (a.apex_m - detection.apex_m).abs();
            let db = (b.apex_m - detection.apex_m).abs();
            da.total_cmp(&db)
        })
        .map(|(i, _)| i)
}

/// Mean separation from one line to all the others, metres.
///
/// Recomputed rather than threaded out of [`line::medoid_lap`]: the pairwise
/// pass is `O(n)` per pair now, so running it twice is cheaper than the plumbing
/// needed to avoid it.
fn spread_of(
    lines: &[&[crate::core::sample::Sample]],
    idx: usize,
    step_m: f32,
) -> (f32, f32, f32) {
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

    /// Build a lap from a curvature programme of `(length_m, signed_curvature)`
    /// segments, integrated into a path on a 1 m grid.
    fn lap_from_curvature(id: u32, program: &[(f32, f32)]) -> Lap {
        let mut samples = Vec::new();
        let (mut heading, mut x, mut z, mut d) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);

        for (length, k) in program {
            for _ in 0..(*length as usize) {
                // Left-handed X/Z: positive curvature turns right.
                heading = wrap_pi(heading + k);
                x += heading.sin();
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
            // Longer than the synthetic laps above, so the `next_corner` wrap
            // test has straight to sit on past the last corner.
            track_length: 1200.0,
            sector_count: 3,
            ac_version: "1.16.3".to_string(),
            sm_version: "1.7".to_string(),
        }
    }

    #[test]
    fn a_corner_only_one_lap_saw_is_not_in_the_model() {
        // Three laps of the same circuit. Only the first has the kink, and it is
        // the shape of the MX5's 1-degree phantom at 1345 m.
        let plain = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
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
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
        let laps: Vec<Lap> = (0..3).map(|i| lap_from_curvature(i, program)).collect();

        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        assert_eq!(model.corners.len(), 2);
        assert_eq!(model.direction_counts(), (1, 1), "one left, one right");
        for c in &model.corners {
            assert_eq!(c.support, 3, "every lap drove this corner");
        }
        // Ids are renumbered from the line with no gaps.
        assert_eq!(model.corners[0].id, CornerId(0));
        assert_eq!(model.corners[1].id, CornerId(1));
    }

    #[test]
    fn ids_are_renumbered_after_the_vote_removes_a_middle_corner() {
        // The phantom sits *between* two real corners, so if renumbering were
        // skipped the surviving ids would be 0 and 2.
        let with_kink = &[
            (300.0, 0.0),
            right_90(),
            (150.0, 0.0),
            kink(),
            (150.0, 0.0),
            left_90(),
            (300.0, 0.0),
        ];
        let plain = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];

        // Two of three laps carry the kink so the *reference* is likely to have
        // it; the vote still needs 2 of 3, which the kink has here — so this
        // case checks numbering, not the vote.
        let laps = vec![
            lap_from_curvature(0, with_kink),
            lap_from_curvature(1, with_kink),
            lap_from_curvature(2, plain),
        ];
        let model = TrackModel::learn(&session(), &laps, "cap.ndjson", &LearnParams::default())
            .expect("three clean laps");

        for (i, c) in model.corners.iter().enumerate() {
            assert_eq!(c.id, CornerId(i as u32));
        }
    }

    #[test]
    fn one_lap_is_refused_rather_than_believed() {
        let laps = vec![lap_from_curvature(0, &[(300.0, 0.0), right_90(), (300.0, 0.0)])];
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

    /// A detection at `apex_m`, spanning it symmetrically.
    fn detection(id: u32, apex_m: f32, half_span: f32, direction: CornerDirection) -> TrackCorner {
        TrackCorner {
            id: CornerId(id),
            start_m: apex_m - half_span,
            end_m: apex_m + half_span,
            apex_m,
            heading_apex_m: apex_m - 5.0,
            direction,
            peak_curvature: 0.02,
            turn_angle: if direction == CornerDirection::Right {
                1.5
            } else {
                -1.5
            },
            min_speed: 25.0,
        }
    }

    #[test]
    fn an_opposite_hand_detection_does_not_vouch_for_a_corner() {
        let right = detection(0, 140.0, 40.0, CornerDirection::Right);
        let mirrored = detection(0, 140.0, 40.0, CornerDirection::Left);

        // Own lap plus a lap that turns the other way at the same place.
        let candidates = vec![right.clone()];
        let support = support_counts(&candidates, &[vec![right], vec![mirrored]], 25.0);
        assert_eq!(
            support,
            vec![1],
            "a left-hander must not support a right-hander"
        );
    }

    #[test]
    fn support_needs_an_apex_inside_the_span_not_merely_an_overlap() {
        // The 3890 m case: one lap's single wide detection overlaps two of
        // another lap's, and must not vouch for both.
        let late = TrackCorner {
            id: CornerId(0),
            start_m: 3890.0,
            end_m: 3921.0,
            apex_m: 3905.0,
            heading_apex_m: 3900.0,
            direction: CornerDirection::Right,
            peak_curvature: 0.008,
            turn_angle: 0.23,
            min_speed: 30.0,
        };
        let wide = TrackCorner {
            start_m: 3778.0,
            end_m: 3904.0,
            apex_m: 3829.0,
            heading_apex_m: 3820.0,
            ..late.clone()
        };

        // `wide` overlaps `late` by 14 m, but its apex is 61 m outside it.
        let candidates = vec![late.clone()];
        let support = support_counts(&candidates, &[vec![late], vec![wide]], 25.0);
        assert_eq!(
            support,
            vec![1],
            "an overlapping detection whose apex is elsewhere is a different corner"
        );
    }

    #[test]
    fn one_apex_cannot_vouch_for_two_adjacent_corners() {
        // The measured Red Bull Ring case: the reference lap's T4 and T5 apexes
        // sit at 2235 m and 2294 m, 59 m apart, so at a 25 m tolerance their
        // windows overlap. Another lap's single apex at 2263 m falls inside
        // both, and must be credited only to the nearer of them.
        let t4 = detection(0, 2235.0, 16.0, CornerDirection::Right);
        let t5 = detection(1, 2294.0, 16.0, CornerDirection::Right);
        let candidates = vec![t4.clone(), t5.clone()];

        let other = detection(0, 2263.0, 22.0, CornerDirection::Right);
        assert!(
            other.apex_m >= t4.start_m - 25.0 && other.apex_m <= t4.end_m + 25.0,
            "precondition: 2263 m is inside T4's window"
        );
        assert!(
            other.apex_m >= t5.start_m - 25.0 && other.apex_m <= t5.end_m + 25.0,
            "precondition: 2263 m is inside T5's window too"
        );

        let support = support_counts(&candidates, &[vec![t4, t5], vec![other]], 25.0);
        // T4's apex is 28 m from 2263, T5's is 31 m, so T4 takes the vote.
        assert_eq!(
            support,
            vec![2, 1],
            "one detection must not manufacture agreement for two corners"
        );
    }

    #[test]
    fn a_lap_that_splits_one_corner_in_two_still_votes_once() {
        // The reverse many-to-one: where a lap splits what the reference merged,
        // both halves land in the same window. That is one lap agreeing.
        let merged = detection(0, 2070.0, 60.0, CornerDirection::Right);
        let first_half = detection(0, 2040.0, 25.0, CornerDirection::Right);
        let second_half = detection(1, 2100.0, 25.0, CornerDirection::Right);

        let candidates = vec![merged.clone()];
        let support = support_counts(
            &candidates,
            &[vec![merged], vec![first_half, second_half]],
            25.0,
        );
        assert_eq!(support, vec![2], "two halves in one lap are one vote");
    }

    #[test]
    fn support_threshold_never_drops_below_two_laps() {
        // A fraction low enough to ask for one lap must still demand two.
        assert_eq!(required_support(3, 0.0), 2);
        assert_eq!(required_support(2, 0.1), 2);
        assert_eq!(required_support(3, 0.5), 2);
        assert_eq!(required_support(6, 0.5), 3);
        assert_eq!(required_support(4, 1.0), 4);
        // Degenerate inputs must not produce a threshold no corner can meet.
        assert_eq!(required_support(3, f32::NAN), 2);
        assert_eq!(required_support(1, 1.0), 2);
    }

    #[test]
    fn corner_lookup_finds_the_containing_corner_and_the_next_one() {
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
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
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
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
        assert_eq!(back.provenance.reference_lap, model.provenance.reference_lap);
        assert_eq!(back.provenance.car, model.provenance.car);
        for (a, b) in back.corners.iter().zip(&model.corners) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.start_m, b.start_m);
            assert_eq!(a.apex_m, b.apex_m);
            assert_eq!(a.direction, b.direction);
            assert_eq!(a.support, b.support);
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

    /// Round-trip a model through JSON after mutating it, to check `load`'s
    /// validation rather than `save`'s output.
    /// A model learned from three identical laps of a right-then-left circuit.
    fn learned() -> TrackModel {
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
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
        let path = dir.join(format!("m{}.json", std::process::id()));
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
            .check_track(&TrackId::new("other_circuit", "layout_a"), 1200.0)
            .expect_err("a model for one circuit must not be used on another");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn check_track_refuses_a_different_layout_of_the_same_track() {
        // The trap this guards: AC ships several layouts in one track folder, so
        // the name can match while the circuit does not.
        let model = learned();
        let err = model
            .check_track(&TrackId::new("test_circuit", "layout_b"), 1200.0)
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
