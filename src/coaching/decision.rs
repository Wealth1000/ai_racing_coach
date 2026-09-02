//! The don't-disturb-the-driver layer.
//!
//! The model layer measures facts. The phraser turns them into sentences.
//! The audio sink plays the sentences. Between the phraser and the audio
//! sink sits this module, and its job is the only one in the crate that is
//! not about facts: it is about *judgement* — which of the many things the
//! model layer could say actually deserve airtime.
//!
//! # Why this layer exists at all
//!
//! Without a decision layer, every issue from every corner fires the moment
//! it is raised. A driver coming out of a messy chicane hears three warnings
//! back to back while still counter-steering; a driver whose late-brake problem
//! has not improved in five laps hears the same sentence for the sixth
//! straight lap and stops listening. Both are failure modes of a coach that
//! says everything.
//!
//! The decision layer makes three promises to the audio layer:
//!
//! 1. **Nothing is spoken more often than it helps.** A [`Severity::Warn`]
//!    that fired recently at this corner is suppressed. A [`Severity::Critical`]
//!    overrides any cooldown — if it is that bad, the driver needs to hear
//!    it *now*.
//! 2. **Repetition fades.** A rule that has fired at the same corner for
//!    K consecutive passes stops firing, on the assumption that the driver
//!    has heard it and either fixed the problem or chosen not to.
//! 3. **Nothing is lost.** Suppression is a present-tense decision. The
//!    counter that drives repetition suppression is per (corner, kind) and
//!    resets the moment a corner passes without the issue firing — the
//!    driver is not punished for one bad lap after five clean ones.
//!
//! # What this layer deliberately does not do
//!
//! * *Not* a prioritiser. When several advices pass the gate at the same
//!   instant, the engine returns them all and lets the sink decide what to
//!   interrupt and what to queue. This is intentional: prioritisation is a
//!   *delivery* concern, and the audio layer is the only place that knows
//!   the current state of the spoken queue.
//! * *Not* a phraser. The phraser decided the words. The engine decides
//!   the timing.
//! * *Not* a session-level summariser. End-of-lap advice ("you lost 0.4 s
//!   total") is a separate concern and the [`DecisionEngine`] trait has the
//!   hook for it, but the default implementation deliberately returns
//!   nothing — the MVP speaks per-corner only.
//!
//! # Determinism
//!
//! All gating depends on the [`Instant`] passed in, the engine's internal
//! state, and the configuration. Same input time → same decision. Tests
//! build synthetic times by subtracting a [`Duration`] from a base, which
//! makes the cooldown logic fully unit-testable without sleeping.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::core::ids::CornerId;
use crate::coaching::advice::Advice;
use crate::models::issue::{IssueKind, Severity};

/// Knobs for [`DecisionEngine`]. Every field is a **tuning knob**.
#[derive(Debug, Clone, Copy)]
pub struct DecisionConfig {
    /// How long after speaking at a corner the engine refuses to speak
    /// *any* issue at that corner, regardless of kind. Critical overrides
    /// this.
    pub corner_cooldown: Duration,
    /// How long after speaking an issue *kind* the engine refuses to speak
    /// that kind *anywhere*. Catches the "lost time, lost time, lost time"
    /// nag when the same time-loss pattern shows up at every corner.
    pub kind_cooldown: Duration,
    /// After this many *consecutive* clean passes where the same kind fired
    /// at the same corner, stop saying it. Resets the first time the corner
    /// passes without that kind firing.
    pub repetition_limit: u32,
    /// Whether [`Severity::Info`] issues should be allowed through the gate.
    /// Default `false`: Info is a "by the way" the driver has to opt into.
    pub info_enabled: bool,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            corner_cooldown: Duration::from_secs(15),
            kind_cooldown: Duration::from_secs(30),
            repetition_limit: 5,
            info_enabled: false,
        }
    }
}

/// The gate between phraser and audio sink.
///
/// Stateful: an engine owns its cooldowns and repetition counters and is
/// therefore not `Copy`. One engine exists per live session.
///
/// The trait is intentionally narrow: [`DecisionEngine::gate`] decides one
/// piece of [`Advice`], and [`DecisionEngine::on_lap_complete`] is a hook
/// for future lap-summary advice. Anything more elaborate belongs in a
/// different layer.
pub trait DecisionEngine {
    /// Should this advice be spoken right now?
    ///
    /// Returning `Some(advice)` means: speak it. Returning `None` means:
    /// suppressed. The same `advice` is returned unchanged because the
    /// engine is not allowed to rewrite phrasing; its only effect on the
    /// payload is the bookkeeping in its own state.
    fn gate(&mut self, advice: Advice, now: Instant) -> Option<Advice>;

