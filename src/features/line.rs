//! How far apart two laps ran, and which lap is most representative.
//!
//! # Why this replaced the Fréchet distance
//!
//! [`crate::features::frechet`] measures the shortest leash that lets a walker
//! traverse one path while a dog traverses the other. That metric exists to
//! handle *reparameterisation*: two curves whose point correspondence is
//! unknown, traced at different speeds, where the comparison must search every
//! monotone pairing to find the best one.
//!
//! This pipeline does not have that problem. [`crate::features::resample`]
//! anchors every lap's grid at absolute distance 0 — the start/finish line — so
//! a sample at 1,842 m is at 1,842 m along the track spline in every lap and in
//! every car. The correspondence is not unknown; establishing it is the entire
//! point of the resampling stage.
//!
//! The identity coupling — pair index `i` with index `i` — is therefore both
//! meaningful *and* one of the couplings the Fréchet DP already searches. So
//! Fréchet can never exceed the largest equal-distance separation, and the
//! interesting question is how much lower it actually goes. Measured on all six
//! pairs of clean laps in the two reference captures, at full 1 m resolution:
//!
//! | Pair | Equal-distance max | Fréchet | Gap |
//! |---|---:|---:|---:|
//! | MX5 1 v 2 | 6.29 m | 6.29 m | 0.00 m |
//! | MX5 1 v 5 | 6.40 m | 6.40 m | 0.00 m |
//! | MX5 2 v 5 | 9.18 m | 9.18 m | 0.00 m |
//! | F138 2 v 4 | 5.83 m | 5.83 m | 0.00 m |
//! | F138 2 v 5 | 11.01 m | 11.01 m | 0.00 m |
//! | F138 4 v 5 | 5.70 m | 5.70 m | 0.00 m |
//!
//! Zero on every pair: **on laps already aligned by track distance, the optimal
//! Fréchet coupling is the identity coupling.** The `O(n²)` search is not wrong,
//! it is redundant — it spends ~265 ms per pair to arrive at the number one
//! `O(n)` pass produces in ~130 µs, a 1,700-2,600x difference measured on the
//! same laps.
//!
//! Two things fall out of no longer needing the search:
//!
//! * **No stride knob.** Fréchet's cost forced downsampling to every 5th point,
//!   and that approximation was not free: strided Fréchet reports 6.19 m for the
//!   F138 pair whose true separation is 5.83 m, a 6% error. This runs at full
//!   grid resolution because it can afford to.
//! * **It reports where.** The DP collapses a lap to one number and discards the
//!   location. A single pass can keep it, and [`Separation::max_at_m`] is the
//!   first thing worth looking at when a model's geometry looks wrong.
//!
//! # Mean rather than worst case
//!
//! A medoid is by definition the element minimising total distance to the
//! others, so [`medoid_lap`] ranks on [`Separation::mean_m`]. Fréchet returns a
//! *maximum*, so averaging Fréchet distances across pairs — what the previous
//! implementation did — produced a mean of maxima: a minimax centre, not a
//! medoid, despite the name.
//!
//! The difference is not academic. One wide exit, one clipped kerb, one lift is
//! enough to set a maximum, and under a max-based ranking that single moment
//! disqualifies an otherwise typical lap. On the F138 capture the two rankings
//! disagree: mean picks lap 2 (mean separation 1.31 m against 1.32 m for lap 4),
//! max picks lap 4, because lap 2 has an 11 m excursion at 392 m. The margin is
//! about 1%, so this is a near-tie rather than a correction — but "usually
//! closest to the others" is the question a reference lap should answer.
//!
//! The maximum is kept regardless. It costs nothing to carry and it is the
//! diagnostic, even though it is not the ranking.
//!
//! # Elevation is dropped
//!
//! Comparison is on the ground plane, `(pos[0], pos[2])`. Two laps that differ
//! only in how high the car sat over a kerb are the same line.

