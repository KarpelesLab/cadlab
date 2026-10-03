//! Geometry of the drawn elements of a sheet: symbol bodies, designator/value text, labels and
//! power symbols as boxes, wires and frame borders as segments. The layout keeps these from
//! overlapping while it places things, and [`overlaps`] checks a finished sheet with the same
//! geometry (text extents from the stroke font, [`crate::render::font::width`]).

use super::symbol::{STYLE, body_across, body_half, field_positions, local_extent, place, rot, symbol_of};
use super::{Dir, LabelKind, Placement, SheetLayout};
use crate::geom::Point;
use crate::model::Project;
use crate::model::part::{Part, Side, Symbol, SymbolStyle};
use crate::render::{HAlign, VAlign, font};
use crate::units::Nm;

/// Axis-aligned box `[x0, y0, x1, y1]` in nanometers.
pub(crate) type Rect = [i64; 4];

/// Boxes closer than this are considered touching, not overlapping (50 µm).
const EPS: i64 = 50_000;

pub(crate) fn nm(v: f64) -> i64 {
    (v * 1e6).round() as i64
}

/// Box of a text: cap height `size` above the baseline, descenders a quarter below.
pub(crate) fn text_rect(text: &str, at: (f64, f64), size: f64, h: HAlign, v: VAlign, q: u8) -> Rect {
    let w = font::width(text, size);
    let x0 = match h {
        HAlign::Left => 0.0,
        HAlign::Center => -w / 2.0,
        HAlign::Right => -w,
    };
    let base = match v {
        VAlign::Top => -size,
        VAlign::Middle => -size / 2.0,
        VAlign::Bottom => 0.0,
    };
    let (a, b) = (rot((nm(x0), nm(base - size * 0.25)), q), rot((nm(x0 + w), nm(base + size)), q));
    let (ax, ay) = (nm(at.0), nm(at.1));
    [ax + a.0.min(b.0), ay + a.1.min(b.1), ax + a.0.max(b.0), ay + a.1.max(b.1)]
}

/// Body boxes of a placed symbol: a box symbol's body with its pins; a two-terminal symbol's
/// drawing plus a thin box along its pin lines (a power symbol may sit beside a pin line).
pub(crate) fn body_rects(sym: &Symbol, pl: &Placement) -> Vec<Rect> {
    let local: Vec<(f64, f64, f64, f64)> = if sym.style == SymbolStyle::Box {
        // Body, and each pin's line with its number beside it.
        let (hw, hh) = sym.body.map_or((5.08, 5.08), |(w, h)| (w.0 as f64 / 2e6, h.0 as f64 / 2e6));
        let mut v = vec![(-hw, -hh, hw, hh)];
        for p in &sym.pins {
            let Some(at) = p.at else { continue };
            let (x, y) = (at.x.0 as f64 / 1e6, at.y.0 as f64 / 1e6);
            v.push(match p.side {
                Some(Side::Left) => (x, y - 0.3, -hw, y + 1.4),
                Some(Side::Right) => (hw, y - 0.3, x, y + 1.4),
                Some(Side::Top) => (x - 1.4, hh, x + 0.3, y),
                _ => (x - 1.4, y, x + 0.3, -hh),
            });
        }
        let _ = local_extent;
        v
    } else {
        let (below, above) = body_across(sym.style);
        let half = sym.pins.iter().filter_map(|p| p.at).map(|a| a.x.0.abs()).max().unwrap_or(nm(3.81)) as f64 / 1e6;
        let core = body_half(sym.style).max(1.5);
        vec![(-core, -below, core, above), (-(half - 1.4), -0.15, half - 1.4, 0.15)]
    };
    local
        .into_iter()
        .map(|(x0, y0, x1, y1)| {
            let a = place(Point::new(Nm(nm(x0)), Nm(nm(y0))), pl);
            let b = place(Point::new(Nm(nm(x1)), Nm(nm(y1))), pl);
            [a.x.0.min(b.x.0), a.y.0.min(b.y.0), a.x.0.max(b.x.0), a.y.0.max(b.y.0)]
        })
        .collect()
}

/// Boxes of a placed symbol: body (see [`body_rects`]), then designator and value.
pub(crate) fn symbol_rects(sym: &Symbol, pl: &Placement, refdes: &str, value: &str) -> Vec<Rect> {
    let [r, v] = field_positions(sym, pl);
    let ts = STYLE.text_size;
    let mut out = body_rects(sym, pl);
    out.push(text_rect(refdes, r.at, ts, r.h, r.v, 0));
    out.push(text_rect(value, v.at, ts, v.h, v.v, 0));
    out
}

