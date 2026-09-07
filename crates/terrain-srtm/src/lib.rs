use std::{collections::BTreeMap, io::Read, sync::Arc};
mod cache;
mod projection;
pub use cache::{SrtmCache, SrtmCacheConfig};
pub use projection::{LocalProjection, MetricRaster};
pub const VOID: i16 = -32768;
/// Surface-height semantics used by LOS and persistent terrain fingerprints.
pub const TERRAIN_MODEL_VERSION: u16 = 2;
const EARTH_RADIUS_M: f64 = 6_371_000.0;
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TileCoordinate {
    pub lat: i16,
    pub lon: i16,
}

/// Returns every one-degree SRTM tile intersecting a conservative geodesic
/// bounding box around a radar. This is preparation only; LOS loops operate on
/// the subsequently projected metric grid.
pub fn tiles_for_radius(
    latitude: f64,
    longitude: f64,
    range_m: f64,
) -> Result<Vec<TileCoordinate>, TerrainError> {
    if !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || !(0.0..=1_000_000.0).contains(&range_m)
    {
        return Err(TerrainError::Coordinates);
    }
    let angular = range_m / EARTH_RADIUS_M;
    let min_lat = (latitude.to_radians() - angular).max(-std::f64::consts::FRAC_PI_2);
    let max_lat = (latitude.to_radians() + angular).min(std::f64::consts::FRAC_PI_2);
    let touches_pole =
        min_lat <= -std::f64::consts::FRAC_PI_2 || max_lat >= std::f64::consts::FRAC_PI_2;
    let lon_delta = if touches_pole {
        std::f64::consts::PI
    } else {
        (angular.sin() / latitude.to_radians().cos().abs())
            .clamp(-1.0, 1.0)
            .asin()
    };
    let min_lat_tile = min_lat.to_degrees().floor() as i16;
    let max_lat_tile = max_lat.to_degrees().floor().min(89.0) as i16;
    let mut result = Vec::new();
    for lat in min_lat_tile..=max_lat_tile {
        if lon_delta >= std::f64::consts::PI {
            for lon in -180..=179 {
                result.push(TileCoordinate { lat, lon });
            }
        } else {
            let west = longitude.to_radians() - lon_delta;
            let east = longitude.to_radians() + lon_delta;
            for lon in -180..=179 {
                let center = (lon as f64 + 0.5).to_radians();
                let relative = (center - longitude.to_radians() + std::f64::consts::PI)
                    .rem_euclid(std::f64::consts::TAU)
                    - std::f64::consts::PI;
                let half_tile = 0.5f64.to_radians();
                if relative >= west - longitude.to_radians() - half_tile
                    && relative <= east - longitude.to_radians() + half_tile
                {
                    result.push(TileCoordinate { lat, lon });
                }
            }
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}
#[derive(Debug)]
pub struct HgtTile {
    pub coordinate: TileCoordinate,
    pub dimension: usize,
    pub heights: Vec<i16>,
}
#[derive(Debug, Default)]
pub struct TerrainMosaic {
    tiles: BTreeMap<TileCoordinate, Arc<HgtTile>>,
}
#[derive(Debug, thiserror::Error)]
pub enum TerrainError {
    #[error("invalid HGT byte length")]
    InvalidLength,
    #[error("unsupported HGT dimension")]
    Dimension,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("missing terrain tile {0:?}")]
    Missing(TileCoordinate),
    #[error("network: {0}")]
    Network(String),
    #[error("download exceeded configured size")]
    TooLarge,
    #[error("invalid geographic coordinates or range")]
    Coordinates,
    #[error("projection failed")]
    Projection,
}
impl HgtTile {
    pub fn decode(coordinate: TileCoordinate, bytes: &[u8]) -> Result<Self, TerrainError> {
        let samples = bytes.len() / 2;
        if samples * 2 != bytes.len() {
            return Err(TerrainError::InvalidLength);
        }
        let dimension = (samples as f64).sqrt() as usize;
        if dimension * dimension != samples || !matches!(dimension, 1201 | 3601) {
            return Err(TerrainError::Dimension);
        }
        let heights = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| i16::from_be_bytes(*v))
            .collect();
        Ok(Self {
            coordinate,
            dimension,
            heights,
        })
    }
    pub fn decode_gzip(
        coordinate: TileCoordinate,
        reader: impl Read,
    ) -> Result<Self, TerrainError> {
        let decoder = flate2::read::GzDecoder::new(reader);
        let mut bytes = Vec::new();
        decoder.take(30_000_000).read_to_end(&mut bytes)?;
        Self::decode(coordinate, &bytes)
    }
    pub fn sample(&self, row: usize, col: usize) -> Option<i16> {
        self.heights
            .get(row.checked_mul(self.dimension)?.checked_add(col)?)
            .copied()
            .filter(|v| *v != VOID)
    }
}
impl TerrainMosaic {
    pub fn new(tiles: impl IntoIterator<Item = Arc<HgtTile>>) -> Self {
        Self {
            tiles: tiles.into_iter().map(|t| (t.coordinate, t)).collect(),
        }
    }
    pub fn tile(&self, c: TileCoordinate) -> Result<&HgtTile, TerrainError> {
        self.tiles
            .get(&c)
            .map(Arc::as_ref)
            .ok_or(TerrainError::Missing(c))
    }
    pub fn len(&self) -> usize {
        self.tiles.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
    pub fn sample(&self, latitude: f64, longitude: f64) -> Result<Option<i16>, TerrainError> {
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return Err(TerrainError::Coordinates);
        }
        let lat_tile = latitude.floor().min(89.0) as i16;
        let lon_tile = if longitude == 180.0 {
            179
        } else {
            longitude.floor() as i16
        };
        let tile = self.tile(TileCoordinate {
            lat: lat_tile,
            lon: lon_tile,
        })?;
        let scale = (tile.dimension - 1) as f64;
        let row = ((1.0 - (latitude - f64::from(lat_tile))) * scale).round() as usize;
        let col = ((longitude - f64::from(lon_tile)) * scale).round() as usize;
        // Skadi includes bathymetry in ocean cells. A terrestrial viewshed
        // follows the water surface, not the sea floor.
        Ok(tile
            .sample(row.min(tile.dimension - 1), col.min(tile.dimension - 1))
            .map(|height| height.max(0)))
    }
    /// Deterministic hash of tile coordinates, dimensions and every decoded
    /// elevation sample. Tile order is stable because the mosaic uses BTreeMap.
    pub fn content_hash(&self) -> String {
        let mut hash = blake3::Hasher::new();
        hash.update(&TERRAIN_MODEL_VERSION.to_le_bytes());
        for (coordinate, tile) in &self.tiles {
            hash.update(&coordinate.lat.to_le_bytes());
            hash.update(&coordinate.lon.to_le_bytes());
            hash.update(&(tile.dimension as u64).to_le_bytes());
            for height in &tile.heights {
                hash.update(&height.to_le_bytes());
            }
        }
        hash.finalize().to_hex().to_string()
    }
}
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<HgtTile>();
    assert_send_sync::<TerrainMosaic>();
};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_is_explicit() {
        assert!(matches!(
            TerrainMosaic::default().tile(TileCoordinate { lat: 0, lon: 0 }),
            Err(TerrainError::Missing(_))
        ))
    }
    #[test]
    fn concurrent_reads() {
        let t = Arc::new(HgtTile {
            coordinate: TileCoordinate { lat: 0, lon: 0 },
            dimension: 1201,
            heights: vec![1; 1201 * 1201],
        });
        let m = Arc::new(TerrainMosaic::new([t]));
        let hs = (0..4)
            .map(|_| {
                let m = m.clone();
                std::thread::spawn(move || {
                    m.tile(TileCoordinate { lat: 0, lon: 0 })
                        .unwrap()
                        .sample(1, 1)
                })
            })
            .collect::<Vec<_>>();
        for h in hs {
            assert_eq!(h.join().unwrap(), Some(1));
        }
    }
    #[test]
    fn radius_loads_multiple_tiles_and_crosses_dateline() {
        let local = tiles_for_radius(45.0, 2.0, 400_000.0).unwrap();
        assert!(local.len() > 1);
        assert!(local.iter().any(|c| c.lat == 41));
        assert!(local.iter().any(|c| c.lat == 48));
        let dateline = tiles_for_radius(0.0, 179.8, 100_000.0).unwrap();
        assert!(dateline.iter().any(|c| c.lon == 179));
        assert!(dateline.iter().any(|c| c.lon == -180));
    }
    #[test]
    fn content_hash_changes_with_terrain() {
        let coordinate = TileCoordinate { lat: 0, lon: 0 };
        let a = TerrainMosaic::new([Arc::new(HgtTile {
            coordinate,
            dimension: 1201,
            heights: vec![1; 1201 * 1201],
        })]);
        let b = TerrainMosaic::new([Arc::new(HgtTile {
            coordinate,
            dimension: 1201,
            heights: vec![2; 1201 * 1201],
        })]);
        assert_ne!(a.content_hash(), b.content_hash());
        assert_eq!(a.content_hash(), a.content_hash());
    }

    #[test]
    fn bathymetry_is_clamped_to_water_surface() {
        let coordinate = TileCoordinate { lat: 0, lon: 0 };
        let mut heights = vec![10; 1201 * 1201];
        heights[600 * 1201 + 600] = -1763;
        let mosaic = TerrainMosaic::new([Arc::new(HgtTile {
            coordinate,
            dimension: 1201,
            heights,
        })]);
        assert_eq!(mosaic.sample(0.5, 0.5).unwrap(), Some(0));
    }
}
