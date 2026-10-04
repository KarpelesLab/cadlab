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
use crate::router::{
    self, ConnStatus, ConnectionReport, Effort, GroupTuned, Hooks, MeanderStyle, PairReport, PairStatus, PlaceMode,
    RouteError, RouteOptions, RouteStats, Scope, SkewTuned, TrackRequest, TuneOptions,
};
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<RouteAll>()
        .register::<RouteNets>()
        .register::<RouteConnection>()
        .register::<RouteTrack>()
        .register::<RouteFanout>()
        .register::<RouteRip>()
        .register::<RouteStatus>()
        .register::<RouteDiffpair>()
        .register::<RouteTune>()
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
    /// Nets whose unlocked routing was pushed aside or rerouted to make room (`shove`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rerouted: Vec<String>,
    /// Differential pairs, routed first as coupled traces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<PairReport>,
}

fn summary(o: &Routed) -> String {
    let s = &o.stats;
    let mut out = format!(
        "routed {}/{} connections ({}%): {} track segment(s), {} via(s), {} of track",
        s.routed, s.connections, s.completion, s.tracks, s.vias, s.length
    );
    if !o.rerouted.is_empty() {
        out.push_str(&format!("; moved {} to make room", o.rerouted.join(", ")));
    }
    for pr in &o.pairs {
        out.push_str(&format!("\n{}", pair_line(pr)));
    }
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
        RouteError::UnknownPair(n) => {
            CommandError::not_found("diffpair.not_found", format!("no differential pair `{n}`"))
                .with_subject(ObjectRef::Named { kind: "diffpair".into(), name: n })
                .with_hint("list pairs with `diffpair.list`; define one with `diffpair.add`")
        }
        RouteError::UnknownGroup(n) => {
            CommandError::not_found("lengthgroup.not_found", format!("no length group `{n}`"))
                .with_subject(ObjectRef::Named { kind: "lengthgroup".into(), name: n })
                .with_hint("list groups with `lengthgroup.list`; define one with `lengthgroup.set`")
        }
    }
}

/// One line about a pair.
fn pair_line(pr: &PairReport) -> String {
    let status = match pr.status {
        PairStatus::Routed => "routed coupled",
        PairStatus::Partial => "partly routed coupled",
        PairStatus::Failed => "not routed coupled",
        PairStatus::Nothing => "nothing to route coupled",
    };
    let mut s = format!(
        "pair {} ({}/{}): {status} ({}/{} connection(s){}), {} wide, {} gap; lengths {} / {}, skew {}, uncoupled {}",
        pr.name,
        pr.p,
        pr.n,
        pr.routed,
        pr.connections,
        if pr.layers.is_empty() { String::new() } else { format!(" on {}", pr.layers.join(", ")) },
        pr.width,
        pr.gap,
        pr.length_p,
        pr.length_n,
        pr.skew,
        pr.uncoupled
    );
    for f in &pr.failures {
        s.push_str(&format!("\n  failed {} -> {}: {}", f.from, f.to, f.reason.as_deref().unwrap_or("unknown reason")));
    }
    s
}

