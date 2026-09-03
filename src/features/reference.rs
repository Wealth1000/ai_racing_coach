//! The driver's own best pass through each corner, kept on disk.
//!
//! The coaching plan's locked decision #5 is that the reference is **the
//! driver's personal best** — there is no external reference-lap dataset to
//! compare against, and comparing against yourself sidesteps car, tyre and
//! setup normalisation entirely. This module is that decision made concrete:
//! one stored pass per canonical corner.
//!
//! # The best *pass*, not the best lap and not a composite
//!
//! Three candidate definitions were considered:
//!
//! * *The fastest lap's numbers everywhere.* One scruffy lap would set every
//!   target, and a corner driven brilliantly last week in a slower overall lap
//!   would be forgotten.
//! * *Per-metric extremes* — highest apex speed ever, shortest braking zone
//!   ever, earliest pickup ever, each possibly from a different day. This is
//!   the classic "theoretical best" composite, and it is a fiction nobody has
//!   ever driven: its entry speed did not coexist with its apex speed. Rules
//!   comparing against it nag about impossible deltas.
//! * *The fastest single pass through each corner* — all numbers from one
//!   real crossing of that span. This is what a driver means by "I know I can
//!   do this corner better; I did it on Tuesday".
//!
//! The third wins. Within one corner the reference stays coherent (a pass
//! that actually happened); across corners it is robust (one bad lap cannot
//! poison the whole set).
//!
//! "Fastest through the corner" means lowest [`CornerFeatures::
//! time_in_corner_s`] over the model's fixed boundaries — comparable between
//! laps precisely because the span does not move. Ties break on higher apex
//! speed (momentum through the corner), then lower lap id, so selection is
//! deterministic for identical input.
//!
//! # Merging, or why a personal best must never reset
//!
//! A capture covers one session. Overwriting the file each time would mean
//! every PB resets whenever you drive again, which is not a personal best, it
//! is a last-result log. So building from a new capture *absorbs*: a corner's
//! stored pass survives unless the new capture drove that span strictly
//! faster. Ties keep what is stored — stability beats churn.
//!
//! Absorbing is only meaningful against the same corners. Corner ordinals are
//! positions in a learned list, and `learn-track` re-runs shift them; a
//! reference keyed to old ordinals describes somewhere else entirely. Every
//! store therefore records [`ReferenceStore::model_fingerprint`] (see
//! [`TrackModel::fingerprint`]) and the car it was driven in, and callers
//! must refuse to merge when either differs — starting fresh with a printed
//! notice instead of silently mixing geometries.
//!
//! # What is deliberately absent
//!
//! No timestamps: they would make saves nondeterministic and add nothing the
//! captures list does not. No slip-angle or off-track figures: a reference is
//! a target, and enshrining a slide as the thing to reproduce is exactly
//! wrong. [`CornerReference::source_lap`] is kept because tracing a number
//! back to the pass that set it is worth an honest caveat about ordinals.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::ids::{CornerId, LapId, TrackId};
use crate::core::sample::Sim;
use crate::core::{CoachError, Result};
use crate::features::corner::CornerDirection;
use crate::features::corner_features::{CornerFeatures, FeatureParams, extract_all};
use crate::features::resample::ResampledLap;
use crate::features::track_model::TrackModel;

/// On-disk format version; [`ReferenceStore::load`] refuses anything else.
pub const REFERENCE_VERSION: u32 = 1;

/// How this artefact names itself in [`CoachError::BadArtefact`].
const ARTEFACT: &str = "personal best";

/// The stored best pass through one corner.
///
/// Distances are signed offsets from the model's boundary or apex, matching
/// how `coach analyse` prints them: negative is before the corner, positive
/// is past it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CornerReference {
    /// Canonical corner this pass went through. Ordinals follow the track
    /// model the store was built against — see `model_fingerprint`.
    pub corner_id: CornerId,
    pub direction: CornerDirection,
    /// Ordinal of the lap, within whichever capture produced this pass. It is
    /// session-local and means nothing on its own; it exists so a surprising
    /// entry can be traced back to the capture in `provenance.captures` and
    /// from there to a lap number.
    pub source_lap: LapId,

    pub entry_speed_mps: f32,
    pub apex_speed_mps: f32,
    pub exit_speed_mps: f32,
    /// Time over the model's span, seconds. The quantity passes are ranked on.
    pub time_in_corner_s: f32,

    /// Signed offset of the braking point from the corner boundary, metres.
    /// `None` if the winning pass took the corner without braking.
    pub brake_offset_m: Option<f32>,
    /// Signed offset past the apex where full throttle returned, metres.
    /// `None` if it never did inside the extraction window.
    pub throttle_pickup_offset_m: Option<f32>,
    pub trail_braking: bool,
}

