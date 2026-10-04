//! Differential pairs routed as coupled traces (docs/ROUTER.md, "Differential pairs").
//!
//! A pair's connections are planned together: each pad of the positive net is matched with
//! the nearest pad of the negative net (an *end*), and the ends are joined by a minimum
//! spanning tree, so both nets get the same topology. Each connection is then routed as one
//! fat path: the pair's centerline, searched on a grid by A* with 0°/45° moves (no 90° bends,
//! no vias: a pair stays on one layer, the one its impedance was solved for), legal when a
//! capsule of radius `gap/2 + width` (plus the outer-corner growth of a 45° bend) keeps the
//! clearance. The centerline is split into the two tracks by offsetting it `(width + gap)/2`
//! to each side (mitered corners). At both ends, short straight *breakout* stubs join each pad
//! to its track; the search's sources and targets are the centerline positions, directions and
//! polarities from which both stubs are legal, priced by their length so the uncoupled part
//! stays short. Every track is checked exactly at the end, and the caller runs the DRC.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::time::Instant;

use crate::id::ObjectId;
use crate::lengths::{self, PairRules};
use crate::model::Project;
use crate::model::board::Track;
use crate::units::Nm;

use super::geo::{BoxF, P, seg_seg_dist};
use super::index::{Blocker, Checker, Index, Item, TOL};
use super::model::{Profile, ProfileKey, RouterBoard};
use super::{Hooks, RouteError};

/// Moves: 45° steps, counter-clockwise from +x.
const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
/// How much farther the outer track of a 45° bend reaches than the centerline's round
/// envelope: `1/cos(22.5°) − 1` times the half pitch of the pair.
const MITER_45: f64 = 0.082_392_200_292_394_1;
/// Pads of the two nets farther apart than this are not taken as one end of the pair (nm).
const MATCH_MAX: f64 = 10_000_000.0;
/// Most grid cells in a search window (the pitch grows beyond).
const MAX_CELLS: usize = 250_000;
/// Most A* expansions per connection and layer.
const MAX_EXPANSIONS: usize = 2_000_000;
/// Cost of a 45° bend (grid steps).
const BEND: f32 = 0.6;
/// Cost of a step of breakout stub, relative to coupled track: uncoupled length is dearer.
const STUB_WEIGHT: f64 = 2.0;

/// A matched pair of pads (one per net) at one end of the pair.
#[derive(Clone, Copy, Debug)]
struct End {
    /// Terminal (pad) of the positive and negative net.
    tp: usize,
    tn: usize,
}

/// Outcome of one coupled connection.
#[derive(Clone, Debug)]
pub(crate) struct ConnOutcome {
    /// Pads: positive net from/to, negative net from/to.
    pub labels: [String; 4],
    /// Routed.
    pub ok: bool,
    /// Why not.
    pub reason: Option<String>,
    /// Where.
    pub at: Option<P>,
    /// Layer routed on (or of the failure).
    pub layer: Option<String>,
    /// Things to try.
    pub hints: Vec<String>,
}

/// Outcome of a pair.
#[derive(Clone, Debug)]
pub(crate) struct PairOutcome {
    /// Pair name.
    pub name: String,
    /// Rules routed with.
    pub rules: PairRules,
    /// New tracks (placeholder IDs) with the index of their connection.
    pub tracks: Vec<(usize, Track)>,
    /// Coupled connections.
    pub conns: Vec<ConnOutcome>,
    /// Connections of the two nets that are not part of the coupled routing (pads without a
    /// partner, such as a pull-up), left to the ordinary router.
    pub left: usize,
    /// A problem with the pair as a whole (missing net).
    pub error: Option<String>,
}

fn dir_unit(k: usize) -> P {
    let (dx, dy) = DIRS[k];
    let l = ((dx * dx + dy * dy) as f64).sqrt();
    P::new(dx as f64 / l, dy as f64 / l)
}

/// Left normal of a unit vector.
fn left(u: P) -> P {
    P::new(-u.y, u.x)
}

fn add(a: P, b: P, s: f64) -> P {
    P::new(a.x + b.x * s, a.y + b.y * s)
}

fn dot(a: P, b: P) -> f64 {
    a.x * b.x + a.y * b.y
}

/// Union-find over small integer sets.
struct Sets(Vec<usize>);

