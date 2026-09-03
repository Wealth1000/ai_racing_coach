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
 .ndjson capture ─▶ telemetry (strict AC schema) ─▶ Sample (canonical units)
                 ─▶ lap tracking ─▶ 1 m distance grid ─▶ curvature
                 ─▶ corner model (learned) ─▶ per-pass features
                 ─▶ rule model ─▶ phraser ─▶ decision engine ─▶ voice / GUI
```

The pipeline is a streaming state machine: one sample in, zero or more events
out, no stage buffering a whole lap. Replay and live share this one code path
— a replay is the same session, minus the sim — and the streaming path is
golden-tested to agree with the offline analysis on real captures.

## Status

**Supported simulator: Assetto Corsa only.** Captures from anything else are
refused with an explicit schema error rather than mis-parsed — the telemetry
reader is deliberately strict. No other simulator is supported at this time.

| Capability | State |
|---|---|
| Capture inspection, track learning, offline analysis, personal bests | done |
| Live coaching from a capture replay (Linux/Windows), voice, GUI, session recording, dataset export | done |
| Reading AC's shared memory directly on Windows (`coach record`, `coach live` without a capture) | in development |

Until the Windows shared-memory reader lands, captures are made with the
bundled C# logger (below) and the coach runs against them — live, at full
pipeline speed.

## Getting started

Requires Rust 1.85+ (stable) and an Assetto Corsa capture in `.ndjson` or
`.ndjson.gz` (see [Recording telemetry](#recording-telemetry-windows)).

```console
$ cargo build --release

$ coach inspect capture.ndjson.gz        # what is in this capture?
$ coach learn-track capture.ndjson.gz    # learn data/tracks/<track>.json
$ coach learn-pb capture.ndjson.gz       # your best pass per corner
$ coach analyse capture.ndjson.gz        # corner-by-corner table, offline
$ coach live --replay capture.ndjson.gz  # live coaching over the capture
```

`coach gui --replay capture.ndjson.gz` opens the coaching window instead of
printing to the terminal.

Useful flags: `--all-laps` (inspect/analyse: every clean lap, not just the
fastest), `--step <m>` (distance-grid spacing), `--model-dir` (where models
live, default `data/tracks`), `--dry-run` (learn without writing).

## Recording telemetry (Windows)

Run `AcTelemetryLogger` (in `Logger Programs/AcTelemetryLogger`) alongside
Assetto Corsa. It attaches to AC's shared-memory pages and writes an NDJSON
capture — plain or gzipped on `.gz`. Copy the capture to the machine running
the coach, or run both on the same box.

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
├── tracks/      <track>_<layout>.json — the learned corner model
│                <track>_<layout>_pb.json — your best pass per corner
└── sessions/    <session-id>.ndjson — one file per live session
```

## Development

```console
$ cargo test      # 199 tests, no hardware or display required
```

The voice and GUI layers are abstracted behind small traits (`Speech`,
`FeedbackSink`, the render-free row model), so everything is testable on a
headless box. `docs/implementation-plan.md` records the design rationale and
the batch plan; the struct layouts in `Logger Programs/AcTelemetryLogger`
are the reference for the Windows shared-memory reader.

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

TBD
