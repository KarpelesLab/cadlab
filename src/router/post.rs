//! Post-processing of grid routes: redundant via removal, collinear merging, 45° pull-tight
//! and corner chamfers. Every change is validated with exact clearance checks against all
//! other copper ([`Checker`]), so it can never introduce a violation.

use std::collections::{BTreeMap, BTreeSet};

use super::engine::{Engine, NetRoute};
use super::geo::{BoxF, P, point_seg_d2};
use super::index::{Checker, Index, Item};

/// A polyline on one layer.
#[derive(Clone, Debug)]
pub(crate) struct Path {
    /// Stackup layer index.
    pub layer: usize,
    pub pts: Vec<P>,
    pub pinned: Vec<bool>,
    /// Wire (index in the net's wires) it comes from.
    pub wire: usize,
}

/// Final geometry of a net.
#[derive(Clone, Debug, Default)]
pub(crate) struct NetGeom {
    pub paths: Vec<Path>,
    /// Via centers with their wire.
    pub vias: Vec<(P, usize)>,
    /// Index ids of the items inserted for this net.
    pub ids: Vec<u32>,
}

fn key(p: P) -> (i64, i64) {
    (p.x.round() as i64, p.y.round() as i64)
}

/// Converts a net's wires to polylines and vias (one vertex per grid node).
pub(crate) fn geometry(e: &Engine<'_>, nr: &NetRoute) -> NetGeom {
    // Junctions: wire ends per layer, with the number of wire ends there.
    let mut pins: BTreeMap<(usize, (i64, i64)), usize> = BTreeMap::new();
    let end_key = |n: u32| {
        let (s, _, _) = e.unpack(n);
        (e.slots[s], key(e.node_pos(n)))
    };
    for w in &nr.wires {
        for &n in [w.nodes.first(), w.nodes.last()].into_iter().flatten() {
            *pins.entry(end_key(n)).or_default() += 1;
        }
    }
    // A wire end with a stub is free to move unless another wire ends there too.
    let shared = |w: &super::engine::Wire, n: u32| {
        let k = end_key(n);
        let own = [w.nodes.first(), w.nodes.last()].into_iter().flatten().filter(|&&m| end_key(m) == k).count();
        pins.get(&k).copied().unwrap_or(0) > own
    };
    let mut g = NetGeom::default();
    for (wi, w) in nr.wires.iter().enumerate() {
        let mut cur: Option<Path> = None;
        for (k, &n) in w.nodes.iter().enumerate() {
            let (s, _, _) = e.unpack(n);
            let layer = e.slots[s];
            let p = e.node_pos(n);
            match &mut cur {
                Some(path) if path.layer == layer => {
                    path.pts.push(p);
                    path.pinned.push(pins.contains_key(&(layer, key(p))));
                }
                _ => {
                    if let Some(done) = cur.take() {
                        g.vias.push((p, wi));
                        g.paths.push(done);
                    }
                    let mut path = Path { layer, pts: vec![], pinned: vec![], wire: wi };
                    if k == 0
                        && let Some(st) = &w.start
                    {
                        // Anchor pinned; an escape's bend may move.
                        for (i, &q) in st.iter().enumerate().rev() {
                            path.pts.push(q);
                            path.pinned.push(i + 1 == st.len());
                        }
                    }
                    path.pts.push(p);
                    path.pinned.push(k > 0 || w.start.is_none() || shared(w, n));
                    cur = Some(path);
                }
            }
        }
        if let Some(mut path) = cur.take() {
            let last_node = *w.nodes.last().expect("node");
            if let Some(last) = path.pinned.last_mut() {
                *last = w.end.is_none() || shared(w, last_node) || w.nodes.len() == 1;
            }
            if let Some(en) = &w.end {
                for (i, &q) in en.iter().enumerate() {
                    path.pts.push(q);
                    path.pinned.push(i + 1 == en.len());
                }
            }
            g.paths.push(path);
        }
    }
    // Ends of every path are pinned (vias, junctions, terminals).
    for p in &mut g.paths {
        if let Some(f) = p.pinned.first_mut() {
            *f = true;
        }
        if let Some(l) = p.pinned.last_mut() {
            *l = true;
        }
    }
    g.vias.sort_by(|a, b| key(a.0).cmp(&key(b.0)).then(a.1.cmp(&b.1)));
    g.vias.dedup_by(|a, b| key(a.0) == key(b.0));
    g
}

