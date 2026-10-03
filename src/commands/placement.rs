//! Board holes and outline cutouts (`board.hole`, `board.cutout`, ...) and placement helpers
//! (`place.near`, `place.align`, `place.distribute`).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::board::{Placed, check_placeable, parse_item, placed_text, unlocked};
use super::util;
use super::zone::RectSpec;
use crate::board::{self as geo, COPPER_TOL, place};
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{Contour, Hole, Segment};
use crate::model::sections::natural_cmp;
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<HoleAdd>()
        .register::<HoleRemove>()
        .register::<CutoutAdd>()
        .register::<CutoutRemove>()
        .register::<PlaceNear>()
        .register::<PlaceAlign>()
        .register::<PlaceDistribute>();
}

// ---- holes ----

/// Board holes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Holes {
    /// Every hole on the board.
    pub holes: Vec<Hole>,
}

fn holes_text(o: &Holes) -> String {
    if o.holes.is_empty() {
        return "no holes".into();
    }
    o.holes
        .iter()
        .map(|h| {
            let kind = match h.pad {
                Some(d) => {
                    format!("plated, pad {d}{}", h.net.as_ref().map(|n| format!(", net {n}")).unwrap_or_default())
                }
                None => "non-plated".into(),
            };
            format!("{} (hole#{}): ({}, {}) drill {}, {kind}", h.name, h.id.0, h.at.x, h.at.y, h.drill)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Add a board hole (mounting hole): non-plated, or plated with a round pad (`pad`) that can
/// be on a net. DRC, zones, rendering, Gerber/drill and KiCad export treat it like a pad.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HoleAdd {
    /// Center: ["3.5mm", "3.5mm"].
    pub at: Point,
    /// Finished hole diameter ("3.2mm" for M3).
    pub drill: Nm,
    /// Plated pad diameter (makes a plated hole); omit for a non-plated hole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad: Option<Nm>,
    /// Net of the plated pad (usually GND).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Name (default H1, H2, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Command for HoleAdd {
    const NAME: &'static str = "board.hole";
    const SUMMARY: &'static str = "Add a mounting hole: non-plated, or plated with a pad (optionally on a net)";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Holes;

    fn run(self, ctx: &mut Context<'_>) -> Result<Holes, CommandError> {
        let p = ctx.project()?;
        if self.drill <= Nm::ZERO {
            return Err(CommandError::invalid_args("board.invalid_hole", "the drill must be positive"));
        }
        if let Some(d) = self.pad
            && d <= self.drill
        {
            return Err(CommandError::invalid_args("board.invalid_hole", "the pad must be larger than the drill")
                .with_hint("give a pad diameter above the drill, or omit it for a non-plated hole"));
        }
        let net = match &self.net {
            Some(n) if self.pad.is_none() => {
                return Err(CommandError::invalid_args(
                    "board.invalid_hole",
                    format!("a non-plated hole cannot be on net {n}"),
                )
                .with_hint("give a plated pad diameter (`pad`)"));
            }
            Some(n) => Some(super::net::net_name(p, n)?),
            None => None,
        };
        let taken = |n: &str| {
            p.board().holes.iter().any(|h| h.name.eq_ignore_ascii_case(n))
                || p.circuit().components.keys().any(|r| r.eq_ignore_ascii_case(n))
                || p.board().footprints.keys().any(|r| r.eq_ignore_ascii_case(n))
        };
        let name = match self.name {
            Some(n) => {
                if n.is_empty() || n.contains(['.', '#', ' ']) {
                    return Err(CommandError::invalid_args(
                        "board.invalid_hole",
                        format!("`{n}` is not a valid hole name"),
                    )
                    .with_hint("use letters and digits, like H1"));
                }
                if taken(&n) {
                    return Err(CommandError::conflict("board.duplicate_hole", format!("`{n}` is already used"))
                        .with_hint("choose another name, or omit it for H1, H2, ..."));
                }
                n
            }
            None => (1..).map(|i| format!("H{i}")).find(|n| !taken(n)).expect("unbounded"),
        };
        let pm = ctx.project_mut()?;
        let id = pm.alloc_id();
        pm.board_mut().holes.push(Hole { id, name, at: self.at, drill: self.drill, pad: self.pad, net });
        Ok(Holes { holes: ctx.project()?.board().holes.clone() })
    }

    fn summarize(o: &Holes) -> String {
        holes_text(o)
    }
}

/// Remove board holes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HoleRemove {
    /// Holes: names ("H1") or IDs ("hole#12").
    pub holes: Vec<String>,
}

impl Command for HoleRemove {
    const NAME: &'static str = "board.hole_remove";
    const SUMMARY: &'static str = "Remove mounting holes by name or ID";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["holes"];
    type Output = Holes;

    fn run(self, ctx: &mut Context<'_>) -> Result<Holes, CommandError> {
        let mut ids = BTreeSet::new();
        let p = ctx.project()?;
        for h in &self.holes {
            let found = if h.starts_with("hole#") {
                let id = parse_item(h, "hole")?;
                p.board().holes.iter().find(|x| x.id == id)
            } else {
                p.board().holes.iter().find(|x| x.name.eq_ignore_ascii_case(h))
            };
            let Some(found) = found else {
                let names: Vec<&str> = p.board().holes.iter().map(|x| x.name.as_str()).collect();
                return Err(CommandError::not_found("board.hole_not_found", format!("no hole `{h}`")).with_hint(
                    if names.is_empty() {
                        "the board has no holes".to_string()
                    } else {
                        format!("holes: {}", names.join(", "))
                    },
                ));
            };
            ids.insert(found.id);
        }
        ctx.project_mut()?.board_mut().holes.retain(|h| !ids.contains(&h.id));
        Ok(Holes { holes: ctx.project()?.board().holes.clone() })
    }

    fn summarize(o: &Holes) -> String {
        holes_text(o)
    }
}

// ---- cutouts ----

/// A circle.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CircleSpec {
    /// Center.
    pub center: Point,
    /// Diameter.
    pub diameter: Nm,
}

