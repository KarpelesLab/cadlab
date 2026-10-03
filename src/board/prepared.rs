//! Prepared (indexed) polygon sets for fast exact queries against large shapes such as zone
//! fills.
//!
//! `polyclip`'s queries ([`polyclip::intersects`], [`polyclip::distance_less_than`]) visit every
//! segment of both operands, so testing thousands of small items against one pour with tens of
//! thousands of vertices costs O(items × pour vertices). A [`Prepared`] set indexes its segments
//! in a uniform grid and its edges in horizontal bands once; a query then hands `polyclip` a
//! [`Geometry`] view that yields only the segments near the other operand and locates points
//! with the edges of one band. `polyclip`'s own exact algorithms run on that view, and every
//! answer is identical to the unprepared query:
//!
//! - `intersects` and `distance_less_than` only use segments whose bounding box meets the other
//!   operand's bounding box (grown by the distance): the view yields at least those;
//! - point location mirrors `polyclip`'s per-ring winding numbers (an edge contributes to a
//!   winding number or puts the point on the boundary only when its y-range contains the
//!   point's y, so the edges of the point's band suffice) and its per-polygon rules;
//! - component points are the same (the first vertex of every polygon).

use polyclip::predicates::{on_segment, orient};
use polyclip::{Geometry, Location, Point, PolygonSet, Rect};

/// Shapes with at least this many segments are worth preparing before many queries.
pub const PREPARE_MIN_SEGMENTS: usize = 256;

/// Number of boundary segments of a polygon set (as `polyclip` visits them).
pub fn segment_count(set: &PolygonSet) -> usize {
    set.iter().map(|p| p.rings().map(|r| r.0.len()).sum::<usize>()).sum()
}

/// A polygon set with a segment grid and an edge band index.
#[derive(Clone, Debug)]
pub struct Prepared<'a> {
    set: &'a PolygonSet,
    bbox: Option<Rect>,
    /// Every segment, as visited by `polyclip`: (a, b, polygon, ring).
    segs: Vec<(Point, Point, u32, u32)>,
    grid: Grid,
    /// Horizontal bands: segment indices whose y-range meets each band (CSR: band `b` holds
    /// `band_items[band_start[b]..band_start[b + 1]]`).
    band_start: Vec<u32>,
    band_items: Vec<u32>,
    band_y0: i64,
    band_h: i64,
    /// Outer ring bounding boxes, per polygon.
    outer_boxes: Vec<Option<Rect>>,
}

#[derive(Clone, Debug)]
struct Grid {
    x0: i64,
    y0: i64,
    cell: i64,
    cols: i64,
    rows: i64,
    /// Segment indices per cell (CSR, row-major cells).
    start: Vec<u32>,
    items: Vec<u32>,
}

impl Grid {
    fn cell(&self, r: i64, c: i64) -> &[u32] {
        let k = (r * self.cols + c) as usize;
        &self.items[self.start[k] as usize..self.start[k + 1] as usize]
    }

    fn col(&self, x: i64) -> i64 {
        ((x.saturating_sub(self.x0)) / self.cell).clamp(0, self.cols - 1)
    }
    fn row(&self, y: i64) -> i64 {
        ((y.saturating_sub(self.y0)) / self.cell).clamp(0, self.rows - 1)
    }
}

