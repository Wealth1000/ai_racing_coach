//! Dump every curvature peak above the adaptive threshold on a captured lap,
//! alongside the corners the detector currently reports. Diagnostic for
//! understanding why a corner count disagrees with the real-world count.

use std::path::PathBuf;

use ai_racing_coach::features::corner::{self, CornerParams};
use ai_racing_coach::features::curvature;
use ai_racing_coach::features::lap::LapTracker;
use ai_racing_coach::features::resample;
use ai_racing_coach::telemetry::replay::NdjsonReplaySource;
use ai_racing_coach::telemetry::TelemetrySource;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = PathBuf::from(&args[1]);

    let mut source = NdjsonReplaySource::open(&path)?;
    let mut tracker_opt: Option<LapTracker> = None;
    let mut lap_count = 0;

    while let Some(frame) = source.next_frame()? {
        if tracker_opt.is_none() {
            let track_length = source
                .session()
                .map(|s| s.track_length)
                .unwrap_or(frame.track_spline_length);
            tracker_opt = Some(LapTracker::new(track_length));
        }
        let tracker = tracker_opt.as_mut().unwrap();
        if let Some(lap) = tracker.push(&frame) {
            if !lap.quality.is_clean() {
                continue;
            }
            let Some(grid) = resample::resample_lap(&lap.samples, 1.0) else {
                continue;
            };
            lap_count += 1;

            let profiles = curvature::corner_profiles(&grid.samples, grid.step_m);
            let threshold =
                corner::adaptive_threshold(&profiles.magnitude, &CornerParams::default());
            let detected = corner::detect_corners_with(&grid, &CornerParams::default());

            // Method A: local maxima of curvature magnitude over a fixed
            // 25 m half-window. Strict definition: the peak must be
            // strictly greater than every sample within 25 m on either side.
            let half = 25usize;
            let mut peaks_local_max: usize = 0;
            for i in half..profiles.magnitude.len().saturating_sub(half) {
                let v = profiles.magnitude[i];
                if v <= threshold {
                    continue;
                }
                let mut is_peak = true;
                for j in 1..=half {
                    if profiles.magnitude[i - j] > v || profiles.magnitude[i + j] > v {
                        is_peak = false;
                        break;
                    }
                }
                if is_peak {
                    peaks_local_max += 1;
                }
            }

            // Method B: integrate |curvature| over a moving window centred
            // on each sample; a corner is a local maximum of this integral.
            // The integral naturally aggregates over a corner's length and
            // splits where curvature dips between consecutive corners.
            // Window length is `SMOOTH_WINDOW_M` (the same one curvature
            // smoothing already uses for *magnitude*) so the only "knob" is
            // not new.
            let win_half = (curvature::SMOOTH_WINDOW_M / 2.0 / grid.step_m).round() as usize;
            let mut integral = vec![0.0f32; profiles.magnitude.len()];
            if profiles.magnitude.len() > 2 * win_half {
                let mut sum: f32 = profiles.magnitude[..2 * win_half]
                    .iter()
                    .map(|v| v.abs())
                    .sum();
                integral[win_half] = sum;
                for i in (win_half + 1)..(profiles.magnitude.len() - win_half) {
                    sum -= profiles.magnitude[i - win_half - 1].abs();
                    sum += profiles.magnitude[i + win_half].abs();
                    integral[i] = sum;
                }
            }
            // Local maxima of the integral, again within ±half window.
            let mut peaks_integral: usize = 0;
            let win_max = integral.len().saturating_sub(win_half);
            for i in win_half..win_max {
                let v = integral[i];
                if v <= 0.0 {
                    continue;
                }
                let mut is_peak = true;
                for j in 1..=win_half {
                    if integral[i - j] > v || integral[i + j] > v {
                        is_peak = false;
                        break;
                    }
                }
                if is_peak {
                    peaks_integral += 1;
                }
            }

            println!(
                "lap {:>2} — {:.2}s, threshold={:.4}, detected={} | local_max_peaks={} | integral_peaks={}",
                lap.id.0,
                lap.lap_time_s(),
                threshold,
                detected.len(),
                peaks_local_max,
                peaks_integral,
            );
        }
    }
    println!("(across {lap_count} clean laps)");
    Ok(())
}