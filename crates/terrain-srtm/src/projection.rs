use crate::{TerrainError, TerrainMosaic, EARTH_RADIUS_M};
#[derive(Clone, Copy, Debug)]
pub struct LocalProjection {
    lat0: f64,
    lon0: f64,
}
impl LocalProjection {
    pub fn new(lat: f64, lon: f64) -> Result<Self, TerrainError> {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err(TerrainError::Coordinates);
        }
        Ok(Self {
            lat0: lat.to_radians(),
            lon0: lon.to_radians(),
        })
    }
    pub fn crs_description(&self) -> String {
        format!(
            "+proj=aeqd +lat_0={} +lon_0={} +R={EARTH_RADIUS_M} +units=m",
            self.lat0.to_degrees(),
            self.lon0.to_degrees()
        )
    }
    pub fn forward(&self, lat: f64, lon: f64) -> Result<(f64, f64), TerrainError> {
        let lat = lat.to_radians();
        let d = (lon.to_radians() - self.lon0 + std::f64::consts::PI)
            .rem_euclid(std::f64::consts::TAU)
            - std::f64::consts::PI;
        let c = (self.lat0.sin() * lat.sin() + self.lat0.cos() * lat.cos() * d.cos())
            .clamp(-1., 1.)
            .acos();
        if c.abs() < 1e-14 {
            return Ok((0., 0.));
        }
        let k = c / c.sin();
        let x = EARTH_RADIUS_M * k * lat.cos() * d.sin();
        let y = EARTH_RADIUS_M
            * k
            * (self.lat0.cos() * lat.sin() - self.lat0.sin() * lat.cos() * d.cos());
        if x.is_finite() && y.is_finite() {
            Ok((x, y))
        } else {
            Err(TerrainError::Projection)
        }
    }
    pub fn inverse(&self, x: f64, y: f64) -> Result<(f64, f64), TerrainError> {
        let rho = x.hypot(y);
        if rho < 1e-9 {
            return Ok((self.lat0.to_degrees(), self.lon0.to_degrees()));
        }
        let c = rho / EARTH_RADIUS_M;
        let lat = (c.cos() * self.lat0.sin() + y * c.sin() * self.lat0.cos() / rho).asin();
        let lon = self.lon0
            + (x * c.sin()).atan2(rho * self.lat0.cos() * c.cos() - y * self.lat0.sin() * c.sin());
        if lat.is_finite() && lon.is_finite() {
            Ok((
                lat.to_degrees(),
                (lon.to_degrees() + 180.).rem_euclid(360.) - 180.,
            ))
        } else {
            Err(TerrainError::Projection)
        }
    }
}
#[derive(Clone, Debug)]
pub struct MetricRaster {
    pub projection: String,
    pub origin_m: [f64; 2],
    pub resolution_m: f64,
    pub width: usize,
    pub height: usize,
    pub elevations_m: Vec<Option<f32>>,
}
impl MetricRaster {
    pub fn from_mosaic(
        m: &TerrainMosaic,
        p: LocalProjection,
        range: f64,
        resolution: f64,
    ) -> Result<Self, TerrainError> {
        if range < 0. || resolution <= 0. {
            return Err(TerrainError::Projection);
        }
        let radius = (range / resolution).ceil() as usize;
        let width = radius
            .checked_mul(2)
            .and_then(|v| v.checked_add(1))
            .ok_or(TerrainError::Projection)?;
        let mut elevations =
            Vec::with_capacity(width.checked_mul(width).ok_or(TerrainError::Projection)?);
        for row in 0..width {
            let y = (radius as isize - row as isize) as f64 * resolution;
            for col in 0..width {
                let x = (col as isize - radius as isize) as f64 * resolution;
                if x * x + y * y > range * range {
                    elevations.push(None)
                } else {
                    let (lat, lon) = p.inverse(x, y)?;
                    elevations.push(m.sample(lat, lon)?.map(f32::from))
                }
            }
        }
        Ok(Self {
            projection: p.crs_description(),
            origin_m: [-(radius as f64) * resolution; 2],
            resolution_m: resolution,
            width,
            height: width,
            elevations_m: elevations,
        })
    }

