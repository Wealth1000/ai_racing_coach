//! The eframe app: the driver's window onto a live session.
//!
//! # Design rule: the UI never touches the pipeline
//!
//! The app owns the consumer's end of [`LiveWiring`] — receivers and
//! counters, nothing else. Every repaint it *drains* what has arrived
//! ([`CoachApp::poll`]); it never blocks, never owns the pipeline thread,
//! and when the window closes it drops the wiring, whose own `Drop` stops
//! the threads. A UI thread that stalls the pipeline is a coach that goes
//! quiet mid-corner.
//!
//! Everything testable here is data logic and lives without a window:
//! [`AdviceRow`] is the whole render model, [`CoachApp::poll`] is the
//! consumer loop, and both are exercised by unit tests in CI (rendering
//! itself needs a display, which CI has not got).

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui;
use eframe::egui::{Color32, ScrollArea, Sense};

use crate::audio::{FeedbackSink, NullSink};
use crate::coaching::Advice;
use crate::models::issue::Severity;
use crate::runtime::threads::LiveWiring;
use crate::runtime::RuntimeEvent;

/// How many rows the feed keeps. The feed is a glance surface, not a log —
/// the session file (Batch 14) is the record; the window is the last few
/// corners' worth of "what did it just tell me".
const FEED_CAP: usize = 50;

/// Repaint cadence. Advice is perishable, so 10 Hz — the sentence appears on
/// screen within 100 ms of the corner that produced it, and the UI thread
/// costs the pipeline nothing between repaints. Driven by
/// `request_repaint_after`, never a busy loop.
const REPAINT: Duration = Duration::from_millis(100);

/// One row of the advice feed: everything the renderer needs, nothing it
/// doesn't. Plain data, built from an [`Advice`] the moment it arrives, so
/// rendering cannot fail and cannot be tested headlessly.
#[derive(Debug, Clone, PartialEq)]
pub struct AdviceRow {
    /// `"7 R"` — the corner's id and direction, as the driver knows them.
    pub corner: String,
    /// The fully-resolved sentence, verbatim from the decision layer.
    pub text: String,
    /// The numeric deltas behind the sentence, for hover. Empty string when
    /// the rule that fired measures nothing comparable.
    pub tooltip: String,
    /// Kept unrendered so the colour is chosen at draw time, where the theme
    /// lives.
    pub severity: Severity,
}

impl AdviceRow {
    pub fn from_advice(advice: &Advice) -> Self {
        let mut tooltip = String::new();
        let mut push = |label: &str, delta: Option<f32>| {
            if let Some(v) = delta {
                if !tooltip.is_empty() {
                    tooltip.push('\n');
                }
                tooltip.push_str(&format!("{label} {v:+.1}"));
            }
        };
        push("brake offset (m)", advice.delta_brake_offset_m);
        push("apex speed (m/s)", advice.delta_apex_speed_mps);
        push("apex offset (m)", advice.delta_apex_offset_m);
        push("throttle pickup (m)", advice.delta_throttle_pickup_offset_m);
        push("time in corner (s)", advice.delta_time_s);

        AdviceRow {
            corner: format!("{} {}", advice.corner_id.0, advice.direction.short()),
            text: advice.phrased.clone(),
            tooltip,
            severity: advice.severity,
        }
    }
}

/// Severity → colour. The three must stay visually distinct: the driver
/// triages by colour at a glance and reads the sentence only if the colour
/// says it is worth it.
pub fn severity_colour(severity: Severity) -> Color32 {
    match severity {
        Severity::Info => Color32::from_rgb(0x88, 0x99, 0xaa), // quiet blue-grey
        Severity::Warn => Color32::from_rgb(0xee, 0xbb, 0x33), // amber
        Severity::Critical => Color32::from_rgb(0xe7, 0x4c, 0x3c), // red
    }
}

