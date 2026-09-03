# The neural coach: a design document

How to take the coaching from the rule model — every threshold a hand-picked
number — to a learned model that earns its thresholds from data. This
document is written against the code as it stands: every input, output and
seam it names exists today, and the paths are the ones the repo uses.

## 1. Why the rules run out of road

`src/models/rules.rs` states its own limitation better than this document
can, in its *threshold problem* section: every rule has at least one
threshold, every threshold is a tuning knob, and the values were chosen so
they "should not fire spuriously" on the captures already in the repo. That
is a model fitted by hand to one driver, one car, two circuits.

What the rules structurally cannot do, no matter how the knobs are set:

- **Combine signals.** A late apex and a low exit speed and a high peak
  slip angle are three separate beeps. A human coach sees one thing: "you
  turned in too early, hung the car out, and paid for it down the
  straight." A threshold model has no mechanism for *interaction* between
  features.
- **Calibrate to the driver.** `late_brake_m = 5.0` is the same 5 metres
  for a hotlapper and a rookie. The right threshold for one is noise or
  silence for the other.
- **Prioritise.** When three rules fire on one corner, the audio layer's
  cooldown picks what gets said by timing, not by importance. Nothing in
  the rule model knows which miss cost the most time.
- **Say how confident it is.** A 0.11 s gap over the `lost_time_s = 0.10`
  threshold fires exactly like a 1.1 s gap. The driver hears no difference
  between "probably nothing" and "certainly something".

A learned model addresses all four at once — interactions are what neural
networks *are*, calibration is what training *does*, and a predicted
magnitude plus a confidence is a regression head's natural output.

## 2. Where the labels come from: the stopwatch

Neural networks learn by carrot and stick: shown an input, they guess; the
guess is scored against a known answer; wrong guesses move the weights.
The obvious objection to a neural coach is "nobody labels my driving" —
and it dissolves in a simulator, because **the sim labels every pass
itself, to the millisecond, with no human in the loop**. The label is the
clock.

Every pass through a corner becomes a row like this (this is literally
what `coach export-dataset` writes):

| entry m/s | apex m/s | brake start | … | **actual `delta_time_s`** |
|---|---|---|---|---|
| 82.1 | 48.3 | −52 m | … | **+0.31** (0.31 s slower than the PB here) |
| 81.8 | 47.9 | −51 m | … | **+0.02** |
| 83.0 | 51.2 | −55 m | … | **−0.14** (faster than the PB) |

The last column is the answer key, written by the game. Training hides it
from the network, the network guesses a cost, and the stick (the loss)
lands in proportion to how wrong the guess was. That is the whole loop.

It also makes "is this call bogus?" countable rather than a matter of
opinion. Run the trained model over a held-out capture it never saw and
count the failures:

- it claims a corner cost 0.3 s, the clock says the pass was on the PB —
  a **caught lie**;
- a corner genuinely cost 0.4 s and the model stayed silent — a **missed
  call**.

Old captures are frozen ground truth; `coach live --replay` re-grades a
model against them any time, for free. This is what §10's evaluation
gates measure.

Two honest caveats. First, "was the *prediction* right" is directly
supervisable; "was the *advice* good" is one step removed — the model can
be right that a corner cost 0.3 s and still name the wrong fix. That is
why the model predicts cost and advice is *derived* from it (§5.3), and
why the final gate is the online one: across dozens of sessions, do
corners the coach speaks about get faster on later laps? Session
recordings already capture enough to measure that trend. Noisy, but it is
the scoreboard that matters. Second, the issue-kind labels are weaker —
they come from the rule model's own firings (§5.2) — which is why time
regression leads and the issue head is scaffolding.

### 2.1 And the extraction? It is the pipeline you already run

The other worry — "how do we get what we need out of the raw data?" — is
already answered by code that exists and is tested, because the rule
coach runs on it today:

```
shared memory noise (400 Hz, out-of-order, gaps)
  → FrameAssembler → ordered Samples
  → lap detection + clean-lap filter (off-track, tyres_out)
  → resample each lap onto a distance grid (laps become comparable point-for-point)
  → cut each lap into corner slices (the TrackModel knows where corners are)
  → summarise each slice into ~18 numbers (CornerFeatures) + the outcome
  → one CSV row. That row is the training row.
```

The rule model eats this table; the neural model eats the same table. The
extraction does not change. What genuinely remains hard is **attribution**
— "you lost 0.3 s, *and here's why*" — and §5.3 covers the partial
answers (input deltas rank themselves; gradient × input is the
principled version).

## 3. The seam is already there

The whole pipeline speaks one interface, and it is exactly the shape a
neural model needs to fill:

