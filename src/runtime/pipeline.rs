//! The sample-at-a-time analysis pipeline.
//!
//! `coach analyse` reads a whole capture, splits it into laps, and slices
//! each lap at the model's corner boundaries. It is the *reference
//! implementation* of what the coach concludes, and everything about it is
//! convenient: laps sit in memory, the fastest one is picked after the fact,
//! and nothing has to decide anything until the file ends.
//!
//! This module is the same conclusions, produced *live*. Samples arrive one
//! at a time; a lap is never buffered whole; advice leaves the pipeline the
//! moment a corner pass completes. The contract is that the two agree: the
//! golden tests below assert that driving this pipeline over a capture
//! yields, for every clean lap, exactly the feature table — and therefore
//! exactly the advice — that the offline path computes.
//!
//! # How the equality is engineered
//!
//! Every stage shares its arithmetic with the offline path rather than
//! re-implementing it:
//!
//! * the lap-boundary rule is [`LapBoundaryDetector`], the same state machine
//!   [`crate::features::lap::LapTracker`] uses internally;
//! * the streaming resampler calls the offline `interpolate` function itself,
//!   so grid points are bit-identical, and anchors each lap's grid exactly as
//!   `resample_lap` does;
//! * feature extraction is not re-implemented at all: the corner tracker
//!   keeps a *window* of the lap's grid — from `brake_search_m` before a
//!   corner's entry to `throttle_search_m` past its apex — and runs the
//!   offline `extract` on that slice, whose indices are relative to the slice
//!   and therefore land on the same samples.
//!
//! The window is the one place the "no lap buffered whole" rule bends, and it
//! bends by a bounded amount: a window spans one corner plus the two search
//! distances, never a lap. See [`Stage`].

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::coaching::{
    Advice, ControllerMode, DecisionEngine, DefaultPhraser, ThrottlingEngine, advise_pass,
};
use crate::core::config::CoachConfig;
use crate::core::ids::LapId;
use crate::core::sample::Sample;
use crate::features::corner_features::{CornerFeatures, FeatureParams, extract};
use crate::features::lap::{LapBoundaryDetector, LapScorer};
use crate::features::reference::ReferenceStore;
use crate::features::resample::{self, ResampledLap};
use crate::features::track_model::{ModelCorner, TrackModel};
use crate::models::rules::RuleModel;

/// What happened in the session, as it happened — the log of everything the
/// pipeline concluded, advice or not.
///
/// Advice alone is not enough to reconstruct a session: it exists only when a
/// rule fired *and* the gate let it through, while the corner passes and lap
/// boundaries happen regardless. Session logging (Batch 14) records these
/// events so a session can be replayed as data — which is what the dataset
/// export turns into rows.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    /// A lap ended. `time_s` is wall-clock (first-to-last sample), and `clean`
    /// uses the same verdicts and the same ordering as the offline
    /// [`crate::features::lap::LapTracker`].
    LapBoundary {
        lap: LapId,
        time_s: f32,
        clean: bool,
    },
    /// A corner pass completed and was measured. Emitted whether or not any
    /// rule had anything to say about it.
    Pass(CornerFeatures),
}

/// One stage of the live pipeline: one sample in, zero or more outputs out.
///
/// Stages are fed in stream order and must not buffer a whole lap — that is
/// the defining property of the live path. The rule bends for exactly one
/// thing, by design: per-corner feature extraction needs the window from
/// `brake_search_m` before a corner's entry to `throttle_search_m` past its
/// apex. That window is bounded by a corner plus the search distances, never
/// by a lap, and is released as soon as the corner's pass completes.
pub trait Stage {
    type Out;

    /// Feed one sample. Returns whatever this stage produced from it — usually
    /// nothing, sometimes one or several items.
    fn on_sample(&mut self, s: &Sample) -> Vec<Self::Out>;

    /// A start/finish crossing happened; `lap` is the id of the lap that just
    /// ended. Stages reset their per-lap state here.
    fn on_lap_boundary(&mut self, _lap: LapId) -> Vec<Self::Out> {
        Vec::new()
    }
}