/// Net flag length along its direction (mm), as drawn.
pub(crate) fn flag_len(net: &str) -> f64 {
    font::width(net, STYLE.text_size) + 2.0
}

/// Where a wire label's text is anchored and aligned, as drawn: beside the wire (above a
/// horizontal one, left of a vertical one), starting at the label point.
pub(crate) fn wire_text(at: (f64, f64), dir: Dir) -> ((f64, f64), HAlign, u8) {
    match dir {
        Dir::Right => ((at.0, at.1 + 0.4), HAlign::Left, 0),
        Dir::Left => ((at.0, at.1 + 0.4), HAlign::Right, 0),
        Dir::Up => ((at.0 - 0.4, at.1), HAlign::Left, 1),
        Dir::Down => ((at.0 - 0.4, at.1), HAlign::Right, 1),
    }
}

/// Power symbol geometry, as drawn: stub length (mm) and text anchor/alignment.
pub(crate) fn power_text(at: (f64, f64), dir: Dir) -> ((f64, f64), HAlign, VAlign) {
    let (dx, dy) = dir.vec();
    let stub = 2.0 + 0.6;
    let p = (at.0 + dx as f64 * stub, at.1 + dy as f64 * stub);
    let (h, v) = match dir {
        Dir::Up => (HAlign::Center, VAlign::Bottom),
        Dir::Down => (HAlign::Center, VAlign::Top),
        Dir::Right => (HAlign::Left, VAlign::Middle),
        Dir::Left => (HAlign::Right, VAlign::Middle),
    };
    (p, h, v)
}

/// Box of a label or power symbol.
pub(crate) fn label_rect(at: Point, dir: Dir, net: &str, kind: LabelKind) -> Rect {
    let (x, y) = (at.x.0, at.y.0);
    let ts = STYLE.text_size;
    let along_across = |len: i64, across: i64| -> Rect {
        match dir {
            Dir::Right => [x, y - across, x + len, y + across],
            Dir::Left => [x - len, y - across, x, y + across],
            Dir::Up => [x - across, y, x + across, y + len],
            Dir::Down => [x - across, y - len, x + across, y],
        }
    };
    let mmp = (x as f64 / 1e6, y as f64 / 1e6);
    match kind {
        LabelKind::Net => along_across(nm(flag_len(net)), nm(1.0)),
        LabelKind::Wire => {
            let (p, h, q) = wire_text(mmp, dir);
            text_rect(net, p, ts, h, VAlign::Bottom, q)
        }
        LabelKind::Power => {
            let sym = along_across(nm(2.0), nm(1.0));
            let (p, h, v) = power_text(mmp, dir);
            union(sym, text_rect(net, p, ts, h, v, 0))
        }
        LabelKind::Ground => along_across(nm(2.5), nm(1.3)),
    }
}

pub(crate) fn union(a: Rect, b: Rect) -> Rect {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Whether two boxes overlap by more than [`EPS`].
pub(crate) fn rects_overlap(a: &Rect, b: &Rect) -> bool {
    a[0] < b[2] - EPS && b[0] < a[2] - EPS && a[1] < b[3] - EPS && b[1] < a[3] - EPS
}

/// Whether an axis-aligned segment enters the inside of a box (running along its edge or ending
/// on it does not count).
pub(crate) fn seg_hits_rect(a: Point, b: Point, r: &Rect) -> bool {
    let (x0, x1) = (a.x.0.min(b.x.0), a.x.0.max(b.x.0));
    let (y0, y1) = (a.y.0.min(b.y.0), a.y.0.max(b.y.0));
    if x0 == x1 {
        x0 > r[0] + EPS && x0 < r[2] - EPS && y0 < r[3] - EPS && y1 > r[1] + EPS
    } else if y0 == y1 {
        y0 > r[1] + EPS && y0 < r[3] - EPS && x0 < r[2] - EPS && x1 > r[0] + EPS
    } else {
        rects_overlap(&[x0, y0, x1, y1], r)
    }
}

/// Whether `p` lies on segment `a`–`b` (ends included).
pub(crate) fn on_seg(p: Point, a: Point, b: Point) -> bool {
    let (x0, x1) = (a.x.0.min(b.x.0), a.x.0.max(b.x.0));
    let (y0, y1) = (a.y.0.min(b.y.0), a.y.0.max(b.y.0));
    (x0 == x1 && p.x.0 == x0 && p.y.0 >= y0 && p.y.0 <= y1) || (y0 == y1 && p.y.0 == y0 && p.x.0 >= x0 && p.x.0 <= x1)
}

/// Whether two axis-aligned segments cross (interior to both) or overlap along a length.
pub(crate) fn segs_cross(a: Point, b: Point, c: Point, d: Point) -> bool {
    let h1 = a.y == b.y;
    let h2 = c.y == d.y;
    let (ax0, ax1) = (a.x.0.min(b.x.0), a.x.0.max(b.x.0));
    let (ay0, ay1) = (a.y.0.min(b.y.0), a.y.0.max(b.y.0));
    let (cx0, cx1) = (c.x.0.min(d.x.0), c.x.0.max(d.x.0));
    let (cy0, cy1) = (c.y.0.min(d.y.0), c.y.0.max(d.y.0));
    match (h1, h2) {
        (true, true) => ay0 == cy0 && ax0.max(cx0) < ax1.min(cx1),
        (false, false) => ax0 == cx0 && ay0.max(cy0) < ay1.min(cy1),
        (true, false) => cx0 > ax0 && cx0 < ax1 && ay0 > cy0 && ay0 < cy1,
        (false, true) => ax0 > cx0 && ax0 < cx1 && cy0 > ay0 && cy0 < ay1,
    }
}

/// A drawn element, for overlap checks.
#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Rect(Rect),
    Seg(Point, Point),
}

