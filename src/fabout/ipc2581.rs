//! IPC-2581 revision C ("Generic Requirements for Printed Board Assembly Products Manufacturing
//! Description Data and Transfer Methodology") XML, namespace `http://webstds.ipc.org/2581`.
//!
//! One file describes the whole product (function mode `ASSEMBLY`):
//!
//! - `Content`: the step, every layer, the BOM, and the dictionaries: standard primitives
//!   (`Circle`, `RectCenter`, `RectRound`, `Oval`; ids name the shape and size, such as
//!   `RECT_1X0.6`) and line descriptions (`LINE_0.15`, round ends).
//! - `LogisticHeader`: a sender role, enterprise and person (placeholders: cadlab stores no
//!   people or companies).
//! - `Bom`: one item per BOM line (part ID as the OEM design number), designators with
//!   `populate="false"` for DNP, value, manufacturer and MPN as textual characteristics.
//! - `Ecad`: `CadHeader` (millimeters), layers (legend, paste, mask, copper and dielectrics in
//!   stack order, then one drill layer per copper span), the stackup (copper and dielectric
//!   thicknesses; dielectrics share what copper leaves of the board thickness), and the step:
//!   profile (outline polygon with arcs, cutouts), packages (outline from the courtyard, land
//!   pattern, body outline as the assembly drawing, pins), components (placement: mirror about
//!   Y for the bottom side, then counter-clockwise rotation), logical nets (pins by pad
//!   number), and layer features: copper pads, vias, tracks (lines and arcs) and zone fills
//!   (contours with cutouts) grouped by net; mask openings (grown by the mask expansion; vias
//!   tented), paste openings (exposed-pad windows), legend strokes (footprint silk, reference
//!   designators, board graphics; not clipped at mask openings, unlike the Gerber legend), and
//!   holes with their plating (`PLATED`, `NONPLATED`, `VIA`).
//!
//! No `HistoryRecord` or `Avl` (both carry dates; output stays deterministic). Element order
//! follows the revision C schema as published by IPC; the schema itself is not shipped.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use polyclip::Ring;

use super::gerber::mm;
use super::layers::{Stroke, text_strokes};
use super::{FileKind, Hole, HoleKind, Options, OutFile, deg, file_name, fp_rotation, holes, pad_rotation};
use crate::board::{self, PlacedPad, footprint_for, side_layer, transform, via_layers};
use crate::geom::Point;
use crate::mcad::{Edge, Loop, arc_edge, board_profile};
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind};
use crate::model::footprint::{Footprint, GraphicGeometry, GraphicLayer, Mount, PadKind, PadShape, Paste};
use crate::units::{Angle, Nm};

/// Namespace of IPC-2581.
pub const NAMESPACE: &str = "http://webstds.ipc.org/2581";

/// Escapes text for an XML attribute value.
pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "&#{};", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

