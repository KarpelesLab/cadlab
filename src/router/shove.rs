//! Geometric push-and-shove: places new copper (the *head*: track polylines and vias) and moves
//! the unlocked tracks and vias of other nets out of its way, keeping every rule.
//!
//! Designed from the general walkaround / shove idea of interactive routers (obstacles are
//! inflated to convex hulls; a line in the way is bent around the hull of what pushes it, a via
//! is pushed out of it, and whatever moved pushes in turn), not from any router's source:
//!
//! 1. **World.** Static copper (pads, locked tracks and vias, arcs, copper of the head's nets),
//!    keep-outs, holes and the board edge come from [`RouterBoard`]. Unlocked straight tracks
//!    of other nets become *lines*: chains of segments of one net and layer between anchors (a
//!    via, a pad, a junction of three or more segments, a free end). Unlocked vias of other
//!    nets are movable unless they sit on static copper of their net. Locked items never move.
//! 2. **Walkaround.** A line (the head in `route.track`, or a shoved line) that runs into
//!    something it cannot move is bent around that thing's hull: the convex hull of the
//!    obstacle grown by an octagon whose inradius is the required distance, so the hull's
//!    edges keep the clearance exactly (0°/45°/90° edges for octilinear obstacles). The part of
//!    the line between its first entry into and its last exit from the hull is replaced by the
//!    hull boundary, on the shorter side that stays clear of other fixed things.
//! 3. **Shove.** Movable lines hit by a pushing item are walked around its hull the same way
//!    and movable vias are moved to the nearest point of its hull (their attached segment ends
//!    follow); every moved item then pushes in turn, in first-in first-out order, until nothing
//!    collides or the operation budget runs out. A line whose fixed end lies inside the hull, or
//!    that crosses the pushing item (a topological crossing), cannot be shoved: the operation
//!    fails and names it.
//! 4. **Spring-back.** Every moved line returns to its original geometry when that is legal
//!    again, else is pulled tight (45° pull-tight and mitered corners, `post`); moved vias go
//!    back when they can.
//! 5. **Check.** Everything new or moved is checked exactly against everything else; the
//!    caller then runs the DRC on the result.
//!
//! Deterministic: items are processed in index order, never in hash order.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::board::ItemRef;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{Track, Via};
use crate::refs::ObjectRef;
use crate::units::Nm;

use super::geo::{BoxF, P, point_seg_d2, seg_seg_dist};
use super::index::{Index, Item, TOL};
use super::model::{ObKind, RouterBoard};
use super::post;

/// Distance kept beyond the rules by moved copper (nm), against rounding.
pub(crate) const MARGIN: f64 = 500.0;
/// Most push operations of one run.
const MAX_OPS: usize = 600;
/// Most walkaround steps for one line in one go.
const MAX_WALKS: usize = 12;

/// How the head may get past copper in its way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Walk around fixed things, shove movable ones.
    Shove,
    /// Walk around everything; move nothing.
    Walkaround,
    /// Place the head as given; anything in the way is a failure.
    Strict,
}

/// A head line: a polyline of a net on a layer.
#[derive(Clone, Debug)]
pub(crate) struct HeadLine {
    pub layer: usize,
    pub net: u32,
    pub width: Nm,
    pub pts: Vec<P>,
}

/// Something in the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Hit {
    /// A static obstacle (`RouterBoard::obstacles`).
    Static(u32),
    /// A drilled hole (`RouterBoard::holes`), for vias.
    Hole(u32),
    /// A line of the world.
    Line(usize),
    /// A via of the world.
    Via(usize),
    /// Outside the board.
    Outside,
}

/// Why a placement failed.
#[derive(Clone, Debug)]
pub(crate) struct Fail {
    /// Where.
    pub at: P,
    /// Stackup layer, when it is about a track.
    pub layer: Option<usize>,
    /// What happened.
    pub why: FailWhy,
    /// What `hit` is, for reports: `pad U1.3 (net GND)`, `track#12 (net SDA)`.
    pub label: String,
    /// Objects involved.
    pub subjects: Vec<ObjectRef>,
    /// Whether `hit` is locked copper (unlocking it would let it move).
    pub locked: bool,
}

/// Kind of failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailWhy {
    /// The head runs into something that cannot move and cannot be walked around.
    Blocked,
    /// A line in the way cannot be bent out of the way (fixed end in the way, or crossing).
    Stuck,
    /// The operation budget ran out.
    Limit,
}

#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub net: u32,
    pub layer: usize,
    pub width: Nm,
    pub hw: f64,
    pub pts: Vec<P>,
    /// Track IDs it was built from (empty for the head).
    pub orig: Vec<ObjectId>,
    pub orig_pts: Vec<P>,
    /// Head lines and lines that must not move.
    pub fixed: bool,
    pub moved: bool,
    ids: Vec<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct WVia {
    pub net: u32,
    pub at: P,
    pub rv: f64,
    pub dr: f64,
    pub orig_at: P,
    /// The board via (`None` for a head via; its fields are the template of the new via).
    pub orig: Option<ObjectId>,
    pub via: Via,
    pub fixed: bool,
    pub moved: bool,
    id: u32,
}

#[derive(Clone, Copy, Debug)]
enum Owner {
    Seg(usize),
    Via(usize),
}

/// The world: static obstacles, lines and vias, with a spatial index.
pub(crate) struct World<'a> {
    pub rb: &'a RouterBoard,
    pub lines: Vec<Line>,
    pub vias: Vec<WVia>,
    index: Index,
    owner: BTreeMap<u32, Owner>,
    /// Largest radius plus clearance of anything, for index queries.
    reach: f64,
    /// Holes of movable vias (they move with the via).
    moving_holes: BTreeSet<ObjectId>,
    ops: usize,
}

/// The outcome of [`run`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Outcome {
    /// New tracks (head and moved lines; IDs are placeholders).
    pub tracks: Vec<Track>,
    /// New vias (head and moved vias).
    pub vias: Vec<Via>,
    pub removed_tracks: Vec<ObjectId>,
    pub removed_vias: Vec<ObjectId>,
    /// Nets whose copper moved.
    pub moved_nets: BTreeSet<u32>,
    /// Whether the head had to detour around something.
    pub walked: bool,
}

fn key(p: P) -> (i64, i64) {
    (p.x.round() as i64, p.y.round() as i64)
}

