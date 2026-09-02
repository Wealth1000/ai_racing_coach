//! The rule-based [`DrivingModel`] for the MVP.
//!
//! # Two tiers, one model
//!
//! *Cheap* rules fire on the pass alone — no reference needed. They are the
//! rules that work from the moment a clean lap exists: was the brake pedal
//! applied inside the corner? Did the throttle ever come back? Was the apex
//! in the right place? They give the driver useful feedback on lap one.
//!
//! *Comparison* rules fire only when a personal best exists. They are the
//! rules that say "compared with your best": braked later, slower at the apex,
//! later on the throttle, lost time. They tell the driver not just *that*
//! something was off but *by how much*.
//!
//! Both tiers live in one type because they share knobs: the `late_brake_m`
//! threshold means the same thing whether the rule is reporting an absolute
//! miss or a delta to the PB. Splitting them would duplicate state.
//!
//! # The threshold problem
//!
//! Every rule below has at least one threshold and every threshold is a
//! tuning knob. The values here were chosen against the F138 captures already
//! in the repo — they should not fire spuriously on a clean lap of those
//! circuits — but a real driver will want them adjusted. The struct is public
//! and every threshold is `pub` precisely so this is one place to edit.
//!
//! What the thresholds are *not* is an excuse for lazy rules. A rule that
//! fires on every lap is a rule that has stopped teaching. If a threshold
//! needs to be very large to avoid noise on a clean lap, the underlying
//! measurement probably wants replacing — but that is a future-batch problem,
//! not one this batch should smuggle in.
//!
//! # Determinism
//!
//! `predict` is a pure function of its inputs and the rule's thresholds. Same
//! features, same reference, same thresholds → same issues in the same order.
//! That is what makes the audio layer's cooldown logic sane: nothing in the
//! pipeline can produce a surprise.

use crate::features::corner_features::CornerFeatures;
use crate::features::reference::CornerReference;
use crate::models::issue::{DrivingIssue, IssueKind, Severity};
use crate::models::DrivingModel;

/// All thresholds the rule model uses. Every field is a **tuning knob**.
#[derive(Debug, Clone, Copy)]
pub struct RuleThresholds {
    // ---- Cheap tier ----

    /// Apex position is considered "late" once it sits at least this far past
    /// the geometric apex, metres. Beyond this, the driver is still slowing
    /// when the corner is already turning back — that costs exit speed.
    pub late_apex_m: f32,
    /// Mirror of [`Self::late_apex_m`] for the early side.
    pub early_apex_m: f32,
    /// Any throttle pickup past this offset from the apex is treated as "did
    /// not pick up inside the search window". Matches the default
    /// `throttle_search_m` in `FeatureParams`; surfaced as a knob so a more
    /// conservative coach can shorten the window and report more often.
    pub no_throttle_window_m: f32,

    // ---- Comparison tier ----

    /// Minimum time gap to the PB before the lost-time rule fires, seconds.
    /// Below this the rule is silent because the timing noise of a single
    /// lap is bigger than the gap being reported.
    pub lost_time_s: f32,
    /// Minimum brake-offset delta to the PB before the late-brake rule fires,
    /// metres. Positive direction = driver braked later than the PB.
    pub late_brake_m: f32,
    /// Minimum apex-speed deficit vs the PB before the slow-apex rule fires,
    /// m/s. The rule fires when the driver was *slower* than the PB; positive
    /// delta (driver faster) is silently ignored.
    pub slow_apex_mps: f32,
    /// Minimum throttle-pickup delta vs the PB before the late-throttle rule
    /// fires, metres.
    pub late_throttle_m: f32,
}

impl Default for RuleThresholds {
    fn default() -> Self {
        Self {
            late_apex_m: 5.0,
            early_apex_m: 5.0,
            no_throttle_window_m: 30.0,
            lost_time_s: 0.10,
            late_brake_m: 5.0,
            slow_apex_mps: 0.5,
            late_throttle_m: 5.0,
        }
    }
}

/// The rule-based model. Holds thresholds; nothing else.
#[derive(Debug, Clone, Copy)]
pub struct RuleModel {
    pub thresholds: RuleThresholds,
}

impl Default for RuleModel {
    fn default() -> Self {
        Self {
            thresholds: RuleThresholds::default(),
        }
    }
}

impl RuleModel {
    pub fn with_thresholds(thresholds: RuleThresholds) -> Self {
        Self { thresholds }
    }
}

