//! Autorouter v1: grid-based maze routing with negotiated congestion. See `docs/ROUTER.md`.
//!
//! [`route`] takes a project and returns new tracks and vias plus a report per connection; it
//! never changes the project (the `route.*` commands apply the result). Pipeline:
//!
//! 1. **Preprocess** (`model`): copper islands and the connections joining them (minimum
//!    spanning tree per net, like the ratsnest; already-connected copper is never re-routed),
//!    obstacles with the clearance each demands (other-net copper, board edge, cutouts,
//!    keep-outs, NPTH holes) and drilled holes for via spacing.
//! 2. **Grid** (`grid`): pitch (track width + clearance) / 2 of the finest routed net class,
//!    aligned with the most pad centers; static legality maps per rule profile, sampled at half
//!    the pitch with an inflation margin that makes the sampling exact.
//!    **Fanout** (`fanout`, on that grid, before routing): dog-bone vias for BGA balls,
//!    staggered escapes for off-grid fine-pitch pads, planned exactly, then existing copper.
//! 3. **Search** (`engine`): A* with 8 directions, costs for length, bends (45° cheap, 90°
//!    dearer, sharper forbidden), vias, wrong-way moves per layer (alternating horizontal /
//!    vertical preference) and congestion; multi-source/multi-target so nets grow as trees.
//! 4. **Negotiated congestion** (PathFinder, McMurchie and Ebeling 1995): route everything
//!    allowing overlaps at a cost, raise history costs on overused points, rip up and reroute
//!    the nets involved until nothing is shared; then legalize what still conflicts and retry
//!    failures by rerouting the nets in their way. Nets with disjoint regions are routed in
//!    batches (on rayon threads with the `parallel` feature); the batches depend only on the
//!    data, so results are identical for any thread count.
//!    Then via minimization: nets with vias are routed again with dear vias, kept when better.
//! 5. **Post-processing** (`post`): via reduction, collinear merging, 45° pull-tight, mitered
//!    corners and optional any-angle shortcuts; gridless refinement (`gridless`: shortest paths
//!    over the clearance hulls of nearby obstacles); optional arc corners (`arcs`). Each step is
//!    validated exactly against all other copper.
//! 6. **Verification**: `crate::drc::check` on the result; any routed item with a DRC error is
//!    ripped up and reported, so the output never violates the rules silently.
//! 7. **Push-and-shove** (`shove`) for what is left: connections that failed get one more try
//!    with the unlocked copper of other nets pushed aside (walkaround, shove, spring-back);
//!    [`place_track`] places a track along waypoints the same way.
//!
//! Deterministic for a given input, options and seed (unless the time budget cuts it short).

mod arcs;
mod engine;
mod fanout;
mod geo;
mod grid;
mod gridless;
mod index;
mod model;
mod post;
mod shove;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{self as geo_board};
use crate::geom::Point;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{Track, Via};
use crate::refs::ObjectRef;
use crate::units::Nm;

use engine::{Access, Conn, Costs, Engine, FailKind, Island, Mode, NetRoute, Rng, Scratch, Stop};
use geo::{BoxF, P};
use index::{Blocker, Checker, Index, Item};
use model::Clear;
use model::{ObKind, RouterBoard};

/// How hard to try.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Few negotiation iterations, weighted search.
    Low,
    /// The default.
    #[default]
    Normal,
    /// Many iterations, more post-processing.
    High,
}

/// Router options.
#[derive(Clone, Debug, Default)]
pub struct RouteOptions {
    /// Wall-clock budget; the best result so far is returned when it runs out (results are
    /// then no longer guaranteed to be identical between runs).
    pub budget: Option<Duration>,
    /// Copper layers to route on (empty: all).
    pub layers: Vec<String>,
    /// Effort.
    pub effort: Effort,
    /// Seed for the reroute order perturbation.
    pub seed: u64,
    /// Grid pitch (default: (track width + clearance) / 2 of the finest routed net).
    pub grid: Option<Nm>,
    /// Negotiation iterations (default by effort: 10 / 40 / 120).
    pub max_iterations: Option<usize>,
    /// Dog-bone fanout of BGA balls and escapes of off-grid fine-pitch pads before routing
    /// (default on; see [`fanout`]).
    pub fanout: Option<bool>,
    /// Allow any-angle shortcuts in post-processing (default: 0°/45°/90° segments only).
    pub any_angle: bool,
    /// For [`Scope::Connection`]: when the connection cannot be routed, move the unlocked
    /// routing of other nets out of the way: push-and-shove (bend their tracks, move their vias),
    /// else rip it, route the connection, route those nets again, and keep the result only if
    /// they end up with no more unrouted connections than before. (`Scope::All` and
    /// `Scope::Nets` always give their failed connections one push-and-shove attempt.)
    pub shove: bool,
    /// Gridless refinement after optimization (`gridless`): shortest paths over the
    /// clearance hulls of nearby obstacles, so tracks are not tied to the grid. `None`: the
    /// default (on).
    pub gridless: Option<bool>,
    /// Arc corners: every bend of the routed tracks becomes a tangent arc, as large as the
    /// rules allow (up to `arc_radius`), for smooth or RF nets.
    pub arcs: bool,
    /// Largest arc radius with `arcs` (default 1 mm).
    pub arc_radius: Option<Nm>,
}

/// Result of [`fanout`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FanoutResult {
    /// Stubs from the balls to their vias.
    pub tracks: Vec<Track>,
    /// Fanout vias.
    pub vias: Vec<Via>,
    /// Pads fanned out (`U1.C3`).
    pub pads: Vec<String>,
}

/// What to route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every unrouted connection.
    All,
    /// The unrouted connections of these nets.
    Nets(Vec<String>),
    /// One connection between two pads (`U1.3`), joining their copper islands.
    Connection {
        /// Pad label.
        from: String,
        /// Pad label.
        to: String,
    },
}

/// Progress and cancellation callbacks.
pub struct Hooks<'a> {
    /// `(done, total, message)`.
    pub progress: &'a dyn Fn(u64, Option<u64>, &str),
    /// Polled regularly; `true` aborts with [`RouteError::Cancelled`].
    pub cancelled: &'a dyn Fn() -> bool,
}

impl Hooks<'_> {
    /// No progress, never cancelled.
    pub fn none() -> Hooks<'static> {
        Hooks { progress: &|_, _, _| {}, cancelled: &|| false }
    }
}

/// Outcome of a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnStatus {
    /// Connected.
    Routed,
    /// Not connected.
    Failed,
}

/// Result for one connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionReport {
    /// Net.
    pub net: String,
    /// One end (`U1.3`, `via#4`).
    pub from: String,
    /// Other end.
    pub to: String,
    /// Outcome.
    pub status: ConnStatus,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Where the problem is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Point>,
    /// Layer of the problem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Things to try.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    /// Objects involved in the failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<ObjectRef>,
}

/// Summary numbers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteStats {
    /// Connections attempted.
    pub connections: usize,
    /// Connections routed.
    pub routed: usize,
    /// Connections failed.
    pub failed: usize,
    /// Routed share in percent (100 when there was nothing to do).
    pub completion: f64,
    /// Track segments added.
    pub tracks: usize,
    /// Vias added.
    pub vias: usize,
    /// Total length of the added tracks.
    pub length: Nm,
    /// Negotiation iterations run.
    pub iterations: usize,
    /// Overused grid points after each iteration.
    pub overuse: Vec<usize>,
    /// Grid pitch used.
    pub grid: Nm,
    /// Vias removed by post-processing.
    pub vias_removed: usize,
    /// Wires removed because the DRC flagged them.
    pub drc_removed: usize,
    /// Whether the time budget ran out.
    pub budget_exhausted: bool,
    /// Dog-bone fanout vias (included in `vias`).
    #[serde(default)]
    pub fanout_vias: usize,
    /// Fine-pitch pads routed through an escape (a straight track out of the pad row, then 45°
    /// onto the grid); part of `tracks`.
    #[serde(default)]
    pub escapes: usize,
}

/// Router output: items to add (IDs allocated in order from the project's allocator) and the
/// report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteResult {
    /// New tracks.
    pub tracks: Vec<Track>,
    /// New vias.
    pub vias: Vec<Via>,
    /// Per connection.
    pub connections: Vec<ConnectionReport>,
    /// Numbers.
    pub stats: RouteStats,
    /// Existing unlocked tracks to remove (the routing of nets moved out of the way by
    /// `shove`; their new routing is in `tracks` / `vias`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_tracks: Vec<ObjectId>,
    /// Existing unlocked vias to remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_vias: Vec<ObjectId>,
    /// Nets whose routing was pushed aside or rerouted to make room (`shove`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rerouted: Vec<String>,
}

/// Router errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouteError {
    /// The board has no outline.
    #[error("the board has no outline")]
    NoOutline,
    /// A layer is not a copper layer.
    #[error("`{0}` is not a copper layer of the board")]
    InvalidLayer(String),
    /// A net does not exist.
    #[error("no net `{0}`")]
    UnknownNet(String),
    /// A connection endpoint is not a placed pad.
    #[error("{0}")]
    Endpoint(String),
    /// Cancelled through the hook.
    #[error("routing cancelled")]
    Cancelled,
}

/// The best negotiation state: (overused points, wires per net, failures per net).
type Snapshot = (usize, Vec<Vec<engine::Wire>>, Vec<Vec<Option<FailKind>>>);

fn costs(effort: Effort) -> Costs {
    Costs {
        via: 10.0,
        bend45: 0.4,
        bend90: 1.5,
        wrong_way: 1.8,
        diag: 1.05,
        weight: if effort == Effort::Low { 1.3 } else { 1.0 },
    }
}

