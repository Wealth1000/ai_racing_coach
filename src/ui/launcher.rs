//! The GUI's top-level app: pick a sim, then its home screen, then coach.
//!
//! `CoachGui` is the eframe app the window actually runs; the session window
//! of Batch 15 ([`CoachApp`]) is now the last of its phases rather than the
//! whole program. The phases:
//!
//! 1. **Picking** — one button per registered provider. No telemetry exists
//!    yet, so there is nothing else to show.
//! 2. **Home** — the chosen sim's sheet of actions (see [`crate::ui::screens`]):
//!    the whole CLI surface, plus the record-while-coaching setting. Coach
//!    Live is one action among them now, not the only thing a sim is for.
//! 3. **Waiting** — a background thread attaches to the chosen sim and holds
//!    its first `next_sample` until the car is on track. The window says so,
//!    plainly, and can be sent back to the home screen.
//! 4. **Live** — the attach thread resolved the session, loaded the model
//!    and spawned the pipeline; the [`CoachApp`] takes over the window and
//!    the voice announces the pickup.
//! 5. **Failed** — the attach or the setup failed; the reason, and a way
//!    back to the home screen.
//!
//! The UI thread never blocks on the sim: the attach thread and the job
//! threads own every blocking call, and the window only ever polls a
//! channel. A window that freezes while waiting for a game to launch looks
//! exactly like a crash.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;

use crate::audio::{FeedbackSink, TtsSink};
use crate::commands as commands_lib;
use crate::core::config::{CoachConfig, InputDevice};
use crate::core::settings::{CAPTURES_DIR, Settings};
use crate::core::CoachError;
use crate::runtime;
use crate::runtime::threads::LiveWiring;
use crate::sims::{self, SimProvider};
use crate::telemetry::PrefixedSource;
use crate::ui::app::CoachApp;
use crate::ui::job::JobScreen;
use crate::ui::screens::{
    CaptureAction, CaptureOut, CaptureScreen, ExportOut, ExportScreen, HomeAction, LearnOut,
    LearnScreen, RecordOut, RecordScreen, SimHome,
};

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
    /// The chosen sim's action sheet (see [`crate::ui::screens`]).
    Home(SimHome),
    /// Inspect / Analyse / Learn PB — one capture to pick.
    Capture(CaptureScreen),
    /// Coach Learn — every capture to tick.
    Learn(LearnScreen),
    /// Record — a lap count to choose.
    Record(RecordScreen),
    /// Export dataset — the directories to name.
    Export(ExportScreen),
    /// A command running on a background thread, with its output.
    Job(JobScreen),
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

