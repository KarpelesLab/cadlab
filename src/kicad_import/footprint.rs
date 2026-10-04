//! Footprints embedded in a `.kicad_pcb` → cadlab footprints and placements.
//!
//! KiCad stores each placed footprint with its children in footprint-local coordinates (Y down,
//! unrotated, already flipped for bottom-side footprints) and pad and text angles as absolute
//! orientations. cadlab footprints are top-view, Y up, and a bottom-side placement mirrors
//! x → −x. So for a footprint at orientation `o`:
//!
//! - top side: rotation `o`, local point `(x, −y)`, pad rotation `pad angle − o`;
//! - bottom side: rotation `o − 180°`, local point `(x, y)`, pad rotation `o − 180° − pad angle`.
//!
//! This is the exact inverse of the exporter (`crate::kicad_pcb`).

use std::collections::{BTreeMap, BTreeSet};

use super::{Ctx, K, Notes, at_of, child_xy, layer_of, mm, pts_of, width_of, yes};
use crate::board::arc_points;
use crate::diag::Diagnostic;
use crate::geom::{BBox, Point};
use crate::model::board::{BoardSide, PlacedFootprint};
use crate::model::footprint::{
    Footprint, Graphic, GraphicGeometry, GraphicLayer, Mount, Pad, PadKind, PadShape, Paste,
};
use crate::model::part::{PinKind, slugify, valid_id};
use crate::netlist::import::pin_kind;
use crate::refs::ObjectRef;
use crate::sexpr::Sexpr;
use crate::units::{Angle, Nm};

/// Arc approximation for footprint drawings (cadlab footprint graphics have no arcs).
const DRAW_TOL: i64 = 5_000;

/// A footprint pad's connection, as the board file gives it.
#[derive(Clone, Debug)]
pub(super) struct PadNet {
    /// Pad number.
    pub number: String,
    /// KiCad net name.
    pub net: Option<String>,
    /// Pin name (`pinfunction`).
    pub function: Option<String>,
    /// Pin type.
    pub kind: Option<PinKind>,
}

/// An Edge.Cuts drawing in board coordinates.
#[derive(Clone, Copy, Debug)]
pub(super) enum EdgeItem {
    Seg { a: Point, b: Point, mid: Option<Point> },
    Circle { center: Point, radius: Nm },
}

/// A converted footprint.
#[derive(Clone, Debug)]
pub(super) struct Converted {
    pub refdes: String,
    pub value: String,
    /// `lib:name` as written.
    pub lib_id: String,
    pub placement: PlacedFootprint,
    pub fp: Footprint,
    /// Properties other than Reference, Value and Footprint.
    pub props: BTreeMap<String, String>,
    pub attrs: BTreeSet<String>,
    pub pads: Vec<PadNet>,
    pub edges: Vec<EdgeItem>,
    /// (uuid, what it is: `fp`, `pad:<n>`, `prop:<name>`).
    pub uuids: Vec<(String, String)>,
}

/// Footprint layer of a KiCad layer name, for a footprint on `side`.
fn fp_layer(layer: &str, side: BoardSide) -> Option<GraphicLayer> {
    let front = match side {
        BoardSide::Top => layer.strip_prefix("F.")?,
        BoardSide::Bottom => layer.strip_prefix("B.")?,
    };
    Some(match front {
        "SilkS" | "Silkscreen" => GraphicLayer::Silk,
        "Fab" => GraphicLayer::Fab,
        "CrtYd" | "Courtyard" => GraphicLayer::Courtyard,
        _ => return None,
    })
}

/// Footprint name usable as a library ID.
pub(super) fn footprint_name(lib_id: &str, refdes: &str) -> String {
    let n = lib_id.rsplit_once(':').map_or(lib_id, |(_, n)| n).trim();
    if valid_id(n) {
        return n.to_string();
    }
    let s = slugify(n);
    if valid_id(&s) && n.chars().any(|c| c.is_ascii_alphanumeric()) { s } else { format!("fp_{}", slugify(refdes)) }
}

struct Local {
    side: BoardSide,
    orient: Angle,
}

impl Local {
    /// cadlab footprint-local point from KiCad footprint-local coordinates.
    fn pt(&self, (x, y): K) -> Point {
        match self.side {
            BoardSide::Top => Point::new(x, -y),
            BoardSide::Bottom => Point::new(x, y),
        }
    }

    /// cadlab pad rotation from KiCad's absolute pad angle.
    fn pad_rot(&self, a: Angle) -> Angle {
        match self.side {
            BoardSide::Top => a - self.orient,
            BoardSide::Bottom => self.orient - Angle::DEG_180 - a,
        }
        .normalized()
    }

    /// A pad-relative KiCad offset (Y down, before the pad rotation) as a pad-relative cadlab
    /// offset (Y up, before the cadlab pad rotation).
    fn pad_offset(&self, (x, y): K) -> Point {
        match self.side {
            BoardSide::Top => Point::new(x, -y),
            BoardSide::Bottom => Point::new(-x, -y),
        }
    }
}

