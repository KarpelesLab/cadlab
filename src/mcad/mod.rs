//! Mechanical CAD exchange (roadmap M9): the board as a 3D model for MCAD tools.
//!
//! - [`step`]: ISO 10303-21 (STEP) with the AP214 schema (`AUTOMOTIVE_DESIGN`): the board
//!   solid and simple component bodies as an assembly.
//! - [`idf`]: IDF 3.0 board (`.emn`) and library (`.emp`) files.
//!
//! Both start from the same data, built here: the outline as closed loops of lines and arcs
//! ([`board_profile`]), the drilled holes, and component bodies as boxes taken from the
//! footprint's package dimensions ([`bodies`]). See `docs/MANUFACTURING.md`.
//!
//! Coordinates are board coordinates (millimeters in the files, Y up), unchanged, so the model
//! lines up with the Gerber and drill files. The board's bottom face is at Z = 0.

pub mod idf;
pub mod step;

use crate::board::{self, footprint_for};
use crate::fabout::{self, HoleKind};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{BoardSide, Segment};
use crate::units::{Angle, Nm};

/// Export options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Drill vias through the board solid / list them as drilled holes (default off: MCAD
    /// rarely needs them and they dominate the file size).
    pub vias: bool,
    /// Include component bodies (default on).
    pub components: bool,
    /// Version written to file headers.
    pub version: String,
}

impl Default for Options {
    fn default() -> Self {
        Options { vias: false, components: true, version: env!("CARGO_PKG_VERSION").to_string() }
    }
}

/// An edge of a closed loop, from the previous vertex to `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    /// Straight line.
    Line {
        /// End point.
        to: Point,
    },
    /// Circular arc around `center` (nanometers, not necessarily integral); a full circle when
    /// `to` is its start.
    Arc {
        /// End point.
        to: Point,
        /// Center (nm).
        center: (f64, f64),
        /// Counter-clockwise (seen from the top).
        ccw: bool,
    },
}

impl Edge {
    /// End point.
    pub fn to(&self) -> Point {
        match *self {
            Edge::Line { to } | Edge::Arc { to, .. } => to,
        }
    }
}

/// A closed loop: `start`, then edges; the last edge ends at `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct Loop {
    /// First vertex.
    pub start: Point,
    /// Edges, the last one ending at `start`.
    pub edges: Vec<Edge>,
}

fn f(p: Point) -> (f64, f64) {
    (p.x.0 as f64, p.y.0 as f64)
}

/// Radius of an arc edge starting at `from`.
pub fn radius(from: Point, center: (f64, f64)) -> f64 {
    let (x, y) = f(from);
    ((x - center.0).powi(2) + (y - center.1).powi(2)).sqrt()
}

/// Signed sweep of an arc edge in degrees (positive counter-clockwise; ±360 for a circle).
pub fn sweep_deg(from: Point, to: Point, center: (f64, f64), ccw: bool) -> f64 {
    if from == to {
        return if ccw { 360.0 } else { -360.0 };
    }
    let a = |p: Point| {
        let (x, y) = f(p);
        (y - center.1).atan2(x - center.0)
    };
    let mut d = (a(to) - a(from)).to_degrees();
    if ccw {
        while d <= 0.0 {
            d += 360.0;
        }
    } else {
        while d >= 0.0 {
            d -= 360.0;
        }
    }
    d
}

/// The arc through `a`, `m`, `b` as an edge ending at `b` (a line when the points are
/// collinear). `a == b` is a full circle with `m` diametrically opposite.
pub fn arc_edge(a: Point, m: Point, b: Point) -> Edge {
    let (ax, ay) = f(a);
    let (mx, my) = f(m);
    let (bx, by) = f(b);
    if a == b {
        return Edge::Arc { to: b, center: ((ax + mx) / 2.0, (ay + my) / 2.0), ccw: true };
    }
    let d = 2.0 * (ax * (my - by) + mx * (by - ay) + bx * (ay - my));
    if d == 0.0 {
        return Edge::Line { to: b };
    }
    let (a2, m2, b2) = (ax * ax + ay * ay, mx * mx + my * my, bx * bx + by * by);
    let cx = (a2 * (my - by) + m2 * (by - ay) + b2 * (ay - my)) / d;
    let cy = (a2 * (bx - mx) + m2 * (ax - bx) + b2 * (mx - ax)) / d;
    // Turning direction of a → m → b.
    let cross = (mx - ax) * (by - my) - (my - ay) * (bx - mx);
    // Arcs drawn around a whole-nanometer center have their mid point rounded: snap back to
    // that center when it fits the end points exactly and the mid point within rounding.
    let d2 = |p: Point, x: i64, y: i64| {
        let (dx, dy) = ((p.x.0 - x) as i128, (p.y.0 - y) as i128);
        dx * dx + dy * dy
    };
    let (x0, y0) = (cx.round() as i64, cy.round() as i64);
    // Prefer the roundest center (most trailing decimal zeros: designers use round numbers),
    // then the best fit of the mid point.
    let zeros = |v: i64| (0..9).take_while(|k| v % 10_i64.pow(k + 1) == 0).count() as i32;
    let mut best: Option<(i32, f64, i64, i64)> = None;
    for dx in -3..=3 {
        for dy in -3..=3 {
            let (x, y) = (x0 + dx, y0 + dy);
            if d2(a, x, y) != d2(b, x, y) {
                continue;
            }
            let err = ((d2(m, x, y) as f64).sqrt() - (d2(a, x, y) as f64).sqrt()).abs();
            let z = -(zeros(x) + zeros(y));
            if err <= 1.5 && best.is_none_or(|(bz, e, ..)| (z, err) < (bz, e)) {
                best = Some((z, err, x, y));
            }
        }
    }
    let center = best.map_or((cx, cy), |(_, _, x, y)| (x as f64, y as f64));
    Edge::Arc { to: b, center, ccw: cross > 0.0 }
}

