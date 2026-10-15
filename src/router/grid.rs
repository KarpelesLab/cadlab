//! The routing grid and its static maps.
//!
//! Cells are spaced `g` apart. Every map is sampled on the *double grid* (spacing `g / 2`):
//! even/even points are cell centers, the others are the midpoints of orthogonal moves
//! (odd/even, even/odd) and of diagonal moves (odd/odd). A move is legal when its start, middle
//! and end samples are all legal, and the obstacle inflation carries a margin `δ` that makes
//! this exact: if two samples `s` apart are both at least `r + δ` from an obstacle, with
//! `δ = √(r² + s²/4) − r`, every point of the segment between them is at least `r` away.

use std::collections::BTreeMap;

use super::geo::{BoxF, P};
use super::index::TOL;
use super::model::{ObKind, RouterBoard};
use super::scoped::St;

/// Map state: nothing near.
pub(crate) const FREE: u32 = 0;
/// Map state: no net may use this point.
pub(crate) const BLOCKED: u32 = u32::MAX;

/// Whether a static map state lets `net` through (free, or only near its own copper).
#[inline]
pub(crate) fn ok(st: u32, net: u32) -> bool {
    st == FREE || st == net + 1
}

fn combine(st: u32, owned: Option<u32>) -> u32 {
    match (st, owned) {
        (FREE, Some(n)) => n + 1,
        (x, Some(n)) if x == n + 1 => x,
        _ => BLOCKED,
    }
}

/// Extra inflation so that sampling every `s` is exact for clearance `r`.
pub(crate) fn delta(r: f64, s: f64) -> f64 {
    (r * r + s * s / 4.0).sqrt() - r
}

/// The grid geometry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Grid {
    /// Center of cell (0, 0).
    pub ox: f64,
    pub oy: f64,
    /// Cell pitch (nm).
    pub g: f64,
    /// Cells per row / column.
    pub w: i32,
    pub h: i32,
}

impl Grid {
    /// A grid covering `area` with pitch `g`, its origin offset by (`offx`, `offy`) (in
    /// `[0, g)`) to align with pad centers.
    pub fn new(area: BoxF, g: f64, offx: f64, offy: f64) -> Grid {
        let ox = area.min.x + offx - g;
        let oy = area.min.y + offy - g;
        let w = (((area.max.x - ox) / g).ceil() as i32 + 1).max(2);
        let h = (((area.max.y - oy) / g).ceil() as i32 + 1).max(2);
        Grid { ox, oy, g, w, h }
    }

    /// Double-grid width.
    pub fn dw(&self) -> i32 {
        2 * self.w - 1
    }

    /// Double-grid height.
    pub fn dh(&self) -> i32 {
        2 * self.h - 1
    }

    /// Number of cells.
    pub fn cells(&self) -> usize {
        (self.w * self.h) as usize
    }

    /// Double-grid point position.
    pub fn dpt(&self, i: i32, j: i32) -> P {
        P::new(self.ox + i as f64 * self.g / 2.0, self.oy + j as f64 * self.g / 2.0)
    }

    /// Cell center.
    pub fn cell(&self, x: i32, y: i32) -> P {
        self.dpt(2 * x, 2 * y)
    }

    pub fn didx(&self, i: i32, j: i32) -> usize {
        (j * self.dw() + i) as usize
    }

    pub fn cidx(&self, x: i32, y: i32) -> usize {
        (y * self.w + x) as usize
    }

    /// Double-grid index range (inclusive) covering a box, or `None` when outside.
    pub fn drange(&self, b: &BoxF) -> Option<(i32, i32, i32, i32)> {
        let h = self.g / 2.0;
        let i0 = ((b.min.x - self.ox) / h).ceil().max(0.0) as i32;
        let j0 = ((b.min.y - self.oy) / h).ceil().max(0.0) as i32;
        let i1 = (((b.max.x - self.ox) / h).floor() as i64).min(self.dw() as i64 - 1);
        let j1 = (((b.max.y - self.oy) / h).floor() as i64).min(self.dh() as i64 - 1);
        if i1 < i0 as i64 || j1 < j0 as i64 {
            return None;
        }
        Some((i0, j0, i1 as i32, j1 as i32))
    }

    /// Cell range (inclusive) covering a box.
    pub fn crange(&self, b: &BoxF) -> Option<(i32, i32, i32, i32)> {
        let x0 = ((b.min.x - self.ox) / self.g).ceil().max(0.0) as i32;
        let y0 = ((b.min.y - self.oy) / self.g).ceil().max(0.0) as i32;
        let x1 = (((b.max.x - self.ox) / self.g).floor() as i64).min(self.w as i64 - 1);
        let y1 = (((b.max.y - self.oy) / self.g).floor() as i64).min(self.h as i64 - 1);
        if x1 < x0 as i64 || y1 < y0 as i64 {
            return None;
        }
        Some((x0, y0, x1 as i32, y1 as i32))
    }
}