/// [`resample::resample_lap`] as a [`Stage`]: same grid, same arithmetic,
/// one sample at a time.
///
/// The grid is anchored exactly as the offline resampler anchors it — at
/// `ceil(first_sample_distance / step) * step` — so grid index *i* means the
/// same absolute distance in both. Stale graphics repeats (equal or backward
/// distance) are dropped on the same rule and counted the same way.
///
/// # Equality guarantee
///
/// Fed the same lap's samples, this stage produces the identical `Vec<Sample>`
/// that `resample_lap` returns for that lap. For a grid point `d` the offline
/// pass advances its segment pointer while the next raw sample sits strictly
/// before `d`, so it interpolates between the last sample below `d` and the
/// first sample at or after `d`. Streaming sees those two samples arrive in
/// order as `prev` and the new sample, and emits every `d` in
/// `(prev.distance, new.distance]` from exactly that pair with exactly the
/// offline `t` — same pair, same parameter, same shared `interpolate`.
pub struct StreamingResampler {
    step_m: f32,
    /// The last accepted raw sample: the `a` of every interpolation until the
    /// next accepted sample arrives.
    prev: Option<Sample>,
    /// Absolute grid index (distance is `gi * step_m`) of the next grid point
    /// to emit.
    next_gi: i64,
    /// Raw samples dropped for being at or behind the previous distance.
    /// Per lap, like the offline counter.
    pub non_monotone_dropped: usize,
}

impl StreamingResampler {
    pub fn new(step_m: f32) -> Self {
        Self {
            step_m,
            prev: None,
            next_gi: 0,
            non_monotone_dropped: 0,
        }
    }

    /// Grid index of the next grid point at or after `distance`.
    fn ceil_gi(&self, distance: f32) -> i64 {
        (distance / self.step_m).ceil() as i64
    }
}

impl Stage for StreamingResampler {
    type Out = Sample;

    fn on_sample(&mut self, s: &Sample) -> Vec<Sample> {
        let mut out = Vec::new();

        let Some(prev) = self.prev else {
            // First sample of a lap: it fixes the grid anchor, exactly as
            // `resample_lap` does.
            self.next_gi = self.ceil_gi(s.lap_distance);
            // A first sample that already sits on the grid is itself the
            // first grid point. The offline pass would interpolate it with
            // t = 0 against the *next* sample, and every channel of
            // `interpolate` is exact at t = 0 (lerp returns `a` unchanged,
            // discrete channels take the nearer sample), so emitting the
            // sample itself is bit-identical.
            if (self.next_gi as f32) * self.step_m == s.lap_distance {
                out.push(*s);
                self.next_gi += 1;
            }
            self.prev = Some(*s);
            return out;
        };

        // Equal-distance frames are the stale-graphics repeats, and
        // interpolating between two identical distances divides by zero.
        if s.lap_distance <= prev.lap_distance {
            self.non_monotone_dropped += 1;
            return out;
        }

        let mut gi = self.next_gi;
        while (gi as f32) * self.step_m <= s.lap_distance {
            let d = gi as f32 * self.step_m;
            let span = s.lap_distance - prev.lap_distance;
            let t = ((d - prev.lap_distance) / span).clamp(0.0, 1.0);
            out.push(resample::interpolate(&prev, s, t, d, self.step_m));
            gi += 1;
        }
        self.next_gi = gi;
        self.prev = Some(*s);
        out
    }

    fn on_lap_boundary(&mut self, _lap: LapId) -> Vec<Sample> {
        // The next sample re-anchors the grid, matching a fresh offline
        // `resample_lap` of the next lap.
        self.prev = None;
        self.non_monotone_dropped = 0;
        Vec::new()
    }
}

/// Streaming lap tracking: the shared wrap rule, plus live lap numbering.
///
/// Ids match [`crate::features::lap::LapTracker`]'s exactly: the opening
/// fragment of a capture is lap 0, and every start/finish crossing closes
/// the lap being driven and starts the next. The id of the lap *being
/// driven* is known from the start — it is the id the offline tracker will
/// assign when the lap closes — so features extracted mid-lap carry the same
/// `lap_id` the offline path stamps on them later.
pub struct LiveLapTracker {
    detector: LapBoundaryDetector,
    track_length: f32,
    current: LapId,
}

