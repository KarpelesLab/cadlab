//! Small floating-point geometry kernel for the router: exact-enough distances between
//! segments, points and polygons, in nanometers.
//!
//! The router compares distances against rule values with a safety margin far larger than the
//! rounding error of `f64` on board-sized coordinates, and every result is checked by the DRC
//! afterwards (`crate::drc::check`), so floats are fine here. Nothing in this module is stored.

use polyclip::PolygonSet;

/// A point in nanometers.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Default)]
pub struct P {
    /// X.
    pub x: f64,
    /// Y.
    pub y: f64,
}

impl P {
    /// Creates a point.
    pub const fn new(x: f64, y: f64) -> P {
        P { x, y }
    }

    /// From a model point.
    pub fn of(p: crate::geom::Point) -> P {
        P::new(p.x.0 as f64, p.y.0 as f64)
    }

    /// Rounded to a model point.
    pub fn to_point(self) -> crate::geom::Point {
        crate::geom::Point::new(crate::units::Nm(self.x.round() as i64), crate::units::Nm(self.y.round() as i64))
    }

    /// Distance to `o`.
    pub fn dist(self, o: P) -> f64 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

/// Axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxF {
    /// Lower-left.
    pub min: P,
    /// Upper-right.
    pub max: P,
}

impl BoxF {
    /// An empty box (inverted).
    pub const EMPTY: BoxF = BoxF { min: P::new(f64::MAX, f64::MAX), max: P::new(f64::MIN, f64::MIN) };

    /// Grows to include `p`.
    pub fn add(&mut self, p: P) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
    }

    /// Box of two points.
    pub fn of2(a: P, b: P) -> BoxF {
        let mut r = BoxF::EMPTY;
        r.add(a);
        r.add(b);
        r
    }

    /// Expanded by `d` on every side.
    pub fn expand(self, d: f64) -> BoxF {
        BoxF { min: P::new(self.min.x - d, self.min.y - d), max: P::new(self.max.x + d, self.max.y + d) }
    }

    /// Whether the box is empty.
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    /// Whether two boxes overlap (closed).
    pub fn intersects(&self, o: &BoxF) -> bool {
        self.min.x <= o.max.x && o.min.x <= self.max.x && self.min.y <= o.max.y && o.min.y <= self.max.y
    }

    /// Union.
    pub fn union(self, o: BoxF) -> BoxF {
        let mut r = self;
        if !o.is_empty() {
            r.add(o.min);
            r.add(o.max);
        }
        r
    }

    /// Distance from `p` to the box (0 inside).
    pub fn dist(&self, p: P) -> f64 {
        let dx = (self.min.x - p.x).max(p.x - self.max.x).max(0.0);
        let dy = (self.min.y - p.y).max(p.y - self.max.y).max(0.0);
        (dx * dx + dy * dy).sqrt()
    }
}

/// Squared distance from `p` to segment `a`–`b`.
pub fn point_seg_d2(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    let t = if l2 <= 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.x + t * dx, a.y + t * dy);
    (p.x - cx).powi(2) + (p.y - cy).powi(2)
}

/// Closest point to `p` on segment `a`–`b`.
pub fn project(p: P, a: P, b: P) -> P {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    let t = if l2 <= 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0) };
    P::new(a.x + t * dx, a.y + t * dy)
}

fn orient(a: P, b: P, c: P) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Whether segments `a`–`b` and `c`–`d` intersect (touching counts).
pub fn segs_intersect(a: P, b: P, c: P, d: P) -> bool {
    let (d1, d2, d3, d4) = (orient(c, d, a), orient(c, d, b), orient(a, b, c), orient(a, b, d));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0)) && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0)) {
        return true;
    }
    let on =
        |p: P, q: P, r: P| r.x >= p.x.min(q.x) && r.x <= p.x.max(q.x) && r.y >= p.y.min(q.y) && r.y <= p.y.max(q.y);
    (d1 == 0.0 && on(c, d, a)) || (d2 == 0.0 && on(c, d, b)) || (d3 == 0.0 && on(a, b, c)) || (d4 == 0.0 && on(a, b, d))
}

/// Distance between segments `a`–`b` and `c`–`d`.
pub fn seg_seg_dist(a: P, b: P, c: P, d: P) -> f64 {
    if segs_intersect(a, b, c, d) {
        return 0.0;
    }
    point_seg_d2(a, c, d).min(point_seg_d2(b, c, d)).min(point_seg_d2(c, a, b)).min(point_seg_d2(d, a, b)).sqrt()
}

/// A polygon with holes (one polygon of a canonical `PolygonSet`).
#[derive(Clone, Debug)]
pub struct Poly {
    /// Outer ring then holes.
    pub rings: Vec<Vec<P>>,
    /// Bounding box.
    pub bbox: BoxF,
}

impl Poly {
    /// From rings (first is the outer boundary).
    pub fn new(rings: Vec<Vec<P>>) -> Poly {
        let mut bbox = BoxF::EMPTY;
        for r in &rings {
            for p in r {
                bbox.add(*p);
            }
        }
        Poly { rings, bbox }
    }