/// The rendered half of a live session: its consumer end plus a voice.
///
/// Constructed with the [`LiveWiring`] of an already-running session and the
/// [`FeedbackSink`] that speaks its advice — [`CoachApp::new`] defaults to
/// the recording [`NullSink`], which is why the headless tests can drive the
/// whole consumer loop. The window closes when the user says so, and closing
/// it ends the session (see the module docs on shutdown).
///
/// This is not itself an eframe app: [`crate::ui::launcher::CoachGui`] owns
/// the window and renders whichever phase the driver is in — one of which is
/// this app, through [`CoachApp::render`].
pub struct CoachApp {
    wiring: Option<LiveWiring>,
    /// `TelemetrySource::describe()`, captured once — the connection
    /// indicator's text.
    source_desc: String,
    /// The session's voice. Every piece of advice drained in [`CoachApp::poll`]
    /// is delivered here in arrival order, so what the driver hears is what
    /// the feed shows.
    sink: Box<dyn FeedbackSink>,
    /// Newest first, capped at [`FEED_CAP`].
    advice: VecDeque<AdviceRow>,
    /// Every piece of advice that ever arrived, including what the cap has
    /// since evicted — the honest count for the end-of-session summary.
    advice_total: u64,
    /// The lap the session is on, from the most recent boundary.
    lap: Option<(u32, bool)>,
    /// The most recent corner pass: the closest thing to "the corner the car
    /// is in" the event stream offers (passes arrive as they complete).
    last_corner: Option<String>,
    /// True once both channels have closed — the session is over, the window
    /// is showing the tail.
    finished: bool,
}

impl CoachApp {
    /// Wire the app to a running session, silently. `source_desc` is
    /// `TelemetrySource::describe()` — the UI thread never sees the source
    /// itself, only what it calls itself.
    pub fn new(wiring: LiveWiring, source_desc: String) -> Self {
        Self::with_sink(wiring, source_desc, Box::new(NullSink::new()))
    }

    /// Wire the app to a running session with a voice.
    pub fn with_sink(
        wiring: LiveWiring,
        source_desc: String,
        sink: Box<dyn FeedbackSink>,
    ) -> Self {
        CoachApp {
            wiring: Some(wiring),
            source_desc,
            sink,
            advice: VecDeque::new(),
            advice_total: 0,
            lap: None,
            last_corner: None,
            finished: false,
        }
    }

    /// Say one announcement through the session's voice — the "{sim} stream
    /// picked up" the launcher speaks the moment telemetry starts flowing.
    pub fn say(&mut self, text: &str) {
        self.sink.say(text);
    }

    /// Stop the session's threads and join them. Called by the launcher when
    /// the window closes; `Drop` is the net for every other path.
    pub fn shutdown(&mut self) {
        if let Some(mut wiring) = self.wiring.take()
            && let Err(e) = wiring.shutdown()
        {
            eprintln!("warning: the session source failed: {e}");
        }
    }

