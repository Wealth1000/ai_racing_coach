//! Stage 3 of the canonical corner learner: decisions inside arcs.
//!
//! Counting curvature arcs is not counting corners: Maggotts–Becketts–Chapel
//! is several decisions inside roughly one geometric gesture, and a linked
//! sequence of flat-out esses can be several gestures that are one decision.
//! The *decisive* signal is where the driver works — pedals, and occasionally
//! the yaw trace — and it is treated exactly the way geometry was in Stage 2:
//! extract events per lap, then keep only the events that recur.
//!
//! # Pedal levels are derived, never fixed
//!
//! A pedal trace is bimodal by nature — a foot is either on a pedal or off
//! it — so on/off levels come from each lap's own distribution
//! (median + modified-z of the MAD), not from constants like 0.05/0.95.
//! "Sustained" means over the resolution floor (10 m): the grid interpolates
//! between graphics updates, so single-point judgements would chase
//! interpolation artefacts rather than feet.
//!
//! # Degradation, stated honestly
//!
//! With dead pedal channels (AMS2 replay has none) this module produces no
//! events and the model reports the geometric arc count, flagged as such in
//! its provenance. That is a documented retreat from the full goal, not a
//! silent one.

use serde::{Deserialize, Serialize};

use crate::features::curvature;
use crate::features::resample::ResampledLap;
use crate::features::segment::{MODIFIED_Z, RESOLUTION_FLOOR_M};
use crate::features::stats;

/// MAD floor for pedal channels, 0..1 scale.
///
/// A channel that never moves has MAD exactly 0; without a floor every
/// float-tiny wiggle would read as pedal work.
const PEDAL_MAD_FLOOR: f32 = 1e-3;

/// A kind of decision a driver can be observed making.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum DecisionKind {
    /// First sample of a braking run (gaps shorter than the sustain window
    /// tolerated — a momentary roll-off inside one zone is the same braking).
    BrakeOnset,
    /// End of that run.
    BrakeRelease,
    /// Local minimum of throttle below the lap's own flat-out level.
    ThrottleDip,
    /// Sustained return to the flat-out level after a dip.
    ThrottlePickup,
    /// A yaw reversal with the pedals flat — the mid-throttle flick.
    FlatDirectionChange,
}

impl DecisionKind {
    pub fn name(self) -> &'static str {
        match self {
            DecisionKind::BrakeOnset => "BrakeOnset",
            DecisionKind::BrakeRelease => "BrakeRelease",
            DecisionKind::ThrottleDip => "ThrottleDip",
            DecisionKind::ThrottlePickup => "ThrottlePickup",
            DecisionKind::FlatDirectionChange => "FlatDirectionChange",
        }
    }
}

/// A confirmed decision boundary inside one canonical corner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEvent {
    pub kind: DecisionKind,
    /// Metres along the spline. May sit outside the corner's geometric span
    /// — a brake onset belongs to the corner but happens before it.
    pub distance_m: f32,
    /// Laps that independently produced this event.
    pub support: u32,
}

/// One lap's raw event, before cross-lap confirmation.
#[derive(Debug, Clone, Copy)]
pub struct LapEvent {
    pub kind: DecisionKind,
    pub distance_m: f32,
}

/// Per-lap pedal levels, derived from the trace's own distribution.
#[derive(Debug, Clone, Copy)]
pub struct PedalLevels {
    /// Brake at or above this is braking.
    pub brake_on: f32,
    /// Throttle at or above this is the lap's own flat-out.
    pub throttle_flat: f32,
    /// Throttle below this is a lift.
    pub throttle_lift: f32,
}

impl PedalLevels {
    /// Whether either pedal channel moved at all this lap. A channel that
    /// never changes value is dead for the session (AMS2 replay publishes
    /// none) and Stage 3 must say so rather than extract events from noise.
    ///
    /// Range, not MAD: a brake trace that is off for 80% of the lap has a
    /// MAD of exactly zero while being perfectly alive, so a MAD-based
    /// liveness test would declare every brake channel dead.
    pub fn pedals_live(&self, lap: &ResampledLap) -> bool {
        fn live(values: impl Iterator<Item = f32>) -> bool {
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            for v in values {
                min = min.min(v);
                max = max.max(v);
            }
            (max - min) > PEDAL_MAD_FLOOR
        }
        live(lap.samples.iter().map(|s| s.brake))
            || live(lap.samples.iter().map(|s| s.throttle))
    }
}

