//! Grid search and negotiated congestion.
//!
//! Every routed net claims a *halo* on the occupancy maps: the double-grid points where the
//! centerline of a track (or the center of a via) of another net would come closer than the
//! clearance, one map per rule profile. A* charges a present-congestion cost for entering
//! claimed points plus a history cost that grows on points that stay overused, as in
//! PathFinder (McMurchie and Ebeling, FPGA 1995): every net is first routed allowing sharing,
//! then nets involved in overuse are ripped up and rerouted with rising costs until no point is
//! shared or the iteration budget runs out. A final legalization pass reroutes what still
//! conflicts with sharing forbidden.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use super::geo::{BoxF, P, point_seg_d2};
use super::grid::{Grid, Statics, delta, ok};
use super::index::TOL;
use super::model::RouterBoard;

/// Direction vectors, counter-clockwise from east.
pub(crate) const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
/// No direction (start, after a via).
pub(crate) const NONE: u8 = 8;

/// Search costs (grid units: one orthogonal step costs 1).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Costs {
    pub via: f32,
    pub bend45: f32,
    pub bend90: f32,
    pub wrong_way: f32,
    pub diag: f32,
    /// Heuristic weight (1 = optimal A*).
    pub weight: f32,
}

/// Search mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mode {
    /// Sharing allowed at a present-congestion cost.
    Negotiate(f32),
    /// No sharing.
    Hard,
    /// Everything passable at a penalty: finds what blocks a failed connection.
    Explain,
}

/// An overused point: (slot, double-grid index) or (`None`, via cell index).
pub(crate) type Conflict = (Option<usize>, usize);

/// Straight runs (slot, from, to) of a wire and its via cells.
pub(crate) type WireGeometry = (Vec<(usize, P, P)>, Vec<(i32, i32)>);

/// Cancellation or budget exhaustion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stop;

/// A grid point where a pad or other copper can be reached.
#[derive(Clone, Debug)]
pub(crate) struct Access {
    pub node: u32,
    /// Stub end (pad center or point on a track) when the cell is not on the copper itself.
    pub stub: Option<P>,
    pub cost: f32,
}

/// A copper island of a net (already-connected copper).
#[derive(Clone, Debug, Default)]
pub(crate) struct Island {
    pub terms: Vec<usize>,
    pub access: Vec<Access>,
}

/// A connection to make: join island `a` with island `b`.
#[derive(Clone, Debug)]
pub(crate) struct Conn {
    pub a: usize,
    pub b: usize,
    pub from: String,
    pub to: String,
    pub from_at: P,
    pub to_at: P,
}

/// A routed path: grid nodes (`slot * cells + cell`), with optional stubs at both ends.
#[derive(Clone, Debug)]
pub(crate) struct Wire {
    pub conn: usize,
    pub nodes: Vec<u32>,
    pub start: Option<P>,
    pub end: Option<P>,
}

/// Why a connection could not be routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailKind {
    /// Not routable even ignoring other new routes (obstacles, rules).
    Static,
    /// Blocked by other routed nets.
    Congestion,
    /// A pad has no legal access point.
    NoAccess,
    /// Removed after DRC verification.
    Drc,
    /// Search budget or time ran out.
    Budget,
}

/// Routing state of one net.
#[derive(Clone, Debug)]
pub(crate) struct NetRoute {
    pub net: u32,
    pub prof: usize,
    pub islands: Vec<Island>,
    pub conns: Vec<Conn>,
    pub wires: Vec<Wire>,
    pub failed: Vec<Option<FailKind>>,
    /// Committed occupancy keys.
    pub keys: Vec<u64>,
    pub bbox: BoxF,
    /// Ratsnest length, for ordering.
    pub length: f64,
}