type Attrs<'a> = &'a [(&'a str, String)];

/// Indented XML writer.
struct Xml {
    out: String,
    depth: usize,
}

impl Xml {
    fn new(depth: usize) -> Xml {
        Xml { out: String::new(), depth }
    }

    fn tag(&mut self, name: &str, attrs: Attrs<'_>, close: bool) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        self.out.push('<');
        self.out.push_str(name);
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", esc(v));
        }
        self.out.push_str(if close { "/>\n" } else { ">\n" });
    }

    fn open(&mut self, name: &str, attrs: Attrs<'_>) {
        self.tag(name, attrs, false);
        self.depth += 1;
    }

    fn empty(&mut self, name: &str, attrs: Attrs<'_>) {
        self.tag(name, attrs, true);
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        let _ = writeln!(self.out, "</{name}>");
    }
}

fn a(k: &'static str, v: impl Into<String>) -> (&'static str, String) {
    (k, v.into())
}

fn xy(p: Point) -> [(&'static str, String); 2] {
    [a("x", mm(p.x.0)), a("y", mm(p.y.0))]
}

/// A primitive element: name and attributes.
/// A dictionary primitive: element name and attributes, or (for a polygon pad) the outline of a
/// `Contour` (element name empty).
type Primitive = (&'static str, Vec<(&'static str, String)>, Vec<Point>);

/// Dictionaries collected while writing features.
#[derive(Default)]
struct Dict {
    /// id → primitive element (name, attributes).
    standard: BTreeMap<String, Primitive>,
    /// Line widths (nm) by id.
    lines: BTreeMap<String, i64>,
}

impl Dict {
    /// The standard primitive of a pad shape, or `None` for an empty shape.
    fn prim(&mut self, shape: PadShape) -> Option<String> {
        let (id, el, attrs) = match shape {
            PadShape::Polygon { points } => {
                if points.len() < 3 {
                    return None;
                }
                // Identical outlines share one entry; ids follow first use.
                let id = match self.standard.iter().find(|(_, (_, _, c))| *c == points) {
                    Some((id, _)) => id.clone(),
                    None => {
                        let n = self.standard.values().filter(|(_, _, c)| !c.is_empty()).count();
                        let id = format!("CONTOUR_{}", n + 1);
                        self.standard.insert(id.clone(), ("", Vec::new(), points));
                        id
                    }
                };
                return Some(id);
            }
            PadShape::Circle { d } => {
                if d.0 <= 0 {
                    return None;
                }
                (format!("CIRCLE_{}", mm(d.0)), "Circle", vec![a("diameter", mm(d.0))])
            }
            PadShape::Rect { w, h } => {
                if w.0 <= 0 || h.0 <= 0 {
                    return None;
                }
                (format!("RECT_{}X{}", mm(w.0), mm(h.0)), "RectCenter", vec![a("width", mm(w.0)), a("height", mm(h.0))])
            }
            PadShape::Oval { w, h } => {
                if w == h {
                    return self.prim(PadShape::Circle { d: w });
                }
                if w.0 <= 0 || h.0 <= 0 {
                    return None;
                }
                (format!("OVAL_{}X{}", mm(w.0), mm(h.0)), "Oval", vec![a("width", mm(w.0)), a("height", mm(h.0))])
            }
            PadShape::RoundRect { w, h, r } => {
                let r = r.0.clamp(0, w.0.min(h.0) / 2);
                if r == 0 {
                    return self.prim(PadShape::Rect { w, h });
                }
                if 2 * r >= w.0.min(h.0) {
                    return self.prim(PadShape::Oval { w, h });
                }
                (
                    format!("RRECT_{}X{}R{}", mm(w.0), mm(h.0), mm(r)),
                    "RectRound",
                    vec![
                        a("width", mm(w.0)),
                        a("height", mm(h.0)),
                        a("radius", mm(r)),
                        a("upperRight", "true"),
                        a("upperLeft", "true"),
                        a("lowerRight", "true"),
                        a("lowerLeft", "true"),
                    ],
                )
            }
        };
        self.standard.entry(id.clone()).or_insert((el, attrs, Vec::new()));
        Some(id)
    }

    fn line(&mut self, width: i64) -> String {
        let id = format!("LINE_{}", mm(width));
        self.lines.insert(id.clone(), width);
        id
    }
}

/// Layer names and functions, in stack order.
struct Layers {
    /// (name, layerFunction, side).
    list: Vec<(String, &'static str, &'static str)>,
    /// Copper names, top to bottom.
    copper: Vec<String>,
    /// Drill layers: (from, to) 1-based → name.
    drills: Vec<((usize, usize), String)>,
}

fn layers(p: &Project, hs: &[Hole]) -> Layers {
    let copper = p.board().stackup.copper_names();
    let n = copper.len();
    let mut list = vec![
        ("F.SilkS".to_string(), "SILKSCREEN", "TOP"),
        ("F.Paste".to_string(), "SOLDERPASTE", "TOP"),
        ("F.Mask".to_string(), "SOLDERMASK", "TOP"),
    ];
    for (i, c) in copper.iter().enumerate() {
        let side = if i == 0 {
            "TOP"
        } else if i == n - 1 {
            "BOTTOM"
        } else {
            "INTERNAL"
        };
        list.push((c.clone(), "CONDUCTOR", side));
        if i + 1 < n.max(2) {
            list.push((format!("Dielectric{}", i + 1), dielectric_function(i, n), "INTERNAL"));
        }
    }
    list.extend([
        ("B.Mask".to_string(), "SOLDERMASK", "BOTTOM"),
        ("B.Paste".to_string(), "SOLDERPASTE", "BOTTOM"),
        ("B.SilkS".to_string(), "SILKSCREEN", "BOTTOM"),
    ]);
    let mut drills: Vec<((usize, usize), String)> = Vec::new();
    for h in hs {
        if !drills.iter().any(|(s, _)| *s == h.span) {
            drills.push((h.span, format!("DRILL_{}_{}", h.span.0, h.span.1)));
        }
    }
    drills.sort();
    for (_, name) in &drills {
        list.push((name.clone(), "DRILL", "ALL"));
    }
    Layers { list, copper, drills }
}

/// Dielectric `i` (0-based, from the top) of `n` copper layers: a core on two-layer boards,
/// else prepreg on the outside alternating with cores.
fn dielectric_function(i: usize, n: usize) -> &'static str {
    if n <= 2 || i % 2 == 1 { "DIELCORE" } else { "DIELPREG" }
}

/// Polygon elements of a loop (full circles as two half arcs).
fn polygon(x: &mut Xml, tag: &str, lp: &Loop) {
    let lp = lp.split_circles();
    x.open(tag, &[]);
    x.empty("PolyBegin", &xy(lp.start));
    for e in &lp.edges {
        match *e {
            Edge::Line { to } => x.empty("PolyStepSegment", &xy(to)),
            Edge::Arc { to, center, ccw } => {
                let [px, py] = xy(to);
                x.empty(
                    "PolyStepCurve",
                    &[
                        px,
                        py,
                        a("centerX", mm(center.0.round() as i64)),
                        a("centerY", mm(center.1.round() as i64)),
                        a("clockwise", if ccw { "false" } else { "true" }),
                    ],
                );
            }
        }
    }
    x.close(tag);
}

/// A polygon from a ring (closed by repeating the first point).
fn ring_polygon(x: &mut Xml, tag: &str, ring: &Ring) {
    let pts = &ring.0;
    let Some(first) = pts.first() else { return };
    x.open(tag, &[]);
    x.empty("PolyBegin", &[a("x", mm(first.x)), a("y", mm(first.y))]);
    for q in pts.iter().skip(1).chain(std::iter::once(first)) {
        x.empty("PolyStepSegment", &[a("x", mm(q.x)), a("y", mm(q.y))]);
    }
    x.close(tag);
}

