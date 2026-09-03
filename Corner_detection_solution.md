# Canonical corner count detection — algorithm specification

## Scope

Given telemetry from a small number of laps (target: converge in under 10) of
one driver on one track, produce the canonical corner count and a full corner
list — including sub-corners inside complexes the driver treats as one arc —
without a track map, without a per-circuit prior, and without depending on
any field, sign convention, or lap counter that's specific to one sim.

This document specifies the offline learner only: the process that builds a
`TrackModel` from accumulating laps. The live tracker that consumes a frozen
`TrackModel` to emit `CornerEntered`/`CornerExited` events at sample rate is a
separate, much simpler component (range checks against precomputed windows)
and isn't the subject here.

Non-goals: multi-car/traffic telemetry, the coaching layer that consumes
`TrackModel`, and UI.

## Design principles

Three decisions shape everything below.

**1. "No magic numbers" means no *track-tuned* numbers, not no numbers.**
Every constant in this spec is either derived from the data every time it's
used, or is a fixed property of an estimator (a robust-statistics convention,
an information-theoretic penalty) whose correctness doesn't depend on which
track produced the data. Section "Constants ledger" below lists every fixed
constant in the document and which category it falls in — that table is the
audit trail.

**2. One robust-statistics primitive, reused everywhere.**
Every place this system needs to ask "is this value unusually large relative
to the rest of this distribution" — the curvature noise floor, the cross-lap
position tolerance, channel scaling before change-point detection, lap-time
outlier weighting — uses the same primitive: median + MAD (median absolute
deviation), compared via a single shared `ROBUST_Z` constant. Four different
ad-hoc thresholds scattered through the code is exactly how the current
detector ended up with a comment that says one number and code that says
another. One named, documented convention, referenced everywhere, closes that
class of bug structurally.

**3. Two primitives, applied recursively, not three fixed stages.**
"Extract candidates from a curvature/pedal signal" and "accumulate candidates
across laps into confirmed, high-confidence corners" are the whole algorithm.
Complex-splitting isn't a third, different mechanism — it's the same two
primitives applied one level down, inside any confirmed corner whose internal
evidence says there's more decision structure. A full joint probabilistic
model (HMM, etc.) was rejected earlier for a structural reason worth
restating: it requires fixing the number of hidden states up front, and the
number of corners is exactly what's unknown for a custom track. Recursive
extract-then-confirm never has to state that number.

## The `Sample` type — the only interface the algorithm sees

```rust
/// Game-agnostic telemetry sample. Every field except `distance_m`, `gas`,
/// and `brake` is optional; the algorithm is required to produce a correct
/// (if less precise) corner list with any subset of the optional fields
/// absent or dead for an entire session.
pub struct Sample {
    pub t: f64,                    // seconds, monotonic within a lap
    pub distance_m: f64,           // canonical along-track distance, see below
    pub position: Option<[f64; 3]>,// world-space; needed for geometric curvature
    pub speed_ms: f64,
    pub heading: Option<f64>,      // radians; sign convention fixed at the adapter
    pub yaw_rate: Option<f64>,     // radians/sec; same sign convention as d(heading)/dt
    pub gas: f64,                  // 0..1
    pub brake: f64,                // 0..1
    pub steer: Option<f64>,        // signed; magnitude scale is game-defined
    pub off_track: Option<bool>,
    pub grip: Option<f64>,
    pub in_pit: bool,
}
```

**Hard requirements**, because nothing downstream can substitute for them:

- `distance_m` — or enough to derive it (raw position plus a lap-boundary
  signal). This is the x-axis of every corner and of cross-lap matching.
- `gas`, `brake` — required for correct sub-corner *counting*. Curvature
  alone finds turns; it can't tell a driver's three-decision complex from one
  smooth arc, and the difference is exactly the part of "canonical corner
  count" that curvature-only detectors get wrong (see the original detector's
  Interlagos result: 15 corners fused down to 9).
