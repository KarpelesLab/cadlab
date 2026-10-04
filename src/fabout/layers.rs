//! Gerber X2 layer images: copper, solder mask, paste, legend, profile, X3 component layers and
//! X2 drill files. Spec references are to "The Gerber Layer Format Specification" rev. 2026.05.
//!
//! - Pads are flashes (spec 6.4: all pads flashed, all flashes pads): standard `C`/`R`/`O`
//!   apertures when the rotation is a multiple of 90°, otherwise a fixed aperture macro built
//!   from outline (4) and circle (1) primitives with pre-rotated coordinates (no macro
//!   variables or primitive rotation, for maximum reader compatibility).
//! - Tracks are draws/arcs with circular apertures; zone fills are regions, fractured into
//!   hole-free contours with polyclip.
//! - Solder masks are negative (the image is the openings, spec 5.6.4); mask, paste and legend
//!   objects take `.AperFunction,Material` (spec 5.6.10, "functions on extra layers").

use polyclip::{Circle, EndCap, FillRule, Join, Op, Path, Polygon, PolygonSet};

use super::gerber::{Gerber, Polarity, Seg, Xy, arc_segs, circle_segs, field, mm};
use super::{FileKind, Hole, Options, OutFile, file_name, grow, holes, is_heatsink, pad_rotation, populated};
use crate::board::{
    self, COPPER_TOL, CopperItem, ItemRef, PlacedPad, footprint_for, side_layer, transform, via_layers,
};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind, Segment};
use crate::model::footprint::{GraphicGeometry, GraphicLayer, Mount, PadKind, PadShape, Paste};
use crate::render::font::{self, HAlign, VAlign};
use crate::units::{Angle, Nm};

fn xy(p: Point) -> Xy {
    (p.x.0, p.y.0)
}

fn rot(x: i64, y: i64, a: Angle) -> Xy {
    xy(Point::new(Nm(x), Nm(y)).rotated(a))
}

fn outline_prim(pts: &[Xy]) -> String {
    let mut s = format!("4,1,{}", pts.len());
    for p in pts.iter().chain(pts.first()) {
        s.push_str(&format!(",{},{}", mm(p.0), mm(p.1)));
    }
    s.push_str(",0");
    s
}

/// Macro body of a (rounded) rectangle `w`×`h`, corner radius `r`, rotated by `a`.
fn round_rect_macro(w: i64, h: i64, r: i64, a: Angle) -> String {
    let mut prims = vec![format!("0 Pad {}x{} r{} rotated {}", mm(w), mm(h), mm(r), super::deg(a))];
    let rect = |w: i64, h: i64| {
        let (hw, hh) = (w / 2, h / 2);
        let pts: Vec<Xy> =
            [(-hw, -hh), (w - hw, -hh), (w - hw, h - hh), (-hw, h - hh)].iter().map(|&(x, y)| rot(x, y, a)).collect();
        outline_prim(&pts)
    };
    if r == 0 {
        prims.push(rect(w, h));
    } else {
        if h - 2 * r > 0 {
            prims.push(rect(w, h - 2 * r));
        }
        if w - 2 * r > 0 {
            prims.push(rect(w - 2 * r, h));
        }
        let (cx, cy) = (w / 2 - r, h / 2 - r);
        let mut seen = Vec::new();
        for (sx, sy) in [(1, 1), (-1, 1), (-1, -1), (1, -1)] {
            let c = rot(sx * cx, sy * cy, a);
            if !seen.contains(&c) {
                seen.push(c);
                prims.push(format!("1,1,{},{},{}", mm(2 * r), mm(c.0), mm(c.1)));
            }
        }
    }
    prims.join("*\n")
}