/// Where a store's contents came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceProvenance {
    /// Car every pass was driven in. Boundaries are per-car (see the
    /// `track_model` module docs); so is everything measured inside them.
    pub car: String,
    /// File names of every capture absorbed since the store was created,
    /// first contribution first, without duplicates.
    pub captures: Vec<String>,
    /// Grid spacing the most recent build extracted features at.
    pub step_m: f32,
}

/// The per-corner personal best for one track, layout and car.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceStore {
    pub version: u32,
    pub sim: Sim,
    pub track: TrackId,
    /// Metres, copied from the track model at build time.
    pub track_length_m: f32,
    /// [`TrackModel::fingerprint`] of the exact corner set these ordinals
    /// index. Stores with different fingerprints describe different corners
    /// and must not be merged.
    pub model_fingerprint: u64,
    /// Ordered by `corner_id`. Ids need not be contiguous: a corner no clean
    /// lap covered simply has no entry yet.
    pub corners: Vec<CornerReference>,
    pub provenance: ReferenceProvenance,
}

/// What [`ReferenceStore::absorb`] changed, for reporting rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeReport {
    /// Corners where the incoming capture was strictly faster.
    pub improved: usize,
    /// Corners where the stored pass stood.
    pub kept: usize,
    /// Corners with no stored pass before this merge.
    pub added: usize,
}

impl ReferenceStore {
    /// Build a fresh store from per-lap feature tables.
    ///
    /// `per_lap` holds one `(lap id, features)` pair per clean lap; the inner
    /// vectors come from `extract_all`, so they follow model order but may
    /// skip corners the lap's grid could not cover. Corners with no pass at
    /// all get no entry.
    pub fn build(
        model: &TrackModel,
        car: String,
        capture: &str,
        step_m: f32,
        per_lap: &[(LapId, Vec<CornerFeatures>)],
    ) -> Result<Self> {
        let model_n = model.corners.len();

        // Group passes by corner id, not by table position: extract_all skips
        // uncovered corners, so position i is not corner i in general.
        let mut candidates: Vec<Vec<CornerFeatures>> = vec![Vec::new(); model_n];
        for (lap_id, features) in per_lap {
            for f in features {
                let idx = f.corner_id.0 as usize;
                if idx >= model_n {
                    return Err(CoachError::BadArtefact {
                        path: capture.to_string(),
                        artefact: ARTEFACT,
                        detail: format!(
                            "lap {} produced features for corner {}, but the model only has {} corners — \
                             corner ordinals must index model geometry",
                            lap_id.0, f.corner_id.0, model_n
                        ),
                    });
                }
                candidates[idx].push(*f);
            }
        }

        let mut corners: Vec<CornerReference> = candidates
            .iter()
            .zip(&model.corners)
            .filter_map(|(passes, model_corner)| {
                best_pass(passes).map(|f| CornerReference {
                    corner_id: model_corner.id,
                    direction: f.direction,
                    source_lap: f.lap_id,
                    entry_speed_mps: f.entry_speed_mps,
                    apex_speed_mps: f.apex_speed_mps,
                    exit_speed_mps: f.exit_speed_mps,
                    time_in_corner_s: f.time_in_corner_s,
                    brake_offset_m: f.braking_length_m.map(|len| -len),
                    throttle_pickup_offset_m: f.throttle_pickup_offset_m,
                    trail_braking: f.trail_braking,
                })
            })
            .collect();

        // `best_pass` ran per corner in model order, which is ascending id
        // order already; sorting keeps that true even if a future change
        // breaks the assumption, and documents the invariant load enforces.
        corners.sort_by_key(|c| c.corner_id);

        Ok(Self {
            version: REFERENCE_VERSION,
            sim: model.sim,
            track: model.track.clone(),
            track_length_m: model.track_length_m,
            model_fingerprint: model.fingerprint(),
            corners,
            provenance: ReferenceProvenance {
                car,
                captures: vec![file_name_of(capture)],
                step_m,
            },
        })
    }

