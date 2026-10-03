//! `route.*`: the autorouter (`crate::router`, docs/ROUTER.md) and routing status.

use std::collections::BTreeMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{self as geo, RatLine};
use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::diag::Diagnostic;
use crate::geom::Point;
use crate::model::Project;
use crate::refs::ObjectRef;
use crate::router::{self, ConnStatus, ConnectionReport, Effort, Hooks, RouteError, RouteOptions, RouteStats, Scope};
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<RouteAll>()
        .register::<RouteNets>()
        .register::<RouteConnection>()
        .register::<RouteRip>()
        .register::<RouteStatus>()
        .register::<ImportSes>();
}

/// Apply a Specctra session (`.ses`, the result of an external autorouter run on the design
/// from `export.dsn`): its wires and vias become tracks and vias, nets matched by name.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportSes {
    /// Session file (relative paths are relative to the project directory).
    pub path: std::path::PathBuf,
    /// Keep the existing unlocked tracks and vias of the session's nets and only add what is
    /// new (default: replace them with the session's wiring). Locked items always stay.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_existing: bool,
}

/// Result of `route.import_ses`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SesImported {
    /// Nets with wiring in the session.
    pub nets: usize,
    /// Track segments added.
    pub tracks_added: usize,
    /// Vias added.
    pub vias_added: usize,
    /// Existing tracks removed (replaced by the session).
    pub tracks_removed: usize,
    /// Existing vias removed.
    pub vias_removed: usize,
    /// Session items already on the board (protected wiring), not added again.
    pub duplicates: usize,
    /// Unrouted connections after the import.
    pub unrouted: usize,
}

impl Command for ImportSes {
    const NAME: &'static str = "route.import_ses";
    const SUMMARY: &'static str = "Apply a Specctra session (.ses) from an external autorouter: tracks and vias";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = SesImported;

    fn run(self, ctx: &mut Context<'_>) -> Result<SesImported, CommandError> {
        use crate::specctra::ses::{ImportError, ImportOptions, Session, plan};
        let path = match ctx.session.root() {
            Some(root) if self.path.is_relative() => root.join(&self.path),
            _ => self.path.clone(),
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CommandError::from(crate::model::ModelError::Io { path: path.clone(), source: e }))?;
        let session = Session::parse(&text).map_err(|e| {
            CommandError::invalid_args("ses.parse", format!("{}: {e}", path.display()))
                .with_hint("give the session file (.ses) the router wrote for the design from `export.dsn`")
        })?;
        let p = ctx.project()?;
        let plan = plan(p, &session, &ImportOptions { keep_existing: self.keep_existing }).map_err(|e| {
            let msg = e.to_string();
            match e {
                ImportError::UnknownNet { name, suggestions } => CommandError::not_found("ses.unknown_net", msg)
                    .with_subject(ObjectRef::Net(name))
                    .with_suggestions(&suggestions)
                    .with_hint("the session belongs to another design: export the DSN again and re-route it"),
                ImportError::UnknownLayer(l) => CommandError::invalid_args("ses.unknown_layer", msg)
                    .with_subject(ObjectRef::Layer(l))
                    .with_hint("the board's copper layers changed since the DSN export: export and route again"),
                ImportError::UnknownPadstack(ps) => CommandError::invalid_args("ses.unknown_padstack", msg)
                    .with_subject(ObjectRef::Named { kind: "padstack".into(), name: ps })
                    .with_hint("use the via padstacks of the exported DSN, or a router that writes `library_out`"),
            }
        })?;
        for d in plan.warnings.iter().cloned() {
            ctx.report(d);
        }
        let pm = ctx.project_mut()?;
        let board = pm.board_mut();
        board.tracks.retain(|t| !plan.remove_tracks.contains(&t.id));
        board.vias.retain(|v| !plan.remove_vias.contains(&v.id));
        let (tracks_added, vias_added) = (plan.tracks.len(), plan.vias.len());
        for mut t in plan.tracks {
            t.id = pm.alloc_id();
            pm.board_mut().tracks.push(t);
        }
        for mut v in plan.vias {
            v.id = pm.alloc_id();
            pm.board_mut().vias.push(v);
        }
        let unrouted = geo::ratsnest(ctx.project()?).len();
        Ok(SesImported {
            nets: plan.nets,
            tracks_added,
            vias_added,
            tracks_removed: plan.remove_tracks.len(),
            vias_removed: plan.remove_vias.len(),
            duplicates: plan.duplicates,
            unrouted,
        })
    }