/// Converts a `(footprint ...)` element. Returns `None` (with a diagnostic) when it has no
/// position.
pub(super) fn convert(e: &Sexpr, ctx: &Ctx, notes: &mut Notes) -> Option<Converted> {
    let lib_id = e.value().unwrap_or("").to_string();
    let side = match e.child_value("layer") {
        Some("B.Cu") => BoardSide::Bottom,
        _ => BoardSide::Top,
    };
    let Some((at_k, orient)) = at_of(e) else {
        notes.not_imported(
            Diagnostic::warning(
                "import.footprint_invalid",
                format!("footprint `{lib_id}` has no position; not imported"),
            )
            .with_hint("open and save the board in KiCad, then import it again"),
        );
        return None;
    };
    let orient = orient.normalized();
    let at = ctx.frame(at_k);
    let rotation = match side {
        BoardSide::Top => orient,
        BoardSide::Bottom => (orient - Angle::DEG_180).normalized(),
    };
    let loc = Local { side, orient };
    let mut uuids = Vec::new();
    if let Some(u) = e.child_value("uuid").or(e.child_value("tstamp")) {
        uuids.push((u.to_string(), "fp".to_string()));
    }

    // Fields: KiCad 8+ `(property "Reference" "R1")`, KiCad 6/7 `(fp_text reference "R1")` and
    // `(property "MPN" "...")`.
    let mut refdes = String::new();
    let mut value = String::new();
    let mut props = BTreeMap::new();
    for c in e.items() {
        match c.head() {
            Some("property") => {
                let it = c.items();
                let (Some(k), v) = (it.get(1).and_then(Sexpr::atom), it.get(2).and_then(Sexpr::atom)) else { continue };
                let v = v.unwrap_or("").to_string();
                if let Some(u) = c.child_value("uuid") {
                    uuids.push((u.to_string(), format!("prop:{k}")));
                }
                match k {
                    "Reference" => refdes = v,
                    "Value" => value = v,
                    "Footprint" => {}
                    _ => {
                        props.insert(k.to_string(), v);
                    }
                }
            }
            Some("fp_text") => {
                let it = c.items();
                let kind = it.get(1).and_then(Sexpr::atom).unwrap_or("");
                let text = it.get(2).and_then(Sexpr::atom).unwrap_or("").to_string();
                match kind {
                    "reference" => refdes = text,
                    "value" => value = text,
                    _ => notes.agg(
                        "import.footprint_text",
                        "fp_text",
                        Diagnostic::info(
                            "import.footprint_text",
                            "footprint user texts are not imported (cadlab footprints have no texts; designators are placed by the legend writer)",
                        )
                        .with_hint("add board texts on the silkscreen if they matter for manufacturing"),
                    ),
                }
            }
            _ => {}
        }
    }
    let refdes = refdes.trim().to_string();
    let subject = ObjectRef::Name(if refdes.is_empty() { lib_id.clone() } else { refdes.clone() });
    super::local_overrides(e, &subject, notes);
    if e.get("net_tie_pad_groups").is_some() {
        notes.agg(
            "import.net_tie",
            "net_tie",
            Diagnostic::warning(
                "import.net_tie",
                "net-tie footprints are imported as ordinary footprints: cadlab has no net ties, so the copper joining their nets is a short for its DRC",
            )
            .with_subject(subject.clone())
            .with_hint("keep the nets joined in the circuit, or replace the net tie with a 0 Ω part"),
        );
    }

    let attrs: BTreeSet<String> = e
        .get("attr")
        .map(|a| a.items().iter().skip(1).filter_map(Sexpr::atom).map(String::from).collect())
        .unwrap_or_default();
    let locked = yes(e, "locked");

    // Graphics.
    let mut graphics: Vec<Graphic> = Vec::new();
    let mut edges = Vec::new();
    let tf_point = |q: Point| -> Point {
        let q = if side == BoardSide::Bottom { Point::new(-q.x, q.y) } else { q };
        q.rotated(rotation) + at
    };
    for c in e.items() {
        let Some(head) = c.head() else { continue };
        let shape = matches!(head, "fp_line" | "fp_rect" | "fp_circle" | "fp_arc" | "fp_poly" | "fp_curve");
        if !shape {
            continue;
        }
        if let Some(u) = c.child_value("uuid") {
            uuids.push((u.to_string(), "fp".to_string()));
        }
        let layer = layer_of(c).unwrap_or("");
        let width = width_of(c);
        let filled = c.get("fill").and_then(Sexpr::value).is_some_and(|f| matches!(f, "solid" | "yes"));
        let geom = local_geometry(head, c, &loc, filled);
        let Some(geom) = geom else {
            notes.not_imported(
                Diagnostic::warning(
                    "import.footprint_graphic",
                    format!("a `{head}` of {subject} could not be read; not imported"),
                )
                .with_subject(subject.clone())
                .with_hint("check the footprint in KiCad's footprint editor"),
            );
            continue;
        };
        if layer == "Edge.Cuts" {
            match &geom {
                GraphicGeometry::Circle { center, radius, .. } => {
                    edges.push(EdgeItem::Circle { center: tf_point(*center), radius: *radius })
                }
                GraphicGeometry::Path { points } | GraphicGeometry::Polygon { points } => {
                    let closed = matches!(geom, GraphicGeometry::Polygon { .. });
                    let mut pts: Vec<Point> = points.iter().map(|q| tf_point(*q)).collect();
                    if closed && pts.len() > 2 {
                        pts.push(pts[0]);
                    }
                    // An arc keeps its exact form: start, mid, end.
                    if head == "fp_arc"
                        && let (Some(s), Some(m), Some(en)) =
                            (child_xy(c, "start"), child_xy(c, "mid"), child_xy(c, "end"))
                    {
                        edges.push(EdgeItem::Seg {
                            a: tf_point(loc.pt(s)),
                            b: tf_point(loc.pt(en)),
                            mid: Some(tf_point(loc.pt(m))),
                        });
                        continue;
                    }
                    for w in pts.windows(2) {
                        edges.push(EdgeItem::Seg { a: w[0], b: w[1], mid: None });
                    }
                }
            }
            continue;
        }
        match fp_layer(layer, side) {
            Some(gl) => {
                if head == "fp_arc" || head == "fp_curve" {
                    notes.agg(
                        "import.footprint_arc",
                        "arc",
                        Diagnostic::info(
                            "import.footprint_arc",
                            "footprint arcs and curves are drawn as polylines (5 µm tolerance): cadlab footprint graphics have no arcs",
                        ),
                    );
                }
                graphics.push(Graphic { layer: gl, width, geometry: geom });
            }
            None => notes.not_imported_agg(
                "import.footprint_layer",
                layer,
                Diagnostic::warning(
                    "import.footprint_layer",
                    format!("footprint drawings on `{layer}` are not imported (cadlab footprints keep silkscreen, fab and courtyard of their own side)"),
                )
                .with_subject(subject.clone())
                .with_subject(ObjectRef::Layer(layer.to_string()))
                .with_hint(if layer.ends_with(".Cu") {
                    "copper drawings in footprints are not supported: make them pads, or board tracks or zones"
                } else {
                    "drawings on other layers (or on the other side) are documentation cadlab footprints do not keep; add board graphics if they matter"
                }),
            ),
        }
    }
    // Consecutive segments of one layer and width that join become one polyline.
    graphics = merge_paths(graphics);
    let courtyard = take_courtyard(&mut graphics);

    // Pads.
    let mut pads: Vec<Pad> = Vec::new();
    let mut pad_nets = Vec::new();
    let mut paste_only: Vec<Aperture> = Vec::new();
    let mut pad_has_paste: Vec<bool> = Vec::new();
    for c in e.all("pad") {
        let Some(conv) = convert_pad(c, &loc, ctx, &subject, notes) else { continue };
        if let Some(u) = c.child_value("uuid") {
            uuids.push((u.to_string(), format!("pad:{}", conv.pad.number)));
        }
        match conv.role {
            PadRole::Copper { paste } => {
                if !conv.pad.number.is_empty() {
                    pad_nets.push(PadNet {
                        number: conv.pad.number.clone(),
                        net: conv.net,
                        function: c.child_value("pinfunction").map(String::from),
                        kind: c.child_value("pintype").and_then(pin_kind),
                    });
                }
                pads.push(conv.pad);
                pad_has_paste.push(paste);
            }
            PadRole::PasteOnly => {
                let (w, h) = conv.pad.shape.size();
                paste_only.push((conv.pad.at, conv.pad.rotation, (w, h)));
            }
        }
    }
    attach_paste(&mut pads, &pad_has_paste, paste_only, &subject, notes);

    let mount = if attrs.contains("through_hole") {
        Mount::Tht
    } else if attrs.contains("smd") || !pads.iter().any(|p| matches!(p.kind, PadKind::Tht { .. })) {
        Mount::Smd
    } else {
        Mount::Tht
    };
    let courtyard = match courtyard {
        Some(c) => c,
        None => {
            let c = pad_box(&pads);
            if !pads.is_empty() {
                notes.agg(
                    "import.courtyard_generated",
                    "courtyard",
                    Diagnostic::info(
                        "import.courtyard_generated",
                        "a footprint without a closed courtyard got one 0.25 mm around its pads",
                    )
                    .with_subject(subject.clone())
                    .with_hint(
                        "draw a closed courtyard in KiCad, or regenerate the footprint with `footprint.generate`",
                    ),
                );
            }
            c
        }
    };
    for m in e.all("model") {
        let _ = m;
        notes.agg(
            "import.footprint_model",
            "model",
            Diagnostic::info("import.footprint_model", "3D model references are not imported (3D comes in M9)"),
        );
    }
    for z in e.all("zone") {
        let _ = z;
        notes.not_imported_agg(
            "import.footprint_zone",
            &refdes,
            Diagnostic::warning("import.footprint_zone", format!("zones inside footprint {subject} are not imported"))
                .with_subject(subject.clone())
                .with_hint("add them as board zones or keep-outs (`zone.add`, `keepout.add`)"),
        );
    }

    let name = footprint_name(&lib_id, &refdes);
    let fp = Footprint {
        name,
        description: e.child_value("descr").unwrap_or("").to_string(),
        mount,
        pads,
        courtyard,
        graphics,
        body: None,
        generator: None,
        model: None,
    };
    Some(Converted {
        refdes,
        value,
        lib_id,
        placement: PlacedFootprint { at, rotation, side, locked, footprint: None },
        fp,
        props,
        attrs,
        pads: pad_nets,
        edges,
        uuids,
    })
}

