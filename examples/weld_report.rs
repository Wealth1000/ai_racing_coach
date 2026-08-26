//! Throwaway diagnostic: for every detected corner on every clean lap, count how
//! many times the *signed* curvature changes sign inside it.
//!
//! A real corner has one sign. A detection containing n sign changes is n+1 real
//! corners welded into one.
//!
//! Usage: cargo run --example weld_report -- <capture>...

use ai_racing_coach::features::corner;
use ai_racing_coach::features::curvature;
use ai_racing_coach::features::lap::{Lap, LapTracker};
use ai_racing_coach::features::resample;
use ai_racing_coach::telemetry::{NdjsonReplaySource, TelemetrySource};

fn main() {
    println!(
        "{:<14} {:>4} {:>6} {:>7} {:>7} {:>7}  {}",
        "track", "lap", "found", "welded", "extra", "implied", "welded detections (dist, parts)"
    );

    for capture in std::env::args().skip(1) {
        let mut source = NdjsonReplaySource::open(std::path::Path::new(&capture)).unwrap();
        let mut tracker: Option<LapTracker> = None;
        let mut laps: Vec<Lap> = Vec::new();
        while let Some(frame) = source.next_frame().unwrap() {
            let t = tracker.get_or_insert_with(|| {
                let length = source
                    .session()
                    .map(|s| s.track_length)
                    .unwrap_or(frame.track_spline_length);
                LapTracker::new(length)
            });
            if let Some(lap) = t.push(&frame) {
                laps.push(lap);
            }
        }
        if let Some(t) = tracker {
            laps.extend(t.finish());
        }

        let track = source
            .session()
            .map(|s| s.track.to_string())
            .unwrap_or_else(|| capture.clone());
        let short: String = track.chars().take(14).collect();

        for lap in laps.iter().filter(|l| l.quality.is_clean()) {
            let Some(grid) = resample::resample_lap(&lap.samples, 1.0) else {
                continue;
            };
            let profiles = curvature::corner_profiles(&grid.samples, grid.step_m);
            let corners = corner::detect_corners(&grid);

            // A sign change only counts if both lobes are substantial, so noise
            // wobbling across zero on a straight is not called a corner.
            let lobe_floor = corner::adaptive_threshold(&profiles.magnitude, &Default::default());

            let mut welded = 0usize;
            let mut extra = 0usize;
            let mut notes: Vec<String> = Vec::new();

            for c in &corners {
                let lo = grid.index_at(c.start_m);
                let hi = grid.index_at(c.end_m).min(grid.samples.len() - 1);

                // Walk the span, recording each run of consistent sign whose peak
                // clears the floor.
                let mut parts = 0usize;
                let mut cur_sign = 0i32;
                let mut cur_peak = 0.0f32;
                for &k in &profiles.signed[lo..=hi] {
                    let s = if k > 0.0 { 1 } else { -1 };
                    if s != cur_sign {
                        if cur_peak > lobe_floor {
                            parts += 1;
                        }
                        cur_sign = s;
                        cur_peak = 0.0;
                    }
                    cur_peak = cur_peak.max(k.abs());
                }
                if cur_peak > lobe_floor {
                    parts += 1;
                }

                if parts > 1 {
                    welded += 1;
                    extra += parts - 1;
                    notes.push(format!("{:.0}m×{}", c.apex_m, parts));
                }
            }

            println!(
                "{:<14} {:>4} {:>6} {:>7} {:>7} {:>7}  {}",
                short,
                lap.id.0,
                corners.len(),
                welded,
                extra,
                corners.len() + extra,
                notes.join(" ")
            );
        }
    }
}
