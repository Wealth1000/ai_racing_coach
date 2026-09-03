pub mod config;
pub mod error;
pub mod ids;
pub mod math;
pub mod sample;
pub mod settings;

pub use config::{CoachConfig, InputDevice, VoiceBackend, VoiceConfig};
pub use error::{CaptureAttempts, CoachError, Result};
pub use ids::{CornerId, LapId, SessionId, TrackId};
pub use sample::{Sample, SessionInfo, Sim};
pub use settings::{CAPTURES_DIR, Settings};
