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
| **D14** | Medium | Only output sink is `{:?}` Debug formatting, with `.unwrap()` on every I/O call. Not machine-readable, panics on a full disk | `debug_helpers/dump_to_file.rs` | Batch 13 |
| **D15** | Medium | Dependencies don't match intent: bare `egui` 0.33.3 with **no `eframe`/`winit`/`wgpu` in `Cargo.lock`** (cannot open a window); `anyhow` declared and unused; no `rodio`/`cpal`, no `tracing` | `Cargo.toml`, `Cargo.lock` | Batches 1, 12, 14 |
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
                     │  AudioSink │            │   ui/ app  │           │  storage/  │
                     │ WAV + rodio│            │   eframe   │           │  NDJSON    │
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
 │    ├── ac_shared_memory.rs#   #[cfg(windows)] SM reader                [Batch 4]
 │    └── record.rs          #   coach record → NDJSON                    [Batch 4]
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
 │    ├── sink.rs            #   trait FeedbackSink, AudioSink            [Batch 12]
 │    └── phrase_bank.rs     #   WAV inventory + composition              [Batch 12]
 ├── ui/
 │    └── app.rs             #   eframe app                               [Batch 14]
 ├── storage/
 │    ├── session.rs         #   session NDJSON                           [Batch 13]
 │    └── dataset.rs         #   flat feature-vector export               [Batch 13]
 └── runtime/
      ├── pipeline.rs        #   CoachPipeline: stage composition          [Batch 3]
      └── threads.rs         #   thread + channel wiring                   [Batch 11]

data/
 ├── tracks/                 # <track>_<layout>.json, <track>_<layout>_pb.json
 └── sessions/               # <session-id>.ndjson
assets/
 └── voice/                  # pre-rendered WAV phrase bank
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

// audio/sink.rs — how advice reaches the driver.              [Batch 12]
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
 loop {                    loop {                       AudioSink  (owns rodio stream)
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
| 4 | Voice | **Pre-rendered WAV clips** + `rodio` | No runtime TTS, no OS speech dependency, no network. Fully offline per the ReadMe. Cost: a fixed phrase inventory (Appendix D). |
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

<!-- CHUNK-BOUNDARY -->
