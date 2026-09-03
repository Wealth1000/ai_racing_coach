//! Flatten sessions into a dataset: one CSV row per corner pass.
//!
//! The loop the whole project runs on: a live session leaves a session file,
//! session files accumulate, and the accumulated corpus is what offline
//! analysis (and, later, learned models) trains on. This module is the
//! join — every pass from every session, with the track model's corner it
//! belongs to, the personal best's numbers where one exists, and the
//! outcome flags that say what the driver actually did with the corner.
//!
//! Sessions whose `model_fingerprint` does not match the model are refused
//! rather than mis-joined: corner ordinal 7 means a *place* only within the
//! model that numbered it.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::core::error::CoachError;
use crate::core::ids::LapId;
use crate::features::corner_features::CornerFeatures;
use crate::features::reference::ReferenceStore;
use crate::features::track_model::TrackModel;
use crate::storage::session::{SessionEvent, read_session};

/// The column names, in order. One source of truth for writer and reader.
const COLUMNS: &[&str] = &[
    "session",
    "lap",
    "lap_clean",
    "corner",
    "direction",
    "entry_speed_mps",
    "apex_speed_mps",
    "exit_speed_mps",
    "speed_min_offset_m",
    "brake_start_m",
    "braking_length_m",
    "peak_brake",
    "trail_braking",
    "throttle_pickup_offset_m",
    "min_throttle_in_corner",
    "time_in_corner_s",
    "peak_abs_slip_rad",
    "off_track_points",
    // Reference columns: the personal best's own numbers where one exists,
    // then the deltas the rules argue in — same sign conventions as
    // `models::rules` (positive = later / slower / past the reference).
    "ref_time_in_corner_s",
    "delta_time_s",
    "delta_brake_m",
    "delta_apex_speed_mps",
    "delta_throttle_pickup_m",
    // Outcome: how many pieces of advice this pass produced.
    "advice_count",
];

/// What an export produced, for the CLI's one-line report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetInfo {
    pub rows: u64,
    pub columns: usize,
}

/// Export every pass in every session file to `out` as CSV.
///
/// `sessions` are the individual `.ndjson` session files (the CLI lists the
/// directory); `reference` may be `None`, in which case the reference and
/// delta columns are empty for every row. Every session's fingerprint must
/// match `model`, or the whole export is refused — one mis-joined corner in
/// a training corpus is worse than no corpus.
pub fn export_dataset(
    sessions: &[PathBuf],
    model: &TrackModel,
    reference: Option<&ReferenceStore>,
    out: &Path,
) -> Result<DatasetInfo, CoachError> {
    // Read every session first: a fingerprint mismatch must fail before a
    // single row is written, not partway through the output file.
    let mut logs = Vec::with_capacity(sessions.len());
    for path in sessions {
        let log = read_session(path)?;
        if log.header.model_fingerprint != model.fingerprint() {
            return Err(CoachError::BadArtefact {
                path: path.display().to_string(),
                artefact: "session file",
                detail: format!(
                    "recorded against model fingerprint {}, but the model is {} — \
                     the corners do not mean the same places",
                    log.header.model_fingerprint,
                    model.fingerprint()
                ),
            });
        }
        logs.push((path, log));
    }

    let file = File::create(out).map_err(|e| CoachError::Io {
        path: out.display().to_string(),
        source: e,
    })?;
    let mut writer = BufWriter::new(file);

    writer
        .write_all(&[COLUMNS.join(",").as_bytes(), b"\n"].concat())
        .map_err(io_err(out))?;

    let mut rows = 0u64;
    for (path, log) in &logs {
        // Clean flags by lap, and advice counts by (lap, corner), from the
        // events that precede and surround the passes. Advice records carry
        // no lap id (the driver does not care which lap a sentence belongs
        // to), so each one is attributed to the lap of the most recent pass
        // of that corner — valid because the consumer writes a session's
        // events before the advice they produced.
        let mut clean_by_lap: HashMap<LapId, bool> = HashMap::new();
        let mut last_lap_of_corner: HashMap<u32, LapId> = HashMap::new();
        let mut advice_counts: HashMap<(LapId, u32), u64> = HashMap::new();
        for event in &log.events {
            match event {
                SessionEvent::LapBoundary { lap, clean, .. } => {
                    clean_by_lap.insert(*lap, *clean);
                }
                SessionEvent::Pass(f) => {
                    last_lap_of_corner.insert(f.corner_id.0, f.lap_id);
                }
                SessionEvent::Advice { advice, .. } => {
                    if let Some(lap) = last_lap_of_corner.get(&advice.corner_id.0) {
                        *advice_counts.entry((*lap, advice.corner_id.0)).or_insert(0) += 1;
                    }
                }
            }
        }

        for event in &log.events {
            if let SessionEvent::Pass(f) = event {
                write_row(
                    &mut writer,
                    out,
                    path,
                    f,
                    clean_by_lap.get(&f.lap_id).copied(),
                    reference,
                    advice_counts
                        .get(&(f.lap_id, f.corner_id.0))
                        .copied()
                        .unwrap_or(0),
                )?;
                rows += 1;
            }
        }
    }

    writer.flush().map_err(io_err(out))?;
    Ok(DatasetInfo {
        rows,
        columns: COLUMNS.len(),
    })
}

