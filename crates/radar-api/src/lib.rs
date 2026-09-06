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
pub struct FusionResponse {
    pub fusion_id: Uuid,
    pub selected_radars: Vec<Uuid>,
    pub target_agl_m: u16,
    pub width: u32,
    pub height: u32,
    pub dataset_url: String,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobRequest {
    pub radars: Vec<Radar>,
    pub resolution_m: u16,
    pub effective_earth_k: f64,
    pub target_heights_agl_m: Vec<u16>,
}
impl JobRequest {
    pub fn validate(&self, max_radars: usize, max_cells: u64) -> Result<JobEstimate, &'static str> {
        if self.radars.is_empty() || self.radars.len() > max_radars {
            return Err("radar count");
        };
        if !matches!(self.resolution_m, 30 | 90 | 180) {
            return Err("resolution");
        };
        if !(1.0..=2.0).contains(&self.effective_earth_k) {
            return Err("effective earth k");
        };
        for radar in &self.radars {
            radar.validate()?
        }
        let max_range = self.radars.iter().map(|r| r.range_m).fold(0.0, f64::max);
        let radius = (max_range / self.resolution_m as f64).ceil() as u64;
        let side = radius
            .checked_mul(2)
            .and_then(|v| v.checked_add(1))
            .ok_or("grid size")?;
        let cells = side.checked_mul(side).ok_or("grid size")?;
        if cells > max_cells {
            return Err("grid cell limit");
        };
        let bytes_per_radar = cells.checked_mul(3).ok_or("memory estimate")?;
        Ok(JobEstimate {
            width: side,
            height: side,
            cells,
            bytes_per_radar,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobEstimate {
    pub width: u64,
    pub height: u64,
    pub cells: u64,
    pub bytes_per_radar: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobStatus {
    pub id: Uuid,
    pub state: JobState,
    pub progress: f32,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub estimate: JobEstimate,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn radar(range: f64) -> Radar {
        Radar {
            id: Uuid::nil(),
            name: "r".into(),
            latitude: 45.,
            longitude: 2.,
            antenna_agl_m: 10.,
            range_m: range,
            active: true,
        }
    }
    #[test]
    fn validates_expected_400km_grid() {
        let r = JobRequest {
            radars: vec![radar(400_000.)],
            resolution_m: 90,
            effective_earth_k: 4. / 3.,
            target_heights_agl_m: vec![30, 50, 100],
        };
        let e = r.validate(8, 100_000_000).unwrap();
        assert_eq!((e.width, e.height, e.cells), (8891, 8891, 79_049_881));
    }
    #[test]
    fn rejects_bad_resolution_and_limits() {
        let mut r = JobRequest {
            radars: vec![radar(400_000.)],
            resolution_m: 42,
            effective_earth_k: 4. / 3.,
            target_heights_agl_m: vec![],
        };
        assert!(r.validate(8, u64::MAX).is_err());
        r.resolution_m = 30;
        assert!(r.validate(8, 100).is_err())
    }
}
