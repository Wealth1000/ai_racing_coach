//! Corner detection from a resampled lap.
//!
//! A hysteresis state machine over the smoothed curvature magnitude: cross an
//! adaptive threshold to enter a corner, stay below it for a sustained distance
//! to leave, then merge corners separated by less than a car-length-ish gap.
//!
//! # What changed from the first implementation
//!
//! Three corrections, all forced by the Red Bull Ring captures:
//!
//! 1. **Direction was inverted.** The old test read `heading_change > 0.0 =>
//!    Left`. Measured on both cars, positive Δheading is a *right* turn: every
//!    clean lap nets +2π and Red Bull Ring runs clockwise, and the ground-plane
//!    cross product agrees with the sign of Δheading on 99.2% / 98.8% of
//!    samples. With the old test the circuit came out 8 left / 2 right, which is
//!    the mirror image of the truth.
//! 2. **Gating smoothed the signed curvature** and took the absolute value
//!    afterwards. See [`curvature::corner_profiles`] — those operations do not
//!    commute, and in a chicane the two directions cancel.
//! 3. **The merge gap was 15 m**, which splits single corners in two. The MX5
//!    line has 18 m between T1 and T2's detected spans, 16 m at T5/T6 and 16 m
//!    at T9/T10; at 15 m the detector reports 13 corners for a 10-corner
//!    circuit. 25 m closes those without touching the F138's genuine 47 m gap
//!    at T7/T8.
//!
//! The threshold constant also disagreed with its own doc comment — it said 30%
//! of the 95th percentile and computed 15%.
//!
//! # Corner boundaries are per-lap, not per-track
//!
//! What comes out of here describes *the line this lap took*. Two cars on the
//! same circuit legitimately disagree: the F138 carries so much more speed that
//! its T7 and T8 spans end up 47 m apart where the MX5's are 16 m, so no single
//! merge distance makes both report ten corners. Deciding on one canonical set
//! of corners per track is a separate job — pick a reference lap, detect on it,
//! and store it — and it belongs to `TrackModel`, not here.

use serde::Serialize;

use crate::core::ids::CornerId;
use crate::core::sample::Sample;
use crate::features::curvature;
use crate::features::resample::ResampledLap;

/// Which way the corner goes.
///
/// Positive curvature and positive Δheading are **right** in Assetto Corsa's
/// left-handed world. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum CornerDirection {
    Left,
    Right,
}

impl CornerDirection {
    /// Name the direction from a signed rotation. Positive is right.
    pub fn from_signed(rotation: f32) -> Self {
        if rotation > 0.0 {
            CornerDirection::Right
        } else {
            CornerDirection::Left
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            CornerDirection::Left => "L",
            CornerDirection::Right => "R",
        }
    }
}

/// One corner, as taken on one lap. All distances are metres from the line.
#[derive(Debug, Clone, Serialize)]
pub struct TrackCorner {
    pub id: CornerId,
    pub start_m: f32,
    pub end_m: f32,
    /// Point of highest curvature — the geometric apex.
    pub apex_m: f32,
    /// Point of fastest heading change, which is usually a little earlier.
    pub heading_apex_m: f32,
    pub direction: CornerDirection,
    /// Peak smoothed curvature magnitude, 1/m.
    pub peak_curvature: f32,
    /// Total rotation through the corner, radians. Positive is right.
    pub turn_angle: f32,
    /// Lowest speed within the corner, m/s.
    pub min_speed: f32,
}

impl TrackCorner {
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
}

/// Detection knobs.
///
/// The two that actually change the answer on this data are
/// [`Self::merge_gap_m`] and [`Self::threshold_fraction`]; the rest are
/// guardrails that no corner at Red Bull Ring comes near.
#[derive(Debug, Clone, Copy)]
pub struct CornerParams {
    /// Threshold as a fraction of the 95th-percentile curvature.
    pub threshold_fraction: f32,
    /// Floor on that threshold, 1/m, so a lap of pure straights finds nothing.
    pub min_threshold: f32,
    /// Sustained distance below threshold before a corner is declared over.
    pub exit_hysteresis_m: f32,
    /// Shorter spans than this are noise, not corners.
    pub min_corner_length_m: f32,
    /// A corner must reach either this curvature or [`Self::min_turn_angle`].
    pub min_peak_curvature: f32,
    /// ...or turn through at least this much, in radians.
    pub min_turn_angle: f32,
    /// Corners closer than this are one corner.
    pub merge_gap_m: f32,
}