/// Static legality maps per rule profile: track centers on the double grid of each routing
/// layer, via centers on the cells.
pub(crate) struct Statics {
    /// `[profile][slot][double-grid index]`; empty for profiles no routed net uses.
    pub track: Vec<Vec<Vec<u32>>>,
    /// `[profile][cell index]`.
    pub via: Vec<Vec<u32>>,
    /// Scoped rules: the state of new tracks near each double-grid point (`[slot][index]`,
    /// into `states`), covering every point of a grid move through it; empty without
    /// positional rules (every state is the default).
    pub tst: Vec<Vec<u16>>,
    /// The state of a via on each cell.
    pub vst: Vec<u16>,
    /// The distinct states.
    pub states: Vec<St>,
}

/// State rasters of the scoped rules (see [`Statics::tst`]).
fn state_rasters(
    rb: &RouterBoard,
    grid: &Grid,
    slots: &[usize],
    active: &[bool],
) -> (Vec<Vec<u16>>, Vec<u16>, Vec<St>) {
    if !rb.scoped.positional() {
        return (vec![], vec![], vec![St::default()]);
    }
    let act = || rb.profiles.iter().zip(active).filter(|(_, a)| **a).map(|(p, _)| p);
    let hw = act().map(|p| p.hw).fold(0.0, f64::max);
    let rv = act().map(|p| p.rv).fold(0.0, f64::max);
    let inner = act().map(|p| p.inner).fold(0.0, f64::max);
    // A move spans one sample spacing: every point of a move is within that of a sample.
    let slack = grid.g * std::f64::consts::SQRT_2 / 2.0 + inner;
    let mut ids: BTreeMap<St, u16> = BTreeMap::new();
    ids.insert(St::default(), 0);
    let mut states = vec![St::default()];
    let mut intern = |st: St| -> u16 {
        *ids.entry(st).or_insert_with(|| {
            states.push(st);
            (states.len() - 1) as u16
        })
    };
    let tst = slots
        .iter()
        .map(|&layer| {
            let mut v = Vec::with_capacity((grid.dw() * grid.dh()) as usize);
            for j in 0..grid.dh() {
                for i in 0..grid.dw() {
                    v.push(intern(rb.scoped.st(super::scoped::Kind::Track, layer, grid.dpt(i, j), slack, hw)));
                }
            }
            v
        })
        .collect();
    let mut vst = Vec::with_capacity(grid.cells());
    for y in 0..grid.h {
        for x in 0..grid.w {
            vst.push(intern(rb.scoped.st(super::scoped::Kind::Via, 0, grid.cell(x, y), 0.0, rv)));
        }
    }
    (tst, vst, states)
}