/// Makes an escape (`pts` from the pad center, on stackup layer `layer`, existing copper of
/// the net) part of the one path that attaches to it, so that the optimizer can smooth the
/// junction. Returns whether it was absorbed; it is not when no path or several paths touch
/// it (the escape copper then stays as it is).
pub(crate) fn absorb(g: &mut NetGeom, layer: usize, pts: &[P]) -> bool {
    let on = |q: P| pts.windows(2).position(|s| point_seg_d2(q, s[0], s[1]) < 4.0);
    let mut hits = Vec::new();
    for (pi, p) in g.paths.iter().enumerate() {
        for (k, q) in p.pts.iter().enumerate() {
            if let Some(seg) = on(*q) {
                hits.push((pi, k, seg));
            }
        }
    }
    let [(pi, k, seg)] = hits[..] else { return false };
    let p = &mut g.paths[pi];
    if p.layer != layer || (k != 0 && k + 1 != p.pts.len()) {
        return false;
    }
    if k != 0 {
        p.pts.reverse();
        p.pinned.reverse();
    }
    let q = p.pts[0];
    let junction = g.vias.iter().any(|v| key(v.0) == key(q));
    let mut new_pts: Vec<P> = pts[..=seg].to_vec();
    let mut new_pin: Vec<bool> = (0..=seg).map(|i| i == 0).collect();
    if new_pts.last().is_some_and(|l| l.dist(q) < 1.0) {
        new_pts.pop();
        new_pin.pop();
    }
    p.pinned[0] = junction;
    new_pts.extend(p.pts.iter().copied());
    new_pin.extend(p.pinned.iter().copied());
    if new_pts.len() < 2 {
        return false;
    }
    if let Some(l) = new_pin.last_mut() {
        *l = true;
    }
    p.pts = new_pts;
    p.pinned = new_pin;
    true
}

/// Inserts a net's geometry in the index.
pub(crate) fn insert(index: &mut Index, net: u32, g: &mut NetGeom, hw: f64, rv: f64) {
    g.ids.clear();
    for p in &g.paths {
        for w in p.pts.windows(2) {
            let id = index
                .insert(Item::Seg { net, layer: p.layer as u8, a: w[0], b: w[1] }, BoxF::of2(w[0], w[1]).expand(hw));
            g.ids.push(id);
        }
        if p.pts.len() == 1 {
            let id = index.insert(
                Item::Seg { net, layer: p.layer as u8, a: p.pts[0], b: p.pts[0] },
                BoxF::of2(p.pts[0], p.pts[0]).expand(hw),
            );
            g.ids.push(id);
        }
    }
    for (v, _) in &g.vias {
        let id = index.insert(Item::Via { net, at: *v }, BoxF::of2(*v, *v).expand(rv));
        g.ids.push(id);
    }
}

pub(crate) fn remove(index: &mut Index, g: &mut NetGeom) {
    for id in g.ids.drain(..) {
        index.remove(id);
    }
}