impl<'a> Prepared<'a> {
    /// Indexes `set`.
    pub fn new(set: &'a PolygonSet) -> Self {
        let bbox = set.bbox();
        let mut segs = Vec::new();
        for (pi, poly) in set.iter().enumerate() {
            for (ri, ring) in poly.rings().enumerate() {
                let pts = &ring.0;
                let n = pts.len();
                for i in 0..n {
                    segs.push((pts[i], pts[(i + 1) % n], pi as u32, ri as u32));
                }
            }
        }
        let outer_boxes = set.iter().map(|p| p.outer.bbox()).collect();
        let b = bbox.unwrap_or(Rect { min: Point::new(0, 0), max: Point::new(0, 0) });
        let (w, h) = ((b.max.x - b.min.x).max(1) as f64, (b.max.y - b.min.y).max(1) as f64);
        // About four segments per grid cell, at most ~1M cells.
        let target = (segs.len() / 4).clamp(1, 1 << 20) as f64;
        let cell = ((w * h / target).sqrt().ceil() as i64).max(1);
        let cols = (((w as i64) / cell) + 1).clamp(1, 4096);
        let rows = (((h as i64) / cell) + 1).clamp(1, 4096);
        let cell = cell.max(((w as i64) / cols) + 1).max(((h as i64) / rows) + 1);
        let mut grid = Grid { x0: b.min.x, y0: b.min.y, cell, cols, rows, start: Vec::new(), items: Vec::new() };
        // About eight segments per band (by count; long edges span several).
        let nb = (segs.len() / 8).clamp(1, 1 << 16) as i64;
        let band_h = ((h as i64) / nb + 1).max(1);
        let nb = ((h as i64) / band_h + 1).max(1);
        let band = |y: i64| ((y.saturating_sub(b.min.y)) / band_h).clamp(0, nb - 1);
        // Two passes each (count, then place) into flat arrays.
        let mut cell_n = vec![0u32; (cols * rows) as usize + 1];
        let mut band_n = vec![0u32; nb as usize + 1];
        let span = |g: &Grid, p: Point, q: Point| {
            (g.col(p.x.min(q.x)), g.col(p.x.max(q.x)), g.row(p.y.min(q.y)), g.row(p.y.max(q.y)))
        };
        for &(p, q, _, _) in &segs {
            let (c0, c1, r0, r1) = span(&grid, p, q);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    cell_n[(r * cols + c) as usize + 1] += 1;
                }
            }
            for bi in band(p.y.min(q.y))..=band(p.y.max(q.y)) {
                band_n[bi as usize + 1] += 1;
            }
        }
        for k in 1..cell_n.len() {
            cell_n[k] += cell_n[k - 1];
        }
        for k in 1..band_n.len() {
            band_n[k] += band_n[k - 1];
        }
        let mut items = vec![0u32; *cell_n.last().unwrap_or(&0) as usize];
        let mut band_items = vec![0u32; *band_n.last().unwrap_or(&0) as usize];
        let (mut cell_fill, mut band_fill) = (cell_n.clone(), band_n.clone());
        for (k, &(p, q, _, _)) in segs.iter().enumerate() {
            let (c0, c1, r0, r1) = span(&grid, p, q);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    let slot = &mut cell_fill[(r * cols + c) as usize];
                    items[*slot as usize] = k as u32;
                    *slot += 1;
                }
            }
            for bi in band(p.y.min(q.y))..=band(p.y.max(q.y)) {
                let slot = &mut band_fill[bi as usize];
                band_items[*slot as usize] = k as u32;
                *slot += 1;
            }
        }
        grid.start = cell_n;
        grid.items = items;
        Prepared { set, bbox, segs, grid, band_start: band_n, band_items, band_y0: b.min.y, band_h, outer_boxes }
    }

    /// The indexed set.
    pub fn set(&self) -> &'a PolygonSet {
        self.set
    }

    /// Exactly `polyclip::intersects(other, set)`.
    pub fn intersects<G: Geometry + ?Sized>(&self, other: &G) -> bool {
        let Some(win) = other.bbox() else { return false };
        polyclip::intersects(other, &self.view(win))
    }

    /// Exactly `polyclip::distance_less_than(other, set, d)`.
    pub fn distance_less_than<G: Geometry + ?Sized>(&self, other: &G, d: i64) -> bool {
        if d <= 0 {
            return false;
        }
        let Some(win) = other.bbox() else { return false };
        polyclip::distance_less_than(other, &self.view(win.expand(d)), d)
    }

    /// A view of the set that yields the segments near `window` (and behaves like the whole
    /// set for every query whose other operand lies within `window`).
    pub fn view(&self, window: Rect) -> View<'_, 'a> {
        View { prep: self, window }
    }

    fn segments_near(&self, w: &Rect, f: &mut dyn FnMut(Point, Point)) {
        let Some(b) = self.bbox else { return };
        if !b.intersects(w) {
            return;
        }
        let g = &self.grid;
        let (c0, c1) = (g.col(w.min.x), g.col(w.max.x));
        let (r0, r1) = (g.row(w.min.y), g.row(w.max.y));
        if (c1 - c0 + 1) * (r1 - r0 + 1) == 1 {
            for &k in g.cell(r0, c0) {
                let (p, q, _, _) = self.segs[k as usize];
                f(p, q);
            }
            return;
        }
        let mut ks: Vec<u32> = Vec::new();
        for r in r0..=r1 {
            for c in c0..=c1 {
                ks.extend_from_slice(g.cell(r, c));
            }
        }
        ks.sort_unstable();
        ks.dedup();
        for k in ks {
            let (p, q, _, _) = self.segs[k as usize];
            f(p, q);
        }
    }

    /// `polyclip`'s location of `p` in the set (per-ring winding numbers, then per-polygon
    /// rules), from the edges of `p`'s band.
    fn locate(&self, p: Point) -> Location {
        if self.segs.is_empty() || !p.in_range() {
            return Location::Outside;
        }
        let bi = (p.y.saturating_sub(self.band_y0)) / self.band_h;
        let nb = self.band_start.len() as i64 - 1;
        if p.y < self.band_y0 || bi >= nb {
            return Location::Outside;
        }
        let band = &self.band_items[self.band_start[bi as usize] as usize..self.band_start[bi as usize + 1] as usize];
        // (polygon, ring) → winding number, None when p is on that ring.
        let mut rings: Vec<((u32, u32), Option<i32>)> = Vec::new();
        for &k in band {
            let (a, b, pi, ri) = self.segs[k as usize];
            let contrib: Option<i32> = if a.y <= p.y {
                if b.y > p.y {
                    match orient(a, b, p) {
                        o if o > 0 => Some(1),
                        0 => None,
                        _ => Some(0),
                    }
                } else if b.y == p.y && on_segment(a, b, p) {
                    None
                } else {
                    Some(0)
                }
            } else if b.y <= p.y {
                match orient(a, b, p) {
                    o if o < 0 => Some(-1),
                    0 => None,
                    _ => Some(0),
                }
            } else {
                Some(0)
            };
            if contrib == Some(0) {
                continue;
            }
            match rings.iter_mut().find(|(id, _)| *id == (pi, ri)) {
                Some((_, w)) => *w = w.and_then(|w| contrib.map(|c| w + c)),
                None => rings.push(((pi, ri), contrib)),
            }
        }
        let ring_loc = |pi: u32, ri: u32| match rings.iter().find(|(id, _)| *id == (pi, ri)) {
            None | Some((_, Some(0))) => Location::Outside,
            Some((_, None)) => Location::OnBoundary,
            Some((_, Some(_))) => Location::Inside,
        };
        let mut polys: Vec<u32> = rings.iter().map(|((pi, _), _)| *pi).collect();
        polys.sort_unstable();
        polys.dedup();
        let mut res = Location::Outside;
        for pi in polys {
            if let Some(b) = self.outer_boxes[pi as usize]
                && !b.contains_point(p)
            {
                continue;
            }
            let loc = match ring_loc(pi, 0) {
                Location::Inside => {
                    let holes = self.set[pi as usize].holes.len() as u32;
                    let mut l = Location::Inside;
                    for h in 1..=holes {
                        match ring_loc(pi, h) {
                            Location::Outside => {}
                            Location::Inside => {
                                l = Location::Outside;
                                break;
                            }
                            Location::OnBoundary => {
                                l = Location::OnBoundary;
                                break;
                            }
                        }
                    }
                    l
                }
                other => other,
            };
            match loc {
                Location::Inside => return Location::Inside,
                Location::OnBoundary => res = Location::OnBoundary,
                Location::Outside => {}
            }
        }
        res
    }
}