/// Geometry of a footprint drawing in cadlab footprint-local coordinates.
fn local_geometry(head: &str, c: &Sexpr, loc: &Local, filled: bool) -> Option<GraphicGeometry> {
    let p = |h: &str| child_xy(c, h).map(|k| loc.pt(k));
    Some(match head {
        "fp_line" => GraphicGeometry::Path { points: vec![p("start")?, p("end")?] },
        "fp_rect" => {
            let (s, e) = (child_xy(c, "start")?, child_xy(c, "end")?);
            let pts = [s, (e.0, s.1), e, (s.0, e.1)].map(|k| loc.pt(k)).to_vec();
            GraphicGeometry::Polygon { points: pts }
        }
        "fp_circle" => {
            let (center, end) = (p("center")?, p("end")?);
            let d = end - center;
            let r = ((d.x.0 as f64).hypot(d.y.0 as f64)).round() as i64;
            GraphicGeometry::Circle { center, radius: Nm(r), filled }
        }
        "fp_arc" => GraphicGeometry::Path { points: arc_points(p("start")?, p("mid")?, p("end")?, DRAW_TOL) },
        "fp_poly" => {
            let pts: Vec<Point> = pts_of(c)?.into_iter().map(|k| loc.pt(k)).collect();
            (pts.len() >= 2).then_some(())?;
            GraphicGeometry::Polygon { points: pts }
        }
        "fp_curve" => {
            let pts: Vec<Point> = pts_of(c)?.into_iter().map(|k| loc.pt(k)).collect();
            GraphicGeometry::Path { points: bezier(&pts)? }
        }
        _ => return None,
    })
}

