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