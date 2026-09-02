//! Turn an issue into the sentence the driver hears.
//!
//! The model layer measures numbers. This module turns them into words.
//!
//! # What this layer is
//!
//! A [`Phraser`] is a pure function from `(DrivingIssue, ControllerMode) -> String`.
//! It knows every [`IssueKind`] variant and produces a deterministic sentence
//! for each, with the measured delta formatted into a driver-readable phrase.
//! It does *not* decide *whether* to speak — that's the decision layer's job
//! — and it does *not* decide *how* to speak — that's the audio layer's job.
//!
//! # What this layer is not
//!
//! * Not localisable. The MVP speaks English; adding a second language means
//!   a second phraser, not a flag.
//! * Not a TTS engine. The audio layer maps these strings to WAV files; this
//!   module produces the lookup key the audio layer matches against.
//! * Not opinionated about urgency. Severity was already chosen by the rule;
//!   phrasing does not re-interpret it.
//!
//! # Why controller-aware
//!
//! A few sentences differ by what the driver's hands can actually do:
//!
//! * A wheel driver modulates brake pressure — "ease off the brake" is real
//!   advice. A pad driver can only release.
//! * A wheel driver with paddles can be told to downshift; a pad driver with
//!   buttons cannot.
//! * A wheel driver with an H-pattern needs shift timing distinct from a
//!   paddle driver.
//!
//! The MVP only ships the wheel/pad split because the existing rules don't
//! touch shifting — there is no shift-related issue to phrase yet. The seam
//! exists so the second mode (and any future one) is one arm of a match
//! rather than a rewrite.
//!
//! # Determinism
//!
//! Same issue, same mode, same phraser → same string. The audio layer
//! pre-renders WAV files at build time keyed on these exact strings, so any
//! nondeterminism would mean a phrase that exists in the table is never
//! spoken, or one that is spoken but has no clip.

use crate::models::issue::{DrivingIssue, IssueKind, Severity};

/// What the driver's hands can do. Drives which sentences are even meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerMode {
    /// Steering wheel with a discrete brake pedal the driver can modulate.
    /// Default for the F138 on a proper sim rig.
    Wheel,
    /// Gamepad / hand controller. Brake is a binary button at the physics
    /// level: pedal pressure is approximated, not modulated.
    Pad,
}

impl Default for ControllerMode {
    fn default() -> Self {
        ControllerMode::Wheel
    }
}

/// The thing that turns issues into strings.
///
/// Stateless by construction: a phraser is a `Copy` and can be shared across
/// threads. The MVP ships [`DefaultPhraser`]; a future batch might add a
/// localised one or one keyed on a driver profile.
pub trait Phraser: Copy {
    fn phrase(&self, issue: &DrivingIssue, mode: ControllerMode) -> String;
}

/// The MVP phraser. One sentence per `(IssueKind, ControllerMode)` arm,
/// formatted with the measured delta.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultPhraser;

impl Phraser for DefaultPhraser {
    fn phrase(&self, issue: &DrivingIssue, mode: ControllerMode) -> String {
        match issue.kind {
            IssueKind::BrakedInsideCorner => {
                let magnitude = issue
                    .delta_brake_offset_m
                    .unwrap_or(0.0)
                    .abs();
                format!(
                    "{prefix} the brake — applied {magnitude:.0} metres past the corner",
                    prefix = prefix_for(issue.severity),
                )
            }

            IssueKind::NoThrottlePickup => match mode {
                ControllerMode::Wheel => "stay on the throttle through the corner".into(),
                ControllerMode::Pad => "hold throttle through the corner".into(),
            },

            IssueKind::LateApex => {
                let off = issue.delta_apex_offset_m.unwrap_or(0.0);
                format!(
                    "{prefix} earlier — apex sat {off:.0} metres past the geometric one",
                    prefix = prefix_for(issue.severity),
                )
            }

            IssueKind::EarlyApex => {
                let off = -issue.delta_apex_offset_m.unwrap_or(0.0);
                format!(
                    "{prefix} later — apex sat {off:.0} metres before the geometric one",
                    prefix = prefix_for(issue.severity),
                )
            }

            IssueKind::LateBrakeVsPb => match mode {
                ControllerMode::Wheel => {
                    let delta = issue.delta_brake_offset_m.unwrap_or(0.0);
                    format!(
                        "brake {delta:.0} metres earlier — your best lap braked here"
                    )
                }
                ControllerMode::Pad => {
                    let delta = issue.delta_brake_offset_m.unwrap_or(0.0);
                    format!("brake {delta:.0} metres earlier, on the line")
                }
            },

            IssueKind::SlowApexVsPb => {
                let delta = issue.delta_apex_speed_mps.unwrap_or(0.0);
                let kmh = -delta * 3.6;
                // Delta is negative when driver was slower; we report a
                // positive "you were X slower" figure.
                if kmh >= 1.0 {
                    format!("carry more speed — {kmh:.0} km/h off your apex")
                } else {
                    format!("carry more speed through the apex")
                }
            }

            IssueKind::LateThrottleVsPb => {
                let delta = issue.delta_throttle_pickup_offset_m.unwrap_or(0.0);
                format!("back on throttle {delta:.0} metres earlier — your best lap got on it here")
            }

            IssueKind::LostTimeVsPb => {
                let delta = issue.delta_time_s.unwrap_or(0.0);
                if delta >= 1.0 {
                    format!("you lost {delta:.2} seconds in this corner")
                } else {
                    format!("you lost {delta:.2} of a second here")
                }
            }
        }
    }
}

