//! Board geometry shared by every consumer (DRC, rendering, outputs, router): absolute pad shapes,
//! copper items per layer, connectivity and the ratsnest. See `docs/BOARD.md`.

use std::collections::BTreeMap;

pub mod clearance;
pub mod holes;
pub mod place;
pub mod prepared;
pub mod zones;

use polyclip::{ArcTol, Circle, Curve, EndCap, Join, Path, Polygon, PolygonSet, Shape, Side};

use crate::geom::Point;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{BoardSide, Contour, GraphicKind, PadConnection, PlacedFootprint, Segment, Track, Via};
use crate::model::footprint::{Footprint, Overrides, Pad, PadKind, PadShape};
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
    /// Side of the board the pad's copper faces: the footprint's, or the other one for an SMD
    /// pad on the back of its footprint ([`Pad::back`]). Through-hole pads: the footprint's.
    pub pad_side: BoardSide,
    /// Local settings in effect (the pad's, else its footprint's).
    pub overrides: Overrides,
    /// A slotted hole: the centers of its two end circles on the board (the hole diameter is
    /// the slot width).
    pub slot: Option<(Point, Point)>,
    /// The footprint's net-tie group holding this pad.
    pub tie: Option<usize>,
}

impl PlacedPad {
    /// The drilled hole's outline (a circle, or a stadium for a slot), if the pad has a hole.
    pub fn hole_shape(&self) -> Option<Polygon> {
        let (d, _) = self.hole?;
        let path = match self.slot {
            Some((a, b)) => vec![pt(a), pt(b)],
            None => vec![pt(self.center)],
        };
        if path.len() == 1 {
            let ring = Circle { center: path[0], radius: d.0 / 2 }.to_ring(COPPER_TOL).ok()?;
            return Some(Polygon::new(ring, vec![]));
        }
        polyclip::offset_paths(&vec![Path(path)], d.0 / 2, Join::Round, EndCap::Round, COPPER_TOL)
            .ok()?
            .into_iter()
            .next()
    }

    /// Whether the pad has a solder mask opening on `side` ([`Pad::mask`]).
    pub fn mask_on(&self, side: BoardSide) -> bool {
        use crate::model::footprint::MaskOpening;
        match (self.pad.kind, self.pad.mask) {
            (_, MaskOpening::None) => false,
            (PadKind::Smd, _) => self.pad_side == side,
            (_, MaskOpening::Pad) => true,
            (_, MaskOpening::Front) => self.side == side,
            (_, MaskOpening::Back) => self.side != side,
        }
    }

    /// Whether the pad has a plated hole.
    pub fn plated(&self) -> bool {
        matches!(self.hole, Some((_, true)))
    }
}

/// The side an SMD pad's copper is on: its footprint's, or the other one for a pad on the
/// back of its footprint.
pub fn pad_side(footprint: BoardSide, back: bool) -> BoardSide {
    match (footprint, back) {
        (s, false) => s,
        (BoardSide::Top, true) => BoardSide::Bottom,
        (BoardSide::Bottom, true) => BoardSide::Top,
    }
}

/// The slot of a footprint pad as the centers of its end circles, in footprint coordinates.
pub fn slot_ends(pad: &Pad) -> Option<(Point, Point)> {
    let (sx, sy) = pad.slot?;
    let half = Nm((sx.0 - sy.0).abs() / 2);
    if half.0 == 0 {
        return None;
    }
    let d = if sx > sy { Point::new(half, Nm::ZERO) } else { Point::new(Nm::ZERO, half) };
    let d = d.rotated(pad.rotation);
    Some((pad.at - d, pad.at + d))
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
        PadShape::Polygon { ref points } => {
            // Normalized (any orientation, self-touching outlines); a degenerate outline is
            // no copper.
            let ring: Vec<polyclip::Point> = points.iter().map(|&q| pt(q)).collect();
            let set = polyclip::union_all(&ring, polyclip::FillRule::NonZero)?;
            Ok(set.into_iter().max_by_key(|pg| pg.outer.signed_area2()).unwrap_or_else(|| Polygon::new(vec![], vec![])))
        }
    }
}

