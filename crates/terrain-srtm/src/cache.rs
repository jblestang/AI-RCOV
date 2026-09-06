use crate::{HgtTile, TerrainError, TerrainMosaic, TileCoordinate};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;

const SRTM_ORIGIN: &str = "https://s3.amazonaws.com/elevation-tiles-prod/skadi";

#[derive(Clone, Debug)]
pub struct SrtmCacheConfig {
    pub disk_directory: PathBuf,
    pub max_download_bytes: usize,
    pub request_timeout: Duration,
    pub retries: u8,
    pub max_concurrent_downloads: usize,
}
impl Default for SrtmCacheConfig {
    fn default() -> Self {
        Self {
            disk_directory: PathBuf::from("data/srtm"),
            max_download_bytes: 30_000_000,
            request_timeout: Duration::from_secs(30),
            retries: 2,
            max_concurrent_downloads: 4,
        }
    }
}

/// Two-level SRTM cache. Locks protect handle management only; the returned
/// `TerrainMosaic` is immutable and lock-free for per-cell reads.
pub struct SrtmCache {
    config: SrtmCacheConfig,
    client: reqwest::Client,
    memory: Mutex<BTreeMap<TileCoordinate, Arc<HgtTile>>>,
    permits: Semaphore,
}
impl SrtmCache {
    pub fn new(config: SrtmCacheConfig) -> Result<Self, TerrainError> {
        if config.max_concurrent_downloads == 0 {
            return Err(TerrainError::Network(
                "download concurrency must be positive".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .gzip(false)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| TerrainError::Network(e.to_string()))?;
        let permits = Semaphore::new(config.max_concurrent_downloads);
        Ok(Self {
            config,
            client,
            memory: Mutex::new(BTreeMap::new()),
            permits,
        })
    }
    pub async fn load(&self, c: TileCoordinate) -> Result<Arc<HgtTile>, TerrainError> {
        if let Some(tile) = self
            .memory
            .lock()
            .map_err(|_| TerrainError::Network("cache lock poisoned".into()))?
            .get(&c)
            .cloned()
        {
            return Ok(tile);
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| TerrainError::Network("cache closed".into()))?;
        if let Some(tile) = self
            .memory
            .lock()
            .map_err(|_| TerrainError::Network("cache lock poisoned".into()))?
            .get(&c)
            .cloned()
        {
            return Ok(tile);
        }
        fs::create_dir_all(&self.config.disk_directory)?;
        let path = self.path(c);
        let compressed = if path.exists() {
            read_limited(&path, self.config.max_download_bytes)?
        } else {
            let bytes = self.download(c).await?;
            atomic_write(&path, &bytes).await?;
            bytes
        };
        let tile = Arc::new(HgtTile::decode_gzip(c, compressed.as_slice())?);
        self.memory
            .lock()
            .map_err(|_| TerrainError::Network("cache lock poisoned".into()))?
            .insert(c, tile.clone());
        Ok(tile)
    }
    pub async fn prepare_mosaic(
        &self,
        coordinates: &[TileCoordinate],
    ) -> Result<Arc<TerrainMosaic>, TerrainError> {
        let mut tiles = Vec::with_capacity(coordinates.len());
        for chunk in coordinates.chunks(self.config.max_concurrent_downloads) {
            let futures = chunk.iter().map(|&c| self.load(c));
            for result in futures::future::join_all(futures).await {
                tiles.push(result?)
            }
        }
        Ok(Arc::new(TerrainMosaic::new(tiles)))
    }
    fn path(&self, c: TileCoordinate) -> PathBuf {
        self.config
            .disk_directory
            .join(format!("{}.hgt.gz", tile_name(c)))
    }
    async fn download(&self, c: TileCoordinate) -> Result<Vec<u8>, TerrainError> {
        let name = tile_name(c);
        let band = &name[..3];
        let url = format!("{SRTM_ORIGIN}/{band}/{name}.hgt.gz");
        let mut last = None;
        for attempt in 0..=self.config.retries {
            match self.client.get(&url).send().await {
                Ok(response) if response.status().is_success() => {
                    if response
                        .content_length()
                        .is_some_and(|n| n > self.config.max_download_bytes as u64)
                    {
                        return Err(TerrainError::TooLarge);
                    }
                    let bytes = response
                        .bytes()
                        .await
                        .map_err(|e| TerrainError::Network(e.to_string()))?;
                    if bytes.len() > self.config.max_download_bytes {
                        return Err(TerrainError::TooLarge);
                    }
                    return Ok(bytes.to_vec());
                }
                Ok(response) => last = Some(format!("HTTP {}", response.status())),
                Err(e) => last = Some(e.to_string()),
            }
            if attempt < self.config.retries {
                tokio::time::sleep(Duration::from_millis(100 * 2u64.pow(attempt as u32))).await
            }
        }
        Err(TerrainError::Network(
            last.unwrap_or_else(|| "download failed".into()),
        ))
    }
}
fn tile_name(c: TileCoordinate) -> String {
    format!(
        "{}{:02}{}{:03}",
        if c.lat >= 0 { 'N' } else { 'S' },
        c.lat.unsigned_abs(),
        if c.lon >= 0 { 'E' } else { 'W' },
        c.lon.unsigned_abs()
    )
}
fn read_limited(path: &Path, max: usize) -> Result<Vec<u8>, TerrainError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > max as u64 {
        return Err(TerrainError::TooLarge);
    }
    Ok(fs::read(path)?)
}
async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), TerrainError> {
    let tmp = path.with_extension(format!("gz.{}.tmp", std::process::id()));
    let result = (|| {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(TerrainError::Io)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_canonical() {
        assert_eq!(tile_name(TileCoordinate { lat: 7, lon: -12 }), "N07W012");
        assert_eq!(tile_name(TileCoordinate { lat: -1, lon: 2 }), "S01E002");
    }
    #[test]
    fn rejects_zero_concurrency() {
        let c = SrtmCacheConfig {
            max_concurrent_downloads: 0,
            ..Default::default()
        };
        assert!(SrtmCache::new(c).is_err())
    }
}
