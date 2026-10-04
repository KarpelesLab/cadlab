//! Board views: layers in PCB-viewer colors (copper, silkscreen, fab, courtyard, outline, holes),
//! ratsnest, highlight, DRC-style markers, and a "realistic" top/bottom view (solder mask,
//! silkscreen, exposed copper finish). See `docs/RENDERING.md`.
//!
//! Geometry comes from [`crate::board`] (the same pads, tracks, vias and zone fills that DRC and
//! the fab outputs use). Copper of one layer is unioned before drawing, so a semi-transparent
//! layer has a uniform color where items overlap.

use std::collections::{BTreeMap, BTreeSet};

use crate::board::{self as geo, ItemRef};
use crate::geom::Point;
use crate::geom::poly::{ArcTol, FillRule, Polygon, PolygonSet, Side};
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind, PlacedFootprint};
use crate::model::footprint::{Footprint, GraphicGeometry, GraphicLayer, Paste};
use crate::units::{LengthUnit, Nm};

use super::font::{self, HAlign, VAlign};
use super::{Color, Prim, Scene};

/// A DRC-style marker: a point with a label.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    /// Position on the board.
    pub at: Point,
    /// Label drawn next to it.
    pub label: String,
}

/// What a board view shows.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardView {
    /// Layers to draw (`F.Cu`, `B.SilkS`, `Edge.Cuts`, ...); `None` means [`default_layers`].
    /// Ignored by the realistic view.
    pub layers: Option<Vec<String>>,
    /// Draw unrouted connections.
    pub ratsnest: bool,
    /// Net names and designators to emphasize; everything else is dimmed. Empty: no dimming.
    pub highlight: Vec<String>,
    /// Realistic view of one side (mask, silkscreen, finish); the bottom view is mirrored.
    pub realistic: Option<BoardSide>,
    /// Markers drawn on top.
    pub markers: Vec<Marker>,
    /// Output resolution, used to keep hairlines (ratsnest, outlines) and marker labels visible.
    pub px_per_mm: f64,
}

impl Default for BoardView {
    fn default() -> Self {
        BoardView {
            layers: None,
            ratsnest: true,
            highlight: Vec::new(),
            realistic: None,
            markers: Vec::new(),
            px_per_mm: 20.0,
        }
    }
}

/// Background of the layer view.
pub const BACKGROUND: Color = Color::hex(0x0e1116);
const REALISTIC_BG: Color = Color::hex(0x2a2d31);

const TECH: [&str; 5] = ["SilkS", "Fab", "CrtYd", "Mask", "Paste"];

/// Layers drawn when none are requested: all copper, both silkscreens and the outline.
pub fn default_layers(p: &Project) -> Vec<String> {
    let mut v = p.board().stackup.copper_names();
    v.extend(["F.SilkS", "B.SilkS", "Edge.Cuts"].map(String::from));
    v
}

/// Every layer name the board view knows: copper, `F.`/`B.` technical layers, `Edge.Cuts` and
/// the layers of board graphics (`User.*`).
pub fn known_layers(p: &Project) -> Vec<String> {
    let mut v = p.board().stackup.copper_names();
    for side in ["F", "B"] {
        v.extend(TECH.iter().map(|t| format!("{side}.{t}")));
    }
    v.push("Edge.Cuts".into());
    for g in &p.board().graphics {
        if !v.contains(&g.layer) {
            v.push(g.layer.clone());
        }
    }
    v
}

