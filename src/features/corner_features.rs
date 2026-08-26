//! Per-corner feature extraction: what one lap actually did in one corner.
//!
//! [`corner`](crate::features::corner) detects where corners are and
//! [`TrackModel`](crate::features::track_model::TrackModel) decides which of
//! those detections are real. Both answer questions about *the track*. This
//! module answers a question about *the driver*: given the canonical corner set
//! and a lap on the distance grid, what did that lap do in each corner — how
//! fast it turned in, how slow it went, where it braked, when it got back on
//! the power.
//!
//! Those numbers are the vocabulary the coaching layer argues in. A rule such
//! as "you braked 20 m later than your best lap and still carried 4 km/h less
//! at the apex" needs exactly the fields below, measured identically for every
//! lap, which is why extraction is a pure function of `(grid, model corner)`
//! with no per-lap state.
//!
//! # Conventions
//!
//! * Speeds are sampled at the *model's* boundaries (`start_m`/`end_m`), not a
//!   detection's. Two laps must be sliced at the same place or their entry
//!   speeds are not comparable; the whole point of the canonical set.
//! * `apex_speed_mps` is the speed minimum inside the span — the physical apex,
//!   which can sit away from the geometric one. How far away is itself a
//!   signal: [`CornerFeatures::speed_min_offset_m`] negative means the driver
//!   did all their slowing early (an early apex, giving the exit away), positive
//!   means the car was still shedding speed well past the geometric apex (a
//!   deep entry the exit pays for).
//! * The braking zone is searched *backwards from the corner*, because braking
//!   for a corner happens on the straight before it. The scan tolerates brief
//!   pedal releases shorter than [`FeatureParams::sustain_m`] — a driver who
//!   rolls off for a heartbeat mid-zone has still braked once.
//! * Throttle pickup is searched *forwards from the geometric apex* and
//!   reported relative to it. It may legitimately be ~0 (a sweeper taken flat)
//!   or `None` (full power never sustained inside the search window).
//! * Time in corner comes from the sample clock across the span. Wall-clock
//!   deltas are correct here: within one clean lap nothing pauses, and AC's own
//!   `iCurrentTime` ticks identically.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

use crate::core::ids::{CornerId, LapId};
use crate::core::sample::Sample;
use crate::features::corner::CornerDirection;
use crate::features::resample::ResampledLap;
use crate::features::track_model::{ModelCorner, TrackModel};

/// Knobs for [`extract`] and [`extract_all`]. All are **tuning knobs** —
/// starting values chosen to survive the F138 captures, not settled numbers.
#[derive(Debug, Clone, Copy)]
pub struct FeatureParams {
    /// Brake pedal at or above this counts as braking. Pedal noise on a
    /// trailing foot sits below it; deliberate braking clears it easily.
    pub brake_on: f32,
    /// Throttle at or above this counts as full power. Set just under 1.0 so a
    /// driver who never quite pins it still reads as "on power".
    pub throttle_full: f32,
    /// Sustained distance, metres, before an on/off judgement fires: this long
    /// above [`Self::brake_on`] to brake, below it to have released, at
    /// [`Self::throttle_full`] to count as back on power. Exists because the
    /// grid interpolates between ~2 m-spaced graphics updates, so single-point
    /// judgements would chase interpolation artefacts rather than pedals.
    pub sustain_m: f32,
    /// How far back past the corner start the braking-point search may run,
    /// metres. Bounds the damage of a pedal left resting on the lever down a
    /// long straight. Monza's first-chicane zone is ~300 m; most others are
    /// well inside 150 m.
    pub brake_search_m: f32,
    /// How far forward of the apex the throttle-pickup search may run, metres.
    /// Beyond this the "pickup" belongs to the following straight, not to how
    /// this corner was driven.
    pub throttle_search_m: f32,
}

impl Default for FeatureParams {
    fn default() -> Self {
        Self {
            brake_on: 0.05,
            throttle_full: 0.95,
            sustain_m: 3.0,
            brake_search_m: 150.0,
            throttle_search_m: 150.0,
        }
    }
}