    /// Whether `p` is inside (even-odd over all rings; boundary counts as inside or outside
    /// arbitrarily, callers compare distances anyway).
    pub fn contains(&self, p: P) -> bool {
        if p.x < self.bbox.min.x || p.x > self.bbox.max.x || p.y < self.bbox.min.y || p.y > self.bbox.max.y {
            return false;
        }
        let mut inside = false;
        for r in &self.rings {
            let n = r.len();
            let mut j = n.wrapping_sub(1);
            for i in 0..n {
                let (a, b) = (r[i], r[j]);
                if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
                    inside = !inside;
                }
                j = i;
            }
        }
        inside
    }

    /// Edges as point pairs.
    pub fn edges(&self) -> impl Iterator<Item = (P, P)> + '_ {
        self.rings.iter().flat_map(|r| (0..r.len()).map(move |i| (r[i], r[(i + 1) % r.len()])))
    }

    /// Distance from segment `a`–`b` to the region (0 when they overlap).
    pub fn dist_seg(&self, a: P, b: P) -> f64 {
        if self.contains(a) || self.contains(b) {
            return 0.0;
        }
        let mut best = f64::MAX;
        for (c, d) in self.edges() {
            best = best.min(seg_seg_dist(a, b, c, d));
            if best == 0.0 {
                break;
            }
        }
        best
    }
}

/// A region the router keeps away from, or connects to.
#[derive(Clone, Debug)]
pub enum Shape {
    /// Stadium: every point within `r` of segment `a`–`b` (tracks; vias and round holes with `a == b`).
    Capsule {
        /// One end.
        a: P,
        /// Other end.
        b: P,
        /// Radius.
        r: f64,
    },
    /// Polygons (pads, keep-outs, arcs).
    Polys(Vec<Poly>),
}

impl Shape {
    /// From a polygon set.
    pub fn of_set(set: &PolygonSet) -> Shape {
        Shape::Polys(
            set.iter()
                .map(|pg| {
                    let mut rings = vec![pg.outer.0.iter().map(|q| P::new(q.x as f64, q.y as f64)).collect()];
                    rings.extend(pg.holes.iter().map(|h| h.0.iter().map(|q| P::new(q.x as f64, q.y as f64)).collect()));
                    Poly::new(rings)
                })
                .collect(),
        )
    }

    /// A polygon from one ring.
    pub fn of_ring(ring: Vec<P>) -> Shape {
        Shape::Polys(vec![Poly::new(vec![ring])])
    }

    /// Bounding box.
    pub fn bbox(&self) -> BoxF {
        match self {
            Shape::Capsule { a, b, r } => BoxF::of2(*a, *b).expand(*r),
            Shape::Polys(v) => v.iter().fold(BoxF::EMPTY, |acc, p| acc.union(p.bbox)),
        }
    }

    /// Distance from segment `a`–`b` (a point when `a == b`) to the shape; 0 when they overlap.
    pub fn dist_seg(&self, a: P, b: P) -> f64 {
        match self {
            Shape::Capsule { a: c, b: d, r } => (seg_seg_dist(a, b, *c, *d) - r).max(0.0),
            Shape::Polys(v) => v.iter().map(|p| p.dist_seg(a, b)).fold(f64::MAX, f64::min),
        }
    }

    /// Distance from a point.
    pub fn dist_point(&self, p: P) -> f64 {
        self.dist_seg(p, p)
    }

    /// Whether the point is inside (or on) the shape.
    pub fn contains(&self, p: P) -> bool {
        match self {
            Shape::Capsule { a, b, r } => point_seg_d2(p, *a, *b) <= r * r,
            Shape::Polys(v) => v.iter().any(|q| q.contains(p)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances() {
        let (a, b) = (P::new(0.0, 0.0), P::new(10.0, 0.0));
        assert_eq!(point_seg_d2(P::new(5.0, 3.0), a, b), 9.0);
        assert_eq!(point_seg_d2(P::new(-3.0, 4.0), a, b), 25.0);
        assert_eq!(seg_seg_dist(a, b, P::new(5.0, -1.0), P::new(5.0, 1.0)), 0.0);
        assert_eq!(seg_seg_dist(a, b, P::new(12.0, 0.0), P::new(20.0, 0.0)), 2.0);
        let sq = Shape::of_ring(vec![P::new(0.0, 0.0), P::new(4.0, 0.0), P::new(4.0, 4.0), P::new(0.0, 4.0)]);
        assert_eq!(sq.dist_point(P::new(2.0, 2.0)), 0.0);
        assert_eq!(sq.dist_point(P::new(7.0, 2.0)), 3.0);
        assert_eq!(sq.dist_seg(P::new(-1.0, 2.0), P::new(6.0, 2.0)), 0.0, "crossing");
        let cap = Shape::Capsule { a, b, r: 1.0 };
        assert_eq!(cap.dist_point(P::new(5.0, 3.0)), 2.0);
        assert!(cap.contains(P::new(10.5, 0.5)));
    }
}
