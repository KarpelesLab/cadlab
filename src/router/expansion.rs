//! Gridless search: A* over the expansion rooms of the free space (`rooms`), with vias
//! between layers, then the shortest path through the rooms found (funnel), 45° where it fits.
//!
//! The search graph's nodes are points on the room portals (one to three per portal, spread
//! along it), via sites (each on every layer whose free space holds it) and the terminals'
//! points; two nodes are joined when they lie in one room, so every edge is a straight segment
//! inside a convex room and legal by construction. Edge costs follow the grid search (D22):
//! length in grid pitches, dearer against the layer's preferred direction, bends priced by
//! their angle, vias at `Costs::via`, and in negotiation the PathFinder costs of the
//! occupancy maps: present congestion and history, sampled along the segment every half
//! pitch on the same maps the grid search uses, so gridless and grid wires negotiate with
//! each other. The node sequence fixes a channel of rooms per layer; the shortest path through
//! that channel is found with the funnel algorithm (string pulling over the portals, as in
//! navigation meshes; Lee and Preparata 1984, Chazelle 1982), which stays inside the channel
//! and therefore legal. Segments that are not 0°/45°/90° are replaced by the octilinear
//! two-segment path when that is legal (exact checks), unless `any_angle`.
//!
//! Negotiation (`Mode::Negotiate`) searches rooms built from the static obstacles only (other
//! nets' routing is a cost, not an obstacle), kept per net between iterations; exact searches
//! (`Mode::Hard`) build rooms that also keep away from every other net's committed routing,
//! in a region around the connection (grown when nothing is found), so their result is legal
//! as found. Every result is checked exactly before it is kept.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;

use super::engine::{Engine, FailKind, Mode, NetRoute, Scratch, Stop, Wire};
use super::geo::{BoxF, P};
use super::index::{Checker, TOL};
use super::rooms::{self, Against, Rooms};

/// Largest room side, in grid pitches.
const ROOM_PITCHES: f64 = 6.0;
/// Largest via-site room side, in via pitches (via diameter plus clearance).
const SITE_PITCHES: f64 = 2.0;
/// Most nodes per portal.
const PORTAL_NODES: usize = 3;
/// Heuristic weight factor of negotiation searches.
const NEGOTIATE_WEIGHT: f32 = 1.5;
/// Margin around a connection for an exact search (nm), plus half its extent.
const LOCAL_MARGIN: f64 = 2_000_000.0;

/// A via site: its center and its room on each slot (`u32::MAX`: none).
#[derive(Clone, Debug)]
struct Site {
    at: P,
    rooms: Vec<u32>,
}

const NONE: u32 = u32::MAX;

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A point on a portal.
    Portal(u32),
    /// A via site.
    Site(u32),
    /// A terminal point of the connection's start.
    Source,
    /// A terminal point of the connection's end (index in the target list).
    Target(u32),
}

#[derive(Clone, Copy, Debug)]
struct Node {
    slot: usize,
    at: P,
    /// Rooms the node belongs to (two for a portal point).
    rooms: [u32; 2],
    kind: Kind,
}

/// The rooms of a net's search region: per slot track rooms, via sites, and the search graph's
/// fixed nodes (portal points and via sites).
#[derive(Debug)]
pub(crate) struct NetRooms {
    region: BoxF,
    layers: Vec<Rooms>,
    nodes: Vec<Node>,
    /// `[slot][room]` → nodes in the room.
    room_nodes: Vec<Vec<Vec<u32>>>,
    /// Nodes of each via site, one per slot that has it.
    site_nodes: Vec<Vec<u32>>,
}

