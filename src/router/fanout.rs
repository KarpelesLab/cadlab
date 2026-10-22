//! Fanout and escape before routing: dog bones for the inner balls of area-array (BGA)
//! footprints, and escape stubs for fine-pitch pads that sit off the routing grid.
//!
//! A footprint whose pads fill most of a regular lattice (at least 4 × 4) is treated as an area
//! array. The outer ring of balls (two rings when a track fits between neighboring pads) is
//! left to the router, which escapes it on the pad layer between the outer balls. Every other
//! ball of a net with something to route gets a *dog bone*: a short track from the ball to a
//! through via at the center of the four surrounding balls, on the diagonal pointing away from
//! the array center (quadrant fanout), so the vias of a quadrant line up and leave routing
//! channels between them on the inner layers. When that diagonal is not legal the other three
//! are tried. Vias never go in pads. Every via and stub is checked exactly against everything
//! placed so far (pads, existing copper, earlier fanouts, holes, keep-outs, the board edge),
//! so the fanout is DRC-clean by construction; the router then treats it as existing copper of
//! the ball's net.
//!
//! Fine-pitch escapes ([`escape`]) are straight tracks out of a pad row, then 45° onto a grid
//! cell, for components whose pads sit off the routing grid.

use std::collections::BTreeMap;

use crate::units::Nm;

use super::engine::NetRoute;
use super::geo::{BoxF, P, point_seg_d2};
use super::grid::{Grid, delta};
use super::index::{Checker, Index, Item, TOL};
use super::model::RouterBoard;
use super::scoped::St;

/// A planned dog bone: stub on `layer` from the pad center to the via.
#[derive(Clone, Debug)]
pub(crate) struct DogBone {
    pub net: u32,
    /// Stackup layer of the pad.
    pub layer: usize,
    pub pad: P,
    pub via: P,
    /// Pad label (`U1.C3`).
    pub label: String,
    /// A via smaller than the net's (diameter, drill): the smallest the rules allow, where the
    /// net's via does not fit between the balls.
    pub small: Option<(Nm, Nm)>,
}

/// Lattice of an area-array footprint.
struct Array {
    /// Terminal indices with lattice coordinates.
    pads: Vec<(usize, i64, i64)>,
    nx: i64,
    ny: i64,
    pitch: (f64, f64),
}

/// Detects area arrays among the pads of each component.
fn arrays(rb: &RouterBoard, only: Option<&[String]>) -> Vec<Array> {
    let mut by_comp: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (ti, t) in rb.terminals.iter().enumerate() {
        if !t.pad {
            continue;
        }
        if let Some((comp, _)) = t.label.split_once('.')
            && only.is_none_or(|o| o.iter().any(|c| c == comp))
        {
            by_comp.entry(comp).or_default().push(ti);
        }
    }
    let mut out = Vec::new();
    for terms in by_comp.into_values() {
        if terms.len() < 16 {
            continue;
        }
        let um = |v: f64| (v / 1_000.0).round() as i64;
        let mut xs: Vec<i64> = terms.iter().map(|&t| um(rb.terminals[t].at.x)).collect();
        let mut ys: Vec<i64> = terms.iter().map(|&t| um(rb.terminals[t].at.y)).collect();
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        if xs.len() < 4 || ys.len() < 4 {
            continue;
        }
        let pitch = |v: &[i64]| v.windows(2).map(|w| w[1] - w[0]).min().unwrap_or(0);
        let (px, py) = (pitch(&xs), pitch(&ys));
        if px <= 0 || py <= 0 {
            continue;
        }
        let (nx, ny) = ((xs[xs.len() - 1] - xs[0]) / px + 1, (ys[ys.len() - 1] - ys[0]) / py + 1);
        // Every pad on the lattice, and the lattice mostly filled.
        let mut pads = Vec::new();
        let mut regular = true;
        for &t in &terms {
            let at = rb.terminals[t].at;
            let (dx, dy) = (um(at.x) - xs[0], um(at.y) - ys[0]);
            if dx % px > 1 && px - dx % px > 1 || dy % py > 1 && py - dy % py > 1 {
                regular = false;
                break;
            }
            pads.push((t, (dx as f64 / px as f64).round() as i64, (dy as f64 / py as f64).round() as i64));
        }
        if !regular || (pads.len() as i64) * 2 < nx * ny {
            continue;
        }
        out.push(Array { pads, nx, ny, pitch: (px as f64 * 1_000.0, py as f64 * 1_000.0) });
    }
    out
}

/// Plans dog-bone fanouts for the area arrays of the nets in `routes`. `index` holds the static
/// obstacles; the planned stubs and vias are added to it.
pub(crate) fn plan(
    rb: &RouterBoard,
    routes: &[NetRoute],
    slots: &[usize],
    index: &mut Index,
    only: Option<&[String]>,
) -> Vec<DogBone> {
    if slots.len() < 2 {
        return vec![];
    }
    let wanted = wanted(rb, routes);
    let mut out = Vec::new();
    for arr in arrays(rb, only) {
        // Rings left to the router: two when a track fits between neighboring pads.
        let pad_size = arr
            .pads
            .iter()
            .map(|&(t, _, _)| {
                let b = rb.terminals[t].shape.bbox();
                (b.max.x - b.min.x).max(b.max.y - b.min.y)
            })
            .fold(0.0, f64::max);
        let mut cells: Vec<(i64, usize, i64, i64)> = Vec::new();
        for &(t, i, j) in &arr.pads {
            let ring = i.min(j).min(arr.nx - 1 - i).min(arr.ny - 1 - j);
            cells.push((ring, t, i, j));
        }
        // Outer rings first, then lattice order: deterministic, and inner vias fill in last.
        cells.sort_by_key(|&(ring, _, i, j)| (ring, j, i));
        for (ring, t, i, j) in cells {
            let term = &rb.terminals[t];
            if !wanted[t] || term.layers.count_ones() != 1 {
                continue;
            }
            let layer = term.layers.trailing_zeros() as usize;
            if !slots.contains(&layer) {
                continue;
            }
            let pr = rb.profile(term.net);
            let gap = arr.pitch.0.min(arr.pitch.1) - pad_size;
            let free_rings = if gap >= 2.0 * pr.hw + 2.0 * rb.c_pad_near(t) { 2 } else { 1 };
            if ring < free_rings {
                continue;
            }
            let sx = if 2 * i + 1 < arr.nx || (2 * i + 1 == arr.nx && j % 2 == 0) { -1.0 } else { 1.0 };
            let sy = if 2 * j + 1 < arr.ny || (2 * j + 1 == arr.ny && i % 2 == 0) { -1.0 } else { 1.0 };
            let at = term.at;
            let ck = Checker { rb, index };
            let dirs = [(sx, sy), (sx, -sy), (-sx, sy), (-sx, -sy)];
            let site = |dx: f64, dy: f64| P::new(at.x + dx * arr.pitch.0 / 2.0, at.y + dy * arr.pitch.1 / 2.0);
            // The net's via, else the smallest via the rules allow (minimum drill and annular
            // ring) when that is smaller.
            let found = dirs
                .into_iter()
                .find_map(|(dx, dy)| {
                    let v = site(dx, dy);
                    let ok = ck.via(v, term.net, None).is_none() && ck.seg(layer, at, v, term.net).is_none();
                    ok.then_some((v, None))
                })
                .or_else(|| {
                    let (dia, drill) = small_via(rb, term.net)?;
                    let (rv, dr) = (dia.0 as f64 / 2.0, drill.0 as f64 / 2.0);
                    dirs.into_iter().find_map(|(dx, dy)| {
                        let v = site(dx, dy);
                        let ok =
                            ck.via_sized(v, term.net, rv, dr).is_none() && ck.seg(layer, at, v, term.net).is_none();
                        ok.then_some((v, Some((dia, drill))))
                    })
                });
            let Some((v, small)) = found else { continue };
            index.insert(Item::Seg { net: term.net, layer: layer as u8, a: at, b: v }, BoxF::of2(at, v).expand(pr.hw));
            index.insert(Item::Via { net: term.net, at: v }, BoxF::of2(v, v).expand(pr.rv));
            out.push(DogBone { net: term.net, layer, pad: at, via: v, label: term.label.clone(), small });
        }
    }
    out
}