/// The drawn elements of a sheet with a description each, and an owner: elements with the same
/// non-zero owner (the parts of one symbol) are not checked against each other.
pub(crate) fn elements(p: &Project, l: &SheetLayout) -> Vec<(String, Shape, usize)> {
    let mut out = Vec::new();
    let lib = p.library();
    for (k, (r, pl)) in l.placements.iter().enumerate() {
        let Some(part) = p.circuit().components.get(r).and_then(|c| lib.parts.get(&c.part)) else { continue };
        let sym = symbol_of(part);
        let rects = symbol_rects(&sym, pl, r, &Part::value(part));
        let n = rects.len();
        for (i, b) in rects.into_iter().enumerate() {
            let what = match n - i {
                2 => "designator",
                1 => "value",
                _ => "body",
            };
            out.push((format!("{r} {what}"), Shape::Rect(b), k + 1));
        }
    }
    for lb in &l.labels {
        let what = format!("{:?} label {} at ({}, {})", lb.kind, lb.net, lb.at.x, lb.at.y);
        out.push((what, Shape::Rect(label_rect(lb.at, lb.dir, &lb.net, lb.kind)), 0));
    }
    for (a, b) in &l.wires {
        out.push((format!("wire ({}, {})-({}, {})", a.x, a.y, b.x, b.y), Shape::Seg(*a, *b), 0));
    }
    for f in &l.frames {
        let (a, b) = (f.min, f.max);
        let c = [a, Point::new(b.x, a.y), b, Point::new(a.x, b.y)];
        for i in 0..4 {
            out.push((format!("frame {}", f.title), Shape::Seg(c[i], c[(i + 1) % 4]), 0));
        }
        let (at, size) = frame_title(f);
        out.push((
            format!("frame {} title", f.title),
            Shape::Rect(text_rect(&f.title, at, size, HAlign::Left, VAlign::Top, 0)),
            0,
        ));
    }
    out
}

/// Title anchor (mm, top-left of the text) and size of a frame.
pub(crate) fn frame_title(f: &super::Frame) -> ((f64, f64), f64) {
    ((f.min.x.0 as f64 / 1e6 + 1.27, f.max.y.0 as f64 / 1e6 - 1.27), 1.8)
}

/// Pairs of drawn elements of a sheet that overlap: boxes that overlap, wires that run through a
/// box, wires that cross or run over each other. Empty for a clean sheet.
pub fn overlaps(p: &Project, l: &SheetLayout) -> Vec<String> {
    let els = elements(p, l);
    let mut out = Vec::new();
    for i in 0..els.len() {
        for j in i + 1..els.len() {
            if els[i].2 != 0 && els[i].2 == els[j].2 {
                continue;
            }
            let hit = match (&els[i].1, &els[j].1) {
                (Shape::Rect(a), Shape::Rect(b)) => rects_overlap(a, b),
                (Shape::Rect(r), Shape::Seg(a, b)) | (Shape::Seg(a, b), Shape::Rect(r)) => seg_hits_rect(*a, *b, r),
                (Shape::Seg(a, b), Shape::Seg(c, d)) => segs_cross(*a, *b, *c, *d),
            };
            if hit {
                out.push(format!("{} overlaps {}", els[i].0, els[j].0));
            }
        }
    }
    out
}