fn points_polygon(x: &mut Xml, tag: &str, pts: &[Point]) {
    let ring: Ring = pts.iter().map(|&q| polyclip::Point::from(q)).collect::<Vec<_>>().into();
    ring_polygon(x, tag, &ring);
}

/// A pad element (board or package coordinates).
fn pad(x: &mut Xml, d: &mut Dict, at: Point, shape: PadShape, rotation: Angle, pin: Option<(Option<&str>, &str)>) {
    let Some(id) = d.prim(shape) else { return };
    x.open("Pad", &[]);
    let r = rotation.normalized();
    if r != Angle::ZERO {
        x.empty("Xform", &[a("rotation", deg(r))]);
    }
    x.empty("Location", &xy(at));
    x.empty("StandardPrimitiveRef", &[a("id", id)]);
    if let Some((comp, number)) = pin {
        match comp {
            Some(c) => x.empty("PinRef", &[a("componentRef", c), a("pin", number)]),
            None => x.empty("PinRef", &[a("pin", number)]),
        }
    }
    x.close("Pad");
}

/// Line or polyline feature for legend strokes and tracks.
fn stroke(x: &mut Xml, d: &mut Dict, st: &Stroke) {
    match st {
        Stroke::Line { pts, width } => {
            let id = d.line(*width);
            if pts.len() == 2 {
                x.open(
                    "Line",
                    &[
                        a("startX", mm(pts[0].0)),
                        a("startY", mm(pts[0].1)),
                        a("endX", mm(pts[1].0)),
                        a("endY", mm(pts[1].1)),
                    ],
                );
                x.empty("LineDescRef", &[a("id", id)]);
                x.close("Line");
            } else if pts.len() > 2 {
                x.open("Polyline", &[]);
                x.empty("PolyBegin", &[a("x", mm(pts[0].0)), a("y", mm(pts[0].1))]);
                for q in &pts[1..] {
                    x.empty("PolyStepSegment", &[a("x", mm(q.0)), a("y", mm(q.1))]);
                }
                x.empty("LineDescRef", &[a("id", id)]);
                x.close("Polyline");
            }
        }
        Stroke::Circle { c, r, width, filled } => {
            let center = Point::new(Nm(c.0), Nm(c.1));
            if *filled {
                let lp = Loop::circle(center, Nm(2 * r + width), true);
                x.open("Contour", &[]);
                polygon(x, "Polygon", &lp);
                x.close("Contour");
            } else {
                let id = d.line(*width);
                for (s, e) in [((c.0 + r, c.1), (c.0 - r, c.1)), ((c.0 - r, c.1), (c.0 + r, c.1))] {
                    x.open(
                        "Arc",
                        &[
                            a("startX", mm(s.0)),
                            a("startY", mm(s.1)),
                            a("endX", mm(e.0)),
                            a("endY", mm(e.1)),
                            a("centerX", mm(c.0)),
                            a("centerY", mm(c.1)),
                            a("clockwise", "false"),
                        ],
                    );
                    x.empty("LineDescRef", &[a("id", id.clone())]);
                    x.close("Arc");
                }
            }
        }
    }
}

/// `<Set>` with the given attributes holding one `Features/UserSpecial` group of strokes.
fn stroke_set(x: &mut Xml, d: &mut Dict, attrs: Attrs<'_>, strokes: &[Stroke]) {
    if strokes.is_empty() {
        return;
    }
    x.open("Set", attrs);
    x.open("Features", &[]);
    x.open("UserSpecial", &[]);
    for st in strokes {
        stroke(x, d, st);
    }
    x.close("UserSpecial");
    x.close("Features");
    x.close("Set");
}

struct Ctx<'a> {
    p: &'a Project,
    o: &'a Options,
    pads: Vec<PlacedPad>,
}

impl Ctx<'_> {
    fn rotation(&self, pp: &PlacedPad) -> Angle {
        pad_rotation(pp, fp_rotation(self.p, &pp.refdes))
    }

    /// The pad's shape and rotation on the board ([`super::oriented`]).
    fn oriented(&self, pp: &PlacedPad) -> (PadShape, Angle) {
        super::oriented(pp, fp_rotation(self.p, &pp.refdes))
    }

    fn is_component(&self, pp: &PlacedPad) -> bool {
        !board::holes::is_hole(self.p, &pp.refdes)
    }

    fn pin<'b>(&self, pp: &'b PlacedPad) -> Option<(Option<&'b str>, &'b str)> {
        (self.is_component(pp) && !pp.number.is_empty()).then_some((Some(pp.refdes.as_str()), pp.number.as_str()))
    }
}

/// Net key sorting named nets first (by name), unconnected last.
fn net_key(n: &Option<String>) -> (bool, String) {
    (n.is_none(), n.clone().unwrap_or_default())
}

fn net_attrs(n: &Option<String>) -> Vec<(&'static str, String)> {
    n.iter().map(|n| a("net", n.clone())).collect()
}