/// The top-level GUI: the sim picker, each sim's home screen, the waiting
/// screen, and the live session behind one window.
pub struct CoachGui {
    phase: GuiPhase,
    /// Where models live — the attach thread needs it to load the session's
    /// model, and the picker does not know the track yet.
    model_dir: PathBuf,
    /// Distance-grid spacing, same role as `--step` on the CLI.
    step: f32,
    /// The persisted settings, shared by every phase: the home screen's
    /// toggle edits them and Coach Live obeys them.
    settings: Settings,
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
            settings: Settings::load(),
        }
    }

    /// Start already coaching — the `--replay` path, where the session exists
    /// before the window does.
    pub fn live(app: CoachApp) -> Self {
        Self {
            phase: GuiPhase::Live(Box::new(app)),
            model_dir: PathBuf::new(),
            step: 0.0,
            settings: Settings::load(),
        }
    }

    /// Go straight to waiting for one provider — the `--sim KEY` path, where
    /// the driver already answered the picker's question on the command line.
    /// Returns false when no provider carries that key: the picker is a
    /// better answer than a "failed" screen for a typo.
    pub fn wait_for(&mut self, key: &str) -> bool {
        let Some(provider) = provider_by_key(key) else {
            return false;
        };
        // The setting applies wherever Coach Live is entered from, command
        // line included — the file is the setting, not the window's memory
        // of it.
        let record = self.settings.record_while_coaching;
        self.begin_attach(provider, record);
        true
    }

    /// A provider by registry key — the way back from a home screen to the
    /// provider that opened it.
    fn provider_of(home: &SimHome) -> Option<&'static dyn SimProvider> {
        provider_by_key(&home.sim_key)
    }

    /// Kick off the attach thread and move to the waiting screen. `record`
    /// is the record-while-coaching setting: the session capture is written
    /// alongside the coaching when the provider can.
    fn begin_attach(&mut self, provider: &'static dyn SimProvider, record: bool) {
        let (tx, rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let sim_name = provider.name().to_string();
        let model_dir = self.model_dir.clone();
        let step = self.step;
        let stop_for_thread = Arc::clone(&stop);
        thread::spawn(move || {
            attach(provider, model_dir, step, record, stop_for_thread, tx)
        });
        self.phase = GuiPhase::Waiting {
            sim_name,
            result: rx,
            stop,
        };
    }

    /// Act on what a home screen asked for. Every action the sheet offers
    /// lands here, so the transitions are plain data logic — testable
    /// without a window, like the attach transitions beside them.
    fn run_home_action(&mut self, home: SimHome, action: HomeAction) {
        match action {
            HomeAction::None => {}
            HomeAction::Back => self.phase = GuiPhase::Picking,
            HomeAction::CoachLive => {
                if let Some(provider) = Self::provider_of(&home) {
                    let record = self.settings.record_while_coaching;
                    self.begin_attach(provider, record);
                }
            }
            HomeAction::Record => {
                self.phase = GuiPhase::Record(RecordScreen::new(home));
            }
            HomeAction::Inspect => {
                self.phase =
                    GuiPhase::Capture(CaptureScreen::new(home, CaptureAction::Inspect));
            }
            HomeAction::Analyse => {
                self.phase =
                    GuiPhase::Capture(CaptureScreen::new(home, CaptureAction::Analyse));
            }
            HomeAction::LearnPb => {
                self.phase =
                    GuiPhase::Capture(CaptureScreen::new(home, CaptureAction::LearnPb));
            }
            HomeAction::Learn => {
                self.phase = GuiPhase::Learn(LearnScreen::new(home));
            }
            HomeAction::Export => {
                self.phase = GuiPhase::Export(ExportScreen::new(home));
            }
        }
    }

    /// Run one capture command as a job. The model dir and step are the
    /// window's, the same defaults the CLI takes.
    fn run_capture_job(&mut self, home: SimHome, action: CaptureAction, capture: PathBuf) {
        let model_dir = self.model_dir.clone();
        let step = self.step;
        let (title, job): (&str, JobFn) = match action {
                CaptureAction::Inspect => (
                    "Inspect",
                    Box::new(move |p| {
                        commands_lib::inspect(&capture, None, step, false, p)
                    }),
                ),
                CaptureAction::Analyse => (
                    "Analyse",
                    Box::new(move |p| {
                        commands_lib::analyse(&capture, None, &model_dir, step, false, p)
                    }),
                ),
                CaptureAction::LearnPb => (
                    "Learn personal best",
                    Box::new(move |p| {
                        commands_lib::learn_pb(&capture, None, &model_dir, step, false, p)
                    }),
                ),
            };
        self.phase = GuiPhase::Job(JobScreen::spawn(
            title.to_string(),
            home,
            None,
            job,
        ));
    }

    /// Run Coach Learn as a job over the ticked captures.
    fn run_learn_job(&mut self, home: SimHome, captures: Vec<PathBuf>) {
        let model_dir = self.model_dir.clone();
        let step = self.step;
        self.phase = GuiPhase::Job(JobScreen::spawn(
            "Coach Learn".to_string(),
            home,
            None,
            move |p| commands_lib::learn_track(&captures, None, &model_dir, step, false, p),
        ));
    }

    /// Run a recording as a job, with the Stop button wired to its stop flag.
    fn run_record_job(&mut self, home: SimHome, laps: Option<u32>) {
        let stop = Arc::new(AtomicBool::new(false));
        let sim = home.sim_key.clone();
        let stop_for_job = Arc::clone(&stop);
        self.phase = GuiPhase::Job(JobScreen::spawn(
            "Record".to_string(),
            home,
            Some(stop),
            move |p| {
                commands_lib::record(None, laps, false, Some(&sim), Some(stop_for_job), p)
            },
        ));
    }

    /// Run the dataset export as a job.
    fn run_export_job(&mut self, home: SimHome, sessions_dir: PathBuf, out: PathBuf) {
        let model_dir = self.model_dir.clone();
        self.phase = GuiPhase::Job(JobScreen::spawn(
            "Export dataset".to_string(),
            home,
            None,
            move |p| commands_lib::export_dataset(&sessions_dir, &out, &model_dir, p),
        ));
    }

    /// The phase's name — for assertions and failure messages, where a name
    /// reads and a debug dump does not.
    #[cfg(test)]
    fn phase_name(&self) -> &'static str {
        match &self.phase {
            GuiPhase::Picking => "Picking",
            GuiPhase::Home(_) => "Home",
            GuiPhase::Capture(_) => "Capture",
            GuiPhase::Learn(_) => "Learn",
            GuiPhase::Record(_) => "Record",
            GuiPhase::Export(_) => "Export",
            GuiPhase::Job(_) => "Job",
            GuiPhase::Waiting { .. } => "Waiting",
            GuiPhase::Live(_) => "Live",
            GuiPhase::Failed { .. } => "Failed",
        }
    }

    /// Poll the attach channel; transition on what arrived. Factored out of
    /// the repaint handler so the transitions are testable without a window.
    fn poll_attach(&mut self) {        let (sim_name, result) = match &self.phase {
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
///
/// `record` is the record-while-coaching setting: the same session capture
/// the logger would write, produced by the same shared-memory reads the
/// coaching is already doing. A provider that cannot record while coaching
/// says so and loses only the byproduct, never the session — the same
/// fallback `coach live` makes on the command line.
fn attach(
    provider: &'static dyn SimProvider,
    model_dir: PathBuf,
    step: f32,
    record: bool,
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

    let live = || provider.live();
    let mut source = if record {
        match provider.live_with_recording(Path::new(CAPTURES_DIR)) {
            Ok(source) => source,
            Err(CoachError::LiveRecordUnsupported { sim }) => {
                eprintln!(
                    "warning: {sim} cannot record while coaching in this build — \
                     coaching without a session capture"
                );
                match live() {
                    Ok(source) => source,
                    Err(e) => return report(AttachResult::Failed(e.to_string())),
                }
            }
            Err(e) => return report(AttachResult::Failed(e.to_string())),
        }
    } else {
        match live() {
            Ok(source) => source,
            Err(e) => return report(AttachResult::Failed(e.to_string())),
        }
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

/// A command the library runs, ready for a job thread — the closure shape
/// [`JobScreen::spawn`] takes.
type JobFn = Box<dyn FnOnce(&mut dyn commands_lib::Progress) -> crate::core::Result<()> + Send>;

/// A provider by its registry key, the way every home screen gets back to
/// the provider that opened it.
fn provider_by_key(key: &str) -> Option<&'static dyn SimProvider> {
    sims::registry().iter().map(|p| &**p).find(|p| p.key() == key)
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
                // Clicking a sim opens its sheet of actions, not an attach:
                // a sim is also the thing you record and learn from, and
                // Coach Live is one button on that sheet.
                if let Some(provider) = chosen {
                    self.phase =
                        GuiPhase::Home(SimHome::new(provider.key(), provider.name()));
                }
            }
            GuiPhase::Home(home) => {
                let mut action = HomeAction::None;
                let frame = egui::Frame::central_panel(&ctx.style())
                    .inner_margin(egui::Margin::same(16));
                egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                    action = home.render(ui, &mut self.settings);
                });
                // Cloned before the call: the phase borrow must end before
                // the phase can change.
                let home = home.clone();
                self.run_home_action(home, action);
            }
            GuiPhase::Capture(screen) => {
                let out = screen.render(ctx);
                let home = screen.home.clone();
                match out {
                    CaptureOut::None => {}
                    CaptureOut::Back => self.phase = GuiPhase::Home(home),
                    CaptureOut::Run(capture) => {
                        let action = screen.action;
                        self.run_capture_job(home, action, capture);
                    }
                }
            }
            GuiPhase::Learn(screen) => {
                let out = screen.render(ctx);
                let home = screen.home.clone();
                match out {
                    LearnOut::None => {}
                    LearnOut::Back => self.phase = GuiPhase::Home(home),
                    LearnOut::Run(captures) => self.run_learn_job(home, captures),
                }
            }
            GuiPhase::Record(screen) => {
                let out = screen.render(ctx);
                let home = screen.home.clone();
                match out {
                    RecordOut::None => {}
                    RecordOut::Back => self.phase = GuiPhase::Home(home),
                    RecordOut::Start { laps } => self.run_record_job(home, laps),
                }
            }
            GuiPhase::Export(screen) => {
                let out = screen.render(ctx);
                let home = screen.home.clone();
                match out {
                    ExportOut::None => {}
                    ExportOut::Back => self.phase = GuiPhase::Home(home),
                    ExportOut::Run { sessions_dir, out } => {
                        self.run_export_job(home, sessions_dir, out);
                    }
                }
            }
            GuiPhase::Job(job) => {
                if job.render(ctx) {
                    // Leaving a running job does not kill it: a learn that
                    // is nearly done should still write its model. A
                    // recording is different — abandoning it without the
                    // stop flag means it records forever — so Back stops it
                    // first and the thread flushes on its own.
                    let home = job.home.clone();
                    job.request_stop();
                    self.phase = GuiPhase::Home(home);
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
        // winds down on its own — and so does a recording, whose thread
        // flushes and finishes its capture once its flag is set.
        if let GuiPhase::Live(app) = &mut self.phase {
            app.shutdown();
        }
        if let GuiPhase::Job(job) = &self.phase {
            job.request_stop();
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

    /// The home sheet is the picker's destination now, and every action on
    /// it lands somewhere real — these are the transitions that make the
    /// CLI's surface a screen.
    #[test]
    fn every_home_action_lands_on_a_screen() {
        type Check = fn(&GuiPhase) -> bool;
        let home = SimHome::new("ac", "Assetto Corsa");
        let cases: [(HomeAction, Check); 7] = [
            (HomeAction::Record, |p| matches!(p, GuiPhase::Record(_))),
            (HomeAction::Inspect, |p| matches!(p, GuiPhase::Capture(_))),
            (HomeAction::Learn, |p| matches!(p, GuiPhase::Learn(_))),
            (HomeAction::LearnPb, |p| matches!(p, GuiPhase::Capture(_))),
            (HomeAction::Analyse, |p| matches!(p, GuiPhase::Capture(_))),
            (HomeAction::Export, |p| matches!(p, GuiPhase::Export(_))),
            (HomeAction::Back, |p| matches!(p, GuiPhase::Picking)),
        ];
        for (action, check) in cases {
            let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
            gui.phase = GuiPhase::Home(home.clone());
            gui.run_home_action(home.clone(), action.clone());
            assert!(
                check(&gui.phase),
                "{action:?} did not land on its screen: {:?}",
                gui.phase_name()
            );
        }
    }

    #[test]
    fn coach_live_from_the_home_sheet_attaches_to_that_sim() {
        let home = SimHome::new("ac", "Assetto Corsa");
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        gui.phase = GuiPhase::Home(home.clone());
        gui.run_home_action(home, HomeAction::CoachLive);
        let GuiPhase::Waiting { sim_name, .. } = &gui.phase else {
            panic!("expected the waiting phase");
        };
        assert_eq!(sim_name, "Assetto Corsa");
    }

    /// Learning is a job like any other: the ticked captures spawn the
    /// library call on a thread, and the screen is the job screen.
    #[test]
    fn learning_from_the_sheet_runs_a_job() {
        let home = SimHome::new("ac", "Assetto Corsa");
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        gui.phase = GuiPhase::Home(home.clone());
        gui.run_learn_job(home, vec![PathBuf::from("ndjson_data/none.ndjson.gz")]);
        let GuiPhase::Job(job) = &gui.phase else {
            panic!("expected the job phase");
        };
        assert_eq!(job.title, "Coach Learn");
        assert!(job.running());
    }

    /// A recording job carries a stop flag — the Stop button has to have
    /// something to stop.
    #[test]
    fn a_recording_from_the_sheet_carries_a_stop_flag() {
        let home = SimHome::new("ac", "Assetto Corsa");
        let mut gui = CoachGui::new(PathBuf::from("data/tracks"), 1.0);
        gui.phase = GuiPhase::Home(home.clone());
        gui.run_record_job(home, Some(3));
        let GuiPhase::Job(job) = &gui.phase else {
            panic!("expected the job phase");
        };
        assert!(job.stop.is_some(), "the Stop button needs the flag");
    }
}
