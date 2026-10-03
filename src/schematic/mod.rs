//! Schematic view: an automatically laid-out, label-style drawing of the circuit for review.
//!
//! The circuit (netlist) is the source of truth (DECISIONS D2); the schematic is derived from it.
//! [`layout`] places symbols on a 2.54 mm grid, one group per IC or connector ("anchor"):
//! - series parts sit inline on the anchor pin they connect to, chains continue outward;
//! - pull-ups/pull-downs and other parts between a pin and a supply branch off the pin's wire;
//! - a crystal between two pins of one side sits next to them with its load capacitors;
//! - decoupling capacitors stand on shared supply/ground wires under their IC;
//! - same-net supply pins on the top or bottom of a symbol share one power symbol;
//! - every other connection is shown with net labels and power/ground symbols.
//!
//! Every element is placed against the boxes already used (symbols, text, labels, wires), so
//! nothing overlaps ([`overlaps`] checks a finished sheet). Two-terminal parts left over form
//! vertical chains. The components of a block instance are laid out together in a frame titled
//! with the instance name. Groups are skyline-packed onto the smallest sheet that holds them
//! ([`layout`], used for the KiCad export); [`layout_sheets`] stops at A3 and continues on more
//! sheets. Placement hints in `schematic.json` override the automatic position of a component.
//! [`draw`] turns a layout into a [`crate::render::Scene`]; [`kicad`] exports it as a KiCad
//! schematic.

mod draw;
pub mod kicad;
mod layout;
mod pack;
mod shapes;
pub mod symbol;

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::Point;
use crate::model::Project;
use crate::units::Nm;

pub use draw::draw;
pub use layout::{layout, layout_sheets};
pub use shapes::overlaps;

/// Schematic grid (100 mil).
pub const GRID: Nm = Nm::from_um(2540);

/// A direction on the sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// −X.
    Left,
    /// +X.
    Right,
    /// +Y.
    Up,
    /// −Y.
    Down,
}

impl Dir {
    /// Unit vector.
    pub fn vec(self) -> (i64, i64) {
        match self {
            Dir::Left => (-1, 0),
            Dir::Right => (1, 0),
            Dir::Up => (0, 1),
            Dir::Down => (0, -1),
        }
    }

    /// Rotated by `q` quarter turns counter-clockwise.
    pub fn rotated(self, q: u8) -> Dir {
        let order = [Dir::Right, Dir::Up, Dir::Left, Dir::Down];
        let i = order.iter().position(|d| *d == self).expect("listed");
        order[(i + q as usize) % 4]
    }

    /// Opposite direction.
    pub fn opposite(self) -> Dir {
        self.rotated(2)
    }

    /// Whether horizontal.
    pub fn horizontal(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }
}

pub use crate::model::sections::SymbolPlacement as Placement;

/// Kind of a net label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LabelKind {
    /// Net name flag at a pin.
    Net,
    /// Net name on a wire.
    Wire,
    /// Power symbol (supply).
    Power,
    /// Ground symbol.
    Ground,
}

/// A net label or power symbol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Label {
    /// Attachment point (a pin end, or a point on a wire).
    pub at: Point,
    /// Direction it extends in.
    pub dir: Dir,
    /// Net name.
    pub net: String,
    /// Kind.
    pub kind: LabelKind,
}

/// A titled frame around the components of a block instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Frame {
    /// Title: instance name, then the block name in parentheses.
    pub title: String,
    /// Bottom-left corner.
    pub min: Point,
    /// Top-right corner.
    pub max: Point,
}

/// A laid-out sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SheetLayout {
    /// Symbol placements by designator.
    pub placements: BTreeMap<String, Placement>,
    /// Wire segments (horizontal or vertical).
    pub wires: Vec<(Point, Point)>,
    /// Labels and power symbols.
    pub labels: Vec<Label>,
    /// No-connect markers.
    pub no_connects: Vec<Point>,
    /// Block instance frames.
    pub frames: Vec<Frame>,
    /// Sheet number (1-based).
    pub sheet: u32,
    /// Number of sheets of the schematic.
    pub sheets: u32,
    /// Sheet size (width, height); the drawing spans (0, 0) to this, Y up.
    pub size: (Nm, Nm),
    /// Paper name (A4, A3, ... or "User").
    pub paper: String,
}

/// Placement hints stored in `schematic.json`.
pub type Hints = BTreeMap<String, Placement>;

/// Junctions: points where three or more wire ends and pins meet, or where a wire ends on the
/// inside of another wire.
pub fn junctions(p: &Project, l: &SheetLayout) -> Vec<Point> {
    let lib = p.library();
    let mut pins: BTreeSet<(i64, i64)> = BTreeSet::new();
    for (r, pl) in &l.placements {
        let Some(part) = p.circuit().components.get(r).and_then(|c| lib.parts.get(&c.part)) else { continue };
        for (_, end, _) in symbol::pin_ends(&symbol::symbol_of(part), pl) {
            pins.insert((end.x.0, end.y.0));
        }
    }
    let mut ends: BTreeMap<(i64, i64), usize> = BTreeMap::new();
    for (a, b) in &l.wires {
        *ends.entry((a.x.0, a.y.0)).or_default() += 1;
        *ends.entry((b.x.0, b.y.0)).or_default() += 1;
    }
    ends.iter()
        .filter(|(pt, n)| {
            let p = Point::new(Nm(pt.0), Nm(pt.1));
            let inside = l.wires.iter().any(|(a, b)| p != *a && p != *b && shapes::on_seg(p, *a, *b));
            **n + usize::from(pins.contains(pt)) >= 3 || inside
        })
        .map(|(pt, _)| Point::new(Nm(pt.0), Nm(pt.1)))
        .collect()
}
