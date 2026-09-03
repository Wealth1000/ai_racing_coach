//! The live wiring: source thread → bounded channels → pipeline thread →
//! advice and event channels.
//!
//! # Why channels, and why bounded ones
//!
//! The telemetry source (Batch 12: a capture replay; Batch 16: shared
//! memory) and the analysis pipeline run at different rates. Graphics frames
//! arrive at ~38 Hz and analysis is per-corner, so in the steady state
//! neither side notices the other. But a burst — a corner completing, a GC
//! pause, a slow consumer of advice — must not be allowed to reach back into
//! the source: the source's job is to keep up with the sim, and any
//! back-pressure there is dropped telemetry.
//!
//! So both handoffs are bounded, drop-oldest channels ([`send_drop_oldest`]):
//! when the queue is full the oldest item is discarded, the new one takes its
//! place, and a counter ticks. Dropping the *oldest* frame rather than the
//! new one keeps the analysis as close to the present as the channel depth
//! allows, and a dropped frame is survivable by design — the resampler
//! interpolates across the gap exactly as it interpolates across AC's
//! stale-graphics repeats, and at racing speed one frame is ~1.3 m of track.
//!
//! # Thread layout
//!
//! ```text
//! source thread                      pipeline thread                consumer
//! ─────────────                      ────────────────                ────────
//! TelemetrySource → Sample ──(256)──▶ CoachPipeline::on_sample
//!                                    advice ──(64)──▶ advice_rx
//!                                    events ─(1024)─▶ event_rx
//!                                    (channel closes → finish())
//! ```
//!
//! The pipeline thread owns the [`CoachPipeline`]; the consumer owns nothing
//! but [`LiveWiring`]'s receivers and the counters. A failure in the source
//! (a broken capture, a vanished shared-memory block) is captured in a
//! shared slot and surfaced by [`LiveWiring::join`], which also joins both
//! threads — so a caller that drains `advice_rx` to the end and then joins
//! knows the whole session ran, or exactly why it did not.
//!
//! # Shutdown
//!
//! A shared `stop` flag is the one switch that ends a session from the
//! consumer side: [`LiveWiring::join`] sets it before joining, and dropping
//! the wiring sets it too, so a GUI that simply lets its window object go
//! still leaves no threads behind. The threads then wind down in a chain —
//! source stops reading, its sender drops, the pipeline's frame receiver
//! closes, the pipeline flushes and exits — rather than any thread waiting
//! on any other.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use crate::coaching::Advice;
use crate::core::error::CoachError;
use crate::core::sample::Sample;
use crate::runtime::pipeline::{CoachPipeline, RuntimeEvent, Stage};
use crate::telemetry::source::TelemetrySource;

/// Frames the source thread may queue ahead of the pipeline. 256 ≈ 6.8 s of
/// graphics updates — enough to absorb any per-corner burst, small enough
/// that the drop-oldest fallback engages rather than delaying advice.
const FRAME_QUEUE: usize = 256;

/// Advice the pipeline thread may queue ahead of the consumer. Advice goes
/// stale fast (it names a corner the driver just left), so this is deep
/// enough for one messy chicane and no more.
const ADVICE_QUEUE: usize = 64;

/// Session events the pipeline thread may queue ahead of the consumer. A lap
/// produces ~20 passes plus one boundary, so this is about a lap of history
/// for a session logger that briefly falls behind.
const EVENT_QUEUE: usize = 1024;

