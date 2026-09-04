# AI Racing Coach for Assetto Corsa

**An offline race engineer that learns your track, coaches you by voice, and measures every lap.**

## What It Does

Every time you hit the track in Assetto Corsa:

- **Learns your corners** – Watches your clean laps and builds a model of every turn on the track. Works on any track, including community tracks — no built-in track database needed.
- **Coaches you live** – Voice feedback the moment you finish a corner ("brake later into T3"), spoken through your OS's speech synthesiser. Advice is perishable: if the voice is busy, the line is skipped — never queued behind a stale sentence.
- **Compares to your best** – Your fastest clean pass per corner becomes the reference; advice argues in deltas against *your* personal best, not someone else's.
- **Records everything** – Every live session is written to disk locally, and exportable as a flat CSV dataset.

No accounts. No cloud. The coach runs entirely on your machine. The only network it ever touches is an **opt-in** "Send to author" button that shares a scrubbed corner-summary table to help train a smarter model — see [PRIVACY.md](PRIVACY.md).

## Get Started

Grab the latest release for your platform:

- **Windows** (`coach-x.y.z-x86_64-pc-windows-msvc.zip`) — the full experience: record straight from the running sim and get live coaching.
- **Linux** (`coach-x.y.z-x86_64-unknown-linux-gnu.tar.gz`) — analysis and replay coaching from capture files.

from the [Releases page](https://github.com/Wealth1000/ai_racing_coach/releases), unzip anywhere, and run `coach gui`.

### The workflow

The coach needs to learn your track before it can coach you on it:

1. **Record** — drive a few clean laps in Assetto Corsa while `coach record` captures telemetry straight from the sim (no capture file juggling; the bundled C# logger also works)
2. **Learn** — build the track model from your laps, then learn your personal best per corner
3. **Drive** — `coach live` coaches you as you drive; `coach gui` gives you the same thing in a window

The GUI walks this order — capture, learn, drive — so a new track needs no instructions.

### Voice

Nothing to install on Windows — the coach uses the SAPI voices that ship with Windows 10/11. On Linux it uses speech-dispatcher (`--voice null` runs a session silently). If no speech backend is found, the coach says so once and continues in silence — your session is never lost.

## How It Works

Corner detection is statistical: clean laps vote on where the corners are, and only corners a majority confirms enter the model. One capture is enough to start; more captures sharpen it. The same pipeline runs live and offline — a replay is the same session, minus the sim.

```
capture / live sim → lap tracking → distance grid → curvature
                  → corner model (learned) → per-pass features
                  → rule model → voice / GUI
```

### Status

This is a beta, and it's honest about it:

- Coaching is **rule-based** today — every threshold is hand-tuned, which is why it's decent but not smart
- Supported simulator: **Assetto Corsa** (architecture is modular; more sims arrive as providers)
- Live-from-sim requires Windows; Linux analyses and replays captures

**That's where you come in.** The plan is a neural coach trained on real telemetry from many drivers ([design doc](docs/neural-coach-design.md)). Every lap you record — especially on different tracks, in different cars, with different driving styles — helps.

## Sharing Telemetry (Optional)

The coach ships with everything local. If you want to help train the next version, you can opt in and press **Send to author**. What leaves your machine:

- **One CSV row per corner pass** — speeds, braking points, apexes, times (24 columns of pure numbers)
- **A short manifest** — coach version, sim, track and car *names*, row counts, and this install's random id
- **Session names, hashed** — the grouping survives, the name does not leave your machine

What never leaves: your name, player ID, hardware, settings, or raw captures (the raw files contain your player name and stay local). Consent is off by default, gated by a dialog that says exactly what's sent, and can be withdrawn at any time.

Full details: [PRIVACY.md](PRIVACY.md).

## FAQ

**Q: Does it work on custom tracks?**
A: Yes — corners are learned from *your* laps, not a built-in database. Tracks with clear, distinct corners work best; vague transitions are harder. If detection struggles on a track, that's exactly the feedback we need.

**Q: What if I don't want to share data?**
A: Nothing happens. Everything runs offline; only the explicit "Send to author" button moves anything off your machine.

**Q: Can I use this in online races?**
A: The live reader attaches to Assetto Corsa's shared memory in single-player sessions (practice, time trial). Online races are out of scope.

**Q: Why does it need laps before coaching?**
A: The coach refuses to guess: with no learned model of the track's corners there's nothing to coach against ("learn one first"). A handful of clean laps is enough.

**Q: Is my driving data used for anything else?**
A: No — donations go into a training corpus for the neural coach. That's the entire sharing programme. See [PRIVACY.md](PRIVACY.md).

**Q: Will this ever cost money?**
A: No. Free, open-source (MIT), built by a sim racer.

## Feedback & Questions

Found a bug, or a track where corner detection struggles? [Open an issue](https://github.com/Wealth1000/ai_racing_coach/issues) — bug reports with the capture file attached are gold.

## For Developers

Rust, MIT, ~250 tests that run on a headless box with no hardware:

```console
$ cargo test
```

[Dev_ReadMe.md](Dev_ReadMe.md) covers the architecture, [Help.md](Help.md) is the full CLI guide, and [docs/](docs/) holds the design documents — including the neural coach plan.

---

**Latest release**: [v0.2.0](https://github.com/Wealth1000/ai_racing_coach/releases) · **Issues**: [GitHub](https://github.com/Wealth1000/ai_racing_coach/issues) · **License**: [MIT](LICENSE)
