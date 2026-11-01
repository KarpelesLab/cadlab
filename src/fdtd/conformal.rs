//! Conformal copper: how much of each mesh edge and face curved or diagonal copper covers.
//!
//! The geometry side of the Dey–Mittra treatment in [`super::engine`]. Copper is a set of prisms
//! (polygons extruded between two z values). Only the cells crossed by a slanted polygon edge are
//! touched, so axis-aligned copper keeps its edges exactly as voxelized (and the thirds rule that
//! goes with them): a slanted edge's open length and a face's open area come from exact
//! intersections of horizontal or vertical lines with the polygons.

use std::collections::{BTreeMap, BTreeSet};

use super::engine::Structure;
use crate::geom::poly::{self, Point, Polygon};

/// A polygon set extruded between two heights (nm; equal: a sheet).
#[derive(Clone, Debug)]
pub struct Prism {
    /// The cross-section.
    pub shape: Vec<Polygon>,
    /// Bottom and top.
    pub z: (i64, i64),
}

/// A disc as a polygon of the same area (for a plated hole's barrel).
pub fn disc(x: i64, y: i64, r: i64) -> Polygon {
    const SIDES: usize = 48;
    let t = std::f64::consts::TAU / SIDES as f64;
    let rr = r as f64 * (t / t.sin()).sqrt();
    let ring: Vec<Point> = (0..SIDES)
        .map(|k| {
            let a = t * k as f64;
            Point::new(x + (rr * a.cos()).round() as i64, y + (rr * a.sin()).round() as i64)
        })
        .collect();
    Polygon::new(ring, Vec::new())
}

/// Open or covered parts below this fraction of an edge or face are not worth a cut.
const SNAP: f64 = 0.02;
/// Sub-rows integrating a face's open area.
const ROWS: usize = 16;

/// What [`apply`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Edges partly covered.
    pub partial_edges: usize,
    /// Faces partly covered.
    pub partial_faces: usize,
}

/// A polygon edge, with its polygon's index.
#[derive(Clone, Copy)]
struct Seg {
    poly: u32,
    a: (f64, f64),
    b: (f64, f64),
}

/// A prism's outline indexed by mesh rows along one axis: horizontal chords in x at a given y
/// (or, swapped, vertical chords in y at a given x).
struct Scan {
    segs: Vec<Seg>,
    /// Per row between consecutive `lines`: the segments overlapping it.
    rows: Vec<Vec<u32>>,
}

impl Scan {
    /// `swap`: chords along y (coordinates swapped).
    fn new(shape: &[Polygon], lines: &[i64], swap: bool) -> Scan {
        let mut segs = Vec::new();
        for (pi, q) in shape.iter().enumerate() {
            for r in q.rings() {
                for (a, b) in r.edges() {
                    let (a, b) = if swap {
                        ((a.y as f64, a.x as f64), (b.y as f64, b.x as f64))
                    } else {
                        ((a.x as f64, a.y as f64), (b.x as f64, b.y as f64))
                    };
                    if a.1 != b.1 {
                        segs.push(Seg { poly: pi as u32, a, b });
                    }
                }
            }
        }
        let mut rows = vec![Vec::new(); lines.len().saturating_sub(1)];
        for (i, s) in segs.iter().enumerate() {
            let (lo, hi) = (s.a.1.min(s.b.1), s.a.1.max(s.b.1));
            let first = lines.partition_point(|&v| (v as f64) <= lo).saturating_sub(1);
            for (row, w) in rows.iter_mut().zip(lines.windows(2)).skip(first) {
                if w[0] as f64 >= hi {
                    break;
                }
                row.push(i as u32);
            }
        }
        Scan { segs, rows }
    }

