//! Zone fills as places to connect to (docs/ROUTER.md, "Pours"; DECISIONS D80).
//!
//! A pad of a poured net does not need a track to another pad: a via into the plane, or a short
//! track into the pour, joins it. Each fill island of a net is rasterized here into the grid
//! cells where a track of the net ending at the cell center lies on the fill's copper (the cell
//! center at least the track's half width plus [`MARGIN`] inside the fill). A connection whose
//! other side holds such a fill may end on any of those cells near its own end
//! ([`Engine::route_net`](super::engine::Engine::route_net)).
//!
//! The fill is the one computed before the run: tracks routed later across a pour split it, and
//! the pour is filled again around them. Whether a connection that ends on a pour is joined is
//! therefore read from the copper islands of the routed board, like every other connection, and
//! what a run leaves cut off is planned and routed again (`route_cut_off`).

use super::geo::{BoxF, P};
use super::grid::Grid;
use super::model::{Fill, RouterBoard};

/// Distance beyond the track's half width that a cell center keeps from the fill's boundary.
const MARGIN: f64 = 20_000.0;

/// A fill island of a net on one routing layer, as the grid cells a track of the net may end on.
#[derive(Clone, Debug)]
pub(crate) struct Pour {
    pub net: u32,
    /// Routing slot of the fill's layer.
    pub slot: usize,
    /// Copper island of the fill (as the terminals' `island`).
    pub island: usize,
    /// Runs of cells (row, first column, last column), by row then column.
    runs: Vec<(i32, i32, i32)>,
}

/// Where the horizontal line at `y` is inside the rings (even-odd): sorted, disjoint intervals.
fn spans(rings: &[Vec<P>], y: f64, out: &mut Vec<(f64, f64)>) {
    let mut xs: Vec<f64> = Vec::new();
    for r in rings {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            if (p.y <= y) != (q.y <= y) {
                xs.push(p.x + (y - p.y) * (q.x - p.x) / (q.y - p.y));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    out.clear();
    out.extend(xs.as_chunks::<2>().0.iter().map(|c| (c[0], c[1])));
}

/// The intersection of two sorted lists of disjoint intervals.
fn meet(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        let (lo, hi) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if lo < hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The cells of `grid` whose center lies at least `m` inside the rings (even-odd), as runs.
fn raster(rings: &[Vec<P>], bbox: BoxF, grid: &Grid, m: f64) -> Vec<(i32, i32, i32)> {
    let mut runs = Vec::new();
    let Some((_, y0, _, y1)) = grid.crange(&bbox) else { return runs };
    let (mut lo, mut mid, mut hi) = (Vec::new(), Vec::new(), Vec::new());
    for y in y0..=y1 {
        let yc = grid.cell(0, y).y;
        spans(rings, yc - m, &mut lo);
        spans(rings, yc, &mut mid);
        spans(rings, yc + m, &mut hi);
        for (a, b) in meet(&meet(&lo, &mid), &hi) {
            let x0 = (((a + m - grid.ox) / grid.g).ceil() as i64).max(0);
            let x1 = (((b - m - grid.ox) / grid.g).floor() as i64).min(i64::from(grid.w) - 1);
            if x0 <= x1 {
                runs.push((y, x0 as i32, x1 as i32));
            }
        }
    }
    runs
}

/// The pours of `fills` on the routing layers `slots`.
pub(crate) fn build(fills: &[Fill], rb: &RouterBoard, grid: &Grid, slots: &[usize]) -> Vec<Pour> {
    let mut out = Vec::new();
    for f in fills {
        let Some(slot) = slots.iter().position(|&l| l == f.layer) else { continue };
        let runs = raster(&f.rings, f.bbox, grid, rb.profile(f.net).hw + MARGIN);
        if !runs.is_empty() {
            out.push(Pour { net: f.net, slot, island: f.island, runs });
        }
    }
    out
}

impl Pour {
    /// The cells of the pour within `reach` cells of cell (`cx`, `cy`), in row then column order.
    pub fn cells_near(&self, cx: i32, cy: i32, reach: i32, out: &mut Vec<(i32, i32)>) {
        let from = self.runs.partition_point(|r| r.0 < cy - reach);
        for &(y, x0, x1) in &self.runs[from..] {
            if y > cy + reach {
                break;
            }
            for x in x0.max(cx - reach)..=x1.min(cx + reach) {
                out.push((x, y));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_keep_the_margin_and_skip_holes() {
        // A 10 x 10 square with a 2 x 2 hole in the middle, on a grid of pitch 1 at the origin.
        let sq = |a: f64, b: f64| vec![P::new(a, a), P::new(b, a), P::new(b, b), P::new(a, b)];
        let rings = vec![sq(0.0, 10.0), sq(4.0, 6.0)];
        let grid = Grid { ox: 0.0, oy: 0.0, g: 1.0, w: 12, h: 12 };
        let bbox = BoxF { min: P::new(0.0, 0.0), max: P::new(10.0, 10.0) };
        let runs = raster(&rings, bbox, &grid, 0.5);
        let inside = |x: i32, y: i32| runs.iter().any(|&(ry, x0, x1)| ry == y && x0 <= x && x <= x1);
        assert!(inside(1, 1) && inside(9, 9) && inside(3, 5) && inside(7, 5));
        assert!(!inside(0, 5), "on the boundary");
        assert!(!inside(5, 5) && !inside(4, 5) && !inside(6, 5), "in the hole or on its edge");
        assert!(!inside(5, 4) && !inside(5, 6), "the margin applies across rows too");
        let pour = Pour { net: 0, slot: 0, island: 0, runs };
        let mut near = Vec::new();
        pour.cells_near(2, 2, 1, &mut near);
        assert_eq!(near, vec![(1, 1), (2, 1), (3, 1), (1, 2), (2, 2), (3, 2), (1, 3), (2, 3), (3, 3)]);
    }
}
