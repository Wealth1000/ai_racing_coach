# The Corner Detection Problem

## The problem in one sentence

Given noisy, inconsistent telemetry from a small number of laps of one
driver on one track, produce a canonical corner list — including the
sub-corners inside complexes the driver treated as one arc — that is
stable enough to anchor per-corner coaching, and do it without a track
map, without per-circuit prior, and without re-scanning the session every
time a new lap arrives.

## Why this is a problem worth naming

The existing corner detector (`src/feature/corner.rs`, ~266 lines) is
heuristic and is provably wrong on the only dataset in the repo. The
implementation plan measured it on `telemetry_cleanAMS2.ndjson` (9 clean
laps, Interlagos, Porsche 911 RSR) and found:

| Metric | Expected | Measured |
|---|---|---|
| Corners on Interlagos | 15 | **9** (3 fused into one 708 m span) |
| Direction split | mixed L/R | **9 Right / 0 Left** (every corner `Right`) |
| Heading-angle signal | live | identically 0.0 |
| Yaw-rate signal | live | identically 0.0 |
| Throttle / brake / steering | live | identically 0.0 |

The cause is upstream, not in `corner.rs` — a schema-deserialization
regression (`frame.rs`) killed the heading and pedal signals so the
corner detector lost the signals it depends on for direction and
boundary placement. But even with that regression closed, the
detector's design has three structural defects that the regression
merely exposed:

- **D8.** Curvature is smoothed as a *signed* mean over a 20 m window,
  then `.abs()`'d. A chicane's two opposite-direction peaks cancel
  inside the window and the corner can vanish.
- **D9.** The threshold comment says 30% of the 95th percentile; the
  code multiplies by 0.15. The detector is a pile of unverified
  constants.
- **D11.** Master-lap selection is O(L²) all-pairs Fréchet that holds
  every lap in memory. That cannot be the live path; the live path
  has to be O(1) per sample.

This document is not a defense of the heuristic or a rewrite of it. The
heuristic is the wrong shape. The job is to restate what we actually
need so a replacement can be designed against honest constraints.

## What "a corner" means here

A corner is a place on the track where the driver has a decision —
entry speed, braking point, line, gear — that affects lap time. A
complex contains several such decisions; a driver who sweeps through it
without lifting has not turned three decisions into one, they have
hidden three decisions inside one smooth arc.

This is why corner detection cannot be solved by counting curvature
peaks. The number of curvature peaks is the number of direction
changes; the number of coaching anchors is the number of decisions.
These differ at exactly the circuits that matter: Maggotts–Beckets–
Chapel, the Porsche Curves at Leet, the Swimming Pool section at
Spa, the first sector at Suzuka.

## Goal

A two-stage system whose outputs are:

1. **A `TrackModel`** — a JSON file written once per track by
   `coach learn-track`. Lists corners by absolute distance with apex,
   entry, exit, direction, and the curvature/pedal evidence that
   established each one. Loads in O(1). Shared between sessions.
2. **A live corner tracker** — given the `TrackModel` and a stream of
   `Sample`s, emit `CornerEntered` / `CornerExited` events as the car
   crosses corner boundaries, and extract per-corner features inside
   the window.

The `TrackModel` is the hard part. Everything after it is bookkeeping.

## Constraints, honestly stated

These are not aspirations. Each one is either imposed by the rest of
the system, measured against the existing data, or both.

### From the architecture (locked, §3 of `docs/implementation-plan.md`)

- **Streaming from Batch 3 onward.** No stage holds a lap-sized buffer.
  The live path runs at 100 Hz. Any corner detector that needs all
  laps in memory to produce a corner list is offline-only.
- **Track-agnostic.** No prior on Silverstone, Monza, Interlagos, or
  any specific circuit. The algorithm sees curvature and pedal
  profiles and reasons from them.
- **No magic numbers.** Every threshold and tolerance is derived from
  the data itself: noise floor from the straightest part of this lap,
  complex-length threshold from the median corner length in this
  track, vote threshold proportional to laps seen so far.