    /// Fold another store's passes into this one, keeping the faster per
    /// corner. Returns what changed.
    ///
    /// The caller decides compatibility — same car, same model fingerprint —
    /// and is expected to have told the user before discarding either side;
    /// this method only merges.
    pub fn absorb(&mut self, incoming: ReferenceStore) -> MergeReport {
        let mut report = MergeReport {
            improved: 0,
            kept: 0,
            added: 0,
        };

        for inc in incoming.corners {
            match self.corners.iter().enumerate().find(|(_, c)| c.corner_id == inc.corner_id) {
                Some((i, existing)) => {
                    if inc.time_in_corner_s.total_cmp(&existing.time_in_corner_s) == std::cmp::Ordering::Less {
                        self.corners[i] = inc;
                        report.improved += 1;
                    } else {
                        report.kept += 1;
                    }
                }
                None => {
                    self.corners.push(inc);
                    report.added += 1;
                }
            }
        }
        self.corners.sort_by_key(|c| c.corner_id);

        // Provenance accumulates: every capture that ever contributed, in the
        // order it first did.
        for cap in incoming.provenance.captures {
            if !self.provenance.captures.contains(&cap) {
                self.provenance.captures.push(cap);
            }
        }
        self.provenance.step_m = incoming.provenance.step_m;

        report
    }

    /// Whether this store may be merged against a model of the given sim, car
    /// and fingerprint. Anything else must start fresh rather than mix corners
    /// from different geometries, different cars or different sims — the last
    /// matters because two sims can name the same circuit, and a PB's corner
    /// ordinals index the *other* sim's learned geometry.
    pub fn compatible_with(&self, sim: Sim, car: &str, model_fingerprint: u64) -> bool {
        self.sim == sim && self.provenance.car == car && self.model_fingerprint == model_fingerprint
    }

    /// A store with no passes: the stand-in a live session uses when no
    /// personal best exists yet, so the comparison tier stays silent.
    ///
    /// Records the model's fingerprint and car, so lookups are consistent with
    /// a real store — there is simply nothing in it. Never save one: its empty
    /// provenance would be refused on load, correctly.
    pub fn empty(model: &TrackModel) -> Self {
        Self {
            version: REFERENCE_VERSION,
            sim: model.sim,
            track: model.track.clone(),
            track_length_m: model.track_length_m,
            model_fingerprint: model.fingerprint(),
            corners: Vec::new(),
            provenance: ReferenceProvenance {
                car: model.provenance.car.clone(),
                captures: Vec::new(),
                step_m: model.provenance.step_m,
            },
        }
    }

    /// The stored best pass through one corner, if any lap has set one.
    ///
    /// Lookups are by the *row* id, matching how [`extract_all`] reports
    /// features — the second row of a line-straddling corner is its own id in
    /// the store too.
    pub fn pass_for(&self, corner: CornerId) -> Option<&CornerReference> {
        self.corners
            .binary_search_by_key(&corner, |c| c.corner_id)
            .ok()
            .map(|i| &self.corners[i])
    }

    /// Conventional file name: `<track>_<layout>_pb.json`, sitting beside the
    /// track model it references.
    pub fn file_name(track: &TrackId) -> String {
        let base = TrackModel::file_name(track);
        format!("{}_pb.json", base.trim_end_matches(".json"))
    }

    /// Path a personal best belongs at: `<dir>/<sim-key>/<track>_<layout>_pb.json`,
    /// beside the model it is pinned to (the same layout
    /// [`TrackModel::path_in`] produces).
    pub fn path_in(dir: impl AsRef<Path>, sim: Sim, track: &TrackId) -> PathBuf {
        dir.as_ref()
            .join(sim.key())
            .join(Self::file_name(track))
    }