    /// End-of-lap hook. The default does nothing; an implementation may
    /// return lap-summary advice here.
    fn on_lap_complete(&mut self, _now: Instant) -> Vec<Advice> {
        Vec::new()
    }
}

/// The MVP engine. Holds a [`DecisionConfig`], a per-corner record of when
/// it last spoke (for the corner cooldown), a per-kind record of when it
/// last spoke (for the kind cooldown), and a per-(corner, kind)
/// consecutive-fires counter for repetition suppression.
#[derive(Debug)]
pub struct ThrottlingEngine {
    config: DecisionConfig,
    /// Corner id → instant of last successful gate at that corner.
    last_spoken_at_corner: HashMap<CornerId, Instant>,
    /// Issue kind → instant of last successful gate for that kind.
    last_spoken_for_kind: HashMap<IssueKind, Instant>,
    /// `(corner, kind)` → consecutive passes where this kind fired.
    /// Resets on the first pass where the kind does *not* fire at that
    /// corner — see [`ThrottlingEngine::note_passed_cleanly`].
    consecutive: HashMap<(CornerId, IssueKind), u32>,
}

impl ThrottlingEngine {
    pub fn new(config: DecisionConfig) -> Self {
        Self {
            config,
            last_spoken_at_corner: HashMap::new(),
            last_spoken_for_kind: HashMap::new(),
            consecutive: HashMap::new(),
        }
    }

    /// How many consecutive clean passes have produced this kind at this
    /// corner. Public for the UI's "we have said this N times" indicator.
    pub fn consecutive_fires(&self, corner: CornerId, kind: IssueKind) -> u32 {
        self.consecutive.get(&(corner, kind)).copied().unwrap_or(0)
    }
}

impl DecisionEngine for ThrottlingEngine {
    fn gate(&mut self, advice: Advice, now: Instant) -> Option<Advice> {
        // Step 1: severity floor. Info is opt-in only.
        if advice.severity == Severity::Info && !self.config.info_enabled {
            return None;
        }

        let key = (advice.corner_id, advice.kind);

        // Step 2: repetition suppression. If we have fired this kind at this
        // corner for `repetition_limit` consecutive passes in a row, stay
        // silent. The counter is *not* incremented here — that happens only
        // when the engine actually speaks, because the rule "stop nagging
        // when you've said it" should not count suppressions against the
        // driver.
        if let Some(&count) = self.consecutive.get(&key) {
            if count >= self.config.repetition_limit {
                return None;
            }
        }

        // Step 3: cooldowns. Critical overrides both. Otherwise, both the
        // per-corner and per-kind cooldowns must have elapsed. Two axes
        // because they catch different nag patterns:
        // * per-corner: "we just spoke at T7, T7 should not speak again for
        //   a while even if the next issue is a different kind."
        // * per-kind: "we just spoke 'lost time' at T7, 'lost time' at T9
        //   five seconds later is the same advice again."
        let allow = match advice.severity {
            Severity::Critical => true,
            Severity::Warn | Severity::Info => {
                let corner_ok = self
                    .last_spoken_at_corner
                    .get(&advice.corner_id)
                    .map(|t| now.saturating_duration_since(*t) >= self.config.corner_cooldown)
                    .unwrap_or(true);
                let kind_ok = self
                    .last_spoken_for_kind
                    .get(&advice.kind)
                    .map(|t| now.saturating_duration_since(*t) >= self.config.kind_cooldown)
                    .unwrap_or(true);
                corner_ok && kind_ok
            }
        };
        if !allow {
            return None;
        }

        // Step 4: bookkeeping. We spoke it.
        self.last_spoken_at_corner.insert(advice.corner_id, now);
        self.last_spoken_for_kind.insert(advice.kind, now);
        self.consecutive
            .entry(key)
            .and_modify(|c| *c = c.saturating_add(1))
            .or_insert(1);
        Some(advice)
    }

    fn on_lap_complete(&mut self, _now: Instant) -> Vec<Advice> {
        // The repetition counter is per (corner, kind) and persists across
        // laps: a driver who is bad at T7 *every* lap still deserves
        // silence. The counter resets the first time a corner passes
        // without the issue firing — see `note_passed_cleanly`, which the
        // MVP pipeline does not call. The default lap-complete hook is
        // intentionally empty.
        Vec::new()
    }
}