/// One lap's driving, summarised into one canonical corner.
///
/// Everything here is a fact about a single pass; comparing passes is the
/// reference store's job, not this struct's.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CornerFeatures {
    /// Lap the pass came from.
    pub lap_id: LapId,
    /// Canonical corner the pass went through.
    pub corner_id: CornerId,
    pub direction: CornerDirection,

    /// Speed at the corner boundary where the driver turns in, m/s.
    pub entry_speed_mps: f32,
    /// Speed minimum inside the span, m/s — the physical apex.
    pub apex_speed_mps: f32,
    /// Speed at the corner boundary where the driver exits, m/s.
    pub exit_speed_mps: f32,
    /// Where the speed minimum sat relative to the geometric apex, metres.
    /// Negative = slowed early, positive = still slowing late.
    pub speed_min_offset_m: f32,

    /// Where braking began, metres along the spline. `None` if the corner was
    /// taken without braking past [`FeatureParams::brake_on`].
    pub brake_start_m: Option<f32>,
    /// Length of the braking zone measured back from the corner boundary,
    /// metres — `42` is a 42 m zone ending at `start_m`. Negative means
    /// braking began *inside* the span: a later, deeper entry than the
    /// reference lap's. `None` alongside [`Self::brake_start_m`].
    pub braking_length_m: Option<f32>,
    /// Hardest application of the brake from the braking point through the
    /// apex, 0..1.
    pub peak_brake: f32,
    /// Brake still being used around the *canonical* apex — trail braking.
    pub trail_braking: bool,

    /// Distance past the geometric apex where throttle first returned to (and
    /// held) [`FeatureParams::throttle_full`], metres. `None` if that never
    /// happened inside the search window. ~0 means never lifted.
    pub throttle_pickup_offset_m: Option<f32>,
    /// Lowest throttle anywhere inside the span, 0..1 — how much of a lift the
    /// corner forced.
    pub min_throttle_in_corner: f32,

    /// Duration over the span, seconds, from the sample clock.
    pub time_in_corner_s: f32,
    /// Largest absolute slip angle inside the span, radians. The raw material
    /// for understeer/oversteer rules later.
    pub peak_abs_slip_rad: f32,
    /// Grid points inside the span with 3+ tyres off track.
    pub off_track_points: u32,
}

