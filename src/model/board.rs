//! The physical board (`board.json`). See `docs/BOARD.md`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::Point;
use crate::id::ObjectId;
use crate::units::{Angle, Nm};
use crate::value::Quantity;

/// Board side of a footprint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BoardSide {
    /// Top (component) side.
    #[default]
    Top,
    /// Bottom side: the footprint is mirrored (x → −x) and its layers swap F ↔ B.
    Bottom,
}

fn is_top(s: &BoardSide) -> bool {
    *s == BoardSide::Top
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Layer build-up and board specification (requirements, not a fab choice: D12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stackup {
    /// Number of copper layers (1, 2, 4, 6, ...).
    pub copper_layers: u8,
    /// Finished board thickness.
    pub thickness: Nm,
    /// Outer copper thickness (35 µm = 1 oz).
    pub outer_copper: Nm,
    /// Inner copper thickness.
    pub inner_copper: Nm,
    /// Surface finish preferences, best first (`ENIG`, `HASL lead-free`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finish: Vec<String>,
    /// Solder mask color preferences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask_color: Vec<String>,
    /// Silkscreen color preferences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub silk_color: Vec<String>,
    /// Dielectric layers between copper layers, top to bottom (`copper_layers − 1` entries:
    /// the first is between `F.Cu` and the next copper layer). Empty: not specified; the
    /// impedance calculator then assumes [`Stackup::effective_dielectrics`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dielectrics: Vec<Dielectric>,
}

/// A dielectric layer of the stackup (core or prepreg), between two copper layers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dielectric {
    /// Thickness, copper to copper.
    pub thickness: Nm,
    /// Relative permittivity (dielectric constant) at the frequency of interest, as an exact
    /// decimal (`"4.5"`).
    pub er: Quantity,
    /// Material or construction, for information (`FR-4 7628 prepreg`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
}

/// Relative permittivity assumed when the stackup gives none: 4.5, a common nominal value for
/// FR-4 around 1 GHz (laminate datasheets give 4.2 to 4.8). Not a fab value.
pub const DEFAULT_ER: Quantity = Quantity::from_parts(45, -1, crate::value::Unit::None);

impl Default for Stackup {
    fn default() -> Self {
        Stackup {
            copper_layers: 2,
            thickness: Nm::from_um(1600),
            outer_copper: Nm::from_um(35),
            inner_copper: Nm(17_500),
            finish: Vec::new(),
            mask_color: Vec::new(),
            silk_color: Vec::new(),
            dielectrics: Vec::new(),
        }
    }
}

impl Stackup {
    /// The dielectrics in effect, top to bottom, and whether they were assumed: the stored
    /// ones when there is one per gap between copper layers, else the material thickness
    /// (board thickness minus copper) split equally over the gaps, at [`DEFAULT_ER`].
    pub fn effective_dielectrics(&self) -> (Vec<Dielectric>, bool) {
        let n = self.copper_layers.max(1) as i64;
        let gaps = (n - 1) as usize;
        if self.dielectrics.len() == gaps {
            return (self.dielectrics.clone(), false);
        }
        if gaps == 0 {
            return (Vec::new(), true);
        }
        let copper = self.outer_copper.0 * 2.min(n) + self.inner_copper.0 * (n - 2).max(0);
        let each = Nm(((self.thickness.0 - copper).max(0)) / gaps as i64);
        let d = Dielectric { thickness: each, er: DEFAULT_ER, material: None };
        (vec![d; gaps], true)
    }

    /// Copper layer names, top to bottom: `F.Cu`, `In1.Cu`, ..., `B.Cu`.
    pub fn copper_names(&self) -> Vec<String> {
        let n = self.copper_layers.max(1) as usize;
        if n == 1 {
            return vec!["F.Cu".into()];
        }
        let mut v = vec!["F.Cu".to_string()];
        v.extend((1..n - 1).map(|i| format!("In{i}.Cu")));
        v.push("B.Cu".into());
        v
    }
}

/// A segment of a closed outline contour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Segment {
    /// Arc through `mid` to `to`.
    Arc {
        /// Point on the arc.
        mid: Point,
        /// End point.
        to: Point,
    },
    /// Straight line to `to`.
    Line {
        /// End point.
        to: Point,
    },
}

/// A closed contour: `start`, then segments; closes back to `start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Contour {
    /// First point.
    pub start: Point,
    /// Segments; the last should end at `start`.
    pub segments: Vec<Segment>,
}

/// Board outline: the first contour is the outer edge, the others are cutouts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    /// Contours.
    #[serde(default)]
    pub contours: Vec<Contour>,
}

