use std::{
    env,
    time::{Duration, Instant},
};
#[allow(dead_code)]
#[path = "../../crates/coverage-core/src/rings.rs"]
mod rings;
fn main() {
    let cell = env::var("RADAR_BENCH_CELL_M")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(90);
    let radars = env::var("RADAR_BENCH_RADARS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);
    let iterations = env::var("RADAR_BENCH_ITERATIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3);
    let sectors = env::var("RADAR_BENCH_SECTORS_PER_RADAR")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3);
    let radius = 400_000u32.div_ceil(cell) as i32;
    let dimension = radius as u64 * 2 + 1;
    let mut times = Vec::new();
    let mut final_hash = 0u64;
    let mut visited = 0u64;
    for _ in 0..iterations {
        let start = Instant::now();
        let mut hash = 0xcbf29ce484222325u64;
        let mut n = 0u64;
        for radar in 0..radars {
            rings::for_each_ring_cell(radius, |x, y, _, _| {
                let sector = ((x as i64 * 73856093) ^ (y as i64 * 19349663)).unsigned_abs()
                    as usize
                    % sectors.max(1);
                hash ^= (x as u32 as u64) << 32 | y as u32 as u64;
                hash = hash
                    .wrapping_mul(0x100000001b3)
                    .wrapping_add((sector as u64) ^ radar as u64);
                n += 1;
            });
        }
        times.push(start.elapsed());
        final_hash = hash;
        visited = n;
    }
    times.sort();
    let median = times[times.len() / 2];
    let throughput = visited as f64 / median.as_secs_f64() / 1e6;
    println!("Radial deterministic standalone benchmark");
    println!(
        "cell={} m dimensions={}x{} nominal_cells_radar={} visited_cells_radar={}",
        cell,
        dimension,
        dimension,
        dimension * dimension * radars as u64,
        visited
    );
    for (i, d) in times.iter().enumerate() {
        println!("iteration_sorted_{}={:.3}s", i + 1, d.as_secs_f64())
    }
    println!(
        "median={:.3}s throughput={:.2} M cells-radar/s sectors={} threads={}",
        median.as_secs_f64(),
        throughput,
        sectors,
        env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "n/a".into())
    );
    println!(
        "estimated_bitset_mib={:.2} hash={:016x}",
        dimension as f64 * dimension as f64 / 8.0 / 1048576.0,
        final_hash
    );
}
#[allow(dead_code)]
fn _duration(_: Duration) {}