/// Moves a section between two vias back to the outer layer when legal, removing both vias.
pub(crate) fn reduce_vias(e: &Engine<'_>, nr: &mut NetRoute, ck: &Checker<'_>) -> usize {
    let mut removed = 0;
    // Grid nodes other wires attach to (cannot move).
    let ends: BTreeSet<u32> = nr.wires.iter().flat_map(|w| [w.nodes[0], *w.nodes.last().expect("node")]).collect();
    let cells = e.cells() as u32;
    for wi in 0..nr.wires.len() {
        let mut changed = true;
        while changed {
            changed = false;
            let nodes = nr.wires[wi].nodes.clone();
            // Via transitions: index i where slot(nodes[i]) != slot(nodes[i+1]).
            let trans: Vec<usize> =
                (0..nodes.len().saturating_sub(1)).filter(|&i| nodes[i] / cells != nodes[i + 1] / cells).collect();
            for pair in trans.windows(2) {
                let (i, k) = (pair[0], pair[1]);
                let (s0, s1, s2) = (nodes[i] / cells, nodes[i + 1] / cells, nodes[k + 1] / cells);
                if s0 != s2 || s1 == s0 {
                    continue;
                }
                let section = &nodes[i + 1..=k];
                let mut attached = section.iter().any(|n| ends.contains(n) && nr.wires.len() > 1);
                // Other wires ending on the via cells (on any layer) keep them.
                for o in [nodes[i], nodes[k + 1]] {
                    let c = o % cells;
                    attached |= nr
                        .wires
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != wi)
                        .any(|(_, w)| [w.nodes[0], *w.nodes.last().expect("node")].iter().any(|n| n % cells == c));
                }
                if attached {
                    continue;
                }
                let layer = e.slots[s0 as usize];
                let moved: Vec<u32> = section.iter().map(|n| s0 * cells + n % cells).collect();
                let legal = moved
                    .windows(2)
                    .all(|w| ck.seg(layer, e.node_pos(w[0]), e.node_pos(w[1]), nr.net).is_none())
                    && (moved.len() > 1 || ck.seg(layer, e.node_pos(moved[0]), e.node_pos(moved[0]), nr.net).is_none());
                if !legal {
                    continue;
                }
                let mut out: Vec<u32> = nodes[..=i].to_vec();
                for n in moved.into_iter().chain(nodes[k + 1..].iter().copied()) {
                    if out.last() != Some(&n) {
                        out.push(n);
                    }
                }
                nr.wires[wi].nodes = out;
                removed += 2;
                changed = true;
                break;
            }
        }
    }
    removed
}

fn collinear(a: P, b: P, c: P) -> bool {
    let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
    let dot = (b.x - a.x) * (c.x - b.x) + (b.y - a.y) * (c.y - b.y);
    cross.abs() < 1.0 && dot > 0.0
}

fn merge(p: &mut Path) {
    let mut i = 1;
    while i + 1 < p.pts.len() {
        if !p.pinned[i] && (collinear(p.pts[i - 1], p.pts[i], p.pts[i + 1]) || p.pts[i] == p.pts[i - 1]) {
            p.pts.remove(i);
            p.pinned.remove(i);
        } else {
            i += 1;
        }
    }
}

/// The two-segment octilinear connections from `a` to `b` (straight then diagonal, diagonal
/// then straight); a single point when one segment suffices.
fn octilinear(a: P, b: P) -> Vec<Vec<P>> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (ax, ay) = (dx.abs(), dy.abs());
    if ax < 1.0 || ay < 1.0 || (ax - ay).abs() < 1.0 {
        return vec![vec![]];
    }
    let m = ax.min(ay);
    let (sx, sy) = (dx.signum(), dy.signum());
    let diag = P::new(sx * m, sy * m);
    let straight = if ax > ay { P::new(dx - sx * m, 0.0) } else { P::new(0.0, dy - sy * m) };
    vec![vec![P::new(a.x + straight.x, a.y + straight.y)], vec![P::new(a.x + diag.x, a.y + diag.y)]]
}

fn len(pts: &[P]) -> f64 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

/// Most vertices a pull-tight or shortcut step replaces at once.
const WINDOW: usize = 24;

