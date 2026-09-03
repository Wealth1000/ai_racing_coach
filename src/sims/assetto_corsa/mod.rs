//! The Assetto Corsa provider.
//!
//! Owns everything AC-specific: the NDJSON capture schema the C# logger
//! writes ([`frame`]), the first-frame plausibility guard ([`schema`]), the
//! replay source ([`replay`]), the logger's `.meta.json` sidecar
//! ([`sidecar`]), the shared-memory page layouts and frame assembly
//! ([`shared_memory`]), and the conversions that turn AC frames into the
//! canonical [`Sample`] and [`SessionInfo`]. The shared-memory reader's
//! Windows mapping layer and `coach record` land here too, as more
//! [`TelemetrySource`] implementations behind the same conversion.
//!
//! [`Sample`]: crate::core::sample::Sample
//! [`SessionInfo`]: crate::core::sample::SessionInfo
//! [`TelemetrySource`]: crate::telemetry::source::TelemetrySource

pub mod convert;
pub mod frame;
pub mod record;
pub mod replay;
pub mod schema;
pub mod shared_memory;
pub mod sidecar;

pub use frame::{AcFrame, AcStatus};
pub use replay::NdjsonReplaySource;
pub use shared_memory::{FrameAssembler, SkipReason};
pub use sidecar::Sidecar;

use std::path::Path;

use crate::core::error::CoachError;
use crate::core::sample::Sim;
use crate::core::Result;
use crate::sims::{CaptureOpen, SimProvider};
#[cfg(windows)]
use crate::sims::{RecordOptions, RecordSummary};
use crate::telemetry::{PrefixedSource, TelemetrySource};

/// The Assetto Corsa provider. A unit struct: everything it serves is in the
/// modules above, and a provider has no state worth holding.
pub struct AssettoCorsa;

impl SimProvider for AssettoCorsa {
    fn sim(&self) -> Sim {
        Sim::AssettoCorsa
    }

    fn open_capture(&self, path: &Path) -> Result<CaptureOpen> {
        // The logger's own verdict comes first: if its sidecar recorded a
        // fatal probe failure, the numbers inside are not worth reading —
        // and its warnings are worth saying even when the capture is fine.
        let mut source = NdjsonReplaySource::open(path)?;
        if let Some(sidecar) = source.sidecar() {
            for warning in sidecar.warnings() {
                eprintln!("warning: {warning}");
            }
        }

        // Claim by reading the first sample. A JSON parse failure on the
        // first line means the file is not AC telemetry — some other sim's
        // format, or not a capture at all — so decline and let the caller
        // try the next provider. Anything else (the file is unreadable, or
        // the line *parsed* but the plausibility guard rejected it) is an
        // error about a file that is ours, and propagates.
        match source.next_sample() {
            Ok(Some(sample)) => Ok(CaptureOpen::Claimed(Box::new(PrefixedSource::new(
                sample,
                Box::new(source),
            )))),
            Ok(None) => Err(CoachError::EmptyCapture {
                path: path.display().to_string(),
            }),
            Err(CoachError::Json { source, .. }) => Ok(CaptureOpen::Declined(format!(
                "first line is not an AC frame: {source}"
            ))),
            Err(other) => Err(other),
        }
    }

    /// On Windows: attach to AC's shared-memory pages. The source starts in
    /// the waiting state — "the sim is not running yet" is not an error but
    /// the whole first phase of a live session, so `live()` itself can never
    /// fail and the caller is free to construct it the moment the driver
    /// picks this sim. Other builds keep the trait default: the pages only
    /// exist on Windows.
    #[cfg(windows)]
    fn live(&self) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(shared_memory::AcSharedMemorySource::new()))
    }

    /// On Windows: record a capture straight off the pages, in the C#
    /// logger's own format. Other builds keep the trait default.
    #[cfg(windows)]
    fn record(&self, opts: &RecordOptions) -> Result<RecordSummary> {
        record::record::<shared_memory::AcPages>(opts)
    }

    /// On Windows: coach live with a session capture running alongside, in
    /// the same logger format `record` writes — one shared-memory reader
    /// feeding both. Other builds keep the trait default (the pages only
    /// exist on Windows), so callers fall back to plain live coaching.
    #[cfg(windows)]
    fn live_with_recording(
        &self,
        out_dir: &Path,
    ) -> Result<Box<dyn TelemetrySource>> {
        Ok(Box::new(
            shared_memory::AcSharedMemorySource::with_recording(out_dir),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONZA_CAPTURE: &str =
        "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";

    #[test]
    fn the_provider_claims_its_own_capture() {
        if !Path::new(MONZA_CAPTURE).exists() {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        }
        let mut source = match AssettoCorsa.open_capture(Path::new(MONZA_CAPTURE)).unwrap() {
            CaptureOpen::Claimed(source) => source,
            CaptureOpen::Declined(reason) => {
                panic!("AC declined its own capture: {reason}")
            }
        };
        let session = source.session().expect("the probe read the first sample");
        assert_eq!(session.sim, Sim::AssettoCorsa);
        assert_eq!(session.track.track, "monza");

        // The probe's sample must be handed back: the claimed stream starts
        // with exactly the sample a direct open would have yielded first.
        let first = source.next_sample().unwrap().expect("first sample");
        let mut direct = NdjsonReplaySource::open(MONZA_CAPTURE).unwrap();
        let expected = direct.next_sample().unwrap().expect("direct first sample");
        assert_eq!(
            first, expected,
            "the probe must not eat the capture's first sample"
        );
    }

    #[test]
    fn the_provider_declines_a_foreign_first_line() {
        let dir = std::env::temp_dir().join("coach_ac_provider_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("foreign.ndjson");
        std::fs::write(&path, "{\"sim\":\"some-other-game\",\"speed_kmh\":42}\n").unwrap();

        match AssettoCorsa.open_capture(&path).unwrap() {
            CaptureOpen::Claimed(_) => panic!("AC must not claim a foreign capture"),
            CaptureOpen::Declined(reason) => {
                assert!(reason.contains("not an AC frame"), "{reason}")
            }
        }
    }
}
