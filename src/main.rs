//! `coach` — command-line entry point.
//!
//! Deliberately thin: parse arguments, dispatch, print. Every command's
//! implementation lives in the library ([`ai_racing_coach::commands`]) so
//! that the GUI runs the same code the terminal does, and everything in
//! there is testable. What stays here is the process-level wiring the
//! library cannot own: `live` and `gui`, which wire threads, sinks and a
//! window and end only when the process does.
//!
//! Running `coach` with no subcommand opens the coaching window — the
//! double-click path. Every command stays available under its own name for
//! terminals and scripts.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use ai_racing_coach::commands::{self, Progress};
use ai_racing_coach::features::resample;

#[derive(Parser)]
#[command(
    name = "coach",
    about = "AI sim racing coach — Assetto Corsa telemetry interpreter",
    version,
    // The generated help lists flags but never shows them combined. Kept on
    // `after_help` rather than `after_long_help` so short `-h` shows it too —
    // `-h` is what anyone actually types.
    after_help = "\
Running `coach` with no command opens the coaching window.

Examples:
  coach                                       open the coaching window
                                              (what a double-clicked exe does)
  coach inspect telemetry_ac.ndjson         analyse the fastest clean lap
  coach inspect capture.ndjson.gz           gzipped captures work directly
  coach inspect capture.ndjson --all-laps   one corner table per clean lap
  coach inspect capture.ndjson --step 0.5   finer distance grid (default 1 m)

  coach learn-track capture.ndjson.gz       write data/tracks/<track>.json
  coach learn-track a.ndjson b.ndjson       several captures vote together —
                                            the refinement loop
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
    /// Omitted: open the coaching window, as a double-clicked exe does.
    #[command(subcommand)]
    command: Option<Command>,
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
    /// Several captures of the same track and car vote together: re-learning
    /// from the original capture plus the ones later sessions recorded is how
    /// a model is refined rather than replaced.
    ///
    /// A corner straddling the start/finish line is stored as two rows linked
    /// by `parent_id`; it is one corner, not two.
    LearnTrack {
        /// One or more `.ndjson` / `.ndjson.gz` captures from the logger, all
        /// of the same track in the same car.
        #[arg(required = true)]
        captures: Vec<PathBuf>,
        /// Which simulator's provider to open the captures, by key (e.g.
        /// "ac"). Omit to offer each file to every registered provider and
        /// use the first that recognises it.
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
    /// This is also what running `coach` with no command does — the
    /// double-click path; the subcommand exists so the flag set
    /// (`--replay`, `--sim`, …) stays reachable from a terminal.
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

/// The CLI's [`Progress`]: results to stdout, warnings to stderr — the
/// convention every one of these commands has always printed under.
struct Stdio;

impl Progress for Stdio {
    fn line(&mut self, text: &str) {
        println!("{text}");
    }
    fn warn(&mut self, text: &str) {
        eprintln!("{text}");
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        // The double-click path: no command, no terminal, straight to the
        // coaching window with the same defaults `coach gui` uses.
        None => gui(
            None,
            Path::new("data/tracks"),
            resample::DEFAULT_STEP_M,
            None,
        ),
        Some(Command::Inspect {
            capture,
            sim,
            step,
            all_laps,
        }) => commands::inspect(&capture, sim.as_deref(), step, all_laps, &mut Stdio),
        Some(Command::LearnTrack {
            captures,
            sim,
            out,
            step,
            dry_run,
        }) => commands::learn_track(&captures, sim.as_deref(), &out, step, dry_run, &mut Stdio),
        Some(Command::Analyse {
            capture,
            sim,
            model_dir,
            step,
            all_laps,
        }) => commands::analyse(
            &capture,
            sim.as_deref(),
            &model_dir,
            step,
            all_laps,
            &mut Stdio,
        ),
        Some(Command::LearnPb {
            capture,
            sim,
            model_dir,
            step,
            dry_run,
        }) => commands::learn_pb(
            &capture,
            sim.as_deref(),
            &model_dir,
            step,
            dry_run,
            &mut Stdio,
        ),
        Some(Command::Live {
            replay,
            sim,
            model_dir,
            step,
            voice,
            record_session,
        }) => live(
            replay.as_deref(),
            &model_dir,
            step,
            sim.as_deref(),
            voice,
            record_session.as_deref(),
        ),
        Some(Command::Record {
            out,
            laps,
            plain,
            sim,
        }) => commands::record(
            out.as_deref(),
            laps,
            plain,
            sim.as_deref(),
            None,
            &mut Stdio,
        ),
        Some(Command::ExportDataset {
            sessions_dir,
            out,
            model_dir,
        }) => commands::export_dataset(&sessions_dir, &out, &model_dir, &mut Stdio),
        Some(Command::Gui {
            replay,
            sim,
            model_dir,
            step,
        }) => gui(replay.as_deref(), &model_dir, step, sim.as_deref()),
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
    use ai_racing_coach::audio::FeedbackSink;
    use ai_racing_coach::core::config::{CoachConfig, InputDevice};
    use ai_racing_coach::telemetry::PrefixedSource;

    // The source, and how the config names where its samples come from. A
    // live attach never fails here: the source starts in its waiting state
    // and the first `next_sample` below holds until the sim runs, saying why
    // once per reason — "the sim is not running yet" is the first phase of a
    // live session, not an error.
    let (mut source, input, sim_name) = match replay {
        Some(capture) => (
            ai_racing_coach::sims::open_capture(capture, sim)?,
            InputDevice::Replay {
                capture: capture.to_path_buf(),
            },
            None,
        ),
        None => {
            let providers: Vec<&dyn ai_racing_coach::sims::SimProvider> =
                ai_racing_coach::sims::registry().iter().map(|p| p.as_ref()).collect();
            let provider = ai_racing_coach::sims::provider_for_live(&providers, sim)?;
            // The record-while-coaching setting: the same capture the logger
            // would write, as a byproduct of coaching, so the session's laps
            // can refine the track model later. A build whose live reader has
            // no recorder says so and loses only the byproduct, never the
            // session.
            let source =
                if ai_racing_coach::core::Settings::load().record_while_coaching {
                    match provider.live_with_recording(std::path::Path::new(
                        ai_racing_coach::core::CAPTURES_DIR,
                    )) {
                        Ok(source) => source,
                        Err(ai_racing_coach::CoachError::LiveRecordUnsupported { sim }) => {
                            eprintln!(
                                "warning: {sim} cannot record while coaching in this \
                                 build — coaching without a session capture"
                            );
                            provider.live()?
                        }
                        Err(other) => return Err(other),
                    }
                } else {
                    provider.live()?
                };
            (source, InputDevice::SharedMemory, Some(provider.name()))
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

    let model = ai_racing_coach::runtime::load_model_for_session(&session, model_dir)?;
    println!();
    println!(
        "Model {} — {} corners learned from {} lap(s) of {}",
        model.track,
        model.corners.len(),
        model.lap_count(),
        model.provenance.car,
    );
    let reference =
        ai_racing_coach::runtime::load_reference_for_session(&session, &model, model_dir);

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
    let pipeline = ai_racing_coach::runtime::CoachPipeline::new(model, reference, config);
    let wiring = ai_racing_coach::runtime::spawn(
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
    let mut sinks: Vec<Box<dyn FeedbackSink>> = match voice {
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
    let dropped_frames = wiring.dropped_frames.load(std::sync::atomic::Ordering::Relaxed);
    let dropped_advice = wiring.dropped_advice.load(std::sync::atomic::Ordering::Relaxed);
    let dropped_events = wiring.dropped_events.load(std::sync::atomic::Ordering::Relaxed);
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
            let mut source = ai_racing_coach::sims::open_capture(capture, sim)?;
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

            let model = ai_racing_coach::runtime::load_model_for_session(&session, model_dir)?;
            let reference =
                ai_racing_coach::runtime::load_reference_for_session(&session, &model, model_dir);

            let config = ai_racing_coach::core::config::CoachConfig {
                input: ai_racing_coach::core::config::InputDevice::Replay {
                    capture: capture.to_path_buf(),
                },
                step_m: step,
                models_dir: model_dir.to_path_buf(),
                voice: Default::default(),
            };
            let pipeline = ai_racing_coach::runtime::CoachPipeline::new(model, reference, config);
            // The connection indicator's text, captured before the source
            // moves into the wiring — the UI thread never sees the source
            // itself, only what it calls itself.
            let source_desc = source.describe();
            let wiring = ai_racing_coach::runtime::spawn(
                Box::new(ai_racing_coach::telemetry::PrefixedSource::new(first, source)),
                pipeline,
            );

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
                    known: ai_racing_coach::sims::registry()
                        .iter()
                        .map(|p| p.key())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
            Box::new(gui)
        }
    };

    // A double-clicked exe (or `coach` with no command) owns the console
    // Windows spawned for it, and nobody is reading that console — hide it,
    // so the window is the program. A terminal launch keeps its console:
    // there is someone reading it, and `coach gui --replay … 2>err.log`
    // must still work.
    #[cfg(windows)]
    hide_owned_console();

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

/// Hide the console window this process owns — the one Windows spawned when
/// the exe was double-clicked.
///
/// `GetConsoleProcessList` tells the two launch modes apart: a double-click
/// makes this process the console's only member, while a terminal launch
/// shares the console with the shell (two or more). Only the owned console
/// is hidden — hiding the shell's would blank the terminal the driver is
/// reading. The console is hidden rather than detached so stdout still has
/// somewhere to go if anything ever writes to it.
///
/// Windows-only and unverifiable on the Linux dev machine; the release
/// workflow's windows-latest run compiles it (same arrangement as the
/// shared-memory mapping layer — see build.rs and Cargo.toml).
#[cfg(windows)]
fn hide_owned_console() {
    use windows_sys::Win32::System::Console::{
        GetConsoleProcessList, GetConsoleWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    // One page of process ids is plenty: the question is "are we alone?",
    // not "who else is there".
    let mut processes = [0u32; 16];
    let count = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), 16) };
    if count == 1 {
        unsafe {
            let console = GetConsoleWindow();
            if !console.is_null() {
                ShowWindow(console, SW_HIDE);
            }
        }
    }
}

/// Translate a pipeline event into its session-file record. The pipeline
/// speaks runtime terms; the session file speaks storage terms; this is the
/// only place they meet, because the mapping is trivially structural —
/// except that advice never appears here (it travels the advice channel,
/// where the sinks can hear it).
fn session_event(event: ai_racing_coach::runtime::RuntimeEvent) -> ai_racing_coach::storage::SessionEvent {
    use ai_racing_coach::storage::SessionEvent;
    match event {
        ai_racing_coach::runtime::RuntimeEvent::LapBoundary { lap, time_s, clean } => {
            SessionEvent::LapBoundary { lap, time_s, clean }
        }
        ai_racing_coach::runtime::RuntimeEvent::Pass(f) => SessionEvent::Pass(f),
    }
}
