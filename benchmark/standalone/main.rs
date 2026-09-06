//! Offline benchmark compiled directly with rustc. It imports the production
//! scientific modules instead of maintaining a second LOS implementation.
use std::{env, time::Instant};
#[path = "../../crates/coverage-core/src/angle.rs"]
mod angle;
#[path = "../../crates/coverage-core/src/bitset.rs"]
mod bitset;
#[allow(dead_code)]
#[path = "../../crates/coverage-core/src/los.rs"]
mod los;
#[allow(dead_code)]
#[path = "../../crates/coverage-core/src/rings.rs"]
mod rings;
pub use angle::AngularTable;
pub use bitset::{merge_counts, BitSet};
pub use rings::for_each_ring_cell;
pub const EARTH_RADIUS_M: f64 = 6_371_000.;
#[inline]
pub fn apparent_height(h: f64, d2: f64, k: f64) -> f64 {
    h - d2 / (2. * k * EARTH_RADIUS_M)
}
fn variable<T: std::str::FromStr>(name: &str, default: T) -> T {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn main() {
    let cell = variable("RADAR_BENCH_CELL_M", 90u32);
    let radars = variable("RADAR_BENCH_RADARS", 1usize);
    let iterations = variable("RADAR_BENCH_ITERATIONS", 3usize);
    let sectors_override = variable("RADAR_BENCH_SECTORS_PER_RADAR", 0usize);
    let range = 400_000.;
    let radius = (range / cell as f64).ceil() as usize;
    let dimension = radius * 2 + 1;
    let cells = dimension.checked_mul(dimension).expect("grid size");
    let terrain = (0..cells)
        .map(|index| {
            let x = index % dimension;
            let y = index / dimension;
            let dx = x as isize - radius as isize;
            let dy = y as isize - radius as isize;
            let ridge = if dx == radius as isize / 3 && (dy.unsigned_abs() < radius / 5) {
                350.
            } else {
                0.
            };
            Some((120. + ridge + ((x * 17 + y * 31) % 23) as f32) as f32)
        })
        .collect::<Vec<_>>();
    let grid = los::Grid::new(dimension, dimension, terrain).expect("synthetic grid");
    let mut durations = Vec::new();
    let mut visible = 0usize;
    let mut hash = 0u64;
    let mut fusion = Vec::new();
    for _ in 0..iterations {
        let start = Instant::now();
        let mut layers = Vec::new();
        for radar in 0..radars {
            let offset = (radar as isize - (radars as isize - 1) / 2) * (radius as isize / 8);
            let sectors = if sectors_override == 0 {
                ((std::f64::consts::TAU * radius as f64).ceil() as usize).max(8)
            } else {
                sectors_override
            };
            let cfg = los::LosConfig {
                radar_x: (radius as isize + offset) as usize,
                radar_y: radius,
                antenna_agl_m: 20.,
                cell_size_m: cell as f64,
                range_m: range,
                effective_earth_k: 4. / 3.,
                angular_sectors: sectors,
            };
            layers.push(los::compute_coverage(&grid, &cfg).expect("LOS"));
        }
        fusion = merge_counts(layers.iter().map(|l| &l.ground_visible), cells);
        visible = layers.iter().map(|l| l.ground_visible.count_ones()).sum();
        hash = hash_results(&layers, &fusion);
        durations.push(start.elapsed());
    }
    durations.sort();
    let median = durations[durations.len() / 2];
    let nominal = cells as u64 * radars as u64;
    println!("Radial production-core LOS benchmark");
    println!("dimensions={dimension}x{dimension} cells={cells} radars={radars} nominal_cells_radar={nominal}");
    for (i, d) in durations.iter().enumerate() {
        println!("iteration_sorted_{}={:.3}s", i + 1, d.as_secs_f64())
    }
    println!(
        "median={:.3}s throughput={:.2} M cells-radar/s visible_cells_radar={visible}",
        median.as_secs_f64(),
        nominal as f64 / median.as_secs_f64() / 1e6
    );
    let area = cells as f64 * (cell as f64).powi(2) / 1e6;
    let visible_area =
        fusion.iter().filter(|v| **v > 0).count() as f64 * (cell as f64).powi(2) / 1e6;
    let memory = grid.elevations_m.len() * std::mem::size_of::<Option<f32>>()
        + radars * cells * 2
        + radars * cells.div_ceil(8)
        + fusion.len();
    println!("surface_total_km2={area:.2} surface_visible_km2={visible_area:.2} estimated_memory_mib={:.2}",memory as f64/1048576.);
    println!(
        "threads={} hash={hash:016x}",
        env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "1 (standalone)".into())
    );
}
fn hash_results(layers: &[los::Coverage], fusion: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for layer in layers {
        for word in layer.ground_visible.words() {
            hash = (hash ^ word).wrapping_mul(0x100000001b3)
        }
        for height in layer.minimum_agl_m.iter().step_by(257) {
            hash = (hash ^ u64::from(*height)).wrapping_mul(0x100000001b3)
        }
    }
    for value in fusion.iter().step_by(257) {
        hash = (hash ^ u64::from(*value)).wrapping_mul(0x100000001b3)
    }
    hash
}