use crate::core::sample::Sample;

/// How far apart two laps ran, compared at equal track distance.
#[derive(Debug, Clone, Copy)]
pub struct Separation {
    /// Mean ground-plane distance between the laps, metres.
    pub mean_m: f32,
    /// Largest ground-plane distance between the laps, metres.
    pub max_m: f32,
    /// Track distance at which [`Self::max_m`] occurred, metres.
    pub max_at_m: f32,
    /// Track distance actually compared — where both laps had samples.
    ///
    /// Worth reporting because a low mean over a short overlap says nothing. Two
    /// laps that share only their first 200 m can look extremely similar.
    pub overlap_m: f32,
}

/// Ground-plane distance between two samples. AC's ground plane is X/Z.
fn ground_distance(a: &Sample, b: &Sample) -> f32 {
    (a.pos[0] - b.pos[0]).hypot(a.pos[2] - b.pos[2])
}

/// Grid index of a resampled sample.
///
/// Integer rather than a float comparison with a tolerance: both laps come off
/// the same anchored grid, so `lap_distance` is an exact multiple of `step_m` on
/// both sides and rounding recovers the index with no epsilon to choose.
fn grid_index(s: &Sample, step_m: f32) -> i64 {
    (s.lap_distance / step_m).round() as i64
}

/// Separation between two resampled laps, compared at equal track distance.
///
/// `None` when the two laps share no grid point, which for clean laps means one
/// of them is empty. Both laps must be on the same `step_m` grid; they need not
/// cover the same range, and only the overlap is compared.
pub fn separation(a: &[Sample], b: &[Sample], step_m: f32) -> Option<Separation> {
    if step_m <= 0.0 || !step_m.is_finite() {
        return None;
    }

    // Merge join on grid index. Both sides are strictly increasing in distance
    // (resample guarantees it), so one pass suffices and neither index rewinds.
    let (mut i, mut j) = (0usize, 0usize);
    let mut total = 0.0f64;
    let mut count = 0u32;
    let mut max_m = 0.0f32;
    let mut max_at_m = 0.0f32;

    while i < a.len() && j < b.len() {
        let (ga, gb) = (grid_index(&a[i], step_m), grid_index(&b[j], step_m));
        if ga < gb {
            i += 1;
        } else if gb < ga {
            j += 1;
        } else {
            let d = ground_distance(&a[i], &b[j]);
            // A non-finite position would poison the mean silently; skip it and
            // let the overlap figure show that fewer points were compared.
            if d.is_finite() {
                total += d as f64;
                count += 1;
                if d > max_m {
                    max_m = d;
                    max_at_m = a[i].lap_distance;
                }
            }
            i += 1;
            j += 1;
        }
    }

    if count == 0 {
        return None;
    }
    Some(Separation {
        mean_m: (total / count as f64) as f32,
        max_m,
        max_at_m,
        overlap_m: count as f32 * step_m,
    })
}

/// Mean separation from each lap to all the others.
///
/// One entry per lap. Pairs that cannot be compared are skipped rather than
/// poisoning the average; a lap with no comparable partner gets
/// `f32::INFINITY`, so it sorts last and cannot be chosen as a medoid.
pub fn mean_separations(laps: &[&[Sample]], step_m: f32) -> Vec<f32> {
    let n = laps.len();
    let mut totals = vec![0.0f32; n];
    let mut counts = vec![0u32; n];

    // Symmetric, so each pair is computed once.
    for i in 0..n {
        for j in (i + 1)..n {
            if let Some(s) = separation(laps[i], laps[j], step_m) {
                totals[i] += s.mean_m;
                totals[j] += s.mean_m;
                counts[i] += 1;
                counts[j] += 1;
            }
        }
    }

    (0..n)
        .map(|i| {
            if counts[i] == 0 {
                f32::INFINITY
            } else {
                totals[i] / counts[i] as f32
            }
        })
        .collect()
}