/// Aperture template for a pad shape rotated by `a` on the board.
pub(crate) fn pad_aperture(g: &mut Gerber, shape: PadShape, a: Angle) -> String {
    let a = a.normalized();
    let q = a.quarter_turns();
    let swap = |w: Nm, h: Nm| if q.is_some_and(|q| q % 2 == 1) { (h.0, w.0) } else { (w.0, h.0) };
    match shape {
        PadShape::Polygon { points } => {
            // The outline may join holes to it by zero-width cuts; macro outlines must not touch
            // themselves, so each part is its own outline: exposed, holes cleared.
            let ring: Vec<polyclip::Point> =
                points.iter().map(|q| rot(q.x.0, q.y.0, a)).map(|(x, y)| polyclip::Point::new(x, y)).collect();
            let set = polyclip::union_all(&ring, FillRule::NonZero).unwrap_or_default();
            let xy = |r: &polyclip::Ring| -> Vec<Xy> { r.0.iter().map(|q| (q.x, q.y)).collect() };
            let mut prims = vec![format!("0 Pad polygon of {} vertices", points.len())];
            for pg in &set {
                prims.push(outline_prim(&xy(&pg.outer)));
                prims.extend(pg.holes.iter().map(|h| outline_prim(&xy(h)).replacen("4,1,", "4,0,", 1)));
            }
            g.macro_def(prims.join("*\n"))
        }
        PadShape::Circle { d } => format!("C,{}", mm(d.0)),
        PadShape::Rect { w, h } => match q {
            Some(_) => {
                let (w, h) = swap(w, h);
                format!("R,{}X{}", mm(w), mm(h))
            }
            None => g.macro_def(round_rect_macro(w.0, h.0, 0, a)),
        },
        PadShape::Oval { w, h } => {
            if w == h {
                return format!("C,{}", mm(w.0));
            }
            match q {
                Some(_) => {
                    let (w, h) = swap(w, h);
                    format!("O,{}X{}", mm(w), mm(h))
                }
                None => g.macro_def(round_rect_macro(w.0, h.0, w.0.min(h.0) / 2, a)),
            }
        }
        PadShape::RoundRect { w, h, r } => {
            let r = r.0.clamp(0, w.0.min(h.0) / 2);
            if r == 0 {
                return pad_aperture(g, PadShape::Rect { w, h }, a);
            }
            if 2 * r >= w.0.min(h.0) {
                return pad_aperture(g, PadShape::Oval { w, h }, a);
            }
            match q {
                Some(_) => {
                    let (w, h) = swap(w, h);
                    g.macro_def(round_rect_macro(w, h, r, Angle::ZERO))
                }
                None => g.macro_def(round_rect_macro(w.0, h.0, r, a)),
            }
        }
    }
}

fn side_name(s: BoardSide) -> &'static str {
    match s {
        BoardSide::Top => "Top",
        BoardSide::Bottom => "Bot",
    }
}

struct Ctx<'a> {
    p: &'a Project,
    o: &'a Options,
    name: String,
    copper: Vec<String>,
    pads: Vec<PlacedPad>,
}

impl Ctx<'_> {
    fn rotation(&self, pp: &PlacedPad) -> Angle {
        pad_rotation(pp, super::fp_rotation(self.p, &pp.refdes))
    }

    /// The pad's shape and rotation to flash ([`super::oriented`]).
    fn oriented(&self, pp: &PlacedPad) -> (PadShape, Angle) {
        super::oriented(pp, super::fp_rotation(self.p, &pp.refdes))
    }

    fn out(&self, kind: FileKind, function: &str, g: Gerber) -> OutFile {
        OutFile { name: file_name(&self.name, &kind), function: function.to_string(), content: g.finish() }
    }

    fn gerber(&self, function: &str, pol: Polarity) -> Gerber {
        Gerber::new(&self.o.version, function, pol)
    }

    /// Pads with a solder mask opening on `side` (SMD pads on that side, all hole pads).
    fn mask_pads(&self, side: BoardSide) -> impl Iterator<Item = &PlacedPad> {
        let cu = side_layer(side, "F.Cu");
        self.pads.iter().filter(move |pp| match pp.pad.kind {
            PadKind::Smd => pp.layers.contains(&cu),
            PadKind::Tht { .. } | PadKind::Npth { .. } => true,
        })
    }
}

/// Every Gerber layer: copper (top to bottom), masks, pastes, legends, profile and the X3
/// component layers.
pub fn gerbers(p: &Project, o: &Options) -> Vec<OutFile> {
    let ctx = Ctx {
        p,
        o,
        name: p.manifest().name.clone(),
        copper: p.board().stackup.copper_names(),
        pads: board::placed_pads(p),
    };
    let items = board::copper_items(p);
    let mut out = Vec::new();
    for i in 0..ctx.copper.len() {
        out.push(copper(&ctx, i, &items));
    }
    for s in [BoardSide::Top, BoardSide::Bottom] {
        out.push(mask(&ctx, s));
    }
    for s in [BoardSide::Top, BoardSide::Bottom] {
        out.push(paste(&ctx, s));
    }
    for s in [BoardSide::Top, BoardSide::Bottom] {
        out.push(legend(&ctx, s));
    }
    out.push(profile(&ctx));
    for s in [BoardSide::Top, BoardSide::Bottom] {
        out.push(component(&ctx, s));
    }
    out
}