/// Depth step (nm) of the crossover via search.
const CROSS_STEP: f64 = 25_000.0;
/// Deepest crossover via (nm beyond the pad's end).
const CROSS_DEPTH: f64 = 3_000_000.0;

/// Crossover vias: in a row of fine-pitch single-layer pads (no track passes between
/// neighbors), two nets whose pads interleave (`X Y X Y`, the D+/D− pairs of a USB-C
/// receptacle) cannot both join their pads on that layer: whichever joins first walls in the
/// other's inner pad. This gives the inner pad of one of the two nets a via straight out of
/// the pad, deep enough that both its neighbors can still leave the row (straight, then at 45°
/// away from the via, past it) and the pads beyond them can turn away first, so the other net
/// can loop around the via on the pad layer and
/// this one joins its pads on another layer. The neighbors' exits are reserved as escapes
/// (other nets' routing would otherwise take them), and the net's other pad of the crossing,
/// when a neighbor's exit leaves the router no grid access to it, gets an exit and a via too. The shallowest legal via of the two
/// candidate pads wins (ties: net, label). Checked exactly like the dog bones; the router
/// treats the result as existing copper, and drops it again if nothing uses it.
pub(crate) fn crossovers(
    rb: &RouterBoard,
    routes: &[NetRoute],
    slots: &[usize],
    grid: &Grid,
    index: &mut Index,
) -> Crossovers {
    if slots.len() < 2 {
        return Crossovers::default();
    }
    let wanted = wanted(rb, routes);
    let mut by_comp: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (ti, t) in rb.terminals.iter().enumerate() {
        if let Some((comp, _)) = t.label.split_once('.')
            && t.pad
        {
            by_comp.entry(comp).or_default().push(ti);
        }
    }
    // Candidates: (depth in µm, net, label, terminal, layer, axis, row, index in it, crossing net).
    #[allow(clippy::type_complexity)]
    let mut cands: Vec<(i64, u32, String, usize, usize, P, Vec<(f64, usize)>, usize, u32)> = Vec::new();
    for terms in by_comp.values() {
        if terms.len() < 4 {
            continue;
        }
        let n = terms.len() as f64;
        let centroid = P::new(
            terms.iter().map(|&t| rb.terminals[t].at.x).sum::<f64>() / n,
            terms.iter().map(|&t| rb.terminals[t].at.y).sum::<f64>() / n,
        );
        // Rows: (layer, axis, position along the axis in 10 µm) → (lateral position, terminal).
        #[allow(clippy::type_complexity)]
        let mut rows: BTreeMap<(usize, i64, i64, i64), Vec<(f64, usize)>> = BTreeMap::new();
        for &t in terms {
            let term = &rb.terminals[t];
            if term.layers.count_ones() != 1 {
                continue;
            }
            let layer = term.layers.trailing_zeros() as usize;
            if !slots.contains(&layer) {
                continue;
            }
            let b = term.shape.bbox();
            let (w, h) = (b.max.x - b.min.x, b.max.y - b.min.y);
            let axis = if w >= 1.5 * h {
                P::new(sign(term.at.x - centroid.x), 0.0)
            } else if h >= 1.5 * w {
                P::new(0.0, sign(term.at.y - centroid.y))
            } else {
                continue;
            };
            if axis.x == 0.0 && axis.y == 0.0 {
                continue;
            }
            let along = term.at.x * axis.x + term.at.y * axis.y;
            let lat = term.at.x * -axis.y + term.at.y * axis.x;
            let key = (layer, axis.x as i64, axis.y as i64, (along / 10_000.0).round() as i64);
            rows.entry(key).or_default().push((lat, t));
        }
        for ((layer, ax, ay, _), mut row) in rows {
            if row.len() < 4 {
                continue;
            }
            row.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let axis = P::new(ax as f64, ay as f64);
            let net_at = |i: usize| rb.terminals[row[i].1].net;
            let isl_at = |i: usize| rb.terminals[row[i].1].island;
            // Lateral half size of a pad.
            let half = |i: usize| {
                let bb = rb.terminals[row[i].1].shape.bbox();
                ((bb.max.x - bb.min.x) * axis.y.abs() + (bb.max.y - bb.min.y) * axis.x.abs()) / 2.0
            };
            let gap = |i: usize, j: usize| (row[j].0 - row[i].0).abs() - half(i) - half(j);
            for k in 1..row.len() - 1 {
                let t = row[k].1;
                let x = net_at(k);
                if !wanted[t] || net_at(k - 1) == x || net_at(k + 1) == x {
                    continue;
                }
                // Boxed: no track passes between the pad and either neighbor.
                let pr = rb.profile(x);
                let c = rb.c_pad_near(t);
                if gap(k - 1, k) >= 2.0 * pr.hw + 2.0 * c || gap(k, k + 1) >= 2.0 * pr.hw + 2.0 * c {
                    continue;
                }
                // Tightly interleaved (X Y X Y): the next pad but one is X's (another island),
                // and the pads on both sides of this one are a net Y's (two islands), so whichever
                // net joins first on this layer walls the other in.
                let mut crossing: Option<u32> = None;
                for (i, j, l) in [(k.wrapping_sub(2), k - 1, k + 1), (k + 2, k + 1, k - 1)] {
                    if i >= row.len() || net_at(i) != x || isl_at(i) == isl_at(k) {
                        continue;
                    }
                    let y = net_at(j);
                    if y != x && net_at(l) == y && isl_at(l) != isl_at(j) && wanted[row[j].1] {
                        crossing = Some(crossing.map_or(y, |c: u32| c.min(y)));
                    }
                }
                let Some(y) = crossing else { continue };
                if let Some((d, _, _)) = crossover_via(rb, index, &row, k, layer, axis, &wanted) {
                    cands.push((d, x, rb.terminals[t].label.clone(), t, layer, axis, row.clone(), k, y));
                }
            }
        }
    }
    cands.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let mut done: Vec<(u32, u32)> = Vec::new();
    let mut out = Crossovers::default();
    for (_, x, label, t, layer, axis, row, k, y) in cands {
        let pair = (x.min(y), x.max(y));
        let taken = [t, row[k - 1].1, row[k + 1].1].iter().any(|n| out.pads.contains(n));
        if done.contains(&pair) || taken {
            continue;
        }
        // Planned again with the crossovers placed so far.
        let Some((_, v, exits)) = crossover_via(rb, index, &row, k, layer, axis, &wanted) else { continue };
        let at = rb.terminals[t].at;
        let pr = rb.profile(x);
        index.insert(Item::Seg { net: x, layer: layer as u8, a: at, b: v }, BoxF::of2(at, v).expand(pr.hw));
        index.insert(Item::Via { net: x, at: v }, BoxF::of2(v, v).expand(pr.rv));
        done.push(pair);
        out.pads.push(t);
        out.bones.push(DogBone { net: x, layer, pad: at, via: v, label, small: None });
        for (n, pts) in exits {
            let term = &rb.terminals[n];
            insert_escape(rb, index, term.net, layer, &pts);
            out.pads.push(n);
            out.exits.push(Escape { net: term.net, layer, pts, label: term.label.clone() });
        }
        // The net's other pad of the crossing, beyond a neighbor's exit: its own via, so the
        // two join on another layer under the other net's loop.
        let partner = [k.wrapping_sub(2), k + 2].into_iter().find(|&i| {
            i < row.len() && rb.terminals[row[i].1].net == x && rb.terminals[row[i].1].island != rb.terminals[t].island
        });
        // Its exit keeps clear of the exit of the neighbor between them, which goes on past the
        // crossover via.
        let between = partner.and_then(|i| {
            let n = rb.terminals[row[(i + k) / 2].1].label.as_str();
            out.exits.iter().rev().find(|e| e.label == n).map(|e| (e.net, *e.pts.last().expect("exit")))
        });
        // Only when the router could not reach it from a grid cell otherwise.
        let partner = partner.filter(|&i| !grid_access(rb, index, grid, row[i].1, layer));
        if let (Some(i), Some((nn, end))) = (partner, between)
            && let Some((pts, pv)) = partner_exit(rb, index, row[i].1, layer, axis, nn, end)
        {
            let pt = &rb.terminals[row[i].1];
            let bend = *pts.last().expect("exit");
            insert_escape(rb, index, x, layer, &pts);
            index.insert(Item::Seg { net: x, layer: layer as u8, a: bend, b: pv }, BoxF::of2(bend, pv).expand(pr.hw));
            index.insert(Item::Via { net: x, at: pv }, BoxF::of2(pv, pv).expand(pr.rv));
            out.pads.push(row[i].1);
            if pts.len() > 1 {
                out.exits.push(Escape { net: x, layer, pts, label: pt.label.clone() });
            }
            out.bones.push(DogBone { net: x, layer, pad: bend, via: pv, label: pt.label.clone(), small: None });
        }
    }
    out
}