- **Player-error-tolerant.** Spins, off-tracks, lift-and-coast, half
  laps are inputs, not outliers to discard. A spin at T7 is evidence
  that T7 is hard.

### From the data (measured)

- **<10 laps to convergence.** The only real dataset in the repo is
  9 laps. A detector that needs 30 is unusable; a detector that needs
  3 has no evidence.
- **The driver is inconsistent across laps.** This is the realistic
  case, not the failure case. Some laps will be clean; others will
  have mistakes; some will take different lines through the same
  complex. The detector has to reconcile.
- **Telemetry is rich but partial.** AC shared memory at 100 Hz gives
  position, world/local velocity, angular velocity, yaw, slip per
  wheel, brake pressure per wheel, throttle, brake, steering, surface
  grip, tyres-out, lap distance. Whatever detector we build has to
  degrade gracefully if a field is missing (e.g. replay from AMS2
  lacks `WheelSlip` and `NumberOfTyresOut`).
- **Curvature resolution is bounded by sampling.** Menger curvature
  on 1 m-resampled data cannot separate two canonical corners that
  are closer than ~10 m. We have to accept that as a hard floor.

### From the user

- **The coach needs the corner list before it can coach.** We cannot
  require the driver to drive 30 perfect laps first. The corner list
  has to be good enough after a handful of laps and refine with
  practice, not the other way round.

## Sub-problems

The problem decomposes into four, in order. Sub-problems 2 and 3 only
exist once sub-problem 1 is solved.

### 1. Where does the track *turn*?

Which curvature peaks are corners (decisions) and which are kinks
(markers the driver takes as given)?

**Evidence available:** curvature magnitude peaks, curvature integral
over a window, yaw rate, lateral g, slip angle, brake-before-peak,
throttle-after-peak.

**Hypothesis:** a decision corner has a consistent curvature peak
across laps, even if the apex position varies. A kink has a peak that
varies in magnitude because the driver treats it differently lap to
lap. Test two consistency criteria:

- *Peak presence:* curvature magnitude exceeds a local noise floor on
  more than N laps. The noise floor is the curvature on the
  straightest part of the lap — i.e., the lowest percentile of the
  curvature distribution of this lap itself.
- *Peak timing consistency:* peak distance is within tolerance on
  more than N laps. Tolerance is the spread of apex positions across
  laps, not a fixed number.

If a peak is present on most laps at roughly the same distance, it
is a corner. If it appears on some laps only, or its position
varies wildly, it is a kink.

### 2. Where does a *complex* end?

A complex is several canonical corners within a few hundred metres,
possibly closer together than the curvature-smoothing window. The
curvature magnitude stays above threshold through the whole complex.

**Evidence available:** curvature magnitude peaks within the span,
curvature derivative, heading rate, and — the decisive signal —
*where the driver brakes and lifts*.

**Hypothesis:** between two canonical corners within a complex, the
driver briefly lifts off the throttle or transitions between braking
and throttle. The transition point is a corner boundary. This is
data-driven: the throttle and brake signals show the driver making
a decision, and the decision's location is the corner boundary.

This is the trick the current heuristic misses. The curvature profile
can be smooth through a complex, but the pedal trace cannot. The
driver cannot drive three consecutive corners at full throttle
without one of them being a different corner than the others — and
if they can (because they are very smooth), then the complex
genuinely is one coaching anchor and the model should report it that
way.

### 3. Where are the corner boundaries?

Once a corner is detected, where do entry, apex, exit lie?

**Hypothesis:** boundaries come from pedal events, not curvature
events.

- *Entry:* the last sample before brake pedal crosses threshold (the
  braking point).
- *Apex:* the sample where curvature magnitude is maximum, or
  equivalently where lateral g peaks. For slow corners this is the
  geometric apex; for fast sweepers, the driver may take a late
  apex, and the corner model should record both.
- *Exit:* the first sample where throttle reaches sustained
  threshold after the apex, or where heading rate drops below a
  local baseline.

### 4. How do we converge in <10 laps?

The model has to stabilise fast. After lap 3 we should have most
corners. After lap 6 we should have all of them. After lap 10 we
should be refining, not discovering.