    /// Drain whatever has arrived since the last look. Never blocks; returns
    /// immediately having taken everything queued. This is the whole consumer
    /// loop, factored out of the repaint handler so a headless test can drive
    /// it. Every drained piece of advice is also handed to the voice, so the
    /// driver hears the session the window is showing.
    pub fn poll(&mut self) {
        let Some(wiring) = &self.wiring else {
            self.finished = true;
            return;
        };

        // Advice first — it is the perishable thing — then the events that
        // keep the status line honest.
        let mut advice_closed = false;
        loop {
            match wiring.advice_rx.try_recv() {
                Ok(advice) => {
                    // The voice hears it before the feed does: a failed
                    // delivery must not lose the row.
                    if let Err(e) = self.sink.deliver(&advice) {
                        eprintln!("warning: the voice failed: {e}");
                    }
                    let row = AdviceRow::from_advice(&advice);
                    self.advice.push_front(row);
                    self.advice_total += 1;
                    while self.advice.len() > FEED_CAP {
                        self.advice.pop_back();
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    advice_closed = true;
                    break;
                }
            }
        }

        let mut events_closed = false;
        loop {
            match wiring.event_rx.try_recv() {
                Ok(RuntimeEvent::LapBoundary { lap, clean, .. }) => {
                    self.lap = Some((lap.0, clean));
                }
                Ok(RuntimeEvent::Pass(f)) => {
                    self.last_corner = Some(format!("{} {}", f.corner_id.0, f.direction.short()));
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    events_closed = true;
                    break;
                }
            }
        }

        if advice_closed && events_closed && !self.finished {
            self.finished = true;
            // A session that ends must not look like one that stalled: say
            // so where a terminal launch can see it too.
            let (frames, advice_dropped, _) = self.counters();
            eprintln!(
                "session ended — {} advice shown, {} frames dropped, {} advice dropped",
                self.advice_total, frames, advice_dropped
            );
        }
    }

    /// True once both channels have closed: the source is done and the
    /// pipeline has flushed. A replay ends here on its own; shared memory
    /// (Batch 16) only ends when the window closes.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// The counters as they stand right now, in display order.
    fn counters(&self) -> (u64, u64, u64) {
        match &self.wiring {
            Some(w) => (
                w.dropped_frames.load(Ordering::Relaxed),
                w.dropped_advice.load(Ordering::Relaxed),
                w.dropped_events.load(Ordering::Relaxed),
            ),
            None => (0, 0, 0),
        }
    }

    fn status_line(&self) -> String {
        let (frames, advice_dropped, events) = self.counters();
        let mut line = self.source_desc.clone();
        if let Some((lap, clean)) = self.lap {
            line.push_str(&format!(
                "   ·   lap {lap}{}",
                if clean { "" } else { " (invalid)" }
            ));
        }
        if let Some(corner) = &self.last_corner {
            line.push_str(&format!("   ·   last corner {corner}"));
        }
        line.push_str(&format!(
            "   ·   dropped: {frames} frames, {advice_dropped} advice, {events} events"
        ));
        line
    }
}

impl CoachApp {
    /// Drain, then draw: the status bar and the advice feed. The repaint
    /// clock itself belongs to whoever owns the window (the launcher), since
    /// the same 10 Hz that drives this feed also drives the waiting screen.
    pub fn render(&mut self, ctx: &egui::Context) {
        self.poll();

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let (frames, _, _) = self.counters();
                let healthy = frames == 0 && !self.finished;
                ui.colored_label(
                    if healthy {
                        Color32::from_rgb(0x2e, 0xcc, 0x71) // green
                    } else if self.finished {
                        Color32::from_rgb(0x88, 0x88, 0x88) // grey: session over
                    } else {
                        Color32::from_rgb(0xe7, 0x4c, 0x3c) // red: dropping
                    },
                    if self.finished { "● done" } else { "● live" },
                );
                ui.label(self.status_line());
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.finished {
                // The session is over (a replay ran out of frames). This
                // must be impossible to mistake for a stall: a frozen feed
                // and a finished feed look identical otherwise.
                ui.heading("Session ended");
                ui.weak(format!(
                    "{} advice shown · {} frames dropped — close the window to exit",
                    self.advice_total,
                    self.counters().0
                ));
                ui.add_space(8.0);
            }
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if self.advice.is_empty() {
                    ui.weak("no advice yet — the coach is listening");
                    return;
                }
                for row in &self.advice {
                    let colour = severity_colour(row.severity);
                    ui.horizontal(|ui| {
                        ui.colored_label(colour, &row.corner);
                        let response = ui
                            .add(egui::Label::new(&row.text).sense(Sense::click()))
                            .on_hover_text(&row.tooltip);
                        if row.tooltip.is_empty() {
                            response.on_disabled_hover_text("no numeric deltas for this rule");
                        }
                    });
                    ui.add_space(2.0);
                }
            });
        });

        // Come back on a clock, not on a whim: without this the window only
        // repaints on input events, and advice that arrives while the driver
        // is not touching the mouse would never render. Once the session has
        // ended there is nothing left to wait for, so the clock slows to a
        // heartbeat — the window still repaints on input — instead of
        // redrawing a static feed at 10 Hz forever.
        ctx.request_repaint_after(if self.finished {
            Duration::from_secs(1)
        } else {
            REPAINT
        });
    }
}

