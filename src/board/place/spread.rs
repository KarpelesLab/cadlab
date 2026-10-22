//! Spread placement, last steps before local improvement: the groups (an anchor with its
//! passives and satellites) are moved, as rigid blocks, so that together they use the board's
//! free area instead of the region the constructive placement gathered them in (connectivity
//! pulls every group towards the first ones placed, which leaves a side of a larger board
//! empty).
//!
//! 1. **Stretch** ([`stretch_groups`]): the central IC (when it dominates: half as many pins
//!    again as any other IC) stays where it was placed (the center);
//!    where the box around all groups leaves an empty band
//!    along a side of the free area wider than an eighth of the board (at the default density;
//!    wider at lower densities), the box is stretched to narrow the band to that, and each
//!    group keeps its relative place in the slack along each axis: groups that were apart stay
//!    apart, so the arrangement, and with it the ratsnest's topology, stays the same. The
//!    caller tries the full stretch, then three quarters, half, a quarter and none, and keeps
//!    the first whose ratsnest (after the relaxation) stays within its budget.
//! 2. **Relax** ([`relax_groups`], D67): groups move by 1 to 8 mm in eight directions, to the
//!    mirror image of their place across the central IC, or jump to any window's center or to
//!    where their connections pull them ([`pull`]; connectors only along their edge, and a lone
//!    connector may turn), where the ratsnest (with tethers, connector crowding and connectors
//!    lying across their edge) plus a weight times the board's crowding ([`Windows`]) drops,
//!    as long as the total ratsnest stays within the budget (a move that shortens it is always
//!    allowed): groups leave crowded parts of the board for the places their connections
//!    prefer (a channel's op-amp goes to its connector), and connectors fill empty stretches of
//!    their edge. Every move is checked against the budget, so a board where the full
//!    relaxation would overshoot keeps the moves that fit instead of losing them all (D55 undid
//!    every group move on such boards). The central IC's group and the groups holding ends of
//!    differential pairs stay (the arrangement of a pair's ends, an ESD array between the
//!    receptacle and the MCU, stays the one the constructive placement chose).
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

/// The density measure: windows over the board, each with the courtyard area it would hold
/// if the parts were spread evenly over the free area (its share). The crowding of a placement
/// is the sum over windows of the squared excess of the courtyard area they hold over their
/// share divided by the density (at 60 %, 1.67 times the share: the parts must spread over at
/// least that much of the board), relative to what a fully free window would hold. For groups
/// bound to an edge, the shortfall below the share counts too (`Windows::cost`).
pub(super) struct Windows {
    /// Window box, what it would hold if all free, its share.
    cells: Vec<(Rect, f64, f64)>,
    /// Windows of the first grid (the first ones in `cells`).
    first: usize,
    /// Most a window may hold before it is crowded, relative to its share (1 / density).
    cap: f64,
}

impl Windows {
    /// Windows over the top side's free area: two grids of about `n` windows, the second
    /// offset by half a window.
    pub(super) fn new(e: &Engine, parts_area: f64, n: f64, density: f64) -> Windows {
        use polyclip::{FillRule, Op};
        let b = e.board;
        let free = &e.free[0];
        let free_area = polyclip::area2(free) as f64 / 2.0;
        let mut cells = Vec::new();
        if free_area <= 0.0 {
            return Windows { cells, first: 0, cap: 1.0 };
        }
        let rho = parts_area / free_area;
        let cap = 1.0 / density.clamp(0.05, 1.0);
        let s = ((b.w() as f64 * b.h() as f64) / n).sqrt();
        let nx = ((b.w() as f64 / s).round() as i64).max(2);
        let ny = ((b.h() as f64 / s).round() as i64).max(2);
        let (cw, ch) = (b.w() / nx, b.h() / ny);
        let add = |cells: &mut Vec<(Rect, f64, f64)>, r: Rect| {
            let ring = vec![polyclip::Polygon::new(r.ring(), vec![])];
            let inside = polyclip::boolean(Op::Intersection, free, &ring, FillRule::NonZero)
                .map_or(0.0, |s| polyclip::area2(&s) as f64 / 2.0);
            let area = r.w() as f64 * r.h() as f64;
            if inside > 0.0 && area > 0.0 {
                cells.push((r, rho * area, rho * inside));
            }
        };
        let mut first = 0;
        for (k, (ox, oy, kx, ky)) in [(0, 0, nx, ny), (cw / 2, ch / 2, nx - 1, ny - 1)].into_iter().enumerate() {
            if k == 1 {
                first = cells.len();
            }
            for j in 0..ky {
                for i in 0..kx {
                    let x0 = b.x0 + ox + i * cw;
                    let y0 = b.y0 + oy + j * ch;
                    add(&mut cells, Rect { x0, y0, x1: x0 + cw, y1: y0 + ch });
                }
            }
        }
        Windows { cells, first, cap }
    }

    /// Centers of the first grid's windows.
    pub(super) fn centers(&self) -> Vec<Point> {
        self.cells.iter().take(self.first).map(|(r, _, _)| r.center()).collect()
    }