/// Routes the connections in `scope`. The project is not modified.
pub fn route(p: &Project, scope: &Scope, opts: &RouteOptions, hooks: &Hooks<'_>) -> Result<RouteResult, RouteError> {
    let start = Instant::now();
    let first = route_once(p, scope, opts, hooks, Soft::No)?;
    if first.stats.failed > 0 && !matches!(scope, Scope::Connection { .. }) {
        return shove_leftovers(p, opts, hooks, first, start);
    }
    if !opts.shove || first.stats.failed == 0 || !matches!(scope, Scope::Connection { .. }) {
        return Ok(first);
    }
    if let Some(r) = push_and_shove(p, scope, opts, hooks)? {
        return Ok(r);
    }
    shove(p, scope, opts, hooks, first)
}

/// Connections left unrouted by a full run get one more chance each: push-and-shove on the
/// routed board (`push_and_shove`), in report order, within what is left of the time budget.
/// Returns `first` unchanged when nothing improves.
fn shove_leftovers(
    p: &Project,
    opts: &RouteOptions,
    hooks: &Hooks<'_>,
    first: RouteResult,
    start: Instant,
) -> Result<RouteResult, RouteError> {
    let mut q = p.clone();
    for t in &first.tracks {
        let id = q.alloc_id();
        q.board_mut().tracks.push(Track { id, ..t.clone() });
    }
    for v in &first.vias {
        let id = q.alloc_id();
        q.board_mut().vias.push(Via { id, ..v.clone() });
    }
    let mut out = first;
    let mut changed = false;
    let mut moved: BTreeSet<String> = BTreeSet::new();
    for ci in 0..out.connections.len() {
        if out.connections[ci].status != ConnStatus::Failed {
            continue;
        }
        let left = opts.budget.map(|b| b.saturating_sub(start.elapsed()));
        if left.is_some_and(|l| l < Duration::from_millis(50)) {
            break;
        }
        if (hooks.cancelled)() {
            return Err(RouteError::Cancelled);
        }
        let c = &out.connections[ci];
        let scope = Scope::Connection { from: c.from.clone(), to: c.to.clone() };
        let sub = RouteOptions { budget: left, ..opts.clone() };
        let Some(r) = push_and_shove(&q, &scope, &sub, hooks)? else { continue };
        let b = q.board_mut();
        b.tracks.retain(|t| !r.removed_tracks.contains(&t.id));
        b.vias.retain(|v| !r.removed_vias.contains(&v.id));
        for t in r.tracks {
            let id = q.alloc_id();
            q.board_mut().tracks.push(Track { id, ..t });
        }
        for v in r.vias {
            let id = q.alloc_id();
            q.board_mut().vias.push(Via { id, ..v });
        }
        moved.extend(r.rerouted);
        let rep = &mut out.connections[ci];
        rep.status = ConnStatus::Routed;
        rep.reason = None;
        rep.at = None;
        rep.layer = None;
        rep.hints.clear();
        rep.subjects.clear();
        changed = true;
    }
    if !changed {
        return Ok(out);
    }
    // The result: what is on `q` and not on `p`, renumbered from the project's allocator.
    let (old_t, old_v): (BTreeSet<ObjectId>, BTreeSet<ObjectId>) =
        (p.board().tracks.iter().map(|t| t.id).collect(), p.board().vias.iter().map(|v| v.id).collect());
    let (new_t, new_v): (BTreeSet<ObjectId>, BTreeSet<ObjectId>) =
        (q.board().tracks.iter().map(|t| t.id).collect(), q.board().vias.iter().map(|v| v.id).collect());
    let mut alloc = p.clone();
    out.tracks = q.board().tracks.iter().filter(|t| !old_t.contains(&t.id)).cloned().collect();
    out.vias = q.board().vias.iter().filter(|v| !old_v.contains(&v.id)).cloned().collect();
    for t in &mut out.tracks {
        t.id = alloc.alloc_id();
    }
    for v in &mut out.vias {
        v.id = alloc.alloc_id();
    }
    out.removed_tracks = old_t.difference(&new_t).copied().collect();
    out.removed_vias = old_v.difference(&new_v).copied().collect();
    out.rerouted = moved.into_iter().collect();
    let st = &mut out.stats;
    st.routed = out.connections.iter().filter(|c| c.status == ConnStatus::Routed).count();
    st.failed = st.connections - st.routed;
    st.completion =
        if st.connections == 0 { 100.0 } else { (st.routed as f64 * 1000.0 / st.connections as f64).round() / 10.0 };
    st.tracks = out.tracks.len();
    st.vias = out.vias.len();
    st.length = tracks_length(&out.tracks);
    Ok(out)
}

/// How [`place_track`] gets past copper in its way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PlaceMode {
    /// Walk around pads, locked copper and other fixed things; push the unlocked tracks and
    /// vias of other nets out of the way (they spring back as far as they can). The default.
    #[default]
    Shove,
    /// Walk around everything; move nothing.
    Walkaround,
    /// Exactly along the points; anything in the way is an error.
    Strict,
}

/// A track to place with [`place_track`]: a polyline of a net on one copper layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackRequest {
    /// Copper layer.
    pub layer: String,
    /// Net.
    pub net: String,
    /// Track width.
    pub width: Nm,
    /// Waypoints (at least two); ends usually on copper of the net.
    pub points: Vec<Point>,
}

/// What [`place_track`] adds and moves (new items have IDs allocated in order from the
/// project's allocator; the project is not modified).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlaceResult {
    /// New track segments: the placed track, then the new geometry of shoved tracks.
    pub tracks: Vec<Track>,
    /// Shoved vias at their new positions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vias: Vec<Via>,
    /// Existing tracks replaced (shoved).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_tracks: Vec<ObjectId>,
    /// Existing vias replaced (shoved).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_vias: Vec<ObjectId>,
    /// Nets whose copper was shoved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shoved: Vec<String>,
    /// Whether the track had to detour around something (walkaround).
    pub walked: bool,
    /// Segments of the placed track (the first `placed` of `tracks`).
    pub placed: usize,
}

/// Why [`place_track`] failed: a stable code, what is in the way, where, and what to try.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct PlaceError {
    /// `route.track_blocked`, `route.shove_failed`, `route.shove_limit`, `route.shove_drc`,
    /// `route.invalid_layer`, `net.not_found`, `track.too_few_points`, `board.no_outline`.
    pub code: &'static str,
    /// What happened.
    pub message: String,
    /// Where.
    pub at: Option<Point>,
    /// Layer.
    pub layer: Option<String>,
    /// Objects involved.
    pub subjects: Vec<ObjectRef>,
    /// What to try.
    pub hint: String,
}

impl PlaceError {
    fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Box<PlaceError> {
        Box::new(PlaceError {
            code,
            message: message.into(),
            at: None,
            layer: None,
            subjects: vec![],
            hint: hint.into(),
        })
    }
}

/// Places one track along waypoints, with push-and-shove (`mode`): the track walks around pads,
/// locked copper, keep-outs and the board edge, and pushes the unlocked tracks and vias of
/// other nets aside (bending them around it, moving vias), which then spring back as far as
/// the rules allow. Never moves locked items. The result is checked exactly and by the DRC;
/// nothing is returned that the DRC would flag. Deterministic.
pub fn place_track(p: &Project, req: &TrackRequest, mode: PlaceMode) -> Result<PlaceResult, Box<PlaceError>> {
    if p.board().outline.contours.is_empty() {
        return Err(PlaceError::new("board.no_outline", "the board has no outline", "set one with `board.outline`"));
    }
    let items = geo_board::copper_items(p);
    let isl = geo_board::islands(&items);
    let rb = RouterBoard::build(p, &items, &isl);
    let layer = rb.layer_names.iter().position(|n| *n == req.layer).ok_or_else(|| {
        PlaceError::new(
            "route.invalid_layer",
            format!("`{}` is not a copper layer of the board", req.layer),
            format!("copper layers: {}", rb.layer_names.join(", ")),
        )
    })?;
    let net = *rb.net_ids.get(&req.net).ok_or_else(|| {
        PlaceError::new("net.not_found", format!("no net `{}`", req.net), "list nets with `net.list`")
    })?;
    if req.points.len() < 2 {
        return Err(PlaceError::new(
            "track.too_few_points",
            "a track needs at least two points",
            "give two or more points",
        ));
    }
    let head = shove::HeadLine { layer, net, width: req.width, pts: req.points.iter().map(|q| P::of(*q)).collect() };
    let m = match mode {
        PlaceMode::Shove => shove::Mode::Shove,
        PlaceMode::Walkaround => shove::Mode::Walkaround,
        PlaceMode::Strict => shove::Mode::Strict,
    };
    let pr = rb.profile(net);
    let grid = ((req.width.0 as f64 + pr.c) / 2.0).max(25_000.0);
    let o = shove::run(p, &rb, &[head], &[], m, grid).map_err(|f| place_failure(&rb, &f, mode))?;
    let applied = apply_outcome(p, &o).map_err(|bad| {
        let d = &bad[0];
        let mut e = PlaceError::new(
            "route.shove_drc",
            format!("the shoved result fails the DRC: {}", d.message),
            "move the waypoints, use mode walkaround, or rip the routing in the way (route.rip) and route again",
        );
        e.at = d.location;
        e.subjects = d.subjects.clone();
        e
    })?;
    let placed = o.tracks.iter().filter(|t| t.net.as_deref() == Some(req.net.as_str())).count();
    Ok(PlaceResult {
        tracks: applied.tracks,
        vias: applied.vias,
        removed_tracks: o.removed_tracks,
        removed_vias: o.removed_vias,
        shoved: o.moved_nets.iter().map(|&n| rb.nets[n as usize].clone()).collect(),
        walked: o.walked,
        placed,
    })
}