/// Color of a layer in the layer view.
pub fn layer_color(layer: &str) -> Color {
    const INNER: [u32; 6] = [0xd8a422, 0x3fae49, 0xb05cd6, 0x2fb8b0, 0xe07b39, 0x8f9cff];
    match layer {
        "F.Cu" => Color::hex(0xd23c3c),
        "B.Cu" => Color::hex(0x3d7fd9),
        "F.SilkS" => Color::hex(0xf0f0f0),
        "B.SilkS" => Color::hex(0xe8d34a),
        "F.Fab" => Color::hex(0xa8a8a8),
        "B.Fab" => Color::hex(0x7c7c94),
        "F.CrtYd" => Color::hex(0xe040e0),
        "B.CrtYd" => Color::hex(0xa040e0),
        "F.Mask" => Color::hex(0xc060a0),
        "B.Mask" => Color::hex(0x20b0b0),
        "F.Paste" => Color::hex(0xa0a0c8),
        "B.Paste" => Color::hex(0x60a0c8),
        "Edge.Cuts" => Color::hex(0xe6d200),
        l => match l.strip_prefix("In").and_then(|r| r.strip_suffix(".Cu")).and_then(|n| n.parse::<usize>().ok()) {
            Some(n) => Color::hex(INNER[(n.max(1) - 1) % INNER.len()]),
            None => Color::hex(0x9fb4c8), // User.* and anything else
        },
    }
}

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

fn mp(p: Point) -> (f64, f64) {
    (mm(p.x), mm(p.y))
}

fn dim(c: Color, lit: bool) -> Color {
    if lit { c } else { c.alpha((c.3 as u16 * 22 / 100) as u8) }
}

/// Draw order (bottom of the stack first) for the layer view seen from the top.
fn layer_order(p: &Project) -> Vec<String> {
    let cu = p.board().stackup.copper_names();
    let mut v: Vec<String> = ["B.CrtYd", "B.Fab"].map(String::from).to_vec();
    if cu.len() > 1 {
        v.push("B.Cu".into());
    }
    v.extend(["B.Paste", "B.Mask", "B.SilkS"].map(String::from));
    v.extend(cu.iter().filter(|l| l.starts_with("In")).rev().cloned());
    v.push("F.Cu".into());
    v.extend(["F.Paste", "F.Mask", "F.SilkS", "F.Fab", "F.CrtYd"].map(String::from));
    for g in &p.board().graphics {
        if !v.contains(&g.layer) && g.layer != "Edge.Cuts" {
            v.push(g.layer.clone());
        }
    }
    v.push("Edge.Cuts".into());
    v
}

/// Unions polygons (orientation normalized first) into a canonical set; falls back to the
/// input when the boolean fails.
fn union(polys: Vec<Polygon>) -> PolygonSet {
    let polys: Vec<Polygon> = polys
        .into_iter()
        .map(|mut p| {
            if !p.outer.is_ccw() {
                p.outer.reverse_orientation();
            }
            for h in &mut p.holes {
                if h.is_ccw() {
                    h.reverse_orientation();
                }
            }
            p
        })
        .collect();
    crate::geom::poly::union_all(&polys, FillRule::NonZero).unwrap_or(polys)
}

fn fill_set(s: &mut Scene, set: &PolygonSet, color: Color) {
    let mut rings = Vec::new();
    for p in set {
        for r in p.rings() {
            rings.push(r.0.iter().map(|q| (q.x as f64 / 1e6, q.y as f64 / 1e6)).collect::<Vec<_>>());
        }
    }
    if !rings.is_empty() {
        s.region(rings, color);
    }
}

/// Highlight test.
struct Lit<'a> {
    set: BTreeSet<&'a str>,
}

impl Lit<'_> {
    fn all(&self) -> bool {
        self.set.is_empty()
    }
    fn net(&self, net: Option<&str>) -> bool {
        self.all() || net.is_some_and(|n| self.set.contains(n))
    }
    fn refdes(&self, r: &str) -> bool {
        self.all() || self.set.contains(r)
    }
    /// Item label like `U1.3` or `via#4`.
    fn label(&self, l: &str) -> bool {
        self.all() || l.split_once('.').is_some_and(|(r, _)| self.set.contains(r))
    }
}

/// A placed footprint with its geometry, in board coordinates.
struct Placed<'a> {
    refdes: &'a str,
    pf: &'a PlacedFootprint,
    fp: &'a Footprint,
}