impl Loop {
    /// A full circle (one arc edge), counter-clockwise or not, starting on its +X side.
    pub fn circle(center: Point, diameter: Nm, ccw: bool) -> Loop {
        let start = Point::new(center.x + Nm(diameter.0 / 2), center.y);
        Loop { start, edges: vec![Edge::Arc { to: start, center: f(center), ccw }] }
    }

    /// Vertex `i` (start of edge `i`).
    pub fn vertex(&self, i: usize) -> Point {
        if i == 0 { self.start } else { self.edges[i - 1].to() }
    }

    /// Signed area (nm², positive when counter-clockwise), arcs included exactly.
    pub fn signed_area(&self) -> f64 {
        let mut s = 0.0;
        for (i, e) in self.edges.iter().enumerate() {
            let (x0, y0) = f(self.vertex(i));
            let (x1, y1) = f(e.to());
            s += x0 * y1 - x1 * y0;
            if let Edge::Arc { center, ccw, .. } = *e {
                // Circular segment between the chord and the arc.
                let r = radius(self.vertex(i), center);
                let t = sweep_deg(self.vertex(i), e.to(), center, ccw).to_radians();
                s += r * r * (t - t.sin());
            }
        }
        s / 2.0
    }

    /// The same loop traversed the other way.
    pub fn reversed(&self) -> Loop {
        let n = self.edges.len();
        let mut edges = Vec::with_capacity(n);
        for i in (0..n).rev() {
            let to = self.vertex(i);
            edges.push(match self.edges[i] {
                Edge::Line { .. } => Edge::Line { to },
                Edge::Arc { center, ccw, .. } => Edge::Arc { to, center, ccw: !ccw },
            });
        }
        Loop { start: self.start, edges }
    }

    /// This loop, oriented counter-clockwise (`ccw`) or clockwise.
    pub fn oriented(self, ccw: bool) -> Loop {
        if (self.signed_area() > 0.0) == ccw { self } else { self.reversed() }
    }

    /// Splits full circles into two half arcs (formats that cannot express a 360° arc).
    pub fn split_circles(&self) -> Loop {
        let mut edges = Vec::new();
        for (i, e) in self.edges.iter().enumerate() {
            let from = self.vertex(i);
            match *e {
                Edge::Arc { to, center, ccw } if to == from => {
                    let (x, y) = f(from);
                    let opp =
                        Point::new(Nm((2.0 * center.0 - x).round() as i64), Nm((2.0 * center.1 - y).round() as i64));
                    edges.push(Edge::Arc { to: opp, center, ccw });
                    edges.push(Edge::Arc { to, center, ccw });
                }
                _ => edges.push(*e),
            }
        }
        Loop { start: self.start, edges }
    }

    /// Polygon approximation (vertices, arcs sampled every ~5°), for containment tests.
    pub fn polygon(&self) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for (i, e) in self.edges.iter().enumerate() {
            let from = self.vertex(i);
            out.push(f(from));
            if let Edge::Arc { center, ccw, to } = *e {
                let r = radius(from, center);
                let t = sweep_deg(from, to, center, ccw);
                let (x, y) = f(from);
                let a0 = (y - center.1).atan2(x - center.0);
                let n = (t.abs() / 5.0).ceil().max(1.0) as usize;
                for k in 1..n {
                    let a = a0 + (t * k as f64 / n as f64).to_radians();
                    out.push((center.0 + r * a.cos(), center.1 + r * a.sin()));
                }
            }
        }
        out
    }
}