impl Sets {
    fn find(&mut self, i: usize) -> usize {
        let mut r = i;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut k = i;
        while self.0[k] != r {
            let nx = self.0[k];
            self.0[k] = r;
            k = nx;
        }
        r
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// Adds (or finds) a profile and returns its index.
fn add_profile(rb: &mut RouterBoard, pr: Profile) -> usize {
    if let Some(i) = rb.profiles.iter().position(|q| q.key == pr.key && q.hw == pr.hw) {
        return i;
    }
    rb.profiles.push(pr);
    rb.profiles.len() - 1
}

/// Routes the coupled connections of the pairs `names` (all when empty) on the board as it
/// is. Returns per pair the tracks (with placeholder IDs) and a report per connection.
pub(crate) fn route_pairs(
    p: &Project,
    names: &[String],
    slots: &[usize],
    deadline: Option<Instant>,
    hooks: &Hooks<'_>,
) -> Result<Vec<PairOutcome>, RouteError> {
    let c = p.circuit();
    let names: Vec<String> = if names.is_empty() { c.diffpairs.keys().cloned().collect() } else { names.to_vec() };
    for n in &names {
        if !c.diffpairs.contains_key(n) {
            return Err(RouteError::UnknownPair(n.clone()));
        }
    }
    let items = crate::board::copper_items(p);
    let isl = crate::board::islands(&items);
    let mut rb = RouterBoard::build(p, &items, &isl);
    // Per pair: the rules, a profile for both nets at the pair width, and a fat net for the
    // centerline.
    let mut fat: BTreeMap<String, u32> = BTreeMap::new();
    let mut rules: BTreeMap<String, PairRules> = BTreeMap::new();
    for name in &names {
        let d = &c.diffpairs[name];
        let r = lengths::pair_rules(p, d);
        let (w, g, cl) = (r.width.0 as f64, r.gap.0 as f64, r.clearance.0 as f64);
        let half = (w + g) / 2.0;
        for net in [&d.p, &d.n] {
            if let Some(&id) = rb.net_ids.get(net) {
                let old = rb.profile(id).clone();
                let key = ProfileKey { width: r.width, clear: r.clearance, ..old.key };
                let k = add_profile(&mut rb, Profile { key, hw: w / 2.0, c: cl, ..old });
                rb.net_profile[id as usize] = k;
            }
        }
        let fr = half * (1.0 + MITER_45) + w / 2.0;
        let key =
            ProfileKey { width: Nm((2.0 * fr).ceil() as i64), clear: r.clearance, via_drill: Nm(0), via_dia: Nm(0) };
        let k = add_profile(&mut rb, Profile { key, hw: fr, c: cl, rv: 0.0, dr: 0.0 });
        let id = rb.nets.len() as u32;
        rb.nets.push(format!("~pair:{name}"));
        rb.net_profile.push(k);
        fat.insert(name.clone(), id);
        rules.insert(name.clone(), r);
    }
    let reach = super::reach(&rb);
    let mut index = Index::new(rb.bbox.expand(2_000_000.0), 500_000.0, reach);
    for (i, ob) in rb.obstacles.iter().enumerate() {
        index.insert(Item::Static(i as u32), ob.bbox);
    }
    let mut out = Vec::new();
    for (k, name) in names.iter().enumerate() {
        if (hooks.cancelled)() {
            return Err(RouteError::Cancelled);
        }
        (hooks.progress)(k as u64, Some(names.len() as u64), &format!("differential pair {name}"));
        let d = &c.diffpairs[name];
        let r = rules[name];
        let mut po = PairOutcome { name: name.clone(), rules: r, tracks: vec![], conns: vec![], left: 0, error: None };
        let (Some(&pn), Some(&nn)) = (rb.net_ids.get(&d.p), rb.net_ids.get(&d.n)) else {
            po.error = Some(format!("pair {name}: net {} or {} does not exist", d.p, d.n));
            out.push(po);
            continue;
        };
        let (ends, conns, left) = plan(&rb, pn, nn);
        po.left = left;
        for (a, b, through) in conns {
            let ea: End = ends[a];
            let eb: End = ends[b];
            let labels = [
                rb.terminals[ea.tp].label.clone(),
                rb.terminals[eb.tp].label.clone(),
                rb.terminals[ea.tn].label.clone(),
                rb.terminals[eb.tn].label.clone(),
            ];
            let job = Job { rb: &rb, pn, nn, fat: fat[name], rules: r, a: ea, b: eb };
            let res = {
                let ck = Checker { rb: &rb, index: &index };
                if through { Ok(job.through(&ck, slots)) } else { job.route(&ck, slots, deadline, hooks) }?
            };
            match res {
                Ok(cp) => {
                    let ci = po.conns.len();
                    let hw = r.width.0 as f64 / 2.0;
                    for (net, pts) in [(pn, &cp.p), (nn, &cp.n)] {
                        for w in pts.windows(2) {
                            index.insert(
                                Item::Seg { net, layer: cp.layer as u8, a: w[0], b: w[1] },
                                BoxF::of2(w[0], w[1]).expand(hw),
                            );
                            po.tracks.push((
                                ci,
                                Track {
                                    id: ObjectId(0),
                                    layer: rb.layer_names[cp.layer].clone(),
                                    width: r.width,
                                    net: Some(rb.nets[net as usize].clone()),
                                    start: w[0].to_point(),
                                    end: w[1].to_point(),
                                    mid: None,
                                    locked: false,
                                },
                            ));
                        }
                    }
                    po.conns.push(ConnOutcome {
                        labels,
                        ok: true,
                        reason: None,
                        at: None,
                        layer: Some(rb.layer_names[cp.layer].clone()),
                        hints: vec![],
                    });
                }
                Err(f) => po.conns.push(ConnOutcome {
                    labels,
                    ok: false,
                    reason: Some(f.reason),
                    at: Some(f.at),
                    layer: f.layer.map(|l| rb.layer_names[l].clone()),
                    hints: f.hints,
                }),
            }
        }
        out.push(po);
    }
    Ok(out)
}

/// Ends of a pair and the connections joining them (indices into the ends, and whether it is a
/// flow-through link: both ends on one component, such as an ESD array whose lines pass
/// through it), plus the number of connections of the two nets left out.
type Plan = (Vec<End>, Vec<(usize, usize, bool)>, usize);

fn plan(rb: &RouterBoard, pn: u32, nn: u32) -> Plan {
    let pads = |net: u32| -> Vec<usize> {
        (0..rb.terminals.len()).filter(|&t| rb.terminals[t].net == net && rb.terminals[t].pad).collect()
    };
    let (pp, np) = (pads(pn), pads(nn));
    // Greedy matching by distance (ties by terminal order).
    let mut cand: Vec<(i64, usize, usize)> = Vec::new();
    for &a in &pp {
        for &b in &np {
            let d = rb.terminals[a].at.dist(rb.terminals[b].at);
            if d <= MATCH_MAX {
                cand.push((d.round() as i64, a, b));
            }
        }
    }
    cand.sort_unstable();
    let (mut used_p, mut used_n) = (vec![false; rb.terminals.len()], vec![false; rb.terminals.len()]);
    let mut ends = Vec::new();
    for (_, a, b) in cand {
        if used_p[a] || used_n[b] {
            continue;
        }
        used_p[a] = true;
        used_n[b] = true;
        ends.push(End { tp: a, tn: b });
    }
    // Islands: union-find over island ids (the lowest item index), per net.
    let mut sp = Sets((0..items_bound(rb)).collect());
    let mut sn = Sets((0..items_bound(rb)).collect());
    let isl = |t: usize| rb.terminals[t].island;
    let mid = |e: &End| {
        let (a, b) = (rb.terminals[e.tp].at, rb.terminals[e.tn].at);
        P::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
    };
    let comp = |t: usize| rb.terminals[t].label.split_once('.').map(|(c, _)| c.to_string());
    let through = |i: usize, j: usize| {
        let c = comp(ends[i].tp);
        c.is_some() && [ends[i].tn, ends[j].tp, ends[j].tn].iter().all(|&t| comp(t) == c)
    };
    // Flow-through links first, then the shortest.
    let mut edges: Vec<(bool, i64, usize, usize)> = Vec::new();
    for i in 0..ends.len() {
        for j in i + 1..ends.len() {
            edges.push((!through(i, j), mid(&ends[i]).dist(mid(&ends[j])).round() as i64, i, j));
        }
    }
    edges.sort_unstable();
    let mut conns = Vec::new();
    for (_, _, i, j) in edges {
        let (a, b) = (ends[i], ends[j]);
        if sp.find(isl(a.tp)) == sp.find(isl(b.tp)) || sn.find(isl(a.tn)) == sn.find(isl(b.tn)) {
            continue;
        }
        sp.join(isl(a.tp), isl(b.tp));
        sn.join(isl(a.tn), isl(b.tn));
        conns.push((i, j, through(i, j)));
    }
    // What is left: islands (with pads) of each net not joined by the coupled routing.
    let groups = |net_pads: &[usize], s: &mut Sets| {
        let mut roots: Vec<usize> = net_pads.iter().map(|&t| s.find(isl(t))).collect();
        roots.sort_unstable();
        roots.dedup();
        roots.len().saturating_sub(1)
    };
    let left = groups(&pp, &mut sp) + groups(&np, &mut sn);
    // Flow-through links, then short connections first.
    conns.sort_by_key(|&(i, j, t)| (!t, mid(&ends[i]).dist(mid(&ends[j])).round() as i64, i, j));
    (ends, conns, left)
}

/// Upper bound of island ids (they are item indices).
fn items_bound(rb: &RouterBoard) -> usize {
    rb.terminals.iter().map(|t| t.island).max().unwrap_or(0) + 1
}

/// A coupled route: the two polylines, pad center to pad center.
struct CoupledRoute {
    layer: usize,
    p: Vec<P>,
    n: Vec<P>,
}

/// Why a coupled connection failed.
struct Failure {
    reason: String,
    at: P,
    layer: Option<usize>,
    hints: Vec<String>,
}

/// One coupled connection to route.
struct Job<'a> {
    rb: &'a RouterBoard,
    pn: u32,
    nn: u32,
    fat: u32,
    rules: PairRules,
    a: End,
    b: End,
}

/// The search grid of one connection.
struct Win {
    ox: f64,
    oy: f64,
    g: f64,
    nx: i32,
    ny: i32,
}

impl Win {
    fn cell(&self, i: i32, j: i32) -> P {
        P::new(self.ox + i as f64 * self.g, self.oy + j as f64 * self.g)
    }
    fn idx(&self, i: i32, j: i32) -> usize {
        (j * self.nx + i) as usize
    }
    fn ij(&self, idx: usize) -> (i32, i32) {
        ((idx as i32) % self.nx, (idx as i32) / self.nx)
    }
    fn cells(&self) -> usize {
        (self.nx * self.ny) as usize
    }
}

/// State: ((cell · 8) + direction) · 2 + polarity (0: positive net on the left).
fn state(cell: usize, k: usize, pol: usize) -> usize {
    (cell * 8 + k) * 2 + pol
}

const GOAL: u32 = 1 << 31;

impl Job<'_> {
    fn route(
        &self,
        ck: &Checker<'_>,
        slots: &[usize],
        deadline: Option<Instant>,
        hooks: &Hooks<'_>,
    ) -> Result<Result<CoupledRoute, Failure>, RouteError> {
        let rb = self.rb;
        let t = |i: usize| &rb.terminals[i];
        let bit = |l: usize| 1u64 << l.min(63);
        let layers: Vec<usize> = slots
            .iter()
            .copied()
            .filter(|&l| [self.a.tp, self.a.tn, self.b.tp, self.b.tn].iter().all(|&x| t(x).layers & bit(l) != 0))
            .collect();
        let mid = |e: End| P::new((t(e.tp).at.x + t(e.tn).at.x) / 2.0, (t(e.tp).at.y + t(e.tn).at.y) / 2.0);
        let mut fail = Failure {
            reason: format!(
                "the pads {}/{} and {}/{} share no routing layer",
                t(self.a.tp).label,
                t(self.a.tn).label,
                t(self.b.tp).label,
                t(self.b.tn).label
            ),
            at: mid(self.a),
            layer: None,
            hints: vec!["a differential pair is routed on one layer: put its pads on a common layer".into()],
        };
        for l in layers {
            match self.on_layer(ck, l, deadline, hooks)? {
                Ok(r) => return Ok(Ok(r)),
                Err(f) => fail = f,
            }
        }
        Ok(Err(fail))
    }

