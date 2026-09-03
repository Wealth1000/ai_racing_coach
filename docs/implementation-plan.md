# Implementation Plan — AI Sim Racing Coach

Batched build plan from the current repository state to a working MVP: a real-time,
offline race engineer for **Assetto Corsa** on Windows that reads live telemetry,
extracts per-corner features, compares them against the driver's own personal best,
and speaks short corrective advice.

- **Status of this document:** plan of record. Supersedes `ReadMe.md` wherever the two disagree.
- **Audit basis:** every claim in [§2](#2-current-state-audit) was verified against the working
  tree at commit `f3f7713` plus the uncommitted changes present in it, and against a real run
  over `telemetry_cleanAMS2.ndjson` (14,159 lines). Measured numbers are marked *measured*.
- **Scope:** ReadMe Phase 1 + Phase 2. Phase 3's corner detection already partly exists and is
  folded into Phase C below; Phase 4 (ML) is explicitly deferred — see [§7](#7-deferred--post-mvp).

---

## 1. Purpose & how to use this document

### 1.1 The rule

**One batch at a time. Every batch ends with the tree green and something demoable.**

A batch is not merged until its *Acceptance criteria* command runs and produces the stated
output. No batch depends on a later batch. If a batch turns out to be wrong, the damage is
bounded to that batch.

### 1.2 How to read a batch

Every batch below has exactly these seven sections:

| Section | What it is for |
|---|---|
| **Goal** | One sentence. If you can't state it in one sentence, the batch is too big. |
| **Why now** | What this unblocks, and why it can't wait. Justifies the ordering. |
| **Files** | Exact paths, marked *new* / *modified* / *deleted*. |
| **Steps** | Numbered and executable. No "consider" or "maybe". |
| **Key types** | Real Rust. Compiles or nearly compiles. Not pseudocode. |
| **Acceptance criteria** | A command to run, and the output to expect. |
| **Risks & gotchas** | What will bite. Referenced defect IDs from §2.4. |

### 1.3 Defect IDs

[§2.4](#24-defect-catalogue) catalogues 18 concrete defects as **D1**–**D18**. Batches
reference them by ID so you can always trace *why* a step exists. When a batch closes a
defect it says so explicitly.

### 1.4 Conventions

- Paths are repo-relative. `src/feature/` (singular) is the current name;
  `src/features/` (plural) is the post-Batch-1 name, matching the ReadMe.
- `coach` means the built binary. Before Batch 1 that is `cargo run --`; after Batch 1 the
  binary is renamed and subcommands are added.
- **Linux** is the development host. **Windows** is the target and the only place live AC
  telemetry exists. Batches say which host they are verified on.
- Proposed constants (resampling grid, thresholds, cooldowns) are marked **tuning knob**.
  They are starting values, not settled ones.

### 1.5 The one thing you should do in parallel

Batches 5–9 develop against AMS2 data because that is the only real dataset in the repo.
AC-specific fields (`WheelSlip`, `LocalVelocity`, `NumberOfTyresOut`, `IsValidLap`) have no
test coverage until an AC capture exists.

> **Action, in parallel with Batches 0–2:** run the existing `Logger Programs/ACProgram.cs`
> on the Windows machine for 10–15 clean laps of one track, and copy the resulting
> `telemetry_ac.ndjson` to the Linux box. Nothing before Batch 5 is blocked without it.
> Batch 4 replaces this logger with `coach record`, but until Batch 4 lands the C# logger is
> the only way to get AC data.

---

## 2. Current state audit

### 2.1 What exists

1 commit. 755 lines of Rust. Zero tests. Zero docs. No `lib.rs`.

| Path | Lines | What it actually does | State |
|---|---:|---|---|
| `src/main.rs` | 140 | Batch CLI: reads one NDJSON (or `.gz`) replay, dedups frames, groups laps, picks a "master lap" by Fréchet medoid, runs corner detection, prints and dumps to `corners.txt` | Works, but batch-only and O(L²). Selection is now O(L) via `features/line.rs` |
| `src/telemetry/frame.rs` | 150 | `TelemetryFrame` — dual-schema deserializer, AC flat keys primary with AMS2 PascalCase fallback via accessor methods | **Regressed** — see §2.3 |
| `src/telemetry/mod.rs` | 1 | Re-export | OK |
| `src/feature/corner.rs` | 266 | Menger curvature, distance-windowed smoothing, heading-change window, adaptive threshold, hysteresis state machine, close-corner merge | Genuinely decent; 3 real defects |
| `src/feature/frechet.rs` | 50 | Discrete Fréchet distance (full DP) + medoid selection | Correct; unguarded. Superseded by `features/line.rs`; reduced to a test oracle |
| `src/feature/sample.rs` | 131 | Frame dedup by timestamp, `FeatureSample` projection, validity gate, lap grouping. **Does no resampling** despite the module name | Regressed + misnamed |
| `src/feature/mod.rs` | 7 | Re-exports | OK |
| `src/debug_helpers/dump_to_file.rs` | 9 | `writeln!("{:?}")` per item, `.unwrap()` on every I/O call | Throwaway |
| `src/debug_helpers/mod.rs` | 1 | Re-export | OK |
| `Logger Programs/ACProgram.cs` | 388 | Out-of-process AC logger. **Shared memory**, 100 Hz physics + 100 Hz graphics + 1 Hz static. Emits ~230 flat keys | Working; ground truth for AC |
| `Logger Programs/AMS2Program.cs` | 298 | Out-of-process AMS2 logger. Shared memory (`$pcars2$`), 20 Hz. Emits **short** keys | Working; drifted (D18) |

Of the ReadMe's eight planned modules, **two exist** (`telemetry/`, `feature/`). `models/`,
`coaching/`, `audio/`, `ui/`, `storage/` and `core/` do not.

There is **no live telemetry ingestion of any kind** — no socket, no shared-memory reader, no
thread, no channel, anywhere in `src/`. `CornerFeatures`, the type the ReadMe's central
`DrivingModel` trait consumes, does not exist, and neither do any of its seven named features.

Artifacts in the tree worth knowing about:

- `corners.txt` — output of `dump_to_file`, tracked in git. Debug-formatted, not machine-readable.
- `curvature_profile.txt` — 1,502 lines of `(dist, curvature, smoothed)` tuples, untracked,
  written by code that **no longer exists** in `main.rs`. Stale artifact from June.
- `telemetry_cleanAMS2.ndjson` — 26 MB, 14,159 lines, untracked. Interlagos GP, Porsche 911
  RSR GTE, 9 laps. **The only real dataset in the repo.**

### 2.2 The schema triangle

Three mutually incompatible JSON schemas are already in play. This is the root cause of
almost everything in §2.3.

| # | Schema | Produced by | Key style | Example keys | Parseable today? |
|---|---|---|---|---|---|
| 1 | **AssettoCorsaFlat** | `ACProgram.cs` | Flat, source-prefixed | `Physics_SpeedKmh`, `Physics_Gas`, `Physics_Heading`, `Graphics_NormalizedCarPosition`, `StaticInfo_TrackSPlineLength`, `PositionX/Y/Z` | Yes — this is what `frame.rs` targets |
| 2 | **Ams2Pascal** | An **older build** of `AMS2Program.cs` | PascalCase, nested `Viewed`, arrays | `Speed`, `Throttle`, `Steering`, `Orientation[3]`, `AngularVelocity[3]`, `Viewed.WorldPosition[3]` | Partially — position/speed/lap only |
| 3 | **Ams2Short** | `AMS2Program.cs` **as committed** | Short abbreviations | `ts`, `sp`, `t`, `b`, `s`, `ori`, `angv`, `v.wp`, `v.cld` | **No — parses to all zeros, silently** |

The dataset on disk is schema 2. The committed logger emits schema 3. That is **D18**: the
AMS2 logger has already drifted away from its own dataset, and the failure mode is not an
error, it is a file full of plausible zeros.

### 2.3 The live regression — root cause and measured signature

An uncommitted rewrite of `src/telemetry/frame.rs` and `src/feature/sample.rs` changed the
parser from *AMS2-only with hard-typed fields* to *AC-first with `#[serde(default)]` on
everything and an AMS2 fallback*. The fallback was implemented for four accessors and
forgotten for the rest.

**Root cause.** `frame.rs` declares every field `#[serde(default)]` and adds
`#[serde(flatten)] pub extra: Value` as a catch-all (`src/telemetry/frame.rs:1-80`). Between
them, *any* key that a schema does not provide becomes `0.0` and *no error is raised*. Four
accessors — `position()`, `speed_ms()`, `current_lap()`, `lap_distance()` — implement an
explicit AC→AMS2 fallback. `heading`, the angular-velocity triple, `throttle`, `brake`,
`steering` and `rpm` have no accessor at all, so `FeatureSample::from_frame` reads the raw
AC-named fields directly and gets zeros on AMS2 input.

The AMS2 values are *present in the same struct*, declared and deserialized correctly at
`src/telemetry/frame.rs:60-75`, and simply never read.

**Measured signature.** `cargo run --release -- telemetry_cleanAMS2.ndjson`:

| Metric | At `HEAD` | Working tree (now) |
|---|---|---|
| Frames read | 14,159 | 14,159 |
| Valid samples | 12,952 | 13,787 |
| Laps grouped | 9 | 9 |
| **Corners detected** | **10** | **9** |
| **Direction split** | **7 Right / 3 Left** | **9 Right / 0 Left** |
| Longest corner | 254 m | **708 m** (corner 3, 1896→2605 m) |
| `heading_angle` | ±3.14 rad | **identically 0.0** |
| `yaw_rate` | ±1.60 rad/s | **identically 0.0** |
| `throttle` / `brake` / `steering` | real | **identically 0.0** |

The visible damage is *not* zero corners — it is that the heading signal is dead, so:

1. `corner_direction` at `src/feature/corner.rs:223-227` tests
   `heading_angle[apex_heading_idx] > 0.0`, which is now never true, so **every corner is
   classified `Right`**.
2. The `max_heading > heading_threshold` branch of the corner-validity test
   (`src/feature/corner.rs:213-215`) can never fire, so corner acceptance and boundary
   placement now rest on curvature alone, and boundaries bloat — three corners exceed 440 m,
   which at Interlagos means whole sequences have been fused into one "corner".

> **Note on a stale artifact.** `corners.txt` was 0 bytes on disk when this plan's proposal
> was written, which suggested detection had collapsed to nothing. That file was a leftover
> from an interrupted June run, not the current behaviour. The measured behaviour is the table
> above. Batch 2's acceptance criteria are written against the measured values.

### 2.4 Defect catalogue

Referenced by ID throughout §5. **Sev** is impact on the MVP, not on today's output.

| ID | Sev | Defect | Evidence | Closed by |
|---|---|---|---|---|
| **D1** | **Critical** | Silent-zero deserialization: `#[serde(default)]` on every field plus `#[serde(flatten)] extra: Value` converts any schema mismatch into plausible zeros with no error | `src/telemetry/frame.rs:1-80` | Batch 2 |
| **D2** | **Critical** | `heading_angle` dead on AMS2 — reads `Physics_Heading`, absent from schema 2 | `frame.rs:33-38`, `sample.rs:66`; *measured* all-zero | Batch 2 |
| **D3** | **Critical** | `yaw_rate` dead on AMS2 — reads `Physics_LocalAngularVelocity1`, absent from schema 2; AMS2's `AngularVelocity[1]` (*measured* ±1.60 rad/s) sits unused | `frame.rs:41-46`, `sample.rs:61` | Batch 2 |
| **D4** | **Critical** | `throttle`/`brake`/`steering`/`rpm` dead on AMS2 — the four missing accessors. Also silently disables the `rpm < 100.0` term of the validity gate | `frame.rs:19-31` vs `60-71`; `sample.rs:76` | Batch 2 |
| **D5** | High | Restoring the AMS2 arrays is not enough: `sample.rs:66` indexes `[0]`. *Measured:* AMS2 `Orientation[0]` ≈ `-6.2e-05`, `Orientation[1]` ∈ [−3.1404, 3.1415] — **yaw is index 1** | `sample.rs:66`; `HEAD` used `[1]` | Batch 2 |
| **D6** | Medium | `normalised_lap_distance` hardcoded `0.0`; `HEAD` computed `CurrentLapDistance / TrackLength`. Unused today, required by Batch 5 | `sample.rs:64` | Batch 2 |
| **D7** | High | Validity gate weakened and sim-wrong. Lost `PitMode == 0`, `CrashState == 0`, `CurrentLapDistance > 0.0`. *Measured:* 835 extra frames admitted, incl. 76 `PitMode == 4` pit frames. For AC the `GameState` key **does not exist** (AC uses `Graphics_Status`), so that gate never fires at all | `sample.rs:70-91` | Batch 2 |
| **D8** | High | `smooth_curvature` averages **signed** curvature over a 20 m window; `detect_corners` then takes `.abs()`. A chicane's two directions cancel inside one window and the corner can vanish | `corner.rs:54-82`, `corner.rs:177` | Batch 6 |
| **D9** | Low | Doc comment says "30% of the 95th percentile"; code multiplies by `0.15` | `corner.rs:115` vs `corner.rs:122` | Batch 6 |
| **D10** | High | Unbounded index arithmetic: `1..lap_data.len() - 1` underflows at `len == 0`; `dp[0][0]` panics on an empty path; `/ (all_laps.len() - 1)` underflows at ≤1 lap; three `partial_cmp().unwrap()` sites panic on `NaN` | `corner.rs:27`, `frechet.rs:11`, `frechet.rs:44`, `corner.rs:119`, `main.rs:92` | Batch 2 |
| **D11** | High | O(L²) master-lap selection needs every lap in memory up front, and calls `transform_to_world_position()` **inside the inner loop** — two ~1,500-element `Vec` allocations per pair, plus a ~9 MB DP table per pair. Incompatible with streaming | `main.rs:110-124` (allocs at `114-115`) | Batch 6 |
| **D12** | High | No resampling, despite `sample.rs`. Dedup is timestamp-only. AMS2 logs at 20 Hz wall-clock (`AMS2Program.cs:231`), so metre-spacing varies ~4× across the *measured* 0.01–73.59 m/s speed range, while curvature and Fréchet both assume comparable arc-length spacing | `sample.rs:17-19` | Batch 5 |
| **D13** | High | **Nothing in the crate derives `Serialize`.** `TrackCorner` is `#[derive(Debug)]` only — not even `Clone`. Persistence is impossible | `corner.rs:10-19` | Batch 6 |
| **D14** | Medium | Only output sink is `{:?}` Debug formatting, with `.unwrap()` on every I/O call. Not machine-readable, panics on a full disk | `debug_helpers/dump_to_file.rs` | Batch 14 |
| **D15** | Medium | Dependencies don't match intent: bare `egui` 0.33.3 with **no `eframe`/`winit`/`wgpu` in `Cargo.lock`** (cannot open a window); `anyhow` declared and unused; no `tts`, no `tracing` | `Cargo.toml`, `Cargo.lock` | Batches 1, 13, 15 |
| **D16** | **Critical** | 26 MB untracked dataset with `.gitignore` containing only `/target`. One `git add -A` puts it in history permanently | `.gitignore` | Batch 0 |
| **D17** | Low | `ReadMe.md:1` opens a ```` ```markdown ```` fence that is never closed — the entire README renders as one code block | `ReadMe.md:1` | Batch 0 |
| **D18** | High | `AMS2Program.cs` emits schema 3 (short keys) via `[JsonPropertyName]`; the committed dataset is schema 2. Re-running the committed logger yields a file the Rust parser reads as all zeros, with no error. **D1 demonstrated in the wild** | `AMS2Program.cs:42-125` vs dataset | Batch 2 |

---

## 3. Target architecture

### 3.1 The central decision: streaming from Batch 3 onward

**The pipeline is a streaming state machine. One `Sample` in, zero-or-more events out. No
stage holds a lap-sized buffer.**

Everything else follows from this. It buys three things:

1. **Replay and live share one code path.** Batch mode is a `NdjsonReplaySource` feeding the
   same `CoachPipeline` that the shared-memory source feeds. "Add real-time later" never
   becomes a rewrite of every stage.
2. **The whole intelligence layer is developable on Linux** from NDJSON on disk, with no sim
   running. Given that AC can only be driven on the Windows machine, this is what makes the
   project developable at all.
3. **Bounded memory and latency.** At 100 Hz, a lap is ~9,000 samples. Stages that buffer laps
   cannot meet a "speak before the next corner" deadline.

The corollary: `main.rs`'s current all-pairs Fréchet master-lap selection (**D11**) cannot
survive as a live-path step. It becomes an **offline track-learning command** (Batch 6) that
writes a `TrackModel` to disk; the live path loads that model in O(1).

**Resolved, Batch 6 — and the metric changed with it.** Fréchet was the wrong tool, not
merely a slow one. It searches every monotone point pairing because it assumes the
correspondence between two curves is unknown; after `resample` anchors every lap's grid at
absolute distance 0, the correspondence *is* known. Measured on all six pairs of clean laps
in the two reference captures, the optimal Fréchet coupling equalled the identity coupling
to the last centimetre on every pair — the O(L²) search was redundant, not wrong. Selection
is now `features::line`: a single merge join on grid index, O(L) per pair, ~130 µs against
~265 ms. Three consequences worth recording. The `frechet_stride: 5` downsampling knob is
gone, and it was never free (strided Fréchet reported 6.19 m where the true separation was
5.83 m, a 6% error). Ranking moved from a mean of maxima — a minimax centre wearing the name
"medoid" — to a mean of means, which is what a medoid actually is. And the comparison now
reports *where* the two laps diverged most, which the DP discards.

### 3.2 Pipeline

```text
                         ┌─────────── offline, run once per track ───────────┐
                         │  coach learn-track <ndjson>                       │
                         │    resample → lap split → line medoid →           │
                         │    curvature → corner detect → TrackModel JSON    │
                         └───────────────────────┬───────────────────────────┘
                                                 │ data/tracks/<track>.json
                                                 ▼
  ┌──────────────┐   TelemetryFrame   ┌──────────────────────────────────────┐
  │ TelemetrySource ├─────────────────►│            CoachPipeline             │
  │              │   (bounded chan)   │                                      │
  │ • AcSharedMem │                   │  Normalise    → Sample               │
  │   (windows)   │                   │  Resample     → Sample @ 1 m grid    │
  │ • NdjsonReplay│                   │  LapTracker   → LapBoundary          │
  │ • (UDP, later)│                   │  CornerTracker (uses TrackModel)     │
  └──────────────┘                    │  FeatureBuilder → CornerFeatures     │
                                      │  ReferenceStore → CornerReference    │
                                      │  DrivingModel   → Vec<DrivingIssue>  │
                                      │  AdviceMapper   → Advice             │
                                      │  DecisionEngine → Advice (throttled) │
                                      └───────────────┬──────────────────────┘
                                                      │ bounded chan, drop-oldest
                            ┌─────────────────────────┼─────────────────────────┐
                            ▼                         ▼                         ▼
                     ┌────────────┐            ┌────────────┐           ┌────────────┐
                     │  TtsSink   │            │   ui/ app  │           │  storage/  │
                     │ OS speech  │            │   eframe   │           │  NDJSON    │
                     └────────────┘            └────────────┘           └────────────┘
```

### 3.3 Module layout

Matches the ReadMe's planned structure, plus `runtime/` for threading and process wiring
(the ReadMe implies it but does not name it).

```text
src/
 ├── lib.rs                  # crate root — everything reachable from tests
 ├── main.rs                 # thin: CLI parse + dispatch only
 ├── core/                   # shared types, no dependencies on other modules
 │    ├── sample.rs          #   Sample — the canonical, sim-agnostic per-tick record
 │    ├── ids.rs             #   LapId, TrackId, CornerId, SessionId
 │    ├── error.rs           #   CoachError (thiserror)
 │    └── config.rs          #   CoachConfig, InputDevice
 ├── telemetry/              # getting frames in
 │    ├── frame.rs           #   TelemetryFrame (as-logged, per schema)
 │    ├── schema.rs          #   Schema enum + detection + loud failure   [Batch 2]
 │    ├── source.rs          #   trait TelemetrySource                    [Batch 3]
 │    ├── replay.rs          #   NdjsonReplaySource                       [Batch 3]
 │    ├── ac_shared_memory.rs#   #[cfg(windows)] SM reader                [Batch 16]
 │    └── record.rs          #   coach record → NDJSON                    [Batch 16]
 ├── features/               # raw → structured  (renamed from feature/)
 │    ├── resample.rs        #   arc-length resampling                    [Batch 5]
 │    ├── lap.rs             #   lap boundary detection                   [Batch 5]
 │    ├── curvature.rs       #   Menger curvature + smoothing             [Batch 6]
 │    ├── corner.rs          #   corner detection state machine
 │    ├── line.rs            #   equal-distance separation, medoid lap    [Batch 6]
 │    ├── frechet.rs         #   discrete Fréchet — test oracle only
 │    ├── track_model.rs     #   TrackModel, load/save                    [Batch 6]
 │    ├── corner_features.rs #   CornerFeatures extraction — the keystone  [Batch 7]
 │    └── reference.rs       #   per-corner personal best                  [Batch 8]
 ├── models/                 # what went wrong
 │    ├── mod.rs             #   trait DrivingModel                       [Batch 9]
 │    ├── issue.rs           #   DrivingIssue, Severity                   [Batch 9]
 │    └── rules.rs           #   RuleModel — two tiers                    [Batch 9]
 ├── coaching/               # how to say it
 │    ├── advice.rs          #   Advice                                   [Batch 10]
 │    ├── phrasing.rs        #   issue → words, controller-aware          [Batch 10]
 │    └── decision.rs        #   don't-distract-the-driver logic          [Batch 11]
 ├── audio/
 │    └── sink.rs            #   trait FeedbackSink, TtsSink, NullSink    [Batch 13]
 ├── ui/
 │    └── app.rs             #   eframe app                               [Batch 15]
 ├── storage/
 │    ├── session.rs         #   session NDJSON                           [Batch 14]
 │    └── dataset.rs         #   flat feature-vector export               [Batch 14]
 └── runtime/
      ├── pipeline.rs        #   CoachPipeline: stage composition          [Batch 12]
      └── threads.rs         #   thread + channel wiring                   [Batch 12]

data/
 ├── tracks/                 # <track>_<layout>.json, <track>_<layout>_pb.json
 └── sessions/               # <session-id>.ndjson
tools/
 └── loggers/                # the two C# loggers (moved in Batch 0)
tests/
 └── fixtures/               # committed golden NDJSON, one per schema
```

### 3.4 The four traits

These are the seams. Everything else is an implementation detail behind one of them.

```rust
// telemetry/source.rs — where frames come from.               [Batch 3]
pub trait TelemetrySource {
    /// `Ok(None)` = end of stream (replay EOF). `Err` = a real failure.
    fn next_frame(&mut self) -> Result<Option<TelemetryFrame>, CoachError>;
    fn schema(&self) -> Schema;
    /// Human-readable, for the UI's connection indicator.
    fn describe(&self) -> String;
}

// runtime/pipeline.rs — the streaming contract.               [Batch 3]
pub trait Stage {
    type Out;
    /// One sample in, zero-or-more events out. Must not buffer a lap.
    fn on_sample(&mut self, s: &Sample) -> Vec<Self::Out>;
    /// Called when the lap tracker sees a wrap. Default: nothing.
    fn on_lap_boundary(&mut self, _lap: LapId) -> Vec<Self::Out> { Vec::new() }
}

// models/mod.rs — what went wrong. The ReadMe's central abstraction. [Batch 9]
pub trait DrivingModel {
    /// `reference` is `None` until a personal best exists for this corner.
    fn predict(
        &self,
        features: &CornerFeatures,
        reference: Option<&CornerReference>,
    ) -> Vec<DrivingIssue>;
    fn name(&self) -> &'static str;
}

// audio/sink.rs — how advice reaches the driver.              [Batch 13]
pub trait FeedbackSink {
    fn deliver(&mut self, advice: &Advice) -> Result<(), CoachError>;
    fn flush(&mut self) {}
}
```

Note the deliberate departure from the ReadMe's sketch
(`fn predict(features: &CornerFeatures) -> DrivingIssue`): `predict` takes `&self` so a model
can hold state (loaded weights, thresholds), takes the reference explicitly so the
personal-best comparison is not smuggled in via globals, and returns `Vec<DrivingIssue>`
because one corner routinely has more than one problem.

### 3.5 Threading model (Batch 11)

```text
 telemetry thread          pipeline thread              consumer threads
 ────────────────          ───────────────              ────────────────
 loop {                    loop {                       TtsSink    (owns speech synth)
   src.next_frame()  ──►     rx.recv()                  ui/app     (eframe, own thread)
   tx.try_send()             stages.on_sample()   ──►   storage    (buffered writer)
 }                           advice_tx.try_send()
                           }
     bounded(256)                 bounded(64)
     drop-oldest on full          drop-oldest on full
```

Both channels are **bounded with explicit drop-oldest**. A stalled UI or a blocking audio
device must never back-pressure the telemetry reader — at 100 Hz, back-pressure means dropped
physics frames and corrupted features. Dropped events are counted and surfaced in the UI.

---

## 4. Locked decisions

Taken with the user. These narrow the ReadMe. **Do not relitigate mid-build.**

| # | Question | Decision | Rationale |
|---|---|---|---|
| 1 | Target sim for MVP | **Assetto Corsa (2014)** — *not* AMS2 | AMS2 "cannot be used right now". The ReadMe names AMS2 as the initial target; this reverses that. |
| 2 | Dev / ship split | Develop on **Linux**, cross-compile a **Windows** binary, test there | Where the user works vs where the sim runs. Forces §3.1's streaming design. |
| 3 | MVP scope | ReadMe **Phase 1 + Phase 2** | Live telemetry, features, rules, real-time feedback, audio, basic GUI, session logging. |
| 4 | Voice | **OS text-to-speech** via the `tts` crate (SAPI on Windows, speech-dispatcher on Linux) | **Reversed 2026-09-03 by the user** — was pre-rendered WAV + `rodio`. The phrased sentences embed measured numbers ("brake 12 metres earlier", "you lost 0.18 of a second"), which a fixed WAV inventory cannot speak without clip-composition machinery; TTS says any string, so no phrase bank and no inventory to keep in sync. Still fully offline: the synthesiser is OS-local, no network, no runtime dependency on a WAV tree. |
| 5 | Reference to coach against | **The driver's own best lap**, per-corner personal best | No reference-lap dataset exists and none can be acquired offline. Also sidesteps car/tyre/setup normalisation entirely. |
| 6 | AC transport | **Shared memory**, not UDP | See §4.1. |
| 7 | AMS2 parsing | **Kept, not removed** | It is the only regression corpus in the repo. AMS2 is deferred as a *live* target only. |
| 8 | Storage format | **NDJSON** for MVP; SQLite deferred | Matches the ReadMe's staging, round-trips through `serde`, greppable. |

### 4.1 Why AC shared memory, not UDP

This plan originally assumed AC's UDP protocol. `Logger Programs/ACProgram.cs` is working
code **in this repo** and is ground truth, so it overrides that assumption:

- It uses the **shared memory** API (`AssettoCorsaSharedMemory`), not UDP — `ACProgram.cs:27-34`.
- `PhysicsInterval = 10` → **100 Hz**, well above the 60 Hz the ReadMe assumes.
- It exposes everything the coaching rules need, and the UDP `RTCarInfo` packet does not:

| Field | `ACProgram.cs` | Needed for |
|---|---|---|
| `Physics_WheelSlip0..3` | `:122-125` | Wheelspin, lockup |
| `Physics_WheelAngularSpeed0..3` | `:134-137` | Lockup (wheel speed vs road speed) |
| `Physics_LocalVelocity0..2` | `:214-216` | Slip angle → **understeer vs oversteer** |
| `Physics_LocalAngularVelocity0..2` | `:182-184` | Yaw rate |
| `Physics_Heading` | `:160` | Corner direction — the field whose loss caused **D2** |
| `Physics_NumberOfTyresOut` | `:169` | Off-track / clean-lap gating |
| `Graphics_IsValidLap` | `:291` | Clean-lap gating |
| `Graphics_SurfaceGrip` | `:263` | Grip context for rule thresholds |
| `StaticInfo_Track`, `_TrackConfiguration`, `_TrackSPlineLength` | `:324, :359, :358` | Track identity + length for the track model |

Slip angle vs yaw rate separating understeer from oversteer is the single diagnosis drivers
most want, and it is the reason shared memory is worth choosing over UDP.

This also consolidates the toolchain — the Rust binary gains a `record` mode emitting the same
NDJSON schema, and **replaces the C# loggers entirely** (Batch 4):

```text
Windows:  coach record   → telemetry_ac.ndjson   (100 Hz, shared memory)
   ↓ copy
Linux:    coach replay   → develop the whole intelligence layer offline
Windows:  coach live     → real-time coaching
```

UDP is demoted to an optional later source for the sim-on-another-machine case. Its layout is
unverified (Appendix B) and it must not gate the MVP.

---

## 5. Remaining batches

**Status note, 2026-09-03.** The offline intelligence stack is done and green: schema and
replay (Batches 1–5), the three-stage corner learner — θ(s) MDL segmentation, ring-alignment
consensus with the Wilson bound, decision-event clustering — replacing the Batch-6 hysteresis
detector (`src/features/{segment,consensus,decision,track_model}.rs`, `TrackModel` v2), and
the full model/coaching chain (Batches 7–11: features, references, rules, advice, phrasing,
the decision engine). 172 tests.

What the earlier batches deliberately deferred — and what is *not* in the tree — is the live
path: `runtime/pipeline.rs`, `runtime/threads.rs`, `core/config.rs`, `audio/`, `storage/`,
`ui/`, and the Windows shared-memory source. Those items were originally tagged
[Batch 3/4/11] but only their trait seams landed (`TelemetrySource`, `NdjsonReplaySource`,
`ThrottlingEngine`). The batches below are the remaining work, renumbered in dependency
order; their numbering supersedes any earlier tags still pointing at 3/4/11 for these files.
Ordering principle: everything through Batch 15 is developed and verified **on Linux against
a replay source**; only Batch 16 needs the Windows box, because only live AC shared memory
exists there.

### Batch 12 — the live pipeline, replay-fed

| Section | |
|---|---|
| **Goal** | Stream a capture through the whole intelligence stack one sample at a time and emit throttled `Advice` on a channel, with no lap buffered whole. |
| **Why now** | Every remaining consumer — voice, UI, storage, and eventually the live Windows source — subscribes to that channel. Nothing downstream can be built or demoed before it exists, and it is the last piece that can be verified purely on Linux replays. |
| **Files** | *new* `src/runtime/mod.rs`, `src/runtime/pipeline.rs`, `src/runtime/threads.rs`, `src/core/config.rs`; *modified* `src/lib.rs`, `src/main.rs` (`coach live --replay <capture>`), `Cargo.toml` (+`crossbeam-channel`) |
| **Steps** | 1. `core/config.rs`: `CoachConfig { input: InputDevice, step_m, models_dir }` — the one struct the live path reads instead of scattered flags. 2. `pipeline.rs`: streaming resampler (incremental arc-length interpolation producing the *same* grid as `features/resample.rs` — assert equality in a test against the offline resampler on a real capture lap); streaming lap tracker (reuse `features/lap.rs`'s wrap rule); corner tracker over the frozen `TrackModel` (enter/exit on boundary crossing, ring-aware); per-corner `CornerFeatures` from the samples since entry; `ReferenceStore` + `RuleModel` + `AdviceMapper` + `ThrottlingEngine` wired exactly as `coach analyse` does offline. 3. `threads.rs`: the §3.5 wiring — source thread → `bounded(256)` drop-oldest → pipeline thread → `bounded(64)` drop-oldest → consumer; dropped counts in atomics, surfaced by every consumer. 4. `coach live --replay <capture> [--voice null]`: runs the wiring headless, prints each advice line with lap/corner, and a summary line `N advice, M frames dropped`. |
| **Key types** | `pub trait Stage { type Out; fn on_sample(&mut self, s: &Sample) -> Vec<Self::Out>; fn on_lap_boundary(&mut self, _lap: LapId) -> Vec<Self::Out> { Vec::new() } }` · `pub struct CoachPipeline { /* resampler, lap tracker, corner tracker, model, engine */ }` with `fn new(model: TrackModel, reference: ReferenceStore, config: CoachConfig) -> Self`, `fn on_sample(&mut self, s: &Sample) -> Vec<Advice>`, `fn on_lap_boundary(&mut self, lap: LapId) -> Vec<Advice>`; `pub struct LiveWiring { pub advice_rx: Receiver<Advice>, pub dropped_frames: Arc<AtomicU64>, pub dropped_advice: Arc<AtomicU64> }`, `pub fn spawn(source: Box<dyn TelemetrySource + Send>, pipeline: CoachPipeline) -> LiveWiring` |
| **Acceptance criteria** | On Linux: `coach live --replay ndjson_data/telemetry_ac_monza_*.ndjson.gz` ends with `… advice, 0 frames dropped`, and the advice set it prints for the fastest lap is the same set `coach analyse` reports for that lap (same corners, same kinds; phrasing identical). A unit test asserts streaming-resampler output equals `features::resample` output on a Monza lap, sample for sample. |
| **Risks & gotchas** | Streaming resample drift vs the offline resampler (make the golden test the first thing, not the last). Back-pressure must never reach the source thread (§3.5; **D11**'s lesson). Corner windows straddling the lap wrap: the tracker must keep the window across the boundary — the TrackModel already stores line-straddling corners as two rows sharing a `parent_id`; the live tracker must treat them as one corner. The `Stage` trait's "must not buffer a lap" rule bends for exactly one thing: per-corner features need the window from entry to exit, which is bounded by a corner, not a lap — document that bound in the trait. |

### Batch 13 — voice: the OS TTS sink

| Section | |
|---|---|
| **Goal** | The driver hears the phrased advice, spoken by the operating system's synthesiser. |
| **Why now** | Locked decision 4 (reversed 2026-09-03): TTS, not pre-rendered WAV. `Advice.phrased` is already a complete spoken sentence — "brake 12 metres earlier — your best lap braked here" — with the measured numbers in it; TTS speaks any such string, so the phrase-bank layer (inventory, composition, Appendix D) simply does not exist anymore. |
| **Files** | *new* `src/audio/mod.rs`, `src/audio/sink.rs`; *modified* `Cargo.toml` (+`tts`), `src/lib.rs`, `src/main.rs` (`coach live --voice {tts,null}`) |
| **Steps** | 1. `audio/sink.rs`: the §3.4 trait verbatim — `FeedbackSink { deliver(&mut self, &Advice) -> Result<(), CoachError>; flush(&mut self) {} }`. 2. `NullSink` (records what it was handed; the test and CI sink). 3. `TtsSink`: wraps `tts::Tts`; `deliver` is *never* allowed to block or queue — if the synthesiser is still speaking the previous line, the new one is counted as skipped and dropped, because coaching advice is perishable (a braking tip delivered three corners late is worse than silence). Skipped counts are surfaced next to the channel drop counts. 4. `coach live --voice tts` on the consumer thread. |
| **Key types** | `pub struct TtsSink { synth: tts::Tts, spoken: u64, skipped: u64 }` · `pub struct NullSink { pub delivered: Vec<Advice> }` · `impl FeedbackSink for TtsSink { fn deliver(&mut self, a: &Advice) -> Result<(), CoachError> { … } }` |
| **Acceptance criteria** | `cargo test` includes: a `NullSink` receives exactly the advice the decision engine emits for a fixture lap (no re-ordering, no loss); a `TtsSink` constructed with no speech backend available reports every deliver as skipped and returns `Ok(())` — voice failure degrades to silence, never to an error path that could stall the pipeline. On a desktop Linux box with speech-dispatcher running, `coach live --replay <capture> --voice tts` audibly speaks the same lines it prints. |
| **Risks & gotchas** | `tts` needs speech-dispatcher (Linux) / SAPI (Windows); absence must degrade, not fail (**D14**'s spirit: a sink that panics on a missing audio daemon is the same bug class). Block-in-speak: some backends synthesise synchronously — measure, and if any backend blocks, move the `tts::Tts` call behind a one-slot channel to a dedicated speaker thread. Voice selection and rate are **tuning knobs** persisted in `CoachConfig`, not flags re-parsed per run. |

### Batch 14 — session logging and dataset export

| Section | |
|---|---|
| **Goal** | Every live session is written to disk as replayable NDJSON, and the accumulated sessions export as flat feature vectors for offline analysis. |
| **Why now** | Closes **D14** (the last `{:?}`/`unwrap` output path). It also feeds the loop the whole project runs on: sessions become the corpus the models and the personal-best store learn from, without anyone re-running captures by hand. |
| **Files** | *new* `src/storage/mod.rs`, `src/storage/session.rs`, `src/storage/dataset.rs`; *modified* `src/lib.rs`, `src/main.rs` (`coach live --record-session <dir>`, `coach export-dataset <dir> <out.csv>`) |
| **Steps** | 1. `session.rs`: a `SessionWriter` emitting NDJSON — one header record (sim, track, car, model fingerprint, config), then one record per lap boundary (lap id, time, clean flag) and one per delivered advice (corner, kind, severity, phrased, deltas, skipped/dropped counters at that moment). Every I/O error is a `CoachError`, never an `unwrap` (**D14**). 2. The writer is itself a `FeedbackSink` consumer plus a lap-boundary observer on the pipeline. 3. `dataset.rs`: read a directory of sessions, join each corner pass with the `TrackModel` corner and any personal best, emit one CSV row per corner pass (the column list = `CornerFeatures` + reference deltas + outcome flags). 4. Both commands verified against the same Monza replay used in Batch 12. |
| **Key types** | `pub struct SessionWriter { out: BufWriter<File>, events: u64 }` with `fn create(dir: &Path, id: SessionId) -> Result<Self, CoachError>`, `fn write_header(&mut self, h: &SessionHeader) -> Result<(), CoachError>`, `fn write_event(&mut self, e: &SessionEvent) -> Result<(), CoachError>`; `pub fn export_dataset(sessions: &[PathBuf], model: &TrackModel, out: &Path) -> Result<u64, CoachError>` |
| **Acceptance criteria** | `coach live --replay <capture> --record-session data/sessions/` writes `data/sessions/<session-id>.ndjson`; `coach inspect` on that file parses it (it is the same schema family the replay source reads — header + records, no `{:?}` output anywhere). `coach export-dataset data/sessions out.csv` prints `W rows, C columns` and the CSV opens with one row per corner pass. Disk-full and permission-denied produce a clean `CoachError` message naming the path — verified by a test writing to a read-only directory. |
| **Risks & gotchas** | Writing must be buffered and on the consumer thread; a sync-per-event writer at 100 Hz would stall the storage consumer and inflate the drop counters (§3.5 again). Schema drift: the session record derives `Serialize` from the same types the rest of the crate uses, so a field added to `Advice` lands in the session log without a second schema to maintain (**D13**'s lesson). Partial sessions (crash mid-recording) must still parse up to the last complete line. |

### Batch 15 — the GUI

| Section | |
|---|---|
| **Goal** | A window showing connection state, the corner the car is in, and a colour-coded feed of spoken advice with the drop/skip counters. |
| **Why now** | Last Linux-verifiable piece. It is the demo surface for the whole MVP and the read-only window into the pipeline while driving (Batch 16 verification will want it on screen). |
| **Files** | *new* `src/ui/mod.rs`, `src/ui/app.rs`; *modified* `Cargo.toml` (+`eframe`, closing the **D15** `egui`-without-`eframe` gap), `src/lib.rs`, `src/main.rs` (`coach gui [--replay <capture>]`) |
| **Steps** | 1. `app.rs`: an `eframe::App` holding a `VecDeque<Advice>` (capped, e.g. last 50), the atomic drop/skip counters from `LiveWiring`, and a connection indicator fed by `TelemetrySource::describe()`. 2. UI thread receives from `advice_rx` on each repaint request (`ctx.request_repaint_after`); it never owns or blocks the pipeline. 3. Rows: severity colour (`Info`/`Warn`/`Critical`), corner id + direction, phrased sentence, the numeric deltas as a tooltip. 4. `coach gui --replay` drives the same wiring as `coach live` with the UI as the sole consumer. |
| **Key types** | `pub struct CoachApp { advice: VecDeque<Advice>, dropped: Arc<AtomicU64>, skipped: Arc<AtomicU64>, source_desc: String }` · `impl eframe::App for CoachApp { fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) }` |
| **Acceptance criteria** | On Linux: `coach gui --replay ndjson_data/telemetry_ac_monza_*.ndjson.gz` opens a window; as the replay streams, advice rows appear in order with the correct severity colours; the drop counters read 0; closing the window exits the process cleanly (no leaked pipeline threads — checked with a test that drops `LiveWiring` and joins). A screenshot replaces the acceptance text once captured. |
| **Risks & gotchas** | GUI frameworks and headless CI do not mix: the acceptance command is interactive, so the CI-visible part is thread-shutdown correctness plus a render-free unit test of the row model. Repaint cadence must be driven by `request_repaint_after`, not a busy loop — a 100 Hz repaint loop would starve the pipeline thread on weak hardware. `eframe` pulls `wgpu`; expect the first cold build to be slow — that's fine, it's once. |

### Batch 16 — Windows: shared memory, record, live

| Section | |
|---|---|
| **Goal** | The coach reads AC's shared memory directly on Windows: `coach record` writes captures, `coach live` coaches during driving. |
| **Why now** | Everything before this is Linux/replay-verified by construction; this batch is the first and only one that *must* be verified on the Windows box, so it lands last when everything it depends on is known good. |
| **Files** | *new* `src/telemetry/ac_shared_memory.rs` (`#[cfg(windows)]`), `src/telemetry/record.rs`; *modified* `src/telemetry/mod.rs`, `src/main.rs` (`coach record`, `coach live` without `--replay`), `.github`/build notes for cross-compilation |
| **Steps** | 1. `ac_shared_memory.rs`: poll AC's shared-memory blocks at 100 Hz per the `ACProgram.cs` field map in §4.1 (`Physics_*` at 10 ms, `Graphics_*`/`StaticInfo_*` on change), mapping straight into the existing `TelemetryFrame` — the schema the C# logger already writes, so every downstream stage is unchanged. 2. `record.rs`: `coach record [--laps N] <out.ndjson>` — same NDJSON bytes the C# logger produces, gz on `.gz`. 3. `coach live` (no `--replay`) builds the pipeline on `AcSharedMemSource`, with TTS on and the GUI optional. 4. Cross-compile (`x86_64-pc-windows-gnu` or `-msvc`), copy over, verify on track. |
| **Key types** | `#[cfg(windows)] pub struct AcSharedMemSource { /* mapped physics/graphics/static views */ }` · `#[cfg(windows)] impl TelemetrySource for AcSharedMemSource { fn next_frame(&mut self) -> Result<Option<TelemetryFrame>, CoachError>; … }` · `pub fn record(source: Box<dyn TelemetrySource>, out: &Path, laps: Option<u32>) -> Result<SessionId, CoachError>` |
| **Acceptance criteria** | On the Windows box: `coach record --laps 5 telemetry_ac_new.ndjson`, then `coach inspect telemetry_ac_new.ndjson` on the Linux box parses it with the schema detected and zero all-zero-field warnings (the **D18** regression check). `coach live` speaks advice at the right corners during a real session, with the GUI showing 0 dropped frames over a full tank. The recording byte-compares on schema detection with the C# logger's output for the same session fields. |
| **Risks & gotchas** | AC's SM layout is fixed per game version — pin the struct offsets to one AC version and fail loudly on mismatch (the loud-failure rule from **D1**; never the silent zeros of **D18**). Poll faster than the physics tick wastes CPU, slower aliases the 100 Hz signal; 100 Hz matches the logger. `--laps N` termination needs the graphics lap counter, which is only meaningful with `IsValidLap` context (**D7**). Windows Defender occasionally blocks raw shared-memory reads — whitelist in the README, don't code around it. |