impl ThrottlingEngine {
    /// Note that a corner was driven cleanly, with no issues of the given
    /// kind raised. Resets the consecutive-fires counter for that
    /// (corner, kind) pair, on the assumption that one clean pass means the
    /// driver has either fixed the problem or moved on.
    ///
    /// The MVP does not call this — the pipeline raises issues per corner
    /// and only fires the counter on a positive gate. A future batch that
    /// sees all clean passes (not just the ones that raised issues) will
    /// call this to give the driver a clean slate.
    pub fn note_passed_cleanly(&mut self, corner: CornerId, kind: IssueKind) {
        self.consecutive.remove(&(corner, kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::corner::CornerDirection;

    fn advice(kind: IssueKind, severity: Severity, corner: u32) -> Advice {
        Advice::from_issue(
            &crate::models::issue::DrivingIssue::new(
                CornerId(corner),
                CornerDirection::Right,
                kind,
                severity,
            ),
            format!("{kind:?}"),
        )
    }

    /// A base `Instant` for tests. Real time is fine here because all the
    /// engine's decisions are deltas, not absolute.
    fn base() -> Instant {
        Instant::now()
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base - Duration::from_secs(1000) + Duration::from_secs(secs)
    }

    #[test]
    fn info_is_suppressed_unless_opted_in() {
        let mut e = ThrottlingEngine::new(DecisionConfig::default());
        let a = advice(IssueKind::BrakedInsideCorner, Severity::Info, 3);
        assert!(e.gate(a.clone(), base()).is_none());
        let mut cfg = DecisionConfig::default();
        cfg.info_enabled = true;
        let mut e = ThrottlingEngine::new(cfg);
        assert!(e.gate(a, base()).is_some());
    }

    #[test]
    fn warn_within_cooldown_at_same_corner_is_suppressed() {
        let cfg = DecisionConfig {
            corner_cooldown: Duration::from_secs(10),
            // Kind cooldown must also be short, otherwise the test would
            // need to wait the kind cooldown before being allowed to
            // re-speak — which is a separate concern the next test covers.
            kind_cooldown: Duration::from_secs(10),
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let a = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);

        // First time: allowed.
        assert!(e.gate(a.clone(), at(base, 0)).is_some());
        // Same corner, kind, severity, 5 s later: suppressed.
        assert!(e.gate(a.clone(), at(base, 5)).is_none());
        // 11 s later: both cooldowns elapsed.
        assert!(e.gate(a.clone(), at(base, 11)).is_some());
    }

    #[test]
    fn kind_cooldown_catches_a_repeat_kind_at_a_different_corner() {
        let cfg = DecisionConfig {
            // Long corner cooldown so the per-corner axis does not fire.
            corner_cooldown: Duration::from_secs(60),
            kind_cooldown: Duration::from_secs(10),
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let t3 = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);
        let t7 = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 7);

        assert!(e.gate(t3, at(base, 0)).is_some());
        // T7 has a fresh corner cooldown, but the same kind was just spoken
        // 5 s ago at T3 → kind cooldown blocks it.
        assert!(e.gate(t7, at(base, 5)).is_none());
        // Past the kind cooldown → allowed.
        assert!(
            e.gate(advice(IssueKind::BrakedInsideCorner, Severity::Warn, 7), at(base, 11))
                .is_some()
        );
    }

    #[test]
    fn warn_cooldown_is_per_corner_not_global() {
        let cfg = DecisionConfig {
            corner_cooldown: Duration::from_secs(10),
            kind_cooldown: Duration::from_secs(0),
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let t3 = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);
        let t7 = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 7);

        assert!(e.gate(t3.clone(), at(base, 0)).is_some());
        // T7 has never been spoken → not in cooldown.
        assert!(e.gate(t7, at(base, 1)).is_some());
        // T3 again, immediately after → in cooldown.
        assert!(e.gate(t3, at(base, 2)).is_none());
    }

    #[test]
    fn critical_overrides_any_cooldown() {
        let mut e = ThrottlingEngine::new(DecisionConfig::default());
        let base = base();
        let warn = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);
        let crit = advice(IssueKind::BrakedInsideCorner, Severity::Critical, 3);

        assert!(e.gate(warn, at(base, 0)).is_some());
        // Critical fires 1 s later, well inside cooldown.
        assert!(e.gate(crit, at(base, 1)).is_some());
    }

    #[test]
    fn consecutive_fires_count_up_until_the_repetition_limit_then_silence() {
        let cfg = DecisionConfig {
            corner_cooldown: Duration::from_millis(0),
            kind_cooldown: Duration::from_millis(0),
            repetition_limit: 3,
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let a = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);

        for fires_u64 in 1u64..=3 {
            let fires = fires_u64 as u32;
            assert!(
                e.gate(a.clone(), at(base, fires_u64)).is_some(),
                "should fire at repetition {fires}"
            );
            assert_eq!(e.consecutive_fires(CornerId(3), IssueKind::BrakedInsideCorner), fires);
        }
        // Fourth fire → silenced.
        assert!(e.gate(a.clone(), at(base, 4)).is_none());
        // The counter did not advance on the suppressed attempt.
        assert_eq!(e.consecutive_fires(CornerId(3), IssueKind::BrakedInsideCorner), 3);
    }