fn placed(p: &Project) -> Vec<Placed<'_>> {
    p.board()
        .footprints
        .iter()
        .filter_map(|(r, pf)| Some(Placed { refdes: r, pf, fp: geo::footprint_for(p, r)? }))
        .collect()
}

/// Courtyard bounding box of a placed footprint on the board, in mm (min x, min y, max x, max y).
pub fn footprint_area(p: &Project, refdes: &str) -> Option<(f64, f64, f64, f64)> {
    let pf = p.board().footprints.get(refdes)?;
    let fp = geo::footprint_for(p, refdes)?;
    let tf = geo::transform(pf);
    let mut pts: Vec<Point> = fp.courtyard.iter().map(|&q| tf(q)).collect();
    if pts.is_empty() {
        pts = fp.pads.iter().map(|pd| tf(pd.at)).collect();
    }
    pts.push(pf.at);
    bbox(pts.into_iter().map(mp))
}

fn bbox(pts: impl Iterator<Item = (f64, f64)>) -> Option<(f64, f64, f64, f64)> {
    pts.fold(None, |b, (x, y)| {
        Some(match b {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        })
    })
}

/// Extent of the board in mm: outline, placed courtyards, tracks and vias. `None` for an empty
/// board.
pub fn extent(p: &Project) -> Option<(f64, f64, f64, f64)> {
    let b = p.board();
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for c in &b.outline.contours {
        pts.extend(
            geo::contour_ring(c, ArcTol::new(10_000, Side::Outside))
                .iter()
                .map(|q| (q.x as f64 / 1e6, q.y as f64 / 1e6)),
        );
    }
    for r in b.footprints.keys() {
        if let Some((x0, y0, x1, y1)) = footprint_area(p, r) {
            pts.extend([(x0, y0), (x1, y1)]);
        }
    }
    for t in &b.tracks {
        let w = mm(t.width) / 2.0;
        for q in [t.start, t.end] {
            let (x, y) = mp(q);
            pts.extend([(x - w, y - w), (x + w, y + w)]);
        }
    }
    for v in &b.vias {
        let (x, y) = mp(v.at);
        let r = mm(v.diameter) / 2.0;
        pts.extend([(x - r, y - r), (x + r, y + r)]);
    }
    bbox(pts.into_iter())
}

/// Mirrors an area across the Y axis (x → −x), as the bottom realistic view does.
pub fn mirror_area(a: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    (-a.2, a.1, -a.0, a.3)
}

/// Stroke text with quarter-turn rotation, optionally mirrored (x → −x around the anchor) for
/// bottom-side layers.
#[allow(clippy::too_many_arguments)]
fn text(s: &mut Scene, t: &str, at: (f64, f64), size: f64, quarter_turns: u8, mirror: bool, width: f64, color: Color) {
    for stroke in font::layout(t, (0.0, 0.0), size, HAlign::Center, VAlign::Middle, quarter_turns) {
        if stroke.len() >= 2 {
            let pts = stroke.into_iter().map(|(x, y)| (at.0 + if mirror { -x } else { x }, at.1 + y)).collect();
            s.line(pts, width, color);
        }
    }
}

/// Footprint graphics on one board layer (`F.SilkS`, `B.Fab`, ...).
fn footprint_graphics(s: &mut Scene, pl: &Placed<'_>, layer: &str, color: Color) {
    let tf = geo::transform(pl.pf);
    let side = pl.pf.side;
    let on = |g: &crate::model::footprint::Graphic| {
        g.layer != GraphicLayer::Copper && geo::side_layer(geo::pad_side(side, g.back), g.layer.front_name()) == layer
    };
    for g in pl.fp.graphics.iter().filter(|g| on(g)) {
        let w = mm(g.width).max(0.05);
        match &g.geometry {
            GraphicGeometry::Path { points } => s.line(points.iter().map(|&q| mp(tf(q))).collect(), w, color),
            GraphicGeometry::Polygon { points } => s.outline(points.iter().map(|&q| mp(tf(q))).collect(), w, color),
            GraphicGeometry::Circle { center, radius, filled } => {
                let c = mp(tf(*center));
                if *filled {
                    s.circle(c, mm(*radius), Some(color), None)
                } else {
                    s.circle(c, mm(*radius), None, Some((w, color)))
                }
            }
        }
    }
    if geo::side_layer(side, "F.CrtYd") == layer && !pl.fp.courtyard.is_empty() {
        s.outline(pl.fp.courtyard.iter().map(|&q| mp(tf(q))).collect(), 0.05, color);
    }
    if geo::side_layer(side, "F.SilkS") == layer {
        refdes_text(s, pl, color);
    }
}

