//! `coach` — command-line entry point.
//!
//! Deliberately thin: parse arguments, drive the library, print. Every piece of
//! analysis lives in the library so that tests can reach it, which the previous
//! version could not do — the crate had no lib target, so `main.rs` was the only
//! place code could live and nothing in it was testable.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use ai_racing_coach::core::sample::Sample;
use ai_racing_coach::features::FeatureParams;
use ai_racing_coach::features::corner::{self, TrackCorner};
use ai_racing_coach::features::corner_features;
use ai_racing_coach::features::curvature;
use ai_racing_coach::features::lap::{Lap, LapTracker};
use ai_racing_coach::features::resample::{self, ResampledLap};
use ai_racing_coach::features::track_model::{LearnParams, ModelCorner, TrackModel};
use ai_racing_coach::telemetry::{NdjsonReplaySource, Sidecar, TelemetrySource};

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

/// Parse a flag that must be a fraction in `0..=1`.
///
/// Rejecting out-of-range values rather than clamping them catches a real
/// footgun: `--min-support 50`, meant as "50%", would otherwise clamp to 1.0 and
/// silently demand that every lap agree on every corner — the opposite of what
/// was asked for, with no warning.
fn fraction(s: &str) -> std::result::Result<f32, String> {
    let value: f32 = s.parse().map_err(|_| "not a number".to_string())?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("must be a fraction between 0 and 1 (0.5 means half the laps)".to_string())
    }
}

#[derive(Subcommand)]
enum Command {
    /// Read a capture and report what is in it: session, laps, corners.
    Inspect {
        /// An `.ndjson` or `.ndjson.gz` capture from the logger.
        capture: PathBuf,

        /// Distance-grid spacing in metres.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// Show the corner table for every clean lap, not just the fastest.
        #[arg(long)]
        all_laps: bool,
    },

    /// Learn the canonical corner set for a track and save it to disk.
    ///
    /// Corners are detected on every clean lap in the capture and only those
    /// that several laps independently agree on enter the model, because a
    /// single lap of a circuit can yield anywhere from 9 to 13 corners for a
    /// ten-corner layout. Geometry comes from the most representative lap.
    ///
    /// The vote removes corners only one lap saw. It cannot add a corner the
    /// detector missed on every lap, so on a circuit whose corners are packed
    /// much tighter than Red Bull Ring's, check the corner count against the
    /// real layout before trusting the model.
    LearnTrack {
        /// An `.ndjson` or `.ndjson.gz` capture from the logger.
        capture: PathBuf,

        /// Directory to write `<track>_<layout>.json` into.
        #[arg(long, default_value = "data/tracks")]
        out: PathBuf,

        /// Distance-grid spacing in metres.
        #[arg(long, default_value_t = resample::DEFAULT_STEP_M, value_parser = positive_metres)]
        step: f32,

        /// Fraction of clean laps that must agree on a corner. Never fewer
        /// than two laps, whatever this is set to.
        #[arg(long, default_value_t = 0.5, value_parser = fraction)]
        min_support: f32,

        /// How far apart two laps' apexes may be and still count as the same
        /// corner, in metres. Lower it on a circuit with closely-spaced
        /// corners, where the default lets neighbours compete for votes.
        #[arg(
            long,
            default_value_t = LearnParams::default().apex_tolerance_m,
            value_parser = positive_metres
        )]
        apex_tolerance: f32,

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Inspect {
            capture,
            step,
            all_laps,
        } => inspect(&capture, step, all_laps),
        Command::LearnTrack {
            capture,
            out,
            step,
            min_support,
            apex_tolerance,
            dry_run,
        } => learn_track(&capture, &out, step, min_support, apex_tolerance, dry_run),
        Command::Analyse {
            capture,
            model_dir,
            step,
            all_laps,
        } => analyse(&capture, &model_dir, step, all_laps),
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
/// Shared by every subcommand: reading frames, honouring the logger's sidecar
/// verdict and finding lap boundaries is the same work regardless of what is
/// done with the laps afterwards. The source is returned alongside them because
/// it owns the [`ai_racing_coach::core::SessionInfo`] and the read statistics.
///
/// Takes no grid spacing: laps here are raw samples at the logger's own rate.
/// Resampling onto a distance grid happens per-lap in the callers.
fn read_laps(capture: &Path) -> ai_racing_coach::Result<(NdjsonReplaySource, Vec<Lap>)> {
    // The logger's own verdict on the capture comes first: if it recorded a
    // fatal probe failure, the numbers inside are not worth reading.
    if let Some(sidecar) = Sidecar::for_capture(capture) {
        sidecar.check(capture)?;
        for warning in sidecar.warnings() {
            eprintln!("warning: {warning}");
        }
    }

    let mut source = NdjsonReplaySource::open(capture)?;

    // Track length comes from StaticInfo_TrackSPlineLength, read once. The
    // previous version estimated it from lap groupings and got 29.9 m for a
    // 4,286 m circuit.
    let mut tracker: Option<LapTracker> = None;
    let mut laps: Vec<Lap> = Vec::new();

    while let Some(frame) = source.next_frame()? {
        let tracker = tracker.get_or_insert_with(|| {
            let length = source
                .session()
                .map(|s| s.track_length)
                .unwrap_or(frame.track_spline_length);
            LapTracker::new(length)
        });
        if let Some(lap) = tracker.push(&frame) {
            laps.push(lap);
        }
    }

    if let Some(tracker) = tracker {
        laps.extend(tracker.finish());
    }

    Ok((source, laps))
}

