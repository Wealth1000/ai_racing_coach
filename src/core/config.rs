//! The one configuration the live path reads.
//!
//! The CLI subcommands each take their own flags (`--step`, `--model-dir`,
//! …), which is right for exploratory offline work. The *live* path is not
//! exploratory: it is the product, and a driver should not be re-assembling
//! its configuration from scattered flags every session. [`CoachConfig`] is
//! the single struct the runtime reads, so Batch 13 (voice) and Batch 14
//! (session recording) extend this struct rather than growing another flag
//! family on `coach live`.

use std::path::PathBuf;

/// Where telemetry comes from.
///
/// Two implementations exist in the plan: replaying a capture file (every
/// batch through 15 is developed and verified against this one, on Linux) and
/// reading Assetto Corsa's shared memory live (Windows only, Batch 16).
#[derive(Debug, Clone)]
pub enum InputDevice {
    /// Stream a recorded capture through the live pipeline as if it were
    /// happening now.
    Replay { capture: PathBuf },
    /// Attach to the running sim's shared-memory telemetry pages through the
    /// provider registry (the `--sim` flag picks which). Windows-only; AC's
    /// reader arrives in Batch 16.
    SharedMemory,
}

/// How advice is delivered to the driver's ears.
///
/// Voice selection and rate are **tuning knobs** that live here — in the one
/// struct the live path reads — rather than as flags re-parsed per run. The
/// CLI's `--voice` only chooses the backend family; a driver who wants the
/// coach to talk faster edits their config once, not every session.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    /// Which sink the consumer builds.
    pub backend: VoiceBackend,
    /// Speech rate, 1.0 = the synthesiser's normal pace. The `tts` crate
    /// clamps to the backend's supported range.
    pub rate: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            backend: VoiceBackend::Tts,
            rate: 1.0,
        }
    }
}

/// The voice backend family. `Null` is what CI and `--voice null` run: the
/// advice is still computed, counted and printed — only the audio is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceBackend {
    /// The OS synthesiser via the `tts` crate. Degrades to counted silence
    /// when no backend exists (see `audio::sink`).
    Tts,
    /// Record and count, speak nothing.
    Null,
}

/// Everything the live pipeline needs besides the learned model and the
/// personal best, which it is handed separately (they are loaded, not
/// configured).
#[derive(Debug, Clone)]
pub struct CoachConfig {
    pub input: InputDevice,
    /// Distance-grid spacing in metres, matching the `--step` of the offline
    /// commands. The track model was learned at a specific step; a different
    /// value here moves grid indices and therefore every measured number.
    pub step_m: f32,
    /// Directory holding `<track>_<layout>.json` models and
    /// `<track>_<layout>_pb.json` personal bests.
    pub models_dir: PathBuf,
    /// How the driver hears the advice.
    pub voice: VoiceConfig,
}