/// The board outline: the outer loop (counter-clockwise) and cutouts (clockwise), or `None`
/// without an outline. Zero-length edges are dropped; open contours are closed with a line.
pub fn board_profile(p: &Project) -> Option<(Loop, Vec<Loop>)> {
    let mut loops = Vec::new();
    for c in &p.board().outline.contours {
        let mut edges = Vec::new();
        let mut cur = c.start;
        for s in &c.segments {
            match *s {
                Segment::Line { to } => {
                    if to != cur {
                        edges.push(Edge::Line { to });
                    }
                    cur = to;
                }
                Segment::Arc { mid, to } => {
                    if to == cur && mid == cur {
                        continue;
                    }
                    edges.push(arc_edge(cur, mid, to));
                    cur = to;
                }
            }
        }
        if cur != c.start {
            edges.push(Edge::Line { to: c.start });
        }
        if edges.is_empty() {
            continue;
        }
        loops.push(Loop { start: c.start, edges });
    }
    let mut it = loops.into_iter();
    let outer = it.next()?.oriented(true);
    Some((outer, it.map(|l| l.oriented(false)).collect()))
}

/// A drilled hole for MCAD outputs.
#[derive(Clone, Debug, PartialEq)]
pub struct DrillHole {
    /// Center.
    pub at: Point,
    /// Finished diameter.
    pub diameter: Nm,
    /// Plated.
    pub plated: bool,
    /// What it is for.
    pub kind: HoleKind,
    /// Component designator (pad holes), or `None` for vias and board holes.
    pub refdes: Option<String>,
}

/// Every drilled hole (pad holes by designator, board holes, then vias when `vias`).
pub fn drill_holes(p: &Project, vias: bool) -> Vec<DrillHole> {
    fabout::holes(p)
        .into_iter()
        .filter(|h| vias || h.kind != HoleKind::Via)
        .map(|h| DrillHole {
            at: h.at,
            diameter: h.diameter,
            plated: h.plated,
            kind: h.kind,
            refdes: h.pad.map(|(r, _)| r).filter(|r| !board::holes::is_hole(p, r)),
        })
        .collect()
}

fn point_in(poly: &[(f64, f64)], (x, y): (f64, f64)) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[(i + n - 1) % n];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
    }
    inside
}

fn dist_to_poly(poly: &[(f64, f64)], (x, y): (f64, f64)) -> f64 {
    let n = poly.len();
    let mut best = f64::INFINITY;
    for i in 0..n {
        let (ax, ay) = poly[i];
        let (bx, by) = poly[(i + 1) % n];
        let (dx, dy) = (bx - ax, by - ay);
        let l2 = dx * dx + dy * dy;
        let t = if l2 == 0.0 { 0.0 } else { (((x - ax) * dx + (y - ay) * dy) / l2).clamp(0.0, 1.0) };
        let (px, py) = (ax + t * dx, ay + t * dy);
        best = best.min(((x - px).powi(2) + (y - py).powi(2)).sqrt());
    }
    best
}

/// Splits holes into those that can be cut from the board solid (strictly inside the outline,
/// clear of cutouts and of each other) and the rest. Of two overlapping holes the larger (then
/// the first) is kept.
pub fn cuttable_holes(outer: &Loop, cutouts: &[Loop], holes: Vec<DrillHole>) -> (Vec<DrillHole>, Vec<DrillHole>) {
    let outer_poly = outer.polygon();
    let cut_polys: Vec<Vec<(f64, f64)>> = cutouts.iter().map(Loop::polygon).collect();
    // Margin so that faces never touch (1 µm).
    let margin = 1_000.0;
    let mut order: Vec<usize> = (0..holes.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(holes[i].diameter), i));
    let mut keep = vec![false; holes.len()];
    let mut kept: Vec<usize> = Vec::new();
    for i in order {
        let h = &holes[i];
        let c = f(h.at);
        let r = h.diameter.0 as f64 / 2.0;
        if h.diameter.0 <= 0 {
            continue;
        }
        let inside = point_in(&outer_poly, c) && dist_to_poly(&outer_poly, c) > r + margin;
        let clear = cut_polys.iter().all(|cp| !point_in(cp, c) && dist_to_poly(cp, c) > r + margin);
        let apart = kept.iter().all(|&j| {
            let o = &holes[j];
            let (ox, oy) = f(o.at);
            ((ox - c.0).powi(2) + (oy - c.1).powi(2)).sqrt() > r + o.diameter.0 as f64 / 2.0 + margin
        });
        if inside && clear && apart {
            keep[i] = true;
            kept.push(i);
        }
    }
    let mut ok = Vec::new();
    let mut rest = Vec::new();
    for (i, h) in holes.into_iter().enumerate() {
        if keep[i] { ok.push(h) } else { rest.push(h) }
    }
    (ok, rest)
}

