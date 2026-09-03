//! The per-sim screens: the home sheet of actions, and the small screens
//! that gather what each action needs before it runs.
//!
//! Clicking a sim no longer starts an attach — it opens the sim's home
//! screen, the whole CLI surface as buttons in the order a new track is
//! actually worked: capture telemetry, learn the model from it, then drive
//! and review against it. Every action carries a "?" with its fuller
//! explanation, because a tool with seven verbs needs each verb to say
//! what it does.
//!
//! The screens are deliberately dumb: they gather a path or a lap count
//! and hand the decision back as a return value; the launcher ([`crate::ui::launcher`])
//! owns the transitions and the threads, which is what keeps the phase
//! logic testable without a window (same arrangement as the attach flow).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eframe::egui;

use crate::core::settings::{CAPTURES_DIR, Settings};
use crate::ui::theme;

/// The home screen of one sim: every command the CLI knows, as buttons,
/// plus the settings that govern Coach Live and sharing.
#[derive(Debug, Clone)]
pub struct SimHome {
    /// The provider's registry key — how the launcher gets back from here
    /// to the provider this screen belongs to.
    pub sim_key: String,
    /// What the driver calls the sim, for the heading.
    pub sim_name: String,
    /// The consent dialog is open: the driver ticked "Share telemetry" and
    /// has not yet answered. The setting does not change until they do.
    pub(crate) share_dialog: bool,
}

impl SimHome {
    pub fn new(sim_key: &str, sim_name: &str) -> Self {
        Self {
            sim_key: sim_key.to_string(),
            sim_name: sim_name.to_string(),
            share_dialog: false,
        }
    }
}

/// What the home screen asked for. `None` means "nothing happened this
/// frame" — egui redraws constantly, so most frames are exactly that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeAction {
    None,
    Back,
    Record,
    Inspect,
    Learn,
    LearnPb,
    Analyse,
    Export,
    CoachLive,
}

impl SimHome {
    /// Draw the sheet. The settings are passed in (not held here) because
    /// they belong to the window, not to one sim's home — `coach live` from
    /// a terminal reads the same file.
    pub fn render(&mut self, ui: &mut egui::Ui, settings: &mut Settings) -> HomeAction {
        let mut action = HomeAction::None;

        ui.add_space(4.0);
        ui.heading(&self.sim_name);
        ui.add_space(2.0);

        // Two cards a row, equal shares of the width. The workflow's real
        // order — capture, learn, review — is the order the groups appear
        // in, which is the one thing a new track's driver needs to be told.
        let width = (ui.available_width() - 8.0) / 2.0;

        theme::eyebrow(ui, "1 · CAPTURE");
        ui.horizontal(|ui| {
            if theme::action_card(
                ui,
                "Record",
                "Capture telemetry from the running sim",
                HELP_RECORD,
                width,
            ) {
                action = HomeAction::Record;
            }
            if theme::action_card(
                ui,
                "Inspect",
                "See what is in a capture",
                HELP_INSPECT,
                width,
            ) {
                action = HomeAction::Inspect;
            }
        });

        theme::eyebrow(ui, "2 · LEARN");
        ui.horizontal(|ui| {
            if theme::action_card(
                ui,
                "Coach Learn",
                "Build the track model from captures",
                HELP_LEARN,
                width,
            ) {
                action = HomeAction::Learn;
            }
            if theme::action_card(
                ui,
                "Learn PB",
                "Learn your personal-best laps",
                HELP_LEARN_PB,
                width,
            ) {
                action = HomeAction::LearnPb;
            }
        });

        theme::eyebrow(ui, "3 · REVIEW");
        ui.horizontal(|ui| {
            if theme::action_card(
                ui,
                "Analyse",
                "Score a capture against the model",
                HELP_ANALYSE,
                width,
            ) {
                action = HomeAction::Analyse;
            }
            if theme::action_card(
                ui,
                "Export dataset",
                "Sessions to CSV for offline analysis",
                HELP_EXPORT,
                width,
            ) {
                action = HomeAction::Export;
            }
        });

        // Coach Live is the destination — everything above is preparation —
        // so it stands apart: full width, session-best purple. The setting
        // rides beside it because it changes what a live session does.
        ui.add_space(16.0);
        let live = ui.add_sized(
            egui::vec2(ui.available_width(), 40.0),
            egui::Button::new(
                egui::RichText::new("● Coach Live")
                    .size(18.0)
                    .color(theme::PURPLE),
            ),
        );
        if live.clicked() {
            action = HomeAction::CoachLive;
        }

        ui.add_space(8.0);
        let mut record = settings.record_while_coaching;
        ui.checkbox(&mut record, "Record while coaching")
            .on_hover_text(HELP_RECORD_WHILE_COACHING);
        if record != settings.record_while_coaching {
            settings.record_while_coaching = record;
            // Saved the moment it changes: a setting the driver toggles
            // before a session must not depend on remembering to save it.
            if let Err(e) = settings.save() {
                ui.colored_label(theme::RED, format!("could not save settings: {e}"));
            }
        }

        // Sharing never turns on with a tick alone — the tick asks, the
        // dialog answers. Turning it *off* needs no ceremony.
        let mut share = settings.share_telemetry;
        ui.checkbox(&mut share, "Share telemetry with the author")
            .on_hover_text(HELP_SHARE);
        if share != settings.share_telemetry {
            if share {
                self.share_dialog = true;
            } else {
                settings.share_telemetry = false;
                if let Err(e) = settings.save() {
                    ui.colored_label(theme::RED, format!("could not save settings: {e}"));
                }
            }
        }
        if self.share_dialog {
            self.render_share_dialog(ui, settings);
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            if ui.button("‹ All sims").clicked() {
                action = HomeAction::Back;
            }
        });