- Either `position` or `heading` — curvature can be computed from either (see
  below); a game that supplies neither has no signal this algorithm can use.

Everything else is confirmatory and the pipeline degrades to a slightly
less precise but still-correct answer without it.

## Preprocessing

Run once, before any of Stage A/B/C, on every lap:

**1. Filter to live driving.** Drop frames where the sim isn't actually
running the driver's car (paused, replay, off). Drop in-pit frames from
corner-candidate extraction specifically — keep them for lap-time bookkeeping
below.

**2. Deduplicate stale-field runs.** Some telemetry sources update different
fields at different physical rates within one logical frame stream (position
and the along-track distance projection are common examples — they can lag
the frame rate and repeat verbatim across several consecutive samples). Any
per-distance resampling must first collapse a run of identical `distance_m`
values into a single knot before computing curvature, or the repeated points
produce spurious zero-length segments. This does *not* mean dropping those
samples for time-domain signals (pedal traces, etc.) — only for the
distance-domain geometry step.

**3. Lock a single sign convention.** If both `heading` and `yaw_rate` are
live, compute d(heading)/dt over a few samples (using circular difference to
handle angle wrap) and check its sign against `yaw_rate`'s sign. If they
disagree, negate `yaw_rate` for the rest of the session so every downstream
consumer sees one consistent convention. This is a one-time, cheap,
self-correcting step — it doesn't require knowing in advance which raw field
uses which convention, only that they must agree with each other.

**4. Liveness-gate optional channels.** On the first lap, compute the
variance of `heading`, `yaw_rate`, `steer`. Any channel whose variance is
indistinguishable from float noise (or is entirely absent from the schema)
is excluded from the pipeline for the rest of the session, and downstream
consumers fall back to the next-best channel for that role (see "Curvature
estimation" and "Direction classification" below). This is what turns the
class of regression that produced "9 Right / 0 Left" into a detected,
handled condition instead of a silent one — it doesn't require knowing in
advance *which* field will die next or in which sim.

**5. Segment laps from the distance axis itself, never from a game's lap
counter.** A lap boundary is a backward jump in the unwrapped distance value
larger than half the track length — no legitimate forward motion produces a
jump that large between consecutive samples, so this threshold is a property
of "a lap can't complete in one sample," not a tuned number. Game-supplied
lap counters are frequently unreliable in ways that are specific to that
game's own event ordering (lagging the actual crossing, not incrementing on
a session joined mid-lap) and are exactly the kind of dependency this spec
is trying to avoid.

## Canonical distance axis

If the source can supply a spline-projected, racing-line-independent
distance value directly, prefer it — it's authoritative and doesn't drift
the way integrating speed or accumulating raw position deltas over a lap
does. When no such projection is available, reconstruct one by accumulating
Euclidean distance between deduplicated position samples over the first
complete lap, and use that as the reference parametrization for every
subsequent lap.

**Adaptive spatial grid, not a fixed meter value.** Curvature needs points
spaced roughly evenly along distance, but "resample to 1 m" silently assumes
a sampling density; a sim logging at 20 Hz and one logging at 360 Hz don't
have the same real spatial resolution. Instead: for each query distance `d`,
select the two original (deduplicated) samples nearest to `d − Δ/2` and
`d + Δ/2`, where `Δ` is the *median* inter-sample arc length observed on this
lap so far, floored at the sensor's own resolvable minimum (empirically,
Menger curvature on typical racing-telemetry sampling densities can't
separate two corners closer than roughly 10 m — this is a hardware floor,
not a choice the algorithm makes). No interpolation is needed; curvature is
computed directly on real telemetry points.

## Curvature estimation — dual estimator with a built-in regression canary

Two independent, mathematically equivalent ways to compute curvature exist,
and using both is a deliberate redundancy:

