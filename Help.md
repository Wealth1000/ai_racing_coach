# `coach` — command-line guide

`coach` reads Assetto Corsa telemetry captures and tells you what is in them: laps,
corners, and a reusable model of the circuit.

Everything here is offline. Nothing needs the sim running, or Windows — a capture on
disk is all the input there is.

---

## Building and running

```bash
cargo build                    # debug binary at ./target/debug/coach
cargo build --release          # faster; worth it on full-stint captures
```

Then either form works:

```bash
./target/debug/coach inspect capture.ndjson
cargo run -- inspect capture.ndjson
```

The `--` in the `cargo run` form matters: it separates cargo's flags from `coach`'s.

## Input files

`coach` accepts `.ndjson` and `.ndjson.gz` — gzip is decompressed transparently, so
there is no need to unpack a capture first. One JSON object per line, as written by the
logger in `Logger Programs/`.

If a capture has a `.meta.json` sidecar beside it, `coach` reads that **first**. The
logger writes its own verdict there, and if it recorded a fatal probe failure `coach`
refuses to analyse the file rather than producing plausible numbers from known-bad data.
Non-fatal sidecar notes print as `warning:` lines and analysis continues.

> **Paths with spaces need quoting.** The captures in this repo live in a directory
> called `ndjson data/`, so every example below quotes the path. Forgetting the quotes
> gives you a confusing "no such file" for a file that plainly exists.

---

## `coach inspect` — what is in this capture?

The command to reach for first. It never writes anything.

```bash
coach inspect <capture> [--step <metres>] [--all-laps]
```

| Flag | Default | Meaning |
|---|---|---|
| `--step <m>` | `1.0` | Distance-grid spacing for resampling. Must be positive. |
| `--all-laps` | off | A corner table for every clean lap, not just the fastest. |

```bash
./target/debug/coach inspect \
  "ndjson data/telemetry_ac_ks_red_bull_ring_ks_mazda_mx5_cup_20260822_135019.ndjson.gz"
```

### Reading the output

```
telemetry_ac_..._135019.ndjson.gz — ks_mazda_mx5_cup in ks_red_bull_ring/layout_gp (4286.7896 m, AC 1.14.1, SM 1.7)

Frames read     51383
Blank lines     0
Unparseable     0
```

The header is car, track/layout, and track length — the last read straight from
`StaticInfo_TrackSPlineLength`, not estimated. **`Unparseable` should be 0.** Anything
else means lines the schema rejected, and the count is the first thing to check if the
rest of the output looks thin.

```
Laps (8 wrap segments)
   id       time  coverage   rotation  samples  quality
    0      3.61s      0.4%    -0.02pi      219  partial lap
    1    131.29s    100.0%     2.02pi     8449  clean (wall clock)
    2    128.90s     99.9%     1.99pi     8289  clean
    3    145.38s    100.0%     4.00pi     9359  spun
    4    175.22s     99.9%     1.98pi     7980  not live (paused/replay/pits)
    5    130.79s    100.0%     2.02pi     8417  clean (wall clock)
    6    130.82s    100.0%     1.90pi     8542  3+ tyres off track
    7      1.97s      0.7%    -1.10pi      128  partial lap
  8 segments, 6 full, 3 clean
```

Every segment between start/finish crossings is listed, including the junk, so you can
see *why* a lap was rejected instead of wondering where it went.

- **coverage** — how much of the track distance the segment actually covers. A lap that
  starts mid-circuit cannot be compared against a full one.
- **rotation** — net heading change in units of pi. A clean lap of a closed circuit is
  `2.00`; lap 3's `4.00pi` is a spin, caught by geometry rather than by a flag.
- **quality** — the verdict. Only `clean` laps are used for anything. `(wall clock)`
  means the sim never reported a lap time, so it was timed from frame timestamps.

```
Lap 2 — 128.90s, 8289 raw samples
  resampled to 4284 points @ 1.00 m (3203 non-monotone samples dropped)
  curvature zeros: 75.8% raw -> 0.0% resampled
  10 corners, 8 right / 2 left
```

