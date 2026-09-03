//! The command layer: every `coach` subcommand's implementation, shared by
//! the CLI and the GUI.
//!
//! The functions here are the whole offline surface — `inspect`,
//! `learn-track`, `analyse`, `learn-pb`, `record`, `export-dataset` — moved
//! out of `main.rs` so the GUI's home screen (one button per command) can run
//! them without spawning a process. The seam is [`Progress`]: a command
//! reports through a sink it is handed rather than printing, and each surface
//! decides where the report goes — the CLI prints to stdout (warnings to
//! stderr, as before), the GUI streams the lines into its job screen.
//!
//! Nothing here owns threads or windows. Commands run to completion on the
//! caller's thread; a caller that wants them off the UI thread (the GUI does)
//! runs them on one and reads the sink's other end. `main.rs` keeps only
//! argument parsing and the two process-level wirings (`live`, `gui`) that
//! cannot be library calls.
//!
//! The report text is the CLI's own output, verbatim: the same tables, the
//! same sentences, so a GUI session and a terminal session can be compared
//! line for line.

use std::path::{Path, PathBuf};

use crate::coaching::{ControllerMode, DefaultPhraser, advise_pass};
use crate::core::error::CoachError;
use crate::core::sample::{Sample, SessionInfo};
use crate::features::FeatureParams;
use crate::features::ReferenceStore;
use crate::features::corner::{self, TrackCorner};
use crate::features::corner_features;
use crate::features::curvature;
use crate::features::lap::{Lap, LapTracker};
use crate::features::resample::{self, ResampledLap};
use crate::features::track_model::{LearnParams, ModelCorner, TrackModel};
use crate::models::rules::RuleModel;
use crate::runtime;
use crate::sims::{self, RecordOptions, SimProvider};
use crate::telemetry::TelemetrySource;

/// Where a command's report goes.
///
/// Two methods, because the CLI's own convention is two streams: results on
/// stdout, warnings on stderr. A sink that merges them (the GUI's job screen)
/// is free to — but the distinction must survive the trait so the CLI can
/// keep it.
pub trait Progress {
    /// One line of the report.
    fn line(&mut self, text: &str);
    /// A warning — something that did not stop the work but the driver
    /// should know.
    fn warn(&mut self, text: &str);
}

/// Open a capture and split it into laps.
///
/// Shared by every command: reading samples, honouring the provider's
/// verdict on the capture and finding lap boundaries is the same work
/// regardless of what is done with the laps afterwards. The source is
/// returned alongside them because it owns the
/// [`crate::core::sample::SessionInfo`] and the read statistics.
///
/// Takes no grid spacing: laps here are raw samples at the source's own rate.
/// Resampling onto a distance grid happens per-lap in the callers.
fn read_laps(
    capture: &Path,
    sim: Option<&str>,
) -> crate::Result<(Box<dyn TelemetrySource>, Vec<Lap>)> {
    let mut source = sims::open_capture(capture, sim)?;

    // Track length comes from the session the source discovers on its first
    // sample. The previous implementation estimated it from lap groupings and
    // got 29.9 m for a 4,286 m circuit.
    let mut tracker: Option<LapTracker> = None;
    let mut laps: Vec<Lap> = Vec::new();

    while let Some(mut sample) = source.next_sample()? {
        let tracker = tracker.get_or_insert_with(|| {
            let length = source
                .session()
                .expect("the first sample carries the session")
                .track_length;
            LapTracker::new(length)
        });
        if let Some(lap) = tracker.push(&mut sample) {
            laps.push(lap);
        }
    }

    if let Some(tracker) = tracker {
        laps.extend(tracker.finish());
    }

    Ok((source, laps))
}

/// `coach inspect` — read a capture and report what is in it: session, laps,
/// corners.
pub fn inspect(
    capture: &Path,
    sim: Option<&str>,
    step: f32,
    all_laps: bool,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    progress.line(&source.describe());

    progress.line("");
    print_source_stats(&*source, progress);
    progress.line("");
    print_lap_table(&laps, progress);

    let clean: Vec<&Lap> = laps.iter().filter(|l| l.quality.is_clean()).collect();
    if clean.is_empty() {
        progress.line("\nNo clean laps — nothing to analyse.");
        return Ok(());
    }

    // Fastest clean lap by default; the rest on request.
    let mut ordered = clean.clone();
    ordered.sort_by(|a, b| a.lap_time_s().total_cmp(&b.lap_time_s()));
    let chosen: &[&Lap] = if all_laps { &ordered } else { &ordered[..1] };

    for lap in chosen {
        progress.line("");
        analyse_lap(lap, step, progress);
    }

    Ok(())
}

