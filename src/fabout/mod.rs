//! Manufacturing outputs (roadmap M4), generic and fab-independent: Gerber X2 layers, Gerber X3
//! component layers, Excellon/XNC drill files (and optional Gerber X2 drill files), pick-and-place
//! CSV, the IPC-D-356A bare-board test netlist and the IPC-2581 revision C XML. See
//! `docs/MANUFACTURING.md`.
//!
//! Every output is deterministic (no dates, stable ordering). File names come from one table,
//! [`file_name`]; fab-specific layouts are applied later by fab profiles (DECISIONS D12).
//!
//! Coordinates: Gerber, drill and IPC-D-356A files use board coordinates unchanged, so they all
//! align (Gerber `.SameCoordinates`). The pick-and-place file is relative to the lower-left
//! corner of the board outline's bounding box.

pub mod excellon;
pub mod gerber;
pub mod ipc2581;
pub mod ipc356;
mod layers;
pub mod pnp;

use std::collections::BTreeMap;

use crate::board::{self, PlacedPad, footprint_for, via_layers};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::model::footprint::{Pad, PadShape};
use crate::units::{Angle, Nm};

pub use layers::{drill_gerbers, gerbers};

/// Export options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Solder mask opening growth beyond pads on every side, for this export, instead of the
    /// board's `mask_expansion` (pads and footprints with their own margin keep it).
    pub mask_expansion: Option<Nm>,
    /// Version written to `.GenerationSoftware` and file comments.
    pub version: String,
}

impl Default for Options {
    fn default() -> Self {
        Options { mask_expansion: None, version: env!("CARGO_PKG_VERSION").to_string() }
    }
}

/// The solder mask margin of a pad: its own (or its footprint's), else the export's, else the
/// board's `mask_expansion`.
pub(crate) fn mask_margin(p: &Project, o: &Options, pp: &PlacedPad) -> i64 {
    pp.overrides.mask_margin.or(o.mask_expansion).unwrap_or(p.board().rules.mask_expansion).0
}

/// Pads with a solder mask opening on `side` ([`PlacedPad::mask_on`]).
pub(crate) fn mask_pads(pads: &[PlacedPad], side: BoardSide) -> impl Iterator<Item = &PlacedPad> {
    pads.iter().filter(move |pp| pp.mask_on(side))
}

/// A pad's mask opening as drawn: shape and rotation ([`oriented`]) grown by the margin.
pub(crate) fn mask_opening(p: &Project, o: &Options, pp: &PlacedPad) -> (PadShape, Angle) {
    let (s, a) = oriented(pp, fp_rotation(p, &pp.refdes));
    (grow(s, mask_margin(p, o, pp)), a)
}

/// Pads with solder paste on `side`: SMD pads facing it (unless `paste: none`), through-hole
/// pads with `paste: pad` on their footprint's side.
pub(crate) fn paste_pads(pads: &[PlacedPad], side: BoardSide) -> impl Iterator<Item = &PlacedPad> {
    pads.iter().filter(move |pp| pp.pad.has_paste() && pp.pad_side == side)
}

/// A pad's paste opening growth along its X and Y axes (before rotation): the margin plus the
/// ratio of the side, each from the pad, else its footprint, else the board.
pub(crate) fn paste_growth(p: &Project, pp: &PlacedPad, shape: &PadShape) -> (i64, i64) {
    let r = &p.board().rules;
    let m = pp.overrides.paste_margin.unwrap_or(r.paste_margin).0;
    let ratio = pp.overrides.paste_ratio.unwrap_or(r.paste_ratio).0 as i128;
    let (w, h) = shape.size();
    let part = |v: Nm| ((v.0 as i128 * ratio) as f64 / 1e6).round() as i64;
    (m + part(w), m + part(h))
}

/// A pad's paste opening as drawn, when it is not a set of windows: shape and rotation
/// ([`oriented`]) resized by the paste margins (a size change, not an offset: rectangles stay
/// sharp and rounded rectangles keep their corner ratio, as KiCad plots paste).
pub(crate) fn paste_opening(p: &Project, pp: &PlacedPad) -> (PadShape, Angle) {
    let (s, a) = oriented(pp, fp_rotation(p, &pp.refdes));
    let (dx, dy) = paste_growth(p, pp, &pp.pad.shape);
    (resize_xy(s, dx, dy), a)
}