impl Default for CornerParams {
    fn default() -> Self {
        Self {
            // The code used to say 30% in the comment and compute 15%. 30% is
            // the value the validated prototype ran with.
            threshold_fraction: 0.30,
            min_threshold: 0.002,
            exit_hysteresis_m: 10.0,
            min_corner_length_m: 30.0,
            min_peak_curvature: 0.003,
            min_turn_angle: 0.10,
            // 15 m split single corners in three places on the MX5 line.
            merge_gap_m: 25.0,
        }
    }
}

/// Adaptive threshold: a fraction of the 95th-percentile curvature magnitude.
///
/// Percentile rather than mean because a lap is mostly straight, and adaptive
/// rather than fixed because a 60 m hairpin and a 400 m radius sweeper differ by
/// an order of magnitude in curvature and both are corners.
pub fn adaptive_threshold(magnitude: &[f32], params: &CornerParams) -> f32 {
    if magnitude.is_empty() {
        return params.min_threshold;
    }
    let mut sorted: Vec<f32> = magnitude.iter().map(|c| c.abs()).collect();
    // total_cmp, not partial_cmp().unwrap(): one NaN used to panic here.
    sorted.sort_by(|a, b| a.total_cmp(b));

    let idx = ((sorted.len() - 1) as f32 * 0.95).round() as usize;
    let p95 = sorted[idx.min(sorted.len() - 1)];
    if !p95.is_finite() {
        return params.min_threshold;
    }
    (p95 * params.threshold_fraction).max(params.min_threshold)
}

/// Detect corners on a resampled lap, with default parameters.
pub fn detect_corners(lap: &ResampledLap) -> Vec<TrackCorner> {
    detect_corners_with(lap, &CornerParams::default())
}

/// Detect corners on a resampled lap.
pub fn detect_corners_with(lap: &ResampledLap, params: &CornerParams) -> Vec<TrackCorner> {
    let samples = &lap.samples;
    // 10 grid points is 10 m; nothing meaningful can be said about less.
    if samples.len() < 10 {
        return Vec::new();
    }

    let profiles = curvature::corner_profiles(samples, lap.step_m);
    let rotation = curvature::cumulative_rotation(samples);
    let threshold = adaptive_threshold(&profiles.magnitude, params);

    // Distances become index counts: the grid is uniform, so this is exact
    // rather than the outward-scanning the old distance-window code did.
    let exit_samples = ((params.exit_hysteresis_m / lap.step_m).round() as usize).max(1);
    let min_length_samples = (params.min_corner_length_m / lap.step_m).round() as usize;

    let mut spans = Vec::new();
    let mut open: Option<Span> = None;

    for i in 0..samples.len() {
        let curv = profiles.magnitude[i];

        if curv > threshold {
            match &mut open {
                None => open = Some(Span::new(i, curv, profiles.heading_change[i].abs())),
                Some(span) => span.observe(i, curv, profiles.heading_change[i].abs()),
            }
        } else if let Some(span) = &mut open {
            span.below += 1;
            if span.below >= exit_samples {
                // `span.end` is the last point that was *above* threshold, so the
                // hysteresis tail is already excluded — reporting the corner as
                // ending here would make every corner exit_hysteresis_m too long.
                spans.push(open.take().expect("checked above"));
            }
        }
    }
    // A corner still open at the end of the lap is a real corner: the last turn
    // of a circuit runs right up to the line.
    if let Some(mut span) = open.take() {
        span.end = samples.len() - 1;
        spans.push(span);
    }

    let mut corners: Vec<TrackCorner> = spans
        .into_iter()
        .filter(|span| {
            span.end.saturating_sub(span.start) >= min_length_samples
                && (span.peak_curvature > params.min_peak_curvature
                    || span.peak_heading_change > params.min_turn_angle)
        })
        .map(|span| span.into_corner(samples, &rotation))
        .collect();

    merge_close_corners(&mut corners, params.merge_gap_m);

    for (i, corner) in corners.iter_mut().enumerate() {
        corner.id = CornerId(i as u32);
    }
    corners
}

/// A corner under construction: indices into the grid, plus running extremes.
struct Span {
    start: usize,
    end: usize,
    apex: usize,
    heading_apex: usize,
    peak_curvature: f32,
    peak_heading_change: f32,
    /// Consecutive grid points below threshold, for the exit hysteresis.
    below: usize,
}