    /// Write the store as JSON, atomically (sibling temp file + rename), as
    /// [`TrackModel::save`] does.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let io = |source: std::io::Error| CoachError::Io {
            path: path.display().to_string(),
            source,
        };

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(io)?;
        }

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

    /// Read a store, refusing anything violating the invariants merging and
    /// display rely on.
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

        let store: Self =
            serde_json::from_str(&text).map_err(|e| bad(format!("malformed JSON: {e}")))?;
        store.validate().map_err(bad)?;
        Ok(store)
    }

    /// Check the invariants. `Err` carries the reason, for the caller to wrap.
    fn validate(&self) -> std::result::Result<(), String> {
        if self.version != REFERENCE_VERSION {
            return Err(format!(
                "format version {}, but this build reads version {REFERENCE_VERSION}",
                self.version
            ));
        }
        if !self.track_length_m.is_finite() || self.track_length_m <= 0.0 {
            return Err(format!("track length {} m", self.track_length_m));
        }
        if self.track.track.is_empty() {
            return Err("no track name".to_string());
        }
        if self.provenance.captures.is_empty() {
            return Err("no contributing captures recorded".to_string());
        }
        if !self.provenance.step_m.is_finite() || self.provenance.step_m <= 0.0 {
            return Err(format!("grid step {} m", self.provenance.step_m));
        }

        let mut prev_id: Option<u32> = None;
        for c in &self.corners {
            let at = format!("corner {}", c.corner_id);

            if let Some(prev) = prev_id {
                if c.corner_id.0 <= prev {
                    return Err(format!(
                        "{at} follows corner {prev}: ids must be strictly increasing"
                    ));
                }
            }
            prev_id = Some(c.corner_id.0);

            for (name, v) in [
                ("entry speed", c.entry_speed_mps),
                ("apex speed", c.apex_speed_mps),
                ("exit speed", c.exit_speed_mps),
                ("time", c.time_in_corner_s),
            ] {
                if !v.is_finite() || v < 0.0 {
                    return Err(format!("{at} has implausible {name}: {v}"));
                }
            }
            if c.time_in_corner_s == 0.0 {
                return Err(format!("{at} claims zero time through the span"));
            }
            for (name, v) in [
                ("brake offset", c.brake_offset_m),
                ("throttle pickup", c.throttle_pickup_offset_m),
            ] {
                if let Some(v) = v {
                    if !v.is_finite() {
                        return Err(format!("{at} has non-finite {name}: {v}"));
                    }
                }
            }
        }
        Ok(())
    }

    /// Extract features for every clean lap and build or update the store.
    ///
    /// This is the whole Batch-8 pipeline in one call — resample, extract,
    /// select winners — shared by tests and the CLI. Returns the store (not
    /// merged into any existing file; the caller owns that policy) alongside
    /// the number of clean laps that could not be put on the grid.
    pub fn harvest(
        model: &TrackModel,
        car: String,
        capture: &str,
        step_m: f32,
        params: &FeatureParams,
        grids: &[(LapId, ResampledLap)],
    ) -> Result<Self> {
        let per_lap: Vec<(LapId, Vec<CornerFeatures>)> = grids
            .iter()
            .map(|(id, grid)| (*id, extract_all(model, grid, params, *id)))
            .collect();
        Self::build(model, car, capture, step_m, &per_lap)
    }
}

/// The pass that defines the personal best among one corner's candidates:
/// fastest through the span, ties broken on apex speed then lap id.
fn best_pass(passes: &[CornerFeatures]) -> Option<CornerFeatures> {
    passes.iter().copied().min_by(|a, b| {
        a.time_in_corner_s
            .total_cmp(&b.time_in_corner_s)
            .then(b.apex_speed_mps.total_cmp(&a.apex_speed_mps))
            .then(a.lap_id.cmp(&b.lap_id))
    })
}