/// Design rules: engineering intent, fab-independent (D12). Net classes override per net.
/// Defaults are a conservative generic IPC class 2 set that mainstream fabs can make.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    /// Copper clearance between different nets.
    pub clearance: Nm,
    /// Default track width.
    pub track_width: Nm,
    /// Minimum track width.
    pub min_track_width: Nm,
    /// Default via drill.
    pub via_drill: Nm,
    /// Default via pad diameter.
    pub via_diameter: Nm,
    /// Minimum annular ring.
    pub min_annular_ring: Nm,
    /// Minimum drill.
    pub min_drill: Nm,
    /// Minimum hole-to-hole distance (edge to edge).
    pub hole_to_hole: Nm,
    /// Minimum copper-to-board-edge distance.
    pub copper_to_edge: Nm,
    /// Minimum silkscreen-to-pad distance.
    pub silk_to_pad: Nm,
    /// Minimum silkscreen line width.
    pub min_silk_width: Nm,
    /// Minimum width of zone fill copper.
    pub zone_min_width: Nm,
    /// IPC-6012 class (2 or 3).
    pub ipc_class: u8,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            clearance: Nm::from_um(200),
            track_width: Nm::from_um(250),
            min_track_width: Nm::from_um(150),
            via_drill: Nm::from_um(300),
            via_diameter: Nm::from_um(600),
            min_annular_ring: Nm::from_um(130),
            min_drill: Nm::from_um(300),
            hole_to_hole: Nm::from_um(500),
            copper_to_edge: Nm::from_um(300),
            silk_to_pad: Nm::from_um(150),
            min_silk_width: Nm::from_um(150),
            zone_min_width: Nm::from_um(200),
            ipc_class: 2,
        }
    }
}

/// Names of the length fields of [`Rules`], in declaration order (as in [`Rules::lengths`]).
pub const RULE_FIELDS: [&str; 12] = [
    "clearance",
    "track_width",
    "min_track_width",
    "via_drill",
    "via_diameter",
    "min_annular_ring",
    "min_drill",
    "hole_to_hole",
    "copper_to_edge",
    "silk_to_pad",
    "min_silk_width",
    "zone_min_width",
];

impl Rules {
    /// The length fields with their names ([`RULE_FIELDS`] order).
    pub fn lengths(&self) -> [(&'static str, Nm); 12] {
        let v = [
            self.clearance,
            self.track_width,
            self.min_track_width,
            self.via_drill,
            self.via_diameter,
            self.min_annular_ring,
            self.min_drill,
            self.hole_to_hole,
            self.copper_to_edge,
            self.silk_to_pad,
            self.min_silk_width,
            self.zone_min_width,
        ];
        std::array::from_fn(|i| (RULE_FIELDS[i], v[i]))
    }

    /// Mutable access to a length field by name ([`RULE_FIELDS`]).
    pub fn length_mut(&mut self, field: &str) -> Option<&mut Nm> {
        Some(match field {
            "clearance" => &mut self.clearance,
            "track_width" => &mut self.track_width,
            "min_track_width" => &mut self.min_track_width,
            "via_drill" => &mut self.via_drill,
            "via_diameter" => &mut self.via_diameter,
            "min_annular_ring" => &mut self.min_annular_ring,
            "min_drill" => &mut self.min_drill,
            "hole_to_hole" => &mut self.hole_to_hole,
            "copper_to_edge" => &mut self.copper_to_edge,
            "silk_to_pad" => &mut self.silk_to_pad,
            "min_silk_width" => &mut self.min_silk_width,
            "zone_min_width" => &mut self.zone_min_width,
            _ => return None,
        })
    }

    /// The rule set of a preset (see [`RulePreset`]).
    pub fn preset(preset: RulePreset) -> Rules {
        match preset {
            RulePreset::Ipc2 => Rules::default(),
            // Class 3: annular ring = IPC-6012 class 3 external minimum (0.05 mm) plus half the
            // IPC-2221 level C fabrication allowance (0.4 mm / 2); default vias sized to match.
            // Sources and verification status: docs/BOARD.md, "Rule presets".
            RulePreset::Ipc3 => Rules {
                via_diameter: Nm::from_um(800),
                min_annular_ring: Nm::from_um(250),
                ipc_class: 3,
                ..Rules::default()
            },
        }
    }
}

/// Built-in design rule presets: engineering intent, not a fab's limits (sources in
/// `docs/BOARD.md`, "Rule presets").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RulePreset {
    /// cadlab's conservative IPC-6012 class 2 defaults (the rules of a new project).
    Ipc2,
    /// IPC-6012 class 3 (high reliability): annular rings and default vias sized for class 3.
    Ipc3,
}

/// A footprint placed on the board, by component designator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlacedFootprint {
    /// Footprint origin.
    pub at: Point,
    /// Counter-clockwise rotation (applied after mirroring for the bottom side).
    #[serde(default)]
    pub rotation: Angle,
    /// Side.
    #[serde(default, skip_serializing_if = "is_top")]
    pub side: BoardSide,
    /// Locked: placement commands and auto-placement leave it alone.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Footprint name, when not the part's preferred one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
}

/// A copper track segment (or arc when `mid` is set).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// ID.
    pub id: ObjectId,
    /// Copper layer.
    pub layer: String,
    /// Width.
    pub width: Nm,
    /// Net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Start point.
    pub start: Point,
    /// End point.
    pub end: Point,
    /// Point on the arc, for arc tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<Point>,
    /// Locked against automatic changes (router rip-up).
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