/// Turns a push-and-shove failure into a [`PlaceError`].
fn place_failure(rb: &RouterBoard, f: &shove::Fail, mode: PlaceMode) -> Box<PlaceError> {
    let (what, locked) = (&f.label, f.locked);
    let at = Some(f.at.to_point());
    let layer = f.layer.map(|l| rb.layer_names[l].clone());
    let mut e = match f.why {
        shove::FailWhy::Blocked => PlaceError::new(
            "route.track_blocked",
            format!("the track cannot get past {what}"),
            if locked {
                "move the waypoints, or unlock that copper so it can be shoved".to_string()
            } else if mode == PlaceMode::Strict {
                "move the waypoints, or use mode shove or walkaround".to_string()
            } else {
                "move the waypoints clear of it, or route on another layer".to_string()
            },
        ),
        shove::FailWhy::Stuck => PlaceError::new(
            "route.shove_failed",
            format!("{what} cannot be pushed out of the way (an end, a crossing or fixed copper is in the way)"),
            "move the waypoints, use mode walkaround, or rip the routing in the way (route.rip) and route again",
        ),
        shove::FailWhy::Limit => PlaceError::new(
            "route.shove_limit",
            "too much copper would have to move".to_string(),
            "move the waypoints to a less crowded path, or rip the routing in the way (route.rip)",
        ),
    };
    e.at = at;
    e.layer = layer;
    e.subjects = f.subjects.clone();
    e
}

/// How a routing run treats movable copper (unlocked straight tracks and unlocked vias).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Soft {
    /// As an obstacle with its clearance.
    No,
    /// New copper may touch it (no clearance): push-and-shove moves it aside afterwards.
    Touch,
    /// Only its centerline is in the way (new routing never crosses it).
    Centerline,
}

/// Softens the movable copper obstacles of `rb` for a [`Soft`] run.
fn soften(rb: &mut RouterBoard, soft: Soft) {
    if soft == Soft::No {
        return;
    }
    let mut vias: BTreeSet<ObjectId> = BTreeSet::new();
    for ob in rb.obstacles.iter_mut() {
        let Some(m) = &ob.movable else { continue };
        if let geo_board::ItemRef::Via(id) = m {
            vias.insert(*id);
        }
        ob.clear = Clear::Fixed(0.0);
        if soft == Soft::Centerline
            && let geo::Shape::Capsule { r, .. } = &mut ob.shape
        {
            *r = 0.0;
            ob.bbox = ob.shape.bbox();
        }
    }
    rb.holes.retain(|h| h.via.is_none_or(|id| !vias.contains(&id)));
}

/// Push-and-shove for a connection that failed: the connection is routed with the movable
/// copper of other nets softened (first touching allowed, then only its centerline in the
/// way), then that copper is shoved out of the new route's way (`shove`). `None` when no
/// attempt gives a legal, DRC-clean result.
fn push_and_shove(
    p: &Project,
    scope: &Scope,
    opts: &RouteOptions,
    hooks: &Hooks<'_>,
) -> Result<Option<RouteResult>, RouteError> {
    // Straight segments only: the new route becomes the pushing head polyline.
    let sub = RouteOptions { shove: false, arcs: false, ..opts.clone() };
    for soft in [Soft::Touch, Soft::Centerline] {
        (hooks.progress)(0, None, "push-and-shove");
        let r = route_once(p, scope, &sub, hooks, soft)?;
        if r.stats.failed > 0 || r.connections.is_empty() {
            continue;
        }
        let items = geo_board::copper_items(p);
        let isl = geo_board::islands(&items);
        let rb = RouterBoard::build(p, &items, &isl);
        let heads: Vec<shove::HeadLine> = r
            .tracks
            .iter()
            .filter_map(|t| {
                Some(shove::HeadLine {
                    layer: rb.layer_names.iter().position(|n| *n == t.layer)?,
                    net: *rb.net_ids.get(t.net.as_ref()?)?,
                    width: t.width,
                    pts: vec![P::of(t.start), P::of(t.end)],
                })
            })
            .collect();
        let grid = r.stats.grid.0 as f64;
        let Ok(o) = shove::run(p, &rb, &heads, &r.vias, shove::Mode::Shove, grid.max(25_000.0)) else { continue };
        let Ok(applied) = apply_outcome(p, &o) else { continue };
        let mut out = r;
        out.tracks = applied.tracks;
        out.vias = applied.vias;
        out.removed_tracks = o.removed_tracks;
        out.removed_vias = o.removed_vias;
        out.rerouted = o.moved_nets.iter().map(|&n| rb.nets[n as usize].clone()).collect();
        let st = &mut out.stats;
        st.tracks = out.tracks.len();
        st.vias = out.vias.len();
        st.length = tracks_length(&out.tracks);
        return Ok(Some(out));
    }
    Ok(None)
}

/// New items of a push-and-shove outcome with IDs (allocated in order from the project's
/// allocator), after checking the result with the DRC: no error may involve them.
struct Applied {
    tracks: Vec<Track>,
    vias: Vec<Via>,
}

fn apply_outcome(p: &Project, o: &shove::Outcome) -> Result<Applied, Vec<crate::diag::Diagnostic>> {
    let mut q = p.clone();
    let mut out = Applied { tracks: vec![], vias: vec![] };
    let mut ids: BTreeSet<(String, u64)> = BTreeSet::new();
    for t in &o.tracks {
        let id = q.alloc_id();
        ids.insert(("track".into(), id.0));
        out.tracks.push(Track { id, ..t.clone() });
    }
    for v in &o.vias {
        let id = q.alloc_id();
        ids.insert(("via".into(), id.0));
        out.vias.push(Via { id, ..v.clone() });
    }
    let b = q.board_mut();
    b.tracks.retain(|t| !o.removed_tracks.contains(&t.id));
    b.vias.retain(|v| !o.removed_vias.contains(&v.id));
    b.tracks.extend(out.tracks.iter().cloned());
    b.vias.extend(out.vias.iter().cloned());
    let bad: Vec<crate::diag::Diagnostic> = crate::drc::check(&q)
        .into_iter()
        .filter(|d| d.severity == crate::diag::Severity::Error && d.code != "drc.unrouted")
        .filter(|d| {
            d.subjects
                .iter()
                .any(|s| matches!(s, ObjectRef::Item { kind, index } if ids.contains(&(kind.clone(), *index))))
        })
        .collect();
    if bad.is_empty() { Ok(out) } else { Err(bad) }
}

/// Total length of tracks (arcs along the arc).
fn tracks_length(tracks: &[Track]) -> Nm {
    Nm(tracks.iter().map(|t| geo_board::track_length(t).0 as f64).sum::<f64>().round() as i64)
}

/// Default largest arc radius with `RouteOptions::arcs` (nm).
const ARC_RADIUS: f64 = 1_000_000.0;

/// Whether gridless refinement is on by default (see `RouteOptions::gridless`).
const GRIDLESS_DEFAULT: bool = true;

/// Rounds of rip-up and retry after legalization.
const RETRY_ROUNDS: usize = 6;

/// Retries kept although they only move a failure to another net.
const SIDEWAYS: usize = 4;

/// Negotiation stops when the best iteration is this many iterations old.
const STAGNATION: usize = 12;

/// Rounds of [`shove`] (each may add the nets found in the way).
const SHOVE_ROUNDS: usize = 3;

/// Makes room for a connection that failed: rips the unlocked routing of the nets reported in
/// the way, routes the connection, then routes those nets again around it. Kept only when the
/// rerouted nets have no more unrouted connections than before; otherwise `first` is returned.
fn shove(
    p: &Project,
    scope: &Scope,
    opts: &RouteOptions,
    hooks: &Hooks<'_>,
    mut first: RouteResult,
) -> Result<RouteResult, RouteError> {
    let own: BTreeSet<String> = first.connections.iter().map(|c| c.net.clone()).collect();
    let in_the_way = |r: &RouteResult, set: &mut BTreeSet<String>| {
        for c in r.connections.iter().filter(|c| c.status == ConnStatus::Failed) {
            for s in &c.subjects {
                if let ObjectRef::Net(n) = s
                    && !own.contains(n)
                {
                    set.insert(n.clone());
                }
            }
        }
    };
    let unrouted =
        |q: &Project, nets: &BTreeSet<String>| geo_board::ratsnest(q).iter().filter(|l| nets.contains(&l.net)).count();
    let mut blockers = BTreeSet::new();
    in_the_way(&first, &mut blockers);
    let sub = RouteOptions { shove: false, ..opts.clone() };
    for _ in 0..SHOVE_ROUNDS {
        if blockers.is_empty() {
            break;
        }
        let mut q = p.clone();
        let movable = |net: &Option<String>| net.as_ref().is_some_and(|n| blockers.contains(n));
        let b = q.board_mut();
        let removed_tracks: Vec<ObjectId> =
            b.tracks.iter().filter(|t| !t.locked && movable(&t.net)).map(|t| t.id).collect();
        let removed_vias: Vec<ObjectId> =
            b.vias.iter().filter(|v| !v.locked && movable(&v.net)).map(|v| v.id).collect();
        if removed_tracks.is_empty() && removed_vias.is_empty() {
            break;
        }
        b.tracks.retain(|t| !removed_tracks.contains(&t.id));
        b.vias.retain(|v| !removed_vias.contains(&v.id));
        (hooks.progress)(0, None, &format!("rerouting {} net(s) in the way", blockers.len()));
        let conn = route_once(&q, scope, &sub, hooks, Soft::No)?;
        if conn.stats.failed > 0 {
            let before = blockers.len();
            in_the_way(&conn, &mut blockers);
            if blockers.len() == before {
                break;
            }
            continue;
        }
        let apply = |q: &mut Project, r: &RouteResult| {
            for t in &r.tracks {
                let id = q.alloc_id();
                q.board_mut().tracks.push(Track { id, ..t.clone() });
            }
            for v in &r.vias {
                let id = q.alloc_id();
                q.board_mut().vias.push(Via { id, ..v.clone() });
            }
        };
        apply(&mut q, &conn);
        let again = route_once(&q, &Scope::Nets(blockers.iter().cloned().collect()), &sub, hooks, Soft::No)?;
        apply(&mut q, &again);
        if unrouted(&q, &blockers) > unrouted(p, &blockers) {
            continue;
        }
        let mut out = conn;
        out.tracks.extend(again.tracks);
        out.vias.extend(again.vias);
        out.connections.extend(again.connections);
        out.removed_tracks = removed_tracks;
        out.removed_vias = removed_vias;
        out.rerouted = blockers.into_iter().collect();
        let st = &mut out.stats;
        st.connections = out.connections.len();
        st.routed = out.connections.iter().filter(|c| c.status == ConnStatus::Routed).count();
        st.failed = st.connections - st.routed;
        st.completion = (st.routed as f64 * 1000.0 / st.connections.max(1) as f64).round() / 10.0;
        st.tracks = out.tracks.len();
        st.vias = out.vias.len();
        st.length = tracks_length(&out.tracks);
        return Ok(out);
    }
    for c in first.connections.iter_mut().filter(|c| c.status == ConnStatus::Failed) {
        c.hints.push("rerouting the nets in the way did not make room; move parts or rip more routing".into());
    }
    Ok(first)
}

