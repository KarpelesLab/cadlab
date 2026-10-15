//! Spread placement, last steps before local improvement: the groups (an anchor with its
//! passives and satellites) are moved, as rigid blocks, so that together they use the board's
//! free area instead of the region the constructive placement gathered them in (connectivity
//! pulls every group towards the first ones placed, which leaves a side of a larger board
//! empty).
//!
//! 1. **Stretch** ([`stretch_groups`]): the central IC stays where it was placed (the center);
//!    where the box around all groups leaves an empty band
//!    along a side of the free area wider than an eighth of the board (at the default density;
//!    wider at lower densities), the box is stretched to narrow the band to that, and each
//!    group keeps its relative place in the slack along each axis: groups that were apart stay
//!    apart, so the arrangement, and with it the ratsnest's topology, stays the same. The
//!    caller tries the full stretch, then three quarters, half, a quarter and none, and keeps
//!    the first whose ratsnest (after the relaxation) stays within its budget.
//! 2. **Relax** ([`relax_groups`]): groups move by 1 to 8 mm in eight directions, or to the
//!    mirror image of their place across the central IC (left to right, top to bottom), where
//!    the ratsnest (with tethers and connector crowding) plus a weight times the board's
//!    unevenness ([`Windows`]) drops: a side of the board the constructive placement left
//!    empty gets the groups that cost least to move there, and a stretch that overshot comes
//!    back towards its connections. The central IC's group and the groups holding ends of
//!    differential pairs stay (the pair's routing depends on where its ends are relative to
//!    each other).
//!
//! Groups at an edge (a connector, a part with an edge hint) stay at it, facing it no worse. A
//! move that does not fit (another part, the outline, a keep-out, a pair's breakout) is
//! shortened or not made. Deterministic: fixed move orders, ties to the first.

use super::{BoardSide, EdgeHint, Engine, Point, Pose, Rect};
use crate::units::Nm;

/// Bounding box of rectangles.
fn union(rs: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    rs.into_iter().reduce(|a, b| Rect {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    })
}

/// The box around a group's parts on the top side.
fn group_box(e: &Engine, g: &[usize]) -> Option<Rect> {
    union(g.iter().filter_map(|&i| e.poses[i].filter(|q| q.side == BoardSide::Top).map(|q| e.parts[i].rect(&q))))
}

