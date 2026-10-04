//! `board.*`, `place.*`, `track.*`, `via.*`: board setup, footprint placement and manual routing.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::board::{self as geo, RatLine};
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::fab::rules::{DerivedRule, Margin};
use crate::geom::{BBox, Point};
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{BoardSide, Contour, PlacedFootprint, RULE_FIELDS, RulePreset, Rules, Segment, Track, Via};
use crate::model::sections::natural_cmp;
use crate::units::{Angle, Nm};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Setup>()
        .register::<Outline>()
        .register::<SetRules>()
        .register::<Info>()
        .register::<Ratsnest>()
        .register::<PlaceSet>()
        .register::<PlaceMove>()
        .register::<PlaceRotate>()
        .register::<PlaceFlip>()
        .register::<PlaceLock>()
        .register::<PlaceRemove>()
        .register::<PlaceList>()
        .register::<PlaceAuto>()
        .register::<TrackAdd>()
        .register::<TrackRemove>()
        .register::<TrackList>()
        .register::<ViaAdd>()
        .register::<ViaRemove>();
}

/// Board overview.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BoardInfo {
    /// Copper layers, top to bottom.
    pub layers: Vec<String>,
    /// Thickness.
    pub thickness: Nm,
    /// Outline size (width, height), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<(Nm, Nm)>,
    /// Design rules.
    pub rules: Rules,
    /// Components placed.
    pub placed: usize,
    /// Components not placed yet.
    pub unplaced: Vec<String>,
    /// Placements of components that no longer exist.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale: Vec<String>,
    /// Tracks.
    pub tracks: usize,
    /// Vias.
    pub vias: usize,
    /// Unrouted connections.
    pub unrouted: usize,
    /// Board holes (mounting holes).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub holes: usize,
    /// Outline cutouts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cutouts: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

pub(super) fn outline_bbox(p: &Project) -> Option<BBox> {
    let c = p.board().outline.contours.first()?;
    let ring = geo::contour_ring(c, geo::COPPER_TOL);
    BBox::of_points(ring.into_iter().map(Point::from))
}

pub(super) fn info(p: &Project) -> BoardInfo {
    let b = p.board();
    let mut unplaced: Vec<String> =
        p.circuit().components.keys().filter(|r| !b.footprints.contains_key(*r)).cloned().collect();
    unplaced.sort_by(|a, b| natural_cmp(a, b));
    let stale = b.footprints.keys().filter(|r| !p.circuit().components.contains_key(*r)).cloned().collect();
    BoardInfo {
        layers: b.stackup.copper_names(),
        thickness: b.stackup.thickness,
        size: outline_bbox(p).map(|bb| (bb.width(), bb.height())),
        rules: b.rules.clone(),
        placed: b.footprints.len(),
        unplaced,
        stale,
        tracks: b.tracks.len(),
        vias: b.vias.len(),
        unrouted: geo::ratsnest(p).len(),
        holes: b.holes.len(),
        cutouts: b.outline.contours.len().saturating_sub(1),
    }
}

pub(super) fn info_text(o: &BoardInfo) -> String {
    let size = o.size.map(|(w, h)| format!("{w} x {h}")).unwrap_or_else(|| "no outline".into());
    let mut s = format!(
        "board: {} layers ({}), {}, {} thick\n  placed {}, unplaced {}, tracks {}, vias {}, unrouted {}",
        o.layers.len(),
        o.layers.join(" "),
        size,
        o.thickness,
        o.placed,
        o.unplaced.len(),
        o.tracks,
        o.vias,
        o.unrouted
    );
    if o.holes + o.cutouts > 0 {
        s += &format!("\n  holes {}, cutouts {}", o.holes, o.cutouts);
    }
    if !o.unplaced.is_empty() {
        s += &format!("\n  not placed: {}", o.unplaced.join(", "));
    }
    if !o.stale.is_empty() {
        s += &format!("\n  placements without component: {}", o.stale.join(", "));
    }
    s
}

/// Set the layer build-up and board specification preferences.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// Copper layers (1, 2, 4, 6, 8, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<u8>,
    /// Finished thickness ("1.6mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<Nm>,
    /// Outer copper thickness ("35um" = 1 oz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_copper: Option<Nm>,
    /// Inner copper thickness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_copper: Option<Nm>,
    /// Surface finish preferences, best first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<Vec<String>>,
    /// Mask color preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_color: Option<Vec<String>>,
    /// Silkscreen color preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silk_color: Option<Vec<String>>,
}