**`curvature zeros` is the health check for the whole pipeline.** Raw AC frames are
unevenly spaced in distance, which leaves curvature degenerate on 75-81% of samples —
no threshold tuning recovers a corner from that. Resampling onto an even grid is what
fixes it, and the `raw -> resampled` pair is the proof it worked. If the resampled
figure is not near 0%, nothing downstream is trustworthy.

The dropped samples are not a problem: they are frames that did not advance in track
distance (stationary, or jitter), and resampling needs a monotone input.

```
  turn  dir     start       end   length     apex   radius      turn   min spd
    T1    R      265m      331m      66m     314m      34m       64°   18.2m/s
    T2    R     1215m     1287m      72m    1251m      21m      129°   13.3m/s
    ...
  top speed 176.0 km/h at 1887 m
```

Distances are metres along the track spline from the start/finish line, so they mean the
same thing in every lap and every car. `radius` is the fitted radius at the apex —
smaller is tighter. `turn` is the total heading change through the corner. `min spd`
is the slowest point, which is what a driver asks about first, so the fastest point on
the lap is printed underneath for contrast.

**Corner numbering here is per-lap and not stable.** One lap of a ten-corner circuit
can yield anywhere from 9 to 13 corners depending on where the driver put the car.
For a numbering you can rely on, use `learn-track`.

---

## `coach learn-track` — build a reusable model of the circuit

`inspect` tells you about one lap. `learn-track` combines every clean lap into a
canonical corner set and saves it, so later commands can talk about "T4" and mean it.

```bash
coach learn-track <capture> [--out <dir>] [--step <m>]
                            [--min-support <fraction>] [--apex-tolerance <m>]
                            [--dry-run]
```

| Flag | Default | Meaning |
|---|---|---|
| `--out <dir>` | `data/tracks` | Where to write `<track>_<layout>.json`. |
| `--step <m>` | `1.0` | Distance-grid spacing. Must be positive. |
| `--min-support <f>` | `0.5` | Fraction of clean laps that must agree on a corner. `0`-`1`. |
| `--apex-tolerance <m>` | `25` | How far apart two laps' apexes may be and still count as the same corner. |
| `--dry-run` | off | Print the model, write nothing. |

**Use `--dry-run` first.** It shows exactly what would be saved.

```bash
./target/debug/coach learn-track \
  "ndjson data/telemetry_ac_ks_red_bull_ring_ks_mazda_mx5_cup_20260822_135019.ndjson.gz" \
  --dry-run
```

### How it decides

Two mechanisms, and it helps to know both because they explain every surprising result.

**A corner must be seen by several laps to exist.** Corners are detected independently
on each clean lap and then vote. `--min-support 0.5` means half the laps must have found
it; the floor is two laps regardless, since one lap agreeing with itself is not evidence.
This is what removes single-lap noise.

**Geometry comes from one representative lap** — the *medoid*, the lap with the lowest
mean separation from all the others, compared at equal track distance. Not the fastest
lap: the fastest lap of a short session is routinely an outlier that caught a tow or
clipped a kerb. The medoid is by construction the most typical line driven.

### Reading the output

```
Track model — ks_red_bull_ring/layout_gp (ks_mazda_mx5_cup), 4287 m
  11 corners, 9 right / 2 left
  learned from 3 clean lap(s) in telemetry_ac_..._135019.ndjson.gz, reference lap 1
  line spread 1.31 m mean, 6.40 m worst at 2083 m, 1.00 m grid
```

`line spread` is how far apart the laps ran. **`worst at` is the diagnostic to reach for
when a model looks wrong** — it names the exact distance where the laps disagreed most,
which is usually a mistake on one lap and often visibly a corner you know.

A mean around 1 m is consistent driving. A mean of several metres means the laps
genuinely took different lines, and the model is an average of driving that was not
repeatable — treat the geometry with suspicion.

