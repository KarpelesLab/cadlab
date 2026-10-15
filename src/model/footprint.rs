//! Footprints (land patterns): pads, courtyard, silkscreen and fabrication outlines.
//!
//! Coordinates are footprint-local, Y up, origin at the package center, in IPC-7351 zero
//! orientation (pin 1 top-left for dual and quad packages, pin 1 left for two-terminal parts).

use std::collections::BTreeMap;

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
    /// Trapezoid (a quadrilateral symmetric about the pad center): a `w`×`h` rectangle whose
    /// left edge is `dx` longer and right edge `dx` shorter (half at each end), and whose bottom
    /// (−Y) edge is `dy` wider and top edge `dy` narrower. Usually one delta is zero.
    Trapezoid {
        /// Width (X) at mid-height.
        w: Nm,
        /// Height (Y) at mid-width.
        h: Nm,
        /// Left edge length minus `h` (the right edge is `h − dx` long).
        dx: Nm,
        /// Bottom edge length minus `w` (the top edge is `w − dy` long).
        dy: Nm,
    },
    /// Rectangle with some corners cut at 45° by `chamfer` along each edge, the other corners
    /// rounded by `r` (0: sharp).
    ChamferedRect {
        /// Width (X).
        w: Nm,
        /// Height (Y).
        h: Nm,
        /// Radius of the corners that are not chamfered.
        #[serde(default, skip_serializing_if = "is_zero_nm")]
        r: Nm,
        /// Length cut off each edge at a chamfered corner (at most half the shorter side).
        chamfer: Nm,
        /// Chamfered corners (pad orientation, Y up: `top_left` is at −X, +Y).
        corners: Vec<Corner>,
    },
}

/// A corner of a rectangular pad, before the pad rotation (Y up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    /// −X, +Y.
    TopLeft,
    /// +X, +Y.
    TopRight,
    /// −X, −Y.
    BottomLeft,
    /// +X, −Y.
    BottomRight,
}

impl Corner {
    /// The corner on the other side of the Y axis (mirrored in X).
    pub fn mirrored_x(self) -> Corner {
        match self {
            Corner::TopLeft => Corner::TopRight,
            Corner::TopRight => Corner::TopLeft,
            Corner::BottomLeft => Corner::BottomRight,
            Corner::BottomRight => Corner::BottomLeft,
        }
    }
}

/// A vertex of a pad's straight-edged outline ([`PadShape::outline_path`]): its position and,
/// when the edge reaching it is a counter-clockwise arc, the arc's center.
pub type OutlineVertex = (Point, Option<Point>);

fn is_zero_nm(v: &Nm) -> bool {
    v.0 == 0
}

impl PadShape {
    /// Width and height of the bounding box (before pad rotation), centered on the pad center
    /// (a polygon off center gets the box around it that is symmetric about the center).
    pub fn size(&self) -> (Nm, Nm) {
        match *self {
            PadShape::Rect { w, h }
            | PadShape::RoundRect { w, h, .. }
            | PadShape::Oval { w, h }
            | PadShape::ChamferedRect { w, h, .. } => (w, h),
            PadShape::Trapezoid { w, h, dx, dy } => (w + dy.abs(), h + dx.abs()),
            PadShape::Circle { d } => (d, d),
            PadShape::Polygon { ref points } => {
                let ex = points.iter().map(|q| q.x.0.abs()).max().unwrap_or(0);
                let ey = points.iter().map(|q| q.y.0.abs()).max().unwrap_or(0);
                (Nm(2 * ex), Nm(2 * ey))
            }
        }
    }

    /// The shape mirrored across its Y axis (X negated): what a pad of a bottom-side footprint
    /// looks like from the top. Only trapezoids, chamfered rectangles and polygons change.
    pub fn mirrored_x(&self) -> PadShape {
        match self {
            PadShape::Trapezoid { w, h, dx, dy } => PadShape::Trapezoid { w: *w, h: *h, dx: -*dx, dy: *dy },
            PadShape::ChamferedRect { w, h, r, chamfer, corners } => {
                let mut corners: Vec<Corner> = corners.iter().map(|c| c.mirrored_x()).collect();
                corners.sort();
                PadShape::ChamferedRect { w: *w, h: *h, r: *r, chamfer: *chamfer, corners }
            }
            PadShape::Polygon { points } => {
                PadShape::Polygon { points: points.iter().rev().map(|q| Point::new(-q.x, q.y)).collect() }
            }
            s => s.clone(),
        }
    }

