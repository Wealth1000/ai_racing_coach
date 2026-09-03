//! `coach` — command-line entry point.
//!
//! Deliberately thin: parse arguments, drive the library, print. Every piece of
//! analysis lives in the library so that tests can reach it, which the previous
//! version could not do — the crate had no lib target, so `main.rs` was the only
//! place code could live and nothing in it was testable.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use clap::{Parser, Subcommand};

use ai_racing_coach::audio::FeedbackSink;
use ai_racing_coach::coaching::{ControllerMode, DefaultPhraser, advise_pass};
use ai_racing_coach::core::config::{CoachConfig, InputDevice};
use ai_racing_coach::core::sample::Sample;
use ai_racing_coach::features::FeatureParams;
use ai_racing_coach::features::ReferenceStore;
use ai_racing_coach::features::corner::{self, TrackCorner};
use ai_racing_coach::features::corner_features;
use ai_racing_coach::features::curvature;
use ai_racing_coach::features::lap::{Lap, LapTracker};
use ai_racing_coach::features::resample::{self, ResampledLap};
use ai_racing_coach::features::track_model::{LearnParams, ModelCorner, TrackModel};
use ai_racing_coach::models::rules::RuleModel;
use ai_racing_coach::runtime;
use ai_racing_coach::sims;
use ai_racing_coach::telemetry::{PrefixedSource, TelemetrySource};

#[derive(Parser)]
#[command(
    name = "coach",
    about = "AI sim racing coach — Assetto Corsa telemetry interpreter",
    version,
    // The generated help lists flags but never shows them combined. Kept on
    // `after_help` rather than `after_long_help` so short `-h` shows it too —
    // `-h` is what anyone actually types.
    after_help = "\
Examples:
  coach inspect telemetry_ac.ndjson         analyse the fastest clean lap
  coach inspect capture.ndjson.gz           gzipped captures work directly
  coach inspect capture.ndjson --all-laps   one corner table per clean lap
  coach inspect capture.ndjson --step 0.5   finer distance grid (default 1 m)

  coach learn-track capture.ndjson.gz       write data/tracks/<track>.json
  coach learn-track capture.ndjson --dry-run   show the model, write nothing

  coach analyse capture.ndjson.gz           how the fastest lap drove each
                                            corner of the learned model
  coach analyse capture.ndjson --all-laps   feature table for every clean lap

  coach learn-pb capture.ndjson.gz          record your best pass per corner
  coach learn-pb capture.ndjson --dry-run   show the bests, write nothing

  coach live --replay capture.ndjson.gz     run the whole pipeline live off a
                                            capture: advice as corners complete
  coach live                                attach to the running sim instead
                                            (Windows): wait, announce, coach
  coach record --laps 3                     capture the running sim's telemetry
                                            (Windows), in the logger's format

Run `coach help <command>` for the full description of a command."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Parse a flag that must be a positive, finite number of metres.
///
/// Argument validation belongs here rather than in [`ai_racing_coach::CoachError`]
/// for three reasons. It fails before any file is opened, instead of after a
/// capture has been decompressed and split into laps. It produces a message that
/// names the flag and prints usage, which `CoachError` cannot do because it knows
/// nothing about the command line. And `CoachError`'s own module docs say every
/// variant names *the field that went wrong* in the telemetry — reusing
/// `ImplausibleValue` for a typo in a flag tacks "the capture is corrupt, or the
/// logger's struct layout no longer matches the sim's shared memory" onto what is
/// simply a mistyped number.
///
/// `> 0.0` is false for `NaN`, so this rejects `--step nan` without a separate
/// check — which matters more than it looks: a NaN `--apex-tolerance` would make
/// every support comparison false and silently produce a model with no corners
/// at all.
fn positive_metres(s: &str) -> std::result::Result<f32, String> {
    let value: f32 = s.parse().map_err(|_| "not a number".to_string())?;
    if value > 0.0 && value.is_finite() {
        Ok(value)
    } else {
        Err("must be a positive distance in metres".to_string())
    }
}

#[derive(Subcommand)]
enum Command {
    /// Read a capture and report what is in it: session, laps, corners.
    Inspect {
        /// An `.ndjson` or `.ndjson.gz` capture from the logger.
        capture: PathBuf,
        /// Which simulator's provider to open the capture, by key (e.g. "ac").
        /// Omit to offer the file to every registered provider and use the
        /// first that recognises it.
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,


        /// Distance-grid spacing in metres.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// Show the corner table for every clean lap, not just the fastest.
        #[arg(long)]
        all_laps: bool,
    },