/// Cubic Bézier through 4 control points, as a 16-segment polyline.
pub(super) fn bezier(c: &[Point]) -> Option<Vec<Point>> {
    let [a, b, cc, d] = c else { return None };
    let f = |p: Point| (p.x.0 as f64, p.y.0 as f64);
    let (a, b, cc, d) = (f(*a), f(*b), f(*cc), f(*d));
    Some(
        (0..=16)
            .map(|i| {
                let t = i as f64 / 16.0;
                let u = 1.0 - t;
                let x = u * u * u * a.0 + 3.0 * u * u * t * b.0 + 3.0 * u * t * t * cc.0 + t * t * t * d.0;
                let y = u * u * u * a.1 + 3.0 * u * u * t * b.1 + 3.0 * u * t * t * cc.1 + t * t * t * d.1;
                Point::new(Nm(x.round() as i64), Nm(y.round() as i64))
            })
            .collect(),
    )
}

/// Joins consecutive two-point paths of one layer and width that share an end point.
fn merge_paths(gs: Vec<Graphic>) -> Vec<Graphic> {
    let mut out: Vec<Graphic> = Vec::new();
    for g in gs {
        if let (Some(last), GraphicGeometry::Path { points: new }) = (out.last_mut(), &g.geometry)
            && last.layer == g.layer
            && last.width == g.width
            && new.len() == 2
            && let GraphicGeometry::Path { points } = &mut last.geometry
            && points.last() == new.first()
            && points.first() != points.last()
        {
            points.push(new[1]);
            continue;
        }
        out.push(g);
    }
    out
}

/// Takes the courtyard outline out of the courtyard drawings (a cadlab footprint has one
/// courtyard polygon, and the drawings it replaces are removed so an export does not write the
/// courtyard twice): a single closed polygon as is, a circle as a polygon around it, paths
/// forming one closed loop; several closed shapes as their union when it is one polygon without
/// holes; else the bounding box of all courtyard drawings.
fn take_courtyard(gs: &mut Vec<Graphic>) -> Option<Vec<Point>> {
    let crt: Vec<usize> = (0..gs.len()).filter(|&i| gs[i].layer == GraphicLayer::Courtyard).collect();
    if crt.is_empty() {
        return None;
    }
    let mut taken: Vec<GraphicGeometry> = crt.iter().rev().map(|&i| gs.remove(i).geometry).collect();
    taken.reverse();
    if let [GraphicGeometry::Polygon { points }] = taken.as_slice() {
        return Some(points.clone());
    }
    // Closed rings: polygons, circles (polygons around them) and loops of paths.
    let mut rings: Vec<Vec<Point>> = Vec::new();
    let mut paths: Vec<Vec<Point>> = Vec::new();
    for g in &taken {
        match g {
            GraphicGeometry::Polygon { points } => rings.push(points.clone()),
            GraphicGeometry::Circle { center, radius, .. } => rings.push(circle_ring(*center, *radius)),
            GraphicGeometry::Path { points } => paths.push(points.clone()),
        }
    }
    let loops_ok = paths.is_empty() || chain_loop(paths.clone()).map(|r| rings.push(r)).is_some();
    if loops_ok {
        if rings.len() == 1 {
            return rings.pop();
        }
        if let Some(r) = union_ring(&rings) {
            return Some(r);
        }
    }
    // Fallback: bounding box of every courtyard drawing.
    let mut pts = Vec::new();
    for g in &taken {
        match g {
            GraphicGeometry::Path { points } | GraphicGeometry::Polygon { points } => {
                pts.extend(points.iter().copied())
            }
            GraphicGeometry::Circle { center, radius, .. } => {
                pts.push(*center - Point::new(*radius, *radius));
                pts.push(*center + Point::new(*radius, *radius));
            }
        }
    }
    let b = BBox::of_points(pts)?;
    Some(vec![b.min, Point::new(b.max.x, b.min.y), b.max, Point::new(b.min.x, b.max.y)])
}

/// Arc tolerance of custom pad outlines: 1 µm, outside (copper is never smaller than drawn).
const PAD_ARC_TOL: i64 = 1_000;

