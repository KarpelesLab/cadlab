//! Board geometry shared by every consumer (DRC, rendering, outputs, router): absolute pad shapes,
//! copper items per layer, connectivity and the ratsnest. See `docs/BOARD.md`.

use std::collections::BTreeMap;

pub mod zones;

use polyclip::{ArcTol, Circle, Curve, EndCap, Join, Path, Polygon, PolygonSet, Shape, Side};

use crate::geom::Point;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{BoardSide, Contour, PlacedFootprint, Segment, Track, Via};
use crate::model::circuit::PinRef;
use crate::model::footprint::{Footprint, Pad, PadKind, PadShape};
use crate::units::{Angle, Nm};

/// Arc approximation tolerance for copper shapes (1 µm, outward: never smaller than true copper).
pub const COPPER_TOL: ArcTol = ArcTol::new(1_000, Side::Outside);

/// The footprint used by a component: the placement's override, else the part's preferred one.
pub fn footprint_for<'a>(p: &'a Project, refdes: &str) -> Option<&'a Footprint> {
    let lib = p.library();
    let over = p.board().footprints.get(refdes).and_then(|f| f.footprint.as_deref());
    let name = match over {
        Some(n) => n.to_string(),
        None => {
            let comp = p.circuit().components.get(refdes)?;
            lib.parts.get(&comp.part)?.footprint()?.footprint.clone()
        }
    };
    lib.footprints.get(&name)
}

/// Footprint-local → board transform: mirror for the bottom side, rotate, translate.
pub fn transform(pf: &PlacedFootprint) -> impl Fn(Point) -> Point + '_ {
    move |p: Point| {
        let p = if pf.side == BoardSide::Bottom { Point::new(-p.x, p.y) } else { p };
        p.rotated(pf.rotation) + pf.at
    }
}

/// Maps a footprint layer name to the board side: `F.*` stays on top-side parts and becomes `B.*`
/// on bottom-side ones.
pub fn side_layer(side: BoardSide, front: &str) -> String {
    match side {
        BoardSide::Top => front.to_string(),
        BoardSide::Bottom => front.strip_prefix("F.").map(|r| format!("B.{r}")).unwrap_or_else(|| front.to_string()),
    }
}

/// A pad on the board.
#[derive(Clone, Debug)]
pub struct PlacedPad {
    /// Component.
    pub refdes: String,
    /// Pad number.
    pub number: String,
    /// Net, from the circuit through the part's pin → pad map.
    pub net: Option<String>,
    /// Center on the board.
    pub center: Point,
    /// Copper shape on the board.
    pub shape: Polygon,
    /// Copper layers it exists on.
    pub layers: Vec<String>,
    /// Hole, if any: (diameter, plated).
    pub hole: Option<(Nm, bool)>,
    /// The footprint pad.
    pub pad: Pad,
    /// Side of the footprint.
    pub side: BoardSide,
}

fn pt(p: Point) -> polyclip::Point {
    p.into()
}

/// Local pad outline as a curved shape centered at the origin (before pad rotation).
fn pad_shape_local(shape: &PadShape) -> Result<Polygon, polyclip::Error> {
    let rect_round = |w: Nm, h: Nm, r: Nm| -> Result<Polygon, polyclip::Error> {
        let (hw, hh) = (w.0 / 2, h.0 / 2);
        let r = r.0.clamp(0, hw.min(hh));
        let p = |x: i64, y: i64| polyclip::Point::new(x, y);
        if r == 0 {
            return Ok(Polygon::new(vec![p(-hw, -hh), p(hw, -hh), p(hw, hh), p(-hw, hh)], vec![]));
        }
        let (ix, iy) = (hw - r, hh - r);
        let c = vec![
            Curve::Line(p(-ix, -hh)),
            Curve::Line(p(ix, -hh)),
            Curve::CenterArc { center: p(ix, -iy), end: p(hw, -iy), ccw: true },
            Curve::Line(p(hw, iy)),
            Curve::CenterArc { center: p(ix, iy), end: p(ix, hh), ccw: true },
            Curve::Line(p(-ix, hh)),
            Curve::CenterArc { center: p(-ix, iy), end: p(-hw, iy), ccw: true },
            Curve::Line(p(-hw, -iy)),
            Curve::CenterArc { center: p(-ix, -iy), end: p(-ix, -hh), ccw: true },
        ];
        Shape::new(c, vec![]).to_polygon(COPPER_TOL)
    };
    match *shape {
        PadShape::Rect { w, h } => rect_round(w, h, Nm::ZERO),
        PadShape::RoundRect { w, h, r } => rect_round(w, h, r),
        PadShape::Oval { w, h } => rect_round(w, h, Nm(w.0.min(h.0) / 2)),
        PadShape::Circle { d } => Ok(Polygon::new(
            Circle { center: polyclip::Point::new(0, 0), radius: d.0 / 2 }.to_ring(COPPER_TOL)?,
            vec![],
        )),
    }
}