fn net_attr(net: &Option<String>) -> (&'static str, String) {
    (".N", net.as_deref().map(field).unwrap_or_default())
}

fn copper(ctx: &Ctx<'_>, i: usize, items: &[CopperItem]) -> OutFile {
    let n = ctx.copper.len();
    let layer = &ctx.copper[i];
    let function = if i == 0 {
        "Copper,L1,Top".to_string()
    } else if i == n - 1 {
        format!("Copper,L{n},Bot")
    } else {
        format!("Copper,L{},Inr", i + 1)
    };
    let outer = i == 0 || i == n - 1;
    let mut g = ctx.gerber(&function, Polarity::Positive);

    g.comment("Pads");
    for pp in ctx.pads.iter().filter(|pp| pp.layers.contains(layer)) {
        let (shape, a) = ctx.oriented(pp);
        let ap = pad_aperture(&mut g, shape, a);
        let f = match pp.pad.kind {
            PadKind::Smd if is_heatsink(&pp.pad) => "HeatsinkPad",
            PadKind::Smd => "SMDPad,CuDef",
            _ => "ComponentPad",
        };
        let d = g.aperture(&ap, Some(f));
        let mut attrs = vec![net_attr(&pp.net)];
        if outer && !pp.number.is_empty() {
            attrs.push((".P", format!("{},{}", field(&pp.refdes), field(&pp.number))));
        }
        attrs.push((".C", field(&pp.refdes)));
        g.attrs(&attrs);
        g.flash(d, xy(pp.center));
    }

    g.comment("Vias");
    for v in &ctx.p.board().vias {
        if !via_layers(ctx.p, v).contains(layer) {
            continue;
        }
        let d = g.aperture(&format!("C,{}", mm(v.diameter.0)), Some("ViaPad"));
        g.attrs(&[net_attr(&v.net)]);
        g.flash(d, xy(v.at));
    }

    g.comment("Tracks");
    for t in ctx.p.board().tracks.iter().filter(|t| &t.layer == layer) {
        let d = g.aperture(&format!("C,{}", mm(t.width.0)), Some("Conductor"));
        g.attrs(&[net_attr(&t.net)]);
        match t.mid {
            None => g.polyline(d, &[xy(t.start), xy(t.end)]),
            Some(m) => g.path(d, xy(t.start), &arc_segs(xy(t.start), xy(m), xy(t.end))),
        }
    }

    // Zone fills and any other copper the shared geometry provides.
    let other: Vec<&CopperItem> = items
        .iter()
        .filter(|it| !matches!(it.item, ItemRef::Pad(..) | ItemRef::Track(_) | ItemRef::Via(_)))
        .filter(|it| it.layers.contains(layer))
        .collect();
    if !other.is_empty() {
        g.comment("Copper pours");
    }
    for it in other {
        g.attrs(&[net_attr(&it.net)]);
        g.region(&fractured(&it.shape), Some("Conductor"));
    }

    graphics(ctx, &mut g, layer, "NonConductor", &[]);
    ctx.out(FileKind::Copper(layer.clone()), &function, g)
}

/// Hole-free contours of a polygon set (cut-ins via polyclip's fracture).
fn fractured(set: &PolygonSet) -> Vec<Vec<Xy>> {
    let rings = polyclip::fracture_set(set).unwrap_or_else(|_| set.iter().map(|p| p.outer.clone()).collect());
    rings.into_iter().map(|r| r.0.iter().map(|q| (q.x, q.y)).collect()).collect()
}