impl<'a> World<'a> {
    /// Builds the world of `p` for heads of the nets `head_nets` (their own copper is static).
    pub fn build(p: &Project, rb: &'a RouterBoard, head_nets: &BTreeSet<u32>) -> World<'a> {
        let board = p.board();
        let movable_net = |net: &Option<String>| -> Option<u32> {
            let n = rb.net_ids.get(net.as_ref()?).copied()?;
            (!head_nets.contains(&n)).then_some(n)
        };
        // Movable tracks and vias.
        let mut moving: BTreeSet<ObjectId> = BTreeSet::new();
        let tracks: Vec<&Track> =
            board.tracks.iter().filter(|t| !t.locked && t.mid.is_none() && movable_net(&t.net).is_some()).collect();
        let vias: Vec<&Via> = board.vias.iter().filter(|v| !v.locked && movable_net(&v.net).is_some()).collect();
        for t in &tracks {
            moving.insert(t.id);
        }
        for v in &vias {
            moving.insert(v.id);
        }
        let mut reach = 0.0f64;
        for pr in &rb.profiles {
            reach = reach.max(pr.hw.max(pr.rv) + pr.c);
        }
        for t in &tracks {
            reach = reach.max(t.width.0 as f64 / 2.0);
        }
        for v in &vias {
            reach = reach.max(v.diameter.0 as f64 / 2.0);
        }
        let cmax = rb.profiles.iter().map(|p| p.c).fold(rb.rules.copper_to_edge.0 as f64, f64::max);
        let reach = 2.0 * reach + cmax + rb.h2h + 2.0 * TOL;
        let mut index = Index::new(rb.bbox.expand(2_000_000.0), 500_000.0, reach);
        for (i, ob) in rb.obstacles.iter().enumerate() {
            if ob.movable.as_ref().is_some_and(|m| match m {
                ItemRef::Track(id) | ItemRef::Via(id) => moving.contains(id),
                _ => false,
            }) {
                continue;
            }
            index.insert(Item::Static(i as u32), ob.bbox);
        }
        let moving_holes: BTreeSet<ObjectId> = vias.iter().map(|v| v.id).collect();
        let mut w = World {
            rb,
            lines: Vec::new(),
            vias: Vec::new(),
            index,
            owner: BTreeMap::new(),
            reach,
            moving_holes,
            ops: 0,
        };
        // Vias: movable unless they sit on static copper of their net.
        let mut via_at: BTreeSet<(u32, (i64, i64))> = BTreeSet::new();
        for v in &vias {
            let net = movable_net(&v.net).expect("movable");
            let at = P::of(v.at);
            via_at.insert((net, key(at)));
            let rv = v.diameter.0 as f64 / 2.0;
            let fixed = w.statics_near(at, rv).into_iter().any(|o| {
                let ob = &rb.obstacles[o as usize];
                ob.net == Some(net) && matches!(ob.kind, ObKind::Pad | ObKind::Copper) && ob.shape.dist_point(at) <= rv
            });
            w.add_via(WVia {
                net,
                at,
                rv,
                dr: v.drill.0 as f64 / 2.0,
                orig_at: at,
                orig: Some(v.id),
                via: (*v).clone(),
                fixed,
                moved: false,
                id: 0,
            });
        }
        // Fixed vias of other nets count as anchors too.
        for v in board.vias.iter().filter(|v| !moving.contains(&v.id)) {
            if let Some(n) = v.net.as_ref().and_then(|n| rb.net_ids.get(n)) {
                via_at.insert((*n, key(P::of(v.at))));
            }
        }
        // Lines: chains of segments per (net, layer) between anchors.
        let names = &rb.layer_names;
        let mut groups: BTreeMap<(u32, usize), Vec<&Track>> = BTreeMap::new();
        for t in &tracks {
            let Some(layer) = names.iter().position(|n| *n == t.layer) else { continue };
            groups.entry((movable_net(&t.net).expect("movable"), layer)).or_default().push(t);
        }
        for ((net, layer), segs) in groups {
            let mut ends: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
            for (i, t) in segs.iter().enumerate() {
                ends.entry(key(P::of(t.start))).or_default().push(i);
                ends.entry(key(P::of(t.end))).or_default().push(i);
            }
            let anchor = |k: (i64, i64), w: &World<'_>| -> bool {
                if ends.get(&k).map_or(0, Vec::len) != 2 || via_at.contains(&(net, k)) {
                    return true;
                }
                let at = P::new(k.0 as f64, k.1 as f64);
                w.statics_near(at, 1.0).into_iter().any(|o| {
                    let ob = &rb.obstacles[o as usize];
                    ob.net == Some(net) && ob.tracks & (1u64 << layer.min(63)) != 0 && ob.shape.dist_point(at) <= 1.0
                })
            };
            let mut used = vec![false; segs.len()];
            let mut chains: Vec<Vec<usize>> = Vec::new();
            // Start from anchors, then whatever is left (cycles).
            let starts: Vec<(i64, i64)> = ends.keys().copied().filter(|k| anchor(*k, &w)).collect();
            let walk = |start: (i64, i64), used: &mut Vec<bool>, chains: &mut Vec<Vec<usize>>, w: &World<'_>| {
                for &s0 in ends.get(&start).map(Vec::as_slice).unwrap_or(&[]) {
                    if used[s0] {
                        continue;
                    }
                    let mut chain = Vec::new();
                    let mut s = s0;
                    let mut at = start;
                    loop {
                        used[s] = true;
                        chain.push(s);
                        let t = segs[s];
                        let other = if key(P::of(t.start)) == at { key(P::of(t.end)) } else { key(P::of(t.start)) };
                        at = other;
                        if anchor(at, w) {
                            break;
                        }
                        match ends[&at].iter().copied().find(|&n| !used[n]) {
                            Some(n) => s = n,
                            None => break,
                        }
                    }
                    chains.push(chain);
                }
            };
            for k in starts {
                walk(k, &mut used, &mut chains, &w);
            }
            let rest: Vec<(i64, i64)> = ends.keys().copied().collect();
            let first_cycle = chains.len();
            for k in rest {
                walk(k, &mut used, &mut chains, &w);
            }
            for (ci, chain) in chains.iter().enumerate() {
                // Points in chain order.
                let first = segs[chain[0]];
                let mut pts = vec![P::of(first.start), P::of(first.end)];
                if chain.len() > 1 {
                    let next = segs[chain[1]];
                    let shared = |q: P| key(q) == key(P::of(next.start)) || key(q) == key(P::of(next.end));
                    if !shared(pts[1]) {
                        pts.reverse();
                    }
                }
                for &s in &chain[1..] {
                    let t = segs[s];
                    let last = key(*pts.last().expect("point"));
                    pts.push(if key(P::of(t.start)) == last { P::of(t.end) } else { P::of(t.start) });
                }
                let width = segs.iter().map(|t| t.width).max().unwrap_or(Nm(0));
                let width = chain.iter().map(|&s| segs[s].width).max().unwrap_or(width);
                // Mixed widths or cycles stay where they are.
                let fixed = ci >= first_cycle || chain.iter().any(|&s| segs[s].width != width);
                w.add_line(Line {
                    net,
                    layer,
                    width,
                    hw: width.0 as f64 / 2.0,
                    orig_pts: pts.clone(),
                    pts,
                    orig: chain.iter().map(|&s| segs[s].id).collect(),
                    fixed,
                    moved: false,
                    ids: vec![],
                });
            }
        }
        // A via holding a line that cannot move cannot move either.
        for vi in 0..w.vias.len() {
            if w.attached(vi).iter().any(|&l| w.lines[l].fixed) {
                w.vias[vi].fixed = true;
            }
        }
        w
    }

    fn statics_near(&self, at: P, r: f64) -> Vec<u32> {
        self.index
            .query(&BoxF::of2(at, at).expand(r + self.reach))
            .into_iter()
            .filter_map(|(_, it)| match it {
                Item::Static(o) => Some(o),
                _ => None,
            })
            .collect()
    }

    fn insert_line(&mut self, li: usize) {
        let l = &self.lines[li];
        let mut ids = Vec::new();
        for s in l.pts.windows(2) {
            let id = self.index.insert(
                Item::Seg { net: l.net, layer: l.layer as u8, a: s[0], b: s[1] },
                BoxF::of2(s[0], s[1]).expand(l.hw),
            );
            self.owner.insert(id, Owner::Seg(li));
            ids.push(id);
        }
        self.lines[li].ids = ids;
    }

    fn remove_line(&mut self, li: usize) {
        for id in std::mem::take(&mut self.lines[li].ids) {
            self.index.remove(id);
            self.owner.remove(&id);
        }
    }

    pub fn add_line(&mut self, l: Line) -> usize {
        self.lines.push(l);
        let li = self.lines.len() - 1;
        self.insert_line(li);
        li
    }

    pub fn add_via(&mut self, v: WVia) -> usize {
        self.vias.push(v);
        let vi = self.vias.len() - 1;
        self.insert_via(vi);
        vi
    }

    fn insert_via(&mut self, vi: usize) {
        let v = &self.vias[vi];
        let id = self.index.insert(Item::Via { net: v.net, at: v.at }, BoxF::of2(v.at, v.at).expand(v.rv.max(v.dr)));
        self.owner.insert(id, Owner::Via(vi));
        self.vias[vi].id = id;
    }

    fn remove_via(&mut self, vi: usize) {
        let id = self.vias[vi].id;
        self.index.remove(id);
        self.owner.remove(&id);
    }

    fn set_line(&mut self, li: usize, pts: Vec<P>) {
        self.remove_line(li);
        self.lines[li].pts = pts;
        self.lines[li].moved = true;
        self.insert_line(li);
    }

    fn c(&self, net: u32) -> f64 {
        self.rb.profile(net).c
    }

    /// Everything a track segment of `net` (half width `hw`) on `layer` along `a`–`b` comes
    /// too close to, with the required distance for each (nm).
    fn seg_hits(&self, layer: usize, a: P, b: P, hw: f64, net: u32) -> Vec<(Hit, f64)> {
        let rb = self.rb;
        let mut out = Vec::new();
        if !rb.inside(a) || !rb.inside(b) {
            out.push((Hit::Outside, 0.0));
        }
        let c = self.c(net);
        let bit = 1u64 << layer.min(63);
        let q = BoxF::of2(a, b).expand(hw + self.reach);
        for (id, it) in self.index.query(&q) {
            match it {
                Item::Static(o) => {
                    let ob = &rb.obstacles[o as usize];
                    if ob.tracks & bit == 0 || (ob.net == Some(net) && matches!(ob.kind, ObKind::Pad | ObKind::Copper))
                    {
                        continue;
                    }
                    let req = hw + ob.clear.with(c);
                    if ob.bbox.expand(req).intersects(&BoxF::of2(a, b)) && ob.shape.dist_seg(a, b) < req - TOL {
                        out.push((Hit::Static(o), req));
                    }
                }
                Item::Seg { net: m, layer: l, a: p, b: q } => {
                    if m == net || l as usize != layer {
                        continue;
                    }
                    let Some(Owner::Seg(li)) = self.owner.get(&id) else { continue };
                    let req = hw + self.lines[*li].hw + c.max(self.c(m));
                    if seg_seg_dist(a, b, p, q) < req - TOL {
                        out.push((Hit::Line(*li), req));
                    }
                }
                Item::Sized { .. } => {} // widened tracks: final pass only
                Item::Via { net: m, at } => {
                    if m == net {
                        continue;
                    }
                    let Some(Owner::Via(vi)) = self.owner.get(&id) else { continue };
                    let req = hw + self.vias[*vi].rv + c.max(self.c(m));
                    if point_seg_d2(at, a, b).sqrt() < req - TOL {
                        out.push((Hit::Via(*vi), req));
                    }
                }
            }
        }
        out.sort_by_key(|x| x.0);
        out.dedup_by(|x, y| x.0 == y.0);
        out
    }

    /// Everything via `vi` (at its current position) comes too close to, with the required
    /// distance between centers / to the shape.
    fn via_hits(&self, vi: usize) -> Vec<(Hit, f64)> {
        let v = &self.vias[vi];
        let (at, rv, dr, net) = (v.at, v.rv, v.dr, v.net);
        let rb = self.rb;
        let mut out = Vec::new();
        if !rb.inside(at) {
            out.push((Hit::Outside, 0.0));
        }
        let c = self.c(net);
        for (hi, h) in rb.holes.iter().enumerate() {
            if h.via.is_some_and(|id| self.moving_holes.contains(&id)) {
                continue;
            }
            let req = dr + h.r + rb.h2h;
            if h.at.dist(at) < req - TOL {
                out.push((Hit::Hole(hi as u32), req));
            }
        }
        let q = BoxF::of2(at, at).expand(rv.max(dr) + self.reach);
        for (id, it) in self.index.query(&q) {
            if id == v.id {
                continue;
            }
            match it {
                Item::Static(o) => {
                    let ob = &rb.obstacles[o as usize];
                    if !ob.vias {
                        continue;
                    }
                    let own = ob.net == Some(net);
                    let req = match ob.kind {
                        ObKind::Pad | ObKind::Copper if own => continue,
                        _ => rv + ob.clear.with(c),
                    };
                    if ob.shape.dist_point(at) < req - TOL {
                        out.push((Hit::Static(o), req));
                    }
                }
                Item::Seg { net: m, a, b, .. } => {
                    if m == net {
                        continue;
                    }
                    let Some(Owner::Seg(li)) = self.owner.get(&id) else { continue };
                    let req = rv + self.lines[*li].hw + c.max(self.c(m));
                    if point_seg_d2(at, a, b).sqrt() < req - TOL {
                        out.push((Hit::Line(*li), req));
                    }
                }
                Item::Sized { .. } => {} // widened tracks: final pass only
                Item::Via { net: m, at: o } => {
                    let Some(Owner::Via(wj)) = self.owner.get(&id) else { continue };
                    let w = &self.vias[*wj];
                    let holes = dr + w.dr + rb.h2h;
                    let req = if m == net { holes } else { holes.max(rv + w.rv + c.max(self.c(m))) };
                    if at.dist(o) < req - TOL {
                        out.push((Hit::Via(*wj), req));
                    }
                }
            }
        }
        out.sort_by_key(|x| x.0);
        out.dedup_by(|x, y| x.0 == y.0);
        out
    }

    /// Hits of a whole line, with the index of the first segment hitting each.
    fn line_hits(&self, li: usize) -> Vec<(Hit, usize)> {
        let l = &self.lines[li];
        let mut out: Vec<(Hit, usize)> = Vec::new();
        for (si, s) in l.pts.windows(2).enumerate() {
            for (h, _) in self.seg_hits(l.layer, s[0], s[1], l.hw, l.net) {
                if h != Hit::Line(li) && !out.iter().any(|(x, _)| *x == h) {
                    out.push((h, si));
                }
            }
        }
        if l.pts.len() == 1 {
            for (h, _) in self.seg_hits(l.layer, l.pts[0], l.pts[0], l.hw, l.net) {
                out.push((h, 0));
            }
        }
        out
    }

    /// Whether `h` can be moved.
    fn movable(&self, h: Hit) -> bool {
        match h {
            Hit::Line(li) => !self.lines[li].fixed,
            Hit::Via(vi) => !self.vias[vi].fixed,
            _ => false,
        }
    }

    /// The hull of `h` for an item of `net` with radius `r` (half width or via radius).
    fn hull_of(&self, h: Hit, r: f64, net: u32) -> Option<Vec<P>> {
        let c = self.c(net);
        match h {
            Hit::Static(o) => {
                let ob = &self.rb.obstacles[o as usize];
                Some(hull_shape(&ob.shape, r + ob.clear.with(c) + MARGIN))
            }
            Hit::Hole(hi) => {
                let hole = &self.rb.holes[hi as usize];
                Some(octagon_hull(hole.at, hole.at, r + hole.r + self.rb.h2h + MARGIN))
            }
            Hit::Line(li) => {
                let l = &self.lines[li];
                Some(octagon_hull_pts(&l.pts, r + l.hw + c.max(self.c(l.net)) + MARGIN))
            }
            Hit::Via(vi) => {
                let v = &self.vias[vi];
                Some(octagon_hull(v.at, v.at, r + v.rv + c.max(self.c(v.net)) + MARGIN))
            }
            Hit::Outside => None,
        }
    }
}

