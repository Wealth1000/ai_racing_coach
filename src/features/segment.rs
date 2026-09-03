//! Stage 1 of the canonical corner learner: candidate arcs by MDL
//! segmentation of the rotation profile.
//!
//! # The signal
//!
//! A circuit is, by construction, a sequence of constant-curvature arcs joined
//! by transitions. The cumulative rotation θ(s) — total signed heading change
//! since the lap origin, unwrapped past ±π, already provided by
//! [`curvature::cumulative_rotation`] — is therefore *piecewise linear* in the
//! distance s:
//!
//! * a segment's **slope** is the curvature (sign = direction, 1/|slope| =
//!   radius),
//! * its **vertical extent** is the turn angle,
//! * its **horizontal extent** is the corner's span, so entry and exit come
//!   free as breakpoints.
//!
//! This choice is immune to the D8 failure class (chicane cancellation) *by
//! construction*: θ is the integral of signed curvature, an S-bend is an S in
//! θ — up then down — and nothing is ever averaged across the reversal. There
//! is no smoothing window on the primary signal at all.
//!
//! # The estimator
//!
//! The piecewise-linear model is fitted by minimum description length: choose
//! the breakpoint set minimising `Σ nᵢ·ln(RSSᵢ/nᵢ) + p·|B|·ln(n)` (pass 1) —
//! the Schwarz/BIC penalty, an information-theoretic convention rather than a
//! fitted number — solved exactly with the standard O(n²) dynamic programme
//! over segment costs, each cost O(1) from prefix sums of `s`, `s²`, `θ`,
//! `sθ`. Noise is estimated from the data, per lap, in two passes: first
//! over-segment deliberately (penalty halved), measure the residual scale — at
//! that level the residual is dominated by sensor noise, not missed structure
//! — then re-fit with σ² fixed to that measurement. A 20 Hz noisy source gets
//! a bigger σ, a bigger penalty in absolute terms, and correctly coarser
//! segments. This self-calibration replaces every "fraction of the 95th
//! percentile" knob the old detector had.
//!
//! # Recall, not precision
//!
//! Stage 1 deliberately over-generates. Over-segmentation is cheap — Stage 2
//! ([`crate::features::consensus`]) votes spurious arcs away — but an arc
//! Stage 1 never proposes is unrecoverable downstream. The only magnitude
//! test a run must pass is that some segment in it clears this lap's own
//! curvature noise floor (MAD-derived, §3.3 of the design), which exists to
//! keep the candidate list finite, not to judge "how curved a corner is".
//! A 60 m hairpin and a 400 m sweeper are both arcs.
//!
//! # Corners are gestures, not segments
//!
//! On real telemetry the DP does not produce one segment per corner: a real
//! corner's curvature varies (Curva Grande tightens and opens), and every
//! variation that pays for a breakpoint becomes its own segment. Candidate
//! arcs are therefore *maximal runs of same-direction turning*: consecutive
//! fitted segments whose slope is distinguishable from zero (at the
//! slope-estimator scale σ_κ/√nseg) fold into one arc, so easements glue
//! onto their corner, while a straight — fitted slope ≈ 0 — or a direction
//! reversal ends the run. Chicanes fall out as alternating runs. See the
//! comments at the run fold in [`segment_lap`] for the measured basis.

use crate::core::math::angle_delta;
use crate::features::corner::CornerDirection;
use crate::features::curvature;
use crate::features::resample::ResampledLap;
use crate::features::stats;

/// Two-corner resolution floor, metres.
///
/// A measured property of Menger-style curvature at these sampling densities,
/// not a tuning knob: below ~10 m nothing meaningful can be said about a
/// second corner. Used both as the DP's minimum segment length and (in
/// [`crate::features::consensus`]) as the floor on matching tolerances.
pub const RESOLUTION_FLOOR_M: f32 = 10.0;

/// Modified-z factor for the slope-vs-noise gate and the pedal/sign-lock
/// levels (Iglewicz–Hoaglin convention for MAD-based outlier tests).
pub const MODIFIED_Z: f32 = 3.0;