impl Command for Setup {
    const NAME: &'static str = "board.setup";
    const SUMMARY: &'static str = "Set copper layer count, thickness, copper weights, finish/color preferences";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = BoardInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<BoardInfo, CommandError> {
        if let Some(n) = self.layers
            && (n == 0 || (n > 1 && n % 2 == 1) || n > 32)
        {
            return Err(CommandError::invalid_args(
                "board.invalid_layers",
                "use 1 or an even number of copper layers up to 32",
            ));
        }
        let p = ctx.project_mut()?;
        let s = &mut p.board_mut().stackup;
        let mut dropped = false;
        if let Some(n) = self.layers {
            if n != s.copper_layers && !s.dielectrics.is_empty() {
                s.dielectrics.clear();
                dropped = true;
            }
            s.copper_layers = n;
        }
        if let Some(v) = self.thickness {
            s.thickness = v;
        }
        if let Some(v) = self.outer_copper {
            s.outer_copper = v;
        }
        if let Some(v) = self.inner_copper {
            s.inner_copper = v;
        }
        if let Some(v) = self.finish {
            s.finish = v;
        }
        if let Some(v) = self.mask_color {
            s.mask_color = v;
        }
        if let Some(v) = self.silk_color {
            s.silk_color = v;
        }
        if dropped {
            ctx.report(
                Diagnostic::warning(
                    "board.dielectrics_cleared",
                    "the copper layer count changed: the stackup's dielectrics were cleared",
                )
                .with_hint("set them again with `board.dielectric` (board.stackup shows the assumed ones)"),
            );
        }
        Ok(info(ctx.project()?))
    }

    fn summarize(o: &BoardInfo) -> String {
        info_text(o)
    }
}

/// Set the board outline: a rectangle (optionally rounded), a circle, or a polygon.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    /// Rectangle width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Nm>,
    /// Rectangle height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Nm>,
    /// Corner radius for the rectangle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_radius: Option<Nm>,
    /// Circle diameter (instead of a rectangle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diameter: Option<Nm>,
    /// Polygon vertices (instead of a rectangle).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub polygon: Vec<Point>,
    /// Lower-left corner of the rectangle / circle bounding box (default origin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Point>,
}

fn rect_contour(o: Point, w: Nm, h: Nm, r: Nm) -> Contour {
    let (x0, y0, x1, y1) = (o.x, o.y, o.x + w, o.y + h);
    let p = Point::new;
    if r.0 <= 0 {
        return Contour {
            start: p(x0, y0),
            segments: vec![
                Segment::Line { to: p(x1, y0) },
                Segment::Line { to: p(x1, y1) },
                Segment::Line { to: p(x0, y1) },
                Segment::Line { to: p(x0, y0) },
            ],
        };
    }
    // Arc midpoints at 45° on each corner.
    let k = Nm((r.0 as f64 * (1.0 - std::f64::consts::FRAC_1_SQRT_2)).round() as i64);
    Contour {
        start: p(x0 + r, y0),
        segments: vec![
            Segment::Line { to: p(x1 - r, y0) },
            Segment::Arc { mid: p(x1 - k, y0 + k), to: p(x1, y0 + r) },
            Segment::Line { to: p(x1, y1 - r) },
            Segment::Arc { mid: p(x1 - k, y1 - k), to: p(x1 - r, y1) },
            Segment::Line { to: p(x0 + r, y1) },
            Segment::Arc { mid: p(x0 + k, y1 - k), to: p(x0, y1 - r) },
            Segment::Line { to: p(x0, y0 + r) },
            Segment::Arc { mid: p(x0 + k, y0 + k), to: p(x0 + r, y0) },
        ],
    }
}

impl Command for Outline {
    const NAME: &'static str = "board.outline";
    const SUMMARY: &'static str = "Set the board outline: rectangle (width, height, corner radius), circle or polygon";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = BoardInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<BoardInfo, CommandError> {
        let o = self.origin.unwrap_or(Point::ORIGIN);
        let contour = match (self.width, self.height, self.diameter, self.polygon.is_empty()) {
            (Some(w), Some(h), None, true) => {
                if w <= Nm::ZERO || h <= Nm::ZERO {
                    return Err(CommandError::invalid_args(
                        "board.invalid_outline",
                        "width and height must be positive",
                    ));
                }
                let r = self.corner_radius.unwrap_or(Nm::ZERO);
                if r.0 * 2 > w.0.min(h.0) {
                    return Err(CommandError::invalid_args("board.invalid_outline", "corner radius too large"));
                }
                rect_contour(o, w, h, r)
            }
            (None, None, Some(d), true) => {
                let r = Nm(d.0 / 2);
                let c = Point::new(o.x + r, o.y + r);
                Contour {
                    start: Point::new(c.x + r, c.y),
                    segments: vec![
                        Segment::Arc { mid: Point::new(c.x, c.y + r), to: Point::new(c.x - r, c.y) },
                        Segment::Arc { mid: Point::new(c.x, c.y - r), to: Point::new(c.x + r, c.y) },
                    ],
                }
            }
            (None, None, None, false) if self.polygon.len() >= 3 => {
                let mut segments: Vec<Segment> = self.polygon[1..].iter().map(|&to| Segment::Line { to }).collect();
                segments.push(Segment::Line { to: self.polygon[0] });
                Contour { start: self.polygon[0], segments }
            }
            _ => {
                return Err(CommandError::invalid_args(
                    "board.invalid_outline",
                    "give width and height (rectangle), diameter (circle), or at least 3 polygon points",
                ));
            }
        };
        let p = ctx.project_mut()?;
        let outline = &mut p.board_mut().outline;
        if outline.contours.is_empty() {
            outline.contours.push(contour);
        } else {
            outline.contours[0] = contour;
        }
        Ok(info(ctx.project()?))
    }

    fn summarize(o: &BoardInfo) -> String {
        info_text(o)
    }
}