/// Whether pad `t` has a grid cell within the router's access reach (1.6 pitches) joined to its
/// center by a legal straight stub on `layer`, as the router's pad access looks for (the cell
/// legal with the grid's sampling margin).
fn grid_access(rb: &RouterBoard, index: &Index, grid: &Grid, t: usize, layer: usize) -> bool {
    let term = &rb.terminals[t];
    let ck = Checker { rb, index };
    let Some((x0, y0, x1, y1)) = grid.crange(&term.shape.bbox().expand(1.6 * grid.g)) else { return false };
    // The cell itself as the grid samples it: with the sampling margin of its static maps.
    let pr = rb.profile(term.net);
    let margin = delta(pr.hw + pr.c.min(rb.c_pad_near(t)), grid.g * std::f64::consts::SQRT_2 / 2.0);
    (y0..=y1).any(|y| {
        (x0..=x1).any(|x| {
            let c = grid.cell(x, y);
            ck.seg_margin(layer, c, c, term.net, margin).is_none() && ck.seg(layer, term.at, c, term.net).is_none()
        })
    })
}

/// The exit and via of pad `t`, the other pad of the crossover via's net: straight out along
/// `axis`, then at 45° away from the exit (of net `nn`, ending at `end`) between them until
/// a track or via of the net keeps clear of that exit's line, then straight on to the first
/// legal via. Returns the exit's points (from the pad center) and the via; exact checks.
fn partner_exit(
    rb: &RouterBoard,
    index: &Index,
    t: usize,
    layer: usize,
    axis: P,
    nn: u32,
    end: P,
) -> Option<(Vec<P>, P)> {
    let term = &rb.terminals[t];
    let (pr, pn) = (rb.profile(term.net), rb.profile(nn));
    let ck = Checker { rb, index };
    let lat = P::new(-axis.y, axis.x);
    let off = (term.at.x - end.x) * lat.x + (term.at.y - end.y) * lat.y;
    let u = if off >= 0.0 { lat } else { P::new(-lat.x, -lat.y) };
    let c = rb.c_nets_near(t, nn);
    let need = (pn.hw + pr.hw.max(pr.rv) + c + 2.0 * TOL - off.abs()).max(0.0);
    let steps = (CROSS_DEPTH / CROSS_STEP).round() as i64;
    for i in 0..=steps {
        let t0 = i as f64 * CROSS_STEP;
        let p1 = P::new(term.at.x + axis.x * t0, term.at.y + axis.y * t0);
        let p2 = P::new(p1.x + (axis.x + u.x) * need, p1.y + (axis.y + u.y) * need);
        let mut pts = vec![term.at, p1, p2];
        pts.dedup_by(|a, b| a.dist(*b) < 1.0);
        if !pts.windows(2).all(|s| ck.seg(layer, s[0], s[1], term.net).is_none()) {
            continue;
        }
        for j in 0..=steps {
            let d = j as f64 * CROSS_STEP;
            let v = P::new(p2.x + axis.x * d, p2.y + axis.y * d);
            if ck.via(v, term.net, None).is_none() && ck.seg(layer, p2, v, term.net).is_none() {
                return Some((pts, v));
            }
        }
    }
    None
}

/// Result of [`crossovers`].
#[derive(Default)]
pub(crate) struct Crossovers {
    /// The vias, as dog bones.
    pub bones: Vec<DogBone>,
    /// The reserved exits of the vias' row neighbors.
    pub exits: Vec<Escape>,
    /// Terminals with a via or an exit (no escape is planned for them).
    pub pads: Vec<usize>,
}

/// An exit of a crossover via's neighbor: (terminal, points from the pad center).
type Exit = (usize, Vec<P>);

