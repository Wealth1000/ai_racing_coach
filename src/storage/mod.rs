//! Session logging and dataset export: what a live session leaves behind.
//!
//! A session file is NDJSON — one record per line, every record
//! self-describing — for the same reason the logger's captures are: a file
//! that can be read line-by-line can also be read *up to* the last complete
//! line, so a crash mid-recording costs the half-written line and nothing
//! else.
//!
//! * [`session`] writes and reads the files: a header, then lap boundaries,
//!   corner passes and delivered advice as they happened.
//! * [`dataset`] flattens a directory of sessions into one CSV row per
//!   corner pass, joined with the track model and any personal best — the
//!   corpus the models and the PB store learn from.
//! * [`share`] packs that corpus into the donation bundle an opted-in
//!   driver sends the author — the corpus-grower for the neural coach.

pub mod dataset;
pub mod session;
pub mod share;

pub use dataset::{DatasetInfo, export_dataset, export_dataset_text};
pub use session::{
    SessionCounters, SessionEvent, SessionHeader, SessionLog, SessionRecord, SessionWriter,
    read_session,
};