fn mask(ctx: &Ctx<'_>, side: BoardSide) -> OutFile {
    let function = format!("Soldermask,{}", side_name(side));
    let mut g = ctx.gerber(&function, Polarity::Negative);
    g.comment("Solder mask openings (negative image); vias are tented");
    let e = ctx.o.mask_expansion.0;
    for pp in ctx.mask_pads(side) {
        let (shape, a) = ctx.oriented(pp);
        let ap = pad_aperture(&mut g, grow(shape, e), a);
        let d = g.aperture(&ap, Some("Material"));
        g.attrs(&[(".C", field(&pp.refdes))]);
        g.flash(d, xy(pp.center));
    }
    graphics(ctx, &mut g, &side_layer(side, "F.Mask"), "Material", &[]);
    ctx.out(FileKind::Mask(side), &function, g)
}

fn paste(ctx: &Ctx<'_>, side: BoardSide) -> OutFile {
    let function = format!("Paste,{}", side_name(side));
    let mut g = ctx.gerber(&function, Polarity::Positive);
    let cu = side_layer(side, "F.Cu");
    for pp in ctx.pads.iter().filter(|pp| pp.pad.kind == PadKind::Smd && pp.layers.contains(&cu)) {
        let a = ctx.rotation(pp);
        g.attrs(&[(".C", field(&pp.refdes))]);
        match &pp.pad.paste {
            None => {
                let (shape, a) = ctx.oriented(pp);
                let ap = pad_aperture(&mut g, shape, a);
                let d = g.aperture(&ap, Some("Material"));
                g.flash(d, xy(pp.center));
            }
            Some(Paste::None) => {}
            Some(Paste::Windows { size, at }) => {
                let ap = pad_aperture(&mut g, PadShape::Rect { w: size.0, h: size.1 }, a);
                let d = g.aperture(&ap, Some("Material"));
                let pf = &ctx.p.board().footprints[&pp.refdes];
                let tf = transform(pf);
                for w in at {
                    let local = w.rotated(pp.pad.rotation) + pp.pad.at;
                    g.flash(d, xy(tf(local)));
                }
            }
        }
    }
    graphics(ctx, &mut g, &side_layer(side, "F.Paste"), "Material", &[]);
    ctx.out(FileKind::Paste(side), &function, g)
}

/// A stroked legend element.
pub(crate) enum Stroke {
    Line { pts: Vec<Xy>, width: i64 },
    Circle { c: Xy, r: i64, width: i64, filled: bool },
}

impl Stroke {
    fn bbox(&self) -> (Xy, Xy) {
        match self {
            Stroke::Line { pts, width } => {
                let h = width / 2 + 1;
                let x0 = pts.iter().map(|p| p.0).min().unwrap_or(0) - h;
                let y0 = pts.iter().map(|p| p.1).min().unwrap_or(0) - h;
                let x1 = pts.iter().map(|p| p.0).max().unwrap_or(0) + h;
                let y1 = pts.iter().map(|p| p.1).max().unwrap_or(0) + h;
                ((x0, y0), (x1, y1))
            }
            Stroke::Circle { c, r, width, .. } => {
                let e = r + width / 2 + 1;
                ((c.0 - e, c.1 - e), (c.0 + e, c.1 + e))
            }
        }
    }

    fn polygons(&self) -> PolygonSet {
        let pt = |p: Xy| polyclip::Point::new(p.0, p.1);
        match self {
            Stroke::Line { pts, width } => {
                let path = Path(pts.iter().map(|&p| pt(p)).collect());
                polyclip::offset_paths(&vec![path], width / 2, Join::Round, EndCap::Round, COPPER_TOL)
                    .unwrap_or_default()
            }
            Stroke::Circle { c, r, width, filled } => {
                let ring = |rad: i64| Circle { center: pt(*c), radius: rad }.to_ring(COPPER_TOL).unwrap_or_default();
                let outer = Polygon::new(ring(r + width / 2), vec![]);
                if *filled || r - width / 2 <= 0 {
                    vec![outer]
                } else {
                    let inner = Polygon::new(ring(r - width / 2), vec![]);
                    polyclip::boolean(Op::Difference, &outer, &inner, FillRule::NonZero).unwrap_or_default()
                }
            }
        }
    }
}

fn overlaps(a: (Xy, Xy), b: (Xy, Xy)) -> bool {
    a.0.0 <= b.1.0 && b.0.0 <= a.1.0 && a.0.1 <= b.1.1 && b.0.1 <= a.1.1
}