/// How close two laps' mean separations must be to count as tied, as a fraction
/// of the better one. **Tuning knob**, but a dimensionless one — it scales with
/// the circuit and the driver rather than assuming a number of metres.
///
/// It exists because the ranking is genuinely near-degenerate on consistent
/// driving. Measured on the F138 capture, laps 2 and 4 sit at 1.305 m and
/// 1.320 m mean separation — a 1.5 cm margin, which then decides the reference
/// lap and through it the model's whole geometry, taking the corner count from 9
/// to 8. Deciding that on 1.5 cm is a coin flip dressed as a measurement.
const TIE_FRACTION: f32 = 0.02;

/// Index of the most representative lap — lowest mean separation to the rest.
///
/// The medoid rather than the fastest lap because the fastest lap of a short
/// session is routinely an outlier that caught a tow or clipped a kerb, whereas
/// the medoid is by construction the lap closest to all the others.
///
/// Ties on the mean are broken by the smaller worst-case separation: among laps
/// that were equally typical, prefer the one with no single bad moment. Without
/// this the choice between two near-identical laps comes down to float noise,
/// and since the reference lap supplies all of the model's geometry, that noise
/// propagates into the corner set.
///
/// `None` for an empty set. A single lap is trivially its own medoid.
pub fn medoid_lap(laps: &[&[Sample]], step_m: f32) -> Option<usize> {
    match laps.len() {
        0 => return None,
        1 => return Some(0),
        _ => {}
    }

    let means = mean_separations(laps, step_m);
    // total_cmp rather than partial_cmp().unwrap(): an INFINITY or NaN mean must
    // sort, not panic.
    let best = (0..laps.len()).min_by(|a, b| means[*a].total_cmp(&means[*b]))?;
    if !means[best].is_finite() {
        // Nothing was comparable. Return the lap anyway rather than None — the
        // caller asked for a reference and one arbitrary lap beats no model.
        return Some(best);
    }

    let cutoff = means[best] * (1.0 + TIE_FRACTION);
    let tied: Vec<usize> = (0..laps.len()).filter(|&i| means[i] <= cutoff).collect();
    if tied.len() < 2 {
        return Some(best);
    }

    // Break the tie on the worst single point across each lap's pairs.
    tied.into_iter().min_by(|&a, &b| {
        worst_separation(laps, a, step_m).total_cmp(&worst_separation(laps, b, step_m))
    })
}

