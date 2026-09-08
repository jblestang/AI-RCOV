//! Standalone benchmark runner for hierarchical H3 coverage export.

use coverage_h3::{extract_h3_records, H3ExportConfig};
use coverage_storage::{Metadata, NO_DATA};
use std::{
    env,
    io::{self, Write},
    time::Instant,
};

fn print_usage() {
    eprintln!(
        r#"Usage:
  h3_bench [options]

Options:
  --res <0-15>         Target H3 resolution (default: 7)
  --start-res <0-15>   Coarse starting resolution for pruning (default: 5)
  --bucket <meters>    Quantize altitude to bucket step in meters (optional)
  --no-boundary        Omit the precalculated polygon boundary in output
  --cell <meters>      Benchmark synthetic cell size in meters (default: 90)
  --range <meters>     Benchmark synthetic range in meters (default: 400000)
  -h, --help           Print this help message
"#
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return Ok(());
    }

    let mut target_res = 7u8;
    let mut start_res = 5u8;
    let mut bucket = None;
    let mut include_boundary = true;
    let mut bench_cell_m = 90.0f64;
    let mut bench_range_m = 400_000.0f64;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--res" => {
                target_res = args.get(i + 1).ok_or("missing --res value")?.parse()?;
                i += 2;
            }
            "--start-res" => {
                start_res = args
                    .get(i + 1)
                    .ok_or("missing --start-res value")?
                    .parse()?;
                i += 2;
            }
            "--bucket" => {
                bucket = Some(args.get(i + 1).ok_or("missing --bucket value")?.parse()?);
                i += 2;
            }
            "--no-boundary" => {
                include_boundary = false;
                i += 1;
            }
            "--cell" => {
                bench_cell_m = args.get(i + 1).ok_or("missing --cell value")?.parse()?;
                i += 2;
            }
            "--range" => {
                bench_range_m = args.get(i + 1).ok_or("missing --range value")?.parse()?;
                i += 2;
            }
            other => {
                return Err(format!("unknown option: {other}").into());
            }
        }
    }

    let config = H3ExportConfig {
        target_resolution: target_res,
        start_resolution: start_res,
        altitude_bucket_m: bucket,
        include_boundary,
    };

    let radius = (bench_range_m / bench_cell_m).ceil() as usize;
    let dimension = radius * 2 + 1;
    let cells = dimension
        .checked_mul(dimension)
        .expect("grid size overflow");

    eprintln!(
        "Generating synthetic benchmark raster: {}x{} ({} cells, range={:.1} km, cell={:.0} m)...",
        dimension,
        dimension,
        cells,
        bench_range_m / 1000.0,
        bench_cell_m
    );

    let half = (dimension as f64 * bench_cell_m) / 2.0;
    let metadata = Metadata {
        radar_id: "bench-radar".to_string(),
        los_algorithm_version: 3,
        radar_config_hash: "benchhash".to_string(),
        terrain_hash: "benchterrain".to_string(),
        calculated_at: "2026-09-08T00:00:00Z".to_string(),
        crs: "+proj=aeqd +lat_0=43.5 +lon_0=6.5 +R=6371000 +units=m".to_string(),
        origin: [-half, -half],
        resolution_m: bench_cell_m,
        extent: [-half, -half, half, half],
        width: dimension as u32,
        height: dimension as u32,
        range_m: bench_range_m,
        effective_earth_k: 1.3333333333333333,
        nodata: NO_DATA,
    };

    // Realistic synthetic visibility: circular mask with mountain shadow wedges
    let mut heights = vec![NO_DATA; cells];
    let mut visible_count = 0usize;

    for r in 0..dimension {
        let dy = (r as isize - radius as isize) as f64 * bench_cell_m;
        for c in 0..dimension {
            let dx = (c as isize - radius as isize) as f64 * bench_cell_m;
            let dist = dx.hypot(dy);
            if dist <= bench_range_m {
                let angle = dy.atan2(dx).to_degrees();
                let is_shadow = (45.0..=85.0).contains(&angle) && dist > 80_000.0;
                if !is_shadow {
                    let h =
                        (50.0 + (dist / 1000.0) * 1.5 + ((c * 17 + r * 31) % 150) as f64) as u16;
                    heights[r * dimension + c] = h;
                    visible_count += 1;
                }
            }
        }
    }

    eprintln!(
        "Synthetic grid ready. Visible pixels: {} / {} ({:.1}%)",
        visible_count,
        cells,
        (visible_count as f64 / cells as f64) * 100.0
    );

    // Warmup
    eprintln!("Warming up...");
    let _ = extract_h3_records(&metadata, &heights, &config)?;

    // Benchmark iterations
    let iterations = 3;
    let mut durations = Vec::new();
    let mut final_stats = None;

    for iter in 1..=iterations {
        let start = Instant::now();
        let (records, stats) = extract_h3_records(&metadata, &heights, &config)?;
        let elapsed = start.elapsed();
        durations.push(elapsed);
        eprintln!(
            "Iteration {}: {:.3}s (exported {} H3 cells)",
            iter,
            elapsed.as_secs_f64(),
            records.len()
        );
        final_stats = Some(stats);
    }

    durations.sort();
    let median = durations[iterations / 2];
    let stats = final_stats.unwrap();

    let mpix_per_sec = (cells as f64 / 1_000_000.0) / median.as_secs_f64();
    let cells_per_sec = (stats.exported_cells as f64) / median.as_secs_f64();

    println!("\n=== H3 Hierarchical Export Benchmark Results ===");
    println!(
        "Raster dimension:     {}x{} ({} pixels)",
        dimension, dimension, cells
    );
    println!("Target H3 resolution: {}", config.target_resolution);
    println!("Start resolution:     {}", config.start_resolution);
    println!("Median time:          {:.3}s", median.as_secs_f64());
    println!("Raster throughput:    {:.2} Mpix/s", mpix_per_sec);
    println!("H3 export throughput: {:.0} cells/s", cells_per_sec);
    println!("Candidate roots:      {}", stats.total_candidate_roots);
    println!("Pruned coarse cells:  {}", stats.pruned_coarse_cells);
    println!("Visited leaf cells:   {}", stats.visited_leaf_cells);
    println!("Exported H3 cells:    {}", stats.exported_cells);
    if let (Some(min), Some(max)) = (stats.min_altitude_m, stats.max_altitude_m) {
        println!("Altitude floor range: {} m to {} m", min, max);
    }

    // Benchmark JSONL output serialization to sink
    let start_io = Instant::now();
    let mut sink = io::sink();
    let (records, _) = extract_h3_records(&metadata, &heights, &config)?;
    for record in &records {
        serde_json::to_writer(&mut sink, record)?;
        sink.write_all(b"\n")?;
    }
    let io_elapsed = start_io.elapsed();
    println!(
        "JSONL serialization:  {:.3}s for {} lines",
        io_elapsed.as_secs_f64(),
        records.len()
    );

    Ok(())
}