impl LiveLapTracker {
    pub fn new(track_length: f32) -> Self {
        Self {
            detector: LapBoundaryDetector::new(),
            track_length,
            current: LapId(0),
        }
    }

    /// The lap currently being driven.
    pub fn current(&self) -> LapId {
        self.current
    }

    /// Clamp one sample in place (the same small-backward-step clamping the
    /// offline tracker applies) and report a start/finish crossing, if this
    /// sample is the first of a new lap.
    pub fn push(&mut self, s: &mut Sample) -> Option<LapId> {
        if self.detector.push(s, self.track_length) {
            let ended = self.current;
            self.current = LapId(self.current.0 + 1);
            Some(ended)
        } else {
            None
        }
    }
}

/// One corner of the frozen model, pre-converted to grid indices.
#[derive(Clone)]
struct CornerGeometry {
    /// Index into the model's corner list, kept so extraction can find the
    /// `ModelCorner` it slices.
    idx: usize,
    /// First grid index the extraction window needs: `start_m` walked back by
    /// the braking search. The offline search reaches back exactly this many
    /// grid points from the braking peak, which is at or after `start_m`, so
    /// nothing below this index is ever read.
    arm_gi: i64,
    /// First grid index past the window: the throttle search (`apex_m` +
    /// `throttle_search_m`) or the corner's own exit, whichever is later.
    /// Nothing at or after this index is ever read.
    done_gi: i64,
}

/// Tracks the model's corners as the car crosses them, one grid sample at a
/// time, and produces each corner's [`CornerFeatures`] the moment its window
/// closes.
///
/// A corner *arms* when the grid reaches `brake_search_m` before its entry
/// and *completes* when the grid passes its throttle-search horizon (or the
/// lap ends, truncating the window exactly as the offline grid of a lap
/// ending there would). Completion calls the offline `extract` on the window
/// — not a streaming re-implementation — so the numbers are the offline
/// numbers.
///
/// Line-straddling corners are two model rows and are tracked as two passes,
/// because that is what the offline path measures; the *advice* layer
/// reports both halves under the parent's id (see [`advise_pass`]).
pub struct CornerTracker {
    corners: Vec<ModelCorner>,
    geometry: Vec<CornerGeometry>,
    params: FeatureParams,
    step_m: f32,
    /// Grid samples of the current lap, from the earliest armed window's
    /// start onward. Bounded by the longest corner plus the two search
    /// distances — never a lap.
    buffer: VecDeque<(i64, Sample)>,
    /// Index into `geometry` of the next corner to arm; corners arm in model
    /// order, which is track order.
    next: usize,
    /// Corners armed but not yet complete. Not necessarily longest-corner-
    /// first, so completion scans all of them.
    armed: VecDeque<CornerGeometry>,
    /// The lap being driven, stamped onto extracted features.
    lap: LapId,
}

impl CornerTracker {
    pub fn new(model: &TrackModel, params: FeatureParams, step_m: f32) -> Self {
        let gi = |m: f32| (m / step_m).round() as i64;
        let back = (params.brake_search_m / step_m).round() as i64;
        let fwd = (params.throttle_search_m / step_m).round() as i64;

        let geometry = model
            .corners
            .iter()
            .enumerate()
            .map(|(idx, c)| CornerGeometry {
                idx,
                arm_gi: gi(c.start_m) - back,
                done_gi: gi(c.apex_m).max(gi(c.end_m)) + fwd,
            })
            .collect();

        Self {
            corners: model.corners.clone(),
            geometry,
            params,
            step_m,
            buffer: VecDeque::new(),
            next: 0,
            armed: VecDeque::new(),
            lap: LapId(0),
        }
    }