impl NetRooms {
    fn build(eng: &Engine<'_>, net: u32, region: BoxF, against: Against) -> NetRooms {
        let rb = eng.rb;
        let pr = rb.profile(net);
        let g = eng.grid.g;
        let layers: Vec<Rooms> = eng
            .slots
            .iter()
            .map(|&l| {
                let free = rooms::track_space(rb, &eng.dynamic, &eng.grown, net, l, &region, against);
                Rooms::from_free(&free, ROOM_PITCHES * g)
            })
            .collect();
        let mut sites = Vec::new();
        if eng.vias_allowed {
            let free = rooms::via_space(rb, &eng.dynamic, &eng.grown, net, &region, against);
            let vr = Rooms::from_free(&free, SITE_PITCHES * (2.0 * pr.rv + pr.c));
            for r in &vr.rooms {
                let at = r.center();
                let rooms: Vec<u32> = layers.iter().map(|l| l.locate(at, 0.5).unwrap_or(NONE)).collect();
                if rooms.iter().filter(|&&r| r != NONE).count() >= 2 {
                    sites.push(Site { at, rooms });
                }
            }
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut room_nodes: Vec<Vec<Vec<u32>>> = layers.iter().map(|l| vec![Vec::new(); l.rooms.len()]).collect();
        for (s, l) in layers.iter().enumerate() {
            for (pi, p) in l.portals.iter().enumerate() {
                let len = p.a.dist(p.b);
                let n = ((len / (2.0 * g)).round() as usize).clamp(1, PORTAL_NODES);
                for k in 0..n {
                    let t = (k as f64 + 0.5) / n as f64;
                    let at = P::new(p.a.x + (p.b.x - p.a.x) * t, p.a.y + (p.b.y - p.a.y) * t);
                    let id = nodes.len() as u32;
                    nodes.push(Node { slot: s, at, rooms: p.rooms, kind: Kind::Portal(pi as u32) });
                    room_nodes[s][p.rooms[0] as usize].push(id);
                    room_nodes[s][p.rooms[1] as usize].push(id);
                }
            }
        }
        let mut site_nodes = Vec::with_capacity(sites.len());
        for (vi, site) in sites.iter().enumerate() {
            let mut ids = Vec::new();
            for (s, &r) in site.rooms.iter().enumerate() {
                if r == NONE {
                    continue;
                }
                let id = nodes.len() as u32;
                nodes.push(Node { slot: s, at: site.at, rooms: [r, NONE], kind: Kind::Site(vi as u32) });
                room_nodes[s][r as usize].push(id);
                ids.push(id);
            }
            site_nodes.push(ids);
        }
        NetRooms { region, layers, nodes, room_nodes, site_nodes }
    }
}

/// A point a path may start or end at: on slot `slot`; `on` is the gridless wire and segment
/// it lies on (a vertex is inserted there when a path attaches to it).
#[derive(Clone, Copy, Debug)]
struct End {
    slot: usize,
    at: P,
    on: Option<(usize, usize)>,
}

/// Points of the copper of islands `isls` and of the wires `wires` of `nr`, per routing slot.
fn ends_of(eng: &Engine<'_>, nr: &NetRoute, isls: &[usize], wires: &[usize]) -> Vec<End> {
    let rb = eng.rb;
    let g = eng.grid.g;
    let mut out = Vec::new();
    for &i in isls {
        for &t in &nr.islands[i].terms {
            let term = &rb.terminals[t];
            for (s, &layer) in eng.slots.iter().enumerate() {
                if term.layers & (1u64 << layer.min(63)) == 0 {
                    continue;
                }
                match term.anchor {
                    super::model::Anchor::Center(c) => {
                        out.push(End { slot: s, at: c, on: None });
                        // Big pads: more points inside, along their long side.
                        let b = term.shape.bbox();
                        let (w, h) = (b.max.x - b.min.x, b.max.y - b.min.y);
                        if w.max(h) > 4.0 * g {
                            for f in [0.2, 0.8] {
                                let q = if w > h { P::new(b.min.x + w * f, c.y) } else { P::new(c.x, b.min.y + h * f) };
                                if term.shape.contains(q) {
                                    out.push(End { slot: s, at: q, on: None });
                                }
                            }
                        }
                    }
                    super::model::Anchor::Segment(a, b) => {
                        let n = ((a.dist(b) / (2.0 * g)).ceil() as usize).clamp(1, 64);
                        for k in 0..=n {
                            let t = k as f64 / n as f64;
                            out.push(End {
                                slot: s,
                                at: P::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t),
                                on: None,
                            });
                        }
                    }
                }
            }
        }
    }
    for &wi in wires {
        let w = &nr.wires[wi];
        match &w.geo {
            Some(pts) => {
                for k in 0..pts.len() {
                    let (s, p) = pts[k];
                    out.push(End { slot: s, at: p, on: None });
                    if k + 1 < pts.len() && pts[k + 1].0 == s {
                        let q = pts[k + 1].1;
                        let n = (p.dist(q) / (2.0 * g)).floor() as usize;
                        for j in 1..n {
                            let t = j as f64 / n as f64;
                            let at = P::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t);
                            out.push(End { slot: s, at, on: Some((wi, k)) });
                        }
                    }
                }
            }
            None => {
                for &n in &w.nodes {
                    let (s, _, _) = eng.unpack(n);
                    out.push(End { slot: s, at: eng.node_pos(n), on: None });
                }
            }
        }
    }
    out
}

/// Per-unit-length cost factor of a direction on a slot: 1 along the preferred direction,
/// `Costs::diag` at 45°, `Costs::wrong_way` across it (as the grid's steps), in between by the
/// tangent of the angle.
fn dir_factor(eng: &Engine<'_>, s: usize, d: P) -> f32 {
    let (ax, ay) = (d.x.abs(), d.y.abs());
    let (along, across) = match eng.pref[s] {
        0 => (ax, ay),
        1 => (ay, ax),
        _ => (ax.max(ay), ax.min(ay)),
    };
    if along <= 0.0 && across <= 0.0 {
        return 1.0;
    }
    let diag = eng.costs.diag;
    if across <= along {
        1.0 + (diag - 1.0) * (across / along) as f32
    } else {
        diag + (eng.costs.wrong_way - diag) * (1.0 - (along / across) as f32)
    }
}