/// Pad number → net for a component, through its part's footprint pin map.
pub fn pad_nets(p: &Project, refdes: &str) -> BTreeMap<String, String> {
    let index = pin_nets(p, Some(refdes));
    pad_nets_in(p, refdes, index.get(refdes))
}

/// Pin number → net per component (the first net in name order holding the pin, as
/// [`Circuit::net_of`](crate::model::circuit::Circuit::net_of)), for one component or all.
fn pin_nets<'a>(p: &'a Project, only: Option<&str>) -> BTreeMap<&'a str, BTreeMap<&'a str, &'a str>> {
    let mut out: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for (name, net) in &p.circuit().nets {
        for pin in &net.pins {
            if only.is_none_or(|r| r == pin.refdes) {
                out.entry(pin.refdes.as_str()).or_default().entry(pin.pin.as_str()).or_insert(name.as_str());
            }
        }
    }
    out
}

fn pad_nets_in(p: &Project, refdes: &str, pins: Option<&BTreeMap<&str, &str>>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(comp) = p.circuit().components.get(refdes) else { return out };
    let Some(part) = p.library().parts.get(&comp.part) else { return out };
    let fref = part.footprint();
    for pin in &part.symbol.pins {
        let Some(net) = pins.and_then(|m| m.get(pin.number.as_str())) else { continue };
        let pads = fref.map(|f| f.pads_for(&pin.number)).unwrap_or_else(|| vec![pin.number.clone()]);
        for pad in pads {
            out.insert(pad, net.to_string());
        }
    }
    out
}

/// Every pad of every placed footprint, then board holes (mounting holes) as pads of
/// designator = hole name ([`holes::hole_pads`]).
pub fn placed_pads(p: &Project) -> Vec<PlacedPad> {
    let board = p.board();
    let copper = board.stackup.copper_names();
    let index = pin_nets(p, None);
    let mut out = Vec::new();
    for (refdes, pf) in &board.footprints {
        let Some(fp) = footprint_for(p, refdes) else { continue };
        let nets = pad_nets_in(p, refdes, index.get(refdes.as_str()));
        let tf = transform(pf);
        for pad in &fp.pads {
            let Ok(local) = pad_shape_local(&pad.shape) else { continue };
            let place = |q: polyclip::Point| -> polyclip::Point {
                let lp = Point::new(Nm(q.x), Nm(q.y)).rotated(pad.rotation) + pad.at;
                pt(tf(lp))
            };
            let mut shape = Polygon::new(
                local.outer.0.iter().map(|q| place(*q)).collect::<Vec<_>>(),
                local.holes.iter().map(|h| h.0.iter().map(|q| place(*q)).collect::<Vec<_>>().into()).collect(),
            );
            // The bottom side mirrors: restore the outer ring's counter-clockwise orientation
            // (and the holes' clockwise one), or a union of shapes under the non-zero rule (zone
            // keep-away offsets) would cancel where a bottom pad overlaps a track.
            if pf.side == BoardSide::Bottom {
                shape.outer.reverse_orientation();
                shape.holes.iter_mut().for_each(|h| h.reverse_orientation());
            }
            let ps = match pad.kind {
                PadKind::Smd => pad_side(pf.side, pad.back),
                _ => pf.side,
            };
            let (layers, hole) = match pad.kind {
                PadKind::Smd => (vec![side_layer(ps, "F.Cu")], None),
                PadKind::Tht { drill } => (copper.clone(), Some((drill, true))),
                PadKind::Npth { drill } => (vec![], Some((drill, false))),
            };
            let slot = if hole.is_some() { slot_ends(pad).map(|(a, b)| (tf(a), tf(b))) } else { None };
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
                pad_side: ps,
                overrides: fp.pad_overrides(pad),
                slot,
                tie: fp.tie_group(&pad.number),
            });
        }
    }
    out.extend(holes::hole_pads(p));
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

