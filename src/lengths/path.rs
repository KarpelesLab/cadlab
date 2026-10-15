//! Signal paths: the routed length of a net between its end points, with the branches off it
//! (stubs) measured separately (docs/ROUTER.md, "Length tuning").
//!
//! A net's copper (tracks, vias, pads) is turned into a graph: track ends and T-junctions are
//! nodes on their layer, tracks are edges weighted by their length (along arcs), a via joins
//! its layers with edges weighted by the distance between their centers through the stackup
//! ([`super::layer_depths`]), and a pad joins every track end or via touching its copper. The
//! signal path is the shortest path between the pads of the two end components (a
//! receptacle's other contact row then hangs off it as a stub), and everything else of the
//! net is grouped into stubs, each with the pads it reaches.
//!
//! End components are given (`diffpair.set --from --to`, `lengthgroup.set --from --to`) or
//! chosen: for a pair, among the components with pads on both nets (the connector, an ESD
//! array, the transceiver; a pull-up has a pad on one net only), the two farthest apart along
//! the routing; for a net, among all its components. Everything is deterministic: nodes and
//! edges follow the board's order and ties go to the lower index.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{NetLength, layer_depths};
use crate::board::{self as geo, PlacedPad};
use crate::geom::Point;
use crate::geom::poly;
use crate::model::Project;
use crate::model::board::Track;
use crate::units::Nm;

/// Stub length above which the DRC warns when the pair or group sets no `max_stub`: 5 mm.
pub const DEFAULT_MAX_STUB: Nm = Nm(5_000_000);
/// Track ends closer than this (nm) are one node.
const JOIN: f64 = 1_000.0;

type F2 = (f64, f64);

fn f2(p: Point) -> F2 {
    (p.x.0 as f64, p.y.0 as f64)
}

fn dist(a: F2, b: F2) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn to_point(a: F2) -> Point {
    Point::new(Nm(a.0.round() as i64), Nm(a.1.round() as i64))
}

/// A branch of a net off its signal path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Stub {
    /// Track length plus via spans.
    pub length: Nm,
    /// Where it leaves the signal path (or, for copper not attached to it, where it starts).
    pub at: Point,
    /// Pads it reaches (`R3.2`), not counting the path's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pads: Vec<String>,
}

/// The routed signal path of a net between its end points.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SignalPath {
    /// Net.
    pub net: String,
    /// Pad where the path starts (`J1.A6`).
    pub from: String,
    /// Pad where it ends.
    pub to: String,
    /// Length along the path: tracks, vias (between the layers where the path enters and
    /// leaves each), total.
    pub length: NetLength,
    /// Copper of the net off the path: the sum of the stubs.
    pub stub_length: Nm,
    /// Branches off the path, longest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stubs: Vec<Stub>,
    /// Per track on the path: the stretches of it on the path (distances from its start).
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) on_path: BTreeMap<u64, Vec<(f64, f64)>>,
    /// Tracks entirely on the path.
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) whole: BTreeSet<u64>,
}

impl SignalPath {
    /// Whether the track with this ID lies entirely on the path.
    pub fn contains_track(&self, id: u64) -> bool {
        self.whole.contains(&id)
    }

    /// The stretches of a track on the path (distances from its start; the whole track for
    /// an arc).
    pub(crate) fn stretches(&self, id: u64) -> &[(f64, f64)] {
        self.on_path.get(&id).map_or(&[], |v| v.as_slice())
    }
}

#[derive(Clone, Copy, Debug)]
enum EdgeKind {
    /// A stretch of a track: (id, from, to) as distances from its start.
    Track(u64, f64, f64),
    /// A via between two of its layers.
    Via,
    /// Copper touching (zero length).
    Join,
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    a: usize,
    b: usize,
    w: f64,
    kind: EdgeKind,
}

#[derive(Clone, Debug)]
struct NodeInfo {
    at: F2,
    /// Layer of a track node or a via node; `None` for a pad (all its layers).
    layer: Option<String>,
    /// Pad: index into the graph's pads.
    pad: Option<usize>,
    /// Via node: (via index, layer index).
    via: Option<(usize, usize)>,
    /// Largest half width of the tracks ending here.
    hw: f64,
}