/// A paste window of `size` (exposed pad windows): drawn as designed, paste margins do not apply
/// (KiCad plots its paste-only apertures unchanged too).
pub(crate) fn paste_window(_p: &Project, _pp: &PlacedPad, size: (Nm, Nm)) -> PadShape {
    PadShape::Rect { w: size.0, h: size.1 }
}

/// A pad shape `2 dx` wider and `2 dy` taller (smaller when negative), its corner radius
/// scaled with the shorter side; polygons are offset by the smaller of the two.
pub(crate) fn resize_xy(shape: PadShape, dx: i64, dy: i64) -> PadShape {
    let gx = |v: Nm| Nm((v.0 + 2 * dx).max(0));
    let gy = |v: Nm| Nm((v.0 + 2 * dy).max(0));
    match shape {
        PadShape::Rect { w, h } => PadShape::Rect { w: gx(w), h: gy(h) },
        PadShape::RoundRect { w, h, r } => {
            let (nw, nh) = (gx(w), gy(h));
            let (old, new) = (w.0.min(h.0).max(1) as i128, nw.0.min(nh.0) as i128);
            PadShape::RoundRect { w: nw, h: nh, r: Nm((r.0 as i128 * new / old) as i64) }
        }
        s => grow_xy(s, dx, dy),
    }
}

/// Mask (or paste) drawings of footprints on `side` (`Mask` or `Paste` layer) as regions.
pub(crate) fn footprint_openings(
    p: &Project,
    layer: crate::model::footprint::GraphicLayer,
    side: BoardSide,
) -> crate::geom::poly::PolygonSet {
    let mut out = Vec::new();
    for (refdes, pf) in &p.board().footprints {
        let Some(fp) = footprint_for(p, refdes) else { continue };
        for g in fp.graphics.iter().filter(|g| g.layer == layer) {
            if board::pad_side(pf.side, g.back) == side {
                out.extend(board::fp_graphic_shape(pf, g));
            }
        }
    }
    out
}

/// A generated file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutFile {
    /// File name (no directory).
    pub name: String,
    /// What it is (the Gerber `.FileFunction`, or `PickPlace`, `TestNetlist`).
    pub function: String,
    /// Content.
    pub content: String,
}

/// Kinds of output file; [`file_name`] maps them to names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Copper layer, by name (`F.Cu`, `In1.Cu`, `B.Cu`).
    Copper(String),
    /// Solder mask.
    Mask(BoardSide),
    /// Solder paste.
    Paste(BoardSide),
    /// Legend (silkscreen).
    Legend(BoardSide),
    /// Board profile (outline).
    Profile,
    /// Gerber X3 component layer.
    Component(BoardSide),
    /// Excellon/XNC drill file: plated or not, copper layer span (1-based), whether the span is
    /// the full stack.
    Drill {
        /// Plated holes.
        plated: bool,
        /// First copper layer (1-based).
        from: usize,
        /// Last copper layer (1-based).
        to: usize,
        /// Through all layers.
        through: bool,
    },
    /// Gerber X2 drill file (same parameters as `Drill`).
    DrillGerber {
        /// Plated holes.
        plated: bool,
        /// First copper layer (1-based).
        from: usize,
        /// Last copper layer (1-based).
        to: usize,
        /// Through all layers.
        through: bool,
    },
    /// Pick-and-place CSV.
    PickPlace,
    /// IPC-D-356A netlist.
    Ipc356,
    /// IPC-2581 revision C XML.
    Ipc2581,
}

fn side_prefix(s: BoardSide) -> &'static str {
    match s {
        BoardSide::Top => "F",
        BoardSide::Bottom => "B",
    }
}