impl Drop for CoachApp {
    fn drop(&mut self) {
        // The window closing normally called `shutdown` already; `Drop` is
        // the net for every other path (a panic in the UI, a caller that
        // built the app but never ran the event loop). Joining can block,
        // but only on a path that is already abnormal — and a briefly
        // blocked exit beats a leaked pipeline thread every time.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_carry_the_corner_the_sentence_and_the_deltas() {
        let advice = Advice {
            corner_id: crate::core::ids::CornerId(7),
            direction: crate::features::corner::CornerDirection::Right,
            kind: crate::models::issue::IssueKind::BrakedInsideCorner,
            severity: Severity::Warn,
            phrased: "brake now earlier".to_string(),
            delta_brake_offset_m: Some(5.25),
            delta_apex_speed_mps: None,
            delta_apex_offset_m: Some(-12.0),
            delta_throttle_pickup_offset_m: None,
            delta_time_s: None,
        };

        let row = AdviceRow::from_advice(&advice);

        assert_eq!(row.corner, "7 R");
        assert_eq!(row.text, "brake now earlier");
        assert_eq!(row.severity, Severity::Warn);
        let tooltip = &row.tooltip;
        assert!(tooltip.contains("brake offset (m) +5.2"), "{tooltip}");
        assert!(tooltip.contains("apex offset (m) -12.0"), "{tooltip}");
        // Absent deltas stay absent — a tooltip that lists "None" is noise.
        assert!(!tooltip.contains("apex speed"), "{tooltip}");
        assert!(!tooltip.contains("throttle"), "{tooltip}");
    }

    #[test]
    fn the_three_severities_are_visually_distinct() {
        // The driver triages by colour before reading; two severities sharing
        // a colour would merge two different urgencies into one signal.
        let colours = [
            severity_colour(Severity::Info),
            severity_colour(Severity::Warn),
            severity_colour(Severity::Critical),
        ];
        assert_ne!(colours[0], colours[1]);
        assert_ne!(colours[0], colours[2]);
        assert_ne!(colours[1], colours[2]);
    }

    #[test]
    fn a_row_without_deltas_has_an_empty_tooltip() {
        let advice = Advice {
            corner_id: crate::core::ids::CornerId(2),
            direction: crate::features::corner::CornerDirection::Left,
            kind: crate::models::issue::IssueKind::NoThrottlePickup,
            severity: Severity::Info,
            phrased: "stay on the throttle through the corner".to_string(),
            delta_brake_offset_m: None,
            delta_apex_speed_mps: None,
            delta_apex_offset_m: None,
            delta_throttle_pickup_offset_m: None,
            delta_time_s: None,
        };

        assert_eq!(AdviceRow::from_advice(&advice).tooltip, "");
    }

    /// The feed is capped: driving 200 rows through a fresh app must leave
    /// exactly [`FEED_CAP`], and they must be the *newest* 200 — the window
    /// shows what the coach just said, not the session's opening lines.
    #[test]
    fn the_feed_keeps_only_the_newest_rows() {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let (_etx, event_rx) = crossbeam_channel::bounded(8);
        let wiring = crate::runtime::threads::test_wiring(rx, event_rx);
        let mut app = CoachApp::new(wiring, "replay test".to_string());

        for i in 0..(FEED_CAP + 150) {
            let advice = Advice {
                corner_id: crate::core::ids::CornerId(i as u32),
                direction: crate::features::corner::CornerDirection::Left,
                kind: crate::models::issue::IssueKind::NoThrottlePickup,
                severity: Severity::Info,
                phrased: format!("line {i}"),
                delta_brake_offset_m: None,
                delta_apex_speed_mps: None,
                delta_apex_offset_m: None,
                delta_throttle_pickup_offset_m: None,
                delta_time_s: None,
            };
            tx.send(advice).expect("consumer is draining");
            app.poll();
        }

        assert_eq!(app.advice.len(), FEED_CAP);
        // Newest first, and the newest is the last one sent.
        assert_eq!(app.advice.front().unwrap().text, format!("line {}", FEED_CAP + 149));
        assert_eq!(app.advice.back().unwrap().text, format!("line {}", 150));
    }