/// A net's copper as a graph.
pub(crate) struct NetGraph {
    net: String,
    nodes: Vec<NodeInfo>,
    edges: Vec<Edge>,
    adj: Vec<Vec<usize>>,
    /// Pads: (designator, number, node).
    pads: Vec<(String, String, usize)>,
    /// Track lengths by ID.
    track_len: BTreeMap<u64, f64>,
}

#[derive(Clone, Copy, PartialEq)]
struct Queued(f64, usize);
impl Eq for Queued {}
impl Ord for Queued {
    fn cmp(&self, o: &Self) -> Ordering {
        // Min-heap on distance, then node index.
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}
impl PartialOrd for Queued {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Closest point of segment `a`–`b` to `q`: (distance along the segment, distance to it).
fn project(q: F2, a: F2, b: F2) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l = dx.hypot(dy);
    if l < 1e-9 {
        return (0.0, dist(q, a));
    }
    let t = (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / l).clamp(0.0, l);
    let c = (a.0 + dx * t / l, a.1 + dy * t / l);
    (t, dist(q, c))
}

impl NetGraph {
    /// Builds the graph of `net` from the board's tracks and vias and the given placed pads
    /// (those of other nets are skipped).
    pub(crate) fn new(p: &Project, net: &str, all_pads: &[PlacedPad]) -> NetGraph {
        let b = p.board();
        let tracks: Vec<&Track> = b.tracks.iter().filter(|t| t.net.as_deref() == Some(net)).collect();
        let mut g = NetGraph {
            net: net.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
            adj: Vec::new(),
            pads: Vec::new(),
            track_len: BTreeMap::new(),
        };
        // Split points of straight tracks where another track of the net ends on them, and the
        // joins that go with them: (end layer, end point, track, distance along it).
        let mut splits: Vec<Vec<f64>> = tracks
            .iter()
            .map(|t| if t.mid.is_some() { vec![] } else { vec![0.0, dist(f2(t.start), f2(t.end))] })
            .collect();
        let mut joins: Vec<(usize, F2, usize, f64)> = Vec::new();
        for (i, ti) in tracks.iter().enumerate() {
            for e in [f2(ti.start), f2(ti.end)] {
                for (j, tj) in tracks.iter().enumerate() {
                    if i == j || tj.layer != ti.layer || tj.mid.is_some() {
                        continue;
                    }
                    // A track ending on another one's centerline (within its half width, off
                    // its ends: ends meet only where they coincide, so the corners of a
                    // meander or a skew bump never short-cut it).
                    let (a, bb) = (f2(tj.start), f2(tj.end));
                    let (t, d) = project(e, a, bb);
                    let len = dist(a, bb);
                    if t > JOIN && t < len - JOIN && d <= (tj.width.0 as f64 / 2.0).max(JOIN) {
                        splits[j].push(t);
                        joins.push((i, e, j, t));
                    }
                }
            }
        }
        for (t, s) in tracks.iter().zip(&mut splits) {
            s.sort_by(f64::total_cmp);
            s.dedup_by(|x, y| (*x - *y).abs() < JOIN);
            // Keep the exact end.
            if let (Some(l), false) = (s.last_mut(), t.mid.is_some()) {
                *l = dist(f2(t.start), f2(t.end));
            }
        }
        // Track nodes and edges.
        for (k, t) in tracks.iter().enumerate() {
            let hw = t.width.0 as f64 / 2.0;
            let len = geo::track_length(t).0 as f64;
            g.track_len.insert(t.id.0, len);
            let (a, bb) = (f2(t.start), f2(t.end));
            if t.mid.is_some() {
                let na = g.pt_node(&t.layer, a, hw);
                let nb = g.pt_node(&t.layer, bb, hw);
                g.edge(na, nb, len, EdgeKind::Track(t.id.0, 0.0, len));
                continue;
            }
            let l = dist(a, bb).max(1e-9);
            let at = |s: f64| (a.0 + (bb.0 - a.0) * s / l, a.1 + (bb.1 - a.1) * s / l);
            let ps = &splits[k];
            let ns: Vec<usize> = ps.iter().map(|&s| g.pt_node(&t.layer, at(s), hw)).collect();
            for w in 0..ps.len().saturating_sub(1) {
                g.edge(ns[w], ns[w + 1], ps[w + 1] - ps[w], EdgeKind::Track(t.id.0, ps[w], ps[w + 1]));
            }
        }
        for (i, e, j, t) in joins {
            let l = dist(f2(tracks[j].start), f2(tracks[j].end)).max(1e-9);
            let (a, bb) = (f2(tracks[j].start), f2(tracks[j].end));
            let q = (a.0 + (bb.0 - a.0) * t / l, a.1 + (bb.1 - a.1) * t / l);
            let na = g.pt_node(&tracks[i].layer, e, 0.0);
            let nb = g.pt_node(&tracks[j].layer, q, 0.0);
            if na != nb {
                g.edge(na, nb, 0.0, EdgeKind::Join);
            }
        }
        // Vias: a node per layer reached, joined through the stackup.
        let names = b.stackup.copper_names();
        let depth = layer_depths(&b.stackup);
        let track_nodes = g.nodes.len();
        for (vi, v) in b.vias.iter().enumerate().filter(|(_, v)| v.net.as_deref() == Some(net)) {
            let span = geo::via_layers(p, v);
            let r = (v.diameter.0 as f64 / 2.0).max(JOIN);
            let c = f2(v.at);
            let mut layer_nodes: BTreeMap<usize, usize> = BTreeMap::new();
            for n in 0..track_nodes {
                let Some(layer) = g.nodes[n].layer.clone() else { continue };
                if !span.contains(&layer) || dist(g.nodes[n].at, c) > r {
                    continue;
                }
                let li = names.iter().position(|x| *x == layer).unwrap_or(0);
                let vn = match layer_nodes.get(&li) {
                    Some(&x) => x,
                    None => {
                        let x = g.node(NodeInfo { at: c, layer: Some(layer), pad: None, via: Some((vi, li)), hw: 0.0 });
                        layer_nodes.insert(li, x);
                        x
                    }
                };
                g.edge(n, vn, 0.0, EdgeKind::Join);
            }
            // Pads the via sits in.
            for pp in all_pads.iter().filter(|pp| pp.net.as_deref() == Some(net)) {
                for l in pp.layers.iter().filter(|l| span.contains(l)) {
                    let li = names.iter().position(|x| x == l).unwrap_or(0);
                    if layer_nodes.contains_key(&li) || !touches(pp.shape_on(l), c, 1.0) {
                        continue;
                    }
                    let x = g.node(NodeInfo { at: c, layer: Some(l.clone()), pad: None, via: Some((vi, li)), hw: 0.0 });
                    layer_nodes.insert(li, x);
                }
            }
            let ls: Vec<(usize, usize)> = layer_nodes.into_iter().collect();
            for (k, &(la, na)) in ls.iter().enumerate() {
                for &(lb, nb) in &ls[k + 1..] {
                    let w = (depth.get(lb).map_or(0, |d| d.0) - depth.get(la).map_or(0, |d| d.0)).abs() as f64;
                    g.edge(na, nb, w, EdgeKind::Via);
                }
            }
        }
        // Pads: joined to the track ends and via nodes touching their copper.
        let others = g.nodes.len();
        for pp in all_pads.iter().filter(|pp| pp.net.as_deref() == Some(net)) {
            let pi = g.pads.len();
            let pn = g.node(NodeInfo { at: f2(pp.center), layer: None, pad: Some(pi), via: None, hw: 0.0 });
            g.pads.push((pp.refdes.clone(), pp.number.clone(), pn));
            for n in 0..others {
                let Some(layer) = g.nodes[n].layer.clone() else { continue };
                if !pp.layers.contains(&layer) {
                    continue;
                }
                let r = if g.nodes[n].via.is_some() { 1.0 } else { g.nodes[n].hw.max(1.0) };
                if touches(pp.shape_on(&layer), g.nodes[n].at, r) {
                    g.edge(pn, n, 0.0, EdgeKind::Join);
                }
            }
        }
        g
    }

