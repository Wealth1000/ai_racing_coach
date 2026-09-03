# Canonical corner detection — a threshold-minimal design

*GLM-5.3's independent answer to `Corner_detection_problem.md`, written after
reading `Corner_detection_solution.md` (Sonnet 5) and the existing
implementation. Where the two documents agree, this one says so once and moves
on; where they disagree, this one argues its case. Sections marked
**[repo]** point at code that already exists and is reused rather than
rewritten.*

## 0. The problem, restated as I'm solving it

Given between 3 and 10 laps of one mediocre controller-driving human on one
unknown track, produce the canonical corner list — the set of places where the
driver makes a decision that costs lap time — with:

- no track prior, no per-circuit tuning, no game-specific dependency;
- tolerance for lap-to-lap line variation, minor mistakes, and controller
  jitter;
- no reliance on absolute thresholds, because corners span "infinite" possible
  radii, lengths, and combinations.

Two facts from the repo's own measurements shape everything:

1. **A single lap is a coin flip.** The MX5 capture detected 13, 10, 13
   corners across three clean laps of a 10-corner circuit; the F138 10, 10, 9
   (`src/features/track_model.rs` module docs). The real corners recur at
   the same distance every lap; the phantoms never repeat.
2. **The signals are partial by design.** AC's graphics page repeats position
   on ~40% of frames (`src/features/resample.rs`); AMS2 replay has no per-wheel
   or grip channels at all (`Corner_detection_problem.md`). Any stage that
   hard-depends on a channel is a defect waiting for the next sim.

## 1. The core insight

> **The only threshold that generalizes across tracks is not a number — it is
> cross-lap agreement.**

Everything a real corner is can be stated as a *correlation* property rather
than a *magnitude* property:

| Property of a real corner | Why thresholds fail on it |
|---|---|
| Present at roughly the same distance every lap | The distance varies by tens of metres with the line (measured: 71 m spread on one Red Bull Ring kink) |
| Same direction every lap | Magnitude scale differs per car/track by 10× |
| Similar turn angle every lap | A 60 m hairpin and a 400 m sweeper are both corners |
| Induces a decision in the driver (pedal work, sometimes a yaw-rate reversal) | Geometry alone cannot split a complex the driver treats as several decisions |

Jitter and driver error are, by contrast, *uncorrelated* between laps. So the
algorithm's shape is forced: generate candidates per lap with deliberate
over-generation (recall at all costs, no magnitude gate), then let cross-lap
consensus be the sole arbiter of existence. Any magnitude test that survives
into the design exists only to keep the candidate list finite, and must be
derived from that lap's own noise scale — never from a notion of "how curved a
corner is".

This is the same conclusion Sonnet's document reaches. Where we differ is in
the *candidate generator* (§3) and the *confidence arithmetic* (§4.3), and I'll
argue both.

## 2. Input contract and preprocessing

### 2.1 The interface

The algorithm consumes the crate's existing `Sample`
(`src/core/sample.rs`) — not a new type. Hard requirements, inherited from
the problem:

- **`lap_distance`** (spline-projected metres): the x-axis of everything. When
  a source lacks a spline projection, reconstruct it by accumulating Euclidean
  position deltas over the first complete lap; but prefer the source's own
  projection when present (AC's `NormalizedCarPosition × TrackSPlineLength`
  tracks true distance to ±2 cm — **[repo]** `frame.rs` docs).
- **`heading` or `pos`**: at least one must be live (curvature is computable
  from either).
- **`throttle` / `brake`**: required for sub-corner *counting* (§5), not for
  top-level corner existence.

Everything else (`steer`, `yaw_rate`, `slip_angle`, `tyres_out`,
`surface_grip`) is confirmatory and the pipeline degrades without it.

### 2.2 Per-lap preprocessing (all **[repo]** or trivial)