/// Derive a lap's pedal on/off levels from the traces themselves.
pub fn pedal_levels(lap: &ResampledLap) -> PedalLevels {
    let brake: Vec<f32> = lap.samples.iter().map(|s| s.brake).collect();
    let throttle: Vec<f32> = lap.samples.iter().map(|s| s.throttle).collect();

    let brake_on = stats::median(&brake) + MODIFIED_Z * stats::sigma_from_mad(&brake, PEDAL_MAD_FLOOR);

    // "Flat out" is whatever this driver's flat is: the upper quantile of
    // their own throttle trace. On a circuit with few braking zones that is
    // 1.0; on a street circuit it is lower, and comparing against 0.95 there
    // would read every corner exit as a lift.
    let throttle_flat = stats::quantile(&throttle, 0.75);
    let throttle_lift =
        throttle_flat - MODIFIED_Z * stats::sigma_from_mad(&throttle, PEDAL_MAD_FLOOR);

    PedalLevels {
        brake_on,
        throttle_flat,
        throttle_lift,
    }
}

/// Extract the lap's decision events, in distance order.
pub fn lap_events(lap: &ResampledLap, levels: &PedalLevels) -> Vec<LapEvent> {
    let samples = &lap.samples;
    let n = samples.len();
    if n < 2 {
        return Vec::new();
    }
    let step = lap.step_m;
    let sustain = ((RESOLUTION_FLOOR_M / step).round() as usize).max(1);
    let mut events = Vec::new();

    // --- Braking runs: onset and release ---------------------------------
    let braking: Vec<bool> = samples.iter().map(|s| s.brake >= levels.brake_on).collect();
    for run in runs(&braking, sustain) {
        events.push(LapEvent {
            kind: DecisionKind::BrakeOnset,
            distance_m: samples[run.0].lap_distance,
        });
        events.push(LapEvent {
            kind: DecisionKind::BrakeRelease,
            distance_m: samples[run.1].lap_distance,
        });
    }

    // --- Throttle lifts: dip and sustained pickup -------------------------
    let lifted: Vec<bool> = samples
        .iter()
        .map(|s| s.throttle < levels.throttle_lift)
        .collect();
    for run in runs(&lifted, sustain) {
        // The dip is the deepest point of the lift, not its first sample —
        // a driver eases in and out of a lift, and the decision anchor is
        // where the pedal was most off.
        let mut deepest = run.0;
        for i in run.0..=run.1 {
            if samples[i].throttle < samples[deepest].throttle {
                deepest = i;
            }
        }
        events.push(LapEvent {
            kind: DecisionKind::ThrottleDip,
            distance_m: samples[deepest].lap_distance,
        });

        // Pickup: the first sustained return to flat after the lift ends.
        let mut pickup = run.1;
        let mut held = 0usize;
        for i in (run.1 + 1)..n {
            if samples[i].throttle >= levels.throttle_flat {
                held += 1;
                if held == 1 {
                    pickup = i;
                }
                if held >= sustain {
                    break;
                }
            } else {
                held = 0;
            }
        }
        events.push(LapEvent {
            kind: DecisionKind::ThrottlePickup,
            distance_m: samples[pickup].lap_distance,
        });
    }

    // --- Flat-pedal direction changes --------------------------------------
    let flat = |i: usize| {
        samples[i].throttle >= levels.throttle_flat && samples[i].brake < levels.brake_on
    };
    let signed = curvature::signed_curvature(samples);
    let smoothed = curvature::smooth(&signed, step, curvature::SMOOTH_WINDOW_M);
    let mut last_emitted = f32::NEG_INFINITY;
    for i in sustain..n.saturating_sub(sustain) {
        let a = smoothed[i - sustain];
        let b = smoothed[i + sustain];
        let noise = |v: f32| v.abs() > MODIFIED_Z * stats::sigma_from_mad(&signed, 1e-7);
        if a * b < 0.0 && noise(a) && noise(b) {
            // Flat across the whole comparison window, and not double-counted
            // with the previous event.
            let window_flat = (i - sustain..=i + sustain).all(&flat);
            if window_flat && samples[i].lap_distance - last_emitted >= RESOLUTION_FLOOR_M {
                events.push(LapEvent {
                    kind: DecisionKind::FlatDirectionChange,
                    distance_m: samples[i].lap_distance,
                });
                last_emitted = samples[i].lap_distance;
            }
        }
    }

    events.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    events
}