/// A small deterministic RNG (SplitMix64).
pub(crate) struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// The search engine and its maps.
pub(crate) struct Engine<'a> {
    pub rb: &'a RouterBoard,
    pub grid: Grid,
    /// Routing layers (stackup indices), by slot.
    pub slots: Vec<usize>,
    pub st: Statics,
    /// Profile index → index into the occupancy maps (`usize::MAX`: inactive).
    pub act: Vec<usize>,
    pub active: Vec<usize>,
    /// `[active profile][slot][double-grid point]`.
    pub occ_t: Vec<Vec<Vec<u16>>>,
    /// `[active profile][cell]`.
    pub occ_v: Vec<Vec<u16>>,
    pub hist_t: Vec<Vec<f32>>,
    pub hist_v: Vec<f32>,
    pub costs: Costs,
    /// Preferred direction per slot: 0 horizontal, 1 vertical, 2 none.
    pub pref: Vec<u8>,
    pub vias_allowed: bool,
    pub max_expansions: usize,
    // Search scratch.
    gc: Vec<f32>,
    parent: Vec<u32>,
    dir: Vec<u8>,
    seen: Vec<u32>,
    tgt: Vec<u32>,
    stamp: u32,
    pub stop: &'a dyn Fn() -> bool,
}

fn turn(a: u8, b: u8) -> u8 {
    let d = (a as i32 - b as i32).unsigned_abs() as u8;
    d.min(8 - d)
}