/// One routing run (see [`route`]).
fn route_once(
    p: &Project,
    scope: &Scope,
    opts: &RouteOptions,
    hooks: &Hooks<'_>,
    soft: Soft,
) -> Result<RouteResult, RouteError> {
    let start = Instant::now();
    let slots = routing_slots(p, &opts.layers)?;
    (hooks.progress)(0, None, "preparing");
    // Fanout: planned on the board as it is, then part of the board the router works on.
    let fan = if opts.fanout != Some(false) && !matches!(scope, Scope::Connection { .. }) {
        fanout_items(p, scope, &slots, opts, None)?
    } else {
        vec![]
    };
    let with_fanout;
    // (track IDs, via ID) of each fanout.
    let mut fan_ids: Vec<(Vec<ObjectId>, Option<ObjectId>)> = Vec::new();
    // Escapes: (net name, layer, points, index in `fan_ids`).
    let mut fan_escapes: Vec<(String, usize, Vec<P>, usize)> = Vec::new();
    let p: &Project = if fan.is_empty() {
        p
    } else {
        let mut q = p.clone();
        for mut f in fan {
            f.tracks_net = f.tracks.first().and_then(|t| t.net.clone());
            let mut ids = (vec![], None);
            for mut t in f.tracks {
                t.id = q.alloc_id();
                ids.0.push(t.id);
                q.board_mut().tracks.push(t);
            }
            if let Some(mut v) = f.via {
                v.id = q.alloc_id();
                ids.1 = Some(v.id);
                q.board_mut().vias.push(v);
            }
            if let (Some((layer, pts)), Some(net)) = (f.escape, f.tracks_net) {
                fan_escapes.push((net, layer, pts, fan_ids.len()));
            }
            fan_ids.push(ids);
        }
        with_fanout = q;
        &with_fanout
    };
    let items = geo_board::copper_items(p);
    let isl = geo_board::islands(&items);
    let mut rb = RouterBoard::build(p, &items, &isl);
    soften(&mut rb, soft);
    let mut routes = plan(&rb, scope)?;
    if (hooks.cancelled)() {
        return Err(RouteError::Cancelled);
    }

    // Grid.
    let mut active = vec![false; rb.profiles.len()];
    for r in &routes {
        active[r.prof] = true;
    }
    let grid = make_grid(&rb, &routes, opts);
    let pitch = grid.g;

    let budget_end = opts.budget.map(|b| start + b);
    let negotiate_end = opts.budget.map(|b| start + b.mul_f64(0.75));
    let phase_end = std::sync::Mutex::new(negotiate_end);
    let stop = || phase_end.lock().expect("deadline").is_some_and(|d| Instant::now() > d);
    let mut eng = Engine::new(&rb, grid, slots.clone(), &active, costs(opts.effort), &stop);
    let mut sc = eng.scratch();
    let pool: std::sync::Mutex<Vec<Scratch>> = std::sync::Mutex::new(Vec::new());

    // Static index for access stubs and exact checks.
    let reach = reach(&rb);
    let mut index = Index::new(rb.bbox.expand(pitch * 4.0), (pitch * 8.0).max(500_000.0), reach);
    for (i, ob) in rb.obstacles.iter().enumerate() {
        index.insert(Item::Static(i as u32), ob.bbox);
    }
    {
        let ck = Checker { rb: &rb, index: &index };
        let mut cache: BTreeMap<usize, Vec<Access>> = BTreeMap::new();
        for r in &mut routes {
            for island in &mut r.islands {
                for &t in &island.terms {
                    let acc = cache.entry(t).or_insert_with(|| eng.access(t, &ck));
                    island.access.extend(acc.iter().cloned());
                }
            }
        }
    }

    let mut stats = RouteStats { grid: Nm(pitch as i64), ..Default::default() };
    let max_iter = opts.max_iterations.unwrap_or(match opts.effort {
        Effort::Low => 10,
        Effort::Normal => 40,
        Effort::High => 120,
    });
    let total_conns: usize = routes.iter().map(|r| r.conns.len()).sum();
    let nroutes = routes.len();

    // ---- negotiated congestion ----
    let mut rng = Rng(opts.seed ^ 0x5EED_CAD1_AB00_0000);
    let mut pres = 0.6f32;
    let hist_fac = 0.5f32;
    let mut order: Vec<usize> = (0..routes.len()).collect();
    let mut reroute: Vec<usize> = order.clone();
    let mut best: Option<Snapshot> = None;
    let mut best_it = 0;
    let mut exhausted = false;
    for it in 0..max_iter.max(1) {
        stats.iterations = it + 1;
        let mut k = 0;
        for batch in engine::batches(&reroute, &routes) {
            if (hooks.cancelled)() {
                return Err(RouteError::Cancelled);
            }
            (hooks.progress)(
                (it * nroutes + k) as u64,
                None,
                &format!("iteration {}: routing {}", it + 1, rb.nets[routes[batch[0]].net as usize]),
            );
            k += batch.len();
            if route_batch(&mut eng, &mut routes, &batch, Mode::Negotiate(pres), &pool).is_err() {
                exhausted = true;
                for &ri in &batch {
                    let r = &mut routes[ri];
                    for (ci, f) in r.failed.iter_mut().enumerate() {
                        if f.is_none() && !r.wires.iter().any(|w| w.conn == ci) {
                            *f = Some(FailKind::Budget);
                        }
                    }
                }
                break;
            }
        }
        // Overuse.
        let mut points: Vec<(engine::Conflict, usize)> = Vec::new();
        let mut net_conf = vec![0usize; routes.len()];
        for (ri, r) in routes.iter().enumerate() {
            let c = eng.conflicts(r);
            net_conf[ri] = c.len();
            points.extend(c.into_iter().map(|c| (c, ri)));
        }
        let total: usize = net_conf.iter().sum();
        stats.overuse.push(total);
        (hooks.progress)(
            ((it + 1) * routes.len()) as u64,
            None,
            &format!("iteration {}: {total} overused points", it + 1),
        );
        if best.as_ref().is_none_or(|b| total < b.0) {
            best = Some((
                total,
                routes.iter().map(|r| r.wires.clone()).collect(),
                routes.iter().map(|r| r.failed.clone()).collect(),
            ));
            best_it = it;
        }
        if total == 0 || exhausted || stop() || it - best_it >= STAGNATION {
            exhausted |= total > 0 && stop();
            break;
        }
        for (c, _) in &points {
            eng.add_history(*c, hist_fac);
        }
        pres *= 1.6;
        // Reroute nets with overuse and nets whose routes are near an overused point.
        let mut set: BTreeSet<usize> = BTreeSet::new();
        for (c, ri) in &points {
            set.insert(*ri);
            let at = eng.conflict_pos(*c);
            for (rj, r) in routes.iter().enumerate() {
                if r.bbox.expand(reach).dist(at) == 0.0 {
                    set.insert(rj);
                }
            }
        }
        reroute = order.iter().copied().filter(|r| set.contains(r)).collect();
        // Perturb the order a little (seeded) to break cycles.
        for i in (1..reroute.len()).rev() {
            if rng.next().is_multiple_of(4) {
                let j = (rng.next() % (i as u64 + 1)) as usize;
                reroute.swap(i, j);
            }
        }
        order.sort_by_key(|&r| std::cmp::Reverse(net_conf[r]));
        order.sort_by(|a, b| routes[*a].length.total_cmp(&routes[*b].length));
    }
    if (hooks.cancelled)() {
        return Err(RouteError::Cancelled);
    }
    // Restore the best iteration.
    if let Some((bt, wires, failed)) = best {
        let cur: usize = routes.iter().map(|r| eng.conflicts(r).len()).sum();
        if bt < cur {
            for r in routes.iter_mut() {
                eng.uncommit(r);
            }
            for ((r, w), f) in routes.iter_mut().zip(wires).zip(failed) {
                r.wires = w;
                r.failed = f;
                eng.commit(r);
            }
        }
    }

    // ---- legalization: rip what still conflicts, reroute without sharing ----
    *phase_end.lock().expect("deadline") = budget_end;
    let mut ripped: Vec<usize> = Vec::new();
    loop {
        let conf: Vec<usize> = routes.iter().map(|r| eng.conflicts(r).len()).collect();
        let Some((worst, _)) = conf
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .max_by(|a, b| a.1.cmp(b.1).then(routes[a.0].length.total_cmp(&routes[b.0].length)))
        else {
            break;
        };
        let r = &mut routes[worst];
        eng.uncommit(r);
        r.wires.clear();
        ripped.push(worst);
    }
    ripped.sort_by(|a, b| routes[*a].length.total_cmp(&routes[*b].length).then(a.cmp(b)));
    for ri in ripped {
        if (hooks.cancelled)() {
            return Err(RouteError::Cancelled);
        }
        let r = &mut routes[ri];
        let res = eng.route_net(&mut sc, r, Mode::Hard);
        eng.commit(r);
        if res.is_err() {
            if (hooks.cancelled)() {
                return Err(RouteError::Cancelled);
            }
            exhausted = true;
            for (ci, f) in r.failed.iter_mut().enumerate() {
                if f.is_none() && !r.wires.iter().any(|w| w.conn == ci) {
                    *f = Some(FailKind::Budget);
                }
            }
        }
    }
    // ---- rip-up and retry: a net that still fails takes the room of the nets in its way,
    // which are routed again around it; kept only when fewer connections fail overall ----
    let mut sideways = SIDEWAYS;
    for _ in 0..RETRY_ROUNDS {
        let mut improved = false;
        let failing: Vec<usize> =
            (0..routes.len()).filter(|&ri| routes[ri].failed.contains(&Some(FailKind::Congestion))).collect();
        for fi in failing {
            if (hooks.cancelled)() {
                return Err(RouteError::Cancelled);
            }
            if stop() {
                break;
            }
            let Some(ci) = routes[fi].failed.iter().position(|f| *f == Some(FailKind::Congestion)) else { continue };
            let Ok(keys) = eng.blocking_keys(&mut sc, &routes[fi], ci) else { break };
            let blockers: Vec<usize> = (0..routes.len())
                .filter(|&j| j != fi && keys.iter().any(|k| routes[j].keys.binary_search(k).is_ok()))
                .collect();
            if blockers.is_empty() || blockers.len() > 8 {
                continue;
            }
            let group: Vec<usize> = std::iter::once(fi).chain(blockers.iter().copied()).collect();
            let fails = |routes: &[NetRoute]| -> usize {
                group.iter().map(|&j| routes[j].failed.iter().filter(|f| f.is_some()).count()).sum()
            };
            let before = fails(&routes);
            let saved: Vec<(Vec<engine::Wire>, Vec<Option<FailKind>>)> =
                group.iter().map(|&j| (routes[j].wires.clone(), routes[j].failed.clone())).collect();
            for &j in &group {
                eng.uncommit(&mut routes[j]);
            }
            let mut ok = true;
            for &j in &group {
                ok &= eng.route_net(&mut sc, &mut routes[j], Mode::Hard).is_ok();
                eng.commit(&mut routes[j]);
            }
            // Kept when fewer connections fail, or (a few times) when as many fail but the
            // failure moved to another net, which gets its own retry next.
            let after = fails(&routes);
            let moved = after == before && routes[fi].failed[ci].is_none() && sideways > 0;
            if ok && (after < before || moved) {
                sideways -= usize::from(moved);
                improved = true;
                continue;
            }
            for (&j, (w, f)) in group.iter().zip(saved) {
                eng.uncommit(&mut routes[j]);
                routes[j].wires = w;
                routes[j].failed = f;
                eng.commit(&mut routes[j]);
            }
            if !ok {
                break;
            }
        }
        if !improved {
            break;
        }
    }
    // ---- via minimization: nets with vias are routed again with dear vias ----
    if !stop() {
        stats.vias_removed += minimize_vias(&mut eng, &mut sc, &mut routes, &stop, hooks)?;
    }
    stats.budget_exhausted = exhausted;
    (hooks.progress)(0, None, "optimizing");

    // ---- post-processing ----
    let mut geoms: Vec<post::NetGeom> = routes.iter().map(|r| post::geometry(&eng, r)).collect();
    // Escapes become part of the path that uses them (fan_ids index → absorbed).
    let mut absorbed: BTreeSet<usize> = BTreeSet::new();
    let absorb = |g: &mut post::NetGeom, net: u32, absorbed: &mut BTreeSet<usize>| {
        for (name, layer, pts, fi) in &fan_escapes {
            if rb.net_ids.get(name) == Some(&net) {
                absorbed.remove(fi);
                if post::absorb(g, *layer, pts) {
                    absorbed.insert(*fi);
                }
            }
        }
    };
    for (r, g) in routes.iter().zip(geoms.iter_mut()) {
        absorb(g, r.net, &mut absorbed);
    }
    for (r, g) in routes.iter().zip(geoms.iter_mut()) {
        let pr = rb.profile(r.net);
        post::insert(&mut index, r.net, g, pr.hw, pr.rv);
    }
    let passes = match opts.effort {
        Effort::Low => 1,
        Effort::Normal => 3,
        Effort::High => 6,
    };
    for ri in 0..routes.len() {
        if (hooks.cancelled)() {
            return Err(RouteError::Cancelled);
        }
        let pr = rb.profile(routes[ri].net);
        post::remove(&mut index, &mut geoms[ri]);
        let removed = {
            let ck = Checker { rb: &rb, index: &index };
            post::reduce_vias(&eng, &mut routes[ri], &ck)
        };
        stats.vias_removed += removed;
        if removed > 0 {
            geoms[ri] = post::geometry(&eng, &routes[ri]);
            absorb(&mut geoms[ri], routes[ri].net, &mut absorbed);
        }
        {
            let ck = Checker { rb: &rb, index: &index };
            post::optimize(&mut geoms[ri], routes[ri].net, &ck, pitch, passes, opts.any_angle);
        }
        post::insert(&mut index, routes[ri].net, &mut geoms[ri], pr.hw, pr.rv);
    }
    // A second round: nets optimized early get the room freed by the later ones.
    if opts.effort != Effort::Low {
        for ri in 0..routes.len() {
            if (hooks.cancelled)() {
                return Err(RouteError::Cancelled);
            }
            let pr = rb.profile(routes[ri].net);
            post::remove(&mut index, &mut geoms[ri]);
            {
                let ck = Checker { rb: &rb, index: &index };
                post::optimize(&mut geoms[ri], routes[ri].net, &ck, pitch, passes, opts.any_angle);
            }
            post::insert(&mut index, routes[ri].net, &mut geoms[ri], pr.hw, pr.rv);
        }
    }
    // Gridless refinement, then the usual cleanup of the new corners.
    if opts.gridless.unwrap_or(GRIDLESS_DEFAULT) {
        for ri in 0..routes.len() {
            if (hooks.cancelled)() {
                return Err(RouteError::Cancelled);
            }
            let pr = rb.profile(routes[ri].net);
            post::remove(&mut index, &mut geoms[ri]);
            {
                let ck = Checker { rb: &rb, index: &index };
                let net = routes[ri].net;
                let free = |layer: usize, a: P, b: P| ck.seg(layer, a, b, net).is_none();
                for path in &mut geoms[ri].paths {
                    if gridless::refine(path, net, &ck, pitch, opts.any_angle) {
                        post::optimize_path(path, &free, pitch, passes, opts.any_angle);
                    }
                }
            }
            post::insert(&mut index, routes[ri].net, &mut geoms[ri], pr.hw, pr.rv);
        }
    }
    // Arc corners, last: nothing after this step moves vertices.
    if opts.arcs {
        let max_r = opts.arc_radius.map_or(ARC_RADIUS, |r| r.0 as f64);
        for ri in 0..routes.len() {
            if (hooks.cancelled)() {
                return Err(RouteError::Cancelled);
            }
            let pr = rb.profile(routes[ri].net);
            post::remove(&mut index, &mut geoms[ri]);
            {
                let ck = Checker { rb: &rb, index: &index };
                arcs::round(&mut geoms[ri], routes[ri].net, &ck, 2.0 * pr.hw, max_r.max(2.0 * pr.hw));
            }
            post::insert(&mut index, routes[ri].net, &mut geoms[ri], pr.hw, pr.rv);
        }
    }

    // ---- verification ----
    let mut drc_reason: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut out;
    let mut round = 0;
    loop {
        out = build_items(p, &rb, &routes, &geoms);
        let mut proj = p.clone();
        let gone: BTreeSet<ObjectId> = absorbed.iter().flat_map(|&fi| fan_ids[fi].0.iter().copied()).collect();
        proj.board_mut().tracks.retain(|t| !gone.contains(&t.id));
        proj.board_mut().tracks.extend(out.tracks.iter().cloned());
        proj.board_mut().vias.extend(out.vias.iter().cloned());
        // Soft runs overlap movable copper on purpose (push-and-shove moves it next).
        let diags = if soft == Soft::No { crate::drc::check(&proj) } else { vec![] };
        let mut bad: BTreeSet<(usize, usize)> = BTreeSet::new();
        for d in diags.iter().filter(|d| d.severity == crate::diag::Severity::Error && d.code != "drc.unrouted") {
            for s in &d.subjects {
                if let ObjectRef::Item { kind, index } = s
                    && let Some(&owner) = out.owner.get(&(kind.clone(), *index))
                {
                    bad.insert(owner);
                    drc_reason.entry(owner).or_insert_with(|| format!("{}: {}", d.code, d.message));
                }
            }
        }
        if bad.is_empty() {
            out.proj = Some(proj);
            break;
        }
        round += 1;
        for &(ri, wi) in &bad {
            stats.drc_removed += 1;
            let g = &mut geoms[ri];
            if round >= 4 {
                g.paths.clear();
                g.vias.clear();
            } else {
                g.paths.retain(|p| p.wire != wi);
                g.vias.retain(|v| v.1 != wi);
            }
            if let Some(w) = routes[ri].wires.get(wi) {
                let ci = w.conn;
                routes[ri].failed[ci] = Some(FailKind::Drc);
            }
        }
    }

    // ---- report ----
    let proj = out.proj.take().expect("verified project");
    let fitems = geo_board::copper_items(&proj);
    let fisl = geo_board::islands(&fitems);
    // Fanouts that ended up joining their ball to nothing else are left out of the result.
    let mut fan_tracks: Vec<Track> = Vec::new();
    let mut fan_vias: Vec<Via> = Vec::new();
    if !fan_ids.is_empty() {
        let mut pads_in: BTreeMap<usize, usize> = BTreeMap::new();
        let mut track_island: BTreeMap<ObjectId, usize> = BTreeMap::new();
        for (it, &k) in fitems.iter().zip(&fisl) {
            match &it.item {
                geo_board::ItemRef::Pad(..) => *pads_in.entry(k).or_default() += 1,
                geo_board::ItemRef::Track(id) => {
                    track_island.insert(*id, k);
                }
                _ => {}
            }
        }
        let b = p.board();
        for (fi, (tids, vid)) in fan_ids.iter().enumerate() {
            if absorbed.contains(&fi) {
                stats.escapes += 1;
                continue;
            }
            let used = track_island.get(&tids[0]).is_some_and(|k| pads_in.get(k).copied().unwrap_or(0) > 1);
            if used {
                fan_tracks.extend(b.tracks.iter().filter(|t| tids.contains(&t.id)).cloned());
                fan_vias.extend(b.vias.iter().filter(|v| Some(v.id) == *vid).cloned());
                stats.fanout_vias += usize::from(vid.is_some());
                stats.escapes += usize::from(vid.is_none());
            }
        }
    }
    let mut island_of: BTreeMap<String, usize> = BTreeMap::new();
    for (i, it) in fitems.iter().enumerate() {
        island_of.entry(it.item.to_string()).or_insert(fisl[i]);
    }
    let ck = Checker { rb: &rb, index: &index };
    let mut reports = Vec::new();
    for (ri, r) in routes.iter().enumerate() {
        for (ci, c) in r.conns.iter().enumerate() {
            let joined = match (island_of.get(&c.from), island_of.get(&c.to)) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            let mut rep = ConnectionReport {
                net: rb.nets[r.net as usize].clone(),
                from: c.from.clone(),
                to: c.to.clone(),
                status: if joined { ConnStatus::Routed } else { ConnStatus::Failed },
                reason: None,
                at: None,
                layer: None,
                hints: vec![],
                subjects: vec![],
            };
            if !joined {
                explain(&mut eng, &mut sc, &ck, r, ci, r.failed[ci], &drc_reason, ri, &slots, &mut rep);
                dedup_hints(&mut rep);
            }
            reports.push(rep);
        }
    }
    stats.connections = total_conns;
    stats.routed = reports.iter().filter(|r| r.status == ConnStatus::Routed).count();
    stats.failed = stats.connections - stats.routed;
    stats.completion = if stats.connections == 0 {
        100.0
    } else {
        (stats.routed as f64 * 1000.0 / stats.connections as f64).round() / 10.0
    };
    fan_tracks.append(&mut out.tracks);
    fan_vias.append(&mut out.vias);
    out.tracks = fan_tracks;
    out.vias = fan_vias;
    stats.tracks = out.tracks.len();
    stats.vias = out.vias.len();
    stats.length = tracks_length(&out.tracks);
    (hooks.progress)(1, Some(1), "done");
    Ok(RouteResult {
        tracks: out.tracks,
        vias: out.vias,
        connections: reports,
        stats,
        removed_tracks: vec![],
        removed_vias: vec![],
        rerouted: vec![],
    })
}