    /// The outline of a trapezoid or chamfered rectangle, counter-clockwise, relative to the pad
    /// center before the pad rotation: straight edges between vertices, except that a rounded
    /// corner is an arc reaching its second tangent point (that vertex carries the arc center).
    /// `None` for the other shapes.
    pub fn outline_path(&self) -> Option<Vec<OutlineVertex>> {
        match *self {
            PadShape::Trapezoid { w, h, dx, dy } => {
                let p = |x: i64, y: i64| (Point::new(Nm(x), Nm(y)), None);
                let (w, h, dx, dy) = (w.0, h.0, dx.0, dy.0);
                Some(vec![
                    p(-(w + dy) / 2, -(h + dx) / 2),
                    p((w + dy) / 2, -(h - dx) / 2),
                    p((w - dy) / 2, (h - dx) / 2),
                    p(-(w - dy) / 2, (h + dx) / 2),
                ])
            }
            PadShape::ChamferedRect { w, h, r, chamfer, ref corners } => {
                let (hw, hh) = (w.0 / 2, h.0 / 2);
                let half = hw.min(hh);
                let (r, c) = (r.0.clamp(0, half), chamfer.0.clamp(0, half));
                let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
                let mut out: Vec<OutlineVertex> = Vec::new();
                // Corner, its position, direction of the edge reaching it, of the edge leaving it.
                let order = [
                    (Corner::BottomRight, (hw, -hh), (1, 0), (0, 1)),
                    (Corner::TopRight, (hw, hh), (0, 1), (-1, 0)),
                    (Corner::TopLeft, (-hw, hh), (-1, 0), (0, -1)),
                    (Corner::BottomLeft, (-hw, -hh), (0, -1), (1, 0)),
                ];
                for (corner, (x, y), (ix, iy), (ox, oy)) in order {
                    let chamfered = corners.contains(&corner);
                    let cut = if chamfered { c } else { r };
                    if cut == 0 {
                        out.push((p(x, y), None));
                        continue;
                    }
                    let center = (!chamfered).then(|| p(x - cut * ix + cut * ox, y - cut * iy + cut * oy));
                    out.push((p(x - cut * ix, y - cut * iy), None));
                    out.push((p(x + cut * ox, y + cut * oy), center));
                }
                // Cuts meeting mid-edge leave repeated vertices: keep the one an arc reaches.
                let mut v: Vec<OutlineVertex> = Vec::with_capacity(out.len());
                for q in out {
                    match v.last_mut() {
                        Some(last) if last.0 == q.0 => last.1 = last.1.or(q.1),
                        _ => v.push(q),
                    }
                }
                if v.len() > 1 && v[0].0 == v[v.len() - 1].0 {
                    let last = v.pop().expect("non-empty");
                    v[0].1 = v[0].1.or(last.1);
                }
                Some(v)
            }
            _ => None,
        }
    }

