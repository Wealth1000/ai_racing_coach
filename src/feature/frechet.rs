use crate::feature::RawLapData;

pub fn calculate_euclidean_distance(world_position_1: [f32; 3], world_position_2: [f32; 3]) -> f32 {
    let raw_distance_x = world_position_2[0] - world_position_1[0];
    //let raw_distance_y = world_position_2[1] - world_position_1[1]; Will be included later.
    let raw_distance_z = world_position_2[2] - world_position_1[2];
    let squared_distance = (raw_distance_x * raw_distance_x) + (raw_distance_z * raw_distance_z);
    let result = squared_distance.sqrt();
    result
}

pub fn calculate_frechet_distance(path_1: &[[f32; 3]], path_2: &[[f32; 3]]) -> f32 {
    let mut result = 0.0;
    let mut double_path_table = vec![vec![0.0_f32; path_2.len()]; path_1.len()];
    double_path_table[0][0] = calculate_euclidean_distance(path_1[0], path_2[0]);
    for i in 1..path_1.len() {
        double_path_table[i][0] =
            double_path_table[i - 1][0].max(calculate_euclidean_distance(path_1[i], path_2[0]));
    }
    for j in 1..path_2.len() {
        double_path_table[0][j] =
            double_path_table[0][j - 1].max(calculate_euclidean_distance(path_1[0], path_2[j]));
    }
    for i in 1..path_1.len() {
        for j in 1..path_2.len() {
            double_path_table[i][j] = double_path_table[i - 1][j]
                .min(double_path_table[i][j - 1])
                .min(double_path_table[i-1][j-1]);

            double_path_table[i][j] =
                double_path_table[i][j].max(calculate_euclidean_distance(path_1[i], path_2[j]));
        }
    }

    result = double_path_table[path_1.len() - 1][path_2.len() - 1];
    result
}

pub fn find_lowest_frechet_distance_average(all_laps: &[&RawLapData]) -> usize {
    let mut result = 0;
    let mut lowest_average = f32::INFINITY;
    let mut world_positions = Vec::<Vec<[f32; 3]>>::new();
    for lap in all_laps {
        let world_position = lap.transform_to_world_position();
        world_positions.push(world_position);
    }

    for i in 0..all_laps.len() {
        let mut sum = 0.0;
        for j in 0..all_laps.len() {
            if j != i {
                sum += calculate_frechet_distance(&world_positions[i].as_slice(), &world_positions[j].as_slice());
            }
        }
        let average = sum / (all_laps.len() - 1) as f32;
        if average < lowest_average {
            lowest_average = average;
            result = i;
        }
    }

    result
}