        action
    }

    /// The consent dialog: what is sent, why, and what never is — said
    /// plainly enough that the tick means something. "Share my telemetry"
    /// is the only way the setting turns on, and it is also when the
    /// install id is minted: an id for a machine that never shared is a
    /// file entry for nothing.
    fn render_share_dialog(&mut self, ui: &mut egui::Ui, settings: &mut Settings) {
        egui::Window::new("Share your telemetry with the author?")
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.set_max_width(420.0);
                ui.label(SHARE_WHAT);
                ui.add_space(6.0);
                ui.label(SHARE_WHY);
                ui.add_space(6.0);
                ui.label(SHARE_NEVER);
                ui.add_space(6.0);
                ui.colored_label(theme::MUTED, SHARE_HONESTY);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Share my telemetry").clicked() {
                        settings.share_telemetry = true;
                        if settings.install_id.is_none() {
                            settings.install_id = Some(Settings::generate_install_id());
                        }
                        if let Err(e) = settings.save() {
                            ui.colored_label(theme::RED, format!("could not save settings: {e}"));
                        }
                        self.share_dialog = false;
                    }
                    if ui.button("Not now").clicked() {
                        self.share_dialog = false;
                    }
                });
            });
    }
}

// ======================================================================
// Capture gathering
// ======================================================================

/// Every capture the tool knows about, newest first: the ones live
/// coaching recorded (`data/captures`) and any sitting in the working
/// directory — where the C# logger and `coach record` write theirs. One
/// list, because a driver should never have to remember which directory a
/// capture landed in before they can learn from it.
pub fn list_captures() -> Vec<PathBuf> {
    captures_in(&[Path::new(CAPTURES_DIR), Path::new(".")])
}