impl DrivingModel for RuleModel {
    fn name(&self) -> &'static str {
        "rule"
    }

    fn predict(
        &self,
        f: &CornerFeatures,
        reference: Option<&CornerReference>,
    ) -> Vec<DrivingIssue> {
        let mut out = Vec::new();
        cheap_tier(&mut out, f, &self.thresholds);
        if let Some(pb) = reference {
            comparison_tier(&mut out, f, pb, &self.thresholds);
        }
        // The two tiers never produce the same kind; sorting by kind is a
        // stable, content-addressable order that matches what the audio
        // layer would naturally say first.
        out.sort_by_key(|i| i.kind as u8);
        out
    }
}

/// Rules that fire from the pass alone. Each is one observation the driver
/// can act on without needing to know what their PB looks like.
fn cheap_tier(out: &mut Vec<DrivingIssue>, f: &CornerFeatures, t: &RuleThresholds) {
    // Braked inside the corner: braking point sat past the canonical entry.
    // `braking_length_m` is signed metres back from `start_m`; negative means
    // the brake was applied *after* entering the corner.
    if let Some(blen) = f.braking_length_m {
        if blen < 0.0 {
            let magnitude = -blen;
            out.push(
                DrivingIssue::new(
                    f.corner_id,
                    f.direction,
                    IssueKind::BrakedInsideCorner,
                    severity_for(magnitude, t.late_brake_m),
                )
                .with_brake_delta(magnitude),
            );
        }
    }

    // No throttle pickup inside the search window. `None` means full power
    // never returned; a value past the window is functionally the same — the
    // pickup belongs to the following straight, not to how this corner was
    // driven.
    let pickup_too_late = match f.throttle_pickup_offset_m {
        None => true,
        Some(off) => off > t.no_throttle_window_m,
    };
    if pickup_too_late {
        out.push(DrivingIssue::new(
            f.corner_id,
            f.direction,
            IssueKind::NoThrottlePickup,
            Severity::Warn,
        ));
    }

    // Apex position. Negative offset = slowed early; positive = slowed late.
    if f.speed_min_offset_m > t.late_apex_m {
        let offset = f.speed_min_offset_m;
        out.push(
            DrivingIssue::new(
                f.corner_id,
                f.direction,
                IssueKind::LateApex,
                severity_for(offset, t.late_apex_m),
            )
            .with_apex_offset(offset),
        );
    } else if f.speed_min_offset_m < -t.early_apex_m {
        let offset = f.speed_min_offset_m;
        out.push(
            DrivingIssue::new(
                f.corner_id,
                f.direction,
                IssueKind::EarlyApex,
                severity_for(-offset, t.early_apex_m),
            )
            .with_apex_offset(offset),
        );
    }
}

/// Rules that fire only when a personal best exists. Each compares one
/// quantity against the PB's same quantity; deltas are reported in the sign
/// the driver would intuit ("you were later / slower").
fn comparison_tier(
    out: &mut Vec<DrivingIssue>,
    f: &CornerFeatures,
    pb: &CornerReference,
    t: &RuleThresholds,
) {
    // Lost time is the headline. A pass that was slower across the span gets
    // this regardless of which input was wrong — the rule layer reports both
    // the headline and the specific cause.
    let time_delta = f.time_in_corner_s - pb.time_in_corner_s;
    if time_delta > t.lost_time_s {
        out.push(
            DrivingIssue::new(
                f.corner_id,
                f.direction,
                IssueKind::LostTimeVsPb,
                severity_for(time_delta, t.lost_time_s),
            )
            .with_time_delta(time_delta),
        );
    }

    // Late brake vs PB. Both sides must have a brake point for this to be
    // meaningful; a PB that rolled through the corner is not a brake-point
    // target. Sign convention: `CornerReference::brake_offset_m` is a signed
    // offset from the boundary (negative = before), so the comparison
    // happens in that space. `CornerFeatures` stores the same physical
    // quantity as `braking_length_m` (signed metres back from `start_m`),
    // with the opposite sign — flip it.
    if let (Some(blen), Some(pb_off)) = (f.braking_length_m, pb.brake_offset_m) {
        let mine_off = -blen;
        let delta = mine_off - pb_off;
        if delta > t.late_brake_m {
            out.push(
                DrivingIssue::new(
                    f.corner_id,
                    f.direction,
                    IssueKind::LateBrakeVsPb,
                    severity_for(delta, t.late_brake_m),
                )
                .with_brake_delta(delta),
            );
        }
    }

    // Slow apex vs PB. Only fires when the driver was *slower* — being faster
    // than your PB is not a problem to report.
    let apex_delta = f.apex_speed_mps - pb.apex_speed_mps;
    if apex_delta < -t.slow_apex_mps {
        let magnitude = -apex_delta;
        out.push(
            DrivingIssue::new(
                f.corner_id,
                f.direction,
                IssueKind::SlowApexVsPb,
                severity_for(magnitude, t.slow_apex_mps),
            )
            .with_apex_delta(apex_delta),
        );
    }

    // Late throttle pickup vs PB. Mirror of the brake rule: both sides must
    // have a pickup, and the comparison is `mine later than PB`.
    if let (Some(mine_off), Some(pb_off)) = (
        f.throttle_pickup_offset_m,
        pb.throttle_pickup_offset_m,
    ) {
        let delta = mine_off - pb_off;
        if delta > t.late_throttle_m {
            out.push(
                DrivingIssue::new(
                    f.corner_id,
                    f.direction,
                    IssueKind::LateThrottleVsPb,
                    severity_for(delta, t.late_throttle_m),
                )
                .with_throttle_pickup_delta(delta),
            );
        }
    }
}