/// Change design rules: from a preset (`ipc2`, `ipc3`), from a fab profile's limits (`fab`,
/// `margin`), and/or field by field (given fields win). With no argument, shows the rules. Only
/// the resulting numbers are stored: nothing refers back to a preset or fab (D12). Net classes
/// whose values fall below the new minimums are reported (`drc.netclass_rule`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetRules {
    /// Start from a preset: `ipc2` (cadlab's conservative class 2 defaults) or `ipc3` (class 3
    /// annular rings and vias). Sources in docs/BOARD.md, "Rule presets".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<RulePreset>,
    /// Derive the manufacturing minimums (and default track/via) from this fab profile
    /// (`jlcpcb`, `pcbway`, `generic`); applied after `preset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fab: Option<String>,
    /// Process of the fab profile (default: the first offering the board's layer count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// With `fab`: `comfortable` (default; minimums 25 % above the fab's limits, default track
    /// and via never below 0.25 mm and 0.3/0.6 mm) or `tightest` (everything at the fab's limits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<Margin>,
    /// Copper clearance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// Default track width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_width: Option<Nm>,
    /// Minimum track width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_track_width: Option<Nm>,
    /// Default via drill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_drill: Option<Nm>,
    /// Default via diameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_diameter: Option<Nm>,
    /// Minimum annular ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_annular_ring: Option<Nm>,
    /// Minimum drill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_drill: Option<Nm>,
    /// Minimum hole-to-hole distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hole_to_hole: Option<Nm>,
    /// Minimum copper-to-edge distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copper_to_edge: Option<Nm>,
    /// Minimum silk-to-pad distance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silk_to_pad: Option<Nm>,
    /// Minimum silk width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silk_width: Option<Nm>,
    /// Minimum zone width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone_min_width: Option<Nm>,
    /// IPC class (1, 2 or 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipc_class: Option<u8>,
}

/// A rule value that changed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RuleChange {
    /// Rule field.
    pub field: String,
    /// Previous value.
    pub from: Nm,
    /// New value.
    pub to: Nm,
}

/// The board's rules after `board.rules`, with what changed and where values came from.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RulesResult {
    /// The rules.
    #[serde(flatten)]
    pub rules: Rules,
    /// Values that changed, in field order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<RuleChange>,
    /// Values taken from the fab profile (before explicit fields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<DerivedRule>,
    /// `<fab> <process>` the values were derived from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
}

impl Command for SetRules {
    const NAME: &'static str = "board.rules";
    const SUMMARY: &'static str =
        "Show or change design rules: IPC class 2/3 presets, limits derived from a fab profile, or field by field";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = RulesResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<RulesResult, CommandError> {
        let before = ctx.project()?.board().rules.clone();
        let mut r = match self.preset {
            Some(p) => Rules::preset(p),
            None => before.clone(),
        };
        let mut derived = Vec::new();
        let mut derived_from = None;
        if let Some(fab) = &self.fab {
            let ps = super::fab::profiles(ctx);
            let (profile, _) = super::fab::profile(&ps, fab)?;
            let process =
                crate::fab::check::select_process(profile, ctx.project()?, self.process.as_deref()).map_err(|d| {
                    let code = if d.code == "fab.layers" { "fab.layers" } else { "fab.unknown_process" };
                    let mut e = CommandError::invalid_args(code, d.message.clone());
                    if let Some(h) = &d.hint {
                        e = e.with_hint(h.clone());
                    }
                    e
                })?;
            let (nr, d) = crate::fab::rules::derive(process, self.margin.unwrap_or_default(), &r);
            r = nr;
            derived = d;
            derived_from = Some(format!("{} {}", profile.id, process.id));
            let unverified: Vec<&str> = derived.iter().filter(|d| d.unverified).map(|d| d.field.as_str()).collect();
            if !unverified.is_empty() {
                ctx.report(
                    Diagnostic::warning(
                        "board.rules_unverified",
                        format!("{} marks the source of {} unverified", profile.name, unverified.join(", ")),
                    )
                    .with_hint(format!("check them against the fab's pages ({})", profile.sources.join(" "))),
                );
            }
        } else if self.process.is_some() || self.margin.is_some() {
            return Err(CommandError::invalid_args("board.invalid_rule", "`process` and `margin` need `fab`")
                .with_hint("pass fab (jlcpcb, pcbway, generic; see fab.list)"));
        }
        let explicit: [Option<Nm>; 12] = [
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
        for (field, v) in RULE_FIELDS.iter().zip(explicit) {
            if let Some(v) = v {
                if v < Nm::ZERO {
                    return Err(CommandError::invalid_args(
                        "board.invalid_rule",
                        format!("`{field}` cannot be negative"),
                    ));
                }
                *r.length_mut(field).expect("rule field") = v;
            }
        }
        if let Some(c) = self.ipc_class {
            if !(1..=3).contains(&c) {
                return Err(CommandError::invalid_args("board.invalid_rule", "IPC class is 1, 2 or 3"));
            }
            r.ipc_class = c;
        }
        if r.via_drill >= r.via_diameter {
            return Err(CommandError::invalid_args("board.invalid_rule", "via diameter must exceed via drill"));
        }
        let changed = before
            .lengths()
            .iter()
            .zip(r.lengths())
            .filter(|(a, b)| a.1 != b.1)
            .map(|(a, b)| RuleChange { field: a.0.into(), from: a.1, to: b.1 })
            .collect();
        ctx.project_mut()?.board_mut().rules = r.clone();
        for d in crate::drc::netclass_conflicts(ctx.project()?) {
            ctx.report(d);
        }
        Ok(RulesResult { rules: r, changed, derived, derived_from })
    }

    fn summarize(o: &RulesResult) -> String {
        let r = &o.rules;
        let mut s = format!(
            "rules (IPC class {}): clearance {}, track {} (min {}), via {}/{}, annular ring {}, drill {}, edge {}",
            r.ipc_class,
            r.clearance,
            r.track_width,
            r.min_track_width,
            r.via_diameter,
            r.via_drill,
            r.min_annular_ring,
            r.min_drill,
            r.copper_to_edge
        );
        if let Some(f) = &o.derived_from {
            s += &format!("\nderived from {f}: {} value(s)", o.derived.len());
        }
        for c in &o.changed {
            s += &format!("\n  {}: {} -> {}", c.field, c.from, c.to);
        }
        s
    }
}

/// Board overview: layers, size, placement and routing status.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Info {}

impl Command for Info {
    const NAME: &'static str = "board.info";
    const SUMMARY: &'static str = "Board overview: layers, size, rules, placement and routing status";
    const KIND: CommandKind = CommandKind::Query;
    type Output = BoardInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<BoardInfo, CommandError> {
        Ok(info(ctx.project()?))
    }

