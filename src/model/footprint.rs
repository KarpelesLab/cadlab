//! Footprints (land patterns): pads, courtyard, silkscreen and fabrication outlines.
//!
//! Coordinates are footprint-local, Y up, origin at the package center, in IPC-7351 zero
//! orientation (pin 1 top-left for dual and quad packages, pin 1 left for two-terminal parts).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::Point;
use crate::units::{Angle, Nm};

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

/// Solder paste for a pad, when it differs from an opening equal to the pad.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Paste {
    /// No paste.
    None,
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
    /// Solder paste; absent means an opening equal to the pad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste: Option<Paste>,
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
    /// Closed polygon outline.
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
}

impl Footprint {
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
