//! Exact order statistics for the small samples the learner works with.
//!
//! The consensus machinery never sees more than ten laps (the data budget the
//! problem statement sets), so every quantile here is computed exactly from a
//! sorted copy rather than approximated. Streaming quantile estimators (P²,
//! t-digest) solve a problem this crate does not have — the live path holds
//! only the frozen model — and would only add error where exactness is free.
//!
//! All functions are NaN-safe (`total_cmp` throughout, per the convention the
//! rest of the crate follows after the D10 defect) and total on empty input
//! (returning 0.0) so callers never need a guard.

/// Median of a sample: the middle element, or the mean of the two middle
/// elements for an even count. Empty input is 0.0.
pub fn median(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// Median absolute deviation about the median, in the sample's own units.
///
/// This is the raw MAD, not the σ-equivalent; scale by 1.4826 yourself (or use
/// [`sigma_from_mad`]) so the two ideas stay distinguishable in callers.
pub fn mad(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let centre = median(values);
    let mut deviations: Vec<f32> = values.iter().map(|v| (v - centre).abs()).collect();
    deviations.sort_by(|a, b| a.total_cmp(b));
    deviations[deviations.len() / 2]
}

/// σ estimate from the MAD, floored: `1.4826 · max(MAD, floor)`.
///
/// 1.4826 is the standard consistency constant that makes the MAD a
/// normal-theory estimate of σ. The floor exists because the traces this is
/// applied to are frequently *exactly* constant — a pedal channel that never
/// moves, a synthetic lap with zero noise — and a zero σ would make every
/// modified-z test divide by zero (or pass everything, which is worse).
pub fn sigma_from_mad(values: &[f32], floor: f32) -> f32 {
    1.4826 * mad(values).max(floor)
}

/// Nearest-rank quantile, `q` in `0..=1`. Empty input is 0.0.
pub fn quantile(values: &[f32], q: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));

    let q = if q.is_finite() { q.clamp(0.0, 1.0) } else { 0.5 };
    let idx = ((sorted.len() - 1) as f32 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_odd_and_even_samples() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        // Even count: the mean of the two middle elements.
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn mad_measures_spread_around_the_median() {
        // Median 0, deviations all 1.0 → MAD 1.0.
        assert!((mad(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
        // A constant sample has no spread at all.
        assert_eq!(mad(&[2.0, 2.0, 2.0]), 0.0);
        assert_eq!(mad(&[]), 0.0);
    }

    #[test]
    fn sigma_from_mad_scales_and_floors() {
        assert!((sigma_from_mad(&[1.0, -1.0, 1.0, -1.0], 1e-6) - 1.4826).abs() < 1e-3);
        // A dead trace must not produce a zero σ.
        assert!(sigma_from_mad(&[0.0; 10], 1e-3) >= 1.4826e-3);
    }

    #[test]
    fn quantile_interpolates_by_rank() {
        let v = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(quantile(&v, 0.0), 10.0);
        assert_eq!(quantile(&v, 1.0), 40.0);
        assert_eq!(quantile(&v, 0.75), 30.0);
        assert_eq!(quantile(&v, 0.5), 30.0, "quantile is nearest-rank; median is not");
        // Degenerate q values must not panic or index out of bounds.
        assert_eq!(quantile(&v, f32::NAN), 30.0);
        assert_eq!(quantile(&[], 0.9), 0.0);
    }
}