impl Span {
    fn new(i: usize, curv: f32, heading_change: f32) -> Self {
        Self {
            start: i,
            end: i,
            apex: i,
            heading_apex: i,
            peak_curvature: curv,
            peak_heading_change: heading_change,
            below: 0,
        }
    }

    fn observe(&mut self, i: usize, curv: f32, heading_change: f32) {
        self.end = i;
        self.below = 0;
        if curv > self.peak_curvature {
            self.peak_curvature = curv;
            self.apex = i;
        }
        if heading_change > self.peak_heading_change {
            self.peak_heading_change = heading_change;
            self.heading_apex = i;
        }
    }

    fn into_corner(self, samples: &[Sample], rotation: &[f32]) -> TrackCorner {
        // Direction from the *net rotation through the whole corner* rather than
        // from one sample of a smoothed profile: it is the quantity a driver
        // would agree with, and it cannot be flipped by a single noisy point.
        let turn_angle = rotation[self.end] - rotation[self.start];

        let min_speed = samples[self.start..=self.end]
            .iter()
            .map(|s| s.speed)
            .fold(f32::INFINITY, f32::min);

        TrackCorner {
            // Renumbered after merging.
            id: CornerId(0),
            start_m: samples[self.start].lap_distance,
            end_m: samples[self.end].lap_distance,
            apex_m: samples[self.apex].lap_distance,
            heading_apex_m: samples[self.heading_apex].lap_distance,
            direction: CornerDirection::from_signed(turn_angle),
            peak_curvature: self.peak_curvature,
            turn_angle,
            min_speed: if min_speed.is_finite() { min_speed } else { 0.0 },
        }
    }
}

/// Fuse corners separated by less than `merge_gap_m`.
///
/// The merged corner keeps the *stronger* of the two apexes, so merging T1's
/// entry arc into T1 proper does not move the apex out to the weaker end.
fn merge_close_corners(corners: &mut Vec<TrackCorner>, merge_gap_m: f32) {
    let mut i = 0;
    while i + 1 < corners.len() {
        let gap = corners[i + 1].start_m - corners[i].end_m;
        if gap < merge_gap_m {
            let next = corners.remove(i + 1);
            let cur = &mut corners[i];

            cur.end_m = next.end_m;
            cur.turn_angle += next.turn_angle;
            cur.min_speed = cur.min_speed.min(next.min_speed);

            if next.peak_curvature > cur.peak_curvature {
                cur.apex_m = next.apex_m;
                cur.heading_apex_m = next.heading_apex_m;
                cur.peak_curvature = next.peak_curvature;
            }
            // Direction is re-derived from the combined rotation: merging an
            // entry kink of the opposite hand into a corner must not leave the
            // corner named after the kink.
            cur.direction = CornerDirection::from_signed(cur.turn_angle);
        } else {
            i += 1;
        }
    }
}

