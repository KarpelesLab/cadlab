//! Gridless refinement of routed polylines: shortest paths in a visibility graph over the
//! inflated obstacles around each stretch of a path, so tracks are no longer tied to the grid.
//!
//! Every obstacle near a stretch of a path (pads, keep-outs, the board edge, other nets'
//! tracks and vias) is grown into its *hull*: its convex hull widened by an octagon whose
//! inradius is the distance the rules require from the track's centerline (plus a 0.5 µm
//! margin), so a centerline along the hull's boundary keeps exactly the clearance. The hull
//! vertices near the stretch are the graph's nodes. A* (octile or Euclidean heuristic) then
//! searches from the stretch's start to its end; an edge between two nodes is a straight
//! segment (any-angle mode) or one of the two-segment 0°/45°/90° connections, validated
//! lazily with the router's exact clearance check. Nodes that cannot shorten the stretch
//! (outside the ellipse through its ends whose size is its current length, or too far from
//! it) are left out, which keeps the graph small. The stretch is replaced only when the new
//! one is shorter. Classic visibility-graph shortest paths (Lozano-Pérez and Wesley, 1979)
//! restricted to a corridor; nothing here depends on the grid.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::geo::{BoxF, P};
use super::index::{Checker, Item};
use super::model::ObKind;
use super::post::{Path, merge, octilinear};
use super::shove::{MARGIN, hull_shape, octagon_hull};

/// Most vertices of a path refined at once.
const STRETCH: usize = 24;
/// Corridor around a stretch, in grid pitches.
const CORRIDOR: f64 = 4.0;
/// Most graph nodes per stretch (closest to the stretch first).
const MAX_NODES: usize = 240;
/// Least saving (nm) for a stretch to be replaced.
const MIN_GAIN: f64 = 1_000.0;

fn octile(a: P, b: P) -> f64 {
    let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
    let (hi, lo) = if dx > dy { (dx, dy) } else { (dy, dx) };
    hi + (std::f64::consts::SQRT_2 - 1.0) * lo
}

fn length(pts: &[P]) -> f64 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

fn dist_to_polyline(q: P, pts: &[P]) -> f64 {
    pts.windows(2).map(|w| super::geo::point_seg_d2(q, w[0], w[1])).fold(f64::MAX, f64::min).sqrt()
}

/// Refines every stretch of `p` (a path of `net`) between pinned vertices; returns whether
/// anything changed.
pub(crate) fn refine(p: &mut Path, net: u32, ck: &Checker<'_>, grid: f64, any_angle: bool) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i + 1 < p.pts.len() {
        // Stretch i..=j: up to STRETCH vertices, ending at the next pinned vertex.
        let mut j = i + 1;
        while j + 1 < p.pts.len() && j - i < STRETCH && !p.pinned[j] {
            j += 1;
        }
        if j - i >= 2
            && let Some(mid) = stretch(&p.pts[i..=j], p.layer, net, ck, grid, any_angle)
        {
            let n = mid.len();
            p.pts.splice(i + 1..j, mid);
            p.pinned.splice(i + 1..j, std::iter::repeat_n(false, n));
            changed = true;
            merge(p);
            // Continue from the end of the replaced stretch.
            i = (i + n + 1).min(p.pts.len() - 1);
            continue;
        }
        i = if p.pinned[j] || j == i + 1 { j } else { j - 1 };
    }
    changed
}

