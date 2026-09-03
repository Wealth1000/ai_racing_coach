//! Session setup: turning a discovered session into the model and reference
//! a coaching run needs.
//!
//! Shared by every entry point that coaches or analyses against canonical
//! corners — `coach analyse`, `coach live`, `coach gui` — so the rules
//! ("which model file", "what a car mismatch means", "when a personal best
//! is unusable") are stated once. It lived in `main.rs` until the live path
//! needed it too: a live session discovers its session facts from the first
//! shared-memory frame, and the GUI attaches in the background where a
//! private copy of the rules could drift.

use std::path::Path;

use crate::core::sample::SessionInfo;
use crate::features::ReferenceStore;
use crate::features::track_model::TrackModel;
use crate::core::Result;

/// Load the track model matching a session, or explain that one must be
/// learned first.
///
/// The car-mismatch warning lives here rather than in the callers: every
/// consumer of a model needs it, and none of them should be able to forget
/// that per-car boundaries make cross-car numbers approximate.
pub fn load_model_for_session(session: &SessionInfo, model_dir: &Path) -> Result<TrackModel> {
    // Models live one directory per sim (data/tracks/ac/…): two sims can name
    // the same circuit, and a model is only meaningful for the sim it was
    // learned in.
    let path = TrackModel::path_in(model_dir, session.sim, &session.track);
    if !path.exists() {
        return Err(crate::core::CoachError::NotEnoughData {
            action: "work from a track model",
            detail: format!(
                "no model for {} at {} — learn one first with `coach learn-track`",
                session.track,
                path.display()
            ),
        });
    }
    let model = TrackModel::load(&path)?;
    model.check_sim(session.sim)?;
    model.check_track(&session.track, session.track_length)?;

    // Boundaries are per-car (see the track_model module docs). Analysing a
    // different car is allowed — every number stays self-consistent within
    // this capture — but the boundaries themselves shift with speed, so it
    // must not happen silently.
    if model.provenance.car != session.car {
        eprintln!(
            "warning: the model was learned from laps of {}, but this capture is a {} — \
             corner boundaries shift with speed, so treat them as approximate",
            model.provenance.car, session.car
        );
    }

    Ok(model)
}

/// Load the personal best matching this session and model, or the empty
/// stand-in when there is none to use.
///
/// An unusable PB is a warning, not an error: the session runs without
/// comparison rather than refusing to run at all.
pub fn load_reference_for_session(
    session: &SessionInfo,
    model: &TrackModel,
    model_dir: &Path,
) -> ReferenceStore {
    let path = ReferenceStore::path_in(model_dir, session.sim, &session.track);
    if !path.exists() {
        return ReferenceStore::empty(model);
    }
    match ReferenceStore::load(&path) {
        Ok(existing)
            if existing.compatible_with(session.sim, &session.car, model.fingerprint()) =>
        {
            existing
        }
        Ok(_) => {
            eprintln!(
                "warning: the personal best at {} was recorded for a different car or an \
                 earlier model of this track — running without comparison",
                path.display()
            );
            ReferenceStore::empty(model)
        }
        Err(e) => {
            eprintln!(
                "warning: could not read {}: {e} — running without comparison",
                path.display()
            );
            ReferenceStore::empty(model)
        }
    }
}
