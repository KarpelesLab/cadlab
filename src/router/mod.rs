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
//! 3. **Search** (`engine`): A* with 8 directions, costs for length, bends (45° cheap, 90°
//!    dearer, sharper forbidden), vias, wrong-way moves per layer (alternating horizontal /
//!    vertical preference) and congestion; multi-source/multi-target so nets grow as trees.
//! 4. **Negotiated congestion** (PathFinder, McMurchie and Ebeling 1995): route everything
//!    allowing overlaps at a cost, raise history costs on overused points, rip up and reroute
//!    the nets involved until nothing is shared; then legalize what still conflicts.
//! 5. **Post-processing** (`post`): via reduction, collinear merging, 45° pull-tight and
//!    chamfers, each validated exactly against all other copper.
//! 6. **Verification**: `crate::drc::check` on the result; any routed item with a DRC error is
//!    ripped up and reported, so the output never violates the rules silently.
//!
//! Deterministic for a given input, options and seed (unless the time budget cuts it short).

mod engine;
mod geo;
mod grid;
mod index;
mod model;
mod post;

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

use engine::{Access, Conn, Costs, Engine, FailKind, Island, Mode, NetRoute, Rng};
use geo::{BoxF, P};
use index::{Blocker, Checker, Index, Item};
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
    if p.board().outline.contours.is_empty() {
        return Err(RouteError::NoOutline);
    }
    let names = p.board().stackup.copper_names();
    let mut slots: Vec<usize> = Vec::new();
    for l in &opts.layers {
        let i = names.iter().position(|n| n == l).ok_or_else(|| RouteError::InvalidLayer(l.clone()))?;
        if !slots.contains(&i) {
            slots.push(i);
        }
    }
    if slots.is_empty() {
        slots = (0..names.len()).collect();
    }
    slots.sort_unstable();
    (hooks.progress)(0, None, "preparing");
    let items = geo_board::copper_items(p);
    let isl = geo_board::islands(&items);
    let rb = RouterBoard::build(p, &items, &isl);
    let mut routes = plan(&rb, scope)?;
    if (hooks.cancelled)() {
        return Err(RouteError::Cancelled);
    }

    // Grid.
    let mut active = vec![false; rb.profiles.len()];
    for r in &routes {
        active[r.prof] = true;
    }
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
    let (offx, offy) = alignment(&rb, &routes, pitch);
    let grid = grid::Grid::new(rb.bbox, pitch, offx, offy);

    let budget_end = opts.budget.map(|b| start + b);
    let negotiate_end = opts.budget.map(|b| start + b.mul_f64(0.75));
    let phase_end = std::cell::Cell::new(negotiate_end);
    let stop = || (hooks.cancelled)() || phase_end.get().is_some_and(|d| Instant::now() > d);
    let mut eng = Engine::new(&rb, grid, slots.clone(), &active, costs(opts.effort), &stop);

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
    let mut exhausted = false;
    for it in 0..max_iter.max(1) {
        stats.iterations = it + 1;
        for (k, &ri) in reroute.iter().enumerate() {
            let r = &mut routes[ri];
            eng.uncommit(r);
            let res = eng.route_net(r, Mode::Negotiate(pres));
            eng.commit(r);
            if res.is_err() {
                if (hooks.cancelled)() {
                    return Err(RouteError::Cancelled);
                }
                exhausted = true;
                for f in r.failed.iter_mut().filter(|f| f.is_none()) {
                    *f = Some(FailKind::Budget);
                }
                break;
            }
            if k % 8 == 0 {
                (hooks.progress)(
                    (it * nroutes + k) as u64,
                    None,
                    &format!("iteration {}: routing {}", it + 1, rb.nets[r.net as usize]),
                );
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
        }
        if total == 0 || exhausted || stop() {
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
    phase_end.set(budget_end);
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
        let r = &mut routes[ri];
        let res = eng.route_net(r, Mode::Hard);
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
    stats.budget_exhausted = exhausted;
    (hooks.progress)(0, None, "optimizing");

    // ---- post-processing ----
    let mut geoms: Vec<post::NetGeom> = routes.iter().map(|r| post::geometry(&eng, r)).collect();
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
        }
        {
            let ck = Checker { rb: &rb, index: &index };
            post::optimize(&mut geoms[ri], routes[ri].net, &ck, pitch, passes);
        }
        post::insert(&mut index, routes[ri].net, &mut geoms[ri], pr.hw, pr.rv);
    }

    // ---- verification ----
    let mut drc_reason: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut out;
    let mut round = 0;
    loop {
        out = build_items(p, &rb, &routes, &geoms);
        let mut proj = p.clone();
        proj.board_mut().tracks.extend(out.tracks.iter().cloned());
        proj.board_mut().vias.extend(out.vias.iter().cloned());
        let diags = crate::drc::check(&proj);
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
                explain(&mut eng, &ck, r, ci, r.failed[ci], &drc_reason, ri, &slots, &mut rep);
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
    stats.tracks = out.tracks.len();
    stats.vias = out.vias.len();
    stats.length = Nm(out.tracks.iter().map(|t| P::of(t.start).dist(P::of(t.end))).sum::<f64>().round() as i64);
    (hooks.progress)(1, Some(1), "done");
    Ok(RouteResult { tracks: out.tracks, vias: out.vias, connections: reports, stats })
}

/// Largest extra distance a check adds beyond an item's own extent.
fn reach(rb: &RouterBoard) -> f64 {
    let mut r = rb.rules.copper_to_edge.0 as f64;
    for p in &rb.profiles {
        r = r.max(p.c + p.hw.max(p.rv)).max(rb.h2h + 2.0 * p.dr);
    }
    r.max(rb.rules.clearance.0 as f64) + 2.0 * index::TOL
}

/// Grid offset aligning cell centers with as many routed pad centers as possible.
fn alignment(rb: &RouterBoard, routes: &[NetRoute], g: f64) -> (f64, f64) {
    let mut cx: BTreeMap<i64, usize> = BTreeMap::new();
    let mut cy: BTreeMap<i64, usize> = BTreeMap::new();
    for r in routes {
        for isl in &r.islands {
            for &t in &isl.terms {
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
        let anchors = |i: usize| -> Vec<usize> {
            islands[i]
                .iter()
                .copied()
                .filter(|&t| rb.terminals[t].pad || rb.terminals[t].label.starts_with("via#"))
                .collect()
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
            for w in path.pts.windows(2) {
                let (a, b) = (w[0].to_point(), w[1].to_point());
                if a == b {
                    continue;
                }
                let id: ObjectId = alloc.alloc_id();
                out.owner.insert(("track".into(), id.0), (ri, path.wire));
                out.tracks.push(Track {
                    id,
                    layer: rb.layer_names[path.layer].clone(),
                    width: key.width,
                    net: net.clone(),
                    start: a,
                    end: b,
                    mid: None,
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
    let path = eng.search(r.net, r.prof, &sources, &targets, Mode::Explain).ok().flatten();
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
                (format!("{} on {lname}", ob.label), ob.owner.clone().map(ObjectRef::Name))
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