/// Via cost of [`minimize_vias`] (grid steps; negotiation uses `Costs::via`).
const MIN_VIA_COST: f32 = 40.0;

/// Rounds of [`minimize_vias`].
const MIN_VIA_ROUNDS: usize = 1;

/// Most extra length (share of the old length, plus `MIN_VIA_SLACK` grid steps per via saved)
/// [`minimize_vias`] accepts.
const MIN_VIA_STRETCH: f64 = 0.1;
const MIN_VIA_SLACK: f64 = 8.0;

/// Via minimization: every net with vias is ripped and routed again, without sharing, with
/// vias four times as dear; the new routing is kept when it has fewer vias, no more failed
/// connections and is at most a little longer. Other nets do not move, so the result stays
/// legal. Returns the number of vias saved.
fn minimize_vias(
    eng: &mut Engine<'_>,
    sc: &mut Scratch,
    routes: &mut [NetRoute],
    stop: &dyn Fn() -> bool,
    hooks: &Hooks<'_>,
) -> Result<usize, RouteError> {
    let mut saved = 0;
    let base = eng.costs;
    let n = routes.len();
    for ri in (0..MIN_VIA_ROUNDS).flat_map(|_| 0..n) {
        if (hooks.cancelled)() {
            eng.costs = base;
            return Err(RouteError::Cancelled);
        }
        if stop() {
            break;
        }
        let (vias, len) = wire_stats(eng, &routes[ri].wires);
        if vias == 0 {
            continue;
        }
        let fails = |r: &NetRoute| r.failed.iter().filter(|f| f.is_some()).count();
        let before = fails(&routes[ri]);
        let old = (routes[ri].wires.clone(), routes[ri].failed.clone());
        eng.uncommit(&mut routes[ri]);
        eng.costs = Costs { via: MIN_VIA_COST, ..base };
        let res = eng.route_net(sc, &mut routes[ri], Mode::Hard);
        eng.costs = base;
        let (nv, nl) = wire_stats(eng, &routes[ri].wires);
        let ok = res.is_ok()
            && fails(&routes[ri]) <= before
            && nv < vias
            && nl <= len * (1.0 + MIN_VIA_STRETCH) + MIN_VIA_SLACK * eng.grid.g * (vias - nv) as f64;
        if ok {
            saved += vias - nv;
        } else {
            routes[ri].wires = old.0;
            routes[ri].failed = old.1;
        }
        eng.commit(&mut routes[ri]);
    }
    Ok(saved)
}

