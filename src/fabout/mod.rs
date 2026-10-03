//! Manufacturing outputs (roadmap M4), generic and fab-independent: Gerber X2 layers, Gerber X3
//! component layers, Excellon/XNC drill files (and optional Gerber X2 drill files), pick-and-place
//! CSV and the IPC-D-356A bare-board test netlist. See `docs/MANUFACTURING.md`.
//!
//! Every output is deterministic (no dates, stable ordering). File names come from one table,
//! [`file_name`]; fab-specific layouts are applied later by fab profiles (DECISIONS D12).
//!
//! Coordinates: Gerber, drill and IPC-D-356A files use board coordinates unchanged, so they all
//! align (Gerber `.SameCoordinates`). The pick-and-place file is relative to the lower-left
//! corner of the board outline's bounding box.

pub mod excellon;
pub mod gerber;
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
    /// Solder mask opening growth beyond the pad, on every side (default 0: openings equal pads).
    pub mask_expansion: Nm,
    /// Version written to `.GenerationSoftware` and file comments.
    pub version: String,
}

impl Default for Options {
    fn default() -> Self {
        Options { mask_expansion: Nm::ZERO, version: env!("CARGO_PKG_VERSION").to_string() }
    }
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
}

fn side_prefix(s: BoardSide) -> &'static str {
    match s {
        BoardSide::Top => "F",
        BoardSide::Bottom => "B",
    }
}

/// The generic file naming scheme, in one place: `<project>-<layer>.gbr` with layer names as
/// on the board (`.` → `_`), `<project>-PTH.drl` / `-NPTH.drl` for drills, `<project>-pos.csv`
/// and `<project>.d356`. Fab profiles will substitute their own table.
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
    }
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

/// A pad shape grown by `d` on every side (shrunk when negative).
pub(crate) fn grow(shape: PadShape, d: i64) -> PadShape {
    let g = |v: Nm| Nm((v.0 + 2 * d).max(0));
    match shape {
        PadShape::Rect { w, h } => PadShape::Rect { w: g(w), h: g(h) },
        PadShape::RoundRect { w, h, r } => PadShape::RoundRect { w: g(w), h: g(h), r: Nm((r.0 + d).max(0)) },
        PadShape::Circle { d: dd } => PadShape::Circle { d: g(dd) },
        PadShape::Oval { w, h } => PadShape::Oval { w: g(w), h: g(h) },
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