/// Parameters per segment in the BIC penalty: slope plus breakpoint.
const PARAMS_PER_SEGMENT: f64 = 2.0;

/// Floor on the per-lap curvature noise scale, 1/m.
///
/// Synthetic laps can be *exactly* piecewise linear, where the raw MAD is
/// zero; the gate still needs a (tiny) scale to divide by.
const SIGMA_K_FLOOR: f32 = 1e-6;

/// Floor on the residual variance from pass 1, rad².
///
/// Zero-noise laps make RSS exactly 0 and the log-likelihood infinite; the
/// floor keeps both passes finite without changing any real fit.
const RSS_FLOOR: f64 = 1e-12;

/// Floor on the measured θ-noise variance σ̂², rad² (σ = 1e-4 rad ≈ 0.006°).
///
/// A guard against *numerical* residual structure, not a tuning knob. On a
/// synthetic lap the only residual left after a perfect fit is f32
/// quantisation dust — and that dust is not homogeneous: θ values near ±2π
/// carry ~10× the ulp of θ values near zero, so a corner straddling the
/// rotation origin has a real step in its noise floor exactly at the seam,
/// and an un-floored σ̂² (measured ~1e-15 on such laps) happily spends a
/// breakpoint on it. Real telemetry never gets near this floor (measured
/// θ-noise on AC captures is ~1e-3 rad), and no corner geometry hides under
/// a 1e-4 rad residual, so the floor binds only where the data is more
/// precise than the arithmetic carrying it.
const SIGMA2_FLOOR: f64 = 1e-8;

/// One candidate arc: a segment of the θ(s) fit whose slope cleared the
/// per-lap noise gate.
///
/// Distances are metres on the track ring, wrapped into `[0, track_length)`.
/// A corner straddling the start/finish line legitimately has
/// `end_m < start_m`; consumers that need a linear span must handle the wrap
/// (see [`Self::span_m`] and [`Self::midpoint_m`]).
#[derive(Debug, Clone)]
pub struct CandidateArc {
    pub start_m: f32,
    pub end_m: f32,
    /// Point of highest curvature magnitude inside the span — geometric apex.
    pub apex_m: f32,
    /// Point of fastest windowed heading change inside the span.
    pub heading_apex_m: f32,
    pub direction: CornerDirection,
    /// Signed least-squares slope of the segment, 1/m. Positive is right.
    pub curvature: f32,
    /// Signed rotation through the arc, radians. Positive is right.
    pub turn_angle: f32,
    /// Peak smoothed curvature magnitude inside the span, 1/m.
    pub peak_curvature: f32,
    /// The per-lap curvature noise scale this arc's slope was judged against.
    pub sigma_k: f32,
}

impl CandidateArc {
    /// Span along the ring, metres — correct for seam-straddling arcs.
    pub fn span_m(&self, track_length_m: f32) -> f32 {
        let d = self.end_m - self.start_m;
        if d >= 0.0 {
            d
        } else {
            d + track_length_m
        }
    }

    /// Midpoint along the ring, wrapped into `[0, track_length_m)`.
    pub fn midpoint_m(&self, track_length_m: f32) -> f32 {
        let mid = self.start_m + self.span_m(track_length_m) / 2.0;
        mid.rem_euclid(track_length_m)
    }

    /// Radius implied by the segment slope, metres, or `None` if degenerate.
    pub fn radius_m(&self) -> Option<f32> {
        if self.curvature.abs() > 1e-9 {
            Some(1.0 / self.curvature.abs())
        } else {
            None
        }
    }
}

/// Stage 1 output for one lap.
#[derive(Debug, Clone)]
pub struct Segmentation {
    pub arcs: Vec<CandidateArc>,
    /// Per-lap curvature noise scale σ_κ = 1.4826·MAD(κ), 1/m.
    pub sigma_k: f32,
}