/// [`list_captures`] against explicit directories — the seam the test uses.
/// Only files named like captures (`telemetry_*.ndjson[.gz]`) count; a
/// directory of anything else is not a promise that there is telemetry in
/// it.
fn captures_in(dirs: &[&Path]) -> Vec<PathBuf> {
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // no directory, no captures from it
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            let looks_like_a_capture = name.starts_with("telemetry_")
                && (name.ends_with(".ndjson") || name.ends_with(".ndjson.gz"));
            if !looks_like_a_capture {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((modified, path));
        }
    }
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified)); // newest first
    found.into_iter().map(|(_, path)| path).collect()
}

/// The one capture that Inspect, Analyse and Learn PB all need, chosen
/// once here so the three screens cannot drift apart.
pub struct CaptureScreen {
    pub home: SimHome,
    /// Which command runs on the chosen capture.
    pub action: CaptureAction,
    /// The chosen capture, if any.
    selected: Option<PathBuf>,
    /// A typed path, for captures outside the known directories.
    manual: String,
}

/// The single-capture commands — they share a screen because they share an
/// input; only the title and the run differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureAction {
    Inspect,
    Analyse,
    LearnPb,
}

impl CaptureAction {
    fn title(self) -> &'static str {
        match self {
            CaptureAction::Inspect => "Inspect a capture",
            CaptureAction::Analyse => "Analyse a capture",
            CaptureAction::LearnPb => "Learn personal best",
        }
    }

    fn run_label(self) -> &'static str {
        match self {
            CaptureAction::Inspect => "Inspect",
            CaptureAction::Analyse => "Analyse",
            CaptureAction::LearnPb => "Learn personal best",
        }
    }
}

/// What the capture screen decided.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureOut {
    None,
    Back,
    /// Run this screen's command on this capture.
    Run(PathBuf),
}

impl CaptureScreen {
    pub fn new(home: SimHome, action: CaptureAction) -> Self {
        Self {
            home,
            action,
            selected: None,
            manual: String::new(),
        }
    }

    pub fn render(&mut self, ctx: &egui::Context) -> CaptureOut {
        let mut out = CaptureOut::None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut back = false;
            theme::title_bar(ui, self.action.title(), &mut back);
            if back {
                out = CaptureOut::Back;
            }

            ui.label("Choose a capture:");
            ui.add_space(4.0);
            let captures = list_captures();
            if captures.is_empty() {
                ui.weak("no captures found — record one, or type a path below");
            }
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for path in &captures {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        let chosen = self.selected.as_deref() == Some(path.as_path());
                        if ui.radio(chosen, &name).clicked() {
                            self.selected = Some(path.clone());
                        }
                    }
                });

            ui.add_space(8.0);
            ui.label("…or a path to it:");
            ui.text_edit_singleline(&mut self.manual);

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                if ui
                    .add_enabled(
                        self.chosen().is_some(),
                        egui::Button::new(self.action.run_label()),
                    )
                    .clicked()
                    && let Some(path) = self.chosen()
                {
                    out = CaptureOut::Run(path);
                }
            });
        });
        out
    }

    /// The capture to run on: the picked one, or the typed path when the
    /// driver chose to trust their typing instead of the list.
    fn chosen(&self) -> Option<PathBuf> {
        if !self.manual.trim().is_empty() {
            Some(PathBuf::from(self.manual.trim()))
        } else {
            self.selected.clone()
        }
    }
}

// ======================================================================
// Coach Learn
// ======================================================================

/// The model builder's screen: every capture worth voting on, ticked.
pub struct LearnScreen {
    pub home: SimHome,
    checked: Vec<PathBuf>,
    /// A typed path, added to the list and ticked when the driver presses
    /// Add — for the original logger capture sitting wherever it was copied.
    manual: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LearnOut {
    None,
    Back,
    /// Learn from these captures, oldest input first for a stable
    /// provenance line.
    Run(Vec<PathBuf>),
}

impl LearnScreen {
    pub fn new(home: SimHome) -> Self {
        Self {
            home,
            checked: Vec::new(),
            manual: String::new(),
        }
    }