/// Cost of the segment `a`–`b` on slot `s` in grid units, with the PathFinder costs sampled
/// every half pitch in negotiation.
fn seg_cost(eng: &Engine<'_>, ai: usize, s: usize, a: P, b: P, pres: Option<f32>) -> f32 {
    let len = a.dist(b);
    let g = eng.grid.g;
    let f = dir_factor(eng, s, P::new(b.x - a.x, b.y - a.y));
    let Some(pres) = pres else { return (len / g) as f32 * f };
    let h = g / 2.0;
    let n = ((len / h).ceil() as usize).max(1);
    let step = (len / n as f64 / g) as f32;
    let mut c = 0.0f32;
    for k in 0..n {
        let t = (k as f64 + 0.5) / n as f64;
        let q = P::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
        let i = (((q.x - eng.grid.ox) / h).round() as i32).clamp(0, eng.grid.dw() - 1);
        let j = (((q.y - eng.grid.oy) / h).round() as i32).clamp(0, eng.grid.dh() - 1);
        let idx = eng.grid.didx(i, j);
        let occ = eng.occ_t[ai][s][idx] as f32;
        c += (step * f + eng.hist_t[s][idx]) * (1.0 + pres * occ);
    }
    c
}

/// Bend cost between unit directions `d0` and `d1` (as the grid's 45° and 90° bends, in
/// between by the cosine of the angle; sharper bends are allowed but dear).
fn bend_cost(eng: &Engine<'_>, d0: (f32, f32), d1: (f32, f32)) -> f32 {
    const C45: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let dot = (d0.0 * d1.0 + d0.1 * d1.1).clamp(-1.0, 1.0);
    let (b45, b90) = (eng.costs.bend45, eng.costs.bend90);
    if dot >= 0.9998 {
        0.0
    } else if dot >= C45 {
        b45 * (1.0 - dot) / (1.0 - C45)
    } else if dot >= 0.0 {
        b45 + (b90 - b45) * (C45 - dot) / C45
    } else {
        b90 * (1.0 - 3.0 * dot)
    }
}

/// The via cost at `at`, with the PathFinder costs of the nearest via cell in negotiation.
fn via_cost(eng: &Engine<'_>, ai: usize, at: P, pres: Option<f32>) -> f32 {
    let g = eng.grid.g;
    let x = (((at.x - eng.grid.ox) / g).round() as i32).clamp(0, eng.grid.w - 1);
    let y = (((at.y - eng.grid.oy) / g).round() as i32).clamp(0, eng.grid.h - 1);
    let cell = eng.grid.cidx(x, y);
    match pres {
        Some(p) => (eng.costs.via + eng.hist_v[cell]) * (1.0 + p * eng.occ_v[ai][cell] as f32),
        None => eng.costs.via,
    }
}

/// A found path: nodes from a source to a target, with the room each was reached through.
struct Found {
    nodes: Vec<u32>,
    rooms: Vec<u32>,
}

/// Per-thread working memory of the gridless A* (reused between searches: entries are valid
/// when their stamp is the current one).
#[derive(Debug, Default)]
pub(crate) struct GScratch {
    stamp: Vec<u32>,
    now: u32,
    gc: Vec<f32>,
    parent: Vec<u32>,
    room: Vec<u32>,
    dir: Vec<(f32, f32)>,
    /// Bit 0: closed; bit 1: cost not checked yet (lazy).
    flags: Vec<u8>,
}

impl GScratch {
    fn reset(&mut self, n: usize) {
        if self.stamp.len() < n {
            self.stamp.resize(n, 0);
            self.gc.resize(n, 0.0);
            self.parent.resize(n, 0);
            self.room.resize(n, 0);
            self.dir.resize(n, (0.0, 0.0));
            self.flags.resize(n, 0);
        }
        self.now = self.now.wrapping_add(1);
        if self.now == 0 {
            self.stamp.iter_mut().for_each(|v| *v = 0);
            self.now = 1;
        }
    }

    fn seen(&self, i: usize) -> bool {
        self.stamp[i] == self.now
    }

    fn g(&self, i: usize) -> f32 {
        if self.seen(i) { self.gc[i] } else { f32::INFINITY }
    }

    fn closed(&self, i: usize) -> bool {
        self.seen(i) && self.flags[i] & 1 != 0
    }

    fn set(&mut self, i: usize, g: f32, parent: u32, room: u32, dir: (f32, f32), lazy: bool) {
        self.stamp[i] = self.now;
        self.gc[i] = g;
        self.parent[i] = parent;
        self.room[i] = room;
        self.dir[i] = dir;
        self.flags[i] = if lazy { 2 } else { 0 };
    }
}

/// No direction yet (start, after a via).
const NO_DIR: (f32, f32) = (0.0, 0.0);

