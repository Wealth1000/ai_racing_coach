//! The job runner: one screen shape for every command the library runs.
//!
//! Inspect, learn, analyse, learn-PB, export and record are all the same
//! interaction — a thing runs, text arrives while it runs, and it ends well
//! or with a reason. So they share one screen: a background thread owns the
//! blocking call (the same rule as the attach thread — the UI thread never
//! blocks on work), a [`Progress`] implementation forwards its lines over a
//! channel, and the window only ever drains and draws.
//!
//! A job that finishes after the driver walked away is not an error: the
//! channel is unbounded, so the thread's last send lands harmlessly in a
//! buffer nobody reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;

use crate::commands::Progress;
use crate::ui::theme;

/// What a running job reports. `Done` carries the command's own verdict:
/// the `Ok(())`/`Err` of the library call, stringified for the screen.
pub enum JobMsg {
    Line(String),
    Warn(String),
    Done(Result<(), String>),
}

/// [`Progress`] over the job's channel — the bridge between the library's
/// stdout-shaped reporting and this window.
struct ChannelProgress {
    tx: Sender<JobMsg>,
}

impl Progress for ChannelProgress {
    fn line(&mut self, text: &str) {
        let _ = self.tx.send(JobMsg::Line(text.to_string()));
    }

    fn warn(&mut self, text: &str) {
        let _ = self.tx.send(JobMsg::Warn(text.to_string()));
    }
}

/// One line of output, with its severity kept so warnings can draw amber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLine {
    pub text: String,
    pub warn: bool,
}

/// The cap on the output log — a very long export can out-print a screen's
/// worth of interest, and the tail is what the driver scrolls for.
const MAX_LINES: usize = 2000;

/// The screen a running command draws.
pub struct JobScreen {
    /// What is running, for the title bar ("Coach Learn — monza").
    pub title: String,
    /// The home screen to return to — jobs are always opened *from* one.
    pub(crate) home: crate::ui::screens::SimHome,
    lines: Vec<JobLine>,
    /// `None` while the job runs; its verdict once it ends.
    outcome: Option<Result<(), String>>,
    /// The job's channel. Taken by [`Self::poll`]'s owner; kept here so the
    /// screen owns everything about the job it shows.
    rx: Option<Receiver<JobMsg>>,
    /// The stop flag for jobs that can be stopped (record). `None` for jobs
    /// whose only cancellation is walking away.
    pub stop: Option<Arc<AtomicBool>>,
}

impl JobScreen {
    /// Run `job` on a background thread and open the screen for it.
    ///
    /// `stop`, when given, is both the flag the job checks and the one this
    /// screen's Stop button sets — the same `Arc`, so a click ends the
    /// recording cleanly (flushed, with its gzip trailer) rather than
    /// killing the thread mid-write.
    pub fn spawn(
        title: String,
        home: crate::ui::screens::SimHome,
        stop: Option<Arc<AtomicBool>>,
        job: impl FnOnce(&mut dyn Progress) -> crate::core::Result<()> + Send + 'static,
    ) -> Self {
        let (tx, rx) = unbounded();
        thread::spawn(move || {
            let mut progress = ChannelProgress { tx: tx.clone() };
            let verdict = job(&mut progress).map_err(|e| e.to_string());
            // `Done` after every line, so the screen cannot mark the job
            // finished while output is still in flight.
            let _ = tx.send(JobMsg::Done(verdict));
        });
        Self {
            title,
            home,
            lines: Vec::new(),
            outcome: None,
            rx: Some(rx),
            stop,
        }
    }