    fn node(&mut self, n: NodeInfo) -> usize {
        self.nodes.push(n);
        self.adj.push(Vec::new());
        self.nodes.len() - 1
    }

    /// The track node at `q` on `layer` (one within [`JOIN`] is reused).
    fn pt_node(&mut self, layer: &str, q: F2, hw: f64) -> usize {
        if let Some(i) = self.nodes.iter().position(|n| {
            n.via.is_none() && n.pad.is_none() && n.layer.as_deref() == Some(layer) && dist(n.at, q) < JOIN
        }) {
            self.nodes[i].hw = self.nodes[i].hw.max(hw);
            return i;
        }
        self.node(NodeInfo { at: q, layer: Some(layer.to_string()), pad: None, via: None, hw })
    }

    fn edge(&mut self, a: usize, b: usize, w: f64, kind: EdgeKind) {
        self.edges.push(Edge { a, b, w, kind });
        let e = self.edges.len() - 1;
        self.adj[a].push(e);
        if b != a {
            self.adj[b].push(e);
        }
    }

    /// Designators with pads on the net, in order.
    pub(crate) fn components(&self) -> BTreeSet<String> {
        self.pads.iter().map(|(r, _, _)| r.clone()).collect()
    }

    fn pad_nodes(&self, unit: &Unit) -> Vec<usize> {
        self.pads
            .iter()
            .filter(|(r, n, _)| match unit {
                Unit::Comp(c) => r == c,
                Unit::Pad(c, num) => r == c && n == num,
            })
            .map(|x| x.2)
            .collect()
    }

