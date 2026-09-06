use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};
pub const RCOV_MAGIC: [u8; 4] = *b"RCOV";
pub const RHGT_MAGIC: [u8; 4] = *b"RHGT";
pub const VERSION: u16 = 1;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Metadata {
    pub radar_id: String,
    pub radar_config_hash: String,
    pub terrain_hash: String,
    pub calculated_at: String,
    pub crs: String,
    pub origin: [f64; 2],
    pub resolution_m: f64,
    pub extent: [f64; 4],
    pub width: u32,
    pub height: u32,
    pub range_m: f64,
    pub effective_earth_k: f64,
    pub nodata: u16,
}
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid format")]
    Format,
    #[error("checksum mismatch")]
    Checksum,
}
pub fn write_atomic(
    path: &Path,
    magic: [u8; 4],
    meta: &Metadata,
    payload: &[u8],
    sync: bool,
) -> Result<(), StorageError> {
    let header = serde_json::to_vec(meta)?;
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(&magic)?;
    f.write_all(&VERSION.to_le_bytes())?;
    f.write_all(&(header.len() as u32).to_le_bytes())?;
    f.write_all(&(payload.len() as u64).to_le_bytes())?;
    f.write_all(&header)?;
    f.write_all(payload)?;
    let mut hash = blake3::Hasher::new();
    hash.update(&magic);
    hash.update(&VERSION.to_le_bytes());
    hash.update(&header);
    hash.update(payload);
    f.write_all(hash.finalize().as_bytes())?;
    f.flush()?;
    if sync {
        f.sync_all()?
    }
    drop(f);
    fs::rename(tmp, path)?;
    Ok(())
}
pub fn read(path: &Path, expected: [u8; 4]) -> Result<(Metadata, Vec<u8>), StorageError> {
    let mut b = Vec::new();
    File::open(path)?.read_to_end(&mut b)?;
    if b.len() < 50 || b[..4] != expected || u16::from_le_bytes([b[4], b[5]]) != VERSION {
        return Err(StorageError::Format);
    }
    let hl = u32::from_le_bytes(b[6..10].try_into().unwrap()) as usize;
    let pl = u64::from_le_bytes(b[10..18].try_into().unwrap()) as usize;
    let end = 18usize
        .checked_add(hl)
        .and_then(|v| v.checked_add(pl))
        .ok_or(StorageError::Format)?;
    if end + 32 != b.len() {
        return Err(StorageError::Format);
    }
    let meta: Metadata = serde_json::from_slice(&b[18..18 + hl])?;
    let mut h = blake3::Hasher::new();
    h.update(&expected);
    h.update(&VERSION.to_le_bytes());
    h.update(&b[18..18 + hl]);
    h.update(&b[18 + hl..end]);
    if h.finalize().as_bytes() != &b[end..] {
        return Err(StorageError::Checksum);
    }
    Ok((meta, b[18 + hl..end].to_vec()))
}
