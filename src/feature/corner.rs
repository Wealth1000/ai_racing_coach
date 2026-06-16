use crate::feature::FeatureSample;
use std::f32::consts::PI;

#[derive(Debug, Clone, Copy)]
pub enum CornerDirection {
    Left,
    Right,
}

#[derive(Debug)]
pub struct TrackCorner {
    pub corner_id: u32,
    pub corner_start: f32,
    pub corner_end: f32,
    pub geometric_apex: f32,
    pub heading_angle_apex: f32,
    pub corner_direction: CornerDirection,
    pub peak_curvature: f32,
}

// -----------------------------------------------------------------------------
// Curvature computation (unchanged from your version, but correct)
// -----------------------------------------------------------------------------
pub fn compute_curvature(lap_data: &[FeatureSample]) -> Vec<f32> {
    let mut curvature_data = Vec::<f32>::new();
    curvature_data.push(0.0);
    for i in 1..lap_data.len() - 1 {
        let dx1 = lap_data[i].world_position[0] - lap_data[i - 1].world_position[0];
        let dx2 = lap_data[i + 1].world_position[0] - lap_data[i].world_position[0];
        let dz1 = lap_data[i].world_position[2] - lap_data[i - 1].world_position[2];
        let dz2 = lap_data[i + 1].world_position[2] - lap_data[i].world_position[2];
        let dx3 = lap_data[i + 1].world_position[0] - lap_data[i - 1].world_position[0];
        let dz3 = lap_data[i + 1].world_position[2] - lap_data[i - 1].world_position[2];

        let cross_product = (dx1 * dz2) - (dz1 * dx2);
        let len1 = (dx1 * dx1 + dz1 * dz1).sqrt();
        let len2 = (dx2 * dx2 + dz2 * dz2).sqrt();
        let len3 = (dx3 * dx3 + dz3 * dz3).sqrt();
        if len1 <= 0.005 || len2 <= 0.005 || len3 <= 0.005 {
            curvature_data.push(0.0);
            continue;
        }
        let curvature = (2.0 * cross_product) / (len1 * len2 * len3);
        curvature_data.push(curvature);
    }
    curvature_data.push(0.0);
    curvature_data
}

// -----------------------------------------------------------------------------
// Smoothing – sample‑based window (could be improved with distance window,
// but left as is for now)
// -----------------------------------------------------------------------------
pub fn smooth_curvature(lap_data: &[FeatureSample], curvature_data: &[f32]) -> Vec<f32> {
    let window_meters = 20.0;
    let mut smoothed = vec![0.0; curvature_data.len()];

    for i in 0..curvature_data.len() {
        let current_dist = lap_data[i].lap_distance;
        let half_window = window_meters / 2.0;

        let mut start = i;
        while start > 0 && (current_dist - lap_data[start].lap_distance) < half_window {
            start -= 1;
        }
        let mut end = i;
        while end < curvature_data.len() - 1 && (lap_data[end].lap_distance - current_dist) < half_window {
            end += 1;
        }

        let mut sum = 0.0;
        let mut count = 0;
        for j in start..=end {
            sum += curvature_data[j];
            count += 1;
        }
        if count > 0 {
            smoothed[i] = sum / count as f32;
        }
    }
    smoothed
}

// -----------------------------------------------------------------------------
// Heading angle change over a fixed distance window (20 meters)
// -----------------------------------------------------------------------------
pub fn compute_heading_angle(lap_data: &[FeatureSample]) -> Vec<f32> {
    let mut heading_angle_data = vec![0.0; lap_data.len()];
    let window_distance = 20.0; // meters

    for i in 0..lap_data.len() {
        let current_dist = lap_data[i].lap_distance;

        let mut start = i;
        while start > 0 && (current_dist - lap_data[start].lap_distance) < window_distance {
            start -= 1;
        }
        let mut end = i;
        while end < lap_data.len() - 1 && (lap_data[end].lap_distance - current_dist) < window_distance {
            end += 1;
        }

        let mut heading_change = lap_data[end].heading_angle - lap_data[start].heading_angle;
        if heading_change > PI {
            heading_change -= 2.0 * PI;
        } else if heading_change < -PI {
            heading_change += 2.0 * PI;
        }
        heading_angle_data[i] = heading_change;
    }
    heading_angle_data
}

// -----------------------------------------------------------------------------
// Adaptive threshold: 30% of the 95th percentile of absolute curvature
// -----------------------------------------------------------------------------
fn adaptive_corner_threshold(curvature: &[f32]) -> f32 {
    let mut sorted: Vec<f32> = curvature.iter().map(|&c| c.abs()).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = (sorted.len() as f32 * 0.95) as usize;
    let p95 = sorted.get(idx).copied().unwrap_or(0.01);
    (p95 * 0.15).max(0.002) // lower bound 0.002
}