/// Text as strokes: centered on `at`, cap height `size`, rotated, mirrored for the bottom side.
pub(crate) fn text_strokes(text: &str, at: Point, size: Nm, rotation: Angle, mirror: bool, width: i64) -> Vec<Stroke> {
    let (s, c) = rotation.to_rad_f64().sin_cos();
    font::layout(text, (0.0, 0.0), size.0 as f64, HAlign::Center, VAlign::Middle, 0)
        .into_iter()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let pts = l
                .into_iter()
                .map(|(x, y)| {
                    let x = if mirror { -x } else { x };
                    let (rx, ry) = (x * c - y * s, x * s + y * c);
                    ((rx.round() as i64) + at.x.0, (ry.round() as i64) + at.y.0)
                })
                .collect();
            Stroke::Line { pts, width }
        })
        .collect()
}

fn emit_stroke(g: &mut Gerber, st: &Stroke, function: &str) {
    match st {
        Stroke::Line { pts, width } => {
            let d = g.aperture(&format!("C,{}", mm(*width)), Some(function));
            g.polyline(d, pts);
        }
        Stroke::Circle { c, r, width, filled: false } => {
            let d = g.aperture(&format!("C,{}", mm(*width)), Some(function));
            let (start, segs) = circle_segs(*c, *r);
            g.path(d, start, &segs);
        }
        Stroke::Circle { c, r, width, filled: true } => {
            let d = g.aperture(&format!("C,{}", mm(2 * r + width)), Some(function));
            g.flash(d, *c);
        }
    }
}

/// Emits strokes, clipped against `openings` (polygon, bbox) where they overlap (drawn as
/// regions there), else as draws.
fn emit_clipped(g: &mut Gerber, strokes: &[Stroke], openings: &[(Polygon, (Xy, Xy))], function: &str) {
    for st in strokes {
        let bb = st.bbox();
        let hits: Vec<Polygon> = openings.iter().filter(|(_, ob)| overlaps(bb, *ob)).map(|(p, _)| p.clone()).collect();
        if hits.is_empty() {
            emit_stroke(g, st, function);
            continue;
        }
        let polys = st.polygons();
        if !polyclip::intersects(&polys, &hits) {
            emit_stroke(g, st, function);
            continue;
        }
        let rest = polyclip::boolean(Op::Difference, &polys, &hits, FillRule::NonZero).unwrap_or_default();
        g.region(&fractured(&rest), Some(function));
    }
}

/// Board graphics on `layer` (lines and texts), clipped against `openings`.
fn graphics(ctx: &Ctx<'_>, g: &mut Gerber, layer: &str, function: &str, openings: &[(Polygon, (Xy, Xy))]) {
    let mirror = layer.starts_with("B.");
    let min_w = ctx.p.board().rules.min_silk_width.0;
    let mut strokes = Vec::new();
    for gr in ctx.p.board().graphics.iter().filter(|gr| gr.layer == layer) {
        match &gr.kind {
            GraphicKind::Line { points, width } => {
                strokes.push(Stroke::Line { pts: points.iter().map(|&p| xy(p)).collect(), width: width.0 })
            }
            GraphicKind::Text { text, at, size, rotation } => {
                strokes.extend(text_strokes(text, *at, *size, *rotation, mirror, (size.0 / 8).max(min_w)))
            }
        }
    }
    if strokes.is_empty() {
        return;
    }
    g.attrs(&[]);
    g.comment(&format!("Board graphics on {layer}"));
    emit_clipped(g, &strokes, openings, function);
}