/// The generic file naming scheme, in one place: `<project>-<layer>.gbr` with layer names as
/// on the board (`.` → `_`), `<project>-PTH.drl` / `-NPTH.drl` for drills, `<project>-pos.csv`
/// `<project>.d356` and `<project>-ipc2581.xml`. Fab profiles will substitute their own table.
pub fn file_name(project: &str, kind: &FileKind) -> String {
    let p = sanitize(project);
    let drill = |plated: bool, from: usize, to: usize, through: bool| {
        let base = if plated { "PTH" } else { "NPTH" };
        if through { base.to_string() } else { format!("{base}-L{from}-L{to}") }
    };
    match kind {
        FileKind::Copper(layer) => format!("{p}-{}.gbr", layer.replace('.', "_")),
        FileKind::Mask(s) => format!("{p}-{}_Mask.gbr", side_prefix(*s)),
        FileKind::Paste(s) => format!("{p}-{}_Paste.gbr", side_prefix(*s)),
        FileKind::Legend(s) => format!("{p}-{}_SilkS.gbr", side_prefix(*s)),
        FileKind::Profile => format!("{p}-Edge_Cuts.gbr"),
        FileKind::Component(s) => format!("{p}-{}_Component.gbr", side_prefix(*s)),
        FileKind::Drill { plated, from, to, through } => format!("{p}-{}.drl", drill(*plated, *from, *to, *through)),
        FileKind::DrillGerber { plated, from, to, through } => {
            format!("{p}-{}-drl.gbr", drill(*plated, *from, *to, *through))
        }
        FileKind::PickPlace => format!("{p}-pos.csv"),
        FileKind::Ipc356 => format!("{p}.d356"),
        FileKind::Ipc2581 => format!("{p}-ipc2581.xml"),
    }
}

/// The portable file stem used for a project's outputs (see [`file_name`]).
pub fn file_stem(project: &str) -> String {
    sanitize(project)
}

/// Keeps file names portable: anything but `[A-Za-z0-9._-]` becomes `_`.
fn sanitize(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    if s.is_empty() { "board".into() } else { s }
}

/// Every output: Gerber layers (with X3 component layers), XNC drill files, pick-and-place and
/// IPC-D-356A.
pub fn all(p: &Project, o: &Options) -> Vec<OutFile> {
    let mut out = gerbers(p, o);
    out.extend(excellon::drills(p, o));
    out.push(pnp::pick_place(p));
    out.push(ipc356::netlist(p, o));
    out
}

/// What a hole is for (Gerber/XNC `.AperFunction` on drill tools).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HoleKind {
    /// Via.
    Via,
    /// Component lead hole.
    Component,
    /// Mechanical (non-plated, mounting, tooling).
    Mechanical,
}

impl HoleKind {
    /// The `.AperFunction` value.
    pub fn function(self) -> &'static str {
        match self {
            HoleKind::Via => "ViaDrill",
            HoleKind::Component => "ComponentDrill",
            HoleKind::Mechanical => "MechanicalDrill",
        }
    }
}

/// A drilled hole.
#[derive(Clone, Debug)]
pub struct Hole {
    /// Center.
    pub at: Point,
    /// Finished diameter.
    pub diameter: Nm,
    /// Plated.
    pub plated: bool,
    /// Copper layer span, 1-based (top = 1).
    pub span: (usize, usize),
    /// Function.
    pub kind: HoleKind,
    /// Net.
    pub net: Option<String>,
    /// Component pad (designator, number), for pad holes.
    pub pad: Option<(String, String)>,
    /// A slot: the centers of its end circles (`diameter` is its width).
    pub slot: Option<(Point, Point)>,
}

/// Every hole: pad holes (by designator and pad order), then vias (board order).
pub fn holes(p: &Project) -> Vec<Hole> {
    let n = p.board().stackup.copper_names().len();
    let mut out = Vec::new();
    for pp in board::placed_pads(p) {
        let Some((d, plated)) = pp.hole else { continue };
        out.push(Hole {
            at: pp.center,
            diameter: d,
            plated,
            span: (1, n),
            // Board holes (mounting holes) are mechanical even when plated.
            kind: if plated && !board::holes::is_hole(p, &pp.refdes) {
                HoleKind::Component
            } else {
                HoleKind::Mechanical
            },
            net: pp.net.clone(),
            pad: Some((pp.refdes.clone(), pp.number.clone())),
            slot: pp.slot,
        });
    }
    let names = p.board().stackup.copper_names();
    for v in &p.board().vias {
        let ls = via_layers(p, v);
        let idx = |l: &String| names.iter().position(|x| x == l).map_or(1, |i| i + 1);
        let (a, b) = (ls.first().map_or(1, idx), ls.last().map_or(n, idx));
        out.push(Hole {
            at: v.at,
            diameter: v.drill,
            plated: true,
            span: (a.min(b), a.max(b)),
            kind: HoleKind::Via,
            net: v.net.clone(),
            pad: None,
            slot: None,
        });
    }
    out
}

