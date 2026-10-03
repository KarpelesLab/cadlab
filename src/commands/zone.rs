//! `zone.*` and `keepout.*`: copper pours and keep-out areas.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{self as geo, zones};
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::geom::{BBox, Point};
use crate::model::Project;
use crate::model::board::{Keepout, PadConnection, Zone};
use crate::refs::ObjectRef;
use crate::suggest::did_you_mean;
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<ZoneAdd>()
        .register::<ZoneSet>()
        .register::<ZoneRemove>()
        .register::<ZoneList>()
        .register::<ZoneFill>()
        .register::<KeepoutAdd>()
        .register::<KeepoutRemove>()
        .register::<KeepoutList>();
}

/// The `"board"` outline keyword.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutlineKeyword {
    /// The board outline's bounding box (fills are clipped to the board outline anyway).
    Board,
}

/// A rectangle by two opposite corners.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RectSpec {
    /// One corner.
    pub from: Point,
    /// The opposite corner.
    pub to: Point,
}

/// An area outline: polygon points, a rectangle, or `"board"`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AreaOutline {
    /// `"board"`: the whole board.
    Keyword(OutlineKeyword),
    /// `{"rect": {"from": [..], "to": [..]}}`.
    Rect {
        /// The rectangle.
        rect: RectSpec,
    },
    /// Polygon vertices: [["0mm", "0mm"], ["10mm", "0mm"], ["10mm", "5mm"]].
    Points(Vec<Point>),
}

fn resolve_outline(p: &Project, o: &AreaOutline, what: &str) -> Result<Vec<Point>, CommandError> {
    let pts = match o {
        AreaOutline::Keyword(OutlineKeyword::Board) => {
            let c = p.board().outline.contours.first().ok_or_else(|| {
                CommandError::conflict("board.no_outline", "the board has no outline")
                    .with_hint("set one with `board.outline`, or give the outline as points or a rect")
            })?;
            let ring = geo::contour_ring(c, geo::COPPER_TOL);
            let bb = BBox::of_points(ring.into_iter().map(Point::from))
                .ok_or_else(|| CommandError::conflict("board.no_outline", "the board outline is empty"))?;
            rect_points(bb.min, bb.max)
        }
        AreaOutline::Rect { rect } => rect_points(rect.from, rect.to),
        AreaOutline::Points(v) => v.clone(),
    };
    let ring: Vec<crate::geom::poly::Point> = pts.iter().map(|&q| q.into()).collect();
    if pts.len() < 3 || crate::geom::poly::ring_area2(&ring) == 0 {
        return Err(CommandError::invalid_args(
            if what == "zone" { "zone.invalid_outline" } else { "keepout.invalid_outline" },
            "the outline needs at least 3 points enclosing a non-zero area",
        )
        .with_hint("give points, {\"rect\": {\"from\": [..], \"to\": [..]}} or \"board\""));
    }
    Ok(pts)
}

fn rect_points(a: Point, b: Point) -> Vec<Point> {
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    vec![Point::new(x0, y0), Point::new(x1, y0), Point::new(x1, y1), Point::new(x0, y1)]
}

fn resolve_layers(p: &Project, layers: &[String], what: &'static str) -> Result<Vec<String>, CommandError> {
    let names = p.board().stackup.copper_names();
    let mut out: Vec<String> = Vec::new();
    for l in layers {
        let found = names.iter().find(|n| n.eq_ignore_ascii_case(l)).ok_or_else(|| {
            CommandError::invalid_args(what, format!("`{l}` is not a copper layer of this board"))
                .with_subject(ObjectRef::Layer(l.clone()))
                .with_hint(format!("copper layers: {}", names.join(", ")))
        })?;
        if !out.contains(found) {
            out.push(found.clone());
        }
    }
    Ok(out)
}

fn zone_index(p: &Project, name: &str) -> Result<usize, CommandError> {
    let name = name.strip_prefix("zone:").unwrap_or(name);
    let zones = &p.board().zones;
    if let Some(i) = zones.iter().position(|z| z.name == name) {
        return Ok(i);
    }
    if let Some(i) = zones.iter().position(|z| z.name.eq_ignore_ascii_case(name)) {
        return Ok(i);
    }
    let s = did_you_mean(name, zones.iter().map(|z| z.name.as_str()), 3);
    Err(CommandError::not_found("zone.not_found", format!("no zone `{name}`"))
        .with_subject(ObjectRef::Named { kind: "zone".into(), name: name.into() })
        .with_suggestions(&s)
        .with_hint_if_none("list zones with `zone.list`"))
}