    /// Adds the intervals the prism covers along the line at `y`, which lies in `row`.
    fn chords(&self, row: usize, y: f64, out: &mut Vec<(f64, f64)>) {
        let Some(list) = self.rows.get(row) else { return };
        let mut xs: Vec<(u32, f64)> = Vec::new();
        for &i in list {
            let s = &self.segs[i as usize];
            let (a, b) = if s.a.1 <= s.b.1 { (s.a, s.b) } else { (s.b, s.a) };
            if a.1 <= y && y < b.1 {
                xs.push((s.poly, a.0 + (y - a.1) * (b.0 - a.0) / (b.1 - a.1)));
            }
        }
        xs.sort_by(|p, q| p.0.cmp(&q.0).then(p.1.total_cmp(&q.1)));
        for [a, b] in xs.as_chunks::<2>().0 {
            if a.0 == b.0 {
                out.push((a.1, b.1));
            }
        }
    }
}

/// Covered length of `[lo, hi]` by intervals sorted by start.
fn covered(intervals: &[(f64, f64)], lo: f64, hi: f64) -> f64 {
    let (mut sum, mut at) = (0.0, lo);
    for &(a, b) in intervals {
        if a >= hi {
            break;
        }
        let (x, y) = (a.max(at), b.min(hi));
        if y > x {
            sum += y - x;
            at = y;
        }
    }
    sum
}

/// The middle of the part of `[lo, hi]` that the intervals cover.
fn covered_middle(intervals: &[(f64, f64)], lo: f64, hi: f64) -> f64 {
    let (mut sum, mut moment, mut at) = (0.0, 0.0, lo);
    for &(a, b) in intervals {
        if a >= hi {
            break;
        }
        let (x, y) = (a.max(at), b.min(hi));
        if y > x {
            sum += y - x;
            moment += (y - x) * (x + y) / 2.0;
            at = y;
        }
    }
    if sum > 0.0 { moment / sum } else { (lo + hi) / 2.0 }
}

/// The copper across an edge's dual cell: the chord at `at` (along the edge, in cell `row`) within
/// half a cell on either side of mesh line `line`.
fn across(scans: &[&Scan], row: usize, at: f64, lines: &[i64], line: usize) -> f64 {
    let mut c = Vec::new();
    for s in scans {
        s.chords(row, at, &mut c);
    }
    let c = sorted(c);
    let v = lines[line] as f64;
    let lo = if line > 0 { v - (lines[line] - lines[line - 1]) as f64 / 2.0 } else { v };
    let hi = if line + 1 < lines.len() { v + (lines[line + 1] - lines[line]) as f64 / 2.0 } else { v };
    covered(&c, lo, hi)
}

/// The outline around z edges: each piece of a slab's polygons goes to the nearest conductor z
/// edge among the corners of its cell (and their neighbours), so the widths add up to the true
/// perimeter. Returns (i, j, width in nm) for the nodes receiving slanted outline.
#[allow(clippy::needless_range_loop)]
fn outline_widths(
    ids: &[u32],
    prisms: &[Prism],
    xs: &[i64],
    ys: &[i64],
    conductor: impl Fn(usize, usize) -> bool,
) -> Vec<(usize, usize, f64)> {
    let mut total: BTreeMap<(usize, usize), (f64, bool)> = BTreeMap::new();
    for &id in ids {
        let others: Vec<&Prism> = ids.iter().filter(|o| **o != id).map(|o| &prisms[*o as usize]).collect();
        for q in &prisms[id as usize].shape {
            for r in q.rings() {
                for (a, b) in r.edges() {
                    let slanted = a.x != b.x && a.y != b.y;
                    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                    let len = dx.hypot(dy);
                    let mx = a.x + (b.x - a.x) / 2;
                    let ix = xs.partition_point(|&v| v <= mx).clamp(1, xs.len() - 1);
                    let my = a.y + (b.y - a.y) / 2;
                    let iy = ys.partition_point(|&v| v <= my).clamp(1, ys.len() - 1);
                    let step = ((xs[ix] - xs[ix - 1]).min(ys[iy] - ys[iy - 1]) as f64 / 8.0).max(1.0);
                    let pieces = ((len / step).ceil() as usize).max(1);
                    let dl = len / pieces as f64;
                    for m in 0..pieces {
                        let t = (m as f64 + 0.5) / pieces as f64;
                        let (x, y) = (a.x as f64 + t * dx, a.y as f64 + t * dy);
                        if !others.is_empty()
                            && others.iter().any(|o| {
                                poly::locate(&o.shape, Point::new(x as i64, y as i64)) != poly::Location::Outside
                            })
                        {
                            continue;
                        }
                        if x < xs[0] as f64
                            || y < ys[0] as f64
                            || x > xs[xs.len() - 1] as f64
                            || y > ys[ys.len() - 1] as f64
                        {
                            continue;
                        }
                        let i = xs.partition_point(|&v| (v as f64) <= x).saturating_sub(1).min(xs.len() - 2);
                        let j = ys.partition_point(|&v| (v as f64) <= y).saturating_sub(1).min(ys.len() - 2);
                        let mut best: Option<(f64, usize, usize)> = None;
                        for jj in j.saturating_sub(1)..=(j + 2).min(ys.len() - 1) {
                            for ii in i.saturating_sub(1)..=(i + 2).min(xs.len() - 1) {
                                if conductor(ii, jj) {
                                    let d = (xs[ii] as f64 - x).hypot(ys[jj] as f64 - y);
                                    if best.is_none_or(|b| d < b.0) {
                                        best = Some((d, ii, jj));
                                    }
                                }
                            }
                        }
                        if let Some((_, ii, jj)) = best {
                            let e = total.entry((jj, ii)).or_insert((0.0, false));
                            e.0 += dl;
                            e.1 |= slanted;
                        }
                    }
                }
            }
        }
    }
    total.into_iter().filter(|x| x.1.1).map(|((j, i), (w, _))| (i, j, w)).collect()
}

