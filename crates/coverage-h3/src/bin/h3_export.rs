//! Production CLI tool for exporting radar coverage (.rhgt) to H3 JSONL.

use coverage_h3::{export_rhgt_file_to_h3_jsonl, H3ExportConfig};
use std::{
    env,
    fs::File,
    io::{self, BufWriter},
    path::PathBuf,
    time::Instant,
};

fn print_usage() {
    eprintln!(
        r#"Usage:
  h3_export <input.rhgt> [output.jsonl] [options]

Arguments:
  <input.rhgt>         Path to input .rhgt radar coverage file
  [output.jsonl]       Path to output .jsonl file (default: stdout)

Options:
  --res <0-15>         Target H3 resolution (default: 7)
  --start-res <0-15>   Coarse starting resolution for hierarchical pruning (default: 5)
  --bucket <meters>    Quantize altitude into bucket steps in meters (optional)
  --no-boundary        Omit the precalculated polygon boundary array in output
  -q, --quiet          Suppress progress and summary messages on stderr
  -h, --help           Print this help message
"#
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return Ok(());
    }

    let mut target_res = 7u8;
    let mut start_res = 5u8;
    let mut bucket = None;
    let mut include_boundary = true;
    let mut quiet = false;
    let mut input_path = None;
    let mut output_path = None;

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
            "-q" | "--quiet" => {
                quiet = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option: {other}").into());
            }
            pos => {
                if input_path.is_none() {
                    input_path = Some(PathBuf::from(pos));
                } else if output_path.is_none() {
                    output_path = Some(PathBuf::from(pos));
                } else {
                    return Err(format!("unexpected argument: {pos}").into());
                }
                i += 1;
            }
        }
    }

    let input = input_path.ok_or("missing input .rhgt file")?;
    let config = H3ExportConfig {
        target_resolution: target_res,
        start_resolution: start_res,
        altitude_bucket_m: bucket,
        include_boundary,
    };

    let start = Instant::now();

    let stats = match output_path {
        Some(ref out) => {
            let file = File::create(out)?;
            let mut writer = BufWriter::new(file);
            let s = export_rhgt_file_to_h3_jsonl(&input, &config, &mut writer)?;
            if !quiet {
                eprintln!(
                    "Exported {} H3 cells to {}",
                    s.exported_cells,
                    out.display()
                );
            }
            s
        }
        None => {
            let stdout = io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            export_rhgt_file_to_h3_jsonl(&input, &config, &mut writer)?
        }
    };

    if !quiet {
        let elapsed = start.elapsed();
        eprintln!(
            "Done in {:.3}s (roots={}, pruned={}, visited={}, exported={})",
            elapsed.as_secs_f64(),
            stats.total_candidate_roots,
            stats.pruned_coarse_cells,
            stats.visited_leaf_cells,
            stats.exported_cells
        );
    }

    Ok(())
}