/// Pad number → net for a component, through its part's footprint pin map.
pub fn pad_nets(p: &Project, refdes: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(comp) = p.circuit().components.get(refdes) else { return out };
    let Some(part) = p.library().parts.get(&comp.part) else { return out };
    let fref = part.footprint();
    for pin in &part.symbol.pins {
        let Some(net) = p.circuit().net_of(&PinRef::new(refdes, &pin.number)) else { continue };
        let pads = fref.map(|f| f.pads_for(&pin.number)).unwrap_or_else(|| vec![pin.number.clone()]);
        for pad in pads {
            out.insert(pad, net.to_string());
        }
    }
    out
}

/// Every pad of every placed footprint.
pub fn placed_pads(p: &Project) -> Vec<PlacedPad> {
    let board = p.board();
    let copper = board.stackup.copper_names();
    let mut out = Vec::new();
    for (refdes, pf) in &board.footprints {
        let Some(fp) = footprint_for(p, refdes) else { continue };
        let nets = pad_nets(p, refdes);
        let tf = transform(pf);
        for pad in &fp.pads {
            let Ok(local) = pad_shape_local(&pad.shape) else { continue };
            let place = |q: polyclip::Point| -> polyclip::Point {
                let lp = Point::new(Nm(q.x), Nm(q.y)).rotated(pad.rotation) + pad.at;
                pt(tf(lp))
            };
            let shape = Polygon::new(
                local.outer.0.iter().map(|q| place(*q)).collect::<Vec<_>>(),
                local.holes.iter().map(|h| h.0.iter().map(|q| place(*q)).collect::<Vec<_>>().into()).collect(),
            );
            let (layers, hole) = match pad.kind {
                PadKind::Smd => (vec![side_layer(pf.side, "F.Cu")], None),
                PadKind::Tht { drill } => (copper.clone(), Some((drill, true))),
                PadKind::Npth { drill } => (vec![], Some((drill, false))),
            };
            out.push(PlacedPad {
                refdes: refdes.clone(),
                number: pad.number.clone(),
                net: if pad.number.is_empty() { None } else { nets.get(&pad.number).cloned() },
                center: tf(pad.at),
                shape,
                layers,
                hole,
                pad: pad.clone(),
                side: pf.side,
            });
        }
    }
    out
}

/// Copper shape of a track (stadium, or a thick arc).
pub fn track_shape(t: &Track) -> PolygonSet {
    let path: Vec<polyclip::Point> = match t.mid {
        None => vec![pt(t.start), pt(t.end)],
        Some(mid) => arc_points(t.start, mid, t.end, 1_000).into_iter().map(pt).collect(),
    };
    polyclip::offset_paths(&vec![Path(path)], t.width.0 / 2, Join::Round, EndCap::Round, COPPER_TOL).unwrap_or_default()
}

