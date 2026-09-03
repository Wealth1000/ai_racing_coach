//! Stage 2 of the canonical corner learner: cross-lap consensus.
//!
//! # The arbiter
//!
//! The only threshold that generalises across tracks is not a number — it is
//! cross-lap agreement. Jitter and driver error are uncorrelated between
//! laps; real corners recur at the same distance, in the same direction,
//! with a similar turn angle, every lap. So Stage 1
//! ([`crate::features::segment`]) over-generates candidates with recall at
//! all costs, and *this* module is the sole arbiter of existence.
//!
//! # Alignment, not nearest-match
//!
//! Corners are strictly ordered along the ring and that order is stable
//! across laps, so each lap's candidates are matched to the running model the
//! way two sequences are aligned: dynamic programming over (match, insert,
//! delete). What that buys is the driver-error tolerance the problem demands
//! — a spin that manufactures a spurious arc, or a skipped kink, becomes a
//! local insertion/deletion instead of shifting every subsequent corner by
//! one slot, which is the failure a greedy nearest-match invites.
//!
//! # Confirmation: a majority with a confidence bound
//!
//! A tentative corner is canonical when the **Wilson lower bound** on its
//! match fraction exceeds ½ at α = 0.10 (one-sided):
//!
//! > "canonical" means "more likely than not present, demonstrated with
//! > statistical confidence".
//!
//! One parameter, with a defensible meaning. The derived schedule
//! (matches required out of laps seen) is 2/2, 3/3, 4/5, 5/6, 6/8, 8/10 —
//! strict at small lap counts, lenient as evidence accumulates. It is
//! slightly stricter than the problem statement's hand sketch at 6 and 10
//! laps; with ten laps on the table, six matches is evidence that the driver
//! *misses* this corner 40% of the time, which is coaching output, not a
//! reason to omit it from the canonical set.
//!
//! # Laps are classified, not discarded
//!
//! Atypical laps (out-lap pace, spins, off-track — anything whose lap time
//! is a robust-z outlier) still vote on existence — a spin at T7 is strong
//! evidence T7 exists — but contribute no geometry, because their line is
//! distorted and would drag the medians.

use crate::features::corner::CornerDirection;
use crate::features::segment::{CandidateArc, RESOLUTION_FLOOR_M};
use crate::features::stats;

/// z for α = 0.10, one-sided — the confidence convention of the design.
pub const WILSON_Z: f64 = 1.2816;

/// Gap penalties as a multiple of the match-cost scale (affine-gap
/// convention from sequence alignment; the multiple sits in units of the
/// data-derived τ²).
pub const GAP_PENALTY_MULT: f32 = 4.0;

/// Match cost for an opposite-hand candidate: prohibitive by fiat — an
/// opposite-hand candidate is a different corner, full stop.
const DIRECTION_MISMATCH_COST: f32 = 1e9;

/// Wilson score interval for `matches` out of `laps`, as `(lower, upper)`.
///
/// Exact, no conjugate prior, no soft evidence weights: a 0.6-weighted vote
/// is not a measurement of anything, and the design rejects them.
pub fn wilson_interval(matches: u32, laps: u32, z: f64) -> (f64, f64) {
    if laps == 0 {
        return (0.0, 1.0);
    }
    let n = laps as f64;
    let p = (matches as f64 / n).clamp(0.0, 1.0);
    let z2 = z * z;
    let centre = p + z2 / (2.0 * n);
    let spread = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    let denom = 1.0 + z2 / n;
    ((centre - spread) / denom, (centre + spread) / denom)
}

/// "More likely than not present, demonstrated with confidence."
pub fn majority_confirmed(matches: u32, laps: u32) -> bool {
    laps > 0 && wilson_interval(matches, laps, WILSON_Z).0 > 0.5
}

/// Circular distance between two ring positions, metres.
pub fn ring_dist(a: f32, b: f32, track_length_m: f32) -> f32 {
    let d = (a - b).abs().rem_euclid(track_length_m);
    d.min(track_length_m - d)
}

/// One lap's observation of one corner: what Stage 1 produced, trimmed to
/// the fields the model keeps per-corner geometry in.
#[derive(Debug, Clone, Copy)]
pub struct CornerObservation {
    pub start_m: f32,
    pub end_m: f32,
    pub apex_m: f32,
    pub heading_apex_m: f32,
    pub direction: CornerDirection,
    pub turn_angle: f32,
    pub peak_curvature: f32,
}