    /// Crowding of the current placement; with `under`, plus the windows' shortfall below
    /// their share (squared, relative to a free window, like the excess). Only connectors and
    /// parts with an edge hint are moved by the shortfall: where they sit along their edge is
    /// free, and filling an empty stretch of edge brings their groups' connections along,
    /// while pulling a group away from its connections into an empty window only lengthens them.
    pub(super) fn cost(&self, e: &Engine, under: bool) -> f64 {
        let gamma = if under { 1.0 } else { 0.0 };
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
                let over = (occ - want * self.cap).max(0.0) / full;
                let under = (want - occ).max(0.0) / full;
                over * over + gamma * under * under
            })
            .sum()
    }
}

/// Moves groups rigidly (`passes` rounds at most) where the ratsnest plus `weight` × the
/// crowding ([`Windows::cost`]) drops, keeping the total ratsnest within `budget` (a move may
/// exceed it only when it shortens the ratsnest). Moves: shifts of 1 to 8 mm in eight
/// directions, the mirror images across the central IC, and jumps of the group's center to each
/// window's center and to where its connections pull it ([`pull`]), along its edge for a group
/// bound to one; a lone part bound to an edge may also turn. Anchors' rooms move with their
/// groups.
#[allow(clippy::too_many_arguments)]
pub(super) fn relax_groups(
    e: &mut Engine,
    groups: &[Vec<usize>],
    pinned: &[bool],
    pivot: Option<usize>,
    win: &Windows,
    weight: f64,
    budget: f64,
    passes: usize,
) {
    const EPS: f64 = 1_000.0;
    let groups_all = groups;
    let steps = [8_000_000i64, 4_000_000, 2_000_000, 1_000_000];
    let dirs = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];
    let mut total = e.total();
    for _ in 0..passes {
        let mut moved = false;
        for (g, _) in groups.iter().zip(pinned).filter(|(_, p)| !**p) {
            let poses: Vec<(usize, Pose)> = g.iter().filter_map(|&i| e.poses[i].map(|q| (i, q))).collect();
            let Some(gb) = group_box(e, g) else { continue };
            let mut nets: Vec<usize> = g.iter().flat_map(|&i| e.part_nets[i].iter().copied()).collect();
            nets.sort_unstable();
            nets.dedup();
            // A group bound to an edge jumps along it only.
            let bound = g.iter().find_map(|&i| {
                let (q, edge) = (e.poses[i]?, e.parts[i].edge_target()?);
                Some(e.nearest_side(&e.parts[i].rect(&q), edge).1)
            });
            // (cost, ratsnest of the group's nets). Connectors pay for lying across their edge.
            let cost = |e: &Engine| -> (f64, f64) {
                let crowd: f64 = g.iter().filter_map(|&i| e.poses[i].map(|q| e.crowding(i, &q) + e.lie(i, &q))).sum();
                let r = e.nets_cost(&nets);
                (e.local_cost_with(g, r) + crowd + weight * win.cost(e, bound.is_some()), r)
            };
            let (before, r0) = cost(e);
            let zones = e.pair_zones.clone();
            // (cost, ratsnest of the group's nets, candidate).
            let mut best: Option<(f64, f64, usize)> = None;
            // Shifts, the group mirrored across the central IC (to the other side of it), then
            // jumps to the windows' centers.
            let mut offsets: Vec<(i64, i64)> =
                steps.iter().flat_map(|&d| dirs.iter().map(move |&(dx, dy)| (dx * d, dy * d))).collect();
            let c = gb.center();
            if let Some(pc) = pivot.and_then(|k| group_box(e, &groups_all[k])) {
                let pc = pc.center();
                let (mx, my) = (2 * (pc.x.0 - c.x.0), 2 * (pc.y.0 - c.y.0));
                offsets.extend([(mx, 0), (0, my), (mx, my)].map(|(x, y)| (super::snap(x), super::snap(y))));
            }
            // Window centers, then where the group's connections pull it: the mean of the pins
            // outside the group on its signal nets (and a ring of 4 mm around it, in case
            // another part sits right there).
            let mut targets = win.centers();
            if let Some(t) = pull(e, g, &nets) {
                let r = 4_000_000;
                for (kx, ky) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                    targets.push(Point::new(Nm(t.x.0 + kx * r), Nm(t.y.0 + ky * r)));
                }
            }
            for w in targets {
                let (dx, dy) = (super::snap(w.x.0 - c.x.0), super::snap(w.y.0 - c.y.0));
                let off = match bound {
                    None => (dx, dy),
                    Some(EdgeHint::Left | EdgeHint::Right) => (0, dy),
                    Some(_) => (dx, 0),
                };
                if off != (0, 0) && !offsets.contains(&off) {
                    offsets.push(off);
                }
            }
            // A lone part bound to an edge (a connector) may also turn, seated back at the same
            // distance from its side: a header lying across its edge turns along it.
            let turns: Vec<Pose> = match (&poses[..], bound) {
                ([(i, q)], Some(side)) => {
                    let (i, q) = (*i, *q);
                    let r = e.parts[i].rect(&q);
                    let b = e.board;
                    // Signed distance from the side (negative beyond it).
                    let from_side = |r: &Rect| match side {
                        EdgeHint::Left => r.x0 - b.x0,
                        EdgeHint::Right => b.x1 - r.x1,
                        EdgeHint::Bottom => r.y0 - b.y0,
                        EdgeHint::Top | EdgeHint::Any => b.y1 - r.y1,
                    };
                    let d0 = from_side(&r);
                    (1..4u8)
                        .map(|k| {
                            let rot = (q.rot + k) % 4;
                            let probe = Pose { rot, ..q };
                            let rr = e.parts[i].rect(&probe);
                            // Same center, then back to the same distance from the side.
                            let (cx, cy) = (r.center().x.0 - rr.center().x.0, r.center().y.0 - rr.center().y.0);
                            let moved =
                                Pose { at: probe.at + Point::new(Nm(super::snap(cx)), Nm(super::snap(cy))), ..probe };
                            // Rounded towards the side, so the part is no farther from it.
                            let gap = from_side(&e.parts[i].rect(&moved)) - d0;
                            let s = gap.div_euclid(super::GRID) * super::GRID
                                + if gap.rem_euclid(super::GRID) == 0 { 0 } else { super::GRID };
                            let seat = match side {
                                EdgeHint::Left => (-s, 0),
                                EdgeHint::Right => (s, 0),
                                EdgeHint::Bottom => (0, -s),
                                EdgeHint::Top | EdgeHint::Any => (0, s),
                            };
                            Pose { at: moved.at + Point::new(Nm(seat.0), Nm(seat.1)), ..moved }
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            let mut cands: Vec<Vec<(usize, Pose)>> = Vec::new();
            for &(ox, oy) in &offsets {
                let off = Point::new(Nm(ox), Nm(oy));
                cands.push(poses.iter().map(|&(i, q)| (i, Pose { at: q.at + off, ..q })).collect());
            }
            for t in &turns {
                cands.push(vec![(poses[0].0, *t)]);
                for &(ox, oy) in &offsets {
                    cands.push(vec![(poses[0].0, Pose { at: t.at + Point::new(Nm(ox), Nm(oy)), ..*t })]);
                }
            }
            for (k, cand) in cands.iter().enumerate() {
                for &(i, m) in cand {
                    e.poses[i] = Some(m);
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
                if ok {
                    let (after, r1) = cost(e);
                    let within = total - r0 + r1 <= budget || r1 < r0;
                    if within && after < before - EPS && best.is_none_or(|b| after < b.0) {
                        best = Some((after, r1, k));
                    }
                }
                for &(i, q) in &poses {
                    e.poses[i] = Some(q);
                }
                e.pair_zones = zones.clone();
            }
            if let Some((_, r1, k)) = best {
                for &(i, m) in &cands[k] {
                    let q = e.poses[i].expect("placed");
                    if let Some(room) = e.rooms[i] {
                        // The room keeps its margin around the part.
                        let r = e.parts[i].rect(&q);
                        e.rooms[i] = Some(e.parts[i].rect(&m).expand(r.x0 - room.x0));
                    }
                    e.poses[i] = Some(m);
                }
                e.pair_zones = super::pair_zones(e);
                total += r1 - r0;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// Where a group's connections pull it: the mean of the placed pins outside the group on its
/// signal nets (neither ground nor power: nets of more than eight pins count as rails), each net
/// weighted equally.
fn pull(e: &Engine, g: &[usize], nets: &[usize]) -> Option<Point> {
    let (mut sx, mut sy, mut w) = (0.0, 0.0, 0.0);
    for &n in nets {
        if e.is_ground(n) || e.net_pins[n].len() > 8 {
            continue;
        }
        let outside: Vec<Point> = e.net_pins[n]
            .iter()
            .filter(|pin| !matches!(pin, super::Pin::Part(j, _) if g.contains(j)))
            .filter_map(|&pin| e.pin_pos(pin))
            .collect();
        if outside.is_empty() {
            continue;
        }
        let k = outside.len() as f64;
        sx += outside.iter().map(|q| q.x.0 as f64).sum::<f64>() / k;
        sy += outside.iter().map(|q| q.y.0 as f64).sum::<f64>() / k;
        w += 1.0;
    }
    (w > 0.0).then(|| Point::new(Nm((sx / w).round() as i64), Nm((sy / w).round() as i64)))
}

/// Whether part `i` is an end of a differential pair (pads of both nets): such groups keep the
/// arrangement the constructive placement gave them in the group moves.
pub(super) fn pair_end(e: &Engine, i: usize) -> bool {
    let nets: Vec<&str> = e.parts[i].pads.iter().filter_map(|(_, n)| n.map(|n| e.net_names[n].as_str())).collect();
    e.p.circuit().diffpairs.values().any(|d| nets.contains(&d.p.as_str()) && nets.contains(&d.n.as_str()))
}
