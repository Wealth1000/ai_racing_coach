//! Resample a lap onto an even distance grid.
//!
//! # Why this stage exists
//!
//! Assetto Corsa publishes car position on its *graphics* page. Physics runs at
//! ~62 Hz but graphics updates at ~38 Hz, so measured over both reference
//! captures, `PositionX/Y/Z` and `NormalizedCarPosition` change on only 61.4%
//! (MX5) and 58.7% (F138) of frames; the rest repeat the previous values byte
//! for byte. Physics `PacketId` advances on 100% of frames, so the frames are
//! genuinely new — the position simply has not moved yet.
//!
//! Menger curvature is computed from three consecutive points and is undefined
//! when any two of them coincide. On raw frames that happens constantly, and the
//! detector's degenerate-triangle guard therefore emits exactly 0.0 for **75.9%
//! (MX5) / 80.9% (F138)** of samples. Smoothing then averages those zeros into
//! the real values, flattening the profile below any sensible corner threshold.
//! That is the actual reason corner detection returned nothing on AC data.
//!
//! Putting the lap on an even 1 m grid first takes the zero fraction to **0.0% /
//! 1.6%**. The residual is legitimate: at 283 km/h the F138 covers ~2.1 m
//! between graphics updates, so a 1 m grid interpolates points that really are
//! collinear, on straights where the curvature really is zero.
//!
//! # Why distance and not time
//!
//! Three reasons, in order of importance:
//!
//! 1. `lap_frac` is a position along the track *spline*, so it is independent of
//!    the racing line. Grid index *i* is the same place on the track in every
//!    lap and in every car, which is what makes laps comparable without any
//!    time-warping step.
//! 2. It is accurate. `delta(lap_frac) * track_length` tracks the true Euclidean
//!    step to p5 -0.019 m, median +0.001 m, p95 +0.019 m per frame.
//! 3. `Timestamp` is `DateTimeOffset.UtcNow` — a wall clock that does not pause
//!    with the sim, so it is the wrong axis regardless.

use crate::core::math::{lerp, lerp_angle};
use crate::core::sample::Sample;

/// Grid spacing in metres.
///
/// 1 m gives ~4,286 points for Red Bull Ring, which is finer than the ~2 m
/// spacing of raw graphics updates at racing speed, so the grid never has to
/// invent structure between two updates — it only ever interpolates along the
/// straight line between them.
pub const DEFAULT_STEP_M: f32 = 1.0;

/// A lap on an even distance grid.
#[derive(Debug, Clone)]
pub struct ResampledLap {
    pub samples: Vec<Sample>,
    pub step_m: f32,
    /// Raw samples dropped for being at or behind the previous distance.
    pub non_monotone_dropped: usize,
}

impl ResampledLap {
    /// Grid index for a distance in metres.
    pub fn index_at(&self, distance_m: f32) -> usize {
        ((distance_m / self.step_m).round() as usize).min(self.samples.len().saturating_sub(1))
    }
}

