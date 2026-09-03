//! The window the driver glances at: connection state, the corner the car is
//! in, and a colour-coded feed of spoken advice with the drop/skip counters.
//!
//! [`app`] is the eframe app; this module's public surface is deliberately
//! larger than "open a window" because the parts worth testing — the row
//! model that turns `Advice` into what a row shows, the cap on the feed —
//! are plain data logic, and CI has no display to render on.

pub mod app;
pub mod icon;

pub use app::CoachApp;
pub use icon::window_icon;