fn inspect(capture: &Path, step: f32, all_laps: bool) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture)?;
    println!("{}", source.describe());

    println!();
    print_source_stats(&source);
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
    out_dir: &Path,
    step: f32,
    min_support: f32,
    apex_tolerance: f32,
    dry_run: bool,
) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture)?;
    println!("{}", source.describe());

    let session = source
        .session()
        .ok_or_else(|| ai_racing_coach::CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let params = LearnParams {
        step_m: step,
        min_support,
        apex_tolerance_m: apex_tolerance,
        ..LearnParams::default()
    };
    let model = TrackModel::learn(session, &laps, &capture.display().to_string(), &params)?;

    println!();
    print_model(&model);

    let path = out_dir.join(TrackModel::file_name(&model.track));

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

    model.save(&path)?;
    println!("\nWrote {}", path.display());
    Ok(())
}

fn print_model(model: &TrackModel) {
    let (left, right) = model.direction_counts();
    println!(
        "Track model — {} ({}), {:.0} m\n  \
         {} corners, {} right / {} left\n  \
         learned from {} clean lap(s) in {}, reference {}\n  \
         line spread {:.2} m mean, {:.2} m worst at {:.0} m, {:.2} m grid",
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
    );

    println!(
        "\n  {:>4}  {:>3}  {:>8}  {:>8}  {:>7}  {:>8}  {:>7}  {:>8}  {:>7}",
        "turn", "dir", "start", "end", "length", "apex", "radius", "turn", "laps"
    );
    for c in &model.corners {
        let radius = match c.apex_radius_m() {
            Some(r) => format!("{r:>6.0}m"),
            None => "     --".to_string(),
        };
        println!(
            "  {:>4}  {:>3}  {:>7.0}m  {:>7.0}m  {:>6.0}m  {:>7.0}m  {}  {:>7.0}°  {:>4}/{}",
            c.id.to_string(),
            c.direction.short(),
            c.start_m,
            c.end_m,
            c.length_m(),
            c.apex_m,
            radius,
            c.turn_degrees(),
            c.support,
            model.lap_count(),
        );
    }
    print_unanimity(&model.corners, model.lap_count());
}

/// Flag the corners not every lap found. These are where the model is least
/// certain, and they are the first thing to check when it looks wrong.
fn print_unanimity(corners: &[ModelCorner], laps: u32) {
    let split: Vec<&ModelCorner> = corners.iter().filter(|c| c.support < laps).collect();
    if split.is_empty() {
        println!("\n  all {laps} laps agreed on every corner");
        return;
    }
    let names: Vec<String> = split
        .iter()
        .map(|c| format!("{} ({}/{})", c.id, c.support, laps))
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
    model_dir: &Path,
    step: f32,
    all_laps: bool,
) -> ai_racing_coach::Result<()> {
    let (source, laps) = read_laps(capture)?;
    println!("{}", source.describe());

    let session = source
        .session()
        .ok_or_else(|| ai_racing_coach::CoachError::EmptyCapture {
            path: capture.display().to_string(),
        })?;

    let path = model_dir.join(TrackModel::file_name(&session.track));
    if !path.exists() {
        return Err(ai_racing_coach::CoachError::NotEnoughData {
            action: "analyse driving against a track model",
            detail: format!(
                "no model for {} at {} — learn one first with `coach learn-track`",
                session.track,
                path.display()
            ),
        });
    }
    let model = TrackModel::load(&path)?;
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
    }

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

fn print_source_stats(source: &NdjsonReplaySource) {
    println!(
        "Frames read     {}\nBlank lines     {}\nUnparseable     {}",
        source.frames_read(),
        source.blank_lines(),
        source.bad_lines()
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
        if lap.ac_lap_time_ms.is_none() && lap.quality.is_clean() {
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
