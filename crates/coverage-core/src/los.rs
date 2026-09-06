use crate::{apparent_height, BitSet};
use std::f64::consts::TAU;

pub const NO_DATA_HEIGHT: u16 = u16::MAX;
/// Increment whenever LOS semantics change in a way that invalidates persisted results.
pub const LOS_ALGORITHM_VERSION: u16 = 2;

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
    if !(config.cell_size_m > 0.0 && config.range_m >= 0.0 && config.effective_earth_k > 0.0) {
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
    let range_squared = config.range_m * config.range_m;
    result.minimum_agl_m[radar_index] = 0;
    let ray_count = dynamic_ray_count(config.range_m, config.cell_size_m);
    for ray in 0..ray_count {
        let angle = TAU * ray as f64 / ray_count as f64;
        let mut horizon = f64::NEG_INFINITY;
        for_each_ray_cell(radius_cells, angle, |dx, dy| {
            let Some(x) = config.radar_x.checked_add_signed(dx as isize) else {
                return;
            };
            let Some(y) = config.radar_y.checked_add_signed(dy as isize) else {
                return;
            };
            if x >= grid.width || y >= grid.height {
                return;
            }
            let distance_squared_cells =
                i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy);
            let distance_squared =
                distance_squared_cells as f64 * config.cell_size_m * config.cell_size_m;
            if distance_squared > range_squared {
                return;
            }
            let index = grid.index(x, y);
            let Some(terrain) = grid.elevations_m[index] else {
                return;
            };
            let distance = distance_squared.sqrt();
            let apparent =
                apparent_height(terrain as f64, distance_squared, config.effective_earth_k);
            let slope = (apparent - radar_height) / distance;
            let needed = (horizon * distance - (apparent - radar_height)).max(0.0);
            let agl = if needed.is_finite() {
                needed.ceil().clamp(0.0, (u16::MAX - 1) as f64) as u16
            } else {
                0
            };
            let stored = &mut result.minimum_agl_m[index];
            *stored = if *stored == NO_DATA_HEIGHT {
                agl
            } else {
                (*stored).max(agl)
            };
            // Only terrain updates the horizon; target AGL is never fed back.
            horizon = horizon.max(slope);
        });
    }
    for (index, minimum) in result.minimum_agl_m.iter().enumerate() {
        if *minimum == 0 {
            result.ground_visible.set(index);
        }
    }
    Ok(result)
}

/// Chooses rays so their separation at maximum range is no wider than one cell.
pub fn dynamic_ray_count(range_m: f64, cell_size_m: f64) -> usize {
    if range_m <= 0.0 || cell_size_m <= 0.0 {
        return 8;
    }
    let angular_step = 2.0 * (0.5 * cell_size_m / range_m).atan();
    (TAU / angular_step).ceil().max(8.0) as usize
}

/// Traverses every cell intersected by a ray, from the radar to the range edge.
fn for_each_ray_cell(radius_cells: i32, angle: f64, mut visit: impl FnMut(i32, i32)) {
    if radius_cells <= 0 {
        return;
    }
    let direction_x = angle.cos();
    let direction_y = angle.sin();
    let step_x = if direction_x >= 0.0 { 1 } else { -1 };
    let step_y = if direction_y >= 0.0 { 1 } else { -1 };
    let delta_x = if direction_x.abs() < f64::EPSILON {
        f64::INFINITY
    } else {
        direction_x.abs().recip()
    };
    let delta_y = if direction_y.abs() < f64::EPSILON {
        f64::INFINITY
    } else {
        direction_y.abs().recip()
    };
    let mut boundary_x = 0.5 * delta_x;
    let mut boundary_y = 0.5 * delta_y;
    let mut x = 0i32;
    let mut y = 0i32;
    let traversal_limit = radius_cells as f64 + std::f64::consts::SQRT_2;
    while boundary_x.min(boundary_y) <= traversal_limit {
        if boundary_x < boundary_y {
            x += step_x;
            boundary_x += delta_x;
        } else if boundary_y < boundary_x {
            y += step_y;
            boundary_y += delta_y;
        } else {
            x += step_x;
            y += step_y;
            boundary_x += delta_x;
            boundary_y += delta_y;
        }
        if i64::from(x) * i64::from(x) + i64::from(y) * i64::from(y)
            <= i64::from(radius_cells) * i64::from(radius_cells)
        {
            visit(x, y);
        }
    }
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

    #[test]
    fn dynamic_rays_scale_with_range_over_resolution() {
        assert_eq!(dynamic_ray_count(0.0, 30.0), 8);
        let coarse = dynamic_ray_count(100_000.0, 90.0);
        let fine = dynamic_ray_count(100_000.0, 30.0);
        assert!(fine > coarse * 2);
        assert!((20_940..=20_950).contains(&fine));
    }

    #[test]
    fn ray_traversal_covers_flat_disk_without_spokes() {
        let radius = 32usize;
        let side = radius * 2 + 1;
        let grid = Grid::new(side, side, vec![Some(0.0); side * side]).unwrap();
        let coverage = compute_coverage(
            &grid,
            &LosConfig {
                radar_x: radius,
                radar_y: radius,
                antenna_agl_m: 1000.0,
                cell_size_m: 1.0,
                range_m: radius as f64,
                effective_earth_k: 1e30,
            },
        )
        .unwrap();
        for y in 0..side {
            for x in 0..side {
                let dx = x.abs_diff(radius);
                let dy = y.abs_diff(radius);
                if dx * dx + dy * dy <= radius * radius {
                    assert!(
                        coverage.ground_visible.contains(grid.index(x, y)),
                        "{x},{y}"
                    );
                }
            }
        }
    }
}