fn copper_feature(c: &Ctx<'_>, x: &mut Xml, d: &mut Dict, layer: &str, items: &[board::CopperItem]) {
    let b = c.p.board();
    let mut nets: BTreeMap<(bool, String), Option<String>> = BTreeMap::new();
    let pads: Vec<&PlacedPad> = c.pads.iter().filter(|pp| pp.layers.iter().any(|l| l == layer)).collect();
    let vias: Vec<_> = b.vias.iter().filter(|v| via_layers(c.p, v).iter().any(|l| l == layer)).collect();
    let tracks: Vec<_> = b.tracks.iter().filter(|t| t.layer == layer).collect();
    let fills: Vec<&board::CopperItem> = items
        .iter()
        .filter(|it| !matches!(it.item, board::ItemRef::Pad(..) | board::ItemRef::Track(_) | board::ItemRef::Via(_)))
        .filter(|it| it.layers.iter().any(|l| l == layer))
        .collect();
    for n in pads
        .iter()
        .map(|pp| &pp.net)
        .chain(vias.iter().map(|v| &v.net))
        .chain(tracks.iter().map(|t| &t.net))
        .chain(fills.iter().map(|f| &f.net))
    {
        nets.insert(net_key(n), n.clone());
    }
    if nets.is_empty() {
        return;
    }
    x.open("LayerFeature", &[a("layerRef", layer)]);
    for net in nets.values() {
        let np: Vec<&&PlacedPad> = pads.iter().filter(|pp| &pp.net == net).collect();
        let nt: Vec<_> = tracks.iter().filter(|t| &t.net == net).collect();
        let nf: Vec<_> = fills.iter().filter(|f| &f.net == net).collect();
        if !np.is_empty() || !nt.is_empty() || !nf.is_empty() {
            x.open("Set", &net_attrs(net));
            for pp in &np {
                let (s, r) = c.oriented(pp);
                pad(x, d, pp.center, s, r, c.pin(pp));
            }
            if !nt.is_empty() || !nf.is_empty() {
                x.open("Features", &[]);
                x.open("UserSpecial", &[]);
                for t in &nt {
                    let id = d.line(t.width.0);
                    match t.mid.map(|m| arc_edge(t.start, m, t.end)) {
                        Some(Edge::Arc { center, ccw, .. }) => {
                            x.open(
                                "Arc",
                                &[
                                    a("startX", mm(t.start.x.0)),
                                    a("startY", mm(t.start.y.0)),
                                    a("endX", mm(t.end.x.0)),
                                    a("endY", mm(t.end.y.0)),
                                    a("centerX", mm(center.0.round() as i64)),
                                    a("centerY", mm(center.1.round() as i64)),
                                    a("clockwise", if ccw { "false" } else { "true" }),
                                ],
                            );
                            x.empty("LineDescRef", &[a("id", id)]);
                            x.close("Arc");
                        }
                        _ => {
                            let pts = vec![(t.start.x.0, t.start.y.0), (t.end.x.0, t.end.y.0)];
                            stroke(x, d, &Stroke::Line { pts, width: t.width.0 });
                        }
                    }
                }
                for f in &nf {
                    for poly in &f.shape {
                        x.open("Contour", &[]);
                        ring_polygon(x, "Polygon", &poly.outer);
                        for h in &poly.holes {
                            ring_polygon(x, "Cutout", h);
                        }
                        x.close("Contour");
                    }
                }
                x.close("UserSpecial");
                x.close("Features");
            }
            x.close("Set");
        }
        let nv: Vec<_> = vias.iter().filter(|v| &v.net == net).collect();
        if !nv.is_empty() {
            let mut attrs = net_attrs(net);
            attrs.push(a("padUsage", "VIA"));
            x.open("Set", &attrs);
            for v in nv {
                pad(x, d, v.at, PadShape::Circle { d: v.diameter }, Angle::ZERO, None);
            }
            x.close("Set");
        }
    }
    x.close("LayerFeature");
}

fn mask_feature(c: &Ctx<'_>, x: &mut Xml, d: &mut Dict, side: BoardSide) {
    let pads: Vec<&PlacedPad> = super::mask_pads(&c.pads, side).collect();
    if pads.is_empty() {
        return;
    }
    x.open("LayerFeature", &[a("layerRef", side_layer(side, "F.Mask"))]);
    x.open("Set", &[]);
    for pp in pads {
        let (s, r) = super::mask_opening(c.p, c.o, pp);
        pad(x, d, pp.center, s, r, c.pin(pp));
    }
    x.close("Set");
    x.close("LayerFeature");
}

fn paste_feature(c: &Ctx<'_>, x: &mut Xml, d: &mut Dict, side: BoardSide) {
    let pads: Vec<&PlacedPad> = super::paste_pads(&c.pads, side).collect();
    if pads.is_empty() {
        return;
    }
    x.open("LayerFeature", &[a("layerRef", side_layer(side, "F.Paste"))]);
    x.open("Set", &[]);
    for pp in pads {
        let r = c.rotation(pp);
        match &pp.pad.paste {
            Some(Paste::Windows { size, at }) => {
                let pf = &c.p.board().footprints[&pp.refdes];
                let tf = transform(pf);
                for w in at {
                    let local = w.rotated(pp.pad.rotation) + pp.pad.at;
                    pad(x, d, tf(local), super::paste_window(c.p, pp, *size), r, c.pin(pp));
                }
            }
            _ => {
                let (s, r) = super::paste_opening(c.p, pp);
                pad(x, d, pp.center, s, r, c.pin(pp))
            }
        }
    }
    x.close("Set");
    x.close("LayerFeature");
}