/// Octagon (inradius `d`) vertices around `c`, counter-clockwise.
fn octagon(c: P, d: f64) -> [P; 8] {
    let r = d / (std::f64::consts::PI / 8.0).cos();
    let mut out = [P::default(); 8];
    for (k, o) in out.iter_mut().enumerate() {
        let a = std::f64::consts::PI / 8.0 + k as f64 * std::f64::consts::PI / 4.0;
        *o = P::new(c.x + r * a.cos(), c.y + r * a.sin());
    }
    out
}

/// Convex hull (counter-clockwise, no collinear points), Andrew's monotone chain.
fn convex_hull(mut pts: Vec<P>) -> Vec<P> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| a.dist(*b) < 1e-6);
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: P, a: P, b: P| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut h: Vec<P> = Vec::with_capacity(2 * pts.len());
    for &p in &pts {
        while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], p) <= 0.0 {
            h.pop();
        }
        h.push(p);
    }
    let lower = h.len() + 1;
    for &p in pts.iter().rev().skip(1) {
        while h.len() >= lower && cross(h[h.len() - 2], h[h.len() - 1], p) <= 0.0 {
            h.pop();
        }
        h.push(p);
    }
    h.pop();
    h
}

/// Hull of segment `a`–`b` grown by `d`.
pub(crate) fn octagon_hull(a: P, b: P, d: f64) -> Vec<P> {
    let mut v = octagon(a, d).to_vec();
    v.extend(octagon(b, d));
    convex_hull(v)
}