/// Segment one resampled lap into candidate arcs.
///
/// `track_length_m` is the ring modulus: the lap's samples cover it once,
/// and every distance this returns is wrapped into `[0, track_length_m)`.
pub fn segment_lap(lap: &ResampledLap, track_length_m: f32) -> Segmentation {
    let samples = &lap.samples;
    let n = samples.len();
    // 10 grid points is 10 m at the usual 1 m step; nothing meaningful can
    // be said about less.
    if n < 10 || track_length_m <= 0.0 {
        return Segmentation {
            arcs: Vec::new(),
            sigma_k: 0.0,
        };
    }
    let step = lap.step_m;
    let min_seg = ((RESOLUTION_FLOOR_M / step).round() as usize).max(1).min(n);

    // --- The signal: θ(s), made circular --------------------------------
    //
    // cumulative_rotation is linear over the captured lap. The lap is really
    // a ring: the gap between the last and first sample crosses the
    // start/finish line, and its rotation is angle_delta of the two headings.
    let theta = curvature::cumulative_rotation(samples);
    let seam_rotation = theta[n - 1] + angle_delta(samples[n - 1].heading, samples[0].heading);

    // Per-interval curvature: dθ/ds. This is the heading-based curvature
    // estimator; the position-based Menger estimator is kept alongside purely
    // as a cross-check (see `estimator_agreement`).
    let kappa: Vec<f32> = (0..n - 1)
        .map(|i| (theta[i + 1] - theta[i]) / step)
        .collect();
    let sigma_k = stats::sigma_from_mad(&kappa, SIGMA_K_FLOOR);

    // --- Lap origin: longest circular run of sub-noise curvature ---------
    //
    // Distance is circular and the DP wants a linear axis, so the lap is cut
    // at the natural origin: the longest straight (run of intervals whose
    // curvature is below the noise gate). On a track with no straight at all
    // the run is empty and the cut stays at index 0 — the ring matching in
    // Stage 2 makes the choice immaterial.
    let straight = |k: f32| k.abs() < MODIFIED_Z * sigma_k;
    let origin = longest_circular_run(&kappa, straight);

    // Rotated arrays. θ' continues through the seam so a corner spanning
    // the line is one segment, not two; s' accumulates *ring* distances, so
    // the step across the seam is the true start/finish gap rather than a
    // fabricated uniform one — otherwise a corner crossing the line would
    // carry a fake slope discontinuity exactly where it must fuse.
    let mut rot_s = vec![0.0f64; n];
    let mut rot_theta = vec![0.0f64; n];
    let mut rot_dist = vec![0.0f32; n];
    for j in 0..n {
        let idx = origin + j;
        let (idx, wrapped) = (idx % n, idx / n);
        rot_theta[j] = theta[idx] as f64 + wrapped as f64 * seam_rotation as f64;
        rot_dist[j] =
            (samples[idx].lap_distance + wrapped as f32 * track_length_m).rem_euclid(track_length_m);
        if j > 0 {
            let step_along_ring =
                crate::features::consensus::ring_dist(rot_dist[j - 1], rot_dist[j], track_length_m);
            rot_s[j] = rot_s[j - 1] + step_along_ring.max(0.0) as f64;
        }
    }

    // --- Two-pass MDL ----------------------------------------------------
    let prefix = Prefix::new(&rot_s, &rot_theta);
    let pass1 = dp_segments(&prefix, min_seg, PassCost::Mdl {
        penalty: 0.5 * PARAMS_PER_SEGMENT * (n as f64).ln(),
    });
    let rss_total: f64 = pass1
        .iter()
        .map(|&(i, j)| prefix.rss(i, j))
        .sum::<f64>()
        .max(0.0);
    let sigma2 = (rss_total / n as f64).max(SIGMA2_FLOOR);
    let segments = dp_segments(&prefix, min_seg, PassCost::FixedVariance {
        sigma2,
        penalty: PARAMS_PER_SEGMENT * (n as f64).ln(),
    });

    // --- Segments to runs, runs to arcs ----------------------------------
    //
    // The DP describes θ(s) as well as BIC can buy, and on real telemetry
    // that is *not* one segment per corner: a real corner's curvature varies
    // (Curva Grande tightens and opens), and each variation that pays for a
    // breakpoint becomes its own constant-curvature segment. A corner,
    // though, is a gesture — the problem statement counts decisions, and
    // several decisions can live inside one gesture (§5 of the design). The
    // geometric unit the model needs is the maximal run of same-direction
    // turning, from the first sustained rotation to the last, easements
    // included.
    //
    // Two tests, both against this lap's own σ_κ, give each segment a role:
    //
    // * *turning?* — |slope| distinguishable from zero at the scale of the
    //   slope estimator, σ_κ/√nseg. This is the statistically correct scale
    //   for asking whether a fitted slope is zero, and it is deliberately
    //   NOT the cornerhood gate: an entry easement at half the apex
    //   curvature is still turning, still part of the gesture. Measured on
    //   real laps the gaps this bridges carry the same sign at ~90% of the
    //   gate, while a true straight fits at ~1e-5 rad/m, orders below.
    // * *corner-making?* — |slope| above the per-point noise gate 3·σ_κ
    //   (§3.3). A run becomes a candidate only when some segment in it
    //   clears this: a hundred metres of near-straight drift is turning but
    //   is not a corner, and such runs are dropped whole.
    //
    // A straight (fitted slope ≈ 0) fails the turning test and so ends a
    // run; a direction reversal ends it by sign. Chicanes fall out as
    // alternating runs; easements glue onto their corner.
    let profiles = curvature::corner_profiles(samples, step);
    let gate = MODIFIED_Z as f64 * sigma_k as f64;

    // (segment window, slope) for segments classified as turning, in order.
    let mut turning: Vec<((usize, usize), f64)> = Vec::new();
    for &(i, j) in &segments {
        let slope = prefix.slope(i, j);
        let slope_sigma = sigma_k as f64 / ((j - i) as f64).sqrt();
        if slope.abs() > MODIFIED_Z as f64 * slope_sigma {
            turning.push(((i, j), slope));
        }
    }

    // Fold consecutive same-direction turning segments into runs. A segment
    // joins the previous run only if it is *contiguous* with it — the fitted
    // segments tile the lap, so a shared boundary means no neutral segment
    // sits between them; a straight or a reversal in between starts a new
    // run. (Sign alone is not enough: two same-hand corners with a straight
    // between them are consecutive in this list.)
    // The ring cut sits inside the longest straight (sub-gate by
    // construction), so no run wraps the array.
    let mut runs: Vec<Vec<((usize, usize), f64)>> = Vec::new();
    for (window, slope) in turning {
        let joins = matches!(
            (runs.last(), slope),
            (Some(run), s)
                if run.last().map_or(false, |(w, s0)| {
                    w.1 == window.0 && s0.signum() == s.signum()
                })
        );
        if joins {
            runs.last_mut().expect("checked above").push((window, slope));
        } else {
            runs.push(vec![(window, slope)]);
        }
    }

    let mut arcs = Vec::new();
    for run in runs {
        // Cornerhood: some member segment must clear the 3·σ_κ gate.
        if !run.iter().any(|&(_, s)| s.abs() > gate) {
            continue;
        }
        let i = run[0].0 .0;
        let j = run[run.len() - 1].0 .1;
        let span = rot_s[j - 1] - rot_s[i];
        // The run is same-sign throughout, so θ over it is monotone: the
        // turn angle is the θ difference, easement slopes included.
        let turn = rot_theta[j - 1] - rot_theta[i];

        // Apexes: extremes of the existing profiles inside the run, looked
        // up through the rotation (smoothed |κ| for the geometric apex,
        // windowed heading change for the heading apex).
        let (apex_j, heading_apex_j, peak) = {
            let mut best_mag = -1.0f32;
            let mut best_head = -1.0f32;
            let mut apex_j = i;
            let mut head_j = i;
            for jj in i..j {
                let idx = (origin + jj) % n;
                let mag = profiles.magnitude[idx];
                if mag > best_mag {
                    best_mag = mag;
                    apex_j = jj;
                }
                let head = profiles.heading_change[idx].abs();
                if head > best_head {
                    best_head = head;
                    head_j = jj;
                }
            }
            (apex_j, head_j, best_mag.max(0.0))
        };

        let slope = turn / span;
        arcs.push(CandidateArc {
            start_m: rot_dist[i],
            end_m: rot_dist[j - 1],
            apex_m: rot_dist[apex_j],
            heading_apex_m: rot_dist[heading_apex_j],
            direction: CornerDirection::from_signed(slope as f32),
            curvature: slope as f32,
            turn_angle: turn as f32,
            peak_curvature: peak,
            sigma_k,
        });
    }

    Segmentation { arcs, sigma_k }
}