    fn summarize(o: &BoardInfo) -> String {
        info_text(o)
    }
}

/// Unrouted connections (shortest links between copper islands of each net).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ratsnest {
    /// Only this net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
}

/// Unrouted connections.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RatsnestResult {
    /// Lines.
    pub lines: Vec<RatLine>,
}

impl Command for Ratsnest {
    const NAME: &'static str = "board.ratsnest";
    const SUMMARY: &'static str = "Unrouted connections between placed pads, per net";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["net"];
    type Output = RatsnestResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<RatsnestResult, CommandError> {
        let p = ctx.project()?;
        let net = match &self.net {
            Some(n) => Some(super::net::net_name(p, n)?),
            None => None,
        };
        let lines = geo::ratsnest(p).into_iter().filter(|l| net.as_ref().is_none_or(|n| *n == l.net)).collect();
        Ok(RatsnestResult { lines })
    }

    fn summarize(o: &RatsnestResult) -> String {
        if o.lines.is_empty() {
            return "fully routed".into();
        }
        o.lines
            .iter()
            .map(|l| format!("{}: {} -> {} ({})", l.net, l.from, l.to, l.length))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---- placement ----

/// Placements, for results.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Placed {
    /// Placements by designator (only those affected, or all for `place.list`).
    pub placements: BTreeMap<String, PlacedFootprint>,
    /// Components not placed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unplaced: Vec<String>,
    /// Total ratsnest length after the change (minimum spanning tree per net over pad
    /// centers), for automatic placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratsnest_length: Option<Nm>,
}

pub(super) fn placed_text(o: &Placed) -> String {
    let mut lines: Vec<String> = o
        .placements
        .iter()
        .map(|(r, p)| {
            format!(
                "{r}: ({}, {}) {}{}{}",
                p.at.x,
                p.at.y,
                p.rotation,
                if p.side == BoardSide::Bottom { " bottom" } else { "" },
                if p.locked { " locked" } else { "" }
            )
        })
        .collect();
    lines.sort_by(|a, b| natural_cmp(a, b));
    if !o.unplaced.is_empty() {
        lines.push(format!("not placed: {}", o.unplaced.join(", ")));
    }
    if let Some(l) = o.ratsnest_length {
        lines.push(format!("ratsnest length: {l}"));
    }
    lines.join("\n")
}

pub(super) fn check_placeable(p: &Project, refdes: &str) -> Result<String, CommandError> {
    let r = util::refdes_key(p, refdes)?;
    if geo::footprint_for(p, &r).is_none() {
        return Err(CommandError::invalid_args("place.no_footprint", format!("{r} has no footprint"))
            .with_hint("give its part a footprint (`part.set --footprint` or `footprint.generate`)"));
    }
    Ok(r)
}

pub(super) fn unlocked<'a>(p: &'a mut Project, r: &str) -> Result<&'a mut PlacedFootprint, CommandError> {
    let fp = p.board_mut().footprints.get_mut(r).ok_or_else(|| {
        CommandError::conflict("place.not_placed", format!("{r} is not placed"))
            .with_hint(format!("place it first: `place.set {r} --at ...`"))
    })?;
    if fp.locked {
        return Err(CommandError::conflict("place.locked", format!("{r} is locked"))
            .with_hint(format!("unlock it: `place.lock {r} --locked false`")));
    }
    Ok(fp)
}

/// Place (or re-place) a component's footprint.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceSet {
    /// Designator.
    pub refdes: String,
    /// Position of the footprint origin: ["12.7mm", "8mm"].
    pub at: Point,
    /// Rotation, degrees counter-clockwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Angle>,
    /// Side: top or bottom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<BoardSide>,
}