    /// A flow-through link: a straight track per net between the two ends' pads (on the
    /// first layer both pads of each net share), not coupled.
    fn through(&self, ck: &Checker<'_>, slots: &[usize]) -> Result<CoupledRoute, Failure> {
        let rb = self.rb;
        let t = |i: usize| &rb.terminals[i];
        let (w, gap, cl) = (self.rules.width.0 as f64, self.rules.gap.0 as f64, self.rules.clearance.0 as f64);
        let (pa, pb, na, nb) = (t(self.a.tp).at, t(self.b.tp).at, t(self.a.tn).at, t(self.b.tn).at);
        let mut why = format!("no common layer for {} and {}", t(self.a.tp).label, t(self.b.tp).label);
        for &l in slots {
            let bit = 1u64 << l.min(63);
            if [self.a.tp, self.a.tn, self.b.tp, self.b.tn].iter().any(|&x| t(x).layers & bit == 0) {
                continue;
            }
            if let Some(b) = ck.seg(l, pa, pb, self.pn).or_else(|| ck.seg(l, na, nb, self.nn)) {
                why = format!("the straight link is blocked by {}", describe(rb, b));
                continue;
            }
            if seg_seg_dist(pa, pb, na, nb) < w + gap.min(cl) - TOL {
                why = "the two links come too close".into();
                continue;
            }
            return Ok(CoupledRoute { layer: l, p: vec![pa, pb], n: vec![na, nb] });
        }
        Err(Failure {
            reason: format!(
                "flow-through link {}/{} to {}/{}: {why}",
                t(self.a.tp).label,
                t(self.a.tn).label,
                t(self.b.tp).label,
                t(self.b.tn).label
            ),
            at: pa,
            layer: None,
            hints: vec!["route it with route.nets, or check the part's pin assignment".into()],
        })
    }

