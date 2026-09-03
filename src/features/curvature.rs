//! Curvature and heading-change profiles along a lap.
//!
//! Split out of the corner detector so the two concerns can be tested apart:
//! this module turns geometry into signals, `corner` turns signals into corners.
//!
//! Everything here assumes its input has been through
//! [`crate::features::resample`] — an even distance grid. That lets the window
//! functions use fixed index offsets instead of scanning outward by distance, and
//! more importantly it is the only reason the curvature is non-degenerate at all
//! (see that module's notes on AC's stale graphics page).

use crate::core::math::angle_delta;
use crate::core::sample::Sample;

/// Smoothing window in metres, centred.
pub const SMOOTH_WINDOW_M: f32 = 20.0;

/// Window over which heading change is measured, in metres.
pub const HEADING_WINDOW_M: f32 = 20.0;

/// Signed Menger curvature at each point, in 1/m.
///
/// Positive is a right-hand turn, matching the sign convention on
/// [`Sample::heading`]. Endpoints are zero.
///
/// Sign derivation: AC's world is left-handed with Y up, so for the ground-plane
/// cross product `dx1*dz2 - dz1*dx2`, a right turn yields a positive value. This
/// was checked against `d(heading)` directly and the two signs agree on 99.2%
/// (MX5) / 98.8% (F138) of samples.
pub fn signed_curvature(lap: &[Sample]) -> Vec<f32> {
    let mut out = vec![0.0; lap.len()];
    if lap.len() < 3 {
        return out;
    }

    for i in 1..lap.len() - 1 {
        let (p, c, n) = (&lap[i - 1], &lap[i], &lap[i + 1]);

        let dx1 = c.pos[0] - p.pos[0];
        let dz1 = c.pos[2] - p.pos[2];
        let dx2 = n.pos[0] - c.pos[0];
        let dz2 = n.pos[2] - c.pos[2];
        let dx3 = n.pos[0] - p.pos[0];
        let dz3 = n.pos[2] - p.pos[2];

        let len1 = dx1.hypot(dz1);
        let len2 = dx2.hypot(dz2);
        let len3 = dx3.hypot(dz3);

        // Degenerate triangle: no circle through three collinear or coincident
        // points. On raw AC frames this fires on 76-81% of samples; on a
        // resampled lap it is rare and means a genuinely straight section.
        if len1 <= 1e-4 || len2 <= 1e-4 || len3 <= 1e-4 {
            continue;
        }

        let cross = dx1 * dz2 - dz1 * dx2;
        out[i] = 2.0 * cross / (len1 * len2 * len3);
    }
    out
}

/// Box-smooth a profile over a centred distance window.
///
/// Works on whatever is handed to it — call it with `|curvature|` to get a
/// magnitude profile for thresholding, or with signed curvature to get a
/// direction profile. Those are not interchangeable; see [`corner_profiles`].
pub fn smooth(values: &[f32], step_m: f32, window_m: f32) -> Vec<f32> {
    let half = ((window_m / 2.0) / step_m).round() as usize;
    if half == 0 || values.is_empty() {
        return values.to_vec();
    }

    // Prefix sums: the window is fixed-width on a uniform grid, so smoothing is
    // O(n) rather than O(n * window).
    let mut prefix = Vec::with_capacity(values.len() + 1);
    prefix.push(0.0f64);
    for v in values {
        prefix.push(prefix[prefix.len() - 1] + *v as f64);
    }

    (0..values.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(values.len());
            ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32
        })
        .collect()
}

/// Running total of heading change from the start of the lap, in radians.
///
/// Unwrapped: it keeps climbing past ±π instead of folding, so the difference
/// between any two entries is the true rotation between those two points.
/// Differencing *this* is seam-safe; differencing raw headings is not.
///
/// Positive is right. Over a whole clean lap of a clockwise circuit the last
/// entry is +2π.
pub fn cumulative_rotation(lap: &[Sample]) -> Vec<f32> {
    let mut out = Vec::with_capacity(lap.len());
    out.push(0.0f32);
    for w in lap.windows(2) {
        let last = out[out.len() - 1];
        out.push(last + angle_delta(w[0].heading, w[1].heading));
    }
    out
}

/// Net heading change over a centred window, in radians. Positive is right.
pub fn heading_change(lap: &[Sample], step_m: f32, window_m: f32) -> Vec<f32> {
    let half = ((window_m / 2.0) / step_m).round() as usize;
    let mut out = vec![0.0; lap.len()];
    if lap.len() < 2 || half == 0 {
        return out;
    }

    let cumulative = cumulative_rotation(lap);

    for i in 0..lap.len() {
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(lap.len() - 1);
        out[i] = cumulative[hi] - cumulative[lo];
    }
    out
}

/// The three profiles the corner detector needs.
pub struct CornerProfiles {
    /// Smoothed *magnitude* of curvature. Use for thresholding.
    pub magnitude: Vec<f32>,
    /// Smoothed *signed* curvature. Use for direction only.
    pub signed: Vec<f32>,
    /// Net heading change over a window. Positive is right.
    pub heading_change: Vec<f32>,
}