/// Largest single-point separation between lap `idx` and any other lap.
fn worst_separation(laps: &[&[Sample]], idx: usize, step_m: f32) -> f32 {
    let mut worst = 0.0f32;
    for (other, lap) in laps.iter().enumerate() {
        if other == idx {
            continue;
        }
        if let Some(s) = separation(laps[idx], lap, step_m) {
            worst = worst.max(s.max_m);
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::frechet;

    /// A straight lap offset sideways by `x_offset`, on a 1 m grid.
    fn straight(x_offset: f32, n: usize) -> Vec<Sample> {
        (0..n).map(|i| sample(i as f32, x_offset)).collect()
    }

    fn sample(distance: f32, x: f32) -> Sample {
        Sample {
            t_ms: distance as i64,
            lap_distance: distance,
            lap_frac: distance / 1000.0,
            // Travel along Z, offset across X: X is the lateral axis here.
            pos: [x, 0.0, distance],
            heading: 0.0,
            speed: 30.0,
            throttle: 1.0,
            brake: 0.0,
            steer: 0.0,
            yaw_rate: 0.0,
            slip_angle: 0.0,
            gear: 4,
            rpm: 6000.0,
            tyres_out: 0,
            surface_grip: 1.0,
            lap_time_ms: distance as i32,
        }
    }

    #[test]
    fn a_lap_is_zero_from_itself() {
        let lap = straight(0.0, 100);
        let s = separation(&lap, &lap, 1.0).expect("non-empty");
        assert_eq!(s.mean_m, 0.0);
        assert_eq!(s.max_m, 0.0);
        assert_eq!(s.overlap_m, 100.0);
    }

    #[test]
    fn parallel_laps_are_their_offset_apart() {
        let s = separation(&straight(0.0, 100), &straight(3.0, 100), 1.0).expect("non-empty");
        assert!((s.mean_m - 3.0).abs() < 1e-4, "mean {}", s.mean_m);
        assert!((s.max_m - 3.0).abs() < 1e-4, "max {}", s.max_m);
    }

    #[test]
    fn only_the_overlapping_distance_is_compared() {
        // One lap spans 0..99 m, the other 50..149 m. Fifty metres in common.
        let a = straight(0.0, 100);
        let b: Vec<Sample> = (50..150).map(|i| sample(i as f32, 2.0)).collect();
        let s = separation(&a, &b, 1.0).expect("they overlap");
        assert_eq!(s.overlap_m, 50.0);
        assert!((s.mean_m - 2.0).abs() < 1e-4, "mean {}", s.mean_m);
    }

    #[test]
    fn laps_that_never_meet_on_the_grid_give_none() {
        let a = straight(0.0, 10);
        let b: Vec<Sample> = (100..110).map(|i| sample(i as f32, 0.0)).collect();
        assert!(separation(&a, &b, 1.0).is_none());
        assert!(separation(&[], &a, 1.0).is_none());
        assert!(separation(&a, &[], 1.0).is_none());
    }

    #[test]
    fn the_maximum_reports_where_it_happened() {
        // Identical except for one 8 m excursion at 40 m.
        let a = straight(0.0, 100);
        let mut b = straight(0.0, 100);
        b[40].pos[0] = 8.0;
        let s = separation(&a, &b, 1.0).expect("non-empty");
        assert!((s.max_m - 8.0).abs() < 1e-4, "max {}", s.max_m);
        assert_eq!(s.max_at_m, 40.0);
        // One point out of a hundred barely moves the mean — which is the point
        // of ranking on the mean rather than the worst case.
        assert!(s.mean_m < 0.1, "mean {} should stay small", s.mean_m);
    }

    #[test]
    fn medoid_picks_the_middle_lap() {
        let laps = [straight(0.0, 60), straight(1.0, 60), straight(10.0, 60)];
        let refs: Vec<&[Sample]> = laps.iter().map(|l| l.as_slice()).collect();
        assert_eq!(medoid_lap(&refs, 1.0), Some(1));
    }

    #[test]
    fn a_near_tie_on_the_mean_is_broken_by_the_worst_case() {
        // Two laps essentially tied on mean separation: one is uniformly a
        // little off, the other is nearly perfect except for one big excursion.
        // The consistent lap should win the reference slot.
        let base = straight(0.0, 400);
        let uniform = straight(0.6, 400); // 0.6 m off everywhere
        let mut spiky = straight(0.0, 400);
        for s in spiky.iter_mut().take(260).skip(200) {
            s.pos[0] = 4.0; // ~4 m off for 60 m, ~0 elsewhere
        }

        let laps = [base.clone(), uniform, spiky];
        let refs: Vec<&[Sample]> = laps.iter().map(|l| l.as_slice()).collect();
        let means = mean_separations(&refs, 1.0);
        // Confirm the premise: laps 1 and 2 really are within the tie band.
        let tie = (means[1] - means[2]).abs() / means[1].min(means[2]);
        assert!(tie < TIE_FRACTION, "premise broken: means {means:?}");

        let chosen = medoid_lap(&refs, 1.0).expect("some medoid");
        assert_ne!(
            chosen, 2,
            "the lap with the 4 m excursion should not win a tie"
        );
    }

    #[test]
    fn a_clear_winner_on_the_mean_is_not_overridden_by_the_worst_case() {
        // Lap 1 is much closer on average but has one bad moment; lap 2 is
        // consistently far away. The tie-break must not promote lap 2.
        let base = straight(0.0, 400);
        let mut close_with_spike = straight(0.1, 400);
        close_with_spike[300].pos[0] = 12.0;
        let far = straight(5.0, 400);

        let laps = [base, close_with_spike, far];
        let refs: Vec<&[Sample]> = laps.iter().map(|l| l.as_slice()).collect();
        assert_ne!(
            medoid_lap(&refs, 1.0),
            Some(2),
            "a consistently distant lap must not win on worst case alone"
        );
    }

    #[test]
    fn medoid_handles_degenerate_sets() {
        assert_eq!(medoid_lap(&[], 1.0), None);
        let one = straight(0.0, 10);
        assert_eq!(medoid_lap(&[one.as_slice()], 1.0), Some(0));

        // An empty lap among real ones must not win and must not panic.
        let a = straight(0.0, 10);
        let c = straight(1.0, 10);
        let refs: Vec<&[Sample]> = vec![a.as_slice(), &[], c.as_slice()];
        assert_ne!(
            medoid_lap(&refs, 1.0),
            Some(1),
            "the empty lap should not win"
        );
    }

    #[test]
    fn a_non_finite_position_is_skipped_not_averaged_in() {
        let a = straight(0.0, 100);
        let mut b = straight(1.0, 100);
        b[50].pos[0] = f32::NAN;
        let s = separation(&a, &b, 1.0).expect("99 good points remain");
        assert_eq!(s.overlap_m, 99.0, "the NaN point must not be counted");
        assert!((s.mean_m - 1.0).abs() < 1e-4, "mean {}", s.mean_m);
    }

    /// The one relationship that is guaranteed rather than measured.
    ///
    /// The identity coupling — pair index `i` with index `i` — is one of the
    /// monotone couplings the Fréchet DP searches over, so the Fréchet distance
    /// can never exceed the largest equal-distance separation. That makes
    /// [`Separation::max_m`] an upper bound on Fréchet, computed in one pass
    /// instead of `n²`.
    #[test]
    fn frechet_never_exceeds_the_equal_distance_maximum() {
        let weave = |shift: f32| -> Vec<Sample> {
            (0..400)
                .map(|i| {
                    let d = i as f32;
                    sample(d, ((d - shift) * 0.05).sin() * 6.0)
                })
                .collect()
        };
        for shift in [0.0, 5.0, 30.0] {
            let a = weave(0.0);
            let b = weave(shift);
            let max_m = separation(&a, &b, 1.0).expect("same grid").max_m;
            let coupled = frechet::discrete_frechet(&frechet::path_of(&a), &frechet::path_of(&b))
                .expect("non-empty");
            assert!(
                coupled <= max_m + 1e-3,
                "shift {shift}: frechet {coupled} exceeded equal-distance max {max_m}"
            );
        }
    }

    /// Why the maximum is kept but not ranked on.
    ///
    /// A single excursion sets the maximum — and therefore sets Fréchet too,
    /// since Fréchet is also a maximum. The mean is what separates "this lap was
    /// consistently different" from "this lap had one bad moment", and only the
    /// first should disqualify a lap from being the reference.
    #[test]
    fn one_excursion_sets_the_maximum_but_not_the_mean() {
        let a = straight(0.0, 400);
        let mut b = straight(0.2, 400);
        for s in b.iter_mut().take(205).skip(200) {
            s.pos[0] = 9.0;
        }
        let s = separation(&a, &b, 1.0).expect("same grid");
        let coupled = frechet::discrete_frechet(&frechet::path_of(&a), &frechet::path_of(&b))
            .expect("non-empty");

        assert!(s.max_m > 8.9, "max {}", s.max_m);
        assert!(coupled > 8.9, "frechet {coupled} is a maximum too");
        assert!(
            s.mean_m < 0.4,
            "mean {} should still read as a near-identical lap",
            s.mean_m
        );
    }
}