/// The pad's rotation on the board: footprint rotation plus pad rotation, with the pad
/// rotation reversed on the bottom side (mirroring a centered, symmetric shape across Y equals
/// rotating it the other way).
pub fn pad_rotation(pp: &PlacedPad, fp_rotation: Angle) -> Angle {
    let r = match pp.side {
        BoardSide::Top => fp_rotation + pp.pad.rotation,
        BoardSide::Bottom => fp_rotation - pp.pad.rotation,
    };
    r.normalized()
}

/// Placement rotation of a placed pad's footprint.
pub(crate) fn fp_rotation(p: &Project, refdes: &str) -> Angle {
    p.board().footprints.get(refdes).map_or(Angle::ZERO, |f| f.rotation)
}

/// A pad shape grown by `d` on every side (shrunk when negative). Growing is an exact offset:
/// a rectangle grows into a rectangle with corners rounded by `d`.
pub(crate) fn grow(shape: PadShape, d: i64) -> PadShape {
    grow_xy(shape, d, d)
}

/// A pad shape grown by `dx` along its X axis and `dy` along Y on each end (shrunk when
/// negative); polygons by the smaller of the two.
pub(crate) fn grow_xy(shape: PadShape, dx: i64, dy: i64) -> PadShape {
    let gx = |v: Nm| Nm((v.0 + 2 * dx).max(0));
    let gy = |v: Nm| Nm((v.0 + 2 * dy).max(0));
    let d = dx.min(dy);
    match shape {
        PadShape::Rect { w, h } if dx == dy && d > 0 => PadShape::RoundRect { w: gx(w), h: gy(h), r: Nm(d) },
        PadShape::Rect { w, h } => PadShape::Rect { w: gx(w), h: gy(h) },
        PadShape::RoundRect { w, h, r } => PadShape::RoundRect { w: gx(w), h: gy(h), r: Nm((r.0 + d).max(0)) },
        PadShape::Circle { d: dd } if dx == dy => PadShape::Circle { d: gx(dd) },
        PadShape::Circle { d: dd } => PadShape::Oval { w: gx(dd), h: gy(dd) },
        PadShape::Oval { w, h } => PadShape::Oval { w: gx(w), h: gy(h) },
        PadShape::Polygon { points } if d != 0 => {
            use crate::geom::poly::{self, ArcTol, Join, Side};
            let ring: Vec<poly::Point> = points.iter().map(|&q| q.into()).collect();
            let grown = poly::Polygon::new(ring, vec![]);
            let tol = ArcTol::new(1_000, if d > 0 { Side::Outside } else { Side::Inside });
            let out = poly::offset(&vec![grown], d, Join::Round, tol).unwrap_or_default();
            let points = out
                .into_iter()
                .max_by_key(|pg| pg.outer.signed_area2())
                .map(|pg| pg.outer.0.iter().map(|&q| q.into()).collect())
                .unwrap_or_default();
            PadShape::Polygon { points }
        }
        s @ PadShape::Polygon { .. } => s,
    }
}

/// A placed pad's shape relative to its center and the rotation to draw it with: standard
/// shapes keep the pad's own shape and its board rotation ([`pad_rotation`]); a polygon pad
/// comes out in board orientation (rotation and bottom-side mirroring applied, from the pad's
/// board copper) with no further rotation.
pub(crate) fn oriented(pp: &PlacedPad, fp_rotation: Angle) -> (PadShape, Angle) {
    match &pp.pad.shape {
        PadShape::Polygon { .. } => {
            // Holes joined back to the outline (the board copper keeps them apart).
            let ring = crate::geom::poly::fracture(&pp.shape).unwrap_or_else(|_| pp.shape.outer.clone());
            let points = ring.0.iter().map(|&q| Point::from(q) - pp.center).collect();
            (PadShape::Polygon { points }, Angle::ZERO)
        }
        s => (s.clone(), pad_rotation(pp, fp_rotation)),
    }
}

/// Lower-left corner of the outline's bounding box (the origin of assembly files), or `None`
/// without an outline.
pub fn outline_origin(p: &Project) -> Option<Point> {
    let c = p.board().outline.contours.first()?;
    let ring = board::contour_ring(c, board::COPPER_TOL);
    let x = ring.iter().map(|q| q.x).min()?;
    let y = ring.iter().map(|q| q.y).min()?;
    Some(Point::new(Nm(x), Nm(y)))
}