    fn summarize(o: &SesImported) -> String {
        let mut s = format!(
            "imported {} net(s): added {} track segment(s) and {} via(s), removed {} track(s) and {} via(s)",
            o.nets, o.tracks_added, o.vias_added, o.tracks_removed, o.vias_removed
        );
        if o.duplicates > 0 {
            s.push_str(&format!(", {} already on the board", o.duplicates));
        }
        s.push_str(&format!("; {} unrouted connection(s) left", o.unrouted));
        s
    }
}

/// Default time budget.
const DEFAULT_BUDGET_MS: u64 = 60_000;

/// What the router did.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Routed {
    /// Numbers: connections, routed, failed, completion %, tracks, vias, length, iterations.
    pub stats: RouteStats,
    /// Every connection attempted, with failure reasons, locations and hints.
    pub connections: Vec<ConnectionReport>,
}

fn summary(o: &Routed) -> String {
    let s = &o.stats;
    let mut out = format!(
        "routed {}/{} connections ({}%): {} track segment(s), {} via(s), {} of track",
        s.routed, s.connections, s.completion, s.tracks, s.vias, s.length
    );
    for c in o.connections.iter().filter(|c| c.status == ConnStatus::Failed) {
        out.push_str(&format!(
            "\nfailed {}: {} -> {}: {}",
            c.net,
            c.from,
            c.to,
            c.reason.as_deref().unwrap_or("unknown reason")
        ));
        if let Some(h) = c.hints.first() {
            out.push_str(&format!(" (hint: {h})"));
        }
    }
    out
}

fn route_error(e: RouteError) -> CommandError {
    match e {
        RouteError::NoOutline => CommandError::conflict("board.no_outline", "the board has no outline")
            .with_hint("set one with `board.outline`"),
        RouteError::InvalidLayer(l) => {
            CommandError::invalid_args("route.invalid_layer", format!("`{l}` is not a copper layer of the board"))
                .with_hint("list copper layers with `board.info`")
        }
        RouteError::UnknownNet(n) => CommandError::not_found("net.not_found", format!("no net `{n}`"))
            .with_subject(ObjectRef::Net(n))
            .with_hint("list nets with `net.list`"),
        RouteError::Endpoint(m) => CommandError::invalid_args("route.invalid_endpoint", m)
            .with_hint("give two placed pins of the same net, e.g. U1.3 and C2.1"),
        RouteError::Cancelled => CommandError::new(ErrorKind::Cancelled, "cancelled", "operation cancelled"),
    }
}

fn options(budget_ms: Option<u64>, layers: &[String], effort: Option<Effort>, seed: Option<u64>) -> RouteOptions {
    RouteOptions {
        budget: Some(Duration::from_millis(budget_ms.unwrap_or(DEFAULT_BUDGET_MS))),
        layers: layers.to_vec(),
        effort: effort.unwrap_or_default(),
        seed: seed.unwrap_or(0),
        ..Default::default()
    }
}