- **Geometric (Menger) curvature**, from three position samples `A, B, C`
  centered at the query distance: `κ = 4·Area(ABC) / (|AB|·|BC|·|CA|)`,
  signed by the cross product's sign for direction. Needs `position`, not
  `heading` — pure geometry, immune to any yaw-integration drift.
- **Heading-derivative curvature**, `κ = dψ/ds` (heading change per unit
  distance, using circular difference for wrap). Needs `heading`, not
  `position` — a valid fallback for a source that exposes orientation but not
  precise world coordinates.

When both channels are live, compute both and compare. Large, sustained
disagreement between them is itself diagnostic: it's the exact signature of
the regression that produced "heading identically 0.0" in the current
detector, and it's caught automatically here without anyone having to notice
a suspicious value by eye. When only one is live, use it; when neither is,
Stage A cannot run for this session (per "hard requirements" above).

## Stage A — per-lap candidate extraction

Runs once per lap, on that lap's own samples only.

1. Compute `κ(d)` for the lap using the estimator selected above.
2. Threshold via **Otsu's method** on the histogram of `|κ|` values for this
   lap: the threshold that maximizes between-class variance. This replaces a
   fixed percentile — it doesn't require guessing what fraction of a lap is
   straight, and it degrades gracefully (rather than breaking) on a track
   with little or no real straight, because it operates on the whole lap's
   distribution rather than assuming a particular shape for it. Otsu also
   returns `η`, the fraction of variance the split explains — a built-in
   confidence signal for how trustworthy this lap's threshold is (a low `η`
   is exactly what a Monaco-like track looks like, and is folded into peak
   confidence below rather than treated as a special case).
3. Local maxima of `κ(d)` above the Otsu threshold are candidates. For each,
   record: distance, peak height, direction (sign of `κ` or heading rate),
   `η` for this lap, whether `brake` crossed a per-lap Otsu-style threshold
   before the peak, and whether `gas` reached sustained travel after it.

```rust
// Illustrative — O(n) after O(1) histogram bins.
fn otsu_threshold(curvature: &[f64], bins: usize) -> (f64 /* T */, f64 /* eta */) { .. }
```

Stage A's job is recall, not precision: over-generating a few spurious
candidates that Stage B votes down over several laps costs nothing; a real
corner Stage A never proposes can never be recovered downstream.

## Confidence model

Every candidate — top-level corner or sub-corner — carries a Beta-Bernoulli
confidence, updated once per lap with weighted evidence:

```rust
struct Confidence { a: f64, b: f64 } // prior Beta(1, 1)

impl Confidence {
    fn update(&mut self, evidence_weight: f64) { // in [0, 1]
        self.a += evidence_weight;
        self.b += 1.0 - evidence_weight;
    }
    fn confirmed(&self) -> bool {
        beta_inv_cdf(1.0 - CREDIBLE_LEVEL, self.a, self.b) > 0.5
    }
}
```

`evidence_weight` is not simply 1.0/0.0 for hit/miss — it's scaled by this
lap's Otsu `η` (a marginal lap contributes less) and by the lap-typicality
weight below (an atypical lap contributes less). Early on, the wide Beta
distribution demands near-unanimous evidence to clear `CREDIBLE_LEVEL`; as
laps accumulate it narrows, so isolated misses stop being fatal. This
produces the "strict-then-lenient" convergence behavior a hand-designed
vote schedule (require 3/3, then 4/6, then 6/10 laps) was approximating by
hand, as a derived consequence instead of a lookup table — and it comes with
a continuous, auditable confidence number per corner for free.

**Lap-typicality weight.** Maintain a running robust median and MAD of
completed lap times. Weight a lap's evidence contribution by a smooth
(logistic) falloff of how many `ROBUST_Z` multiples of MAD its time sits
from the median — an out-lap, a spin-heavy lap, and a wet-track outlier all
get the same treatment: down-weighted, never discarded outright, consistent
with treating driver error as evidence rather than noise to reject. The
first lap of a session gets full weight by construction, since there's
nothing yet to compare it against.