/// Stretches the arrangement of `groups` (part indices) where it leaves an empty band along a
/// side of the free area's box wider than `margin` (a share of the box's width or height):
/// the band is narrowed to that (`fill` 1), or part of the way (`fill` below 1). With a
/// `pivot` group (the central IC), the groups on each side of it along an axis are stretched
/// away from it on their side. `pinned` groups stay. Anchors' rooms move with their groups.
pub(super) fn stretch_groups(
    e: &mut Engine,
    groups: &[Vec<usize>],
    pinned: &[bool],
    pivot: Option<usize>,
    margin: f64,
    fill: f64,
) {
    let boxes: Vec<Option<Rect>> = groups.iter().map(|g| group_box(e, g)).collect();
    let (Some(all), Some(free)) = (union(boxes.iter().flatten().copied()), polyclip::Geometry::bbox(&e.free[0])) else {
        return;
    };
    let free = Rect { x0: free.min.x, y0: free.min.y, x1: free.max.x, y1: free.max.y };
    let (mx, my) = ((free.w() as f64 * margin) as i64, (free.h() as f64 * margin) as i64);
    let part = |a: i64, b: i64| a + ((b - a) as f64 * fill.clamp(0.0, 1.0)).round() as i64;
    let target = Rect {
        x0: part(all.x0, all.x0.min(free.x0 + mx)),
        y0: part(all.y0, all.y0.min(free.y0 + my)),
        x1: part(all.x1, all.x1.max(free.x1 - mx)),
        y1: part(all.y1, all.y1.max(free.y1 - my)),
    };
    if target == all {
        return;
    }
    // A group keeps its relative place in the slack the arrangement leaves it along each axis:
    // `(x0 − all.x0) / (all.w − w)` becomes `(x0' − target.x0) / (target.w − w)`. Groups apart
    // along an axis stay apart (the share grows with the position), those at the arrangement's
    // edges go to the target's.
    let place = |a0: i64, a1: i64, t0: i64, t1: i64, x0: i64, w: i64| -> i64 {
        let slack = (a1 - a0 - w) as f64;
        let u = if slack > 0.0 { (x0 - a0) as f64 / slack } else { 0.5 };
        t0 + (u * (t1 - t0 - w) as f64).round() as i64
    };
    // Along one axis: the whole arrangement, or with a pivot the side of it the group is on
    // (groups level with the pivot stay).
    let shift = |a: (i64, i64), t: (i64, i64), p: Option<(i64, i64)>, x0: i64, w: i64| -> i64 {
        match p {
            None => place(a.0, a.1, t.0, t.1, x0, w) - x0,
            Some((p0, _)) if x0 + w <= p0 => place(a.0, p0, t.0, p0, x0, w) - x0,
            Some((_, p1)) if x0 >= p1 => place(p1, a.1, p1, t.1, x0, w) - x0,
            Some(_) => 0,
        }
    };
    let pb = pivot.and_then(|k| boxes.get(k).copied().flatten());
    // Each group's move, then kept at the edges its edge-bound parts are at.
    let mut moves: Vec<(usize, i64, i64)> = Vec::new();
    for (k, g) in groups.iter().enumerate() {
        let Some(b) = boxes[k] else { continue };
        if Some(k) == pivot || pinned.get(k).copied().unwrap_or(false) {
            continue;
        }
        let mut dx = shift((all.x0, all.x1), (target.x0, target.x1), pb.map(|p| (p.x0, p.x1)), b.x0, b.w());
        let mut dy = shift((all.y0, all.y1), (target.y0, target.y1), pb.map(|p| (p.y0, p.y1)), b.y0, b.h());
        for &i in g {
            let (Some(q), Some(edge)) = (e.poses[i], e.parts[i].edge_target()) else { continue };
            match e.nearest_side(&e.parts[i].rect(&q), edge).1 {
                EdgeHint::Left => dx = dx.min(0),
                EdgeHint::Right => dx = dx.max(0),
                EdgeHint::Bottom => dy = dy.min(0),
                EdgeHint::Top | EdgeHint::Any => dy = dy.max(0),
            }
        }
        if dx != 0 || dy != 0 {
            moves.push((k, dx, dy));
        }
    }
    let len = |m: &(usize, i64, i64)| (m.1 as f64).hypot(m.2 as f64);
    moves.sort_by(|a, b| len(b).total_cmp(&len(a)).then(a.0.cmp(&b.0)));
    for (k, dx, dy) in moves {
        let poses: Vec<(usize, Pose)> = groups[k].iter().filter_map(|&i| e.poses[i].map(|q| (i, q))).collect();
        let zones = e.pair_zones.clone();
        for t in [4i64, 3, 2, 1] {
            let off = Point::new(Nm(super::snap(dx * t / 4)), Nm(super::snap(dy * t / 4)));
            for &(i, q) in &poses {
                e.poses[i] = Some(Pose { at: q.at + off, ..q });
            }
            e.pair_zones = super::pair_zones(e);
            let ok = poses.iter().all(|&(i, q)| {
                let m = e.poses[i].expect("moved");
                e.valid(i, &m)
                    && e.parts[i].edge_target().is_none_or(|edge| {
                        e.edge_distance(&e.parts[i].rect(&m), edge).0 <= e.edge_distance(&e.parts[i].rect(&q), edge).0
                            && e.facing(i, &m, edge) <= e.facing(i, &q, edge)
                    })
            });
            if ok {
                for &(i, _) in &poses {
                    if let Some(r) = e.rooms[i] {
                        e.rooms[i] = Some(r.shift(off));
                    }
                }
                break;
            }
            for &(i, q) in &poses {
                e.poses[i] = Some(q);
            }
            e.pair_zones = zones.clone();
        }
    }
}

/// The evenness measure: windows over the board, each with the courtyard area it would hold
/// if the parts were spread evenly over the free area. The unevenness of a placement is the
/// sum over windows of the squared difference between the courtyard area they hold and that,
/// relative to what a fully free window would hold.
pub(super) struct Windows {
    /// Window box, what it would hold if all free, what it should hold.
    cells: Vec<(Rect, f64, f64)>,
}

impl Windows {
    /// Windows over the top side's free area: two grids of about `n` windows, the second
    /// offset by half a window.
    pub(super) fn new(e: &Engine, parts_area: f64, n: f64) -> Windows {
        use polyclip::{FillRule, Op};
        let b = e.board;
        let free = &e.free[0];
        let free_area = polyclip::area2(free) as f64 / 2.0;
        let mut cells = Vec::new();
        if free_area <= 0.0 {
            return Windows { cells };
        }
        let rho = parts_area / free_area;
        let s = ((b.w() as f64 * b.h() as f64) / n).sqrt();
        let nx = ((b.w() as f64 / s).round() as i64).max(2);
        let ny = ((b.h() as f64 / s).round() as i64).max(2);
        let (cw, ch) = (b.w() / nx, b.h() / ny);
        let mut add = |r: Rect| {
            let ring = vec![polyclip::Polygon::new(r.ring(), vec![])];
            let inside = polyclip::boolean(Op::Intersection, free, &ring, FillRule::NonZero)
                .map_or(0.0, |s| polyclip::area2(&s) as f64 / 2.0);
            let area = r.w() as f64 * r.h() as f64;
            if inside > 0.0 && area > 0.0 {
                cells.push((r, rho * area, rho * inside));
            }
        };
        for (ox, oy, kx, ky) in [(0, 0, nx, ny), (cw / 2, ch / 2, nx - 1, ny - 1)] {
            for j in 0..ky {
                for i in 0..kx {
                    let x0 = b.x0 + ox + i * cw;
                    let y0 = b.y0 + oy + j * ch;
                    add(Rect { x0, y0, x1: x0 + cw, y1: y0 + ch });
                }
            }
        }
        Windows { cells }
    }

