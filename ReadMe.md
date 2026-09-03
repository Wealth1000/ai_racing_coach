# AI Sim Racing Coach

An offline, real-time race engineer for Assetto Corsa, written in Rust. It
learns a track's corners from your own laps, measures how you drive each one,
and coaches you through them — by voice, at the moment it matters — with every
session recorded as data.

Everything runs on your machine. No network, no accounts, no cloud.

## What it does

- **Learns the track.** Clean laps vote on where the corners are; only
  corners a statistical majority confirms enter the model. One capture is
  enough; more captures sharpen it.
- **Measures every pass.** Entry/apex/exit speeds, braking point and length,
  throttle pickup, slip, time in corner — as a feature row per corner per
  lap.
- **Coaches in real time.** Advice is produced the moment a corner pass
  completes and spoken through the OS speech synthesiser. Advice is
  perishable: if the synth is still busy, the line is skipped and counted —
  never queued behind a stale sentence.
- **Compares against your best.** Your fastest clean pass per corner becomes
  the reference; advice argues in deltas against it.
- **Records everything.** Every live session is written to disk as NDJSON and
  exportable as a flat CSV corpus — the dataset a future learned model
  trains on.

## How it works

```text
 .ndjson capture ─▶ sim provider (AC) ─▶ Sample (canonical units)
                 ─▶ lap tracking ─▶ 1 m distance grid ─▶ curvature
                 ─▶ corner model (learned) ─▶ per-pass features
                 ─▶ rule model ─▶ phraser ─▶ decision engine ─▶ voice / GUI
```

The pipeline is a streaming state machine: one sample in, zero or more events
out, no stage buffering a whole lap. Replay and live share this one code path
— a replay is the same session, minus the sim — and the streaming path is
golden-tested to agree with the offline analysis on real captures.