/// Corner counts by direction, for reporting.
pub fn direction_counts(corners: &[TrackCorner]) -> (usize, usize) {
    let right = corners
        .iter()
        .filter(|c| c.direction == CornerDirection::Right)
        .count();
    (corners.len() - right, right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::math::wrap_pi;
    use crate::features::resample::ResampledLap;

    /// Build a synthetic lap on a 1 m grid from a curvature programme:
    /// `(length_m, signed_curvature)` segments, integrated into a path.
    fn lap_from_curvature(program: &[(f32, f32)]) -> ResampledLap {
        let mut samples = Vec::new();
        let mut heading = 0.0f32;
        let mut x = 0.0f32;
        let mut z = 0.0f32;
        let mut d = 0.0f32;

        for (length, k) in program {
            let steps = *length as usize;
            for _ in 0..steps {
                // Left-handed X/Z: positive curvature must turn right, which
                // means heading grows and the path bends towards +X.
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

        ResampledLap {
            samples,
            step_m: 1.0,
            non_monotone_dropped: 0,
        }
    }

    /// A 90-degree right-hander: 1/50 1/m over ~79 m turns pi/2.
    fn right_90() -> (f32, f32) {
        (78.5, 1.0 / 50.0)
    }

    fn left_90() -> (f32, f32) {
        (78.5, -1.0 / 50.0)
    }

    #[test]
    fn a_right_hander_is_reported_as_right() {
        // This is the regression test for the inverted direction test. With the
        // old `heading_change > 0.0 => Left` this comes out Left.
        let lap = lap_from_curvature(&[(300.0, 0.0), right_90(), (300.0, 0.0)]);
        let corners = detect_corners(&lap);
        assert_eq!(corners.len(), 1, "expected one corner, got {corners:#?}");
        assert_eq!(corners[0].direction, CornerDirection::Right);
        assert!(
            corners[0].turn_angle > 0.0,
            "a right turn must have positive rotation, got {}",
            corners[0].turn_angle
        );
        assert!(
            (corners[0].turn_degrees() - 90.0).abs() < 15.0,
            "expected ~90 deg, got {}",
            corners[0].turn_degrees()
        );
    }

    #[test]
    fn a_left_hander_is_reported_as_left() {
        let lap = lap_from_curvature(&[(300.0, 0.0), left_90(), (300.0, 0.0)]);
        let corners = detect_corners(&lap);
        assert_eq!(corners.len(), 1, "expected one corner, got {corners:#?}");
        assert_eq!(corners[0].direction, CornerDirection::Left);
        assert!(corners[0].turn_angle < 0.0);
    }

    #[test]
    fn corners_separated_by_a_short_gap_become_one() {
        // 16 m apart: exactly the MX5's T5/T6 case that 15 m failed to merge.
        let lap = lap_from_curvature(&[
            (300.0, 0.0),
            right_90(),
            (16.0, 0.0),
            right_90(),
            (300.0, 0.0),
        ]);
        let corners = detect_corners(&lap);
        assert_eq!(
            corners.len(),
            1,
            "a 16 m gap must merge at 25 m; got {} corners",
            corners.len()
        );
        // The merged corner turns through both halves.
        assert!(
            corners[0].turn_degrees() > 150.0,
            "merged turn angle should be ~180 deg, got {}",
            corners[0].turn_degrees()
        );
    }

    #[test]
    fn corners_separated_by_a_long_gap_stay_apart() {
        // The F138's T7/T8 gap. It must survive the merge.
        let lap = lap_from_curvature(&[
            (300.0, 0.0),
            right_90(),
            (47.0, 0.0),
            right_90(),
            (300.0, 0.0),
        ]);
        let corners = detect_corners(&lap);
        assert_eq!(corners.len(), 2, "a 47 m gap must not merge at 25 m");
    }

    #[test]
    fn straights_produce_no_corners() {
        let lap = lap_from_curvature(&[(1000.0, 0.0)]);
        assert!(detect_corners(&lap).is_empty());
    }

    #[test]
    fn corner_span_excludes_the_hysteresis_tail() {
        // The corner is 78.5 m of arc. Reporting it as 88 m would mean the
        // 10 m exit hysteresis leaked into the span.
        let lap = lap_from_curvature(&[(300.0, 0.0), right_90(), (300.0, 0.0)]);
        let corners = detect_corners(&lap);
        let len = corners[0].length_m();
        assert!(
            len < 78.5 + 8.0,
            "corner reported {len} m for a 78.5 m arc — hysteresis leaked in"
        );
    }

    #[test]
    fn an_opposite_hand_kink_does_not_rename_the_corner() {
        // A short, weak left immediately before a long, strong right. Merged,
        // the pair must still be called a right.
        let lap = lap_from_curvature(&[
            (300.0, 0.0),
            (30.0, -1.0 / 200.0),
            (5.0, 0.0),
            right_90(),
            (300.0, 0.0),
        ]);
        let corners = detect_corners(&lap);
        assert_eq!(corners.len(), 1);
        assert_eq!(corners[0].direction, CornerDirection::Right);
    }

    #[test]
    fn threshold_survives_a_nan() {
        // The old adaptive_corner_threshold used partial_cmp().unwrap() and
        // panicked here.
        let params = CornerParams::default();
        let t = adaptive_threshold(&[0.01, f32::NAN, 0.05, 0.002], &params);
        assert!(t.is_finite(), "threshold came out {t}");
        assert!(adaptive_threshold(&[], &params) >= params.min_threshold);
    }

    #[test]
    fn a_short_lap_is_refused_rather_than_panicking() {
        // The old compute_curvature did `1..len-1` and underflowed on len 0.
        for n in 0..10 {
            let lap = ResampledLap {
                samples: lap_from_curvature(&[(n as f32, 0.0)]).samples,
                step_m: 1.0,
                non_monotone_dropped: 0,
            };
            assert!(detect_corners(&lap).is_empty(), "n = {n}");
        }
    }
}