1. **Gate live driving**: drop non-live and in-pit frames (`AcFrame::is_live`,
   `in_pits` — or the source's equivalents).
2. **Deduplicate stale distances**, then **resample onto an even distance
   grid** — `src/features/resample.rs` exists precisely because AC's graphics
   page makes 76–81% of raw Menger evaluations degenerate. Grid step: the
   median live inter-sample spacing of this lap, floored at the ~10 m
   two-corner resolution limit that Menger curvature on this data admits
   (measured, `Corner_detection_problem.md`). A 20 Hz sim and a 100 Hz sim
   then get the same effective grid without either over- or under-resolving.
3. **Lap boundaries from the distance wrap**, never the game's lap counter
   (AC's `CompletedLaps` lags and misses mid-session joins — measured,
   `frame.rs` docs). Boundary = backward jump larger than half the track
   length; that is a property of "a lap cannot complete in one sample", not a
   tuned number.
4. **Sign lock.** Compute signed curvature both ways — Menger from `pos`, and
   dψ/ds from `heading` (circular difference) — and fix the session
   convention by whichever sign relationship the majority of samples show.
   The two estimators are then permanently cross-checked: sustained
   disagreement is the exact signature of a dead or lying channel (the
   regression that produced "9 Right / 0 Left"), caught here as a loud
   condition rather than four stages later.
5. **Channel liveness gates**: a channel whose per-lap variance is float noise
   is excluded for the session; consumers fall back per §6.

## 3. Stage 1 — candidate arcs by MDL segmentation of the rotation profile

This is my first substantive departure from the Sonnet document, which
thresholds `|κ|` with Otsu's method. I reject Otsu for a structural reason:
**Otsu assumes the curvature histogram is bimodal** (straight vs corner). On a
track with no real straights — Monaco, or a kart loop — or with a unimodal
distribution of medium corners, it places the split near the mode of the
distribution, which is exactly where the answer is most ambiguous. Sonnet's
own limitations section concedes this. The whole pipeline's candidate quality
gates on that split.

### 3.1 The signal

Use the **cumulative rotation** θ(s): total signed heading change since the
lap origin, unwrapped past ±π. This already exists —
`curvature::cumulative_rotation` **[repo]** — and it is seam-safe where raw
heading differencing is not.

θ(s) is the right signal for a physical reason: a circuit is, by construction,
a sequence of constant-curvature arcs joined by transitions. θ(s) is therefore
*piecewise linear*:

- a segment's **slope is the curvature** (sign = direction, 1/|slope| = radius);
- a segment's **vertical extent is the turn angle**;
- a segment's **horizontal extent is the corner's span** — entry and exit come
  free as breakpoints.

And it is immune to the D8 failure class *by construction*: D8 (chicane
cancellation) happened because *signed* curvature was smoothed and then
`.abs()`'d, letting opposite peaks cancel inside a window. θ is the
*integral* of signed curvature; an S-bend is an S in θ — up then down — and
nothing is ever averaged across the reversal. There is no smoothing window on
the primary signal at all.

### 3.2 The estimator

Fit the piecewise-linear model by **minimum description length**: choose the
breakpoint set B minimizing

```
n · ln(RSS(B) / n)  +  p · |B| · ln(n)
```

where RSS is the residual sum of squares of the piecewise-linear fit, n is the
grid length, |B| the number of segments, and p ≈ 2 parameters per segment
(slope + breakpoint). The first term is the data cost; the second is the
Schwarz (BIC) penalty — an information-theoretic convention, not a fitted
number.

Solve it with the standard O(n²) dynamic program over segment costs, each cost
evaluated in O(1) from prefix sums of `s`, `s²`, `θ`, `sθ`. For a 5 km lap on
a 1 m grid that is ~25 million cheap operations, once per lap, on the learner
side — tens of milliseconds in release mode. (PELT pruning can be added later
as an optimization; correctness does not need it.)

**Noise is estimated from the data, per lap, in two passes.** First
over-segment deliberately (penalty halved), measure the residual scale — at
that level the residual is dominated by sensor noise, not by missed structure
— then re-fit at the BIC penalty with σ² fixed to that measurement. This is
the self-calibration step that replaces every "fraction of the 95th
percentile" knob the old detector had: a 20 Hz noisy source gets a bigger σ,
a higher penalty in absolute terms, and correctly coarser segments.

### 3.3 From segments to candidate arcs

A segment is a **candidate arc** iff its slope exceeds the per-lap noise
scale of curvature, σ_κ = MAD(κ) over this lap, by the conventional
modified-z factor (3, Iglewicz–Hoaglin — the same convention used everywhere
else in this design; see §8). Since most of any lap is near-straight, the
median absolute curvature *is* the noise floor, whatever the track. A track
with no straights does not break this: the MAD is simply measured on its own
low-curvature sections, whatever they are.

Each candidate carries: midpoint, span, signed slope (direction + radius),
turn angle (slope × span), and the per-lap σ_κ it was judged against.

Stage 1's job is **recall, not precision**. Over-segmentation is cheap —
Stage 2 votes spurious arcs away — but an arc Stage 1 never proposes is
unrecoverable downstream. The MDL penalty is therefore allowed to err
generous; if in doubt, split.

### 3.4 The start/finish seam

Distance is circular, so the learner treats it that way everywhere: all
distances compared modulo track length, and the DP alignment (§4) runs on the
candidate ring. This dissolves the "corner straddling the line" problem —
known-open in the current `track_model.rs` — rather than special-casing it.
When a single lap's segmentation is needed on a linear axis (the DP wants a
start point), rotate the lap to start at its longest run of sub-noise
curvature — the longest straight is the natural lap origin, and *that* is a
derived quantity. On a track with no straight at all, any stable rotation
works; the ring matching makes the choice immaterial.