/// Warnings about pairs: failures, skew or uncoupled length over the pair's limits, and
/// connections left to the ordinary router.
fn report_pairs(ctx: &mut Context<'_>, pairs: &[PairReport], left_hint: bool) -> Result<(), CommandError> {
    let c = ctx.project()?.circuit().clone();
    for pr in pairs {
        let subj = || ObjectRef::Named { kind: "diffpair".into(), name: pr.name.clone() };
        for f in &pr.failures {
            let mut d = Diagnostic::warning(
                "route.diffpair_failed",
                format!("pair {}: {} -> {}: {}", pr.name, f.from, f.to, f.reason.as_deref().unwrap_or("not routed")),
            )
            .with_subject(subj())
            .with_hint(f.hints.first().cloned().unwrap_or_else(|| "move the parts to give the pair room".into()));
            if let Some(at) = f.at {
                d = d.at(at);
            }
            ctx.report(d);
        }
        let Some(dp) = c.diffpairs.get(&pr.name) else { continue };
        if let Some(m) = dp.max_skew
            && pr.routed > 0
            && pr.skew > m
        {
            ctx.report(
                Diagnostic::warning(
                    "route.diffpair_skew",
                    format!("pair {}: skew {} is over its {} limit", pr.name, pr.skew, m),
                )
                .with_subject(subj())
                .with_hint("run route.tune, or give the shorter net room for skew bumps"),
            );
        }
        if let Some(m) = dp.max_uncoupled
            && pr.routed > 0
            && pr.uncoupled > m
        {
            ctx.report(
                Diagnostic::warning(
                    "route.diffpair_uncoupled",
                    format!("pair {}: {} runs uncoupled, over its {} limit", pr.name, pr.uncoupled, m),
                )
                .with_subject(subj())
                .with_hint("place the pair's pads closer together and in line, or raise max_uncoupled"),
            );
        }
        if left_hint && pr.left > 0 {
            ctx.report(
                Diagnostic::info(
                    "route.diffpair_left",
                    format!(
                        "pair {}: {} connection(s) of {}/{} are not part of the coupled routing (pads without a partner)",
                        pr.name, pr.left, pr.p, pr.n
                    ),
                )
                .with_subject(subj())
                .with_hint(format!("route them with route.nets {} {} (or route.all)", pr.p, pr.n)),
            );
        }
    }
    Ok(())
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

/// Gridless refinement and arc options.
fn shape_options(
    opts: &mut RouteOptions,
    gridless: Option<bool>,
    arcs: Option<bool>,
    arc_radius: Option<Nm>,
) -> Result<(), CommandError> {
    if arc_radius.is_some_and(|r| r <= Nm::ZERO) {
        return Err(CommandError::invalid_args("route.invalid_arc_radius", "arc_radius must be positive")
            .with_hint("give a radius such as \"1mm\", or leave it out for the default"));
    }
    opts.gridless = gridless;
    opts.arcs = arcs.unwrap_or(false);
    opts.arc_radius = arc_radius;
    Ok(())
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
    let board = pm.board_mut();
    board.tracks.retain(|t| !result.removed_tracks.contains(&t.id));
    board.vias.retain(|v| !result.removed_vias.contains(&v.id));
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
    report_pairs(ctx, &result.pairs, false)?;
    Ok(Routed { stats: result.stats, connections: result.connections, rerouted: result.rerouted, pairs: result.pairs })
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
    /// Fan out BGAs (dog-bone vias) and escape off-grid fine-pitch pads before routing
    /// (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout: Option<bool>,
    /// Allow any-angle shortcuts when optimizing (default false: 0°/45°/90° only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any_angle: Option<bool>,
    /// Gridless refinement after optimization: shortest paths hugging the clearance of nearby
    /// obstacles, off the routing grid (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gridless: Option<bool>,
    /// Round every bend of the new tracks into a tangent arc (smooth / RF nets; default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arcs: Option<bool>,
    /// Largest arc radius with `arcs` (default 1mm); smaller where the rules require.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_radius: Option<Nm>,
    /// Route differential pairs first, as coupled traces, and compensate their skew
    /// (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairs: Option<bool>,
}

impl Command for RouteAll {
    const NAME: &'static str = "route.all";
    const SUMMARY: &'static str = "Autoroute every unrouted connection (grid router, negotiated congestion)";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Routed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Routed, CommandError> {
        let mut opts = options(self.budget_ms, &self.layers, self.effort, self.seed);
        opts.fanout = self.fanout;
        opts.any_angle = self.any_angle.unwrap_or(false);
        opts.pairs = self.pairs;
        shape_options(&mut opts, self.gridless, self.arcs, self.arc_radius)?;
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
    /// Fan out BGAs (dog-bone vias) and escape off-grid fine-pitch pads before routing
    /// (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout: Option<bool>,
    /// Allow any-angle shortcuts when optimizing (default false: 0°/45°/90° only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any_angle: Option<bool>,
    /// Gridless refinement after optimization: shortest paths hugging the clearance of nearby
    /// obstacles, off the routing grid (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gridless: Option<bool>,
    /// Round every bend of the new tracks into a tangent arc (smooth / RF nets; default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arcs: Option<bool>,
    /// Largest arc radius with `arcs` (default 1mm); smaller where the rules require.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_radius: Option<Nm>,
    /// Route differential pairs whose two nets are listed first, as coupled traces (default
    /// true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairs: Option<bool>,
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
        let mut opts = options(self.budget_ms, &self.layers, self.effort, self.seed);
        opts.fanout = self.fanout;
        opts.any_angle = self.any_angle.unwrap_or(false);
        opts.pairs = self.pairs;
        shape_options(&mut opts, self.gridless, self.arcs, self.arc_radius)?;
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
    /// When there is no room, move the unlocked routing of other nets out of the way:
    /// push-and-shove (bend their tracks, move their vias; DRC-checked), else rip it, route
    /// this connection and route those nets again (kept only if they end up no less routed
    /// than before). Locked items never move. Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shove: Option<bool>,
    /// Gridless refinement after optimization: shortest paths hugging the clearance of nearby
    /// obstacles, off the routing grid (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gridless: Option<bool>,
    /// Round every bend of the new tracks into a tangent arc (smooth / RF nets; default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arcs: Option<bool>,
    /// Largest arc radius with `arcs` (default 1mm); smaller where the rules require.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_radius: Option<Nm>,
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
        let mut opts = options(self.budget_ms, &self.layers, self.effort, None);
        opts.shove = self.shove.unwrap_or(true);
        shape_options(&mut opts, self.gridless, self.arcs, self.arc_radius)?;
        run_router(ctx, Scope::Connection { from, to }, opts)
    }

    fn summarize(o: &Routed) -> String {
        if o.connections.is_empty() {
            return "already connected".into();
        }
        summary(o)
    }
}

