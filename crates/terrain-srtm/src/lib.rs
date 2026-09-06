use std::{collections::BTreeMap, io::Read, sync::Arc};
mod cache;
pub use cache::{SrtmCache, SrtmCacheConfig};
pub const VOID: i16 = -32768;
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TileCoordinate {
    pub lat: i16,
    pub lon: i16,
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
}
