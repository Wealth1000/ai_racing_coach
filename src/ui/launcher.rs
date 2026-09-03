//! The GUI's top-level app: pick a sim, wait for its telemetry, coach.
//!
//! `CoachGui` is the eframe app the window actually runs; the session window
//! of Batch 15 ([`CoachApp`]) is now the last of its phases rather than the
//! whole program, because a live session no longer starts when the program
//! does — it starts when the driver picks a sim *and* drives out of the
//! garage. The phases:
//!
//! 1. **Picking** — one button per registered provider. No telemetry exists
//!    yet, so there is nothing else to show.
//! 2. **Waiting** — a background thread attaches to the chosen sim and holds
//!    its first `next_sample` until the car is on track. The window says so,
//!    plainly, and can be sent back to the picker.
//! 3. **Live** — the attach thread resolved the session, loaded the model
//!    and spawned the pipeline; the [`CoachApp`] takes over the window and
//!    the voice announces the pickup.
//! 4. **Failed** — the attach or the setup failed; the reason, and a way
//!    back to the picker.
//!
//! The UI thread never blocks on the sim: the attach thread owns every
//! blocking call (attach retry, first sample, model load), and the window
//! only ever polls a channel. A window that freezes while waiting for a game
//! to launch looks exactly like a crash.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;

use crate::audio::{FeedbackSink, TtsSink};
use crate::core::config::{CoachConfig, InputDevice};
use crate::runtime;
use crate::runtime::threads::LiveWiring;
use crate::sims::{self, SimProvider};
use crate::telemetry::PrefixedSource;
use crate::ui::app::CoachApp;

/// Same 10 Hz as the session feed: the waiting screen polls its channel on
/// this clock, so one constant drives the whole window.
const REPAINT: Duration = Duration::from_millis(100);

/// What the attach thread reports back. `Ready` carries a fully-running
/// session — wiring, the source's own description, and the voice that will
/// speak its advice — because everything blocking happens on that thread and
/// the UI thread should only ever receive finished things.
pub enum AttachResult {
    Ready {
        wiring: LiveWiring,
        source_desc: String,
        sink: Box<dyn FeedbackSink>,
    },
    Failed(String),
}

/// Which screen the window is showing.
pub enum GuiPhase {
    /// Choose which sim to coach.
    Picking,
    /// The background thread is attaching to the chosen sim. `stop` is the
    /// flag that interrupts its blocking reads when the driver goes back.
    Waiting {
        sim_name: String,
        result: Receiver<AttachResult>,
        stop: Arc<AtomicBool>,
    },
    /// Coaching. The inner app owns the wiring and renders the session.
    Live(Box<CoachApp>),
    /// The attach or session setup failed. The error, and a way back.
    Failed { sim_name: String, error: String },
}

/// The top-level GUI: the sim picker, the waiting screen, and the live
/// session behind one window.
pub struct CoachGui {
    phase: GuiPhase,
    /// Where models live — the attach thread needs it to load the session's
    /// model, and the picker does not know the track yet.
    model_dir: PathBuf,
    /// Distance-grid spacing, same role as `--step` on the CLI.
    step: f32,
}

impl CoachGui {
    /// Start at the sim picker. `model_dir` and `step` are the same knobs
    /// `coach gui --replay` takes; they ride along until the chosen session
    /// resolves them.
    pub fn new(model_dir: PathBuf, step: f32) -> Self {
        Self {
            phase: GuiPhase::Picking,
            model_dir,
            step,
        }
    }

    /// Start already coaching — the `--replay` path, where the session exists
    /// before the window does.
    pub fn live(app: CoachApp) -> Self {
        Self {
            phase: GuiPhase::Live(Box::new(app)),
            model_dir: PathBuf::new(),
            step: 0.0,
        }
    }

    /// Go straight to waiting for one provider — the `--sim KEY` path, where
    /// the driver already answered the picker's question on the command line.
    /// Returns false when no provider carries that key: the picker is a
    /// better answer than a "failed" screen for a typo.
    pub fn wait_for(&mut self, key: &str) -> bool {
        let Some(provider) = sims::registry().iter().map(|p| &**p).find(|p| p.key() == key)
        else {
            return false;
        };
        self.begin_attach(provider);
        true
    }