## Stage B — cross-lap accumulation

Runs once per lap, after Stage A, against the running model (tens of
corners, not the sample stream — this keeps it well clear of the O(L²)
all-laps problem that made the previous detector's master-lap selection
unusable on the live path).

**Correspondence via sequence alignment, not greedy nearest-match.** Corners
are strictly ordered along distance, and that order is stable across laps
(barring a driver literally skipping a corner). Align this lap's candidate
list against the model's corner list the way two biological sequences are
aligned: cost of a match = distance/direction mismatch; cost of an insertion
= an unmatched candidate this lap (new corner, or a spurious wiggle); cost of
a deletion = a model corner with no evidence this lap. Solve by dynamic
programming — cheap, since both lists are tens of items long. This is what
actually delivers driver-error tolerance: a spin that briefly produces an
extra spurious curvature peak becomes an ignorable insertion instead of
shifting every corner after it by one slot, which is the failure mode a
greedy nearest-match invites.

**Position and tolerance via streaming robust statistics, not a running
mean.** For each matched corner, update a streaming median (P² algorithm —
O(1) memory, O(1) per-sample update, fits the no-buffering constraint) of
its apex distance across laps, and a streaming MAD alongside it. Set the
matching tolerance to `ROBUST_Z · MAD_running`, floored at the ~10 m
resolution limit from the curvature section — tolerance never has to be told
a track-specific number, and it can't shrink below what the sensor can
actually resolve even after several very consistent laps.

## Stage C — recursive complex/sub-corner splitting

Runs once per lap, after Stage B, on every confirmed corner's span —
regardless of length. There is deliberately no length threshold gating
whether this runs (a fixed multiple of median corner length was considered
and rejected: the pedal/heading evidence itself should decide whether a span
needs splitting, not a guessed multiple).

1. Standardize each of `[gas, brake, yaw_rate, steer]` within the span using
   the same median/MAD primitive as everywhere else in this document — this
   matters because these channels are on incomparable native scales, and an
   un-standardized change-point cost function would let whichever channel
   happens to have the largest raw units dominate.
2. Run change-point detection (PELT, penalized by BIC: `k·ln(n)` per change
   point, `k` = channel count, `n` = segment length — an information-
   theoretic penalty, not a fitted one) over the standardized multivariate
   series.
3. Zero change points found → the span is one corner. This is correct
   output for a driver smooth enough to leave no statistical trace of a
   second decision, not a fallback being settled for.
4. Each change point found is a sub-corner boundary. Because `yaw_rate` is
   one of the watched channels alongside the pedals, a direction reversal
   with a flat pedal trace (a flick taken at sustained mid-corner throttle)
   is caught the same way a braking transition is — there's no separate
   fallback rule needed for that case.
5. Sub-corner candidates are confirmed exactly like top-level corners: the
   same `Confidence` struct, the same recursive call into Stage B logic,
   treating the corner's span as its own miniature lap. This is the
   "recursive, not three fixed stages" principle made concrete.

Entry/apex/exit for any confirmed corner or sub-corner: entry = last sample
before `brake` crosses this lap's own Otsu-style pedal threshold; apex =
sample of maximum `|κ|` (recorded as both a fixed apex and, when it differs
meaningfully, a late-apex position, since fast sweepers genuinely have two);
exit = first sample where `gas` reaches sustained travel, or where `κ`
returns below the local baseline, whichever the live channels support.

## Convergence — when has the canonical corner count been found

```rust
fn is_converged(model: &TrackModel, laps_seen: usize) -> bool {
    laps_seen >= 3
        && model.corners.iter().all(|c| c.confidence.confirmed())
        && model.laps_since_last_topology_change() >= 2   // promotion/demotion
        && model.laps_since_last_subcorner_split() >= 2   // Stage C found nothing new
}
```