    /// Learn the canonical corner set for a track and save it to disk.
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
    LearnTrack {
        /// An `.ndjson` or `.ndjson.gz` capture from the logger.
        capture: PathBuf,
        /// Which simulator's provider to open the capture, by key (e.g. "ac").
        /// Omit to offer the file to every registered provider and use the
        /// first that recognises it.
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,


        /// Directory to write `<track>_<layout>.json` into.
        #[arg(long, default_value = "data/tracks")]
        out: PathBuf,

        /// Distance-grid spacing in metres.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// Print the model without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Describe how a capture's clean laps drove each corner of a track model.
    ///
    /// The model supplies *where* the corners are, learned once per track and
    /// car; this command reports *what each lap did inside them* — turn-in
    /// speed, apex speed and where it sat relative to the geometric apex,
    /// braking point, trail braking, throttle pickup. Slicing at the model's
    /// boundaries rather than per-lap detections is what makes two laps'
    /// numbers comparable.
    Analyse {
        /// An `.ndjson` or `.ndjson.gz` capture from the logger.
        capture: PathBuf,
        /// Which simulator's provider to open the capture, by key (e.g. "ac").
        /// Omit to offer the file to every registered provider and use the
        /// first that recognises it.
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,


        /// Directory holding `<track>_<layout>.json` models.
        #[arg(long, default_value = "data/tracks")]
        model_dir: PathBuf,

        /// Distance-grid spacing in metres.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// Show feature tables for every clean lap, not just the fastest.
        #[arg(long)]
        all_laps: bool,
    },

    /// Record the driver's best pass through each corner as a personal best.
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
    LearnPb {
        /// An `.ndjson` or `.ndjson.gz` capture from the logger.
        capture: PathBuf,
        /// Which simulator's provider to open the capture, by key (e.g. "ac").
        /// Omit to offer the file to every registered provider and use the
        /// first that recognises it.
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,


        /// Directory holding `<track>_<layout>.json` models.
        #[arg(long, default_value = "data/tracks")]
        model_dir: PathBuf,

        /// Distance-grid spacing in metres.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// Print the result without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run the full pipeline live off a telemetry source.
    ///
    /// With `--replay`, a capture streams through the same analysis
    /// `coach analyse` does after the fact, but one at a time, with advice
    /// printed the moment each corner pass completes. Without it, the coach
    /// attaches to the running sim (Assetto Corsa's shared-memory pages on
    /// Windows): it waits for the sim to start, announces the stream when
    /// telemetry flows, and coaches from the first frame.
    Live {
        /// Stream this capture through the live pipeline. Omit to attach to
        /// the running sim instead.
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Which simulator's provider to open the capture, by key (e.g. "ac").
        /// Omit to offer the file to every registered provider and use the
        /// first that recognises it.
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,


        /// Directory holding `<track>_<layout>.json` models and
        /// `<track>_<layout>_pb.json` personal bests.
        #[arg(long, default_value = "data/tracks")]
        model_dir: PathBuf,

        /// Distance-grid spacing in metres. The model was learned at a
        /// specific step; a different value here moves grid indices and
        /// therefore every measured number.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// How advice is delivered: the OS synthesiser (degrades to counted
        /// silence when no speech backend exists) or nothing at all.
        #[arg(long, value_enum, default_value_t = VoiceChoice::Tts)]
        voice: VoiceChoice,

        /// Write the session — lap boundaries, corner passes, delivered
        /// advice — to `<dir>/<session-id>.ndjson` as it happens.
        #[arg(long, value_name = "DIR")]
        record_session: Option<PathBuf>,
    },

    /// Capture telemetry live from the running sim.
    ///
    /// The C# logger's job, done by the coach itself — one program on the sim
    /// machine instead of two. What it writes is the logger's own NDJSON,
    /// key for key, so `coach inspect`, `learn-track` and everything else
    /// cannot tell a `coach record` capture from a logger one. Waits for the
    /// sim to start, then records until Ctrl-C or `--laps` laps.
    Record {
        /// Write to this file instead of the logger's default
        /// `telemetry_ac_<track>_<car>_<stamp>.ndjson.gz` in the working
        /// directory. Never overwrites an existing file.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,

        /// Stop after this many laps have been recorded.
        #[arg(long)]
        laps: Option<u32>,

        /// Write plain NDJSON instead of gzip.
        #[arg(long)]
        plain: bool,

        /// Which simulator's provider to record from, by key (e.g. "ac").
        /// Omit to let the single registered provider be selected.
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,
    },

    /// Export a directory of recorded sessions as one CSV row per corner pass.
    ///
    /// The dataset is the corpus offline analysis learns from: every pass
    /// from every session, joined with the track model's corners and any
    /// personal best, with the outcome flags (clean lap, off-track points,
    /// advice delivered) attached.
    ExportDataset {
        /// Directory of `<session-id>.ndjson` files written by
        /// `coach live --record-session`.
        sessions_dir: PathBuf,

        /// The CSV file to write.
        out: PathBuf,

        /// Directory holding `<track>_<layout>.json` models and
        /// `<track>_<layout>_pb.json` personal bests.
        #[arg(long, default_value = "data/tracks")]
        model_dir: PathBuf,
    },

    /// Open the coaching window: pick a sim, wait for the car, get coached.
    ///
    /// Without `--replay` the window starts at a sim picker; picking one
    /// shows a waiting screen until the car is on track, the stream is
    /// announced in text and voice, and coaching begins. With `--replay`, a
    /// capture streams through the same live pipeline immediately — the same
    /// live session as `coach live --replay`, with a window for its consumer.
    Gui {
        /// Stream this capture through the live pipeline. Omit to pick a
        /// sim and attach to it live instead.
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Which simulator to use, by key (e.g. "ac"): with `--replay`, the
        /// provider that opens the capture; without it, the sim the window
        /// waits for (skipping the picker).
        #[arg(long, value_name = "KEY")]
        sim: Option<String>,


        /// Directory holding `<track>_<layout>.json` models and
        /// `<track>_<layout>_pb.json` personal bests.
        #[arg(long, default_value = "data/tracks")]
        model_dir: PathBuf,

        /// Distance-grid spacing in metres. The model was learned at a
        /// specific step; a different value here moves grid indices and
        /// therefore every measured number.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Inspect {
            capture,
            sim,
            step,
            all_laps,
        } => inspect(&capture, sim.as_deref(), step, all_laps),
        Command::LearnTrack {
            capture,
            sim,
            out,
            step,
            dry_run,
        } => learn_track(&capture, sim.as_deref(), &out, step, dry_run),
        Command::Analyse {
            capture,
            sim,
            model_dir,
            step,
            all_laps,
        } => analyse(&capture, sim.as_deref(), &model_dir, step, all_laps),
        Command::LearnPb {
            capture,
            sim,
            model_dir,
            step,
            dry_run,
        } => learn_pb(&capture, sim.as_deref(), &model_dir, step, dry_run),
        Command::Live {
            replay,
            sim,
            model_dir,
            step,
            voice,
            record_session,
        } => live(
            replay.as_deref(),
            &model_dir,
            step,
            sim.as_deref(),
            voice,
            record_session.as_deref(),
        ),
        Command::Record {
            out,
            laps,
            plain,
            sim,
        } => record(out.as_deref(), laps, plain, sim.as_deref()),
        Command::ExportDataset {
            sessions_dir,
            out,
            model_dir,
        } => export_dataset(&sessions_dir, &out, &model_dir),
        Command::Gui {
            replay,
            sim,
            model_dir,
            step,
        } => gui(replay.as_deref(), &model_dir, step, sim.as_deref()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            // thiserror chains the cause; show it, since "invalid type: string"
            // is useless without the line number that produced it.
            let mut source = std::error::Error::source(&e);
            while let Some(s) = source {
                eprintln!("  caused by: {s}");
                source = s.source();
            }
            ExitCode::FAILURE
        }
    }
}

/// Open a capture and split it into laps.
///
/// Shared by every subcommand: reading samples, honouring the provider's
/// verdict on the capture and finding lap boundaries is the same work
/// regardless of what is done with the laps afterwards. The source is
/// returned alongside them because it owns the
/// [`ai_racing_coach::core::SessionInfo`] and the read statistics.
///
/// Takes no grid spacing: laps here are raw samples at the source's own rate.
/// Resampling onto a distance grid happens per-lap in the callers.
fn read_laps(
    capture: &Path,
    sim: Option<&str>,
) -> ai_racing_coach::Result<(Box<dyn TelemetrySource>, Vec<Lap>)> {
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

fn inspect(
    capture: &Path,
    sim: Option<&str>,
    step: f32,
    all_laps: bool,
) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    println!("{}", source.describe());

    println!();
    print_source_stats(&*source);
    println!();
    print_lap_table(&laps);

    let clean: Vec<&Lap> = laps.iter().filter(|l| l.quality.is_clean()).collect();
    if clean.is_empty() {
        println!("\nNo clean laps — nothing to analyse.");
        return Ok(());
    }

    // Fastest clean lap by default; the rest on request.
    let mut ordered = clean.clone();
    ordered.sort_by(|a, b| a.lap_time_s().total_cmp(&b.lap_time_s()));
    let chosen: &[&Lap] = if all_laps { &ordered } else { &ordered[..1] };

    for lap in chosen {
        println!();
        analyse_lap(lap, step);
    }

    Ok(())
}

/// `coach learn-track` — build the canonical corner set and save it.
fn learn_track(
    capture: &Path,
    sim: Option<&str>,
    out_dir: &Path,
    step: f32,
    dry_run: bool,
) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    println!("{}", source.describe());

