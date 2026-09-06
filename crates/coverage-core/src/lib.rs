//! Scientific geometric line-of-sight engine.
//!
//! This crate models terrain intervisibility only. It does **not** model RF
//! power, free-space/atmospheric loss, Fresnel zones, diffraction or clutter.

mod angle;
mod bitset;
mod los;
#[cfg(feature = "rayon")]
mod parallel;
mod profile;
mod rings;

pub use angle::AngularTable;
pub use bitset::{merge_counts, BitSet};
pub use los::{
    compute_coverage, Coverage, CoverageError, Grid, LosConfig, LOS_ALGORITHM_VERSION,
    NO_DATA_HEIGHT,
};
#[cfg(feature = "rayon")]
pub use parallel::{compute_coverages_bounded, ParallelConfig};
pub use profile::{compute_profile, ProfilePoint, ProfileResult};
pub use rings::{for_each_ring_cell, ring_cells, Side};

/// Mean Earth radius used by the effective-Earth model, in metres.
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Returns terrain elevation after effective-Earth curvature correction.
#[inline]
pub fn apparent_height(terrain_m: f64, distance_squared_m2: f64, k: f64) -> f64 {
    terrain_m - distance_squared_m2 / (2.0 * k * EARTH_RADIUS_M)
}