/// A cutout of the outline.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CutoutInfo {
    /// Number, from 1 (for `board.cutout_remove`).
    pub index: usize,
    /// Lower-left corner of its bounding box.
    pub min: Point,
    /// Upper-right corner of its bounding box.
    pub max: Point,
}

/// Outline cutouts.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Cutouts {
    /// Every cutout.
    pub cutouts: Vec<CutoutInfo>,
}

fn cutouts(p: &Project) -> Cutouts {
    let list = p
        .board()
        .outline
        .contours
        .iter()
        .skip(1)
        .enumerate()
        .filter_map(|(i, c)| {
            let r = place::Rect::of(geo::contour_ring(c, COPPER_TOL).into_iter().map(Point::from))?;
            Some(CutoutInfo { index: i + 1, min: Point::new(Nm(r.x0), Nm(r.y0)), max: Point::new(Nm(r.x1), Nm(r.y1)) })
        })
        .collect();
    Cutouts { cutouts: list }
}

fn cutouts_text(o: &Cutouts) -> String {
    if o.cutouts.is_empty() {
        return "no cutouts".into();
    }
    o.cutouts
        .iter()
        .map(|c| format!("cutout {}: ({}, {}) .. ({}, {})", c.index, c.min.x, c.min.y, c.max.x, c.max.y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Cut an opening into the board: a rectangle, a circle or a polygon, inside the outline and
/// clear of other cutouts. It becomes an inner contour of the outline (Edge.Cuts, DRC,
/// zone fill and placement avoid it).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutoutAdd {
    /// Rectangle: {"from": ["5mm", "5mm"], "to": ["10mm", "8mm"]}.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<RectSpec>,
    /// Circle: {"center": ["20mm", "15mm"], "diameter": "6mm"}.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circle: Option<CircleSpec>,
    /// Polygon vertices (at least 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub polygon: Vec<Point>,
}

impl Command for CutoutAdd {
    const NAME: &'static str = "board.cutout";
    const SUMMARY: &'static str = "Cut an opening into the board outline (rect, circle or polygon)";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Cutouts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Cutouts, CommandError> {
        let invalid = |m: &str| {
            CommandError::invalid_args("board.invalid_cutout", m.to_string())
                .with_hint("give exactly one of rect {from, to}, circle {center, diameter} or polygon (3+ points)")
        };
        let contour = match (self.rect, self.circle, self.polygon.is_empty()) {
            (Some(r), None, true) => {
                let (x0, x1) = (r.from.x.min(r.to.x), r.from.x.max(r.to.x));
                let (y0, y1) = (r.from.y.min(r.to.y), r.from.y.max(r.to.y));
                if x0 == x1 || y0 == y1 {
                    return Err(invalid("the rectangle is empty"));
                }
                let p = Point::new;
                Contour {
                    start: p(x0, y0),
                    segments: vec![
                        Segment::Line { to: p(x1, y0) },
                        Segment::Line { to: p(x1, y1) },
                        Segment::Line { to: p(x0, y1) },
                        Segment::Line { to: p(x0, y0) },
                    ],
                }
            }
            (None, Some(c), true) => {
                if c.diameter <= Nm::ZERO {
                    return Err(invalid("the diameter must be positive"));
                }
                let (o, r) = (c.center, Nm(c.diameter.0 / 2));
                Contour {
                    start: Point::new(o.x + r, o.y),
                    segments: vec![
                        Segment::Arc { mid: Point::new(o.x, o.y + r), to: Point::new(o.x - r, o.y) },
                        Segment::Arc { mid: Point::new(o.x, o.y - r), to: Point::new(o.x + r, o.y) },
                    ],
                }
            }
            (None, None, false) if self.polygon.len() >= 3 => {
                let mut segments: Vec<Segment> = self.polygon[1..].iter().map(|&to| Segment::Line { to }).collect();
                segments.push(Segment::Line { to: self.polygon[0] });
                Contour { start: self.polygon[0], segments }
            }
            _ => return Err(invalid("give exactly one cutout shape")),
        };
        let p = ctx.project()?;
        let contours = &p.board().outline.contours;
        let Some(outer) = contours.first() else {
            return Err(CommandError::conflict("board.no_outline", "the board has no outline")
                .with_hint("set one with `board.outline` first"));
        };
        let ring: polyclip::Ring = geo::contour_ring(&contour, COPPER_TOL).into();
        let outer: polyclip::Ring = geo::contour_ring(outer, COPPER_TOL).into();
        if ring.len() < 3 || polyclip::ring_area2(&ring.0) == 0 {
            return Err(invalid("the cutout has no area"));
        }
        let outer_path = polyclip::Path::from(outer.clone());
        if !polyclip::contains(&outer, &ring) || polyclip::intersects(&outer_path, &ring) {
            return Err(CommandError::invalid_args(
                "board.invalid_cutout",
                "the cutout is not inside the board outline",
            )
            .with_hint("a cutout must lie strictly inside the outer edge; change the outline for notches"));
        }
        for (i, c) in contours.iter().enumerate().skip(1) {
            let other: polyclip::Ring = geo::contour_ring(c, COPPER_TOL).into();
            if polyclip::intersects(&other, &ring) {
                return Err(CommandError::conflict("board.invalid_cutout", format!("the cutout touches cutout {i}"))
                    .with_hint("remove the other cutout first (board.cutout_remove) or draw one combined polygon"));
            }
        }
        ctx.project_mut()?.board_mut().outline.contours.push(contour);
        Ok(cutouts(ctx.project()?))
    }

    fn summarize(o: &Cutouts) -> String {
        cutouts_text(o)
    }
}

/// Remove an outline cutout by number (see `board.cutout` output or `board.info`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CutoutRemove {
    /// Cutout number, from 1.
    pub index: usize,
}

