//! How to say what the model layer decided.
//!
//! Three layers meet here:
//!
//! * [`crate::models`] emits [`DrivingIssue`]s — facts about driving.
//! * This module turns those into [`Advice`]s — facts plus a phrased
//!   sentence — via [`Phraser`]. The decision layer (Batch 11) then decides
//!   which of those [`Advice`]s to actually deliver.
//! * [`crate::audio`] (Batch 12) plays the chosen [`Advice`]s.
//!
//! None of these layers know each other's types beyond the seams above. The
//! audio layer does not see a [`DrivingIssue`], the model layer does not see
//! a [`Phraser`], and the decision layer does not see a sentence.
//!
//! See [`phrasing`] for how controller mode shapes the wording.

pub mod advice;
pub mod decision;
pub mod phrasing;

pub use advice::Advice;
pub use decision::{DecisionConfig, DecisionEngine, ThrottlingEngine};
pub use phrasing::{ControllerMode, DefaultPhraser, Phraser};

use crate::core::ids::CornerId;
use crate::features::corner_features::CornerFeatures;
use crate::features::reference::CornerReference;
use crate::models::DrivingModel;

/// Map one corner pass to the advice a driver could hear: run the model,
/// phrase each issue, and stamp every result with the corner the driver
/// should *hear* named.
///
/// `report_corner` is the id spoken to the driver, which is not always the id
/// the features carry: the second row of a corner straddling the start/finish
/// line reports its parent's id, so both halves of one physical corner are
/// named (and throttled) as the same turn.
///
/// This is the one place issues become advice, shared by `coach analyse`
/// (offline, unthrottled) and the live pipeline (which then gates through a
/// [`DecisionEngine`]). Neither invents its own mapping, which is what makes
/// the two report identical sets for identical passes.
pub fn advise_pass<M: DrivingModel, P: Phraser>(
    model: &M,
    phraser: &P,
    mode: ControllerMode,
    features: &CornerFeatures,
    report_corner: CornerId,
    reference: Option<&CornerReference>,
) -> Vec<Advice> {
    model
        .predict(features, reference)
        .into_iter()
        .map(|mut issue| {
            issue.corner_id = report_corner;
            Advice::from_issue(&issue, phraser.phrase(&issue, mode))
        })
        .collect()
}