**Hypothesis:** the learner is online. Each new lap updates the
model incrementally. Confidence in a corner candidate is the
fraction of laps that have shown it. The vote threshold decreases
as evidence accumulates — after 3 laps require 3/3; after 6 laps
require 4/6; after 10 laps require 6/10. Otherwise a noisy first
lap poisons the whole session.

## Telemetry fields that actually matter

From AC shared memory, the fields the corner detector uses:

| Field | Used for |
|---|---|
| `PositionX/Y/Z` | Curvature. **Required.** |
| `VelocityX/Y/Z` | Speed. Required for valid Menger curvature (samples too far apart in time inflate it). |
| `LocalAngularVelocity1` | Yaw rate. Required as a curvature cross-check and for direction classification. |
| `Heading` (`Orientation[1]`) | Yaw for direction classification. Required. |
| `Gas`, `Brake` | Pedal trace. **Required** — this is the complex-splitter signal. |
| `SteeringAngle` | Driver intent. Confirms the driver is actually turning. |
| `WheelPressure0..3` | More precise than `Brake` for entry detection. |
| `WheelSlip0..3` | Instability signal. Off-track / spin detection. |
| `NormalizedCarPosition` / `CurrentLapDistance` | The x-axis of every corner. **Required.** |
| `TrackSPlineLength` | Normalisation. Required. |
| `NumberOfTyresOut`, `IsValidLap`, `SurfaceGrip` | Clean-lap gating. |
| `Track`, `TrackConfiguration` | Identity for the `TrackModel` filename. |

Fields ignored for MVP corner detection: `PacketId`, `GForce`,
`PerformanceMeter`, `TurboBoost`, `WaterTemp`, all static info
except the three above.

## Telemetry fields the detector must keep working without

AMS2 replay produces position, velocity, yaw, throttle, brake,
steering, lap distance. It does **not** produce per-wheel slip,
per-wheel pressure, surface grip, or tyres-out. The detector has to
work on AMS2 replay (because that is the only regression corpus in
the repo today) and improve when AC replay arrives (because that
adds the per-wheel signals). Anything that hard-depends on a
field AMS2 does not emit is a defect waiting to happen.

## What an honest MVP looks like

Three stages. None of them is the existing detector.

**Stage A — per-lap candidate extraction.** Run once per lap,
online. Resample position to a 1 m grid, compute Menger curvature,
find local maxima above the per-lap noise floor (lowest 10th
percentile of curvature magnitude on this lap). Record for each
peak: distance, height, brake-before, throttle-after. Output: a
list of corner candidates for this lap.

**Stage B — cross-lap accumulation.** Run after every lap.
Project this lap's candidates onto the running model's coordinate
frame. For each, find the closest existing corner within adaptive
tolerance (median inter-corner spacing so far). If close enough,
increment support, update position with a running median. If not,
add a tentative candidate; promote to confirmed when support
crosses the adaptive threshold.

**Stage C — complex sub-corner detection.** Run after every lap.
For each confirmed corner spanning more than ~1.5× the median
corner length in this track, look at the pedal trace inside it.
Pedal transitions inside the span are candidate sub-corner
boundaries. Sub-corners are tentative until additional laps of
pedal evidence confirm them.

Nothing in stages A–C is a fixed number. The noise floor is per
lap. The complex-length threshold is a multiple of the median
corner length in this track. The vote threshold is proportional
to laps seen. Each constant is auditable against the data, not
against the developer's intuition.

## Things I have not decided and want feedback on

These are the places where I am guessing and want to be told I am
guessing.

1. **Per-lap noise floor.** "Lowest 10th percentile of curvature
   magnitude" is a guess. A track with no real straights (Monaco)
   has no low percentile; a chicane has a bimodal curvature
   distribution and the 10th percentile sits in the wrong mode.
   The right statistic may be the kurtosis, the modal value, or a
   robust scale estimate (MAD). I do not know.