/// The designator at the footprint origin, sized to fit the courtyard, mirrored on the bottom.
fn refdes_text(s: &mut Scene, pl: &Placed<'_>, color: Color) {
    let tf = geo::transform(pl.pf);
    let pts: Vec<(f64, f64)> = if pl.fp.courtyard.is_empty() {
        pl.fp.pads.iter().map(|pd| mp(tf(pd.at))).collect()
    } else {
        pl.fp.courtyard.iter().map(|&q| mp(tf(q))).collect()
    };
    let (w, h) = bbox(pts.into_iter()).map_or((2.0, 1.0), |b| (b.2 - b.0, b.3 - b.1));
    let vertical = h > w * 1.15;
    let (along, across) = if vertical { (h, w) } else { (w, h) };
    let per_unit = font::width(pl.refdes, 1.0).max(0.5);
    let size = (along * 0.6 / per_unit).min(across * 0.33).clamp(0.3, 1.5);
    let mirror = pl.pf.side == BoardSide::Bottom;
    text(s, pl.refdes, mp(pl.pf.at), size, u8::from(vertical), mirror, size * 0.15, color);
}

/// Board graphics (lines, texts) on a layer.
fn board_graphics(s: &mut Scene, p: &Project, layer: &str, color: Color) {
    for g in p.board().graphics.iter().filter(|g| g.layer == layer) {
        match &g.kind {
            GraphicKind::Line { points, width } => {
                s.line(points.iter().map(|&q| mp(q)).collect(), mm(*width).max(0.05), color)
            }
            GraphicKind::Polygon { points, .. } => s.fill(points.iter().map(|&q| mp(q)).collect(), color, None),
            GraphicKind::Text { text: t, at, size, rotation } => {
                let qt = ((rotation.to_deg_f64() / 90.0).round().rem_euclid(4.0)) as u8;
                let size = mm(*size);
                text(s, t, mp(*at), size, qt, layer.starts_with("B."), size * 0.15, color);
            }
        }
    }
}

/// Board outline rings in mm.
fn outline_rings(p: &Project) -> Vec<Vec<(f64, f64)>> {
    p.board()
        .outline
        .contours
        .iter()
        .map(|c| {
            geo::contour_ring(c, ArcTol::new(5_000, Side::Outside))
                .iter()
                .map(|q| (q.x as f64 / 1e6, q.y as f64 / 1e6))
                .collect()
        })
        .filter(|r: &Vec<(f64, f64)>| r.len() >= 3)
        .collect()
}

/// A drill hole to draw: center, diameter, plated, and the outline of a slot.
type HoleDraw = ((f64, f64), f64, bool, Option<Vec<(f64, f64)>>);

/// Drill holes (slots with their stadium outline).
fn holes(p: &Project, pads: &[geo::PlacedPad]) -> Vec<HoleDraw> {
    let mut v: Vec<HoleDraw> = pads
        .iter()
        .filter_map(|pp| {
            let (d, plated) = pp.hole?;
            let slot = pp
                .slot
                .and_then(|_| pp.hole_shape())
                .map(|s| s.outer.0.iter().map(|q| (q.x as f64 / 1e6, q.y as f64 / 1e6)).collect());
            Some((mp(pp.center), mm(d), plated, slot))
        })
        .collect();
    v.extend(p.board().vias.iter().map(|vi| (mp(vi.at), mm(vi.drill), true, None)));
    v
}