    /// Shortest distances from `sources`, with the edge each node was reached by.
    fn dijkstra(&self, sources: &[usize]) -> (Vec<f64>, Vec<Option<usize>>) {
        let mut d = vec![f64::INFINITY; self.nodes.len()];
        let mut prev = vec![None; self.nodes.len()];
        let mut q = BinaryHeap::new();
        for &s in sources {
            d[s] = 0.0;
            q.push(Queued(0.0, s));
        }
        while let Some(Queued(du, u)) = q.pop() {
            if du > d[u] {
                continue;
            }
            for &e in &self.adj[u] {
                let ed = &self.edges[e];
                let v = if ed.a == u { ed.b } else { ed.a };
                let nd = du + ed.w;
                if nd < d[v] {
                    d[v] = nd;
                    prev[v] = Some(e);
                    q.push(Queued(nd, v));
                }
            }
        }
        (d, prev)
    }

    /// Distance along the routing between two units (infinite when not connected).
    fn unit_distance(&self, a: &Unit, b: &Unit) -> f64 {
        let (d, _) = self.dijkstra(&self.pad_nodes(a));
        self.pad_nodes(b).into_iter().map(|n| d[n]).fold(f64::INFINITY, f64::min)
    }

    /// Straight-line distance between the closest pads of two units.
    fn unit_gap(&self, a: &Unit, b: &Unit) -> f64 {
        let (na, nb) = (self.pad_nodes(a), self.pad_nodes(b));
        na.iter()
            .flat_map(|&x| nb.iter().map(move |&y| (x, y)))
            .map(|(x, y)| dist(self.nodes[x].at, self.nodes[y].at))
            .fold(f64::INFINITY, f64::min)
    }

    fn pad_label(&self, node: usize) -> String {
        let pi = self.nodes[node].pad.expect("pad node");
        format!("{}.{}", self.pads[pi].0, self.pads[pi].1)
    }