2. **Pedal-trace complex splitting.** The hypothesis is that the
   driver lifts between sub-corners. What if the driver is
   consistent enough not to lift, even early in the session? Then
   the pedal trace is also smooth and the algorithm reports one
   corner there. The user accepts that — the coach says "this
   corner takes 3 seconds" without claiming it is 3 corners. But
   this is a retreat from the goal, and I want to know whether
   the retreat is acceptable or whether we need a second signal
   (curvature derivative, heading rate of change) for the
   difficult case.
3. **Adaptive vote threshold vs fixed.** Is the
   "require-N-of-M-proportional-to-laps" rule the right
   convergence mechanism, or should corners confirm after a fixed
   number of laps? The proportional rule prevents an early false
   positive from cascading; the fixed rule is simpler and easier
   to reason about. I do not know which the data prefers.
4. **Complex-length threshold.** "~1.5× the median corner length"
   is a guess at what separates a long corner from a complex. I
   do not know whether 1.5 is right, or whether the right
   quantity is something else (e.g., the curvature integral over
   the span).
5. **Direction reversals that do not show up in pedal traces.** A
   flick through a complex where the driver carries mid-corner
   throttle: pedal trace is flat, but heading rate has two
   zero-crossings. Should the detector use heading-rate
   zero-crossings as a fallback when the pedal trace is
   uninformative?
6. **Minimum telemetry set.** The MVP uses position, heading,
   yaw rate, gas, brake, steering, lap distance. Is that enough?
   Would `WheelSlip` or per-wheel brake pressure give materially
   better corner boundaries, or is it noise the rules layer can
   absorb?
7. **Out-lap and formation-lap detection.** The detector has to
   exclude the first lap of a session without contaminating the
   model. I have not thought about what signal marks the first
   lap. `IsValidLap == false` is a guess; it may not exist on
   every sim.

## What the existing detector gets wrong, as a checklist

Closing each of these is a precondition for the new design to be
trustworthy:

- [ ] **D2/D3/D4.** Heading, yaw rate, throttle, brake, steering
      read zero on AMS2 because `frame.rs` deserialises them with
      `#[serde(default)]` and no accessor fallback. Fix the
      parser; do not work around it in the detector.
- [ ] **D8.** Smooth signed curvature, then `.abs()`. Either
      smooth unsigned curvature, or detect signed peaks before
      smoothing.
- [ ] **D9.** Threshold comment and code disagree. Pick one and
      justify it against data, or — better — derive it from data.
- [ ] **D10.** Unbounded index arithmetic and three
      `partial_cmp().unwrap()` sites panic on edge inputs. Guard
      them.
- [ ] **D11.** O(L²) all-pairs master-lap selection. Demote to
      `coach learn-track` (offline), or replace with
      `features::line` (O(L) per pair) which is the path the
      implementation plan took.
- [ ] **D13.** `TrackCorner` does not derive `Serialize`, `Clone`,
      or anything. The new `TrackModel` must derive all of them.

## Honest framing

This document restates the corner-detection problem so the next
attempt is designed against measured constraints, not against the
shape of the heuristic it replaces. The previous draft of this file
proposed a single monolithic algorithm; that framing was wrong
because it ignored the streaming constraint that the rest of the
system has already accepted. The honest shape is offline learner
plus live tracker, with the `TrackModel` as the contract between
them.

What I want from a stronger model is an evaluation of whether:

1. The three-stage split (per-lap extraction → cross-lap
   accumulation → pedal-trace sub-corner splitting) is the right
   decomposition, or whether there is a fundamentally different
   approach (probabilistic model over the curvature profile,
   online HMM, change-point detection on pedal traces) that
   converges faster or handles edge cases the heuristic misses.
2. The honest constraints above are correct, missing, or
   overstated. Specifically: is the "no magic numbers" rule
   worth its complexity, or are well-justified constants
   acceptable if they are auditable?
3. The minimum telemetry set is enough, or whether the detector
   is structurally incapable without per-wheel signals.

I have not built any of this yet. The first thing to build is
stage A against the existing 9-lap AMS2 dataset, with the parser
defects (D2/D3/D4) closed, and see whether the per-lap candidates
look sane on real data. If they do not, every downstream plan in
this document is suspect.