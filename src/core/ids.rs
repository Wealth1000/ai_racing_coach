//! Newtype identifiers.
//!
//! These exist so that a lap index, a corner index and a raw sample index —
//! which are all `usize`-shaped and all in play at the same time in the corner
//! detector — cannot be passed to each other's functions by mistake.

use std::fmt;

/// Index of a lap within a single session, in the order the laps were driven.
///
/// This is *not* Assetto Corsa's `Graphics_CompletedLaps`. AC's counter lags the
/// start/finish line crossing by 1-2 frames and does not increment at all on the
/// first crossing of a session that began mid-lap, so it cannot be used to
/// delimit laps. See `features::lap`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct LapId(pub u32);

/// Identifies a track *and layout* pair, e.g. `ks_red_bull_ring` +
/// `layout_gp`. The layout matters: the same track folder ships several, with
/// different lengths and different corner counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TrackId {
    pub track: String,
    pub layout: String,
}

/// Ordinal of a corner along the lap, counting from the start/finish line.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct CornerId(pub u32);

/// Identifies one recorded coaching session.
///
/// Generated at session start from the wall clock (millisecond resolution, so
/// two sessions started in the same process do not collide) and used as the
/// session file's name: `<id>.ndjson`. A string rather than a counter so ids
/// stay unique across machines without coordination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    /// A fresh id for a session starting now.
    pub fn generate() -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Self(format!("session_{ms}"))
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for LapId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lap {}", self.0)
    }
}

impl fmt::Display for CornerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 1-based for humans: drivers say "turn 1", never "turn 0".
        write!(f, "T{}", self.0 + 1)
    }
}

impl TrackId {
    pub fn new(track: impl Into<String>, layout: impl Into<String>) -> Self {
        Self {
            track: track.into(),
            layout: layout.into(),
        }
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.layout.is_empty() {
            write!(f, "{}", self.track)
        } else {
            write!(f, "{}/{}", self.track, self.layout)
        }
    }
}