/// Vias and track length (nm) of wires.
fn wire_stats(eng: &Engine<'_>, wires: &[engine::Wire]) -> (usize, f64) {
    let mut vias = 0;
    let mut len = 0.0;
    for w in wires {
        let (segs, v) = eng.wire_geometry(w);
        vias += v.len();
        len += segs.iter().map(|(_, a, b)| a.dist(*b)).sum::<f64>();
    }
    (vias, len)
}

/// Routes a batch of nets whose regions do not meet: every net is ripped up, all are routed
/// against the same occupancy (in parallel with the `parallel` feature), then committed in
/// batch order. The outcome does not depend on the number of threads. When the time budget
/// stops a search, the whole batch is put back as it was.
fn route_batch(
    eng: &mut Engine<'_>,
    routes: &mut [NetRoute],
    batch: &[usize],
    mode: Mode,
    pool: &std::sync::Mutex<Vec<Scratch>>,
) -> Result<(), Stop> {
    let saved: Vec<(Vec<engine::Wire>, Vec<Option<FailKind>>)> =
        batch.iter().map(|&ri| (routes[ri].wires.clone(), routes[ri].failed.clone())).collect();
    for &ri in batch {
        eng.uncommit(&mut routes[ri]);
    }
    let mut taken: Vec<NetRoute> = batch.iter().map(|&ri| std::mem::take(&mut routes[ri])).collect();
    let e: &Engine<'_> = eng;
    let run = |nr: &mut NetRoute| -> Result<(), Stop> {
        let mut sc = pool.lock().expect("scratch pool").pop().unwrap_or_else(|| e.scratch());
        let r = e.route_net(&mut sc, nr, mode);
        pool.lock().expect("scratch pool").push(sc);
        r
    };
    #[cfg(feature = "parallel")]
    let res: Vec<Result<(), Stop>> = if taken.len() > 1 {
        use rayon::prelude::*;
        taken.par_iter_mut().map(run).collect()
    } else {
        taken.iter_mut().map(run).collect()
    };
    #[cfg(not(feature = "parallel"))]
    let res: Vec<Result<(), Stop>> = taken.iter_mut().map(run).collect();
    let stopped = res.iter().any(Result::is_err);
    for ((&ri, mut nr), (w, f)) in batch.iter().zip(taken).zip(saved) {
        if stopped {
            nr.wires = w;
            nr.failed = f;
        }
        routes[ri] = nr;
        eng.commit(&mut routes[ri]);
    }
    if stopped { Err(Stop) } else { Ok(()) }
}

/// Routing layers (stackup indices) from layer names (all copper layers when empty).
fn routing_slots(p: &Project, layers: &[String]) -> Result<Vec<usize>, RouteError> {
    if p.board().outline.contours.is_empty() {
        return Err(RouteError::NoOutline);
    }
    let names = p.board().stackup.copper_names();
    let mut slots: Vec<usize> = Vec::new();
    for l in layers {
        let i = names.iter().position(|n| n == l).ok_or_else(|| RouteError::InvalidLayer(l.clone()))?;
        if !slots.contains(&i) {
            slots.push(i);
        }
    }
    if slots.is_empty() {
        slots = (0..names.len()).collect();
    }
    slots.sort_unstable();
    Ok(slots)
}

/// Plans dog-bone fanouts (tracks and vias with placeholder IDs, and the pad label) for the
/// nets in `scope`, limited to the components `only` when given.
fn fanout_items(
    p: &Project,
    scope: &Scope,
    slots: &[usize],
    opts: &RouteOptions,
    only: Option<&[String]>,
) -> Result<Vec<FanItem>, RouteError> {
    let items = geo_board::copper_items(p);
    let isl = geo_board::islands(&items);
    let rb = RouterBoard::build(p, &items, &isl);
    let routes = plan(&rb, scope)?;
    if routes.is_empty() {
        return Ok(vec![]);
    }
    let mut index = Index::new(rb.bbox.expand(1_000_000.0), 500_000.0, reach(&rb));
    for (i, ob) in rb.obstacles.iter().enumerate() {
        index.insert(Item::Static(i as u32), ob.bbox);
    }
    let bones = fanout::plan(&rb, &routes, slots, &mut index, only);
    // Escapes end on cells from which the router can move outwards (static maps).
    let grid = make_grid(&rb, &routes, opts);
    let mut active = vec![false; rb.profiles.len()];
    for r in &routes {
        active[r.prof] = true;
    }
    let st = grid::Statics::build(&rb, &grid, slots, &active);
    let open = |net: u32, layer: usize, (x, y): (i32, i32), (dx, dy): (i32, i32)| {
        let Some(s) = slots.iter().position(|&l| l == layer) else { return false };
        let map = &st.track[rb.net_profile[net as usize]][s];
        [(2 * x, 2 * y), (2 * x + dx, 2 * y + dy), (2 * x + 2 * dx, 2 * y + 2 * dy)]
            .into_iter()
            .all(|(i, j)| i >= 0 && j >= 0 && i < grid.dw() && j < grid.dh() && grid::ok(map[grid.didx(i, j)], net))
    };
    let escapes = fanout::escape(&rb, &routes, slots, &grid, &mut index, only, &open);
    let (first, last) = (rb.layer_names[0].clone(), rb.layer_names[rb.layer_names.len() - 1].clone());
    let track = |net: u32, layer: usize, a: P, b: P| {
        let key = rb.profile(net).key;
        Track {
            id: ObjectId(0),
            layer: rb.layer_names[layer].clone(),
            width: key.width,
            net: Some(rb.nets[net as usize].clone()),
            start: a.to_point(),
            end: b.to_point(),
            mid: None,
            locked: false,
        }
    };
    let mut out: Vec<FanItem> = bones
        .into_iter()
        .map(|b| {
            let key = rb.profile(b.net).key;
            let net = Some(rb.nets[b.net as usize].clone());
            let via = Via {
                id: ObjectId(0),
                at: b.via.to_point(),
                drill: key.via_drill,
                diameter: key.via_dia,
                net,
                from: first.clone(),
                to: last.clone(),
                locked: false,
            };
            FanItem {
                tracks: vec![track(b.net, b.layer, b.pad, b.via)],
                via: Some(via),
                label: b.label,
                escape: None,
                tracks_net: None,
            }
        })
        .collect();
    for e in escapes {
        let tracks = e.pts.windows(2).map(|w| track(e.net, e.layer, w[0], w[1])).collect();
        out.push(FanItem { tracks, via: None, label: e.label, escape: Some((e.layer, e.pts)), tracks_net: None });
    }
    Ok(out)
}