impl CornerObservation {
    pub fn from_arc(arc: &CandidateArc) -> Self {
        Self {
            start_m: arc.start_m,
            end_m: arc.end_m,
            apex_m: arc.apex_m,
            heading_apex_m: arc.heading_apex_m,
            direction: arc.direction,
            turn_angle: arc.turn_angle,
            peak_curvature: arc.peak_curvature,
        }
    }

    /// Midpoint on the ring, derived from the span so the aligner and the
    /// geometry medians agree on what "position" means.
    pub fn midpoint_m(&self, track_length_m: f32) -> f32 {
        let span = (self.end_m - self.start_m).rem_euclid(track_length_m);
        (self.start_m + span / 2.0).rem_euclid(track_length_m)
    }
}

/// How a lap participates in the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapStanding {
    /// Typical lap time, clean: votes on existence *and* contributes
    /// geometry to the running medians.
    Representative,
    /// Outlier lap: votes on existence, contributes no geometry.
    Atypical,
}

/// A confirmed corner, with per-field medians over the representative laps.
#[derive(Debug, Clone)]
pub struct ConsensusCorner {
    pub start_m: f32,
    pub end_m: f32,
    pub apex_m: f32,
    pub heading_apex_m: f32,
    pub direction: CornerDirection,
    pub turn_angle: f32,
    pub peak_curvature: f32,
    /// Laps that matched this corner (existence votes, atypical included).
    pub support: u32,
    /// Laps this corner has been exposed to since it entered the model.
    pub laps_seen: u32,
    /// `support / laps_seen`.
    pub match_fraction: f32,
}

/// Learning state for one tentative-or-confirmed corner.
struct CornerState {
    direction: CornerDirection,
    /// Matched observations, as `(lap index, observation, geometry-eligible)`.
    entries: Vec<(usize, CornerObservation, bool)>,
    votes: u32,
    /// Laps processed since (and including) the lap that introduced this
    /// corner. Laps that ran before it existed never had a chance to vote
    /// on it, and counting them as misses would make a corner the first lap
    /// missed permanently unconfirmable.
    laps_seen: u32,
}

impl CornerState {
    fn new(lap: usize, obs: CornerObservation, standing: LapStanding) -> Self {
        Self {
            direction: obs.direction,
            entries: vec![(lap, obs, standing == LapStanding::Representative)],
            votes: 1,
            laps_seen: 1,
        }
    }

    fn record(&mut self, lap: usize, obs: CornerObservation, standing: LapStanding) {
        self.entries.push((lap, obs, standing == LapStanding::Representative));
        self.votes += 1;
    }

    /// Median midpoint of the matched observations — the corner's position.
    fn midpoint(&self, track_length_m: f32) -> f32 {
        let mut mids: Vec<f32> = self
            .entries
            .iter()
            .map(|(_, o, _)| o.midpoint_m(track_length_m))
            .collect();
        mids.sort_by(|a, b| a.total_cmp(b));
        // Circular median of a tight cluster: sorting linearly is fine
        // because cluster spread is bounded by τ, well under half a track.
        mids[mids.len() / 2]
    }

    /// Matching tolerance: the cluster's own MAD, floored at the resolution
    /// limit — as everywhere in this design.
    fn tau(&self, track_length_m: f32) -> f32 {
        let mids: Vec<f32> = self
            .entries
            .iter()
            .map(|(_, o, _)| o.midpoint_m(track_length_m))
            .collect();
        stats::sigma_from_mad(&mids, RESOLUTION_FLOOR_M)
    }
}

/// The running cross-lap model. Each lap talks only to this model — never to
/// another lap — which is what keeps the learner O(laps) rather than the
/// O(laps²) all-pairs master-lap comparison the old design's D11 defect
/// described.
pub struct ConsensusLearner {
    track_length_m: f32,
    corners: Vec<CornerState>,
    laps: u32,
}

impl ConsensusLearner {
    pub fn new(track_length_m: f32) -> Self {
        Self {
            track_length_m,
            corners: Vec::new(),
            laps: 0,
        }
    }

    pub fn laps(&self) -> u32 {
        self.laps
    }

