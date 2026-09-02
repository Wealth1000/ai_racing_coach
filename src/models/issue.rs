//! What went wrong on one pass through one corner.
//!
//! The model layer's job is to turn `(CornerFeatures, Option<CornerReference>)`
//! into zero or more of these. The coaching layer then picks phrasing; the
//! decision layer picks whether to say it. Neither needs to know *why* an issue
//! was raised, only its kind and its numbers — which is why this type carries
//! the deltas that triggered it, not a sentence.
//!
//! # Severity is a priority, not a verdict
//!
//! Severity tells the throttler how *much* the issue matters, not whether the
//! pass was wrong. A driver who lost 0.3 s in one corner and 0.05 s in another
//! gets the same `Kind` for both; the larger one deserves the airtime. The
//! opposite would be false too: a 0.05 s "late apex" is below the bar of
//! usefulness regardless of whether 0.3 s is the headline.
//!
//! `Severity::Info` is for observations the driver asked for; for now nothing
//! emits it, and that is on purpose — silence is the default.
//!
//! # What is deliberately absent
//!
//! No `String` message. Phrasing lives in [`coaching::phrasing`](crate::coaching::phrasing)
//! (Batch 10) and the rule layer is the only thing that knows what *kinds* of
//! numbers go with what *kinds* of issue. Letting either side build sentences
//! directly couples them, which the trait seam exists to avoid.
//!
//! No timestamps. Issues are reported at corner boundaries; if a future model
//! needs to say "you braked 0.4 s before the PB", that's a new field here, not a
//! string change.

use serde::{Deserialize, Serialize};

use crate::core::ids::CornerId;
use crate::features::corner::CornerDirection;

/// How loud an issue deserves to be.
///
/// Ordered, so the throttler can compare directly and so the audio layer can
/// map each band onto a calm-to-urgent phrase without re-checking.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Severity {
    /// "By the way" — only fires when the driver opted in.
    Info,
    /// Worth saying. The driver should hear this once.
    Warn,
    /// Worth saying twice. The driver should hear this and remember it.
    Critical,
}

/// What category of mistake this is.
///
/// The coaching layer uses this to pick phrasing; the rule layer uses it to
/// decide what to compare against. Adding a new rule is a new variant here and
/// a new arm in the phrasing switch — that's the whole contract.
///
/// Variants are ordered roughly from inputs (when you braked) to outcomes (how
/// much time that cost), so the audio layer can phrase a list in that order if
/// it ever needs to enumerate more than one issue at a corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IssueKind {
    /// The braking point sat *inside* the corner, past the canonical boundary.
    /// A cheap-tier signal: no reference needed.
    BrakedInsideCorner,
    /// The throttle never reached full inside the pickup search window.
    /// Cheap-tier.
    NoThrottlePickup,
    /// The physical apex sat well past the geometric apex — the driver was
    /// still slowing when the corner was already turning back. Cheap-tier.
    LateApex,
    /// The physical apex sat well before the geometric apex — the driver
    /// turned in too early and gave the exit away. Cheap-tier.
    EarlyApex,
    /// Braking point was meaningfully later than the personal best. Reference-tier.
    LateBrakeVsPb,
    /// Apex speed was meaningfully lower than the personal best. Reference-tier.
    SlowApexVsPb,
    /// Throttle pickup was meaningfully later than the personal best. Reference-tier.
    LateThrottleVsPb,
    /// Time over the span was meaningfully longer than the personal best.
    /// Reference-tier, and the catch-all: any other rule will also surface a
    /// lost-time issue if the gap is large enough.
    LostTimeVsPb,
}

impl IssueKind {
    /// Short stable name for logs and tests. Lower-case, snake-style; phrases
    /// are matched on this so it must not change once a clip exists.
    pub fn as_str(self) -> &'static str {
        match self {
            IssueKind::BrakedInsideCorner => "braked_inside_corner",
            IssueKind::NoThrottlePickup => "no_throttle_pickup",
            IssueKind::LateApex => "late_apex",
            IssueKind::EarlyApex => "early_apex",
            IssueKind::LateBrakeVsPb => "late_brake_vs_pb",
            IssueKind::SlowApexVsPb => "slow_apex_vs_pb",
            IssueKind::LateThrottleVsPb => "late_throttle_vs_pb",
            IssueKind::LostTimeVsPb => "lost_time_vs_pb",
        }
    }

    /// Whether this kind of issue requires a personal best to be meaningful.
    /// Rules that fire from structure alone (cheap tier) return `false`.
    pub fn needs_reference(self) -> bool {
        matches!(
            self,
            IssueKind::LateBrakeVsPb
                | IssueKind::SlowApexVsPb
                | IssueKind::LateThrottleVsPb
                | IssueKind::LostTimeVsPb
        )
    }
}