```
  turn  dir     start       end   length      apex   radius      turn     laps
    T1    R      283m      339m      56m      319m      37m       58°     3/3
    T5    R     2279m     2312m      33m     2294m     110m       16°     2/3
    ...
  not unanimous: T5 (2/3), T8 (2/3), T9 (2/3)
```

Same columns as `inspect`, with `laps` replacing `min spd`: how many clean laps found
this corner. **The `not unanimous` line is where the model is least certain and the
first thing to check when it looks wrong.** Above, T5, T8 and T9 were each found by 2
of 3 laps. T8 and T9 are 30 m long and 27 m apart — plausibly one corner that the
detector split, or a kink one lap straightened.

### Writing, and overwriting

Without `--dry-run` the model is written to `<out>/<track>_<layout>.json`, atomically
(temp file plus rename, so an interrupted write cannot leave a half-model on disk).

The filename keys on **track and layout only**, so learning the same circuit in a second
car lands on the same path. That is deliberate — a model is per-car by construction,
because corner boundaries depend on where the car could actually carry speed, and there
is no car-independent answer to fall back on. But it is never silent: an overwrite prints
the old and new corner counts, and says so explicitly when the car changed.

```
Replacing the model at data/tracks/ks_red_bull_ring_layout_gp.json
  was: 11 corners from 3 lap(s) of ks_mazda_mx5_cup
  now: 8 corners from 3 lap(s) of ks_ferrari_f138
  note: different car — boundaries shift with speed, so this is a different model
        of the same circuit, not a correction of the old one
```

Two cars disagreeing about the corner count is expected, not a bug. The F138 carries
speed through kinks the MX5 has to slow for, so the MX5 sees more corners.

---

## When the corner count looks wrong

The most likely complaint, so worth its own section.

**The vote can only remove corners, never add them.** Every lap runs the same detector
with the same thresholds, so a corner missed on every lap is missed by the model too —
no amount of `--min-support` recovers it. Check the count against the real layout before
trusting a model on a circuit you have not tried before.

- **Too many corners** — one real corner split into two. Look for adjacent short corners
  in the table with low support (T8/T9 above). Raise `--min-support` to demand more
  agreement, or lower `--apex-tolerance` if the split pieces are close together.
- **Too few corners** — two real corners merged, or one never detected. `--apex-tolerance`
  defaults to 25 m; on a circuit with corners packed tighter than that, neighbours
  compete for the same votes. Lower it and re-run with `--dry-run`.
- **Wildly different from `inspect`** — compare against the reference lap named in the
  output, not the fastest. Those are usually different laps.

A caveat worth knowing: `--apex-tolerance`'s default is a **tuning knob**, not a derived
constant. It inherits its magnitude from work on one circuit. The same is true of several
thresholds inside corner detection that are not exposed on the CLI yet.

**Corners straddling the start/finish line are not handled.** On a circuit where a corner
begins before the line and ends after it, expect the model to be wrong there.

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | The command ran and failed — unreadable file, unusable capture, not enough clean laps, a corrupt model on disk. |
| `2` | The command line itself was wrong — bad flag, bad value. Nothing was read or written. |

`1` and `2` are worth distinguishing in a script: `2` means fix the invocation, `1` means
look at the data.

Runtime failures name the field that went wrong rather than the symptom, and print the
underlying cause beneath:

```
error: ndjson data/capture.ndjson:1204: could not parse telemetry frame: invalid type: string "abc", expected f32
  caused by: invalid type: string "abc", expected f32
```

Bad arguments are caught before any file is opened:

```
error: invalid value '50' for '--min-support <MIN_SUPPORT>': must be a fraction between 0 and 1 (0.5 means half the laps)
```

That one is a real trap: `--min-support 50` looks like "50%" and is not.

---

## Getting help from the tool

```bash
coach --help                # subcommands, plus a worked examples block
coach help learn-track      # the full description of one subcommand
coach --version
```

`-h` includes the examples block too, since `-h` is what people actually type.