/// The shallowest via straight out of pad `row[k]` (along `axis`) that is legal, leaves both
/// row neighbors an exit and leaves the wanted pads beyond them a way out past those exits
/// ([`outer_exit`]): (depth in µm, via center, the exits).
#[allow(clippy::too_many_arguments)]
fn crossover_via(
    rb: &RouterBoard,
    index: &mut Index,
    row: &[(f64, usize)],
    k: usize,
    layer: usize,
    axis: P,
    wanted: &[bool],
) -> Option<(i64, P, Vec<Exit>)> {
    let term = &rb.terminals[row[k].1];
    let (net, at) = (term.net, term.at);
    let pr = rb.profile(net);
    let b = term.shape.bbox();
    let half = ((b.max.x - b.min.x) * axis.x.abs() + (b.max.y - b.min.y) * axis.y.abs()) / 2.0;
    let mut d = half;
    while d <= half + CROSS_DEPTH {
        let v = P::new(at.x + axis.x * d, at.y + axis.y * d);
        let legal = {
            let ck = Checker { rb, index };
            ck.via(v, net, None).is_none() && ck.seg(layer, at, v, net).is_none()
        };
        if legal {
            let mut ids = vec![
                index.insert(Item::Seg { net, layer: layer as u8, a: at, b: v }, BoxF::of2(at, v).expand(pr.hw)),
                index.insert(Item::Via { net, at: v }, BoxF::of2(v, v).expand(pr.rv)),
            ];
            let mut exits = Vec::new();
            for j in [k - 1, k + 1] {
                let n = row[j].1;
                let Some(pts) = neighbor_exit(rb, index, n, layer, axis, at, d, pr.rv) else { break };
                ids.extend(insert_escape(rb, index, rb.terminals[n].net, layer, &pts));
                exits.push((n, pts));
            }
            // The pads beyond the neighbors still get out: their exits turn away before the
            // neighbors' exits turn towards them.
            let outer_ok = exits.len() == 2
                && exits.iter().zip([k.wrapping_sub(2), k + 2]).all(|((n, pts), o)| {
                    o >= row.len()
                        || !wanted[row[o].1]
                        || outer_exit(rb, index, row[o].1, layer, axis, *n, pts, at, d).is_some()
                });
            for id in ids {
                index.remove(id);
            }
            if outer_ok {
                return Some(((d / 1_000.0).round() as i64, v, exits));
            }
        }
        d += CROSS_STEP;
    }
    None
}

/// An exit for pad `t` out of its row past a via of radius `rv` at `depth` along `axis` from
/// `from`: straight out, then at 45° away from the via until it clears it, then straight on to
/// the via's depth (beyond it a track can keep turning away); exact checks against `index`.
/// The turn is the deepest legal one.
#[allow(clippy::too_many_arguments)]
fn neighbor_exit(
    rb: &RouterBoard,
    index: &Index,
    t: usize,
    layer: usize,
    axis: P,
    from: P,
    depth: f64,
    rv: f64,
) -> Option<Vec<P>> {
    let term = &rb.terminals[t];
    let pr = rb.profile(term.net);
    let ck = Checker { rb, index };
    let lat = P::new(-axis.y, axis.x);
    let off = (term.at.x - from.x) * lat.x + (term.at.y - from.y) * lat.y;
    let u = if off >= 0.0 { lat } else { P::new(-lat.x, -lat.y) };
    let need = (rv + pr.hw + rb.c_tracks_near(t) + 2.0 * TOL - off.abs()).max(0.0);
    let base = (term.at.x - from.x) * axis.x + (term.at.y - from.y) * axis.y;
    let end = depth - base;
    // Deepest turn first: the exit stays in its pad's line as long as the via allows, which
    // leaves the pad beyond it room to turn away first.
    let steps = ((end - need) / CROSS_STEP).floor();
    if steps < 0.0 {
        return None;
    }
    let mut i = steps as i64;
    while i >= 0 {
        let t0 = i as f64 * CROSS_STEP;
        let p1 = P::new(term.at.x + axis.x * t0, term.at.y + axis.y * t0);
        let p2 = P::new(p1.x + (axis.x + u.x) * need, p1.y + (axis.y + u.y) * need);
        let rest = end - t0 - need;
        let p3 = P::new(p2.x + axis.x * rest, p2.y + axis.y * rest);
        let mut pts = vec![term.at, p1, p2, p3];
        pts.dedup_by(|a, b| a.dist(*b) < 1.0);
        if pts.len() > 1 && pts.windows(2).all(|s| ck.seg(layer, s[0], s[1], term.net).is_none()) {
            return Some(pts);
        }
        i -= 1;
    }
    None
}

/// An exit for pad `t`, the pad beyond a crossover via's row neighbor `n` (whose exit `npts`
/// turns towards it at the via), checked against `index` (which holds the via and the
/// neighbors' exits): straight out, then at 45° away from the via by as much as the neighbor's
/// exit moves, then straight on to the via's depth (`depth` along `axis` from `from`). Only
/// tells that such a pad is not walled in by the crossover; its escape is planned later.
#[allow(clippy::too_many_arguments)]
fn outer_exit(
    rb: &RouterBoard,
    index: &Index,
    t: usize,
    layer: usize,
    axis: P,
    n: usize,
    npts: &[P],
    from: P,
    depth: f64,
) -> Option<Vec<P>> {
    let term = &rb.terminals[t];
    let ck = Checker { rb, index };
    let lat = P::new(-axis.y, axis.x);
    let off = (term.at.x - from.x) * lat.x + (term.at.y - from.y) * lat.y;
    let u = if off >= 0.0 { lat } else { P::new(-lat.x, -lat.y) };
    let n0 = rb.terminals[n].at;
    let n1 = *npts.last()?;
    let shift = ((n1.x - n0.x) * lat.x + (n1.y - n0.y) * lat.y).abs();
    let base = (term.at.x - from.x) * axis.x + (term.at.y - from.y) * axis.y;
    let end = depth - base;
    let mut t0 = 0.0;
    while t0 + shift <= end {
        let p1 = P::new(term.at.x + axis.x * t0, term.at.y + axis.y * t0);
        let p2 = P::new(p1.x + (axis.x + u.x) * shift, p1.y + (axis.y + u.y) * shift);
        let rest = end - t0 - shift;
        let p3 = P::new(p2.x + axis.x * rest, p2.y + axis.y * rest);
        let mut pts = vec![term.at, p1, p2, p3];
        pts.dedup_by(|a, b| a.dist(*b) < 1.0);
        if pts.len() > 1 && pts.windows(2).all(|s| ck.seg(layer, s[0], s[1], term.net).is_none()) {
            return Some(pts);
        }
        t0 += CROSS_STEP;
    }
    None
}