/// "Brake" vs "Brake harder" — severity-aware opening word. The audio layer
/// uses this to pick a calmer or more urgent clip for the same sentence.
fn prefix_for(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "note",
        Severity::Warn => "brake",
        Severity::Critical => "brake now",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::CornerId;
    use crate::features::corner::CornerDirection;

    fn issue_with(kind: IssueKind, severity: Severity) -> DrivingIssue {
        DrivingIssue::new(CornerId(2), CornerDirection::Right, kind, severity)
    }

    fn issue_with_brake(kind: IssueKind, severity: Severity, delta: f32) -> DrivingIssue {
        issue_with(kind, severity).with_brake_delta(delta)
    }

    fn issue_with_apex(kind: IssueKind, severity: Severity, delta_mps: f32) -> DrivingIssue {
        issue_with(kind, severity).with_apex_delta(delta_mps)
    }

    fn issue_with_time(kind: IssueKind, severity: Severity, delta_s: f32) -> DrivingIssue {
        issue_with(kind, severity).with_time_delta(delta_s)
    }

    fn issue_with_pickup(kind: IssueKind, severity: Severity, delta_m: f32) -> DrivingIssue {
        issue_with(kind, severity).with_throttle_pickup_delta(delta_m)
    }

    fn issue_with_apex_offset(kind: IssueKind, severity: Severity, delta_m: f32) -> DrivingIssue {
        issue_with(kind, severity).with_apex_offset(delta_m)
    }

    #[test]
    fn phrases_are_stable_for_a_given_input() {
        let p = DefaultPhraser;
        let i = issue_with_brake(IssueKind::LateBrakeVsPb, Severity::Warn, 12.0);
        let a = p.phrase(&i, ControllerMode::Wheel);
        let b = p.phrase(&i, ControllerMode::Wheel);
        assert_eq!(a, b);
        assert!(a.contains("12"), "the delta must be in the phrase, got {a:?}");
    }

    #[test]
    fn every_kind_has_a_phrase_in_every_mode() {
        // Locks in the contract that no rule ever silently produces empty
        // output for any combination of (kind, mode). A new mode added later
        // will make this test fail until each kind has a phrase in that mode.
        let p = DefaultPhraser;
        let modes = [ControllerMode::Wheel, ControllerMode::Pad];
        let kinds = [
            IssueKind::BrakedInsideCorner,
            IssueKind::NoThrottlePickup,
            IssueKind::LateApex,
            IssueKind::EarlyApex,
            IssueKind::LateBrakeVsPb,
            IssueKind::SlowApexVsPb,
            IssueKind::LateThrottleVsPb,
            IssueKind::LostTimeVsPb,
        ];
        let mut deltas: std::collections::HashMap<IssueKind, DrivingIssue> =
            std::collections::HashMap::new();
        deltas.insert(
            IssueKind::BrakedInsideCorner,
            issue_with_brake(IssueKind::BrakedInsideCorner, Severity::Warn, 8.0),
        );
        deltas.insert(
            IssueKind::NoThrottlePickup,
            issue_with(IssueKind::NoThrottlePickup, Severity::Warn),
        );
        deltas.insert(
            IssueKind::LateApex,
            issue_with_apex_offset(IssueKind::LateApex, Severity::Warn, 8.0),
        );
        deltas.insert(
            IssueKind::EarlyApex,
            issue_with_apex_offset(IssueKind::EarlyApex, Severity::Warn, -8.0),
        );
        deltas.insert(
            IssueKind::LateBrakeVsPb,
            issue_with_brake(IssueKind::LateBrakeVsPb, Severity::Warn, 6.0),
        );
        deltas.insert(
            IssueKind::SlowApexVsPb,
            issue_with_apex(IssueKind::SlowApexVsPb, Severity::Warn, -2.0),
        );
        deltas.insert(
            IssueKind::LateThrottleVsPb,
            issue_with_pickup(IssueKind::LateThrottleVsPb, Severity::Warn, 8.0),
        );
        deltas.insert(
            IssueKind::LostTimeVsPb,
            issue_with_time(IssueKind::LostTimeVsPb, Severity::Warn, 0.18),
        );

        for kind in kinds {
            let issue = deltas.get(&kind).copied().expect("test fixture missing");
            for mode in modes {
                let phrase = p.phrase(&issue, mode);
                assert!(
                    !phrase.trim().is_empty(),
                    "empty phrase for {kind:?} in {mode:?}"
                );
            }
        }
    }

    #[test]
    fn wheel_and_pad_phrases_differ_for_throttle_and_brake_kinds() {
        let p = DefaultPhraser;
        // Throttle: hold vs stay on
        let throttle = issue_with(IssueKind::NoThrottlePickup, Severity::Warn);
        assert_ne!(
            p.phrase(&throttle, ControllerMode::Wheel),
            p.phrase(&throttle, ControllerMode::Pad),
            "throttle phrasing must differ by mode"
        );

        // Brake: explicit point-of-reference vs "on the line"
        let brake = issue_with_brake(IssueKind::LateBrakeVsPb, Severity::Warn, 6.0);
        assert_ne!(
            p.phrase(&brake, ControllerMode::Wheel),
            p.phrase(&brake, ControllerMode::Pad),
            "late-brake phrasing must differ by mode"
        );
    }

    #[test]
    fn lost_time_phrase_uses_fractional_english_under_one_second() {
        let p = DefaultPhraser;
        let small = issue_with_time(IssueKind::LostTimeVsPb, Severity::Warn, 0.18);
        let s = p.phrase(&small, ControllerMode::Wheel);
        assert!(
            s.contains("0.18") && s.contains("of a second"),
            "small lost-time should use fractional form, got {s:?}"
        );

        let big = issue_with_time(IssueKind::LostTimeVsPb, Severity::Critical, 1.4);
        let s = p.phrase(&big, ControllerMode::Wheel);
        assert!(
            s.contains("1.40") && s.contains("seconds") && !s.contains("of a second"),
            "big lost-time should use plural seconds, got {s:?}"
        );
    }

    #[test]
    fn slow_apex_phrase_falls_back_to_a_short_form_when_delta_is_tiny() {
        let p = DefaultPhraser;
        // Delta < 1 km/h → use the short form, no number.
        let tiny = issue_with_apex(IssueKind::SlowApexVsPb, Severity::Info, -0.1);
        let s = p.phrase(&tiny, ControllerMode::Wheel);
        assert!(s.contains("apex"), "got {s:?}");
        assert!(!s.contains("km/h"), "tiny delta should skip the number, got {s:?}");

        let real = issue_with_apex(IssueKind::SlowApexVsPb, Severity::Warn, -1.5);
        let s = p.phrase(&real, ControllerMode::Wheel);
        assert!(s.contains("km/h"), "got {s:?}");
    }

    #[test]
    fn severity_appears_in_the_brake_inside_corner_phrase() {
        let p = DefaultPhraser;
        let warn = issue_with_brake(IssueKind::BrakedInsideCorner, Severity::Warn, 8.0);
        let crit = issue_with_brake(IssueKind::BrakedInsideCorner, Severity::Critical, 12.0);
        let warn_s = p.phrase(&warn, ControllerMode::Wheel);
        let crit_s = p.phrase(&crit, ControllerMode::Wheel);
        assert!(warn_s.starts_with("brake "));
        assert!(crit_s.starts_with("brake now "));
        assert_ne!(warn_s, crit_s);
    }

    #[test]
    fn apex_phrases_carry_the_measured_offset_in_metres() {
        let p = DefaultPhraser;
        // +8 m past geometric apex → "8 metres past".
        let late = issue_with_apex_offset(IssueKind::LateApex, Severity::Warn, 8.0);
        let late_s = p.phrase(&late, ControllerMode::Wheel);
        assert!(late_s.contains("8"), "got {late_s:?}");
        assert!(late_s.contains("past"), "got {late_s:?}");

        // -8 m before geometric apex → "8 metres before".
        let early = issue_with_apex_offset(IssueKind::EarlyApex, Severity::Warn, -8.0);
        let early_s = p.phrase(&early, ControllerMode::Wheel);
        assert!(early_s.contains("8"), "got {early_s:?}");
        assert!(early_s.contains("before"), "got {early_s:?}");
    }

    #[test]
    fn a_missing_apex_offset_still_produces_a_sensible_phrase() {
        // The rule *should* populate this; the phraser must not panic when
        // an issue arrives without it (a unit test of the phraser against a
        // hand-built issue can plausibly forget).
        let p = DefaultPhraser;
        let late = issue_with(IssueKind::LateApex, Severity::Warn);
        let s = p.phrase(&late, ControllerMode::Wheel);
        assert!(s.contains("0"), "missing delta should fall back to 0 m, got {s:?}");
    }
}