/// The outline of a KiCad custom pad in its own coordinates (KiCad's, Y down): the anchor
/// (circle of the pad width, or the `size` rectangle) united with the primitives (filled
/// polygons, lines and arcs stroked with their width, circles filled or as rings, rectangles).
/// Holes are joined to the outline by zero-width cuts. Returns the outline and the number of
/// separate parts (only the largest is kept), or `None` when nothing usable comes out.
fn custom_outline(c: &Sexpr, (w, h): K) -> Option<(Vec<K>, usize)> {
    use crate::geom::poly::{self, ArcTol, Circle, EndCap, FillRule, Join, Path, Polygon, Ring, Side};
    let tol = ArcTol::new(PAD_ARC_TOL, Side::Outside);
    let p = |q: Point| -> poly::Point { q.into() };
    let mut shapes: Vec<Polygon> = Vec::new();
    let stroke = |pts: Vec<poly::Point>, width: Nm, shapes: &mut Vec<Polygon>| {
        if width.0 > 0 && pts.len() >= 2 {
            shapes.extend(
                poly::offset_paths(&vec![Path(pts)], width.0 / 2, Join::Round, EndCap::Round, tol).unwrap_or_default(),
            );
        }
    };
    let rect = |x0: i64, y0: i64, x1: i64, y1: i64| {
        let (x0, x1, y0, y1) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
        Polygon::new(
            vec![
                poly::Point::new(x0, y0),
                poly::Point::new(x1, y0),
                poly::Point::new(x1, y1),
                poly::Point::new(x0, y1),
            ],
            vec![],
        )
    };
    let anchor_circle = c.get("options").and_then(|o| o.child_value("anchor")) == Some("circle");
    if anchor_circle {
        if w.0 > 0 {
            shapes.push(Polygon::new(Circle::new(poly::Point::new(0, 0), w.0 / 2).to_ring(tol).ok()?, vec![]));
        }
    } else if w.0 > 0 && h.0 > 0 {
        shapes.push(rect(-w.0 / 2, -h.0 / 2, w.0 - w.0 / 2, h.0 - h.0 / 2));
    }
    for g in c.get("primitives").map(|ps| ps.items().iter().skip(1).collect::<Vec<_>>()).unwrap_or_default() {
        let width = width_of(g);
        let xy = |hd: &str| child_xy(g, hd).map(|(x, y)| Point::new(x, y));
        let filled = g.get("fill").and_then(|f| f.value()).is_none_or(|v| v == "yes" || v == "solid");
        match g.head() {
            Some("gr_poly") => {
                let pts: Vec<poly::Point> =
                    pts_of(g).unwrap_or_default().into_iter().map(|(x, y)| p(Point::new(x, y))).collect();
                if pts.len() >= 3 {
                    if filled {
                        shapes.extend(poly::union_all(&pts, FillRule::NonZero).ok()?);
                    }
                    let mut closed = pts.clone();
                    closed.push(pts[0]);
                    stroke(closed, width, &mut shapes);
                }
            }
            Some("gr_line") => {
                if let (Some(a), Some(b)) = (xy("start"), xy("end")) {
                    stroke(vec![p(a), p(b)], width, &mut shapes);
                }
            }
            Some("gr_arc") => {
                if let (Some(s), Some(m), Some(en)) = (xy("start"), xy("mid"), xy("end")) {
                    stroke(arc_points(s, m, en, PAD_ARC_TOL).into_iter().map(p).collect(), width, &mut shapes);
                }
            }
            Some("gr_circle") => {
                if let (Some(ce), Some(en)) = (xy("center"), xy("end")) {
                    let d = en - ce;
                    let r = ((d.x.0 as f64).hypot(d.y.0 as f64)).round() as i64;
                    let outer = Circle::new(p(ce), r + width.0 / 2).to_ring(tol).ok()?;
                    if filled || 2 * r <= width.0 {
                        shapes.push(Polygon::new(outer, vec![]));
                    } else {
                        let inner =
                            Circle::new(p(ce), r - width.0 / 2).to_ring(ArcTol::new(PAD_ARC_TOL, Side::Inside)).ok()?;
                        let mut hole = inner;
                        hole.reverse_orientation();
                        shapes.push(Polygon::new(outer, vec![hole]));
                    }
                }
            }
            Some("gr_rect") => {
                if let (Some(a), Some(b)) = (xy("start"), xy("end")) {
                    if filled {
                        shapes.push(rect(a.x.0, a.y.0, b.x.0, b.y.0));
                    }
                    let corners = vec![p(a), p(Point::new(b.x, a.y)), p(b), p(Point::new(a.x, b.y)), p(a)];
                    stroke(corners, width, &mut shapes);
                }
            }
            _ => {}
        }
    }
    let set = poly::union_all(&shapes, FillRule::NonZero).ok()?;
    let parts = set.len();
    let largest = set.into_iter().max_by_key(|pg| pg.outer.signed_area2())?;
    let ring: Ring = poly::fracture(&largest).ok()?;
    (ring.0.len() >= 3).then(|| (ring.0.iter().map(|q| (Nm(q.x), Nm(q.y))).collect(), parts))
}

/// A polygon around a circle (within 5 µm outside it).
fn circle_ring(center: Point, radius: Nm) -> Vec<Point> {
    use crate::geom::poly::{ArcTol, Circle, Side};
    match Circle::new(center.into(), radius.0).to_ring(ArcTol::new(5_000, Side::Outside)) {
        Ok(r) => r.0.iter().map(|&q| q.into()).collect(),
        Err(_) => {
            let r = radius;
            vec![
                center - Point::new(r, r),
                center + Point::new(r, -r),
                center + Point::new(r, r),
                center + Point::new(-r, r),
            ]
        }
    }
}

/// The union of closed rings when it is one polygon without holes.
fn union_ring(rings: &[Vec<Point>]) -> Option<Vec<Point>> {
    use crate::geom::poly::{self, FillRule, Ring};
    let rs: Vec<Ring> =
        rings.iter().map(|r| Ring::from(r.iter().map(|&q| q.into()).collect::<Vec<poly::Point>>())).collect();
    let set = poly::union_all(&rs, FillRule::NonZero).ok()?;
    match set.as_slice() {
        [pg] if pg.holes.is_empty() => Some(pg.outer.0.iter().map(|&q| q.into()).collect()),
        _ => None,
    }
}

/// Polylines forming exactly one closed loop → its vertices (without repeating the first).
fn chain_loop(mut paths: Vec<Vec<Point>>) -> Option<Vec<Point>> {
    let mut ring = paths.remove(0);
    while !paths.is_empty() {
        let end = *ring.last()?;
        let i = paths.iter().position(|p| p.first() == Some(&end) || p.last() == Some(&end))?;
        let mut p = paths.remove(i);
        if p.first() != Some(&end) {
            p.reverse();
        }
        ring.extend(p.into_iter().skip(1));
    }
    (ring.len() >= 4 && ring.first() == ring.last()).then(|| {
        ring.pop();
        ring
    })
}

/// A box 0.25 mm around the pads (courtyard of footprints without one).
fn pad_box(pads: &[Pad]) -> Vec<Point> {
    let mut pts = Vec::new();
    for p in pads {
        let (w, h) = p.shape.size();
        let r = Nm(w.0.max(h.0) / 2);
        pts.push(p.at - Point::new(r, r));
        pts.push(p.at + Point::new(r, r));
    }
    let Some(b) = BBox::of_points(pts) else { return Vec::new() };
    let m = Nm::from_um(250);
    let (lo, hi) = (b.min - Point::new(m, m), b.max + Point::new(m, m));
    vec![lo, Point::new(hi.x, lo.y), hi, Point::new(lo.x, hi.y)]
}

enum PadRole {
    /// A copper pad; `paste` when it has a paste opening of its own.
    Copper { paste: bool },
    /// A paste aperture without copper (exposed pad paste windows).
    PasteOnly,
}