pub(crate) fn octagon_hull_pts(pts: &[P], d: f64) -> Vec<P> {
    convex_hull(pts.iter().flat_map(|&p| octagon(p, d)).collect())
}

pub(crate) fn hull_shape(s: &super::geo::Shape, d: f64) -> Vec<P> {
    match s {
        super::geo::Shape::Capsule { a, b, r } => octagon_hull(*a, *b, r + d),
        super::geo::Shape::Polys(v) => {
            let pts: Vec<P> = v.iter().flat_map(|p| p.rings[0].iter().copied()).collect();
            octagon_hull_pts(&pts, d)
        }
    }
}

fn inside_convex(h: &[P], p: P) -> bool {
    let n = h.len();
    n >= 3
        && (0..n).all(|i| {
            let (a, b) = (h[i], h[(i + 1) % n]);
            (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x) > 1e-6
        })
}

/// Intersection parameter on `a`–`b` with edge `c`–`d` (`t` on the first in [0, 1], `u` on
/// the edge in [0, 1)).
fn cross_param(a: P, b: P, c: P, d: P) -> Option<f64> {
    let r = P::new(b.x - a.x, b.y - a.y);
    let s = P::new(d.x - c.x, d.y - c.y);
    let den = r.x * s.y - r.y * s.x;
    if den.abs() < 1e-9 {
        return None;
    }
    let qp = P::new(c.x - a.x, c.y - a.y);
    let t = (qp.x * s.y - qp.y * s.x) / den;
    let u = (qp.x * r.y - qp.y * r.x) / den;
    ((0.0..=1.0).contains(&t) && (0.0..1.0).contains(&u)).then_some(t)
}