/// Fraction of samples where the two curvature estimators agree in sign.
///
/// The sign-lock canary from the design's preprocessing contract: signed
/// Menger curvature from `pos` and dθ/ds from `heading` are independent
/// estimators of the same physical quantity, and sustained disagreement is
/// the exact signature of a dead or lying channel — the failure that once
/// produced a "9 Right / 0 Left" model four stages downstream. Only intervals
/// where both estimators are above the lap's noise floor vote, so noise
/// cannot manufacture disagreement. Returns 1.0 when there is nothing to
/// compare (a lap of pure straights).
pub fn estimator_agreement(lap: &ResampledLap) -> f32 {
    let samples = &lap.samples;
    let n = samples.len();
    if n < 3 {
        return 1.0;
    }
    let menger = curvature::signed_curvature(samples);
    let theta = curvature::cumulative_rotation(samples);
    let kappa: Vec<f32> = (0..n - 1)
        .map(|i| (theta[i + 1] - theta[i]) / lap.step_m)
        .collect();
    let sigma_k = stats::sigma_from_mad(&kappa, SIGMA_K_FLOOR);

    let mut agree = 0usize;
    let mut compared = 0usize;
    for i in 1..n - 1 {
        if menger[i].abs() > MODIFIED_Z * sigma_k && kappa[i].abs() > MODIFIED_Z * sigma_k {
            compared += 1;
            if menger[i].signum() == kappa[i].signum() {
                agree += 1;
            }
        }
    }
    if compared == 0 {
        1.0
    } else {
        agree as f32 / compared as f32
    }
}

