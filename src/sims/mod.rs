//! One module per supported simulator.
//!
//! The provider trait ([`SimProvider`]) is the plug-in point for a new sim:
//! implement it, add the sim's [`crate::core::sample::Sim`] variant, register
//! the provider in [`registry`] — and nothing outside this directory changes.
//! The pipeline, coaching, storage and UI all speak the canonical
//! [`crate::core::sample::Sample`]; a provider's whole job is turning its
//! sim's raw telemetry into those.

pub mod assetto_corsa;

use std::path::Path;

use crate::core::error::CoachError;
use crate::core::sample::Sim;
use crate::core::Result;
use crate::telemetry::TelemetrySource;

/// A provider's verdict on a capture file it was offered.
///
/// `Err` is *not* a third verdict: it means "this file is mine and it is
/// broken" — the provider recognised its format but cannot read the file
/// (a schema drift, an implausible value, the logger's own sidecar flagging
/// it). A probing caller propagates that error rather than offering the file
/// to the next provider, because no other provider can do better with it.
pub enum CaptureOpen {
    /// The file is this provider's format; here is its source, positioned at
    /// the first sample (the one the probe consumed is handed back through it).
    Claimed(Box<dyn TelemetrySource>),
    /// The file is not this provider's format. The string says why, so the
    /// "no provider recognised this" error can name every attempt.
    Declined(String),
}

/// Everything the sim-agnostic layers may know about one supported simulator.
///
/// The unit type of a provider is stateless — the trait object in
/// [`registry`] is shared across threads, hence the `Send + Sync` bounds.
pub trait SimProvider: Send + Sync {
    /// Which sim this provider serves. The [`Sim`] enum is closed on purpose:
    /// adding a variant is a compile error everywhere the sim matters, which
    /// is the audit trail for "a second sim landed".
    fn sim(&self) -> Sim;

    /// Short key used in on-disk paths and the `--sim` flag. Defaults to the
    /// [`Sim`]'s own key, which is where it is defined once for everyone.
    fn key(&self) -> &'static str {
        self.sim().key()
    }

    /// Human-readable name, for messages the driver reads.
    fn name(&self) -> &'static str {
        self.sim().name()
    }

    /// Open a capture file for replay. See [`CaptureOpen`] for the verdicts.
    fn open_capture(&self, path: &Path) -> Result<CaptureOpen>;

    /// Attach to the running sim. Default: this build has no live reader for
    /// the sim (AC's shared-memory reader arrives in Batch 16).
    fn live(&self) -> Result<Box<dyn TelemetrySource>> {
        Err(CoachError::LiveAttachUnsupported {
            sim: self.name().to_string(),
        })
    }

    /// Record a capture live from the running sim. Default: this build has
    /// no live reader, so there is nothing to record either.
    fn record(&self, _opts: &RecordOptions) -> Result<RecordSummary> {
        Err(CoachError::LiveAttachUnsupported {
            sim: self.name().to_string(),
        })
    }

    /// Coach live *and* record the session's raw telemetry to `out_dir` —
    /// the record-while-coaching setting. Default: coaching without a
    /// recorder, reported as [`CoachError::LiveRecordUnsupported`] so the
    /// caller can warn and fall back to [`Self::live`] rather than losing
    /// the session over its byproduct.
    fn live_with_recording(
        &self,
        _out_dir: &Path,
    ) -> Result<Box<dyn TelemetrySource>> {
        Err(CoachError::LiveRecordUnsupported {
            sim: self.name().to_string(),
        })
    }
}