/// A component body: a box from the package dimensions, centered on the footprint origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyBox {
    /// Designator.
    pub refdes: String,
    /// Footprint name (the geometry name).
    pub footprint: String,
    /// Part number: the MPN, else the project part ID.
    pub part_number: String,
    /// Footprint origin on the board.
    pub at: Point,
    /// Placement rotation (counter-clockwise, applied after mirroring on the bottom side).
    pub rotation: Angle,
    /// Board side.
    pub side: BoardSide,
    /// Size along the footprint's X axis.
    pub width: Nm,
    /// Size along the footprint's Y axis.
    pub length: Nm,
    /// Height above the board surface.
    pub height: Nm,
}

/// Bodies of the placed, populated components (DNP excluded), by designator, and the
/// designators left out because their footprint has no package body.
pub fn bodies(p: &Project) -> (Vec<BodyBox>, Vec<String>) {
    let info = fabout::populated(p);
    let mut out = Vec::new();
    let mut missing = Vec::new();
    for (refdes, pf) in &p.board().footprints {
        let Some(ci) = info.get(refdes) else { continue };
        let fp = footprint_for(p, refdes);
        let Some((fp, body)) = fp.and_then(|fp| fp.body.map(|b| (fp, b))) else {
            missing.push(refdes.clone());
            continue;
        };
        if body.width.0 <= 0 || body.length.0 <= 0 || body.height.0 <= 0 {
            missing.push(refdes.clone());
            continue;
        }
        let part = p.circuit().components.get(refdes).map(|c| c.part.clone()).unwrap_or_default();
        out.push(BodyBox {
            refdes: refdes.clone(),
            footprint: fp.name.clone(),
            part_number: ci.mpn.clone().unwrap_or(part),
            at: pf.at,
            rotation: pf.rotation,
            side: pf.side,
            width: body.width,
            length: body.length,
            height: body.height,
        });
    }
    (out, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn arcs_and_orientation() {
        // Quarter circle of radius 1000 around the origin, counter-clockwise.
        let e = arc_edge(pt(1000, 0), pt(707, 707), pt(0, 1000));
        let Edge::Arc { center, ccw, .. } = e else { panic!("{e:?}") };
        assert!(ccw);
        assert!(center.0.abs() < 1.0 && center.1.abs() < 1.0, "{center:?}");
        assert!((sweep_deg(pt(1000, 0), pt(0, 1000), center, true) - 90.0).abs() < 1e-6);
        assert!((sweep_deg(pt(1000, 0), pt(0, 1000), center, false) + 270.0).abs() < 1e-6);
        assert_eq!(arc_edge(pt(0, 0), pt(1, 1), pt(2, 2)), Edge::Line { to: pt(2, 2) });

        let sq = Loop {
            start: pt(0, 0),
            edges: vec![
                Edge::Line { to: pt(10, 0) },
                Edge::Line { to: pt(10, 10) },
                Edge::Line { to: pt(0, 10) },
                Edge::Line { to: pt(0, 0) },
            ],
        };
        assert_eq!(sq.signed_area(), 100.0);
        let r = sq.reversed();
        assert_eq!(r.signed_area(), -100.0);
        assert_eq!(r.vertex(1), pt(0, 10));
        assert_eq!(r.clone().oriented(true), sq);
        let c = Loop::circle(pt(0, 0), Nm(2000), true);
        assert!((c.signed_area() - std::f64::consts::PI * 1e6).abs() < 1e-3);
        assert!((c.reversed().signed_area() + std::f64::consts::PI * 1e6).abs() < 1e-3);
        assert_eq!(c.split_circles().edges.len(), 2);
    }

    #[test]
    fn hole_selection() {
        let outer = Loop {
            start: pt(0, 0),
            edges: vec![
                Edge::Line { to: pt(10_000_000, 0) },
                Edge::Line { to: pt(10_000_000, 10_000_000) },
                Edge::Line { to: pt(0, 10_000_000) },
                Edge::Line { to: pt(0, 0) },
            ],
        };
        let h = |x, y, d| DrillHole {
            at: pt(x, y),
            diameter: Nm(d),
            plated: false,
            kind: HoleKind::Mechanical,
            refdes: None,
        };
        let (ok, rest) = cuttable_holes(
            &outer,
            &[],
            vec![h(5_000_000, 5_000_000, 1_000_000), h(5_200_000, 5_000_000, 300_000), h(100_000, 100_000, 400_000)],
        );
        assert_eq!(ok.len(), 1);
        assert_eq!(rest.len(), 2, "overlapping and crossing the edge");
    }
}