/// Index (into the interval array) starting the longest circular run matching
/// `is_straight`. Empty runs mean the cut stays at 0.
fn longest_circular_run(intervals: &[f32], is_straight: impl Fn(f32) -> bool) -> usize {
    let n = intervals.len();
    if n == 0 {
        return 0;
    }
    // Doubling the array makes circular runs contiguous; capped at 2n.
    let mut best_len = 0usize;
    let mut best_start = 0usize;
    let mut run_len = 0usize;
    let mut run_start = 0usize;
    for i in 0..2 * n {
        if is_straight(intervals[i % n]) {
            if run_len == 0 {
                run_start = i;
            }
            run_len += 1;
            if run_len > best_len {
                best_len = run_len;
                best_start = run_start;
            }
        } else {
            run_len = 0;
        }
    }
    // A run that covers the whole doubled array is the whole ring.
    if best_len >= n {
        return 0;
    }
    best_start % n
}

/// Prefix sums for O(1) least-squares costs over any segment `[i, j)`.
struct Prefix {
    s: Vec<f64>,
    s2: Vec<f64>,
    t: Vec<f64>,
    t2: Vec<f64>,
    st: Vec<f64>,
}

impl Prefix {
    fn new(s: &[f64], t: &[f64]) -> Self {
        let n = s.len();
        let mut p = Self {
            s: vec![0.0; n + 1],
            s2: vec![0.0; n + 1],
            t: vec![0.0; n + 1],
            t2: vec![0.0; n + 1],
            st: vec![0.0; n + 1],
        };
        for i in 0..n {
            p.s[i + 1] = p.s[i] + s[i];
            p.s2[i + 1] = p.s2[i] + s[i] * s[i];
            p.t[i + 1] = p.t[i] + t[i];
            p.t2[i + 1] = p.t2[i] + t[i] * t[i];
            p.st[i + 1] = p.st[i] + s[i] * t[i];
        }
        p
    }

    /// Least-squares slope over points `[i, j)`.
    fn slope(&self, i: usize, j: usize) -> f64 {
        let n = (j - i) as f64;
        let s1 = self.s[j] - self.s[i];
        let s2 = self.s2[j] - self.s2[i];
        let t1 = self.t[j] - self.t[i];
        let st = self.st[j] - self.st[i];
        let denom = n * s2 - s1 * s1;
        if denom.abs() < 1e-9 {
            return 0.0;
        }
        (n * st - s1 * t1) / denom
    }