/// A via.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Via {
    /// ID.
    pub id: ObjectId,
    /// Center.
    pub at: Point,
    /// Finished hole diameter.
    pub drill: Nm,
    /// Pad diameter.
    pub diameter: Nm,
    /// Net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// First copper layer (through via: `F.Cu`).
    pub from: String,
    /// Last copper layer (through via: `B.Cu`).
    pub to: String,
    /// Locked against automatic changes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

/// How pads of the zone's net connect to the zone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PadConnection {
    /// Thermal relief spokes.
    #[default]
    Thermal,
    /// Solid connection.
    Solid,
    /// Not connected.
    None,
}

/// A copper pour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    /// ID.
    pub id: ObjectId,
    /// Name, unique on the board (`GND_bottom`).
    pub name: String,
    /// Net (usually ground or a supply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Copper layers filled.
    pub layers: Vec<String>,
    /// Outline polygon.
    pub outline: Vec<Point>,
    /// Higher priority zones fill first and are avoided by lower ones.
    #[serde(default)]
    pub priority: u32,
    /// Clearance to other nets (default: the rules' clearance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// Minimum copper width (default: the rules' zone minimum).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<Nm>,
    /// Pad connection style.
    #[serde(default)]
    pub pads: PadConnection,
    /// Thermal relief gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_gap: Option<Nm>,
    /// Thermal relief spoke width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_spoke: Option<Nm>,
}

/// An area where some items are forbidden.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Keepout {
    /// ID.
    pub id: ObjectId,
    /// Name.
    pub name: String,
    /// Layers it applies to (copper layers; empty = all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Outline polygon.
    pub outline: Vec<Point>,
    /// No tracks.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_tracks: bool,
    /// No vias.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_vias: bool,
    /// No copper pours.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_pours: bool,
    /// No footprints (courtyards).
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_footprints: bool,
}

/// A board-level drilled hole (mounting hole): non-plated, or plated with a round copper pad.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hole {
    /// ID.
    pub id: ObjectId,
    /// Name, unique on the board and distinct from component designators (`H1`).
    pub name: String,
    /// Center.
    pub at: Point,
    /// Finished hole diameter.
    pub drill: Nm,
    /// Diameter of the plated copper pad; absent for a non-plated hole (NPTH).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad: Option<Nm>,
    /// Net of the plated pad (usually ground).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
}

impl Hole {
    /// Outer diameter: the pad when plated, else the drill.
    pub fn diameter(&self) -> Nm {
        self.pad.unwrap_or(self.drill).max(self.drill)
    }
}

/// A drawing on a non-copper layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BoardGraphic {
    /// ID.
    pub id: ObjectId,
    /// Layer (`F.SilkS`, `B.Fab`, `User.1`, ...).
    pub layer: String,
    /// What it is.
    #[serde(flatten)]
    pub kind: GraphicKind,
}

/// Board graphic geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphicKind {
    /// Polyline.
    Line {
        /// Vertices.
        points: Vec<Point>,
        /// Stroke width.
        width: Nm,
    },
    /// Text.
    Text {
        /// The text.
        text: String,
        /// Anchor (center).
        at: Point,
        /// Character height.
        size: Nm,
        /// Rotation.
        #[serde(default)]
        rotation: Angle,
    },
}

/// The board (`board.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Board {
    /// Layer build-up.
    #[serde(default)]
    pub stackup: Stackup,
    /// Board outline.
    #[serde(default)]
    pub outline: Outline,
    /// Design rules.
    #[serde(default)]
    pub rules: Rules,
    /// Placed footprints by designator.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub footprints: BTreeMap<String, PlacedFootprint>,
    /// Tracks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
    /// Vias.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vias: Vec<Via>,
    /// Copper zones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<Zone>,
    /// Keep-out areas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keepouts: Vec<Keepout>,
    /// Board-level holes (mounting holes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<Hole>,
    /// Graphics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphics: Vec<BoardGraphic>,
}

impl Board {
    /// Whether `name` is a copper layer of this board.
    pub fn is_copper(&self, name: &str) -> bool {
        self.stackup.copper_names().iter().any(|n| n == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_names() {
        let mut s = Stackup::default();
        assert_eq!(s.copper_names(), ["F.Cu", "B.Cu"]);
        s.copper_layers = 4;
        assert_eq!(s.copper_names(), ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]);
        s.copper_layers = 1;
        assert_eq!(s.copper_names(), ["F.Cu"]);
    }

    #[test]
    fn serde_defaults_and_segments() {
        let b: Board = serde_json::from_str("{}").unwrap();
        assert_eq!(b, Board::default());
        let c: Contour = serde_json::from_str(
            r#"{"start": ["0mm", "0mm"], "segments": [{"to": ["10mm", "0mm"]}, {"mid": ["15mm", "5mm"], "to": ["10mm", "10mm"]}, {"to": ["0mm", "0mm"]}]}"#,
        )
        .unwrap();
        assert!(matches!(c.segments[1], Segment::Arc { .. }));
        assert!(matches!(c.segments[0], Segment::Line { .. }));
    }
}