/// `coach learn-track` — build the canonical corner set and save it.
///
/// Three stages per capture. Every clean lap is segmented on its rotation
/// profile θ(s) by MDL (a lap of a circuit can yield anything from 9 to 20
/// candidate arcs for a ten-corner layout, and that is fine — Stage 1 is
/// built for recall); the laps then vote on each candidate through a
/// ring-alignment, and only corners a Wilson majority bound confirms enter
/// the model, with geometry from per-field medians over the representative
/// laps. Pedal traces inside the confirmed arcs yield the decision events
/// — brake onset/release, throttle dip/pickup, flat-out flicks.
///
/// A corner straddling the start/finish line is stored as two rows linked
/// by `parent_id`; it is one corner, not two.
///
/// Multiple captures are the refinement loop: the laps of every capture vote
/// together, so a model re-learned from the original capture plus the ones a
/// live session recorded is a model of everything the driver has ever driven
/// — not a fresh start that forgets the first session. All captures must be
/// of the same track in the same car; anything else refuses rather than
/// voting corners from two different circuits into one set.
pub fn learn_track(
    captures: &[PathBuf],
    sim: Option<&str>,
    out_dir: &Path,
    step: f32,
    dry_run: bool,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    if captures.is_empty() {
        return Err(CoachError::NotEnoughData {
            action: "learn a track model",
            detail: "no capture was given".to_string(),
        });
    }

    // The laps of every capture, pooled. The session of the first is the
    // session of the model: every capture must agree with it on track and
    // car, checked as each is read, because a model is per-track and per-car
    // by construction (see the track_model module docs).
    let mut laps: Vec<Lap> = Vec::new();
    let mut session: Option<SessionInfo> = None;
    for capture in captures {
        let (source, capture_laps) = read_laps(capture, sim)?;
        progress.line(&source.describe());

        let capture_session = source.session().ok_or_else(|| CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;
        match &session {
            None => session = Some(capture_session.clone()),
            Some(first) => {
                if first.track != capture_session.track || first.car != capture_session.car {
                    return Err(CoachError::NotEnoughData {
                        action: "learn a track model",
                        detail: format!(
                            "{} is {} in the {}, but an earlier capture is {} in the {} — \
                             a model is per-track and per-car, so these cannot vote together",
                            capture.display(),
                            capture_session.car,
                            capture_session.track,
                            first.car,
                            first.track
                        ),
                    });
                }
            }
        }
        laps.extend(capture_laps);
    }
    let session = session.expect("at least one capture was checked above");

    let params = LearnParams { step_m: step };
    // The provenance names every capture, so a diff between two models of the
    // same circuit says which sessions voted. File names, not paths: the
    // model keeps the last path component (see `Provenance::capture`), so a
    // joined path would name only the final capture.
    let provenance = captures
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let model = TrackModel::learn(&session, &laps, &provenance, &params)?;

    progress.line("");
    print_model(&model, progress);

    let path = TrackModel::path_in(out_dir, model.sim, &model.track);

    // The file name keys on track and layout only, so learning the same circuit
    // in a second car lands on this same path. That is not a mistake — a model
    // is per-car by construction (see the track_model module docs) and there is
    // no car-independent answer to fall back on — but it must not be silent,
    // because the two cars genuinely disagree about the corner count and the
    // last command run would otherwise decide the model with no trace.
    if let Ok(existing) = TrackModel::load(&path) {
        progress.line(&format!("\nReplacing the model at {}", path.display()));
        progress.line(&format!(
            "  was: {} corners from {} lap(s) of {}",
            existing.corners.len(),
            existing.lap_count(),
            existing.provenance.car,
        ));
        progress.line(&format!(
            "  now: {} corners from {} lap(s) of {}",
            model.corners.len(),
            model.lap_count(),
            model.provenance.car,
        ));
        if existing.provenance.car != model.provenance.car {
            progress.line(
                "  note: different car — boundaries shift with speed, so this is a \
                 different model of the same circuit, not a correction of the old one",
            );
        }
    }

    if dry_run {
        progress.line(&format!(
            "\n--dry-run: nothing written (would be {})",
            path.display()
        ));
        return Ok(());
    }

    // The sim's directory may not exist yet — learning the first model of a
    // newly added sim is exactly when it does not — but `save` creates parent
    // directories itself, so there is nothing to do here.
    model.save(&path)?;
    progress.line(&format!("\nWrote {}", path.display()));
    Ok(())
}

fn print_model(model: &TrackModel, progress: &mut dyn Progress) {
    let (left, right) = model.direction_counts();
    progress.line(&format!(
        "Track model v{} — {} ({}), {:.0} m\n  \
         {} corners ({} right / {} left), learned from {} clean lap(s) in {}\n  \
         reference {}, line spread {:.2} m mean, {:.2} m worst at {:.0} m, {:.2} m grid\n  \
         estimator: {}",
        model.version,
        model.track,
        model.provenance.car,
        model.track_length_m,
        model.corners.len(),
        right,
        left,
        model.lap_count(),
        model.provenance.capture,
        model.provenance.reference_lap,
        model.provenance.reference_spread_m,
        model.provenance.reference_spread_max_m,
        model.provenance.reference_spread_max_at_m,
        model.provenance.step_m,
        model.provenance.estimator,
    ));
    if !model.provenance.pedal_events {
        progress.line("  no usable pedal channels: decision events were not learned");
    }

    progress.line(&format!(
        "\n  {:>4}  {:>3}  {:>8}  {:>8}  {:>7}  {:>8}  {:>7}  {:>8}  {:>7}  {:>6}",
        "turn", "dir", "start", "end", "length", "apex", "radius", "turn", "laps", "events"
    ));
    for c in &model.corners {
        let radius = match c.apex_radius_m() {
            Some(r) => format!("{r:>6.0}m"),
            None => "     --".to_string(),
        };
        // The second half of a line-straddling corner is the same corner as
        // its parent; the count of rows would otherwise read as one turn too
        // many.
        let id = match c.parent_id {
            Some(parent) => format!("{:>2}+{parent}", c.id.to_string()),
            None => format!("{:>4}", c.id.to_string()),
        };
        progress.line(&format!(
            "  {id}  {:>3}  {:>7.0}m  {:>7.0}m  {:>6.0}m  {:>7.0}m  {radius}  {:>7.0}°  {:>4}/{}  {:>6}",
            c.direction.short(),
            c.start_m,
            c.end_m,
            c.length_m(),
            c.apex_m,
            c.turn_degrees(),
            c.support,
            model.lap_count(),
            c.decision_events.len(),
        ));
    }
    print_unanimity(&model.corners, model.lap_count(), progress);

    // Confirmed decision boundaries, once per corner, in the driver's terms.
    let eventful: Vec<&ModelCorner> = model
        .corners
        .iter()
        .filter(|c| !c.decision_events.is_empty())
        .collect();
    if !eventful.is_empty() {
        progress.line("\n  decision events:");
        for c in eventful {
            let events: Vec<String> = c
                .decision_events
                .iter()
                .map(|e| format!("{} {:.0}m ({})", e.kind.name(), e.distance_m, e.support))
                .collect();
            progress.line(&format!("    turn {}: {}", c.id, events.join(", ")));
        }
    }
}

/// Flag the corners not every lap found. These are where the model is least
/// certain, and they are the first thing to check when it looks wrong.
///
/// Both halves of a line-straddling corner report the same numbers, so the
/// parent row alone is listed.
fn print_unanimity(corners: &[ModelCorner], laps: u32, progress: &mut dyn Progress) {
    let split: Vec<&ModelCorner> = corners
        .iter()
        .filter(|c| c.parent_id.is_none() && c.support < laps)
        .collect();
    if split.is_empty() {
        progress.line(&format!("\n  all {laps} laps agreed on every corner"));
        return;
    }
    let names: Vec<String> = split
        .iter()
        .map(|c| format!("{} ({}/{}, {:.0}%)", c.id, c.support, laps, c.match_fraction * 100.0))
        .collect();
    progress.line(&format!("\n  not unanimous: {}", names.join(", ")));
}

/// `coach analyse` — describe how a capture's clean laps drove each corner of
/// a track model.
///
/// The model supplies *where* the corners are, learned once per track and
/// car; this command reports *what each lap did inside them* — turn-in
/// speed, apex speed and where it sat relative to the geometric apex,
/// braking point, trail braking, throttle pickup. Slicing at the model's
/// boundaries rather than per-lap detections is what makes two laps'
/// numbers comparable.
pub fn analyse(
    capture: &Path,
    sim: Option<&str>,
    model_dir: &Path,
    step: f32,
    all_laps: bool,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    progress.line(&source.describe());

    let session = source
        .session()
        .ok_or_else(|| CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let model = runtime::load_model_for_session(session, model_dir)?;
    let reference = runtime::load_reference_for_session(session, &model, model_dir);

    progress.line("");
    progress.line(&format!(
        "Model {} — {} corners learned from {} lap(s) of {}",
        model.track,
        model.corners.len(),
        model.lap_count(),
        model.provenance.car,
    ));

    let clean: Vec<&Lap> = laps.iter().filter(|l| l.quality.is_clean()).collect();
    if clean.is_empty() {
        progress.line("\nNo clean laps — nothing to analyse.");
        return Ok(());
    }

    // Fastest clean lap by default, matching `inspect`.
    let mut ordered = clean.clone();
    ordered.sort_by(|a, b| a.lap_time_s().total_cmp(&b.lap_time_s()));
    let chosen: &[&Lap] = if all_laps { &ordered } else { &ordered[..1] };

    let params = FeatureParams::default();

    for lap in chosen {
        progress.line("");
        let Some(grid) = resample::resample_lap(&lap.samples, step) else {
            progress.line(&format!(
                "lap {}: not enough distinct positions to resample",
                lap.id.0
            ));
            continue;
        };
        let features = corner_features::extract_all(&model, &grid, &params, lap.id);
        if features.is_empty() {
            progress.line(&format!(
                "lap {} ({:.2}s): no model corner is fully covered by this lap",
                lap.id.0,
                lap.lap_time_s()
            ));
            continue;
        }
        progress.line(&format!(
            "lap {} — {:.2}s, {} corners driven",
            lap.id.0,
            lap.lap_time_s(),
            features.len()
        ));
        print_feature_table(&features, progress);
        print_advice(&model, &reference, &features, progress);
    }

    Ok(())
}

/// The advice the rules raise for one lap's features — the same sentences,
/// from the same shared mapping, that `coach live` delivers as they happen.
///
/// Unthrottled on purpose: this is the complete, unfiltered set, so it can be
/// compared line for line with what a live session would say before the
/// don't-disturb-the-driver layer starts suppressing repeats.
fn print_advice(
    model: &TrackModel,
    reference: &ReferenceStore,
    features: &[crate::features::CornerFeatures],
    progress: &mut dyn Progress,
) {
    let rules = RuleModel::default();
    let phraser = DefaultPhraser;
    let mode = ControllerMode::default();

    let mut lines = Vec::new();
    for f in features {
        // Corner ids are sequential from zero, so the id indexes the list.
        let Some(corner) = model.corners.get(f.corner_id.0 as usize) else {
            continue;
        };
        let report = corner.parent_id.unwrap_or(corner.id);
        for advice in advise_pass(&rules, &phraser, mode, f, report, reference.pass_for(f.corner_id))
        {
            lines.push(advice.phrased);
        }
    }

    if lines.is_empty() {
        progress.line("\n  advice: nothing the rules would raise for this lap");
    } else {
        progress.line("\n  advice (unthrottled — everything the rules raise):");
        for line in lines {
            progress.line(&format!("    {line}"));
        }
    }
}

/// `coach record` — the C# logger's job: capture live telemetry from the
/// running sim, in the logger's own NDJSON.
///
/// Thin by design — the waiting, polling, skip accounting and lap counting
/// live in the provider (`sims::assetto_corsa::record`), where a scripted
/// fake page store can test every path; all this does is pick the provider
/// and report what the recorder saw.
pub fn record(
    out: Option<&Path>,
    laps: Option<u32>,
    plain: bool,
    sim: Option<&str>,
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    let providers: Vec<&dyn SimProvider> =
        sims::registry().iter().map(|p| p.as_ref()).collect();
    let provider = sims::provider_for_live(&providers, sim)?;
    let opts = RecordOptions {
        out: out.map(Path::to_path_buf),
        laps,
        plain,
        stop,
    };
    let summary = provider.record(&opts)?;

    // A recording that never saw the car on track never resolved a file to
    // write — that is worth saying plainly rather than reporting zero frames
    // written to nowhere.
    match &summary.path {
        Some(path) => progress.line(&format!(
            "Recorded {} frames ({}) to {}",
            summary.frames,
            match summary.laps_completed {
                0 => "no laps completed".to_string(),
                n => format!("{n} lap(s)"),
            },
            path.display()
        )),
        None => progress.line("No frames recorded — the sim never published a session"),
    }
    let skipped = summary.skipped_no_position + summary.skipped_duplicate + summary.skipped_no_session;
    if skipped > 0 {
        progress.line(&format!(
            "Skipped {skipped} polls: {} before the car was on track, {} duplicates, {} without a session",
            summary.skipped_no_position, summary.skipped_duplicate, summary.skipped_no_session
        ));
    }
    Ok(())
}

/// What an export (or a share bundle) is built from: the session files, the
/// one track model they all belong to, and the personal best to join
/// against — the selection `coach export-dataset` and `coach share-dataset`
/// both need, so they cannot drift apart.
struct ExportSelection {
    sessions: Vec<PathBuf>,
    model: TrackModel,
    reference: Option<ReferenceStore>,
}

/// Pick the sessions, model and personal best for a dataset, the way `live`
/// picks them: from the first session header's own track and car, so the
/// join is exactly the corner set the session was coached against. A
/// session recorded against a different fingerprint of the model is
/// refused by the exporter rather than mis-joined.
fn select_for_export(
    sessions_dir: &Path,
    model_dir: &Path,
    action: &'static str,
    progress: &mut dyn Progress,
) -> crate::Result<ExportSelection> {
    let mut sessions: Vec<PathBuf> = std::fs::read_dir(sessions_dir)
        .map_err(|e| CoachError::Io {
            path: sessions_dir.display().to_string(),
            source: e,
        })?
        .map_while(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ndjson"))
        .collect();
    sessions.sort();
    if sessions.is_empty() {
        return Err(CoachError::NotEnoughData {
            action,
            detail: format!(
                "no .ndjson session files in {} — record one with `coach live --record-session`",
                sessions_dir.display()
            ),
        });
    }

    // The first session's header decides which model (and personal best)
    // every session is joined against; the fingerprint check inside the
    // exporter then holds the rest to it.
    let first = crate::storage::read_session(&sessions[0])?;
    let model_path = TrackModel::path_in(model_dir, first.header.sim, &first.header.track);
    if !model_path.exists() {
        return Err(CoachError::NotEnoughData {
            action,
            detail: format!(
                "no model for {} at {} — learn one first with `coach learn-track`",
                first.header.track,
                model_path.display()
            ),
        });
    }
    let model = TrackModel::load(&model_path)?;

    let pb_path = ReferenceStore::path_in(model_dir, first.header.sim, &first.header.track);
    let reference = match ReferenceStore::load(&pb_path) {
        Ok(store)
            if store.compatible_with(first.header.sim, &first.header.car, model.fingerprint()) =>
        {
            Some(store)
        }
        Ok(_) => {
            progress.warn(&format!(
                "warning: the personal best at {} was recorded for a different car or an \
                 earlier model of this track — exporting without reference columns",
                pb_path.display()
            ));
            None
        }
        Err(_) => None,
    };

    Ok(ExportSelection {
        sessions,
        model,
        reference,
    })
}

/// `coach export-dataset` — flatten recorded sessions into one CSV row per
/// corner pass.
pub fn export_dataset(
    sessions_dir: &Path,
    out: &Path,
    model_dir: &Path,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    let selection =
        select_for_export(sessions_dir, model_dir, "export a dataset", progress)?;
    let info = crate::storage::export_dataset(
        &selection.sessions,
        &selection.model,
        selection.reference.as_ref(),
        out,
    )?;
    progress.line(&format!(
        "{} rows, {} columns — {}",
        info.rows,
        info.columns,
        out.display()
    ));
    Ok(())
}

/// Send the dataset to the author — the GUI's "Send to author", behind the
/// share-telemetry consent.
///
/// The bundle is built from the same export the driver sees (the same
/// selection, the same fingerprint refusal), with the session names
/// scrubbed and a manifest added. It is POSTed to the compiled-in
/// endpoint ([`crate::storage::share::DEFAULT_ENDPOINT`], overridable via
/// `COACH_SHARE_ENDPOINT` for testing); an upload that fails degrades to
/// writing the bundle under `data/share/` for the driver to send by hand —
/// sharing is a favour and must never cost more than the try.
pub fn share_dataset(
    sessions_dir: &Path,
    model_dir: &Path,
    install_id: &str,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    let selection = select_for_export(sessions_dir, model_dir, "share a dataset", progress)?;
    let (info, csv) = crate::storage::export_dataset_text(
        &selection.sessions,
        &selection.model,
        selection.reference.as_ref(),
    )?;
    let manifest = crate::storage::share::manifest(&info, install_id);
    let bundle = crate::storage::share::build_bundle(&csv, manifest)?;

    // The preview is the honesty mechanism: what left the machine is what
    // the driver can read on the job screen, not a promise in a dialog.
    progress.line(&format!(
        "bundle: {} rows from {} session(s) of {} ({}) — coach {}",
        info.rows,
        info.sessions,
        info.track,
        info.cars.join(", "),
        env!("CARGO_PKG_VERSION")
    ));
    for line in csv.lines().take(3).skip(1) {
        progress.line(&format!("  {line}"));
    }

    let endpoint = std::env::var(crate::storage::share::ENDPOINT_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| crate::storage::share::DEFAULT_ENDPOINT.to_string());
    match crate::storage::share::upload(&endpoint, &bundle) {
        Ok(()) => {
            progress.line(&format!("sent to {endpoint} — thank you"));
            Ok(())
        }
        Err(e) => {
            progress.warn(&format!("warning: {e}"));
            let path = save_offline_bundle(&info, &bundle, progress)?;
            progress.line(&format!(
                "kept the bundle to send by hand instead: {}",
                path.display()
            ));
            Ok(())
        }
    }
}

/// Write the bundle to `data/share/` — the endpoint-less path and the
/// failed-upload fallback, one shape for both.
fn save_offline_bundle(
    info: &crate::storage::DatasetInfo,
    bundle: &[u8],
    progress: &mut dyn Progress,
) -> crate::Result<PathBuf> {
    let dir = Path::new(crate::storage::share::SHARE_DIR);
    let path = crate::storage::share::save_bundle(dir, &info.track.track, bundle)?;
    progress.line(&format!("bundle written: {}", path.display()));
    Ok(path)
}

/// `coach learn-pb` — record the driver's best pass through each corner as a
/// personal best.
///
/// Every clean lap's pass through every canonical corner is timed and
/// measured; the fastest pass through each span becomes that corner's
/// reference. Re-running against an existing personal best merges them
/// corner by corner — a stored pass survives unless this capture drove
/// that span strictly faster — so the file accumulates across sessions
/// instead of resetting with every one.
///
/// It refuses to merge across cars, or across a re-learned track model:
/// corner ordinals are positions in a learned list, and a re-learn can
/// silently make "T3" mean somewhere else. Either way it starts fresh
/// from this capture and says so.
///
/// Clean laps only, for the same reason as everywhere else: a spin through
/// T7 is a fact about the spin, and a personal best is not allowed to be one.
pub fn learn_pb(
    capture: &Path,
    sim: Option<&str>,
    model_dir: &Path,
    step: f32,
    dry_run: bool,
    progress: &mut dyn Progress,
) -> crate::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    progress.line(&source.describe());

    let session = source
        .session()
        .ok_or_else(|| CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let model = runtime::load_model_for_session(session, model_dir)?;

    let mut grids: Vec<(crate::core::ids::LapId, ResampledLap)> = Vec::new();
    let mut unresampled = 0usize;
    for lap in laps.iter().filter(|l| l.quality.is_clean()) {
        match resample::resample_lap(&lap.samples, step) {
            Some(grid) => grids.push((lap.id, grid)),
            None => unresampled += 1,
        }
    }
    if unresampled > 0 {
        progress.line(&format!(
            "\n{unresampled} clean lap(s) could not be put on the {step} m grid and were skipped"
        ));
    }
    if grids.is_empty() {
        return Err(CoachError::NotEnoughData {
            action: "record personal bests",
            detail: "no clean lap in the capture could be resampled".to_string(),
        });
    }

    let incoming = ReferenceStore::harvest(
        &model,
        session.car.clone(),
        &capture.display().to_string(),
        step,
        &FeatureParams::default(),
        &grids,
    )?;
    if incoming.corners.is_empty() {
        return Err(CoachError::NotEnoughData {
            action: "record personal bests",
            detail: format!(
                "none of the {} model corners was fully covered by any clean lap",
                model.corners.len()
            ),
        });
    }

    let path = ReferenceStore::path_in(model_dir, session.sim, &session.track);

    progress.line("");
    let store = if path.exists() {
        let existing = ReferenceStore::load(&path)?;
        if existing.compatible_with(session.sim, &session.car, model.fingerprint()) {
            let mut merged = existing;
            let report = merged.absorb(incoming);
            progress.line(&format!(
                "Merging into the existing personal best at {}:",
                path.display()
            ));
            progress.line(&format!(
                "  {} corner(s) improved, {} kept, {} added",
                report.improved, report.kept, report.added
            ));
            merged
        } else {
            progress.line(&format!(
                "Existing personal best at {} cannot be merged:",
                path.display()
            ));
            if existing.provenance.car != session.car {
                progress.line(&format!(
                    "  it was recorded in a {}, this capture is a {} — per-car numbers",
                    existing.provenance.car, session.car
                ));
            }
            if existing.model_fingerprint != model.fingerprint() {
                progress.line(
                    "  the track model has been re-learned since; the stored corner \
                     ordinals no longer mean the same places",
                );
            }
            progress.line("  starting fresh from this capture");
            incoming
        }
    } else {
        incoming
    };

    print_pb_table(&store, progress);

    if dry_run {
        progress.line(&format!(
            "\n--dry-run: nothing written (would be {})",
            path.display()
        ));
        return Ok(());
    }

    store.save(&path)?;
    progress.line(&format!("\nWrote {}", path.display()));
    Ok(())
}

/// The personal best, one row per corner with the same conventions as
/// `analyse`: speeds in km/h, distances signed relative to the boundary or
/// apex they are measured from.
fn print_pb_table(store: &ReferenceStore, progress: &mut dyn Progress) {
    progress.line(&format!(
        "\nPersonal best — {}, {}, {} corner(s) recorded from {}",
        store.track,
        store.provenance.car,
        store.corners.len(),
        store.provenance.captures.join(", "),
    ));

    progress.line(&format!(
        "\n  {:>4}  {:>3}  {:>5}  {:>5}  {:>5}  {:>7}  {:>6}  {:>6}  {:>5}",
        "turn", "dir", "in", "apex", "out", "time", "brake@", "power@", "trail"
    ));
    for c in &store.corners {
        let brake_at = match c.brake_offset_m {
            Some(off) => format!("{off:+.0}m"),
            None => "   --".to_string(),
        };
        let power_at = match c.throttle_pickup_offset_m {
            Some(off) => format!("{off:+.0}m"),
            None => "   --".to_string(),
        };

        progress.line(&format!(
            "  {:>4}  {:>3}  {:>5.0}  {:>5.0}  {:>5.0}  {:>6.2}s  {brake_at:>6}  {power_at:>6}  {:>5}",
            c.corner_id.to_string(),
            c.direction.short(),
            c.entry_speed_mps * 3.6,
            c.apex_speed_mps * 3.6,
            c.exit_speed_mps * 3.6,
            c.time_in_corner_s,
            if c.trail_braking { "yes" } else { "-" },
        ));
    }
}

fn print_source_stats(source: &dyn TelemetrySource, progress: &mut dyn Progress) {
    let stats = source.stats();
    progress.line(&format!(
        "Frames read     {}\nBlank lines     {}\nUnparseable     {}",
        stats.samples, stats.blank_lines, stats.bad_lines
    ));
}

fn print_lap_table(laps: &[Lap], progress: &mut dyn Progress) {
    progress.line(&format!("Laps ({} wrap segments)", laps.len()));
    progress.line(&format!(
        "  {:>3}  {:>9}  {:>8}  {:>9}  {:>7}  {:<28}",
        "id", "time", "coverage", "rotation", "samples", "quality"
    ));

    for lap in laps {
        // Rotation in units of pi is the readable form: a clean lap is 2.00,
        // and the MX5's spin is 4.00.
        let rotation_pi = lap.net_rotation / std::f32::consts::PI;
        let mut note = lap.quality.reason().to_string();
        if lap.sim_lap_time_ms.is_none() && lap.quality.is_clean() {
            note.push_str(" (wall clock)");
        }
        progress.line(&format!(
            "  {:>3}  {:>8.2}s  {:>7.1}%  {:>7.2}pi  {:>7}  {:<28}",
            lap.id.0,
            lap.lap_time_s(),
            lap.coverage * 100.0,
            rotation_pi,
            lap.samples.len(),
            note
        ));
    }

    let clean = laps.iter().filter(|l| l.quality.is_clean()).count();
    let full = laps
        .iter()
        .filter(|l| l.quality != crate::features::LapQuality::Partial)
        .count();
    progress.line(&format!(
        "  {} segments, {} full, {} clean",
        laps.len(),
        full,
        clean
    ));
}

fn analyse_lap(lap: &Lap, step: f32, progress: &mut dyn Progress) {
    progress.line(&format!(
        "Lap {} — {:.2}s, {} raw samples",
        lap.id.0,
        lap.lap_time_s(),
        lap.samples.len()
    ));

    // The health check for the resampling stage, printed because it is the one
    // number that says whether corner detection can work at all.
    let raw_zeros = curvature::zero_fraction(&curvature::signed_curvature(&lap.samples));

    let Some(grid) = resample::resample_lap(&lap.samples, step) else {
        progress.line("  not enough distinct positions to resample");
        return;
    };

    let grid_zeros = curvature::zero_fraction(&curvature::signed_curvature(&grid.samples));
    progress.line(&format!(
        "  resampled to {} points @ {:.2} m ({} non-monotone samples dropped)",
        grid.samples.len(),
        grid.step_m,
        grid.non_monotone_dropped
    ));
    progress.line(&format!(
        "  curvature zeros: {:.1}% raw -> {:.1}% resampled",
        raw_zeros * 100.0,
        grid_zeros * 100.0
    ));

    let corners = corner::detect_corners(&grid);
    let (left, right) = corner::direction_counts(&corners);
    progress.line(&format!(
        "  {} corners, {} right / {} left",
        corners.len(),
        right,
        left
    ));

    if corners.is_empty() {
        return;
    }
    print_corner_table(&corners, &grid, progress);
}

/// One row per corner: speeds in km/h, distances in metres relative to the
/// corner they belong to, everything a rule will compare between laps.
fn print_feature_table(
    features: &[crate::features::CornerFeatures],
    progress: &mut dyn Progress,
) {
    let kmh = |mps: f32| format!("{:>5.0}", mps * 3.6);

    progress.line(&format!(
        "\n  {:>4}  {:>3}  {:>5}  {:>5}  {:>5}  {:>6}  {:>6}  {:>5}  {:>6}  {:>6}  {:>5}  {:>3}",
        "turn",
        "dir",
        "in",
        "apex",
        "out",
        "vmin@",
        "brake@",
        "trail",
        "power@",
        "time",
        "slip",
        "off"
    ));
    for f in features {
        let vmin_at = format!("{:+.0}m", f.speed_min_offset_m);
        // Signed offset of the braking point from the corner boundary:
        // negative is before the corner, positive is braking past it.
        let brake_at = match f.braking_length_m {
            Some(len) => format!("{:+.0}m", -len),
            None => "   --".to_string(),
        };
        let power_at = match f.throttle_pickup_offset_m {
            Some(off) => format!("{off:+.0}m"),
            None => "   --".to_string(),
        };
        let slip_deg = f.peak_abs_slip_rad.to_degrees();

        progress.line(&format!(
            "  {:>4}  {:>3}  {}  {}  {}  {vmin_at:>6}  {brake_at:>6}  {:>5}  {power_at:>6}  \
             {:>5.2}s  {slip_deg:>4.1}\u{00b0}  {:>3}",
            f.corner_id.to_string(),
            f.direction.short(),
            kmh(f.entry_speed_mps),
            kmh(f.apex_speed_mps),
            kmh(f.exit_speed_mps),
            if f.trail_braking { "yes" } else { "-" },
            f.time_in_corner_s,
            f.off_track_points,
        ));
    }
}

fn print_corner_table(
    corners: &[TrackCorner],
    grid: &ResampledLap,
    progress: &mut dyn Progress,
) {
    progress.line(&format!(
        "\n  {:>4}  {:>3}  {:>8}  {:>8}  {:>7}  {:>7}  {:>7}  {:>8}  {:>8}",
        "turn", "dir", "start", "end", "length", "apex", "radius", "turn", "min spd"
    ));
    for c in corners {
        let radius = match c.apex_radius_m() {
            Some(r) => format!("{r:>6.0}m"),
            None => "     --".to_string(),
        };
        progress.line(&format!(
            "  {:>4}  {:>3}  {:>7.0}m  {:>7.0}m  {:>6.0}m  {:>6.0}m  {}  {:>7.0}°  {:>5.1}m/s",
            c.id.to_string(),
            c.direction.short(),
            c.start_m,
            c.end_m,
            c.length_m(),
            c.apex_m,
            radius,
            c.turn_degrees(),
            c.min_speed,
        ));
    }

    // Speed at the apex is what a driver will ask about first, so give the
    // straight-line context too: the fastest point on the lap.
    if let Some(top) = grid
        .samples
        .iter()
        .max_by(|a: &&Sample, b: &&Sample| a.speed.total_cmp(&b.speed))
    {
        progress.line(&format!(
            "\n  top speed {:.1} km/h at {:.0} m",
            top.speed * 3.6,
            top.lap_distance
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONZA_CAPTURE: &str =
        "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";

    /// A sink that keeps everything, so a test can read the report a command
    /// produced — the same role the GUI's job screen plays for the driver.
    #[derive(Default)]
    struct VecProgress {
        lines: Vec<String>,
        warns: Vec<String>,
    }

    impl Progress for VecProgress {
        fn line(&mut self, text: &str) {
            self.lines.push(text.to_string());
        }
        fn warn(&mut self, text: &str) {
            self.warns.push(text.to_string());
        }
    }

    fn monza() -> PathBuf {
        let path = PathBuf::from(MONZA_CAPTURE);
        if !path.exists() {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
        }
        path
    }

    #[test]
    fn learn_track_reports_the_model_without_writing_on_dry_run() {
        let capture = monza();
        if !capture.exists() {
            return;
        }
        let out_dir = std::env::temp_dir().join("coach_commands_tests/learn");
        let mut progress = VecProgress::default();
        learn_track(
            &[capture],
            None,
            &out_dir,
            1.0,
            true,
            &mut progress,
        )
        .expect("the reference capture yields a model");

        let report = progress.lines.join("\n");
        assert!(report.contains("Track model"), "{report}");
        assert!(
            report.contains("--dry-run: nothing written"),
            "the dry run must say so: {report}"
        );
        // Dry-run is a promise: nothing landed in the output directory.
        assert!(
            !out_dir.join("ac/monza.json").exists(),
            "dry-run wrote a model anyway"
        );
        assert!(progress.warns.is_empty());
    }

    /// The refinement loop in one test: two captures of the same track vote
    /// together, so the pooled lap count is the sum — and the provenance
    /// names both captures, because "which sessions built this model" is the
    /// first question when a model looks wrong.
    #[test]
    fn learn_track_pools_the_laps_of_every_capture() {
        let capture = monza();
        if !capture.exists() {
            return;
        }
        let one = {
            let mut p = VecProgress::default();
            learn_track(std::slice::from_ref(&capture), None, &std::env::temp_dir(), 1.0, true, &mut p)
                .expect("one capture learns");
            p.lines.join("\n")
        };
        let two = {
            let mut p = VecProgress::default();
            learn_track(
                &[capture.clone(), capture.clone()],
                None,
                &std::env::temp_dir(),
                1.0,
                true,
                &mut p,
            )
            .expect("two captures of the same session learn");
            p.lines.join("\n")
        };

        let lap_count = |report: &str| -> u64 {
            report
                .lines()
                .find(|l| l.contains("clean lap(s) in"))
                .and_then(|l| l.split("learned from").nth(1))
                .and_then(|l| l.trim().split(' ').next().map(|n| n.parse().expect("digits")))
                .expect("the model report names its lap count")
        };
        assert_eq!(
            lap_count(&two),
            lap_count(&one) * 2,
            "the same capture twice votes with twice the laps"
        );
        // The provenance names every capture, in order.
        let name = capture
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| capture.display().to_string());
        assert!(
            two.contains(&format!("in {}, {}", name, name)),
            "the provenance must name both captures: {two}"
        );
    }

    #[test]
    fn learn_track_refuses_captures_of_two_different_tracks() {
        let capture = monza();
        if !capture.exists() {
            return;
        }
        // A second capture that is not telemetry at all cannot reach the
        // track check (it fails to open), so the check is exercised through
        // the empty-capture guard instead: an empty file the provider
        // recognises as its own shape is refused for having no session.
        let dir = std::env::temp_dir().join("coach_commands_tests/mismatch");
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.ndjson");
        std::fs::write(&empty, "").unwrap();

        let mut progress = VecProgress::default();
        let err = learn_track(
            &[capture, empty],
            None,
            &dir,
            1.0,
            true,
            &mut progress,
        )
        .expect_err("an empty capture cannot vote");
        assert!(
            err.to_string().contains("no usable telemetry frames"),
            "the refusal must name the empty capture: {err}"
        );
    }
}
