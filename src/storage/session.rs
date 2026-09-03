//! Session files: the NDJSON record of one live session.
//!
//! # Schema
//!
//! One JSON object per line, every line tagged with a `"record"` field:
//!
//! ```text
//! {"record":"header", ...session facts...}          ← first line, exactly one
//! {"record":"lap_boundary","lap":1,"time_s":91.2,"clean":true}
//! {"record":"pass", ...one CornerFeatures...}
//! {"record":"advice", ...one Advice..., counters...}
//! ```
//!
//! The record types derive `Serialize` from the same types the rest of the
//! crate uses (`CornerFeatures`, `Advice`, `LapId`) rather than mirroring
//! their fields into a second schema, so a field added anywhere lands in the
//! session log without a parallel definition to keep alive. Deserializing
//! back needs the same types to implement `Deserialize`, which they do.
//!
//! # Failure rules
//!
//! Every I/O error is a [`CoachError`] naming the path — never an `unwrap`
//! — and a session whose last line was cut short by a crash still parses up
//! to the last complete line, with `SessionLog::truncated` saying so. A
//! malformed line in the *middle* is a real error: the file was corrupted,
//! not interrupted.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::audio::FeedbackSink;
use crate::coaching::Advice;
use crate::core::error::CoachError;
use crate::core::ids::{CornerId, LapId, SessionId, TrackId};
use crate::core::sample::Sim;
use crate::features::corner_features::CornerFeatures;

/// Facts about one recorded session, written as the file's first line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub session_id: SessionId,
    pub sim: Sim,
    pub track: TrackId,
    pub car: String,
    /// [`crate::features::track_model::TrackModel::fingerprint`] of the model
    /// the session was coached against. A session exported against a
    /// re-learned model would silently mis-join corners; the fingerprint is
    /// how the exporter refuses to.
    pub model_fingerprint: u64,
    /// Grid spacing the pipeline ran at, metres.
    pub step_m: f32,
    /// Wall-clock Unix milliseconds when the session started.
    pub started_at_unix_ms: u64,
}

/// One thing that happened during a session, after the header.
///
/// Advice records exist only for what the decision gate delivered; pass and
/// lap-boundary records exist for everything the pipeline measured, which is
/// what makes a session reconstructable as data rather than as a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A lap ended: its id, wall-clock duration, and whether it was clean.
    LapBoundary { lap: LapId, time_s: f32, clean: bool },
    /// A corner pass completed. The full measured feature row.
    Pass(CornerFeatures),
    /// A piece of advice was delivered, with the channel/skip counters as
    /// they stood at that moment — the honest account of what the driver was
    /// told versus what the coach decided.
    Advice {
        advice: Advice,
        dropped_frames: u64,
        dropped_advice: u64,
        voice_skipped: u64,
    },
}

impl SessionEvent {
    /// The corner this event is about, if any.
    pub fn corner(&self) -> Option<CornerId> {
        match self {
            SessionEvent::Pass(f) => Some(f.corner_id),
            SessionEvent::Advice { advice, .. } => Some(advice.corner_id),
            SessionEvent::LapBoundary { .. } => None,
        }
    }
}

/// Any line of a session file: the header, or one of its events.
///
/// The mirror of [`SessionEvent`] with the header added. The two enums must
/// stay tag-compatible — the round-trip test below is what enforces it, by
/// writing every event variant through [`SessionWriter`] and parsing the
/// file back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum SessionRecord {
    Header(SessionHeader),
    LapBoundary { lap: LapId, time_s: f32, clean: bool },
    Pass(CornerFeatures),
    Advice {
        advice: Advice,
        dropped_frames: u64,
        dropped_advice: u64,
        voice_skipped: u64,
    },
}

impl From<SessionEvent> for SessionRecord {
    fn from(e: SessionEvent) -> Self {
        match e {
            SessionEvent::LapBoundary { lap, time_s, clean } => {
                SessionRecord::LapBoundary { lap, time_s, clean }
            }
            SessionEvent::Pass(f) => SessionRecord::Pass(f),
            SessionEvent::Advice {
                advice,
                dropped_frames,
                dropped_advice,
                voice_skipped,
            } => SessionRecord::Advice {
                advice,
                dropped_frames,
                dropped_advice,
                voice_skipped,
            },
        }
    }
}