    /// Unevenness of the current placement.
    pub(super) fn cost(&self, e: &Engine) -> f64 {
        let rects: Vec<Rect> = e
            .poses
            .iter()
            .enumerate()
            .filter_map(|(i, q)| q.filter(|q| q.side == BoardSide::Top).map(|q| e.parts[i].rect(&q)))
            .collect();
        self.cells
            .iter()
            .map(|(c, full, want)| {
                let occ: f64 = rects
                    .iter()
                    .map(|r| {
                        let w = (r.x1.min(c.x1) - r.x0.max(c.x0)).max(0) as f64;
                        let h = (r.y1.min(c.y1) - r.y0.max(c.y0)).max(0) as f64;
                        w * h
                    })
                    .sum();
                ((occ - want) / full).powi(2)
            })
            .sum()
    }
}

/// Moves groups rigidly (`passes` rounds at most) where the ratsnest plus `weight` × the
/// unevenness drops. Anchors' rooms move with their groups.
pub(super) fn relax_groups(
    e: &mut Engine,
    groups: &[Vec<usize>],
    pinned: &[bool],
    pivot: Option<usize>,
    win: &Windows,
    weight: f64,
    passes: usize,
) {
    const EPS: f64 = 1_000.0;
    let groups_all = groups;
    let steps = [8_000_000i64, 4_000_000, 2_000_000, 1_000_000];
    let dirs = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];
    for _ in 0..passes {
        let mut moved = false;
        for (g, _) in groups.iter().zip(pinned).filter(|(_, p)| !**p) {
            let poses: Vec<(usize, Pose)> = g.iter().filter_map(|&i| e.poses[i].map(|q| (i, q))).collect();
            if poses.is_empty() {
                continue;
            }
            let mut nets: Vec<usize> = g.iter().flat_map(|&i| e.part_nets[i].iter().copied()).collect();
            nets.sort_unstable();
            nets.dedup();
            let cost = |e: &Engine| -> f64 {
                let crowd: f64 = g.iter().filter_map(|&i| e.poses[i].map(|q| e.crowding(i, &q))).sum();
                e.local_cost(g, &nets) + crowd + weight * win.cost(e)
            };
            let before = cost(e);
            let zones = e.pair_zones.clone();
            let mut best: Option<(f64, (i64, i64))> = None;
            // Shifts, then the group mirrored across the central IC (to the other side of it).
            let mut offsets: Vec<(i64, i64)> =
                steps.iter().flat_map(|&d| dirs.iter().map(move |&(dx, dy)| (dx * d, dy * d))).collect();
            if let (Some(pc), Some(c)) = (pivot.and_then(|k| group_box(e, &groups_all[k])), group_box(e, g)) {
                let (pc, c) = (pc.center(), c.center());
                let (mx, my) = (2 * (pc.x.0 - c.x.0), 2 * (pc.y.0 - c.y.0));
                offsets.extend([(mx, 0), (0, my), (mx, my)].map(|(x, y)| (super::snap(x), super::snap(y))));
            }
            for (ox, oy) in offsets {
                {
                    let off = Point::new(Nm(ox), Nm(oy));
                    for &(i, q) in &poses {
                        e.poses[i] = Some(Pose { at: q.at + off, ..q });
                    }
                    e.pair_zones = super::pair_zones(e);
                    let ok = poses.iter().all(|&(i, q)| {
                        let m = e.poses[i].expect("moved");
                        e.valid(i, &m)
                            && e.parts[i].edge_target().is_none_or(|edge| {
                                e.edge_distance(&e.parts[i].rect(&m), edge).0
                                    <= e.edge_distance(&e.parts[i].rect(&q), edge).0
                                    && e.facing(i, &m, edge) <= e.facing(i, &q, edge)
                            })
                    });
                    let after = if ok { cost(e) } else { f64::INFINITY };
                    if after < before - EPS && best.is_none_or(|b| after < b.0) {
                        best = Some((after, (ox, oy)));
                    }
                    for &(i, q) in &poses {
                        e.poses[i] = Some(q);
                    }
                    e.pair_zones = zones.clone();
                }
            }
            if let Some((_, (ox, oy))) = best {
                let off = Point::new(Nm(ox), Nm(oy));
                for &(i, q) in &poses {
                    e.poses[i] = Some(Pose { at: q.at + off, ..q });
                    if let Some(r) = e.rooms[i] {
                        e.rooms[i] = Some(r.shift(off));
                    }
                }
                e.pair_zones = super::pair_zones(e);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// Whether part `i` is an end of a differential pair (pads of both nets): such groups keep the
/// relative places the constructive placement gave them, which the pair's routing depends on.
pub(super) fn pair_end(e: &Engine, i: usize) -> bool {
    let nets: Vec<&str> = e.parts[i].pads.iter().filter_map(|(_, n)| n.map(|n| e.net_names[n].as_str())).collect();
    e.p.circuit().diffpairs.values().any(|d| nets.contains(&d.p.as_str()) && nets.contains(&d.n.as_str()))
}
