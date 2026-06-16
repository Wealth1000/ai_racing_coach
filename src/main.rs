use flate2::read::GzDecoder;
use serde_json;
use std::fs::File;
use std::io::{BufRead, BufReader};
mod debug_helpers;
mod feature;
mod telemetry;
use crate::feature::frechet::find_lowest_frechet_distance_average;
use debug_helpers::dump_to_file::dump_to_file;
use feature::{
    FeatureSample, FrameSampler, RawLapData,
    corner::{compute_curvature, compute_heading_angle, smooth_curvature},
    detect_corners,
};
use telemetry::TelemetryFrame;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "telemetry_clean.ndjson".to_string());
    let file = File::open(&path).unwrap();

    let reader: Box<dyn BufRead> = if path.ends_with(".gz") {
        Box::new(BufReader::new(GzDecoder::new(file)))
    } else {
        Box::new(BufReader::new(file))
    };

    let mut sampler = FrameSampler { prev_frame: None };
    let mut lap_data = Vec::<FeatureSample>::new();
    for line in reader.lines() {
        let line = line.unwrap();
        let frame: TelemetryFrame = serde_json::from_str(&line).unwrap();
        let sample = sampler.continuous_sampling(&frame);
        if let Some(sample) = sample {
            lap_data.push(sample);
        }
    }

    // Build laps – this consumes lap_data
    let accumulated_lap_data = RawLapData::from_feature_samples(lap_data);

    // Debug: show sample counts per lap (we can't use lap_data anymore)
    let total_samples: usize = accumulated_lap_data.iter().map(|lap| lap.data.len()).sum();
    println!("Total samples: {}", total_samples);
    println!("Total laps: {}", accumulated_lap_data.len());
    for (i, lap) in accumulated_lap_data.iter().enumerate() {
        println!("Lap {}: {} samples", i, lap.data.len());

        // Determine track length from the longest lap's last sample
        let track_length = accumulated_lap_data
            .iter()
            .filter_map(|lap| lap.data.last().map(|s| s.lap_distance))
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap_or(0.0);

        println!("Estimated track length: {:.1} m", track_length);

        // Filter laps that are at least 95% of the track length
        let mut valid_laps: Vec<(usize, &RawLapData)> = accumulated_lap_data
            .iter()
            .enumerate()
            .filter(|(_, lap)| {
                let last = lap.data.last().map(|s| s.lap_distance).unwrap_or(0.0);
                last > track_length * 0.95
            })
            .collect();

        // If no valid laps, use all laps (but this should rarely happen)
        if valid_laps.is_empty() {
            valid_laps = accumulated_lap_data.iter().enumerate().collect();
        }

        // Build a slice of references to valid laps
        let ref_laps: Vec<&RawLapData> = valid_laps.iter().map(|(_, lap)| *lap).collect();

        // Find the lap with the lowest average Frechet distance (most representative)
        let best_idx_in_valid = find_lowest_frechet_distance_average(&ref_laps);

        // Get the original index of that lap
        let master_lap_trace = valid_laps[best_idx_in_valid].0;

        println!("Selected master lap index: {}", master_lap_trace);
        // Optional: uncomment to dump curvature profile
        let raw_curv = compute_curvature(&accumulated_lap_data[master_lap_trace].data);
        let smooth_curv = smooth_curvature(&accumulated_lap_data[master_lap_trace].data, &raw_curv);
        let heading = compute_heading_angle(&accumulated_lap_data[master_lap_trace].data);
        let mut profile = Vec::new();
        for i in 0..accumulated_lap_data[master_lap_trace].data.len() {
            profile.push((
                accumulated_lap_data[master_lap_trace].data[i].lap_distance,
                smooth_curv[i],
                heading[i],
            ));
        }
        dump_to_file(&profile, "curvature_profile.txt");

        let corners = detect_corners(&accumulated_lap_data[master_lap_trace].data);

        dump_to_file(&corners, "corners.txt");
        println!("Number of corners: {}", corners.len());
    }
}
