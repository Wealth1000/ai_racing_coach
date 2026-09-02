//! How the coach says what the model concluded.
//!
//! `Advice` is the seam between three layers:
//!
//! * the *model* layer ([`crate::models`]) emits [`DrivingIssue`]s — facts;
//! * the *decision* layer ([`crate::coaching::decision`], Batch 11) chooses
//!   which facts become spoken feedback — that decision;
//! * the *delivery* layer ([`crate::audio`], Batch 12) plays the words — the
//!   act.
//!
//! The model layer should never know how an issue sounds. The decision layer
//! should never know the *words* — only that an issue is worth saying. The
//! delivery layer should never know what an *issue* is — only that it has a
//! pre-phrased sentence, a corner, and a severity.
//!
//! `Advice` carries everything the delivery layer needs to do its job without
//! going back to the model: the corner identity, the severity, the issue
//! category (so the UI can colour-code), the *deltas* the model measured
//! (so a future UI can show a numeric badge alongside the phrase), and the
//! fully-resolved [`phrased`](Self::phrased) sentence. That last field is
//! the seam: it is the one thing the model layer is forbidden from
//! constructing.
//!
//! # Why the phrased string lives here, not in audio
//!
//! Batch 12's audio sink picks WAV clips by the `phrased` field. If phrasing
//! were redone in audio, two layers would have to agree on the same lookup
//! table, and the table would have to be reachable from both. Centralising
//! it on `Advice` means one place to edit, one test surface, and one
//! canonical string the UI can also display.
//!
//! The cost is a fixed string on every `Advice`. It is the right trade:
//! audio wants it, UI wants it, and the strings are short.

use serde::{Deserialize, Serialize};

use crate::core::ids::CornerId;
use crate::features::corner::CornerDirection;
use crate::models::issue::{DrivingIssue, IssueKind, Severity};

/// What gets handed to the delivery layer.
///
/// Cheap to clone (one heap string, a `Copy` corner id, and a small struct of
/// primitives) so the audio thread, UI thread, and storage thread can each
/// take their own copy without locking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Advice {
    pub corner_id: CornerId,
    pub direction: CornerDirection,
    pub kind: IssueKind,
    pub severity: Severity,

    /// The fully-resolved sentence the driver will hear or read.
    ///
    /// Empty is not a value this type carries: if there's nothing to say, no
    /// `Advice` should exist. The decision layer is responsible for not
    /// producing one.
    pub phrased: String,

    /// Numeric support the model measured. Carried forward unchanged so the
    /// UI can show them without re-running the rule.
    pub delta_brake_offset_m: Option<f32>,
    pub delta_apex_speed_mps: Option<f32>,
    pub delta_apex_offset_m: Option<f32>,
    pub delta_throttle_pickup_offset_m: Option<f32>,
    pub delta_time_s: Option<f32>,
}

impl Advice {
    /// Build an [`Advice`] from a [`DrivingIssue`] and a phraser. The phraser
    /// owns the rule→string mapping; `Advice` is just the carrier.
    ///
    /// The corner identity, severity, and deltas are copied from the issue
    /// unchanged. `phrased` is whatever [`Phraser::phrase`] returned.
    pub fn from_issue(
        issue: &DrivingIssue,
        phrased: String,
    ) -> Self {
        Self {
            corner_id: issue.corner_id,
            direction: issue.direction,
            kind: issue.kind,
            severity: issue.severity,
            phrased,
            delta_brake_offset_m: issue.delta_brake_offset_m,
            delta_apex_speed_mps: issue.delta_apex_speed_mps,
            delta_apex_offset_m: issue.delta_apex_offset_m,
            delta_throttle_pickup_offset_m: issue.delta_throttle_pickup_offset_m,
            delta_time_s: issue.delta_time_s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(kind: IssueKind, severity: Severity) -> DrivingIssue {
        DrivingIssue::new(CornerId(3), CornerDirection::Right, kind, severity)
            .with_brake_delta(7.0)
            .with_apex_delta(-0.5)
            .with_time_delta(0.12)
    }

    #[test]
    fn from_issue_copies_identity_severity_and_deltas_unchanged() {
        let i = issue(IssueKind::LateBrakeVsPb, Severity::Warn);
        let a = Advice::from_issue(&i, "brake 7m earlier".into());

        assert_eq!(a.corner_id, i.corner_id);
        assert_eq!(a.direction, i.direction);
        assert_eq!(a.kind, i.kind);
        assert_eq!(a.severity, i.severity);
        assert_eq!(a.delta_brake_offset_m, Some(7.0));
        assert_eq!(a.delta_apex_speed_mps, Some(-0.5));
        assert_eq!(a.delta_apex_offset_m, None);
        assert_eq!(a.delta_throttle_pickup_offset_m, None);
        assert_eq!(a.delta_time_s, Some(0.12));
    }

    #[test]
    fn from_issue_keeps_the_phrased_string_exactly() {
        let i = issue(IssueKind::NoThrottlePickup, Severity::Warn);
        let a = Advice::from_issue(&i, "  no throttle  ".into());
        // No trimming, no transformation: the audio layer picks the WAV from
        // this exact string and the UI displays it as-is. If you want
        // normalisation, do it in the phraser, not here.
        assert_eq!(a.phrased, "  no throttle  ");
    }

    #[test]
    fn advice_is_cloneable_for_per_consumer_copies() {
        let i = issue(IssueKind::LostTimeVsPb, Severity::Critical);
        let a = Advice::from_issue(&i, "0.4 seconds slower".into());
        let b = a.clone();
        assert_eq!(a, b);
    }
}