## 4. Stage 2 — cross-lap consensus

### 4.1 Alignment, not nearest-match

Corners are strictly ordered along the ring and that order is stable across
laps. Match each lap's candidate list to the running model the way two
sequences are aligned: dynamic programming over (match, insert, delete) with

- **match cost** = (circular distance from candidate midpoint to the model
  corner's running median)² / τ², where τ = max(running MAD of that corner's
  midpoints, the ~10 m resolution floor), **plus** a direction-mismatch
  term that is prohibitively large (an opposite-hand candidate is a different
  corner, full stop — the current `nearest_candidate` **[repo]** already
  enforces this and is right to);
- **gap costs** (insertion = unmatched candidate, deletion = unmatched model
  corner) = a small multiple of the typical match-cost scale, the standard
  affine-gap convention from sequence alignment.

Both lists are tens of items; the DP is trivial in cost. What it buys is the
actual driver-error tolerance the problem demands: a spin that manufactures a
spurious arc, or a skipped kink, becomes a local insertion/deletion instead of
shifting every subsequent corner by one slot — the failure a greedy
nearest-match invites. (The existing `support_counts` **[repo]** is greedy
nearest-candidate with one-vote-per-lap bookkeeping; it works at 25 m
tolerance on Red Bull Ring but has no way to represent "one lap produced
three arcs where another produced one" other than by luck of apex placement.)

### 4.2 What each lap contributes

After alignment, every model corner has, per lap, either a matched
observation (midpoint, span, turn angle, radius) or a miss. New candidates
unmatched to anything enter the model as *tentative*.

Laps are classified, not discarded, using the lap-time robust-z (median/MAD
of lap times, the same primitive as everywhere):

- **Representative laps** (typical time, clean by `LapQuality` **[repo]**)
  vote on existence *and* contribute geometry to the running medians.
- **Atypical laps** (out-lap pace, spin-heavy, off-track) still vote on
  existence — a spin at T7 is strong evidence T7 exists — but contribute no
  geometry, because their line is distorted and would drag the medians.
  Down-weighted, never thrown away: this is the problem statement's "player
  error is an input, not an outlier" made operational.

Because ≤10 laps are in play, the model stores **every observation** and
computes exact medians and MADs. Streaming quantile machinery (P², t-digest)
solves a problem this design does not have — the live path holds only the
frozen model — and approximate quantiles would only add error where exactness
is free.

### 4.3 Confirmation: a majority with a confidence bound

A tentative corner is **canonical** when

```
Wilson_lower(p̂, W_eff, α = 0.10, one-sided) > 1/2
```

where p̂ is the fraction of laps that matched it and W_eff the effective
number of voting laps (sum of lap weights; binary votes keep W_eff = laps
seen, fractional weights from §4.2 only depress atypical laps' *geometry*
contributions in this design).

This is my second departure from the Sonnet document, which uses a
Beta-Bernoulli confidence whose evidence is scaled by Otsu's η and a
lap-typicality weight. Those soft weights are exactly the kind of unmoored
numbers the "no magic numbers" rule exists to prevent — a 0.6-weighted vote
is not a measurement of anything. The binomial rule has one parameter with a
defensible meaning: **"canonical" means "more likely than not present,
demonstrated with statistical confidence"** — and the confidence interval
reproduces the strict-then-lenient schedule the problem statement sketched by
hand, as a derivation instead of a lookup:

| Laps seen | Matches required by the rule | Problem doc's hand guess |
|---|---|---|
| 2 | 2 | — |
| 3 | 3 | 3/3 |
| 5 | 4 | — |
| 6 | 4 | 4/6 |
| 8 | 5 | — |
| 10 | 7 | 6/10 |

The derived schedule is marginally stricter at 10 laps than the guess. That
is the honest answer, not a regression: with 10 laps on the table, 6 is not
evidence of "more likely than not" — it is evidence of "this driver misses
this corner 40% of the time", which is itself coaching output, not a reason to
omit it from the canonical set.

Demotion is symmetric: a canonical corner whose running match fraction's
Wilson *upper* bound falls below ½ is demoted back to tentative. Nothing is
ever deleted outright within a session.

### 4.4 Geometry of a confirmed corner

Boundaries, apex, direction are per-field exact medians across the
representative laps' matched observations — medians, not means, so one wild
lap cannot drag an apex; and per-field, not "one reference lap wholesale" as
the current `track_model.rs` does. The repo's own measured objection to
averaging was that *means* of wandering boundaries produce numbers no lap
drove (`track_model.rs` docs); the median of the MX5's 2164/2205/2235 m apex
spread is 2205 m — a lap's actual value — which is the property that objection
actually wanted. The reference lap (medoid, `features::line` **[repo]**) is
still recorded in provenance for audit, it just stops being the geometry.

## 5. Stage 3 — decisions inside arcs (sub-corners)

The problem statement is explicit that counting curvature arcs is not counting
corners: Maggotts–Becketts–Chapel is several decisions inside roughly one
geometric gesture. The *decisive* signal is where the driver works — pedals,
and occasionally the yaw trace — and it must be treated the same way geometry
was: extract events per lap, then keep only the events that recur.

### 5.1 Event taxonomy (per lap, per confirmed arc span)

| Event | Definition | Reuse |
|---|---|---|
| **Brake onset** | first sample of a braking run (sustained above the pedal-noise level, gaps shorter than a sustain window tolerated) | `corner_features::braking_run_start` **[repo]** already implements exactly this |
| **Brake release** | end of that run | same walk |
| **Throttle dip / resumption** | local minimum of throttle below this lap's own flat-out level (its upper quantile), and the sustained return from it | `first_extreme` + sustain logic **[repo]** |
| **Flat-pedal direction change** | a ψ̇ sign reversal with pedals flat — the mid-throttle flick | needs only `yaw_rate`/`heading` |

Pedal "on/off" levels are derived per lap from the pedal trace's own
distribution (the trace is bimodal by nature: a foot is either on a pedal or
off it — MAD-based levels, not fixed 0.05/0.95).

### 5.2 Confirmation, recursively

Cluster events of the same type across laps by circular distance; a cluster
whose recurring-member fraction clears the same §4.3 Wilson bound is a
**confirmed decision boundary**. This is deliberately the *same* machinery as
Stage 2 applied one level down — no third mechanism, no separate
per-span model. The tolerance is the cluster's own running MAD floored at the
resolution limit, as everywhere.

The canonical decision count of a top-level arc is:

```
1 + (number of confirmed interior decision boundaries)
```

Two asymmetries are intended and match the problem's philosophy:

- **One gesture, several decisions** (the driver brakes, releases, brakes
  again through a complex): split. The pedal trace cannot lie about work the
  driver did.
- **Several gestures, one decision** (flat-out linked esses): *not* split.
  If the driver leaves no statistical trace of a second decision, the complex
  genuinely is one coaching anchor, and the model reports it that way — the
  retreat the problem statement explicitly accepts.

Entry/exit placement for any corner or sub-corner then reads naturally off
the events: entry = the confirmed brake-onset cluster (or the arc's leading
breakpoint where no braking exists — flat corners are real); apex = the
|κ| maximum inside the partition (with the repo's existing
geometric-vs-heading apex distinction carried along); exit = the confirmed
throttle-resumption cluster or the trailing breakpoint.

### 5.3 Degradation, stated honestly

With dead pedal channels (AMS2 replay), Stage 3 cannot run and the model
reports the geometric arc count, flagged as such in the model's provenance.
That is a documented retreat from the full goal, consistent with "degrade
gracefully" — not a silent one. With `yaw_rate`/`heading` also dead, Stage 1
cannot run and the learner refuses loudly (`NotEnoughData` **[repo]** error
type), because a model built from nothing would be worse than no model.

## 6. Convergence

```rust
fn is_converged(model: &TrackModel, laps_seen: usize) -> bool {
    laps_seen >= 3
        && model.corners.iter().all(|c| c.is_confirmed())
        && model.laps_since_any_promotion_or_demotion() >= 2
        && model.laps_since_any_new_decision_boundary() >= 2
}
```

Corner-count stability alone is an insufficient stopping rule — two corners
wrongly fused into one arc look perfectly stable forever if only the
top-level count is watched (the 15→9 Interlagos failure was stable in exactly
this sense). Both the arc level and the decision level must have gone quiet.

Expected behaviour: a reasonably clean driver confirms most corners by lap 3
(3/3 clears the bound), nearly all by lap 5–6, with the stubborn complexes
needing the remaining laps to settle their interior structure. The hard cap
of 10 laps comes from the data budget, not the algorithm; if 10 laps are
insufficient the model is emitted with per-corner confidence attached rather
than refused, because "probably right, here's how sure" beats nothing.

## 7. Output — the existing `TrackModel`, extended

The schema stays `src/features/track_model.rs`'s JSON shape (it already
derives, validates, saves atomically, and checks track identity — **[repo]**),
with a version bump and three additions:

