```markdown
# AI Sim Racing Coach (Windows, Offline)

An **offline, real-time race engineer** built in Rust that analyzes telemetry and provides actionable driving feedback while you race.

Designed to be:

- ⚡ Fast (native Rust performance)
- 🔌 Fully offline (no internet required)
- 🎮 Controller-aware (not just wheel users)
- 🧠 ML-ready (rule-based now → machine learning later)

---

## Vision

Most existing tools either:

- rely on cloud-based AI (latency, instability), or
- use large general-purpose models that are too heavy for local use

This project focuses on:

> A **lightweight, specialized driving intelligence system** tailored for sim racing.

---

## Supported Simulator (Initial Target)

- Automobilista 2

Support for other simulators (Assetto Corsa, iRacing, etc.) will come later.

---

## Core Features

### Real-Time Driving Coach

- Detects mistakes from telemetry
- Provides short, actionable feedback
- Designed to avoid distracting the driver

**Example feedback:**
- “Brake slightly earlier into this corner”
- “Apply throttle more gradually on exit”

---

### Fully Offline

- No API calls
- No subscriptions
- No network dependency

Everything runs locally.

---

### Controller-Aware Coaching

Unlike most tools, this system adapts for:

- analog stick steering
- trigger-based throttle/braking
- limited input precision

---

### ML-Ready Architecture

```

Today: Rule-Based Engine
Future: ML Model (ONNX)

```

The system is designed so ML can be added later without rewriting core logic.

---

## Architecture

The system follows a modular pipeline:

```

Simulator
↓
Telemetry Reader
↓
Feature Extractor
↓
Driving Model (Rule-based / ML)
↓
Decision Engine
↓
Feedback (Audio / UI)
↓
Data Logger (for training)

````

---

## Key Design Principles

### 1. Feature-First Design

Raw telemetry is converted into structured features:

- entry_speed
- apex_speed
- exit_speed
- brake_point
- throttle_timing
- steering_variance
- lap_delta

These features are:

- used by rule logic
- used by ML later
- stable long-term

---

### 2. Model Abstraction

Core interface:

```rust
trait DrivingModel {
    fn predict(features: &CornerFeatures) -> DrivingIssue;
}
````

Implementations:

* `RuleModel` (initial)
* `MLModel` (future)

---

### 3. Separation of Concerns

* **Detection** → what went wrong
* **Coaching** → how to fix it

Example:

```
late_brake → "Brake earlier before corner entry"
```

---

### 4. Built-In Data Collection

Every session logs:

* features
* detected issue
* lap performance

This becomes your future ML dataset automatically.

---

## Project Structure (Planned)

```
src/
 ├── telemetry/        # Automobilista 2 integration
 ├── features/         # Feature extraction
 ├── models/           # Rule + ML models
 ├── coaching/         # Advice generation
 ├── audio/            # Voice feedback
 ├── ui/               # GUI
 ├── storage/          # Logging & datasets
 └── core/             # Shared types

data/
 ├── sessions/
 └── datasets/

models/
 ├── rules/
 └── ml/
```

---

## Tech Stack

**Core**

* Rust

**GUI**

* egui (recommended for simplicity)
* or tauri (optional)

**Telemetry**

* UDP (Automobilista 2 shared memory/UDP output)

**Audio**

* rodio or cpal

**Storage**

* JSON / NDJSON (initial)
* SQLite (later)

**ML (Future)**

* Python (training)
* ONNX (model format)
* onnxruntime (Rust inference)

---

## Getting Started

### Requirements

* Windows 10/11
* Rust (latest stable)

### Setup

```bash
git clone https://github.com/yourname/ai-racing-coach
cd ai-racing-coach
cargo run
```

---

## Development Roadmap

### Phase 1 — Foundation

* [ ] Automobilista 2 telemetry reader
* [ ] Raw telemetry output (console)
* [ ] Basic feature extraction
* [ ] Simple rule-based feedback

### Phase 2 — Usable Coach

* [ ] Real-time feedback system
* [ ] Audio output
* [ ] Basic GUI
* [ ] Session logging

### Phase 3 — Intelligence Layer

* [ ] Corner detection
* [ ] Replay/debug tools
* [ ] Data pipeline

### Phase 4 — ML Integration

* [ ] Dataset export
* [ ] Model training (Python)
* [ ] ONNX integration
* [ ] Hybrid rule + ML system

---

## GUI Goals

* Select simulator
* Start/stop session
* Show live telemetry
* Display coaching feedback
* Review previous sessions

---

## Long-Term Goals

* Advanced telemetry visualization
* Track-specific analysis modules
* Setup coaching assistant
* Community datasets
* Ghost lap comparisons

---

## Contributing

Contributions are welcome in:

* telemetry integration
* feature engineering
* UI/UX
* ML experimentation

---

## License

TBD

---

## Next Step

Start with:

1. Automobilista 2 telemetry output
2. Print raw data
3. Build feature extractor