/// Send onto a bounded channel, dropping the oldest queued item to make room
/// when full.
///
/// `drain` is a clone of the channel's receiving end, used *only* to evict
/// from the sending side — a crossbeam `Sender` cannot receive, and std's
/// `Receiver` is `!Sync`, so this arrangement is the one that makes
/// drop-oldest possible without the consumer's cooperation.
///
/// The drop is counted; the count is the honest signal that the consumer is
/// not keeping up. Never blocks, so back-pressure can never reach the caller
/// — which is the whole point of the live wiring.
///
/// Returns `false` when the consumer is gone, so the *sender* can stop too:
/// a source streaming into a dead pipeline, or a pipeline advising a dead
/// consumer, is work nobody will ever read.
fn send_drop_oldest<T>(
    tx: &Sender<T>,
    drain: &Receiver<T>,
    value: T,
    dropped: &AtomicU64,
) -> bool {
    let mut value = Some(value);
    loop {
        match tx.try_send(value.take().expect("value only None after return")) {
            Ok(()) => return true,
            // Full: evict the oldest queued item and retry. The recv can only
            // fail if the consumer drained the queue in the meantime, in
            // which case the retry send simply succeeds.
            Err(TrySendError::Full(v)) => {
                value = Some(v);
                if drain.try_recv().is_ok() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Consumer gone: session is over. Discard quietly.
            Err(TrySendError::Disconnected(v)) => {
                drop(v);
                return false;
            }
        }
    }
}

/// The consumer's handle on a live session.
///
/// Cheap to hold and `Send`; clone the `Arc` counters only to read them.
///
/// Dropping it stops the session: both threads see the `stop` flag and wind
/// down (see the module docs), so a consumer that walks away — a GUI whose
/// window closed, a caller that hit an error — cannot leak them.
pub struct LiveWiring {
    /// Everything the pipeline decided to deliver, in delivery order. Closes
    /// when the session ends.
    pub advice_rx: Receiver<Advice>,
    /// Lap boundaries and corner passes, in session order. Closes when the
    /// session ends.
    pub event_rx: Receiver<RuntimeEvent>,
    /// Frames evicted from the source→pipeline channel. Zero on a healthy
    /// session.
    pub dropped_frames: Arc<AtomicU64>,
    /// Advice evicted from the pipeline→consumer channel. Non-zero means the
    /// consumer is slower than the coach.
    pub dropped_advice: Arc<AtomicU64>,
    /// Events evicted from the pipeline→consumer channel. Non-zero means the
    /// session logger is not keeping up.
    pub dropped_events: Arc<AtomicU64>,
    /// Set by [`LiveWiring::join`] and by `Drop` to end the session.
    stop: Arc<AtomicBool>,
    /// The source thread's terminal error, if it had one.
    failure: Arc<Mutex<Option<CoachError>>>,
    /// Both threads, taken by [`LiveWiring::join`]. An `Option` because a
    /// type with a `Drop` impl cannot have fields moved out of it.
    handles: Option<(JoinHandle<()>, JoinHandle<()>)>,
}

impl LiveWiring {
    /// Wait for both threads to finish and report the source's failure, if
    /// any. Consumes the wiring; call after `advice_rx` has gone quiet (or
    /// whenever the consumer is done and wants the threads stopped).
    pub fn join(mut self) -> Result<(), CoachError> {
        self.shutdown()
    }

    /// The same stop-and-wait as [`LiveWiring::join`], without consuming the
    /// wiring — for the consumer that keeps it alive past the session's end
    /// (a GUI holds it in its app state until the window closes) but still
    /// wants the threads joined before it answers for them.
    pub fn shutdown(&mut self) -> Result<(), CoachError> {
        // Stop first, then join: the threads must be *told* to end before
        // anyone waits for them to, or a session that could run forever
        // (Batch 16's shared memory) would never return from this call.
        self.stop.store(true, Ordering::Relaxed);
        if let Some((source, pipeline)) = self.handles.take() {
            for handle in [source, pipeline] {
                if let Err(panic) = handle.join() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
        match self.failure.lock().expect("failure slot poisoned").take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for LiveWiring {
    fn drop(&mut self) {
        // The same switch `join` pulls, for the consumer that never calls
        // `join` — dropping the wiring is enough to end the session.
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start a live session: one thread reading the source, one running the
/// pipeline, connected by bounded drop-oldest channels.
///
/// The pipeline's own advice counters ([`CoachPipeline::spoken`]) are not
/// observable from outside its thread; count what arrives on
/// [`LiveWiring::advice_rx`] instead, and read `dropped_*` for what did not.
pub fn spawn(source: Box<dyn TelemetrySource + Send>, pipeline: CoachPipeline) -> LiveWiring {
    let (frame_tx, frame_rx) = bounded::<Sample>(FRAME_QUEUE);
    let (advice_tx, advice_rx) = bounded::<Advice>(ADVICE_QUEUE);
    let (event_tx, event_rx) = bounded::<RuntimeEvent>(EVENT_QUEUE);

    let dropped_frames = Arc::new(AtomicU64::new(0));
    let dropped_advice = Arc::new(AtomicU64::new(0));
    let dropped_events = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let failure: Arc<Mutex<Option<CoachError>>> = Arc::new(Mutex::new(None));

    // Source thread: frames in, samples out. The first frame also fixes the
    // track length (from the session the source discovers on reading it),
    // which is why the conversion happens here rather than in the pipeline:
    // the pipeline is pure Sample-in, advice-out.
    let frames_counter = Arc::clone(&dropped_frames);
    let failure_slot = Arc::clone(&failure);
    let source_stop = Arc::clone(&stop);
    let frame_drain = frame_rx.clone();
    let source_handle = std::thread::spawn(move || {
        let mut source = source;
        let mut track_length: Option<f32> = None;
        while !source_stop.load(Ordering::Relaxed) {
            match source.next_frame() {
                Ok(Some(frame)) => {
                    let length = track_length.get_or_insert_with(|| {
                        source
                            .session()
                            .map(|s| s.track_length)
                            .unwrap_or(frame.track_spline_length)
                    });
                    let sample = Sample::from_ac_frame(&frame, *length);
                    if !send_drop_oldest(&frame_tx, &frame_drain, sample, &frames_counter) {
                        // The pipeline is gone; nothing left to feed.
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    *failure_slot.lock().expect("failure slot poisoned") = Some(e);
                    break;
                }
            }
        }
        // `frame_tx` drops here, closing the pipeline's input.
    });

    // Pipeline thread: samples in, advice and events out. When the frames
    // channel closes, the stream is over and the trailing lap is flushed.
    let advice_counter = Arc::clone(&dropped_advice);
    let event_counter = Arc::clone(&dropped_events);
    let pipeline_stop = Arc::clone(&stop);
    let advice_drain = advice_rx.clone();
    let event_drain = event_rx.clone();
    let pipeline_handle = std::thread::spawn(move || {
        let mut pipeline = pipeline;
        let mut alive = true;
        for sample in frame_rx {
            // One sample at a time: analyse, then hand over the events it
            // produced, then the advice. Events before advice is a guarantee
            // the session logger relies on — a pass record must reach the
            // consumer no later than the advice that pass produced, or the
            // advice cannot be attributed to the right lap.
            let advice = pipeline.on_sample(&sample);
            let events = pipeline.take_events();
            for event in events {
                if !send_drop_oldest(&event_tx, &event_drain, event, &event_counter) {
                    alive = false;
                    break;
                }
            }
            if alive {
                for a in advice {
                    if !send_drop_oldest(&advice_tx, &advice_drain, a, &advice_counter) {
                        alive = false;
                        break;
                    }
                }
            }
            if !alive {
                break;
            }
        }
        if alive && !pipeline_stop.load(Ordering::Relaxed) {
            // The natural end of the stream, not a shutdown: flush the
            // trailing lap the way `coach analyse` would.
            let advice = pipeline.finish();
            let events = pipeline.take_events();
            for event in events {
                if !send_drop_oldest(&event_tx, &event_drain, event, &event_counter) {
                    break;
                }
            }
            for a in advice {
                if !send_drop_oldest(&advice_tx, &advice_drain, a, &advice_counter) {
                    break;
                }
            }
        }
    });

    LiveWiring {
        advice_rx,
        event_rx,
        dropped_frames,
        dropped_advice,
        dropped_events,
        stop,
        failure,
        handles: Some((source_handle, pipeline_handle)),
    }
}

/// A wiring with no threads behind it, for tests that want to drive the
/// consumer side (the GUI's row model, the drain loops) with a pair of
/// channels they control directly.
#[cfg(test)]
pub(crate) fn test_wiring(
    advice_rx: Receiver<Advice>,
    event_rx: Receiver<RuntimeEvent>,
) -> LiveWiring {
    LiveWiring {
        advice_rx,
        event_rx,
        dropped_frames: Arc::new(AtomicU64::new(0)),
        dropped_advice: Arc::new(AtomicU64::new(0)),
        dropped_events: Arc::new(AtomicU64::new(0)),
        stop: Arc::new(AtomicBool::new(false)),
        failure: Arc::new(Mutex::new(None)),
        handles: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::coaching::DecisionConfig;
    use crate::features::reference::ReferenceStore;
    use crate::features::track_model::TrackModel;
    use crate::telemetry::NdjsonReplaySource;

    const MONZA_CAPTURE: &str =
        "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";
    const MONZA_MODEL: &str = "data/tracks/monza.json";

    fn permissive() -> DecisionConfig {
        DecisionConfig {
            corner_cooldown: Duration::ZERO,
            kind_cooldown: Duration::ZERO,
            repetition_limit: u32::MAX,
            info_enabled: true,
        }
    }

    #[test]
    fn send_drop_oldest_evicts_the_oldest_when_full() {
        let (tx, rx) = bounded(2);
        let drain = rx.clone();
        let dropped = AtomicU64::new(0);

        for i in 0..3 {
            send_drop_oldest(&tx, &drain, i, &dropped);
        }

        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Ok(2));
        assert!(rx.try_recv().is_err(), "only the newest two survive");
    }

    #[test]
    fn send_drop_oldest_discards_quietly_when_disconnected() {
        let (tx, rx) = bounded(2);
        let drain = rx.clone();
        drop(rx);
        let dropped = AtomicU64::new(0);

        send_drop_oldest(&tx, &drain, "gone", &dropped);

        assert_eq!(dropped.load(Ordering::Relaxed), 0, "no eviction happened");
    }

    /// The threaded wiring must deliver exactly what driving the same
    /// pipeline by hand delivers, and drop nothing: same samples, same
    /// advice, same order.
    #[test]
    fn threaded_session_equals_direct_drive() {
        let Ok(model) = TrackModel::load(MONZA_MODEL) else {
            eprintln!("skipping: {MONZA_MODEL} not present");
            return;
        };
        let Ok(source) = NdjsonReplaySource::open(MONZA_CAPTURE) else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };

        // Direct drive: the same pipeline, fed frames in-line.
        let config = crate::core::CoachConfig {
            input: crate::core::InputDevice::Replay {
                capture: MONZA_CAPTURE.into(),
            },
            step_m: model.provenance.step_m,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let direct_reference = ReferenceStore::empty(&model);
        let mut direct = CoachPipeline::new(model.clone(), direct_reference, config.clone())
            .with_decision_config(permissive());
        let mut expected = Vec::new();
        let mut replay = NdjsonReplaySource::open(MONZA_CAPTURE).expect("reopen capture");
        let mut track_length: Option<f32> = None;
        while let Some(frame) = replay.next_frame().expect("read capture") {
            let length = track_length.get_or_insert_with(|| {
                replay
                    .session()
                    .map(|s| s.track_length)
                    .unwrap_or(frame.track_spline_length)
            });
            expected.extend(direct.on_sample(&Sample::from_ac_frame(&frame, *length)));
        }
        expected.extend(direct.finish());

        // Threaded: the same source through `spawn`. The consumer must be
        // slower than the producer here only by scheduling, never by
        // processing, so nothing may be dropped.
        let live_reference = ReferenceStore::empty(&model);
        let live = CoachPipeline::new(model, live_reference, config)
            .with_decision_config(permissive());
        let wiring = spawn(Box::new(source), live);

        let mut delivered = Vec::new();
        while let Ok(advice) = wiring.advice_rx.recv() {
            delivered.push(advice);
        }
        let dropped_frames = wiring.dropped_frames.load(Ordering::Relaxed);
        let dropped_advice = wiring.dropped_advice.load(Ordering::Relaxed);
        wiring.join().expect("session must end cleanly");

        assert_eq!(
            dropped_frames, 0,
            "the pipeline is faster than the source; nothing may be dropped"
        );
        assert_eq!(
            dropped_advice, 0,
            "the consumer keeps up with advice"
        );
        assert_eq!(delivered, expected);
    }

    /// The GUI's exit path: it holds `LiveWiring` in its app state, so the
    /// window closing drops the wiring — and that alone must end both
    /// threads. The observer is a clone of `advice_rx` kept alive past the
    /// drop: when the pipeline thread exits, its sender goes with it and the
    /// channel reports disconnection. That is only possible once the source
    /// thread has stopped reading too (its sender close is what ends the
    /// pipeline), so one disconnection proves both threads finished.
    #[test]
    fn dropping_the_wiring_stops_both_threads() {
        use crossbeam_channel::RecvTimeoutError;

        let Ok(model) = TrackModel::load(MONZA_MODEL) else {
            eprintln!("skipping: {MONZA_MODEL} not present");
            return;
        };
        let Ok(source) = NdjsonReplaySource::open(MONZA_CAPTURE) else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };
        let config = crate::core::CoachConfig {
            input: crate::core::InputDevice::Replay {
                capture: MONZA_CAPTURE.into(),
            },
            step_m: model.provenance.step_m,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let reference = ReferenceStore::empty(&model);
        let pipeline =
            CoachPipeline::new(model, reference, config).with_decision_config(permissive());
        let wiring = spawn(Box::new(source), pipeline);
        let advice_rx = wiring.advice_rx.clone();

        drop(wiring);

        // Drain until the channel closes. The stop flag was set by the drop,
        // so the source stops reading and the pipeline drains and exits
        // within moments; 30 s is generous, and a timeout means a leaked
        // thread — the exact failure this test exists to catch.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match advice_rx.recv_timeout(Duration::from_secs(1)) {
                // Leftover advice from before the stop: keep draining.
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "threads did not terminate within 30 s of dropping the wiring"
                    );
                }
            }
        }
    }
}
