//! Exact outlines of simple copper items, for measuring distances without the arc
//! approximation of their polygons ([`super::COPPER_TOL`]).
//!
//! An item is a [`Core`] grown by a radius: a via is its center grown by half its diameter, a
//! straight track its center line grown by half its width, a round-rectangle pad the rectangle
//! between its corner centers grown by the corner radius. Distances between two cores minus
//! both radii are then exact up to floating point. Arc tracks, zone fills, drawings and pads
//! with rounded chamfered corners have no core.

use crate::geom::Point;
use crate::model::Project;
use crate::model::footprint::PadShape;
use crate::units::Nm;

use super::{CopperItem, ItemRef, footprint_for, stack_layer, transform};

/// A point, a segment chain (`closed` false) or a closed polygon (`closed` true, filled), grown
/// by `radius` nanometers.
#[derive(Clone, Debug, PartialEq)]
pub struct Core {
    /// Vertices on the board.
    pub points: Vec<Point>,
    /// Whether the vertices bound a filled polygon.
    pub closed: bool,
    /// Growth, in nanometers.
    pub radius: f64,
}

/// The exact outline of a copper item on its layers, if it has one: straight tracks, vias and
/// pads other than chamfered rectangles with rounded corners.
pub fn item_core(p: &Project, item: &CopperItem) -> Option<Core> {
    let board = p.board();
    match &item.item {
        ItemRef::Track(id) => {
            let t = board.tracks.iter().find(|t| &t.id == id)?;
            if t.mid.is_some() {
                return None;
            }
            Some(Core { points: vec![t.start, t.end], closed: false, radius: t.width.0 as f64 / 2.0 })
        }
        ItemRef::Via(id) => {
            let v = board.vias.iter().find(|v| &v.id == id)?;
            Some(Core { points: vec![v.at], closed: false, radius: v.diameter.0 as f64 / 2.0 })
        }
        ItemRef::Pad(refdes, number) => {
            let pf = board.footprints.get(refdes)?;
            let fp = footprint_for(p, refdes)?;
            let tf = transform(pf);
            let pad = fp.pads.iter().find(|pad| &pad.number == number && tf(pad.at) == item.anchor)?;
            let copper = board.stackup.copper_names();
            let shape = match item.layers.first() {
                Some(l) => pad.shape_on(stack_layer(&copper, l, pf.side)),
                None => &pad.shape,
            };
            let (points, closed, radius) = shape_core(shape)?;
            let place = |q: Point| tf(q.rotated(pad.rotation) + pad.at);
            Some(Core { points: points.into_iter().map(place).collect(), closed, radius })
        }
        _ => None,
    }
}

/// A pad shape's core relative to the pad center, before the pad rotation.
fn shape_core(shape: &PadShape) -> Option<(Vec<Point>, bool, f64)> {
    let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
    // A rectangle of half sizes `hx`, `hy` (either may be zero): a point, a segment or a polygon.
    let rect = |hx: i64, hy: i64| -> (Vec<Point>, bool) {
        match (hx > 0, hy > 0) {
            (false, false) => (vec![p(0, 0)], false),
            (true, false) => (vec![p(-hx, 0), p(hx, 0)], false),
            (false, true) => (vec![p(0, -hy), p(0, hy)], false),
            (true, true) => (vec![p(-hx, -hy), p(hx, -hy), p(hx, hy), p(-hx, hy)], true),
        }
    };
    match *shape {
        PadShape::Circle { d } => Some((vec![p(0, 0)], false, d.0 as f64 / 2.0)),
        PadShape::Rect { w, h } => {
            let (pts, closed) = rect(w.0 / 2, h.0 / 2);
            closed.then_some((pts, true, 0.0))
        }
        PadShape::RoundRect { w, h, r } => {
            let (hw, hh) = (w.0 / 2, h.0 / 2);
            let r = r.0.clamp(0, hw.min(hh));
            let (pts, closed) = rect(hw - r, hh - r);
            (r > 0 || closed).then_some((pts, closed, r as f64))
        }
        PadShape::ChamferedRect { .. } => {
            // Straight edges only: rounded corners have no exact core here.
            let path = shape.outline_path()?;
            path.iter().all(|v| v.1.is_none()).then(|| (path.into_iter().map(|v| v.0).collect(), true, 0.0))
        }
        PadShape::Oval { w, h } => {
            let r = w.0.min(h.0) as f64 / 2.0;
            let (pts, _) = rect((w.0 - h.0).max(0) / 2, (h.0 - w.0).max(0) / 2);
            Some((pts, false, r))
        }
        PadShape::Trapezoid { w, h, dx, dy } if dx.0.abs() < h.0 && dy.0.abs() < w.0 => {
            let pts = shape.outline_path()?.into_iter().map(|v| v.0).collect::<Vec<_>>();
            Some((pts, true, 0.0))
        }
        PadShape::Polygon { ref points } if points.len() >= 3 => Some((points.clone(), true, 0.0)),
        _ => None,
    }
}

