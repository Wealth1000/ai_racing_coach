//! The window the driver glances at: connection state, the corner the car is
//! in, and a colour-coded feed of spoken advice with the drop/skip counters.
//!
//! [`launcher`] is the top-level app the window runs — pick a sim, wait for
//! its telemetry, coach — and [`app`] is the session screen it ends on. This
//! module's public surface is deliberately larger than "open a window"
//! because the parts worth testing — the row model that turns `Advice` into
//! what a row shows, the cap on the feed, the phase transitions — are plain
//! data logic, and CI has no display to render on.

pub mod app;
pub mod icon;
pub mod launcher;

pub use app::CoachApp;
pub use icon::window_icon;
pub use launcher::{AttachResult, CoachGui, GuiPhase};