struct PadConv {
    pad: Pad,
    role: PadRole,
    net: Option<String>,
}

fn convert_pad(c: &Sexpr, loc: &Local, ctx: &Ctx, subject: &ObjectRef, notes: &mut Notes) -> Option<PadConv> {
    let it = c.items();
    let number = it.get(1).and_then(Sexpr::atom).unwrap_or("").to_string();
    let kind = it.get(2).and_then(Sexpr::atom).unwrap_or("");
    let shape = it.get(3).and_then(Sexpr::atom).unwrap_or("");
    let label = if number.is_empty() { format!("{subject}") } else { format!("{subject}.{number}") };
    let pad_subject = ObjectRef::Name(label.clone());
    let skip = |why: &str, notes: &mut Notes| {
        notes.not_imported(
            Diagnostic::warning("import.pad_unsupported", format!("pad {label}: {why}; not imported"))
                .with_subject(pad_subject.clone())
                .with_hint("edit the footprint in KiCad (or replace it with a generated one: `footprint.generate`)"),
        );
    };
    let (Some((pos, a)), Some(size)) = (at_of(c), child_xy(c, "size")) else {
        skip("no position or size", notes);
        return None;
    };
    super::local_overrides(c, &pad_subject, notes);
    let mut at = loc.pt(pos);
    let rotation = loc.pad_rot(a);
    let (w, h) = size;
    let approx = |what: String, notes: &mut Notes| {
        notes.agg(
            "import.pad_approximated",
            &format!("{label}/{what}"),
            Diagnostic::warning("import.pad_approximated", format!("pad {label}: {what}"))
                .with_subject(pad_subject.clone())
                .with_hint("check clearances around it, or replace the footprint with one cadlab generates (`footprint.generate`)"),
        );
    };
    if let Some(ps) = c.get("padstack")
        && ps.child_value("mode").is_some_and(|m| m != "normal")
    {
        approx("its per-layer pad stack is reduced to the front layer's shape".into(), notes);
    }
    let mut pshape = match shape {
        "rect" => PadShape::Rect { w, h },
        "circle" => PadShape::Circle { d: w },
        "oval" => PadShape::Oval { w, h },
        "roundrect" => {
            let ratio: f64 = c.child_value("roundrect_rratio").and_then(|r| r.parse().ok()).unwrap_or(0.25);
            let r = (ratio.clamp(0.0, 0.5) * w.0.min(h.0) as f64).round() as i64;
            if c.get("chamfer").is_some_and(|ch| ch.items().len() > 1) {
                approx("chamfered corners are drawn rounded".into(), notes);
            }
            PadShape::RoundRect { w, h, r: Nm(r) }
        }
        "trapezoid" => {
            let d = child_xy(c, "rect_delta").unwrap_or((Nm::ZERO, Nm::ZERO));
            approx("a trapezoid is drawn as its bounding rectangle".into(), notes);
            PadShape::Rect { w: w + d.1.abs(), h: h + d.0.abs() }
        }
        "custom" if custom_outline(c, (w, h)).is_some() => {
            let (ring, parts) = custom_outline(c, (w, h)).expect("checked");
            if parts > 1 {
                approx(format!("a custom shape in {parts} separate parts keeps only its largest part"), notes);
            }
            PadShape::Polygon { points: ring.into_iter().map(|k| loc.pad_offset(k)).collect() }
        }
        "custom" => {
            // Bounding box of the anchor and the primitives, in KiCad pad coordinates.
            let anchor_circle = c.get("options").and_then(|o| o.child_value("anchor")) == Some("circle");
            let (hw, hh) = if anchor_circle { (Nm(w.0 / 2), Nm(w.0 / 2)) } else { (Nm(w.0 / 2), Nm(h.0 / 2)) };
            let mut bb = BBox::new(Point::new(-hw, -hh), Point::new(hw, hh));
            if let Some(prims) = c.get("primitives") {
                for g in prims.items().iter().skip(1) {
                    let half = Nm(width_of(g).0 / 2);
                    let mut pts: Vec<Point> = Vec::new();
                    let xy = |h: &str| child_xy(g, h).map(|(x, y)| Point::new(x, y));
                    match g.head() {
                        Some("gr_poly") => {
                            pts.extend(pts_of(g).unwrap_or_default().into_iter().map(|(x, y)| Point::new(x, y)))
                        }
                        Some("gr_line" | "gr_rect") => pts.extend([xy("start"), xy("end")].into_iter().flatten()),
                        Some("gr_arc") => {
                            if let (Some(s), Some(m), Some(en)) = (xy("start"), xy("mid"), xy("end")) {
                                pts.extend(arc_points(s, m, en, DRAW_TOL));
                            }
                        }
                        Some("gr_circle") => {
                            if let (Some(ce), Some(en)) = (xy("center"), xy("end")) {
                                let d = en - ce;
                                let r = Nm(((d.x.0 as f64).hypot(d.y.0 as f64)).round() as i64);
                                pts.push(ce - Point::new(r, r));
                                pts.push(ce + Point::new(r, r));
                            }
                        }
                        _ => {}
                    }
                    for q in pts {
                        bb.add_point(q - Point::new(half, half));
                        bb.add_point(q + Point::new(half, half));
                    }
                }
            }
            let center = Point::new(Nm((bb.min.x.0 + bb.max.x.0) / 2), Nm((bb.min.y.0 + bb.max.y.0) / 2));
            at = at + loc.pad_offset((center.x, center.y)).rotated(rotation);
            approx("a custom shape is drawn as its bounding rectangle".into(), notes);
            PadShape::Rect { w: bb.width(), h: bb.height() }
        }
        other => {
            skip(&format!("unknown shape `{other}`"), notes);
            return None;
        }
    };
    let layers: Vec<String> = c
        .get("layers")
        .map(|l| l.items().iter().skip(1).filter_map(Sexpr::atom).flat_map(|x| ctx.expand_layer(x)).collect())
        .unwrap_or_default();
    let has = |l: &str| layers.iter().any(|x| x == l);
    let (front, back) = match loc.side {
        BoardSide::Top => ("F.", "B."),
        BoardSide::Bottom => ("B.", "F."),
    };
    let drill = c.get("drill").and_then(|d| {
        let it: Vec<&str> = d.items().iter().skip(1).filter_map(Sexpr::atom).collect();
        let (oval, nums) = match it.first() {
            Some(&"oval") => (true, &it[1..]),
            _ => (false, &it[..]),
        };
        let a = mm(nums.first()?)?;
        let b = nums.get(1).and_then(|s| mm(s)).unwrap_or(a);
        if child_xy(d, "offset").is_some_and(|(x, y)| x.0 != 0 || y.0 != 0) {
            approx("its drill offset is ignored (the hole is at the pad center)".into(), notes);
        }
        if oval && a != b {
            approx(
                format!("an oval {}×{} slot is drilled as a round {} hole", Nm(a.0), Nm(b.0), Nm(a.0.min(b.0))),
                notes,
            );
        }
        Some(Nm(a.0.min(b.0)))
    });
    let net = ctx.raw_net(c);
    let (pkind, role) = match kind {
        "thru_hole" => {
            let Some(d) = drill else {
                skip("a through-hole pad without a drill", notes);
                return None;
            };
            if has(&format!("{front}Paste")) || has(&format!("{back}Paste")) {
                notes.agg(
                    "import.tht_paste",
                    "paste",
                    Diagnostic::warning(
                        "import.tht_paste",
                        "through-hole pads with solder paste (paste-in-hole) get no paste: cadlab puts paste on SMD pads only",
                    )
                    .with_subject(pad_subject.clone())
                    .with_hint("add the paste in the paste layer after export if the assembly reflows these parts"),
                );
            }
            (PadKind::Tht { drill: d }, PadRole::Copper { paste: has(&format!("{front}Paste")) })
        }
        "np_thru_hole" => {
            let Some(d) = drill else {
                skip("a hole without a drill", notes);
                return None;
            };
            if !matches!(pshape, PadShape::Circle { .. } | PadShape::Oval { .. }) || w != h {
                pshape = PadShape::Circle { d };
            }
            (PadKind::Npth { drill: d }, PadRole::Copper { paste: false })
        }
        "smd" | "connect" => {
            let cu = has(&format!("{front}Cu"));
            let paste = has(&format!("{front}Paste")) && kind == "smd";
            if !cu && has(&format!("{back}Cu")) {
                skip(
                    "copper on the other side than the footprint (cadlab SMD pads are on the footprint's side)",
                    notes,
                );
                return None;
            }
            if !cu {
                if paste {
                    return Some(PadConv {
                        pad: Pad { number, at, rotation, shape: pshape, kind: PadKind::Smd, paste: None },
                        role: PadRole::PasteOnly,
                        net: None,
                    });
                }
                skip("no copper (mask or paste aperture only)", notes);
                return None;
            }
            (PadKind::Smd, PadRole::Copper { paste })
        }
        other => {
            skip(&format!("unknown pad type `{other}`"), notes);
            return None;
        }
    };
    let paste = match (&pkind, &role) {
        (PadKind::Smd | PadKind::Tht { .. }, PadRole::Copper { paste: false }) => Some(Paste::None),
        _ => None,
    };
    Some(PadConv { pad: Pad { number, at, rotation, shape: pshape, kind: pkind, paste }, role, net })
}