/// A planned fanout or escape (IDs not allocated yet).
struct FanItem {
    tracks: Vec<Track>,
    via: Option<Via>,
    /// Pad label.
    label: String,
    /// An escape's layer and points (pad center first).
    escape: Option<(usize, Vec<P>)>,
    /// Net of the tracks (filled when applied).
    tracks_net: Option<String>,
}

/// Fanout of the components `components` (all when empty), for the pads whose nets have
/// unrouted connections: dog bones for the inner balls of BGA (area-array) footprints (a short
/// track to a through via between the balls) and escapes for off-grid fine-pitch pads
/// (docs/ROUTER.md, "Fanout and escape"). Returns the items to add (IDs allocated from a copy
/// of the project's allocator); the project is not modified.
pub fn fanout(p: &Project, components: &[String], layers: &[String]) -> Result<FanoutResult, RouteError> {
    let slots = routing_slots(p, layers)?;
    let only = (!components.is_empty()).then_some(components);
    let items = fanout_items(p, &Scope::All, &slots, &RouteOptions::default(), only)?;
    let mut alloc = p.clone();
    let mut out = FanoutResult { tracks: vec![], vias: vec![], pads: vec![] };
    for f in items {
        for mut t in f.tracks {
            t.id = alloc.alloc_id();
            out.tracks.push(t);
        }
        if let Some(mut v) = f.via {
            v.id = alloc.alloc_id();
            out.vias.push(v);
        }
        out.pads.push(f.label);
    }
    Ok(out)
}

/// Largest extra distance a check adds beyond an item's own extent.
fn reach(rb: &RouterBoard) -> f64 {
    let mut r = rb.rules.copper_to_edge.0 as f64;
    for p in &rb.profiles {
        r = r.max(p.c + p.hw.max(p.rv)).max(rb.h2h + 2.0 * p.dr);
    }
    r.max(rb.rules.clearance.0 as f64) + 2.0 * index::TOL
}

/// The routing grid: pitch (track width + clearance) / 2 of the finest routed profile (or the
/// `grid` option), aligned with the routed pads. Fanout is planned on the same grid.
fn make_grid(rb: &RouterBoard, routes: &[NetRoute], opts: &RouteOptions) -> grid::Grid {
    let pitch = opts.grid.map(|g| g.0 as f64).unwrap_or_else(|| {
        routes
            .iter()
            .map(|r| {
                let k = rb.profiles[r.prof].key;
                (k.width.0 + k.clear.0) as f64 / 2.0
            })
            .fold(f64::MAX, f64::min)
    });
    let pitch = if pitch.is_finite() { pitch.round().max(25_000.0) } else { 225_000.0 };
    let (offx, offy) = alignment(rb, routes, pitch);
    grid::Grid::new(rb.bbox, pitch, offx, offy)
}

/// Grid offset aligning cell centers with as many routed pad centers as possible.
fn alignment(rb: &RouterBoard, routes: &[NetRoute], g: f64) -> (f64, f64) {
    let mut cx: BTreeMap<i64, usize> = BTreeMap::new();
    let mut cy: BTreeMap<i64, usize> = BTreeMap::new();
    for r in routes {
        for isl in &r.islands {
            for &t in isl.terms.iter().filter(|&&t| rb.terminals[t].pad) {
                let at = rb.terminals[t].at;
                *cx.entry(((at.x - rb.bbox.min.x).rem_euclid(g) / 1_000.0).round() as i64).or_default() += 1;
                *cy.entry(((at.y - rb.bbox.min.y).rem_euclid(g) / 1_000.0).round() as i64).or_default() += 1;
            }
        }
    }
    let pick = |m: &BTreeMap<i64, usize>| -> f64 {
        m.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(k, _)| *k as f64 * 1_000.0).unwrap_or(0.0)
    };
    (pick(&cx).min(g - 1.0), pick(&cy).min(g - 1.0))
}

/// Margin around a net's terminals for its batching region (nm), plus a quarter of its extent.
const REGION_MARGIN: f64 = 1_500_000.0;

/// Nets to route with their islands and connections.
fn plan(rb: &RouterBoard, scope: &Scope) -> Result<Vec<NetRoute>, RouteError> {
    let wanted: Option<BTreeSet<u32>> = match scope {
        Scope::All => None,
        Scope::Nets(list) => {
            let mut s = BTreeSet::new();
            for n in list {
                s.insert(*rb.net_ids.get(n).ok_or_else(|| RouteError::UnknownNet(n.clone()))?);
            }
            Some(s)
        }
        Scope::Connection { .. } => None,
    };
    // Terminals by net, then island.
    let mut by_net: BTreeMap<u32, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
    for (ti, t) in rb.terminals.iter().enumerate() {
        by_net.entry(t.net).or_default().entry(t.island).or_default().push(ti);
    }
    let mut routes = Vec::new();
    for (net, isls) in by_net {
        if wanted.as_ref().is_some_and(|w| !w.contains(&net)) {
            continue;
        }
        // Islands with a pad or via (track-only copper is not a ratsnest endpoint).
        let islands: Vec<Vec<usize>> = isls
            .into_values()
            .filter(|ts| ts.iter().any(|&t| rb.terminals[t].pad || rb.terminals[t].label.starts_with("via#")))
            .collect();
        if islands.len() < 2 {
            continue;
        }
        // Ratsnest anchors: the pads of an island (its vias when it has none).
        let anchors = |i: usize| -> Vec<usize> {
            let pads: Vec<usize> = islands[i].iter().copied().filter(|&t| rb.terminals[t].pad).collect();
            if !pads.is_empty() {
                return pads;
            }
            islands[i].iter().copied().filter(|&t| rb.terminals[t].label.starts_with("via#")).collect()
        };
        let mut conns = Vec::new();
        match scope {
            Scope::Connection { from, to } => {
                let find = |label: &str| -> Option<(usize, usize)> {
                    islands
                        .iter()
                        .enumerate()
                        .find_map(|(i, ts)| ts.iter().find(|&&t| rb.terminals[t].label == label).map(|&t| (i, t)))
                };
                let (Some((ia, ta)), Some((ib, tb))) = (find(from), find(to)) else { continue };
                if ia != ib {
                    conns.push(Conn {
                        a: ia,
                        b: ib,
                        from: from.clone(),
                        to: to.clone(),
                        from_at: rb.terminals[ta].at,
                        to_at: rb.terminals[tb].at,
                    });
                }
            }
            _ => {
                // Prim over islands, edge = closest anchor pair (as the ratsnest).
                let n = islands.len();
                let mut in_tree = vec![false; n];
                in_tree[0] = true;
                for _ in 1..n {
                    let mut best: Option<(f64, usize, usize, usize, usize)> = None;
                    for gi in (0..n).filter(|&i| in_tree[i]) {
                        for hj in (0..n).filter(|&j| !in_tree[j]) {
                            for &a in &anchors(gi) {
                                for &b in &anchors(hj) {
                                    let d = rb.terminals[a].at.dist(rb.terminals[b].at).round();
                                    if best.is_none_or(|x| d < x.0) {
                                        best = Some((d, gi, hj, a, b));
                                    }
                                }
                            }
                        }
                    }
                    let Some((_, gi, hj, a, b)) = best else { break };
                    in_tree[hj] = true;
                    conns.push(Conn {
                        a: gi,
                        b: hj,
                        from: rb.terminals[a].label.clone(),
                        to: rb.terminals[b].label.clone(),
                        from_at: rb.terminals[a].at,
                        to_at: rb.terminals[b].at,
                    });
                }
            }
        }
        if conns.is_empty() {
            continue;
        }
        let length = conns.iter().map(|c| c.from_at.dist(c.to_at)).sum();
        let mut region = BoxF::EMPTY;
        for &t in islands.iter().flatten() {
            region = region.union(rb.terminals[t].shape.bbox());
        }
        let side = (region.max.x - region.min.x).max(region.max.y - region.min.y);
        let region = region.expand(REGION_MARGIN + 0.25 * side);
        routes.push(NetRoute {
            net,
            prof: rb.net_profile[net as usize],
            islands: islands.into_iter().map(|terms| Island { terms, access: vec![] }).collect(),
            failed: vec![None; conns.len()],
            conns,
            wires: vec![],
            keys: vec![],
            bbox: BoxF::EMPTY,
            length,
            region,
        });
    }
    if let Scope::Connection { from, to } = scope
        && routes.is_empty()
    {
        let known = |l: &str| rb.terminals.iter().find(|t| t.label == l).map(|t| t.net);
        return match (known(from), known(to)) {
            (None, _) => Err(RouteError::Endpoint(format!("{from} is not a placed pad with a net"))),
            (_, None) => Err(RouteError::Endpoint(format!("{to} is not a placed pad with a net"))),
            (Some(a), Some(b)) if a != b => Err(RouteError::Endpoint(format!(
                "{from} (net {}) and {to} (net {}) are on different nets",
                rb.nets[a as usize], rb.nets[b as usize]
            ))),
            _ => Ok(routes), // already connected
        };
    }
    // Short nets first: they have the least freedom.
    routes.sort_by(|a, b| a.length.total_cmp(&b.length).then(a.net.cmp(&b.net)));
    Ok(routes)
}