impl<'a> Engine<'a> {
    pub fn new(
        rb: &'a RouterBoard,
        grid: Grid,
        slots: Vec<usize>,
        active_profiles: &[bool],
        costs: Costs,
        stop: &'a dyn Fn() -> bool,
    ) -> Engine<'a> {
        let st = Statics::build(rb, &grid, &slots, active_profiles);
        let mut act = vec![usize::MAX; rb.profiles.len()];
        let mut active = Vec::new();
        for (i, a) in active_profiles.iter().enumerate() {
            if *a {
                act[i] = active.len();
                active.push(i);
            }
        }
        let dn = (grid.dw() * grid.dh()) as usize;
        let cells = grid.cells();
        let ns = slots.len();
        let pref = if ns < 2 { vec![2; ns] } else { (0..ns).map(|s| (s % 2) as u8).collect() };
        Engine {
            rb,
            grid,
            occ_t: active.iter().map(|_| vec![vec![0u16; dn]; ns]).collect(),
            occ_v: active.iter().map(|_| vec![0u16; cells]).collect(),
            hist_t: vec![vec![0.0; dn]; ns],
            hist_v: vec![0.0; cells],
            vias_allowed: ns > 1,
            slots,
            st,
            act,
            active,
            costs,
            pref,
            max_expansions: usize::MAX,
            gc: vec![0.0; cells * ns],
            parent: vec![0; cells * ns],
            dir: vec![NONE; cells * ns],
            seen: vec![0; cells * ns],
            tgt: vec![0; cells * ns],
            stamp: 0,
            stop,
        }
    }

    pub fn cells(&self) -> usize {
        self.grid.cells()
    }

    /// Node → (slot, x, y).
    pub fn unpack(&self, n: u32) -> (usize, i32, i32) {
        let c = self.cells() as u32;
        let (s, r) = (n / c, n % c);
        (s as usize, (r % self.grid.w as u32) as i32, (r / self.grid.w as u32) as i32)
    }

    pub fn pack(&self, s: usize, x: i32, y: i32) -> u32 {
        (s * self.cells()) as u32 + self.grid.cidx(x, y) as u32
    }

    pub fn node_pos(&self, n: u32) -> P {
        let (_, x, y) = self.unpack(n);
        self.grid.cell(x, y)
    }

    fn key(&self, ai: usize, map: usize, idx: usize) -> u64 {
        (((ai * (self.slots.len() + 1) + map) as u64) << 32) | idx as u64
    }

    // ---- occupancy ----------------------------------------------------------------------

    /// Halo keys of a track segment of `net` on `slot`.
    fn raster_seg(&self, net: u32, slot: usize, a: P, b: P, offgrid: bool, out: &mut Vec<u64>) {
        let pn = self.rb.profile(net);
        let s_diag = self.grid.g * std::f64::consts::SQRT_2 / 2.0;
        let bb = BoxF::of2(a, b);
        for (ai, &pi) in self.active.iter().enumerate() {
            let pp = &self.rb.profiles[pi];
            let c = pn.c.max(pp.c);
            let mut r = pn.hw + pp.hw + c - TOL;
            if offgrid {
                r += delta(r, s_diag);
            }
            if let Some((i0, j0, i1, j1)) = self.grid.drange(&bb.expand(r)) {
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        if point_seg_d2(self.grid.dpt(i, j), a, b) < r * r {
                            out.push(self.key(ai, slot, self.grid.didx(i, j)));
                        }
                    }
                }
            }
            let rv = pn.hw + pp.rv + c - TOL;
            if let Some((x0, y0, x1, y1)) = self.grid.crange(&bb.expand(rv)) {
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        if point_seg_d2(self.grid.cell(x, y), a, b) < rv * rv {
                            out.push(self.key(ai, self.slots.len(), self.grid.cidx(x, y)));
                        }
                    }
                }
            }
        }
    }

    /// Halo keys of a through via of `net` at cell (x, y).
    fn raster_via(&self, net: u32, x: i32, y: i32, out: &mut Vec<u64>) {
        let pn = self.rb.profile(net);
        let at = self.grid.cell(x, y);
        for (ai, &pi) in self.active.iter().enumerate() {
            let pp = &self.rb.profiles[pi];
            let c = pn.c.max(pp.c);
            let r = pn.rv + pp.hw + c - TOL;
            if let Some((i0, j0, i1, j1)) = self.grid.drange(&BoxF::of2(at, at).expand(r)) {
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        if self.grid.dpt(i, j).dist(at) < r {
                            for s in 0..self.slots.len() {
                                out.push(self.key(ai, s, self.grid.didx(i, j)));
                            }
                        }
                    }
                }
            }
            let rv = (pn.rv + pp.rv + c).max(pn.dr + pp.dr + self.rb.h2h) - TOL;
            if let Some((x0, y0, x1, y1)) = self.grid.crange(&BoxF::of2(at, at).expand(rv)) {
                for yy in y0..=y1 {
                    for xx in x0..=x1 {
                        if self.grid.cell(xx, yy).dist(at) < rv {
                            out.push(self.key(ai, self.slots.len(), self.grid.cidx(xx, yy)));
                        }
                    }
                }
            }
        }
    }

    /// Straight runs of a wire: (slot, from, to) and via cells.
    pub fn wire_geometry(&self, w: &Wire) -> WireGeometry {
        let mut segs = Vec::new();
        let mut vias = Vec::new();
        let n = &w.nodes;
        if let (Some(s), Some(&first)) = (w.start, n.first()) {
            let (sl, _, _) = self.unpack(first);
            segs.push((sl, s, self.node_pos(first)));
        }
        let mut i = 0;
        while i + 1 < n.len() {
            let (s0, x0, y0) = self.unpack(n[i]);
            let (s1, x1, y1) = self.unpack(n[i + 1]);
            if s0 != s1 {
                vias.push((x0, y0));
                i += 1;
                continue;
            }
            let d = (x1 - x0, y1 - y0);
            let mut j = i + 1;
            while j + 1 < n.len() {
                let (s2, x2, y2) = self.unpack(n[j + 1]);
                let (_, xj, yj) = self.unpack(n[j]);
                if s2 != s0 || (x2 - xj, y2 - yj) != d {
                    break;
                }
                j += 1;
            }
            segs.push((s0, self.node_pos(n[i]), self.node_pos(n[j])));
            i = j;
        }
        if let (Some(e), Some(&last)) = (w.end, n.last()) {
            let (sl, _, _) = self.unpack(last);
            segs.push((sl, self.node_pos(last), e));
        }
        (segs, vias)
    }

    /// Claims the halo of every wire of a net.
    pub fn commit(&mut self, nr: &mut NetRoute) {
        let mut keys = Vec::new();
        let mut bbox = BoxF::EMPTY;
        for w in &nr.wires {
            let (segs, vias) = self.wire_geometry(w);
            let stub_ends = [w.start, w.end];
            for (k, (s, a, b)) in segs.iter().enumerate() {
                let off = (k == 0 && stub_ends[0].is_some()) || (k + 1 == segs.len() && stub_ends[1].is_some());
                self.raster_seg(nr.net, *s, *a, *b, off, &mut keys);
                bbox.add(*a);
                bbox.add(*b);
            }
            for (x, y) in vias {
                self.raster_via(nr.net, x, y, &mut keys);
            }
            if w.nodes.len() == 1 {
                // A single access cell joining two stubs: claim the cell itself.
                let p = self.node_pos(w.nodes[0]);
                let (s, _, _) = self.unpack(w.nodes[0]);
                self.raster_seg(nr.net, s, p, p, false, &mut keys);
            }
        }
        keys.sort_unstable();
        keys.dedup();
        let ns = self.slots.len();
        for &k in &keys {
            let (m, idx) = ((k >> 32) as usize, (k & 0xFFFF_FFFF) as usize);
            let (ai, map) = (m / (ns + 1), m % (ns + 1));
            if map == ns {
                self.occ_v[ai][idx] = self.occ_v[ai][idx].saturating_add(1);
            } else {
                self.occ_t[ai][map][idx] = self.occ_t[ai][map][idx].saturating_add(1);
            }
        }
        nr.keys = keys;
        nr.bbox = bbox;
    }

    /// Releases a net's halo.
    pub fn uncommit(&mut self, nr: &mut NetRoute) {
        let ns = self.slots.len();
        for &k in &nr.keys {
            let (m, idx) = ((k >> 32) as usize, (k & 0xFFFF_FFFF) as usize);
            let (ai, map) = (m / (ns + 1), m % (ns + 1));
            if map == ns {
                self.occ_v[ai][idx] = self.occ_v[ai][idx].saturating_sub(1);
            } else {
                self.occ_t[ai][map][idx] = self.occ_t[ai][map][idx].saturating_sub(1);
            }
        }
        nr.keys.clear();
    }

    /// Points where a committed net overlaps another net's halo: (slot or `None` for via
    /// cells, index).
    pub fn conflicts(&self, nr: &NetRoute) -> Vec<Conflict> {
        let ai = self.act[nr.prof];
        let mut out = Vec::new();
        for w in &nr.wires {
            for (k, &n) in w.nodes.iter().enumerate() {
                let (s, x, y) = self.unpack(n);
                if k == 0 {
                    let idx = self.grid.didx(2 * x, 2 * y);
                    if self.occ_t[ai][s][idx] > 1 {
                        out.push((Some(s), idx));
                    }
                    continue;
                }
                let (ps, px, py) = self.unpack(w.nodes[k - 1]);
                if ps != s {
                    let idx = self.grid.cidx(x, y);
                    if self.occ_v[ai][idx] > 1 {
                        out.push((None, idx));
                    }
                    continue;
                }
                for (i, j) in [(px + x, py + y), (2 * x, 2 * y)] {
                    let idx = self.grid.didx(i, j);
                    if self.occ_t[ai][s][idx] > 1 {
                        out.push((Some(s), idx));
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Position of a conflict point.
    pub fn conflict_pos(&self, c: Conflict) -> P {
        match c.0 {
            Some(_) => {
                let dw = self.grid.dw() as usize;
                self.grid.dpt((c.1 % dw) as i32, (c.1 / dw) as i32)
            }
            None => {
                let w = self.grid.w as usize;
                self.grid.cell((c.1 % w) as i32, (c.1 / w) as i32)
            }
        }
    }

    pub fn add_history(&mut self, c: Conflict, amount: f32) {
        match c.0 {
            Some(s) => self.hist_t[s][c.1] += amount,
            None => self.hist_v[c.1] += amount,
        }
    }

    // ---- access points ------------------------------------------------------------------

    /// Grid access points of terminal `ti` for its net: cells on its copper (no stub), else
    /// cells nearby with a legal straight stub to its anchor.
    pub fn access(&self, ti: usize, checker: &super::index::Checker<'_>) -> Vec<Access> {
        let t = &self.rb.terminals[ti];
        let pr = self.rb.profile(t.net);
        let pi = self.rb.net_profile[t.net as usize];
        let mut out = Vec::new();
        let reach = 1.6 * self.grid.g;
        let Some((x0, y0, x1, y1)) = self.grid.crange(&t.shape.bbox().expand(reach)) else { return out };
        for (s, &layer) in self.slots.iter().enumerate() {
            if t.layers & (1u64 << layer.min(63)) == 0 {
                continue;
            }
            let map = &self.st.track[pi][s];
            let mut stubs: Vec<Access> = Vec::new();
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if !ok(map[self.grid.didx(2 * x, 2 * y)], t.net) {
                        continue;
                    }
                    let c = self.grid.cell(x, y);
                    let d = t.shape.dist_point(c);
                    let node = self.pack(s, x, y);
                    if d <= 0.0 || (d < pr.hw * 0.5 && t.shape.contains(c)) {
                        out.push(Access { node, stub: None, cost: 0.0 });
                        continue;
                    }
                    let anchor = match t.anchor {
                        super::model::Anchor::Center(a) => a,
                        super::model::Anchor::Segment(a, b) => super::geo::project(c, a, b),
                    };
                    let len = c.dist(anchor);
                    if len
                        > reach + t.shape.bbox().max.x - t.shape.bbox().min.x + t.shape.bbox().max.y
                            - t.shape.bbox().min.y
                    {
                        continue;
                    }
                    if checker.seg(layer, anchor, c, t.net).is_some() {
                        continue;
                    }
                    stubs.push(Access { node, stub: Some(anchor), cost: (len / self.grid.g) as f32 + 0.5 });
                }
            }
            stubs.sort_by(|a, b| a.cost.total_cmp(&b.cost).then(a.node.cmp(&b.node)));
            out.extend(stubs.into_iter().take(12));
        }
        out
    }

    // ---- search -------------------------------------------------------------------------

    /// A* from `sources` (node, initial cost) to any of `targets`. Returns the node path.
    pub fn search(
        &mut self,
        net: u32,
        prof: usize,
        sources: &[(u32, f32)],
        targets: &[u32],
        mode: Mode,
    ) -> Result<Option<Vec<u32>>, Stop> {
        if sources.is_empty() || targets.is_empty() {
            return Ok(None);
        }
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen.iter_mut().for_each(|v| *v = 0);
            self.tgt.iter_mut().for_each(|v| *v = 0);
            self.stamp = 1;
        }
        let stamp = self.stamp;
        let mut tb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &t in targets {
            self.tgt[t as usize] = stamp;
            let (_, x, y) = self.unpack(t);
            tb = (tb.0.min(x), tb.1.min(y), tb.2.max(x), tb.3.max(y));
        }
        let ai = self.act[prof];
        let pres = match mode {
            Mode::Negotiate(p) => p,
            Mode::Hard => 0.0,
            Mode::Explain => 0.0,
        };
        let w = self.costs.weight;
        let heur = |x: i32, y: i32| -> f32 {
            let dx = (tb.0 - x).max(x - tb.2).max(0) as f32;
            let dy = (tb.1 - y).max(y - tb.3).max(0) as f32;
            let (a, b) = if dx > dy { (dx, dy) } else { (dy, dx) };
            (a + (std::f32::consts::SQRT_2 - 1.0) * b) * w
        };
        let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        for &(n, c0) in sources {
            let (s, x, y) = self.unpack(n);
            let k = self.grid.didx(2 * x, 2 * y);
            let occ = self.occ_t[ai][s][k] as f32;
            if mode == Mode::Hard && occ > 0.0 {
                continue;
            }
            let c = c0 + self.hist_t[s][k] + pres * occ;
            let i = n as usize;
            if self.seen[i] == stamp && self.gc[i] <= c {
                continue;
            }
            self.seen[i] = stamp;
            self.gc[i] = c;
            self.parent[i] = u32::MAX;
            self.dir[i] = NONE;
            heap.push(Reverse(((c + heur(x, y)).to_bits(), n)));
        }
        let cells = self.cells();
        let ns = self.slots.len();
        let mut pops = 0usize;
        let block_pen = 60.0f32;
        while let Some(Reverse((fb, u))) = heap.pop() {
            let ui = u as usize;
            let (s, x, y) = self.unpack(u);
            let gu = self.gc[ui];
            if f32::from_bits(fb) > gu + heur(x, y) + 1e-3 {
                continue; // stale
            }
            if self.tgt[ui] == stamp {
                let mut path = vec![u];
                let mut c = u;
                while self.parent[c as usize] != u32::MAX {
                    c = self.parent[c as usize];
                    path.push(c);
                }
                path.reverse();
                return Ok(Some(path));
            }
            pops += 1;
            if pops & 4095 == 0 && (self.stop)() {
                return Err(Stop);
            }
            if pops > self.max_expansions {
                return Ok(None);
            }
            let du = self.dir[ui];
            let tmap = &self.st.track[prof][s];
            for (nd, &(dx, dy)) in DIRS.iter().enumerate() {
                let t = if du == NONE { 0 } else { turn(du, nd as u8) };
                if t >= 3 {
                    continue;
                }
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= self.grid.w || ny >= self.grid.h {
                    continue;
                }
                let km = self.grid.didx(2 * x + dx, 2 * y + dy);
                let kq = self.grid.didx(2 * nx, 2 * ny);
                let mut pen = 0.0;
                if !ok(tmap[km], net) || !ok(tmap[kq], net) {
                    if mode != Mode::Explain {
                        continue;
                    }
                    pen += block_pen;
                }
                let occ = (self.occ_t[ai][s][km] + self.occ_t[ai][s][kq]) as f32;
                if occ > 0.0 {
                    match mode {
                        Mode::Hard => continue,
                        Mode::Explain => pen += block_pen,
                        Mode::Negotiate(_) => {}
                    }
                }
                let diag = dx != 0 && dy != 0;
                let base = if diag {
                    std::f32::consts::SQRT_2 * self.costs.diag
                } else if (self.pref[s] == 0 && dy != 0) || (self.pref[s] == 1 && dx != 0) {
                    self.costs.wrong_way
                } else {
                    1.0
                };
                let hist = self.hist_t[s][km] + self.hist_t[s][kq];
                let bend = match t {
                    1 => self.costs.bend45,
                    2 => self.costs.bend90,
                    _ => 0.0,
                };
                let c = gu + (base + hist) * (1.0 + pres * occ) + bend + pen;
                let v = (s * cells) as u32 + self.grid.cidx(nx, ny) as u32;
                let vi = v as usize;
                if self.seen[vi] == stamp && self.gc[vi] <= c {
                    continue;
                }
                self.seen[vi] = stamp;
                self.gc[vi] = c;
                self.parent[vi] = u;
                self.dir[vi] = nd as u8;
                heap.push(Reverse(((c + heur(nx, ny)).to_bits(), v)));
            }
            if self.vias_allowed {
                let cell = self.grid.cidx(x, y);
                let mut pen = 0.0;
                let legal = ok(self.st.via[prof][cell], net);
                if !legal {
                    if mode != Mode::Explain {
                        continue;
                    }
                    pen += block_pen;
                }
                let occ = self.occ_v[ai][cell] as f32;
                if occ > 0.0 {
                    match mode {
                        Mode::Hard => continue,
                        Mode::Explain => pen += block_pen,
                        Mode::Negotiate(_) => {}
                    }
                }
                let c0 = gu + (self.costs.via + self.hist_v[cell]) * (1.0 + pres * occ) + pen;
                for s2 in 0..ns {
                    if s2 == s {
                        continue;
                    }
                    let k = self.grid.didx(2 * x, 2 * y);
                    if !ok(self.st.track[prof][s2][k], net) && mode != Mode::Explain {
                        continue;
                    }
                    let v = (s2 * cells) as u32 + cell as u32;
                    let vi = v as usize;
                    if self.seen[vi] == stamp && self.gc[vi] <= c0 {
                        continue;
                    }
                    self.seen[vi] = stamp;
                    self.gc[vi] = c0;
                    self.parent[vi] = u;
                    self.dir[vi] = NONE;
                    heap.push(Reverse(((c0 + heur(x, y)).to_bits(), v)));
                }
            }
        }
        Ok(None)
    }

    /// Routes every connection of a net (its wires are replaced). Connections whose islands
    /// are already joined by earlier wires are skipped.
    pub fn route_net(&mut self, nr: &mut NetRoute, mode: Mode) -> Result<(), Stop> {
        nr.wires.clear();
        let n = nr.islands.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let mut tree: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
        for ci in 0..nr.conns.len() {
            if matches!(nr.failed[ci], Some(FailKind::Static | FailKind::NoAccess | FailKind::Drc)) {
                continue;
            }
            nr.failed[ci] = None;
            let (ra, rb) = (find(&mut parent, nr.conns[ci].a), find(&mut parent, nr.conns[ci].b));
            if ra == rb {
                continue;
            }
            let mut src: BTreeMap<u32, (f32, Option<P>)> = BTreeMap::new();
            for &isl in &members[ra] {
                for a in &nr.islands[isl].access {
                    let e = src.entry(a.node).or_insert((a.cost, a.stub));
                    if a.cost < e.0 {
                        *e = (a.cost, a.stub);
                    }
                }
            }
            for &t in &tree[ra] {
                src.insert(t, (0.0, None));
            }
            let mut dst: BTreeMap<u32, Option<P>> = BTreeMap::new();
            for &isl in &members[rb] {
                for a in &nr.islands[isl].access {
                    let better = match dst.get(&a.node) {
                        None => true,
                        Some(s) => s.is_some() && a.stub.is_none(),
                    };
                    if better {
                        dst.insert(a.node, a.stub);
                    }
                }
            }
            for &t in &tree[rb] {
                dst.insert(t, None);
            }
            if src.is_empty() || dst.is_empty() {
                nr.failed[ci] = Some(FailKind::NoAccess);
                continue;
            }
            let sources: Vec<(u32, f32)> = src.iter().map(|(k, v)| (*k, v.0)).collect();
            let targets: Vec<u32> = dst.keys().copied().collect();
            match self.search(nr.net, nr.prof, &sources, &targets, mode)? {
                Some(path) => {
                    let start = src[&path[0]].1;
                    let end = dst[path.last().expect("path")];
                    let (ma, mb) = (std::mem::take(&mut members[rb]), std::mem::take(&mut tree[rb]));
                    members[ra].extend(ma);
                    tree[ra].extend(mb);
                    tree[ra].extend(path.iter().copied());
                    parent[rb] = ra;
                    nr.wires.push(Wire { conn: ci, nodes: path, start, end });
                }
                None => {
                    nr.failed[ci] = Some(match mode {
                        Mode::Negotiate(_) if self.max_expansions == usize::MAX => FailKind::Static,
                        Mode::Negotiate(_) => FailKind::Budget,
                        _ => FailKind::Congestion,
                    });
                }
            }
        }
        Ok(())
    }
}