    /// The outline of a trapezoid or chamfered rectangle as a polygon (rounded corners as
    /// chords at most `tol` inside the arc, from [`PadShape::outline_path`]); `None` for the other
    /// shapes.
    pub fn outline_points(&self, tol: Nm) -> Option<Vec<Point>> {
        let path = self.outline_path()?;
        let mut out: Vec<Point> = Vec::new();
        let mut prev = path.last().map(|v| v.0)?;
        for &(q, center) in &path {
            if let Some(c) = center {
                let (a, b) = (prev - c, q - c);
                let r = (a.x.0 as f64).hypot(a.y.0 as f64);
                let a0 = (a.y.0 as f64).atan2(a.x.0 as f64);
                let mut sweep = (b.y.0 as f64).atan2(b.x.0 as f64) - a0;
                if sweep <= 0.0 {
                    sweep += std::f64::consts::TAU;
                }
                let step = if r > tol.0 as f64 { 2.0 * (1.0 - tol.0 as f64 / r).acos() } else { sweep };
                let n = ((sweep / step).ceil() as usize).clamp(1, 64);
                for i in 1..n {
                    let t = a0 + sweep * i as f64 / n as f64;
                    out.push(c + Point::new(Nm((r * t.cos()).round() as i64), Nm((r * t.sin()).round() as i64)));
                }
            }
            out.push(q);
            prev = q;
        }
        Some(out)
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
    /// Where the hole is relative to the pad center (before the pad rotation), when the copper
    /// is not centered on it (KiCad's drill offset). Through-hole and non-plated pads only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drill_offset: Option<Point>,
    /// Per-layer copper shapes of a through-hole pad (a pad stack): its shape on inner layers
    /// and on the other side of the footprint when they differ from `shape`, which is the
    /// shape on the footprint's side. Absent: `shape` on every layer. SMD and non-plated pads
    /// ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<PadStack>,
    /// Local settings of this pad, overriding the footprint's and the board's.
    #[serde(flatten)]
    pub overrides: Overrides,
}

/// A copper layer of a pad, relative to its footprint: the footprint's side, the other side, or
/// an inner layer counted from the footprint's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StackLayer {
    /// The outer layer on the footprint's side.
    Front,
    /// An inner layer: 1 is the one nearest the footprint's side.
    Inner(u32),
    /// The outer layer on the other side of the footprint.
    Back,
}

/// Copper shapes of a through-hole pad on layers other than the footprint's side (KiCad 9 pad
/// stacks: `front_inner_back` sets `inner` and `back`, `custom` lists inner layers one by one).
/// A shape left out is the pad's own `shape`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PadStack {
    /// Shape on the inner copper layers (those not in `layers`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner: Option<PadShape>,
    /// Shape on the outer layer on the other side of the footprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back: Option<PadShape>,
    /// Shapes of single inner layers, by number counted from the footprint's side (1: the inner
    /// layer nearest it; `In1.Cu` on a top-side footprint, the deepest inner layer on a
    /// bottom-side one).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layers: BTreeMap<u32, PadShape>,
}

impl PadStack {
    /// Whether no layer differs from the pad's own shape.
    pub fn is_empty(&self) -> bool {
        self.inner.is_none() && self.back.is_none() && self.layers.is_empty()
    }
}

impl Pad {
    /// The copper shape on a layer: `shape` on the footprint's side, else the pad stack's
    /// shape for that layer when it has one. SMD and non-plated pads have one shape.
    pub fn shape_on(&self, layer: StackLayer) -> &PadShape {
        let Some(st) = self.stack.as_ref().filter(|_| matches!(self.kind, PadKind::Tht { .. })) else {
            return &self.shape;
        };
        match layer {
            StackLayer::Front => &self.shape,
            StackLayer::Back => st.back.as_ref().unwrap_or(&self.shape),
            StackLayer::Inner(k) => st.layers.get(&k).or(st.inner.as_ref()).unwrap_or(&self.shape),
        }
    }