/// Builds the scene for a board view.
pub fn draw(p: &Project, v: &BoardView) -> Scene {
    let items = geo::copper_items(p);
    let mut s = match v.realistic {
        Some(side) => realistic(p, side, &items),
        None => layers(p, v, &items),
    };
    let hair = (1.5 / v.px_per_mm.max(0.1)).max(0.03);
    let lit = Lit { set: v.highlight.iter().map(String::as_str).collect() };
    if v.ratsnest && v.realistic.is_none() {
        for r in geo::ratsnest_items(&items) {
            let on = lit.net(Some(&r.net)) || lit.label(&r.from) || lit.label(&r.to);
            s.line(vec![mp(r.from_at), mp(r.to_at)], hair, dim(Color::hex(0xd8e4ff).alpha(220), on));
        }
    }
    if v.realistic == Some(BoardSide::Bottom) {
        mirror_scene(&mut s);
    }
    // Markers are drawn after mirroring so their labels stay readable.
    let flip = if v.realistic == Some(BoardSide::Bottom) { -1.0 } else { 1.0 };
    let red = Color::hex(0xff5a1f);
    for m in &v.markers {
        let c = (flip * mm(m.at.x), mm(m.at.y));
        let r = (10.0 / v.px_per_mm).max(0.4);
        s.circle(c, r, Some(red.alpha(70)), Some(((2.0 / v.px_per_mm).max(0.05), red)));
        let k = r * 0.55;
        let w = (1.5 / v.px_per_mm).max(0.04);
        s.line(vec![(c.0 - k, c.1 - k), (c.0 + k, c.1 + k)], w, red);
        s.line(vec![(c.0 - k, c.1 + k), (c.0 + k, c.1 - k)], w, red);
        if !m.label.is_empty() {
            let size = (12.0 / v.px_per_mm).max(0.5);
            s.text(&m.label, (c.0 + r * 1.3, c.1), size, HAlign::Left, VAlign::Middle, 0, Color::hex(0xffd0b0));
        }
    }
    s
}

/// Mirrors every primitive across the Y axis (x → −x).
fn mirror_scene(s: &mut Scene) {
    let f = |pts: &mut Vec<(f64, f64)>| pts.iter_mut().for_each(|p| p.0 = -p.0);
    for p in &mut s.prims {
        match p {
            Prim::Line { pts, .. } | Prim::Polygon { pts, .. } => f(pts),
            Prim::Region { rings, .. } => rings.iter_mut().for_each(f),
            Prim::Circle { c, .. } => c.0 = -c.0,
        }
    }
}