/// Writes a session file: buffered, one record per line, on the consumer
/// thread.
///
/// Buffered because a sync-per-event writer at the rate sessions produce
/// them would stall the consumer and inflate the drop counters — the same
/// §3.5 rule as the live channels. [`SessionWriter::flush`] (or `Drop`) is
/// where the bytes actually hit the disk, once per session.
///
/// The writer is also a [`FeedbackSink`]: it can be dropped into the same
/// consumer loop as the voice sink, where it records each delivered piece of
/// advice together with the drop/skip counters as they stood at that moment.
pub struct SessionWriter {
    out: BufWriter<File>,
    path: PathBuf,
    /// Records written after the header.
    pub events: u64,
    /// Channel drop counters snapshotted into each advice record.
    counters: SessionCounters,
}

/// The counters an advice record carries, shared with the live wiring.
#[derive(Clone, Default)]
pub struct SessionCounters {
    pub dropped_frames: Arc<AtomicU64>,
    pub dropped_advice: Arc<AtomicU64>,
    pub voice_skipped: Arc<AtomicU64>,
}

impl SessionWriter {
    /// Create (and truncate) `<dir>/<session-id>.ndjson`.
    ///
    /// Creates `dir` if it does not exist, so `--record-session data/sessions`
    /// works on a fresh checkout. `counters` should be the wiring's own
    /// `Arc`s so advice records quote the live numbers, not zeros.
    pub fn create(dir: &Path, id: &SessionId, counters: SessionCounters) -> Result<Self, CoachError> {
        std::fs::create_dir_all(dir).map_err(|e| CoachError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;
        let path = dir.join(format!("{id}.ndjson"));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| CoachError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
        Ok(Self {
            out: BufWriter::new(file),
            path,
            events: 0,
            counters,
        })
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the first line of the file. Exactly once, first: a session file
    /// without a header is not a session file.
    pub fn write_header(&mut self, h: &SessionHeader) -> Result<(), CoachError> {
        self.write_line(&SessionRecord::Header(h.clone()))
    }

    /// Append one event, flushing the buffer to the OS when it fills.
    pub fn write_event(&mut self, e: &SessionEvent) -> Result<(), CoachError> {
        self.events += 1;
        self.write_line(&SessionRecord::from(e.clone()))
    }

    fn write_line(&mut self, record: &SessionRecord) -> Result<(), CoachError> {
        serde_json::to_writer(&mut self.out, record).map_err(|e| CoachError::Io {
            path: self.path.display().to_string(),
            source: std::io::Error::other(e.to_string()),
        })?;
        self.out
            .write_all(b"\n")
            .map_err(|e| CoachError::Io {
                path: self.path.display().to_string(),
                source: e,
            })
    }
}

impl FeedbackSink for SessionWriter {
    fn deliver(&mut self, advice: &Advice) -> Result<(), CoachError> {
        self.write_event(&SessionEvent::Advice {
            advice: advice.clone(),
            dropped_frames: self.counters.dropped_frames.load(Ordering::Relaxed),
            dropped_advice: self.counters.dropped_advice.load(Ordering::Relaxed),
            voice_skipped: self.counters.voice_skipped.load(Ordering::Relaxed),
        })
    }

    fn flush(&mut self) {
        // A session that cannot even be flushed to disk is over; the error
        // surfaces on the next write if there is one, and nothing about the
        // coaching itself depended on the log succeeding.
        let _ = self.out.flush();
    }
}

impl Drop for SessionWriter {
    fn drop(&mut self) {
        let _ = self.out.flush();
    }
}

/// A session file parsed back in: its header and every complete event.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionLog {
    pub header: SessionHeader,
    pub events: Vec<SessionEvent>,
    /// The file's last line was incomplete (a crash mid-recording) and the
    /// events stop at the last complete one before it.
    pub truncated: bool,
}

/// Parse one session file.
///
/// A missing header is an error — there is no session to speak of. An
/// incomplete *last* line is not: it is the crash case, and the log parses
/// up to the line before it with [`SessionLog::truncated`] set. A malformed
/// line anywhere else is reported with its line number.
pub fn read_session(path: &Path) -> Result<SessionLog, CoachError> {
    let file = File::open(path).map_err(|e| CoachError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    // Session files are small (a few MB per hour of driving); holding the
    // lines is what makes the "was the bad line the last one?" question
    // answerable, which is the whole truncated-vs-corrupt distinction.
    let lines: Vec<std::io::Result<String>> = BufReader::new(file).lines().collect();
    let total = lines.len();

    let mut header = None;
    let mut events = Vec::new();
    let mut truncated = false;
    for (i, line) in lines.into_iter().enumerate() {
        let line_number = i + 1;
        let last = line_number == total;
        let line = line.map_err(|e| CoachError::Io {
            path: path.display().to_string(),
            source: e,
        })?;

        let record = match serde_json::from_str::<SessionRecord>(&line) {
            Ok(r) => r,
            Err(e) => {
                if last {
                    // The crash case: a record half-written when the process
                    // died. Everything before it is intact.
                    truncated = true;
                    break;
                }
                return Err(CoachError::Json {
                    path: path.display().to_string(),
                    line: line_number,
                    source: e,
                });
            }
        };
        match record {
            SessionRecord::Header(h) => {
                if header.is_none() {
                    header = Some(h);
                }
            }
            other => events.push(event_of(other)),
        }
    }

    let header = header.ok_or_else(|| CoachError::BadArtefact {
        path: path.display().to_string(),
        artefact: "session file",
        detail: "no header record — the first line must be the session header".to_string(),
    })?;
    Ok(SessionLog {
        header,
        events,
        truncated,
    })
}

/// The [`SessionEvent`] side of a parsed [`SessionRecord`].
///
/// A header in the middle of a file (which the writer never produces) is
/// skipped rather than recorded; the first header wins.
fn event_of(record: SessionRecord) -> SessionEvent {
    match record {
        SessionRecord::Header(_) => unreachable!("headers are matched before this is called"),
        SessionRecord::LapBoundary { lap, time_s, clean } => {
            SessionEvent::LapBoundary { lap, time_s, clean }
        }
        SessionRecord::Pass(f) => SessionEvent::Pass(f),
        SessionRecord::Advice {
            advice,
            dropped_frames,
            dropped_advice,
            voice_skipped,
        } => SessionEvent::Advice {
            advice,
            dropped_frames,
            dropped_advice,
            voice_skipped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::corner::CornerDirection;
    use crate::models::issue::{DrivingIssue, IssueKind, Severity};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai_racing_coach_test_{}_{tag}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }

    fn header() -> SessionHeader {
        SessionHeader {
            session_id: SessionId("session_test".to_string()),
            sim: Sim::AssettoCorsa,
            track: TrackId::new("monza", "layout_gp"),
            car: "ks_ferrari_sf70h".to_string(),
            model_fingerprint: 0xdead_beef,
            step_m: 1.0,
            started_at_unix_ms: 1_777_000_000_000,
        }
    }

    fn advice(text: &str) -> Advice {
        Advice::from_issue(
            &DrivingIssue::new(
                CornerId(3),
                CornerDirection::Right,
                IssueKind::LateBrakeVsPb,
                Severity::Warn,
            ),
            text.to_string(),
        )
    }

    fn events() -> Vec<SessionEvent> {
        vec![
            SessionEvent::LapBoundary {
                lap: LapId(1),
                time_s: 91.2,
                clean: true,
            },
            SessionEvent::Advice {
                advice: advice("brake 7 m earlier"),
                dropped_frames: 0,
                dropped_advice: 0,
                voice_skipped: 1,
            },
        ]
    }

    #[test]
    fn session_round_trips_through_the_file() {
        let dir = temp_dir("roundtrip");
        let mut writer =
            SessionWriter::create(&dir, &SessionId("session_test".to_string()), Default::default())
                .expect("create writer");
        writer.write_header(&header()).expect("header");
        for e in events() {
            writer.write_event(&e).expect("event");
        }
        drop(writer);

        let log = read_session(&dir.join("session_test.ndjson")).expect("read back");
        assert_eq!(log.header, header());
        assert_eq!(log.events, events());
        assert!(!log.truncated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_event_variant_round_trips() {
        let dir = temp_dir("variants");
        let pass = SessionEvent::Pass(CornerFeatures {
            lap_id: LapId(0),
            corner_id: CornerId(2),
            direction: CornerDirection::Left,
            entry_speed_mps: 80.0,
            apex_speed_mps: 60.0,
            exit_speed_mps: 75.0,
            speed_min_offset_m: -3.0,
            brake_start_m: Some(712.0),
            braking_length_m: Some(42.0),
            peak_brake: 0.9,
            trail_braking: true,
            throttle_pickup_offset_m: Some(11.0),
            min_throttle_in_corner: 0.2,
            time_in_corner_s: 3.1,
            peak_abs_slip_rad: 0.08,
            off_track_points: 0,
        });
        let all = vec![
            SessionEvent::LapBoundary {
                lap: LapId(0),
                time_s: 12.5,
                clean: false,
            },
            pass,
            SessionEvent::Advice {
                advice: advice("hold the apex"),
                dropped_frames: 2,
                dropped_advice: 1,
                voice_skipped: 0,
            },
        ];

        let mut writer =
            SessionWriter::create(&dir, &SessionId("session_test".to_string()), Default::default())
                .expect("create writer");
        writer.write_header(&header()).expect("header");
        for e in &all {
            writer.write_event(e).expect("event");
        }
        drop(writer);

        let log = read_session(&dir.join("session_test.ndjson")).expect("read back");
        assert_eq!(log.events, all, "every variant must survive the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_mid_record_still_parses_to_the_last_complete_line() {
        let dir = temp_dir("truncated");
        let mut writer =
            SessionWriter::create(&dir, &SessionId("session_test".to_string()), Default::default())
                .expect("create writer");
        writer.write_header(&header()).expect("header");
        for e in events() {
            writer.write_event(&e).expect("event");
        }
        drop(writer);

        // Simulate the crash: chop the last line in half.
        let path = dir.join("session_test.ndjson");
        let full = std::fs::read_to_string(&path).expect("read");
        let cut = full.len() - 25;
        std::fs::write(&path, &full[..cut]).expect("write truncated");

        let log = read_session(&path).expect("a truncated session still parses");
        assert!(log.truncated);
        assert_eq!(log.events.len(), 1, "the complete records survive");
        assert_eq!(log.header, header());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_line_in_the_middle_is_an_error_naming_the_line() {
        let dir = temp_dir("corrupt");
        let mut writer =
            SessionWriter::create(&dir, &SessionId("session_test".to_string()), Default::default())
                .expect("create writer");
        writer.write_header(&header()).expect("header");
        for e in events() {
            writer.write_event(&e).expect("event");
        }
        drop(writer);

        // Corrupt the FIRST event line, leaving a good line after it.
        let path = dir.join("session_test.ndjson");
        let full = std::fs::read_to_string(&path).expect("read");
        let mut lines: Vec<&str> = full.lines().collect();
        lines[1] = "{\"record\":";
        std::fs::write(&path, lines.join("\n") + "\n").expect("write corrupt");

        let err = read_session(&path).expect_err("corruption must not parse");
        match err {
            CoachError::Json { path, line, .. } => {
                assert!(path.ends_with("session_test.ndjson"), "names the file: {path}");
                assert_eq!(line, 2, "names the corrupt line");
            }
            other => panic!("expected a Json error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unwritable_directory_is_an_error_naming_the_path() {
        let dir = temp_dir("readonly");
        std::fs::create_dir_all(&dir).expect("make dir");
        let mut perms = std::fs::metadata(&dir).expect("stat").permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o555);
        std::fs::set_permissions(&dir, perms.clone()).expect("make read-only");

        let err = match SessionWriter::create(
            &dir,
            &SessionId("session_x".to_string()),
            Default::default(),
        ) {
            Ok(_) => panic!("a read-only directory must not be writable"),
            Err(e) => e,
        };
        match err {
            CoachError::Io { path, .. } => {
                assert!(path.contains("readonly"), "names the path: {path}");
            }
            other => panic!("expected an Io error, got {other:?}"),
        }

        perms.set_mode(0o755);
        std::fs::set_permissions(&dir, perms).expect("restore");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_without_a_header_is_refused() {
        let dir = temp_dir("noheader");
        std::fs::create_dir_all(&dir).expect("make dir");
        let path = dir.join("session_test.ndjson");
        std::fs::write(
            &path,
            serde_json::to_string(&SessionRecord::from(events()[0].clone())).expect("json") + "\n",
        )
        .expect("write");

        let err = read_session(&path).expect_err("no header, no session");
        assert!(matches!(err, CoachError::BadArtefact { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_writer_as_a_sink_records_advice_with_the_live_counters() {
        let dir = temp_dir("sink");
        let counters = SessionCounters::default();
        counters.dropped_frames.store(3, Ordering::Relaxed);
        let mut writer =
            SessionWriter::create(&dir, &SessionId("session_test".to_string()), counters.clone())
                .expect("create writer");
        writer.write_header(&header()).expect("header");

        FeedbackSink::deliver(&mut writer, &advice("carry more speed")).expect("deliver");
        drop(writer);

        let log = read_session(&dir.join("session_test.ndjson")).expect("read back");
        assert_eq!(log.events.len(), 1);
        match &log.events[0] {
            SessionEvent::Advice {
                advice,
                dropped_frames,
                voice_skipped,
                ..
            } => {
                assert_eq!(advice.phrased, "carry more speed");
                assert_eq!(*dropped_frames, 3, "counters snapshotted at the moment");
                assert_eq!(*voice_skipped, 0);
            }
            other => panic!("expected an advice record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