impl Command for PlaceSet {
    const NAME: &'static str = "place.set";
    const SUMMARY: &'static str = "Place a component's footprint at a position, rotation and side";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let r = check_placeable(ctx.project()?, &self.refdes)?;
        let p = ctx.project_mut()?;
        let existing = p.board().footprints.get(&r).cloned();
        if existing.as_ref().is_some_and(|f| f.locked) {
            return Err(CommandError::conflict("place.locked", format!("{r} is locked")));
        }
        let mut fp = existing.unwrap_or(PlacedFootprint {
            at: self.at,
            rotation: Angle::ZERO,
            side: BoardSide::Top,
            locked: false,
            footprint: None,
        });
        fp.at = self.at;
        if let Some(a) = self.rotation {
            fp.rotation = a.normalized();
        }
        if let Some(s) = self.side {
            fp.side = s;
        }
        p.board_mut().footprints.insert(r.clone(), fp.clone());
        Ok(Placed { placements: [(r, fp)].into(), unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Move placed footprints by an offset.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceMove {
    /// Designators.
    pub refdes: Vec<String>,
    /// Offset: ["2mm", "-1mm"].
    pub by: Point,
}

impl Command for PlaceMove {
    const NAME: &'static str = "place.move";
    const SUMMARY: &'static str = "Move placed footprints by an offset";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let mut out = BTreeMap::new();
        for r in &self.refdes {
            let r = util::refdes_key(ctx.project()?, r)?;
            let fp = unlocked(ctx.project_mut()?, &r)?;
            fp.at = fp.at + self.by;
            out.insert(r, fp.clone());
        }
        Ok(Placed { placements: out, unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Rotate placed footprints around their origin.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceRotate {
    /// Designators.
    pub refdes: Vec<String>,
    /// Added rotation, degrees counter-clockwise (default 90).
    #[serde(default = "ninety")]
    pub by: Angle,
}

fn ninety() -> Angle {
    Angle::DEG_90
}

impl Command for PlaceRotate {
    const NAME: &'static str = "place.rotate";
    const SUMMARY: &'static str = "Rotate placed footprints (default +90°)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let mut out = BTreeMap::new();
        for r in &self.refdes {
            let r = util::refdes_key(ctx.project()?, r)?;
            let fp = unlocked(ctx.project_mut()?, &r)?;
            fp.rotation = (fp.rotation + self.by).normalized();
            out.insert(r, fp.clone());
        }
        Ok(Placed { placements: out, unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Move footprints to the other side of the board.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceFlip {
    /// Designators.
    pub refdes: Vec<String>,
}

impl Command for PlaceFlip {
    const NAME: &'static str = "place.flip";
    const SUMMARY: &'static str = "Move footprints to the other side (mirrored)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let mut out = BTreeMap::new();
        for r in &self.refdes {
            let r = util::refdes_key(ctx.project()?, r)?;
            let fp = unlocked(ctx.project_mut()?, &r)?;
            fp.side = if fp.side == BoardSide::Top { BoardSide::Bottom } else { BoardSide::Top };
            out.insert(r, fp.clone());
        }
        Ok(Placed { placements: out, unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Lock or unlock footprints.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceLock {
    /// Designators.
    pub refdes: Vec<String>,
    /// True to lock, false to unlock.
    #[serde(default = "yes")]
    pub locked: bool,
}

fn yes() -> bool {
    true
}

impl Command for PlaceLock {
    const NAME: &'static str = "place.lock";
    const SUMMARY: &'static str = "Lock or unlock footprints against moves and auto-placement";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let mut out = BTreeMap::new();
        for r in &self.refdes {
            let r = util::refdes_key(ctx.project()?, r)?;
            let p = ctx.project_mut()?;
            let fp = p
                .board_mut()
                .footprints
                .get_mut(&r)
                .ok_or_else(|| CommandError::conflict("place.not_placed", format!("{r} is not placed")))?;
            fp.locked = self.locked;
            out.insert(r, fp.clone());
        }
        Ok(Placed { placements: out, unplaced: vec![], ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Remove footprints from the board (components stay in the circuit).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceRemove {
    /// Designators.
    pub refdes: Vec<String>,
}

impl Command for PlaceRemove {
    const NAME: &'static str = "place.remove";
    const SUMMARY: &'static str = "Take footprints off the board (components stay in the circuit)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        for r in &self.refdes {
            let key = ctx.project()?.board().footprints.keys().find(|k| k.eq_ignore_ascii_case(r)).cloned();
            let Some(k) = key else {
                return Err(CommandError::conflict("place.not_placed", format!("{r} is not placed")));
            };
            unlocked(ctx.project_mut()?, &k)?;
            ctx.project_mut()?.board_mut().footprints.remove(&k);
        }
        let p = ctx.project()?;
        Ok(Placed { placements: BTreeMap::new(), unplaced: info(p).unplaced, ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// List placements and unplaced components.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceList {}

impl Command for PlaceList {
    const NAME: &'static str = "place.list";
    const SUMMARY: &'static str = "List footprint placements and unplaced components";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let p = ctx.project()?;
        Ok(Placed { placements: p.board().footprints.clone(), unplaced: info(p).unplaced, ratsnest_length: None })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Auto-placement strategy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// By schematic-like groups: ICs and connectors spread over the board (connectors at the
    /// edges), their passives next to the pins they connect to, then local improvement of the
    /// ratsnest length.
    #[default]
    Groups,
    /// Rows inside the outline, largest first (a plain packing).
    Rows,
}

/// Place footprints automatically inside the board outline (a starting point to refine).
/// Locked footprints, and placed ones unless `replace`, stay put and are obstacles; cutouts,
/// footprint keep-outs and holes are avoided.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceAuto {
    /// "groups" (default) or "rows".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Strategy>,
    /// Gap between courtyards (default 0.25mm for groups, 0.5mm for rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Nm>,
    /// Also re-place footprints already on the board (except locked ones).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

impl Command for PlaceAuto {
    const NAME: &'static str = "place.auto";
    const SUMMARY: &'static str =
        "Place footprints automatically: by schematic groups (default) or in rows; locked ones stay";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Placed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placed, CommandError> {
        let p = ctx.project()?;
        if outline_bbox(p).is_none() {
            return Err(CommandError::conflict("board.no_outline", "the board has no outline")
                .with_hint("set one with `board.outline`"));
        }
        let mut moving: Vec<String> = info(p).unplaced;
        if self.replace {
            moving.extend(
                p.board()
                    .footprints
                    .iter()
                    .filter(|(r, f)| !f.locked && p.circuit().components.contains_key(*r))
                    .map(|(r, _)| r.clone()),
            );
            moving.sort_by(|a, b| natural_cmp(a, b));
        }
        let old: BTreeMap<String, PlacedFootprint> =
            moving.iter().filter_map(|r| p.board().footprints.get(r).map(|f| (r.clone(), f.clone()))).collect();
        let (mut out, outside) = match self.strategy.unwrap_or_default() {
            Strategy::Groups => {
                let gap = self.spacing.unwrap_or(geo::place::DEFAULT_GAP);
                let r = geo::place::auto_place(p, &moving, gap)
                    .map_err(|e| CommandError::conflict(e.code, e.message).with_hint(e.hint))?;
                (r.placements, r.unplaced)
            }
            Strategy::Rows => {
                let pm = ctx.project_mut()?;
                for r in old.keys() {
                    pm.board_mut().footprints.remove(r);
                }
                rows(ctx.project()?, &moving, self.spacing.unwrap_or(Nm::from_um(500)).0)
            }
        };
        let pm = ctx.project_mut()?;
        for (r, fp) in out.iter_mut() {
            if let Some(o) = old.get(r) {
                fp.footprint = o.footprint.clone();
            }
            pm.board_mut().footprints.insert(r.clone(), fp.clone());
        }
        if !outside.is_empty() {
            ctx.report(
                crate::diag::Diagnostic::warning(
                    "place.no_room",
                    format!("no room left on the board for {}", outside.join(", ")),
                )
                .with_hint("enlarge the outline or place them by hand"),
            );
        }
        let length = geo::place::ratsnest_length(ctx.project()?);
        Ok(Placed { placements: out, unplaced: outside, ratsnest_length: Some(length) })
    }

    fn summarize(o: &Placed) -> String {
        placed_text(o)
    }
}

/// Rows inside the outline's bounding box, largest courtyard first.
fn rows(p: &Project, moving: &[String], gap: i64) -> (BTreeMap<String, PlacedFootprint>, Vec<String>) {
    let Some(area) = outline_bbox(p) else { return (BTreeMap::new(), moving.to_vec()) };
    // Footprints to place with their courtyard boxes (local).
    let mut todo: Vec<(String, BBox)> = Vec::new();
    for r in moving {
        if let Some(fp) = geo::footprint_for(p, r)
            && let Some(b) = BBox::of_points(fp.courtyard.iter().copied())
        {
            todo.push((r.clone(), b));
        }
    }
    todo.sort_by(|a, b| {
        let area = |x: &BBox| (x.width().0 as i128) * (x.height().0 as i128);
        area(&b.1).cmp(&area(&a.1)).then_with(|| natural_cmp(&a.0, &b.0))
    });
    let margin = p.board().rules.copper_to_edge.0 + gap;
    let (mut x, mut y, mut row_h) = (area.min.x.0 + margin, area.max.y.0 - margin, 0i64);
    let snap = |v: i64| v.div_euclid(50_000) * 50_000;
    let mut out = BTreeMap::new();
    let mut outside = Vec::new();
    for (r, b) in todo {
        let (w, h) = (b.width().0, b.height().0);
        if x + w > area.max.x.0 - margin && x > area.min.x.0 + margin {
            x = area.min.x.0 + margin;
            y -= row_h + gap;
            row_h = 0;
        }
        if y - h < area.min.y.0 + margin {
            outside.push(r);
            continue;
        }
        // Origin so that the courtyard's top-left lands at (x, y).
        let at = Point::new(Nm(snap(x - b.min.x.0)), Nm(snap(y - b.max.y.0)));
        let fp = PlacedFootprint { at, rotation: Angle::ZERO, side: BoardSide::Top, locked: false, footprint: None };
        out.insert(r, fp);
        x += w + gap;
        row_h = row_h.max(h);
    }
    (out, outside)
}

// ---- tracks and vias ----

/// A track point: coordinates, or a pin (`U1.3`, `U1.VIN`) meaning its pad's center.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TrackPoint {
    /// A pin: its (first) pad's center.
    Pin(String),
    /// Coordinates.
    At(Point),
}

fn resolve_point(p: &Project, tp: &TrackPoint) -> Result<(Point, Option<String>), CommandError> {
    match tp {
        TrackPoint::At(pt) => Ok((*pt, None)),
        TrackPoint::Pin(s) => {
            let pins = crate::connect::resolve_pins(p, s)?;
            let pin = pins.first().expect("resolve returns at least one pin");
            let comp = &p.circuit().components[&pin.refdes];
            let part = &p.library().parts[&comp.part];
            let pads = part.footprint().map(|f| f.pads_for(&pin.pin)).unwrap_or_else(|| vec![pin.pin.clone()]);
            let pp = geo::placed_pads(p)
                .into_iter()
                .find(|pp| pp.refdes == pin.refdes && pads.contains(&pp.number))
                .ok_or_else(|| {
                    CommandError::conflict("place.not_placed", format!("{} is not placed on the board", pin.refdes))
                })?;
            Ok((pp.center, pp.net))
        }
    }
}

fn net_width(p: &Project, net: Option<&str>) -> Nm {
    net.and_then(|n| p.circuit().nets.get(n))
        .and_then(|n| n.class.as_ref())
        .and_then(|c| p.circuit().netclasses.get(c))
        .and_then(|c| c.track_width)
        .unwrap_or(p.board().rules.track_width)
}

/// Add a track along points on a copper layer.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackAdd {
    /// Copper layer ("F.Cu", "B.Cu", "In1.Cu").
    pub layer: String,
    /// Points: coordinates ["10mm", "5mm"] or pins "U1.3" (pad center); at least two.
    pub points: Vec<TrackPoint>,
    /// Width (default: the net class's, else the rules' track width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Nm>,
    /// Net (default: from a pin among the points).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
}

/// Items created.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Created {
    /// Tracks created.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
    /// Vias created.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vias: Vec<Via>,
}

impl Command for TrackAdd {
    const NAME: &'static str = "track.add";
    const SUMMARY: &'static str = "Add a track through points or pins on a copper layer";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Created;

    fn run(self, ctx: &mut Context<'_>) -> Result<Created, CommandError> {
        let p = ctx.project()?;
        if !p.board().is_copper(&self.layer) {
            return Err(CommandError::invalid_args(
                "track.invalid_layer",
                format!("`{}` is not a copper layer", self.layer),
            )
            .with_hint(format!("copper layers: {}", p.board().stackup.copper_names().join(", "))));
        }
        if self.points.len() < 2 {
            return Err(CommandError::invalid_args("track.too_few_points", "a track needs at least two points"));
        }
        let mut pts = Vec::new();
        let mut nets = Vec::new();
        for tp in &self.points {
            let (pt, net) = resolve_point(p, tp)?;
            pts.push(pt);
            if let Some(n) = net {
                nets.push(n);
            }
        }
        nets.dedup();
        let net = match (&self.net, nets.as_slice()) {
            (Some(n), _) => Some(super::net::net_name(p, n)?),
            (None, []) => None,
            (None, [one]) => Some(one.clone()),
            (None, many) => {
                return Err(CommandError::conflict(
                    "track.short",
                    format!("the points connect different nets: {}", many.join(", ")),
                )
                .with_hint("a track joins one net; check the pins"));
            }
        };
        let width = self.width.unwrap_or_else(|| net_width(p, net.as_deref()));
        if width <= Nm::ZERO {
            return Err(CommandError::invalid_args("track.invalid_width", "width must be positive"));
        }
        let mut tracks = Vec::new();
        for w in pts.windows(2) {
            if w[0] == w[1] {
                continue;
            }
            let id = ctx.project_mut()?.alloc_id();
            let t = Track {
                id,
                layer: self.layer.clone(),
                width,
                net: net.clone(),
                start: w[0],
                end: w[1],
                mid: None,
                locked: false,
            };
            ctx.project_mut()?.board_mut().tracks.push(t.clone());
            tracks.push(t);
        }
        Ok(Created { tracks, vias: vec![] })
    }

    fn summarize(o: &Created) -> String {
        format!(
            "added {} track segment(s){}",
            o.tracks.len(),
            o.tracks.first().and_then(|t| t.net.as_ref()).map(|n| format!(" on {n}")).unwrap_or_default()
        )
    }
}

pub(super) fn parse_item(s: &str, kind: &str) -> Result<ObjectId, CommandError> {
    let n = s.strip_prefix(&format!("{kind}#")).unwrap_or(s);
    n.parse::<u64>().map(ObjectId).map_err(|_| {
        CommandError::invalid_args("board.invalid_ref", format!("`{s}` is not a {kind} reference ({kind}#12)"))
    })
}

/// Remove tracks (by ID, or every track of a net).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackRemove {
    /// Tracks: "track#12" or 12.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// Remove every unlocked track of this net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
}

/// Count of removed items.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Removed {
    /// Number removed.
    pub removed: usize,
}

impl Command for TrackRemove {
    const NAME: &'static str = "track.remove";
    const SUMMARY: &'static str = "Remove tracks by ID or all unlocked tracks of a net";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["ids"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let ids: Vec<ObjectId> = self.ids.iter().map(|s| parse_item(s, "track")).collect::<Result<_, _>>()?;
        let net = match &self.net {
            Some(n) => Some(super::net::net_name(ctx.project()?, n)?),
            None => None,
        };
        let tracks = &mut ctx.project_mut()?.board_mut().tracks;
        let before = tracks.len();
        for id in &ids {
            if !tracks.iter().any(|t| t.id == *id) {
                return Err(CommandError::not_found("track.not_found", format!("no track#{}", id.0)));
            }
        }
        tracks.retain(|t| !ids.contains(&t.id) && !(net.is_some() && t.net == net && !t.locked));
        Ok(Removed { removed: before - tracks.len() })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed {} track(s)", o.removed)
    }
}

/// List tracks and vias.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackList {
    /// Only this net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
}

impl Command for TrackList {
    const NAME: &'static str = "track.list";
    const SUMMARY: &'static str = "List tracks and vias (optionally of one net)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["net"];
    type Output = Created;

    fn run(self, ctx: &mut Context<'_>) -> Result<Created, CommandError> {
        let p = ctx.project()?;
        let net = match &self.net {
            Some(n) => Some(super::net::net_name(p, n)?),
            None => None,
        };
        let keep = |n: &Option<String>| net.is_none() || *n == net;
        Ok(Created {
            tracks: p.board().tracks.iter().filter(|t| keep(&t.net)).cloned().collect(),
            vias: p.board().vias.iter().filter(|v| keep(&v.net)).cloned().collect(),
        })
    }

    fn summarize(o: &Created) -> String {
        let mut s: Vec<String> = o
            .tracks
            .iter()
            .map(|t| {
                format!(
                    "track#{} {} {} ({}, {}) -> ({}, {}) w {}",
                    t.id.0,
                    t.layer,
                    t.net.as_deref().unwrap_or("-"),
                    t.start.x,
                    t.start.y,
                    t.end.x,
                    t.end.y,
                    t.width
                )
            })
            .collect();
        s.extend(o.vias.iter().map(|v| {
            format!(
                "via#{} {} ({}, {}) {}/{}",
                v.id.0,
                v.net.as_deref().unwrap_or("-"),
                v.at.x,
                v.at.y,
                v.diameter,
                v.drill
            )
        }));
        if s.is_empty() { "no tracks or vias".into() } else { s.join("\n") }
    }
}

/// Add a via.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViaAdd {
    /// Position.
    pub at: Point,
    /// Net (recommended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Drill (default: net class, else rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drill: Option<Nm>,
    /// Pad diameter (default: net class, else rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diameter: Option<Nm>,
    /// First layer (default F.Cu).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Last layer (default B.Cu).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl Command for ViaAdd {
    const NAME: &'static str = "via.add";
    const SUMMARY: &'static str = "Add a via (through by default)";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Created;

    fn run(self, ctx: &mut Context<'_>) -> Result<Created, CommandError> {
        let p = ctx.project()?;
        let net = match &self.net {
            Some(n) => Some(super::net::net_name(p, n)?),
            None => None,
        };
        let class = net
            .as_deref()
            .and_then(|n| p.circuit().nets.get(n))
            .and_then(|n| n.class.as_ref())
            .and_then(|c| p.circuit().netclasses.get(c));
        let rules = &p.board().rules;
        let drill = self.drill.or(class.and_then(|c| c.via_drill)).unwrap_or(rules.via_drill);
        let diameter = self.diameter.or(class.and_then(|c| c.via_diameter)).unwrap_or(rules.via_diameter);
        if drill >= diameter {
            return Err(CommandError::invalid_args("via.invalid_size", "via diameter must exceed its drill"));
        }
        let names = p.board().stackup.copper_names();
        let from = self.from.unwrap_or_else(|| names[0].clone());
        let to = self.to.unwrap_or_else(|| names[names.len() - 1].clone());
        for l in [&from, &to] {
            if !names.contains(l) {
                return Err(CommandError::invalid_args("via.invalid_layer", format!("`{l}` is not a copper layer")));
            }
        }
        let id = ctx.project_mut()?.alloc_id();
        let v = Via { id, at: self.at, drill, diameter, net, from, to, locked: false };
        ctx.project_mut()?.board_mut().vias.push(v.clone());
        Ok(Created { tracks: vec![], vias: vec![v] })
    }

    fn summarize(o: &Created) -> String {
        o.vias
            .iter()
            .map(|v| format!("added via#{} at ({}, {})", v.id.0, v.at.x, v.at.y))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Remove vias.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViaRemove {
    /// Vias: "via#12" or 12.
    pub ids: Vec<String>,
}

impl Command for ViaRemove {
    const NAME: &'static str = "via.remove";
    const SUMMARY: &'static str = "Remove vias";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["ids"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let ids: Vec<ObjectId> = self.ids.iter().map(|s| parse_item(s, "via")).collect::<Result<_, _>>()?;
        let vias = &mut ctx.project_mut()?.board_mut().vias;
        for id in &ids {
            if !vias.iter().any(|v| v.id == *id) {
                return Err(CommandError::not_found("via.not_found", format!("no via#{}", id.0)));
            }
        }
        let before = vias.len();
        vias.retain(|v| !ids.contains(&v.id));
        Ok(Removed { removed: before - vias.len() })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed {} via(s)", o.removed)
    }
}