```json
{
  "version": 2,
  "track": { "track": "ks_silverstone", "layout": "gp" },
  "track_length_m": 5803.9966,
  "laps_seen": 7,
  "corners": [
    {
      "id": 7, "parent_id": null,
      "start_m": 1788.0, "apex_m": 1875.0, "end_m": 1899.0,
      "direction": "Left", "turn_angle": -1.83,
      "radius_m": 52.0, "support": 7, "match_fraction": 1.0,
      "decision_events": [
        { "type": "BrakeOnset",   "distance_m": 1701.0, "support": 6 },
        { "type": "ThrottleDip",  "distance_m": 1836.0, "support": 5 },
        { "type": "ThrottlePickup","distance_m": 1888.0, "support": 7 }
      ]
    }
  ],
  "provenance": { "...": "as today, plus estimator and σ_κ per lap" }
}
```

- `parent_id` supports sub-corner partitions when a later revision chooses to
  materialize them as corners; the MVP computes the count from
  `decision_events` and keeps one row per top-level arc.
- `match_fraction` / per-event `support` surface confidence instead of hiding
  it — a coach that says "Turn 9, medium confidence" is more honest than one
  that cannot.
- Validation gains the obvious new invariants (event distances inside the
  span; support ≤ laps_seen) in the existing `validate()` **[repo]**.