/// The cells each prism's slanted polygon edges cross: (row along y, column along x).
#[allow(clippy::needless_range_loop)]
fn slanted_cells(shape: &[Polygon], xs: &[i64], ys: &[i64]) -> BTreeSet<(u32, u32)> {
    let mut out = BTreeSet::new();
    for q in shape {
        for r in q.rings() {
            for (a, b) in r.edges() {
                if a.x == b.x || a.y == b.y {
                    continue;
                }
                let (a, b) = if a.y <= b.y { (a, b) } else { (b, a) };
                let (ax, ay, bx, by) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64);
                let first = ys.partition_point(|&v| v <= a.y).saturating_sub(1);
                for j in first..ys.len().saturating_sub(1) {
                    if ys[j] >= b.y {
                        break;
                    }
                    let at = |y: f64| ax + (y.clamp(ay, by) - ay) * (bx - ax) / (by - ay);
                    let (u, v) = (at(ys[j] as f64), at(ys[j + 1] as f64));
                    let (lo, hi) = (u.min(v), u.max(v));
                    let i0 = xs.partition_point(|&x| (x as f64) <= lo).saturating_sub(1);
                    for i in i0..xs.len().saturating_sub(1) {
                        if xs[i] as f64 >= hi {
                            break;
                        }
                        out.insert((j as u32, i as u32));
                    }
                }
            }
        }
    }
    out
}

/// The open fractions of a set of prisms covering one plane: x edges, y edges (node `(i, j)`),
/// faces normal to z (cell `(i, j)`); (j, i) → fraction.
#[derive(Default)]
struct Cut {
    x: BTreeMap<(u32, u32), f64>,
    y: BTreeMap<(u32, u32), f64>,
    z: BTreeMap<(u32, u32), f64>,
    /// Copper across the dual cell of x and y edges that are not all open (nm).
    wx: BTreeMap<(u32, u32), f64>,
    wy: BTreeMap<(u32, u32), f64>,
}

fn snap(o: f64) -> f64 {
    if o < SNAP {
        0.0
    } else if o > 1.0 - SNAP {
        1.0
    } else {
        o
    }
}

struct Indexed {
    h: Scan,
    v: Scan,
    cells: BTreeSet<(u32, u32)>,
}

fn sorted(mut v: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    v.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.total_cmp(&q.1)));
    v
}

