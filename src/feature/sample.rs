use serde::Deserialize;
use crate::telemetry::frame::TelemetryFrame;


#[derive(Clone, Debug, Deserialize)]
pub struct FrameSampler{
    pub prev_frame: Option<TelemetryFrame>,
}

impl FrameSampler{
    pub fn process_frames(&mut self, current_frame: &TelemetryFrame) -> Option<FeatureSample> {
        match &self.prev_frame {
            None => {
                // First frame: store it, but don’t emit anything
                self.prev_frame = Some(current_frame.clone());
                None
            }
            Some(prev) => {
                // Compare with previous
                if prev.Timestamp == current_frame.Timestamp {
                    return None;
                }
    
                // Not a duplicate → produce sample
                let sample = FeatureSample::from_frame(current_frame);
    
                // Update state
                self.prev_frame = Some(current_frame.clone());
    
                Some(sample)
            }
        }
    }

    pub fn continuous_sampling(&mut self, frame: &TelemetryFrame) -> Option<FeatureSample>{
        if !FeatureSample::is_valid(frame){
            return None;
        }
        let result =self.process_frames(frame);
        result
    }
}

#[derive(Debug, Deserialize)]
pub struct FeatureSample{
    pub timestamp: i64,
    pub speed: f32,
    pub throttle: f32,
    pub brake: f32,
    pub steering: f32,
    pub yaw_rate: f32,
    pub lap: u32,
    pub lap_distance: f32,
    pub normalised_lap_distance: f32,
    pub world_position: [f32; 3],
    pub heading_angle: f32,
}

impl FeatureSample{
    pub fn from_frame(frame: &TelemetryFrame) -> Self{
        Self{
            timestamp: frame.Timestamp,
            speed: frame.Speed,
            throttle: frame.Throttle,
            brake: frame.Brake,
            steering: frame.Steering,
            yaw_rate: frame.AngularVelocity[1],
            lap: frame.Viewed.CurrentLap,
            lap_distance: frame.Viewed.CurrentLapDistance,
            normalised_lap_distance: frame.Viewed.CurrentLapDistance / frame.TrackLength,
            world_position: frame.Viewed.WorldPosition,
            heading_angle: frame.Orientation[1],

        }
    }

    pub fn is_valid(frame: &TelemetryFrame) -> bool {
        if (frame.GameState == 2 && frame.PitMode == 0 && frame.CrashState == 0 && frame.Viewed.CurrentLapDistance > 0.0){
            return true;
        }
        return false;
    }
}  


pub struct RawLapData{
    pub lap_number: u32,
    pub data: Vec<FeatureSample>,
}

impl RawLapData{
    pub fn from_feature_samples(samples: Vec<FeatureSample>) -> Vec<RawLapData>{
        if samples.is_empty(){
            return Vec::<RawLapData>::new();
        }

        let mut accumulated_lap = Vec::<RawLapData>::new();
        let mut lap_data = Vec::<FeatureSample>::new();
        let mut current_lap_number = samples[0].lap;

        for sample in samples{
            if sample.lap != current_lap_number{
                accumulated_lap.push(RawLapData{
                    lap_number: current_lap_number,
                    data: lap_data,
                });
                lap_data = Vec::<FeatureSample>::new();
                current_lap_number = sample.lap;
            }
            lap_data.push(sample);
        }

        accumulated_lap.push(RawLapData{
            lap_number: current_lap_number,
            data: lap_data,
        });
        accumulated_lap
    }

    pub fn transform_to_world_position(&self) -> Vec<[f32; 3]>{
        let mut result = Vec::<[f32; 3]>::new();
        for sample in &self.data{
            result.push(sample.world_position);
        }
        result
    }
}