    fn on_grid_sample(&mut self, s: &Sample) -> Vec<CornerFeatures> {
        // Grid samples sit exactly on the grid by construction
        // (`lap_distance == gi * step_m`), so the index round-trips.
        let gi = (s.lap_distance / self.step_m).round() as i64;

        // Arm every corner whose window this sample enters, in model order
        // (which is arm order: `start_m` is increasing).
        while self.next < self.geometry.len() && self.geometry[self.next].arm_gi <= gi {
            self.armed.push_back(self.geometry[self.next].clone());
            self.next += 1;
        }

        self.buffer.push_back((gi, *s));

        // `done_gi` is not monotone across corners (a short corner after a
        // long one finishes first), so scan the whole armed set, not just the
        // front.
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.armed.len() {
            if self.armed[i].done_gi <= gi
                && let Some(g) = self.armed.remove(i)
                && let Some(f) = self.finalize(&g)
            {
                out.push(f);
            } else {
                i += 1;
            }
        }

        // Release everything before the earliest window still open. When
        // nothing is armed the whole buffer goes: a straight between corners
        // holds nothing the coach needs.
        match self.armed.front() {
            Some(earliest) => {
                while self
                    .buffer
                    .front()
                    .is_some_and(|(g, _)| *g < earliest.arm_gi)
                {
                    self.buffer.pop_front();
                }
            }
            None => self.buffer.clear(),
        }

        out
    }

    /// Run the offline extraction over one armed corner's window.
    fn finalize(&self, g: &CornerGeometry) -> Option<CornerFeatures> {
        let start = self.buffer.iter().position(|(gi, _)| *gi >= g.arm_gi)?;
        let samples: Vec<Sample> = self.buffer.iter().skip(start).map(|(_, s)| *s).collect();
        if samples.len() < 2 {
            return None;
        }

        // A contiguous slice of the lap grid is itself a grid with the same
        // spacing and its own anchor, so `extract`'s index math lands on the
        // same samples the offline whole-lap grid would select.
        let window = ResampledLap {
            first_distance_m: samples[0].lap_distance,
            samples,
            step_m: self.step_m,
            non_monotone_dropped: 0,
        };
        let corner = &self.corners[g.idx];
        extract(&window, corner, &self.params, self.lap)
    }

    /// Close every armed corner at the lap boundary and reset for the next
    /// lap. Windows truncate at the line exactly as the offline grid of that
    /// lap does, so a corner the lap never finished is skipped by the same
    /// coverage check, not by a special case here.
    fn on_boundary(&mut self, ended: LapId) -> Vec<CornerFeatures> {
        let armed = std::mem::take(&mut self.armed);
        let mut out = Vec::new();
        for g in &armed {
            if let Some(f) = self.finalize(g) {
                out.push(f);
            }
        }
        debug_assert_eq!(
            self.lap, ended,
            "tracker and pipeline must agree on lap ids"
        );
        self.buffer.clear();
        self.next = 0;
        self.lap = LapId(ended.0 + 1);
        out
    }
}

/// The whole intelligence stack, streamable: lap tracking, resampling,
/// corner tracking, rules, phrasing, throttling.
///
/// Hand it samples (in stream order, from any [`crate::telemetry::source::
/// TelemetrySource`]); it hands back the advice worth delivering, the moment
/// each corner pass completes. A lap is never buffered whole.
pub struct CoachPipeline {
    model: TrackModel,
    reference: ReferenceStore,
    rules: RuleModel,
    phraser: DefaultPhraser,
    mode: ControllerMode,
    engine: ThrottlingEngine,
    laps: LiveLapTracker,
    scorer: LapScorer,
    resampler: StreamingResampler,
    tracker: CornerTracker,
    /// Virtual clock anchor: the `Instant` the first sample was seen and the
    /// wall-clock millisecond it carried. Cooldowns run on telemetry time, so
    /// a replayed capture throttles itself exactly as the same drive would
    /// live — and the same pipeline works unchanged for both.
    clock: Option<(Instant, i64)>,
    last_now: Instant,
    /// Completed passes this session, newest last, for reporting and tests.
    passes: Vec<CornerFeatures>,
    /// Session events (lap boundaries, passes) this session, newest last.
    events: Vec<RuntimeEvent>,
    /// Advice that passed the gate; advice the gate suppressed.
    pub spoken: u64,
    pub suppressed: u64,
}