impl Command for CutoutRemove {
    const NAME: &'static str = "board.cutout_remove";
    const SUMMARY: &'static str = "Remove an outline cutout by number";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["index"];
    type Output = Cutouts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Cutouts, CommandError> {
        let n = ctx.project()?.board().outline.contours.len().saturating_sub(1);
        if self.index == 0 || self.index > n {
            return Err(CommandError::not_found("board.cutout_not_found", format!("no cutout {}", self.index))
                .with_hint(if n == 0 {
                    "the board has no cutouts".to_string()
                } else {
                    format!("cutouts are 1..{n}")
                }));
        }
        ctx.project_mut()?.board_mut().outline.contours.remove(self.index);
        Ok(cutouts(ctx.project()?))
    }

    fn summarize(o: &Cutouts) -> String {
        cutouts_text(o)
    }
}

// ---- placement helpers ----

/// Place a part right next to a pin (`U1.VDD`) or another part: outside the target's
/// courtyard, its pad of the same net facing and aligned with the pin, without courtyard
/// overlap (it slides along the side, then away, until it fits).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceNear {
    /// Designator of the part to place.
    pub refdes: String,
    /// A pin ("U1.VDD", "U1.8") or a designator ("U1"); must be placed.
    pub target: String,
    /// Side of the target: left, right, above, below (default: the side the pin faces, or
    /// the best side for a part).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<place::Direction>,
    /// Gap between the courtyards (default 0.25mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<Nm>,
}