/// Runs the router and applies its result to the project.
fn run_router(ctx: &mut Context<'_>, scope: Scope, opts: RouteOptions) -> Result<Routed, CommandError> {
    let result = {
        let p = ctx.project()?;
        let progress = |done: u64, total: Option<u64>, msg: &str| ctx.progress(done, total, msg);
        let cancelled = || ctx.check_cancelled().is_err();
        router::route(p, &scope, &opts, &Hooks { progress: &progress, cancelled: &cancelled }).map_err(route_error)?
    };
    let pm = ctx.project_mut()?;
    for mut t in result.tracks {
        t.id = pm.alloc_id();
        pm.board_mut().tracks.push(t);
    }
    for mut v in result.vias {
        v.id = pm.alloc_id();
        pm.board_mut().vias.push(v);
    }
    let failed: Vec<&ConnectionReport> = result.connections.iter().filter(|c| c.status == ConnStatus::Failed).collect();
    if !failed.is_empty() {
        let mut d = Diagnostic::warning(
            "route.incomplete",
            format!("{} of {} connection(s) could not be routed", failed.len(), result.connections.len()),
        )
        .with_hint(
            failed
                .iter()
                .find_map(|c| c.hints.first().cloned())
                .unwrap_or_else(|| "see the per-connection reasons".into()),
        );
        if let Some(at) = failed[0].at {
            d = d.at(at);
        }
        for c in failed.iter().take(10) {
            let s = ObjectRef::Net(c.net.clone());
            if !d.subjects.contains(&s) {
                d = d.with_subject(s);
            }
        }
        ctx.report(d);
    }
    Ok(Routed { stats: result.stats, connections: result.connections })
}

/// Route every unrouted connection on the board.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteAll {
    /// Time budget in milliseconds (default 60000); the best result so far is kept when it
    /// runs out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ms: Option<u64>,
    /// Copper layers to route on (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Effort: low, normal (default), high.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Seed for the reroute order (default 0); same input and seed give the same result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl Command for RouteAll {
    const NAME: &'static str = "route.all";
    const SUMMARY: &'static str = "Autoroute every unrouted connection (grid router, negotiated congestion)";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Routed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Routed, CommandError> {
        let opts = options(self.budget_ms, &self.layers, self.effort, self.seed);
        run_router(ctx, Scope::All, opts)
    }

    fn summarize(o: &Routed) -> String {
        summary(o)
    }
}

/// Route the unrouted connections of some nets (other copper stays as it is).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteNets {
    /// Nets to route; a net class name (`class:power` or a bare class name that is not a net)
    /// stands for all its nets.
    pub nets: Vec<String>,
    /// Time budget in milliseconds (default 60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ms: Option<u64>,
    /// Copper layers to route on (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Effort: low, normal (default), high.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Seed (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

impl Command for RouteNets {
    const NAME: &'static str = "route.nets";
    const SUMMARY: &'static str = "Autoroute the unrouted connections of some nets";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["nets"];
    type Output = Routed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Routed, CommandError> {
        if self.nets.is_empty() {
            return Err(CommandError::invalid_args("route.no_nets", "give at least one net"));
        }
        let p = ctx.project()?;
        let mut nets = Vec::new();
        for n in &self.nets {
            let class = n.strip_prefix("class:").unwrap_or(n);
            let c = p.circuit();
            if (n.starts_with("class:") || !c.nets.contains_key(n.as_str())) && c.netclasses.contains_key(class) {
                nets.extend(
                    c.nets.iter().filter(|(_, net)| net.class.as_deref() == Some(class)).map(|(k, _)| k.clone()),
                );
            } else {
                nets.push(super::net::net_name(p, n)?);
            }
        }
        nets.sort();
        nets.dedup();
        let opts = options(self.budget_ms, &self.layers, self.effort, self.seed);
        run_router(ctx, Scope::Nets(nets), opts)
    }

    fn summarize(o: &Routed) -> String {
        summary(o)
    }
}

/// Pad label (`U1.3`) of a pin given as `U1.3` or `U1.VIN`.
fn pad_label(p: &Project, pin: &str) -> Result<String, CommandError> {
    let pins = crate::connect::resolve_pins(p, pin)?;
    let pin = pins.first().expect("resolve returns at least one pin");
    let comp = &p.circuit().components[&pin.refdes];
    let part = &p.library().parts[&comp.part];
    let pads = part.footprint().map(|f| f.pads_for(&pin.pin)).unwrap_or_else(|| vec![pin.pin.clone()]);
    let placed = geo::placed_pads(p);
    let pp = placed.iter().find(|pp| pp.refdes == pin.refdes && pads.contains(&pp.number)).ok_or_else(|| {
        CommandError::conflict("place.not_placed", format!("{} is not placed on the board", pin.refdes))
    })?;
    Ok(format!("{}.{}", pp.refdes, pp.number))
}

