use crate::{apparent_height, CoverageError, Grid, LosConfig};

#[derive(Clone, Debug)]
pub struct ProfilePoint {
    pub distance_m: f64,
    pub terrain_m: f64,
    pub apparent_terrain_m: f64,
    pub los_height_m: f64,
    pub horizon_slope: f64,
    pub target_height_m: f64,
    pub is_obstruction: bool,
    pub vertical_margin_m: f64,
}

#[derive(Clone, Debug)]
pub struct ProfileResult {
    pub points: Vec<ProfilePoint>,
    pub first_obstacle_distance_m: Option<f64>,
    pub visible: bool,
}

pub fn compute_profile(
    grid: &Grid,
    config: &LosConfig,
    target_x: usize,
    target_y: usize,
    target_agl_m: f64,
) -> Result<ProfileResult, CoverageError> {
    if config.radar_x >= grid.width
        || config.radar_y >= grid.height
        || target_x >= grid.width
        || target_y >= grid.height
    {
        return Err(CoverageError::RadarOutsideGrid);
    }
    let radar_ground = grid.elevations_m[grid.index(config.radar_x, config.radar_y)]
        .ok_or(CoverageError::RadarOnNoData)? as f64;
    let target_ground =
        grid.elevations_m[grid.index(target_x, target_y)].ok_or(CoverageError::InvalidGrid)? as f64;
    let radar_h = radar_ground + config.antenna_agl_m;
    let dx = target_x as isize - config.radar_x as isize;
    let dy = target_y as isize - config.radar_y as isize;
    let steps = dx.unsigned_abs().max(dy.unsigned_abs());
    if steps == 0 {
        return Ok(ProfileResult {
            points: Vec::new(),
            first_obstacle_distance_m: None,
            visible: true,
        });
    }
    let total_d2 = ((dx * dx + dy * dy) as f64) * config.cell_size_m.powi(2);
    let total_d = total_d2.sqrt();
    let target_apparent =
        apparent_height(target_ground, total_d2, config.effective_earth_k) + target_agl_m;
    let mut points = Vec::with_capacity(steps + 1);
    let mut horizon = f64::NEG_INFINITY;
    let mut first = None;
    for step in 1..=steps {
        let fraction = step as f64 / steps as f64;
        let x = (config.radar_x as f64 + dx as f64 * fraction).round() as usize;
        let y = (config.radar_y as f64 + dy as f64 * fraction).round() as usize;
        let Some(terrain) = grid.elevations_m[grid.index(x, y)] else {
            continue;
        };
        let distance = total_d * fraction;
        let d2 = distance * distance;
        let apparent = apparent_height(terrain as f64, d2, config.effective_earth_k);
        let los_height = radar_h + (target_apparent - radar_h) * fraction;
        let margin = los_height - apparent;
        let obstruction = step < steps && margin < 0.0;
        if obstruction && first.is_none() {
            first = Some(distance);
        }
        let slope = (apparent - radar_h) / distance;
        points.push(ProfilePoint {
            distance_m: distance,
            terrain_m: terrain as f64,
            apparent_terrain_m: apparent,
            los_height_m: los_height,
            horizon_slope: horizon,
            target_height_m: target_apparent,
            is_obstruction: obstruction,
            vertical_margin_m: margin,
        });
        horizon = horizon.max(slope);
    }
    Ok(ProfileResult {
        points,
        first_obstacle_distance_m: first,
        visible: first.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ridge_obstructs_profile() {
        let grid = Grid::new(3, 1, vec![Some(0.), Some(100.), Some(0.)]).unwrap();
        let cfg = LosConfig {
            radar_x: 0,
            radar_y: 0,
            antenna_agl_m: 0.,
            cell_size_m: 1000.,
            range_m: 2000.,
            effective_earth_k: 1e30,
        };
        let p = compute_profile(&grid, &cfg, 2, 0, 0.).unwrap();
        assert!(!p.visible);
        assert_eq!(p.first_obstacle_distance_m, Some(1000.));
    }
}