    /// Kick off the attach thread and move to the waiting screen.
    fn begin_attach(&mut self, provider: &'static dyn SimProvider) {
        let (tx, rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let sim_name = provider.name().to_string();
        let model_dir = self.model_dir.clone();
        let step = self.step;
        let stop_for_thread = Arc::clone(&stop);
        thread::spawn(move || attach(provider, model_dir, step, stop_for_thread, tx));
        self.phase = GuiPhase::Waiting {
            sim_name,
            result: rx,
            stop,
        };
    }

    /// Poll the attach channel; transition on what arrived. Factored out of
    /// the repaint handler so the transitions are testable without a window.
    fn poll_attach(&mut self) {
        let (sim_name, result) = match &self.phase {
            GuiPhase::Waiting { sim_name, result, .. } => (sim_name.clone(), result),
            _ => return,
        };
        match result.try_recv() {
            Ok(AttachResult::Ready {
                wiring,
                source_desc,
                sink,
            }) => {
                let mut app = CoachApp::with_sink(wiring, source_desc, sink);
                // The driver picked this sim minutes before the stream
                // existed — say the waiting is over, in text and in voice.
                let announcement = format!("{sim_name} stream picked up");
                println!("{announcement}");
                app.say(&announcement);
                self.phase = GuiPhase::Live(Box::new(app));
            }
            Ok(AttachResult::Failed(error)) => {
                self.phase = GuiPhase::Failed { sim_name, error };
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            // The attach thread only ends by sending; a disconnected channel
            // means it panicked, and the waiting screen must not become a
            // frozen one.
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.phase = GuiPhase::Failed {
                    sim_name,
                    error: "the attach thread died unexpectedly".to_string(),
                };
            }
        }
    }
}

/// The attach thread's whole job: everything blocking, off the UI thread.
///
/// Attach → first sample (which holds until the car is on track) → session →
/// model and personal best → pipeline → wiring. Every step's failure is a
/// message for the Failed screen, not a panic: a driver who forgot to learn
/// the track model should read that, not see the window vanish.
fn attach(
    provider: &'static dyn SimProvider,
    model_dir: PathBuf,
    step: f32,
    stop: Arc<AtomicBool>,
    tx: Sender<AttachResult>,
) {
    let report = |r: AttachResult| {
        let _ = tx.send(r); // a closed channel just means the driver went back
    };
    // Building the voice here, not on the UI thread: connecting a speech
    // backend can take long enough to visibly stutter a repaint.
    let voice_skipped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sink: Box<dyn FeedbackSink> = Box::new(TtsSink::connect(voice_skipped));

    let mut source = match provider.live() {
        Ok(source) => source,
        Err(e) => return report(AttachResult::Failed(e.to_string())),
    };
    // So "Back" on the waiting screen interrupts this thread's blocking
    // reads rather than abandoning them forever.
    source.set_stop_flag(stop);

    let first = match source.next_sample() {
        Ok(Some(sample)) => sample,
        Ok(None) => {
            return report(AttachResult::Failed(
                "the sim's stream ended before a single sample".to_string(),
            ))
        }
        Err(e) => return report(AttachResult::Failed(e.to_string())),
    };
    let session = match source.session() {
        Some(session) => session.clone(),
        None => {
            return report(AttachResult::Failed(
                "the stream carried no session (track and car)".to_string(),
            ))
        }
    };

    let model = match runtime::load_model_for_session(&session, &model_dir) {
        Ok(model) => model,
        Err(e) => return report(AttachResult::Failed(e.to_string())),
    };
    let reference = runtime::load_reference_for_session(&session, &model, &model_dir);

    let config = CoachConfig {
        input: InputDevice::SharedMemory,
        step_m: step,
        models_dir: model_dir,
        voice: Default::default(),
    };
    let pipeline = runtime::CoachPipeline::new(model, reference, config);
    let source_desc = source.describe();
    let wiring = runtime::spawn(Box::new(PrefixedSource::new(first, source)), pipeline);
    report(AttachResult::Ready {
        wiring,
        source_desc,
        sink,
    });
}

impl eframe::App for CoachGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Transitions first, drawing second: a result that arrived since the
        // last repaint must not wait a frame to matter, and the waiting
        // screen must never be drawn after it has already ended.
        if matches!(self.phase, GuiPhase::Waiting { .. }) {
            self.poll_attach();
        }

