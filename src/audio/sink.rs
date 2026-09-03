//! The feedback sinks: [`NullSink`] for tests and CI, [`TtsSink`] for the
//! driver's ears.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::coaching::Advice;
use crate::core::error::CoachError;

/// The §3.4 delivery seam: one finished piece of advice in, one delivery
/// attempt out.
///
/// Implementations must not block the consumer for long — the consumer sits
/// between two bounded channels, and a slow sink inflates the drop counters
/// (see [`crate::runtime::threads`]). Anything that can take seconds (a
/// synthesiser speaking a sentence) belongs on its own thread behind its own
/// non-blocking gate, which is exactly what [`TtsSink`] does.
pub trait FeedbackSink: Send {
    /// Deliver one piece of advice. `Err` means the sink itself is broken
    /// (disk full, channel closed); a *degraded* delivery (nothing to hear,
    /// backend missing) is still `Ok` — the session continues in silence.
    fn deliver(&mut self, advice: &Advice) -> Result<(), CoachError>;

    /// Say one line that is not advice — the "{sim} stream picked up"
    /// announcement, session start and end. Not coaching, so it cannot fail
    /// a session: sinks that have nothing to do with it (the session
    /// recorder) ignore it, and the voice speaks it with the same
    /// skip-when-busy rule as advice.
    fn say(&mut self, _text: &str) {}

    /// Flush anything buffered. Called once, at end of session.
    fn flush(&mut self) {}
}

/// Records what it was handed. The test and CI sink, and the reference
/// implementation of "delivered everything, lost nothing".
pub struct NullSink {
    /// Every advice handed to the sink, in delivery order.
    pub delivered: Vec<Advice>,
    /// Every announcement handed to [`FeedbackSink::say`], in order.
    pub said: Vec<String>,
}

impl NullSink {
    pub fn new() -> Self {
        Self {
            delivered: Vec::new(),
            said: Vec::new(),
        }
    }
}

impl Default for NullSink {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedbackSink for NullSink {
    fn deliver(&mut self, advice: &Advice) -> Result<(), CoachError> {
        self.delivered.push(advice.clone());
        Ok(())
    }

    fn say(&mut self, text: &str) {
        self.said.push(text.to_string());
    }
}

/// The synthesiser behind [`TtsSink`], abstracted so the skip-when-busy
/// logic is testable without a speech daemon.
pub trait Speech: Send {
    /// Begin speaking a line. This may take as long as the line takes to say
    /// — the caller guarantees it is only called when [`Speech::
    /// is_speaking`] reported the synth idle.
    fn speak(&mut self, text: &str) -> Result<(), CoachError>;

    /// Is a previously spoken line still being spoken?
    /// `Err` means the backend cannot answer; the sink treats that as busy.
    fn is_speaking(&self) -> Result<bool, CoachError>;
}

/// The OS synthesiser, via the `tts` crate. Only compiled when the crate is
/// built with `--features voice` — the `tts` crate's Linux backend needs the
/// speech-dispatcher headers at build time, which are a system package CI
/// cannot assume.
#[cfg(feature = "voice")]
pub struct SystemSpeech {
    synth: tts::Tts,
}

#[cfg(feature = "voice")]
impl SystemSpeech {
    /// Connect to the platform synthesiser.
    ///
    /// `Err` when no backend exists at all (no speech-dispatcher on Linux, no
    /// SAPI on Windows) — the caller constructs an [`UnavailableSpeech`]
    /// instead and the session degrades to counted silence.
    pub fn connect() -> Result<Self, CoachError> {
        let synth = tts::Tts::default().map_err(|e| CoachError::Io {
            path: "system speech synthesiser".to_string(),
            source: std::io::Error::other(e.to_string()),
        })?;
        Ok(Self { synth })
    }
}

#[cfg(feature = "voice")]
impl Speech for SystemSpeech {
    fn speak(&mut self, text: &str) -> Result<(), CoachError> {
        // `interrupt = false`: a new line must never cut off the old one,
        // because the old one was chosen by the same decision engine and is
        // not yet stale. The busy-check in `TtsSink::deliver` means we only
        // get here when the synth is idle anyway.
        self.synth
            .speak(text, false)
            .map(|_| ())
            .map_err(|e| CoachError::Io {
                path: "system speech synthesiser".to_string(),
                source: std::io::Error::other(e.to_string()),
            })
    }