/// Points along the arc through `a`, `m`, `b`, with chord error at most `tol` nm.
pub fn arc_points(a: Point, m: Point, b: Point, tol: i64) -> Vec<Point> {
    let f = |p: Point| (p.x.0 as f64, p.y.0 as f64);
    let ((ax, ay), (mx, my), (bx, by)) = (f(a), f(m), f(b));
    let d = 2.0 * (ax * (my - by) + mx * (by - ay) + bx * (ay - my));
    if d.abs() < 1e-6 {
        return vec![a, b];
    }
    let (a2, m2, b2) = (ax * ax + ay * ay, mx * mx + my * my, bx * bx + by * by);
    let cx = (a2 * (my - by) + m2 * (by - ay) + b2 * (ay - my)) / d;
    let cy = (a2 * (bx - mx) + m2 * (ax - bx) + b2 * (mx - ax)) / d;
    let r = ((ax - cx).powi(2) + (ay - cy).powi(2)).sqrt();
    let ang = |x: f64, y: f64| (y - cy).atan2(x - cx);
    let (t0, tm, t1) = (ang(ax, ay), ang(mx, my), ang(bx, by));
    // Sweep from t0 to t1 passing through tm.
    let tau = std::f64::consts::TAU;
    let norm = |t: f64| t.rem_euclid(tau);
    let ccw = norm(tm - t0) < norm(t1 - t0);
    let sweep = if ccw { norm(t1 - t0) } else { -norm(t0 - t1) };
    let step = 2.0 * (1.0 - (tol as f64 / r).min(1.0)).acos();
    let n = ((sweep.abs() / step.max(1e-3)).ceil() as usize).clamp(2, 4096);
    (0..=n)
        .map(|i| {
            if i == 0 {
                return a;
            }
            if i == n {
                return b;
            }
            let t = t0 + sweep * i as f64 / n as f64;
            Point::new(Nm((cx + r * t.cos()).round() as i64), Nm((cy + r * t.sin()).round() as i64))
        })
        .collect()
}

/// Copper shape of a via pad.
pub fn via_shape(v: &Via) -> Polygon {
    let ring = Circle { center: pt(v.at), radius: v.diameter.0 / 2 }.to_ring(COPPER_TOL).unwrap_or_default();
    Polygon::new(ring, vec![])
}

/// Copper layers a via spans.
pub fn via_layers(p: &Project, v: &Via) -> Vec<String> {
    let names = p.board().stackup.copper_names();
    let a = names.iter().position(|n| *n == v.from).unwrap_or(0);
    let b = names.iter().position(|n| *n == v.to).unwrap_or(names.len() - 1);
    names[a.min(b)..=a.max(b)].to_vec()
}

/// What a copper item is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemRef {
    /// A pad: (designator, pad number).
    Pad(String, String),
    /// A track.
    Track(ObjectId),
    /// A via.
    Via(ObjectId),
    /// One island of a zone fill: (zone, copper layer, island index in the fill).
    Zone(ObjectId, String, usize),
}

impl std::fmt::Display for ItemRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemRef::Pad(r, n) => write!(f, "{r}.{n}"),
            ItemRef::Track(id) => write!(f, "track#{}", id.0),
            ItemRef::Via(id) => write!(f, "via#{}", id.0),
            ItemRef::Zone(id, layer, i) => write!(f, "zone#{}@{layer}/{i}", id.0),
        }
    }
}

/// A piece of copper.
#[derive(Clone, Debug)]
pub struct CopperItem {
    /// Identity.
    pub item: ItemRef,
    /// Net.
    pub net: Option<String>,
    /// Layers.
    pub layers: Vec<String>,
    /// Shape (same on every layer).
    pub shape: PolygonSet,
    /// A representative point (pad center, via center, track start).
    pub anchor: Point,
}

/// All copper items: pads, tracks, vias and zone fills (one item per fill island, computed
/// from the other items by [`zones::fill_zones`]).
pub fn copper_items(p: &Project) -> Vec<CopperItem> {
    let mut out = base_copper_items(p);
    let z = zones::zone_items(p, &out);
    out.extend(z);
    out
}

/// Copper items other than zone fills: pads, tracks, vias.
pub fn base_copper_items(p: &Project) -> Vec<CopperItem> {
    let mut out: Vec<CopperItem> = placed_pads(p)
        .into_iter()
        .filter(|pp| !pp.layers.is_empty())
        .map(|pp| CopperItem {
            item: ItemRef::Pad(pp.refdes.clone(), pp.number.clone()),
            net: pp.net.clone(),
            layers: pp.layers.clone(),
            shape: vec![pp.shape.clone()],
            anchor: pp.center,
        })
        .collect();
    for t in &p.board().tracks {
        out.push(CopperItem {
            item: ItemRef::Track(t.id),
            net: t.net.clone(),
            layers: vec![t.layer.clone()],
            shape: track_shape(t),
            anchor: t.start,
        });
    }
    for v in &p.board().vias {
        out.push(CopperItem {
            item: ItemRef::Via(v.id),
            net: v.net.clone(),
            layers: via_layers(p, v),
            shape: vec![via_shape(v)],
            anchor: v.at,
        });
    }
    out
}