/// Route one connection between two pins of a net.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteConnection {
    /// One pin ("U1.3" or "U1.VIN").
    pub from: String,
    /// The other pin, same net.
    pub to: String,
    /// Time budget in milliseconds (default 60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ms: Option<u64>,
    /// Copper layers to route on (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Effort: low, normal (default), high.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

impl Command for RouteConnection {
    const NAME: &'static str = "route.connection";
    const SUMMARY: &'static str = "Autoroute one connection between two pins of a net";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["from", "to"];
    type Output = Routed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Routed, CommandError> {
        let p = ctx.project()?;
        let (from, to) = (pad_label(p, &self.from)?, pad_label(p, &self.to)?);
        let opts = options(self.budget_ms, &self.layers, self.effort, None);
        run_router(ctx, Scope::Connection { from, to }, opts)
    }

    fn summarize(o: &Routed) -> String {
        if o.connections.is_empty() {
            return "already connected".into();
        }
        summary(o)
    }
}

/// Remove unlocked tracks and vias (of some nets, or all).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteRip {
    /// Nets whose routing to remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nets: Vec<String>,
    /// Remove all unlocked routing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

/// Removed routing.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Ripped {
    /// Tracks removed.
    pub tracks: usize,
    /// Vias removed.
    pub vias: usize,
    /// Locked items kept.
    pub locked_kept: usize,
}

impl Command for RouteRip {
    const NAME: &'static str = "route.rip";
    const SUMMARY: &'static str = "Remove unlocked tracks and vias of some nets, or all";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["nets"];
    type Output = Ripped;

    fn run(self, ctx: &mut Context<'_>) -> Result<Ripped, CommandError> {
        if self.nets.is_empty() && !self.all {
            return Err(CommandError::invalid_args("route.rip_what", "give nets to rip, or all: true")
                .with_hint("route.rip {\"nets\": [\"GND\"]} or route.rip {\"all\": true}"));
        }
        let p = ctx.project()?;
        let nets = self.nets.iter().map(|n| super::net::net_name(p, n)).collect::<Result<Vec<_>, _>>()?;
        let hit = |n: &Option<String>| self.all || n.as_ref().is_some_and(|n| nets.contains(n));
        let board = ctx.project_mut()?.board_mut();
        let (t0, v0) = (board.tracks.len(), board.vias.len());
        let locked = board.tracks.iter().filter(|t| t.locked && hit(&t.net)).count()
            + board.vias.iter().filter(|v| v.locked && hit(&v.net)).count();
        board.tracks.retain(|t| t.locked || !hit(&t.net));
        board.vias.retain(|v| v.locked || !hit(&v.net));
        Ok(Ripped { tracks: t0 - board.tracks.len(), vias: v0 - board.vias.len(), locked_kept: locked })
    }

    fn summarize(o: &Ripped) -> String {
        let mut s = format!("removed {} track(s) and {} via(s)", o.tracks, o.vias);
        if o.locked_kept > 0 {
            s.push_str(&format!(", kept {} locked item(s)", o.locked_kept));
        }
        s
    }
}

/// Routing status: unrouted connections, completion, problem areas.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteStatus {}

/// An area with many unrouted connections.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProblemArea {
    /// Center of the area (5 mm tiles).
    pub at: Point,
    /// Unrouted connections with their midpoint in the area.
    pub unrouted: usize,
    /// Nets involved.
    pub nets: Vec<String>,
    /// Components involved.
    pub components: Vec<String>,
}

/// Routing status.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Status {
    /// Connections needed to join every net's pads (pads − 1 per net).
    pub connections: usize,
    /// Unrouted connections (ratsnest lines).
    pub unrouted: usize,
    /// Routed share in percent.
    pub completion: f64,
    /// Track segments on the board.
    pub tracks: usize,
    /// Vias on the board.
    pub vias: usize,
    /// Total track length.
    pub length: Nm,
    /// Unrouted connections by net (count).
    pub unrouted_by_net: BTreeMap<String, usize>,
    /// The unrouted connections.
    pub lines: Vec<RatLine>,
    /// Areas with the most unrouted connections, worst first.
    pub problem_areas: Vec<ProblemArea>,
}

