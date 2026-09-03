# Batch 16 handoff — Windows live telemetry

Written 2026-09-03, at the moment the owner left to test the Windows build
in-game. Everything below is the state a new session needs to pick up.

## Where things stand

Batch 16 (shared-memory reader + live attach + GUI flow) is **code-complete,
committed, and pushed**. All work through the `MEMORY_MAPPED_VIEW_ADDRESS`
fix is in:

- `43cb3cc` MVP state has been reached. — Batch 16 body (shared_memory.rs,
  record.rs, launcher.rs, `say()` on the sinks, optional `--replay`,
  ReadMe/Help updates)
- `69f0601` Fix memory map exposure — the windows-sys 0.59 fix (see below)

Local verification state at commit time:

- `cargo test`: **247 passed, 0 failed** (218 before Batch 16).
- `cargo clippy --lib --tests --bins`: 17 lib warnings, **all pre-existing**
  (coaching/, features/, models/, sims/assetto_corsa/schema.rs,
  storage/session.rs). Nothing in new code.
- Linux GUI smoke: `gui --replay <capture>`, `gui` (picker), `gui --sim ac`
  (waiting → failed screen on this build) all open and run without errors.
- `coach live --voice null` on Linux refuses cleanly: "live telemetry from
  Assetto Corsa is not supported in this build" (exit 1).

## What is happening right now

The owner is on Windows testing the real thing, in this order:

1. **The workflow** — `.github/workflows/release.yml` builds and runs
   `cargo test --locked` on `windows-latest`; the previous run caught the
   `MapViewOfFile` type error (fixed in `69f0601`). The re-run after that
   commit is the first full compile of the `cfg(windows)` code.
2. **In-game**: `coach record --laps 2` first (cheapest end-to-end check —
   the capture it writes must be indistinguishable from a C# logger one),
   then `coach gui` → pick Assetto Corsa → waiting screen → drive out and
   listen for "Assetto Corsa stream picked up" (printed and spoken) → TTS
   coaching, then `coach live` with no `--replay`.

If the workflow or the game surfaces another Windows-only compile/runtime
error, it will be in one of two places:

- `src/sims/assetto_corsa/shared_memory.rs`, `mod windows` (mapping layer —
  the only `cfg(windows)` code in the crate; everything below it is
  cross-platform and locally tested)
- provider `#[cfg(windows)]` impls in `src/sims/assetto_corsa/mod.rs`
  (`live()` / `record()`, both one-liners over the shared module)

Fix pattern for the mapping layer: windows-sys 0.59 wraps view addresses in
`MEMORY_MAPPED_VIEW_ADDRESS { pub Value: *mut c_void }` (it is `Copy`).
`MappedView` stores the wrapper; `.Value` feeds `VirtualQuery` and
`ptr::read`; the wrapper itself feeds `UnmapViewOfFile`. **No Windows target
is installed on the Linux machine and none may be installed** (owner's rule:
never install packages/toolchains on it) — the workflow is the cross-check.

## What Batch 16 added (file map)

| File | What it is |
|---|---|
| `src/sims/assetto_corsa/shared_memory.rs` | Page structs (580/296/688 B, `#[repr(C)]`, no explicit pads — repr(C) inserts the ones C# `Pack=4` does), `FrameAssembler` (position hold + packet-id dedupe, shared by live + record), `PageStore` trait (makes everything testable via `mod fake`'s thread-local scripted store), `SharedMemorySource<R>` (waiting state, `StepOutcome {Sample, Nothing, Waiting, Fatal}` separates retry from plausibility-guard failure), `mod windows` (AcPages over three MMFs, read-only `FILE_MAP_READ`) |
| `src/sims/assetto_corsa/record.rs` | `coach record`: 192-field `RecordFrame` in the C# logger's exact key order, `LineWriter` (create_new, gzip default, flush/200), `default_path` + UTC `yyyyMMdd_HHmmss`, `record<R>()` loop with baseline-relative lap counting |
| `src/sims/mod.rs` | `record()` default on `SimProvider`, `RecordOptions`/`RecordSummary`, `provider_for_live()` (—key or single provider; several without a key is an error naming them) |
| `src/runtime/setup.rs` | `load_model_for_session` / `load_reference_for_session`, moved out of main.rs so live/GUI setup can't drift |
| `src/ui/launcher.rs` | `CoachGui` (the eframe app now): phases Picking → Waiting → Live(`CoachApp`) → Failed; background `attach()` thread owns every blocking call; pickup announcement printed + spoken |
| `src/ui/app.rs` | `CoachApp` reworked: `with_sink` (voice), `say()`, `render()` (no longer an eframe::App itself — CoachGui owns the window), `shutdown()` |
| `src/audio/sink.rs` | `FeedbackSink: Send` + `say()` (TtsSink speaks-when-idle and counts; NullSink records `said`), `Speech: Send` |
| `src/telemetry/source.rs` | `set_stop_flag(Arc<AtomicBool>)` on `TelemetrySource` (default no-op) so blocking live sources observe shutdown; wired in `runtime/threads.rs` |

Design decisions worth knowing before changing anything:

- **Waiting is a state, not an error.** `provider.live()` never fails; the
  source retries attach in `next_sample`, printing each distinct reason once.
  A plausibility-guard failure on the first frame IS a hard error
  (`StepOutcome::Fatal`) — same refusal a corrupt capture gets.
- **Interchange contract**: a `coach record` capture must stay
  byte-compatible (key set and order) with the C# logger's. `record.rs` tests
  pin this against the real Monza capture's first line. Only known
  difference: integral floats print `1.0` vs the logger's `1` (both parse
  identically).
- **The GUI never blocks on the sim** — the attach thread does attach, first
  sample, model load, pipeline spawn, even `TtsSink::connect`. The UI thread
  only polls a crossbeam channel.
- No `.meta.json` sidecar from `coach record` — the recorder *is* the probe.

## Test-fixture dependency to know about

Tests that read `ndjson_data/*.ndjson.gz` (the real captures) **skip with a
note when the files are absent** — they are gitignored, so CI runs without
them. `data/tracks/ac/*.json` models *are* tracked and their tests do run.
If a CI run looks suspiciously fast, that is why (247 pass either way, but
the capture-based ones become no-ops off this machine).

## Likely next steps after the owner returns

- Any Windows compile/runtime fixes the test surfaces (see above for where
  they'd be).
- In-game results: page offsets and the 10 ms poll rate were verified against
  the C# logger's live probe; real-AC behaviour (e.g. teardown NaNs, replay
  mode pages, online sessions) is what this trip checks.
- If the pickup/waiting UX needs tuning, the strings live in
  `src/ui/launcher.rs` ("Waiting, when you are on track in {sim name}, the
  results will show here.", "{sim name} stream picked up") and
  `src/main.rs` `live()` (the CLI announcement).
- Untouched backlog: nothing else is pending from the batch plan
  (`docs/implementation-plan.md`); a second sim provider remains the next
  architectural milestone, deliberately not started.

## Standing rules

- Never install packages/toolchains on the Linux machine — hand commands to
  the owner or let CI do it. Cargo target-scoped deps are fine.
- Nothing gets committed by the assistant unless asked; the owner makes the
  commits.
- Verification bar: `cargo test` green after every step; clippy must not add
  warnings to the 17-warning baseline.