/// A* over the rooms of `rooms` from `sources` to `targets` (both located in the rooms). In
/// negotiation an edge is first priced by its length (a lower bound) and its congestion is
/// sampled only when its end is taken from the queue (lazy evaluation).
#[allow(clippy::too_many_arguments)]
fn astar(
    eng: &Engine<'_>,
    sc: &mut Scratch,
    net: u32,
    ai: usize,
    rooms: &NetRooms,
    sources: &[End],
    targets: &[End],
    pres: Option<f32>,
) -> Result<Option<(Found, Vec<Node>)>, Stop> {
    let rb = eng.rb;
    let pr = rb.profile(net);
    let own_min = 2.0 * pr.dr + rb.h2h - TOL;
    // Per-search nodes: the fixed ones, then sources and targets.
    let base = rooms.nodes.len();
    let mut extra: Vec<Node> = Vec::new();
    // Per slot: (room, node) of the per-search nodes.
    let mut extra_room: Vec<Vec<(u32, u32)>> = vec![Vec::new(); rooms.layers.len()];
    let mut tb = BoxF::EMPTY;
    for (k, e) in sources.iter().chain(targets).enumerate() {
        let Some(r) = rooms.layers[e.slot].locate(e.at, 0.5) else { continue };
        let kind = if k < sources.len() { Kind::Source } else { Kind::Target((k - sources.len()) as u32) };
        if matches!(kind, Kind::Target(_)) {
            tb.add(e.at);
        }
        let id = (base + extra.len()) as u32;
        extra.push(Node { slot: e.slot, at: e.at, rooms: [r, NONE], kind });
        extra_room[e.slot].push((r, id));
    }
    if tb.is_empty() || !extra.iter().any(|n| n.kind == Kind::Source) {
        return Ok(None);
    }
    for v in &mut extra_room {
        v.sort_unstable();
    }
    let node = |i: u32| -> &Node {
        let i = i as usize;
        if i < base { &rooms.nodes[i] } else { &extra[i - base] }
    };
    let total = base + extra.len();
    let g = eng.grid.g;
    // Negotiation searches whole net regions with lazily priced edges: a weighted heuristic
    // keeps them from expanding every room (the exact searches are local and stay optimal).
    let w = if pres.is_some() { eng.costs.weight * NEGOTIATE_WEIGHT } else { eng.costs.weight };
    let heur = |p: P| -> f32 { (tb.dist(p) / g) as f32 * w };
    let own_vias = std::mem::take(&mut sc.own_via_pts);
    let st = &mut sc.gl;
    st.reset(total);
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    for (k, n) in extra.iter().enumerate() {
        if n.kind == Kind::Source {
            let id = base + k;
            st.set(id, 0.0, u32::MAX, NONE, NO_DIR, false);
            heap.push(Reverse((heur(n.at).to_bits(), id as u32)));
        }
    }
    let mut pops = 0usize;
    let mut neigh: Vec<u32> = Vec::new();
    let mut result = None;
    while let Some(Reverse((fb, u))) = heap.pop() {
        let ui = u as usize;
        if st.closed(ui) {
            continue;
        }
        let nu = *node(u);
        if f32::from_bits(fb) > st.gc[ui] + heur(nu.at) + 1e-3 {
            continue; // stale
        }
        if st.flags[ui] & 2 != 0 {
            // Lazy: price the edge from the parent with its congestion now.
            let p = st.parent[ui];
            let np = *node(p);
            let mut c = st.gc[p as usize] + seg_cost(eng, ai, nu.slot, np.at, nu.at, pres);
            let pd = st.dir[p as usize];
            if pd != NO_DIR && st.dir[ui] != NO_DIR {
                c += bend_cost(eng, pd, st.dir[ui]);
            }
            st.flags[ui] &= !2;
            if c > st.gc[ui] + 1e-4 {
                st.gc[ui] = c;
                heap.push(Reverse(((c + heur(nu.at)).to_bits(), u)));
                continue;
            }
        }
        st.flags[ui] |= 1;
        if let Kind::Target(_) = nu.kind {
            let mut nodes = vec![u];
            let mut rms = vec![st.room[ui]];
            let mut c = u;
            while st.parent[c as usize] != u32::MAX {
                c = st.parent[c as usize];
                nodes.push(c);
                rms.push(st.room[c as usize]);
            }
            nodes.reverse();
            rms.reverse();
            result = Some(Found { nodes, rooms: rms });
            break;
        }
        pops += 1;
        if pops & 1023 == 0 && (eng.stop)() {
            sc.own_via_pts = own_vias;
            return Err(Stop);
        }
        if pops > eng.max_expansions {
            break;
        }
        let gu = st.gc[ui];
        let du = st.dir[ui];
        let s = nu.slot;
        for &r in nu.rooms.iter().filter(|&&r| r != NONE) {
            neigh.clear();
            neigh.extend(rooms.room_nodes[s][r as usize].iter().copied());
            let ex = &extra_room[s];
            let lo = ex.partition_point(|e| e.0 < r);
            neigh.extend(ex[lo..].iter().take_while(|e| e.0 == r).map(|e| e.1));
            for &v in &neigh {
                let vi = v as usize;
                if v == u || st.closed(vi) {
                    continue;
                }
                let nv = node(v);
                if nv.kind == Kind::Source {
                    continue;
                }
                if let (Kind::Portal(a), Kind::Portal(b)) = (nu.kind, nv.kind)
                    && a == b
                {
                    continue;
                }
                let d = P::new(nv.at.x - nu.at.x, nv.at.y - nu.at.y);
                let len = (d.x * d.x + d.y * d.y).sqrt();
                let nd = if len > 1.0 { ((d.x / len) as f32, (d.y / len) as f32) } else { du };
                // Lower bound (length only) in negotiation, exact otherwise.
                let lazy = pres.is_some();
                let mut c = gu
                    + if lazy {
                        seg_cost(eng, ai, s, nu.at, nv.at, None)
                    } else {
                        seg_cost(eng, ai, s, nu.at, nv.at, pres)
                    };
                if du != NO_DIR && len > 1.0 {
                    c += bend_cost(eng, du, nd);
                }
                if c < st.g(vi) {
                    st.set(vi, c, u, r, nd, lazy);
                    heap.push(Reverse(((c + heur(nv.at)).to_bits(), v)));
                }
            }
        }
        // A via at a site: to the same site on the other slots.
        if let Kind::Site(vi) = nu.kind {
            let at = nu.at;
            let ok = own_vias.iter().all(|&o| o.dist(at) < 1.0 || o.dist(at) >= own_min);
            if ok {
                let c = gu + via_cost(eng, ai, at, pres);
                for &v in &rooms.site_nodes[vi as usize] {
                    let i = v as usize;
                    if v == u || st.closed(i) || c >= st.g(i) {
                        continue;
                    }
                    st.set(i, c, u, NONE, NO_DIR, false);
                    heap.push(Reverse(((c + heur(at)).to_bits(), v)));
                }
            }
        }
    }
    sc.own_via_pts = own_vias;
    Ok(result.map(|f| (f, extra)))
}