fn check_name(p: &Project, name: &str, kind: &'static str, skip: Option<usize>) -> Result<(), CommandError> {
    let taken = match kind {
        "zone" => p.board().zones.iter().enumerate().any(|(i, z)| Some(i) != skip && z.name.eq_ignore_ascii_case(name)),
        _ => p.board().keepouts.iter().any(|k| k.name.eq_ignore_ascii_case(name)),
    };
    let (empty, dup) = if kind == "zone" {
        ("zone.invalid_name", "zone.duplicate")
    } else {
        ("keepout.invalid_name", "keepout.duplicate")
    };
    if name.trim().is_empty() || name.contains(char::is_whitespace) {
        return Err(CommandError::invalid_args(empty, format!("`{name}` is not a valid {kind} name"))
            .with_hint("use a non-empty name without spaces, e.g. GND_bottom"));
    }
    if taken {
        return Err(CommandError::conflict(dup, format!("a {kind} named `{name}` already exists"))
            .with_subject(ObjectRef::Named { kind: kind.into(), name: name.into() })
            .with_hint(format!(
                "choose another name, or change the existing one with `{kind}.{}`",
                if kind == "zone" { "set" } else { "remove" }
            )));
    }
    Ok(())
}

fn check_len(v: Option<Nm>, field: &str, positive: bool) -> Result<(), CommandError> {
    if let Some(v) = v
        && (v < Nm::ZERO || (positive && v == Nm::ZERO))
    {
        return Err(CommandError::invalid_args(
            "zone.invalid_value",
            format!("{field} must be {}", if positive { "positive" } else { "zero or positive" }),
        ));
    }
    Ok(())
}

/// Zones, for results.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Zones {
    /// Zones.
    pub zones: Vec<Zone>,
}

