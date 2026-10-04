//! Footprints (land patterns): pads, courtyard, silkscreen and fabrication outlines.
//!
//! Coordinates are footprint-local, Y up, origin at the package center, in IPC-7351 zero
//! orientation (pin 1 top-left for dual and quad packages, pin 1 left for two-terminal parts).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::Point;
use crate::model::board::PadConnection;
use crate::units::{Angle, Nm, Scale};

/// Pad copper shape.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum PadShape {
    /// Rectangle.
    Rect {
        /// Width (X).
        w: Nm,
        /// Height (Y).
        h: Nm,
    },
    /// Rectangle with rounded corners.
    RoundRect {
        /// Width (X).
        w: Nm,
        /// Height (Y).
        h: Nm,
        /// Corner radius.
        r: Nm,
    },
    /// Circle.
    Circle {
        /// Diameter.
        d: Nm,
    },
    /// Stadium (obround): a rectangle with fully rounded short ends.
    Oval {
        /// Width (X).
        w: Nm,
        /// Height (Y).
        h: Nm,
    },
    /// Any outline (KiCad custom pads, solder jumpers): a simple polygon, vertices relative to
    /// the pad center before the pad rotation, curves already approximated.
    Polygon {
        /// Vertices (not repeating the first).
        points: Vec<Point>,
    },
}

impl PadShape {
    /// Width and height of the bounding box (before pad rotation), centered on the pad center
    /// (a polygon off center gets the box around it that is symmetric about the center).
    pub fn size(&self) -> (Nm, Nm) {
        match *self {
            PadShape::Rect { w, h } | PadShape::RoundRect { w, h, .. } | PadShape::Oval { w, h } => (w, h),
            PadShape::Circle { d } => (d, d),
            PadShape::Polygon { ref points } => {
                let ex = points.iter().map(|q| q.x.0.abs()).max().unwrap_or(0);
                let ey = points.iter().map(|q| q.y.0.abs()).max().unwrap_or(0);
                (Nm(2 * ex), Nm(2 * ey))
            }
        }
    }
}

/// How a pad is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PadKind {
    /// Surface-mount on the top side: copper, paste and mask openings.
    Smd,
    /// Plated through hole: copper on all layers.
    Tht {
        /// Finished hole diameter.
        drill: Nm,
    },
    /// Non-plated hole, no copper.
    Npth {
        /// Hole diameter.
        drill: Nm,
    },
}

/// Solder paste for a pad, when it differs from the default (SMD pads: an opening equal to the
/// pad, grown by the paste margins; through-hole pads: none).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Paste {
    /// No paste.
    None,
    /// An opening equal to the pad (grown by the paste margins), also on a through-hole pad
    /// (paste-in-hole reflow).
    Pad,
    /// Several rectangular openings (exposed pads), centers relative to the pad.
    Windows {
        /// Opening size.
        size: (Nm, Nm),
        /// Opening centers relative to the pad center.
        at: Vec<Point>,
    },
}

/// A pad. (No `deny_unknown_fields`: serde does not support it with flattened fields.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Pad {
    /// Pad number; parts map pins to pad numbers. Empty for mechanical holes.
    pub number: String,
    /// Center.
    pub at: Point,
    /// Rotation around the center.
    #[serde(default, skip_serializing_if = "is_zero_angle")]
    pub rotation: Angle,
    /// Copper shape.
    #[serde(flatten)]
    pub shape: PadShape,
    /// SMD, through-hole or non-plated.
    #[serde(flatten)]
    pub kind: PadKind,
    /// Solder paste; absent means an opening equal to the pad for SMD pads, none for
    /// through-hole pads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste: Option<Paste>,
    /// An SMD pad on the other side of the footprint (`B.Cu` for a footprint placed on top):
    /// card-edge fingers, pads soldered from the back.
    #[serde(default, skip_serializing_if = "is_false")]
    pub back: bool,
    /// Solder mask openings: on the pad's outer copper (default), on one side only, or none
    /// (tented thermal vias, heat spreaders, test copper covered by mask).
    #[serde(default, skip_serializing_if = "MaskOpening::is_default")]
    pub mask: MaskOpening,
    /// Slotted (oval) hole of a through-hole or non-plated pad: the hole's size along the pad's
    /// X and Y axes (before the pad rotation). The kind's `drill` is the smaller of the two (the
    /// slot width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<(Nm, Nm)>,
    /// Local settings of this pad, overriding the footprint's and the board's.
    #[serde(flatten)]
    pub overrides: Overrides,
}