    /// Feed one lap's observations into the model.
    pub fn add_lap(&mut self, obs: &[CornerObservation], standing: LapStanding) {
        self.laps += 1;
        let lap = self.laps as usize - 1;

        if self.corners.is_empty() {
            // The first lap seeds the model; every candidate is tentative.
            for o in obs {
                self.corners.push(CornerState::new(lap, *o, standing));
            }
            self.sort_corners();
            return;
        }

        let assignment = self.align(obs);
        let mut matched_obs = vec![false; obs.len()];

        for (ci, oi) in assignment.iter().enumerate() {
            if let Some(oi) = oi {
                matched_obs[*oi] = true;
                self.corners[ci].record(lap, obs[*oi], standing);
                self.corners[ci].laps_seen += 1;
            } else {
                self.corners[ci].laps_seen += 1;
            }
        }

        // Candidates the model has no corner for enter as tentative; if they
        // recur they will clear the bound, and if not they never confirm.
        for (oi, o) in obs.iter().enumerate() {
            if !matched_obs[oi] {
                self.corners.push(CornerState::new(lap, *o, standing));
            }
        }
        self.sort_corners();
    }

    /// Keep the model in ring order — the aligner relies on it.
    fn sort_corners(&mut self) {
        let len = self.track_length_m;
        self.corners
            .sort_by(|a, b| a.midpoint(len).total_cmp(&b.midpoint(len)));
    }

    /// Align one lap's candidates to the model: for each model corner, the
    /// index of the candidate it matched, if any.
    ///
    /// Both sequences are cyclic. The DP cuts the candidate sequence at each
    /// possible position in turn (a cut between two candidates can never
    /// split a match) and rotates the model to the corner nearest the cut,
    /// then runs a standard Needleman–Wunsch pass; the best cut wins. Lists
    /// are tens of items, so trying every cut is free.
    fn align(&self, obs: &[CornerObservation]) -> Vec<Option<usize>> {
        let len = self.track_length_m;
        let m = self.corners.len();
        let c = obs.len();
        if c == 0 {
            return vec![None; m];
        }

        // Candidates in ring order.
        let mut order: Vec<usize> = (0..c).collect();
        order.sort_by(|&a, &b| {
            obs[a]
                .midpoint_m(len)
                .total_cmp(&obs[b].midpoint_m(len))
        });

        let taus: Vec<f32> = self.corners.iter().map(|k| k.tau(len)).collect();
        let tau_bar = taus.iter().sum::<f32>() / taus.len() as f32;
        let midpoints: Vec<f32> = self.corners.iter().map(|k| k.midpoint(len)).collect();

        let mut best_cost = f32::INFINITY;
        let mut best_assignment: Vec<Option<usize>> = vec![None; m];

        for cut in 0..c {
            // Candidates linearised at this cut.
            let seq: Vec<usize> = (0..c).map(|k| order[(cut + k) % c]).collect();

            // Rotate the model to the corner nearest the first candidate, so
            // a gap run crossing the model's index 0 is not charged twice.
            let anchor = obs[seq[0]].midpoint_m(len);
            let m0 = (0..m)
                .min_by(|&a, &b| {
                    ring_dist(midpoints[a], anchor, len)
                        .total_cmp(&ring_dist(midpoints[b], anchor, len))
                })
                .unwrap_or(0);
            let model: Vec<usize> = (0..m).map(|k| (m0 + k) % m).collect();

            // Needleman–Wunsch. `d[i][j]` = cost of aligning model[..i] to
            // candidates[..j].
            let mut d = vec![vec![f32::INFINITY; c + 1]; m + 1];
            let mut from = vec![vec![0u8; c + 1]; m + 1];
            d[0][0] = 0.0;
            for i in 1..=m {
                let del = GAP_PENALTY_MULT * taus[model[i - 1]].powi(2);
                d[i][0] = d[i - 1][0] + del;
                from[i][0] = 1;
            }
            for j in 1..=c {
                d[0][j] = d[0][j - 1] + GAP_PENALTY_MULT * tau_bar.powi(2);
                from[0][j] = 2;
            }
            for i in 1..=m {
                for j in 1..=c {
                    let corner = &self.corners[model[i - 1]];
                    let cand = &obs[seq[j - 1]];
                    let match_cost = if cand.direction != corner.direction {
                        DIRECTION_MISMATCH_COST
                    } else {
                        let dist = ring_dist(cand.midpoint_m(len), midpoints[model[i - 1]], len);
                        (dist / taus[model[i - 1]]).powi(2)
                    };
                    let del = GAP_PENALTY_MULT * taus[model[i - 1]].powi(2);
                    let ins = GAP_PENALTY_MULT * tau_bar.powi(2);

                    let mut best = d[i - 1][j - 1] + match_cost;
                    let mut step = 0u8;
                    if d[i - 1][j] + del < best {
                        best = d[i - 1][j] + del;
                        step = 1;
                    }
                    if d[i][j - 1] + ins < best {
                        best = d[i][j - 1] + ins;
                        step = 2;
                    }
                    d[i][j] = best;
                    from[i][j] = step;
                }
            }

            if d[m][c] < best_cost {
                best_cost = d[m][c];
                // Backtrack into a model-indexed assignment.
                let mut assignment: Vec<Option<usize>> = vec![None; m];
                let (mut i, mut j) = (m, c);
                while i > 0 || j > 0 {
                    match from[i][j] {
                        0 => {
                            assignment[model[i - 1]] = Some(seq[j - 1]);
                            i -= 1;
                            j -= 1;
                        }
                        1 => i -= 1,
                        _ => j -= 1,
                    }
                }
                best_assignment = assignment;
            }
        }

        best_assignment
    }

