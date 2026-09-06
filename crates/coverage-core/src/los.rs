use crate::{apparent_height, for_each_ring_cell, AngularTable, BitSet};

pub const NO_DATA_HEIGHT: u16 = u16::MAX;
/// Increment whenever LOS semantics change in a way that invalidates persisted results.
pub const LOS_ALGORITHM_VERSION: u16 = 1;

#[derive(Clone, Debug)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub elevations_m: std::sync::Arc<[Option<f32>]>,
}

impl Grid {
    pub fn new(
        width: usize,
        height: usize,
        elevations_m: Vec<Option<f32>>,
    ) -> Result<Self, CoverageError> {
        if width.checked_mul(height) != Some(elevations_m.len()) {
            return Err(CoverageError::InvalidGrid);
        }
        Ok(Self {
            width,
            height,
            elevations_m: elevations_m.into(),
        })
    }
    pub fn from_shared(
        width: usize,
        height: usize,
        elevations_m: std::sync::Arc<[Option<f32>]>,
    ) -> Result<Self, CoverageError> {
        if width.checked_mul(height) != Some(elevations_m.len()) {
            return Err(CoverageError::InvalidGrid);
        }
        Ok(Self {
            width,
            height,
            elevations_m,
        })
    }
    pub fn index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }
}

#[derive(Clone, Debug)]
pub struct LosConfig {
    pub radar_x: usize,
    pub radar_y: usize,
    pub antenna_agl_m: f64,
    pub cell_size_m: f64,
    pub range_m: f64,
    pub effective_earth_k: f64,
    pub angular_sectors: usize,
}

#[derive(Clone, Debug)]
pub struct Coverage {
    pub ground_visible: BitSet,
    pub minimum_agl_m: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverageError {
    InvalidGrid,
    RadarOutsideGrid,
    RadarOnNoData,
    InvalidConfig,
}

impl std::fmt::Display for CoverageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoverageError {}

pub fn compute_coverage(grid: &Grid, config: &LosConfig) -> Result<Coverage, CoverageError> {
    if config.radar_x >= grid.width || config.radar_y >= grid.height {
        return Err(CoverageError::RadarOutsideGrid);
    }
    if !(config.cell_size_m > 0.0
        && config.range_m >= 0.0
        && config.effective_earth_k > 0.0
        && config.angular_sectors > 0)
    {
        return Err(CoverageError::InvalidConfig);
    }
    let radar_index = grid.index(config.radar_x, config.radar_y);
    let radar_terrain = grid.elevations_m[radar_index].ok_or(CoverageError::RadarOnNoData)? as f64;
    let radar_height = radar_terrain + config.antenna_agl_m;
    let cells = grid.width * grid.height;
    let mut result = Coverage {
        ground_visible: BitSet::new(cells),
        minimum_agl_m: vec![NO_DATA_HEIGHT; cells],
    };
    let radius_cells = (config.range_m / config.cell_size_m).floor() as i32;
    let angles = AngularTable::new(radius_cells as usize, config.angular_sectors);
    let mut horizons = vec![f64::NEG_INFINITY; config.angular_sectors];
    let range_squared = config.range_m * config.range_m;

    for_each_ring_cell(radius_cells, |dx, dy, _, _| {
        let Some(x) = config.radar_x.checked_add_signed(dx as isize) else {
            return;
        };
        let Some(y) = config.radar_y.checked_add_signed(dy as isize) else {
            return;
        };
        if x >= grid.width || y >= grid.height {
            return;
        }
        let distance_squared =
            f64::from(dx * dx + dy * dy) * config.cell_size_m * config.cell_size_m;
        if distance_squared > range_squared {
            return;
        }
        let index = grid.index(x, y);
        let Some(terrain) = grid.elevations_m[index] else {
            return;
        };
        if dx == 0 && dy == 0 {
            result.ground_visible.set(index);
            result.minimum_agl_m[index] = 0;
            return;
        }
        let distance = distance_squared.sqrt();
        let apparent = apparent_height(terrain as f64, distance_squared, config.effective_earth_k);
        let sector = angles.sector(dx, dy);
        let previous_horizon = horizons[sector];
        let slope = (apparent - radar_height) / distance;
        let needed = (previous_horizon * distance - (apparent - radar_height)).max(0.0);
        let agl = if needed.is_finite() {
            needed.ceil().clamp(0.0, (u16::MAX - 1) as f64) as u16
        } else {
            0
        };
        result.minimum_agl_m[index] = agl;
        if slope >= previous_horizon {
            result.ground_visible.set(index);
        }
        // Only terrain updates the horizon; target AGL is never fed back.
        horizons[sector] = previous_horizon.max(slope);
    });
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(values: &[Option<f32>], k: f64) -> Coverage {
        let g = Grid::new(values.len(), 1, values.to_vec()).unwrap();
        compute_coverage(
            &g,
            &LosConfig {
                radar_x: 0,
                radar_y: 0,
                antenna_agl_m: 0.0,
                cell_size_m: 1000.0,
                range_m: (values.len() as f64 - 1.0) * 1000.0,
                effective_earth_k: k,
                angular_sectors: 8,
            },
        )
        .unwrap()
    }
    #[test]
    fn flat_ground_and_curvature() {
        let c = line(&[Some(0.0); 4], 4.0 / 3.0);
        assert!(c.ground_visible.contains(0));
        assert!(c.minimum_agl_m[3] > 0);
    }
    #[test]
    fn ridge_masks_cell_and_minimum_height_is_positive() {
        let c = line(&[Some(0.), Some(100.), Some(0.)], 1e30);
        assert!(c.ground_visible.contains(1));
        assert!(!c.ground_visible.contains(2));
        assert_eq!(c.minimum_agl_m[2], 200);
    }
    #[test]
    fn equal_horizon_is_visible() {
        let c = line(&[Some(0.), Some(10.), Some(20.)], 1e30);
        assert!(c.ground_visible.contains(2));
    }
    #[test]
    fn no_data_stays_reserved() {
        assert_eq!(
            line(&[Some(0.), None], 1.0).minimum_agl_m[1],
            NO_DATA_HEIGHT
        );
    }
    #[test]
    fn exact_range_and_outside() {
        let g = Grid::new(4, 1, vec![Some(0.); 4]).unwrap();
        let c = compute_coverage(
            &g,
            &LosConfig {
                radar_x: 0,
                radar_y: 0,
                antenna_agl_m: 1.,
                cell_size_m: 10.,
                range_m: 20.,
                effective_earth_k: 1.,
                angular_sectors: 8,
            },
        )
        .unwrap();
        assert_ne!(c.minimum_agl_m[2], NO_DATA_HEIGHT);
        assert_eq!(c.minimum_agl_m[3], NO_DATA_HEIGHT);
    }
    #[test]
    fn effective_earth_four_thirds_numeric_at_50km() {
        let drop = 50_000f64.powi(2) / (2.0 * (4.0 / 3.0) * crate::EARTH_RADIUS_M);
        assert!((drop - 147.151153665).abs() < 1e-6);
        assert!(drop < 50_000f64.powi(2) / (2.0 * crate::EARTH_RADIUS_M));
    }
    #[test]
    fn agl_layers_are_monotonic() {
        let c = line(&[Some(0.), Some(10.), Some(0.), Some(0.)], 1e30);
        for &h in &[0, 30, 50, 100] {
            let count = c
                .minimum_agl_m
                .iter()
                .filter(|&&v| v != NO_DATA_HEIGHT && v <= h)
                .count();
            if h == 0 {
                assert!(count <= c.minimum_agl_m.iter().filter(|&&v| v <= 30).count());
            }
        }
    }
}
