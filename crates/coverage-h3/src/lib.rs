//! Hierarchical H3 coverage exporter from native `.rhgt` raster grids.
//!
//! Converts radar intervisibility and minimum detection height rasters
//! into discrete H3 hexagonal cells with exact floor elevation.

use coverage_storage::{Metadata, NO_DATA};
use h3o::{CellIndex, LatLng, Resolution};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
use terrain_srtm::LocalProjection;

#[derive(Debug, thiserror::Error)]
pub enum H3ExportError {
    #[error("invalid resolution {0}: must be between 0 and 15")]
    InvalidResolution(u8),
    #[error("invalid CRS or projection: {0}")]
    InvalidProjection(String),
    #[error("dimension overflow or mismatch: metadata has {expected} cells, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("H3 coordinate error: {0}")]
    H3Coord(String),
}

/// Configuration for the H3 coverage export.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct H3ExportConfig {
    /// Target H3 resolution (default: 7). Valid values: 0..=15.
    pub target_resolution: u8,
    /// Coarse starting resolution for hierarchical pruning (default: 5).
    pub start_resolution: u8,
    /// Optional altitude bucket step in meters (e.g. 100 for [0, 100[, [100, 200[...).
    pub altitude_bucket_m: Option<u16>,
    /// Whether to include precalculated closed polygon boundary coordinates [lon, lat] in degrees.
    pub include_boundary: bool,
}

impl Default for H3ExportConfig {
    fn default() -> Self {
        Self {
            target_resolution: 7,
            start_resolution: 5,
            altitude_bucket_m: None,
            include_boundary: true,
        }
    }
}

/// A single exported H3 cell representing visible radar coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct H3Record {
    /// H3 cell index in lowercase 15-character hexadecimal string.
    pub h3: String,
    /// H3 resolution level.
    pub res: u8,
    /// Minimum visible altitude floor in meters (lowest altitude across all intersecting pixels).
    pub min_floor_m: u16,
    /// Optional quantized altitude bucket in meters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude_bucket_m: Option<u16>,
    /// Optional closed hexagon boundary coordinates as `[[lon, lat], ...]` (GeoJSON RFC 7946).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<Vec<[f64; 2]>>,
}

/// Execution statistics for the export operation.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct H3ExportStats {
    pub total_candidate_roots: usize,
    pub pruned_coarse_cells: usize,
    pub visited_leaf_cells: usize,
    pub exported_cells: usize,
    pub min_altitude_m: Option<u16>,
    pub max_altitude_m: Option<u16>,
}