/// A paste-only aperture: center, rotation and size, in footprint coordinates.
type Aperture = (Point, Angle, (Nm, Nm));

/// Turns paste-only apertures into paste windows of the copper pad they lie on.
fn attach_paste(pads: &mut [Pad], has_paste: &[bool], windows: Vec<Aperture>, subject: &ObjectRef, notes: &mut Notes) {
    let mut per_pad: BTreeMap<usize, Vec<Aperture>> = BTreeMap::new();
    let mut lost = 0;
    for w in windows {
        // The SMD pad without its own paste whose box (in its frame) holds the aperture center.
        let host = pads.iter().enumerate().position(|(i, p)| {
            if !matches!(p.kind, PadKind::Smd) || has_paste[i] {
                return false;
            }
            let d = (w.0 - p.at).rotated(-p.rotation);
            let (pw, ph) = p.shape.size();
            d.x.0.abs() <= pw.0 / 2 && d.y.0.abs() <= ph.0 / 2
        });
        match host {
            Some(i) => per_pad.entry(i).or_default().push(w),
            None => lost += 1,
        }
    }
    for (i, ws) in per_pad {
        let pad = &mut pads[i];
        // Window sizes in the pad's frame.
        let sizes: Vec<(Nm, Nm)> = ws
            .iter()
            .map(|(_, r, (w, h))| match (*r - pad.rotation).normalized().quarter_turns() {
                Some(1 | 3) => (*h, *w),
                _ => (*w, *h),
            })
            .collect();
        let aligned = ws.iter().all(|(_, r, _)| (*r - pad.rotation).normalized().quarter_turns().is_some());
        if !aligned || sizes.iter().any(|s| *s != sizes[0]) {
            lost += ws.len();
            continue;
        }
        let at = ws.iter().map(|(c, _, _)| (*c - pad.at).rotated(-pad.rotation)).collect();
        pad.paste = Some(Paste::Windows { size: sizes[0], at });
    }
    if lost > 0 {
        notes.not_imported(
            Diagnostic::warning(
                "import.paste_aperture",
                format!("{lost} paste aperture(s) of {subject} are not on a pad or not a grid of equal windows; not imported"),
            )
            .with_subject(subject.clone())
            .with_hint("the pads they belong to get no paste: check the stencil, or regenerate the footprint (`footprint.generate`)"),
        );
    }
}