/// A shorter replacement for the inner vertices of `pts` (its ends stay), if any.
fn stretch(pts: &[P], layer: usize, net: u32, ck: &Checker<'_>, grid: f64, any_angle: bool) -> Option<Vec<P>> {
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let cur = length(pts);
    let metric = |u: P, v: P| if any_angle { u.dist(v) } else { octile(u, v) };
    if cur - metric(a, b) < MIN_GAIN {
        return None;
    }
    let rb = ck.rb;
    let pr = rb.profile(net);
    let corridor = CORRIDOR * grid;
    let mut area = BoxF::EMPTY;
    for q in pts {
        area.add(*q);
    }
    let area = area.expand(corridor);
    let bit = 1u64 << layer.min(63);
    // Hull vertices of everything near the stretch.
    let mut cand: Vec<(f64, P)> = Vec::new();
    let mut push = |hull: Vec<P>| {
        for v in hull {
            if !area.intersects(&BoxF::of2(v, v)) {
                continue;
            }
            // Could only help if a path through it is shorter than the stretch.
            if metric(a, v) + metric(v, b) >= cur - MIN_GAIN {
                continue;
            }
            let d = dist_to_polyline(v, pts);
            if d <= corridor {
                cand.push((d, v));
            }
        }
    };
    for (_, it) in ck.index.query(&area.expand(pr.hw + ck.index.reach)) {
        match it {
            Item::Static(o) => {
                let ob = &rb.obstacles[o as usize];
                if ob.tracks & bit == 0 || (ob.net == Some(net) && matches!(ob.kind, ObKind::Pad | ObKind::Copper)) {
                    continue;
                }
                push(hull_shape(&ob.shape, pr.hw + ob.clear.with(pr.c) + MARGIN));
            }
            Item::Seg { net: m, layer: l, a: s, b: e } => {
                if m == net || l as usize != layer {
                    continue;
                }
                let po = rb.profile(m);
                push(octagon_hull(s, e, pr.hw + po.hw + pr.c.max(po.c) + MARGIN));
            }
            Item::Via { net: m, at } => {
                if m == net {
                    continue;
                }
                let po = rb.profile(m);
                push(octagon_hull(at, at, pr.hw + po.rv + pr.c.max(po.c) + MARGIN));
            }
        }
    }
    cand.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.x.total_cmp(&y.1.x)).then(x.1.y.total_cmp(&y.1.y)));
    cand.dedup_by(|x, y| x.1.dist(y.1) < 1.0);
    cand.truncate(MAX_NODES);
    let mut nodes: Vec<P> = vec![a, b];
    nodes.extend(cand.into_iter().map(|c| c.1).filter(|v| rb.inside(*v)));
    let free = |u: P, v: P| ck.seg(layer, u, v, net).is_none();
    // Legal connection from u to v: its inner points (empty for one segment).
    let edge = |u: P, v: P| -> Option<Vec<P>> {
        if any_angle {
            return free(u, v).then(Vec::new);
        }
        octilinear(u, v).into_iter().find(|mid| {
            let mut q = vec![u];
            q.extend(mid.iter().copied());
            q.push(v);
            q.windows(2).all(|s| free(s[0], s[1]))
        })
    };
    // A* with lazily validated edges.
    let n = nodes.len();
    let mut g = vec![f64::MAX; n];
    let mut parent: Vec<Option<(usize, Vec<P>)>> = vec![None; n];
    let mut closed = vec![false; n];
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::new();
    let bound = cur - MIN_GAIN;
    g[0] = 0.0;
    heap.push(Reverse((metric(a, b).to_bits(), 0)));
    let mut checks = 0usize;
    while let Some(Reverse((_, u))) = heap.pop() {
        if closed[u] {
            continue;
        }
        closed[u] = true;
        if u == 1 {
            break;
        }
        for v in 1..n {
            if closed[v] || v == u {
                continue;
            }
            let est = g[u] + metric(nodes[u], nodes[v]);
            if est >= g[v] || est + metric(nodes[v], b) >= bound {
                continue;
            }
            checks += 1;
            if checks > 4 * MAX_NODES * 8 {
                return None;
            }
            if let Some(mid) = edge(nodes[u], nodes[v]) {
                g[v] = est;
                parent[v] = Some((u, mid));
                heap.push(Reverse(((est + metric(nodes[v], b)).to_bits(), v)));
            }
        }
    }
    if g[1] >= bound {
        return None;
    }
    // Inner points from a to b.
    let mut out: Vec<P> = Vec::new();
    let mut v = 1;
    while let Some((u, mid)) = parent[v].clone() {
        for q in mid.into_iter().rev() {
            out.push(q);
        }
        if u != 0 {
            out.push(nodes[u]);
        }
        v = u;
        if v == 0 {
            break;
        }
    }
    out.reverse();
    Some(out)
}