    /// Residual sum of squares of the least-squares line over `[i, j)`.
    fn rss(&self, i: usize, j: usize) -> f64 {
        let n = (j - i) as f64;
        let s1 = self.s[j] - self.s[i];
        let s2 = self.s2[j] - self.s2[i];
        let t1 = self.t[j] - self.t[i];
        let t2 = self.t2[j] - self.t2[i];
        let st = self.st[j] - self.st[i];
        let denom = n * s2 - s1 * s1;
        if denom.abs() < 1e-9 {
            // Degenerate in s (all points at one distance): the fit is the
            // mean, and the RSS is the variance.
            let mean = t1 / n;
            return (t2 - n * mean * mean).max(0.0);
        }
        let b = (n * st - s1 * t1) / denom;
        let a = (t1 - b * s1) / n;
        // Σ(y − a − bx)² = Σy² − aΣy − bΣxy at the least-squares solution.
        (t2 - a * t1 - b * st).max(0.0)
    }
}

/// How a DP pass scores a segment.
enum PassCost {
    /// `n·ln(RSS/n)` per segment — MDL/BIC with the variance free.
    Mdl { penalty: f64 },
    /// `RSS/σ²` per segment — BIC with the variance fixed by pass 1.
    FixedVariance { sigma2: f64, penalty: f64 },
}

impl PassCost {
    fn cost(&self, prefix: &Prefix, i: usize, j: usize) -> f64 {
        match *self {
            PassCost::Mdl { penalty } => {
                let n = (j - i) as f64;
                n * (prefix.rss(i, j).max(RSS_FLOOR) / n).ln() + penalty
            }
            PassCost::FixedVariance { sigma2, penalty } => {
                prefix.rss(i, j) / sigma2 + penalty
            }
        }
    }
}