/// A windowed view of a [`Prepared`] set, see [`Prepared::view`].
#[derive(Clone, Copy, Debug)]
pub struct View<'p, 'a> {
    prep: &'p Prepared<'a>,
    window: Rect,
}

impl Geometry for View<'_, '_> {
    fn bbox(&self) -> Option<Rect> {
        self.prep.bbox
    }
    fn is_areal(&self) -> bool {
        true
    }
    fn visit_segments(&self, f: &mut dyn FnMut(Point, Point)) {
        self.prep.segments_near(&self.window, f)
    }
    fn locate(&self, p: Point) -> Location {
        self.prep.locate(p)
    }
    fn any_point(&self) -> Option<Point> {
        self.prep.set.any_point()
    }
    fn component_points(&self, f: &mut dyn FnMut(Point)) {
        self.prep.set.component_points(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polyclip::{ArcTol, Boolean, Circle, FillRule, Op, Polygon, Side};

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: i64) -> i64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % n as u64) as i64
        }
    }

    /// A zone-like set: a square minus random circles, plus islands inside holes.
    fn swiss(seed: u64) -> PolygonSet {
        let mut r = Lcg(seed);
        let sq = |x0: i64, y0: i64, x1: i64, y1: i64| {
            Polygon::new(vec![Point::new(x0, y0), Point::new(x1, y0), Point::new(x1, y1), Point::new(x0, y1)], vec![])
        };
        let tol = ArcTol::new(50, Side::Outside);
        let holes: Vec<Polygon> = (0..60)
            .map(|_| {
                let c = Point::new(r.next(100_000), r.next(100_000));
                Polygon::new(Circle::new(c, 1_000 + r.next(6_000)).to_ring(tol).unwrap(), vec![])
            })
            .collect();
        let mut set = Boolean::new()
            .subject(&sq(0, 0, 100_000, 100_000), FillRule::NonZero)
            .clip(&holes, FillRule::NonZero)
            .op(Op::Difference)
            .execute()
            .unwrap();
        set.push(sq(120_000, 0, 130_000, 10_000));
        Boolean::new().subject(&set, FillRule::NonZero).execute().unwrap()
    }

    #[test]
    fn matches_polyclip_queries() {
        for seed in 1..4 {
            let set = swiss(seed);
            let prep = Prepared::new(&set);
            let mut r = Lcg(seed * 7919);
            for _ in 0..1500 {
                let c = Point::new(r.next(140_000) - 5_000, r.next(110_000) - 5_000);
                // Points, including vertices and points on edges.
                let probe = if r.next(4) == 0 {
                    let poly = &set[r.next(set.len() as i64) as usize];
                    let ring = &poly.outer.0;
                    let k = r.next(ring.len() as i64) as usize;
                    let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                    if r.next(2) == 0 { a } else { Point::new((a.x + b.x) / 2, (a.y + b.y) / 2) }
                } else {
                    c
                };
                assert_eq!(prep.locate(probe), polyclip::locate(&set, probe), "locate {probe:?}");
                let size = 1 + r.next(3_000);
                let item: PolygonSet = vec![Polygon::new(
                    vec![
                        Point::new(probe.x - size, probe.y - size),
                        Point::new(probe.x + size, probe.y - size),
                        Point::new(probe.x + size, probe.y + size),
                        Point::new(probe.x - size, probe.y + size),
                    ],
                    vec![],
                )];
                assert_eq!(prep.intersects(&item), polyclip::intersects(&item, &set), "intersects {probe:?}");
                let d = r.next(4_000);
                assert_eq!(
                    prep.distance_less_than(&item, d),
                    polyclip::distance_less_than(&item, &set, d),
                    "distance {probe:?} {d}"
                );
            }
        }
    }

    #[test]
    fn empty_and_far() {
        let empty: PolygonSet = vec![];
        let prep = Prepared::new(&empty);
        let item = swiss(1);
        assert!(!prep.intersects(&item));
        assert!(!prep.distance_less_than(&item, 1_000));
        assert_eq!(prep.locate(Point::new(0, 0)), Location::Outside);
    }
}