/// 45° pull-tight: replaces runs of vertices by a shorter octilinear connection when legal.
fn pull_tight(p: &mut Path, net: u32, ck: &Checker<'_>) -> bool {
    let mut any = false;
    let mut i = 0;
    while i + 2 < p.pts.len() {
        // Furthest j with no pinned vertex strictly between i and j.
        let mut jmax = i + 2;
        while jmax + 1 < p.pts.len() && jmax < i + WINDOW && !p.pinned[jmax - 1] {
            jmax += 1;
        }
        let mut done = false;
        for j in (i + 2..=jmax).rev() {
            if (i + 1..j).any(|k| p.pinned[k]) {
                continue;
            }
            let (a, b) = (p.pts[i], p.pts[j]);
            let cur = len(&p.pts[i..=j]);
            for mid in octilinear(a, b) {
                let mut cand = vec![a];
                cand.extend(mid.iter().copied());
                cand.push(b);
                if len(&cand) >= cur - 1.0 && !(cand.len() < j - i + 1 && len(&cand) <= cur + 1.0) {
                    continue;
                }
                if cand.windows(2).all(|w| ck.seg(p.layer, w[0], w[1], net).is_none()) {
                    let n_mid = mid.len();
                    p.pts.splice(i + 1..j, mid);
                    p.pinned.splice(i + 1..j, std::iter::repeat_n(false, n_mid));
                    any = true;
                    done = true;
                    break;
                }
            }
            if done {
                break;
            }
        }
        if !done {
            i += 1;
        }
    }
    any
}

/// Cuts 90° corners with a 45° segment.
fn chamfer(p: &mut Path, net: u32, ck: &Checker<'_>, g: f64) {
    let mut i = 1;
    while i + 1 < p.pts.len() {
        if p.pinned[i] {
            i += 1;
            continue;
        }
        let (a, b, c) = (p.pts[i - 1], p.pts[i], p.pts[i + 1]);
        let (l1, l2) = (a.dist(b), b.dist(c));
        let dot = (a.x - b.x) * (c.x - b.x) + (a.y - b.y) * (c.y - b.y);
        if l1 < 1.0 || l2 < 1.0 || (dot / (l1 * l2)).abs() > 1e-6 {
            i += 1;
            continue;
        }
        let mut d = (l1.min(l2) / 2.0).min(2.0 * g);
        let mut applied = false;
        while d >= g / 4.0 {
            let b1 = P::new(b.x + (a.x - b.x) / l1 * d, b.y + (a.y - b.y) / l1 * d);
            let b2 = P::new(b.x + (c.x - b.x) / l2 * d, b.y + (c.y - b.y) / l2 * d);
            if ck.seg(p.layer, b1, b2, net).is_none() {
                p.pts.splice(i..=i, [b1, b2]);
                p.pinned.splice(i..=i, [false, false]);
                applied = true;
                break;
            }
            d /= 2.0;
        }
        i += if applied { 2 } else { 1 };
    }
}

/// Any-angle shortcuts: replaces a run of vertices by one straight segment when it is shorter
/// and legal (furthest reachable vertex first).
fn shortcut(p: &mut Path, net: u32, ck: &Checker<'_>) -> bool {
    let mut any = false;
    let mut i = 0;
    while i + 2 < p.pts.len() {
        let mut jmax = i + 2;
        while jmax + 1 < p.pts.len() && jmax < i + WINDOW && !p.pinned[jmax - 1] {
            jmax += 1;
        }
        let mut done = false;
        for j in (i + 2..=jmax).rev() {
            if (i + 1..j).any(|k| p.pinned[k]) {
                continue;
            }
            let (a, b) = (p.pts[i], p.pts[j]);
            if a.dist(b) < len(&p.pts[i..=j]) - 1.0 && ck.seg(p.layer, a, b, net).is_none() {
                p.pts.drain(i + 1..j);
                p.pinned.drain(i + 1..j);
                any = true;
                done = true;
                break;
            }
        }
        if !done {
            i += 1;
        }
    }
    any
}

/// Post-processes a net's polylines in place: collinear merging, 45° pull-tight (`passes`
/// rounds), 90° corners mitered, and with `any_angle` straight shortcuts at any angle.
pub(crate) fn optimize(g: &mut NetGeom, net: u32, ck: &Checker<'_>, grid: f64, passes: usize, any_angle: bool) {
    for p in &mut g.paths {
        merge(p);
        for _ in 0..passes {
            if !pull_tight(p, net, ck) {
                break;
            }
            merge(p);
        }
        chamfer(p, net, ck, grid);
        merge(p);
        if any_angle {
            for _ in 0..passes {
                if !shortcut(p, net, ck) {
                    break;
                }
                merge(p);
            }
        }
    }
}
