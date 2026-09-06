/// Disjoint ownership of the four corners is encoded by these side ranges:
/// top owns both top corners, right owns bottom-right, bottom owns bottom-left,
/// and left owns no corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// Calls `visit` for every integer cell in the radius-clipped square rings.
/// Side/circle intersection is computed before iteration, so square corners
/// outside the radar disk never invoke the callback.
pub fn for_each_ring_cell(radius_cells: i32, mut visit: impl FnMut(i32, i32, u32, Side)) {
    if radius_cells < 0 {
        return;
    }
    visit(0, 0, 0, Side::Top);
    let r2 = i64::from(radius_cells).pow(2);
    for r in 1..=radius_cells {
        let remaining = r2 - i64::from(r).pow(2);
        if remaining < 0 {
            continue;
        }
        let limit = integer_sqrt(remaining).min(i64::from(r)) as i32;
        // top: y=-r, includes both corners when they intersect the disk
        for x in -limit..=limit {
            visit(x, -r, r as u32, Side::Top);
        }
        // right/left exclude top and bottom rows, keeping sides disjoint
        let y_limit = limit.min(r - 1);
        for y in (-y_limit..=y_limit).rev() {
            visit(r, y, r as u32, Side::Right);
        }
        // bottom owns its clipped interval; distinct from top for r > 0
        for x in (-limit..=limit).rev() {
            visit(x, r, r as u32, Side::Bottom);
        }
        for y in -y_limit..=y_limit {
            visit(-r, y, r as u32, Side::Left);
        }
    }
}

pub fn ring_cells(radius_cells: i32) -> Vec<(i32, i32)> {
    let mut cells = Vec::new();
    for_each_ring_cell(radius_cells, |x, y, _, _| cells.push((x, y)));
    cells
}

fn integer_sqrt(value: i64) -> i64 {
    if value <= 0 {
        return 0;
    }
    let mut x = (value as f64).sqrt() as i64;
    while (x + 1) * (x + 1) <= value {
        x += 1;
    }
    while x * x > value {
        x -= 1;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    #[test]
    fn center_once() {
        assert_eq!(ring_cells(0), vec![(0, 0)]);
    }
    #[test]
    fn square_ring_has_8r_without_clipping() {
        for r in 1..20 {
            let mut count = 0;
            // radius large enough; select the ring explicitly.
            for_each_ring_cell(30, |_, _, ring, _| {
                if ring == r {
                    count += 1
                }
            });
            assert_eq!(count, 8 * r as usize);
        }
    }
    #[test]
    fn disk_is_exact_and_unique() {
        for radius in 0..20 {
            let cells = ring_cells(radius);
            let unique: HashSet<_> = cells.iter().copied().collect();
            assert_eq!(unique.len(), cells.len());
            let expected = (-radius..=radius)
                .flat_map(|y| (-radius..=radius).map(move |x| (x, y)))
                .filter(|(x, y)| x * x + y * y <= radius * radius)
                .count();
            assert_eq!(cells.len(), expected);
        }
    }
    #[test]
    fn callback_never_receives_outside_cell() {
        for_each_ring_cell(17, |x, y, _, _| assert!(x * x + y * y <= 17 * 17));
    }
}