/// Resample a lap's samples onto a grid of `step_m` metres.
///
/// Returns `None` if there is not enough distinct data to interpolate.
pub fn resample_lap(samples: &[Sample], step_m: f32) -> Option<ResampledLap> {
    assert!(step_m > 0.0, "grid step must be positive");

    // Keep only strictly increasing distances. Equal-distance frames are the
    // stale-graphics repeats, and interpolating between two identical distances
    // is a division by zero.
    let mut pts: Vec<&Sample> = Vec::with_capacity(samples.len());
    let mut dropped = 0usize;
    for s in samples {
        match pts.last() {
            Some(prev) if s.lap_distance <= prev.lap_distance => dropped += 1,
            _ => pts.push(s),
        }
    }
    if pts.len() < 2 {
        return None;
    }

    let start = pts[0].lap_distance;
    let end = pts[pts.len() - 1].lap_distance;
    if end - start < step_m {
        return None;
    }

    // The grid is anchored at absolute distance 0 (the start/finish line), not
    // at the lap's first sample. That is what makes index i mean the same place
    // in every lap: a lap starting at 0.4 m and one starting at 0.9 m must not
    // end up half a metre out of phase with each other.
    let first_idx = (start / step_m).ceil() as i64;
    let last_idx = (end / step_m).floor() as i64;
    if last_idx < first_idx {
        return None;
    }

    let mut out = Vec::with_capacity((last_idx - first_idx + 1) as usize);
    let mut seg = 0usize;

    for gi in first_idx..=last_idx {
        let d = gi as f32 * step_m;

        // Advance to the segment containing d. Monotone in both loops, so the
        // whole resampling is a single linear pass.
        while seg + 2 < pts.len() && pts[seg + 1].lap_distance < d {
            seg += 1;
        }
        let a = pts[seg];
        let b = pts[seg + 1];

        let span = b.lap_distance - a.lap_distance;
        let t = if span > 0.0 {
            ((d - a.lap_distance) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };

        out.push(interpolate(a, b, t, d, step_m));
    }

    Some(ResampledLap {
        samples: out,
        step_m,
        non_monotone_dropped: dropped,
    })
}

/// Interpolate a single grid point between two raw samples.
fn interpolate(a: &Sample, b: &Sample, t: f32, distance: f32, step_m: f32) -> Sample {
    Sample {
        // Time is interpolated for reference only; it stays a wall clock.
        t_ms: a.t_ms + ((b.t_ms - a.t_ms) as f32 * t) as i64,
        lap_distance: distance,
        lap_frac: lerp(a.lap_frac, b.lap_frac, t),

        pos: [
            lerp(a.pos[0], b.pos[0], t),
            lerp(a.pos[1], b.pos[1], t),
            lerp(a.pos[2], b.pos[2], t),
        ],

        // Angular, not linear. A plain lerp across the +/-pi seam swings the long
        // way round and fabricates a ~2*pi jump; a lap crosses that seam about
        // twice, and each crossing would produce an enormous false curvature
        // spike that the corner detector would happily report as a corner.
        heading: lerp_angle(a.heading, b.heading, t),

        speed: lerp(a.speed, b.speed, t),
        throttle: lerp(a.throttle, b.throttle, t),
        brake: lerp(a.brake, b.brake, t),
        steer: lerp(a.steer, b.steer, t),
        yaw_rate: lerp(a.yaw_rate, b.yaw_rate, t),
        slip_angle: lerp(a.slip_angle, b.slip_angle, t),

        // Discrete channels take the nearer sample's value rather than a
        // fictional average: gear 2.5 does not exist.
        gear: if t < 0.5 { a.gear } else { b.gear },
        tyres_out: if t < 0.5 { a.tyres_out } else { b.tyres_out },

        rpm: lerp(a.rpm, b.rpm, t),
        surface_grip: lerp(a.surface_grip, b.surface_grip, t),
        lap_time_ms: a.lap_time_ms + ((b.lap_time_ms - a.lap_time_ms) as f32 * t) as i32,
    }
    .also_check(step_m)
}

impl Sample {
    /// No-op hook kept so `interpolate` reads as a single expression; debug
    /// builds assert the invariant the rest of the pipeline relies on.
    #[inline]
    fn also_check(self, _step_m: f32) -> Self {
        debug_assert!(
            self.heading.abs() <= std::f32::consts::PI + 1e-3,
            "interpolated heading left the wrapped range: {}",
            self.heading
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_at(distance: f32, heading: f32) -> Sample {
        Sample {
            t_ms: (distance * 10.0) as i64,
            lap_distance: distance,
            lap_frac: distance / 1000.0,
            pos: [distance, 0.0, 0.0],
            heading,
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
            lap_time_ms: (distance * 10.0) as i32,
        }
    }

    #[test]
    fn grid_is_anchored_to_the_start_line_not_the_first_sample() {
        // Two laps whose first samples are offset from each other must still
        // land on the same absolute distances, or index i means different
        // places in different laps and lap comparison is meaningless.
        let a: Vec<Sample> = (0..20)
            .map(|i| sample_at(0.4 + i as f32 * 2.0, 0.0))
            .collect();
        let b: Vec<Sample> = (0..20)
            .map(|i| sample_at(0.9 + i as f32 * 2.0, 0.0))
            .collect();

        let ra = resample_lap(&a, 1.0).expect("lap a");
        let rb = resample_lap(&b, 1.0).expect("lap b");

        assert_eq!(ra.samples[0].lap_distance, 1.0);
        assert_eq!(rb.samples[0].lap_distance, 1.0);
        for s in ra.samples.iter().chain(rb.samples.iter()) {
            assert_eq!(s.lap_distance.fract(), 0.0, "off-grid: {}", s.lap_distance);
        }
    }

    #[test]
    fn stale_repeated_positions_are_dropped() {
        // The AC staleness pattern: every position repeated once.
        let mut raw = Vec::new();
        for i in 0..20 {
            let d = i as f32 * 2.0;
            raw.push(sample_at(d, 0.0));
            raw.push(sample_at(d, 0.0)); // stale repeat
        }
        let r = resample_lap(&raw, 1.0).expect("resample");
        assert_eq!(r.non_monotone_dropped, 20);
        // Output is on the grid and strictly increasing.
        for w in r.samples.windows(2) {
            assert!(w[1].lap_distance > w[0].lap_distance);
        }
    }

    #[test]
    fn heading_interpolation_does_not_fabricate_a_seam_spike() {
        // A lap crossing the +/-pi seam. With a linear lerp the midpoint would
        // come out near 0.0 — a ~pi error, and a colossal fake curvature.
        let raw = vec![
            sample_at(0.0, 3.10),
            sample_at(10.0, -3.10),
            sample_at(20.0, -3.00),
        ];
        let r = resample_lap(&raw, 1.0).expect("resample");
        for s in &r.samples {
            // Every interpolated heading must stay out near ±π. A linear lerp
            // would put the midpoint at ~0.0, a π-sized error.
            assert!(
                s.heading.abs() > 2.9,
                "heading {} at {} m swung through zero instead of across the seam",
                s.heading,
                s.lap_distance
            );
        }
    }

    #[test]
    fn too_short_a_lap_yields_nothing() {
        assert!(resample_lap(&[], 1.0).is_none());
        assert!(resample_lap(&[sample_at(0.0, 0.0)], 1.0).is_none());
        // All-identical distances: nothing to interpolate between.
        let flat = vec![sample_at(5.0, 0.0); 10];
        assert!(resample_lap(&flat, 1.0).is_none());
    }
}