fn legend_feature(c: &Ctx<'_>, x: &mut Xml, d: &mut Dict, side: BoardSide) {
    let mut body = Xml::new(x.depth + 1);
    for (refdes, pf) in &c.p.board().footprints {
        if pf.side != side {
            continue;
        }
        let Some(fp) = footprint_for(c.p, refdes) else { continue };
        let tf = transform(pf);
        let xy = |q: Point| (tf(q).x.0, tf(q).y.0);
        let mut strokes = Vec::new();
        for gr in fp.graphics.iter().filter(|gr| gr.layer == GraphicLayer::Silk) {
            match &gr.geometry {
                GraphicGeometry::Path { points } => {
                    strokes.push(Stroke::Line { pts: points.iter().map(|&q| xy(q)).collect(), width: gr.width.0 })
                }
                GraphicGeometry::Polygon { points } => {
                    let mut pts: Vec<_> = points.iter().map(|&q| xy(q)).collect();
                    if let Some(&f) = pts.first() {
                        pts.push(f);
                    }
                    strokes.push(Stroke::Line { pts, width: gr.width.0 })
                }
                GraphicGeometry::Circle { center, radius, filled } => {
                    strokes.push(Stroke::Circle { c: xy(*center), r: radius.0, width: gr.width.0, filled: *filled })
                }
            }
        }
        let attrs = [a("geometryUsage", "GRAPHIC"), a("componentRef", refdes.clone())];
        stroke_set(&mut body, d, &attrs, &strokes);
        if let Some(t) = super::refdes_text(c.p, refdes) {
            let text = text_strokes(refdes, t.at, t.size, Angle::ZERO, side == BoardSide::Bottom, t.width.0);
            let attrs = [a("geometryUsage", "TEXT"), a("componentRef", refdes.clone())];
            stroke_set(&mut body, d, &attrs, &text);
        }
    }
    let layer = side_layer(side, "F.SilkS");
    let min_w = c.p.board().rules.min_silk_width.0;
    for gr in c.p.board().graphics.iter().filter(|g| g.layer == layer) {
        match &gr.kind {
            GraphicKind::Line { points, width } => {
                let pts = points.iter().map(|q| (q.x.0, q.y.0)).collect();
                stroke_set(&mut body, d, &[a("geometryUsage", "GRAPHIC")], &[Stroke::Line { pts, width: width.0 }]);
            }
            GraphicKind::Polygon { points, width } => {
                // The outline (IPC-2581 legend features are strokes).
                let mut pts: Vec<(i64, i64)> = points.iter().map(|q| (q.x.0, q.y.0)).collect();
                if let Some(&f) = pts.first() {
                    pts.push(f);
                }
                stroke_set(
                    &mut body,
                    d,
                    &[a("geometryUsage", "GRAPHIC")],
                    &[Stroke::Line { pts, width: width.0.max(min_w) }],
                );
            }
            GraphicKind::Text { text, at, size, rotation } => {
                let w = (size.0 / 8).max(min_w);
                let strokes = text_strokes(text, *at, *size, *rotation, side == BoardSide::Bottom, w);
                stroke_set(&mut body, d, &[a("geometryUsage", "TEXT")], &strokes);
            }
        }
    }
    if body.out.is_empty() {
        return;
    }
    x.open("LayerFeature", &[a("layerRef", layer)]);
    x.out.push_str(&body.out);
    x.close("LayerFeature");
}

fn drill_feature(x: &mut Xml, name: &str, hs: &[&Hole], first: &mut usize) {
    x.open("LayerFeature", &[a("layerRef", name)]);
    let mut nets: BTreeMap<(bool, String), Option<String>> = BTreeMap::new();
    for h in hs {
        nets.insert(net_key(&h.net), h.net.clone());
    }
    for net in nets.values() {
        x.open("Set", &net_attrs(net));
        for h in hs.iter().filter(|h| &h.net == net) {
            let plating = match (h.kind, h.plated) {
                (HoleKind::Via, _) => "VIA",
                (_, true) => "PLATED",
                (_, false) => "NONPLATED",
            };
            *first += 1;
            let [hx, hy] = xy(h.at);
            x.empty(
                "Hole",
                &[
                    a("name", format!("H{first}")),
                    a("diameter", mm(h.diameter.0)),
                    a("platingStatus", plating),
                    a("plusTol", "0"),
                    a("minusTol", "0"),
                    hx,
                    hy,
                ],
            );
        }
        x.close("Set");
    }
    x.close("LayerFeature");
}

/// Pin 1 corner in the package's zero orientation.
fn pin_one_orientation(fp: &Footprint) -> &'static str {
    let Some(p1) = fp.pads.iter().find(|p| p.number == "1") else { return "OTHER" };
    match (p1.at.x.0.signum(), p1.at.y.0.signum()) {
        (-1, 1) => "UPPER_LEFT",
        (-1, 0) => "LEFT",
        (-1, -1) => "LOWER_LEFT",
        (0, 1) => "UPPER_CENTER",
        (1, 1) => "UPPER_RIGHT",
        (1, 0) => "RIGHT",
        (1, -1) => "LOWER_RIGHT",
        (0, -1) => "LOWER_CENTER",
        _ => "CENTER",
    }
}