/// What `coach record` was asked to do. Sim-agnostic by construction: the
/// options say where to write and when to stop, and the provider decides
/// what "attached to the sim" means.
#[derive(Debug, Default)]
pub struct RecordOptions {
    /// The capture file to write. `None` lets the provider pick its default
    /// name, resolved once the session is known (AC's is
    /// `telemetry_ac_<track>_<car>_<stamp>.ndjson.gz`).
    pub out: Option<std::path::PathBuf>,
    /// Stop after this many laps complete, counted from the lap the
    /// recording started on. `None` records until the process is stopped.
    pub laps: Option<u32>,
    /// Write plain NDJSON instead of gzip.
    pub plain: bool,
    /// Stop when this flag is set, checked between polls. `None` never
    /// stops on demand — the GUI's record screen sets it so its Stop button
    /// ends a `--laps`-less recording cleanly (flushed and readable) instead
    /// of killing the process and costing the gzip trailer.
    pub stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// What a recording did — the counters a driver checks before trusting the
/// file, mirroring the C# logger's exit summary.
#[derive(Debug, Default)]
pub struct RecordSummary {
    /// The capture that was written, once the first frame resolved the name.
    pub path: Option<std::path::PathBuf>,
    /// Frames written.
    pub frames: usize,
    /// Polls skipped because the car was not yet on track.
    pub skipped_no_position: usize,
    /// Polls skipped as republished duplicates.
    pub skipped_duplicate: usize,
    /// Frames held back because no session (track/car) was loaded yet.
    pub skipped_no_session: usize,
    /// Laps completed during the recording.
    pub laps_completed: i32,
}

/// Every registered provider, in probing order.
///
/// A `LazyLock` static rather than a build script or dynamic loading: the
/// set of sims is a property of what this crate was compiled with, and
/// probing order is registry order — stable, and irrelevant while there is
/// one provider.
pub fn registry() -> &'static [Box<dyn SimProvider>] {
    static REGISTRY: std::sync::LazyLock<Vec<Box<dyn SimProvider>>> =
        std::sync::LazyLock::new(|| vec![Box::new(assetto_corsa::AssettoCorsa)]);
    &REGISTRY
}

/// The provider to take live telemetry from (`coach live` with no capture,
/// `coach gui` with no capture, `coach record`).
///
/// With a key: that provider, or an error listing the registered keys. Without
/// one: the single registered provider — a driver should never have to type
/// `--sim ac` while there is only one sim to mean — and once there are
/// several, an error naming the choices, because guessing a sim is worse
/// than asking.
pub fn provider_for_live<'a>(
    providers: &[&'a dyn SimProvider],
    sim: Option<&str>,
) -> Result<&'a dyn SimProvider> {
    let candidates: Vec<&'a dyn SimProvider> = match sim {
        Some(key) => {
            let provider = providers
                .iter()
                .copied()
                .find(|p| p.key() == key)
                .ok_or_else(|| CoachError::UnknownSim {
                    key: key.to_string(),
                    known: providers
                        .iter()
                        .map(|p| p.key().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                })?;
            vec![provider]
        }
        None => providers.to_vec(),
    };
    match candidates.as_slice() {
        [only] => Ok(*only),
        many => Err(CoachError::UnknownSim {
            key: "(unset)".to_string(),
            known: format!(
                "several sims are registered — pass --sim with one of: {}",
                many.iter().map(|p| p.key()).collect::<Vec<_>>().join(", ")
            ),
        }),
    }
}