    /// The signal path between two units.
    fn path(&self, from: &Unit, to: &Unit) -> Option<SignalPath> {
        let (d, prev) = self.dijkstra(&self.pad_nodes(from));
        let end = self
            .pad_nodes(to)
            .into_iter()
            .filter(|&n| d[n].is_finite())
            .min_by(|&x, &y| d[x].total_cmp(&d[y]).then(x.cmp(&y)))?;
        let mut on_edges: BTreeSet<usize> = BTreeSet::new();
        let mut on_nodes: BTreeSet<usize> = BTreeSet::from([end]);
        let mut n = end;
        while let Some(e) = prev[n] {
            on_edges.insert(e);
            let ed = &self.edges[e];
            n = if ed.a == n { ed.b } else { ed.a };
            on_nodes.insert(n);
        }
        let start = n;
        let (mut tracks, mut vias) = (0.0, 0.0);
        let mut on_path: BTreeMap<u64, Vec<(f64, f64)>> = BTreeMap::new();
        for &e in &on_edges {
            match self.edges[e].kind {
                EdgeKind::Track(id, t0, t1) => {
                    tracks += self.edges[e].w;
                    on_path.entry(id).or_default().push((t0, t1));
                }
                EdgeKind::Via => vias += self.edges[e].w,
                EdgeKind::Join => {}
            }
        }
        let mut whole = BTreeSet::new();
        for (id, iv) in &mut on_path {
            iv.sort_by(|a, b| a.0.total_cmp(&b.0));
            let len = self.track_len[id];
            if super::union_len(iv.clone()) >= len - 2.0 * JOIN {
                whole.insert(*id);
            }
        }
        // Stubs: edges off the path, grouped through nodes off the path.
        let off: Vec<usize> = (0..self.edges.len()).filter(|e| !on_edges.contains(e)).collect();
        let mut parent: BTreeMap<usize, usize> = off.iter().map(|&e| (e, e)).collect();
        fn find(p: &mut BTreeMap<usize, usize>, x: usize) -> usize {
            let mut r = x;
            while p[&r] != r {
                r = p[&r];
            }
            let mut y = x;
            while p[&y] != r {
                let nx = p[&y];
                p.insert(y, r);
                y = nx;
            }
            r
        }
        for (node, adj) in self.adj.iter().enumerate() {
            if on_nodes.contains(&node) {
                continue;
            }
            let es: Vec<usize> = adj.iter().copied().filter(|e| !on_edges.contains(e)).collect();
            for w in es.windows(2) {
                let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
                if a != b {
                    parent.insert(a.max(b), a.min(b));
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for &e in &off {
            let r = find(&mut parent, e);
            groups.entry(r).or_default().push(e);
        }
        let mut stubs = Vec::new();
        for es in groups.values() {
            let length: f64 = es.iter().map(|&e| self.edges[e].w).sum();
            if length < JOIN {
                continue;
            }
            let nodes: BTreeSet<usize> = es.iter().flat_map(|&e| [self.edges[e].a, self.edges[e].b]).collect();
            let at = nodes.iter().find(|n| on_nodes.contains(n)).or(nodes.first()).map(|&n| self.nodes[n].at);
            let pads: Vec<String> = nodes
                .iter()
                .filter(|n| !on_nodes.contains(n) && self.nodes[**n].pad.is_some())
                .map(|&n| self.pad_label(n))
                .collect();
            stubs.push(Stub { length: Nm(length.round() as i64), at: to_point(at.unwrap_or_default()), pads });
        }
        stubs.sort_by(|a, b| b.length.cmp(&a.length).then(a.at.cmp(&b.at)));
        let tracks_nm = Nm(tracks.round() as i64);
        let vias_nm = Nm(vias.round() as i64);
        Some(SignalPath {
            net: self.net.clone(),
            from: self.pad_label(start),
            to: self.pad_label(end),
            length: NetLength { tracks: tracks_nm, vias: vias_nm, total: Nm(tracks_nm.0 + vias_nm.0) },
            stub_length: Nm(stubs.iter().map(|s| s.length.0).sum()),
            stubs,
            on_path,
            whole,
        })
    }
}

/// Whether copper `shape` comes within `r` of `q`.
fn touches(shape: &poly::Polygon, q: F2, r: f64) -> bool {
    let pt = poly::Point::new(q.0.round() as i64, q.1.round() as i64);
    poly::distance_less_than(shape, &pt, (r.ceil() as i64).max(1) + 1)
}

/// An end of a signal path: a component, or one pad when the net has a single component.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Unit {
    Comp(String),
    Pad(String, String),
}

/// The end units for the given graphs (the first one decides; the others break a tie when
/// it is not connected): explicit components, else the two candidate units farthest apart
/// along the routing (straight-line when no two are connected).
fn choose_ends(graphs: &[&NetGraph], from: Option<&str>, to: Option<&str>) -> Option<(Unit, Unit)> {
    let g0 = graphs.first()?;
    // Candidates: components on every net (a pair's ends), else those of the first net, else
    // its pads.
    let mut comps: BTreeSet<String> = g0.components();
    for g in &graphs[1..] {
        comps = comps.intersection(&g.components()).cloned().collect();
    }
    if comps.len() < 2 {
        comps = g0.components();
    }
    let units: Vec<Unit> = if comps.len() >= 2 {
        comps.into_iter().map(Unit::Comp).collect()
    } else {
        g0.pads.iter().map(|(r, n, _)| Unit::Pad(r.clone(), n.clone())).collect()
    };
    let named = |c: &str| -> Option<Unit> {
        let u = Unit::Comp(c.to_string());
        graphs.iter().all(|g| !g.pad_nodes(&u).is_empty()).then_some(u)
    };
    let fixed = |x: Option<&str>| -> Result<Option<Unit>, ()> {
        match x {
            Some(c) => named(c).map(Some).ok_or(()),
            None => Ok(None),
        }
    };
    let (f, t) = (fixed(from).ok()?, fixed(to).ok()?);
    if let (Some(f), Some(t)) = (&f, &t) {
        return Some((f.clone(), t.clone()));
    }
    // Score of a pair of units: (connected, distance), the larger the better.
    let score = |a: &Unit, b: &Unit| -> (bool, f64) {
        for g in graphs {
            let d = g.unit_distance(a, b);
            if d.is_finite() {
                return (true, d);
            }
        }
        (false, g0.unit_gap(a, b))
    };
    let better = |x: (bool, f64), y: (bool, f64)| x.0 && !y.0 || (x.0 == y.0 && x.1 > y.1);
    let mut best: Option<((bool, f64), Unit, Unit)> = None;
    match f.or(t) {
        Some(one) => {
            for u in units.iter().filter(|u| **u != one) {
                let s = score(&one, u);
                if best.as_ref().is_none_or(|b| better(s, b.0)) {
                    best = Some((s, one.clone(), u.clone()));
                }
            }
            // Keep the given end where it was given.
            let (_, a, b) = best?;
            if from.is_some() { Some((a, b)) } else { Some((b, a)) }
        }
        None => {
            for (i, a) in units.iter().enumerate() {
                for b in &units[i + 1..] {
                    let s = score(a, b);
                    if best.as_ref().is_none_or(|x| better(s, x.0)) {
                        best = Some((s, a.clone(), b.clone()));
                    }
                }
            }
            best.map(|(_, a, b)| (a, b))
        }
    }
}

/// End components of a pair or a net: explicit ones first.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ends<'a> {
    /// Start component.
    pub from: Option<&'a str>,
    /// End component.
    pub to: Option<&'a str>,
}

/// The signal paths of the given nets between common end components (both nets of a pair,
/// or one net). `None` for a net whose ends are not connected by its routing.
pub fn signal_paths(p: &Project, nets: &[&str], ends: Ends<'_>) -> Vec<Option<SignalPath>> {
    let pads = geo::placed_pads(p);
    let graphs: Vec<NetGraph> = nets.iter().map(|n| NetGraph::new(p, n, &pads)).collect();
    let refs: Vec<&NetGraph> = graphs.iter().collect();
    let Some((a, b)) = choose_ends(&refs, ends.from, ends.to) else { return vec![None; nets.len()] };
    graphs.iter().map(|g| g.path(&a, &b)).collect()
}

/// The signal path of one net.
pub fn signal_path(p: &Project, net: &str, ends: Ends<'_>) -> Option<SignalPath> {
    signal_paths(p, &[net], ends).pop().flatten()
}