fn package(x: &mut Xml, d: &mut Dict, fp: &Footprint) {
    let mut attrs = vec![a("name", fp.name.clone()), a("type", "OTHER")];
    if fp.pads.iter().any(|p| p.number == "1") {
        attrs.push(a("pinOne", "1"));
    }
    attrs.push(a("pinOneOrientation", pin_one_orientation(fp)));
    if let Some(b) = fp.body {
        attrs.push(a("height", mm(b.height.0)));
    }
    x.open("Package", &attrs);
    let outline: Vec<Point> = if fp.courtyard.len() >= 3 {
        fp.courtyard.clone()
    } else {
        // No courtyard: the pads' bounding box.
        let mut bb: Option<(i64, i64, i64, i64)> = None;
        for p in &fp.pads {
            let (w, h) = p.shape.size();
            let r = w.0.max(h.0) / 2;
            let e = (p.at.x.0 - r, p.at.y.0 - r, p.at.x.0 + r, p.at.y.0 + r);
            bb = Some(match bb {
                None => e,
                Some(b) => (b.0.min(e.0), b.1.min(e.1), b.2.max(e.2), b.3.max(e.3)),
            });
        }
        let (x0, y0, x1, y1) = bb.unwrap_or((0, 0, 0, 0));
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].iter().map(|&(px, py)| Point::new(Nm(px), Nm(py))).collect()
    };
    let line = d.line(50_000);
    x.open("Outline", &[]);
    points_polygon(x, "Polygon", &outline);
    x.empty("LineDescRef", &[a("id", line.clone())]);
    x.close("Outline");
    x.empty("PickupPoint", &[a("x", "0"), a("y", "0")]);
    x.open("LandPattern", &[]);
    for p in &fp.pads {
        let pin = (!p.number.is_empty()).then_some((None, p.number.as_str()));
        pad(x, d, p.at, p.shape.clone(), p.rotation, pin);
    }
    x.close("LandPattern");
    if let Some(b) = fp.body {
        let (hw, hl) = (b.width.0 / 2, b.length.0 / 2);
        let (w, l) = (b.width.0 - hw, b.length.0 - hl);
        let pts: Vec<Point> =
            [(-hw, -hl), (w, -hl), (w, l), (-hw, l)].iter().map(|&(px, py)| Point::new(Nm(px), Nm(py))).collect();
        x.open("AssemblyDrawing", &[]);
        x.open("Outline", &[]);
        points_polygon(x, "Polygon", &pts);
        x.empty("LineDescRef", &[a("id", line)]);
        x.close("Outline");
        x.close("AssemblyDrawing");
    }
    let mut seen: Vec<&str> = Vec::new();
    for p in &fp.pads {
        if p.number.is_empty() || seen.contains(&p.number.as_str()) {
            continue;
        }
        seen.push(&p.number);
        let Some(id) = d.prim(p.shape.clone()) else { continue };
        let kind = if matches!(p.kind, PadKind::Smd) { "SURFACE" } else { "THRU" };
        let el = if matches!(p.kind, PadKind::Npth { .. }) { "MECHANICAL" } else { "ELECTRICAL" };
        x.open("Pin", &[a("number", p.number.clone()), a("type", kind), a("electricalType", el)]);
        if p.rotation.normalized() != Angle::ZERO {
            x.empty("Xform", &[a("rotation", deg(p.rotation.normalized()))]);
        }
        x.empty("Location", &xy(p.at));
        x.empty("StandardPrimitiveRef", &[a("id", id)]);
        x.close("Pin");
    }
    x.close("Package");
}

/// Splits the board thickness: (copper thicknesses top to bottom, dielectric thicknesses).
fn stack_thicknesses(p: &Project) -> (Vec<i64>, Vec<i64>) {
    let s = &p.board().stackup;
    let n = s.copper_names().len();
    let cu: Vec<i64> = (0..n).map(|i| if i == 0 || i == n - 1 { s.outer_copper.0 } else { s.inner_copper.0 }).collect();
    let k = n.saturating_sub(1).max(1) as i64;
    let rest = (s.thickness.0 - cu.iter().sum::<i64>()).max(0);
    let mut diel = vec![rest / k; k as usize];
    diel[0] += rest - (rest / k) * k;
    (cu, diel)
}