Corner-count stability alone is an insufficient stopping condition: two
corners wrongly fused into one complex will look "stable" indefinitely if
only the top-level count is checked. Convergence therefore requires both the
top-level accumulation (Stage B) and the recursive splitter (Stage C) to
have stopped finding new structure, not just the former.

```rust
fn canonical_corner_count(model: &TrackModel) -> usize {
    model.corners.iter()
        .filter(|c| c.parent_id.is_none())
        .map(|c| {
            let confirmed_children = model.corners.iter()
                .filter(|s| s.parent_id.as_deref() == Some(&c.id) && s.confidence.confirmed())
                .count();
            confirmed_children.max(1) // no confirmed children => the parent itself is one corner
        })
        .sum()
}
```

## `TrackModel` schema

```json
{
  "schema_version": 1,
  "track_length_m": 4286.79,
  "identity": { "track_name": "ks_red_bull_ring", "track_configuration": "layout_gp" },
  "laps_seen": 7,
  "corners": [
    {
      "id": "c03",
      "distance_m": 812.4,
      "direction": "left",
      "entry_m": 780.1,
      "apex_m": 815.0,
      "exit_m": 860.2,
      "confidence": 0.94,
      "evidence_laps": 7,
      "parent_id": null
    },
    {
      "id": "c03a",
      "distance_m": 808.0,
      "direction": "left",
      "entry_m": 780.1,
      "apex_m": 808.0,
      "exit_m": 822.0,
      "confidence": 0.81,
      "evidence_laps": 5,
      "parent_id": "c03"
    }
  ]
}
```

Note on identity: don't trust `track_name`/`track_configuration` alone to
decide whether a stored model matches the current session — they're
game-supplied strings and custom tracks aren't guaranteed unique or even
present. Treat `track_length_m` as a cheap sanity check: if it differs
materially from the stored model's, refuse to reuse the model rather than
silently applying a corner list from a different track.

The `Corner`/`TrackModel` types should derive `Serialize`, `Deserialize`,
`Clone`, `Debug` from the start — the previous type not deriving any of
these was itself one of the defects in the old design.

## Reference adapter — Assetto Corsa `AcFrame` → `Sample`

This is the only sim-specific part of the whole design; everything above
operates on `Sample` alone.

| `Sample` field | From `AcFrame` | Note |
|---|---|---|
| `t` | `timestamp` (ms → s) | wall-clock, ordering/dt only, per the struct's own doc comment |
| `distance_m` | `normalized_car_position * track_spline_length` | authoritative, spline-projected, ~2 cm/frame accuracy — use this over integrating `position`, not just as a fallback |
| `position` | `[pos_x, pos_y, pos_z]` | updates at ~38 Hz within a faster frame stream — dedup before curvature, per Preprocessing step 2 |
| `speed_ms` | `speed_kmh / 3.6` | |
| `heading` | `heading` | already radians, matches `-atan2(dx, dz)` per the struct's own measured error — reliable enough to serve as the curvature cross-check |
| `yaw_rate` | `-local_ang_vel_1` | **sign is flipped versus `d(heading)/dt`** per the struct's doc comment — this is exactly the kind of per-source convention the sign-lock-in preprocessing step exists to catch even if it weren't documented |
| `gas` / `brake` | `gas` / `brake` | already 0..1 |
| `steer` | `steer_angle` | measured range is asymmetric (≈ −0.91..1.00), not a clean ±1 — standardize per-lap via median/MAD (Stage C step 1) rather than assuming a fixed symmetric scale |
| `off_track` | `tyres_out > 0` | coarse; per-wheel slip/pressure aren't in this field contract at all, and nothing here structurally requires them — they'd only sharpen entry-point precision if added later |
| `grip` | `surface_grip` | |
| `in_pit` | `in_pits()` | uses the struct's own helper |
| lap boundary | *(none — derive from `distance_m` wrap)* | `completed_laps` is explicitly documented as unreliable for this in the struct's own comments (lags the crossing, doesn't increment on a mid-session join) — reinforces Preprocessing step 5's rule to never use a game's own lap counter |