    /// The confirmed corners, in ring order, with per-field medians.
    ///
    /// Medians, not means, so one wild lap cannot drag an apex — and
    /// per-field, not "one reference lap wholesale", so the geometry is a
    /// lap's actual value wherever a real lap sat at the median. Nothing is
    /// deleted within a session: corners that never cleared the bound are
    /// simply not confirmed, and tentative corners stay in the model to
    /// receive later laps' votes.
    pub fn confirmed(&self) -> Vec<ConsensusCorner> {
        self.corners
            .iter()
            .filter(|k| majority_confirmed(k.votes, k.laps_seen))
            .map(|k| {
                let geometry: Vec<&CornerObservation> = {
                    let eligible: Vec<_> = k
                        .entries
                        .iter()
                        .filter(|(_, _, geom)| *geom)
                        .map(|(_, o, _)| o)
                        .collect();
                    // A corner every representative lap missed but an
                    // atypical lap found still exists; fall back to all
                    // observations rather than emitting NaN geometry.
                    if eligible.is_empty() {
                        k.entries.iter().map(|(_, o, _)| o).collect()
                    } else {
                        eligible
                    }
                };
                let median_field = |extract: fn(&CornerObservation) -> f32| -> f32 {
                    stats::median(
                        &geometry
                            .iter()
                            .map(|o| extract(*o))
                            .collect::<Vec<f32>>(),
                    )
                };
                ConsensusCorner {
                    start_m: median_field(|o| o.start_m),
                    end_m: median_field(|o| o.end_m),
                    apex_m: median_field(|o| o.apex_m),
                    heading_apex_m: median_field(|o| o.heading_apex_m),
                    direction: k.direction,
                    turn_angle: median_field(|o| o.turn_angle),
                    peak_curvature: median_field(|o| o.peak_curvature),
                    support: k.votes,
                    laps_seen: k.laps_seen,
                    match_fraction: k.votes as f32 / k.laps_seen.max(1) as f32,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Observation at a given midpoint with a symmetric span.
    fn obs_at(midpoint: f32, span: f32, direction: CornerDirection) -> CornerObservation {
        CornerObservation {
            start_m: midpoint - span / 2.0,
            end_m: midpoint + span / 2.0,
            apex_m: midpoint,
            heading_apex_m: midpoint - 3.0,
            direction,
            turn_angle: if direction == CornerDirection::Right {
                1.5
            } else {
                -1.5
            },
            peak_curvature: 0.02,
        }
    }

    const LEN: f32 = 4000.0;

    #[test]
    fn the_confirmation_schedule_is_strict_then_lenient() {
        // (laps, matches) → confirmed? Derived from the Wilson bound, not a
        // lookup: this test is the schedule's specification.
        let cases: &[(u32, u32, bool)] = &[
            (2, 2, true),
            (2, 1, false),
            (3, 3, true),
            (3, 2, false),
            (5, 4, true),
            (5, 3, false),
            (6, 5, true),
            (6, 4, false),
            (8, 6, true),
            (8, 5, false),
            (10, 8, true),
            (10, 7, false),
        ];
        for &(laps, matches, want) in cases {
            assert_eq!(
                majority_confirmed(matches, laps),
                want,
                "{matches}/{laps} should {}",
                if want { "confirm" } else { "refuse" }
            );
        }
    }

    #[test]
    fn a_corner_every_lap_agrees_on_is_confirmed_with_full_support() {
        let mut learner = ConsensusLearner::new(LEN);
        for _ in 0..3 {
            learner.add_lap(
                &[obs_at(500.0, 80.0, CornerDirection::Right)],
                LapStanding::Representative,
            );
        }
        let confirmed = learner.confirmed();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].support, 3);
        assert!((confirmed[0].match_fraction - 1.0).abs() < 1e-6);
        assert!((confirmed[0].apex_m - 500.0).abs() < 1e-3);
    }

    #[test]
    fn a_corner_only_one_lap_saw_is_not_confirmed() {
        let mut learner = ConsensusLearner::new(LEN);
        learner.add_lap(
            &[
                obs_at(500.0, 80.0, CornerDirection::Right),
                obs_at(1345.0, 40.0, CornerDirection::Right), // the phantom
            ],
            LapStanding::Representative,
        );
        for _ in 0..2 {
            learner.add_lap(
                &[obs_at(500.0, 80.0, CornerDirection::Right)],
                LapStanding::Representative,
            );
        }
        let confirmed = learner.confirmed();
        assert_eq!(confirmed.len(), 1, "the phantom must not confirm");
        assert!((confirmed[0].apex_m - 500.0).abs() < 10.0);
    }

    #[test]
    fn an_opposite_hand_candidate_is_a_different_corner() {
        // Four laps of a right-hander, then one lap turning the other way at
        // the same place. The right-hander confirms at 4/5 — the left-hand
        // lap is a miss, as it should be, since it is evidence *against*
        // this corner; the left-hander itself is a tentative corner that
        // never recurs, and a single 1/1 sighting does not clear the bound.
        let mut learner = ConsensusLearner::new(LEN);
        for _ in 0..4 {
            learner.add_lap(
                &[obs_at(500.0, 80.0, CornerDirection::Right)],
                LapStanding::Representative,
            );
        }
        learner.add_lap(
            &[obs_at(500.0, 80.0, CornerDirection::Left)],
            LapStanding::Representative,
        );

        let confirmed = learner.confirmed();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(
            confirmed[0].direction,
            CornerDirection::Right,
            "a left-hander must not support a right-hander"
        );
        assert_eq!(confirmed[0].support, 4);
    }

    #[test]
    fn a_spurious_insertion_does_not_shift_the_alignment() {
        // Lap 2 manufactures a spin artefact between two real corners. A
        // greedy matcher would shift every subsequent slot; the DP must not.
        let mut learner = ConsensusLearner::new(LEN);
        learner.add_lap(
            &[
                obs_at(500.0, 80.0, CornerDirection::Right),
                obs_at(1500.0, 80.0, CornerDirection::Left),
                obs_at(2500.0, 80.0, CornerDirection::Right),
            ],
            LapStanding::Representative,
        );
        learner.add_lap(
            &[
                obs_at(505.0, 80.0, CornerDirection::Right),
                obs_at(1100.0, 30.0, CornerDirection::Right), // spin artefact
                obs_at(1495.0, 80.0, CornerDirection::Left),
                obs_at(2505.0, 80.0, CornerDirection::Right),
            ],
            LapStanding::Representative,
        );
        learner.add_lap(
            &[
                obs_at(498.0, 80.0, CornerDirection::Right),
                obs_at(1502.0, 80.0, CornerDirection::Left),
                obs_at(2498.0, 80.0, CornerDirection::Right),
            ],
            LapStanding::Representative,
        );

        let confirmed = learner.confirmed();
        assert_eq!(confirmed.len(), 3, "all three real corners, artefact voted out");
        // Each with full support: the artefact became a tentative corner that
        // never recurred, not a slot shift.
        assert!(confirmed.iter().all(|c| c.support == 3));
    }

    #[test]
    fn a_lap_that_splits_one_corner_in_two_matches_once() {
        // Where a lap splits what another merged, both halves land near the
        // same model corner; that is one lap agreeing, not two.
        let mut learner = ConsensusLearner::new(LEN);
        learner.add_lap(
            &[obs_at(2000.0, 120.0, CornerDirection::Right)],
            LapStanding::Representative,
        );
        learner.add_lap(
            &[
                obs_at(1960.0, 50.0, CornerDirection::Right),
                obs_at(2040.0, 50.0, CornerDirection::Right),
            ],
            LapStanding::Representative,
        );
        learner.add_lap(
            &[obs_at(2000.0, 120.0, CornerDirection::Right)],
            LapStanding::Representative,
        );

        let confirmed = learner.confirmed();
        // The merged corner confirms at 3/3; the stray half is tentative at
        // best and must not appear.
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].support, 3);
    }

