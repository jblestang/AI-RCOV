use std::f64::consts::{FRAC_PI_4, TAU};

/// Shared immutable first-octant angular lookup table.
#[derive(Debug)]
pub struct AngularTable {
    bins: usize,
    ratios: Vec<f64>,
    angles: Vec<f64>,
    boundary_guard: f64,
}

impl AngularTable {
    pub fn new(max_offset: usize, bins: usize) -> Self {
        assert!(bins > 0);
        let samples = max_offset.max(1);
        let ratios = (0..=samples)
            .map(|i| i as f64 / samples as f64)
            .collect::<Vec<_>>();
        let angles = ratios.iter().map(|r| r.atan()).collect();
        // Linear interpolation error for atan on [0,1] is bounded by h²/8
        // times max |f''| (< 0.65). Include floating point bin scaling.
        let h = 1.0 / samples as f64;
        let boundary_guard = 0.082 * h * h + 16.0 * f64::EPSILON;
        Self {
            bins,
            ratios,
            angles,
            boundary_guard,
        }
    }

    pub fn bins(&self) -> usize {
        self.bins
    }

    #[inline]
    pub fn sector(&self, dx: i32, dy: i32) -> usize {
        if dx == 0 && dy == 0 {
            return 0;
        }
        let ax = dx.unsigned_abs() as f64;
        let ay = dy.unsigned_abs() as f64;
        let (small, large) = if ax <= ay { (ax, ay) } else { (ay, ax) };
        let ratio = small / large;
        let pos = ratio * (self.ratios.len() - 1) as f64;
        let lo = (pos.floor() as usize).min(self.angles.len() - 2);
        let fraction = pos - lo as f64;
        let base = self.angles[lo] + fraction * (self.angles[lo + 1] - self.angles[lo]);
        let first_quadrant = if ax >= ay {
            base
        } else {
            FRAC_PI_4 * 2.0 - base
        };
        let angle = match (dx >= 0, dy >= 0) {
            (true, true) => first_quadrant,
            (false, true) => std::f64::consts::PI - first_quadrant,
            (false, false) => std::f64::consts::PI + first_quadrant,
            (true, false) => TAU - first_quadrant,
        } % TAU;
        let scaled = angle * self.bins as f64 / TAU;
        let distance_to_boundary = (scaled - scaled.round()).abs() * TAU / self.bins as f64;
        if distance_to_boundary <= self.boundary_guard {
            return exact_sector(dx, dy, self.bins);
        }
        scaled.floor() as usize % self.bins
    }
}

fn exact_sector(dx: i32, dy: i32, bins: usize) -> usize {
    let angle = (dy as f64).atan2(dx as f64).rem_euclid(TAU);
    (angle * bins as f64 / TAU).floor() as usize % bins
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_exact_across_axes_quadrants_and_boundaries() {
        for bins in [1, 4, 17, 360, 27_928] {
            let table = AngularTable::new(4_445, bins);
            for y in -200..=200 {
                for x in -200..=200 {
                    if x != 0 || y != 0 {
                        assert_eq!(
                            table.sector(x, y),
                            exact_sector(x, y, bins),
                            "{x},{y}, bins={bins}"
                        );
                    }
                }
            }
        }
    }
}