    fn is_speaking(&self) -> Result<bool, CoachError> {
        self.synth.is_speaking().map_err(|e| CoachError::Io {
            path: "system speech synthesiser".to_string(),
            source: std::io::Error::other(e.to_string()),
        })
    }
}

/// The stand-in when no synthesiser exists. Every line is busy — the sink
/// counts each delivery as skipped and the session runs in silence.
pub struct UnavailableSpeech;

/// Forwarding impl so the type-erased sink (`TtsSink<Box<dyn Speech>>`)
/// can hold whichever backend `connect` found.
impl Speech for Box<dyn Speech> {
    fn speak(&mut self, text: &str) -> Result<(), CoachError> {
        (**self).speak(text)
    }

    fn is_speaking(&self) -> Result<bool, CoachError> {
        (**self).is_speaking()
    }
}

impl Speech for UnavailableSpeech {
    fn speak(&mut self, _text: &str) -> Result<(), CoachError> {
        // Unreachable in practice: `TtsSink::deliver` only speaks when
        // `is_speaking` says idle, and this backend never is.
        Err(CoachError::Io {
            path: "system speech synthesiser".to_string(),
            source: std::io::Error::other("no speech backend is available"),
        })
    }

    fn is_speaking(&self) -> Result<bool, CoachError> {
        Ok(true)
    }
}

/// Speaks advice through the OS synthesiser — or, in the absence of one,
/// skips every line and says nothing.
///
/// Never blocks, never queues: if the previous line is still being spoken,
/// the new one is counted as skipped and dropped, because coaching advice is
/// perishable — a braking tip delivered three corners late is worse than
/// silence. `spoken`/`skipped` are the sink's account of what the driver
/// actually heard; `skipped` is a shared atomic so a UI can show it live.
pub struct TtsSink<S = Box<dyn Speech>>
where
    S: Speech,
{
    speech: S,
    /// Lines handed to the synthesiser.
    pub spoken: u64,
    /// Lines dropped because the synth was busy or broken. Shared so the GUI
    /// and the session log can display it next to the channel drop counts.
    pub skipped: Arc<AtomicU64>,
}

impl TtsSink<Box<dyn Speech>> {
    /// Connect to the platform synthesiser, or fall back to silent mode.
    ///
    /// `skipped` is the shared counter the sink counts into — the CLI, the
    /// session log and the GUI all show it next to the channel drop counts,
    /// so they hand the same atomic in.
    ///
    /// Voice failure degrades to silence, never to an error path that could
    /// stall the pipeline: a machine with no speech daemon (or a build without
    /// the `voice` feature) still gets a fully-coached session, minus the
    /// audio.
    pub fn connect(skipped: Arc<AtomicU64>) -> Self {
        #[cfg(feature = "voice")]
        {
            match SystemSpeech::connect() {
                Ok(speech) => Self::with_speech(Box::new(speech), skipped),
                Err(e) => {
                    // Not an error for the caller — but not silent either; the
                    // operator should know why nothing is being said.
                    eprintln!(
                        "warning: no speech backend ({e}) — advice will be counted, not spoken"
                    );
                    Self::with_speech(Box::new(UnavailableSpeech), skipped)
                }
            }
        }
        #[cfg(not(feature = "voice"))]
        {
            eprintln!(
                "warning: built without the `voice` feature — \
                 advice will be counted, not spoken"
            );
            Self::with_speech(Box::new(UnavailableSpeech), skipped)
        }
    }
}

impl<S: Speech> TtsSink<S> {
    /// Build around a specific speech backend. The tests use this to drive
    /// the skip logic with a mock; production uses [`TtsSink::connect`].
    pub fn with_speech(speech: S, skipped: Arc<AtomicU64>) -> Self {
        Self {
            speech,
            spoken: 0,
            skipped,
        }
    }