/// A planned escape: a track from a fine-pitch pad center straight out along the pad, then at
/// 45° onto a grid cell.
#[derive(Clone, Debug)]
pub(crate) struct Escape {
    pub net: u32,
    /// Stackup layer of the pad.
    pub layer: usize,
    /// Pad center, bend (if any), grid cell.
    pub pts: Vec<P>,
    /// Pad label (`U1.7`).
    pub label: String,
}

/// Plans escapes for fine-pitch pads (no track passes between a pad and its nearest neighbor).
/// A component gets them when at least one such pad is too far off the routing grid for a track
/// along the nearest grid line to clear its neighbors; then every off-grid fine-pitch pad of it
/// gets one, so that neighbors stagger consistently. Without them the grid router could only
/// reach such pads with slanted stubs that clip the neighbors. A pad's candidate exits end on
/// cells beyond the pad that are legal (checked exactly against everything placed so far) and
/// from which the router can move outwards (`open(net, layer, cell, step)`), shallowest first;
/// one exit per pad is then chosen for the whole package at once ([`assign`]: no two chosen
/// exits break a clearance or end in each other's halo as the router rasterizes it), the pads
/// labeled in `prio` served first. Pads that had candidates but got no exit are added to
/// `missed`. The pads of the crossovers already planned (`prior`) are left out, and the new
/// escapes keep apart from their exits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn escape(
    rb: &RouterBoard,
    routes: &[NetRoute],
    slots: &[usize],
    grid: &Grid,
    index: &mut Index,
    only: Option<&[String]>,
    open: &dyn Fn(u32, usize, (i32, i32), (i32, i32)) -> bool,
    prior: &Crossovers,
    prio: &[String],
    missed: &mut Vec<String>,
) -> Vec<Escape> {
    let wanted = wanted(rb, routes);
    let mut by_comp: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (ti, t) in rb.terminals.iter().enumerate() {
        if let Some((comp, _)) = t.label.split_once('.')
            && t.pad
            && only.is_none_or(|o| o.iter().any(|c| c == comp))
        {
            by_comp.entry(comp).or_default().push(ti);
        }
    }
    let g = grid.g;
    // The crossover exits first: new escapes keep apart from them too.
    let mut out: Vec<Escape> = prior.exits.clone();
    for terms in by_comp.into_values() {
        if terms.len() < 4 {
            continue;
        }
        let n = terms.len() as f64;
        let centroid = P::new(
            terms.iter().map(|&t| rb.terminals[t].at.x).sum::<f64>() / n,
            terms.iter().map(|&t| rb.terminals[t].at.y).sum::<f64>() / n,
        );
        // Fine-pitch pads: (terminal, layer, axis, half length, lateral unit) and whether any
        // of them is too far off the grid for a straight exit; then all or none get escapes,
        // so that neighbors stagger consistently.
        let mut fine: Vec<(usize, usize, P, f64, P)> = Vec::new();
        let mut needed = false;
        for &t in &terms {
            let term = &rb.terminals[t];
            if !wanted[t] || term.layers.count_ones() != 1 || prior.pads.contains(&t) {
                continue;
            }
            let layer = term.layers.trailing_zeros() as usize;
            if !slots.contains(&layer) {
                continue;
            }
            let b = term.shape.bbox();
            let (w, h) = (b.max.x - b.min.x, b.max.y - b.min.y);
            // Axis along the pad's long side, pointing away from the component.
            let (axis, half_len, lat_size) = if w >= 1.5 * h {
                (P::new(sign(term.at.x - centroid.x), 0.0), w / 2.0, h)
            } else if h >= 1.5 * w {
                (P::new(0.0, sign(term.at.y - centroid.y)), h / 2.0, w)
            } else {
                continue;
            };
            if axis.x == 0.0 && axis.y == 0.0 {
                continue;
            }
            let lat = P::new(-axis.y, axis.x);
            let pr = rb.profile(term.net);
            // Fine pitch: the nearest pad of the component leaves no room for a track.
            let nearest =
                terms.iter().filter(|&&o| o != t).map(|&o| rb.terminals[o].at.dist(term.at)).fold(f64::MAX, f64::min);
            let c = rb.c_pad_near(t);
            if nearest - lat_size >= 2.0 * pr.hw + 2.0 * c {
                continue;
            }
            // Too far from a grid line for a track along it to clear the neighbors.
            let u = if axis.x != 0.0 { term.at.y - grid.oy } else { term.at.x - grid.ox };
            let off = u.rem_euclid(g);
            needed |= nearest - lat_size / 2.0 - pr.hw - off.min(g - off) < c + TOL;
            if off.min(g - off) >= 1_000.0 {
                fine.push((t, layer, axis, half_len, lat));
            }
        }
        if !needed {
            continue;
        }
        // Every pad's candidate exits (legal against everything placed so far), then one
        // exit per pad chosen for the whole package at once (`assign`).
        let cands: Vec<Vec<Vec<P>>> = fine
            .iter()
            .map(|&(t, layer, axis, half_len, lat)| {
                escape_cands(rb, grid, index, open, &out, t, layer, axis, half_len, lat)
            })
            .collect();
        let pads: Vec<(u32, usize)> = fine.iter().map(|&(t, layer, ..)| (rb.terminals[t].net, layer)).collect();
        let first: Vec<bool> = fine.iter().map(|&(t, ..)| prio.contains(&rb.terminals[t].label)).collect();
        let chosen = assign(rb, grid, &pads, &cands, &first);
        let mut best: Vec<Escape> = Vec::new();
        for (k, c) in chosen.into_iter().enumerate() {
            let term = &rb.terminals[fine[k].0];
            let Some(i) = c else {
                // Boxed in by its neighbors' exits (it had candidates of its own).
                if !cands[k].is_empty() {
                    missed.push(term.label.clone());
                }
                continue;
            };
            best.push(Escape { net: term.net, layer: fine[k].1, pts: cands[k][i].clone(), label: term.label.clone() });
        }
        for e in &best {
            insert_escape(rb, index, e.net, e.layer, &e.pts);
        }
        out.extend(best);
    }
    out.split_off(prior.exits.len())
}

/// Adds an escape's segments to the index; returns their ids.
fn insert_escape(rb: &RouterBoard, index: &mut Index, net: u32, layer: usize, pts: &[P]) -> Vec<u32> {
    let hw = rb.profile(net).hw;
    pts.windows(2)
        .map(|s| {
            index.insert(Item::Seg { net, layer: layer as u8, a: s[0], b: s[1] }, BoxF::of2(s[0], s[1]).expand(hw))
        })
        .collect()
}

/// Most candidate exits per pad in [`escape`].
const ESCAPE_CANDS: usize = 24;

/// Farthest an escape's end cell lies beside its pad's axis (grid pitches).
const ESCAPE_SIDE: f64 = 2.0;