    /// True while the command is still running.
    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }

    /// Drain what arrived since the last repaint. Called before drawing, so
    /// a verdict that landed between frames never waits a frame to matter.
    pub fn poll(&mut self) {
        // Taken out for the drain and put back if the job is still running:
        // the channel and the line buffer are both `self`, so only one can
        // be borrowed at a time.
        let Some(rx) = self.rx.take() else { return };
        loop {
            match rx.try_recv() {
                Ok(JobMsg::Line(text)) => self.push(JobLine { text, warn: false }),
                Ok(JobMsg::Warn(text)) => self.push(JobLine { text, warn: true }),
                Ok(JobMsg::Done(verdict)) => {
                    self.outcome = Some(verdict);
                    return; // the channel stays taken: there is nothing more
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    self.rx = Some(rx);
                    return;
                }
                // The job thread only ends by sending `Done`; a channel
                // closed before one means it panicked, and "failed" is a
                // better screen than a spinner that never stops.
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.outcome =
                        Some(Err("the job thread died unexpectedly".to_string()));
                    return;
                }
            }
        }
    }

    fn push(&mut self, line: JobLine) {
        if self.lines.len() >= MAX_LINES {
            self.lines.remove(0);
        }
        self.lines.push(line);
    }

    /// Ask a stoppable job to stop. The thread still owns its shutdown —
    /// this only sets the flag it checks between polls.
    pub fn request_stop(&self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Draw the screen. Returns true when the driver asked to go back.
    pub fn render(&mut self, ctx: &egui::Context) -> bool {
        self.poll();

        let mut back = false;
        egui::TopBottomPanel::top("job_header").show(ctx, |ui| {
            theme::title_bar(ui, &self.title, &mut back);
            ui.horizontal(|ui| {
                match &self.outcome {
                    None => {
                        ui.spinner();
                        ui.weak("working…");
                        if self.stop.is_some() && ui.button("Stop").clicked() {
                            // The recording ends on its own terms: the flag
                            // breaks its loop, the loop flushes, and the
                            // file keeps its trailer.
                            self.request_stop();
                        }
                    }
                    Some(Ok(())) => {
                        ui.colored_label(theme::GREEN, "● done");
                        ui.weak("finished");
                    }
                    Some(Err(reason)) => {
                        ui.colored_label(theme::RED, "● failed");
                        ui.weak(reason);
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.lines.is_empty() && self.running() {
                ui.weak("waiting for the first line…");
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.lines {
                        if line.warn {
                            ui.colored_label(theme::AMBER, &line.text);
                        } else {
                            ui.monospace(&line.text);
                        }
                    }
                });
        });
        back
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> crate::ui::screens::SimHome {
        crate::ui::screens::SimHome::new("ac", "Assetto Corsa")
    }

    #[test]
    fn a_jobs_lines_and_verdict_arrive_through_the_screen() {
        let (tx, rx) = unbounded();
        let mut screen = JobScreen {
            title: "Inspect".to_string(),
            home: home(),
            lines: Vec::new(),
            outcome: None,
            rx: Some(rx),
            stop: None,
        };

        screen.poll();
        assert!(screen.running());
        assert!(screen.lines.is_empty());

        tx.send(JobMsg::Line("session: monza".into())).unwrap();
        tx.send(JobMsg::Warn("sidecar warned".into())).unwrap();
        screen.poll();
        assert_eq!(
            screen.lines,
            vec![
                JobLine { text: "session: monza".into(), warn: false },
                JobLine { text: "sidecar warned".into(), warn: true },
            ]
        );

        tx.send(JobMsg::Done(Ok(()))).unwrap();
        screen.poll();
        assert!(!screen.running());
        assert_eq!(screen.outcome, Some(Ok(())));
    }

    #[test]
    fn a_dead_job_thread_fails_the_screen_instead_of_spinning() {
        let (tx, rx) = unbounded();
        drop(tx);
        let mut screen = JobScreen {
            title: "Coach Learn".to_string(),
            home: home(),
            lines: Vec::new(),
            outcome: None,
            rx: Some(rx),
            stop: None,
        };
        screen.poll();
        assert!(!screen.running());
        assert!(screen.outcome.is_some_and(|r| r.is_err()));
    }

    #[test]
    fn a_jobs_verdict_can_be_an_error_the_screen_shows() {
        let (tx, rx) = unbounded();
        tx.send(JobMsg::Done(Err("no clean laps".into()))).unwrap();
        let mut screen = JobScreen {
            title: "Coach Learn".to_string(),
            home: home(),
            lines: Vec::new(),
            outcome: None,
            rx: Some(rx),
            stop: None,
        };
        screen.poll();
        assert_eq!(screen.outcome, Some(Err("no clean laps".to_string())));
    }

    #[test]
    fn the_output_log_is_capped_from_the_front() {
        let (tx, rx) = unbounded();
        let mut screen = JobScreen {
            title: "Export".to_string(),
            home: home(),
            lines: Vec::new(),
            outcome: None,
            rx: Some(rx),
            stop: None,
        };
        for n in 0..(MAX_LINES + 100) {
            tx.send(JobMsg::Line(format!("line {n}"))).unwrap();
        }
        screen.poll();
        assert_eq!(screen.lines.len(), MAX_LINES);
        assert_eq!(
            screen.lines.first().map(|l| l.text.as_str()),
            Some("line 100"),
            "the newest lines are the ones kept"
        );
    }
}