fn cross(a: P, b: P, c: P) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Shortest path from `start` to `end` through `portals` (each `(left, right)` seen in the
/// direction of travel), by the funnel algorithm.
fn funnel(start: P, end: P, portals: &[(P, P)]) -> Vec<P> {
    let mut pts: Vec<(P, P)> = Vec::with_capacity(portals.len() + 2);
    pts.push((start, start));
    pts.extend_from_slice(portals);
    pts.push((end, end));
    let mut out = vec![start];
    let (mut apex, mut left, mut right) = (start, start, start);
    let (mut ai, mut li, mut ri) = (0usize, 0usize, 0usize);
    let mut i = 1;
    let mut guard = 0usize;
    while i < pts.len() {
        guard += 1;
        if guard > 16 * pts.len() + 64 {
            break;
        }
        let (l, r) = pts[i];
        // Tighten the right side.
        if cross(apex, right, r) >= 0.0 {
            if ai == ri || cross(apex, left, r) < 0.0 {
                right = r;
                ri = i;
            } else {
                // The right side crosses the left: the left point is a corner.
                apex = left;
                ai = li;
                out.push(apex);
                right = apex;
                ri = ai;
                i = ai + 1;
                continue;
            }
        }
        // Tighten the left side.
        if cross(apex, left, l) <= 0.0 {
            if ai == li || cross(apex, right, l) > 0.0 {
                left = l;
                li = i;
            } else {
                apex = right;
                ai = ri;
                out.push(apex);
                left = apex;
                li = ai;
                i = ai + 1;
                continue;
            }
        }
        i += 1;
    }
    if out.last().is_none_or(|q| q.dist(end) > 0.5) {
        out.push(end);
    }
    out.dedup_by(|a, b| a.dist(*b) < 0.5);
    out
}

/// Whether segment `a`–`b` of `net` on stackup layer `layer` is legal: against the static
/// obstacles in negotiation, against everything for an exact search.
fn legal(ck: &Checker<'_>, layer: usize, a: P, b: P, net: u32, exact: bool) -> bool {
    if exact { ck.seg(layer, a, b, net).is_none() } else { ck.seg_static(layer, a, b, net).is_none() }
}

fn octilinear_ok(a: P, b: P) -> bool {
    let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
    dx < 2.0 || dy < 2.0 || (dx - dy).abs() < 2.0
}

/// Replaces the segments of `pts` that are not 0°/45°/90° by a legal two-segment octilinear
/// path where there is one; keeps them otherwise.
fn octilinearize(pts: &[P], ok: &dyn Fn(P, P) -> bool) -> Vec<P> {
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if !octilinear_ok(a, b)
            && let Some(m) = super::post::octilinear(a, b).into_iter().find_map(|mid| match mid[..] {
                [m] if ok(a, m) && ok(m, b) => Some(m),
                _ => None,
            })
        {
            out.push(m);
        }
        out.push(b);
    }
    out.dedup_by(|a, b| a.dist(*b) < 0.5);
    out
}