/// The union along a line through mesh line `at` of index `line` (`usize::MAX` for none): half a
/// nanometer to each side, so copper ending on the line counts, as it does when voxelizing.
fn along_line(scans: &[&Scan], line: usize, at: i64) -> Vec<(f64, f64)> {
    let mut v = Vec::new();
    for s in scans {
        s.chords(line, at as f64 + 0.5, &mut v);
        if line > 0 {
            s.chords(line - 1, at as f64 - 0.5, &mut v);
        }
    }
    sorted(v)
}

/// Computes the cut of a set of prisms.
fn cut(set: &[&Indexed], xs: &[i64], ys: &[i64]) -> Cut {
    let mut out = Cut::default();
    let cells: BTreeSet<(u32, u32)> = set.iter().flat_map(|p| p.cells.iter().copied()).collect();
    if cells.is_empty() {
        return out;
    }
    // Edges by line, so each line is intersected once.
    let mut by_y: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    let mut by_x: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for &(j, i) in &cells {
        by_y.entry(j).or_default().insert(i);
        by_y.entry(j + 1).or_default().insert(i);
        by_x.entry(i).or_default().insert(j);
        by_x.entry(i + 1).or_default().insert(j);
    }
    let h: Vec<&Scan> = set.iter().map(|p| &p.h).collect();
    let v: Vec<&Scan> = set.iter().map(|p| &p.v).collect();
    for (&j, list) in &by_y {
        let chords = along_line(&h, j as usize, ys[j as usize]);
        for &i in list {
            let (a, b) = (xs[i as usize] as f64, xs[i as usize + 1] as f64);
            let o = snap(1.0 - covered(&chords, a, b) / (b - a));
            out.x.insert((j, i), o);
            if o < 1.0 {
                out.wx.insert((j, i), across(&v, i as usize, covered_middle(&chords, a, b), ys, j as usize));
            }
        }
    }
    for (&i, list) in &by_x {
        let chords = along_line(&v, i as usize, xs[i as usize]);
        for &j in list {
            let (a, b) = (ys[j as usize] as f64, ys[j as usize + 1] as f64);
            let o = snap(1.0 - covered(&chords, a, b) / (b - a));
            out.y.insert((j, i), o);
            if o < 1.0 {
                out.wy.insert((j, i), across(&h, j as usize, covered_middle(&chords, a, b), xs, i as usize));
            }
        }
    }
    // Faces: the mean covered length over sub-rows.
    let mut by_row: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(j, i) in &cells {
        by_row.entry(j).or_default().push(i);
    }
    for (&j, list) in &by_row {
        let (y0, y1) = (ys[j as usize] as f64, ys[j as usize + 1] as f64);
        let mut sums = vec![0.0; list.len()];
        for m in 0..ROWS {
            let y = y0 + (y1 - y0) * (m as f64 + 0.5) / ROWS as f64;
            let mut c = Vec::new();
            for s in &h {
                s.chords(j as usize, y, &mut c);
            }
            let c = sorted(c);
            for (sum, &i) in sums.iter_mut().zip(list) {
                let (a, b) = (xs[i as usize] as f64, xs[i as usize + 1] as f64);
                *sum += covered(&c, a, b) / (b - a);
            }
        }
        for (sum, &i) in sums.iter().zip(list) {
            out.z.insert((j, i), snap(1.0 - sum / ROWS as f64));
        }
    }
    out
}

/// The slanted tracks of a shape too narrow for the mesh: polygons whose width (twice area over
/// perimeter) is below `cells` of the larger cell under their longest slanted edge, that edge being
/// at least three widths long (a track, not a round pad). Their width and that cell, in nm.
pub fn narrow(shape: &[Polygon], xs: &[i64], ys: &[i64], cells: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for q in shape {
        let (mut area, mut perimeter) = (0.0, 0.0);
        let mut longest: Option<(f64, Point, Point)> = None;
        for (ri, r) in q.rings().enumerate() {
            let mut twice = 0.0;
            for (a, b) in r.edges() {
                let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                let len = dx.hypot(dy);
                perimeter += len;
                twice += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
                if a.x != b.x && a.y != b.y && longest.is_none_or(|l| len > l.0) {
                    longest = Some((len, a, b));
                }
            }
            area += if ri == 0 { twice.abs() / 2.0 } else { -twice.abs() / 2.0 };
        }
        let Some((length, a, b)) = longest else { continue };
        let cell_at = |l: &[i64], v: i64| {
            let i = l.partition_point(|&x| x <= v).clamp(1, l.len() - 1);
            (l[i] - l[i - 1]) as f64
        };
        let cell = cell_at(xs, a.x + (b.x - a.x) / 2).max(cell_at(ys, a.y + (b.y - a.y) / 2));
        let width = 2.0 * area / perimeter;
        if width < cells * cell && length >= 3.0 * width {
            out.push((width, cell));
        }
    }
    out
}