impl Command for PlaceNear {
    const NAME: &'static str = "place.near";
    const SUMMARY: &'static str = "Place a part next to a pin or part, facing it, without courtyard overlap";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes", "target"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let p = ctx.project()?;
        let r = check_placeable(p, &self.refdes)?;
        if p.board().footprints.get(&r).is_some_and(|f| f.locked) {
            return Err(CommandError::conflict("place.locked", format!("{r} is locked"))
                .with_hint(format!("unlock it: `place.lock {r} --locked false`")));
        }
        let target = if self.target.contains('.') {
            let pins = crate::connect::resolve_pins(p, &self.target)?;
            let pin = pins.first().expect("resolve returns at least one pin");
            let comp = &p.circuit().components[&pin.refdes];
            let pads = p.library().parts[&comp.part]
                .footprint()
                .map(|f| f.pads_for(&pin.pin))
                .unwrap_or_else(|| vec![pin.pin.clone()]);
            place::NearTarget::Pad { refdes: pin.refdes.clone(), pad: pads.into_iter().next().unwrap_or_default() }
        } else {
            place::NearTarget::Part(util::refdes_key(p, &self.target)?)
        };
        let distance = self.distance.unwrap_or(place::DEFAULT_GAP);
        if distance < Nm::ZERO {
            return Err(CommandError::invalid_args("place.invalid_distance", "the distance cannot be negative"));
        }
        let pf = place::place_near(p, &r, &target, self.side, distance)
            .map_err(|e| CommandError::conflict(e.code, e.message).with_hint(e.hint))?;
        ctx.project_mut()?.board_mut().footprints.insert(r.clone(), pf.clone());
        Ok(Placed { placements: [(r, pf)].into(), unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Axis of an alignment or distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// X coordinates.
    X,
    /// Y coordinates.
    Y,
}

fn coord(p: Point, a: Axis) -> Nm {
    match a {
        Axis::X => p.x,
        Axis::Y => p.y,
    }
}

fn with_coord(p: Point, a: Axis, v: Nm) -> Point {
    match a {
        Axis::X => Point::new(v, p.y),
        Axis::Y => Point::new(p.x, v),
    }
}

/// Placed, unlocked designators, in the given order, without duplicates.
fn movable(ctx: &mut Context<'_>, refdes: &[String]) -> Result<Vec<String>, CommandError> {
    let mut out: Vec<String> = Vec::new();
    for r in refdes {
        let k = util::refdes_key(ctx.project()?, r)?;
        unlocked(ctx.project_mut()?, &k)?;
        if !out.contains(&k) {
            out.push(k);
        }
    }
    Ok(out)
}

/// Align footprint origins on one coordinate: `axis: "x"` gives them the same X (a column),
/// `"y"` the same Y (a row).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceAlign {
    /// Designators (at least two).
    pub refdes: Vec<String>,
    /// Coordinate made equal: x or y.
    pub axis: Axis,
    /// Reference: "first" (default, the first listed part), "center" (middle of the extremes),
    /// "min", "max", or a coordinate ("12.5mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl Command for PlaceAlign {
    const NAME: &'static str = "place.align";
    const SUMMARY: &'static str = "Align footprint origins on X or Y (to the first, the center, min, max or a value)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let refs = movable(ctx, &self.refdes)?;
        if refs.is_empty() {
            return Err(CommandError::invalid_args("place.too_few", "name the parts to align"));
        }
        let p = ctx.project()?;
        let vals: Vec<Nm> = refs.iter().map(|r| coord(p.board().footprints[r].at, self.axis)).collect();
        let (lo, hi) = (*vals.iter().min().expect("non-empty"), *vals.iter().max().expect("non-empty"));
        let to = match self.to.as_deref().unwrap_or("first") {
            "first" => vals[0],
            "center" => Nm((lo.0 + hi.0).div_euclid(2)),
            "min" => lo,
            "max" => hi,
            v => Nm::parse(v).map_err(|e| {
                CommandError::invalid_args("place.invalid_align", format!("`{v}`: {e}"))
                    .with_hint("use first, center, min, max or a length like 12.5mm")
            })?,
        };
        let mut out = BTreeMap::new();
        let pm = ctx.project_mut()?;
        for r in refs {
            let f = pm.board_mut().footprints.get_mut(&r).expect("placed");
            f.at = with_coord(f.at, self.axis, to);
            out.insert(r, f.clone());
        }
        Ok(Placed { placements: out, unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Distribute parts along X or Y by their courtyards: with `spacing`, consecutive courtyards
/// get exactly that gap starting from the first part; without, the outermost parts stay and
/// the gaps between courtyards become equal. Parts are taken in coordinate order.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceDistribute {
    /// Designators (at least two).
    pub refdes: Vec<String>,
    /// Axis along which to distribute: x or y.
    pub axis: Axis,
    /// Gap between consecutive courtyards (default: equal gaps between the outermost parts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Nm>,
}

impl Command for PlaceDistribute {
    const NAME: &'static str = "place.distribute";
    const SUMMARY: &'static str = "Distribute parts along X or Y with equal or given gaps between courtyards";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let refs = movable(ctx, &self.refdes)?;
        if refs.len() < 2 {
            return Err(CommandError::invalid_args("place.too_few", "name at least two parts to distribute"));
        }
        let p = ctx.project()?;
        // (designator, low edge, high edge) along the axis.
        let mut items: Vec<(String, i64, i64)> = Vec::new();
        for r in &refs {
            let b = place::courtyard_box(p, r).ok_or_else(|| {
                CommandError::conflict("place.no_footprint", format!("{r} has no footprint or courtyard"))
            })?;
            let (lo, hi) = match self.axis {
                Axis::X => (b.x0, b.x1),
                Axis::Y => (b.y0, b.y1),
            };
            items.push((r.clone(), lo, hi));
        }
        items.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| natural_cmp(&a.0, &b.0)));
        let gap = match self.spacing {
            Some(s) => s.0,
            None => {
                let span = items.last().expect("two").2 - items[0].1;
                let sizes: i64 = items.iter().map(|x| x.2 - x.1).sum();
                (span - sizes).div_euclid(items.len() as i64 - 1)
            }
        };
        let mut out = BTreeMap::new();
        let mut next = items[0].1;
        let last = items.len() - 1;
        let pm = ctx.project_mut()?;
        for (k, (r, lo, hi)) in items.into_iter().enumerate() {
            // Without spacing the last part stays where it is (absorbs rounding).
            let delta = if self.spacing.is_none() && k == last { 0 } else { next - lo };
            let f = pm.board_mut().footprints.get_mut(&r).expect("placed");
            f.at = with_coord(f.at, self.axis, coord(f.at, self.axis) + Nm(delta));
            next = hi + delta + gap;
            out.insert(r, f.clone());
        }
        Ok(Placed { placements: out, unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}