/// The layer view.
fn layers(p: &Project, v: &BoardView, items: &[geo::CopperItem]) -> Scene {
    let mut s = Scene { background: BACKGROUND, prims: Vec::new() };
    let want: Vec<String> = v.layers.clone().unwrap_or_else(|| default_layers(p));
    let lit = Lit { set: v.highlight.iter().map(String::as_str).collect() };
    let board = p.board();
    let copper = board.stackup.copper_names();
    let pads = geo::placed_pads(p);
    let fps = placed(p);
    let hair = (1.5 / v.px_per_mm.max(0.1)).max(0.03);
    let any_copper = want.iter().any(|l| copper.contains(l));
    let order = layer_order(p);
    // Copper of each shown layer, dimmed and lit, unioned (layers in parallel).
    let shown: Vec<&String> = order.iter().filter(|l| want.contains(l) && copper.contains(l)).collect();
    let unions: BTreeMap<&str, (PolygonSet, PolygonSet)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = shown
            .iter()
            .map(|layer| {
                let (mut on, mut off) = (Vec::new(), Vec::new());
                for it in items.iter().filter(|it| it.layers.contains(layer)) {
                    let is_lit = match &it.item {
                        ItemRef::Pad(r, _) => lit.refdes(r) || lit.net(it.net.as_deref()),
                        _ => lit.net(it.net.as_deref()),
                    };
                    if is_lit { &mut on } else { &mut off }.extend(it.shape.iter().cloned());
                }
                (layer.as_str(), sc.spawn(move || (union(off), union(on))))
            })
            .collect();
        jobs.into_iter().map(|(l, h)| (l, h.join().expect("layer union thread"))).collect()
    });
    for layer in order.iter().filter(|l| want.contains(l)) {
        let color = layer_color(layer);
        if let Some((off, on)) = unions.get(layer.as_str()) {
            fill_set(&mut s, off, dim(color.alpha(175), false));
            fill_set(&mut s, on, color.alpha(175));
            if layer == "F.Cu" && any_copper {
                draw_holes(&mut s, p, &pads, hair);
            }
            continue;
        }
        if layer == "Edge.Cuts" {
            for r in outline_rings(p) {
                s.outline(r, hair.max(0.1), color);
            }
            board_graphics(&mut s, p, layer, color);
            continue;
        }
        if let Some(front) = layer.strip_prefix("F.").or_else(|| layer.strip_prefix("B.")).filter(|t| TECH.contains(t))
        {
            if front == "Mask" || front == "Paste" {
                pad_openings(&mut s, board, &pads, layer, front == "Paste", color, &lit, hair);
            } else {
                for pl in &fps {
                    footprint_graphics(&mut s, pl, layer, dim(color, lit.refdes(pl.refdes)));
                }
            }
        }
        board_graphics(&mut s, p, layer, color);
    }
    // Holes go through the board: draw them even when the top copper layer is hidden.
    if any_copper && !want.iter().any(|l| l == "F.Cu") {
        draw_holes(&mut s, p, &pads, hair);
    }
    s
}

fn draw_holes(s: &mut Scene, p: &Project, pads: &[geo::PlacedPad], hair: f64) {
    for (c, d, plated, slot) in holes(p, pads) {
        let stroke = (!plated).then_some((hair, Color::hex(0x9a9a9a)));
        match slot {
            Some(ring) => s.fill(ring, BACKGROUND, stroke),
            None => s.circle(c, d / 2.0, Some(BACKGROUND), stroke),
        }
    }
}

/// Mask openings (pad shapes) or paste openings (pad shapes or paste windows) on one side.
#[allow(clippy::too_many_arguments)]
fn pad_openings(
    s: &mut Scene,
    board: &crate::model::board::Board,
    pads: &[geo::PlacedPad],
    layer: &str,
    paste: bool,
    color: Color,
    lit: &Lit<'_>,
    hair: f64,
) {
    let cu = if layer.starts_with("F.") { "F.Cu" } else { "B.Cu" };
    for pp in pads {
        let outer = pp.layers.iter().any(|l| l == cu);
        let c = dim(color, lit.refdes(&pp.refdes) || lit.net(pp.net.as_deref()));
        if paste {
            if !outer || !pp.pad.has_paste() {
                continue;
            }
            match &pp.pad.paste {
                Some(Paste::None) => {}
                Some(Paste::Windows { size, at }) => {
                    // Window centers are relative to the pad center, in the pad's local axes.
                    let Some(pf) = board.footprints.get(&pp.refdes) else { continue };
                    let tf = geo::transform(pf);
                    let (hw, hh) = (size.0.0 / 2, size.1.0 / 2);
                    for w in at {
                        let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
                            .map(|(x, y)| {
                                let local = Point::new(Nm(w.x.0 + x), Nm(w.y.0 + y)).rotated(pp.pad.rotation);
                                mp(tf(local + pp.pad.at))
                            })
                            .to_vec();
                        s.fill(corners, c.alpha(140), None);
                    }
                }
                None | Some(Paste::Pad) => fill_set(s, &vec![pp.shape.clone()], c.alpha(140)),
            }
        } else if pp.mask_on(if layer.starts_with("F.") { BoardSide::Top } else { BoardSide::Bottom }) {
            let ring: Vec<(f64, f64)> = pp.shape.outer.0.iter().map(|q| (q.x as f64 / 1e6, q.y as f64 / 1e6)).collect();
            s.outline(ring, hair, c);
        }
    }
}