    /// Whether the copper differs between layers (a non-empty pad stack on a through-hole
    /// pad).
    pub fn has_stack(&self) -> bool {
        matches!(self.kind, PadKind::Tht { .. }) && self.stack.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// Every copper shape of the pad: `shape`, then the pad stack's (back, inner, single inner
    /// layers), repeats included.
    pub fn shapes(&self) -> Vec<&PadShape> {
        let mut v = vec![&self.shape];
        if self.has_stack()
            && let Some(st) = &self.stack
        {
            v.extend(st.back.iter().chain(st.inner.iter()).chain(st.layers.values()));
        }
        v
    }

    /// Width and height of the box around every copper shape of the pad, centered on the pad
    /// center, before the pad rotation ([`PadShape::size`] of `shape` without a pad stack).
    pub fn size(&self) -> (Nm, Nm) {
        self.shapes().iter().map(|s| s.size()).fold((Nm::ZERO, Nm::ZERO), |(w, h), (a, b)| (w.max(a), h.max(b)))
    }
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
            drill_offset: None,
            stack: None,
            overrides: Overrides::default(),
        }
    }

    /// The hole center in footprint coordinates: the pad center moved by the drill offset.
    pub fn hole_center(&self) -> Point {
        match self.drill_offset {
            Some(o) => self.at + o.rotated(self.rotation),
            None => self.at,
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
    /// Thermal relief gap between the pad and the zone, instead of the zone's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_gap: Option<Nm>,
    /// Thermal relief spoke width, instead of the zone's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_spoke: Option<Nm>,
    /// Direction of the first thermal spoke relative to the pad's orientation, instead of the
    /// zone's (default: 45° for round pads, 90° for others).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_angle: Option<Angle>,
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
            thermal_gap: self.thermal_gap.or(fallback.thermal_gap),
            thermal_spoke: self.thermal_spoke.or(fallback.thermal_spoke),
            thermal_angle: self.thermal_angle.or(fallback.thermal_angle),
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
    /// Center of the body in footprint coordinates (default: the origin). Footprints whose
    /// origin is not the body center (connectors drawn from a datasheet origin) set it.
    #[serde(default, skip_serializing_if = "Point::is_origin")]
    pub at: Point,
}

impl Body {
    /// A body of `width` x `length` x `height` centered on the origin.
    pub fn centered(width: Nm, length: Nm, height: Nm) -> Body {
        Body { width, length, height, at: Point::ORIGIN }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    #[test]
    fn trapezoid_and_chamfered_outlines() {
        let t = PadShape::Trapezoid { w: Nm(1_000), h: Nm(600), dx: Nm(200), dy: Nm(0) };
        assert_eq!(t.size(), (Nm(1_000), Nm(800)));
        let pts: Vec<Point> = t.outline_path().unwrap().into_iter().map(|v| v.0).collect();
        assert_eq!(pts, vec![p(-500, -400), p(500, -200), p(500, 200), p(-500, 400)]);
        assert_eq!(t.mirrored_x(), PadShape::Trapezoid { w: Nm(1_000), h: Nm(600), dx: Nm(-200), dy: Nm(0) });
        // A square chamfered by half its side on two adjacent corners: the cuts meet mid-edge.
        let c = PadShape::ChamferedRect {
            w: Nm(1_000),
            h: Nm(1_000),
            r: Nm(0),
            chamfer: Nm(500),
            corners: vec![Corner::TopLeft, Corner::TopRight],
        };
        let pts: Vec<Point> = c.outline_path().unwrap().into_iter().map(|v| v.0).collect();
        assert_eq!(pts, vec![p(500, -500), p(500, 0), p(0, 500), p(-500, 0), p(-500, -500)]);
        // Rounded corners carry their arc centers.
        let r = PadShape::ChamferedRect { w: Nm(1_000), h: Nm(800), r: Nm(100), chamfer: Nm(200), corners: vec![] };
        let path = r.outline_path().unwrap();
        assert_eq!(path.iter().filter(|v| v.1.is_some()).count(), 4);
        assert_eq!(path[1], (p(500, -300), Some(p(400, -300))));
        let poly = r.outline_points(Nm(1)).unwrap();
        assert!(poly.len() > 20 && poly.iter().all(|q| q.x.0.abs() <= 500 && q.y.0.abs() <= 400));
    }

    #[test]
    fn new_pad_fields_serialize() {
        let mut pad = Pad::new(
            "1",
            p(0, 0),
            PadShape::ChamferedRect {
                w: Nm(1_000),
                h: Nm(800),
                r: Nm(0),
                chamfer: Nm(200),
                corners: vec![Corner::BottomLeft],
            },
            PadKind::Tht { drill: Nm(300) },
        );
        pad.drill_offset = Some(p(100, 0));
        let json = serde_json::to_string(&pad).unwrap();
        assert!(json.contains(r#""shape":"chamfered_rect""#) && !json.contains(r#""r":"#), "{json}");
        assert!(json.contains(r#""corners":["bottom_left"]"#), "{json}");
        let back: Pad = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pad);
        assert_eq!(back.hole_center(), p(100, 0));
    }
}