/// Place one track along waypoints with push-and-shove (interactive-style routing): the track
/// walks around pads, locked copper, keep-outs and the board edge, and pushes the unlocked
/// tracks and vias of other nets out of its way, which then spring back as far as the rules
/// allow. Locked items never move; the result is DRC-checked.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteTrack {
    /// Copper layer ("F.Cu").
    pub layer: String,
    /// Waypoints: coordinates ["10mm", "5mm"] or pins "U1.3" (pad center); at least two.
    pub points: Vec<super::board::TrackPoint>,
    /// Net (default: from a pin among the points).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Width (default: the net class's, else the rules' track width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Nm>,
    /// shove (default): walk around fixed things, push other nets' unlocked copper aside;
    /// walkaround: walk around everything, move nothing; strict: exactly along the points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PlaceMode>,
}

/// What `route.track` placed and moved.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TrackPlaced {
    /// Segments of the new track.
    pub segments: usize,
    /// Its length.
    pub length: Nm,
    /// Whether it detoured around something (walkaround).
    pub walked: bool,
    /// Nets whose copper was pushed aside.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shoved: Vec<String>,
    /// Track segments replaced by shoved geometry.
    pub tracks_moved: usize,
    /// Vias moved.
    pub vias_moved: usize,
}

impl Command for RouteTrack {
    const NAME: &'static str = "route.track";
    const SUMMARY: &'static str = "Place a track along waypoints, pushing other nets' unlocked routing aside";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = TrackPlaced;