// -----------------------------------------------------------------------------
// Merge corners that are separated by less than 15 meters
// -----------------------------------------------------------------------------
fn merge_close_corners(corners: &mut Vec<TrackCorner>) {
    let merge_dist = 15.0; // meters
    let mut i = 0;
    while i < corners.len().saturating_sub(1) {
        let gap = corners[i + 1].corner_start - corners[i].corner_end;
        if gap < merge_dist {
            // merge i+1 into i
            corners[i].corner_end = corners[i + 1].corner_end;
            if corners[i + 1].peak_curvature > corners[i].peak_curvature {
                corners[i].geometric_apex = corners[i + 1].geometric_apex;
                corners[i].heading_angle_apex = corners[i + 1].heading_angle_apex;
                corners[i].corner_direction = corners[i + 1].corner_direction;
                corners[i].peak_curvature = corners[i + 1].peak_curvature;
            }
            corners.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

// -----------------------------------------------------------------------------
// Main corner detection with hysteresis and adaptive threshold
// -----------------------------------------------------------------------------
pub fn detect_corners(lap_data: &[FeatureSample]) -> Vec<TrackCorner> {
    if lap_data.len() < 10 {
        return Vec::new();
    }

    let curvature_raw = compute_curvature(lap_data);
    let curvature_smooth = smooth_curvature(lap_data, &curvature_raw);
    let heading_angle = compute_heading_angle(lap_data);

    let corner_threshold = adaptive_corner_threshold(&curvature_smooth);
    let exit_hysteresis_dist = 10.0; // meters
    let min_corner_length = 30.0;    // meters
    let min_curvature = 0.003;       // absolute minimum curvature to consider a corner (safety)
    let heading_threshold = 0.10;

    let mut corners = Vec::new();
    let mut in_corner = false;
    let mut corner_start_idx = 0;
    let mut apex_curv_idx = 0;
    let mut apex_heading_idx = 0;
    let mut max_curv = 0.0;
    let mut max_heading = 0.0;
    let mut below_counter_dist = 0.0; // distance below threshold

    for i in 0..curvature_smooth.len() {
        let curv = curvature_smooth[i].abs();
        let heading = heading_angle[i].abs();

        if curv > corner_threshold {
            // Inside a corner
            if !in_corner {
                in_corner = true;
                corner_start_idx = i;
                apex_curv_idx = i;
                apex_heading_idx = i;
                max_curv = curv;
                max_heading = heading;
            } else {
                if curv > max_curv {
                    max_curv = curv;
                    apex_curv_idx = i;
                }
                if heading > max_heading {
                    max_heading = heading;
                    apex_heading_idx = i;
                }
            }
            below_counter_dist = 0.0; // reset exit counter
        } else {
            // Below threshold – check if we should exit
            if in_corner {
                if i == corner_start_idx {
                    below_counter_dist = 0.0;
                } else {
                    let dist_step = lap_data[i].lap_distance - lap_data[i - 1].lap_distance;
                    below_counter_dist += dist_step;
                }
                if below_counter_dist >= exit_hysteresis_dist {
                    // Exit confirmed
                    in_corner = false;
                    let corner_len = lap_data[i].lap_distance - lap_data[corner_start_idx].lap_distance;
                    if corner_len >= min_corner_length
                        && (max_curv > min_curvature || max_heading > heading_threshold)
                    {
                        corners.push(TrackCorner {
                            corner_id: corners.len() as u32,
                            corner_start: lap_data[corner_start_idx].lap_distance,
                            corner_end: lap_data[i].lap_distance,
                            geometric_apex: lap_data[apex_curv_idx].lap_distance,
                            heading_angle_apex: lap_data[apex_heading_idx].lap_distance,
                            peak_curvature: max_curv,
                            corner_direction: if heading_angle[apex_heading_idx] > 0.0 {
                                CornerDirection::Left
                            } else {
                                CornerDirection::Right
                            },
                        });
                    }
                }
            }
        }
    }

    // Handle corner that ends at the lap end
    if in_corner {
        let last_idx = lap_data.len() - 1;
        let corner_len = lap_data[last_idx].lap_distance - lap_data[corner_start_idx].lap_distance;
        if corner_len >= min_corner_length
            && (max_curv > min_curvature || max_heading > heading_threshold)
        {
            corners.push(TrackCorner {
                corner_id: corners.len() as u32,
                corner_start: lap_data[corner_start_idx].lap_distance,
                corner_end: lap_data[last_idx].lap_distance,
                geometric_apex: lap_data[apex_curv_idx].lap_distance,
                heading_angle_apex: lap_data[apex_heading_idx].lap_distance,
                peak_curvature: max_curv,
                corner_direction: if heading_angle[apex_heading_idx] > 0.0 {
                    CornerDirection::Left
                } else {
                    CornerDirection::Right
                },
            });
        }
    }

    // Merge corners that are too close
    merge_close_corners(&mut corners);

    // Re‑number corners after merging
    for (id, corner) in corners.iter_mut().enumerate() {
        corner.corner_id = id as u32;
    }

    corners
}