/// Extract features for one canonical corner from one resampled lap.
///
/// Returns `None` when the grid does not actually cover the corner: a lap that
/// ends mid-corner would otherwise produce a truncated exit speed and a fake
/// pickup, which read as facts about driving when they are facts about logging.
pub fn extract(
    grid: &ResampledLap,
    corner: &ModelCorner,
    params: &FeatureParams,
    lap_id: LapId,
) -> Option<CornerFeatures> {
    let samples = &grid.samples;
    if samples.len() < 2 {
        return None;
    }

    // `index_at` rounds to the nearest grid point, so on a clean lap both ends
    // land within half a step of the requested distances. Anything further out
    // means the lap does not reach this corner.
    let lo = grid.index_at(corner.start_m);
    let hi = grid.index_at(corner.end_m);
    if hi <= lo
        || (samples[lo].lap_distance - corner.start_m).abs() > grid.step_m
        || (samples[hi].lap_distance - corner.end_m).abs() > grid.step_m
    {
        return None;
    }

    let apex_idx = first_extreme(samples, lo, hi, |s| s.speed, Ordering::Less);
    // The canonical apex anchors everything that must mean the same place in
    // every lap: trail braking and throttle pickup. The physical speed
    // minimum wanders lap to lap; the learned apex does not.
    let canon = grid.index_at(corner.apex_m);

    let entry_speed_mps = samples[lo].speed;
    let exit_speed_mps = samples[hi].speed;
    let apex_speed_mps = samples[apex_idx].speed;
    let speed_min_offset_m = samples[apex_idx].lap_distance - corner.apex_m;

    let time_in_corner_s = (samples[hi].t_ms - samples[lo].t_ms).max(0) as f32 / 1000.0;

    let min_throttle_in_corner = (lo..=hi)
        .map(|i| samples[i].throttle)
        .min_by(|a, b| a.total_cmp(b))
        .unwrap_or(0.0);

    let peak_abs_slip_rad = (lo..=hi)
        .map(|i| samples[i].slip_angle.abs())
        .max_by(|a, b| a.total_cmp(b))
        .unwrap_or(0.0);

    let off_track_points = (lo..=hi)
        .filter(|&i| samples[i].tyres_out >= OFF_TRACK_TYRES)
        .count() as u32;

    let sustain_pts = ((params.sustain_m / grid.step_m).round() as usize).max(1);

    // Braking is anchored at the *peak* application between entry and the
    // canonical apex rather than at the boundary sample: the pedal is often
    // already rolling off again by `start_m`, and anchoring there would find
    // no zone at all for exactly the hardest-braked corners.
    let peak_brake_idx = first_extreme(samples, lo, canon, |s| s.brake, Ordering::Greater);

    let (brake_start_m, braking_length_m, peak_brake) =
        if samples[peak_brake_idx].brake >= params.brake_on {
            let floor = peak_brake_idx
                .saturating_sub(((params.brake_search_m / grid.step_m).round()) as usize);
            let b = braking_run_start(samples, peak_brake_idx, floor, sustain_pts, params.brake_on);
            let peak = (b..=peak_brake_idx)
                .map(|i| samples[i].brake)
                .max_by(|a, b| a.total_cmp(b))
                .unwrap_or(samples[peak_brake_idx].brake);
            (
                Some(samples[b].lap_distance),
                Some(corner.start_m - samples[b].lap_distance),
                peak,
            )
        } else {
            (None, None, samples[peak_brake_idx].brake.max(0.0))
        };

    // Trail braking: any real brake application within ±sustain of the
    // canonical apex. The physical minimum would drift lap to lap and make the
    // flag mean different places in different laps.
    let trail_half = sustain_pts;
    let t_lo = canon.saturating_sub(trail_half);
    let t_hi = (canon + trail_half).min(samples.len() - 1);
    let trail_braking = (t_lo..=t_hi).any(|i| samples[i].brake >= params.brake_on);

    let search_fwd = ((params.throttle_search_m / grid.step_m).round()) as usize;
    let limit = (canon + search_fwd).min(samples.len() - 1);
    let throttle_pickup_offset_m = (canon..=limit).find_map(|j| {
        let window_end = (j + sustain_pts - 1).min(limit);
        if samples[j].throttle < params.throttle_full {
            return None;
        }
        (j..=window_end)
            .all(|k| samples[k].throttle >= params.throttle_full)
            .then(|| samples[j].lap_distance - corner.apex_m)
    });

    Some(CornerFeatures {
        lap_id,
        corner_id: corner.id,
        direction: corner.direction,
        entry_speed_mps,
        apex_speed_mps,
        exit_speed_mps,
        speed_min_offset_m,
        brake_start_m,
        braking_length_m,
        peak_brake,
        trail_braking,
        throttle_pickup_offset_m,
        min_throttle_in_corner,
        time_in_corner_s,
        peak_abs_slip_rad,
        off_track_points,
    })
}

/// Extract features for every corner of a learned model, skipping corners the
/// grid does not cover. Output order follows the model, so entry *i* is
/// corner *i* whenever lengths match.
pub fn extract_all(
    model: &TrackModel,
    grid: &ResampledLap,
    params: &FeatureParams,
    lap_id: LapId,
) -> Vec<CornerFeatures> {
    model
        .corners
        .iter()
        .filter_map(|c| extract(grid, c, params, lap_id))
        .collect()
}

/// Index of the *first* extreme of `key` within `samples[lo..=hi]`.
///
/// Ties resolve to the earliest index deliberately. Under a plateau — a
/// constant-speed stretch, a pedal held steady — `min_by`/`max_by` return the
/// **last** equal element, which would silently place a corner's "physical
/// apex" at its exit and anchor braking searches 40 m too late. The first
/// moment an extreme is reached is the one a driver would point at.
///
/// `want` is [`Ordering::Less`] for a minimum, [`Ordering::Greater`] for a
/// maximum; `total_cmp` makes the choice total, so NaN input sorts rather than
/// panics and can never win either way.
fn first_extreme(
    samples: &[Sample],
    lo: usize,
    hi: usize,
    key: impl Fn(&Sample) -> f32,
    want: Ordering,
) -> usize {
    let mut best = lo;
    for i in (lo + 1)..=hi {
        if key(&samples[i]).total_cmp(&key(&samples[best])) == want {
            best = i;
        }
    }
    best
}