```rust
// src/models/mod.rs
pub trait DrivingModel {
    fn predict(
        &self,
        features: &CornerFeatures,
        reference: Option<&CornerReference>,
    ) -> Vec<DrivingIssue>;
    fn name(&self) -> &'static str;
}
```

Implement this and nothing downstream changes. This is the same deal the
`SimProvider` trait gives a new simulator: the model is a plug-in point,
and the phraser, the audio cooldown, the UI feed, the session recorder and
the dataset export all consume `DrivingIssue → Advice` without knowing or
caring whether a threshold table or a tensor produced it.

**Keep it that way.** The neural model's job is to produce
`Vec<DrivingIssue>` — kind, severity, and the numeric deltas — and let
`Advice::from_issue` plus the existing `Phraser` do the talking. A model
that generates sentences directly forfeits the cooldown logic, the delta
tooltips, and the recorded sessions' advice counts, and buys nothing: the
sentence templates are not the hard part.

## 4. Inputs: what a pass looks like as numbers

### 4.1 The per-pass vector (start here)

One row per corner pass already exists, computed by
`features::corner_features`, exported per-pass by `coach export-dataset`
(`src/storage/dataset.rs`, `COLUMNS`). The model-facing subset:

| Group | Fields |
|---|---|
| Speeds | `entry_speed_mps`, `apex_speed_mps`, `exit_speed_mps`, `speed_min_offset_m` |
| Braking | `brake_start_m`*, `braking_length_m`*, `peak_brake`, `trail_braking` |
| Throttle | `throttle_pickup_offset_m`*, `min_throttle_in_corner` |
| Outcome | `time_in_corner_s`, `peak_abs_slip_rad`, `off_track_points` |
| Context | `corner_id` ordinal, `direction` (L/R) |

(*`Option<f32>` — encode as value-plus-is-present flag, never as a magic
number. "Did not brake" and "braked at −7.0 m" must not share a value.)

### 4.2 The reference (personal best) features

When a PB exists, `CornerReference` adds the target the pass is measured
against: entry/apex/exit speeds, `time_in_corner_s`, `brake_offset_m`*,
`throttle_pickup_offset_m`*, `trail_braking`. The dataset already exports
the deltas in the rules' sign conventions (positive = later / slower /
past the reference): `delta_time_s`, `delta_brake_m`,
`delta_apex_speed_mps`, `delta_throttle_pickup_m`.

Feed the network **both** the raw features and the deltas, plus a
`has_pb` flag. The deltas are the strongest features you have — they are
what the rules reason with — but the raw values are what lets the model
learn that "5 m late braking" means something different into Turn 1 at
260 km/h than into the Lesmo at 130.

### 4.3 Context beyond the pass

`CornerFeatures` is a summary of one corner in isolation. Two cheap
enrichments, both derivable from data the pipeline already holds:

- **Corner geometry from the track model** (`TrackModel.corners`): entry
  and exit headings relative to the approach straight, corner span length,
  direction. Same corner ordinal across sessions, so the network can learn
  "this *shape* of corner rewards late apex" without re-learning per track.
- **Neighbouring-corner outcomes from the same lap** (the pass before and
  after: exit speed, time). Corner exits feed the next entry; a model that
  can see the chain can say "you lost this corner on the *exit of the last
  one*."

A later phase (§9.3) replaces the summary with the raw resampled grid
slice, but do not start there. The per-pass vector is 20-odd numbers, the
dataset is already shaped for it, and it is the fastest path to a model
that beats the thresholds at their own game.

## 5. Outputs: what to predict

This is the design decision the document exists for, and the recommendation
is **not** "generate advice directly". Predict outcomes; derive advice.

### 5.1 Head A — time delta (the primary target)

Regression on `delta_time_s`: how much slower (or faster) than the personal
best was this pass through this corner. This is the honest target because:

- it is a *physical* number, measured by the sim clock, not an opinion
  (§2);