impl core::fmt::Display for IssueKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One observation about one pass through one corner.
///
/// `delta_*` are signed so the rule that produced them is reconstructable from
/// the struct alone: positive `delta_brake_offset_m` means "you braked later
/// than the reference", positive `delta_apex_speed_mps` means "you were faster
/// than the reference". All deltas are in canonical units; sign conventions
/// are owned by [`super::rules`].
///
/// `None` on a delta means the rule that raised this issue did not measure
/// that quantity. The set of populated deltas depends on `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DrivingIssue {
    pub corner_id: CornerId,
    pub direction: CornerDirection,
    pub kind: IssueKind,
    pub severity: Severity,

    /// Signed metres. Positive = driver braked later than the reference, or
    /// for the cheap `BrakedInsideCorner` rule, the magnitude by which the
    /// braking point overshot the corner boundary.
    pub delta_brake_offset_m: Option<f32>,
    /// Signed m/s. Positive = driver was faster than the reference at the apex.
    pub delta_apex_speed_mps: Option<f32>,
    /// Signed metres past the geometric apex where the *physical* apex sat.
    /// Positive = late apex, negative = early apex. Populated by the cheap
    /// [`IssueKind::LateApex`] / [`IssueKind::EarlyApex`] rules.
    pub delta_apex_offset_m: Option<f32>,
    /// Signed metres. Positive = driver picked up the throttle later than the
    /// reference, relative to the geometric apex.
    pub delta_throttle_pickup_offset_m: Option<f32>,
    /// Signed seconds. Positive = the pass took longer than the reference.
    /// Only populated for [`IssueKind::LostTimeVsPb`].
    pub delta_time_s: Option<f32>,
}

impl DrivingIssue {
    /// The smallest constructor: kind + severity + corner. Deltas default to
    /// `None`, which is right for cheap-tier rules.
    pub fn new(
        corner_id: CornerId,
        direction: CornerDirection,
        kind: IssueKind,
        severity: Severity,
    ) -> Self {
        Self {
            corner_id,
            direction,
            kind,
            severity,
            delta_brake_offset_m: None,
            delta_apex_speed_mps: None,
            delta_apex_offset_m: None,
            delta_throttle_pickup_offset_m: None,
            delta_time_s: None,
        }
    }

    /// Convenience: build with the brake delta already filled in.
    pub fn with_brake_delta(mut self, delta_m: f32) -> Self {
        self.delta_brake_offset_m = Some(delta_m);
        self
    }

    /// Convenience: build with the apex-speed delta already filled in.
    pub fn with_apex_delta(mut self, delta_mps: f32) -> Self {
        self.delta_apex_speed_mps = Some(delta_mps);
        self
    }

    /// Convenience: build with the apex-position delta already filled in.
    pub fn with_apex_offset(mut self, delta_m: f32) -> Self {
        self.delta_apex_offset_m = Some(delta_m);
        self
    }

    /// Convenience: build with the throttle-pickup delta already filled in.
    pub fn with_throttle_pickup_delta(mut self, delta_m: f32) -> Self {
        self.delta_throttle_pickup_offset_m = Some(delta_m);
        self
    }

    /// Convenience: build with the time delta already filled in.
    pub fn with_time_delta(mut self, delta_s: f32) -> Self {
        self.delta_time_s = Some(delta_s);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_info_below_warn_below_critical() {
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Critical);
    }

    #[test]
    fn issue_kind_names_are_stable_strings() {
        assert_eq!(IssueKind::LateBrakeVsPb.as_str(), "late_brake_vs_pb");
        assert_eq!(IssueKind::BrakedInsideCorner.as_str(), "braked_inside_corner");
        // The Display impl must agree with as_str: anything else would mean
        // logs and rule output disagree.
        assert_eq!(format!("{}", IssueKind::LostTimeVsPb), "lost_time_vs_pb");
    }

    #[test]
    fn needs_reference_partitions_kinds_cleanly() {
        let cheap = [
            IssueKind::BrakedInsideCorner,
            IssueKind::NoThrottlePickup,
            IssueKind::LateApex,
            IssueKind::EarlyApex,
        ];
        let pb = [
            IssueKind::LateBrakeVsPb,
            IssueKind::SlowApexVsPb,
            IssueKind::LateThrottleVsPb,
            IssueKind::LostTimeVsPb,
        ];

        for k in cheap {
            assert!(!k.needs_reference(), "cheap kind {k} must not need a PB");
        }
        for k in pb {
            assert!(k.needs_reference(), "PB kind {k} must need a PB");
        }
    }

    #[test]
    fn new_issue_has_no_deltas_filled_in() {
        let issue = DrivingIssue::new(
            CornerId(3),
            CornerDirection::Right,
            IssueKind::NoThrottlePickup,
            Severity::Warn,
        );
        assert_eq!(issue.delta_brake_offset_m, None);
        assert_eq!(issue.delta_apex_speed_mps, None);
        assert_eq!(issue.delta_apex_offset_m, None);
        assert_eq!(issue.delta_throttle_pickup_offset_m, None);
        assert_eq!(issue.delta_time_s, None);
        assert_eq!(issue.corner_id, CornerId(3));
    }

    #[test]
    fn delta_builders_fill_their_own_field_only() {
        let issue = DrivingIssue::new(
            CornerId(0),
            CornerDirection::Left,
            IssueKind::LateBrakeVsPb,
            Severity::Critical,
        )
        .with_brake_delta(7.5)
        .with_apex_delta(-0.6)
        .with_time_delta(0.42);

        assert_eq!(issue.delta_brake_offset_m, Some(7.5));
        assert_eq!(issue.delta_apex_speed_mps, Some(-0.6));
        assert_eq!(issue.delta_time_s, Some(0.42));
        assert_eq!(
            issue.delta_throttle_pickup_offset_m, None,
            "throttle builder not called: stays None"
        );
    }
}