/// The two ways around hull `h` for polyline `pts` (counter-clockwise first), or `None` when
/// an end lies inside the hull or the line does not pass through it.
pub(crate) fn walkaround(pts: &[P], h: &[P]) -> Option<[Vec<P>; 2]> {
    let n = h.len();
    if n < 3 || pts.len() < 2 {
        return None;
    }
    if inside_convex(h, pts[0]) || inside_convex(h, pts[pts.len() - 1]) {
        return None;
    }
    // Crossings: (position along the path, segment, point, hull edge).
    let mut xs: Vec<(f64, usize, P, usize)> = Vec::new();
    for (i, s) in pts.windows(2).enumerate() {
        for k in 0..n {
            if let Some(t) = cross_param(s[0], s[1], h[k], h[(k + 1) % n]) {
                xs.push((i as f64 + t, i, P::new(s[0].x + t * (s[1].x - s[0].x), s[0].y + t * (s[1].y - s[0].y)), k));
            }
        }
    }
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));
    if xs.len() < 2 {
        return None;
    }
    let (e, x) = (xs[0], xs[xs.len() - 1]);
    let build = |ccw: bool| -> Vec<P> {
        let mut out: Vec<P> = pts[..=e.1].to_vec();
        out.push(e.2);
        if ccw {
            let cnt = (x.3 + n - e.3) % n;
            for m in 0..cnt {
                out.push(h[(e.3 + 1 + m) % n]);
            }
        } else {
            let cnt = (e.3 + n - x.3) % n;
            for m in 0..cnt {
                out.push(h[(e.3 + n - m) % n]);
            }
        }
        out.push(x.2);
        out.extend_from_slice(&pts[x.1 + 1..]);
        clean(out)
    };
    Some([build(true), build(false)])
}

/// Drops repeated and collinear points.
pub(crate) fn clean(pts: Vec<P>) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last().is_some_and(|l: &P| l.dist(p) < 1.0) {
            continue;
        }
        while out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let cross = (b.x - a.x) * (p.y - b.y) - (b.y - a.y) * (p.x - b.x);
            let dot = (b.x - a.x) * (p.x - b.x) + (b.y - a.y) * (p.y - b.y);
            let l = a.dist(b) * b.dist(p);
            if l > 0.0 && cross.abs() / l < 1e-9 && dot > 0.0 {
                out.pop();
            } else {
                break;
            }
        }
        out.push(p);
    }
    out
}

fn length(pts: &[P]) -> f64 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

/// Closest point of the boundary of convex polygon `h` to `p`, nudged outwards.
fn exit_point(h: &[P], p: P) -> P {
    let n = h.len();
    let mut best = (f64::MAX, p);
    for k in 0..n {
        let q = super::geo::project(p, h[k], h[(k + 1) % n]);
        let d = q.dist(p);
        if d < best.0 {
            best = (d, q);
        }
    }
    // Nudge away from the centroid.
    let c = P::new(h.iter().map(|q| q.x).sum::<f64>() / n as f64, h.iter().map(|q| q.y).sum::<f64>() / n as f64);
    let (dx, dy) = (best.1.x - c.x, best.1.y - c.y);
    let l = (dx * dx + dy * dy).sqrt().max(1.0);
    P::new(best.1.x + dx / l * 2.0, best.1.y + dy / l * 2.0)
}