    fn run(self, ctx: &mut Context<'_>) -> Result<TrackPlaced, CommandError> {
        let p = ctx.project()?;
        if self.points.len() < 2 {
            return Err(CommandError::invalid_args("track.too_few_points", "a track needs at least two points")
                .with_hint("give two or more points or pins"));
        }
        let mut pts = Vec::new();
        let mut nets = Vec::new();
        for tp in &self.points {
            let (pt, net) = super::board::resolve_point(p, tp)?;
            pts.push(pt);
            if let Some(n) = net
                && !nets.contains(&n)
            {
                nets.push(n);
            }
        }
        let net = match (&self.net, nets.as_slice()) {
            (Some(n), _) => super::net::net_name(p, n)?,
            (None, [one]) => one.clone(),
            (None, []) => {
                return Err(CommandError::invalid_args(
                    "route.track_no_net",
                    "no net given and no pin among the points",
                )
                .with_hint("give `net`, or start or end the track on a pin"));
            }
            (None, many) => {
                return Err(CommandError::conflict(
                    "track.short",
                    format!("the points connect different nets: {}", many.join(", ")),
                )
                .with_hint("a track joins one net; check the pins"));
            }
        };
        let width = self.width.unwrap_or_else(|| super::board::net_width(p, Some(&net)));
        if width <= Nm::ZERO {
            return Err(CommandError::invalid_args("track.invalid_width", "width must be positive"));
        }
        let req = TrackRequest { layer: self.layer.clone(), net, width, points: pts };
        let res = router::place_track(p, &req, self.mode.unwrap_or_default()).map_err(|e| {
            let kind = match e.code {
                "route.invalid_layer" | "track.too_few_points" => ErrorKind::InvalidArgs,
                "net.not_found" => ErrorKind::NotFound,
                _ => ErrorKind::Conflict,
            };
            let mut err = CommandError::new(kind, e.code, e.message).with_hint(e.hint);
            for s in e.subjects {
                err = err.with_subject(s);
            }
            if let Some(l) = e.layer {
                err = err.with_subject(ObjectRef::Layer(l));
            }
            if let Some(at) = e.at {
                err.diagnostic.location = Some(at);
            }
            err
        })?;
        let length: f64 = res.tracks[..res.placed].iter().map(|t| geo::track_length(t).0 as f64).sum();
        let out = TrackPlaced {
            segments: res.placed,
            length: Nm(length.round() as i64),
            walked: res.walked,
            shoved: res.shoved.clone(),
            tracks_moved: res.removed_tracks.len(),
            vias_moved: res.removed_vias.len(),
        };
        let pm = ctx.project_mut()?;
        let board = pm.board_mut();
        board.tracks.retain(|t| !res.removed_tracks.contains(&t.id));
        board.vias.retain(|v| !res.removed_vias.contains(&v.id));
        for mut t in res.tracks {
            t.id = pm.alloc_id();
            pm.board_mut().tracks.push(t);
        }
        for mut v in res.vias {
            v.id = pm.alloc_id();
            pm.board_mut().vias.push(v);
        }
        Ok(out)
    }

    fn summarize(o: &TrackPlaced) -> String {
        let mut s = format!("placed a track of {} segment(s), {}", o.segments, o.length);
        if o.walked {
            s.push_str(", detouring around obstacles");
        }
        if !o.shoved.is_empty() {
            s.push_str(&format!(
                "; pushed {} aside ({} track segment(s), {} via(s) moved)",
                o.shoved.join(", "),
                o.tracks_moved,
                o.vias_moved
            ));
        }
        s
    }
}

/// Fan out BGA footprints before routing: a short track from every inner ball to a via between
/// the balls (dog bone), and escape stubs for fine-pitch pads off the routing grid. Only pads
/// whose nets still have unrouted connections are fanned out; everything is DRC-checked.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteFanout {
    /// Components to fan out (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refdes: Vec<String>,
    /// Copper layers the router may use (default: all); fewer than two means no vias.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
}

/// What `route.fanout` added.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FannedOut {
    /// Pads given a fanout or escape (`U1.C3`).
    pub pads: Vec<String>,
    /// Track segments added.
    pub tracks: usize,
    /// Vias added.
    pub vias: usize,
}

impl Command for RouteFanout {
    const NAME: &'static str = "route.fanout";
    const SUMMARY: &'static str = "Fan out BGAs (dog-bone vias) and escape fine-pitch pads before routing";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = FannedOut;

    fn run(self, ctx: &mut Context<'_>) -> Result<FannedOut, CommandError> {
        let p = ctx.project()?;
        let refdes = self.refdes.iter().map(|r| super::util::refdes_key(p, r)).collect::<Result<Vec<_>, _>>()?;
        let result = router::fanout(p, &refdes, &self.layers).map_err(route_error)?;
        if result.pads.is_empty() {
            ctx.report(
                Diagnostic::info("route.nothing_to_fan_out", "no pad needs a fanout or escape")
                    .with_hint("fanout applies to BGA footprints and off-grid fine-pitch pads with unrouted nets"),
            );
        }
        let pm = ctx.project_mut()?;
        let (tracks, vias) = (result.tracks.len(), result.vias.len());
        for mut t in result.tracks {
            t.id = pm.alloc_id();
            pm.board_mut().tracks.push(t);
        }
        for mut v in result.vias {
            v.id = pm.alloc_id();
            pm.board_mut().vias.push(v);
        }
        Ok(FannedOut { pads: result.pads, tracks, vias })
    }