/// Mask/finish palettes for the realistic view.
struct Look {
    substrate: Color,
    mask_copper: Color,
    silk: Color,
    finish: Color,
}

fn look(p: &Project) -> Look {
    let st = &p.board().stackup;
    let pref = |v: &[String]| v.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let (substrate, mask_copper) = match pref(&st.mask_color).as_str() {
        "red" => (0x8c1d1d, 0xb83232),
        "blue" => (0x153a7a, 0x2756a8),
        "black" => (0x141414, 0x262626),
        "white" => (0xd8d8d4, 0xeeeeea),
        "purple" => (0x4a1f6e, 0x6a3496),
        "yellow" => (0xb89a12, 0xd6b928),
        "matte green" | "matt green" => (0x1f4d2a, 0x2c6a3a),
        _ => (0x1e5a2c, 0x2f8040),
    };
    let silk = match pref(&st.silk_color).as_str() {
        "black" => 0x101010,
        "yellow" => 0xf0d840,
        _ => 0xf4f4f0,
    };
    let finish = pref(&st.finish);
    let finish = if finish.contains("hasl") {
        0xc6cbd1
    } else if finish.contains("osp") || finish.contains("bare") {
        0xc8783c
    } else {
        0xd9b35b // ENIG (and no preference)
    };
    Look {
        substrate: Color::hex(substrate),
        mask_copper: Color::hex(mask_copper),
        silk: Color::hex(silk),
        finish: Color::hex(finish),
    }
}

/// The realistic view of one side in board coordinates, never mirrored (the 3D view uses it as
/// the texture of the board faces).
pub fn realistic_scene(p: &Project, side: BoardSide) -> Scene {
    realistic(p, side, &geo::copper_items(p))
}

/// The realistic view of one side, in board coordinates (the caller mirrors the bottom view).
fn realistic(p: &Project, side: BoardSide, items: &[geo::CopperItem]) -> Scene {
    let mut s = Scene { background: REALISTIC_BG, prims: Vec::new() };
    let lk = look(p);
    let cu = if side == BoardSide::Top { "F.Cu" } else { "B.Cu" };
    let silk = geo::side_layer(side, "F.SilkS");
    let rings = outline_rings(p);
    if !rings.is_empty() {
        // Masked laminate.
        s.region(rings.clone(), lk.substrate);
    }
    let covered: Vec<Polygon> =
        items.iter().filter(|it| it.layers.iter().any(|l| l == cu)).flat_map(|it| it.shape.iter().cloned()).collect();
    fill_set(&mut s, &union(covered), lk.mask_copper);
    for pl in placed(p) {
        footprint_graphics(&mut s, &pl, &silk, lk.silk);
    }
    board_graphics(&mut s, p, &silk, lk.silk);
    let pads = geo::placed_pads(p);
    let exposed: Vec<Polygon> = pads
        .iter()
        .filter(|pp| pp.mask_on(side) && pp.layers.iter().any(|l| l == cu))
        .map(|pp| pp.shape.clone())
        .collect();
    fill_set(&mut s, &union(exposed), lk.finish);
    for (c, d, _, slot) in holes(p, &pads) {
        match slot {
            Some(ring) => s.fill(ring, REALISTIC_BG, None),
            None => s.circle(c, d / 2.0, Some(REALISTIC_BG), None),
        }
    }
    for r in rings {
        s.outline(r, 0.05, Color::hex(0x8a7a4a));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_order() {
        assert_eq!(layer_color("F.Cu"), Color::hex(0xd23c3c));
        assert_eq!(layer_color("In1.Cu"), Color::hex(0xd8a422));
        assert_ne!(layer_color("In2.Cu"), layer_color("In1.Cu"));
        assert_eq!(mirror_area((1.0, 2.0, 3.0, 4.0)), (-3.0, 2.0, -1.0, 4.0));
    }
}
