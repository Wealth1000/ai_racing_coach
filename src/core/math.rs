//! Angle helpers.
//!
//! Every one of these exists because heading is an angle on a circle, and the
//! naive arithmetic silently produces ~2π errors at the ±π seam. A lap of Red
//! Bull Ring crosses that seam about twice, and a fabricated 2π jump in heading
//! turns into an enormous fake curvature spike, which the corner detector then
//! reports as a corner. So: never subtract or interpolate headings directly.

use std::f32::consts::PI;

pub const TAU: f32 = 2.0 * PI;

/// Wrap an angle into `(-π, π]`.
#[inline]
pub fn wrap_pi(mut a: f32) -> f32 {
    while a > PI {
        a -= TAU;
    }
    while a <= -PI {
        a += TAU;
    }
    a
}

/// Shortest signed rotation taking `from` to `to`, in `(-π, π]`.
///
/// Use this for *every* heading difference. `to - from` is wrong whenever the
/// pair straddles the seam: heading 3.10 → -3.10 is a 0.08 rad nudge to the
/// right, not a 6.20 rad swerve to the left.
#[inline]
pub fn angle_delta(from: f32, to: f32) -> f32 {
    wrap_pi(to - from)
}

/// Interpolate between two angles the short way round, `t` in `[0, 1]`.
#[inline]
pub fn lerp_angle(from: f32, to: f32, t: f32) -> f32 {
    wrap_pi(from + angle_delta(from, to) * t)
}

/// Plain linear interpolation, for the many non-angular channels.
#[inline]
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// Sum of shortest-path heading deltas along a path — the *unwrapped* total
/// rotation, which keeps accumulating past ±π instead of wrapping.
///
/// This is how a lap is distinguished from a spin. A clean lap of a closed
/// circuit nets exactly one full rotation (measured: +2π on every clean lap of
/// Red Bull Ring, which runs clockwise); a lap containing a spin nets two
/// (measured: +4.0016π on the MX5's 3rd lap).
pub fn net_rotation(headings: &[f32]) -> f32 {
    headings
        .windows(2)
        .map(|w| angle_delta(w[0], w[1]))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_pi_folds_into_range() {
        assert!((wrap_pi(0.0) - 0.0).abs() < 1e-6);
        assert!((wrap_pi(PI) - PI).abs() < 1e-6);
        assert!((wrap_pi(TAU + 0.5) - 0.5).abs() < 1e-6);
        assert!((wrap_pi(-TAU - 0.5) + 0.5).abs() < 1e-6);
        // Whatever goes in, the result is always in range.
        for i in -100..100 {
            let a = i as f32 * 0.37;
            let w = wrap_pi(a);
            assert!(w > -PI - 1e-5 && w <= PI + 1e-5, "wrap_pi({a}) = {w}");
        }
    }

    #[test]
    fn angle_delta_takes_the_short_way_across_the_seam() {
        // The case that breaks naive subtraction: 3.10 -> -3.10 is a small
        // right-hand nudge through π, not a 6.2 rad swing back the other way.
        let d = angle_delta(3.10, -3.10);
        assert!(d > 0.0, "expected a small positive delta, got {d}");
        assert!((d - 0.0831853).abs() < 1e-4, "got {d}");

        // And the mirror image.
        let d = angle_delta(-3.10, 3.10);
        assert!(d < 0.0, "expected a small negative delta, got {d}");
    }

    #[test]
    fn lerp_angle_does_not_swing_the_long_way() {
        // Midpoint of 3.10 and -3.10 is just past π, i.e. ~-3.1416, NOT 0.0.
        let mid = lerp_angle(3.10, -3.10, 0.5);
        assert!(
            mid.abs() > 3.0,
            "midpoint across the seam should stay near ±π, got {mid}"
        );
    }

    #[test]
    fn net_rotation_accumulates_past_pi() {
        // One full turn, walked in 8 steps, must total ~2π and not wrap to 0.
        let headings: Vec<f32> = (0..=8).map(|i| wrap_pi(i as f32 * TAU / 8.0)).collect();
        let net = net_rotation(&headings);
        assert!((net - TAU).abs() < 1e-4, "expected 2π, got {net}");

        // Two full turns must total ~4π — this is the spin signature.
        let headings: Vec<f32> = (0..=16).map(|i| wrap_pi(i as f32 * TAU / 8.0)).collect();
        let net = net_rotation(&headings);
        assert!((net - 2.0 * TAU).abs() < 1e-4, "expected 4π, got {net}");
    }
}