    let session = source
        .session()
        .ok_or_else(|| ai_racing_coach::CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let params = LearnParams { step_m: step };
    let model = TrackModel::learn(session, &laps, &capture.display().to_string(), &params)?;

    println!();
    print_model(&model);

    let path = TrackModel::path_in(out_dir, model.sim, &model.track);

    // The file name keys on track and layout only, so learning the same circuit
    // in a second car lands on this same path. That is not a mistake — a model
    // is per-car by construction (see the track_model module docs) and there is
    // no car-independent answer to fall back on — but it must not be silent,
    // because the two cars genuinely disagree about the corner count and the
    // last command run would otherwise decide the model with no trace.
    if let Ok(existing) = TrackModel::load(&path) {
        println!("\nReplacing the model at {}", path.display());
        println!(
            "  was: {} corners from {} lap(s) of {}",
            existing.corners.len(),
            existing.lap_count(),
            existing.provenance.car,
        );
        println!(
            "  now: {} corners from {} lap(s) of {}",
            model.corners.len(),
            model.lap_count(),
            model.provenance.car,
        );
        if existing.provenance.car != model.provenance.car {
            println!(
                "  note: different car — boundaries shift with speed, so this is a \
                 different model of the same circuit, not a correction of the old one"
            );
        }
    }

    if dry_run {
        println!("\n--dry-run: nothing written (would be {})", path.display());
        return Ok(());
    }

    // The sim's directory may not exist yet — learning the first model of a
    // newly added sim is exactly when it does not — but `save` creates parent
    // directories itself, so there is nothing to do here.
    model.save(&path)?;
    println!("\nWrote {}", path.display());
    Ok(())
}

fn print_model(model: &TrackModel) {
    let (left, right) = model.direction_counts();
    println!(
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
    );
    if !model.provenance.pedal_events {
        println!("  no usable pedal channels: decision events were not learned");
    }

    println!(
        "\n  {:>4}  {:>3}  {:>8}  {:>8}  {:>7}  {:>8}  {:>7}  {:>8}  {:>7}  {:>6}",
        "turn", "dir", "start", "end", "length", "apex", "radius", "turn", "laps", "events"
    );
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
        println!(
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
        );
    }
    print_unanimity(&model.corners, model.lap_count());

    // Confirmed decision boundaries, once per corner, in the driver's terms.
    let eventful: Vec<&ModelCorner> = model
        .corners
        .iter()
        .filter(|c| !c.decision_events.is_empty())
        .collect();
    if !eventful.is_empty() {
        println!("\n  decision events:");
        for c in eventful {
            let events: Vec<String> = c
                .decision_events
                .iter()
                .map(|e| format!("{} {:.0}m ({})", e.kind.name(), e.distance_m, e.support))
                .collect();
            println!("    turn {}: {}", c.id, events.join(", "));
        }
    }
}