    pub fn render(&mut self, ctx: &egui::Context) -> LearnOut {
        let mut out = LearnOut::None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut back = false;
            theme::title_bar(ui, "Coach Learn", &mut back);
            if back {
                out = LearnOut::Back;
            }
            ui.label(
                "Tick every capture to learn from — the original plus any this \
                 tool recorded while coaching. More captures, better corners.",
            );
            ui.add_space(4.0);

            let captures = list_captures();
            if captures.is_empty() {
                ui.weak("no captures found — record one, or add a path below");
            }
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for path in &captures {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        let mut ticked = self.checked.contains(path);
                        ui.checkbox(&mut ticked, &name);
                        if ticked && !self.checked.contains(path) {
                            self.checked.push(path.clone());
                        } else if !ticked {
                            self.checked.retain(|p| p != path);
                        }
                    }
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Add a path:");
                let add = ui.add(
                    egui::TextEdit::singleline(&mut self.manual)
                        .desired_width(ui.available_width() - 60.0),
                );
                if ui.button("Add").clicked() && !self.manual.trim().is_empty() {
                    let path = PathBuf::from(self.manual.trim());
                    if !self.checked.contains(&path) {
                        self.checked.push(path);
                    }
                    self.manual.clear();
                }
                add.on_hover_text("A capture outside the known directories");
            });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                if ui
                    .add_enabled(!self.checked.is_empty(), egui::Button::new("Learn"))
                    .clicked()
                {
                    out = LearnOut::Run(self.checked.clone());
                }
            });
        });
        out
    }
}

// ======================================================================
// Record
// ======================================================================

pub struct RecordScreen {
    pub home: SimHome,
    /// Laps to record; zero means "until I press Stop".
    laps: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordOut {
    None,
    Back,
    Start { laps: Option<u32> },
}

impl RecordScreen {
    pub fn new(home: SimHome) -> Self {
        Self { home, laps: 0 }
    }

    pub fn render(&mut self, ctx: &egui::Context) -> RecordOut {
        let mut out = RecordOut::None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut back = false;
            theme::title_bar(ui, "Record", &mut back);
            if back {
                out = RecordOut::Back;
            }

            ui.label(
                "Waits for the sim to start, then captures telemetry in the \
                 logger's own format — inspect, learn and analyse all read it.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Laps:");
                ui.add(egui::DragValue::new(&mut self.laps).range(0..=999));
                ui.weak("(0 = record until stopped)");
            });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                if ui.button("Start recording").clicked() {
                    out = RecordOut::Start {
                        laps: (self.laps > 0).then_some(self.laps),
                    };
                }
            });
        });
        out
    }
}

// ======================================================================
// Export dataset
// ======================================================================

pub struct ExportScreen {
    pub home: SimHome,
    sessions_dir: String,
    out: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExportOut {
    None,
    Back,
    Run { sessions_dir: PathBuf, out: PathBuf },
    /// The driver asked to donate the export — the job re-runs the export
    /// itself (into the bundle), so only the sessions directory is needed.
    Send { sessions_dir: PathBuf },
}

impl ExportScreen {
    pub fn new(home: SimHome) -> Self {
        Self {
            home,
            sessions_dir: "data/sessions".to_string(),
            out: "data/dataset.csv".to_string(),
        }
    }