    /// The end-to-end consumer loop over a real capture: poll until the
    /// session ends, then require that the window model agrees with what
    /// the wiring delivered — advice arrived, the last corner was noted,
    /// and a healthy session dropped nothing.
    #[test]
    fn polling_a_replay_to_its_end_fills_the_feed_and_marks_it_finished() {
        use crate::coaching::DecisionConfig;
        use crate::core::{CoachConfig, InputDevice};
        use crate::features::reference::ReferenceStore;
        use crate::features::track_model::TrackModel;
        use crate::runtime::{CoachPipeline, spawn};
        use crate::sims::assetto_corsa::NdjsonReplaySource;
        use crate::telemetry::source::TelemetrySource;

        const MONZA_CAPTURE: &str =
            "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";
        const MONZA_MODEL: &str = "data/tracks/ac/monza.json";

        let Ok(model) = TrackModel::load(MONZA_MODEL) else {
            eprintln!("skipping: {MONZA_MODEL} not present");
            return;
        };
        let Ok(mut source) = NdjsonReplaySource::open(MONZA_CAPTURE) else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };
        // The first sample carries the session facts the header-ish state
        // wants; feed it back through the loop below.
        let mut first = source.next_sample().expect("read capture");
        let desc = source.describe();

        let config = CoachConfig {
            input: InputDevice::Replay {
                capture: MONZA_CAPTURE.into(),
            },
            step_m: model.provenance.step_m,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let reference = ReferenceStore::empty(&model);
        let pipeline =
            CoachPipeline::new(model, reference, config).with_decision_config(DecisionConfig {
                corner_cooldown: Duration::ZERO,
                kind_cooldown: Duration::ZERO,
                repetition_limit: u32::MAX,
                info_enabled: true,
            });

        // A source that yields the peeked sample then the rest — the app
        // construction must not consume the capture.
        struct Prefixed {
            first: Option<crate::core::sample::Sample>,
            inner: NdjsonReplaySource,
        }
        impl TelemetrySource for Prefixed {
            fn next_sample(
                &mut self,
            ) -> crate::core::error::Result<Option<crate::core::sample::Sample>> {
                match self.first.take() {
                    Some(s) => Ok(Some(s)),
                    None => self.inner.next_sample(),
                }
            }
            fn session(&self) -> Option<&crate::core::sample::SessionInfo> {
                self.inner.session()
            }
            fn describe(&self) -> String {
                self.inner.describe()
            }
        }
        let boxed: Box<dyn TelemetrySource + Send> = Box::new(Prefixed {
            first: first.take(),
            inner: source,
        });
        let wiring = spawn(boxed, pipeline);
        let mut app = CoachApp::new(wiring, desc);

        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !app.finished() {
            app.poll();
            std::thread::sleep(Duration::from_millis(20));
            assert!(
                std::time::Instant::now() < deadline,
                "the replay is finite; the app should have seen it end"
            );
        }

        assert!(
            !app.advice.is_empty(),
            "the fixture must produce advice for the check to mean anything"
        );
        assert!(app.last_corner.is_some(), "at least one pass must complete");
        let (frames, advice_dropped, _events) = app.counters();
        assert_eq!(frames, 0, "the UI consumer keeps up with the source");
        assert_eq!(advice_dropped, 0, "the UI consumer keeps up with advice");
        // Newest first: the front row's sentence is one the pipeline said.
        assert!(!app.advice.front().unwrap().text.is_empty());
    }
}