/// The distance between two cores' copper (between the cores, less both radii); zero or less
/// when they touch.
pub fn distance(a: &Core, b: &Core) -> f64 {
    if inside(a, b) || inside(b, a) {
        return -(a.radius + b.radius);
    }
    let mut best = f64::INFINITY;
    for (p, q) in edges(a) {
        for (r, s) in edges(b) {
            best = best.min(seg_seg(p, q, r, s));
        }
    }
    best - a.radius - b.radius
}

/// Whether an item's copper reaches into a closed area by more than `tol` nanometers: its core
/// is inside or crosses the area's outline, or comes closer to it than its radius less `tol`.
pub fn reaches_into(item: &Core, area: &Core, tol: f64) -> bool {
    if inside(area, item) || inside(item, area) {
        return true;
    }
    let mut best = f64::INFINITY;
    for (p, q) in edges(item) {
        for (r, s) in edges(area) {
            best = best.min(seg_seg(p, q, r, s));
        }
    }
    best == 0.0 || best < item.radius + area.radius - tol
}

/// Whether a vertex of `b` lies inside the closed polygon `a` (even-odd).
fn inside(a: &Core, b: &Core) -> bool {
    if !a.closed || a.points.len() < 3 {
        return false;
    }
    let Some(q) = b.points.first().map(|q| f(*q)) else { return false };
    let mut c = false;
    let n = a.points.len();
    for i in 0..n {
        let (p0, p1) = (f(a.points[i]), f(a.points[(i + 1) % n]));
        if (p0.1 > q.1) != (p1.1 > q.1) && q.0 < p0.0 + (q.1 - p0.1) * (p1.0 - p0.0) / (p1.1 - p0.1) {
            c = !c;
        }
    }
    c
}

type F = (f64, f64);

fn f(p: Point) -> F {
    (p.x.0 as f64, p.y.0 as f64)
}

fn edges(c: &Core) -> Vec<(F, F)> {
    let pts: Vec<F> = c.points.iter().map(|q| f(*q)).collect();
    match pts.len() {
        0 => vec![],
        1 => vec![(pts[0], pts[0])],
        n => {
            let mut v: Vec<(F, F)> = pts.windows(2).map(|w| (w[0], w[1])).collect();
            if c.closed && n > 2 {
                v.push((pts[n - 1], pts[0]));
            }
            v
        }
    }
}

fn point_seg(p: F, a: F, b: F) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

fn cross(o: F, a: F, b: F) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn seg_seg(p: F, q: F, r: F, s: F) -> f64 {
    let (d1, d2, d3, d4) = (cross(r, s, p), cross(r, s, q), cross(p, q, r), cross(p, q, s));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0)) && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0)) {
        return 0.0;
    }
    point_seg(p, r, s).min(point_seg(q, r, s)).min(point_seg(r, p, q)).min(point_seg(s, p, q))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn distances() {
        let via = Core { points: vec![pt(0, 0)], closed: false, radius: 300_000.0 };
        let track =
            Core { points: vec![pt(1_000_000, -500_000), pt(1_000_000, 500_000)], closed: false, radius: 100_000.0 };
        assert!((distance(&via, &track) - 600_000.0).abs() < 1e-6);
        let square = Core {
            points: vec![pt(-100, -100), pt(100, -100), pt(100, 100), pt(-100, 100)],
            closed: true,
            radius: 50.0,
        };
        let far = Core { points: vec![pt(400, 0)], closed: false, radius: 100.0 };
        assert!((distance(&square, &far) - 150.0).abs() < 1e-9);
        let within = Core { points: vec![pt(10, 10)], closed: false, radius: 1.0 };
        assert!(distance(&square, &within) < 0.0);
        // A round rectangle's corner.
        let (pts, closed, r) =
            shape_core(&PadShape::RoundRect { w: Nm(200_000), h: Nm(875_000), r: Nm(50_000) }).unwrap();
        assert!(closed && r == 50_000.0);
        assert_eq!(pts[2], pt(50_000, 387_500));
    }
}
