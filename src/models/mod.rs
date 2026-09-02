//! What went wrong: turning per-corner driving facts into per-corner issues.
//!
//! The rule layer is the brain of the coach. Everything else — the feature
//! extraction, the reference store, the audio, the decision throttler — exists
//! to feed it well-shaped inputs and to act on its outputs. Keeping the seam
//! narrow is what lets the rest of the system stay simple.
//!
//! # The trait
//!
//! [`DrivingModel::predict`] takes a corner's features and, optionally, the
//! driver's best pass through that corner. The `Option` is the entire reason
//! this trait exists: a rule that only makes sense with a reference — "you
//! braked 8 m later than your best lap" — must not pretend to fire on the
//! first capture of the day, when no PB exists. A rule that does not need a
//! reference must not demand one.
//!
//! Returning [`Vec<DrivingIssue>`] rather than a single issue reflects what
//! one corner can actually be doing wrong: a pass can be late on the brakes
//! and slow at the apex at the same time, and the coaching layer would rather
//! know both.
//!
//! The plan originally sketched `fn predict(features) -> DrivingIssue`. That
//! version was wrong in three ways: it took `&mut self` so a model could not
//! hold loaded thresholds or weights; it implicitly assumed a global
//! reference was available somewhere; and it forced every corner into a
//! single-issue straitjacket that does not match the data.
//!
//! # Composition
//!
//! Multiple [`DrivingModel`]s compose trivially: call them in order, concat
//! the outputs. There is intentionally no priority, voting, or suppression at
//! this layer — that is the decision engine's job in Batch 11. A rule saying
//! "critical" and a rule saying "info" can coexist; both reach the throttler
//! and the throttler chooses.
//!
//! # Tiering
//!
//! The plan calls for two tiers. They are realised as two methods on a single
//! type rather than two separate traits, because the cheap tier and the
//! reference tier share state — the same thresholds, the same logged corner
//! id — and splitting them would duplicate that for no gain. See
//! [`crate::features::corner_features::CornerFeatures`] for the input the
//! cheap tier works from, and [`crate::features::reference::CornerReference`]
//! for the input the reference tier compares against.

use crate::features::corner_features::CornerFeatures;
use crate::features::reference::CornerReference;

pub mod issue;
pub mod rules;

pub use issue::{DrivingIssue, IssueKind, Severity};

/// Given one corner's pass and (optionally) the driver's best pass through it,
/// return the issues — possibly none — that the model wants flagged.
///
/// Returning empty is the right answer for a clean pass against a personal
/// best, and the right answer for a messy pass when no personal best exists
/// yet. The coaching layer interprets silence as "carry on".
///
/// Implementations must not panic on missing data: if `reference` is `None`
/// and the model would otherwise compare, it returns issues from the cheap
/// tier only. `CornerFeatures` is `Copy` so implementations may take it by
/// value; references are kept on the left for symmetry with the API contract
/// and so a future pass through the model can carry additional state.
pub trait DrivingModel {
    fn predict(
        &self,
        features: &CornerFeatures,
        reference: Option<&CornerReference>,
    ) -> Vec<DrivingIssue>;

    /// Short, human-readable name for logs and the UI's model picker. Should
    /// be stable across runs of the same binary so a driver can recognise
    /// which model is currently wired in.
    fn name(&self) -> &'static str;
}