impl World<'_> {
    fn fail(&self, hit: Hit, at: P, layer: Option<usize>, why: FailWhy) -> Fail {
        let rb = self.rb;
        let net_ref = |n: u32| ObjectRef::Net(rb.nets[n as usize].clone());
        let (label, subjects, locked) = match hit {
            Hit::Static(o) => {
                let ob = &rb.obstacles[o as usize];
                let mut subjects = Vec::new();
                if let Some(n) = ob.net {
                    subjects.push(net_ref(n));
                }
                if let Some(owner) = &ob.owner {
                    subjects.push(ObjectRef::Name(owner.clone()));
                }
                (ob.label.clone(), subjects, ob.kind == ObKind::Copper && ob.movable.is_none())
            }
            Hit::Hole(_) => ("a drilled hole (hole-to-hole)".to_string(), vec![], false),
            Hit::Outside => ("the board outline".to_string(), vec![], false),
            Hit::Line(li) => {
                let l = &self.lines[li];
                let mut subjects = vec![net_ref(l.net)];
                subjects.extend(l.orig.iter().map(|id| ObjectRef::Item { kind: "track".into(), index: id.0 }));
                let first = l.orig.first().map(|id| format!("track#{}", id.0)).unwrap_or_else(|| "a track".into());
                (format!("{first} (net {})", rb.nets[l.net as usize]), subjects, false)
            }
            Hit::Via(vi) => {
                let v = &self.vias[vi];
                let mut subjects = vec![net_ref(v.net)];
                let what = match v.orig {
                    Some(id) => {
                        subjects.push(ObjectRef::Item { kind: "via".into(), index: id.0 });
                        format!("via#{}", id.0)
                    }
                    None => "a new via".into(),
                };
                (format!("{what} (net {})", rb.nets[v.net as usize]), subjects, false)
            }
        };
        Fail { at, layer, why, label, subjects, locked }
    }

    /// A point of hit `h` for reports.
    pub fn hit_at(&self, h: Hit, near: P) -> P {
        match h {
            Hit::Static(o) => {
                let b = self.rb.obstacles[o as usize].bbox;
                P::new((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0)
            }
            Hit::Hole(hi) => self.rb.holes[hi as usize].at,
            Hit::Line(li) => {
                let l = &self.lines[li];
                l.pts.iter().copied().min_by(|a, b| a.dist(near).total_cmp(&b.dist(near))).unwrap_or(near)
            }
            Hit::Via(vi) => self.vias[vi].at,
            Hit::Outside => near,
        }
    }

    /// Bends line `li` around everything `blocks` accepts, until it is clear of them.
    fn walk_line(&mut self, li: usize, blocks: &dyn Fn(&World<'_>, Hit) -> bool) -> Result<bool, Fail> {
        let mut walked = false;
        for _ in 0..MAX_WALKS {
            let hits: Vec<(Hit, usize)> = self.line_hits(li).into_iter().filter(|(h, _)| blocks(self, *h)).collect();
            let Some(&(h, si)) = hits.first() else { return Ok(walked) };
            let (at, layer, pts) = (self.lines[li].pts[si], self.lines[li].layer, self.lines[li].pts.clone());
            if h == Hit::Outside {
                return Err(self.fail(h, at, Some(layer), FailWhy::Blocked));
            }
            let hull = self.hull_for_line(h, li, si);
            let Some(cands) = walkaround(&pts, &hull) else {
                return Err(self.fail(h, self.hit_at(h, at), Some(layer), FailWhy::Blocked));
            };
            let pick = self.pick(li, cands, blocks);
            let Some(pts) = pick else {
                return Err(self.fail(h, self.hit_at(h, at), Some(layer), FailWhy::Blocked));
            };
            self.set_line(li, pts);
            walked = true;
        }
        let hits: Vec<(Hit, usize)> = self.line_hits(li).into_iter().filter(|(h, _)| blocks(self, *h)).collect();
        match hits.first() {
            None => Ok(walked),
            Some(&(h, si)) => {
                let l = &self.lines[li];
                Err(self.fail(h, self.hit_at(h, l.pts[si]), Some(l.layer), FailWhy::Blocked))
            }
        }
    }

    /// The hull of `h` for line `li` (around its segment `si` when `h` is a line: only the
    /// segments of `h` near that segment, so long lines do not get one coarse hull).
    fn hull_for_line(&self, h: Hit, li: usize, si: usize) -> Vec<P> {
        let l = &self.lines[li];
        match h {
            Hit::Line(lj) => {
                let o = &self.lines[lj];
                let d = l.hw + o.hw + self.c(l.net).max(self.c(o.net)) + MARGIN;
                let (a, b) = (l.pts[si], l.pts[(si + 1).min(l.pts.len() - 1)]);
                // The segments of `o` that come too close to `a`–`b`.
                let near: Vec<usize> = (0..o.pts.len().saturating_sub(1))
                    .filter(|&k| seg_seg_dist(a, b, o.pts[k], o.pts[k + 1]) < d - TOL)
                    .collect();
                let (k0, k1) = (near.first().copied().unwrap_or(0), near.last().copied().unwrap_or(0));
                octagon_hull_pts(&o.pts[k0.min(o.pts.len() - 1)..=(k1 + 1).min(o.pts.len() - 1)], d)
            }
            _ => self.hull_of(h, l.hw, l.net).unwrap_or_default(),
        }
    }

    /// Of two walkaround candidates for line `li`, the one with the fewest blocking hits,
    /// then the shortest; `None` when both still hit what they went around.
    fn pick(&mut self, li: usize, cands: [Vec<P>; 2], blocks: &dyn Fn(&World<'_>, Hit) -> bool) -> Option<Vec<P>> {
        let old = self.lines[li].pts.clone();
        let moved = self.lines[li].moved;
        let mut scored: Vec<(usize, f64, Vec<P>)> = Vec::new();
        for c in cands {
            if c.len() < 2 {
                continue;
            }
            self.set_line(li, c.clone());
            let bad = self.line_hits(li).into_iter().filter(|(h, _)| blocks(self, *h)).count();
            scored.push((bad, length(&c), c));
        }
        self.set_line(li, old);
        self.lines[li].moved = moved;
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        scored.into_iter().next().map(|s| s.2)
    }

    /// Moves via `vi` out of the hull of `h` (its attached segment ends follow).
    fn push_via(&mut self, vi: usize, hull: &[P]) {
        let old = self.vias[vi].at;
        let new = exit_point(hull, old);
        self.move_via(vi, new);
    }

    fn move_via(&mut self, vi: usize, new: P) {
        let old = self.vias[vi].at;
        let net = self.vias[vi].net;
        self.remove_via(vi);
        self.vias[vi].at = new;
        self.vias[vi].moved = true;
        self.insert_via(vi);
        let k = key(old);
        for li in 0..self.lines.len() {
            let l = &self.lines[li];
            if l.net != net || l.fixed {
                continue;
            }
            let (s, e) = (key(l.pts[0]), key(l.pts[l.pts.len() - 1]));
            if s != k && e != k {
                continue;
            }
            let mut pts = l.pts.clone();
            if s == k {
                pts[0] = new;
            }
            if e == k {
                let n = pts.len();
                pts[n - 1] = new;
            }
            self.set_line(li, clean(pts));
        }
    }

    /// Lines attached to via `vi` (an end at its center, same net).
    fn attached(&self, vi: usize) -> Vec<usize> {
        let v = &self.vias[vi];
        let k = key(v.at);
        (0..self.lines.len())
            .filter(|&li| {
                let l = &self.lines[li];
                l.net == v.net && (key(l.pts[0]) == k || key(l.pts[l.pts.len() - 1]) == k)
            })
            .collect()
    }

    /// Shoves everything movable that item `b` collides with; moved items are queued.
    fn shove_from(&mut self, b: Body, queue: &mut VecDeque<Body>) -> Result<(), Fail> {
        let hits: Vec<(Hit, usize)> = match b {
            Body::Line(li) => self.line_hits(li),
            Body::Via(vi) => self.via_hits(vi).into_iter().map(|(h, _)| (h, 0)).collect(),
        };
        // Vias first: moving them drags the ends of their lines along.
        let mut hits = hits;
        hits.sort_by_key(|(h, _)| !matches!(h, Hit::Via(_)));
        for (h, si) in hits {
            if !self.movable(h) {
                continue;
            }
            self.ops += 1;
            if self.ops > MAX_OPS {
                let at = self.hit_at(h, P::default());
                return Err(self.fail(h, at, None, FailWhy::Limit));
            }
            // Is it still in the way (an earlier shove may have moved it)?
            let still = match b {
                Body::Line(li) => self.line_hits(li).iter().any(|(x, _)| *x == h),
                Body::Via(vi) => self.via_hits(vi).iter().any(|(x, _)| *x == h),
            };
            if !still {
                continue;
            }
            match h {
                Hit::Line(lj) => {
                    // Walk `lj` around the pusher (segment by segment for a line).
                    let pusher = b;
                    let blocks = move |w: &World<'_>, x: Hit| match pusher {
                        Body::Line(li) => x == Hit::Line(li),
                        Body::Via(vi) => x == Hit::Via(vi),
                    } || !w.movable(x);
                    for _ in 0..MAX_WALKS {
                        let hit_now: Option<usize> = self
                            .line_hits(lj)
                            .into_iter()
                            .find(|(x, _)| match pusher {
                                Body::Line(li) => *x == Hit::Line(li),
                                Body::Via(vi) => *x == Hit::Via(vi),
                            })
                            .map(|(_, s)| s);
                        let Some(sj) = hit_now else { break };
                        let hull = match pusher {
                            Body::Line(li) => {
                                // The pusher's segments near `lj`'s segment.
                                let me = &self.lines[li];
                                let o = &self.lines[lj];
                                let d = me.hw + o.hw + self.c(me.net).max(self.c(o.net)) + MARGIN;
                                let (a, bb) = (o.pts[sj], o.pts[(sj + 1).min(o.pts.len() - 1)]);
                                let near: Vec<usize> = (0..me.pts.len().saturating_sub(1))
                                    .filter(|&k| seg_seg_dist(a, bb, me.pts[k], me.pts[k + 1]) < d - TOL)
                                    .collect();
                                let (k0, k1) = (near.first().copied().unwrap_or(0), near.last().copied().unwrap_or(0));
                                octagon_hull_pts(&me.pts[k0..=(k1 + 1).min(me.pts.len() - 1)], d)
                            }
                            Body::Via(vi) => {
                                let v = &self.vias[vi];
                                let o = &self.lines[lj];
                                octagon_hull(v.at, v.at, v.rv + o.hw + self.c(v.net).max(self.c(o.net)) + MARGIN)
                            }
                        };
                        let pts = self.lines[lj].pts.clone();
                        let Some(cands) = walkaround(&pts, &hull) else {
                            let at = pts[sj];
                            return Err(self.fail(h, at, Some(self.lines[lj].layer), FailWhy::Stuck));
                        };
                        let Some(np) = self.pick(lj, cands, &blocks) else {
                            return Err(self.fail(h, pts[sj], Some(self.lines[lj].layer), FailWhy::Stuck));
                        };
                        self.set_line(lj, np);
                    }
                    queue.push_back(Body::Line(lj));
                }
                Hit::Via(vj) => {
                    let hull = match b {
                        Body::Line(li) => {
                            let me = &self.lines[li];
                            let v = &self.vias[vj];
                            let d = me.hw + v.rv + self.c(me.net).max(self.c(v.net)) + MARGIN;
                            let s = si.min(me.pts.len().saturating_sub(2));
                            let (a, bb) = (me.pts[s], me.pts[(s + 1).min(me.pts.len() - 1)]);
                            octagon_hull(a, bb, d)
                        }
                        Body::Via(vi) => {
                            let me = &self.vias[vi];
                            let v = &self.vias[vj];
                            let holes = me.dr + v.dr + self.rb.h2h;
                            let d = holes.max(me.rv + v.rv + self.c(me.net).max(self.c(v.net))) + MARGIN;
                            octagon_hull(me.at, me.at, d)
                        }
                    };
                    self.push_via(vj, &hull);
                    queue.push_back(Body::Via(vj));
                    for l in self.attached(vj) {
                        queue.push_back(Body::Line(l));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Resolves a moved via's collisions with fixed things by pushing it further.
    fn settle_via(&mut self, vi: usize) -> Result<(), Fail> {
        for _ in 0..MAX_WALKS {
            let hits: Vec<(Hit, f64)> = self.via_hits(vi).into_iter().filter(|(h, _)| !self.movable(*h)).collect();
            let Some(&(h, _)) = hits.first() else { return Ok(()) };
            let v = &self.vias[vi];
            let hull = match h {
                Hit::Line(lj) => {
                    let o = &self.lines[lj];
                    octagon_hull_pts(&o.pts, v.rv + o.hw + self.c(v.net).max(self.c(o.net)) + MARGIN)
                }
                Hit::Via(vj) => {
                    let o = &self.vias[vj];
                    let holes = v.dr + o.dr + self.rb.h2h;
                    let req =
                        if o.net == v.net { holes } else { holes.max(v.rv + o.rv + self.c(v.net).max(self.c(o.net))) };
                    octagon_hull(o.at, o.at, req + MARGIN)
                }
                Hit::Hole(hi) => {
                    let hole = &self.rb.holes[hi as usize];
                    octagon_hull(hole.at, hole.at, v.dr + hole.r + self.rb.h2h + MARGIN)
                }
                Hit::Static(o) => {
                    let ob = &self.rb.obstacles[o as usize];
                    hull_shape(&ob.shape, v.rv + ob.clear.with(self.c(v.net)) + MARGIN)
                }
                Hit::Outside => return Err(self.fail(h, v.at, None, FailWhy::Stuck)),
            };
            self.push_via(vi, &hull);
        }
        let hits: Vec<(Hit, f64)> = self.via_hits(vi).into_iter().filter(|(h, _)| !self.movable(*h)).collect();
        match hits.first() {
            None => Ok(()),
            Some(&(h, _)) => Err(self.fail(h, self.vias[vi].at, None, FailWhy::Stuck)),
        }
    }

    /// Whether line `li` and via `vi` are clear of everything.
    fn line_clear(&self, li: usize) -> bool {
        self.line_hits(li).is_empty()
    }

    /// Spring-back: moved lines go back to their original geometry when legal, otherwise
    /// they are pulled tight; moved vias go back when they and their lines can.
    fn spring_back(&mut self, grid: f64) {
        for _ in 0..2 {
            for vi in 0..self.vias.len() {
                let v = &self.vias[vi];
                if !v.moved || v.fixed || v.at.dist(v.orig_at) < 1.0 {
                    continue;
                }
                let (now, orig) = (v.at, v.orig_at);
                let saved: Vec<(usize, Vec<P>)> =
                    self.attached(vi).into_iter().map(|l| (l, self.lines[l].pts.clone())).collect();
                self.move_via(vi, orig);
                let ok = self.via_hits(vi).is_empty() && self.attached(vi).iter().all(|&l| self.line_clear(l));
                if !ok {
                    self.move_via(vi, now);
                    for (l, pts) in saved {
                        self.set_line(l, pts);
                    }
                }
            }
            for li in 0..self.lines.len() {
                let l = &self.lines[li];
                if !l.moved || l.fixed {
                    continue;
                }
                let cur = l.pts.clone();
                let orig = l.orig_pts.clone();
                let (layer, net, hw) = (l.layer, l.net, l.hw);
                let ends_same = key(orig[0]) == key(cur[0]) && key(orig[orig.len() - 1]) == key(cur[cur.len() - 1]);
                if ends_same && cur != orig {
                    self.set_line(li, orig);
                    if self.line_clear(li) {
                        continue;
                    }
                    self.set_line(li, cur.clone());
                }
                // Pull tight against everything else.
                self.remove_line(li);
                let mut path = post::Path {
                    layer,
                    pinned: (0..cur.len()).map(|i| i == 0 || i + 1 == cur.len()).collect(),
                    pts: cur.clone(),
                    wire: 0,
                    mids: vec![],
                    widths: vec![],
                };
                {
                    let w: &World<'_> = self;
                    let free = |lay: usize, a: P, b: P| w.seg_hits(lay, a, b, hw, net).is_empty();
                    post::optimize_path(&mut path, &free, grid, 3, false);
                }
                self.lines[li].pts = clean(path.pts);
                self.insert_line(li);
                if !self.line_clear(li) {
                    self.set_line(li, cur);
                }
            }
        }
    }
}

/// Something that pushes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Body {
    Line(usize),
    Via(usize),
}

/// Places the head lines and vias in the world of `p`, moving other nets' unlocked copper out
/// of the way as `mode` allows. `grid` is the pull-tight step for spring-back.
pub(crate) fn run(
    p: &Project,
    rb: &RouterBoard,
    heads: &[HeadLine],
    head_vias: &[Via],
    mode: Mode,
    grid: f64,
) -> Result<Outcome, Fail> {
    let mut nets: BTreeSet<u32> = heads.iter().map(|h| h.net).collect();
    for v in head_vias {
        if let Some(n) = v.net.as_ref().and_then(|n| rb.net_ids.get(n)) {
            nets.insert(*n);
        }
    }
    let mut w = World::build(p, rb, &nets);
    let first_head_line = w.lines.len();
    let first_head_via = w.vias.len();
    for v in head_vias {
        let net = v.net.as_ref().and_then(|n| rb.net_ids.get(n)).copied().unwrap_or(0);
        let at = P::of(v.at);
        w.add_via(WVia {
            net,
            at,
            rv: v.diameter.0 as f64 / 2.0,
            dr: v.drill.0 as f64 / 2.0,
            orig_at: at,
            orig: None,
            via: v.clone(),
            fixed: true,
            moved: false,
            id: 0,
        });
    }
    for h in heads {
        w.add_line(Line {
            net: h.net,
            layer: h.layer,
            width: h.width,
            hw: h.width.0 as f64 / 2.0,
            pts: clean(h.pts.clone()),
            orig: vec![],
            orig_pts: h.pts.clone(),
            fixed: true,
            moved: false,
            ids: vec![],
        });
    }
    let mut walked = false;
    // Head walkaround: around fixed things (everything in walkaround mode).
    if mode != Mode::Strict {
        for li in first_head_line..w.lines.len() {
            let all = mode == Mode::Walkaround;
            let blocks = move |w: &World<'_>, h: Hit| all || !w.movable(h);
            walked |= w.walk_line(li, &blocks)?;
        }
    }
    // Head vias: fixed things in the way are failures.
    for vi in first_head_via..w.vias.len() {
        if let Some(&(h, _)) = w.via_hits(vi).iter().find(|(h, _)| mode != Mode::Shove || !w.movable(*h)) {
            return Err(w.fail(h, w.vias[vi].at, None, FailWhy::Blocked));
        }
    }
    if mode == Mode::Strict {
        for li in first_head_line..w.lines.len() {
            if let Some(&(h, si)) = w.line_hits(li).first() {
                let l = &w.lines[li];
                return Err(w.fail(h, w.hit_at(h, l.pts[si]), Some(l.layer), FailWhy::Blocked));
            }
        }
    }
    if mode == Mode::Shove {
        let mut queue: VecDeque<Body> = VecDeque::new();
        queue.extend((first_head_via..w.vias.len()).map(Body::Via));
        queue.extend((first_head_line..w.lines.len()).map(Body::Line));
        while let Some(b) = queue.pop_front() {
            // A moved item first gets clear of fixed things (walkaround / push), then shoves.
            match b {
                Body::Line(li) if !w.lines[li].fixed => {
                    let blocks = |w: &World<'_>, h: Hit| !w.movable(h);
                    if let Err(f) = w.walk_line(li, &blocks) {
                        return Err(w.fail(Hit::Line(li), f.at, f.layer, FailWhy::Stuck));
                    }
                }
                Body::Via(vi) if !w.vias[vi].fixed => w.settle_via(vi)?,
                _ => {}
            }
            w.shove_from(b, &mut queue)?;
        }
        w.spring_back(grid);
    }
    // Final check: everything new or moved is clear of everything.
    for li in 0..w.lines.len() {
        let l = &w.lines[li];
        if !(l.moved || li >= first_head_line) {
            continue;
        }
        if let Some(&(h, si)) = w.line_hits(li).first() {
            let why = if li >= first_head_line { FailWhy::Blocked } else { FailWhy::Stuck };
            let hit = if li >= first_head_line { h } else { Hit::Line(li) };
            return Err(w.fail(hit, w.hit_at(h, l.pts[si.min(l.pts.len() - 1)]), Some(l.layer), why));
        }
    }
    for vi in 0..w.vias.len() {
        let v = &w.vias[vi];
        if !(v.moved || vi >= first_head_via) {
            continue;
        }
        if let Some(&(h, _)) = w.via_hits(vi).first() {
            return Err(w.fail(h, v.at, None, FailWhy::Stuck));
        }
    }
    // Output.
    let mut out = Outcome { walked, ..Default::default() };
    let names = &rb.layer_names;
    for li in (first_head_line..w.lines.len()).chain(0..first_head_line) {
        let l = &w.lines[li];
        let head = li >= first_head_line;
        if !head && (!l.moved || l.pts == l.orig_pts) {
            continue;
        }
        if !head {
            out.removed_tracks.extend(l.orig.iter().copied());
            out.moved_nets.insert(l.net);
        }
        for s in l.pts.windows(2) {
            let (a, b) = (s[0].to_point(), s[1].to_point());
            if a == b {
                continue;
            }
            out.tracks.push(Track {
                id: ObjectId(0),
                layer: names[l.layer].clone(),
                width: l.width,
                net: Some(rb.nets[l.net as usize].clone()),
                start: a,
                end: b,
                mid: None,
                locked: false,
            });
        }
    }
    for vi in (first_head_via..w.vias.len()).chain(0..first_head_via) {
        let v = &w.vias[vi];
        let head = vi >= first_head_via;
        if !head && (!v.moved || v.at.dist(v.orig_at) < 0.5) {
            continue;
        }
        if let Some(id) = v.orig {
            out.removed_vias.push(id);
            out.moved_nets.insert(v.net);
        }
        out.vias.push(Via { id: ObjectId(0), at: v.at.to_point(), ..v.via.clone() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_and_walkaround() {
        let h = octagon_hull(P::new(0.0, 0.0), P::new(10_000.0, 0.0), 1_000.0);
        assert!(h.len() >= 8);
        // Every hull vertex keeps at least the distance.
        for q in &h {
            assert!(point_seg_d2(*q, P::new(0.0, 0.0), P::new(10_000.0, 0.0)).sqrt() >= 1_000.0 - 1e-6);
        }
        // A line crossing the hull vertically above the segment walks around it.
        let line = [P::new(5_000.0, 5_000.0), P::new(5_000.0, 500.0), P::new(12_000.0, 500.0)];
        let [a, b] = walkaround(&line, &h).expect("walk");
        for c in [&a, &b] {
            assert_eq!(c[0], line[0]);
            assert_eq!(*c.last().unwrap(), line[2]);
            for s in c.windows(2) {
                assert!(seg_seg_dist(s[0], s[1], P::new(0.0, 0.0), P::new(10_000.0, 0.0)) >= 1_000.0 - 1.0, "{c:?}");
            }
        }
        // An end inside the hull cannot walk around.
        assert!(walkaround(&[P::new(5_000.0, 5_000.0), P::new(5_000.0, 0.0)], &h).is_none());
        assert_eq!(clean(vec![P::new(0.0, 0.0), P::new(1.0, 0.0), P::new(5.0, 0.0), P::new(5.0, 5.0)]).len(), 3);
    }
}
