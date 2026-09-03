//! Replay an AC capture file as a telemetry source.
//!
//! A thin adapter over the shared [`NdjsonLines`] machinery: AC's part is the
//! schema (`AcFrame`), the first-frame plausibility guard, the session facts
//! from the first frame's static block, and the frame→sample conversion. The
//! reading discipline — line streaming, gzip, blank lines, the bad-line
//! policy — is format-generic and lives in `telemetry`.

use std::path::{Path, PathBuf};

use crate::core::sample::{Sample, SessionInfo};
use crate::core::Result;
use crate::sims::assetto_corsa::frame::AcFrame;
use crate::sims::assetto_corsa::schema::validate_frame;
use crate::sims::assetto_corsa::sidecar::Sidecar;
use crate::telemetry::ndjson::NdjsonLines;
use crate::telemetry::source::TelemetrySource;
use crate::telemetry::SourceStats;

pub struct NdjsonReplaySource {
    path: PathBuf,
    lines: NdjsonLines,
    session: Option<SessionInfo>,
    sidecar: Option<Sidecar>,
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

        let sidecar = Sidecar::for_capture(&path);
        if let Some(sc) = &sidecar {
            sc.check(&path)?;
        }

        Ok(Self {
            lines: NdjsonLines::open(&path)?,
            path,
            session: None,
            sidecar,
            validated: false,
        })
    }

    pub fn sidecar(&self) -> Option<&Sidecar> {
        self.sidecar.as_ref()
    }
}

impl TelemetrySource for NdjsonReplaySource {
    fn next_sample(&mut self) -> Result<Option<Sample>> {
        // One frame in, one sample out: the conversion is AC's business and
        // stays inside the provider, which is the whole point of the source
        // trait yielding samples rather than frames.
        let frame = self.lines.next(|s| serde_json::from_str::<AcFrame>(s))?;
        let Some(frame) = frame else {
            return Ok(None);
        };

        // The plausibility guard runs on the first frame only: a struct-layout
        // drift is a property of the file, and the check costs nothing to skip
        // thereafter. The session comes from the same first frame, and with it
        // the one track length every conversion uses.
        if !self.validated {
            validate_frame(&frame)?;
            self.validated = true;
            self.session = Some(SessionInfo::from_ac_frame(&frame));
        }
        let track_length = self
            .session
            .as_ref()
            .expect("session is set with the first frame")
            .track_length;
        Ok(Some(Sample::from_ac_frame(&frame, track_length)))
    }

    fn session(&self) -> Option<&SessionInfo> {
        self.session.as_ref()
    }

    fn stats(&self) -> crate::telemetry::SourceStats {
        let s = self.lines.stats();
        SourceStats {
            samples: s.parsed,
            bad_lines: s.bad_lines,
            blank_lines: s.blank_lines,
        }
    }

    fn describe(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        match &self.session {
            Some(s) => format!(
                "{name} — {} in {} ({} m, {})",
                s.car, s.track, s.track_length, s.sim_version
            ),
            None => format!("{name} — not yet read"),
        }
    }
}
