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

use super::engine::NetRoute;
use super::geo::{BoxF, P, point_seg_d2};
use super::grid::{Grid, delta};
use super::index::{Checker, Index, Item, TOL};
use super::model::RouterBoard;

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
            let free_rings = if gap >= 2.0 * pr.hw + 2.0 * pr.c { 2 } else { 1 };
            if ring < free_rings {
                continue;
            }
            let sx = if 2 * i + 1 < arr.nx || (2 * i + 1 == arr.nx && j % 2 == 0) { -1.0 } else { 1.0 };
            let sy = if 2 * j + 1 < arr.ny || (2 * j + 1 == arr.ny && i % 2 == 0) { -1.0 } else { 1.0 };
            let at = term.at;
            let ck = Checker { rb, index };
            let found = [(sx, sy), (sx, -sy), (-sx, sy), (-sx, -sy)].into_iter().find_map(|(dx, dy)| {
                let v = P::new(at.x + dx * arr.pitch.0 / 2.0, at.y + dy * arr.pitch.1 / 2.0);
                let ok = ck.via(v, term.net, None).is_none() && ck.seg(layer, at, v, term.net).is_none();
                ok.then_some(v)
            });
            let Some(v) = found else { continue };
            index.insert(Item::Seg { net: term.net, layer: layer as u8, a: at, b: v }, BoxF::of2(at, v).expand(pr.hw));
            index.insert(Item::Via { net: term.net, at: v }, BoxF::of2(v, v).expand(pr.rv));
            out.push(DogBone { net: term.net, layer, pad: at, via: v, label: term.label.clone() });
        }
    }
    out
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
/// reach such pads with slanted stubs that clip the neighbors. Each pad gets the shallowest end
/// cell beyond the pad that is legal (checked exactly against everything placed so far, and
/// clear of the other escapes' halos as the router rasterizes them) and from which the router
/// can move outwards (`open(net, layer, cell, step)`).
pub(crate) fn escape(
    rb: &RouterBoard,
    routes: &[NetRoute],
    slots: &[usize],
    grid: &Grid,
    index: &mut Index,
    only: Option<&[String]>,
    open: &dyn Fn(u32, usize, (i32, i32), (i32, i32)) -> bool,
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
    let mut out = Vec::new();
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
            if !wanted[t] || term.layers.count_ones() != 1 {
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
            if nearest - lat_size >= 2.0 * pr.hw + 2.0 * pr.c {
                continue;
            }
            // Too far from a grid line for a track along it to clear the neighbors.
            let u = if axis.x != 0.0 { term.at.y - grid.oy } else { term.at.x - grid.ox };
            let off = u.rem_euclid(g);
            needed |= nearest - lat_size / 2.0 - pr.hw - off.min(g - off) < pr.c + TOL;
            if off.min(g - off) >= 1_000.0 {
                fine.push((t, layer, axis, half_len, lat));
            }
        }
        if !needed {
            continue;
        }
        for (t, layer, axis, half_len, lat) in fine {
            let term = &rb.terminals[t];
            let pr = rb.profile(term.net);
            // Candidate end cells beyond the pad, shallowest first.
            let area = BoxF::of2(term.at, term.at).expand(half_len + 8.0 * g);
            let Some((x0, y0, x1, y1)) = grid.crange(&area) else { continue };
            let mut cands: Vec<(i64, i64, i32, i32)> = Vec::new();
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let e = grid.cell(x, y);
                    let d = P::new(e.x - term.at.x, e.y - term.at.y);
                    let along = d.x * axis.x + d.y * axis.y;
                    let side = d.x * lat.x + d.y * lat.y;
                    if side.abs() > 2.0 * g || along < half_len - g || along < side.abs() {
                        continue;
                    }
                    cands.push(((along / 1_000.0).round() as i64, (side.abs() / 1_000.0).round() as i64, x, y));
                }
            }
            cands.sort_unstable();
            let ck = Checker { rb, index };
            let found = cands.into_iter().find_map(|(_, _, x, y)| {
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
                let step = (axis.x as i32, axis.y as i32);
                let ok = pts.len() > 1
                    && open(term.net, layer, (x, y), step)
                    && pts.windows(2).all(|s| ck.seg(layer, s[0], s[1], term.net).is_none())
                    && out.iter().all(|o: &Escape| apart(rb, grid, o, term.net, layer, &pts));
                ok.then_some(pts)
            });
            let Some(pts) = found else { continue };
            for s in pts.windows(2) {
                index.insert(
                    Item::Seg { net: term.net, layer: layer as u8, a: s[0], b: s[1] },
                    BoxF::of2(s[0], s[1]).expand(pr.hw),
                );
            }
            out.push(Escape { net: term.net, layer, pts, label: term.label.clone() });
        }
    }
    out
}

/// Whether a new escape `pts` of `net` keeps clear of escape `o` with the router's margins:
/// neither end cell lies in the other's clearance halo as the router rasterizes it (off-grid
/// segments are inflated by the sampling margin), so both escapes can be used together.
fn apart(rb: &RouterBoard, grid: &Grid, o: &Escape, net: u32, layer: usize, pts: &[P]) -> bool {
    if o.layer != layer || o.net == net {
        return true;
    }
    let (a, b) = (rb.profile(net), rb.profile(o.net));
    let r = a.hw + b.hw + a.c.max(b.c);
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