impl H3ExportStats {
    pub fn merge(&mut self, other: &Self) {
        self.total_candidate_roots += other.total_candidate_roots;
        self.pruned_coarse_cells += other.pruned_coarse_cells;
        self.visited_leaf_cells += other.visited_leaf_cells;
        self.exported_cells += other.exported_cells;
        self.min_altitude_m = match (self.min_altitude_m, other.min_altitude_m) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.max_altitude_m = match (self.max_altitude_m, other.max_altitude_m) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Parses a `LocalProjection` from an AEQD CRS description string.
pub fn parse_local_projection(crs: &str) -> Result<LocalProjection, H3ExportError> {
    let mut lat0 = None;
    let mut lon0 = None;
    for token in crs.split_whitespace() {
        if let Some(val) = token.strip_prefix("+lat_0=") {
            lat0 = val.parse::<f64>().ok();
        } else if let Some(val) = token.strip_prefix("+lon_0=") {
            lon0 = val.parse::<f64>().ok();
        }
    }
    match (lat0, lon0) {
        (Some(lat), Some(lon)) => LocalProjection::new(lat, lon)
            .map_err(|e| H3ExportError::InvalidProjection(e.to_string())),
        _ => Err(H3ExportError::InvalidProjection(format!(
            "missing +lat_0 or +lon_0 in CRS: {crs}"
        ))),
    }
}

/// Approximate H3 edge length in meters at a given resolution (0..=15).
fn approx_h3_edge_length_m(res: u8) -> f64 {
    const EDGE_LENGTHS_M: [f64; 16] = [
        1_107_712.0, // Res 0
        418_676.0,   // Res 1
        158_244.0,   // Res 2
        59_810.0,    // Res 3
        22_606.0,    // Res 4
        8_544.0,     // Res 5
        3_229.0,     // Res 6
        1_220.0,     // Res 7
        461.0,       // Res 8
        174.0,       // Res 9
        66.0,        // Res 10
        25.0,        // Res 11
        9.4,         // Res 12
        3.5,         // Res 13
        1.3,         // Res 14
        0.5,         // Res 15
    ];
    EDGE_LENGTHS_M.get(res as usize).copied().unwrap_or(0.5)
}

/// Reads and validates an `.rhgt` file, returning metadata and the decoded `u16` heights.
pub fn read_rhgt_file(path: &std::path::Path) -> Result<(Metadata, Vec<u16>), H3ExportError> {
    let (meta, raw_bytes) = coverage_storage::read(path, coverage_storage::RHGT_MAGIC)
        .map_err(|e| H3ExportError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    let heights = raw_bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    Ok((meta, heights))
}

/// Directly converts a `.rhgt` file on disk to an H3 JSONL output stream.
pub fn export_rhgt_file_to_h3_jsonl<W: Write>(
    rhgt_path: &std::path::Path,
    config: &H3ExportConfig,
    writer: &mut W,
) -> Result<H3ExportStats, H3ExportError> {
    let (meta, heights) = read_rhgt_file(rhgt_path)?;
    export_rhgt_to_h3_jsonl(&meta, &heights, config, writer)
}

/// Converts a radar `.rhgt` raster dataset into a stream of H3 JSONL records.
pub fn export_rhgt_to_h3_jsonl<W: Write>(
    metadata: &Metadata,
    heights: &[u16],
    config: &H3ExportConfig,
    writer: &mut W,
) -> Result<H3ExportStats, H3ExportError> {
    let (records, stats) = extract_h3_records(metadata, heights, config)?;

    for record in &records {
        serde_json::to_writer(&mut *writer, record)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    Ok(stats)
}

/// Computes the H3 records and statistics using hierarchical reverse-pull traversal.
pub fn extract_h3_records(
    metadata: &Metadata,
    heights: &[u16],
    config: &H3ExportConfig,
) -> Result<(Vec<H3Record>, H3ExportStats), H3ExportError> {
    if config.target_resolution > 15 {
        return Err(H3ExportError::InvalidResolution(config.target_resolution));
    }
    let expected_cells = metadata
        .cells()
        .map_err(|_e| H3ExportError::DimensionMismatch {
            expected: 0,
            actual: heights.len(),
        })?;
    if heights.len() != expected_cells {
        return Err(H3ExportError::DimensionMismatch {
            expected: expected_cells,
            actual: heights.len(),
        });
    }

    let projection = parse_local_projection(&metadata.crs)?;
    let target_res = Resolution::try_from(config.target_resolution)
        .map_err(|_| H3ExportError::InvalidResolution(config.target_resolution))?;

    let start_res_val = config.start_resolution.min(config.target_resolution);
    let start_res = Resolution::try_from(start_res_val)
        .map_err(|_| H3ExportError::InvalidResolution(start_res_val))?;

    // Center candidate search on the actual raster extent center
    let center_x = (metadata.extent[0] + metadata.extent[2]) / 2.0;
    let center_y = (metadata.extent[1] + metadata.extent[3]) / 2.0;
    let (center_lat, center_lon) = projection
        .inverse(center_x, center_y)
        .map_err(|e| H3ExportError::InvalidProjection(e.to_string()))?;

    let center_latlng = LatLng::new(center_lat, center_lon)
        .map_err(|e| H3ExportError::H3Coord(format!("{e:?}")))?;
    let center_cell = center_latlng.to_cell(start_res);

    // Compute disk radius k based on extent radius and edge length
    let half_w = (metadata.extent[2] - metadata.extent[0]).abs() / 2.0;
    let half_h = (metadata.extent[3] - metadata.extent[1]).abs() / 2.0;
    let extent_radius_m = half_w.hypot(half_h);

    let edge_len = approx_h3_edge_length_m(start_res_val);
    let inter_center_dist = edge_len * 3.0f64.sqrt();
    let k = ((extent_radius_m / (inter_center_dist * 0.9)).ceil() as u32).saturating_add(3);

    let candidate_roots: Vec<CellIndex> = center_cell.grid_disk_safe(k).collect();
    let total_candidate_roots = candidate_roots.len();

    // Process candidate root trees in parallel with Rayon
    let parallel_results: Vec<(Vec<H3Record>, H3ExportStats)> = candidate_roots
        .into_par_iter()
        .map(|root_cell| {
            let mut local_records = Vec::new();
            let mut local_stats = H3ExportStats::default();

            traverse_h3_node(
                root_cell,
                metadata,
                heights,
                &projection,
                config,
                target_res,
                &mut local_records,
                &mut local_stats,
            );

            (local_records, local_stats)
        })
        .collect();

    let mut all_records = Vec::new();
    let mut total_stats = H3ExportStats {
        total_candidate_roots,
        ..Default::default()
    };

    for (records, stats) in parallel_results {
        all_records.extend(records);
        total_stats.merge(&stats);
    }

    // Sort deterministically by H3 string
    all_records.sort_by(|a, b| a.h3.cmp(&b.h3));

    Ok((all_records, total_stats))
}

/// Recursive hierarchical traversal function.
fn traverse_h3_node(
    cell: CellIndex,
    metadata: &Metadata,
    heights: &[u16],
    projection: &LocalProjection,
    config: &H3ExportConfig,
    target_res: Resolution,
    records: &mut Vec<H3Record>,
    stats: &mut H3ExportStats,
) {
    let current_res = cell.resolution();

    // Bounding box in raster coordinates
    let bbox = match cell_raster_bbox(cell, metadata, projection) {
        Some(b) => b,
        None => {
            // Completely outside raster extent
            stats.pruned_coarse_cells += 1;
            return;
        }
    };

    let width = metadata.width as usize;
    let nodata = metadata.nodata;

    // Coarse resolution check: check if any pixel is visible
    if current_res < target_res {
        let mut has_visible = false;
        'outer: for r in bbox.row_min..=bbox.row_max {
            let row_offset = r * width;
            for c in bbox.col_min..=bbox.col_max {
                if heights[row_offset + c] != nodata {
                    has_visible = true;
                    break 'outer;
                }
            }
        }

        if !has_visible {
            stats.pruned_coarse_cells += 1;
            return;
        }

        // Descend to children
        let next_res = match Resolution::try_from(u8::from(current_res) + 1) {
            Ok(r) => r,
            Err(_) => return,
        };

        for child in cell.children(next_res) {
            traverse_h3_node(
                child, metadata, heights, projection, config, target_res, records, stats,
            );
        }
    } else {
        // Leaf cell at target resolution
        stats.visited_leaf_cells += 1;

        let mut min_val = NO_DATA;
        let mut matching_pixels = 0;

        let res_m = metadata.resolution_m;
        let extent_min_x = metadata.extent[0];
        let extent_max_y = metadata.extent[3];

        for r in bbox.row_min..=bbox.row_max {
            let row_offset = r * width;
            let y = extent_max_y - (r as f64 + 0.5) * res_m;

            for c in bbox.col_min..=bbox.col_max {
                let val = heights[row_offset + c];
                if val == nodata {
                    continue;
                }

                let x = extent_min_x + (c as f64 + 0.5) * res_m;
                if let Ok((lat, lon)) = projection.inverse(x, y) {
                    if let Ok(ll) = LatLng::new(lat, lon) {
                        if ll.to_cell(target_res) == cell {
                            matching_pixels += 1;
                            min_val = min_val.min(val);
                        }
                    }
                }
            }
        }

        // Fallback for cases where cell is smaller than a single pixel
        if matching_pixels == 0 {
            let center_ll = LatLng::from(cell);
            if let Ok((cx, cy)) = projection.forward(center_ll.lat(), center_ll.lng()) {
                let c = ((cx - extent_min_x) / res_m).floor() as isize;
                let r = ((extent_max_y - cy) / res_m).floor() as isize;
                if c >= 0 && c < metadata.width as isize && r >= 0 && r < metadata.height as isize {
                    let val = heights[r as usize * width + c as usize];
                    if val != nodata {
                        min_val = val;
                    }
                }
            }
        }

        if min_val != NO_DATA {
            let boundary = if config.include_boundary {
                let mut coords: Vec<[f64; 2]> = cell
                    .boundary()
                    .iter()
                    .map(|pt| [pt.lng(), pt.lat()])
                    .collect();
                if let Some(&first) = coords.first() {
                    coords.push(first);
                }
                Some(coords)
            } else {
                None
            };

            let altitude_bucket_m = config.altitude_bucket_m.map(|step| (min_val / step) * step);

            records.push(H3Record {
                h3: cell.to_string(),
                res: u8::from(target_res),
                min_floor_m: min_val,
                altitude_bucket_m,
                boundary,
            });

            stats.exported_cells += 1;
            stats.min_altitude_m = Some(stats.min_altitude_m.map_or(min_val, |m| m.min(min_val)));
            stats.max_altitude_m = Some(stats.max_altitude_m.map_or(min_val, |m| m.max(min_val)));
        }
    }
}

struct RasterBBox {
    col_min: usize,
    col_max: usize,
    row_min: usize,
    row_max: usize,
}

/// Projects an H3 cell boundary into raster row/col indices.
fn cell_raster_bbox(
    cell: CellIndex,
    metadata: &Metadata,
    projection: &LocalProjection,
) -> Option<RasterBBox> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for pt in cell.boundary().iter() {
        if let Ok((x, y)) = projection.forward(pt.lat(), pt.lng()) {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }

    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        return None;
    }

    let res = metadata.resolution_m;
    let extent = metadata.extent;

    let c_min = ((min_x - extent[0]) / res).floor() as isize;
    let c_max = ((max_x - extent[0]) / res).ceil() as isize;
    let r_min = ((extent[3] - max_y) / res).floor() as isize;
    let r_max = ((extent[3] - min_y) / res).ceil() as isize;

    let width = metadata.width as isize;
    let height = metadata.height as isize;

    if c_max < 0 || c_min >= width || r_max < 0 || r_min >= height {
        return None;
    }

    Some(RasterBBox {
        col_min: c_min.max(0) as usize,
        col_max: (c_max.min(width - 1)) as usize,
        row_min: r_min.max(0) as usize,
        row_max: (r_max.min(height - 1)) as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_local_projection() {
        let crs = "+proj=aeqd +lat_0=43.5415275 +lon_0=6.554722 +R=6371000 +units=m";
        let proj = parse_local_projection(crs).unwrap();
        let (x, y) = proj.forward(43.5415275, 6.554722).unwrap();
        assert!(x.abs() < 1e-6);
        assert!(y.abs() < 1e-6);
    }
}