    pub fn from_bounds(
        m: &TerrainMosaic,
        p: LocalProjection,
        bounds: [f64; 4],
        coverage_disks: &[[f64; 3]],
        resolution: f64,
        max_cells: usize,
    ) -> Result<Self, TerrainError> {
        if resolution <= 0.
            || bounds[2] < bounds[0]
            || bounds[3] < bounds[1]
            || coverage_disks.is_empty()
            || coverage_disks
                .iter()
                .any(|disk| !disk.iter().all(|value| value.is_finite()) || disk[2] < 0.0)
        {
            return Err(TerrainError::Projection);
        }
        let width = ((bounds[2] - bounds[0]) / resolution).ceil() as usize + 1;
        let height = ((bounds[3] - bounds[1]) / resolution).ceil() as usize + 1;
        let cells = width
            .checked_mul(height)
            .filter(|n| *n <= max_cells)
            .ok_or(TerrainError::Projection)?;
        let mut elevations = Vec::with_capacity(cells);
        for row in 0..height {
            let y = bounds[3] - row as f64 * resolution;
            for col in 0..width {
                let x = bounds[0] + col as f64 * resolution;
                if !coverage_disks.iter().any(|disk| {
                    let dx = x - disk[0];
                    let dy = y - disk[1];
                    dx * dx + dy * dy <= disk[2] * disk[2]
                }) {
                    elevations.push(None);
                    continue;
                }
                let (lat, lon) = p.inverse(x, y)?;
                elevations.push(m.sample(lat, lon)?.map(f32::from));
            }
        }
        Ok(Self {
            projection: p.crs_description(),
            origin_m: [bounds[0], bounds[1]],
            resolution_m: resolution,
            width,
            height,
            elevations_m: elevations,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HgtTile, TileCoordinate};
    use std::sync::Arc;
    #[test]
    fn round_trip() {
        let p = LocalProjection::new(45., 2.).unwrap();
        let (x, y) = p.forward(46., 3.).unwrap();
        let (lat, lon) = p.inverse(x, y).unwrap();
        assert!((lat - 46.).abs() < 1e-9 && (lon - 3.).abs() < 1e-9);
        assert!(x.hypot(y) > 100_000.)
    }
    #[test]
    fn raster_keeps_outside_nodata() {
        let t = Arc::new(HgtTile {
            coordinate: TileCoordinate { lat: 45, lon: 2 },
            dimension: 1201,
            heights: vec![123; 1201 * 1201],
        });
        let m = TerrainMosaic::new([t]);
        let g = MetricRaster::from_mosaic(&m, LocalProjection::new(45.5, 2.5).unwrap(), 100., 90.)
            .unwrap();
        assert_eq!((g.width, g.height), (5, 5));
        assert_eq!(g.elevations_m[12], Some(123.));
        assert_eq!(g.elevations_m[0], None)
    }

    #[test]
    fn bounded_raster_does_not_sample_outside_coverage_disks() {
        let tile = Arc::new(HgtTile {
            coordinate: TileCoordinate { lat: 45, lon: 2 },
            dimension: 1201,
            heights: vec![321; 1201 * 1201],
        });
        let mosaic = TerrainMosaic::new([tile]);
        let projection = LocalProjection::new(45.5, 2.5).unwrap();
        let raster = MetricRaster::from_bounds(
            &mosaic,
            projection,
            [-200_000.0, -200_000.0, 200_000.0, 200_000.0],
            &[[0.0, 0.0, 1.0]],
            200_000.0,
            9,
        )
        .unwrap();
        assert_eq!(raster.elevations_m[raster.width + 1], Some(321.0));
        assert_eq!(raster.elevations_m[0], None);
        assert_eq!(raster.elevations_m[raster.elevations_m.len() - 1], None);
    }
}