impl Pad {
    /// A pad on the footprint's side, without paste setting, slot or overrides.
    pub fn new(number: impl Into<String>, at: Point, shape: PadShape, kind: PadKind) -> Pad {
        Pad {
            number: number.into(),
            at,
            rotation: Angle::ZERO,
            shape,
            kind,
            paste: None,
            back: false,
            mask: Default::default(),
            slot: None,
            overrides: Overrides::default(),
        }
    }

    /// Whether the pad gets solder paste: SMD pads unless `paste` is `none`, through-hole pads
    /// only with `paste: pad` (or windows); non-plated holes never.
    pub fn has_paste(&self) -> bool {
        match self.kind {
            PadKind::Smd => self.paste != Some(Paste::None),
            PadKind::Tht { .. } => matches!(self.paste, Some(Paste::Pad | Paste::Windows { .. })),
            PadKind::Npth { .. } => false,
        }
    }

    /// The hole's size along the pad's X and Y axes (before rotation): the slot, else
    /// `(drill, drill)`. `None` for SMD pads.
    pub fn hole_size(&self) -> Option<(Nm, Nm)> {
        match self.kind {
            PadKind::Tht { drill } | PadKind::Npth { drill } => Some(self.slot.unwrap_or((drill, drill))),
            PadKind::Smd => None,
        }
    }
}

/// Where a pad gets solder mask openings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaskOpening {
    /// On every outer side its copper reaches: an SMD pad's side, both sides of a hole.
    #[default]
    Pad,
    /// Only on the footprint's side (pads with a hole).
    Front,
    /// Only on the other side of the footprint (pads with a hole).
    Back,
    /// None: the pad stays covered by solder mask.
    None,
}

impl MaskOpening {
    /// Whether this is the default (`pad`).
    pub fn is_default(&self) -> bool {
        *self == MaskOpening::Pad
    }
}

/// Settings of a pad (or, as defaults for its pads, of a footprint) that override the board's:
/// mask and paste margins, clearance, zone connection. Unset values fall back to the
/// footprint's, then to the board's (`board.rules`, the net class, the zone).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Overrides {
    /// Solder mask opening growth beyond the copper on every side (negative shrinks it),
    /// instead of the board's `mask_expansion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_margin: Option<Nm>,
    /// Solder paste opening growth beyond the copper on every side (usually negative), instead
    /// of the board's `paste_margin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste_margin: Option<Nm>,
    /// Solder paste growth as a fraction of the pad size along each axis (`-0.05` shrinks a
    /// 1 mm side by 0.05 mm at both ends), added to the margin, instead of the board's
    /// `paste_ratio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste_ratio: Option<Scale>,
    /// Copper clearance of this pad to other nets, instead of the net (class) clearance: when
    /// either item has a local clearance, the DRC and zone fills use the larger local value of
    /// the two (never below `board.rules` `min_clearance`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// How zones of the pad's net connect to it, instead of the zone's own pad connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_connection: Option<PadConnection>,
}

impl Overrides {
    /// Whether nothing is overridden.
    pub fn is_empty(&self) -> bool {
        *self == Overrides::default()
    }

    /// These settings, with unset values taken from `fallback`.
    pub fn or(&self, fallback: &Overrides) -> Overrides {
        Overrides {
            mask_margin: self.mask_margin.or(fallback.mask_margin),
            paste_margin: self.paste_margin.or(fallback.paste_margin),
            paste_ratio: self.paste_ratio.or(fallback.paste_ratio),
            clearance: self.clearance.or(fallback.clearance),
            zone_connection: self.zone_connection.or(fallback.zone_connection),
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero_angle(a: &Angle) -> bool {
    *a == Angle::ZERO
}

/// Layer a footprint graphic is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphicLayer {
    /// Silkscreen.
    Silk,
    /// Fabrication drawing (body outline).
    Fab,
    /// Courtyard (keep-out envelope for placement).
    Courtyard,
    /// Copper on the outer layer of the footprint's side (the other side with `back`): logos,
    /// net-tie bridges. Copper for the DRC, zone fills and Gerber output, on the net of the
    /// first pad it touches (no net when it touches none).
    Copper,
    /// Solder mask opening (the footprint's side, or the other one with `back`).
    Mask,
    /// Solder paste opening (the footprint's side, or the other one with `back`).
    Paste,
}

impl GraphicLayer {
    /// The front-side board layer name of this footprint layer (`F.SilkS`, `F.Cu`, ...).
    pub fn front_name(self) -> &'static str {
        match self {
            GraphicLayer::Silk => "F.SilkS",
            GraphicLayer::Fab => "F.Fab",
            GraphicLayer::Courtyard => "F.CrtYd",
            GraphicLayer::Copper => "F.Cu",
            GraphicLayer::Mask => "F.Mask",
            GraphicLayer::Paste => "F.Paste",
        }
    }
}

/// A line drawing on a footprint layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Graphic {
    /// Layer.
    pub layer: GraphicLayer,
    /// Stroke width.
    pub width: Nm,
    /// Polyline vertices.
    #[serde(flatten)]
    pub geometry: GraphicGeometry,
    /// On the other side of the footprint (copper, mask and paste drawings only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub back: bool,
}