/// Maximal true-runs of a boolean series, bridging false-gaps shorter than
/// `sustain`: a momentary return inside one braking zone or lift is the same
/// event, exactly as [`crate::features::corner_features::braking_run_start`]
/// has always treated braking.
fn runs(flags: &[bool], sustain: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut gap = 0usize;
    for (i, &f) in flags.iter().enumerate() {
        if f {
            if start.is_none() {
                start = Some(i);
            }
            gap = 0;
        } else if start.is_some() {
            gap += 1;
            if gap >= sustain {
                out.push((start.take().expect("checked"), i - gap));
                gap = 0;
            }
        }
    }
    if let Some(s) = start {
        out.push((s, flags.len() - 1));
    }
    out
}

/// One arc-shaped window the events of a lap are distributed over:
/// the arc's (unwrapped, linear) span, which the assignment rules in
/// [`assign_events`] extend along the ring to the neighbouring arcs.
#[derive(Debug, Clone, Copy)]
pub struct EventWindow {
    pub start_m: f32,
    pub end_m: f32,
}

/// Assign a lap's events to arcs.
///
/// The rules are physical, not positional bookkeeping:
///
/// * a **brake run** (onset + release) belongs to the arc it brakes for —
///   the first arc ending at or after the onset, wrapping;
/// * a **throttle dip** belongs to the arc it prepares or lives in — the
///   first arc ending at or after it;
/// * a **throttle pickup** belongs to the arc it exits — the last arc
///   starting at or before it, wrapping;
/// * a **flat direction change** happens inside an arc — first arc ending at
///   or after it.
///
/// Returns, per arc, that lap's events in distance order.
pub fn assign_events(events: &[LapEvent], windows: &[EventWindow]) -> Vec<Vec<LapEvent>> {
    let mut per_arc = vec![Vec::new(); windows.len()];
    let n = windows.len();
    if n == 0 {
        return per_arc;
    }

    let first_ending_after = |d: f32| -> usize {
        windows
            .iter()
            .position(|w| w.end_m >= d)
            .unwrap_or(0) // past the last arc: the first arc of the next lap
    };
    let last_starting_before = |d: f32| -> usize {
        let mut best = 0;
        for (i, w) in windows.iter().enumerate() {
            if w.start_m <= d {
                best = i;
            } else {
                break;
            }
        }
        best // before the first arc: wraps to the last arc of the lap
    };

    for e in events {
        let arc = match e.kind {
            DecisionKind::BrakeOnset | DecisionKind::BrakeRelease => {
                // Pair the release with its run: the onset's arc decides both,
                // so a release that trails past the geometric end still
                // belongs to the corner that was being braked.
                // (`events` is in distance order, so the onset of this run is
                // the nearest preceding BrakeOnset.)
                let onset = per_arc
                    .iter()
                    .flatten()
                    .rev()
                    .find(|ev| ev.kind == DecisionKind::BrakeOnset)
                    .map(|ev| {
                        windows
                            .iter()
                            .position(|w| w.end_m >= ev.distance_m)
                            .unwrap_or(0)
                    });
                match (e.kind, onset) {
                    (DecisionKind::BrakeRelease, Some(a)) => a,
                    _ => first_ending_after(e.distance_m),
                }
            }
            DecisionKind::ThrottleDip | DecisionKind::FlatDirectionChange => {
                first_ending_after(e.distance_m)
            }
            DecisionKind::ThrottlePickup => last_starting_before(e.distance_m),
        };
        per_arc[arc].push(*e);
    }

    per_arc
}