/// Length of a track: along its arc for an arc track.
pub fn track_length(t: &Track) -> Nm {
    let len = |pts: &[Point]| -> f64 {
        pts.windows(2).map(|w| ((w[1].x.0 - w[0].x.0) as f64).hypot((w[1].y.0 - w[0].y.0) as f64)).sum()
    };
    let l = match t.mid {
        None => len(&[t.start, t.end]),
        Some(m) => len(&arc_points(t.start, m, t.end, 10)),
    };
    Nm(l.round() as i64)
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
    /// A board drawing or text on a copper layer.
    Graphic(ObjectId),
    /// A copper drawing of a footprint: (designator, index in the footprint's graphics).
    FpGraphic(String, usize),
}

impl std::fmt::Display for ItemRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemRef::Pad(r, n) => write!(f, "{r}.{n}"),
            ItemRef::Track(id) => write!(f, "track#{}", id.0),
            ItemRef::Via(id) => write!(f, "via#{}", id.0),
            ItemRef::Zone(id, layer, i) => write!(f, "zone#{}@{layer}/{i}", id.0),
            ItemRef::Graphic(id) => write!(f, "graphic#{}", id.0),
            ItemRef::FpGraphic(r, i) => write!(f, "copper:{r}/{i}"),
        }
    }
}

impl ItemRef {
    /// The footprint (designator) the item belongs to: pads and footprint copper drawings.
    pub fn footprint(&self) -> Option<&str> {
        match self {
            ItemRef::Pad(r, _) | ItemRef::FpGraphic(r, _) => Some(r),
            _ => None,
        }
    }
}

/// Settings a copper item carries besides its shape: local overrides of pads and how a
/// footprint ties nets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemLocal {
    /// Local clearance (a pad's, else its footprint's).
    pub clearance: Option<Nm>,
    /// Zone connection override (pads).
    pub connection: Option<PadConnection>,
    /// A plated through-hole pad.
    pub through: bool,
    /// The net-tie group of its footprint it belongs to: (designator, group).
    pub tie: Option<(String, usize)>,
    /// For a copper drawing of a net-tie footprint: the nets of its group, which it may touch
    /// anywhere (it is the bridge between them).
    pub tie_nets: Vec<String>,
}

/// Whether two copper items of different nets may touch: both belong to one net-tie group of
/// the same footprint.
pub fn tied(a: &CopperItem, b: &CopperItem) -> bool {
    let bridges = |x: &CopperItem, y: &CopperItem| y.net.as_ref().is_some_and(|n| x.local.tie_nets.contains(n));
    a.net != b.net && ((a.local.tie.is_some() && a.local.tie == b.local.tie) || bridges(a, b) || bridges(b, a))
}

/// A piece of copper.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Local settings and net ties.
    pub local: ItemLocal,
}

/// All copper items: pads, tracks, vias and zone fills (one item per fill island, computed
/// from the other items by [`zones::fill_zones`]).
pub fn copper_items(p: &Project) -> Vec<CopperItem> {
    let mut out = base_copper_items(p);
    let z = zones::zone_items(p, &out);
    out.extend(z);
    out
}

/// Copper items other than zone fills: pads, tracks, vias, then copper drawings (footprint and
/// board).
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
            local: ItemLocal {
                clearance: pp.overrides.clearance,
                connection: pp.overrides.zone_connection,
                through: pp.plated(),
                tie: pp.tie.map(|g| (pp.refdes.clone(), g)),
                tie_nets: Vec::new(),
            },
        })
        .collect();
    for t in &p.board().tracks {
        out.push(CopperItem {
            item: ItemRef::Track(t.id),
            net: t.net.clone(),
            layers: vec![t.layer.clone()],
            shape: track_shape(t),
            anchor: t.start,
            local: ItemLocal::default(),
        });
    }
    for v in &p.board().vias {
        out.push(CopperItem {
            item: ItemRef::Via(v.id),
            net: v.net.clone(),
            layers: via_layers(p, v),
            shape: vec![via_shape(v)],
            anchor: v.at,
            local: ItemLocal::default(),
        });
    }
    let graphics = graphic_items(p, &out);
    out.extend(graphics);
    out
}