    fn summarize(o: &FannedOut) -> String {
        format!("fanned out {} pad(s): {} track segment(s), {} via(s)", o.pads.len(), o.tracks, o.vias)
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
        let length: f64 = board.tracks.iter().map(|t| geo::track_length(t).0 as f64).sum();
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

/// Route differential pairs as coupled traces: each pair's connections are routed as one
/// centerline at the pair's width and gap (net class `diff_pair_width` / `diff_pair_gap`, as
/// `impedance.solve` writes them), split into the two tracks, with short breakouts at the pads;
/// on one layer, no vias. Then the skew is compensated with small bumps on the shorter net.
/// Pads without a partner (a pull-up on one net) are left to `route.nets` / `route.all`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteDiffpair {
    /// Pairs to route (default: all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<String>,
    /// Copper layers the pairs may use (default: all; each connection stays on one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Time budget in milliseconds (default 60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ms: Option<u64>,
    /// Compensate the skew with small bumps on the shorter net (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skew: Option<bool>,
}

/// What `route.diffpair` did.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PairsRouted {
    /// Per pair: outcome, width, gap, lengths, skew, coupled and uncoupled length.
    pub pairs: Vec<PairReport>,
    /// Track segments added.
    pub tracks: usize,
    /// Track segments replaced.
    pub tracks_removed: usize,
}

impl Command for RouteDiffpair {
    const NAME: &'static str = "route.diffpair";
    const SUMMARY: &'static str =
        "Route differential pairs as coupled traces at their width and gap, then fix their skew";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["pairs"];
    type Output = PairsRouted;

    fn run(self, ctx: &mut Context<'_>) -> Result<PairsRouted, CommandError> {
        let p = ctx.project()?;
        if p.circuit().diffpairs.is_empty() {
            return Err(CommandError::conflict("diffpair.none", "the circuit has no differential pairs")
                .with_hint("define one with diffpair.add (diffpair.suggest finds them by net names)"));
        }
        let mut opts = options(self.budget_ms, &self.layers, None, None);
        opts.skew = self.skew;
        let result = {
            let progress = |done: u64, total: Option<u64>, msg: &str| ctx.progress(done, total, msg);
            let cancelled = || ctx.check_cancelled().is_err();
            router::route_diffpairs(p, &self.pairs, &opts, &Hooks { progress: &progress, cancelled: &cancelled })
                .map_err(route_error)?
        };
        let pm = ctx.project_mut()?;
        pm.board_mut().tracks.retain(|t| !result.removed_tracks.contains(&t.id));
        let (tracks, tracks_removed) = (result.tracks.len(), result.removed_tracks.len());
        for mut t in result.tracks {
            t.id = pm.alloc_id();
            pm.board_mut().tracks.push(t);
        }
        report_pairs(ctx, &result.pairs, true)?;
        Ok(PairsRouted { pairs: result.pairs, tracks, tracks_removed })
    }

    fn summarize(o: &PairsRouted) -> String {
        let mut s = format!("added {} track segment(s)", o.tracks);
        for p in &o.pairs {
            s.push_str(&format!("\n{}", pair_line(p)));
        }
        s
    }
}

/// Length tuning: add meanders so every length group member reaches its target (or the
/// longest member) within the tolerance, and compensate the skew of differential pairs with
/// small bumps on the shorter net. Pairs are meandered as pairs (both tracks together).
/// Meanders are checked exactly and by the DRC; a member whose meanders the DRC flags is
/// left as it was and reported. Tuning only adds length.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteTune {
    /// Length group to tune (default: all groups, and the skew of every pair).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Meander shape: trombone (default; U bumps on one side), accordion (U bumps alternating
    /// sides) or sawtooth (triangular teeth).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<MeanderStyle>,
    /// Largest bump height (default 1mm, at least 2.5 spacings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplitude: Option<Nm>,
    /// Center distance between neighboring meander legs (default 4 track widths, at least
    /// width + clearance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<Nm>,
    /// Corner size: chamfer or arc radius (default a quarter of the spacing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner: Option<Nm>,
    /// Round meander corners with arcs (default false: 45° chamfers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arcs: Option<bool>,
    /// Compensate differential pair skew (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skew: Option<bool>,
}

/// What `route.tune` achieved.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Tuned {
    /// Per length group: target, range, and per member the length before and after and the
    /// residual error.
    pub groups: Vec<GroupTuned>,
    /// Per differential pair: skew before and after.
    pub pairs: Vec<SkewTuned>,
    /// Track segments added.
    pub tracks: usize,
    /// Track segments replaced.
    pub tracks_removed: usize,
}