/// Cluster one arc's same-kind event distances across laps and keep the
/// clusters that recur with cross-lap confidence.
///
/// Deliberately the same machinery as Stage 2 applied one level down — no
/// third mechanism. The linkage tolerance is the cluster's own MAD floored
/// at the resolution limit, as everywhere.
pub fn confirm_events(
    kind: DecisionKind,
    // Per lap: every event of this kind this lap produced in this arc.
    per_lap: &[Vec<f32>],
) -> Vec<DecisionEvent> {
    let laps = per_lap.len() as u32;
    if laps == 0 {
        return Vec::new();
    }

    let all: Vec<f32> = per_lap.iter().flatten().copied().collect();
    if all.is_empty() {
        return Vec::new();
    }

    // Single-linkage clustering at the event set's own spread, floored at
    // the resolution limit. Sort first: consecutive gaps decide linkage.
    let tolerance = stats::sigma_from_mad(&all, RESOLUTION_FLOOR_M);
    let mut sorted = all.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));

    let mut clusters: Vec<Vec<f32>> = vec![vec![sorted[0]]];
    for &d in &sorted[1..] {
        let cluster = clusters.last_mut().expect("seeded above");
        if d - *cluster.last().expect("non-empty") <= tolerance {
            cluster.push(d);
        } else {
            clusters.push(vec![d]);
        }
    }

    let mut confirmed = Vec::new();
    for cluster in clusters {
        // Which laps contributed to this cluster (a lap may contribute more
        // than one event — the "brakes, releases, brakes again" complex).
        let (lo, hi) = (
            *cluster.first().expect("non-empty") - tolerance,
            *cluster.last().expect("non-empty") + tolerance,
        );
        let member_laps = per_lap
            .iter()
            .filter(|events| events.iter().any(|d| *d >= lo && *d <= hi))
            .count() as u32;

        // The same bound as corners: recurring with statistical confidence.
        if crate::features::consensus::majority_confirmed(member_laps, laps) {
            confirmed.push(DecisionEvent {
                kind,
                distance_m: stats::median(&cluster),
                support: member_laps,
            });
        }
    }
    confirmed.sort_by(|a, b| a.distance_m.total_cmp(&b.distance_m));
    confirmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample::Sample;

    /// A straight-line lap on a 1 m grid, with per-sample channel overrides.
    fn straight_lap(channels: impl Fn(usize) -> (f32, f32)) -> ResampledLap {
        let samples: Vec<Sample> = (0..600)
            .map(|i| {
                let (throttle, brake) = channels(i);
                let d = i as f32;
                Sample {
                    t_ms: (d * 33.0) as i64,
                    lap_distance: d,
                    lap_frac: d / 600.0,
                    pos: [0.0, 0.0, d],
                    heading: 0.0,
                    speed: 50.0,
                    throttle,
                    brake,
                    steer: 0.0,
                    yaw_rate: 0.0,
                    slip_angle: 0.0,
                    gear: 4,
                    rpm: 6000.0,
                    tyres_out: 0,
                    live: true,
                    surface_grip: 1.0,
                    lap_time_ms: (d * 33.0) as i32,
                }
            })
            .collect();
        ResampledLap {
            samples,
            step_m: 1.0,
            non_monotone_dropped: 0,
            first_distance_m: 0.0,
        }
    }

    #[test]
    fn pedal_levels_come_from_the_trace_not_a_constant() {
        // Mostly flat with one deep brake-and-lift zone: the levels must sit
        // between the modes, wherever the driver happens to hold them.
        let lap = straight_lap(|i| {
            if (200..260).contains(&i) {
                (0.2, 0.7)
            } else {
                (1.0, 0.0)
            }
        });
        let levels = pedal_levels(&lap);
        assert!(levels.brake_on > 0.0 && levels.brake_on < 0.7);
        assert!((levels.throttle_flat - 1.0).abs() < 1e-6);
        assert!(levels.throttle_lift < 1.0 && levels.throttle_lift > 0.2);
    }

    #[test]
    fn a_dead_pedal_channel_is_flagged() {
        let lap = straight_lap(|_| (1.0, 0.0));
        let levels = pedal_levels(&lap);
        assert!(!levels.pedals_live(&lap), "a flat trace is a dead channel");

        let live = straight_lap(|i| if i < 100 { (1.0, 0.5) } else { (1.0, 0.0) });
        assert!(
            pedal_levels(&live).pedals_live(&live),
            "a mostly-off brake channel with real applications is alive"
        );
    }

    #[test]
    fn a_braking_run_produces_onset_and_release() {
        let lap = straight_lap(|i| {
            if (200..260).contains(&i) {
                (0.1, 0.8)
            } else {
                (1.0, 0.0)
            }
        });
        let events = lap_events(&lap, &pedal_levels(&lap));

        let onset = events
            .iter()
            .find(|e| e.kind == DecisionKind::BrakeOnset)
            .expect("an onset");
        let release = events
            .iter()
            .find(|e| e.kind == DecisionKind::BrakeRelease)
            .expect("a release");
        assert!((onset.distance_m - 200.0).abs() <= 2.0, "onset at {}", onset.distance_m);
        assert!((release.distance_m - 259.0).abs() <= 2.0, "release at {}", release.distance_m);
    }

    #[test]
    fn a_momentary_release_inside_a_zone_is_the_same_braking() {
        // The brake dips below the level for 4 m inside a 60 m zone: with a
        // 10 m sustain window that is one run, not two.
        let lap = straight_lap(|i| {
            if (200..260).contains(&i) {
                let brake = if (230..234).contains(&i) { 0.0 } else { 0.8 };
                (0.1, brake)
            } else {
                (1.0, 0.0)
            }
        });
        let events = lap_events(&lap, &pedal_levels(&lap));
        let onsets = events
            .iter()
            .filter(|e| e.kind == DecisionKind::BrakeOnset)
            .count();
        assert_eq!(onsets, 1, "one braking run despite the blip");
    }

    #[test]
    fn a_throttle_lift_produces_a_dip_and_a_sustained_pickup() {
        let lap = straight_lap(|i| {
            if (300..340).contains(&i) {
                (0.4, 0.0)
            } else {
                (1.0, 0.0)
            }
        });
        let events = lap_events(&lap, &pedal_levels(&lap));

        let dip = events
            .iter()
            .find(|e| e.kind == DecisionKind::ThrottleDip)
            .expect("a dip");
        let pickup = events
            .iter()
            .find(|e| e.kind == DecisionKind::ThrottlePickup)
            .expect("a pickup");
        assert!(dip.distance_m >= 300.0 && dip.distance_m <= 340.0);
        assert!(pickup.distance_m >= 340.0, "pickup after the lift ends");
    }

    #[test]
    fn events_are_assigned_to_the_arc_they_belong_to() {
        // Two arcs: 300–380 and 700–800. A braking run starting on the
        // straight before the first arc belongs to arc 0; a pickup 30 m
        // after the second arc's end belongs to arc 1.
        let windows = vec![
            EventWindow {
                start_m: 300.0,
                end_m: 380.0,
            },
            EventWindow {
                start_m: 700.0,
                end_m: 800.0,
            },
        ];
        let events = vec![
            LapEvent {
                kind: DecisionKind::BrakeOnset,
                distance_m: 250.0,
            },
            LapEvent {
                kind: DecisionKind::BrakeRelease,
                distance_m: 330.0,
            },
            LapEvent {
                kind: DecisionKind::ThrottlePickup,
                distance_m: 830.0,
            },
        ];
        let assigned = assign_events(&events, &windows);
        assert_eq!(assigned[0].len(), 2, "brake run to the arc it brakes for");
        assert_eq!(assigned[1].len(), 1, "pickup to the arc it exits");
    }

    #[test]
    fn recurring_events_confirm_and_one_lap_wonders_do_not() {
        // Three laps; every lap lifts at ~500 m, and lap 0 alone also lifts
        // at 620 m.
        let per_lap: Vec<Vec<f32>> = vec![
            vec![500.0, 620.0],
            vec![504.0],
            vec![497.0],
        ];
        let confirmed = confirm_events(DecisionKind::ThrottleDip, &per_lap);

        assert_eq!(confirmed.len(), 1, "the recurring dip only: {confirmed:#?}");
        assert_eq!(confirmed[0].support, 3);
        assert!(
            (confirmed[0].distance_m - 500.0).abs() < 5.0,
            "cluster distance is the median of members"
        );
    }

    #[test]
    fn two_decisions_inside_one_arc_stay_two() {
        // The Maggotts–Becketts case: every lap brakes twice in one arc.
        let per_lap: Vec<Vec<f32>> = vec![
            vec![500.0, 560.0],
            vec![502.0, 558.0],
            vec![498.0, 562.0],
        ];
        let confirmed = confirm_events(DecisionKind::BrakeOnset, &per_lap);
        assert_eq!(confirmed.len(), 2, "both decisions are real: {confirmed:#?}");
        assert!(confirmed.iter().all(|e| e.support == 3));
    }

    #[test]
    fn a_lap_that_brakes_twice_where_others_brake_once_is_not_two_clusters() {
        // The stray second braking of lap 1 must not drag the confirmed
        // cluster, but it also must not confirm on its own.
        let per_lap: Vec<Vec<f32>> = vec![
            vec![500.0],
            vec![503.0, 580.0],
            vec![498.0],
        ];
        let confirmed = confirm_events(DecisionKind::BrakeOnset, &per_lap);
        assert_eq!(confirmed.len(), 1, "the 580 m event is one lap's, not a corner's");
        assert!((confirmed[0].distance_m - 500.0).abs() < 5.0);
    }
}