## Constants ledger

| Constant | Value | Used for | Why it's not track-tuned |
|---|---|---|---|
| `ROBUST_Z` | 3.0 | noise-floor confidence, position tolerance floor, lap-typicality weighting, channel standardization | modified z-score convention (Iglewicz & Hoya); a property of the MAD estimator, identical for any distribution |
| `CREDIBLE_LEVEL` | 0.90 | corner/sub-corner confirmation | one-sided 90% confidence bound — a standard statistical convention |
| BIC penalty | `k · ln(n)` | PELT change-point stopping | Schwarz (1978) information criterion, derived, not fitted |
| Otsu bins | 256 | curvature histogram resolution | numerical precision bound tied to sensor float noise, not to any track's geometry |
| min. resolvable corner spacing | ~10 m | spatial grid floor | a hardware fact about position-sampling density, not an algorithmic choice |
| lap-wrap threshold | "backward jump > half the track length" | lap segmentation | self-justifying: no legitimate forward motion produces a jump that large in one sample |
| spatial grid `Δ` | median inter-sample arc length, this lap | curvature computation | derived per lap, not fixed |

## Complexity / streaming compliance

| Stage | Runs | Complexity | Holds |
|---|---|---|---|
| Preprocessing | once per lap | O(samples in lap) | one lap's samples, transiently |
| Stage A | once per lap | O(samples in lap) | one lap's samples |
| Stage B | once per lap | O(candidates × model corners) | the model (tens of corners) |
| Stage C | once per lap, per confirmed span | O(samples in span) | one span's samples, transiently |
| Live tracker (out of scope here) | every sample, 100 Hz | O(1) | nothing — only the frozen `TrackModel` |

None of the learner-side stages touch the live 100 Hz path or hold more than
one lap in memory at a time, which is the distinction that matters against
the "no lap-sized buffer" constraint: that constraint is about the live
path, and the learner already runs once-per-lap by design.

## Traceability to the existing detector's defects

- **D8** (signed curvature smoothed then `.abs()`'d, cancels chicanes) —
  closed: curvature is never smoothed as a signed mean; Otsu operates on
  `|κ|` values directly and PELT operates on standardized raw channels.
- **D9** (threshold comment vs. code disagreement) — closed structurally:
  one named `ROBUST_Z`, referenced everywhere a threshold is needed, instead
  of independent inline constants.
- **D10** (unbounded index arithmetic, panicking `partial_cmp().unwrap()`) —
  use `f64::total_cmp` for all curvature/confidence comparisons; bounds-check
  every distance-axis lookup against the deduplicated sample list's actual
  range.
- **D11** (O(L²) all-pairs master-lap selection) — closed: Stage B never
  compares laps to each other, only each lap's candidates to the running
  model.
- **D13** (`TrackCorner` derives nothing) — closed: `Corner`/`TrackModel`
  derive `Serialize`, `Deserialize`, `Clone`, `Debug` from the outset.

## Known limitations, unresolved by this spec

- Otsu's quality (`η`) on a genuinely single-mode, no-straight track is
  still lower than on a track with clear straights; the confidence-weighting
  mitigates this but doesn't eliminate the extra laps such a track will
  need to converge.
- The ~10 m curvature resolution floor is a hardware limit, not something
  any algorithm change here can improve — two canonical corners closer than
  that will not be separable regardless of confidence-model refinements.
- Very short custom tracks (tight kart-track-scale loops) haven't been
  validated against the streaming-quantile tolerance logic; it's plausible
  MAD-based tolerance needs more than 10 laps to stabilize when corner
  spacing is itself close to the resolution floor. This needs empirical
  testing, not a guess.