impl Command for RouteTune {
    const NAME: &'static str = "route.tune";
    const SUMMARY: &'static str = "Length tuning: meanders to bring length groups to target and fix pair skew";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["group"];
    type Output = Tuned;

    fn run(self, ctx: &mut Context<'_>) -> Result<Tuned, CommandError> {
        for (label, v) in [("amplitude", self.amplitude), ("spacing", self.spacing), ("corner", self.corner)] {
            if v.is_some_and(|v| v <= Nm::ZERO) {
                return Err(CommandError::invalid_args("route.invalid_meander", format!("`{label}` must be positive"))
                    .with_hint("leave it out for the default"));
            }
        }
        let p = ctx.project()?;
        let c = p.circuit();
        if c.length_groups.is_empty() && c.diffpairs.is_empty() {
            return Err(CommandError::conflict("route.nothing_to_tune", "no length groups and no differential pairs")
                .with_hint("define a group with lengthgroup.set or a pair with diffpair.add"));
        }
        let opts = TuneOptions {
            style: self.style.unwrap_or_default(),
            amplitude: self.amplitude,
            spacing: self.spacing,
            corner: self.corner,
            arcs: self.arcs.unwrap_or(false),
            skew: self.skew.unwrap_or(true),
        };
        let res = router::tune(p, self.group.as_deref(), &opts).map_err(route_error)?;
        for g in &res.groups {
            for m in &g.members {
                if let (Some(e), Some(why)) = (m.error, &m.reason)
                    && e != Nm::ZERO
                {
                    ctx.report(
                        Diagnostic::warning(
                            "route.tune_unmet",
                            format!(
                                "length group {}: {} {} is {} off its range: {why}",
                                g.name,
                                m.kind,
                                m.name,
                                e.abs()
                            ),
                        )
                        .with_subject(ObjectRef::Named { kind: "lengthgroup".into(), name: g.name.clone() })
                        .with_subject(ObjectRef::Name(m.name.clone()))
                        .with_hint(
                            "raise amplitude or lower spacing, move nearby copper, or rip and route the member again",
                        ),
                    );
                }
            }
        }
        for s in &res.pairs {
            if let Some(why) = &s.reason {
                ctx.report(
                    Diagnostic::warning("route.tune_skew", format!("pair {}: skew {} left: {why}", s.name, s.after))
                        .with_subject(ObjectRef::Named { kind: "diffpair".into(), name: s.name.clone() })
                        .with_hint("move nearby copper away from the shorter net, or route the pair again"),
                );
            }
        }
        let pm = ctx.project_mut()?;
        pm.board_mut().tracks.retain(|t| !res.removed_tracks.contains(&t.id));
        let (tracks, tracks_removed) = (res.tracks.len(), res.removed_tracks.len());
        for mut t in res.tracks {
            t.id = pm.alloc_id();
            pm.board_mut().tracks.push(t);
        }
        Ok(Tuned { groups: res.groups, pairs: res.pairs, tracks, tracks_removed })
    }

    fn summarize(o: &Tuned) -> String {
        let mut s = format!("added {} track segment(s), replaced {}", o.tracks, o.tracks_removed);
        let opt = |v: Option<Nm>| v.map_or("-".to_string(), |x| x.to_string());
        for g in &o.groups {
            s.push_str(&format!(
                "\ngroup {}: target {} (range {} .. {})",
                g.name,
                opt(g.target),
                opt(g.min),
                opt(g.max)
            ));
            for m in &g.members {
                s.push_str(&format!(
                    "\n  {} {}: {} -> {}{}{}",
                    m.kind,
                    m.name,
                    opt(m.before),
                    opt(m.after),
                    match m.error {
                        Some(e) if e != Nm::ZERO => format!(" ({} off)", e),
                        Some(_) => " (ok)".to_string(),
                        None => String::new(),
                    },
                    if m.meanders > 0 { format!(", {} meander(s)", m.meanders) } else { String::new() }
                ));
            }
        }
        for p in &o.pairs {
            s.push_str(&format!("\npair {}: skew {} -> {}", p.name, p.before, p.after));
            if p.bumps > 0 {
                s.push_str(&format!(" ({} bump(s))", p.bumps));
            }
        }
        s
    }
}