    /// `sharing` is whether the driver consented — the Send button only
    /// exists for them, and its absence says why rather than sitting there
    /// dead.
    pub fn render(&mut self, ctx: &egui::Context, sharing: bool) -> ExportOut {
        let mut out = ExportOut::None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut back = false;
            theme::title_bar(ui, "Export dataset", &mut back);
            if back {
                out = ExportOut::Back;
            }

            ui.label(
                "One CSV row per corner pass across every recorded coaching \
                 session, joined with the track model and any personal best.",
            );
            ui.add_space(8.0);
            ui.label("Sessions directory:");
            ui.text_edit_singleline(&mut self.sessions_dir);
            ui.label("Output CSV:");
            ui.text_edit_singleline(&mut self.out);

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                let ready = !self.sessions_dir.trim().is_empty()
                    && !self.out.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new("Export"))
                    .clicked()
                {
                    out = ExportOut::Run {
                        sessions_dir: PathBuf::from(self.sessions_dir.trim()),
                        out: PathBuf::from(self.out.trim()),
                    };
                }
                // The send is the donation's one explicit act — never
                // automatic, never hidden behind the export itself.
                if sharing {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.sessions_dir.trim().is_empty(),
                                egui::Button::new("Send to author"),
                            )
                            .clicked()
                        {
                            out = ExportOut::Send {
                                sessions_dir: PathBuf::from(self.sessions_dir.trim()),
                            };
                        }
                        theme::help(ui, HELP_SEND);
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.weak("Send to author");
                        ui.weak(
                            "— turn on \"Share telemetry\" on the sim's home screen \
                             to donate your data",
                        );
                    });
                }
            });
        });
        out
    }
}

// ======================================================================
// Help texts
// ======================================================================

/// Each is the fuller explanation the CLI carries in `--help`, said in the
/// window's voice — what it does, what it needs, where the result lands.
pub const HELP_RECORD: &str = "Captures telemetry straight from the running sim, in \
    the logger's own file format — inspect, learn and analyse all read it. Waits for \
    the sim to start, then records until the lap count is reached or you press Stop. \
    The capture lands beside the program with the logger's default name \
    (telemetry_ac_<track>_<car>_<stamp>.ndjson.gz).";

pub const HELP_INSPECT: &str = "Reads a capture and reports what is in it — the \
    session, every lap with its quality, and the corners the laps agree on. The quick \
    check before learning from a file.";

pub const HELP_LEARN: &str = "Learns the canonical corner set for a track from the \
    clean laps in one or more captures, and writes the track model the coach coaches \
    from (data/tracks/<track>.json, replacing any previous one). Tick the original \
    capture and any this tool recorded while coaching — the model becomes a picture \
    of everything you have driven.";

pub const HELP_LEARN_PB: &str = "Learns your personal-best reference laps from a \
    capture — the braking points and lines the coach measures you against. Needs a \
    track model for the same track first.";

pub const HELP_ANALYSE: &str = "Describes how a capture's clean laps drove each \
    corner of the track model — the same analysis live coaching does, but all at \
    once after the fact. Needs a track model for the same track first.";

pub const HELP_EXPORT: &str = "Flattens recorded coaching sessions into one CSV row \
    per corner pass, joined with the model's corners and your personal best — the \
    corpus offline analysis learns from.";

pub const HELP_LIVE: &str = "Attaches to the running sim and coaches out loud as \
    you drive — corner passes, braking and apex deltas against your personal best. \
    Waits for the sim to start and the car to go on track.";

pub const HELP_RECORD_WHILE_COACHING: &str = "Keeps writing the session's raw \
    telemetry to data/captures while coaching, so you can re-learn the model from it \
    later. Costs a few MB per session.";

pub const HELP_SHARE: &str = "Sends your exported corner-pass data to the coach's \
    author when you choose Send — nothing is sent automatically. Grows the corpus the \
    neural coach will train on. The dialog on first tick says exactly what goes.";

pub const HELP_SEND: &str = "Packs the same export the Export button writes into a \
    donation bundle (session names scrubbed, a small manifest added) and sends it to \
    the author — or saves it under data/share when no endpoint is configured. Only \
    available while Share telemetry is on.";

/// The consent dialog's three answers. One paragraph each, no hedging: the
/// tick has to mean the same thing to the driver as it does to the code.
pub const SHARE_WHAT: &str = "What gets sent: one CSV row per corner pass — \
    speeds, braking points, apexes, times — plus a short manifest (the coach's \
    version, the track and car names, and this install's random id). Nothing is \
    sent until you press Send to author on the export screen.";