/// Furthest-back index of the braking run that reaches `from`.
///
/// Walks backwards allowing gaps of up to `sustain - 1` consecutive
/// below-threshold samples — a momentary roll-off inside one braking zone is
/// the same braking — but stops at the first gap long enough to be a genuine
/// release, so the zone never swallows the previous corner's braking.
fn braking_run_start(
    samples: &[Sample],
    from: usize,
    floor: usize,
    sustain: usize,
    brake_on: f32,
) -> usize {
    let mut start = from;
    let mut gap = 0usize;
    let mut i = from;
    while i > floor {
        i -= 1;
        if samples[i].brake >= brake_on {
            start = i;
            gap = 0;
        } else {
            gap += 1;
            if gap >= sustain {
                break;
            }
        }
    }
    start
}

/// Tyres-off threshold, matching [`crate::features::lap`]: three or more
/// wheels off is AC's own definition of leaving the track.
const OFF_TRACK_TYRES: u8 = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::TrackId;
    use crate::core::sample::Sim;
    use crate::features::track_model::{MODEL_VERSION, Provenance};

    /// A straight-line lap on a 1 m grid with channel profiles under test
    /// control. Extraction slices by distance, so geometry beyond a straight
    /// line is irrelevant here.
    fn lap(
        n: usize,
        speed: impl Fn(f32) -> f32,
        brake: impl Fn(f32) -> f32,
        throttle: impl Fn(f32) -> f32,
    ) -> ResampledLap {
        ResampledLap {
            samples: (0..n)
                .map(|i| {
                    let d = i as f32;
                    Sample {
                        t_ms: (d * 33.0) as i64,
                        lap_distance: d,
                        lap_frac: d / 1200.0,
                        pos: [0.0, 0.0, d],
                        heading: 0.0,
                        speed: speed(d),
                        throttle: throttle(d),
                        brake: brake(d),
                        steer: 0.0,
                        yaw_rate: 0.0,
                        slip_angle: 0.0,
                        gear: 4,
                        rpm: 6000.0,
                        tyres_out: 0,
                        surface_grip: 1.0,
                        lap_time_ms: (d * 33.0) as i32,
                    }
                })
                .collect(),
            step_m: 1.0,
            non_monotone_dropped: 0,
        }
    }

    /// An 80 m corner spanning 300–380 m with its geometric apex at 340 m.
    fn corner90() -> ModelCorner {
        ModelCorner {
            id: CornerId(3),
            start_m: 300.0,
            end_m: 380.0,
            apex_m: 340.0,
            heading_apex_m: 338.0,
            direction: CornerDirection::Right,
            turn_angle: 1.5,
            peak_curvature: 0.02,
            support: 3,
        }
    }

    const P: FeatureParams = FeatureParams {
        brake_on: 0.05,
        throttle_full: 0.95,
        sustain_m: 3.0,
        brake_search_m: 150.0,
        throttle_search_m: 150.0,
    };

    #[test]
    fn a_flat_out_corner_has_no_brakes_and_no_lift() {
        let g = lap(500, |_| 50.0, |_| 0.0, |_| 1.0);
        let f = extract(&g, &corner90(), &P, LapId(7)).expect("covered");

        assert_eq!(f.lap_id, LapId(7));
        assert_eq!(f.corner_id, CornerId(3));
        assert_eq!(f.direction, CornerDirection::Right);
        assert_eq!(f.entry_speed_mps, 50.0);
        assert_eq!(f.apex_speed_mps, 50.0);
        assert_eq!(f.exit_speed_mps, 50.0);
        assert_eq!(f.brake_start_m, None);
        assert_eq!(f.braking_length_m, None);
        assert!(!f.trail_braking);
        // Full throttle already held at the apex: pickup lands immediately.
        let pickup = f.throttle_pickup_offset_m.expect("flat out");
        assert!(
            (-1.0..=1.0).contains(&pickup),
            "pickup should be ~0, got {pickup}"
        );
        assert_eq!(f.min_throttle_in_corner, 1.0);
        assert_eq!(f.off_track_points, 0);
    }

    #[test]
    fn time_in_corner_comes_from_the_sample_clock() {
        let g = lap(500, |_| 50.0, |_| 0.0, |_| 1.0);
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");

        // 80 m of span = 81 grid points = 80 one-metre steps at 33 ms each.
        let expected = 80.0 * 0.033;
        assert!(
            (f.time_in_corner_s - expected).abs() < 1e-3,
            "time {} vs expected {expected}",
            f.time_in_corner_s
        );
    }

    #[test]
    fn the_braking_zone_reaches_back_before_the_corner_and_tolerates_a_lift() {
        // Brake from 200 m to the corner at 300 m, with a one-sample dip at
        // 250 m — shorter than the 3 m sustain, so the zone must survive it.
        let g = lap(
            500,
            |d| {
                if d < 200.0 {
                    70.0
                } else if d < 300.0 {
                    70.0 - (d - 200.0) * 0.4
                } else {
                    30.0
                }
            },
            |d| {
                if (200.0..305.0).contains(&d) && (d - 250.0).abs() > 0.5 {
                    0.8
                } else {
                    0.0
                }
            },
            |_| 1.0,
        );
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");

        let start = f.brake_start_m.expect("there is a braking zone");
        assert!(
            (start - 200.0).abs() <= 2.0,
            "braking should begin near 200 m, got {start}"
        );
        let len = f.braking_length_m.expect("zone length");
        assert!(
            (len - 100.0).abs() <= 2.0,
            "zone should be ~100 m long, got {len}"
        );
        assert!(
            (f.peak_brake - 0.8).abs() < 1e-4,
            "peak brake {}",
            f.peak_brake
        );
        // Braking ended right after the corner began, so no trail braking.
        assert!(!f.trail_braking);
    }

    #[test]
    fn holding_the_brake_past_the_apex_is_trail_braking() {
        let g = lap(
            500,
            |d| if d < 300.0 { 60.0 } else { 35.0 },
            |d| {
                if (250.0..360.0).contains(&d) {
                    0.4
                } else {
                    0.0
                }
            },
            |_| 1.0,
        );
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");
        assert!(f.trail_braking, "brake held to 360 m, 20 m past the apex");
    }

    #[test]
    fn releasing_the_brake_at_entry_is_not_trail_braking() {
        let g = lap(
            500,
            |d| if d < 300.0 { 55.0 } else { 40.0 },
            |d| {
                if (250.0..302.0).contains(&d) {
                    0.6
                } else {
                    0.0
                }
            },
            |_| 1.0,
        );
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");
        assert!(!f.trail_braking, "pedal released 38 m before the apex");
    }

    #[test]
    fn throttle_pickup_is_measured_from_the_apex() {
        // Off power through the corner, full from 360 m — 20 m past the apex.
        let g = lap(
            500,
            |d| if d < 340.0 { 40.0 } else { 42.0 },
            |_| 0.0,
            |d| if d >= 360.0 { 1.0 } else { 0.2 },
        );
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");

        let pickup = f.throttle_pickup_offset_m.expect("power returns");
        assert!(
            (pickup - 20.0).abs() <= 2.0,
            "pickup should be ~+20 m past the apex, got {pickup:+.1}m"
        );
        assert_eq!(f.min_throttle_in_corner, 0.2);
    }

    #[test]
    fn a_lift_that_never_returns_to_full_power_gives_none() {
        let g = lap(
            500,
            |_| 40.0,
            |_| 0.0,
            |d| if d < 400.0 { 0.5 } else { 0.9 },
        );
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");
        // 0.9 is below the 0.95 threshold everywhere inside the window.
        assert_eq!(f.throttle_pickup_offset_m, None);
    }

    #[test]
    fn slip_and_off_track_excursions_are_counted() {
        let mut g = lap(500, |_| 45.0, |_| 0.0, |_| 1.0);
        // A slide at the apex and three metres of wheels off just after it.
        g.samples[340].slip_angle = 0.18;
        g.samples[341].slip_angle = -0.09;
        g.samples[342].tyres_out = 3;
        g.samples[343].tyres_out = 4;
        g.samples[344].tyres_out = 3;
        // Off-track outside the span must not count.
        g.samples[290].tyres_out = 4;

        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");
        assert!(
            (f.peak_abs_slip_rad - 0.18).abs() < 1e-6,
            "peak slip {}",
            f.peak_abs_slip_rad
        );
        assert_eq!(f.off_track_points, 3);
    }

    #[test]
    fn the_speed_minimum_reports_how_far_it_sat_from_the_geometric_apex() {
        // Slowest point at 330 m, 10 m before the geometric apex at 340 m.
        let g = lap(
            500,
            |d| 40.0 - 10.0 * (-((d - 330.0).abs() / 25.0)).exp(),
            |_| 0.0,
            |_| 1.0,
        );
        let f = extract(&g, &corner90(), &P, LapId(0)).expect("covered");
        assert!(
            (f.speed_min_offset_m + 10.0).abs() <= 2.0,
            "minimum should sit ~-10 m from the apex, got {:+.1}m",
            f.speed_min_offset_m
        );
        assert!(f.apex_speed_mps < 31.5 && f.apex_speed_mps > 29.0);
    }

    #[test]
    fn a_corner_the_grid_does_not_cover_is_skipped() {
        // Grid runs 100–500 m; the corner starts at 80 m.
        let short_head: Vec<Sample> = lap(500, |_| 40.0, |_| 0.0, |_| 1.0)
            .samples
            .into_iter()
            .skip(100)
            .collect();
        let g = ResampledLap {
            samples: short_head,
            step_m: 1.0,
            non_monotone_dropped: 0,
        };
        assert!(extract(&g, &corner90(), &P, LapId(0)).is_none());

        // And one that ends past the grid: the lap stops at 449 m, the corner
        // at 470 m.
        let tail: Vec<Sample> = lap(450, |_| 40.0, |_| 0.0, |_| 1.0).samples;
        let g2 = ResampledLap {
            samples: tail,
            step_m: 1.0,
            non_monotone_dropped: 0,
        };
        let late = ModelCorner {
            start_m: 420.0,
            end_m: 470.0,
            ..corner90()
        };
        assert!(extract(&g2, &late, &P, LapId(0)).is_none());
    }

    fn model(corners: Vec<ModelCorner>) -> TrackModel {
        TrackModel {
            version: MODEL_VERSION,
            sim: Sim::AssettoCorsa,
            track: TrackId::new("test_circuit", ""),
            track_length_m: 1200.0,
            provenance: Provenance {
                car: "test_car".to_string(),
                capture: "cap.ndjson".to_string(),
                reference_lap: LapId(0),
                lap_ids: vec![LapId(0), LapId(1)],
                reference_spread_m: 0.5,
                reference_spread_max_m: 1.0,
                reference_spread_max_at_m: 100.0,
                step_m: 1.0,
            },
            corners,
        }
    }

    #[test]
    fn extract_all_walks_the_model_in_order_and_skips_uncovered_corners() {
        let m = model(vec![
            corner90(),
            ModelCorner {
                id: CornerId(1),
                start_m: 700.0,
                end_m: 900.0,
                apex_m: 800.0,
                ..corner90()
            },
        ]);

        // Grid stops at 600 m: the second corner cannot be covered.
        let g = lap(600, |_| 40.0, |_| 0.0, |_| 1.0);
        let all = extract_all(&m, &g, &P, LapId(2));

        assert_eq!(all.len(), 1, "only the covered corner yields features");
        assert_eq!(all[0].corner_id, CornerId(3));
        assert_eq!(all[0].lap_id, LapId(2));

        // With full coverage, order follows the model regardless of ids.
        let g = lap(1200, |_| 40.0, |_| 0.0, |_| 1.0);
        let all = extract_all(&m, &g, &P, LapId(0));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].corner_id, CornerId(3));
        assert_eq!(all[1].corner_id, CornerId(1));
    }

    #[test]
    fn a_degenerate_grid_yields_nothing_rather_than_panicking() {
        let empty = ResampledLap {
            samples: vec![],
            step_m: 1.0,
            non_monotone_dropped: 0,
        };
        assert!(extract(&empty, &corner90(), &P, LapId(0)).is_none());
    }
}