impl CoachPipeline {
    pub fn new(model: TrackModel, reference: ReferenceStore, config: CoachConfig) -> Self {
        let params = FeatureParams::default();
        let laps = LiveLapTracker::new(model.track_length_m);
        Self {
            tracker: CornerTracker::new(&model, params, config.step_m),
            model,
            reference,
            rules: RuleModel::default(),
            phraser: DefaultPhraser,
            mode: ControllerMode::default(),
            engine: ThrottlingEngine::new(crate::coaching::DecisionConfig::default()),
            laps,
            scorer: LapScorer::new(),
            resampler: StreamingResampler::new(config.step_m),
            clock: None,
            last_now: Instant::now(),
            passes: Vec::new(),
            events: Vec::new(),
            spoken: 0,
            suppressed: 0,
        }
    }

    /// Override the decision engine's configuration. The golden tests use a
    /// fully permissive engine so that everything the rules raise is
    /// delivered, which is what makes live output comparable with the
    /// unthrottled `coach analyse`.
    pub fn with_decision_config(mut self, config: crate::coaching::DecisionConfig) -> Self {
        self.engine = ThrottlingEngine::new(config);
        self
    }

    /// Completed corner passes so far, newest last.
    pub fn take_passes(&mut self) -> Vec<CornerFeatures> {
        std::mem::take(&mut self.passes)
    }

    /// Session events so far, newest last: lap boundaries and corner passes,
    /// whether or not they produced advice.
    pub fn take_events(&mut self) -> Vec<RuntimeEvent> {
        std::mem::take(&mut self.events)
    }

    /// Telemetry-time `Instant` for one sample's wall-clock millisecond.
    fn tick(&mut self, t_ms: i64) -> Instant {
        let now = match self.clock {
            None => {
                let base = Instant::now();
                self.clock = Some((base, t_ms));
                base
            }
            Some((base, t0)) => {
                let dt = (t_ms - t0).max(0) as u64;
                base.checked_add(Duration::from_millis(dt)).unwrap_or(base)
            }
        };
        self.last_now = now;
        now
    }

    /// One corner pass through rules → phrasing → the decision gate.
    fn advise(&mut self, f: &CornerFeatures, now: Instant) -> Vec<Advice> {
        // Corner ids are sequential from zero and the corner list is ordered
        // by `start_m`, so the id indexes the list.
        let Some(corner) = self.model.corners.get(f.corner_id.0 as usize) else {
            return Vec::new();
        };
        // Both halves of a line-straddling corner are named — and throttled —
        // as the one physical corner they are.
        let report = corner.parent_id.unwrap_or(corner.id);
        let reference = self.reference.pass_for(f.corner_id);
        let raised = advise_pass(&self.rules, &self.phraser, self.mode, f, report, reference);

        let mut out = Vec::new();
        for advice in raised {
            match self.engine.gate(advice, now) {
                Some(delivered) => {
                    self.spoken += 1;
                    out.push(delivered);
                }
                None => self.suppressed += 1,
            }
        }
        out
    }

    /// Finalize the lap that just ended: complete its in-flight corner
    /// windows, deliver their advice, run the lap-summary hook, and record
    /// the boundary as an event with the offline-identical quality verdict.
    ///
    /// The scorer is closed *before* the boundary sample is folded into the
    /// next lap — the same accounting [`crate::features::lap::LapTracker`]
    /// does, where the crossing sample belongs to the lap it starts.
    fn complete_lap(&mut self, ended: LapId, now: Instant) -> Vec<Advice> {
        let mut out = Vec::new();
        for f in self.tracker.on_boundary(ended) {
            self.passes.push(f);
            self.events.push(RuntimeEvent::Pass(f));
            out.extend(self.advise(&f, now));
        }
        out.extend(self.engine.on_lap_complete(now));
        self.resampler.on_lap_boundary(ended);
        if !self.scorer.is_empty() {
            let score = self.scorer.close();
            self.events.push(RuntimeEvent::LapBoundary {
                lap: ended,
                time_s: score.wall_duration_ms as f32 / 1000.0,
                clean: score.quality.is_clean(),
            });
        }
        out
    }

