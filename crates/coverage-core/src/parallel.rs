use crate::{compute_coverage, Coverage, CoverageError, Grid, LosConfig};
use rayon::prelude::*;

#[derive(Clone, Debug)]
pub struct ParallelConfig {
    pub max_threads: usize,
    pub memory_budget_bytes: usize,
    pub bytes_per_cell: usize,
}
impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            max_threads: std::thread::available_parallelism().map_or(1, usize::from),
            memory_budget_bytes: 2 * 1024 * 1024 * 1024,
            bytes_per_cell: 3,
        }
    }
}
impl ParallelConfig {
    pub fn concurrent_radars(&self, cells: usize, radars: usize) -> usize {
        let per_result = cells.saturating_mul(self.bytes_per_cell).max(1);
        let by_memory = (self.memory_budget_bytes / per_result).max(1);
        radars.min(self.max_threads.max(1)).min(by_memory).max(1)
    }
}

/// Computes independent radar layers with a bounded Rayon pool. Each task owns
/// its horizons, bitset and minimum-height grid; no LOS-loop mutex is shared.
/// Results are returned in input order for deterministic persistence/fusion.
pub fn compute_coverages_bounded(
    grid: &Grid,
    configs: &[LosConfig],
    parallel: &ParallelConfig,
) -> Result<Vec<Coverage>, CoverageError> {
    if configs.is_empty() {
        return Ok(Vec::new());
    }
    let workers = parallel.concurrent_radars(grid.width.saturating_mul(grid.height), configs.len());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .map_err(|_| CoverageError::InvalidConfig)?;
    pool.install(|| {
        configs
            .par_iter()
            .map(|config| compute_coverage(grid, config))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(x: usize) -> LosConfig {
        LosConfig {
            radar_x: x,
            radar_y: 1,
            antenna_agl_m: 10.,
            cell_size_m: 90.,
            range_m: 180.,
            effective_earth_k: 4. / 3.,
        }
    }
    #[test]
    fn memory_budget_limits_workers() {
        let p = ParallelConfig {
            max_threads: 8,
            memory_budget_bytes: 300,
            bytes_per_cell: 3,
        };
        assert_eq!(p.concurrent_radars(100, 8), 1)
    }
    #[test]
    fn parallel_matches_sequential() {
        let grid = Grid::new(5, 3, vec![Some(0.); 15]).unwrap();
        let configs = [config(1), config(3)];
        let expected = configs
            .iter()
            .map(|c| compute_coverage(&grid, c).unwrap())
            .collect::<Vec<_>>();
        let actual = compute_coverages_bounded(
            &grid,
            &configs,
            &ParallelConfig {
                max_threads: 2,
                memory_budget_bytes: 10000,
                bytes_per_cell: 3,
            },
        )
        .unwrap();
        for (i, c) in actual.iter().enumerate() {
            assert_eq!(c.ground_visible, expected[i].ground_visible);
            assert_eq!(c.minimum_agl_m, expected[i].minimum_agl_m)
        }
    }
}