fn zones_text(o: &Zones) -> String {
    if o.zones.is_empty() {
        return "no zones".into();
    }
    o.zones
        .iter()
        .map(|z| {
            format!(
                "zone:{} {} on {}, priority {}, pads {}{}",
                z.name,
                z.net.as_deref().unwrap_or("(no net)"),
                z.layers.join(" "),
                z.priority,
                match z.pads {
                    PadConnection::Thermal => "thermal",
                    PadConnection::Solid => "solid",
                    PadConnection::None => "none",
                },
                z.clearance.map(|c| format!(", clearance {c}")).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Add a copper zone (pour) on one or more copper layers.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneAdd {
    /// Unique name ("GND_bottom").
    pub name: String,
    /// Net to pour.
    pub net: String,
    /// Copper layers ("F.Cu", "B.Cu").
    pub layers: Vec<String>,
    /// Outline: points, {"rect": {"from", "to"}}, or "board" (the whole board).
    pub outline: AreaOutline,
    /// Priority: higher fills first and is avoided by lower ones (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Clearance to other nets (default: rules, or the net class if larger).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// Minimum copper width (default: the rules' zone_min_width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<Nm>,
    /// Pad connection: thermal (default), solid or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pads: Option<PadConnection>,
    /// Thermal relief gap (default: the clearance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_gap: Option<Nm>,
    /// Thermal spoke width (default: max(track width, 0.25mm)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_spoke: Option<Nm>,
}

impl Command for ZoneAdd {
    const NAME: &'static str = "zone.add";
    const SUMMARY: &'static str =
        "Add a copper pour: net, layers, outline (points, rect or \"board\"), priority, clearances, thermals";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Zones;

    fn run(self, ctx: &mut Context<'_>) -> Result<Zones, CommandError> {
        let p = ctx.project()?;
        check_name(p, &self.name, "zone", None)?;
        let net = super::net::net_name(p, &self.net)?;
        if self.layers.is_empty() {
            return Err(CommandError::invalid_args("zone.invalid_layer", "give at least one copper layer")
                .with_hint(format!("copper layers: {}", p.board().stackup.copper_names().join(", "))));
        }
        let layers = resolve_layers(p, &self.layers, "zone.invalid_layer")?;
        let outline = resolve_outline(p, &self.outline, "zone")?;
        check_len(self.clearance, "clearance", false)?;
        check_len(self.min_width, "min_width", false)?;
        check_len(self.thermal_gap, "thermal_gap", false)?;
        check_len(self.thermal_spoke, "thermal_spoke", true)?;
        let p = ctx.project_mut()?;
        let id = p.alloc_id();
        let z = Zone {
            id,
            name: self.name,
            net: Some(net),
            layers,
            outline,
            priority: self.priority.unwrap_or(0),
            clearance: self.clearance,
            min_width: self.min_width,
            pads: self.pads.unwrap_or_default(),
            thermal_gap: self.thermal_gap,
            thermal_spoke: self.thermal_spoke,
        };
        p.board_mut().zones.push(z.clone());
        Ok(Zones { zones: vec![z] })
    }

    fn summarize(o: &Zones) -> String {
        format!("added {}", zones_text(o))
    }
}

/// Change a zone's properties. Only given values change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneSet {
    /// Zone name.
    pub name: String,
    /// New name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<String>,
    /// Net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Copper layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<String>>,
    /// Outline: points, {"rect": {"from", "to"}}, or "board".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<AreaOutline>,
    /// Priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Clearance to other nets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clearance: Option<Nm>,
    /// Minimum copper width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<Nm>,
    /// Pad connection: thermal, solid or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pads: Option<PadConnection>,
    /// Thermal relief gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_gap: Option<Nm>,
    /// Thermal spoke width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_spoke: Option<Nm>,
}

impl Command for ZoneSet {
    const NAME: &'static str = "zone.set";
    const SUMMARY: &'static str = "Change a zone: name, net, layers, outline, priority, clearance, min width, thermals";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Zones;

    fn run(self, ctx: &mut Context<'_>) -> Result<Zones, CommandError> {
        let p = ctx.project()?;
        let i = zone_index(p, &self.name)?;
        if let Some(n) = &self.rename {
            check_name(p, n, "zone", Some(i))?;
        }
        let net = self.net.as_deref().map(|n| super::net::net_name(p, n)).transpose()?;
        let layers = match &self.layers {
            Some(l) if l.is_empty() => {
                return Err(CommandError::invalid_args("zone.invalid_layer", "give at least one copper layer"));
            }
            Some(l) => Some(resolve_layers(p, l, "zone.invalid_layer")?),
            None => None,
        };
        let outline = self.outline.as_ref().map(|o| resolve_outline(p, o, "zone")).transpose()?;
        check_len(self.clearance, "clearance", false)?;
        check_len(self.min_width, "min_width", false)?;
        check_len(self.thermal_gap, "thermal_gap", false)?;
        check_len(self.thermal_spoke, "thermal_spoke", true)?;
        let z = &mut ctx.project_mut()?.board_mut().zones[i];
        if let Some(n) = self.rename {
            z.name = n;
        }
        if let Some(n) = net {
            z.net = Some(n);
        }
        if let Some(l) = layers {
            z.layers = l;
        }
        if let Some(o) = outline {
            z.outline = o;
        }
        if let Some(v) = self.priority {
            z.priority = v;
        }
        if self.clearance.is_some() {
            z.clearance = self.clearance;
        }
        if self.min_width.is_some() {
            z.min_width = self.min_width;
        }
        if let Some(v) = self.pads {
            z.pads = v;
        }
        if self.thermal_gap.is_some() {
            z.thermal_gap = self.thermal_gap;
        }
        if self.thermal_spoke.is_some() {
            z.thermal_spoke = self.thermal_spoke;
        }
        Ok(Zones { zones: vec![z.clone()] })
    }

    fn summarize(o: &Zones) -> String {
        zones_text(o)
    }
}

/// Remove a zone.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneRemove {
    /// Zone name.
    pub name: String,
}

impl Command for ZoneRemove {
    const NAME: &'static str = "zone.remove";
    const SUMMARY: &'static str = "Remove a zone";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Zones;

    fn run(self, ctx: &mut Context<'_>) -> Result<Zones, CommandError> {
        let i = zone_index(ctx.project()?, &self.name)?;
        let z = ctx.project_mut()?.board_mut().zones.remove(i);
        Ok(Zones { zones: vec![z] })
    }

    fn summarize(o: &Zones) -> String {
        format!("removed {}", o.zones.iter().map(|z| format!("zone:{}", z.name)).collect::<Vec<_>>().join(", "))
    }
}

/// List zones.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneList {}

impl Command for ZoneList {
    const NAME: &'static str = "zone.list";
    const SUMMARY: &'static str = "List copper zones";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Zones;

    fn run(self, ctx: &mut Context<'_>) -> Result<Zones, CommandError> {
        Ok(Zones { zones: ctx.project()?.board().zones.clone() })
    }

    fn summarize(o: &Zones) -> String {
        zones_text(o)
    }
}

/// Compute zone fills and report them (fills are derived data, recomputed on demand).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneFill {
    /// Only this zone (default: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The fill of one zone layer.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayerFill {
    /// Zone name.
    pub zone: String,
    /// Net.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Copper layer.
    pub layer: String,
    /// Filled copper area, mm² (rounded to 0.001).
    pub area_mm2: f64,
    /// Number of separate copper islands.
    pub islands: usize,
    /// Number of polygon vertices (output complexity).
    pub vertices: usize,
    /// Geometry error, if the fill failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Zone fill report.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FillReport {
    /// Per zone and layer, in board order.
    pub fills: Vec<LayerFill>,
}

impl Command for ZoneFill {
    const NAME: &'static str = "zone.fill";
    const SUMMARY: &'static str =
        "Compute zone fills: area and islands per zone/layer, warnings for empty or split zones";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = FillReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<FillReport, CommandError> {
        let p = ctx.project()?;
        let only = self.name.as_deref().map(|n| zone_index(p, n).map(|i| p.board().zones[i].id)).transpose()?;
        let base = geo::base_copper_items(p);
        let fills = zones::fill_zones(p, &base);
        let mut out = Vec::new();
        let mut diags = Vec::new();
        for f in fills.into_iter().filter(|f| only.is_none_or(|id| id == f.zone)) {
            let subject = ObjectRef::Named { kind: "zone".into(), name: f.name.clone() };
            if let Some(e) = &f.error {
                diags.push(
                    Diagnostic::error("zone.fill_failed", format!("zone:{} on {}: fill failed: {e}", f.name, f.layer))
                        .with_subject(subject.clone())
                        .with_hint("check the zone and board outlines for degenerate or self-crossing shapes"),
                );
            } else if f.fill.is_empty() {
                diags.push(
                    Diagnostic::warning("zone.empty", format!("zone:{} on {} has no copper", f.name, f.layer))
                        .with_subject(subject.clone())
                        .with_hint(
                            "the outline may miss the board, clearances or keep-outs may cover it, or no pad, via or \
                             track of its net touches it (unconnected copper is removed): add a via of the net inside",
                        ),
                );
            } else if f.fill.len() > 1 {
                diags.push(
                    Diagnostic::warning(
                        "zone.split",
                        format!("zone:{} on {} is split into {} islands", f.name, f.layer, f.fill.len()),
                    )
                    .with_subject(subject)
                    .with_hint("join the pieces with vias or tracks of the net, or move what divides them"),
                );
            }
            out.push(LayerFill {
                zone: f.name.clone(),
                net: f.net.clone(),
                layer: f.layer.clone(),
                area_mm2: (zones::area_mm2(&f.fill) * 1000.0).round() / 1000.0,
                islands: f.fill.len(),
                vertices: f.fill.iter().map(|q| q.vertex_count()).sum(),
                error: f.error,
            });
        }
        for d in diags {
            ctx.report(d);
        }
        Ok(FillReport { fills: out })
    }

    fn summarize(o: &FillReport) -> String {
        if o.fills.is_empty() {
            return "no zones".into();
        }
        o.fills
            .iter()
            .map(|f| {
                format!(
                    "zone:{} {} on {}: {:.3} mm², {} island(s){}",
                    f.zone,
                    f.net.as_deref().unwrap_or("(no net)"),
                    f.layer,
                    f.area_mm2,
                    f.islands,
                    f.error.as_ref().map(|e| format!(" (failed: {e})")).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---- keep-outs ----

/// Keep-outs, for results.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Keepouts {
    /// Keep-outs.
    pub keepouts: Vec<Keepout>,
}

fn keepouts_text(o: &Keepouts) -> String {
    if o.keepouts.is_empty() {
        return "no keep-outs".into();
    }
    o.keepouts
        .iter()
        .map(|k| {
            let mut what = Vec::new();
            for (b, n) in
                [(k.no_tracks, "tracks"), (k.no_vias, "vias"), (k.no_pours, "pours"), (k.no_footprints, "footprints")]
            {
                if b {
                    what.push(n);
                }
            }
            let layers = if k.layers.is_empty() { "all layers".to_string() } else { k.layers.join(" ") };
            format!("keepout:{} on {layers}: no {}", k.name, what.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Add a keep-out area. With no `no_*` flag given, everything is forbidden.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeepoutAdd {
    /// Unique name.
    pub name: String,
    /// Outline: points, {"rect": {"from", "to"}}, or "board".
    pub outline: AreaOutline,
    /// Copper layers (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Forbid tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_tracks: Option<bool>,
    /// Forbid vias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_vias: Option<bool>,
    /// Forbid copper pours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_pours: Option<bool>,
    /// Forbid footprints (courtyards).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_footprints: Option<bool>,
}

impl Command for KeepoutAdd {
    const NAME: &'static str = "keepout.add";
    const SUMMARY: &'static str = "Add a keep-out area forbidding tracks, vias, pours and/or footprints";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Keepouts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Keepouts, CommandError> {
        let p = ctx.project()?;
        check_name(p, &self.name, "keepout", None)?;
        let layers = resolve_layers(p, &self.layers, "keepout.invalid_layer")?;
        let outline = resolve_outline(p, &self.outline, "keepout")?;
        let flags = [self.no_tracks, self.no_vias, self.no_pours, self.no_footprints];
        let all = flags.iter().all(Option::is_none);
        let f = |v: Option<bool>| v.unwrap_or(all);
        if !all && !flags.contains(&Some(true)) {
            return Err(CommandError::invalid_args("keepout.nothing_forbidden", "the keep-out forbids nothing")
                .with_hint("set at least one of no_tracks, no_vias, no_pours, no_footprints to true"));
        }
        let p = ctx.project_mut()?;
        let id = p.alloc_id();
        let k = Keepout {
            id,
            name: self.name,
            layers,
            outline,
            no_tracks: f(self.no_tracks),
            no_vias: f(self.no_vias),
            no_pours: f(self.no_pours),
            no_footprints: f(self.no_footprints),
        };
        p.board_mut().keepouts.push(k.clone());
        Ok(Keepouts { keepouts: vec![k] })
    }

    fn summarize(o: &Keepouts) -> String {
        format!("added {}", keepouts_text(o))
    }
}

/// Remove a keep-out area.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeepoutRemove {
    /// Keep-out name.
    pub name: String,
}

impl Command for KeepoutRemove {
    const NAME: &'static str = "keepout.remove";
    const SUMMARY: &'static str = "Remove a keep-out area";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Keepouts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Keepouts, CommandError> {
        let p = ctx.project()?;
        let name = self.name.strip_prefix("keepout:").unwrap_or(&self.name);
        let ks = &p.board().keepouts;
        let Some(i) = ks
            .iter()
            .position(|k| k.name == name)
            .or_else(|| ks.iter().position(|k| k.name.eq_ignore_ascii_case(name)))
        else {
            let s = did_you_mean(name, ks.iter().map(|k| k.name.as_str()), 3);
            return Err(CommandError::not_found("keepout.not_found", format!("no keep-out `{name}`"))
                .with_subject(ObjectRef::Named { kind: "keepout".into(), name: name.into() })
                .with_suggestions(&s)
                .with_hint_if_none("list keep-outs with `keepout.list`"));
        };
        let k = ctx.project_mut()?.board_mut().keepouts.remove(i);
        Ok(Keepouts { keepouts: vec![k] })
    }

    fn summarize(o: &Keepouts) -> String {
        format!("removed {}", o.keepouts.iter().map(|k| format!("keepout:{}", k.name)).collect::<Vec<_>>().join(", "))
    }
}

/// List keep-out areas.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeepoutList {}

impl Command for KeepoutList {
    const NAME: &'static str = "keepout.list";
    const SUMMARY: &'static str = "List keep-out areas";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Keepouts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Keepouts, CommandError> {
        Ok(Keepouts { keepouts: ctx.project()?.board().keepouts.clone() })
    }

    fn summarize(o: &Keepouts) -> String {
        keepouts_text(o)
    }
}