Downstream, `corner_features::extract` and the live tracker are untouched:
they consume `(start_m, apex_m, end_m)` windows, which this model still
provides.

## 8. Constants ledger

Every fixed number in this design, and why it is not track-tuned:

| Constant | Value | Used for | Why it's not track-tuned |
|---|---|---|---|
| BIC penalty | `p·|B|·ln(n)` | segmentation (§3.2) | Schwarz (1978); a property of the estimator, invariant to data scale |
| modified-z | 3 | slope-vs-σ_κ gate, lap typicality, pedal levels | Iglewicz–Hoaglin convention for MAD-based outlier tests |
| consensus bound | Wilson lower > ½ at α = 0.10 (one-sided) | corner/event confirmation | "canonical = majority, with confidence"; α is a standard statistical convention, and ½ is the definition of majority, not a tuning knob |
| gap-penalty multiple | small fixed multiple of τ² | alignment DP (§4.1) | sequence-alignment convention (affine gaps); sits in units of the data-derived match scale |
| resolution floor | ~10 m | tolerance floor, grid floor | measured hardware property of Menger curvature at these sampling densities |
| lap-wrap threshold | backward jump > L/2 | lap segmentation | self-justifying: no forward motion covers half a track in one sample |
| σ_κ, τ, pedal levels, grid step, lap origin | — | everywhere | all derived per lap from that lap's own distribution |

Compare with the old detector: seven knobs (`threshold_fraction`,
`min_threshold`, `exit_hysteresis_m`, `min_corner_length_m`,
`min_peak_curvature`, `min_turn_angle`, `merge_gap_m`), every one an absolute
number someone guessed at a particular circuit, and one of which already
disagreed with its own doc comment (D9). This table has no entry of that kind.

## 9. Complexity and the streaming boundary

| Stage | Runs | Cost | State held |
|---|---|---|---|
| Preprocess + resample | per lap | O(lap) | one lap, transiently |
| MDL segmentation | per lap | O(n²) DP, n ≈ grid points, offline learner | one lap |
| Alignment + consensus | per lap | O(candidates × model corners) — tens × tens | the model (tens of corners, ≤10 observations each) |
| Event clustering | per lap, per arc | O(events²) within one arc | events per arc |
| Live tracker | per sample, 100 Hz | O(1) range check | nothing — frozen model only |