const TILE: i64 = 5_000_000;

/// Per tile: unrouted count, nets, components.
type Tile = (usize, Vec<String>, Vec<String>);

impl Command for RouteStatus {
    const NAME: &'static str = "route.status";
    const SUMMARY: &'static str = "Routing status: unrouted connections, completion %, problem areas";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Status;

    fn run(self, ctx: &mut Context<'_>) -> Result<Status, CommandError> {
        let p = ctx.project()?;
        let mut pads_by_net: BTreeMap<String, usize> = BTreeMap::new();
        for pp in geo::placed_pads(p) {
            if let Some(n) = pp.net
                && !pp.layers.is_empty()
            {
                *pads_by_net.entry(n).or_default() += 1;
            }
        }
        let connections: usize = pads_by_net.values().map(|k| k.saturating_sub(1)).sum();
        let lines = geo::ratsnest(p);
        let unrouted = lines.len();
        let mut by_net: BTreeMap<String, usize> = BTreeMap::new();
        let mut tiles: BTreeMap<(i64, i64), Tile> = BTreeMap::new();
        for l in &lines {
            *by_net.entry(l.net.clone()).or_default() += 1;
            let mx = (l.from_at.x.0 + l.to_at.x.0) / 2;
            let my = (l.from_at.y.0 + l.to_at.y.0) / 2;
            let e = tiles.entry((mx.div_euclid(TILE), my.div_euclid(TILE))).or_default();
            e.0 += 1;
            if !e.1.contains(&l.net) {
                e.1.push(l.net.clone());
            }
            for end in [&l.from, &l.to] {
                if let Some((c, _)) = end.split_once('.')
                    && !end.contains('#')
                    && !e.2.iter().any(|x| x == c)
                {
                    e.2.push(c.to_string());
                }
            }
        }
        let mut areas: Vec<ProblemArea> = tiles
            .into_iter()
            .map(|((tx, ty), (n, mut nets, mut comps))| {
                nets.sort();
                comps.sort_by(|a, b| crate::model::sections::natural_cmp(a, b));
                ProblemArea {
                    at: Point::new(Nm(tx * TILE + TILE / 2), Nm(ty * TILE + TILE / 2)),
                    unrouted: n,
                    nets,
                    components: comps,
                }
            })
            .collect();
        areas.sort_by(|a, b| b.unrouted.cmp(&a.unrouted).then(a.at.cmp(&b.at)));
        areas.truncate(5);
        let board = p.board();
        let length: f64 = board
            .tracks
            .iter()
            .map(|t| {
                let (dx, dy) = ((t.end.x.0 - t.start.x.0) as f64, (t.end.y.0 - t.start.y.0) as f64);
                (dx * dx + dy * dy).sqrt()
            })
            .sum();
        let done = connections.saturating_sub(unrouted);
        Ok(Status {
            connections,
            unrouted,
            completion: if connections == 0 {
                100.0
            } else {
                (done as f64 * 1000.0 / connections as f64).round() / 10.0
            },
            tracks: board.tracks.len(),
            vias: board.vias.len(),
            length: Nm(length.round() as i64),
            unrouted_by_net: by_net,
            lines,
            problem_areas: areas,
        })
    }

    fn summarize(o: &Status) -> String {
        let mut s = format!(
            "{}% routed: {} of {} connection(s) unrouted; {} track segment(s), {} via(s), {} of track",
            o.completion, o.unrouted, o.connections, o.tracks, o.vias, o.length
        );
        for a in &o.problem_areas {
            s.push_str(&format!(
                "\nproblem area around ({}, {}): {} unrouted ({}; {})",
                a.at.x,
                a.at.y,
                a.unrouted,
                a.nets.join(", "),
                a.components.join(", ")
            ));
        }
        s
    }
}