/// Last path component, for the provenance record.
fn file_name_of(capture: &str) -> String {
    Path::new(capture)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| capture.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::ids::TrackId;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique-within-this-process id for tests that share a temp directory.
    /// The directory key (`coach_reference_validate`) is fixed, so without
    /// this counter parallel tests race on the same file.
    pub(crate) fn next_id() -> u32 {
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// A `CornerFeatures` with plausible defaults and the knobs tests name.
    fn pass(lap: u32, corner: u32, time_s: f32) -> CornerFeatures {
        CornerFeatures {
            lap_id: LapId(lap),
            corner_id: CornerId(corner),
            direction: CornerDirection::Right,
            entry_speed_mps: 40.0,
            apex_speed_mps: 30.0,
            exit_speed_mps: 45.0,
            speed_min_offset_m: 0.0,
            brake_start_m: Some(250.0),
            braking_length_m: Some(50.0),
            peak_brake: 0.8,
            trail_braking: false,
            throttle_pickup_offset_m: Some(15.0),
            min_throttle_in_corner: 0.2,
            time_in_corner_s: time_s,
            peak_abs_slip_rad: 0.05,
            off_track_points: 0,
        }
    }

    fn model() -> TrackModel {
        TrackModel {
            version: crate::features::track_model::MODEL_VERSION,
            sim: Sim::AssettoCorsa,
            track: TrackId::new("test_circuit", ""),
            track_length_m: 1200.0,
            provenance: crate::features::track_model::Provenance {
                car: "test_car".to_string(),
                capture: "cap.ndjson".to_string(),
                estimator: crate::features::track_model::ESTIMATOR.to_string(),
                reference_lap: LapId(0),
                lap_ids: vec![LapId(0), LapId(1)],
                reference_spread_m: 0.5,
                reference_spread_max_m: 1.0,
                reference_spread_max_at_m: 100.0,
                step_m: 1.0,
                sigma_k_per_lap: vec![1e-3, 1e-3],
                pedal_events: false,
            },
            corners: vec![
                crate::features::track_model::ModelCorner {
                    id: CornerId(0),
                    start_m: 300.0,
                    end_m: 380.0,
                    apex_m: 340.0,
                    heading_apex_m: 338.0,
                    direction: CornerDirection::Right,
                    turn_angle: 1.5,
                    peak_curvature: 0.02,
                    support: 3,
                    parent_id: None,
                    match_fraction: 1.0,
                    decision_events: Vec::new(),
                },
                crate::features::track_model::ModelCorner {
                    id: CornerId(1),
                    start_m: 700.0,
                    end_m: 800.0,
                    apex_m: 750.0,
                    heading_apex_m: 748.0,
                    direction: CornerDirection::Left,
                    turn_angle: -1.2,
                    peak_curvature: 0.02,
                    support: 3,
                    parent_id: None,
                    match_fraction: 1.0,
                    decision_events: Vec::new(),
                },
            ],
        }
    }

    /// Same shape as [`model`] but with a third corner, for tests that need
    /// to absorb a new corner the initial store doesn't yet know about.
    fn model3() -> TrackModel {
        let mut m = model();
        m.corners.push(crate::features::track_model::ModelCorner {
            id: CornerId(2),
            start_m: 950.0,
            end_m: 1020.0,
            apex_m: 985.0,
            heading_apex_m: 983.0,
            direction: CornerDirection::Right,
            turn_angle: 0.8,
            peak_curvature: 0.02,
            support: 3,
            parent_id: None,
            match_fraction: 1.0,
            decision_events: Vec::new(),
        });
        m
    }

    #[test]
    fn the_fastest_pass_wins_and_ties_break_on_apex_then_lap() {
        let passes = vec![
            pass(4, 0, 4.20),
            pass(1, 0, 4.00),
            pass(9, 0, 4.10),
        ];
        assert_eq!(best_pass(&passes).map(|f| f.lap_id), Some(LapId(1)));

        // Equal times: the higher apex speed wins.
        let mut faster_apex = pass(2, 0, 4.00);
        faster_apex.apex_speed_mps = 31.5;
        let passes = vec![pass(1, 0, 4.00), faster_apex];
        assert_eq!(best_pass(&passes).map(|f| f.lap_id), Some(LapId(2)));

        // Fully identical times and apexes: determinism, not coin flips.
        let passes = vec![pass(3, 0, 4.00), pass(2, 0, 4.00)];
        assert_eq!(best_pass(&passes).map(|f| f.lap_id), Some(LapId(2)));

        assert!(best_pass(&[]).is_none());
    }

    #[test]
    fn build_keys_entries_by_corner_id_and_skips_uncovered_corners() {
        let m = model();
        // Lap 0 covers both corners; lap 1 only corner 1 (extract_all order
        // is model order, but corner 0 was skipped, proving grouping is by
        // id and not by position).
        let per_lap = vec![
            (
                LapId(0),
                vec![pass(0, 0, 4.00), pass(0, 1, 3.00)],
            ),
            (LapId(1), vec![pass(1, 1, 2.80)]),
        ];

        let store = ReferenceStore::build(&m, "test_car".into(), "cap.ndjson", 1.0, &per_lap)
            .expect("build");

        assert_eq!(store.model_fingerprint, m.fingerprint());
        assert_eq!(
            store.corners.iter().map(|c| c.corner_id).collect::<Vec<_>>(),
            vec![CornerId(0), CornerId(1)]
        );
        assert_eq!(store.corners[0].source_lap, LapId(0));
        assert_eq!(store.corners[1].time_in_corner_s, 2.80);
        assert_eq!(store.provenance.captures, vec!["cap.ndjson"]);
    }

    #[test]
    fn build_refuses_a_corner_id_outside_the_model() {
        let m = model();
        let per_lap = vec![(LapId(0), vec![pass(0, 9, 4.0)])];
        let err = ReferenceStore::build(&m, "car".into(), "cap.ndjson", 1.0, &per_lap)
            .expect_err("out-of-range id");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn build_records_the_brake_point_as_a_signed_offset() {
        let m = model();
        // braking_length +50 (zone ends 50 m before the boundary) → -50.
        let per_lap = vec![(LapId(0), vec![pass(0, 0, 4.0)])];
        let store = ReferenceStore::build(&m, "car".into(), "cap.ndjson", 1.0, &per_lap)
            .expect("build");
        assert_eq!(store.corners[0].brake_offset_m, Some(-50.0));
    }

    #[test]
    fn a_corner_no_lap_covered_gets_no_entry() {
        let m = model();
        let per_lap = vec![(LapId(0), vec![pass(0, 1, 3.0)])];
        let store = ReferenceStore::build(&m, "car".into(), "cap.ndjson", 1.0, &per_lap)
            .expect("build");
        assert_eq!(store.corners.len(), 1);
        assert_eq!(store.corners[0].corner_id, CornerId(1));
    }

    #[test]
    fn absorb_improves_keeps_and_adds_and_unions_captures() {
        let m = model3();
        let mut store = ReferenceStore::build(
            &m,
            "test_car".into(),
            "first.ndjson",
            1.0,
            &[(
                LapId(0),
                vec![pass(0, 0, 4.00), pass(0, 1, 3.00)],
            )],
        )
        .expect("build first");

        // Second session: faster through corner 0, slower through corner 1,
        // plus a corner the first capture never saw.
        let second = ReferenceStore::build(
            &m,
            "test_car".into(),
            "second.ndjson",
            1.0,
            &[(
                LapId(3),
                vec![pass(3, 0, 3.70), pass(3, 1, 3.40), {
                    let mut p = pass(3, 2, 2.00);
                    p.direction = CornerDirection::Left;
                    p
                }],
            )],
        )
        .expect("build second");

        let report = store.absorb(second);
        assert_eq!(report, MergeReport { improved: 1, kept: 1, added: 1 });

        assert_eq!(store.corners[0].time_in_corner_s, 3.70);
        assert_eq!(store.corners[0].source_lap, LapId(3));
        assert_eq!(store.corners[1].time_in_corner_s, 3.00, "slower must not replace");
        assert_eq!(store.corners[2].corner_id, CornerId(2));

        assert_eq!(
            store.provenance.captures,
            vec!["first.ndjson", "second.ndjson"]
        );
        // Still sorted after the append.
        assert!(
            store.corners.windows(2).all(|w| w[0].corner_id < w[1].corner_id),
            "absorb must restore ordering"
        );
    }

    #[test]
    fn absorb_breaks_ties_in_favour_of_what_is_stored() {
        let m = model();
        let mut store =
            ReferenceStore::build(&m, "car".into(), "a.ndjson", 1.0, &[(LapId(0), vec![pass(0, 0, 4.00)])])
                .expect("build");
        let equal = ReferenceStore::build(
            &m,
            "car".into(),
            "b.ndjson",
            1.0,
            &[(LapId(5), vec![pass(5, 0, 4.00)])],
        )
        .expect("build");

        let report = store.absorb(equal);
        assert_eq!(report.improved, 0);
        assert_eq!(report.kept, 1);
        assert_eq!(store.corners[0].source_lap, LapId(0), "ties keep the incumbent");
    }

    #[test]
    fn compatibility_needs_both_the_same_car_and_the_same_geometry() {
        let m = model();
        let store = ReferenceStore::build(&m, "ks_ferrari_f138".into(), "a.ndjson", 1.0, &[])
            .expect("build");
        let fp = m.fingerprint();

        assert!(store.compatible_with(Sim::AssettoCorsa, "ks_ferrari_f138", fp));
        assert!(!store.compatible_with(Sim::AssettoCorsa, "ks_mazda_mx5_cup", fp), "different car");
        assert!(!store.compatible_with(Sim::AssettoCorsa, "ks_ferrari_f138", fp ^ 1), "re-learned model");
    }

    #[test]
    fn a_saved_store_reloads_identically() {
        let m = model();
        let store = ReferenceStore::build(
            &m,
            "test_car".into(),
            "cap.ndjson",
            1.0,
            &[(LapId(0), vec![pass(0, 0, 4.0), pass(0, 1, 3.0)])],
        )
        .expect("build");

        let dir = std::env::temp_dir().join("coach_reference_roundtrip");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join(ReferenceStore::file_name(&m.track));

        store.save(&path).expect("save");
        assert_eq!(path.file_name().unwrap(), "test_circuit_pb.json");
        let back = ReferenceStore::load(&path).expect("load");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(back, store);
    }

    #[test]
    fn file_name_sits_beside_the_track_model() {
        assert_eq!(
            ReferenceStore::file_name(&TrackId::new("ks_red_bull_ring", "layout_gp")),
            "ks_red_bull_ring_layout_gp_pb.json"
        );
        assert_eq!(
            ReferenceStore::file_name(&TrackId::new("magione", "")),
            "magione_pb.json"
        );
    }

    #[test]
    fn path_in_sits_beside_the_track_model_under_the_sim() {
        // The same layout [`TrackModel::path_in`] produces, so a personal best
        // and the model it is pinned to can never end up in different
        // directories.
        assert_eq!(
            ReferenceStore::path_in("data/tracks", Sim::AssettoCorsa, &TrackId::new("monza", "")),
            PathBuf::from("data/tracks/ac/monza_pb.json")
        );
    }

    /// Round-trip a store through JSON after mutating it, checking `load`'s
    /// validation.
    fn load_mutated(mutate: impl FnOnce(&mut serde_json::Value)) -> Result<ReferenceStore> {
        let m = model();
        let store = ReferenceStore::build(
            &m,
            "test_car".into(),
            "cap.ndjson",
            1.0,
            &[(LapId(0), vec![pass(0, 0, 4.0), pass(0, 1, 3.0)])],
        )
        .expect("build");
        let mut json = serde_json::to_value(&store).expect("serialise");
        mutate(&mut json);

        let dir = std::env::temp_dir().join("coach_reference_validate");
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("r{}_{}.json", std::process::id(), crate::features::reference::tests::next_id()));
        fs::write(&path, serde_json::to_string(&json).unwrap()).expect("write");
        let result = ReferenceStore::load(&path);
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn load_refuses_a_future_version() {
        let err = load_mutated(|j| j["version"] = serde_json::json!(REFERENCE_VERSION + 1))
            .expect_err("version mismatch");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_out_of_order_or_duplicate_ids() {
        let err = load_mutated(|j| {
            j["corners"][0]["corner_id"] = serde_json::json!(1);
            j["corners"][1]["corner_id"] = serde_json::json!(1);
        })
        .expect_err("duplicate ids");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_implausible_numbers() {
        let err = load_mutated(|j| j["corners"][0]["apex_speed_mps"] = serde_json::json!(-5.0))
            .expect_err("negative speed");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");

        let err = load_mutated(|j| j["corners"][0]["time_in_corner_s"] = serde_json::json!(f32::NAN))
            .expect_err("NaN time");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");

        let err = load_mutated(|j| j["corners"][1]["time_in_corner_s"] = serde_json::json!(0.0))
            .expect_err("zero time through a span is not a pass");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_a_store_with_no_provenance() {
        let err = load_mutated(|j| j["provenance"]["captures"] = serde_json::json!([]))
            .expect_err("untraceable store");
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }

    #[test]
    fn load_refuses_malformed_json() {
        let dir = std::env::temp_dir().join("coach_reference_malformed");
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("broken.json");
        fs::write(&path, b"{not json").expect("write");
        let err = ReferenceStore::load(&path).expect_err("malformed");
        let _ = fs::remove_file(&path);
        assert!(matches!(err, CoachError::BadArtefact { .. }), "got {err}");
    }
}