    #[test]
    fn an_atypical_lap_votes_but_does_not_drag_the_geometry() {
        let mut learner = ConsensusLearner::new(LEN);
        learner.add_lap(
            &[obs_at(500.0, 80.0, CornerDirection::Right)],
            LapStanding::Representative,
        );
        // Atypical lap: matched the corner but with a wild geometry.
        learner.add_lap(
            &[obs_at(560.0, 40.0, CornerDirection::Right)],
            LapStanding::Atypical,
        );
        learner.add_lap(
            &[obs_at(500.0, 80.0, CornerDirection::Right)],
            LapStanding::Representative,
        );

        let confirmed = learner.confirmed();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].support, 3, "the atypical lap still voted");
        // Median of representative geometry (500, 500) — not dragged to 560.
        assert!((confirmed[0].apex_m - 500.0).abs() < 1e-3);
    }

    #[test]
    fn a_corner_the_first_lap_missed_can_still_confirm() {
        // Lap 1 merged a complex into one arc; laps 2 and 3 saw two corners.
        // The second corner enters at lap 2 with 2/2 — which clears the
        // bound. Counting lap 1 as a miss would make it unconfirmable, which
        // is why `laps_seen` starts at the introducing lap.
        let mut learner = ConsensusLearner::new(LEN);
        learner.add_lap(
            &[obs_at(2000.0, 140.0, CornerDirection::Right)],
            LapStanding::Representative,
        );
        for _ in 0..2 {
            learner.add_lap(
                &[
                    obs_at(1960.0, 60.0, CornerDirection::Right),
                    obs_at(2040.0, 60.0, CornerDirection::Right),
                ],
                LapStanding::Representative,
            );
        }
        let confirmed = learner.confirmed();
        assert!(
            confirmed.len() >= 2,
            "both members of the complex should confirm: {confirmed:#?}"
        );
    }

    #[test]
    fn corners_wrap_around_the_ring() {
        // A corner at 3950 m and one at 30 m are 80 m apart on the ring.
        assert!((ring_dist(3950.0, 30.0, LEN) - 80.0).abs() < 1e-3);
        assert!((ring_dist(30.0, 3950.0, LEN) - 80.0).abs() < 1e-3);

        // And the aligner must match across the seam: lap 2's corner at 10 m
        // is the same corner as lap 1's at 3990 m.
        let mut learner = ConsensusLearner::new(LEN);
        let mut near_seam = obs_at(3990.0, 80.0, CornerDirection::Right);
        near_seam.start_m = 3950.0;
        near_seam.end_m = 4030.0; // wrapped span, see CandidateArc
        learner.add_lap(&[near_seam], LapStanding::Representative);
        learner.add_lap(
            &[obs_at(20.0, 80.0, CornerDirection::Right)],
            LapStanding::Representative,
        );
        let confirmed = learner.confirmed();
        assert_eq!(confirmed.len(), 1, "the seam corner must not double");
    }

    #[test]
    fn the_wilson_interval_is_sane_at_the_extremes() {
        assert_eq!(wilson_interval(0, 0, WILSON_Z), (0.0, 1.0));
        let (lo, hi) = wilson_interval(3, 3, WILSON_Z);
        assert!(lo > 0.5 && lo < 1.0 && hi <= 1.0);
        let (lo, hi) = wilson_interval(0, 4, WILSON_Z);
        assert!(lo == 0.0 && hi < 0.5);
    }
}
