//! Versioned, checksummed, atomic coverage storage and streaming fusion.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
pub const RCOV_MAGIC: [u8; 4] = *b"RCOV";
pub const RHGT_MAGIC: [u8; 4] = *b"RHGT";
pub const RDEM_MAGIC: [u8; 4] = *b"RDEM";
pub const VERSION: u16 = 1;
pub const NO_DATA: u16 = u16::MAX;
const PREFIX: usize = 18;
const CHECKSUM: usize = 32;
const BLOCK: usize = 64 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Metadata {
    pub radar_id: String,
    pub los_algorithm_version: u16,
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
impl Metadata {
    pub fn cells(&self) -> Result<usize, StorageError> {
        (self.width as usize)
            .checked_mul(self.height as usize)
            .ok_or(StorageError::Format("dimension overflow"))
    }
    fn same_grid(&self, o: &Self) -> bool {
        self.crs == o.crs
            && self.origin == o.origin
            && self.resolution_m == o.resolution_m
            && self.extent == o.extent
            && self.width == o.width
            && self.height == o.height
            && self.nodata == o.nodata
    }
}
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid format: {0}")]
    Format(&'static str),
    #[error("checksum mismatch")]
    Checksum,
    #[error("incompatible grids")]
    IncompatibleGrid,
}
struct Header {
    metadata: Metadata,
    json: Vec<u8>,
    payload_len: u64,
}
pub fn write_rcov(
    path: &Path,
    m: &Metadata,
    words: &[u64],
    sync: bool,
) -> Result<(), StorageError> {
    if words.len() != m.cells()?.div_ceil(64) {
        return Err(StorageError::Format("rcov words"));
    }
    let p = words
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    write_atomic(path, RCOV_MAGIC, m, &p, sync)
}
pub fn write_rhgt(
    path: &Path,
    m: &Metadata,
    heights: &[u16],
    sync: bool,
) -> Result<(), StorageError> {
    if heights.len() != m.cells()? {
        return Err(StorageError::Format("rhgt cells"));
    }
    let p = heights
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    write_atomic(path, RHGT_MAGIC, m, &p, sync)
}

/// Persists signed terrain elevations without materializing a second payload.
pub fn write_rdem(
    path: &Path,
    m: &Metadata,
    elevations: &[Option<f32>],
    sync: bool,
) -> Result<(), StorageError> {
    if elevations.len() != m.cells()? {
        return Err(StorageError::Format("rdem cells"));
    }
    let json = serde_json::to_vec(m)?;
    let payload_len = elevations
        .len()
        .checked_mul(2)
        .ok_or(StorageError::Format("payload too large"))?;
    let tmp = temp_path(path);
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(&RDEM_MAGIC)?;
        f.write_all(&VERSION.to_le_bytes())?;
        f.write_all(
            &u32::try_from(json.len())
                .map_err(|_| StorageError::Format("header too large"))?
                .to_le_bytes(),
        )?;
        f.write_all(&(payload_len as u64).to_le_bytes())?;
        f.write_all(&json)?;
        let mut hash = hasher(RDEM_MAGIC, &json);
        let mut buffer = Vec::with_capacity(BLOCK);
        for elevation in elevations {
            let value = elevation.map_or(i16::MIN, |v| {
                v.round().clamp(i16::MIN as f32 + 1.0, i16::MAX as f32) as i16
            });
            buffer.extend_from_slice(&value.to_le_bytes());
            if buffer.len() == BLOCK {
                f.write_all(&buffer)?;
                hash.update(&buffer);
                buffer.clear();
            }
        }
        if !buffer.is_empty() {
            f.write_all(&buffer)?;
            hash.update(&buffer);
        }
        f.write_all(hash.finalize().as_bytes())?;
        f.flush()?;
        if sync {
            f.sync_all()?;
        }
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

pub fn read_rdem_value(path: &Path, index: usize) -> Result<Option<i16>, StorageError> {
    let mut r = BufReader::new(File::open(path)?);
    let h = read_header(&mut r, RDEM_MAGIC)?;
    let cells = h.metadata.cells()?;
    if index >= cells || h.payload_len != cells as u64 * 2 {
        return Err(StorageError::Format("rdem index"));
    }
    let offset = PREFIX as u64 + h.json.len() as u64 + index as u64 * 2;
    r.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0u8; 2];
    r.read_exact(&mut bytes)?;
    let value = i16::from_le_bytes(bytes);
    Ok((value != i16::MIN).then_some(value))
}
pub fn write_atomic(
    path: &Path,
    magic: [u8; 4],
    m: &Metadata,
    payload: &[u8],
    sync: bool,
) -> Result<(), StorageError> {
    let json = serde_json::to_vec(m)?;
    let tmp = temp_path(path);
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(&magic)?;
        f.write_all(&VERSION.to_le_bytes())?;
        f.write_all(
            &u32::try_from(json.len())
                .map_err(|_| StorageError::Format("header too large"))?
                .to_le_bytes(),
        )?;
        f.write_all(
            &u64::try_from(payload.len())
                .map_err(|_| StorageError::Format("payload too large"))?
                .to_le_bytes(),
        )?;
        f.write_all(&json)?;
        f.write_all(payload)?;
        let mut h = hasher(magic, &json);
        h.update(payload);
        f.write_all(h.finalize().as_bytes())?;
        f.flush()?;
        if sync {
            f.sync_all()?
        }
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub fn read(path: &Path, magic: [u8; 4]) -> Result<(Metadata, Vec<u8>), StorageError> {
    let mut r = BufReader::new(File::open(path)?);
    let h = read_header(&mut r, magic)?;
    let mut p =
        vec![0; usize::try_from(h.payload_len).map_err(|_| StorageError::Format("payload size"))?];
    r.read_exact(&mut p)?;
    validate_tail(&mut r, magic, &h.json, &p)?;
    Ok((h.metadata, p))
}

/// Validates an envelope, including its payload checksum, without retaining the
/// payload in memory. Returns the trusted metadata when the file is complete.
pub fn validate(path: &Path, magic: [u8; 4]) -> Result<Metadata, StorageError> {
    let mut r = BufReader::new(File::open(path)?);
    let h = read_header(&mut r, magic)?;
    let mut hash = hasher(magic, &h.json);
    let mut buffer = vec![0u8; BLOCK];
    let mut remaining = h.payload_len;
    while remaining > 0 {
        let n = usize::try_from(remaining.min(BLOCK as u64))
            .map_err(|_| StorageError::Format("payload size"))?;
        r.read_exact(&mut buffer[..n])?;
        hash.update(&buffer[..n]);
        remaining -= n as u64;
    }
    let mut stored = [0; CHECKSUM];
    r.read_exact(&mut stored)?;
    if hash.finalize().as_bytes() != &stored {
        return Err(StorageError::Checksum);
    }
    eof(&mut r)?;
    Ok(h.metadata)
}
/// Fuses `.rhgt` layers using one 64 KiB input buffer; layers are never loaded together.
pub fn merge_rhgt_counts_streaming(
    paths: &[PathBuf],
    target: u16,
) -> Result<(Option<Metadata>, Vec<u8>), StorageError> {
    if paths.is_empty() {
        return Ok((None, Vec::new()));
    }
    let mut reference = None;
    let mut out: Vec<u8> = Vec::new();
    for path in paths {
        let mut r = BufReader::new(File::open(path)?);
        let h = read_header(&mut r, RHGT_MAGIC)?;
        let cells = h.metadata.cells()?;
        if h.payload_len != cells as u64 * 2 {
            return Err(StorageError::Format("rhgt payload"));
        }
        if let Some(m) = &reference {
            if !Metadata::same_grid(m, &h.metadata) {
                return Err(StorageError::IncompatibleGrid);
            }
        } else {
            out.resize(cells, 0);
            reference = Some(h.metadata.clone())
        }
        let mut hash = hasher(RHGT_MAGIC, &h.json);
        let mut buffer = vec![0u8; BLOCK];
        let mut remaining = h.payload_len as usize;
        let mut offset = 0;
        while remaining > 0 {
            let n = remaining.min(buffer.len());
            r.read_exact(&mut buffer[..n])?;
            hash.update(&buffer[..n]);
            for pair in buffer[..n].as_chunks::<2>().0 {
                let value = u16::from_le_bytes(*pair);
                if value != NO_DATA && value <= target {
                    out[offset] = out[offset].saturating_add(1)
                }
                offset += 1
            }
            remaining -= n
        }
        let mut stored = [0; CHECKSUM];
        r.read_exact(&mut stored)?;
        if hash.finalize().as_bytes() != &stored {
            return Err(StorageError::Checksum);
        }
        eof(&mut r)?;
    }
    Ok((reference, out))
}

/// Returns the best (lowest) valid minimum-detection height per cell while
/// keeping only one input block and the output grid resident.
pub fn merge_rhgt_minimum_streaming(
    paths: &[PathBuf],
) -> Result<(Option<Metadata>, Vec<u16>), StorageError> {
    if paths.is_empty() {
        return Ok((None, Vec::new()));
    }
    let mut reference = None;
    let mut out = Vec::new();
    for path in paths {
        let mut r = BufReader::new(File::open(path)?);
        let h = read_header(&mut r, RHGT_MAGIC)?;
        let cells = h.metadata.cells()?;
        if h.payload_len != cells as u64 * 2 {
            return Err(StorageError::Format("rhgt payload"));
        }
        if let Some(m) = &reference {
            if !Metadata::same_grid(m, &h.metadata) {
                return Err(StorageError::IncompatibleGrid);
            }
        } else {
            out.resize(cells, NO_DATA);
            reference = Some(h.metadata.clone())
        }
        let mut hash = hasher(RHGT_MAGIC, &h.json);
        let mut buffer = vec![0u8; BLOCK];
        let mut remaining = h.payload_len as usize;
        let mut offset = 0;
        while remaining > 0 {
            let n = remaining.min(buffer.len());
            r.read_exact(&mut buffer[..n])?;
            hash.update(&buffer[..n]);
            for pair in buffer[..n].as_chunks::<2>().0 {
                let value = u16::from_le_bytes(*pair);
                if value != NO_DATA {
                    out[offset] = out[offset].min(value)
                }
                offset += 1
            }
            remaining -= n
        }
        let mut stored = [0; CHECKSUM];
        r.read_exact(&mut stored)?;
        if hash.finalize().as_bytes() != &stored {
            return Err(StorageError::Checksum);
        }
        eof(&mut r)?
    }
    Ok((reference, out))
}
pub fn find_by_config_hash(
    dir: &Path,
    magic: [u8; 4],
    hash: &str,
) -> Result<Vec<PathBuf>, StorageError> {
    let mut found = Vec::new();
    if !dir.exists() {
        return Ok(found);
    }
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        if !p.is_file() {
            continue;
        }
        let mut r = BufReader::new(File::open(&p)?);
        if let Ok(h) = read_header(&mut r, magic) {
            if h.metadata.radar_config_hash == hash {
                found.push(p)
            }
        }
    }
    found.sort();
    Ok(found)
}
fn read_header(r: &mut (impl Read + Seek), magic: [u8; 4]) -> Result<Header, StorageError> {
    let len = r.seek(SeekFrom::End(0))?;
    r.rewind()?;
    if len < (PREFIX + CHECKSUM) as u64 {
        return Err(StorageError::Format("truncated"));
    }
    let mut p = [0; PREFIX];
    r.read_exact(&mut p)?;
    if p[..4] != magic {
        return Err(StorageError::Format("magic"));
    }
    if u16::from_le_bytes([p[4], p[5]]) != VERSION {
        return Err(StorageError::Format("version"));
    }
    let hl = u32::from_le_bytes(p[6..10].try_into().expect("prefix")) as usize;
    let pl = u64::from_le_bytes(p[10..18].try_into().expect("prefix"));
    let expected = (PREFIX as u64)
        .checked_add(hl as u64)
        .and_then(|v| v.checked_add(pl))
        .and_then(|v| v.checked_add(CHECKSUM as u64))
        .ok_or(StorageError::Format("length overflow"))?;
    if len != expected {
        return Err(StorageError::Format("length"));
    }
    let mut json = vec![0; hl];
    r.read_exact(&mut json)?;
    let metadata = serde_json::from_slice(&json)?;
    Ok(Header {
        metadata,
        json,
        payload_len: pl,
    })
}
fn validate_tail(
    r: &mut impl Read,
    magic: [u8; 4],
    json: &[u8],
    payload: &[u8],
) -> Result<(), StorageError> {
    let mut stored = [0; CHECKSUM];
    r.read_exact(&mut stored)?;
    let mut h = hasher(magic, json);
    h.update(payload);
    if h.finalize().as_bytes() != &stored {
        return Err(StorageError::Checksum);
    }
    eof(r)
}
fn eof(r: &mut impl Read) -> Result<(), StorageError> {
    let mut b = [0];
    if r.read(&mut b)? != 0 {
        return Err(StorageError::Format("trailing bytes"));
    }
    Ok(())
}
fn hasher(magic: [u8; 4], json: &[u8]) -> blake3::Hasher {
    let mut h = blake3::Hasher::new();
    h.update(&magic);
    h.update(&VERSION.to_le_bytes());
    h.update(json);
    h
}
fn temp_path(path: &Path) -> PathBuf {
    let n = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("coverage");
    path.with_file_name(format!(".{n}.{}.tmp", std::process::id()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    fn meta(id: &str) -> Metadata {
        Metadata {
            radar_id: id.into(),
            los_algorithm_version: 1,
            radar_config_hash: format!("hash-{id}"),
            terrain_hash: "terrain".into(),
            calculated_at: "2026-09-06T00:00:00Z".into(),
            crs: "EPSG:3857".into(),
            origin: [0., 0.],
            resolution_m: 90.,
            extent: [0., 0., 180., 180.],
            width: 2,
            height: 2,
            range_m: 400_000.,
            effective_earth_k: 4. / 3.,
            nodata: NO_DATA,
        }
    }
    fn dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("radial-{}-{n}-{sequence}", std::process::id()));
        fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn round_trips_and_finds() {
        let d = dir();
        let r = d.join("a.rhgt");
        write_rhgt(&r, &meta("a"), &[0, 30, NO_DATA, 100], false).unwrap();
        assert_eq!(validate(&r, RHGT_MAGIC).unwrap(), meta("a"));
        assert_eq!(read(&r, RHGT_MAGIC).unwrap().1.len(), 8);
        assert_eq!(
            find_by_config_hash(&d, RHGT_MAGIC, "hash-a").unwrap(),
            vec![r]
        );
        let c = d.join("a.rcov");
        write_rcov(&c, &meta("a"), &[10], false).unwrap();
        assert_eq!(read(&c, RCOV_MAGIC).unwrap().1, 10u64.to_le_bytes());
        let terrain = d.join("a.rdem");
        write_rdem(
            &terrain,
            &meta("a"),
            &[Some(-12.0), Some(345.0), None, Some(1.0)],
            false,
        )
        .unwrap();
        assert_eq!(validate(&terrain, RDEM_MAGIC).unwrap(), meta("a"));
        assert_eq!(read_rdem_value(&terrain, 0).unwrap(), Some(-12));
        assert_eq!(read_rdem_value(&terrain, 1).unwrap(), Some(345));
        assert_eq!(read_rdem_value(&terrain, 2).unwrap(), None);
        fs::remove_dir_all(d).unwrap()
    }
    #[test]
    fn streaming_merge_and_empty_selection() {
        let d = dir();
        let a = d.join("a.rhgt");
        let b = d.join("b.rhgt");
        write_rhgt(&a, &meta("a"), &[0, 30, NO_DATA, 100], false).unwrap();
        write_rhgt(&b, &meta("b"), &[50, 10, 100, NO_DATA], false).unwrap();
        assert_eq!(
            merge_rhgt_counts_streaming(&[a, b], 50).unwrap().1,
            vec![2, 2, 0, 0]
        );
        assert!(merge_rhgt_counts_streaming(&[], 50).unwrap().1.is_empty());
        assert_eq!(
            merge_rhgt_minimum_streaming(&[d.join("a.rhgt"), d.join("b.rhgt")])
                .unwrap()
                .1,
            vec![0, 10, 100, 100]
        );
        fs::remove_dir_all(d).unwrap()
    }
    #[test]
    fn detects_checksum_and_truncation() {
        let d = dir();
        let p = d.join("a.rhgt");
        write_rhgt(&p, &meta("a"), &[0; 4], false).unwrap();
        let mut b = fs::read(&p).unwrap();
        let last = b.len() - 1;
        b[last] ^= 1;
        fs::write(&p, &b).unwrap();
        assert!(matches!(read(&p, RHGT_MAGIC), Err(StorageError::Checksum)));
        b.truncate(12);
        fs::write(&p, &b).unwrap();
        assert!(matches!(read(&p, RHGT_MAGIC), Err(StorageError::Format(_))));
        fs::remove_dir_all(d).unwrap()
    }
    #[test]
    fn rejects_incompatible_grid() {
        let d = dir();
        let a = d.join("a.rhgt");
        let b = d.join("b.rhgt");
        write_rhgt(&a, &meta("a"), &[0; 4], false).unwrap();
        let mut m = meta("b");
        m.width = 1;
        write_rhgt(&b, &m, &[0; 2], false).unwrap();
        assert!(matches!(
            merge_rhgt_counts_streaming(&[a, b], 0),
            Err(StorageError::IncompatibleGrid)
        ));
        fs::remove_dir_all(d).unwrap()
    }
    #[test]
    fn rejects_magic_and_unknown_version() {
        let d = dir();
        let p = d.join("a.rhgt");
        write_rhgt(&p, &meta("a"), &[0; 4], false).unwrap();
        assert!(matches!(
            read(&p, RCOV_MAGIC),
            Err(StorageError::Format("magic"))
        ));
        let mut bytes = fs::read(&p).unwrap();
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        fs::write(&p, bytes).unwrap();
        assert!(matches!(
            read(&p, RHGT_MAGIC),
            Err(StorageError::Format("version"))
        ));
        fs::remove_dir_all(d).unwrap()
    }

    #[test]
    fn rejects_legacy_metadata_without_los_algorithm_version() {
        let mut value = serde_json::to_value(meta("legacy")).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("los_algorithm_version");
        assert!(serde_json::from_value::<Metadata>(value).is_err());
    }
}
