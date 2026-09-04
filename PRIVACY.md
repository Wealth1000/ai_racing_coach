# Privacy

AI Racing Coach is local-first software. This document describes exactly what
the coach does with your data, written against the code as it stands
(`src/storage/share.rs`, `src/core/settings.rs`, `share-backend/src/worker.js`).
If the code and this document ever disagree, that's a bug — please
[open an issue](https://github.com/Wealth1000/ai_racing_coach/issues).

## The short version

- The coach runs entirely on your machine. It makes **no network requests**
  except the optional, opt-in "Send to author" described below.
- No accounts, no sign-up, no analytics, no crash reporting, no telemetry
  beacon. There is nothing phoning home in the background.
- Your raw telemetry captures contain your player name (the simulator puts
  it there). They **never leave your machine**.

## What the coach stores locally

Everything the coach writes lives under `data/` next to the executable:

| Path | Contents |
|---|---|
| `data/captures/` | Raw telemetry captures (`.ndjson.gz`) recorded by the coach |
| `data/tracks/ac/` | Learned track models and your personal bests per corner |
| `data/sessions/` | Recorded live sessions (one NDJSON file per session) |
| `data/share/` | Bundles prepared for sharing (including ones that failed to upload) |
| `data/settings.json` | Settings, and — only if you opt in — your install id |

Nothing in this table is uploaded automatically. Deleting the `data/`
directory removes every trace of your driving from the tool.

## The optional share: what "Send to author" sends

Sharing is a two-step opt-in:

1. **Consent, off by default.** Ticking "Share telemetry" opens a dialog that
   states what is sent and why. It stays off unless you confirm it there.
2. **An explicit button, per export.** Even with consent on, nothing is sent
   until you press "Send to author" on the export screen. Each press sends
   one bundle.

You can turn consent off at any time; nothing further is sent.

### The bundle

One gzipped JSON object: a small manifest plus a CSV of one row per corner
pass. Concretely, the manifest contains:

- the coach's version and the share schema number
- the simulator (`ac`) and the **names** of the track and the cars driven
- the number of sessions and the number of data rows
- this install's random id (below)

The CSV is the 24-column corner-pass table the coach's own analysis uses:
speeds (m/s), braking points and lengths (m), apex offsets, throttle pickup,
slip angles, corner times (s), deltas against your personal best, and advice
counts. It is pure numbers plus a per-session id.

### Session names

Session names are the one field in that CSV a driver could have made their
own. Before the CSV enters the bundle, each name is replaced with
`s_` plus a 64-bit hash of `(install id, session name)`. The hash is
deterministic — the same session yields the same id across uploads, which is
what lets the training corpus group rows honestly — but the name itself never
leaves the machine, and the hash cannot be inverted to recover it.

### The install id

A random id (`install_<hex>_<hex>`) generated **only when you first consent**
— an id for a machine that never shared is never even created. It is stored
in your local settings file and is stable across your uploads, so the corpus
can measure whether an anonymous driver's laps get faster over time. It
contains no name, hardware, or anything else about you.

### What never leaves your machine

- Your name and player ID
- Your hardware, OS, or settings
- Your raw telemetry captures — they contain your player name and are never
  bundled, in whole or in part
- Your session names (hashed away, as above)

### One honest caveat

The same honesty the consent dialog gives you: telemetry has no names, but a
driving style is itself a fingerprint. With enough laps, the author could
recognise "this is probably the same driver" — that is precisely what the
install id exists to make explicit and non-identifying: the linkage is a
random id, not a person.

## Where the bundle goes and what happens to it

The upload is a single HTTPS POST to the author's receiver, a Cloudflare
Worker writing to a key-value store (`https://coach-share.anthonyaddo999.workers.dev/`,
compiled in; the `COACH_SHARE_ENDPOINT` environment variable can override or
disable it for testing). The receiver validates the content type, gzip magic
bytes and an 8 MB cap, then stores the bundle under
`share/<upload-date>/<uuid>.json.gz`. It writes no other record: no IP
logging, no sender metadata, no accounts — only the bundle bytes and
`{schema, size}` as store metadata. If your upload fails (receiver down,
network gone), the coach never blocks or errors you: the bundle is saved to
`data/share/` so you can send it by hand or delete it.

## Retention and your choices

- Bundles are kept until the neural coach project ends, and are used only to
  build the training corpus described in
  [docs/neural-coach-design.md](docs/neural-coach-design.md).
- There is no per-bundle deletion endpoint (the receiver cannot verify who
  owns an anonymous bundle); if you want a specific bundle withdrawn, open an
  issue with the upload date and we'll remove that day's matching keys.
- Turning consent off stops all future sends. Deleting the `data/`
  directory removes the local install id and every local artefact.

## Contact

Questions about this policy: [open an issue](https://github.com/Wealth1000/ai_racing_coach/issues).