fn legend(ctx: &Ctx<'_>, side: BoardSide) -> OutFile {
    let function = format!("Legend,{}", side_name(side));
    let mut g = ctx.gerber(&function, Polarity::Positive);
    let e = ctx.o.mask_expansion.0;
    let openings: Vec<(Polygon, (Xy, Xy))> = ctx
        .mask_pads(side)
        .filter_map(|pp| {
            let poly = if e == 0 {
                pp.shape.clone()
            } else {
                polyclip::offset(&pp.shape, e, Join::Round, COPPER_TOL).ok()?.into_iter().next()?
            };
            let bb = poly.bbox()?;
            Some((poly, ((bb.min.x, bb.min.y), (bb.max.x, bb.max.y))))
        })
        .collect();
    for (refdes, pf) in &ctx.p.board().footprints {
        if pf.side != side {
            continue;
        }
        let Some(fp) = footprint_for(ctx.p, refdes) else { continue };
        let tf = transform(pf);
        let mut strokes = Vec::new();
        for gr in fp.graphics.iter().filter(|gr| gr.layer == GraphicLayer::Silk) {
            match &gr.geometry {
                GraphicGeometry::Path { points } => {
                    strokes.push(Stroke::Line { pts: points.iter().map(|&q| xy(tf(q))).collect(), width: gr.width.0 })
                }
                GraphicGeometry::Polygon { points } => {
                    let mut pts: Vec<Xy> = points.iter().map(|&q| xy(tf(q))).collect();
                    if let Some(&f) = pts.first() {
                        pts.push(f);
                    }
                    strokes.push(Stroke::Line { pts, width: gr.width.0 })
                }
                GraphicGeometry::Circle { center, radius, filled } => {
                    strokes.push(Stroke::Circle { c: xy(tf(*center)), r: radius.0, width: gr.width.0, filled: *filled })
                }
            }
        }
        // Reference designator above the courtyard.
        if let Some(t) = super::refdes_text(ctx.p, refdes) {
            strokes.extend(text_strokes(refdes, t.at, t.size, Angle::ZERO, side == BoardSide::Bottom, t.width.0));
        }
        g.attrs(&[(".C", field(refdes))]);
        emit_clipped(&mut g, &strokes, &openings, "Material");
    }
    graphics(ctx, &mut g, &side_layer(side, "F.SilkS"), "Material", &openings);
    ctx.out(FileKind::Legend(side), &function, g)
}

/// Profile aperture: the draw's center line is the board edge (spec 6.5).
const PROFILE_WIDTH: i64 = 100_000;

fn profile(ctx: &Ctx<'_>) -> OutFile {
    let function = "Profile,NP";
    let mut g = ctx.gerber(function, Polarity::Positive);
    let d = g.aperture(&format!("C,{}", mm(PROFILE_WIDTH)), Some("Profile"));
    for c in &ctx.p.board().outline.contours {
        let mut cur = xy(c.start);
        let mut segs = Vec::new();
        for s in &c.segments {
            match *s {
                Segment::Line { to } => segs.push(Seg::Line(xy(to))),
                Segment::Arc { mid, to } => segs.extend(arc_segs(cur, xy(mid), xy(to))),
            }
            cur = match *s {
                Segment::Line { to } | Segment::Arc { to, .. } => xy(to),
            };
        }
        if cur != xy(c.start) {
            segs.push(Seg::Line(xy(c.start)));
        }
        g.path(d, xy(c.start), &segs);
    }
    for gr in ctx.p.board().graphics.iter().filter(|gr| gr.layer == "Edge.Cuts") {
        if let GraphicKind::Line { points, .. } = &gr.kind {
            g.polyline(d, &points.iter().map(|&p| xy(p)).collect::<Vec<_>>());
        }
    }
    ctx.out(FileKind::Profile, function, g)
}

