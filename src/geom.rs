//! Geometry primitives in nanometer coordinates.
//!
//! This module is the single boundary between cadlab and the polygon library (`polyclip`).
//! The rest of cadlab uses the types here; polygon operations go through [`poly`], which
//! re-exports `polyclip`. Its integer coordinates are nanometers.

use std::borrow::Cow;
use std::ops::{Add, Neg, Sub};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::units::{Angle, Nm};

/// Polygon geometry (booleans, offsets, arcs, queries). Coordinates are nanometers.
pub use polyclip as poly;

/// A point (or vector) in board or schematic space. Y points up.
///
/// Serialized as a two-element array of lengths: `["12.7mm", "8.4mm"]`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(from = "(Nm, Nm)", into = "(Nm, Nm)")]
pub struct Point {
    /// X coordinate.
    pub x: Nm,
    /// Y coordinate.
    pub y: Nm,
}

impl Point {
    /// The origin.
    pub const ORIGIN: Point = Point { x: Nm(0), y: Nm(0) };

    /// Creates a point.
    pub const fn new(x: Nm, y: Nm) -> Self {
        Point { x, y }
    }

    /// Rotates around the origin. Exact for multiples of 90°, rounded to the nearest nanometer
    /// otherwise.
    pub fn rotated(self, a: Angle) -> Point {
        let (x, y) = (self.x.0, self.y.0);
        match a.quarter_turns() {
            Some(0) => self,
            Some(1) => Point::new(Nm(-y), Nm(x)),
            Some(2) => Point::new(Nm(-x), Nm(-y)),
            Some(3) => Point::new(Nm(y), Nm(-x)),
            _ => {
                let (s, c) = a.to_rad_f64().sin_cos();
                let (xf, yf) = (x as f64, y as f64);
                Point::new(
                    Nm((xf * c - yf * s).round() as i64),
                    Nm((xf * s + yf * c).round() as i64),
                )
            }
        }
    }
}

impl From<(Nm, Nm)> for Point {
    fn from((x, y): (Nm, Nm)) -> Self {
        Point { x, y }
    }
}

impl From<Point> for (Nm, Nm) {
    fn from(p: Point) -> Self {
        (p.x, p.y)
    }
}

impl From<Point> for poly::Point {
    fn from(p: Point) -> Self {
        poly::Point::new(p.x.0, p.y.0)
    }
}

impl From<poly::Point> for Point {
    fn from(p: poly::Point) -> Self {
        Point::new(Nm(p.x), Nm(p.y))
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

impl Neg for Point {
    type Output = Point;
    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}

impl JsonSchema for Point {
    fn schema_name() -> Cow<'static, str> {
        "Point".into()
    }

    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        let len = g.subschema_for::<Nm>();
        json_schema!({
            "type": "array",
            "description": "[x, y] with units, Y up. Example: [\"12.7mm\", \"8.4mm\"].",
            "prefixItems": [len, len],
            "minItems": 2,
            "maxItems": 2
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// Axis-aligned bounding box (inclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct BBox {
    /// Lower-left corner.
    pub min: Point,
    /// Upper-right corner.
    pub max: Point,
}

impl BBox {
    /// Box spanning two corners in any order.
    pub fn new(a: Point, b: Point) -> Self {
        BBox {
            min: Point::new(a.x.min(b.x), a.y.min(b.y)),
            max: Point::new(a.x.max(b.x), a.y.max(b.y)),
        }
    }

    /// Smallest box containing all points, or `None` if there are none.
    pub fn of_points<I: IntoIterator<Item = Point>>(pts: I) -> Option<BBox> {
        let mut it = pts.into_iter();
        let first = it.next()?;
        let mut b = BBox {
            min: first,
            max: first,
        };
        for p in it {
            b.add_point(p);
        }
        Some(b)
    }

    /// Grows the box to include `p`.
    pub fn add_point(&mut self, p: Point) {
        self.min = Point::new(self.min.x.min(p.x), self.min.y.min(p.y));
        self.max = Point::new(self.max.x.max(p.x), self.max.y.max(p.y));
    }

    /// Width.
    pub fn width(&self) -> Nm {
        self.max.x - self.min.x
    }

    /// Height.
    pub fn height(&self) -> Nm {
        self.max.y - self.min.y
    }

    /// Whether `p` is inside or on the border.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }
}

/// Placement transform: optional mirror across the Y axis (for bottom-side parts), then
/// rotation around the origin, then translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Transform {
    /// Translation applied last.
    pub offset: Point,
    /// Counter-clockwise rotation.
    pub rotation: Angle,
    /// Mirror X (x → -x) before rotating.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
}

impl Transform {
    /// Identity.
    pub const IDENTITY: Transform = Transform {
        offset: Point::ORIGIN,
        rotation: Angle::ZERO,
        mirror: false,
    };

    /// Applies the transform to a point.
    pub fn apply(&self, p: Point) -> Point {
        let p = if self.mirror {
            Point::new(-p.x, p.y)
        } else {
            p
        };
        p.rotated(self.rotation) + self.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::mm;

    #[test]
    fn rotation_exact_on_quarters() {
        let p = Point::new(mm(1), mm(2));
        assert_eq!(p.rotated(Angle::DEG_90), Point::new(mm(-2), mm(1)));
        assert_eq!(p.rotated(Angle::DEG_180), Point::new(mm(-1), mm(-2)));
        assert_eq!(p.rotated(Angle::from_deg(-90)), Point::new(mm(2), mm(-1)));
        let q = Point::new(mm(1), Nm(0)).rotated(Angle::from_deg(45));
        assert_eq!(q, Point::new(Nm(707_107), Nm(707_107)));
    }

    #[test]
    fn transform_order() {
        let t = Transform {
            offset: Point::new(mm(10), mm(0)),
            rotation: Angle::DEG_90,
            mirror: true,
        };
        // mirror (1,0) -> (-1,0); rotate 90 -> (0,-1); translate -> (10,-1)
        assert_eq!(
            t.apply(Point::new(mm(1), Nm(0))),
            Point::new(mm(10), mm(-1))
        );
    }

    #[test]
    fn point_serde() {
        let p = Point::new(Nm(12_700_000), Nm(8_400_000));
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, r#"["12.7mm","8.4mm"]"#);
        assert_eq!(
            serde_json::from_str::<Point>(r#"["0.5in", "100mil"]"#).unwrap(),
            Point::new(Nm(12_700_000), Nm(2_540_000))
        );
    }

    #[test]
    fn polyclip_roundtrip() {
        let p = Point::new(Nm(3), Nm(-4));
        let q: poly::Point = p.into();
        assert_eq!(Point::from(q), p);
    }
}