/// The standard O(n²) changepoint dynamic programme.
///
/// `F[j]` = best total cost of segmenting `[0, j)`, with every segment at
/// least `min_seg` points long. Returns the chosen breakpoints as
/// `(start, end)` index pairs in order. O(n) memory: costs are evaluated from
/// prefix sums on the fly rather than tabulated.
fn dp_segments(prefix: &Prefix, min_seg: usize, cost: PassCost) -> Vec<(usize, usize)> {
    let n = prefix.t.len() - 1;
    let mut f = vec![f64::INFINITY; n + 1];
    let mut back = vec![0usize; n + 1];
    f[0] = 0.0;

    for j in min_seg..=n {
        // A segment [i, j) needs i ≤ j − min_seg, and i itself must be a
        // valid boundary (0, or ≥ min_seg so the previous segment fits).
        let mut best = f[0] + cost.cost(prefix, 0, j);
        let mut best_i = 0usize;
        for i in min_seg..=(j - min_seg) {
            let candidate = f[i] + cost.cost(prefix, i, j);
            if candidate < best {
                best = candidate;
                best_i = i;
            }
        }
        f[j] = best;
        back[j] = best_i;
    }

    let mut segments = Vec::new();
    let mut j = n;
    while j > 0 {
        let i = back[j];
        if i >= j {
            break; // Defensive: never expected, prevents an infinite loop.
        }
        segments.push((i, j));
        j = i;
    }
    segments.reverse();
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample::Sample;
    use crate::core::math::wrap_pi;
    use crate::features::resample::ResampledLap;

    /// Build a synthetic lap on a 1 m grid from a curvature programme:
    /// `(length_m, signed_curvature)` segments, integrated into a path.
    ///
    /// The path is built to agree with the crate's sign convention — positive
    /// curvature is right *and* yields a positive ground-plane cross product
    /// — so the dual-estimator canary in [`estimator_agreement`] passes on
    /// it. (A naive `x += heading.sin()` integration produces a mirror-image
    /// path whose Menger curvature has the opposite sign; the canary exists
    /// precisely to catch channels that disagree like that.)
    fn lap_from_curvature(program: &[(f32, f32)]) -> ResampledLap {
        let mut samples = Vec::new();
        let (mut heading, mut x, mut z, mut d) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);

        for (length, k) in program {
            for _ in 0..(*length as usize) {
                heading = wrap_pi(heading + k);
                x -= heading.sin();
                z += heading.cos();
                d += 1.0;
                samples.push(Sample {
                    t_ms: (d * 33.0) as i64,
                    lap_distance: d,
                    lap_frac: d / 1000.0,
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
            last_lap_time_ms: 0,
                });
            }
        }

        ResampledLap {
            samples,
            step_m: 1.0,
            non_monotone_dropped: 0,
            first_distance_m: 0.0,
        }
    }

    fn right_90() -> (f32, f32) {
        (78.5, 1.0 / 50.0)
    }

    fn left_90() -> (f32, f32) {
        (78.5, -1.0 / 50.0)
    }

    fn track_length_of(program: &[(f32, f32)]) -> f32 {
        program.iter().map(|(l, _)| *l).sum()
    }

    #[test]
    fn a_right_then_left_track_yields_two_arcs_of_the_right_hands() {
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
        let lap = lap_from_curvature(program);
        let seg = segment_lap(&lap, track_length_of(program));

        assert!(
            (2..=4).contains(&seg.arcs.len()),
            "expected ~2 arcs (allowing deliberate over-segmentation), got {}: {:#?}",
            seg.arcs.len(),
            seg.arcs
        );

        let directions: Vec<_> = seg.arcs.iter().map(|a| a.direction).collect();
        assert!(
            directions.contains(&CornerDirection::Right),
            "no right-hander among {directions:?}"
        );
        assert!(
            directions.contains(&CornerDirection::Left),
            "no left-hander among {directions:?}"
        );

        // The 90° turns must come out as 90°: turn angle is the segment's
        // vertical extent.
        for arc in &seg.arcs {
            assert!(
                (arc.turn_angle.abs() - std::f32::consts::FRAC_PI_2).abs() < 0.25,
                "turn angle {} rad, expected ~π/2",
                arc.turn_angle
            );
            assert_eq!(arc.direction, CornerDirection::from_signed(arc.turn_angle));
        }
    }

    #[test]
    fn a_chicane_is_two_arcs_not_a_cancelled_one() {
        // The D8 regression: a right-left pair tight enough that a smoothing
        // window would cancel it. θ(s) is its integral — an S — and cannot
        // cancel.
        let program = &[(300.0, 0.0), (40.0, 1.0 / 30.0), (15.0, 0.0), (40.0, -1.0 / 30.0), (300.0, 0.0)];
        let lap = lap_from_curvature(program);
        let seg = segment_lap(&lap, track_length_of(program));

        let rights = seg.arcs.iter().filter(|a| a.direction == CornerDirection::Right).count();
        let lefts = seg.arcs.iter().filter(|a| a.direction == CornerDirection::Left).count();
        assert!(rights >= 1 && lefts >= 1, "chicane cancelled: {:#?}", seg.arcs);
    }

    #[test]
    fn a_pure_straight_yields_no_arcs() {
        let program = &[(1000.0, 0.0)];
        let lap = lap_from_curvature(program);
        let seg = segment_lap(&lap, track_length_of(program));
        assert!(seg.arcs.is_empty(), "straights produced arcs: {:#?}", seg.arcs);
    }

    #[test]
    fn arcs_land_where_the_programme_put_them() {
        // Corner 1 starts at 300 m; corner 2 at 300 + 79 + 300 ≈ 679 m.
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
        let lap = lap_from_curvature(program);
        let seg = segment_lap(&lap, track_length_of(program));

        let first = seg
            .arcs
            .iter()
            .min_by(|a, b| a.start_m.total_cmp(&b.start_m))
            .expect("at least one arc");
        assert!(
            (first.start_m - 300.0).abs() < 15.0,
            "first arc starts at {} m, expected ~300",
            first.start_m
        );
    }

    #[test]
    fn the_seam_corner_is_one_arc_not_two() {
        // A closed 360° programme whose first and last segments are the two
        // halves of one corner crossing the sample boundary. The old linear
        // detector saw two corners here; the ring treatment must see one
        // wrapped arc.
        //
        // The programme is built from integer lengths with k = 2π/312, so
        // the ring closes exactly: the rotation across the 1 m start/finish
        // gap is exactly one grid step of turn, θ' is smooth through the
        // seam, and nothing about the gap itself pays for a breakpoint. (A
        // programme whose lengths are not grid multiples hides a spurious
        // slope kink inside the gap, which even a zero-noise fitter must
        // split — an artefact of the synthetic lap, not of the estimator.)
        let k = std::f32::consts::TAU / 312.0;
        let half = (39.0f32, k); // half of the seam corner
        let corner = (78.0f32, k); // a full 90°
        let program = &[
            half,          // A: second half of the seam corner
            (250.0, 0.0),
            corner,
            (250.0, 0.0),
            corner,
            (250.0, 0.0),
            corner,
            (250.0, 0.0),
            half,          // G: first half of the seam corner
        ];
        let lap = lap_from_curvature(program);
        let len = track_length_of(program);
        let seg = segment_lap(&lap, len);

        // Three 90s plus the fused seam corner; a little over-segmentation
        // is Stage 1 working as designed, so allow a split or two.
        assert!(
            (4..=6).contains(&seg.arcs.len()),
            "expected ~4 arcs, got {}: {:#?}",
            seg.arcs.len(),
            seg.arcs
        );

        // The wrapped arc: the seam corner must cross the start/finish line
        // as one arc, not terminate at it and restart.
        let wrapped: Vec<_> = seg.arcs.iter().filter(|a| a.end_m < a.start_m).collect();
        assert_eq!(
            wrapped.len(),
            1,
            "exactly one seam-straddling arc: {:#?}",
            seg.arcs
        );
        let span = wrapped[0].span_m(len);
        assert!(
            (span - 78.0).abs() < 12.0,
            "wrapped arc spans {span} m, expected ~78; arcs: {:#?}",
            seg.arcs
        );

        // And the lap still turns exactly 360°.
        let total: f32 = seg.arcs.iter().map(|a| a.turn_angle).sum();
        assert!(
            (total - std::f32::consts::TAU).abs() < 0.3,
            "arcs sum to {total} rad, expected 2π"
        );
    }

    #[test]
    fn a_weak_kink_is_still_a_candidate() {
        // Stage 1 is recall-biased: the shape of the MX5's phantom 1-degree
        // detections must still be proposed — Stage 2 decides whether it
        // recurs. A 1/180 1/m arc over 40 m is ~13°.
        let program = &[(300.0, 0.0), (40.0, 1.0 / 180.0), (300.0, 0.0)];
        let lap = lap_from_curvature(program);
        let seg = segment_lap(&lap, track_length_of(program));
        assert!(
            !seg.arcs.is_empty(),
            "a weak kink must still be a candidate arc"
        );
    }

    #[test]
    fn a_short_lap_is_refused_rather_than_panicking() {
        let lap = lap_from_curvature(&[(3.0, 0.0)]);
        let seg = segment_lap(&lap, 3.0);
        assert!(seg.arcs.is_empty());
    }

    #[test]
    fn the_two_estimators_agree_on_a_well_formed_lap() {
        let program = &[(300.0, 0.0), right_90(), (300.0, 0.0), left_90(), (300.0, 0.0)];
        let lap = lap_from_curvature(program);
        assert!(
            estimator_agreement(&lap) > 0.9,
            "heading and position curvature should agree on a synthetic lap"
        );
    }

    #[test]
    fn a_lying_heading_channel_is_detected() {
        // Invert the heading channel: every turn reads the wrong way round,
        // which is the "9 Right / 0 Left" failure signature.
        let mut lap = lap_from_curvature(&[(300.0, 0.0), right_90(), (300.0, 0.0)]);
        for s in &mut lap.samples {
            s.heading = -s.heading;
        }
        assert!(
            estimator_agreement(&lap) < 0.1,
            "an inverted heading channel must disagree loudly"
        );
    }
}
