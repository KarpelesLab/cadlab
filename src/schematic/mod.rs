//! Schematic view: an automatically laid-out, label-style drawing of the circuit for review.
//!
//! The circuit (netlist) is the source of truth (DECISIONS D2); the schematic is derived from it.
//! [`layout`] places symbols on a 2.54 mm grid: ICs and connectors become anchors, passives sit
//! next to the anchor pin they connect to (short straight wires, chains continue outward),
//! decoupling capacitors line up under their IC, and every other connection is shown with net
//! labels and power/ground symbols. Placement hints in `schematic.json` override the automatic
//! position of a component. [`draw`] turns a layout into a [`crate::render::Scene`];
//! [`kicad`] writes it as a KiCad schematic.

mod draw;
pub mod kicad;
mod layout;
pub mod symbol;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::Point;
use crate::units::Nm;

pub use draw::draw;
pub use layout::layout;

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
    /// Sheet size (width, height); the drawing spans (0, 0) to this, Y up.
    pub size: (Nm, Nm),
    /// Paper name (A4, A3, ... or "User").
    pub paper: String,
}

/// Placement hints stored in `schematic.json`.
pub type Hints = BTreeMap<String, Placement>;