    #[test]
    fn note_passed_cleanly_resets_the_consecutive_counter() {
        let cfg = DecisionConfig {
            corner_cooldown: Duration::from_millis(0),
            kind_cooldown: Duration::from_millis(0),
            repetition_limit: 2,
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let a = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);

        assert!(e.gate(a.clone(), at(base, 1)).is_some());
        assert!(e.gate(a.clone(), at(base, 2)).is_some());
        // Limit reached; would be silent on next attempt.
        assert!(e.gate(a.clone(), at(base, 3)).is_none());

        // Driver cleans up at T3.
        e.note_passed_cleanly(CornerId(3), IssueKind::BrakedInsideCorner);
        assert_eq!(e.consecutive_fires(CornerId(3), IssueKind::BrakedInsideCorner), 0);
        // Next attempt fires again.
        assert!(e.gate(a, at(base, 4)).is_some());
    }

    #[test]
    fn different_kinds_at_the_same_corner_track_independently() {
        let cfg = DecisionConfig {
            corner_cooldown: Duration::from_secs(10),
            kind_cooldown: Duration::from_secs(10),
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let brake = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);
        let apex = advice(IssueKind::LateApex, Severity::Warn, 3);

        assert!(e.gate(brake.clone(), at(base, 0)).is_some());
        // Same corner, same instant: corner cooldown blocks any second
        // issue at this corner regardless of kind. That is the per-corner
        // cooldowns's job — to stop a corner from firing twice in quick
        // succession.
        assert!(
            e.gate(apex.clone(), at(base, 0)).is_none(),
            "corner cooldown must also block a different kind at the same corner"
        );
        // 11 s later: both cooldowns elapsed, apex fires.
        assert!(e.gate(apex, at(base, 11)).is_some());
        // And its own kind counter starts at 1, separate from brake's.
        assert_eq!(e.consecutive_fires(CornerId(3), IssueKind::BrakedInsideCorner), 1);
        assert_eq!(e.consecutive_fires(CornerId(3), IssueKind::LateApex), 1);
    }

    #[test]
    fn different_corners_with_the_same_kind_track_independently() {
        // This is the case the per-corner cooldown exists for: advice at
        // T3 should not silence advice at T7 in the next instant, because
        // they're about different parts of the track.
        let cfg = DecisionConfig {
            corner_cooldown: Duration::from_secs(10),
            kind_cooldown: Duration::from_secs(0), // disable kind cooldown
            ..DecisionConfig::default()
        };
        let mut e = ThrottlingEngine::new(cfg);
        let base = base();
        let t3 = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);
        let t7 = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 7);

        assert!(e.gate(t3, at(base, 0)).is_some());
        // T7 is a different corner → fresh corner cooldown.
        assert!(e.gate(t7, at(base, 1)).is_some());
    }

    #[test]
    fn gate_returns_the_advice_unchanged_when_it_passes() {
        let mut e = ThrottlingEngine::new(DecisionConfig::default());
        let a = advice(IssueKind::BrakedInsideCorner, Severity::Critical, 3);
        let out = e.gate(a.clone(), base()).expect("critical should always pass");
        assert_eq!(out, a);
    }

    #[test]
    fn gate_is_deterministic_for_identical_state() {
        // Two engines built from the same config and fed the same input
        // produce the same decision — the trait's guarantee that the audio
        // layer can replay a session deterministically.
        let cfg = DecisionConfig::default();
        let mut a = ThrottlingEngine::new(cfg);
        let mut b = ThrottlingEngine::new(cfg);
        let base = base();
        let adv = advice(IssueKind::BrakedInsideCorner, Severity::Warn, 3);

        let ta = a.gate(adv.clone(), at(base, 0));
        let tb = b.gate(adv.clone(), at(base, 0));
        assert_eq!(ta, tb);
        assert!(ta.is_some());

        let ta2 = a.gate(adv.clone(), at(base, 1));
        let tb2 = b.gate(adv.clone(), at(base, 1));
        assert_eq!(ta2, tb2);
        assert!(ta2.is_none());
    }

    #[test]
    fn on_lap_complete_is_a_no_op_in_the_mvp() {
        let mut e = ThrottlingEngine::new(DecisionConfig::default());
        assert!(e.on_lap_complete(base()).is_empty());
    }
}