/// The candidate exits of one pad, shallowest first (see [`escape`]): each legal against
/// everything in `index`, apart from the escapes `done`, and ending on a cell from which the
/// router can move outwards. When no exit turning at 45° straight onto its cell is legal, exits
/// turning earlier and then running straight on to the cell are tried.
#[allow(clippy::too_many_arguments)]
fn escape_cands(
    rb: &RouterBoard,
    grid: &Grid,
    index: &Index,
    open: &dyn Fn(u32, usize, (i32, i32), (i32, i32)) -> bool,
    done: &[Escape],
    t: usize,
    layer: usize,
    axis: P,
    half_len: f64,
    lat: P,
) -> Vec<Vec<P>> {
    let g = grid.g;
    let term = &rb.terminals[t];
    // Candidate end cells beyond the pad, shallowest first.
    let area = BoxF::of2(term.at, term.at).expand(half_len + 8.0 * g);
    let Some((x0, y0, x1, y1)) = grid.crange(&area) else { return vec![] };
    let mut cells: Vec<(i64, i64, i32, i32)> = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let e = grid.cell(x, y);
            let d = P::new(e.x - term.at.x, e.y - term.at.y);
            let along = d.x * axis.x + d.y * axis.y;
            let side = d.x * lat.x + d.y * lat.y;
            if side.abs() > ESCAPE_SIDE * g || along < half_len - g || along < side.abs() {
                continue;
            }
            cells.push(((along / 1_000.0).round() as i64, (side.abs() / 1_000.0).round() as i64, x, y));
        }
    }
    cells.sort_unstable();
    let ck = Checker { rb, index };
    let step = (axis.x as i32, axis.y as i32);
    let mut out = Vec::new();
    for &(_, _, x, y) in &cells {
        let e = grid.cell(x, y);
        let d = P::new(e.x - term.at.x, e.y - term.at.y);
        let side = (d.x * lat.x + d.y * lat.y).abs();
        let along = d.x * axis.x + d.y * axis.y;
        let mut pts = vec![term.at];
        if side > 1.0 {
            pts.push(P::new(term.at.x + axis.x * (along - side), term.at.y + axis.y * (along - side)));
        }
        pts.push(e);
        pts.dedup_by(|a, b| a.dist(*b) < 1.0);
        let ok = pts.len() > 1
            && open(term.net, layer, (x, y), step)
            && pts.windows(2).all(|s| ck.seg(layer, s[0], s[1], term.net).is_none())
            && done.iter().all(|o: &Escape| apart(rb, grid, o, term.net, layer, &pts));
        if ok {
            out.push(pts);
            if out.len() == ESCAPE_CANDS {
                break;
            }
        }
    }
    if !out.is_empty() {
        return out;
    }
    // None: the turn earlier, then straight on to the cell (a neighbor's track turning
    // towards the pad further out leaves it no room to turn there), deepest turn first.
    for &(_, _, x, y) in &cells {
        let e = grid.cell(x, y);
        let d = P::new(e.x - term.at.x, e.y - term.at.y);
        let side = d.x * lat.x + d.y * lat.y;
        let along = d.x * axis.x + d.y * axis.y;
        if side.abs() <= 1.0 || !open(term.net, layer, (x, y), step) {
            continue;
        }
        let mut t0 = along - side.abs() - CROSS_STEP;
        while t0 >= 0.0 {
            let p1 = P::new(term.at.x + axis.x * t0, term.at.y + axis.y * t0);
            let p2 = P::new(p1.x + axis.x * side.abs() + lat.x * side, p1.y + axis.y * side.abs() + lat.y * side);
            let mut pts = vec![term.at, p1, p2, e];
            pts.dedup_by(|a, b| a.dist(*b) < 1.0);
            if pts.windows(2).all(|s| ck.seg(layer, s[0], s[1], term.net).is_none())
                && done.iter().all(|o: &Escape| apart(rb, grid, o, term.net, layer, &pts))
            {
                out.push(pts);
                break;
            }
            t0 -= CROSS_STEP;
        }
        if out.len() == ESCAPE_CANDS {
            break;
        }
    }
    out
}

/// Whether two candidate exits (of pads `a`, `b`: net and layer) cannot both be used: their
/// tracks come closer than the rules allow, or one ends in the other's halo as the router
/// rasterizes it ([`apart`]).
fn clash(rb: &RouterBoard, grid: &Grid, a: (u32, usize), pa: &[P], b: (u32, usize), pb: &[P]) -> bool {
    if a.1 != b.1 || a.0 == b.0 {
        return false;
    }
    let layer = a.1;
    let o = Escape { net: b.0, layer, pts: pb.to_vec(), label: String::new() };
    if !apart(rb, grid, &o, a.0, layer, pa) {
        return true;
    }
    let (ra, rbp) = (rb.profile(a.0), rb.profile(b.0));
    pa.windows(2).any(|s| {
        let mine: Vec<(P, P, St)> =
            if rb.scoped.on { rb.pieces(ra, layer, s[0], s[1], ra.hw) } else { vec![(s[0], s[1], St::default())] };
        pb.windows(2).any(|q| rb.segs_conflict(ra, layer, &mine, 0.0, rbp, q[0], q[1], rbp.hw))
    })
}

/// Most chosen exits a pad without one may move out of its way in [`choose`].
const EJECT: usize = 3;

/// Levels of further moves an exit moved out of the way may make in [`choose`].
const CHAIN: usize = 2;

/// Candidate clashes of one package, computed once per pair of candidates of pads close
/// enough to meet.
struct Clashes<'a> {
    /// Candidates per pad.
    counts: &'a [usize],
    /// Pads whose candidates may meet, per pad.
    near: Vec<Vec<usize>>,
    /// Whether candidate `i` of pad `k` and candidate `j` of pad `l` clash: `(k, i, l, j)`.
    test: &'a mut dyn FnMut(usize, usize, usize, usize) -> bool,
    memo: BTreeMap<(usize, usize, usize, usize), bool>,
}

impl Clashes<'_> {
    fn get(&mut self, k: usize, i: usize, l: usize, j: usize) -> bool {
        let key = if k < l { (k, i, l, j) } else { (l, j, k, i) };
        if let Some(&v) = self.memo.get(&key) {
            return v;
        }
        let v = (self.test)(key.0, key.1, key.2, key.3);
        self.memo.insert(key, v);
        v
    }

    /// The pads (other than those in `skip`) whose chosen exit clashes with candidate `i` of
    /// pad `k`.
    fn blockers(&mut self, k: usize, i: usize, skip: &[usize], chosen: &[Option<usize>]) -> Vec<usize> {
        let near = std::mem::take(&mut self.near[k]);
        let out = near
            .iter()
            .copied()
            .filter(|&l| !skip.contains(&l) && chosen[l].is_some_and(|j| self.get(k, i, l, j)))
            .collect();
        self.near[k] = near;
        out
    }

    /// Moves pad `q` to another candidate that clashes with no chosen exit (those of the pads
    /// in `fixed` as given there), allowing `depth` more levels of such moves for one pad in
    /// its way.
    fn relocate(&mut self, q: usize, fixed: &[(usize, usize)], depth: usize, chosen: &mut Vec<Option<usize>>) -> bool {
        let cur = chosen[q];
        for j in 0..self.counts[q] {
            if Some(j) == cur {
                continue;
            }
            if fixed.iter().any(|&(f, fi)| self.near[q].contains(&f) && self.get(q, j, f, fi)) {
                continue;
            }
            let skip: Vec<usize> = fixed.iter().map(|f| f.0).chain([q]).collect();
            let bl = self.blockers(q, j, &skip, chosen);
            if bl.is_empty() {
                chosen[q] = Some(j);
                return true;
            }
            if depth == 0 || bl.len() > 1 {
                continue;
            }
            let saved = chosen.clone();
            chosen[q] = Some(j);
            let mut fx = fixed.to_vec();
            fx.push((q, j));
            if self.relocate(bl[0], &fx, depth - 1, chosen) {
                return true;
            }
            *chosen = saved;
        }
        false
    }
}