pub const SHARE_WHY: &str = "Why: one driver's laps cannot train the neural \
    coach. Every donation grows a corpus that trains a coach shipped back to \
    everyone — the same loop your own sessions already run on, one level up.";

pub const SHARE_NEVER: &str = "What never gets sent: your name, hardware, \
    settings, or raw captures — the raw telemetry files contain your player \
    name and stay on this machine. Session names are scrubbed from the data \
    before it leaves. Turn this off at any time and nothing further is sent.";

pub const SHARE_HONESTY: &str = "One honest caveat: telemetry has no names, but \
    a driving style is still a fingerprint — the author will be able to \
    recognise this install's driving across uploads. That is what the random \
    install id is for.";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action the home screen offers has a help text worth reading:
    /// non-empty, and saying more than its title does.
    #[test]
    fn every_help_text_says_something() {
        let helps = [
            HELP_RECORD,
            HELP_INSPECT,
            HELP_LEARN,
            HELP_LEARN_PB,
            HELP_ANALYSE,
            HELP_EXPORT,
            HELP_LIVE,
            HELP_RECORD_WHILE_COACHING,
            HELP_SHARE,
            HELP_SEND,
        ];
        for help in helps {
            assert!(help.len() > 80, "too thin: {help}");
        }
    }

    /// The consent dialog is the whole consent: each of its three answers
    /// must be a paragraph that says something, because the tick only means
    /// what the dialog explains.
    #[test]
    fn the_share_dialog_answers_what_why_and_what_not() {
        for text in [SHARE_WHAT, SHARE_WHY, SHARE_NEVER, SHARE_HONESTY] {
            assert!(text.len() > 80, "too thin: {text}");
        }
        // And they must actually be the three different answers.
        assert!(SHARE_WHAT.to_lowercase().starts_with("what gets sent"));
        assert!(SHARE_WHY.to_lowercase().starts_with("why"));
        assert!(SHARE_NEVER.to_lowercase().starts_with("what never gets sent"));
    }

    #[test]
    fn the_capture_list_keeps_only_captures_newest_first() {
        let dir = std::env::temp_dir().join("coach_screens_tests/captures");
        std::fs::create_dir_all(&dir).unwrap();
        let older = dir.join("telemetry_ac_monza_a.ndjson.gz");
        let newer = dir.join("telemetry_ac_monza_b.ndjson");
        let not_a_capture = dir.join("notes.txt");
        let wrong_name = dir.join("monza.ndjson.gz");
        std::fs::write(&older, "").unwrap();
        std::fs::write(&not_a_capture, "").unwrap();
        std::fs::write(&wrong_name, "").unwrap();
        // A moment apart, so the ordering is real and not an accident of
        // equal timestamps.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&newer, "").unwrap();

        let found = captures_in(&[&dir]);
        assert_eq!(found, vec![newer.clone(), older.clone()]);
    }

    #[test]
    fn a_missing_directory_is_simply_no_captures() {
        let missing = std::env::temp_dir().join("coach_screens_tests/missing");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(captures_in(&[&missing]).is_empty());
    }

    #[test]
    fn a_typed_path_wins_over_the_picked_capture() {
        let mut screen = CaptureScreen::new(
            SimHome::new("ac", "Assetto Corsa"),
            CaptureAction::Inspect,
        );
        assert_eq!(screen.chosen(), None);
        screen.selected = Some(PathBuf::from("data/old.ndjson.gz"));
        assert_eq!(screen.chosen(), Some(PathBuf::from("data/old.ndjson.gz")));
        // Typing a path is a deliberate override of the list.
        screen.manual = "ndjson_data/other.ndjson.gz".to_string();
        assert_eq!(
            screen.chosen(),
            Some(PathBuf::from("ndjson_data/other.ndjson.gz"))
        );
    }
}