/// Gerber X3 component layer (spec 6.9).
fn component(ctx: &Ctx<'_>, side: BoardSide) -> OutFile {
    let n = ctx.copper.len();
    let function = match side {
        BoardSide::Top => "Component,L1,Top".to_string(),
        BoardSide::Bottom => format!("Component,L{n},Bot"),
    };
    let mut g = ctx.gerber(&function, Polarity::Positive);
    let main = g.aperture("C,0.3", Some("ComponentMain"));
    let outline = g.aperture("C,0.1", Some("ComponentOutline,Courtyard"));
    let key = g.aperture("P,0.36X4X0", Some("ComponentPin"));
    let pin = g.aperture("C,0.1", Some("ComponentPin"));
    let info = populated(ctx.p);
    for (refdes, pf) in &ctx.p.board().footprints {
        if pf.side != side {
            continue;
        }
        let Some(ci) = info.get(refdes) else { continue };
        let crot = match side {
            BoardSide::Top => pf.rotation,
            // X3 bottom zero is the top zero flipped about the X axis; our bottom placement
            // mirrors about Y, which equals that flip plus 180°.
            BoardSide::Bottom => pf.rotation + Angle::DEG_180,
        }
        .normalized();
        let mut attrs = vec![(".C", field(refdes)), (".CRot", super::deg(crot))];
        if let Some(m) = &ci.manufacturer {
            attrs.push((".CMfr", field(m)));
        }
        if let Some(m) = &ci.mpn {
            attrs.push((".CMPN", field(m)));
        }
        attrs.push((".CVal", field(&ci.value)));
        if let Some(m) = ci.mount {
            attrs.push((".CMnt", if m == Mount::Smd { "SMD" } else { "TH" }.into()));
        }
        if !ci.footprint.is_empty() {
            attrs.push((".CFtp", field(&ci.footprint)));
        }
        if !ci.package.is_empty() {
            attrs.push((".CPgN", field(&ci.package)));
        }
        if let Some(h) = ci.height {
            attrs.push((".CHgt", mm(h.0)));
        }
        g.attrs(&attrs);
        g.flash(main, xy(pf.at));
        let Some(fp) = footprint_for(ctx.p, refdes) else { continue };
        let tf = transform(pf);
        if fp.courtyard.len() >= 3 {
            g.attrs(&[(".C", field(refdes))]);
            let mut pts: Vec<Xy> = fp.courtyard.iter().map(|&q| xy(tf(q))).collect();
            pts.push(pts[0]);
            g.polyline(outline, &pts);
        }
        let mut seen: Vec<&str> = Vec::new();
        for pp in ctx.pads.iter().filter(|pp| &pp.refdes == refdes && !pp.number.is_empty()) {
            if seen.contains(&pp.number.as_str()) {
                continue;
            }
            seen.push(&pp.number);
            g.attrs(&[net_attr(&pp.net), (".P", format!("{},{}", field(refdes), field(&pp.number)))]);
            let d = if pp.number == "1" || pp.number == "A1" { key } else { pin };
            g.flash(d, xy(pp.center));
        }
    }
    ctx.out(FileKind::Component(side), &function, g)
}

/// Holes grouped into drill files: (plated, from, to) → holes; plated through first, then
/// partial spans, then non-plated.
pub(crate) fn drill_groups(p: &Project) -> Vec<((bool, usize, usize), Vec<Hole>)> {
    let mut groups: Vec<((bool, usize, usize), Vec<Hole>)> = Vec::new();
    for h in holes(p) {
        let key = (h.plated, h.span.0, h.span.1);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(h),
            None => groups.push((key, vec![h])),
        }
    }
    let n = p.board().stackup.copper_names().len();
    groups.sort_by_key(|((plated, a, b), _)| (!plated, !(*a == 1 && *b == n), *a, *b));
    groups
}

/// The `.FileFunction` of a drill file and whether it spans all layers.
pub(crate) fn drill_function(plated: bool, a: usize, b: usize, n: usize) -> (String, bool) {
    let through = a == 1 && b == n;
    let kind = match (plated, through, a == 1 || b == n) {
        (true, true, _) => "PTH",
        (false, true, _) => "NPTH",
        (_, false, true) => "Blind",
        (_, false, false) => "Buried",
    };
    (format!("{},{a},{b},{kind}", if plated { "Plated" } else { "NonPlated" }), through)
}

/// Tools of a drill file: one per (function, diameter), vias first, then by diameter.
pub(crate) fn drill_tools(holes: &[Hole]) -> Vec<(super::HoleKind, Nm)> {
    let mut tools: Vec<(super::HoleKind, Nm)> = holes.iter().map(|h| (h.kind, h.diameter)).collect();
    tools.sort();
    tools.dedup();
    tools
}