impl Graphic {
    /// A drawing on the footprint's side.
    pub fn new(layer: GraphicLayer, width: Nm, geometry: GraphicGeometry) -> Graphic {
        Graphic { layer, width, geometry, back: false }
    }
}

/// Graphic geometry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GraphicGeometry {
    /// Open polyline.
    Path {
        /// Vertices.
        points: Vec<Point>,
    },
    /// Closed polygon outline (filled on copper, mask and paste layers).
    Polygon {
        /// Vertices.
        points: Vec<Point>,
    },
    /// Circle.
    Circle {
        /// Center.
        center: Point,
        /// Radius.
        radius: Nm,
        /// Filled.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        filled: bool,
    },
}

/// Package body, for the fab drawing, placement checks and 3D previews.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Body {
    /// Body size along X.
    pub width: Nm,
    /// Body size along Y.
    pub length: Nm,
    /// Maximum height above the board.
    pub height: Nm,
}

/// Whether a footprint is surface-mount or through-hole, for assembly files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mount {
    /// Surface mount.
    Smd,
    /// Through hole.
    Tht,
}

/// A land pattern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Footprint {
    /// Library-unique name; IPC-7351 name for generated footprints (`RESC1005X40N`).
    pub name: String,
    /// Description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Mounting technology.
    pub mount: Mount,
    /// Pads.
    pub pads: Vec<Pad>,
    /// Courtyard outline (closed).
    pub courtyard: Vec<Point>,
    /// Silkscreen, fab and other drawings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphics: Vec<Graphic>,
    /// Package body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    /// Generator spec that produced it (a `PackageSpec`), for regeneration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<serde_json::Value>,
    /// 3D model for renders and MCAD exports, instead of a body generated from the package
    /// dimensions (`footprint.model_set`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::model::model3d::Model3d>,
    /// Where it came from, for footprints imported from library files
    /// (`footprint.import_kicad`): the source file and the license the user gave. Absent for
    /// generated footprints and footprints taken from boards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::model::part::Provenance>,
    /// Defaults for its pads' local settings (mask and paste margins, clearance, zone
    /// connection); a pad's own values win.
    #[serde(default, skip_serializing_if = "Overrides::is_empty")]
    pub overrides: Overrides,
    /// Net ties: groups of pad numbers whose nets this footprint joins on purpose (net-tie
    /// parts, Kelvin sense resistors). Its own copper (pads, copper drawings) joining nets of
    /// one group is not a short, and the nets stay distinct for connectivity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net_ties: Vec<Vec<String>>,
}

impl Footprint {
    /// The settings of one of its pads: the pad's own, else the footprint's.
    pub fn pad_overrides(&self, pad: &Pad) -> Overrides {
        pad.overrides.or(&self.overrides)
    }

    /// The net-tie group holding pad `number`, if any.
    pub fn tie_group(&self, number: &str) -> Option<usize> {
        if number.is_empty() {
            return None;
        }
        self.net_ties.iter().position(|g| g.iter().any(|n| n == number))
    }

    /// Pad numbers, in order, deduplicated (exposed pads may repeat a number).
    pub fn pad_numbers(&self) -> Vec<&str> {
        let mut seen = Vec::new();
        for p in &self.pads {
            if !p.number.is_empty() && !seen.contains(&p.number.as_str()) {
                seen.push(p.number.as_str());
            }
        }
        seen
    }
}