/// Chooses at most one candidate exit per pad of a package (`cands[k]`, best first) so that no
/// two chosen exits clash ([`clash`]), for as many pads as possible ([`choose`]).
fn assign(
    rb: &RouterBoard,
    grid: &Grid,
    pads: &[(u32, usize)],
    cands: &[Vec<Vec<P>>],
    first: &[bool],
) -> Vec<Option<usize>> {
    let n = pads.len();
    let boxes: Vec<BoxF> = cands
        .iter()
        .map(|cs| {
            let mut bb = BoxF::EMPTY;
            for q in cs.iter().flatten() {
                bb.add(*q);
            }
            bb
        })
        .collect();
    let width = rb.profiles.iter().map(|p| 2.0 * p.hw).fold(0.0, f64::max);
    let reach = width + rb.max_clear() + 2.0 * grid.g;
    let near: Vec<Vec<usize>> = (0..n)
        .map(|k| {
            (0..n)
                .filter(|&l| {
                    l != k
                        && !boxes[k].is_empty()
                        && !boxes[l].is_empty()
                        && boxes[k].expand(reach).intersects(&boxes[l])
                })
                .collect()
        })
        .collect();
    let counts: Vec<usize> = cands.iter().map(Vec::len).collect();
    let mut test =
        |k: usize, i: usize, l: usize, j: usize| clash(rb, grid, pads[k], &cands[k][i], pads[l], &cands[l][j]);
    choose(&counts, near, first, &mut test)
}

/// The assignment of [`assign`]: pad `k` has `counts[k]` candidates (best first), `near[k]`
/// lists the pads whose candidates may clash with its own, `test(k, i, l, j)` tells whether
/// candidate `i` of pad `k` clashes with candidate `j` of pad `l`. The whole package is planned
/// before any exit is committed. Pads marked `first` (boxed in on an earlier run) are served
/// first, then pad order; each takes its best candidate free of the exits chosen so far. Then
/// every pad left without one tries its candidates in turn, moving the chosen exits it clashes
/// with (at most [`EJECT`]) to other candidates of theirs, which may in turn move one more
/// exit in their way, up to [`CHAIN`] levels (ejection chains), and keeps the first candidate
/// for which that works; repeated until no pad gains an exit. The number of pads with an exit
/// never drops, and everything follows the data's order, so the result is deterministic.
fn choose(
    counts: &[usize],
    near: Vec<Vec<usize>>,
    first: &[bool],
    test: &mut dyn FnMut(usize, usize, usize, usize) -> bool,
) -> Vec<Option<usize>> {
    let n = counts.len();
    let mut cl = Clashes { counts, near, test, memo: BTreeMap::new() };
    let mut chosen: Vec<Option<usize>> = vec![None; n];
    let order: Vec<usize> = (0..n).filter(|&k| first[k]).chain((0..n).filter(|&k| !first[k])).collect();
    for &k in &order {
        chosen[k] = (0..counts[k]).find(|&i| cl.blockers(k, i, &[], &chosen).is_empty());
    }
    loop {
        let mut gained = false;
        for &k in &order {
            if chosen[k].is_some() {
                continue;
            }
            for i in 0..counts[k] {
                let bl = cl.blockers(k, i, &[], &chosen);
                if bl.len() > EJECT {
                    continue;
                }
                let saved = chosen.clone();
                chosen[k] = Some(i);
                let mut fixed = vec![(k, i)];
                let mut ok = true;
                for &q in &bl {
                    if !cl.relocate(q, &fixed, CHAIN, &mut chosen) {
                        ok = false;
                        break;
                    }
                    fixed.push((q, chosen[q].expect("moved")));
                }
                // Every moved exit clear of every chosen one (a chain may have moved a pad
                // checked before it).
                ok = ok
                    && (0..n).filter(|&q| chosen[q] != saved[q]).all(|q| {
                        let j = chosen[q].expect("chosen");
                        cl.blockers(q, j, &[q], &chosen).is_empty()
                    });
                if ok {
                    gained = true;
                    break;
                }
                chosen = saved;
            }
        }
        if !gained {
            break;
        }
    }
    chosen
}

/// Whether a new escape `pts` of `net` keeps clear of escape `o` with the router's margins:
/// neither end cell lies in the other's clearance halo as the router rasterizes it (off-grid
/// segments are inflated by the sampling margin), so both escapes can be used together.
fn apart(rb: &RouterBoard, grid: &Grid, o: &Escape, net: u32, layer: usize, pts: &[P]) -> bool {
    if o.layer != layer || o.net == net {
        return true;
    }
    let (a, b) = (rb.profile(net), rb.profile(o.net));
    let c = if rb.scoped.on {
        let mut bb = BoxF::EMPTY;
        for q in pts.iter().chain(&o.pts) {
            bb.add(*q);
        }
        let st = rb.st_track_box(a, &[layer], &bb.expand(a.hw.max(b.hw) + grid.g));
        rb.c_cu(a, st, b, st)
    } else {
        a.c.max(b.c)
    };
    let r = a.hw + b.hw + c;
    let r = r + delta(r, grid.g * std::f64::consts::SQRT_2 / 2.0) + 2.0 * TOL;
    let clear = |cell: P, segs: &[P]| segs.windows(2).all(|s| point_seg_d2(cell, s[0], s[1]) >= r * r);
    clear(*pts.last().expect("end"), &o.pts) && clear(*o.pts.last().expect("end"), pts)
}

fn sign(v: f64) -> f64 {
    if v > 1.0 {
        1.0
    } else if v < -1.0 {
        -1.0
    } else {
        0.0
    }
}

/// Terminals of the nets in `routes` (those with something to route).
fn wanted(rb: &RouterBoard, routes: &[NetRoute]) -> Vec<bool> {
    let mut wanted = vec![false; rb.terminals.len()];
    for r in routes {
        for isl in &r.islands {
            for &t in &isl.terms {
                wanted[t] = true;
            }
        }
    }
    wanted
}

/// Width resolution of neck-downs (nm): neck widths are multiples of it.
const NECK_STEP: f64 = 5_000.0;