- it is what the driver actually wants to know ("this corner cost you
  0.31 s");
- it is dense — every labelled pass trains it, whereas issue kinds are
  mostly zeros (see §5.2);
- it converts directly into severity: the existing three levels map
  naturally onto predicted cost bands, and the model can learn where those
  bands sit per driver and per corner, which is exactly the calibration
  the thresholds cannot do.

### 5.2 Head B — issue kinds (multi-label, auxiliary)

A sigmoid per `IssueKind` (the 8 in `src/models/issue.rs`:
`BrakedInsideCorner`, `NoThrottlePickup`, `LateApex`, `EarlyApex`,
`LateBrakeVsPb`, `SlowApexVsPb`, `LateThrottleVsPb`, `LostTimeVsPb`).
This head is trained first by **distilling the rule model**: run the rules
offline over every exported pass, use their firings as labels. The
distilled net's value is not that it replicates the rules — it is that
afterwards you fine-tune it on what the rules *missed*, and it smooths the
threshold cliffs the rules were forced into.

Do not expect this head to stay eight labels forever. The interesting
discoveries are the labels that do not exist yet — an understeer pattern,
a consistent early-turn-in at high-speed entries — and finding them is a
clustering exercise on the network's hidden representation (§9.4), not a
matter of writing more rules.

### 5.3 From prediction to `DrivingIssue`

At inference, per pass:

1. Run the network → predicted `delta_time_s` and issue-kind
   probabilities.
2. **Gate on the regression, not the labels**: a kind only becomes an
   issue when the predicted time cost clears a severity band. This is what
   fixes the "three beeps for one mistake" problem — the model may know
   the apex was late AND the throttle late, but it says the one thing that
   cost the time.
3. Attribute: the deltas in the input (brake offset, apex speed, throttle
   pickup) rank themselves — predicted cost × input delta gives "the brake
   point is the miss" essentially for free, and gradient × input
   (`∂ŷ/∂x`, computable in the inference runtime) is the principled
   version if the simple one under-delivers.
4. Emit `DrivingIssue { kind, severity, deltas }`. Phrasing, cooldown,
   audio, UI: all existing code.

## 6. The corpus, honestly

The arithmetic that should shape every decision below: **one 9-lap session
at Monza is ~150 passes.** The repo's real-capture corpus today is a
hundred-ish rows. This is a small-data problem wearing a deep-learning
costume, and the plan must respect it:

- **The flywheel already exists.** Record-while-coaching writes every
  session's capture to `data/captures`; `learn-track` pools them; live
  sessions leave `data/sessions/*.ndjson`; `export-dataset` flattens them
  into rows. The single highest-leverage act is *driving more laps with
  the coach running* — the model improves with the driver. And one driver
  is not enough, which is what §7 is about.
- **Split by session, never by row.** Rows from one lap are correlated
  (same tyres, same mood, same setup). A random row split leaks and will
  lie to you. Split at the session (or at minimum the capture) level.
- **Generalisation axes are per-car and per-corner.** Speeds are
  car-dependent; corner shape is track-dependent. Either normalise
  features that carry absolute units (see §8.3) or train per-car and
  accept the smaller corpus per model. Starting per-car (the PB already
  is) is the honest small-data move; a per-car model with 50 sessions of
  one driver is a *better coach* than a general model with 5.
- **The labels are cheap but not free.** Rule distillation is automatic.
  Time-delta regression needs a PB to exist (reference rows only); passes
  without a PB still train the no-reference path with `has_pb = 0` against
  an absolute `time_in_corner_s` target.

A realistic first milestone: **one driver, one car, one track, 30-50
sessions** (~5,000-8,000 passes). Enough to beat the thresholds on
calibration and to learn interactions; not enough for anything that needs
millions of rows.

## 7. Gathering data at scale: the sharing programme

One driver cannot gather enough laps. The users of the coach can, together
— if they choose to. The design goal is a donation flow that is honest,
opt-in, and sends telemetry and nothing else.

### 7.1 What is sent, and what never is

Sent: the **per-pass dataset CSV** — pure numbers (speeds, metres,
seconds), no names, no hardware, no file paths — plus a small manifest
(coach version, track, session count, row count, a random install id).
The install id is a UUID generated once at first enable; it lets the
author dedupe uploads and keep one stranger's sessions linkable for
split-by-session correctness without ever knowing who they are.

Never sent: raw captures (`telemetry_ac_*.ndjson.gz`). The AC static page
embeds the player name, so raw captures would need scrubbing first; the
per-pass table needs none. Also never: settings, hardware info, anything
the coach knows about the machine. (Possible later: scrubbed raw captures
for §9.3's sequence models — the consent dialog would need to say so.)

One honest line the dialog must carry: telemetry has no names, but a
driving-style *fingerprint* is still a fingerprint. The dialog says what
that means plainly rather than pretending anonymity.

Session ids in the CSV are remapped before sending — each distinct
session column value becomes an opaque, install-scoped hash — so a
user-named session file ("Dave's hotlap") never leaves the machine, while
the grouping the training split needs survives. Track and car names stay:
they are required to pool data correctly (§6), and a track name is not a
person.

### 7.2 The consent UX

- A settings toggle — **"Share my telemetry with the coach's author"** —
  **off by default**, and nothing is ever sent until it is flipped.
- The first flip shows the dialog: **what gets sent** (the corner-pass
  table, the manifest — itemised), **why** (one driver's laps cannot train
  the network; every donation trains a coach that ships back to everyone),
  and **what never gets sent**. The toggle only sticks if the driver
  confirms; cancel leaves it off.
- The Export screen gains a **"Send to author"** action that first shows a
  preview — row count and the first lines, literally the bytes about to
  leave — and then runs the send as a job with the same job-screen
  treatment as every other command. Explicit action first; opt-in
  auto-send is a later, trust-earned step.

### 7.3 The bucket

**Workers KV first** — the receiver lives in `share-backend/` (a
Cloudflare Worker, deployed with the README's two paths: wrangler CLI, or
browser-only with zero local tooling). KV's free tier is 1 GB storage and
1,000 writes/day with **no card on file anywhere**, and a bundle is
10-100 KB, so that is thousands of donations. The Worker validates what
the client sends (schema header, `application/gzip`, gzip magic bytes, an
8 MB cap) and stores under `share/<date>/<uuid>.json.gz`; anything wrong
is refused with a reason, and the client degrades to saving the bundle on
disk — a refusal never costs the driver anything.

**R2 when KV runs out** (more than 1 GB or 1,000 uploads/day): 10 GB
free with zero egress fees — the egress part matters at scale, because
the training data gets downloaded back out — but activation asks for a
payment method. The switch is three lines (README documents it): one
binding change, one `.put()` option dropped. Nothing client-side moves.

The client is compiled against the receiver: the endpoint ships in the
binary (`storage::share::DEFAULT_ENDPOINT`), overridable with
`COACH_SHARE_ENDPOINT` for testing, and a failed upload degrades to
writing the bundle under `data/share/` — the offline fallback that keeps
a send from ever costing the driver more than the try.

## 8. Training (off this machine, in Python)

The dev machine rule applies: no toolchains installed here. Training lives
in a separate Python environment wherever you keep one; the repo's job is
to hand that environment clean tensors and receive back one file.

### 8.1 The loop

1. `coach export-dataset data/sessions data/dataset.csv --model-dir
   data/tracks` (the GUI's Export dataset screen is the same call).
   Accumulate: append each session's export, or export per-session files
   and concatenate — the format is stable (`COLUMNS` is one source of
   truth for writer and reader). Donated bundles from §7 are the same
   rows; concatenate and de-duplicate by session id.
2. A small training script (PyTorch is the path of least resistance, and
   its ONNX export is mature): load the CSV, build the feature tensor
   per §4, split by session, train, export `coach-net.onnx`.
3. Copy the ONNX file into `data/models/`; the Rust side loads it at
   session start (§9).

### 8.2 Architecture for the per-pass model

Deliberately boring:

```
inputs (~30 floats, normalised)
  → Linear 64 → LayerNorm → ReLU
  → Linear 32 → LayerNorm → ReLU
  → heads:
      time delta  : Linear 1   (regression, Huber loss)
      issue kinds : Linear 8   (sigmoid, BCE)
```

Two hidden layers. Not a typo, not a compromise — at 10³-10⁴ rows, every
extra layer buys overfitting and nothing else. The corpus size is the
binding constraint, not the architecture; resist the transformer.

Loss: `Huber(delta_time_s) + 0.3 · BCE(issue_kinds)`. Weight the
regression head higher — it is the product; the label head is scaffolding.
Early-stop on a held-out session's time-delta MAE.

### 8.3 Normalisation (the part that silently ruins these projects)

Fit scalers on the training split only, and **ship them with the model**
(three small vectors inside the ONNX graph or a JSON sidecar): the Rust
inference path must apply byte-identical transforms. Speeds in m/s span
20-90; offsets span −30..+30; `off_track_points` spans 0..4 —
unnormalised, the network is a speed detector. Boolean features are 0/1.
Missing-value flags ride beside their values. Record every scaler's
parameters in the model's metadata so a mismatch is diagnosable from the
log, not from the behaviour.

## 9. Inference (in this crate, in Rust)

### 9.1 Runtime

Two credible options, both pure-Rust with no system dependencies (the
Windows release build must stay self-contained):

- **`tract`** — ONNX inference, no unsafe, widely used in production Rust;
  the safe default.
- **`candle`** (HuggingFace) — also has ONNX loading; heavier, more active
  API churn.

Add as an opt-in cargo feature (`neural`) so the default build — and the
CI's `cargo test --locked` — is untouched until the model is real.

### 9.2 The plug-in

```rust
pub struct NeuralModel {
    // tract Model + the scaler parameters, loaded once at session start
}

impl DrivingModel for NeuralModel {
    fn predict(&self, f: &CornerFeatures, reference: Option<&CornerReference>)
        -> Vec<DrivingIssue>
    {
        // §5.3: run heads, gate on predicted cost, attribute, emit issues
    }
    fn name(&self) -> &'static str { "neural" }
}
```

Determinism is a house rule the network must keep: same inputs, same
weights → same issues, same order (sort by kind as the rule model does).
Inference is a few hundred multiply-adds; the pipeline runs at 10 Hz with
a corner pass every few seconds — latency is a non-concern, and this
should be stated in the module docs so nobody "optimises" it later.

### 9.3 Later: sequence input

`features::resample` puts every lap on a fixed distance grid; a corner
pass is therefore a small matrix (grid points × sample channels: speed,
brake, throttle, steer, slip). A 1-D convolution or a tiny GRU over the
pass slice lets the network see *the shape of the braking curve* rather
than its summary statistics — trail braking as a slope, not a boolean.
This is the right second architecture, after the per-pass model has
earned trust, and it needs no new data (the captures are already the
source of the grid).

### 9.4 Later: discovering new issue kinds

Take the trained encoder's penultimate layer, pool it per pass, and
cluster. Clusters that correlate with time loss but map to no existing
`IssueKind` are the coach's next vocabulary — understeer corners, chicane
kerb-riding styles, whatever the data says. Each one graduates by adding
an `IssueKind` variant (a compile error everywhere, by design — the same
audit trail `Sim` has) and a phrased template.

## 10. Evaluation

Three gates, in order; a model that fails an earlier gate does not proceed
to a later one:

1. **Offline regression.** Held-out *sessions*: MAE on `delta_time_s`,
   reported per corner ordinal (a model that is great at Turn 1 and blind
   at the Lesmos is a Monza-shaped lie). Baseline to beat: the trivial
   predictor "delta = 0" (i.e. always guess the PB) and the linear
   regression on the four exported deltas. If the net cannot beat the
   linear baseline, the data is not there yet — drive more.
2. **Offline classification.** Rule-distilled labels: precision/recall
   per issue kind on held-out sessions. The point is not to match the
   rules exactly — firing *less* on clean passes while catching the same
   misses is the win (false-positive rate is what makes a coach get
   muted).
3. **Replay A/B, in the existing harness.** `coach analyse` and
   `coach live --replay <capture>` already exist; run both engines over
   the same capture and diff the advice. The metrics that matter to the
   driver: pieces of advice per clean lap (rules fire ~N; fewer with the
   same information is better) and agreement on the biggest miss per
   corner. The replay harness is the A/B rig — no new infrastructure.

## 11. Model files and fingerprints

- Models live in `data/models/` (a new sibling of `data/tracks`), named
  `neural_<sim>_<car>.onnx` + a JSON metadata sidecar: scaler parameters,
  training-corpus provenance (session ids), eval numbers from §10, and a
  version.
- The metadata sidecar is the trust boundary: the Rust loader refuses a
  model whose metadata is missing or whose provenance references sessions
  the loader cannot reconcile — same philosophy as `ReferenceStore`
  refusing to merge across model fingerprints. A model trained against a
  stale track model produces confidently wrong corner ordinals, and that
  must be a loud refusal, not a subtle one. (Include the track model's
  fingerprint in the metadata for exactly this check.)
- The `DrivingModel` picker: the runtime gains a `--model rule|neural`
  flag and the GUI's home screen a toggle, so the two engines can be A/B'd
  in the window too.

## 12. Phased plan

**Phase 0 — plumbing (no learning).** `export-dataset` accumulates
corpora across sessions (verify: two exports, concatenated, load clean in
Python). The share toggle, consent dialog and bundle builder land here
(§7) — the sooner donations start, the sooner Phase 2 has data. A
`neural` cargo feature adds `tract` behind it. Nothing else changes;
`cargo test` stays green.

**Phase 1 — distillation.** Train the per-pass model on rule labels +
time-delta regression over the existing corpus. Gate: §10.2 offline, then
replay A/B. Success is *matching* the rules with smooth thresholds — the
calibration, not the vocabulary.

**Phase 2 — the flywheel.** Drive with the neural coach; every session
adds rows; donated bundles pool in; retrain on cadence. Success is §10.1
beating the linear baseline and the advice-per-clean-lap count falling
while corner times don't.

**Phase 3 — vocabulary.** §9.3 sequence input; §9.4 discovered issue
kinds; new `IssueKind` variants and phrases.

The order is the point: every phase produces a coach that works, and the
learning is what makes the next phase's coach better — the same loop the
track model already runs on, one level up.