    /// Breakout stubs from `pad` (positive) and `pad2` (negative) to the offset points of a
    /// centerline point `c` with direction `k`; `source`: the stubs lead into the pair, else
    /// out of it. Returns the stub cost (grid steps) when both are legal.
    #[allow(clippy::too_many_arguments)]
    fn breakout(
        &self,
        ck: &Checker<'_>,
        layer: usize,
        c: P,
        k: usize,
        pol: usize,
        source: bool,
        e: End,
        g: f64,
    ) -> Option<f32> {
        let rb = self.rb;
        let (w, gap) = (self.rules.width.0 as f64, self.rules.gap.0 as f64);
        let gmin = gap.min(self.rules.clearance.0 as f64);
        let half = (w + gap) / 2.0;
        let u = dir_unit(k);
        let s = if pol == 0 { 1.0 } else { -1.0 };
        let (op, on) = (add(c, left(u), half * s), add(c, left(u), -half * s));
        let (pp, np) = (rb.terminals[e.tp].at, rb.terminals[e.tn].at);
        // No stub may double back against the coupled direction (interior angle ≥ 90°).
        let fwd = |pad: P, o: P| {
            let v = if source { P::new(o.x - pad.x, o.y - pad.y) } else { P::new(pad.x - o.x, pad.y - o.y) };
            dot(v, u) >= -1e-6 * (v.x.hypot(v.y) + 1.0)
        };
        if !fwd(pp, op) || !fwd(np, on) {
            return None;
        }
        // The stubs and the first step of the other track keep the pair's gap.
        let step = if source { add(on, u, g) } else { add(on, u, -g) };
        let step_p = if source { add(op, u, g) } else { add(op, u, -g) };
        let need = w + gmin - TOL;
        if seg_seg_dist(pp, op, np, on) < need
            || seg_seg_dist(pp, op, on, step) < need
            || seg_seg_dist(np, on, op, step_p) < need
        {
            return None;
        }
        if ck.seg(layer, pp, op, self.pn).is_some() || ck.seg(layer, np, on, self.nn).is_some() {
            return None;
        }
        Some(((pp.dist(op) + np.dist(on)) / 2.0 * STUB_WEIGHT / g) as f32)
    }