/// The IPC-2581 revision C document of the project.
pub fn document(p: &Project, o: &Options) -> OutFile {
    let name = p.manifest().name.clone();
    let step = if name.is_empty() { "board".to_string() } else { name.clone() };
    let bom_name = format!("{step}_bom");
    let hs = holes(p);
    let ls = layers(p, &hs);
    let c = Ctx { p, o, pads: board::placed_pads(p) };
    let mut d = Dict::default();

    // Ecad first (it fills the dictionaries), at depth 1.
    let mut e = Xml::new(1);
    e.open("Ecad", &[a("name", step.clone())]);
    e.empty("CadHeader", &[a("units", "MILLIMETER")]);
    e.open("CadData", &[]);
    for (lname, function, side) in &ls.list {
        let attrs =
            [a("name", lname.clone()), a("layerFunction", *function), a("side", *side), a("polarity", "POSITIVE")];
        match ls.drills.iter().find(|(_, n)| n == lname) {
            Some(((from, to), _)) => {
                e.open("Layer", &attrs);
                e.empty(
                    "Span",
                    &[a("fromLayer", ls.copper[from - 1].clone()), a("toLayer", ls.copper[to - 1].clone())],
                );
                e.close("Layer");
            }
            None => e.empty("Layer", &attrs),
        }
    }
    let (cu, diel) = stack_thicknesses(p);
    let total = mm(p.board().stackup.thickness.0);
    let tol = || [a("tolPlus", "0"), a("tolMinus", "0")];
    let [tp, tm] = tol();
    e.open(
        "Stackup",
        &[a("name", "stackup"), a("overallThickness", total.clone()), tp, tm, a("whereMeasured", "METAL")],
    );
    let [tp, tm] = tol();
    e.open("StackupGroup", &[a("name", "stackup_group"), a("thickness", total), tp, tm]);
    let mut seq = 0;
    for (i, cname) in ls.copper.iter().enumerate() {
        let mut stack = vec![(cname.clone(), cu[i])];
        if i < diel.len() {
            stack.push((format!("Dielectric{}", i + 1), diel[i]));
        }
        for (lname, t) in stack {
            seq += 1;
            let [tp, tm] = tol();
            e.empty(
                "StackupLayer",
                &[a("layerOrGroupRef", lname), a("thickness", mm(t)), tp, tm, a("sequence", seq.to_string())],
            );
        }
    }
    e.close("StackupGroup");
    e.close("Stackup");

    e.open("Step", &[a("name", step.clone()), a("type", "BOARD")]);
    e.empty("Datum", &[a("x", "0"), a("y", "0")]);
    if let Some((outer, cutouts)) = board_profile(p) {
        e.open("Profile", &[]);
        polygon(&mut e, "Polygon", &outer);
        for cut in &cutouts {
            polygon(&mut e, "Cutout", cut);
        }
        e.close("Profile");
    }
    // Packages: every footprint used by a component.
    let mut fps: BTreeMap<String, &Footprint> = BTreeMap::new();
    for refdes in p.circuit().components.keys() {
        if let Some(fp) = footprint_for(p, refdes) {
            fps.entry(fp.name.clone()).or_insert(fp);
        }
    }
    for fp in fps.values() {
        package(&mut e, &mut d, fp);
    }
    for (refdes, pf) in &p.board().footprints {
        let Some(fp) = footprint_for(p, refdes) else { continue };
        let Some(comp) = p.circuit().components.get(refdes) else { continue };
        let layer = if pf.side == BoardSide::Top {
            ls.copper[0].clone()
        } else {
            ls.copper.last().cloned().unwrap_or_default()
        };
        let mount = if fp.mount == Mount::Smd { "SMT" } else { "THMT" };
        e.open(
            "Component",
            &[
                a("refDes", refdes.clone()),
                a("packageRef", fp.name.clone()),
                a("part", comp.part.clone()),
                a("layerRef", layer),
                a("mountType", mount),
            ],
        );
        let r = pf.rotation.normalized();
        let mut xf = Vec::new();
        if r != Angle::ZERO {
            xf.push(a("rotation", deg(r)));
        }
        if pf.side == BoardSide::Bottom {
            xf.push(a("mirror", "true"));
        }
        if !xf.is_empty() {
            e.empty("Xform", &xf);
        }
        e.empty("Location", &xy(pf.at));
        e.close("Component");
    }
    // Logical nets, from the placed component pads.
    let mut nets: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for pp in &c.pads {
        if let (Some(n), Some((Some(r), num))) = (pp.net.as_deref(), c.pin(pp)) {
            let v = nets.entry(n).or_default();
            if !v.contains(&(r, num)) {
                v.push((r, num));
            }
        }
    }
    for (n, pins) in &nets {
        e.open("LogicalNet", &[a("name", *n)]);
        for (r, num) in pins {
            e.empty("PinRef", &[a("componentRef", *r), a("pin", *num)]);
        }
        e.close("LogicalNet");
    }
    // Layer features, in layer order.
    let items = board::copper_items(p);
    let mut hole_no = 0;
    for (lname, function, _) in &ls.list {
        match *function {
            "CONDUCTOR" => copper_feature(&c, &mut e, &mut d, lname, &items),
            "SOLDERMASK" => mask_feature(&c, &mut e, &mut d, side_of(lname)),
            "SOLDERPASTE" => paste_feature(&c, &mut e, &mut d, side_of(lname)),
            "SILKSCREEN" => legend_feature(&c, &mut e, &mut d, side_of(lname)),
            "DRILL" => {
                let span = ls.drills.iter().find(|(_, n)| n == lname).map(|(s, _)| *s);
                let group: Vec<&Hole> = hs.iter().filter(|h| Some(h.span) == span).collect();
                if !group.is_empty() {
                    drill_feature(&mut e, lname, &group, &mut hole_no);
                }
            }
            _ => {}
        }
    }
    e.close("Step");
    e.close("CadData");
    e.close("Ecad");

    // Bill of materials.
    let mut bom = Xml::new(1);
    bom.open("Bom", &[a("name", bom_name.clone())]);
    bom.open("BomHeader", &[a("assembly", step.clone()), a("revision", "1.0")]);
    bom.empty("StepRef", &[a("name", step.clone())]);
    bom.close("BomHeader");
    for row in crate::bom::rows(p) {
        let any = row.refdes.first().or(row.dnp.first()).cloned().unwrap_or_default();
        let fp = footprint_for(p, &any);
        let pins = fp.map_or(0, |f| f.pad_numbers().len());
        let mut attrs = vec![
            a("OEMDesignNumberRef", row.part.clone()),
            a("quantity", row.quantity.to_string()),
            a("pinCount", pins.to_string()),
            a("category", "ELECTRICAL"),
        ];
        if !row.description.is_empty() {
            attrs.push(a("description", row.description.clone()));
        }
        bom.open("BomItem", &attrs);
        for (r, populate) in row.refdes.iter().map(|r| (r, true)).chain(row.dnp.iter().map(|r| (r, false))) {
            let pkg = footprint_for(p, r).map(|f| f.name.clone()).or_else(|| row.footprint.clone()).unwrap_or_default();
            let layer = match p.board().footprints.get(r) {
                Some(pf) if pf.side == BoardSide::Bottom => ls.copper.last().cloned().unwrap_or_default(),
                _ => ls.copper[0].clone(),
            };
            bom.empty(
                "RefDes",
                &[
                    a("name", r.clone()),
                    a("packageRef", pkg),
                    a("populate", if populate { "true" } else { "false" }),
                    a("layerRef", layer),
                ],
            );
        }
        bom.open("Characteristics", &[a("category", "ELECTRICAL")]);
        let mut chars = vec![("Value", row.value.clone())];
        if let Some(m) = &row.manufacturer {
            chars.push(("Manufacturer", m.clone()));
        }
        if let Some(m) = &row.mpn {
            chars.push(("MPN", m.clone()));
        }
        if let Some(pk) = &row.package {
            chars.push(("Package", pk.clone()));
        }
        for (k, v) in chars {
            bom.empty(
                "Textual",
                &[
                    a("definitionSource", "cadlab"),
                    a("textualCharacteristicName", k),
                    a("textualCharacteristicValue", v),
                ],
            );
        }
        bom.close("Characteristics");
        bom.close("BomItem");
    }
    bom.close("Bom");

    // Header, content and dictionaries.
    let mut x = Xml::new(0);
    x.out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.open(
        "IPC-2581",
        &[a("revision", "C"), a("xmlns", NAMESPACE), a("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance")],
    );
    x.open("Content", &[a("roleRef", "Owner")]);
    x.empty("FunctionMode", &[a("mode", "ASSEMBLY")]);
    x.empty("StepRef", &[a("name", step.clone())]);
    for (lname, _, _) in &ls.list {
        x.empty("LayerRef", &[a("name", lname.clone())]);
    }
    x.empty("BomRef", &[a("name", bom_name)]);
    x.open("DictionaryLineDesc", &[a("units", "MILLIMETER")]);
    for (id, w) in &d.lines {
        x.open("EntryLineDesc", &[a("id", id.clone())]);
        x.empty("LineDesc", &[a("lineEnd", "ROUND"), a("lineWidth", mm(*w))]);
        x.close("EntryLineDesc");
    }
    x.close("DictionaryLineDesc");
    x.open("DictionaryStandard", &[a("units", "MILLIMETER")]);
    for (id, (el, attrs, contour)) in &d.standard {
        x.open("EntryStandard", &[a("id", id.clone())]);
        if contour.is_empty() {
            x.empty(el, attrs);
        } else {
            // The outline joins holes by zero-width cuts; IPC-2581 has cutouts for them.
            let ring: Vec<polyclip::Point> = contour.iter().map(|&q| q.into()).collect();
            let set = polyclip::union_all(&ring, polyclip::FillRule::NonZero).unwrap_or_default();
            for pg in &set {
                x.open("Contour", &[]);
                ring_polygon(&mut x, "Polygon", &pg.outer);
                for h in &pg.holes {
                    ring_polygon(&mut x, "Cutout", h);
                }
                x.close("Contour");
            }
        }
        x.close("EntryStandard");
    }
    x.close("DictionaryStandard");
    x.close("Content");
    x.open("LogisticHeader", &[]);
    x.empty("Role", &[a("id", "Owner"), a("roleFunction", "SENDER")]);
    x.empty("Enterprise", &[a("id", "UNKNOWN"), a("code", "NONE")]);
    x.empty("Person", &[a("name", "UNKNOWN"), a("enterpriseRef", "UNKNOWN"), a("roleRef", "Owner")]);
    x.close("LogisticHeader");
    x.out.push_str(&bom.out);
    x.out.push_str(&e.out);
    x.close("IPC-2581");
    OutFile { name: file_name(&name, &FileKind::Ipc2581), function: "IPC-2581C".into(), content: x.out }
}

