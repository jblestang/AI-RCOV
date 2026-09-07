use crate::{apparent_height, BitSet};
use std::f64::consts::TAU;
#[cfg(feature = "rayon")]
use std::sync::atomic::{AtomicU16, Ordering};

#[cfg(feature = "rayon")]
use rayon::prelude::*;

pub const NO_DATA_HEIGHT: u16 = u16::MAX;
/// Increment whenever LOS semantics change in a way that invalidates persisted results.
pub const LOS_ALGORITHM_VERSION: u16 = 3;

#[derive(Clone, Copy)]
struct PolarRay {
    angle: f64,
    width: f64,
    horizon: f64,
    last_distance_m: f64,
}

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
        #[cfg(feature = "rayon")]
        minimum_agl_m: Vec::new(),
        #[cfg(not(feature = "rayon"))]
        minimum_agl_m: vec![NO_DATA_HEIGHT; cells],
    };
    let initial_rays = 8;
    #[cfg(feature = "rayon")]
    {
        // Zero means unvisited; computed AGL is stored as value + 1. Atomic
        // max makes overlapping boundary projections deterministic without a
        // full result grid per worker.
        let minimum = (0..cells).map(|_| AtomicU16::new(0)).collect::<Vec<_>>();
        minimum[radar_index].store(1, Ordering::Relaxed);
        (0..initial_rays).into_par_iter().for_each(|sector| {
            walk_polar_sector(
                grid,
                config,
                radar_height,
                sector,
                initial_rays,
                |index, agl| {
                    minimum[index].fetch_max(agl.saturating_add(1), Ordering::Relaxed);
                },
            );
        });
        result.minimum_agl_m = minimum
            .into_iter()
            .map(|value| match value.into_inner() {
                0 => NO_DATA_HEIGHT,
                stored => stored - 1,
            })
            .collect();
    }
    #[cfg(not(feature = "rayon"))]
    {
        result.minimum_agl_m[radar_index] = 0;
        for sector in 0..initial_rays {
            walk_polar_sector(
                grid,
                config,
                radar_height,
                sector,
                initial_rays,
                |index, agl| {
                    let stored = &mut result.minimum_agl_m[index];
                    *stored = if *stored == NO_DATA_HEIGHT {
                        agl
                    } else {
                        (*stored).max(agl)
                    };
                },
            );
        }
    }
    for (index, minimum) in result.minimum_agl_m.iter().enumerate() {
        if *minimum == 0 {
            result.ground_visible.set(index);
        }
    }
    Ok(result)
}

fn walk_polar_sector(
    grid: &Grid,
    config: &LosConfig,
    radar_height: f64,
    sector: usize,
    sector_count: usize,
    mut visit: impl FnMut(usize, u16),
) {
    let initial_width = TAU / sector_count as f64;
    let mut rays = vec![PolarRay {
        angle: initial_width * sector as f64,
        width: initial_width,
        horizon: f64::NEG_INFINITY,
        last_distance_m: 0.0,
    }];
    let radius_cells = (config.range_m / config.cell_size_m).floor() as usize;
    let range_squared = config.range_m * config.range_m;
    for radial_cell in 1..=radius_cells {
        let radius_m = radial_cell as f64 * config.cell_size_m;
        subdivide_rays(&mut rays, radius_m, config.cell_size_m);
        for ray in &mut rays {
            let dx = (radial_cell as f64 * ray.angle.cos()).round() as i32;
            let dy = (radial_cell as f64 * ray.angle.sin()).round() as i32;
            let Some(x) = config.radar_x.checked_add_signed(dx as isize) else {
                continue;
            };
            let Some(y) = config.radar_y.checked_add_signed(dy as isize) else {
                continue;
            };
            if x >= grid.width || y >= grid.height {
                continue;
            }
            let distance_squared_cells =
                i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy);
            let distance_squared =
                distance_squared_cells as f64 * config.cell_size_m * config.cell_size_m;
            if distance_squared > range_squared {
                continue;
            }
            let distance = distance_squared.sqrt();
            // Rounding a polar sample to the nearest Cartesian centre can
            // occasionally produce the same or a nearer centre at the next
            // radial step. Horizons must remain strictly distance ordered.
            if distance <= ray.last_distance_m {
                continue;
            }
            ray.last_distance_m = distance;
            let index = grid.index(x, y);
            let Some(terrain) = grid.elevations_m[index] else {
                continue;
            };
            let apparent =
                apparent_height(terrain as f64, distance_squared, config.effective_earth_k);
            let slope = (apparent - radar_height) / distance;
            let needed = (ray.horizon * distance - (apparent - radar_height)).max(0.0);
            let agl = if needed.is_finite() {
                needed.ceil().clamp(0.0, (u16::MAX - 1) as f64) as u16
            } else {
                0
            };
            visit(index, agl);
            // Only terrain updates the horizon; target AGL is never fed back.
            ray.horizon = ray.horizon.max(slope);
        }
    }
}

/// Number of adaptive polar sectors at maximum range.
///
/// Sectors are repeatedly bisected, so their transverse width `r * d_theta`
/// never exceeds half a Cartesian grid cell. The half-cell margin, combined
/// with a one-cell radial step, ensures nearest-cell projection covers square
/// cell centres at every bearing.
pub fn dynamic_ray_count(range_m: f64, cell_size_m: f64) -> usize {
    if range_m <= 0.0 || cell_size_m <= 0.0 {
        return 8;
    }
    let mut rays = 8usize;
    while range_m * (TAU / rays as f64) > cell_size_m * 0.5 {
        let Some(next) = rays.checked_mul(2) else {
            return usize::MAX;
        };
        rays = next;
    }
    rays
}

fn subdivide_rays(rays: &mut Vec<PolarRay>, radius_m: f64, cell_size_m: f64) {
    while radius_m * rays[0].width > cell_size_m * 0.5 {
        let mut children = Vec::with_capacity(rays.len() * 2);
        for ray in rays.drain(..) {
            let child_width = ray.width * 0.5;
            let offset = child_width * 0.5;
            children.push(PolarRay {
                angle: (ray.angle - offset).rem_euclid(TAU),
                width: child_width,
                horizon: ray.horizon,
                last_distance_m: ray.last_distance_m,
            });
            children.push(PolarRay {
                angle: (ray.angle + offset).rem_euclid(TAU),
                width: child_width,
                horizon: ray.horizon,
                last_distance_m: ray.last_distance_m,
            });
        }
        *rays = children;
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
        assert_eq!(fine, 65_536);
        assert!(100_000.0 * TAU / fine as f64 <= 15.0);
        assert!(100_000.0 * TAU / (fine / 2) as f64 > 15.0);
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