    fn on_layer(
        &self,
        ck: &Checker<'_>,
        layer: usize,
        deadline: Option<Instant>,
        hooks: &Hooks<'_>,
    ) -> Result<Result<CoupledRoute, Failure>, RouteError> {
        let rb = self.rb;
        let t = |i: usize| &rb.terminals[i];
        let (w, gap, cl) = (self.rules.width.0 as f64, self.rules.gap.0 as f64, self.rules.clearance.0 as f64);
        let half = (w + gap) / 2.0;
        let fr = rb.profile(self.fat).hw;
        let mid = |e: End| P::new((t(e.tp).at.x + t(e.tn).at.x) / 2.0, (t(e.tp).at.y + t(e.tn).at.y) / 2.0);
        let (ma, mb) = (mid(self.a), mid(self.b));
        let radius = |e: End| (1.2 * t(e.tp).at.dist(t(e.tn).at) + 2.0 * fr + cl).max(1_500_000.0);
        let (ra, rbk) = (radius(self.a), radius(self.b));
        // Window and pitch.
        let mut bx = BoxF::EMPTY;
        for e in [self.a, self.b] {
            bx.add(t(e.tp).at);
            bx.add(t(e.tn).at);
        }
        let span = (bx.max.x - bx.min.x).max(bx.max.y - bx.min.y);
        let bx = bx.expand((0.25 * span).max(4_000_000.0).max(ra.max(rbk)));
        let bx = BoxF {
            min: P::new(bx.min.x.max(rb.bbox.min.x), bx.min.y.max(rb.bbox.min.y)),
            max: P::new(bx.max.x.min(rb.bbox.max.x), bx.max.y.min(rb.bbox.max.y)),
        };
        let mut g = ((2.0 * fr + cl) / 4.0).max(half).max(25_000.0).round();
        let area = (bx.max.x - bx.min.x) * (bx.max.y - bx.min.y);
        if area / (g * g) > MAX_CELLS as f64 {
            g = (area / MAX_CELLS as f64).sqrt().ceil();
        }
        // Align a node with the start's midpoint.
        let ox = ma.x - ((ma.x - bx.min.x) / g).floor() * g;
        let oy = ma.y - ((ma.y - bx.min.y) / g).floor() * g;
        let win = Win {
            ox,
            oy,
            g,
            nx: (((bx.max.x - ox) / g).floor() as i32 + 1).max(1),
            ny: (((bx.max.y - oy) / g).floor() as i32 + 1).max(1),
        };
        let ncell = win.cells();
        let failure = |reason: String, at: P, hints: Vec<String>| Failure { reason, at, layer: Some(layer), hints };
        // Fat-point legality per cell (0 unknown, 1 ok, 2 blocked) and edges per cell and
        // direction.
        let mut point_ok = vec![0u8; ncell];
        let mut edge_ok = vec![0u8; ncell * 8];
        let mut cell_ok = |c: usize| -> bool {
            if point_ok[c] == 0 {
                let (i, j) = win.ij(c);
                let q = win.cell(i, j);
                point_ok[c] = if ck.seg(layer, q, q, self.fat).is_none() { 1 } else { 2 };
            }
            point_ok[c] == 1
        };
        // Sources.
        let mut sources: Vec<(usize, f32)> = Vec::new();
        let near = |c: P, r: f64, i0: &mut i32, i1: &mut i32, j0: &mut i32, j1: &mut i32| {
            *i0 = (((c.x - r - win.ox) / g).floor() as i32).max(0);
            *i1 = (((c.x + r - win.ox) / g).ceil() as i32).min(win.nx - 1);
            *j0 = (((c.y - r - win.oy) / g).floor() as i32).max(0);
            *j1 = (((c.y + r - win.oy) / g).ceil() as i32).min(win.ny - 1);
        };
        let (mut i0, mut i1, mut j0, mut j1) = (0, 0, 0, 0);
        near(ma, ra, &mut i0, &mut i1, &mut j0, &mut j1);
        for j in j0..=j1 {
            for i in i0..=i1 {
                let q = win.cell(i, j);
                if q.dist(ma) > ra || !rb.inside(q) {
                    continue;
                }
                let c = win.idx(i, j);
                if !cell_ok(c) {
                    continue;
                }
                for k in 0..8 {
                    for pol in 0..2 {
                        if let Some(cost) = self.breakout(ck, layer, q, k, pol, true, self.a, g) {
                            sources.push((state(c, k, pol), cost));
                        }
                    }
                }
            }
        }
        let mk_hints = |what: &str| {
            vec![
                format!(
                    "move the parts so that {what} have room for a {} wide pair with {} gap",
                    self.rules.width, self.rules.gap
                ),
                "lower the pair's clearance or gap (netclass.set) if the impedance allows it".to_string(),
            ]
        };
        if sources.is_empty() {
            let what = format!("{}/{}", t(self.a.tp).label, t(self.a.tn).label);
            return Ok(Err(failure(
                format!("no room to break out of {what} into a coupled pair on {}", rb.layer_names[layer]),
                ma,
                mk_hints(&what),
            )));
        }
        // A*.
        let nstates = ncell * 16;
        let mut gs = vec![f32::INFINITY; nstates];
        let mut parent = vec![u32::MAX; nstates];
        let mut closed = vec![false; nstates];
        let mut tcache: BTreeMap<usize, Option<f32>> = BTreeMap::new();
        let h = |c: usize| -> f32 {
            let (i, j) = win.ij(c);
            let q = win.cell(i, j);
            let (dx, dy) = ((q.x - mb.x).abs() / g, (q.y - mb.y).abs() / g);
            let oct = dx.max(dy) + (std::f64::consts::SQRT_2 - 1.0) * dx.min(dy);
            (oct - 1.1 * rbk / g).max(0.0) as f32
        };
        let key = |f: f32, s: u32| -> (Reverse<u32>, Reverse<u32>) { (Reverse(f.to_bits()), Reverse(s)) };
        let mut heap: BinaryHeap<(Reverse<u32>, Reverse<u32>)> = BinaryHeap::new();
        for &(s, cost) in &sources {
            if cost < gs[s] {
                gs[s] = cost;
                heap.push(key(cost + h(s / 16), s as u32));
            }
        }
        let mut goal: Option<(usize, f32)> = None;
        let mut expansions = 0usize;
        while let Some((Reverse(fb), Reverse(sid))) = heap.pop() {
            if sid & GOAL != 0 {
                goal = Some(((sid & !GOAL) as usize, f32::from_bits(fb)));
                break;
            }
            let s = sid as usize;
            if closed[s] {
                continue;
            }
            closed[s] = true;
            expansions += 1;
            if expansions.is_multiple_of(4096) {
                if (hooks.cancelled)() {
                    return Err(RouteError::Cancelled);
                }
                if deadline.is_some_and(|d| Instant::now() > d) || expansions > MAX_EXPANSIONS {
                    break;
                }
            }
            let (c, k, pol) = (s / 16, (s / 2) % 8, s % 2);
            let (i, j) = win.ij(c);
            let q = win.cell(i, j);
            if q.dist(mb) <= rbk {
                let tc = *tcache.entry(s).or_insert_with(|| self.breakout(ck, layer, q, k, pol, false, self.b, g));
                if let Some(tc) = tc {
                    heap.push(key(gs[s] + tc, s as u32 | GOAL));
                }
            }
            let first = parent[s] == u32::MAX;
            for dk in [0i32, -1, 1] {
                if first && dk != 0 {
                    continue; // the source's direction is its first move
                }
                let k2 = (k as i32 + dk).rem_euclid(8) as usize;
                let (dx, dy) = DIRS[k2];
                let (i2, j2) = (i + dx, j + dy);
                if i2 < 0 || j2 < 0 || i2 >= win.nx || j2 >= win.ny {
                    continue;
                }
                let c2 = win.idx(i2, j2);
                let e = c * 8 + k2;
                if edge_ok[e] == 0 {
                    let q2 = win.cell(i2, j2);
                    edge_ok[e] = if rb.inside(q2) && ck.seg(layer, q, q2, self.fat).is_none() { 1 } else { 2 };
                }
                if edge_ok[e] != 1 {
                    continue;
                }
                let step = if dx != 0 && dy != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
                let s2 = state(c2, k2, pol);
                let ng = gs[s] + step + if dk != 0 { BEND } else { 0.0 };
                if ng < gs[s2] && !closed[s2] {
                    gs[s2] = ng;
                    parent[s2] = s as u32;
                    heap.push(key(ng + h(c2), s2 as u32));
                }
            }
        }
        let Some((end, _)) = goal else {
            let what = format!("{}/{}", t(self.b.tp).label, t(self.b.tn).label);
            // Would the other polarity have fit? Then the nets swap sides between the ends.
            let flipped = tcache.iter().filter(|(_, v)| v.is_none()).take(20_000).any(|(&s, _)| {
                let (c, k, pol) = (s / 16, (s / 2) % 8, s % 2);
                let (i, j) = win.ij(c);
                self.breakout(ck, layer, win.cell(i, j), k, pol ^ 1, false, self.b, g).is_some()
            });
            if flipped {
                return Ok(Err(failure(
                    format!(
                        "polarity: {} and {} swap sides between {}/{} and {what} (the pair would have to cross)",
                        rb.nets[self.pn as usize],
                        rb.nets[self.nn as usize],
                        t(self.a.tp).label,
                        t(self.a.tn).label
                    ),
                    mb,
                    vec![
                        "rotate or mirror a part, or swap the pin assignment at one end, so that the positive net stays on the same side".into(),
                        "a pad row tapped from one side (an ESD array without flow-through pins) reverses the pair: route the pair past it and connect the pads with stubs".into(),
                    ],
                )));
            }
            let reason = if tcache.values().all(Option::is_none) && !tcache.is_empty() {
                format!("no room to break out of {what} into a coupled pair on {}", rb.layer_names[layer])
            } else {
                format!(
                    "no coupled path from {}/{} to {what} on {}",
                    t(self.a.tp).label,
                    t(self.a.tn).label,
                    rb.layer_names[layer]
                )
            };
            return Ok(Err(failure(reason, mb, mk_hints("the pair's pads"))));
        };
        // Centerline: states from the source to the goal.
        let mut chain = vec![end];
        while parent[*chain.last().expect("chain")] != u32::MAX {
            chain.push(parent[*chain.last().expect("chain")] as usize);
        }
        chain.reverse();
        let pol = end % 2;
        let pts: Vec<(P, usize)> = chain
            .iter()
            .map(|&s| {
                let (i, j) = win.ij(s / 16);
                (win.cell(i, j), (s / 2) % 8)
            })
            .collect();
        // Vertices where the direction changes; directions of the segments between them.
        let mut verts = vec![pts[0].0];
        let mut dirs = vec![pts[0].1];
        for w in pts.windows(2) {
            if w[1].1 != *dirs.last().expect("dir") {
                verts.push(w[0].0);
                dirs.push(w[1].1);
            }
        }
        if pts.len() > 1 {
            verts.push(pts[pts.len() - 1].0);
        }
        let offset = |sgn: f64| -> Vec<P> {
            let d = half * sgn;
            if verts.len() == 1 {
                return vec![add(verts[0], left(dir_unit(dirs[0])), d)];
            }
            let mut o = Vec::with_capacity(verts.len());
            for (vi, &v) in verts.iter().enumerate() {
                let q = if vi == 0 {
                    add(v, left(dir_unit(dirs[0])), d)
                } else if vi + 1 == verts.len() {
                    add(v, left(dir_unit(dirs[vi - 1])), d)
                } else {
                    let (a, b) = (left(dir_unit(dirs[vi - 1])), left(dir_unit(dirs[vi])));
                    let s = d / (1.0 + dot(a, b));
                    P::new(v.x + (a.x + b.x) * s, v.y + (a.y + b.y) * s)
                };
                o.push(q);
            }
            o
        };
        let sp = if pol == 0 { 1.0 } else { -1.0 };
        let build = |a: P, mut body: Vec<P>, b: P| -> Vec<P> {
            let mut v = vec![a];
            v.append(&mut body);
            v.push(b);
            v.dedup_by(|x, y| x.dist(*y) < 1.0);
            v
        };
        let route = CoupledRoute {
            layer,
            p: build(t(self.a.tp).at, offset(sp), t(self.b.tp).at),
            n: build(t(self.a.tn).at, offset(-sp), t(self.b.tn).at),
        };
        // Exact check of both tracks.
        let gmin = gap.min(cl);
        for (net, pts) in [(self.pn, &route.p), (self.nn, &route.n)] {
            for s in pts.windows(2) {
                if let Some(bl) = ck.seg(layer, s[0], s[1], net) {
                    let what = describe(rb, bl);
                    return Ok(Err(failure(
                        format!("the coupled route of {} conflicts with {what}", rb.nets[net as usize]),
                        s[0],
                        mk_hints("the pair's pads"),
                    )));
                }
            }
        }
        for a in route.p.windows(2) {
            for b in route.n.windows(2) {
                if seg_seg_dist(a[0], a[1], b[0], b[1]) < w + gmin - TOL {
                    return Ok(Err(failure(
                        "the breakout stubs of the pair cross or come closer than the gap".to_string(),
                        a[0],
                        mk_hints("the pair's pads"),
                    )));
                }
            }
        }
        Ok(Ok(route))
    }
}

/// What a blocker is, in words.
fn describe(rb: &RouterBoard, b: Blocker) -> String {
    match b {
        Blocker::Obstacle(o) => rb.obstacles[o as usize].label.clone(),
        Blocker::Net(n, _) => format!("routing of net {}", rb.nets[n as usize]),
        Blocker::Hole => "a drilled hole".into(),
        Blocker::Outside => "the board outline".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_and_dirs() {
        let mut s = Sets((0..5).collect());
        s.join(3, 1);
        s.join(4, 3);
        assert_eq!(s.find(4), 1);
        assert_ne!(s.find(0), s.find(4));
        for k in 0..8 {
            let u = dir_unit(k);
            assert!((u.x.hypot(u.y) - 1.0).abs() < 1e-12);
            let l = left(u);
            assert!(dot(u, l).abs() < 1e-12);
        }
        // The outer corner of a 45° bend at offset d reaches d / cos(22.5°).
        let c = 1.0 / (22.5f64.to_radians()).cos() - 1.0;
        assert!((c - MITER_45).abs() < 1e-12);
    }
}
