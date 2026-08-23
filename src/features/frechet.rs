//! Discrete Fréchet distance between two racing lines.
//!
//! Fréchet distance is the "dog walking" metric: the shortest leash that lets a
//! walker traverse path A while a dog traverses path B, neither ever going
//! backwards. It exists to compare curves whose point correspondence is
//! *unknown* — traced at different speeds, with no shared parameter — by
//! searching every monotone pairing for the best one.
//!
//! # No production caller
//!
//! This module is kept deliberately, and deliberately small: nothing in the
//! pipeline calls it. [`crate::features::line`] replaced it for reference-lap
//! selection, because [`crate::features::resample`] anchors every lap's grid at
//! absolute distance 0 and so the correspondence is *not* unknown — establishing
//! it is the whole point of the resampling stage. The measurements are in
//! `line`'s module docs; the short version is that on distance-aligned laps the
//! optimal coupling is the identity coupling, on all six pairs tested, to the
//! last centimetre.
//!
//! What survives is [`discrete_frechet`] as an **independent reference
//! implementation**. `line`'s central claim — that the largest equal-distance
//! separation is an upper bound on the true coupled distance, because the
//! identity coupling is one of the couplings the DP searches — is the one
//! relationship there that holds by construction rather than by measurement on
//! one circuit. Checking it needs a second implementation that does the search
//! for real, and this is it. See
//! `line::tests::frechet_never_exceeds_the_equal_distance_maximum`.
//!
//! The medoid half of the old module was deleted rather than kept. It averaged
//! Fréchet distances across pairs, and since each is a *maximum*, the mean of
//! them ranked laps by a minimax centre while calling the result a medoid — the
//! defect that made an 11 m excursion at one point on one lap outweigh being
//! closest everywhere else.
//!
//! # Cost
//!
//! `O(L²)` per pair — ~18 M steps for two Red Bull Ring laps, ~265 ms measured.
//! The `L × L` table is never needed: only the previous row is read, so this
//! keeps two rows (34 KB rather than 73 MB at 4,286 points per lap). Empty and
//! single-element paths return `None` rather than indexing `[0]` unguarded.

use crate::core::sample::Sample;

/// A racing line as ground-plane points. Elevation is dropped: two laps that
/// differ only in how high the car sat over a kerb are the same line.
pub type Path = Vec<[f32; 2]>;

/// Project samples onto the ground plane. AC's ground plane is X/Z.
pub fn path_of(samples: &[Sample]) -> Path {
    samples.iter().map(|s| [s.pos[0], s.pos[2]]).collect()
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Discrete Fréchet distance in metres, or `None` if either path is empty.
///
/// The *discrete* variant pairs vertices rather than points along the segments
/// between them, so it is invariant to parameterisation only up to the sample
/// spacing — see `densifying_a_path_shifts_the_distance_by_at_most_the_sample_spacing`.
pub fn discrete_frechet(a: &[[f32; 2]], b: &[[f32; 2]]) -> Option<f32> {
    if a.is_empty() || b.is_empty() {
        return None;
    }

    // Rolling rows. `prev[j]` is the best leash length for a[i-1] against b[j].
    let mut prev = vec![0.0f32; b.len()];
    let mut cur = vec![0.0f32; b.len()];

    prev[0] = dist(a[0], b[0]);
    for j in 1..b.len() {
        prev[j] = prev[j - 1].max(dist(a[0], b[j]));
    }

    for &ai in a.iter().skip(1) {
        cur[0] = prev[0].max(dist(ai, b[0]));
        for j in 1..b.len() {
            // Best of: advance on a, advance on b, advance on both.
            let best = prev[j].min(cur[j - 1]).min(prev[j - 1]);
            cur[j] = best.max(dist(ai, b[j]));
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    Some(prev[b.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x_offset: f32, n: usize) -> Path {
        (0..n).map(|i| [x_offset, i as f32]).collect()
    }

    #[test]
    fn a_path_is_zero_distance_from_itself() {
        let p = line(0.0, 50);
        assert_eq!(discrete_frechet(&p, &p), Some(0.0));
    }

    #[test]
    fn parallel_lines_are_their_separation_apart() {
        let a = line(0.0, 50);
        let b = line(3.0, 50);
        let d = discrete_frechet(&a, &b).expect("both non-empty");
        assert!((d - 3.0).abs() < 1e-4, "expected 3.0, got {d}");
    }

    #[test]
    fn densifying_a_path_shifts_the_distance_by_at_most_the_sample_spacing() {
        // The *continuous* Fréchet distance is exactly parameterisation-
        // invariant. The discrete one matches vertices only, so doubling one
        // path's sample rate forces some pairing to a half-step neighbour: here
        // hypot(3.0, 0.5) = 3.041 rather than 3.0.
        let a = line(0.0, 50);
        let b_dense: Path = (0..99).map(|i| [3.0, i as f32 / 2.0]).collect();
        let d = discrete_frechet(&a, &b_dense).expect("both non-empty");
        assert!(
            (d - 3.0).abs() < 0.6,
            "expected ~3.0 within a sample step, got {d}"
        );
    }

    #[test]
    fn it_is_symmetric() {
        let a: Path = (0..40)
            .map(|i| [(i as f32 * 0.1).sin(), i as f32])
            .collect();
        let b: Path = (0..37)
            .map(|i| [(i as f32 * 0.2).cos(), i as f32])
            .collect();
        let ab = discrete_frechet(&a, &b).unwrap();
        let ba = discrete_frechet(&b, &a).unwrap();
        assert!((ab - ba).abs() < 1e-4, "{ab} != {ba}");
    }

    #[test]
    fn empty_paths_give_none_instead_of_panicking() {
        let p = line(0.0, 10);
        assert_eq!(discrete_frechet(&[], &p), None);
        assert_eq!(discrete_frechet(&p, &[]), None);
        assert_eq!(discrete_frechet(&[], &[]), None);
    }

    /// The search must be able to beat the identity coupling, or it would not be
    /// worth having as the oracle `line` checks itself against.
    ///
    /// Two identical shapes offset *along* the path: pairing index `i` with index
    /// `i` is badly wrong, and a monotone re-coupling recovers most of it.
    #[test]
    fn the_coupling_search_beats_the_identity_pairing() {
        let a: Path = (0..200)
            .map(|i| [(i as f32 * 0.05).sin() * 6.0, i as f32])
            .collect();
        // Same curve, shifted 20 samples along its own parameter.
        let b: Path = (0..200)
            .map(|i| [((i + 20) as f32 * 0.05).sin() * 6.0, i as f32])
            .collect();

        let identity = a
            .iter()
            .zip(&b)
            .map(|(p, q)| dist(*p, *q))
            .fold(0.0f32, f32::max);
        let coupled = discrete_frechet(&a, &b).expect("both non-empty");

        assert!(
            coupled < identity,
            "the DP found nothing better than the identity pairing: {coupled} vs {identity}"
        );
    }
}