/// Offer a capture to each provider in turn; the first to claim it wins.
///
/// Without a key, every provider is offered the file in the given order, and
/// when none claims it the error names each provider's reason for declining —
/// a foreign format is a diagnosis, not a mystery. With a key, only that
/// provider is asked and its refusal is reported alone: the flag is a claim
/// about the file, and probing around it would only bury the reason.
///
/// An `Err` from a provider propagates immediately rather than being recorded
/// as an attempt, because of what the error means (see [`CaptureOpen`]): the
/// provider recognised *its* file and found it broken, and no other provider
/// can do better with it.
pub fn open_capture_from(
    providers: &[&dyn SimProvider],
    path: &Path,
    sim: Option<&str>,
) -> Result<Box<dyn TelemetrySource>> {
    let selected: Vec<&dyn SimProvider> = match sim {
        Some(key) => {
            let provider = providers
                .iter()
                .copied()
                .find(|p| p.key() == key)
                .ok_or_else(|| CoachError::UnknownSim {
                    key: key.to_string(),
                    known: providers
                        .iter()
                        .map(|p| p.key().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                })?;
            vec![provider]
        }
        None => providers.to_vec(),
    };

    let mut attempts: Vec<(String, String)> = Vec::new();
    for provider in selected {
        match provider.open_capture(path)? {
            CaptureOpen::Claimed(source) => return Ok(source),
            CaptureOpen::Declined(reason) => attempts.push((provider.name().to_string(), reason)),
        }
    }

    Err(CoachError::UnrecognisedCapture {
        path: path.display().to_string(),
        attempts: crate::core::error::CaptureAttempts(attempts),
    })
}

/// Open a capture through the [`registry`] — the CLI's entry point.
pub fn open_capture(path: &Path, sim: Option<&str>) -> Result<Box<dyn TelemetrySource>> {
    let providers: Vec<&dyn SimProvider> = registry().iter().map(|p| p.as_ref()).collect();
    open_capture_from(&providers, path, sim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample::{Sample, SessionInfo};
    use crate::features::lap::LapTracker;
    use crate::features::reference::ReferenceStore;
    use crate::features::track_model::{LearnParams, TrackModel};
    use crate::runtime::pipeline::CoachPipeline;
    use crate::runtime::Stage;
    use crate::telemetry::ndjson::NdjsonLines;

    /// A second provider with a format and conversion conventions of its own,
    /// here to prove the seam is real: if `SimProvider` leaks anything
    /// AC-shaped, this fails to compile or produces nothing.
    ///
    /// It reuses `Sim::AssettoCorsa` because the enum is closed until a real
    /// second sim arrives (its documented purpose); what is under test is the
    /// trait, not the variant list.
    struct SyntheticSim;

    /// The synthetic format: one JSON object per line, field names and units
    /// chosen to be nothing like AC's — speed in km/h, steering in radians,
    /// positions in a flat (x, z) world.
    #[derive(serde::Deserialize)]
    struct SyntheticFrame {
        d_m: f32,
        frac: f32,
        x_m: f32,
        z_m: f32,
        hdg_rad: f32,
        v_kmh: f32,
        str_rad: f32,
        brk: f32,
        thr: f32,
        lap_ms: i32,
        last_lap_ms: i32,
    }

    /// Full steering lock in the synthetic sim's convention: 30 degrees.
    const SYNTH_MAX_STEER_RAD: f32 = std::f32::consts::PI / 6.0;

    impl SyntheticSim {
        fn convert(f: &SyntheticFrame, t_ms: i64) -> Sample {
            Sample {
                t_ms,
                lap_distance: f.d_m,
                lap_frac: f.frac,
                pos: [f.x_m, 0.0, f.z_m],
                heading: crate::core::math::wrap_pi(f.hdg_rad),
                // The sim publishes km/h; the pipeline speaks m/s.
                speed: f.v_kmh / 3.6,
                throttle: f.thr,
                brake: f.brk,
                // The sim publishes steering as an angle; the pipeline speaks
                // a normalised -1..1 channel.
                steer: (f.str_rad / SYNTH_MAX_STEER_RAD).clamp(-1.0, 1.0),
                yaw_rate: 0.0,
                slip_angle: 0.0,
                gear: 4,
                rpm: 6000.0,
                tyres_out: 0,
                live: true,
                surface_grip: 1.0,
                lap_time_ms: f.lap_ms,
                last_lap_time_ms: f.last_lap_ms,
            }
        }
    }

    impl SimProvider for SyntheticSim {
        fn sim(&self) -> crate::core::sample::Sim {
            crate::core::sample::Sim::AssettoCorsa
        }

        fn key(&self) -> &'static str {
            "synth"
        }

        fn name(&self) -> &'static str {
            "Synthetic Test Sim"
        }

        fn open_capture(&self, path: &Path) -> Result<CaptureOpen> {
            let mut lines = NdjsonLines::open(path)?;
            let first = match lines.next(|s| serde_json::from_str::<SyntheticFrame>(s)) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    return Err(CoachError::EmptyCapture {
                        path: path.display().to_string(),
                    })
                }
                Err(CoachError::Json { source, .. }) => {
                    return Ok(CaptureOpen::Declined(format!(
                        "first line is not a synthetic frame: {source}"
                    )))
                }
                Err(other) => return Err(other),
            };

            let track_length = 4.0 * (SYNTH_STRAIGHT_M + SYNTH_ARC_M);
            let session = SessionInfo {
                sim: self.sim(),
                track: crate::core::TrackId {
                    track: "synthetic_oval".into(),
                    layout: "gp".into(),
                },
                car: "Synth 01".into(),
                track_length,
                sector_count: 3,
                sim_version: "Synth 0.0".into(),
            };
            let sample = Self::convert(&first, 0);
            let source = SyntheticSource {
                lines,
                session,
                // The first sample's timestamp plus one tick.
                next_t_ms: 20,
                pending: Some(sample),
            };
            Ok(CaptureOpen::Claimed(Box::new(source)))
        }
    }

    struct SyntheticSource {
        lines: NdjsonLines,
        session: SessionInfo,
        /// The timestamp the next converted sample will carry, so the wall
        /// clock advances even though the format itself has no time field.
        next_t_ms: i64,
        pending: Option<Sample>,
    }

    impl TelemetrySource for SyntheticSource {
        fn next_sample(&mut self) -> Result<Option<Sample>> {
            if let Some(p) = self.pending.take() {
                return Ok(Some(p));
            }
            let frame = self
                .lines
                .next(|s| serde_json::from_str::<SyntheticFrame>(s))?;
            let Some(frame) = frame else {
                return Ok(None);
            };
            let sample = SyntheticSim::convert(&frame, self.next_t_ms);
            // Timestamps advance by the local speed: 20 ms per sample at
            // 50 m/s would be 1 m, so scale by the speed the frame reports.
            self.next_t_ms += (20.0 * (frame.v_kmh / 3.6) / 50.0) as i64;
            Ok(Some(sample))
        }

        fn session(&self) -> Option<&SessionInfo> {
            Some(&self.session)
        }

        fn describe(&self) -> String {
            format!(
                "synthetic oval — {} ({} m)",
                self.session.car, self.session.track_length
            )
        }
    }

    // --- the synthetic track: a rounded square, clockwise (four right-handers)
    //
    // Straights of SYNTH_STRAIGHT_M joined by 90° arcs of radius 60 m.
    // Positions follow the canonical convention derived in
    // `features/curvature`: travel direction (-sin θ, cos θ) in (x, z), so a
    // positive dθ/ds (a right-hander) yields a positive ground-plane cross
    // product — the position and heading curvature estimators must agree.

    const SYNTH_RADIUS_M: f32 = 60.0;
    const SYNTH_STRAIGHT_M: f32 = 300.0;
    const SYNTH_ARC_M: f32 = std::f32::consts::FRAC_PI_2 * SYNTH_RADIUS_M;
    const SYNTH_TRACK_LEN_M: f32 = 4.0 * (SYNTH_STRAIGHT_M + SYNTH_ARC_M);
    const SYNTH_SAMPLE_STEP_M: f32 = 2.0;

    /// Heading rate at a lap position: 0 on the straights, +1/r in the arcs.
    fn synth_turn_rate(s: f32) -> f32 {
        let seg = SYNTH_STRAIGHT_M + SYNTH_ARC_M;
        let pos = s % seg;
        if pos < SYNTH_STRAIGHT_M {
            0.0
        } else {
            1.0 / SYNTH_RADIUS_M
        }
    }

    /// Write a synthetic capture: `laps` laps of the rounded square, identical
    /// driving every lap (voting needs agreement, not variety).
    fn write_synth_capture(path: &Path, laps: usize) {
        let mut lines = String::new();
        let mut x = 0.0f32;
        let mut z = 0.0f32;
        let mut hdg = 0.0f32;
        let mut lap_ms = 0i32;
        let mut lap_t_ms = 0i64;

        for lap in 0..laps {
            let mut s = 0.0f32;
            let lap_start_t = lap_t_ms;
            while s < SYNTH_TRACK_LEN_M {
                let in_corner = synth_turn_rate(s) > 0.0;
                // Braking starts 55 m before each corner — the pedal trace
                // the decision-event learner reads.
                let braking = {
                    let seg = SYNTH_STRAIGHT_M + SYNTH_ARC_M;
                    (SYNTH_STRAIGHT_M - 55.0..SYNTH_STRAIGHT_M).contains(&(s % seg))
                };

                // km/h in the format, m/s in the reasoning.
                let v_mps = if in_corner { 22.0 } else { 55.0 };
                let brake = if braking { 0.7 } else { 0.0 };
                let throttle = if in_corner { 0.45 } else { 1.0 };
                let steer_rad = if in_corner { 0.22 } else { 0.0 };

                let lap_time_ms = (lap_t_ms - lap_start_t) as i32;
                lines.push_str(&format!(
                    "{{\"d_m\":{:.3},\"frac\":{:.6},\"x_m\":{:.3},\"z_m\":{:.3},\
                     \"hdg_rad\":{:.4},\"v_kmh\":{:.2},\"str_rad\":{:.3},\
                     \"brk\":{:.2},\"thr\":{:.2},\"lap_ms\":{},\"last_lap_ms\":{}}}\n",
                    s,
                    s / SYNTH_TRACK_LEN_M,
                    x,
                    z,
                    hdg,
                    v_mps * 3.6,
                    steer_rad,
                    brake,
                    throttle,
                    lap_time_ms,
                    if lap == 0 { 0 } else { lap_ms },
                ));

                // Advance the car along the lap.
                let ds = SYNTH_SAMPLE_STEP_M;
                x += -hdg.sin() * ds;
                z += hdg.cos() * ds;
                hdg += synth_turn_rate(s) * ds;
                s += ds;
                lap_t_ms += (ds / v_mps * 1000.0) as i64;
            }
            lap_ms = (lap_t_ms - lap_start_t) as i32;
        }
        std::fs::write(path, lines).unwrap();
    }

    fn temp_capture(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("coach_sims_tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn providers() -> Vec<&'static dyn SimProvider> {
        // Not the real registry — a registry *shaped* like one with two sims
        // in it. The real registry gains its second entry exactly when a real
        // second sim lands.
        vec![
            &assetto_corsa::AssettoCorsa as &'static dyn SimProvider,
            &SyntheticSim as &'static dyn SimProvider,
        ]
    }

    #[test]
    fn each_provider_claims_its_own_format_and_declines_the_other() {
        let synth_path = temp_capture("synthetic.ndjson");
        write_synth_capture(&synth_path, 2);

        let list = providers();
        let source = open_capture_from(&list, &synth_path, None).unwrap();
        let desc = source.describe();
        assert!(
            desc.contains("synthetic"),
            "the synthetic capture must be claimed by the synthetic provider, got: {desc}"
        );

        // The AC provider, asked alone, declines it.
        let ac: &dyn SimProvider = &assetto_corsa::AssettoCorsa;
        match ac.open_capture(&synth_path).unwrap() {
            CaptureOpen::Claimed(_) => panic!("AC must not claim a synthetic capture"),
            CaptureOpen::Declined(reason) => {
                assert!(reason.contains("not an AC frame"), "{reason}")
            }
        }

        // And the synthetic provider declines AC's own capture, when present.
        const MONZA: &str =
            "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";
        if std::path::Path::new(MONZA).exists() {
            let synth: &dyn SimProvider = &SyntheticSim;
            match synth.open_capture(Path::new(MONZA)).unwrap() {
                CaptureOpen::Claimed(_) => panic!("the synthetic provider must not claim AC data"),
                CaptureOpen::Declined(reason) => {
                    assert!(reason.contains("not a synthetic frame"), "{reason}")
                }
            }
            let source = open_capture_from(&list, Path::new(MONZA), None).unwrap();
            let desc = source.describe();
            assert!(
                desc.contains("ks_ferrari_sf70h"),
                "the AC capture must be claimed by AC, got: {desc}"
            );
        }
    }

    #[test]
    fn a_file_no_provider_knows_is_refused_loudly_with_every_reason() {
        let path = temp_capture("garbage.ndjson");
        std::fs::write(&path, "this is not telemetry at all\n").unwrap();

        let err = open_capture_from(&providers(), &path, None).map(|_| ()).unwrap_err();
        match &err {
            CoachError::UnrecognisedCapture { attempts, .. } => {
                let text = attempts.to_string();
                assert!(text.contains("Assetto Corsa"), "{text}");
                assert!(text.contains("Synthetic Test Sim"), "{text}");
            }
            other => panic!("expected UnrecognisedCapture, got {other:?}"),
        }
        assert!(
            err.to_string().contains("garbage.ndjson"),
            "the error must name the file: {err}"
        );

        // --sim narrows the attempt list to the one provider asked.
        let err = open_capture_from(&providers(), &path, Some("ac")).map(|_| ()).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("Assetto Corsa"), "{text}");
        assert!(!text.contains("Synthetic Test Sim"), "{text}");

        // An unknown key names the keys that exist.
        let err = open_capture_from(&providers(), &path, Some("nope")).map(|_| ()).unwrap_err();
        match &err {
            CoachError::UnknownSim { known, .. } => assert!(known.contains("ac"), "{known}"),
            other => panic!("expected UnknownSim, got {other:?}"),
        }
    }

    /// The proof the seam is real: a second provider's captures run through
    /// the whole coaching pipeline — lap splitting, model learning, live
    /// analysis — and produce corner passes and advice, without any
    /// AC-specific code involved anywhere in the path.
    #[test]
    fn a_second_provider_drives_the_whole_pipeline_end_to_end() {
        let path = temp_capture("synthetic_e2e.ndjson");
        write_synth_capture(&path, 4);

        // Learn a model from the capture, exactly as `coach learn-track` does.
        let mut source = open_capture_from(&providers(), &path, Some("synth")).unwrap();
        let session = source.session().expect("claimed source has a session").clone();
        let mut tracker = LapTracker::new(session.track_length);
        let mut laps = Vec::new();
        while let Some(mut sample) = source.next_sample().unwrap() {
            if let Some(lap) = tracker.push(&mut sample) {
                laps.push(lap);
            }
        }
        laps.extend(tracker.finish());
        assert!(
            laps.iter().any(|l| l.quality.is_clean()),
            "the synthetic laps must be clean; got: {:?}",
            laps.iter().map(|l| l.quality).collect::<Vec<_>>()
        );

        let model = TrackModel::learn(
            &session,
            &laps,
            &path.display().to_string(),
            &LearnParams { step_m: 1.0 },
        )
        .expect("a model can be learned from synthetic laps");
        assert!(
            model.corners.len() == 4,
            "the rounded square has four corners; learned {}",
            model.corners.len()
        );

        // Drive the live pipeline off a fresh copy of the same capture.
        let mut source = open_capture_from(&providers(), &path, Some("synth")).unwrap();
        let config = crate::core::config::CoachConfig {
            input: crate::core::config::InputDevice::Replay {
                capture: path.clone(),
            },
            step_m: 1.0,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let reference = ReferenceStore::empty(&model);
        let mut pipeline =
            CoachPipeline::new(model, reference, config).with_decision_config(crate::coaching::DecisionConfig {
                corner_cooldown: std::time::Duration::ZERO,
                kind_cooldown: std::time::Duration::ZERO,
                repetition_limit: u32::MAX,
                info_enabled: true,
            });

        let mut advice = Vec::new();
        while let Some(sample) = source.next_sample().unwrap() {
            advice.extend(pipeline.on_sample(&sample));
        }
        advice.extend(pipeline.finish());

        let passes = pipeline.take_passes();
        assert!(
            !passes.is_empty(),
            "four laps of a four-corner track must produce corner passes"
        );
        assert!(
            !advice.is_empty(),
            "a permissive decision layer must have something to say about {} passes",
            passes.len()
        );
    }
}