/// Map a magnitude against its threshold to a [`Severity`].
///
/// One threshold past the bar: `Warn`. Twice past: `Critical`. Below: `Info`
/// is *not* used — silence is the right answer for magnitudes below the bar,
/// and the rules already enforce that. The Info arm exists so the function is
/// total and so a future rule that wants an "always-say" observation has a
/// home.
fn severity_for(magnitude: f32, threshold: f32) -> Severity {
    if threshold <= 0.0 {
        // Defensive: a misconfigured threshold must not silence the rule.
        return Severity::Warn;
    }
    let ratio = magnitude / threshold;
    if ratio >= 2.0 {
        Severity::Critical
    } else if ratio >= 1.0 {
        Severity::Warn
    } else {
        Severity::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::{CornerId, LapId};
    use crate::features::CornerDirection;

    const RIGHT: CornerDirection = CornerDirection::Right;

    /// Helper for tests: build a `CornerFeatures` with plausible defaults and
    /// the named fields overridden. Keeps the test bodies short.
    fn features(
        corner: u32,
        direction: CornerDirection,
        braking_length_m: Option<f32>,
        throttle_pickup: Option<f32>,
        speed_min_offset_m: f32,
        apex_speed_mps: f32,
        time_s: f32,
    ) -> CornerFeatures {
        CornerFeatures {
            lap_id: LapId(0),
            corner_id: CornerId(corner),
            direction,
            entry_speed_mps: 40.0,
            apex_speed_mps,
            exit_speed_mps: 45.0,
            speed_min_offset_m,
            brake_start_m: braking_length_m.map(|blen| 250.0 + blen),
            braking_length_m,
            peak_brake: 0.8,
            trail_braking: false,
            throttle_pickup_offset_m: throttle_pickup,
            min_throttle_in_corner: 0.2,
            time_in_corner_s: time_s,
            peak_abs_slip_rad: 0.05,
            off_track_points: 0,
        }
    }

    /// Helper for tests: build a `CornerReference` with the named fields.
    /// `brake_offset_m` here follows the same signed convention
    /// `CornerReference` uses (negative = before the boundary).
    fn reference(
        corner: u32,
        direction: CornerDirection,
        brake_offset_m: Option<f32>,
        throttle_pickup: Option<f32>,
        apex_speed_mps: f32,
        time_s: f32,
    ) -> CornerReference {
        CornerReference {
            corner_id: CornerId(corner),
            direction,
            source_lap: LapId(0),
            entry_speed_mps: 40.0,
            apex_speed_mps,
            exit_speed_mps: 45.0,
            time_in_corner_s: time_s,
            brake_offset_m,
            throttle_pickup_offset_m: throttle_pickup,
            trail_braking: false,
        }
    }

    #[test]
    fn cheap_tier_reports_braking_inside_the_corner() {
        let m = RuleModel::default();
        // braking_length_m = -10 → braked 10 m past the boundary. 10 / 5 = 2×
        // the threshold, so severity is Critical.
        let f = features(0, RIGHT, Some(-10.0), Some(5.0), 0.0, 30.0, 4.0);
        let issues = m.predict(&f, None);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, IssueKind::BrakedInsideCorner);
        assert_eq!(issues[0].delta_brake_offset_m, Some(10.0));
        assert_eq!(issues[0].severity, Severity::Critical);
    }

    #[test]
    fn cheap_tier_reports_late_apex() {
        let m = RuleModel::default();
        let f = features(0, RIGHT, Some(50.0), Some(5.0), 7.0, 30.0, 4.0);
        let issues = m.predict(&f, None);
        let late = issues
            .iter()
            .find(|i| i.kind == IssueKind::LateApex)
            .expect("late-apex issue");
        assert_eq!(late.delta_apex_offset_m, Some(7.0));
    }

    #[test]
    fn cheap_tier_reports_early_apex() {
        let m = RuleModel::default();
        let f = features(0, RIGHT, Some(50.0), Some(5.0), -7.0, 30.0, 4.0);
        let issues = m.predict(&f, None);
        let early = issues
            .iter()
            .find(|i| i.kind == IssueKind::EarlyApex)
            .expect("early-apex issue");
        assert_eq!(early.delta_apex_offset_m, Some(-7.0));
    }

    #[test]
    fn cheap_tier_reports_no_throttle_pickup_when_none() {
        let m = RuleModel::default();
        let f = features(0, RIGHT, Some(50.0), None, 0.0, 30.0, 4.0);
        let issues = m.predict(&f, None);
        assert!(
            issues.iter().any(|i| i.kind == IssueKind::NoThrottlePickup),
            "got {issues:?}"
        );
    }

    #[test]
    fn cheap_tier_reports_no_throttle_pickup_when_past_window() {
        let m = RuleModel::default();
        // Window default is 30 m; 50 m is past it.
        let f = features(0, RIGHT, Some(50.0), Some(50.0), 0.0, 30.0, 4.0);
        let issues = m.predict(&f, None);
        assert!(
            issues.iter().any(|i| i.kind == IssueKind::NoThrottlePickup),
            "got {issues:?}"
        );
    }

    #[test]
    fn cheap_tier_stays_silent_on_a_clean_pass() {
        let m = RuleModel::default();
        let f = features(0, RIGHT, Some(40.0), Some(15.0), 1.0, 30.0, 4.0);
        assert!(m.predict(&f, None).is_empty());
    }

    #[test]
    fn comparison_tier_is_silent_without_a_reference() {
        let m = RuleModel::default();
        // A clearly suboptimal pass against an absent PB: no PB → no
        // comparison issues. Cheap tier is also silent because the cheap
        // thresholds are not exceeded.
        let f = features(0, RIGHT, Some(40.0), Some(15.0), 1.0, 30.0, 4.0);
        assert!(m.predict(&f, None).is_empty());
    }

    #[test]
    fn comparison_tier_reports_lost_time_above_threshold() {
        let m = RuleModel::default();
        let pb = reference(0, RIGHT, Some(-40.0), Some(15.0), 30.0, 4.00);
        // Half a second over the PB — well above the 0.10 s default.
        let f = features(0, RIGHT, Some(40.0), Some(15.0), 1.0, 30.0, 4.50);
        let issues = m.predict(&f, Some(&pb));
        let lost = issues
            .iter()
            .find(|i| i.kind == IssueKind::LostTimeVsPb)
            .expect("lost-time issue");
        assert_eq!(lost.delta_time_s, Some(0.50));
        // 0.50 / 0.10 = 5.0, well past 2.0 → critical.
        assert_eq!(lost.severity, Severity::Critical);
    }

    #[test]
    fn comparison_tier_reports_late_brake_vs_pb() {
        let m = RuleModel::default();
        let pb = reference(0, RIGHT, Some(-50.0), Some(15.0), 30.0, 4.00);
        // braked_length_m = 30 → brake_offset_m = -30 → 20 m later than PB.
        let f = features(0, RIGHT, Some(30.0), Some(15.0), 1.0, 30.0, 4.10);
        let issues = m.predict(&f, Some(&pb));
        let late = issues
            .iter()
            .find(|i| i.kind == IssueKind::LateBrakeVsPb)
            .expect("late-brake issue");
        assert_eq!(late.delta_brake_offset_m, Some(20.0));
    }

    #[test]
    fn comparison_tier_reports_slow_apex_when_slower_than_pb() {
        let m = RuleModel::default();
        let pb = reference(0, RIGHT, Some(-40.0), Some(15.0), 35.0, 4.00);
        let f = features(0, RIGHT, Some(40.0), Some(15.0), 1.0, 30.0, 4.20);
        let issues = m.predict(&f, Some(&pb));
        let slow = issues
            .iter()
            .find(|i| i.kind == IssueKind::SlowApexVsPb)
            .expect("slow-apex issue");
        // 30 - 35 = -5 m/s; magnitude 5 m/s is past the 0.5 default and over
        // 2×, so critical.
        assert_eq!(slow.delta_apex_speed_mps, Some(-5.0));
        assert_eq!(slow.severity, Severity::Critical);
    }

    #[test]
    fn comparison_tier_stays_silent_when_faster_than_pb() {
        let m = RuleModel::default();
        let pb = reference(0, RIGHT, Some(-40.0), Some(15.0), 30.0, 4.00);
        // Driver apex 35 m/s, PB apex 30 m/s: faster than PB. No slow-apex
        // issue, no lost-time issue, no late-brake.
        let f = features(0, RIGHT, Some(40.0), Some(15.0), 1.0, 35.0, 3.80);
        let issues = m.predict(&f, Some(&pb));
        assert!(
            !issues.iter().any(|i| i.kind == IssueKind::SlowApexVsPb),
            "being faster is not an issue: got {issues:?}"
        );
        assert!(
            !issues.iter().any(|i| i.kind == IssueKind::LostTimeVsPb),
            "being faster is not lost time: got {issues:?}"
        );
    }

    #[test]
    fn comparison_tier_reports_late_throttle_pickup_vs_pb() {
        let m = RuleModel::default();
        let pb = reference(0, RIGHT, Some(-40.0), Some(10.0), 30.0, 4.00);
        let f = features(0, RIGHT, Some(40.0), Some(20.0), 1.0, 30.0, 4.20);
        let issues = m.predict(&f, Some(&pb));
        let late = issues
            .iter()
            .find(|i| i.kind == IssueKind::LateThrottleVsPb)
            .expect("late-throttle issue");
        assert_eq!(late.delta_throttle_pickup_offset_m, Some(10.0));
    }

    #[test]
    fn comparison_tier_skips_brake_comparison_when_either_side_lacks_a_point() {
        let m = RuleModel::default();
        // PB rolled through the corner without braking.
        let pb = reference(0, RIGHT, None, Some(15.0), 30.0, 4.00);
        // Driver braked; lost time, but the late-brake rule has nothing to
        // compare against.
        let f = features(0, RIGHT, Some(20.0), Some(15.0), 1.0, 30.0, 4.20);
        let issues = m.predict(&f, Some(&pb));
        assert!(
            !issues.iter().any(|i| i.kind == IssueKind::LateBrakeVsPb),
            "got {issues:?}"
        );
        // Lost-time rule still fires.
        assert!(
            issues.iter().any(|i| i.kind == IssueKind::LostTimeVsPb),
            "got {issues:?}"
        );
    }

    #[test]
    fn both_tiers_can_fire_on_the_same_pass() {
        let m = RuleModel::default();
        // Late apex *and* braked inside the corner *and* lost time vs PB.
        let pb = reference(0, RIGHT, Some(-50.0), Some(10.0), 32.0, 4.00);
        let f = features(0, RIGHT, Some(-5.0), Some(50.0), 8.0, 28.0, 4.50);
        let issues = m.predict(&f, Some(&pb));
        let kinds: Vec<IssueKind> = issues.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&IssueKind::BrakedInsideCorner));
        assert!(kinds.contains(&IssueKind::LateApex));
        assert!(kinds.contains(&IssueKind::NoThrottlePickup));
        assert!(kinds.contains(&IssueKind::LostTimeVsPb));
        assert!(kinds.contains(&IssueKind::SlowApexVsPb));
    }

    #[test]
    fn severity_scales_with_magnitude_past_threshold() {
        // 1× threshold → Warn; 2× → Critical; below → Info.
        assert_eq!(severity_for(0.5, 1.0), Severity::Info);
        assert_eq!(severity_for(1.0, 1.0), Severity::Warn);
        assert_eq!(severity_for(2.0, 1.0), Severity::Critical);
        assert_eq!(severity_for(0.0, 0.0), Severity::Warn, "zero threshold is misconfigured");
        assert_eq!(severity_for(1.0, -1.0), Severity::Warn, "negative threshold is misconfigured");
    }

    #[test]
    fn predict_is_deterministic_for_identical_inputs() {
        let m = RuleModel::default();
        let pb = reference(0, RIGHT, Some(-40.0), Some(15.0), 30.0, 4.00);
        let f = features(0, RIGHT, Some(-3.0), Some(15.0), 7.0, 29.0, 4.30);
        let a = m.predict(&f, Some(&pb));
        let b = m.predict(&f, Some(&pb));
        assert_eq!(a, b);
    }
}