/// Stroke width of board texts: an eighth of the height, at least the minimum silkscreen
/// width (texts carry no stroke width of their own).
pub fn text_stroke_width(p: &Project, size: Nm) -> Nm {
    Nm((size.0 / 8).max(p.board().rules.min_silk_width.0))
}

/// Text strokes as polylines: centered on `at`, cap height `size`, rotated, mirrored (bottom
/// side) — the Hershey font every output uses.
pub fn text_paths(text: &str, at: Point, size: Nm, rotation: Angle, mirror: bool) -> Vec<Vec<Point>> {
    use crate::render::font::{self, HAlign, VAlign};
    let (s, c) = rotation.to_rad_f64().sin_cos();
    font::layout(text, (0.0, 0.0), size.0 as f64, HAlign::Center, VAlign::Middle, 0)
        .into_iter()
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.into_iter()
                .map(|(x, y)| {
                    let x = if mirror { -x } else { x };
                    let (rx, ry) = (x * c - y * s, x * s + y * c);
                    Point::new(Nm(rx.round() as i64) + at.x, Nm(ry.round() as i64) + at.y)
                })
                .collect()
        })
        .collect()
}

/// A filled polygon with its outline stroked by `width` (0: the area alone), as one region set.
pub fn polygon_area(points: &[Point], width: Nm) -> PolygonSet {
    let ring: Vec<polyclip::Point> = points.iter().map(|&q| pt(q)).collect();
    let mut s = polyclip::union_all(&ring, polyclip::FillRule::NonZero).unwrap_or_default();
    if width.0 > 0 && !points.is_empty() {
        let mut closed = points.to_vec();
        closed.push(points[0]);
        s.extend(stroke_set(vec![closed], width));
        s = polyclip::union_all(&s, polyclip::FillRule::NonZero).unwrap_or(s);
    }
    s
}

fn stroke_set(paths: Vec<Vec<Point>>, width: Nm) -> PolygonSet {
    let paths: Vec<Path> =
        paths.into_iter().filter(|p| !p.is_empty()).map(|p| Path(p.into_iter().map(pt).collect())).collect();
    if paths.is_empty() {
        return PolygonSet::new();
    }
    polyclip::offset_paths(&paths, (width.0 / 2).max(1), Join::Round, EndCap::Round, COPPER_TOL).unwrap_or_default()
}

/// A footprint drawing as a region in board coordinates: polylines stroked, polygons filled
/// (and stroked), circles filled or as rings.
pub fn fp_graphic_shape(pf: &PlacedFootprint, g: &crate::model::footprint::Graphic) -> PolygonSet {
    use crate::model::footprint::GraphicGeometry;
    let tf = transform(pf);
    let mut set = match &g.geometry {
        GraphicGeometry::Path { points } => stroke_set(vec![points.iter().map(|q| tf(*q)).collect()], g.width),
        GraphicGeometry::Polygon { points } => {
            let ring: Vec<polyclip::Point> = points.iter().map(|q| pt(tf(*q))).collect();
            let mut s = polyclip::union_all(&ring, polyclip::FillRule::NonZero).unwrap_or_default();
            if g.width.0 > 0 {
                let mut closed: Vec<Point> = points.iter().map(|q| tf(*q)).collect();
                if let Some(&f) = closed.first() {
                    closed.push(f);
                }
                s.extend(stroke_set(vec![closed], g.width));
            }
            s
        }
        GraphicGeometry::Circle { center, radius, filled } => {
            let c = pt(tf(*center));
            let outer = Circle { center: c, radius: radius.0 + g.width.0 / 2 }.to_ring(COPPER_TOL).unwrap_or_default();
            let inner = radius.0 - g.width.0 / 2;
            if *filled || inner <= 0 {
                vec![Polygon::new(outer, vec![])]
            } else {
                let mut hole =
                    Circle { center: c, radius: inner }.to_ring(ArcTol::new(1_000, Side::Inside)).unwrap_or_default();
                hole.reverse_orientation();
                vec![Polygon::new(outer, vec![hole])]
            }
        }
    };
    if set.len() > 1 {
        set = polyclip::union_all(&set, polyclip::FillRule::NonZero).unwrap_or(set);
    }
    set
}

