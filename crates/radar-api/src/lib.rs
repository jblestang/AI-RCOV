use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Radar {
    pub id: Uuid,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub antenna_agl_m: f64,
    pub range_m: f64,
    pub active: bool,
}
impl Radar {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(-90.0..=90.0).contains(&self.latitude) {
            return Err("latitude");
        };
        if !(-180.0..=180.0).contains(&self.longitude) {
            return Err("longitude");
        };
        if !(0.0..=400_000.0).contains(&self.range_m) {
            return Err("range");
        };
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FusionRequest {
    pub radar_ids: Vec<Uuid>,
    pub target_agl_m: u16,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}