/// The polyline of a found path: per layer run, the funnel through the portals crossed, made
/// octilinear where legal; vias between runs. `None` when the result fails the final checks.
#[allow(clippy::too_many_arguments)]
fn polyline(
    eng: &Engine<'_>,
    rooms: &NetRooms,
    extra: &[Node],
    f: &Found,
    net: u32,
    exact: bool,
    own_vias: &[P],
) -> Option<Vec<(usize, P)>> {
    let base = rooms.nodes.len();
    let node = |i: u32| -> Node {
        let i = i as usize;
        if i < base { rooms.nodes[i] } else { extra[i - base] }
    };
    let ck = Checker { rb: eng.rb, index: &eng.dynamic };
    let mut out: Vec<(usize, P)> = Vec::new();
    let mut k = 0;
    while k < f.nodes.len() {
        // A run: nodes k..=e on one slot.
        let s = node(f.nodes[k]).slot;
        let mut e = k;
        while e + 1 < f.nodes.len() && node(f.nodes[e + 1]).slot == s {
            e += 1;
        }
        let layer = eng.slots[s];
        let lr = &rooms.layers[s];
        let start = node(f.nodes[k]).at;
        let end = node(f.nodes[e]).at;
        let mut portals: Vec<(P, P)> = Vec::new();
        let mut raw: Vec<P> = vec![start];
        for j in k + 1..e {
            let n = node(f.nodes[j]);
            let (r0, r1) = (f.rooms[j], f.rooms[j + 1]);
            raw.push(n.at);
            match n.kind {
                Kind::Portal(pi) if r0 != r1 && r0 != NONE && r1 != NONE => {
                    let p = lr.portals[pi as usize];
                    let (ca, cb) = (lr.rooms[r0 as usize].center(), lr.rooms[r1 as usize].center());
                    let d = P::new(cb.x - ca.x, cb.y - ca.y);
                    let m = P::new((p.a.x + p.b.x) / 2.0, (p.a.y + p.b.y) / 2.0);
                    let left_a = d.x * (p.a.y - m.y) - d.y * (p.a.x - m.x) > 0.0;
                    portals.push(if left_a { (p.a, p.b) } else { (p.b, p.a) });
                }
                Kind::Portal(_) | Kind::Site(_) => {}
                // Terminal points passed on the way: kept as corners.
                _ => portals.push((n.at, n.at)),
            }
        }
        raw.push(end);
        raw.dedup_by(|a, b| a.dist(*b) < 0.5);
        let ok = |a: P, b: P| legal(&ck, layer, a, b, net, exact);
        let pulled = funnel(start, end, &portals);
        let mut run = if pulled.windows(2).all(|w| ok(w[0], w[1])) { pulled } else { raw };
        if !run.windows(2).all(|w| ok(w[0], w[1])) {
            return None;
        }
        if !eng.any_angle {
            run = octilinearize(&run, &ok);
        }
        for p in run {
            if out.last().is_none_or(|&(ls, lp)| ls != s || lp.dist(p) >= 0.5) {
                out.push((s, p));
            }
        }
        k = e + 1;
    }
    // Vias: legal, and hole-to-hole with the net's other vias.
    let pr = eng.rb.profile(net);
    let own_min = 2.0 * pr.dr + eng.rb.h2h - TOL;
    let mut vias: Vec<P> = own_vias.to_vec();
    for w in out.windows(2) {
        if w[0].0 != w[1].0 {
            let at = w[0].1;
            let bad = if exact { ck.via(at, net, None).is_some() } else { ck.via_static(at, net).is_some() };
            if bad || vias.iter().any(|&o| o.dist(at) >= 1.0 && o.dist(at) < own_min) {
                return None;
            }
            vias.push(at);
        }
    }
    Some(out)
}

/// The region of an exact search between `a` and `b` ends.
fn local_region(sources: &[End], targets: &[End]) -> BoxF {
    let mut b = BoxF::EMPTY;
    for e in sources.iter().chain(targets) {
        b.add(e.at);
    }
    let side = (b.max.x - b.min.x).max(b.max.y - b.min.y);
    b.expand(LOCAL_MARGIN + 0.5 * side)
}

/// A found connection: its polyline and the start and end points it joins.
type Connected = (Vec<(usize, P)>, End, End);