/// Flag the corners not every lap found. These are where the model is least
/// certain, and they are the first thing to check when it looks wrong.
///
/// Both halves of a line-straddling corner report the same numbers, so the
/// parent row alone is listed.
fn print_unanimity(corners: &[ModelCorner], laps: u32) {
    let split: Vec<&ModelCorner> = corners
        .iter()
        .filter(|c| c.parent_id.is_none() && c.support < laps)
        .collect();
    if split.is_empty() {
        println!("\n  all {laps} laps agreed on every corner");
        return;
    }
    let names: Vec<String> = split
        .iter()
        .map(|c| format!("{} ({}/{}, {:.0}%)", c.id, c.support, laps, c.match_fraction * 100.0))
        .collect();
    println!("\n  not unanimous: {}", names.join(", "));
}

/// `coach analyse` — per-corner driving numbers against a learned model.
///
/// The model supplies *where* the corners are; extraction reports *what each
/// clean lap did inside them*. Only clean laps are analysed, for the same
/// reason [`TrackModel::learn`] only lets clean laps vote: a spin's numbers
/// are facts about the spin, not about how the corner is driven.
fn analyse(
    capture: &Path,
    sim: Option<&str>,
    model_dir: &Path,
    step: f32,
    all_laps: bool,
) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    println!("{}", source.describe());

    let session = source
        .session()
        .ok_or_else(|| ai_racing_coach::CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let model = runtime::load_model_for_session(session, model_dir)?;
    let reference = runtime::load_reference_for_session(session, &model, model_dir);

    println!();
    println!(
        "Model {} — {} corners learned from {} lap(s) of {}",
        model.track,
        model.corners.len(),
        model.lap_count(),
        model.provenance.car,
    );

    let clean: Vec<&Lap> = laps.iter().filter(|l| l.quality.is_clean()).collect();
    if clean.is_empty() {
        println!("\nNo clean laps — nothing to analyse.");
        return Ok(());
    }

    // Fastest clean lap by default, matching `inspect`.
    let mut ordered = clean.clone();
    ordered.sort_by(|a, b| a.lap_time_s().total_cmp(&b.lap_time_s()));
    let chosen: &[&Lap] = if all_laps { &ordered } else { &ordered[..1] };

    let params = FeatureParams::default();

    for lap in chosen {
        println!();
        let Some(grid) = resample::resample_lap(&lap.samples, step) else {
            println!(
                "lap {}: not enough distinct positions to resample",
                lap.id.0
            );
            continue;
        };
        let features = corner_features::extract_all(&model, &grid, &params, lap.id);
        if features.is_empty() {
            println!(
                "lap {} ({:.2}s): no model corner is fully covered by this lap",
                lap.id.0,
                lap.lap_time_s()
            );
            continue;
        }
        println!(
            "lap {} — {:.2}s, {} corners driven",
            lap.id.0,
            lap.lap_time_s(),
            features.len()
        );
        print_feature_table(&features);
        print_advice(&model, &reference, &features);
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
    features: &[ai_racing_coach::features::CornerFeatures],
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
        println!("\n  advice: nothing the rules would raise for this lap");
    } else {
        println!("\n  advice (unthrottled — everything the rules raise):");
        for line in lines {
            println!("    {line}");
        }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
enum VoiceChoice {
    /// The OS synthesiser, via the `tts` crate.
    Tts,
    /// Compute and print everything, speak nothing. The CI voice.
    Null,
}

impl From<VoiceChoice> for ai_racing_coach::core::VoiceConfig {
    fn from(choice: VoiceChoice) -> Self {
        Self {
            backend: match choice {
                VoiceChoice::Tts => ai_racing_coach::core::VoiceBackend::Tts,
                VoiceChoice::Null => ai_racing_coach::core::VoiceBackend::Null,
            },
            rate: 1.0,
        }
    }
}

/// `coach live` — the whole pipeline, running as if it were happening now.
///
/// The source is a capture with `--replay`, or the running sim without it.
/// Either way the first sample must be read before anything else: the session
/// it carries decides which model (and personal best) to load, and the
/// pipeline cannot be built without them. That sample is then handed back to
/// the stream through [`PrefixedSource`], so the pipeline still sees every
/// sample exactly once.
///
/// A live attach announces itself — "Assetto Corsa stream picked up", printed
/// and spoken — because the driver picked the sim minutes before the stream
/// existed and deserves to hear that the waiting is over.
///
/// What prints here is the advice the decision layer *delivers* — cooldowns
/// and repetition suppression apply, on a clock driven by the source's own
/// timestamps so a replay throttles itself exactly as the same drive would
/// live. `coach analyse` prints the unthrottled set for comparison.
///
/// With `--record-session`, the session is written to disk as it happens:
/// lap boundaries and corner passes from the event channel, delivered advice
/// (with the drop/skip counters at that moment) through the session writer,
/// which is a `FeedbackSink` like the voice.
fn live(
    replay: Option<&Path>,
    model_dir: &Path,
    step: f32,
    sim: Option<&str>,
    voice: VoiceChoice,
    record_session: Option<&Path>,
) -> ai_racing_coach::Result<()> {
    // The source, and how the config names where its samples come from. A
    // live attach never fails here: the source starts in its waiting state
    // and the first `next_sample` below holds until the sim runs, saying why
    // once per reason — "the sim is not running yet" is the first phase of a
    // live session, not an error.
    let (mut source, input, sim_name) = match replay {
        Some(capture) => (
            sims::open_capture(capture, sim)?,
            InputDevice::Replay {
                capture: capture.to_path_buf(),
            },
            None,
        ),
        None => {
            let providers: Vec<&dyn sims::SimProvider> =
                sims::registry().iter().map(|p| p.as_ref()).collect();
            let provider = sims::provider_for_live(&providers, sim)?;
            (provider.live()?, InputDevice::SharedMemory, Some(provider.name()))
        }
    };
    let first = source.next_sample()?.ok_or_else(|| match replay {
        Some(capture) => ai_racing_coach::CoachError::EmptyCapture {
            path: capture.display().to_string(),
        },
        None => ai_racing_coach::CoachError::NotEnoughData {
            action: "coach a live session",
            detail: "the sim's stream ended before a single sample".to_string(),
        },
    })?;
    // Cloned, not borrowed: the source moves into the wiring below, and the
    // session facts outlive it — the recorder quotes them in its header.
    let session = source.session()
        .ok_or_else(|| match replay {
            Some(capture) => ai_racing_coach::CoachError::EmptyCapture {
                path: capture.display().to_string(),
            },
            None => ai_racing_coach::CoachError::NotEnoughData {
                action: "coach a live session",
                detail: "the stream carried no session (track and car)".to_string(),
            },
        })?
        .clone();

    println!("{}", source.describe());

    let model = runtime::load_model_for_session(&session, model_dir)?;
    println!();
    println!(
        "Model {} — {} corners learned from {} lap(s) of {}",
        model.track,
        model.corners.len(),
        model.lap_count(),
        model.provenance.car,
    );
    let reference = runtime::load_reference_for_session(&session, &model, model_dir);

    let voice_config: ai_racing_coach::core::VoiceConfig = voice.into();
    let config = CoachConfig {
        input,
        step_m: step,
        models_dir: model_dir.to_path_buf(),
        voice: voice_config,
    };
    // Captured before the model moves into the pipeline: the recorder needs
    // the fingerprint to stamp its header, and after `CoachPipeline::new` the
    // model is gone.
    let model_fingerprint = model.fingerprint();
    let pipeline = runtime::CoachPipeline::new(model, reference, config);
    let wiring = runtime::spawn(
        Box::new(PrefixedSource::new(first, source)),
        pipeline,
    );

    // The sinks hear what the terminal sees: the same delivered advice, in
    // the same order. `--voice null` keeps the whole session silent (CI);
    // `--voice tts` skips lines the synth cannot take rather than queueing
    // them — a late braking tip is worse than a missed one.
    // One counter shared by the voice and the recorder, so a session file's
    // `voice_skipped` quotes the same number the driver heard (or didn't).
    let voice_skipped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut sinks: Vec<Box<dyn ai_racing_coach::audio::FeedbackSink>> = match voice {
        VoiceChoice::Tts => vec![Box::new(
            ai_racing_coach::audio::TtsSink::connect(voice_skipped.clone()),
        )],
        VoiceChoice::Null => Vec::new(),
    };

    // The session recorder, when asked for. It is a `FeedbackSink` like the
    // voice (advice in through `deliver`), plus an event consumer for lap
    // boundaries and passes; it shares the wiring's counters so each advice
    // record quotes them at the moment of delivery.
    let mut writer = match record_session {
        Some(dir) => {
            let id = ai_racing_coach::core::SessionId::generate();
            let counters = ai_racing_coach::storage::SessionCounters {
                dropped_frames: wiring.dropped_frames.clone(),
                dropped_advice: wiring.dropped_advice.clone(),
                voice_skipped: voice_skipped.clone(),
            };
            let mut writer = ai_racing_coach::storage::SessionWriter::create(dir, &id, counters)?;
            writer.write_header(&ai_racing_coach::storage::SessionHeader {
                session_id: id.clone(),
                sim: session.sim,
                track: session.track.clone(),
                car: session.car.clone(),
                model_fingerprint,
                step_m: step,
                started_at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            })?;
            println!("Recording session {id}");
            Some((id, writer))
        }
        None => None,
    };

    // The live pickup announcement. A replay begins the moment it is launched
    // and its first lines say so; a live attach spent an unknown time
    // waiting for the sim, and the driver was probably elsewhere — say the
    // wait is over, in text and in voice.
    if let Some(sim_name) = sim_name {
        let announcement = format!("{sim_name} stream picked up");
        println!("{announcement}");
        for sink in &mut sinks {
            sink.say(&announcement);
        }
    }

    println!();
    let mut spoken = 0u64;
    let mut recorded_events = 0u64;
    loop {
        // Events first: the pipeline guarantees a pass record is queued no
        // later than the advice it produced, so this ordering keeps the
        // session file self-consistent. When not recording, the drain is
        // what keeps the event channel from filling and dropping.
        while let Ok(event) = wiring.event_rx.try_recv() {
            if let Some((_, w)) = writer.as_mut() {
                w.write_event(&session_event(event))?;
                recorded_events += 1;
            }
        }
        match wiring.advice_rx.recv() {
            Ok(advice) => {
                spoken += 1;
                println!("  {}", advice.phrased);
                for sink in &mut sinks {
                    sink.deliver(&advice)?;
                }
                if let Some((_, w)) = writer.as_mut() {
                    w.deliver(&advice)?;
                }
            }
            Err(_) => break,
        }
    }
    // The pipeline thread is done; drain whatever events it queued last.
    while let Ok(event) = wiring.event_rx.try_recv() {
        if let Some((_, w)) = writer.as_mut() {
            w.write_event(&session_event(event))?;
            recorded_events += 1;
        }
    }
    for sink in &mut sinks {
        sink.flush();
    }
    if let Some((_, w)) = writer.as_mut() {
        w.flush();
    }
    let dropped_frames = wiring.dropped_frames.load(Ordering::Relaxed);
    let dropped_advice = wiring.dropped_advice.load(Ordering::Relaxed);
    let dropped_events = wiring.dropped_events.load(Ordering::Relaxed);
    wiring.join()?;

    if dropped_advice > 0 {
        eprintln!(
            "warning: {dropped_advice} advice dropped — the consumer could not keep up"
        );
    }
    if dropped_events > 0 {
        eprintln!("warning: {dropped_events} session events dropped");
    }
    if let Some((id, _)) = writer.take() {
        println!("Session {id}: {recorded_events} events recorded");
    }
    println!("\n{spoken} advice, {dropped_frames} frames dropped");
    Ok(())
}

/// `coach record` — the C# logger's job: capture live telemetry from the
/// running sim, in the logger's own NDJSON.
///
/// Thin by design — the waiting, polling, skip accounting and lap counting
/// live in the provider (`sims::assetto_corsa::record`), where a scripted
/// fake page store can test every path; all this does is pick the provider
/// and report what the recorder saw.
fn record(
    out: Option<&Path>,
    laps: Option<u32>,
    plain: bool,
    sim: Option<&str>,
) -> ai_racing_coach::Result<()> {
    let providers: Vec<&dyn sims::SimProvider> =
        sims::registry().iter().map(|p| p.as_ref()).collect();
    let provider = sims::provider_for_live(&providers, sim)?;
    let opts = sims::RecordOptions {
        out: out.map(Path::to_path_buf),
        laps,
        plain,
    };
    let summary = provider.record(&opts)?;

    // A recording that never saw the car on track never resolved a file to
    // write — that is worth saying plainly rather than reporting zero frames
    // written to nowhere.
    match &summary.path {
        Some(path) => println!(
            "Recorded {} frames ({}) to {}",
            summary.frames,
            match summary.laps_completed {
                0 => "no laps completed".to_string(),
                n => format!("{n} lap(s)"),
            },
            path.display()
        ),
        None => println!("No frames recorded — the sim never published a session"),
    }
    let skipped = summary.skipped_no_position + summary.skipped_duplicate + summary.skipped_no_session;
    if skipped > 0 {
        println!(
            "Skipped {skipped} polls: {} before the car was on track, {} duplicates, {} without a session",
            summary.skipped_no_position, summary.skipped_duplicate, summary.skipped_no_session
        );
    }
    Ok(())
}

/// `coach gui` — the coaching window.
///
/// Without `--replay`, the window opens at the sim picker: pick one and it
/// waits ("Waiting, when you are on track in …") while a background thread
/// attaches, reads the first sample, loads the model and spawns the
/// pipeline; the pickup is announced in text and voice and the session
/// window takes over. With `--replay`, the session exists before the window
/// does and it starts coaching immediately — the same wiring as `coach live
/// --replay`, with a window for its consumer.
///
/// Either way, closing the window stops the session and joins the threads,
/// so the process exits clean.
fn gui(
    replay: Option<&Path>,
    model_dir: &Path,
    step: f32,
    sim: Option<&str>,
) -> ai_racing_coach::Result<()> {
    let app: Box<dyn eframe::App> = match replay {
        Some(capture) => {
            let mut source = sims::open_capture(capture, sim)?;
            let first = source.next_sample()?.ok_or_else(|| {
                ai_racing_coach::CoachError::EmptyCapture {
                    path: capture.display().to_string(),
                }
            })?;
            let session = source
                .session()
                .ok_or_else(|| {
                    ai_racing_coach::CoachError::EmptyCapture {
                        path: capture.display().to_string(),
                    }
                })?
                .clone();

            let model = runtime::load_model_for_session(&session, model_dir)?;
            let reference = runtime::load_reference_for_session(&session, &model, model_dir);

            let config = CoachConfig {
                input: InputDevice::Replay {
                    capture: capture.to_path_buf(),
                },
                step_m: step,
                models_dir: model_dir.to_path_buf(),
                voice: Default::default(),
            };
            let pipeline = runtime::CoachPipeline::new(model, reference, config);
            // The connection indicator's text, captured before the source
            // moves into the wiring — the UI thread never sees the source
            // itself, only what it calls itself.
            let source_desc = source.describe();
            let wiring =
                runtime::spawn(Box::new(PrefixedSource::new(first, source)), pipeline);

            // The GUI speaks the advice; the terminal does not echo it.
            let voice_skipped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let app = ai_racing_coach::ui::CoachApp::with_sink(
                wiring,
                source_desc,
                Box::new(ai_racing_coach::audio::TtsSink::connect(voice_skipped)),
            );
            Box::new(ai_racing_coach::ui::CoachGui::live(app))
        }
        None => {
            let mut gui = ai_racing_coach::ui::CoachGui::new(model_dir.to_path_buf(), step);
            // `--sim` answers the picker's question before the window opens.
            if let Some(key) = sim
                && !gui.wait_for(key)
            {
                return Err(ai_racing_coach::CoachError::UnknownSim {
                    key: key.to_string(),
                    known: sims::registry()
                        .iter()
                        .map(|p| p.key())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
            Box::new(gui)
        }
    };

    // The window-manager icon (taskbar/alt-tab on Linux too); the Windows exe
    // additionally carries the same art as an embedded .ico resource via
    // build.rs. A failed decode means no icon, not no window.
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([720.0, 480.0])
        .with_min_inner_size([480.0, 240.0]);
    if let Some(icon) = ai_racing_coach::ui::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "AI Racing Coach",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(move |_cc| Ok(app)),
    )
    .map_err(|e| ai_racing_coach::CoachError::Ui {
        detail: e.to_string(),
    })
}

/// Translate a pipeline event into its session-file record. The pipeline
/// speaks runtime terms; the session file speaks storage terms; this is the
/// only place they meet, because the mapping is trivially structural —
/// except that advice never appears here (it travels the advice channel,
/// where the sinks can hear it).
fn session_event(event: runtime::RuntimeEvent) -> ai_racing_coach::storage::SessionEvent {
    use ai_racing_coach::storage::SessionEvent;
    match event {
        runtime::RuntimeEvent::LapBoundary { lap, time_s, clean } => {
            SessionEvent::LapBoundary { lap, time_s, clean }
        }
        runtime::RuntimeEvent::Pass(f) => SessionEvent::Pass(f),
    }
}

/// `coach export-dataset` — flatten recorded sessions into one CSV row per
/// corner pass.
///
/// The model and personal best are selected the same way `live` selects
/// them, from the session header's own track and car, so an export joins
/// exactly the corner set the session was coached against. A session
/// recorded against a different fingerprint of the model is refused rather
/// than mis-joined.
fn export_dataset(sessions_dir: &Path, out: &Path, model_dir: &Path) -> ai_racing_coach::Result<()> {
    let mut sessions: Vec<PathBuf> = std::fs::read_dir(sessions_dir)
        .map_err(|e| ai_racing_coach::CoachError::Io {
            path: sessions_dir.display().to_string(),
            source: e,
        })?
        .map_while(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ndjson"))
        .collect();
    sessions.sort();
    if sessions.is_empty() {
        return Err(ai_racing_coach::CoachError::NotEnoughData {
            action: "export a dataset",
            detail: format!(
                "no .ndjson session files in {} — record one with `coach live --record-session`",
                sessions_dir.display()
            ),
        });
    }

    // The first session's header decides which model (and personal best)
    // every session is joined against; the fingerprint check inside the
    // exporter then holds the rest to it.
    let first = ai_racing_coach::storage::read_session(&sessions[0])?;
    let model_path = TrackModel::path_in(model_dir, first.header.sim, &first.header.track);
    if !model_path.exists() {
        return Err(ai_racing_coach::CoachError::NotEnoughData {
            action: "export a dataset",
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
            eprintln!(
                "warning: the personal best at {} was recorded for a different car or an \
                 earlier model of this track — exporting without reference columns",
                pb_path.display()
            );
            None
        }
        Err(_) => None,
    };

    let info = ai_racing_coach::storage::export_dataset(
        &sessions,
        &model,
        reference.as_ref(),
        out,
    )?;
    println!(
        "{} rows, {} columns — {}",
        info.rows,
        info.columns,
        out.display()
    );
    Ok(())
}

/// One row per corner: speeds in km/h, distances in metres relative to the
/// corner they belong to, everything a rule will compare between laps.
fn print_feature_table(features: &[ai_racing_coach::features::CornerFeatures]) {
    let kmh = |mps: f32| format!("{:>5.0}", mps * 3.6);

    println!(
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
    );
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

        println!(
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
        );
    }
}

/// `coach learn-pb` — record the best pass through each corner.
///
/// Clean laps only, for the same reason as everywhere else: a spin through
/// T7 is a fact about the spin, and a personal best is not allowed to be one.
fn learn_pb(
    capture: &Path,
    sim: Option<&str>,
    model_dir: &Path,
    step: f32,
    dry_run: bool,
) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture, sim)?;
    println!("{}", source.describe());

    let session = source
        .session()
        .ok_or_else(|| ai_racing_coach::CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let model = runtime::load_model_for_session(session, model_dir)?;

    let mut grids: Vec<(ai_racing_coach::core::ids::LapId, ResampledLap)> = Vec::new();
    let mut unresampled = 0usize;
    for lap in laps.iter().filter(|l| l.quality.is_clean()) {
        match resample::resample_lap(&lap.samples, step) {
            Some(grid) => grids.push((lap.id, grid)),
            None => unresampled += 1,
        }
    }
    if unresampled > 0 {
        println!(
            "\n{unresampled} clean lap(s) could not be put on the {step} m grid and were skipped"
        );
    }
    if grids.is_empty() {
        return Err(ai_racing_coach::CoachError::NotEnoughData {
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
        return Err(ai_racing_coach::CoachError::NotEnoughData {
            action: "record personal bests",
            detail: format!(
                "none of the {} model corners was fully covered by any clean lap",
                model.corners.len()
            ),
        });
    }

    let path = ReferenceStore::path_in(model_dir, session.sim, &session.track);

    println!();
    let store = if path.exists() {
        let existing = ReferenceStore::load(&path)?;
        if existing.compatible_with(session.sim, &session.car, model.fingerprint()) {
            let mut merged = existing;
            let report = merged.absorb(incoming);
            println!("Merging into the existing personal best at {}:", path.display());
            println!(
                "  {} corner(s) improved, {} kept, {} added",
                report.improved, report.kept, report.added
            );
            merged
        } else {
            println!("Existing personal best at {} cannot be merged:", path.display());
            if existing.provenance.car != session.car {
                println!(
                    "  it was recorded in a {}, this capture is a {} — per-car numbers",
                    existing.provenance.car, session.car
                );
            }
            if existing.model_fingerprint != model.fingerprint() {
                println!(
                    "  the track model has been re-learned since; the stored corner \
                     ordinals no longer mean the same places"
                );
            }
            println!("  starting fresh from this capture");
            incoming
        }
    } else {
        incoming
    };

    print_pb_table(&store);

    if dry_run {
        println!("\n--dry-run: nothing written (would be {})", path.display());
        return Ok(());
    }

    store.save(&path)?;
    println!("\nWrote {}", path.display());
    Ok(())
}

/// The personal best, one row per corner with the same conventions as
/// `analyse`: speeds in km/h, distances signed relative to the boundary or
/// apex they are measured from.
fn print_pb_table(store: &ReferenceStore) {
    println!(
        "\nPersonal best — {}, {}, {} corner(s) recorded from {}",
        store.track,
        store.provenance.car,
        store.corners.len(),
        store.provenance.captures.join(", "),
    );

    println!(
        "\n  {:>4}  {:>3}  {:>5}  {:>5}  {:>5}  {:>7}  {:>6}  {:>6}  {:>5}",
        "turn", "dir", "in", "apex", "out", "time", "brake@", "power@", "trail"
    );
    for c in &store.corners {
        let brake_at = match c.brake_offset_m {
            Some(off) => format!("{off:+.0}m"),
            None => "   --".to_string(),
        };
        let power_at = match c.throttle_pickup_offset_m {
            Some(off) => format!("{off:+.0}m"),
            None => "   --".to_string(),
        };

        println!(
            "  {:>4}  {:>3}  {:>5.0}  {:>5.0}  {:>5.0}  {:>6.2}s  {brake_at:>6}  {power_at:>6}  {:>5}",
            c.corner_id.to_string(),
            c.direction.short(),
            c.entry_speed_mps * 3.6,
            c.apex_speed_mps * 3.6,
            c.exit_speed_mps * 3.6,
            c.time_in_corner_s,
            if c.trail_braking { "yes" } else { "-" },
        );
    }
}

fn print_source_stats(source: &dyn TelemetrySource) {
    let stats = source.stats();
    println!(
        "Frames read     {}\nBlank lines     {}\nUnparseable     {}",
        stats.samples, stats.blank_lines, stats.bad_lines
    );
}

fn print_lap_table(laps: &[Lap]) {
    println!("Laps ({} wrap segments)", laps.len());
    println!(
        "  {:>3}  {:>9}  {:>8}  {:>9}  {:>7}  {:<28}",
        "id", "time", "coverage", "rotation", "samples", "quality"
    );

    for lap in laps {
        // Rotation in units of pi is the readable form: a clean lap is 2.00,
        // and the MX5's spin is 4.00.
        let rotation_pi = lap.net_rotation / std::f32::consts::PI;
        let mut note = lap.quality.reason().to_string();
        if lap.sim_lap_time_ms.is_none() && lap.quality.is_clean() {
            note.push_str(" (wall clock)");
        }
        println!(
            "  {:>3}  {:>8.2}s  {:>7.1}%  {:>7.2}pi  {:>7}  {:<28}",
            lap.id.0,
            lap.lap_time_s(),
            lap.coverage * 100.0,
            rotation_pi,
            lap.samples.len(),
            note
        );
    }

    let clean = laps.iter().filter(|l| l.quality.is_clean()).count();
    let full = laps
        .iter()
        .filter(|l| l.quality != ai_racing_coach::features::LapQuality::Partial)
        .count();
    println!("  {} segments, {} full, {} clean", laps.len(), full, clean);
}

fn analyse_lap(lap: &Lap, step: f32) {
    println!(
        "Lap {} — {:.2}s, {} raw samples",
        lap.id.0,
        lap.lap_time_s(),
        lap.samples.len()
    );

    // The health check for the resampling stage, printed because it is the one
    // number that says whether corner detection can work at all.
    let raw_zeros = curvature::zero_fraction(&curvature::signed_curvature(&lap.samples));

    let Some(grid) = resample::resample_lap(&lap.samples, step) else {
        println!("  not enough distinct positions to resample");
        return;
    };

    let grid_zeros = curvature::zero_fraction(&curvature::signed_curvature(&grid.samples));
    println!(
        "  resampled to {} points @ {:.2} m ({} non-monotone samples dropped)",
        grid.samples.len(),
        grid.step_m,
        grid.non_monotone_dropped
    );
    println!(
        "  curvature zeros: {:.1}% raw -> {:.1}% resampled",
        raw_zeros * 100.0,
        grid_zeros * 100.0
    );

    let corners = corner::detect_corners(&grid);
    let (left, right) = corner::direction_counts(&corners);
    println!(
        "  {} corners, {} right / {} left",
        corners.len(),
        right,
        left
    );

    if corners.is_empty() {
        return;
    }
    print_corner_table(&corners, &grid);
}

fn print_corner_table(corners: &[TrackCorner], grid: &ResampledLap) {
    println!(
        "\n  {:>4}  {:>3}  {:>8}  {:>8}  {:>7}  {:>7}  {:>7}  {:>8}  {:>8}",
        "turn", "dir", "start", "end", "length", "apex", "radius", "turn", "min spd"
    );
    for c in corners {
        let radius = match c.apex_radius_m() {
            Some(r) => format!("{r:>6.0}m"),
            None => "     --".to_string(),
        };
        println!(
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
        );
    }

    // Speed at the apex is what a driver will ask about first, so give the
    // straight-line context too: the fastest point on the lap.
    if let Some(top) = grid
        .samples
        .iter()
        .max_by(|a: &&Sample, b: &&Sample| a.speed.total_cmp(&b.speed))
    {
        println!(
            "\n  top speed {:.1} km/h at {:.0} m",
            top.speed * 3.6,
            top.lap_distance
        );
    }
}