        match &mut self.phase {
            GuiPhase::Picking => {
                // One big button per provider — this is a glance surface used
                // with one hand before a session, not a menu.
                let mut chosen: Option<&'static dyn SimProvider> = None;
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading("Which simulator are you driving?");
                    ui.add_space(12.0);
                    for provider in sims::registry() {
                        let provider: &'static dyn SimProvider = &**provider;
                        let button = egui::Button::new(
                            egui::RichText::new(provider.name()).size(20.0),
                        )
                        .min_size(egui::vec2(ui.available_width(), 40.0));
                        if ui.add(button).clicked() {
                            chosen = Some(provider);
                        }
                    }
                });
                if let Some(provider) = chosen {
                    self.begin_attach(provider);
                }
            }
            GuiPhase::Waiting { sim_name, stop, .. } => {
                let mut back = false;
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading(format!("Waiting for {sim_name}"));
                    ui.add_space(8.0);
                    ui.label(format!(
                        "Waiting, when you are on track in {sim_name}, the results \
                         will show here."
                    ));
                    ui.add_space(12.0);
                    back = ui.button("Back").clicked();
                });
                if back {
                    // Interrupt the attach thread and drop its channel: the
                    // thread may still send its verdict, but nobody is
                    // listening anymore.
                    stop.store(true, Ordering::Relaxed);
                    self.phase = GuiPhase::Picking;
                }
            }
            GuiPhase::Live(app) => app.render(ctx),
            GuiPhase::Failed { sim_name, error } => {
                let mut back = false;
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.heading(format!("{sim_name} could not be coached"));
                    ui.add_space(8.0);
                    // The full reason: these are the "learn the model first"
                    // and "not supported in this build" messages, and a
                    // truncated one teaches nothing.
                    ui.label(error.as_str());
                    ui.add_space(12.0);
                    back = ui.button("Back to the sims").clicked();
                });
                if back {
                    self.phase = GuiPhase::Picking;
                }
            }
        }

        // The waiting screen polls a channel and the feed drains two; both
        // need the same 10 Hz clock the session feed had on its own.
        ctx.request_repaint_after(REPAINT);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // The window is closing: stop the session's threads and join them
        // *now*, so the process exits with nothing running rather than
        // racing the detached pipeline to the exit code. Only the Live phase
        // owns threads; a waiting attach thread observes its stop flag and
        // winds down on its own.
        if let GuiPhase::Live(app) = &mut self.phase {
            app.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_moves_the_waiting_phase_to_failed_with_its_reason() {
        let (tx, rx) = unbounded();
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        gui.phase = GuiPhase::Waiting {
            sim_name: "Assetto Corsa".to_string(),
            result: rx,
            stop: Arc::new(AtomicBool::new(false)),
        };

        // Nothing arrived yet: still waiting.
        gui.poll_attach();
        assert!(matches!(gui.phase, GuiPhase::Waiting { .. }));

        tx.send(AttachResult::Failed("learn the model first".to_string()))
            .unwrap();
        gui.poll_attach();
        let GuiPhase::Failed { sim_name, error } = &gui.phase else {
            panic!("expected the failed phase");
        };
        assert_eq!(sim_name, "Assetto Corsa");
        assert_eq!(error, "learn the model first");
    }

    #[test]
    fn a_dead_attach_thread_becomes_a_failed_screen_not_a_frozen_one() {
        let (tx, rx) = unbounded();
        drop(tx); // the thread "died" without reporting
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        gui.phase = GuiPhase::Waiting {
            sim_name: "Assetto Corsa".to_string(),
            result: rx,
            stop: Arc::new(AtomicBool::new(false)),
        };
        gui.poll_attach();
        assert!(matches!(gui.phase, GuiPhase::Failed { .. }));
    }

    #[test]
    fn wait_for_rejects_an_unknown_key_without_leaving_the_picker() {
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        assert!(!gui.wait_for("no-such-sim"));
        assert!(matches!(gui.phase, GuiPhase::Picking));
    }

    #[test]
    fn wait_for_a_known_key_starts_waiting_for_it() {
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        assert!(gui.wait_for("ac"));
        let GuiPhase::Waiting { sim_name, .. } = &gui.phase else {
            panic!("expected the waiting phase");
        };
        assert_eq!(sim_name, "Assetto Corsa");
    }
}