/// Searches one connection from `sources` to `targets`; returns the polyline and the ends
/// reached.
#[allow(clippy::too_many_arguments)]
fn connect(
    eng: &Engine<'_>,
    sc: &mut Scratch,
    nr: &mut NetRoute,
    sources: &[End],
    targets: &[End],
    mode: Mode,
) -> Result<Option<Connected>, Stop> {
    let net = nr.net;
    let ai = eng.act[nr.prof];
    let pres = match mode {
        Mode::Negotiate(p) => Some(p),
        _ => None,
    };
    let exact = pres.is_none();
    let mut tries: Vec<(BoxF, bool)> = Vec::new(); // (region, cache in nr)
    if exact {
        let local = local_region(sources, targets);
        tries.push((local, false));
        let wide = local.union(nr.region).expand(LOCAL_MARGIN);
        tries.push((wide, false));
    } else {
        tries.push((nr.region, true));
    }
    let mut last: Option<BoxF> = None;
    for (region, cache) in tries {
        if last.is_some_and(|l| {
            l.min.x <= region.min.x && l.min.y <= region.min.y && l.max.x >= region.max.x && l.max.y >= region.max.y
        }) {
            continue;
        }
        last = Some(region);
        let rooms: Arc<NetRooms> = match (&nr.rooms, cache) {
            (Some(r), true) if r.region == region => r.clone(),
            _ => {
                let against = if exact { Against::All } else { Against::Static };
                let r = Arc::new(NetRooms::build(eng, net, region, against));
                if cache {
                    nr.rooms = Some(r.clone());
                }
                r
            }
        };
        let res = astar(eng, sc, net, ai, &rooms, sources, targets, pres)?;
        let Some((found, extra)) = res else { continue };
        let first = extra[found.nodes[0] as usize - rooms.nodes.len()];
        let last_n = extra[*found.nodes.last().expect("node") as usize - rooms.nodes.len()];
        let Some(pts) = polyline(eng, &rooms, &extra, &found, net, exact, &sc.own_via_pts) else { continue };
        let pick = |n: Node, list: &[End]| -> End {
            *list.iter().find(|e| e.slot == n.slot && e.at.dist(n.at) < 0.5).expect("end")
        };
        return Ok(Some((pts, pick(first, sources), pick(last_n, targets))));
    }
    Ok(None)
}

/// Inserts a vertex at `e.at` into the gridless wire it lies on (so the junction is pinned).
fn attach(nr: &mut NetRoute, e: End) {
    if let Some((wi, k)) = e.on
        && let Some(g) = nr.wires[wi].geo.as_mut()
        && k + 1 < g.len()
    {
        g.insert(k + 1, (e.slot, e.at));
    }
}

/// Routes the connections of a net with the gridless search (see `Engine::route_net`).
pub(crate) fn route_net(eng: &Engine<'_>, sc: &mut Scratch, nr: &mut NetRoute, mode: Mode) -> Result<(), Stop> {
    nr.wires.clear();
    let mut tree = Tree::of(eng, sc, nr);
    for ci in 0..nr.conns.len() {
        if matches!(nr.failed[ci], Some(FailKind::Static | FailKind::NoAccess | FailKind::Drc)) {
            continue;
        }
        nr.failed[ci] = None;
        let Some((sources, targets)) = tree.ends(eng, nr, ci) else { continue };
        match connect(eng, sc, nr, &sources, &targets, mode)? {
            Some((pts, s, t)) => tree.add(sc, nr, ci, pts, s, t),
            None => {
                nr.failed[ci] = Some(match mode {
                    Mode::Negotiate(_) if eng.max_expansions == usize::MAX => FailKind::Static,
                    Mode::Negotiate(_) => FailKind::Budget,
                    _ => FailKind::Congestion,
                });
            }
        }
    }
    Ok(())
}

/// The copper islands of a net joined by its wires: union-find over islands, with the islands
/// and wires of each component.
struct Tree {
    comp: Vec<usize>,
    members: Vec<Vec<usize>>,
    wires_of: Vec<Vec<usize>>,
}

impl Tree {
    fn of(eng: &Engine<'_>, sc: &mut Scratch, nr: &NetRoute) -> Tree {
        let n = nr.islands.len();
        let mut t =
            Tree { comp: (0..n).collect(), members: (0..n).map(|i| vec![i]).collect(), wires_of: vec![Vec::new(); n] };
        sc.own_via_pts.clear();
        for (wi, w) in nr.wires.iter().enumerate() {
            let c = &nr.conns[w.conn];
            let ra = t.join(c.a, c.b);
            t.wires_of[ra].push(wi);
            sc.own_via_pts.extend(eng.wire_geometry(w).1);
        }
        t
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.comp[i] != i {
            self.comp[i] = self.comp[self.comp[i]];
            i = self.comp[i];
        }
        i
    }

    /// Joins the components of islands `a` and `b`; returns the root.
    fn join(&mut self, a: usize, b: usize) -> usize {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            let (mb, wb) = (std::mem::take(&mut self.members[rb]), std::mem::take(&mut self.wires_of[rb]));
            self.members[ra].extend(mb);
            self.wires_of[ra].extend(wb);
            self.comp[rb] = ra;
        }
        ra
    }

    /// Start and end points of connection `ci`, or `None` when its islands are joined.
    fn ends(&mut self, eng: &Engine<'_>, nr: &NetRoute, ci: usize) -> Option<(Vec<End>, Vec<End>)> {
        let (ra, rb) = (self.find(nr.conns[ci].a), self.find(nr.conns[ci].b));
        if ra == rb {
            return None;
        }
        Some((
            ends_of(eng, nr, &self.members[ra], &self.wires_of[ra]),
            ends_of(eng, nr, &self.members[rb], &self.wires_of[rb]),
        ))
    }

    /// Adds a found path for connection `ci` as a gridless wire.
    fn add(&mut self, sc: &mut Scratch, nr: &mut NetRoute, ci: usize, pts: Vec<(usize, P)>, s: End, t: End) {
        attach(nr, s);
        attach(nr, t);
        for w in pts.windows(2) {
            if w[0].0 != w[1].0 {
                sc.own_via_pts.push(w[0].1);
            }
        }
        let ra = self.join(nr.conns[ci].a, nr.conns[ci].b);
        self.wires_of[ra].push(nr.wires.len());
        nr.wires.push(Wire::gridless(ci, pts));
        nr.failed[ci] = None;
    }
}