fn touches(a: &CopperItem, b: &CopperItem) -> bool {
    if !a.layers.iter().any(|l| b.layers.contains(l)) {
        return false;
    }
    let (Some(ba), Some(bb)) = (polyclip::Geometry::bbox(&a.shape), polyclip::Geometry::bbox(&b.shape)) else {
        return false;
    };
    if !ba.intersects(&bb) {
        return false;
    }
    polyclip::intersects(&a.shape, &b.shape)
}

/// Copper islands: groups of items that touch (same layer, overlapping shapes; vias and
/// through-hole pads join layers). Returns the island index of every item.
pub fn islands(items: &[CopperItem]) -> Vec<usize> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut i = i;
        while p[i] != r {
            let next = p[i];
            p[i] = r;
            i = next;
        }
        r
    }
    for i in 0..n {
        for j in i + 1..n {
            if touches(&items[i], &items[j]) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[b] = a;
                }
            }
        }
    }
    (0..n).map(|i| find(&mut parent, i)).collect()
}

/// An unrouted connection.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RatLine {
    /// Net.
    pub net: String,
    /// One end (item label, e.g. `U1.3` or `via#4`).
    pub from: String,
    /// Its position.
    pub from_at: Point,
    /// Other end.
    pub to: String,
    /// Its position.
    pub to_at: Point,
    /// Straight-line length.
    pub length: Nm,
}

fn dist(a: Point, b: Point) -> i64 {
    let (dx, dy) = ((a.x.0 - b.x.0) as f64, (a.y.0 - b.y.0) as f64);
    (dx * dx + dy * dy).sqrt().round() as i64
}

/// Unrouted connections: for each net, the shortest links joining its copper islands
/// (minimum spanning tree between islands, measured between pad/via anchors).
pub fn ratsnest(p: &Project) -> Vec<RatLine> {
    let items = copper_items(p);
    let isl = islands(&items);
    let mut by_net: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(n) = &it.net
            && !matches!(it.item, ItemRef::Track(_) | ItemRef::Zone(..))
        {
            by_net.entry(n.as_str()).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for (net, idx) in by_net {
        // Islands of this net.
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in idx {
            groups.entry(isl[i]).or_default().push(i);
        }
        let groups: Vec<Vec<usize>> = groups.into_values().collect();
        if groups.len() < 2 {
            continue;
        }
        // Prim's algorithm over islands; edge weight = closest anchor pair.
        let mut in_tree = vec![false; groups.len()];
        in_tree[0] = true;
        for _ in 1..groups.len() {
            let mut best: Option<(i64, usize, usize, usize)> = None;
            for (gi, g) in groups.iter().enumerate().filter(|(gi, _)| in_tree[*gi]) {
                let _ = gi;
                for (hj, h) in groups.iter().enumerate().filter(|(hj, _)| !in_tree[*hj]) {
                    for &a in g {
                        for &b in h {
                            let d = dist(items[a].anchor, items[b].anchor);
                            if best.is_none_or(|x| d < x.0) {
                                best = Some((d, a, b, hj));
                            }
                        }
                    }
                }
            }
            let Some((d, a, b, hj)) = best else { break };
            in_tree[hj] = true;
            out.push(RatLine {
                net: net.to_string(),
                from: items[a].item.to_string(),
                from_at: items[a].anchor,
                to: items[b].item.to_string(),
                to_at: items[b].anchor,
                length: Nm(d),
            });
        }
    }
    out
}

/// Outline contour as a polygon ring (arcs approximated).
pub fn contour_ring(c: &Contour, tol: ArcTol) -> Vec<polyclip::Point> {
    let mut curves = vec![Curve::Line(pt(c.start))];
    for s in &c.segments {
        curves.push(match *s {
            Segment::Line { to } => Curve::Line(pt(to)),
            Segment::Arc { mid, to } => Curve::Arc { mid: pt(mid), end: pt(to) },
        });
    }
    Shape::new(curves, vec![]).to_polygon(tol).map(|p| p.outer.0).unwrap_or_default()
}

/// Rotation helper for commands: normalizes to [0°, 360°).
pub fn norm(a: Angle) -> Angle {
    a.normalized()
}