struct Built {
    tracks: Vec<Track>,
    vias: Vec<Via>,
    /// (`track`/`via`, id) → (route, wire).
    owner: BTreeMap<(String, u64), (usize, usize)>,
    proj: Option<Project>,
}

fn build_items(p: &Project, rb: &RouterBoard, routes: &[NetRoute], geoms: &[post::NetGeom]) -> Built {
    let mut alloc = p.clone();
    let mut out = Built { tracks: vec![], vias: vec![], owner: BTreeMap::new(), proj: None };
    let (first, last) = (rb.layer_names[0].clone(), rb.layer_names[rb.layer_names.len() - 1].clone());
    for (ri, (r, g)) in routes.iter().zip(geoms).enumerate() {
        let key = rb.profile(r.net).key;
        let net = Some(rb.nets[r.net as usize].clone());
        for path in &g.paths {
            for (k, w) in path.pts.windows(2).enumerate() {
                let (a, b) = (w[0].to_point(), w[1].to_point());
                if a == b {
                    continue;
                }
                let mid = path.mids.get(k).copied().flatten().map(P::to_point);
                let id: ObjectId = alloc.alloc_id();
                out.owner.insert(("track".into(), id.0), (ri, path.wire));
                out.tracks.push(Track {
                    id,
                    layer: rb.layer_names[path.layer].clone(),
                    width: key.width,
                    net: net.clone(),
                    start: a,
                    end: b,
                    mid,
                    locked: false,
                });
            }
        }
        for (v, wi) in &g.vias {
            let id = alloc.alloc_id();
            out.owner.insert(("via".into(), id.0), (ri, *wi));
            out.vias.push(Via {
                id,
                at: v.to_point(),
                drill: key.via_drill,
                diameter: key.via_dia,
                net: net.clone(),
                from: first.clone(),
                to: last.clone(),
                locked: false,
            });
        }
    }
    out
}

/// Fills in why a connection failed, where, and what to try.
#[allow(clippy::too_many_arguments)]
fn explain(
    eng: &mut Engine<'_>,
    sc: &mut Scratch,
    ck: &Checker<'_>,
    r: &NetRoute,
    ci: usize,
    kind: Option<FailKind>,
    drc: &BTreeMap<(usize, usize), String>,
    ri: usize,
    slots: &[usize],
    rep: &mut ConnectionReport,
) {
    let rb = eng.rb;
    let c = &r.conns[ci];
    let layers_hint = if slots.len() < rb.layer_names.len() {
        Some("allow more copper layers (layers option)".to_string())
    } else if rb.layer_names.len() < 2 {
        Some("a second copper layer would allow crossings (board.setup layers 2)".to_string())
    } else {
        None
    };
    if kind == Some(FailKind::Drc) {
        let msg = r
            .wires
            .iter()
            .enumerate()
            .find(|(_, w)| w.conn == ci)
            .and_then(|(wi, _)| drc.get(&(ri, wi)))
            .cloned()
            .unwrap_or_else(|| "a DRC check".into());
        rep.reason = Some(format!("route removed after verification ({msg})"));
        rep.at = Some(c.from_at.to_point());
        rep.hints.push("move nearby parts apart or reduce the clearance, then route again".into());
        return;
    }
    if kind == Some(FailKind::Budget) {
        rep.reason = Some("the time budget ran out before this connection was routed".into());
        rep.at = Some(c.from_at.to_point());
        rep.hints.push("raise budget_ms or the effort, or route the remaining nets with route.nets".into());
        return;
    }
    // Access.
    for (isl, label) in [(c.a, &c.from), (c.b, &c.to)] {
        if r.islands[isl].access.is_empty() {
            let t = &r.islands[isl].terms[0];
            let term = &rb.terminals[*t];
            let mut blockers = Vec::new();
            for &l in slots {
                if term.layers & (1u64 << l.min(63)) == 0 {
                    continue;
                }
                // What sits near the pad: probe a ring of points around it.
                let bb = term.shape.bbox();
                let pr = rb.profile(r.net);
                let d = pr.hw + pr.c;
                for q in [
                    P::new(bb.min.x - d, term.at.y),
                    P::new(bb.max.x + d, term.at.y),
                    P::new(term.at.x, bb.min.y - d),
                    P::new(term.at.x, bb.max.y + d),
                    term.at,
                ] {
                    if let Some(b) = ck.seg(l, term.at, q, r.net) {
                        blockers.push((b, l, q));
                    }
                }
            }
            describe(rb, ck, &blockers, rep);
            rep.reason = Some(format!(
                "no legal way out of {label} for a {} track: {}",
                rb.profile(r.net).key.width,
                rep.reason.take().unwrap_or_else(|| "its surroundings are blocked".into())
            ));
            if rep.at.is_none() {
                rep.at = Some(term.at.to_point());
            }
            rep.hints.push(format!("use a narrower track or smaller clearance for net {}", rep.net));
            rep.hints.extend(layers_hint);
            return;
        }
    }
    // Explain search: everything passable at a penalty; collect what the cheapest such path
    // runs through.
    let sources: Vec<(u32, f32)> = r.islands[c.a].access.iter().map(|a| (a.node, a.cost)).collect();
    let targets: Vec<u32> = r.islands[c.b].access.iter().map(|a| a.node).collect();
    let saved = eng.max_expansions;
    eng.max_expansions = 2_000_000;
    let path = eng.search(sc, r.net, r.prof, &sources, &targets, Mode::Explain).ok().flatten();
    eng.max_expansions = saved;
    let mut blockers: Vec<(Blocker, usize, P)> = Vec::new();
    if let Some(path) = path {
        for w in path.windows(2) {
            let (s0, x0, y0) = eng.unpack(w[0]);
            let (s1, x1, y1) = eng.unpack(w[1]);
            if s0 != s1 {
                let at = eng.grid.cell(x0, y0);
                if let Some(b) = ck.via(at, r.net, None) {
                    blockers.push((b, slots[s0], at));
                }
                continue;
            }
            let (a, b) = (eng.grid.cell(x0, y0), eng.grid.cell(x1, y1));
            let _ = (x1, y1);
            if let Some(bl) = ck.seg(slots[s0], a, b, r.net) {
                blockers.push((bl, slots[s0], P::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)));
            }
        }
    }
    describe(rb, ck, &blockers, rep);
    let what = if kind == Some(FailKind::Congestion) { "no room left" } else { "no path" };
    rep.reason = Some(match rep.reason.take() {
        Some(b) => format!("{what} from {} to {}: blocked by {b}", c.from, c.to),
        None => format!("{what} from {} to {} on the routing grid", c.from, c.to),
    });
    if rep.at.is_none() {
        rep.at = Some(c.from_at.to_point());
    }
    if kind == Some(FailKind::Congestion) {
        rep.hints.push("route this net first (route.nets) or rip and reroute the nets in the way (route.rip)".into());
    }
    rep.hints.extend(layers_hint);
    rep.hints.push(format!("reduce the clearance or track width of net {} (net class) if the fab allows it", rep.net));
}

/// Turns blockers into a reason, a location, subjects and hints.
fn describe(rb: &RouterBoard, _ck: &Checker<'_>, blockers: &[(Blocker, usize, P)], rep: &mut ConnectionReport) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut parts = Vec::new();
    let mut movers: BTreeSet<String> = BTreeSet::new();
    for (b, layer, at) in blockers {
        let lname = &rb.layer_names[*layer];
        let (label, subject) = match b {
            Blocker::Obstacle(o) => {
                let ob = &rb.obstacles[*o as usize];
                if let Some(owner) = &ob.owner {
                    movers.insert(owner.clone());
                }
                match ob.kind {
                    ObKind::Edge => {
                        rep.hints.push("move parts away from the board edge, or lower copper_to_edge".into());
                    }
                    ObKind::Keepout => rep.hints.push(format!("change {}", ob.label)),
                    ObKind::Copper => {
                        if let Some(n) = ob.net {
                            rep.hints.push(format!(
                                "rip the existing routing in the way (route.rip nets [\"{}\"]) and route again",
                                rb.nets[n as usize]
                            ));
                        }
                    }
                    _ => {}
                }
                let subject = match (ob.kind, ob.net) {
                    (ObKind::Copper, Some(n)) => Some(ObjectRef::Net(rb.nets[n as usize].clone())),
                    _ => ob.owner.clone().map(ObjectRef::Name),
                };
                (format!("{} on {lname}", ob.label), subject)
            }
            Blocker::Net(n, l) => {
                let name = &rb.nets[*n as usize];
                let what = match l {
                    Some(l) => format!("routing of net {name} on {}", rb.layer_names[*l as usize]),
                    None => format!("a via of net {name}"),
                };
                (what, Some(ObjectRef::Net(name.clone())))
            }
            Blocker::Hole => ("a drilled hole too close for a via (hole-to-hole)".to_string(), None),
            Blocker::Outside => ("the board outline".to_string(), None),
        };
        if !seen.insert(label.clone()) {
            continue;
        }
        if rep.at.is_none() {
            rep.at = Some(at.to_point());
            rep.layer = Some(lname.clone());
        }
        if let Some(s) = subject
            && !rep.subjects.contains(&s)
        {
            rep.subjects.push(s);
        }
        parts.push(label);
        if parts.len() >= 3 {
            break;
        }
    }
    for m in movers.into_iter().take(2) {
        rep.hints.push(format!("move {m} to open a channel (place.move)"));
    }
    if !parts.is_empty() {
        rep.reason = Some(parts.join(", "));
    }
}

fn dedup_hints(rep: &mut ConnectionReport) {
    let mut seen = BTreeSet::new();
    rep.hints.retain(|h| seen.insert(h.clone()));
}