/// Whether a failure is one the gridless search may fix.
pub(crate) fn retryable(f: &Option<FailKind>) -> bool {
    matches!(f, Some(FailKind::Congestion | FailKind::Static | FailKind::NoAccess))
}

/// Routes the failed connections of a net with exact gridless searches, keeping its wires
/// (the `auto` search: grid first, then this). Returns the number of connections routed.
pub(crate) fn route_failed(eng: &Engine<'_>, sc: &mut Scratch, nr: &mut NetRoute) -> Result<usize, Stop> {
    let mut tree = Tree::of(eng, sc, nr);
    let mut routed = 0;
    for ci in 0..nr.conns.len() {
        if !retryable(&nr.failed[ci]) {
            continue;
        }
        let Some((sources, targets)) = tree.ends(eng, nr, ci) else {
            nr.failed[ci] = None;
            continue;
        };
        if let Some((pts, s, t)) = connect(eng, sc, nr, &sources, &targets, Mode::Hard)? {
            tree.add(sc, nr, ci, pts, s, t);
            routed += 1;
        }
    }
    Ok(routed)
}

/// Present-congestion factor of [`probe`]: other nets' routing is passable, at a price.
const PROBE_PRESENT: f32 = 4.0;

/// A path for the failed connection `ci` of `nr` that keeps every static rule but may run
/// through other nets' routing (priced by the occupancy maps): the way to make room for it.
pub(crate) fn probe(
    eng: &Engine<'_>,
    sc: &mut Scratch,
    nr: &mut NetRoute,
    ci: usize,
) -> Result<Option<Vec<(usize, P)>>, Stop> {
    let mut tree = Tree::of(eng, sc, nr);
    let Some((sources, targets)) = tree.ends(eng, nr, ci) else { return Ok(None) };
    Ok(connect(eng, sc, nr, &sources, &targets, Mode::Negotiate(PROBE_PRESENT))?.map(|r| r.0))
}

/// Adds `pts` (from [`probe`], legal now that the nets in its way are gone) as the wire of
/// connection `ci`.
pub(crate) fn add_probe(eng: &Engine<'_>, sc: &mut Scratch, nr: &mut NetRoute, ci: usize, pts: Vec<(usize, P)>) {
    let mut tree = Tree::of(eng, sc, nr);
    let Some((sources, targets)) = tree.ends(eng, nr, ci) else { return };
    let find = |list: &[End], s: usize, p: P| list.iter().copied().find(|e| e.slot == s && e.at.dist(p) < 0.5);
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    let (Some(s), Some(t)) = (find(&sources, first.0, first.1), find(&targets, last.0, last.1)) else { return };
    tree.add(sc, nr, ci, pts, s, t);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funnel_pulls_the_path_tight_around_corners() {
        // A corridor turning around the corner (10, 10): portals left/right in travel order.
        let start = P::new(0.0, 0.0);
        let end = P::new(20.0, 20.0);
        // Going right along y in [0, 10] then up through x in [10, 20].
        let portals = [(P::new(5.0, 10.0), P::new(5.0, -5.0)), (P::new(10.0, 10.0), P::new(20.0, 10.0))];
        let p = funnel(start, end, &portals);
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(p[1].dist(P::new(10.0, 10.0)) < 1e-9, "corner at the inside of the turn: {p:?}");
        // A straight corridor gives a straight line.
        let portals = [(P::new(5.0, 5.0), P::new(5.0, -5.0)), (P::new(10.0, 5.0), P::new(10.0, -5.0))];
        assert_eq!(funnel(P::new(0.0, 0.0), P::new(20.0, 0.0), &portals).len(), 2);
    }

    #[test]
    fn octilinear_replacement_keeps_legal_segments() {
        let pts = [P::new(0.0, 0.0), P::new(30.0, 10.0)];
        let out = octilinearize(&pts, &|_, _| true);
        assert_eq!(out.len(), 3);
        assert!(out.windows(2).all(|w| octilinear_ok(w[0], w[1])));
        // Nothing legal: the any-angle segment stays.
        assert_eq!(octilinearize(&pts, &|a, b| a.dist(b) > 31.0).len(), 2);
    }
}