/// Refdes text height on the legend.
const REFDES_SIZE: Nm = Nm(1_000_000);
/// Gap between the courtyard and the refdes text.
const REFDES_GAP: Nm = Nm(300_000);

/// A reference designator as printed on the legend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefdesText {
    /// Text center.
    pub at: Point,
    /// Cap height.
    pub size: Nm,
    /// Stroke width.
    pub width: Nm,
}

/// Where the legend prints a footprint's reference designator: upright (mirrored on the
/// bottom side), centered above the placed courtyard's bounding box, or on the footprint
/// origin without a courtyard. `None` when the footprint is not placed or unknown. The KiCad
/// export places its Reference field the same way, so both show the same legend.
pub fn refdes_text(p: &Project, refdes: &str) -> Option<RefdesText> {
    let pf = p.board().footprints.get(refdes)?;
    let fp = footprint_for(p, refdes)?;
    let tf = board::transform(pf);
    let cy: Vec<Point> = fp.courtyard.iter().map(|&q| tf(q)).collect();
    let at = match (cy.iter().map(|q| q.x).min(), cy.iter().map(|q| q.x).max(), cy.iter().map(|q| q.y).max()) {
        (Some(x0), Some(x1), Some(top)) => Point::new(Nm((x0.0 + x1.0) / 2), top + REFDES_GAP + Nm(REFDES_SIZE.0 / 2)),
        _ => pf.at,
    };
    let width = p.board().rules.min_silk_width.max(Nm(150_000));
    Some(RefdesText { at, size: REFDES_SIZE, width })
}

/// Whether `pad` is a heat-sink (exposed) pad: an SMD pad with its own paste windows.
pub(crate) fn is_heatsink(pad: &Pad) -> bool {
    matches!(pad.paste, Some(crate::model::footprint::Paste::Windows { .. }))
}

/// Package and footprint info for a component, for assembly outputs.
#[derive(Clone, Debug, Default)]
pub(crate) struct CompInfo {
    pub value: String,
    pub package: String,
    pub footprint: String,
    pub manufacturer: Option<String>,
    pub mpn: Option<String>,
    pub mount: Option<crate::model::footprint::Mount>,
    pub height: Option<Nm>,
}

/// Assembly info by designator, for populated components only (DNP excluded).
pub(crate) fn populated(p: &Project) -> BTreeMap<String, CompInfo> {
    let mut out = BTreeMap::new();
    for row in crate::bom::rows(p) {
        for r in &row.refdes {
            let fp = footprint_for(p, r);
            out.insert(
                r.clone(),
                CompInfo {
                    value: row.value.clone(),
                    package: row.package.clone().unwrap_or_default(),
                    footprint: fp.map(|f| f.name.clone()).or_else(|| row.footprint.clone()).unwrap_or_default(),
                    manufacturer: row.manufacturer.clone(),
                    mpn: row.mpn.clone(),
                    mount: fp.map(|f| f.mount).or(row.mount),
                    height: fp.and_then(|f| f.body).map(|b| b.height),
                },
            );
        }
    }
    out
}

/// Formats a millideg angle in degrees (`90`, `12.5`).
pub(crate) fn deg(a: Angle) -> String {
    let m = a.millideg();
    let (sign, m) = if m < 0 { ("-", -(m as i64)) } else { ("", m as i64) };
    if m % 1000 == 0 {
        format!("{sign}{}", m / 1000)
    } else {
        format!("{sign}{}.{}", m / 1000, format!("{:03}", m % 1000).trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_table() {
        assert_eq!(file_name("demo", &FileKind::Copper("In1.Cu".into())), "demo-In1_Cu.gbr");
        assert_eq!(file_name("my board", &FileKind::Mask(BoardSide::Bottom)), "my_board-B_Mask.gbr");
        assert_eq!(file_name("d", &FileKind::Drill { plated: true, from: 1, to: 4, through: true }), "d-PTH.drl");
        assert_eq!(
            file_name("d", &FileKind::Drill { plated: true, from: 1, to: 2, through: false }),
            "d-PTH-L1-L2.drl"
        );
        assert_eq!(file_name("", &FileKind::Ipc356), "board.d356");
        assert_eq!(deg(Angle::from_deg(90)), "90");
        assert_eq!(deg(Angle(-12_500)), "-12.5");
    }
}