    /// Flush at end of stream: the trailing (unwrapped) lap is closed the
    /// same way a crossing closes one.
    pub fn finish(&mut self) -> Vec<Advice> {
        let now = self.last_now;
        self.complete_lap(self.laps.current(), now)
    }
}

impl Stage for CoachPipeline {
    type Out = Advice;

    /// Feed one sample. Lap boundaries are detected here and handled
    /// internally, so a caller driving samples by hand never needs to call
    /// [`Self::on_lap_boundary`] itself — only [`Self::finish`] at end of
    /// stream.
    fn on_sample(&mut self, s: &Sample) -> Vec<Advice> {
        let mut s = *s;
        let now = self.tick(s.t_ms);
        let mut out = Vec::new();

        if let Some(ended) = self.laps.push(&mut s) {
            out.extend(self.complete_lap(ended, now));
        }

        // The scorer sees the clamped sample, after any boundary handling —
        // the crossing sample counts toward the lap it starts, matching the
        // offline tracker.
        self.scorer.push(&s);

        for grid in self.resampler.on_sample(&s) {
            for f in self.tracker.on_grid_sample(&grid) {
                self.passes.push(f);
                self.events.push(RuntimeEvent::Pass(f));
                out.extend(self.advise(&f, now));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coaching::DecisionConfig;
    use crate::core::ids::{CornerId, LapId};
    use crate::features::corner_features::extract_all;
    use crate::features::lap::{Lap, LapTracker};
    use crate::features::resample::{DEFAULT_STEP_M, resample_lap};
    use crate::sims::assetto_corsa::NdjsonReplaySource;
    use crate::telemetry::source::TelemetrySource;

    const MONZA_CAPTURE: &str =
        "ndjson_data/telemetry_ac_monza_ks_ferrari_sf70h_20260902_161237.ndjson.gz";
    const MONZA_MODEL: &str = "data/tracks/ac/monza.json";

    /// Split a capture into laps the way `main.rs` does. `None` (test
    /// skipped) when the capture is not present — the golden tests are only
    /// meaningful against real data.
    fn read_laps(path: &str) -> Option<Vec<Lap>> {
        let mut source = NdjsonReplaySource::open(path).ok()?;
        let mut tracker: Option<LapTracker> = None;
        let mut laps = Vec::new();
        while let Ok(Some(mut sample)) = source.next_sample() {
            let tracker = tracker.get_or_insert_with(|| {
                // The provider sets the session facts on the first sample,
                // including the track length its conversion used.
                let length = source
                    .session()
                    .expect("the first sample carries the session")
                    .track_length;
                LapTracker::new(length)
            });
            if let Some(lap) = tracker.push(&mut sample) {
                laps.push(lap);
            }
        }
        if let Some(tracker) = tracker {
            laps.extend(tracker.finish());
        }
        Some(laps)
    }

    /// A decision config that suppresses nothing, so what the rules raise is
    /// exactly what the pipeline delivers.
    fn permissive() -> DecisionConfig {
        DecisionConfig {
            corner_cooldown: Duration::ZERO,
            kind_cooldown: Duration::ZERO,
            repetition_limit: u32::MAX,
            info_enabled: true,
        }
    }

    #[test]
    fn streaming_resampler_matches_offline_sample_for_sample() {
        let Some(laps) = read_laps(MONZA_CAPTURE) else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };
        let lap = laps
            .iter()
            .find(|l| l.quality.is_clean())
            .expect("the Monza capture has clean laps");

        let offline = resample_lap(&lap.samples, DEFAULT_STEP_M).expect("offline resample");
        let mut stream = StreamingResampler::new(DEFAULT_STEP_M);
        let mut streamed = Vec::with_capacity(offline.samples.len());
        for s in &lap.samples {
            streamed.extend(stream.on_sample(s));
        }

        assert_eq!(
            stream.non_monotone_dropped, offline.non_monotone_dropped,
            "drop counting must match too"
        );
        assert_eq!(streamed.len(), offline.samples.len());
        assert_eq!(streamed, offline.samples, "grid must be bit-identical");
    }

    #[test]
    fn live_pipeline_reproduces_offline_analysis_for_every_clean_lap() {
        let Some(laps) = read_laps(MONZA_CAPTURE) else {
            eprintln!("skipping: {MONZA_CAPTURE} not present");
            return;
        };
        let Ok(model) = TrackModel::load(MONZA_MODEL) else {
            eprintln!("skipping: {MONZA_MODEL} not present");
            return;
        };

        // Offline reference: exactly what `coach analyse` computes.
        let params = FeatureParams::default();
        let mut offline = Vec::new();
        for lap in laps.iter().filter(|l| l.quality.is_clean()) {
            let grid = resample_lap(&lap.samples, model.provenance.step_m)
                .expect("clean lap resamples");
            let features = extract_all(&model, &grid, &params, lap.id);
            offline.push((lap.id, features));
        }
        assert!(!offline.is_empty(), "the Monza capture has clean laps");

        // What the driver should hear named for each corner, captured before
        // the model moves into the pipeline.
        let report_ids: Vec<CornerId> = model
            .corners
            .iter()
            .map(|c| c.parent_id.unwrap_or(c.id))
            .collect();

        // Live: the same capture, one sample at a time, exactly as the live
        // source thread delivers them (the conversion now lives inside the
        // AC provider's source).
        let mut source = NdjsonReplaySource::open(MONZA_CAPTURE).expect("reopen capture");
        let mut samples = Vec::new();
        while let Some(sample) = source.next_sample().expect("read capture") {
            samples.push(sample);
        }
        let track_length = source
            .session()
            .expect("the first sample carries the session")
            .track_length;

        let config = CoachConfig {
            input: crate::core::InputDevice::Replay {
                capture: MONZA_CAPTURE.into(),
            },
            step_m: model.provenance.step_m,
            models_dir: "data/tracks".into(),
            voice: Default::default(),
        };
        let reference = ReferenceStore::empty(&model);
        let mut pipeline =
            CoachPipeline::new(model, reference, config).with_decision_config(permissive());

        // Advice bucketed by the lap it belongs to, using a second lap
        // tracker that agrees with the pipeline's by construction (same rule,
        // same input). Tag *after* driving the sample: a boundary's advice
        // belongs to the lap that just ended.
        let mut advice_by_lap: Vec<(LapId, Vec<Advice>)> = vec![(LapId(0), Vec::new())];
        let mut tagger = LiveLapTracker::new(track_length);
        for sample in &mut samples {
            let advice = pipeline.on_sample(sample);
            advice_by_lap
                .last_mut()
                .expect("always at least the current lap")
                .1
                .extend(advice);
            if tagger.push(sample).is_some() {
                advice_by_lap.push((tagger.current(), Vec::new()));
            }
        }
        let trailing = pipeline.finish();
        advice_by_lap
            .last_mut()
            .expect("always at least the current lap")
            .1
            .extend(trailing);

        // Corner passes: identical, lap by lap, to the offline tables.
        let live_passes = pipeline.take_passes();
        for (lap_id, offline_features) in &offline {
            let live: Vec<CornerFeatures> = live_passes
                .iter()
                .filter(|f| f.lap_id == *lap_id)
                .copied()
                .collect();
            assert_eq!(
                live, *offline_features,
                "lap {lap_id}: live features must equal the offline feature table"
            );
        }

        // Advice: the same sets `coach analyse` would print, lap by lap —
        // same corners, kinds, order and phrasing.
        for (lap_id, offline_features) in &offline {
            let expected: Vec<Advice> = offline_features
                .iter()
                .flat_map(|f| {
                    advise_pass(
                        &RuleModel::default(),
                        &DefaultPhraser,
                        ControllerMode::default(),
                        f,
                        report_ids[f.corner_id.0 as usize],
                        None,
                    )
                })
                .collect();
            let live = advice_by_lap
                .iter()
                .find(|(id, _)| id == lap_id)
                .map(|(_, a)| a.clone())
                .unwrap_or_default();
            assert_eq!(
                live, expected,
                "lap {lap_id}: live advice must equal the offline advice set"
            );
        }
    }
}