Simulators plug in below the pipeline: each one is a *provider*
(`src/sims/<name>/`) that owns its capture format, its unit conversions and —
eventually — its live telemetry reader, and hands the pipeline the canonical
`Sample` stream. The registry in `src/sims/mod.rs` decides which provider a
capture belongs to (a file no provider recognises is refused loudly, with
every provider's reason), and `--sim <key>` skips the guessing. Adding a sim
means one new module and one registry entry; the pipeline, coaching, storage
and UI are untouched.

## Status

**Supported simulator: Assetto Corsa** — the only provider registered today.
Captures from anything else are refused with an explicit schema error rather
than mis-parsed — the provider is deliberately strict about what it claims.
The architecture is modular (see above): further sims arrive as new
providers, without touching the pipeline.

| Capability | State |
|---|---|
| Capture inspection, track learning, offline analysis, personal bests | done |
| Live coaching from a capture replay (Linux/Windows), voice, GUI, session recording, dataset export | done |
| Reading AC's shared memory directly on Windows (`coach record`, `coach live`/`coach gui` without a capture) | done |

Captures can be made two ways on Windows: `coach record` (the coach itself
reading AC's shared memory, writing the same NDJSON the C# logger writes) or
the bundled C# logger (below). Either file feeds everything else — live, at
full pipeline speed.

## Getting started

Requires Rust 1.85+ (stable) and an Assetto Corsa capture in `.ndjson` or
`.ndjson.gz` (see [Recording telemetry](#recording-telemetry-windows)).

```console
$ cargo build --release

$ coach inspect capture.ndjson.gz        # what is in this capture?
$ coach learn-track capture.ndjson.gz    # learn data/tracks/ac/<track>.json
$ coach learn-pb capture.ndjson.gz       # your best pass per corner
$ coach analyse capture.ndjson.gz        # corner-by-corner table, offline
$ coach live --replay capture.ndjson.gz  # live coaching over the capture
```

On Windows, with Assetto Corsa running, the coach can also take its telemetry
live from the sim itself — no capture file needed:

```console
$ coach record                           # write a capture straight from the sim
$ coach live                             # wait for the sim, then coach live
$ coach gui                              # pick the sim in the window, then coach
```

`coach live` waits for the sim to start (saying why, once per reason),
announces "Assetto Corsa stream picked up" when telemetry flows — printed and
spoken — and coaches from the first frame. `coach gui` opens at a sim picker:
pick one, and the window waits ("Waiting, when you are on track in Assetto
Corsa, the results will show here.") until the car is placed, then announces
the pickup and coaches. `coach gui --replay capture.ndjson.gz` skips all that
and streams the capture through the same window.

Useful flags: `--all-laps` (inspect/analyse: every clean lap, not just the
fastest), `--step <m>` (distance-grid spacing), `--model-dir` (where models
live, default `data/tracks`), `--sim <key>` (force a provider instead of
probing — e.g. `--sim ac`), `--dry-run` (learn without writing). For
`coach record`: `--out FILE` (never overwrites an existing capture),
`--laps N` (stop after N laps), `--plain` (uncompressed NDJSON).

## Recording telemetry (Windows)

The coach itself can record: `coach record` attaches to AC's shared-memory
pages and writes an NDJSON capture — the same format, key for key, as the
bundled C# logger, so nothing downstream can tell them apart. `--laps N`
gives a clean stop; without it, Ctrl-C ends the recording (the last unflushed
line and the gzip trailer are lost, everything before is readable).

Alternatively, run `AcTelemetryLogger` (in `Logger Programs/AcTelemetryLogger`)
alongside Assetto Corsa. It writes the same captures; it also writes a
`.meta.json` sidecar with its own probe verdicts, which `coach` reads first.

## Voice output

Advice is spoken through the OS speech synthesiser, on by default. If no
speech backend is available, the coach prints one warning and continues in
silence — the session and its recording are never lost, and unspoken lines
are counted in the summary.

### Linux

The backend is speech-dispatcher.

Build-time (headers for the `tts` crate):

```console
$ sudo dnf install speech-dispatcher-devel        # Fedora
$ sudo apt install libspeechd-dev                 # Debian / Ubuntu
```

Run-time (a voice — the espeak-ng module is the usual choice):

```console
$ sudo dnf install speech-dispatcher-espeak-ng    # Fedora
$ sudo apt install speech-dispatcher-espeak-ng    # Debian / Ubuntu
```

The daemon starts itself on first use; verify outside the app with
`spd-say "test"`.

To build without any speech support (a machine without the headers, or CI):

```console
$ cargo build --no-default-features
```

To run one session silently without changing the build:

```console
$ coach live --replay capture.ndjson.gz --voice null
```

### Windows

Nothing to install: the coach uses the SAPI voices that ship with Windows
10/11. Additional and higher-quality voices can be added under
*Settings → Time & Language → Speech → Manage voices*. Voice support
compiles identically on both platforms and will be active with the Windows
live build.

## Sessions and datasets

`coach live --record-session <dir>` writes one NDJSON file per session: a
header, then lap boundaries, corner passes and delivered advice with the
channel counters at each moment. A crash mid-recording costs the half-written
line and nothing else — the file parses to the last complete line.

`coach export-dataset <dir> <out.csv>` flattens a directory of sessions into
one row per corner pass (24 columns: the measured features, the personal
best's numbers, the deltas, and the outcome flags). Sessions recorded against
a different version of the track model are refused rather than mis-joined —
corner number 7 means a place only within the model that numbered it.

## Data layout

```text
data/
├── tracks/
│   └── ac/       one directory per simulator (the provider's key)
│       ├── <track>_<layout>.json — the learned corner model
│       └── <track>_<layout>_pb.json — your best pass per corner
└── sessions/    <session-id>.ndjson — one file per live session
```

The per-sim directory is not cosmetic: two simulators can name the same
circuit, and a model's corners are only true in the sim whose telemetry
produced them — a model is refused at load time if its sim does not match
the session's.

## Development

```console
$ cargo test      # 247 tests, no hardware or display required
```

The voice and GUI layers are abstracted behind small traits (`Speech`,
`FeedbackSink`, the render-free row model and phase machine), so everything
is testable on a headless box. `docs/implementation-plan.md` records the
design rationale and the batch plan; the struct layouts in `Logger
Programs/AcTelemetryLogger` are the reference the Windows shared-memory
reader was built against.

### Assets

The logo art lives in `assets/`: `logo.png` (full art), `icon_256.png`
(squared window icon, embedded in the binary at build time) and `logo.ico`
(16–256 px, embedded in the Windows executable via `build.rs`). To refresh
the derived files after changing the logo:

```console
$ magick assets/logo.png -resize 256x256 -gravity center -background none \
      -extent 256x256 assets/icon_256.png
$ for s in 128 64 48 32; do magick assets/icon_256.png -resize ${s}x${s} /tmp/icon_$s.png; done
$ magick assets/icon_256.png /tmp/icon_128.png /tmp/icon_64.png \
      /tmp/icon_48.png /tmp/icon_32.png assets/logo_16.png assets/logo.ico
```

`logo_16.png` is the hand-tuned 16 px art and is used as-is for the ICO's
smallest entry. On Windows the exe icon compiles with the SDK's `rc.exe` no
extra setup; cross-compiling from Linux additionally needs `mingw64-windres`.

## License

MIT — see [LICENSE](LICENSE).