/// Copper drawings as copper items: footprint copper drawings (on the net of the first pad of
/// their footprint they touch, in its net-tie group) and board drawings and texts on copper
/// layers (no net). `pads` are the footprint pads' items, used for the nets.
fn graphic_items(p: &Project, items: &[CopperItem]) -> Vec<CopperItem> {
    use crate::model::footprint::GraphicLayer;
    let board = p.board();
    let copper = board.stackup.copper_names();
    let mut out = Vec::new();
    for (refdes, pf) in &board.footprints {
        let Some(fp) = footprint_for(p, refdes) else { continue };
        for (i, g) in fp.graphics.iter().enumerate() {
            if g.layer != GraphicLayer::Copper {
                continue;
            }
            let layer = side_layer(pad_side(pf.side, g.back), "F.Cu");
            if !copper.contains(&layer) {
                continue;
            }
            let shape = fp_graphic_shape(pf, g);
            if shape.is_empty() {
                continue;
            }
            let pad = items.iter().find(|it| {
                matches!(&it.item, ItemRef::Pad(r, _) if r == refdes)
                    && it.layers.contains(&layer)
                    && polyclip::intersects(&it.shape, &shape)
            });
            let anchor = shape[0].outer.0.first().copied().map(Into::into).unwrap_or(pf.at);
            let tie = pad.and_then(|it| it.local.tie.clone());
            let mut tie_nets: Vec<String> = items
                .iter()
                .filter(|it| tie.is_some() && it.local.tie == tie)
                .filter_map(|it| it.net.clone())
                .collect();
            tie_nets.sort();
            tie_nets.dedup();
            out.push(CopperItem {
                item: ItemRef::FpGraphic(refdes.clone(), i),
                net: pad.and_then(|it| it.net.clone()),
                layers: vec![layer],
                shape,
                anchor,
                local: ItemLocal { tie: tie.clone(), tie_nets, ..Default::default() },
            });
        }
    }
    for g in &board.graphics {
        if !copper.contains(&g.layer) {
            continue;
        }
        let (shape, anchor) = match &g.kind {
            GraphicKind::Line { points, width } => (stroke_set(vec![points.clone()], *width), points.first().copied()),
            GraphicKind::Polygon { points, width } => (polygon_area(points, *width), points.first().copied()),
            GraphicKind::Text { text, at, size, rotation } => {
                let mirror = g.layer == "B.Cu";
                let paths = text_paths(text, *at, *size, *rotation, mirror);
                let set = stroke_set(paths, text_stroke_width(p, *size));
                let set = polyclip::union_all(&set, polyclip::FillRule::NonZero).unwrap_or(set);
                (set, Some(*at))
            }
        };
        if shape.is_empty() {
            continue;
        }
        out.push(CopperItem {
            item: ItemRef::Graphic(g.id),
            net: None,
            layers: vec![g.layer.clone()],
            shape,
            anchor: anchor.unwrap_or_default(),
            local: ItemLocal::default(),
        });
    }
    out
}

/// Union-find whose roots are the lowest index of their set.
struct MinRootSets(Vec<usize>);

