//! Throwaway diagnostic: print the signed curvature profile over a distance
//! window, to see whether a detected "corner" is actually two of opposite hand.
//!
//! Usage: cargo run --example curvature_dump -- <capture> <from_m> <to_m>

use ai_racing_coach::features::corner;
use ai_racing_coach::features::curvature;
use ai_racing_coach::features::lap::{Lap, LapTracker};
use ai_racing_coach::features::resample;
use ai_racing_coach::telemetry::{NdjsonReplaySource, TelemetrySource};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let capture = &args[1];
    let from: f32 = args[2].parse().unwrap();
    let to: f32 = args[3].parse().unwrap();

    let mut source = NdjsonReplaySource::open(std::path::Path::new(capture)).unwrap();
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

    let lap = laps
        .iter()
        .find(|l| l.quality.is_clean())
        .expect("a clean lap");
    let grid = resample::resample_lap(&lap.samples, 1.0).expect("resample");
    let profiles = curvature::corner_profiles(&grid.samples, grid.step_m);
    let threshold =
        corner::adaptive_threshold(&profiles.magnitude, &corner::CornerParams::default());

    println!(
        "lap {}  threshold {:.5}  (signed curvature; + is right)",
        lap.id.0, threshold
    );
    println!("{:>7}  {:>10}  {:>6}  bar", "dist", "signed", "above");

    for (i, s) in grid.samples.iter().enumerate() {
        let d = s.lap_distance;
        if d < from || d > to || (d as i32) % 4 != 0 {
            continue;
        }
        let signed = profiles.signed[i];
        let above = if profiles.magnitude[i] > threshold {
            "yes"
        } else {
            " . "
        };
        // Bar centred on zero: left of centre is a left-hander.
        let scale = 40.0 / threshold.max(1e-6);
        let n = ((signed * scale) as i32).clamp(-38, 38);
        let mut bar = String::new();
        for col in -38..=38 {
            bar.push(if col == 0 {
                '|'
            } else if (n > 0 && col > 0 && col <= n) || (n < 0 && col < 0 && col >= n) {
                '#'
            } else {
                ' '
            });
        }
        println!("{d:>7.0}  {signed:>10.5}  {above:>6}  {bar}");
    }
}