/// Gerber X2 drill files (spec 6.6): one per span and plating, flashes of the finished diameter.
pub fn drill_gerbers(p: &Project, o: &Options) -> Vec<OutFile> {
    let n = p.board().stackup.copper_names().len();
    let name = p.manifest().name.clone();
    let mut out = Vec::new();
    for ((plated, a, b), hs) in drill_groups(p) {
        let (function, through) = drill_function(plated, a, b, n);
        let function = format!("{function},Drill");
        let mut g = Gerber::new(&o.version, &function, Polarity::Positive);
        for (kind, d) in drill_tools(&hs) {
            g.aperture(&format!("C,{}", mm(d.0)), Some(kind.function()));
        }
        for (kind, d) in drill_tools(&hs) {
            let ap = g.aperture(&format!("C,{}", mm(d.0)), Some(kind.function()));
            for h in hs.iter().filter(|h| h.kind == kind && h.diameter == d) {
                if plated {
                    g.attrs(&[net_attr(&h.net)]);
                }
                g.flash(ap, xy(h.at));
            }
        }
        out.push(OutFile {
            name: file_name(&name, &FileKind::DrillGerber { plated, from: a, to: b, through }),
            function,
            content: g.finish(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_apertures() {
        let mut g = Gerber::new("t", "Copper,L1,Top", Polarity::Positive);
        let rect = PadShape::Rect { w: Nm(1_000_000), h: Nm(600_000) };
        assert_eq!(pad_aperture(&mut g, rect.clone(), Angle::ZERO), "R,1X0.6");
        assert_eq!(pad_aperture(&mut g, rect.clone(), Angle::DEG_90), "R,0.6X1");
        assert_eq!(pad_aperture(&mut g, rect.clone(), Angle::DEG_270), "R,0.6X1");
        let oval = PadShape::Oval { w: Nm(1_000_000), h: Nm(600_000) };
        assert_eq!(pad_aperture(&mut g, oval, Angle::DEG_180), "O,1X0.6");
        assert_eq!(pad_aperture(&mut g, PadShape::Circle { d: Nm(500_000) }, Angle(12_000)), "C,0.5");
        let rr = PadShape::RoundRect { w: Nm(1_000_000), h: Nm(600_000), r: Nm(150_000) };
        assert_eq!(pad_aperture(&mut g, rr.clone(), Angle::ZERO), "Shape1");
        assert_eq!(pad_aperture(&mut g, rr.clone(), Angle::DEG_180), "Shape1", "same macro at 180°");
        assert_eq!(pad_aperture(&mut g, rr.clone(), Angle::DEG_90), "Shape2");
        assert_eq!(pad_aperture(&mut g, rect.clone(), Angle::from_deg(45)), "Shape3");
        // Fully rounded corners are an obround.
        let full = PadShape::RoundRect { w: Nm(1_000_000), h: Nm(600_000), r: Nm(300_000) };
        assert_eq!(pad_aperture(&mut g, full, Angle::ZERO), "O,1X0.6");
        let body = round_rect_macro(1_000_000, 600_000, 150_000, Angle::ZERO);
        assert!(body.contains("4,1,4,-0.5,-0.15,0.5,-0.15,0.5,0.15,-0.5,0.15,-0.5,-0.15,0"), "{body}");
        assert!(body.contains("1,1,0.3,0.35,0.15"), "{body}");
    }

    #[test]
    fn polygon_pad_apertures() {
        let mut g = Gerber::new("t", "Copper,L1,Top", Polarity::Positive);
        let p = |x: i64, y: i64| Point::new(Nm(x * 100_000), Nm(y * 100_000));
        // A square ring: the 2 × 2 hole joined to the 6 × 6 outline by a cut along y = 0.
        let ring = vec![
            p(-3, -3),
            p(3, -3),
            p(3, 0),
            p(1, 0),
            p(1, -1),
            p(-1, -1),
            p(-1, 1),
            p(1, 1),
            p(1, 0),
            p(3, 0),
            p(3, 3),
            p(-3, 3),
        ];
        pad_aperture(&mut g, PadShape::Polygon { points: ring }, Angle::ZERO);
        let body = g.finish();
        assert_eq!(body.matches("4,1,").count(), 1, "one exposed outline: {body}");
        assert_eq!(body.matches("4,0,").count(), 1, "the hole cleared: {body}");
        // A quarter turn rotates the vertices: (0.4, 0) → (0, 0.4).
        let mut g = Gerber::new("t", "Copper,L1,Top", Polarity::Positive);
        let tri = vec![p(0, 0), p(4, 0), p(0, 2)];
        pad_aperture(&mut g, PadShape::Polygon { points: tri }, Angle::DEG_90);
        let body = g.finish();
        assert!(body.contains(",0,0.4,"), "{body}");
    }

    #[test]
    fn drill_functions() {
        assert_eq!(drill_function(true, 1, 4, 4), ("Plated,1,4,PTH".into(), true));
        assert_eq!(drill_function(true, 1, 2, 4), ("Plated,1,2,Blind".into(), false));
        assert_eq!(drill_function(true, 2, 3, 4), ("Plated,2,3,Buried".into(), false));
        assert_eq!(drill_function(false, 1, 2, 2), ("NonPlated,1,2,NPTH".into(), true));
    }
}