/// Inside-board mask on the double grid (scanline over the outline rings, even-odd).
fn inside_mask(rb: &RouterBoard, grid: &Grid) -> Vec<bool> {
    let (dw, dh) = (grid.dw(), grid.dh());
    let mut out = vec![false; (dw * dh) as usize];
    let rings: Vec<&Vec<P>> = rb.outer.iter().chain(rb.cutouts.iter()).flat_map(|p| p.rings.iter()).collect();
    let mut xs: Vec<f64> = Vec::new();
    for j in 0..dh {
        let y = grid.dpt(0, j).y;
        xs.clear();
        for r in &rings {
            let n = r.len();
            for k in 0..n {
                let (a, b) = (r[k], r[(k + 1) % n]);
                if (a.y > y) != (b.y > y) {
                    xs.push((b.x - a.x) * (y - a.y) / (b.y - a.y) + a.x);
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        let mut k = 0;
        for i in 0..dw {
            let x = grid.dpt(i, j).x;
            while k < xs.len() && xs[k] <= x {
                k += 1;
            }
            out[grid.didx(i, j)] = k % 2 == 1;
        }
    }
    out
}

impl Statics {
    /// Builds the maps for the `active` profiles on the routing layers `slots` (stackup
    /// indices).
    pub fn build(rb: &RouterBoard, grid: &Grid, slots: &[usize], active: &[bool]) -> Statics {
        let inside = inside_mask(rb, grid);
        let s_diag = grid.g * std::f64::consts::SQRT_2 / 2.0;
        let (tst, vst, states) = state_rasters(rb, grid, slots, active);
        let scoped = rb.scoped.on;
        let mut track = Vec::new();
        let mut via = Vec::new();
        for (pi, pr) in rb.profiles.iter().enumerate() {
            if !active[pi] {
                track.push(Vec::new());
                via.push(Vec::new());
                continue;
            }
            let mut per_slot = Vec::new();
            // Per state, the clearance to the current obstacle (NaN: not computed yet).
            let mut cache: Vec<f64> = vec![f64::NAN; states.len()];
            for (si, &layer) in slots.iter().enumerate() {
                let bit = 1u64 << layer.min(63);
                let mut m: Vec<u32> = inside.iter().map(|&i| if i { FREE } else { BLOCKED }).collect();
                for ob in &rb.obstacles {
                    if ob.tracks & bit == 0 {
                        continue;
                    }
                    let owned = if matches!(ob.kind, ObKind::Pad | ObKind::Copper) { ob.net } else { None };
                    // Clearance per point when it depends on the state of the copper there.
                    let varies = scoped && ob.item.is_some() && !tst.is_empty();
                    let r_of = |c: f64| {
                        let r = pr.hw + c;
                        r + delta(r, s_diag) - TOL
                    };
                    let hi = if varies {
                        cache.iter_mut().for_each(|c| *c = f64::NAN);
                        let nb = ob.bbox.expand(pr.hw + rb.max_clear() + s_diag);
                        rb.c_ob(pr, rb.scoped.st_box(super::scoped::Kind::Track, &[layer], &nb, pr.hw), ob)
                    } else {
                        rb.c_ob(pr, St::default(), ob)
                    };
                    let reach = r_of(hi);
                    let Some((i0, j0, i1, j1)) = grid.drange(&ob.bbox.expand(reach)) else { continue };
                    for j in j0..=j1 {
                        for i in i0..=i1 {
                            let k = grid.didx(i, j);
                            let st = m[k];
                            if st == BLOCKED || owned.is_some_and(|n| st == n + 1) {
                                continue;
                            }
                            let reach = if varies {
                                let sid = tst[si][k] as usize;
                                if cache[sid].is_nan() {
                                    cache[sid] = r_of(rb.c_ob(pr, states[sid], ob));
                                }
                                cache[sid]
                            } else {
                                reach
                            };
                            if ob.shape.dist_point(grid.dpt(i, j)) < reach {
                                m[k] = combine(st, owned);
                            }
                        }
                    }
                }
                per_slot.push(m);
            }
            track.push(per_slot);
            // Vias: every copper layer (through vias), holes, keep-outs.
            let mut v: Vec<u32> = (0..grid.h)
                .flat_map(|y| (0..grid.w).map(move |x| (x, y)))
                .map(|(x, y)| if inside[grid.didx(2 * x, 2 * y)] { FREE } else { BLOCKED })
                .collect();
            for ob in rb.obstacles.iter().filter(|o| o.vias) {
                let owned = if matches!(ob.kind, ObKind::Pad | ObKind::Copper) { ob.net } else { None };
                let varies = scoped && ob.item.is_some() && !vst.is_empty();
                let hi = if varies {
                    let nb = ob.bbox.expand(rb.via_out(pr, rb.max_clear()));
                    rb.c_ob(pr, rb.scoped.st_box(super::scoped::Kind::Via, &[], &nb, pr.rv), ob)
                } else {
                    rb.c_ob(pr, St::default(), ob)
                };
                let req = rb.via_out(pr, hi) - TOL;
                let Some((x0, y0, x1, y1)) = grid.crange(&ob.bbox.expand(req)) else { continue };
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let k = grid.cidx(x, y);
                        let st = v[k];
                        if st == BLOCKED {
                            continue;
                        }
                        let req =
                            if varies { rb.via_out(pr, rb.c_ob(pr, states[vst[k] as usize], ob)) - TOL } else { req };
                        let d = ob.shape.dist_point(grid.cell(x, y));
                        if ob.kind == ObKind::Pad && d < pr.rv - TOL {
                            v[k] = BLOCKED; // no via in pad
                        } else if d < req && !owned.is_some_and(|n| st == n + 1 && ob.kind == ObKind::Copper) {
                            v[k] = combine(st, owned);
                        }
                    }
                }
            }
            for h in &rb.holes {
                let req = pr.dr + h.r + rb.h2h - TOL;
                let Some((x0, y0, x1, y1)) = grid.crange(&BoxF::of2(h.at, h.at).expand(req)) else { continue };
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        if grid.cell(x, y).dist(h.at) < req {
                            v[grid.cidx(x, y)] = BLOCKED;
                        }
                    }
                }
            }
            via.push(v);
        }
        Statics { track, via, tst, vst, states }
    }
}