impl MinRootSets {
    fn new(n: usize) -> Self {
        MinRootSets((0..n).collect())
    }
    fn find(&mut self, i: usize) -> usize {
        let p = &mut self.0;
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
    /// Joins the sets of `a` and `b` (roots, already found).
    fn join_roots(&mut self, a: usize, b: usize) {
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// Layer sets of items as bit masks (`None` past 128 distinct layer names).
fn layer_masks(items: &[CopperItem]) -> Option<Vec<u128>> {
    let mut names: BTreeMap<&str, u32> = BTreeMap::new();
    for it in items {
        for l in &it.layers {
            let k = names.len() as u32;
            names.entry(l.as_str()).or_insert(k);
        }
    }
    if names.len() > 128 {
        return None;
    }
    Some(items.iter().map(|it| it.layers.iter().fold(0u128, |m, l| m | (1u128 << names[l.as_str()]))).collect())
}

/// Copper islands: groups of items that touch (same layer, overlapping shapes; vias and
/// through-hole pads join layers). Returns the island index of every item: the lowest index
/// of the items in its island.
///
/// Candidate pairs come from a sweep over bounding boxes; pairs already known to be connected
/// skip the shape test, and large shapes (zone fills) are indexed once ([`prepared::Prepared`])
/// so each item is tested against the pour's nearby edges only. The result is exactly that of
/// testing every pair with `polyclip::intersects`.
pub fn islands(items: &[CopperItem]) -> Vec<usize> {
    let n = items.len();
    let mut sets = MinRootSets::new(n);
    let masks = layer_masks(items);
    let share = |i: usize, j: usize| match &masks {
        Some(m) => m[i] & m[j] != 0,
        None => items[i].layers.iter().any(|l| items[j].layers.contains(l)),
    };
    let boxes: Vec<Option<polyclip::Rect>> = items.iter().map(|it| polyclip::Geometry::bbox(&it.shape)).collect();
    let big: Vec<bool> =
        items.iter().map(|it| prepared::segment_count(&it.shape) >= prepared::PREPARE_MIN_SEGMENTS).collect();
    // Small items: sweep over bounding boxes sorted by their left edge.
    let mut order: Vec<(polyclip::Rect, usize)> =
        boxes.iter().enumerate().filter(|(i, _)| !big[*i]).filter_map(|(i, b)| b.map(|b| (b, i))).collect();
    order.sort_by_key(|(b, i)| (b.min.x, *i));
    for (k, (bi, i)) in order.iter().enumerate() {
        for (bj, j) in &order[k + 1..] {
            if bj.min.x > bi.max.x {
                break;
            }
            if !bi.intersects(bj) || !share(*i, *j) {
                continue;
            }
            let (a, b) = (sets.find(*i), sets.find(*j));
            if a != b && !tied(&items[*i], &items[*j]) && polyclip::intersects(&items[*i].shape, &items[*j].shape) {
                sets.join_roots(a, b);
            }
        }
    }
    // Large items against everything else.
    for b in (0..n).filter(|&b| big[b]) {
        let Some(bb) = boxes[b] else { continue };
        let prep = prepared::Prepared::new(&items[b].shape);
        for j in 0..n {
            if j == b || (big[j] && j < b) || !boxes[j].is_some_and(|bj| bj.intersects(&bb)) || !share(b, j) {
                continue;
            }
            let (x, y) = (sets.find(b), sets.find(j));
            if x != y && !tied(&items[b], &items[j]) && prep.intersects(&items[j].shape) {
                sets.join_roots(x, y);
            }
        }
    }
    (0..n).map(|i| sets.find(i)).collect()
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
    ratsnest_items(&copper_items(p))
}

/// [`ratsnest`] from copper items already computed (e.g. [`copper_items`]), for callers that
/// also use the items.
pub fn ratsnest_items(items: &[CopperItem]) -> Vec<RatLine> {
    ratsnest_from(items, &islands(items))
}

/// [`ratsnest`] from copper items and their [`islands`].
///
/// Per net, Prim's algorithm over islands, where the link between two islands is their closest
/// pair of anchors (pads and vias; tracks and zone fills are never endpoints). Ties go to the
/// lowest (length, tree island, new island, tree item, new item), islands ordered by island
/// index and items by index. Each out-of-tree anchor keeps its best link to the tree, so a net
/// with `k` anchors costs O(k²).
pub fn ratsnest_from(items: &[CopperItem], isl: &[usize]) -> Vec<RatLine> {
    let mut by_net: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(n) = &it.net
            && !matches!(it.item, ItemRef::Track(_) | ItemRef::Zone(..) | ItemRef::Graphic(_) | ItemRef::FpGraphic(..))
        {
            by_net.entry(n.as_str()).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for (net, idx) in by_net {
        // Islands of this net, in island order; anchors by item index within each.
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in idx {
            groups.entry(isl[i]).or_default().push(i);
        }
        if groups.len() < 2 {
            continue;
        }
        let groups: Vec<Vec<usize>> = groups.into_values().collect();
        let mut group_of: Vec<usize> = Vec::new();
        let mut anchors: Vec<usize> = Vec::new();
        for (g, members) in groups.iter().enumerate() {
            for &i in members {
                group_of.push(g);
                anchors.push(i);
            }
        }
        let starts: Vec<usize> = groups
            .iter()
            .scan(0usize, |acc, g| {
                let s = *acc;
                *acc += g.len();
                Some(s)
            })
            .collect();
        let mut in_tree = vec![false; groups.len()];
        // Best link of each anchor to the tree: (length, tree group, tree anchor).
        let mut best: Vec<Option<(i64, usize, usize)>> = vec![None; anchors.len()];
        let add = |g: usize, in_tree: &mut [bool], best: &mut [Option<(i64, usize, usize)>]| {
            in_tree[g] = true;
            for ka in starts[g]..starts[g] + groups[g].len() {
                let a = anchors[ka];
                for (kb, &b) in anchors.iter().enumerate() {
                    if in_tree[group_of[kb]] {
                        continue;
                    }
                    let cand = (dist(items[a].anchor, items[b].anchor), g, a);
                    if best[kb].is_none_or(|x| cand < x) {
                        best[kb] = Some(cand);
                    }
                }
            }
        };
        add(0, &mut in_tree, &mut best);
        for _ in 1..groups.len() {
            let mut pick: Option<(i64, usize, usize, usize, usize)> = None;
            for (kb, &b) in anchors.iter().enumerate() {
                let gb = group_of[kb];
                if in_tree[gb] {
                    continue;
                }
                let Some((d, ga, a)) = best[kb] else { continue };
                let key = (d, ga, gb, a, b);
                if pick.is_none_or(|k| key < k) {
                    pick = Some(key);
                }
            }
            let Some((d, _, gb, a, b)) = pick else { break };
            out.push(RatLine {
                net: net.to_string(),
                from: items[a].item.to_string(),
                from_at: items[a].anchor,
                to: items[b].item.to_string(),
                to_at: items[b].anchor,
                length: Nm(d),
            });
            add(gb, &mut in_tree, &mut best);
        }
    }
    out
}

/// Courtyard of a placed footprint in board coordinates; `None` when the component is not
/// placed, has no footprint or its courtyard is empty.
pub fn placed_courtyard(p: &Project, refdes: &str) -> Option<polyclip::Ring> {
    let pf = p.board().footprints.get(refdes)?;
    let fp = footprint_for(p, refdes)?;
    if fp.courtyard.len() < 3 {
        return None;
    }
    let tf = transform(pf);
    Some(fp.courtyard.iter().map(|q| pt(tf(*q))).collect())
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

#[cfg(test)]
mod tests;