fn side_of(layer: &str) -> BoardSide {
    if layer.starts_with("B.") { BoardSide::Bottom } else { BoardSide::Top }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_and_escaping() {
        let mut d = Dict::default();
        assert_eq!(d.prim(PadShape::Circle { d: Nm(600_000) }).unwrap(), "CIRCLE_0.6");
        assert_eq!(d.prim(PadShape::Oval { w: Nm(600_000), h: Nm(600_000) }).unwrap(), "CIRCLE_0.6");
        assert_eq!(d.prim(PadShape::RoundRect { w: Nm(1_000_000), h: Nm(600_000), r: Nm(0) }).unwrap(), "RECT_1X0.6");
        assert_eq!(
            d.prim(PadShape::RoundRect { w: Nm(1_000_000), h: Nm(600_000), r: Nm(150_000) }).unwrap(),
            "RRECT_1X0.6R0.15"
        );
        assert_eq!(
            d.prim(PadShape::RoundRect { w: Nm(1_000_000), h: Nm(600_000), r: Nm(300_000) }).unwrap(),
            "OVAL_1X0.6"
        );
        assert!(d.prim(PadShape::Rect { w: Nm(0), h: Nm(1) }).is_none());
        assert_eq!(d.standard.len(), 4);
        assert_eq!(esc("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
        assert_eq!(dielectric_function(0, 2), "DIELCORE");
        assert_eq!(dielectric_function(0, 4), "DIELPREG");
        assert_eq!(dielectric_function(1, 4), "DIELCORE");
    }
}