/// Build all three profiles for a resampled lap.
///
/// # Why magnitude and signed are computed separately
///
/// The first implementation smoothed *signed* curvature and then took the
/// absolute value when thresholding. Those two operations do not commute, and
/// the order matters at exactly the places that are hardest to detect: in a
/// chicane, a left and a right of similar radius inside one 20 m window cancel,
/// the smoothed signed value passes through zero, and the detector sees a
/// straight where the driver felt two corners.
///
/// So the magnitude profile is built by taking the absolute value *first* and
/// smoothing that, which cannot cancel; the signed profile is kept alongside it
/// purely to name the direction at the apex.
///
/// Note this is a correctness fix on inspection rather than one this project's
/// data proves: Red Bull Ring has no true chicane, so both orderings give the
/// same ten corners there.
pub fn corner_profiles(lap: &[Sample], step_m: f32) -> CornerProfiles {
    let raw = signed_curvature(lap);
    let magnitudes: Vec<f32> = raw.iter().map(|c| c.abs()).collect();

    CornerProfiles {
        magnitude: smooth(&magnitudes, step_m, SMOOTH_WINDOW_M),
        signed: smooth(&raw, step_m, SMOOTH_WINDOW_M),
        heading_change: heading_change(lap, step_m, HEADING_WINDOW_M),
    }
}

/// Fraction of a profile that is exactly zero.
///
/// The health metric for the resampling stage: on raw AC frames this is
/// 0.76-0.81, and on a resampled lap it should be near zero.
pub fn zero_fraction(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().filter(|v| **v == 0.0).count() as f32 / values.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Build a circular arc of the given radius, turning right when
    /// `radius > 0`. Grid spacing is 1 m along the arc.
    fn arc(radius: f32, points: usize) -> Vec<Sample> {
        let r = radius.abs();
        (0..points)
            .map(|i| {
                let s = i as f32; // arc length, 1 m spacing
                let theta = s / r * radius.signum();
                // Right turn in AC's left-handed X/Z plane.
                let x = r * theta.sin().abs() * radius.signum();
                let z = r * theta.cos();
                Sample {
                    t_ms: i as i64 * 30,
                    lap_distance: s,
                    lap_frac: s / 4286.0,
                    pos: [x, 0.0, z],
                    heading: -x.atan2(z),
                    speed: 30.0,
                    throttle: 0.5,
                    brake: 0.0,
                    steer: 0.0,
                    yaw_rate: 0.0,
                    slip_angle: 0.0,
                    gear: 4,
                    rpm: 6000.0,
                    tyres_out: 0,
                    live: true,
                    surface_grip: 1.0,
                    lap_time_ms: i as i32 * 30,
            last_lap_time_ms: 0,
                }
            })
            .collect()
    }

    #[test]
    fn curvature_of_an_arc_is_one_over_its_radius() {
        let lap = arc(50.0, 60);
        let k = signed_curvature(&lap);
        // Interior points only; endpoints are zero by construction.
        let mid = &k[10..k.len() - 10];
        let mean = mid.iter().map(|v| v.abs()).sum::<f32>() / mid.len() as f32;
        assert!(
            (mean - 1.0 / 50.0).abs() < 2e-3,
            "expected ~0.02 1/m for a 50 m radius, got {mean}"
        );
    }

    #[test]
    fn straight_line_has_no_curvature() {
        let lap: Vec<Sample> = (0..50)
            .map(|i| {
                let mut s = arc(1e6, 1)[0].clone();
                s.pos = [0.0, 0.0, i as f32];
                s.lap_distance = i as f32;
                s.heading = 0.0;
                s
            })
            .collect();
        let k = signed_curvature(&lap);
        assert!(k.iter().all(|v| v.abs() < 1e-6), "straight line curved");
    }

    #[test]
    fn magnitude_smoothing_survives_a_chicane_that_cancels_when_signed() {
        // Half a window of right, half a window of left, same magnitude: the
        // exact case where smoothing-then-abs reads as a straight.
        let n = 40;
        let signed: Vec<f32> = (0..n)
            .map(|i| if i < n / 2 { 0.05 } else { -0.05 })
            .collect();
        let magnitudes: Vec<f32> = signed.iter().map(|v| v.abs()).collect();

        let smoothed_signed = smooth(&signed, 1.0, 20.0);
        let smoothed_magnitude = smooth(&magnitudes, 1.0, 20.0);

        // At the transition the signed profile collapses towards zero...
        let mid = n / 2;
        assert!(
            smoothed_signed[mid].abs() < 0.01,
            "signed profile should cancel at the transition, got {}",
            smoothed_signed[mid]
        );
        // ...while the magnitude profile holds the real curvature.
        assert!(
            smoothed_magnitude[mid] > 0.04,
            "magnitude profile must not cancel, got {}",
            smoothed_magnitude[mid]
        );
    }

    #[test]
    fn heading_change_is_seam_safe() {
        // A lap section rotating steadily through the ±π seam.
        let lap: Vec<Sample> = (0..60)
            .map(|i| {
                let mut s = arc(50.0, 1)[0].clone();
                s.lap_distance = i as f32;
                // Sweep from 2.9 rad up through π and out the other side.
                s.heading = crate::core::math::wrap_pi(2.9 + i as f32 * 0.02);
                s
            })
            .collect();
        let hc = heading_change(&lap, 1.0, 20.0);
        // 20 m at 0.02 rad/m is 0.4 rad, everywhere — including across the seam.
        for (i, v) in hc.iter().enumerate().take(50).skip(10) {
            assert!(
                (v - 0.4).abs() < 0.05,
                "heading change at {i} was {v}, expected ~0.4 — the seam leaked"
            );
        }
        assert!(hc.iter().all(|v| v.abs() < PI));
    }

    #[test]
    fn zero_fraction_counts_exact_zeros() {
        assert_eq!(zero_fraction(&[0.0, 1.0, 0.0, 2.0]), 0.5);
        assert_eq!(zero_fraction(&[]), 0.0);
    }
}