/// Whether two footprints are the same land pattern, allowing the few nanometers of rounding a
/// rotated placement leaves in exported local coordinates (names and descriptions ignored).
pub(super) fn equivalent(a: &Footprint, b: &Footprint) -> bool {
    // Order-independent matching: KiCad may reorder pads and drawings when it saves a board.
    fn matched<T>(a: &[T], b: &[T], eq: impl Fn(&T, &T) -> bool) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut used = vec![false; b.len()];
        a.iter().all(|x| match (0..b.len()).find(|&j| !used[j] && eq(x, &b[j])) {
            Some(j) => {
                used[j] = true;
                true
            }
            None => false,
        })
    }
    const TOL: i64 = 3;
    let close = |p: Point, q: Point| (p.x.0 - q.x.0).abs() <= TOL && (p.y.0 - q.y.0).abs() <= TOL;
    let close_pts = |p: &[Point], q: &[Point]| p.len() == q.len() && p.iter().zip(q).all(|(x, y)| close(*x, *y));
    let ang = |x: Angle, y: Angle| {
        let d = (x - y).normalized().0;
        d <= 2 || d >= 360_000 - 2
    };
    let shape_eq = |x: &PadShape, y: &PadShape| match (x, y) {
        (PadShape::RoundRect { w, h, r }, PadShape::RoundRect { w: w2, h: h2, r: r2 }) => {
            w == w2 && h == h2 && (r.0 - r2.0).abs() <= 10
        }
        (PadShape::Polygon { points: p }, PadShape::Polygon { points: q }) => close_pts(p, q),
        _ => x == y,
    };
    let paste_eq = |x: &Option<Paste>, y: &Option<Paste>| match (x, y) {
        (Some(Paste::Windows { size: s1, at: a1 }), Some(Paste::Windows { size: s2, at: a2 })) => {
            s1 == s2 && matched(a1, a2, |x, y| close(*x, *y))
        }
        _ => x == y,
    };
    let pad_eq = |x: &Pad, y: &Pad| {
        x.number == y.number
            && x.kind == y.kind
            && shape_eq(&x.shape, &y.shape)
            && close(x.at, y.at)
            && ang(x.rotation, y.rotation)
            && (matches!(x.kind, PadKind::Npth { .. }) || paste_eq(&x.paste, &y.paste))
    };
    let graphic_eq = |x: &Graphic, y: &Graphic| {
        x.layer == y.layer
            && x.width == y.width
            && match (&x.geometry, &y.geometry) {
                (GraphicGeometry::Path { points: p }, GraphicGeometry::Path { points: q }) => {
                    // Segments, either direction.
                    close_pts(p, q) || (p.len() == 2 && q.len() == 2 && close(p[0], q[1]) && close(p[1], q[0]))
                }
                (GraphicGeometry::Polygon { points: p }, GraphicGeometry::Polygon { points: q }) => close_pts(p, q),
                (
                    GraphicGeometry::Circle { center: c1, radius: r1, filled: f1 },
                    GraphicGeometry::Circle { center: c2, radius: r2, filled: f2 },
                ) => close(*c1, *c2) && (r1.0 - r2.0).abs() <= TOL && f1 == f2,
                _ => false,
            }
    };
    // Polylines are compared segment by segment: KiCad splits, reorders and reverses lines.
    let split = |gs: &[Graphic]| -> Vec<Graphic> {
        gs.iter()
            .flat_map(|g| match &g.geometry {
                GraphicGeometry::Path { points } if points.len() > 2 => points
                    .windows(2)
                    .map(|w| Graphic {
                        layer: g.layer,
                        width: g.width,
                        geometry: GraphicGeometry::Path { points: w.to_vec() },
                    })
                    .collect(),
                _ => vec![g.clone()],
            })
            .collect()
    };
    a.mount == b.mount
        && close_pts(&a.courtyard, &b.courtyard)
        && matched(&a.pads, &b.pads, pad_eq)
        && matched(&split(&a.graphics), &split(&b.graphics), graphic_eq)
}

/// Absolute geometry of a footprint drawing on the board: the drawing in board coordinates.
pub(super) fn to_board(pf: &PlacedFootprint, g: &GraphicGeometry) -> Vec<Point> {
    let tf = crate::board::transform(pf);
    match g {
        GraphicGeometry::Path { points } => points.iter().map(|q| tf(*q)).collect(),
        GraphicGeometry::Polygon { points } => {
            let mut v: Vec<Point> = points.iter().map(|q| tf(*q)).collect();
            if let Some(f) = v.first().copied() {
                v.push(f);
            }
            v
        }
        GraphicGeometry::Circle { center, radius, .. } => {
            let c = *center;
            let r = *radius;
            let (e, w) = (c + Point::new(r, Nm::ZERO), c - Point::new(r, Nm::ZERO));
            let mut v = arc_points(e, c + Point::new(Nm::ZERO, r), w, DRAW_TOL);
            v.extend(arc_points(w, c - Point::new(Nm::ZERO, r), e, DRAW_TOL).into_iter().skip(1));
            v.into_iter().map(tf).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(footprint_name("Resistor_SMD:R_0402_1005Metric", "R1"), "R_0402_1005Metric");
        assert_eq!(footprint_name("lib:My Part (v2)", "U1"), "My_Part_v2");
        assert_eq!(footprint_name("", "U1"), "fp_U1");
    }

    #[test]
    fn chains_loops() {
        let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        let ring = chain_loop(vec![
            vec![p(0, 0), p(10, 0)],
            vec![p(10, 10), p(10, 0)],
            vec![p(10, 10), p(0, 10)],
            vec![p(0, 10), p(0, 0)],
        ])
        .unwrap();
        assert_eq!(ring, vec![p(0, 0), p(10, 0), p(10, 10), p(0, 10)]);
        assert!(chain_loop(vec![vec![p(0, 0), p(10, 0)], vec![p(10, 0), p(10, 10)]]).is_none());
    }
}