The learner is a `coach learn-track`-style offline pass, exactly as the
architecture (§3.1 of `docs/implementation-plan.md`) already decrees; the live
path never touches anything but the frozen JSON. This closes D11 by
construction: no stage compares laps to each other, ever — each lap talks only
to the running model.

## 10. Traceability to the defect catalogue

- **D8** (signed curvature smoothed then `.abs()`'d) — closed *structurally*:
  the primary signal is the integral θ(s); nothing is ever averaged across a
  direction reversal, and no smoothing window exists on the detection signal.
- **D9** (comment/code threshold disagreement) — closed: no absolute
  curvature threshold exists anywhere; the §8 ledger is exhaustive.
- **D10** (index underflow, `partial_cmp().unwrap()` panics) — closed:
  `f64::total_cmp` throughout; all distance lookups bounds-checked against the
  grid's actual range (the repo's newer code already follows this convention —
  keep it).
- **D11** (O(L²) all-pairs master-lap) — closed: §9.
- **D13** (nothing derives) — already closed in `track_model.rs`; the v2
  additions inherit it.

## 11. Where I agree and disagree with `Corner_detection_solution.md`

**Agreed and kept:** cross-lap consensus as the arbiter; sequence alignment
over greedy matching; dual curvature estimators with disagreement as a
canary; sign-lock preprocessing; recursive sub-corner confirmation; the
degrade-gracefully channel contract; ~10 m floor; never trusting game lap
counters. These are right, and two of them (alignment, canary) I'd steal
verbatim.

**Replaced, with reasons:**

1. **Otsu on `|κ|` → MDL segmentation of θ(s)** (§3). Otsu's bimodality
   assumption fails precisely on the tracks that are hard, and everything
   downstream inherits that fragility. The MDL formulation makes no
   distributional assumption, estimates its own noise per lap, produces
   entry/exit/direction/radius as segment attributes rather than requiring a
   separate hysteresis state machine to find spans, and is immune to D8 by
   construction rather than by care.
2. **Weighted Beta-Bernoulli → exact binomial Wilson bound** (§4.3). Same
   strict-then-lenient behaviour, one parameter with a defensible meaning,
   and none of the soft evidence-weights (η-scaled, typicality-scaled) that
   quietly reintroduce the unmoored numbers the design was supposed to
   eliminate.
3. **Per-lap PELT on standardized multi-channel pedal series → discrete
   decision-event clustering** (§5). Pedal transitions are discrete,
   physically-named events; extracting them per lap (with logic the repo
   already has) and clustering across laps reuses the Stage-2 machinery
   instead of adding a second, differently-calibrated change-point model
   whose BIC penalty across standardized incomparable channels is the
   shakiest constant in that document.

**One deliberate scope difference:** that document specifies a new `Sample`
type with Option-fields and an adapter table; this one targets the crate's
existing `Sample` and the existing `TrackModel`/`ModelCorner` types, so the
design is implementable as a replacement of `detect_corners_with` inside
`track_model::learn` plus a schema bump, rather than a parallel telemetry
layer. Where a genuinely missing field is needed (`parent_id`, event list),
it is added to the existing types.

## 12. Known limitations

- **The ~10 m floor is physics, not policy.** Two canonical corners closer
  than the sampling resolution will fuse at the arc level; only a
  decision-event between them can still split them, and only if the driver
  makes one there.
- **A driver too smooth for their own good** leaves no decision trace in a
  complex, and gets one coaching anchor where a textbook says three. The
  problem statement accepts this; so does this design, out loud (§5.2).
- **Very short/kart-scale circuits** have corner spacing near the resolution
  floor and lap times short enough that lap-time MADs are noisy; the
  consensus schedule is derived for the lap counts in scope (≤10) and has not
  been validated below ~60 s laps. Empirical question, flagged as such.
- **The gap-penalty multiple in §4.1** is the one constant here I would watch
  in testing: alignment behaviour under wildly wrong gap costs is well
  understood (over-splitting vs over-merging) and the multiple is bounded by
  the data-derived τ², but its "small fixed" value is a convention I have not
  measured against this repo's captures.
- **Car dependence of boundaries** (the MX5/F138 T7 disagreement) is *reduced*
  — medians over one driver's laps replace one lap's geometry wholesale — but
  not eliminated, because a faster car genuinely moves braking points. The
  `Provenance.car` record stays, and cross-car reconciliation remains
  unattempted, as in the current model.
