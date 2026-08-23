//! Replay a capture file as a telemetry source.
//!
//! Streams line by line: memory stays flat regardless of capture size, which
//! matters because a 14-minute session is 51,383 frames / 26 MB gzipped and the
//! pipeline is meant to run the same way live.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use crate::core::{CoachError, Result, SessionInfo};
use crate::telemetry::frame::AcFrame;
use crate::telemetry::schema::validate_frame;
use crate::telemetry::sidecar::Sidecar;
use crate::telemetry::source::TelemetrySource;

/// Above this fraction of unparseable lines, stop rather than carry on.
///
/// A handful of bad lines at the tail of a capture is normal — the logger
/// flushes every 200 lines, so killing the sim mid-buffer can truncate the last
/// one. A high rate means something else is wrong, and analysing whatever
/// survives would produce a confident answer from a broken file.
const MAX_BAD_LINE_FRACTION: f64 = 0.01;

pub struct NdjsonReplaySource {
    path: PathBuf,
    reader: Box<dyn BufRead>,
    session: Option<SessionInfo>,
    sidecar: Option<Sidecar>,
    line_no: usize,
    frames: usize,
    bad_lines: usize,
    blank_lines: usize,
    /// Set once the first frame has passed the plausibility guard, so the guard
    /// runs exactly once per capture.
    validated: bool,
}

impl NdjsonReplaySource {
    /// Open a capture. Gzip is detected by the `.gz` extension.
    ///
    /// Also reads the logger's `.meta.json` sidecar if one sits beside the file,
    /// and refuses the capture outright if the logger flagged a fatal problem
    /// with it. The logger already knows whether its own output is trustworthy;
    /// there is no reason to rediscover that downstream.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| CoachError::Io {
            path: path.display().to_string(),
            source,
        })?;

        let reader: Box<dyn BufRead> =
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gz")) {
                Box::new(BufReader::new(GzDecoder::new(file)))
            } else {
                Box::new(BufReader::new(file))
            };

        let sidecar = Sidecar::for_capture(&path);
        if let Some(sc) = &sidecar {
            sc.check(&path)?;
        }

        Ok(Self {
            path,
            reader,
            session: None,
            sidecar,
            line_no: 0,
            frames: 0,
            bad_lines: 0,
            blank_lines: 0,
            validated: false,
        })
    }

    pub fn sidecar(&self) -> Option<&Sidecar> {
        self.sidecar.as_ref()
    }

    pub fn frames_read(&self) -> usize {
        self.frames
    }

    pub fn bad_lines(&self) -> usize {
        self.bad_lines
    }

    pub fn blank_lines(&self) -> usize {
        self.blank_lines
    }

    fn io_err(&self, source: std::io::Error) -> CoachError {
        CoachError::Io {
            path: self.path.display().to_string(),
            source,
        }
    }
}

impl TelemetrySource for NdjsonReplaySource {
    fn next_frame(&mut self) -> Result<Option<AcFrame>> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| self.io_err(e))?;
            if n == 0 {
                // End of stream. An empty capture is an error, not an empty
                // result: it means the logger ran but the sim was not.
                if self.frames == 0 {
                    return Err(CoachError::EmptyCapture {
                        path: self.path.display().to_string(),
                    });
                }
                return Ok(None);
            }
            self.line_no += 1;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                self.blank_lines += 1;
                continue;
            }

            match serde_json::from_str::<AcFrame>(trimmed) {
                Ok(frame) => {
                    // The plausibility guard runs on the first frame only: a
                    // struct-layout drift is a property of the file, and the
                    // check costs nothing to skip thereafter.
                    if !self.validated {
                        validate_frame(&frame)?;
                        self.validated = true;
                        self.session = Some(SessionInfo::from_frame(&frame));
                    }
                    self.frames += 1;
                    return Ok(Some(frame));
                }
                Err(source) => {
                    // Failing on the very first line means the file is not what
                    // we think it is — a schema change, or not AC telemetry at
                    // all. Report that immediately with the serde message,
                    // which names the offending field.
                    if self.frames == 0 {
                        return Err(CoachError::Json {
                            path: self.path.display().to_string(),
                            line: self.line_no,
                            source,
                        });
                    }
                    self.bad_lines += 1;
                    // Deliberately not printed per line: on a corrupt capture
                    // that would emit one message per frame, tens of thousands
                    // of them. The count is reported once, by the caller.
                    let seen = self.frames + self.bad_lines;
                    if self.bad_lines as f64 / seen as f64 > MAX_BAD_LINE_FRACTION && seen > 100 {
                        return Err(CoachError::Json {
                            path: self.path.display().to_string(),
                            line: self.line_no,
                            source,
                        });
                    }
                }
            }
        }
    }

    fn session(&self) -> Option<&SessionInfo> {
        self.session.as_ref()
    }

    fn describe(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        match &self.session {
            Some(s) => format!(
                "{name} — {} in {} ({} m, AC {}, SM {})",
                s.car, s.track, s.track_length, s.ac_version, s.sm_version
            ),
            None => format!("{name} — not yet read"),
        }
    }
}