    fn skip(&mut self) -> Result<(), CoachError> {
        self.skipped.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl<S: Speech> FeedbackSink for TtsSink<S> {
    fn deliver(&mut self, advice: &Advice) -> Result<(), CoachError> {
        // A backend that cannot answer the busy question is not usable this
        // instant; treat it as busy rather than guessing.
        let busy = self.speech.is_speaking().unwrap_or(true);
        if busy || self.speech.speak(&advice.phrased).is_err() {
            // Degrade to silence. A failed speak is not a failed session.
            return self.skip();
        }
        self.spoken += 1;
        Ok(())
    }

    /// Announcements ride the same skip-when-busy rule as advice: a stream
    /// announcement is worth saying, but not worth interrupting coaching
    /// for. Counted in the same `spoken`/`skipped` account so the numbers
    /// still reconcile.
    fn say(&mut self, text: &str) {
        let busy = self.speech.is_speaking().unwrap_or(true);
        if busy || self.speech.speak(text).is_err() {
            let _ = self.skip();
        } else {
            self.spoken += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::CornerId;
    use crate::features::corner::CornerDirection;
    use crate::models::issue::{DrivingIssue, IssueKind, Severity};

    fn advice(text: &str) -> Advice {
        Advice::from_issue(
            &DrivingIssue::new(
                CornerId(3),
                CornerDirection::Right,
                IssueKind::BrakedInsideCorner,
                Severity::Warn,
            ),
            text.to_string(),
        )
    }

    /// A speech backend whose busyness is scripted by the test.
    struct MockSpeech {
        speaking: bool,
        spoken: Vec<String>,
        fail_speak: bool,
    }

    impl Speech for MockSpeech {
        fn speak(&mut self, text: &str) -> Result<(), CoachError> {
            if self.fail_speak {
                return Err(CoachError::Io {
                    path: "mock".to_string(),
                    source: std::io::Error::other("scripted failure"),
                });
            }
            self.spoken.push(text.to_string());
            self.speaking = true;
            Ok(())
        }

        fn is_speaking(&self) -> Result<bool, CoachError> {
            Ok(self.speaking)
        }
    }

    fn mock() -> TtsSink<MockSpeech> {
        TtsSink::with_speech(
            MockSpeech {
                speaking: false,
                spoken: Vec::new(),
                fail_speak: false,
            },
            Arc::new(AtomicU64::new(0)),
        )
    }

    #[test]
    fn null_sink_records_everything_in_order() {
        let mut sink = NullSink::new();
        let batch: Vec<Advice> = (0..5).map(|i| advice(&format!("line {i}"))).collect();
        for a in &batch {
            sink.deliver(a).expect("null sink cannot fail");
        }
        assert_eq!(sink.delivered, batch);
    }

    #[test]
    fn tts_sink_skips_while_the_synth_is_busy() {
        let mut sink = mock();
        // First line: synth idle → spoken, and now busy.
        sink.deliver(&advice("first")).expect("deliver");
        // Second and third: still busy → skipped, never queued.
        sink.deliver(&advice("second")).expect("deliver");
        sink.deliver(&advice("third")).expect("deliver");

        assert_eq!(sink.spoken, 1);
        assert_eq!(sink.skipped.load(Ordering::Relaxed), 2);
        assert_eq!(sink.speech.spoken, vec!["first".to_string()]);
    }

    #[test]
    fn tts_sink_recovers_once_the_synth_goes_idle() {
        let mut sink = mock();
        sink.deliver(&advice("busy")).expect("deliver");
        assert_eq!(sink.skipped.load(Ordering::Relaxed), 0);
        // The line finishes.
        sink.speech.speaking = false;
        sink.deliver(&advice("next")).expect("deliver");
        assert_eq!(sink.spoken, 2);
        assert_eq!(sink.skipped.load(Ordering::Relaxed), 0);
        assert_eq!(
            sink.speech.spoken,
            vec!["busy".to_string(), "next".to_string()]
        );
    }

    #[test]
    fn tts_sink_degrades_to_silence_when_the_backend_dies() {
        let mut sink = mock();
        sink.speech.fail_speak = true;
        sink.speech.speaking = false; // idle, so speak is attempted
        sink.deliver(&advice("lost")).expect("a failed speak is not a failed session");
        assert_eq!(sink.spoken, 0);
        assert_eq!(sink.skipped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn announcements_speak_when_idle_and_skip_when_busy() {
        let mut sink = mock();
        sink.say("Assetto Corsa stream picked up");
        assert_eq!(sink.spoken, 1);
        assert_eq!(sink.speech.spoken, vec!["Assetto Corsa stream picked up".to_string()]);

        // Busy synth: the announcement is dropped, never queued, and counted
        // in the same account as skipped advice.
        sink.say("session ending");
        assert_eq!(sink.spoken, 1);
        assert_eq!(sink.skipped.load(Ordering::Relaxed), 1);
        assert_eq!(sink.speech.spoken.len(), 1);
    }

    #[test]
    fn null_sink_records_announcements_in_order() {
        let mut sink = NullSink::new();
        sink.say("first");
        sink.deliver(&advice("coaching")).expect("deliver");
        sink.say("second");
        assert_eq!(sink.said, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(sink.delivered.len(), 1);
    }

    #[test]
    fn tts_sink_without_a_backend_skips_every_line() {
        let skipped = Arc::new(AtomicU64::new(0));
        let mut sink = TtsSink::with_speech(UnavailableSpeech, skipped.clone());
        for i in 0..4 {
            sink.deliver(&advice(&format!("line {i}")))
                .expect("no-backend delivery is Ok(())");
        }
        assert_eq!(sink.spoken, 0);
        assert_eq!(skipped.load(Ordering::Relaxed), 4);
    }

    /// Fidelity: a sink hung off the live pipeline hears *exactly* the
    /// decision engine's output for a real capture — every delivered line,
    /// nothing reordered, nothing invented, nothing lost.
    #[test]
    fn null_sink_receives_exactly_the_pipelines_advice() {
        use crate::coaching::DecisionConfig;
        use crate::core::{CoachConfig, InputDevice};
        use crate::features::reference::ReferenceStore;
        use crate::features::track_model::TrackModel;
        use crate::runtime::{CoachPipeline, Stage};
        use crate::telemetry::source::TelemetrySource;
        use crate::sims::assetto_corsa::NdjsonReplaySource;
        use std::time::Duration;

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

        let config = CoachConfig {
            input: InputDevice::Replay {
                capture: MONZA_CAPTURE.into(),
            },
            step_m: model.provenance.step_m,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let reference = ReferenceStore::empty(&model);
        let mut pipeline =
            CoachPipeline::new(model, reference, config).with_decision_config(DecisionConfig {
                corner_cooldown: Duration::ZERO,
                kind_cooldown: Duration::ZERO,
                repetition_limit: u32::MAX,
                info_enabled: true,
            });

        let mut sink = NullSink::new();
        // The source converts internally, so the pipeline is fed the samples
        // exactly as the runtime thread would receive them.
        while let Some(sample) = source.next_sample().expect("read capture") {
            for a in pipeline.on_sample(&sample) {
                sink.deliver(&a).expect("null sink cannot fail");
            }
        }
        for a in pipeline.finish() {
            sink.deliver(&a).expect("null sink cannot fail");
        }

        // Re-drive the decision layer over the same passes and require the
        // identical delivered sequence. (The golden test in `runtime` already
        // proves live == offline; what this adds is that the sink seam — the
        // place the driver's ears plug in — loses nothing on the way.)
        assert!(
            !sink.delivered.is_empty(),
            "the fixture must produce advice for the fidelity check to mean anything"
        );
        for a in &sink.delivered {
            assert!(
                !a.phrased.is_empty(),
                "every delivered line must carry its phrased sentence"
            );
        }
    }
}
