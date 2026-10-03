//! Project sections. The circuit gains nets in M2, the schematic hints in M3 and the board in M4.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::footprint::Footprint;
use crate::model::part::Part;

pub use crate::model::circuit::{Block, BlockComponent, Circuit, Component, Net, NetClass, PinRef};

/// Parts and footprints available to the project, stored one file each under `library/`.
/// Every part a project uses is copied here, so a project never depends on external libraries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    /// Parts by ID.
    pub parts: BTreeMap<String, Part>,
    /// Footprints by name.
    pub footprints: BTreeMap<String, Footprint>,
}

impl Library {
    /// Finds a part ID case-insensitively (IDs are file names; some filesystems ignore case).
    pub fn find_part_id_ci(&self, id: &str) -> Option<&str> {
        self.parts.keys().find(|k| k.eq_ignore_ascii_case(id)).map(String::as_str)
    }

    /// Finds a footprint name case-insensitively.
    pub fn find_footprint_ci(&self, name: &str) -> Option<&str> {
        self.footprints.keys().find(|k| k.eq_ignore_ascii_case(name)).map(String::as_str)
    }
}

/// An alternate manufacturer part approved for a BOM line.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovedPart {
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Manufacturer part number.
    pub mpn: String,
}

/// Sourcing information for all components using one part.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BomLine {
    /// Approved manufacturer parts, in order of preference. For a concrete part, its own MPN
    /// comes first implicitly; these are alternates. For a generic part, these are the
    /// candidates that satisfy it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<ApprovedPart>,
    /// Notes for purchasing or assembly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Sourcing overlay (`bom.json`). The BOM itself is computed from the circuit's components.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bom {
    /// Per-part sourcing, by part ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lines: BTreeMap<String, BomLine>,
    /// Components not populated at assembly (they stay on the board).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub dnp: BTreeSet<String>,
}

/// Where a schematic symbol sits: origin and rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolPlacement {
    /// Symbol origin on the sheet (Y up, origin at the bottom-left corner).
    pub at: crate::geom::Point,
    /// Counter-clockwise quarter turns (0..3).
    #[serde(default)]
    pub rot: u8,
}

/// Optional schematic presentation hints (`schematic.json`). The drawing is generated; hints
/// only pin components to chosen positions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Schematic {
    /// Fixed symbol positions, by designator.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub placements: std::collections::BTreeMap<String, SymbolPlacement>,
}

/// Physical board: stackup, outline, placement, copper (`board.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Board {}

/// Natural ordering: digit runs compare numerically (`R2` < `R10`, `U1.PA9` < `U1.PA10`).
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["R10", "R2", "C1", "R1", "U1.PA10", "U1.PA9"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["C1", "R1", "R2", "R10", "U1.PA9", "U1.PA10"]);
    }

    #[test]
    fn next_refdes() {
        let mut c = Circuit::default();
        assert_eq!(c.next_refdes("R"), "R1");
        for r in ["R1", "R7", "RN3"] {
            c.components.insert(
                r.into(),
                Component { id: crate::id::ObjectId(1), part: "x".into(), block: None, properties: Default::default() },
            );
        }
        assert_eq!(c.next_refdes("R"), "R8");
        assert_eq!(c.next_refdes("C"), "C1");
    }
}