/// Records on the structure how the prisms cut its edges and faces. Call after voxelizing the
/// copper: edges in the cut cells take their exact coverage instead of the staircase's.
pub fn apply(s: &mut Structure, prisms: &[Prism]) -> Stats {
    let lines = s.lines().clone();
    let [xs, ys, zs] = &lines;
    let indexed: Vec<Indexed> = prisms
        .iter()
        .map(|p| Indexed {
            h: Scan::new(&p.shape, ys, false),
            v: Scan::new(&p.shape, xs, true),
            cells: slanted_cells(&p.shape, xs, ys),
        })
        .collect();
    let mut cache: BTreeMap<Vec<u32>, Cut> = BTreeMap::new();
    let get = |ids: Vec<u32>, cache: &mut BTreeMap<Vec<u32>, Cut>| {
        cache.entry(ids.clone()).or_insert_with(|| {
            let set: Vec<&Indexed> = ids.iter().map(|&i| &indexed[i as usize]).collect();
            cut(&set, xs, ys)
        });
        ids
    };
    let mut stats = Stats::default();
    for k in 0..zs.len() {
        // Prisms through this plane: its in-plane edges and faces.
        let plane: Vec<u32> = (0..prisms.len() as u32)
            .filter(|&i| {
                let z = prisms[i as usize].z;
                z.0 <= zs[k] && zs[k] <= z.1
            })
            .collect();
        if !plane.is_empty() {
            let ids = get(plane, &mut cache);
            let c = &cache[&ids];
            for (&(j, i), &o) in &c.x {
                s.set_open_edge(0, [i as usize, j as usize, k], o);
                stats.partial_edges += usize::from(o > 0.0 && o < 1.0);
            }
            for (&(j, i), &o) in &c.y {
                s.set_open_edge(1, [i as usize, j as usize, k], o);
                stats.partial_edges += usize::from(o > 0.0 && o < 1.0);
            }
            for (&(j, i), &o) in &c.z {
                s.set_open_face(2, [i as usize, j as usize, k], o);
                stats.partial_faces += usize::from(o > 0.0 && o < 1.0);
            }
            for (&(j, i), &w) in &c.wx {
                s.set_surface_width(0, [i as usize, j as usize, k], w * 1e-9);
            }
            for (&(j, i), &w) in &c.wy {
                s.set_surface_width(1, [i as usize, j as usize, k], w * 1e-9);
            }
        }
        // Prisms through the whole cell above it: its vertical faces follow the edges' cover.
        if k + 1 < zs.len() {
            let slab: Vec<u32> = (0..prisms.len() as u32)
                .filter(|&i| {
                    let z = prisms[i as usize].z;
                    z.0 <= zs[k] && zs[k + 1] <= z.1
                })
                .collect();
            if !slab.is_empty() {
                let ids = get(slab, &mut cache);
                let c = &cache[&ids];
                for (j, i, w) in outline_widths(&ids, prisms, xs, ys, |i, j| s.is_pec(2, [i, j, k])) {
                    s.set_surface_width(2, [i, j, k], w * 1e-9);
                }
                // A face normal to y lies along an x edge, one normal to x along a y edge.
                for (&(j, i), &o) in &c.x {
                    s.set_open_face(1, [i as usize, j as usize, k], o);
                    stats.partial_faces += usize::from(o > 0.0 && o < 1.0);
                }
                for (&(j, i), &o) in &c.y {
                    s.set_open_face(0, [i as usize, j as usize, k], o);
                    stats.partial_faces += usize::from(o > 0.0 && o < 1.0);
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: i64, d: i64) -> Vec<i64> {
        (0..=n).map(|i| i * d).collect()
    }

    fn triangle() -> Polygon {
        // Below the diagonal of the first two cells, from (0, 0) to (2000, 2000).
        let ring: Vec<Point> = [(0, 0), (2000, 0), (2000, 2000)].iter().map(|&(x, y)| Point::new(x, y)).collect();
        Polygon::new(ring, Vec::new())
    }

    #[test]
    fn exact_lengths_and_areas() {
        let (xs, ys) = (lines(4, 1000), lines(4, 1000));
        let shape = [triangle()];
        let idx = Indexed {
            h: Scan::new(&shape, &ys, false),
            v: Scan::new(&shape, &xs, true),
            cells: slanted_cells(&shape, &xs, &ys),
        };
        assert_eq!(idx.cells, [(0, 0), (1, 1)].into_iter().collect());
        let c = cut(&[&idx], &xs, &ys);
        // The diagonal halves its cells' faces.
        assert!((c.z[&(0, 0)] - 0.5).abs() < 1e-3, "{}", c.z[&(0, 0)]);
        assert!((c.z[&(1, 1)] - 0.5).abs() < 1e-3);
        // The bottom edge of the first cell lies on the copper's edge: covered.
        assert_eq!(c.x[&(0, 0)], 0.0);
        // Its top edge meets the triangle only in its corner: open.
        assert_eq!(c.x[&(1, 0)], 1.0);
        assert_eq!(c.y[&(0, 0)], 1.0);
        assert_eq!(c.y[&(0, 1)], 0.0);
    }

    #[test]
    fn cut_edges_are_fractions() {
        // A line crossing the edges in the middle of a cell: y = x - 500.
        let ring: Vec<Point> = [(500, 0), (3000, 0), (3000, 2500)].iter().map(|&(x, y)| Point::new(x, y)).collect();
        let shape = [Polygon::new(ring, Vec::new())];
        let (xs, ys) = (lines(4, 1000), lines(4, 1000));
        let idx = Indexed {
            h: Scan::new(&shape, &ys, false),
            v: Scan::new(&shape, &xs, true),
            cells: slanted_cells(&shape, &xs, &ys),
        };
        let c = cut(&[&idx], &xs, &ys);
        // The x edge at y = 1000 from x = 1000 is open up to x = 1500.
        assert!((c.x[&(1, 1)] - 0.5).abs() < 1e-3, "{}", c.x[&(1, 1)]);
        // The y edge at x = 2000 from y = 1000 is covered up to y = 1500.
        assert!((c.y[&(1, 2)] - 0.5).abs() < 1e-3, "{}", c.y[&(1, 2)]);
    }

    #[test]
    fn perimeter_goes_to_the_nearest_conductor() {
        let (xs, ys) = (lines(40, 1000), lines(40, 1000));
        let disc_shape = disc(20_000, 20_000, 10_300);
        let prisms = [Prism { shape: vec![disc_shape], z: (0, 1000) }];
        let inside = |i: usize, j: usize| {
            let (x, y) = (xs[i] - 20_000, ys[j] - 20_000);
            x * x + y * y <= 10_300 * 10_300
        };
        let w = outline_widths(&[0], &prisms, &xs, &ys, inside);
        let sum: f64 = w.iter().map(|x| x.2).sum();
        assert!((sum / (std::f64::consts::TAU * 10_300.0) - 1.0).abs() < 0.01, "{sum}");
    }

    #[test]
    fn narrow_finds_tracks_not_round_pads() {
        let (xs, ys) = (lines(40, 1000), lines(40, 1000));
        let track: Vec<Point> =
            [(1000, 0), (20_000, 19_000), (19_000, 20_000), (0, 1000)].iter().map(|&(x, y)| Point::new(x, y)).collect();
        assert_eq!(narrow(&[Polygon::new(track, Vec::new())], &xs, &ys, 1.5).len(), 1);
        assert!(narrow(&[disc(20_000, 20_000, 500)], &xs, &ys, 1.5).is_empty());
    }
}