fn write_row<W: Write>(
    writer: &mut W,
    out: &Path,
    session: &Path,
    f: &CornerFeatures,
    clean: Option<bool>,
    reference: Option<&ReferenceStore>,
    advice_count: u64,
) -> Result<(), CoachError> {
    let ref_pass = reference.and_then(|r| r.pass_for(f.corner_id).copied());

    let mut columns: Vec<String> = vec![
        csv_field(
            session
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        f.lap_id.0.to_string(),
        clean.map(|c| c.to_string()).unwrap_or_default(),
        f.corner_id.0.to_string(),
        f.direction.short().to_string(),
        fmt(f.entry_speed_mps),
        fmt(f.apex_speed_mps),
        fmt(f.exit_speed_mps),
        fmt(f.speed_min_offset_m),
        f.brake_start_m.map(fmt).unwrap_or_default(),
        f.braking_length_m.map(fmt).unwrap_or_default(),
        fmt(f.peak_brake),
        f.trail_braking.to_string(),
        f.throttle_pickup_offset_m.map(fmt).unwrap_or_default(),
        fmt(f.min_throttle_in_corner),
        fmt(f.time_in_corner_s),
        fmt(f.peak_abs_slip_rad),
        f.off_track_points.to_string(),
    ];

    // Deltas, in the sign conventions the rules use: positive brake delta is
    // later than the reference, positive time delta is slower, negative apex
    // delta is less speed carried.
    match &ref_pass {
        Some(r) => {
            columns.push(fmt(r.time_in_corner_s));
            columns.push(fmt(f.time_in_corner_s - r.time_in_corner_s));
            columns.push(
                match (f.braking_length_m, r.brake_offset_m) {
                    (Some(mine), Some(pb)) => Some(fmt(mine - pb)),
                    _ => None,
                }
                .unwrap_or_default(),
            );
            columns.push(fmt(f.apex_speed_mps - r.apex_speed_mps));
            columns.push(
                match (f.throttle_pickup_offset_m, r.throttle_pickup_offset_m) {
                    (Some(mine), Some(pb)) => Some(fmt(mine - pb)),
                    _ => None,
                }
                .unwrap_or_default(),
            );
        }
        None => columns.extend(std::iter::repeat_n(String::new(), 5)),
    }
    columns.push(advice_count.to_string());

    debug_assert_eq!(columns.len(), COLUMNS.len());
    let mut line = columns.join(",").into_bytes();
    line.push(b'\n');
    writer.write_all(&line).map_err(io_err(out))
}

fn fmt(v: f32) -> String {
    format!("{v:.4}")
}

/// Quote a CSV field if it contains a comma, quote, or newline.
fn csv_field(s: String) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> CoachError + '_ {
    move |e| CoachError::Io {
        path: path.display().to_string(),
        source: e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_fields_are_quoted_only_when_they_need_it() {
        assert_eq!(csv_field("monza".to_string()), "monza");
        assert_eq!(csv_field("a,b".to_string()), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\"".to_string()), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn the_column_list_matches_what_write_row_emits() {
        // `write_row` builds its column vector to match `COLUMNS`; a
        // mismatch is a debug_assert away, but this pins the two together
        // even in release builds.
        assert_eq!(COLUMNS.len(), 24);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai_racing_coach_test_{}_{tag}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// The loop the crate runs on, end to end over a real capture: record a
    /// session exactly the way `coach live --record-session` does (events
    /// before the advice they produced), then export it as a dataset and read
    /// the CSV back. What this pins is the join — every pass in the session
    /// becomes exactly one row, every delivered line of advice is attributed
    /// to the pass that produced it, and nothing else appears.
    #[test]
    fn a_recorded_session_exports_as_one_row_per_pass() {
        use crate::audio::FeedbackSink;
        use crate::coaching::DecisionConfig;
        use crate::core::{CoachConfig, InputDevice, Sample, SessionId};
        use crate::features::reference::ReferenceStore;
        use crate::runtime::{CoachPipeline, RuntimeEvent, Stage};
        use crate::storage::session::{SessionCounters, SessionEvent, SessionHeader, SessionWriter};
        use crate::telemetry::NdjsonReplaySource;
        use crate::telemetry::source::TelemetrySource;
        use std::io::BufRead;
        use std::sync::Arc;
        use std::time::Duration;

        const MONZA_CAPTURE: &str =
            "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";
        const MONZA_MODEL: &str = "data/tracks/monza.json";

        let Ok(model_for_export) = TrackModel::load(MONZA_MODEL) else {
            eprintln!("skipping: {MONZA_MODEL} not present");
            return;
        };
        let Ok(mut source) = NdjsonReplaySource::open(MONZA_CAPTURE) else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };
        // The session facts arrive with the first frame, so read it before
        // asking for them — and hand it back to the stream below so the
        // pipeline still sees every sample exactly once (the same trick as
        // `coach live`'s `PrefixedSource`).
        let mut first_frame = source.next_frame().expect("read capture");
        let session = source.session().expect("capture has session info").clone();

        // A second copy of the model drives the pipeline (the first is kept
        // back for the export — the pipeline consumes what it is given).
        let Ok(model) = TrackModel::load(MONZA_MODEL) else {
            eprintln!("skipping: {MONZA_MODEL} not present");
            return;
        };
        let config = CoachConfig {
            input: InputDevice::Replay {
                capture: MONZA_CAPTURE.into(),
            },
            step_m: model.provenance.step_m,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let reference = ReferenceStore::empty(&model);
        let mut pipeline =
            CoachPipeline::new(model, reference, config).with_decision_config(DecisionConfig {
                corner_cooldown: Duration::ZERO,
                kind_cooldown: Duration::ZERO,
                repetition_limit: u32::MAX,
                info_enabled: true,
            });

        let dir = temp_dir("export_e2e");
        std::fs::create_dir_all(&dir).expect("make session dir");
        let id = SessionId::generate();
        let counters = SessionCounters {
            dropped_frames: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            dropped_advice: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            voice_skipped: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let mut writer = SessionWriter::create(&dir, &id, counters).expect("create writer");
        writer
            .write_header(&SessionHeader {
                session_id: id.clone(),
                sim: session.sim,
                track: session.track.clone(),
                car: session.car.clone(),
                model_fingerprint: model_for_export.fingerprint(),
                step_m: 1.0,
                started_at_unix_ms: 0,
            })
            .expect("write header");

        // The consumer's exact ordering: events queued by the pipeline thread
        // land before the advice they produced, so a session file can always
        // attribute advice to the pass that caused it.
        let translate = |e: RuntimeEvent| match e {
            RuntimeEvent::LapBoundary { lap, time_s, clean } => {
                SessionEvent::LapBoundary { lap, time_s, clean }
            }
            RuntimeEvent::Pass(f) => SessionEvent::Pass(f),
        };
        let mut passes = 0u64;
        let mut advice_total = 0u64;
        let mut track_length: Option<f32> = None;
        loop {
            let frame = match first_frame.take() {
                Some(f) => f,
                None => match source.next_frame().expect("read capture") {
                    Some(f) => f,
                    None => break,
                },
            };
            let length = track_length
                .get_or_insert_with(|| source.session().map(|s| s.track_length).unwrap_or(0.0));
            let advice = pipeline.on_sample(&Sample::from_ac_frame(&frame, *length));
            for event in pipeline.take_events() {
                if matches!(event, RuntimeEvent::Pass(_)) {
                    passes += 1;
                }
                writer.write_event(&translate(event)).expect("write event");
            }
            for a in advice {
                writer.deliver(&a).expect("record advice");
                advice_total += 1;
            }
        }
        let advice = pipeline.finish();
        for event in pipeline.take_events() {
            if matches!(event, RuntimeEvent::Pass(_)) {
                passes += 1;
            }
            writer.write_event(&translate(event)).expect("write event");
        }
        for a in advice {
            writer.deliver(&a).expect("record advice");
            advice_total += 1;
        }
        writer.flush();
        let session_path = dir.join(format!("{id}.ndjson"));
        drop(writer);

        assert!(passes > 0, "the fixture must produce passes for the check to mean anything");
        assert!(advice_total > 0, "the fixture must produce advice for the check to mean anything");

        let csv_path = dir.join("dataset.csv");
        let info = export_dataset(
            &[session_path],
            &model_for_export,
            None,
            &csv_path,
        )
        .expect("export");
        assert_eq!(info.rows, passes, "one CSV row per recorded pass");
        assert_eq!(info.columns, 24);

        // Read the CSV back and hold it to its own header: line count, field
        // count, and the advice-attribution join.
        let file = File::open(&csv_path).expect("open csv");
        let mut lines = std::io::BufReader::new(file).lines();
        let header_line = lines.next().expect("header line").expect("read header");
        assert_eq!(header_line, COLUMNS.join(","));
        let mut advice_counted = 0u64;
        let mut rows_read = 0u64;
        for line in lines {
            let line = line.expect("read row");
            let fields = split_csv_row(&line);
            assert_eq!(fields.len(), COLUMNS.len(), "every row carries every column");
            let advice_n: u64 = fields[23].parse().expect("advice_count is an integer");
            advice_counted += advice_n;
            let clean = fields[2].as_str();
            assert!(
                clean.is_empty() || clean == "true" || clean == "false",
                "lap_clean is a flag or empty, not {clean:?}"
            );
            rows_read += 1;
        }
        assert_eq!(rows_read, passes);
        assert_eq!(
            advice_counted, advice_total,
            "every delivered line of advice is attributed to the pass that produced it"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Split one CSV row back into fields, honouring the quoting
    /// `csv_field` applies (no field in this dataset needs it, but the reader
    /// should not be wrong about its own writer either).
    fn split_csv_row(line: &str) -> Vec<String> {
        let mut fields = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if in_quotes && chars.peek() == Some(&'"') => {
                    current.push('"');
                    chars.next();
                }
                '"' => in_quotes = !in_quotes,
                ',' if !in_quotes => fields.push(std::mem::take(&mut current)),
                _ => current.push(c),
            }
        }
        fields.push(current);
        fields
    }
}