/// Neck-down widths: for every net with a pad that a track of the net's width cannot leave
/// (no straight exit from the pad center out of the pad, in any of the eight directions on any
/// routing layer, is legal against the other nets' pads and the static obstacles), the widest
/// width, in `NECK_STEP` steps and not under the board's `min_track_width`, at which every such
/// pad of the net has an exit, capped so that two such tracks fit side by side out of
/// neighboring pads (pad pitch minus clearance). Nets whose pads fit, or that no allowed width
/// gets out, are left out. `index` holds the static obstacles.
pub(crate) fn necks(rb: &RouterBoard, index: &Index, slots: &[usize], nets: &[u32]) -> Vec<(u32, f64)> {
    let ck = Checker { rb, index };
    let min_w = rb.rules.min_track_width.0 as f64;
    let mut by_comp: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (ti, t) in rb.terminals.iter().enumerate() {
        if let Some((comp, _)) = t.label.split_once('.')
            && t.pad
        {
            by_comp.entry(comp).or_default().push(ti);
        }
    }
    let dirs: Vec<P> = (0..8)
        .map(|k| {
            let a = k as f64 * std::f64::consts::FRAC_PI_4;
            P::new(a.cos(), a.sin())
        })
        .collect();
    let mut out: BTreeMap<u32, f64> = BTreeMap::new();
    for terms in by_comp.values() {
        for &t in terms {
            let term = &rb.terminals[t];
            if nets.binary_search(&term.net).is_err() {
                continue;
            }
            let pr = rb.profile(term.net);
            let layers: Vec<usize> =
                slots.iter().copied().filter(|&l| term.layers & (1u64 << l.min(63)) != 0).collect();
            if layers.is_empty() {
                continue;
            }
            let b = term.shape.bbox();
            let at = term.at;
            // Exits: from the center to where the ray leaves the pad's box.
            let exits: Vec<P> = dirs
                .iter()
                .map(|d| {
                    let tx = if d.x.abs() < 1e-9 {
                        f64::MAX
                    } else {
                        ((if d.x > 0.0 { b.max.x } else { b.min.x }) - at.x) / d.x
                    };
                    let ty = if d.y.abs() < 1e-9 {
                        f64::MAX
                    } else {
                        ((if d.y > 0.0 { b.max.y } else { b.min.y }) - at.y) / d.y
                    };
                    let s = tx.min(ty).max(0.0);
                    P::new(at.x + d.x * s, at.y + d.y * s)
                })
                .collect();
            let fits = |w: f64| {
                layers
                    .iter()
                    .any(|&l| exits.iter().any(|&e| ck.seg_margin(l, at, e, term.net, w / 2.0 - pr.hw).is_none()))
            };
            // Room for a neighbor's neck beside this one.
            let pitch = terms
                .iter()
                .filter(|&&o| o != t && rb.terminals[o].net != term.net)
                .map(|&o| rb.terminals[o].at.dist(at))
                .fold(f64::MAX, f64::min);
            // Two necked tracks at the pad pitch must also clear each other as the grid samples
            // them (escapes end on cells beside their neighbors'): keep the sampling margin of
            // the finest grid such a width can give (pitch (w + c) / 2 ≤ half the pad pitch)
            // and the router's tolerances.
            let margin = delta(pitch, pitch * std::f64::consts::SQRT_2 / 4.0) + 3.0 * TOL;
            // A single-layer pad of a fine-pitch row (no track between it and its neighbor)
            // whose class width gets out of the pad but cannot run beside its neighbor's track
            // out of the row at the pad pitch: every pad of the row can have an exit only at a
            // width that can.
            let crowded = || {
                let lat = (b.max.x - b.min.x).min(b.max.y - b.min.y);
                term.layers.count_ones() == 1
                    && pitch < f64::MAX
                    && pitch - lat < 2.0 * pr.hw + 2.0 * rb.c_pad_near(t)
                    && 2.0 * pr.hw + rb.c_tracks_near(t) + 2.0 * TOL + margin > pitch
            };
            if fits(2.0 * pr.hw) && !crowded() {
                continue;
            }
            // The narrowest width the rules allow here (custom track width rules too).
            let min_w = min_w.max(rb.min_width_near(t));
            let cap = (pitch - rb.c_tracks_near(t) - 2.0 * TOL - margin).min(2.0 * pr.hw).max(min_w);
            let mut w = (cap / NECK_STEP).floor() * NECK_STEP;
            if w >= 2.0 * pr.hw {
                w -= NECK_STEP;
            }
            while w >= min_w && !fits(w) {
                w -= NECK_STEP;
            }
            if w < min_w {
                // The minimum width itself (when not a multiple of the step).
                if min_w < 2.0 * pr.hw && fits(min_w) {
                    w = min_w;
                } else {
                    continue; // nothing the rules allow gets out
                }
            }
            let e = out.entry(term.net).or_insert(w);
            *e = e.min(w);
        }
    }
    out.into_iter().collect()
}

/// The smallest via the rules allow (diameter, drill): the minimum drill with the minimum
/// annular ring, when smaller than the net's via; `None` otherwise.
fn small_via(rb: &RouterBoard, net: u32) -> Option<(Nm, Nm)> {
    let key = rb.profile(net).key;
    let drill = rb.rules.min_drill.max(Nm(1_000));
    let dia = Nm(drill.0 + 2 * rb.rules.min_annular_ring.0);
    (dia < key.via_dia && drill <= key.via_drill).then_some((dia, drill))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pads in a row, each candidate clashing with listed candidates of other pads.
    /// (pad, candidate).
    type Cand = (usize, usize);

    fn run(counts: &[usize], clashes: &[(Cand, Cand)], first: &[bool]) -> Vec<Option<usize>> {
        let n = counts.len();
        let near: Vec<Vec<usize>> = (0..n).map(|k| (0..n).filter(|&l| l != k).collect()).collect();
        let mut test = |k: usize, i: usize, l: usize, j: usize| {
            clashes.iter().any(|&(a, b)| (a == (k, i) && b == (l, j)) || (a == (l, j) && b == (k, i)))
        };
        choose(counts, near, first, &mut test)
    }

    #[test]
    fn exits_move_aside_for_a_boxed_pad() {
        // Pad 0's best exit takes pad 1's only one; pad 0 has another.
        let c = [((0, 0), (1, 0))];
        assert_eq!(run(&[2, 1], &c, &[false, false]), [Some(1), Some(0)]);
        // A chain: pad 0's other exit clashes with pad 2's best, which has another.
        let c = [((0, 0), (1, 0)), ((0, 1), (2, 0))];
        assert_eq!(run(&[2, 1, 2], &c, &[false; 3]), [Some(1), Some(0), Some(1)]);
        // No room for both: the pad served first keeps its exit.
        let c = [((0, 0), (1, 0))];
        assert_eq!(run(&[1, 1], &c, &[false, false]), [Some(0), None]);
        assert_eq!(run(&[1, 1], &c, &[false, true]), [None, Some(0)]);
    }
}
