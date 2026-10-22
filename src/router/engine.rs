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

use std::collections::BTreeMap;

use super::geo::{BoxF, P, point_seg_d2};
use super::grid::{Grid, Statics, delta, ok};
use super::index::TOL;
use super::model::RouterBoard;
use super::scoped::St;

/// Direction vectors, counter-clockwise from east.
pub(crate) const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];
/// No direction (start, after a via).
pub(crate) const NONE: u8 = 8;

/// Nodes settled backwards from the targets of a long negotiation search
/// (`Engine::near_costs`); searches get them after twice as many expansions.
const NEAR_COSTS: usize = 5000;
/// Present-congestion factor from which negotiation searches use `Engine::near_costs`
/// (from the sixth iteration on): earlier, the costs around the targets are still close to
/// the distance and the searches rarely flood.
const NEAR_COSTS_PRES: f32 = 5.0;

/// Most nodes `Engine::pocket` visits before deciding the targets are not walled in.
const POCKET: usize = 50_000;

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

/// Straight runs (slot, from, to) of a wire and its via centers.
pub(crate) type WireGeometry = (Vec<(usize, P, P)>, Vec<P>);

/// Cancellation or budget exhaustion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stop;

/// An off-grid connection from a grid cell to copper: its points after the cell, ending at
/// the anchor (pad center or a point on a track). Usually one point (a straight stub); an
/// escape (`fanout::escape`) has a bend.
pub(crate) type Stub = Vec<P>;

/// A grid point where a pad or other copper can be reached.
#[derive(Clone, Debug)]
pub(crate) struct Access {
    pub node: u32,
    /// Stub to the copper when the cell is not on the copper itself.
    pub stub: Option<Stub>,
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

/// A routed path: grid nodes (`slot * cells + cell`), with optional stubs at both ends; or,
/// from the gridless search, a polyline with its slot per vertex (`geo`, `nodes` empty).
#[derive(Clone, Debug)]
pub(crate) struct Wire {
    pub conn: usize,
    pub nodes: Vec<u32>,
    /// Stub from the first node to its copper.
    pub start: Option<Stub>,
    /// Stub from the last node to its copper.
    pub end: Option<Stub>,
    /// Gridless polyline: (slot, point) per vertex; consecutive vertices at one point on two
    /// slots are a via.
    pub geo: Option<Vec<(usize, P)>>,
}

impl Wire {
    /// A gridless wire.
    pub fn gridless(conn: usize, pts: Vec<(usize, P)>) -> Wire {
        Wire { conn, nodes: vec![], start: None, end: None, geo: Some(pts) }
    }
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
    /// Where the net is expected to route (its terminals with a margin): nets whose regions
    /// do not meet are routed in the same batch (see `batches`).
    pub region: BoxF,
    /// Ids of the committed wires' items in `Engine::dynamic`.
    pub dyn_ids: Vec<u32>,
    /// The rooms of the gridless search for this net (static obstacles), kept between
    /// iterations.
    pub rooms: Option<std::sync::Arc<super::expansion::NetRooms>>,
    /// Wires the next [`Engine::route_net`] keeps (by index in `wires`; empty: none), set by
    /// negotiation for nets with many connections whose wires are clear of the overuse.
    pub keep: Vec<bool>,
}

impl Default for NetRoute {
    fn default() -> NetRoute {
        NetRoute {
            net: 0,
            prof: 0,
            islands: vec![],
            conns: vec![],
            wires: vec![],
            failed: vec![],
            keys: vec![],
            bbox: BoxF::EMPTY,
            length: 0.0,
            region: BoxF::EMPTY,
            dyn_ids: vec![],
            rooms: None,
            keep: vec![],
        }
    }
}

/// Most nets routed in one batch.
pub(crate) const MAX_BATCH: usize = 64;

/// Splits an ordered list of nets into batches of nets whose regions do not meet (level
/// scheduling): a net goes into the first batch after every batch holding an earlier net whose
/// region meets its own, so nets that may interact keep their relative order. Depends only on
/// the data (never on the thread count), so results are the same with any number of threads.
pub(crate) fn batches(order: &[usize], routes: &[NetRoute]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut level: Vec<(usize, usize)> = Vec::new(); // (net, batch)
    for &ri in order {
        let r = &routes[ri].region;
        let mut b =
            level.iter().filter(|(o, _)| routes[*o].region.intersects(r)).map(|(_, b)| b + 1).max().unwrap_or(0);
        while b < out.len() && out[b].len() >= MAX_BATCH {
            b += 1;
        }
        if b == out.len() {
            out.push(Vec::new());
        }
        out[b].push(ri);
        level.push((ri, b));
    }
    out
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
    /// Polled during searches (thread-safe: deadline and cancellation flag only).
    pub stop: &'a (dyn Fn() -> bool + Sync),
    /// Whether `route_net` uses the gridless search (`expansion`) instead of the grid.
    pub gridless: bool,
    /// The search option of the run (for reports).
    pub search: super::SearchKind,
    /// Static obstacles and the committed routing (wire segments and vias of every net), for
    /// the gridless search and its exact conflict checks.
    pub dynamic: super::index::Index,
    /// Whether each item of `dynamic` (by id) is off the grid: a stub, or a segment or via of a
    /// gridless wire. The halos see such copper only at the grid's samples.
    offgrid: Vec<bool>,
    /// Clearance regions of the static obstacles (gridless search).
    pub grown: super::rooms::Grown,
    /// Any-angle segments in gridless paths (else 0°/45°/90° where they fit).
    pub any_angle: bool,
    /// Static reachability per profile (searches that cannot succeed are skipped).
    pub comps: super::components::Components,
}

/// Per-search working memory (one per thread).
pub(crate) struct Scratch {
    gc: Vec<f32>,
    parent: Vec<u32>,
    dir: Vec<u8>,
    seen: Vec<u32>,
    tgt: Vec<u32>,
    stamp: u32,
    /// Via cells of the net being routed (earlier wires): new vias keep hole-to-hole from them.
    pub own_vias: Vec<(i32, i32)>,
    /// Via centers of the net being routed (gridless search).
    pub own_via_pts: Vec<P>,
    /// Gridless search memory.
    pub gl: super::expansion::GScratch,
    /// Pocket test (`Engine::pocket`): visited and source marks, by stamp.
    rseen: Vec<u32>,
    rsrc: Vec<u32>,
    rstamp: u32,
    /// The A* queue.
    heap: Queue,
    /// Costs to the targets near them (`Engine::near_costs`).
    lb: NearCosts,
}

/// Exact costs to the targets of a search from the nodes nearest to them (a backward
/// Dijkstra on the search's own costs without bends), by stamp, and the cost below which
/// every node was settled.
#[derive(Default)]
struct NearCosts {
    dist: Vec<f32>,
    seen: Vec<u32>,
    stamp: u32,
    heap: Queue,
}

impl Scratch {
    pub fn new(nodes: usize) -> Scratch {
        Scratch {
            gc: vec![0.0; nodes],
            parent: vec![0; nodes],
            dir: vec![NONE; nodes],
            seen: vec![0; nodes],
            tgt: vec![0; nodes],
            stamp: 0,
            own_vias: Vec::new(),
            own_via_pts: Vec::new(),
            gl: Default::default(),
            rseen: vec![0; nodes],
            rsrc: vec![0; nodes],
            rstamp: 0,
            heap: Queue::default(),
            lb: NearCosts { dist: vec![0.0; nodes], seen: vec![0; nodes], stamp: 0, heap: Queue::default() },
        }
    }
}

/// A 4-ary min-heap of (cost, node), ordered by cost then node: the same order as a binary
/// heap of `Reverse((cost bits, node))` (costs are never negative), so the search pops nodes in
/// exactly the same sequence; one `u64` comparison per step and a shallower tree.
#[derive(Default)]
struct Queue {
    keys: Vec<u64>,
}

impl Queue {
    fn clear(&mut self) {
        self.keys.clear();
    }

    fn push(&mut self, cost: f32, node: u32) {
        let key = ((cost.to_bits() as u64) << 32) | node as u64;
        let k = &mut self.keys;
        let mut i = k.len();
        k.push(key);
        while i > 0 {
            let p = (i - 1) / 4;
            if k[p] <= key {
                break;
            }
            k[i] = k[p];
            i = p;
        }
        k[i] = key;
    }

    fn pop(&mut self) -> Option<(f32, u32)> {
        let k = &mut self.keys;
        let top = *k.first()?;
        let last = k.pop().expect("non-empty");
        let n = k.len();
        if n > 0 {
            let mut i = 0;
            loop {
                let c = 4 * i + 1;
                if c >= n {
                    break;
                }
                let mut m = c;
                for j in c + 1..(c + 4).min(n) {
                    if k[j] < k[m] {
                        m = j;
                    }
                }
                if k[m] >= last {
                    break;
                }
                k[i] = k[m];
                i = m;
            }
            k[i] = last;
        }
        Some((f32::from_bits((top >> 32) as u32), top as u32))
    }
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
        stop: &'a (dyn Fn() -> bool + Sync),
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
        let mut dynamic =
            super::index::Index::new(rb.bbox.expand(grid.g * 4.0), (grid.g * 8.0).max(500_000.0), super::reach(rb));
        for (i, ob) in rb.obstacles.iter().enumerate() {
            dynamic.insert(super::index::Item::Static(i as u32), ob.bbox);
        }
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
            stop,
            gridless: false,
            search: super::SearchKind::Grid,
            dynamic,
            offgrid: Vec::new(),
            grown: super::rooms::Grown::new(rb),
            any_angle: false,
            comps: super::components::Components::new(rb.profiles.len()),
        }
    }

    pub fn cells(&self) -> usize {
        self.grid.cells()
    }

    /// Working memory for [`Engine::search`].
    pub fn scratch(&self) -> Scratch {
        Scratch::new(self.cells() * self.slots.len())
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

    pub(crate) fn key(&self, ai: usize, map: usize, idx: usize) -> u64 {
        (((ai * (self.slots.len() + 1) + map) as u64) << 32) | idx as u64
    }

    // ---- occupancy ----------------------------------------------------------------------

    /// Halo keys of a track segment of `net` on `slot`.
    fn raster_seg(&self, net: u32, slot: usize, a: P, b: P, offgrid: bool, out: &mut Vec<u64>) {
        let pn = self.rb.profile(net);
        if !self.rb.scoped.on {
            self.raster_piece(net, slot, a, b, St::default(), offgrid, out);
            return;
        }
        for (pa, pb, st) in self.rb.pieces(pn, self.slots[slot], a, b, pn.hw) {
            self.raster_piece(net, slot, pa, pb, st, offgrid, out);
        }
    }

    /// Halo keys of a piece of track of `net` on `slot` in state `sn`.
    #[allow(clippy::too_many_arguments)]
    fn raster_piece(&self, net: u32, slot: usize, a: P, b: P, sn: St, offgrid: bool, out: &mut Vec<u64>) {
        let pn = self.rb.profile(net);
        let s_diag = self.grid.g * std::f64::consts::SQRT_2 / 2.0;
        let bb = BoxF::of2(a, b);
        let scoped = self.rb.scoped.positional();
        let mut cache: Vec<f64> = Vec::new();
        for (ai, &pi) in self.active.iter().enumerate() {
            let pp = &self.rb.profiles[pi];
            // Track centers of `pp` near the piece, per state there.
            let radius = |c: f64| {
                let mut r = pn.hw + pp.hw + c - TOL;
                if offgrid {
                    r += delta(r, s_diag);
                }
                r
            };
            let hi = if scoped { self.rb.max_clear() } else { self.rb.c_cu(pn, sn, pp, St::default()) };
            let r = radius(hi);
            cache.clear();
            cache.resize(self.st.states.len(), f64::NAN);
            if let Some((i0, j0, i1, j1)) = self.grid.drange(&bb.expand(r)) {
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        let k = self.grid.didx(i, j);
                        let d2 = point_seg_d2(self.grid.dpt(i, j), a, b);
                        if d2 >= r * r {
                            continue;
                        }
                        let rk = if scoped {
                            let sid = if self.st.tst.is_empty() { 0 } else { self.st.tst[slot][k] as usize };
                            if cache[sid].is_nan() {
                                cache[sid] = radius(self.rb.c_cu(pn, sn, pp, self.st.states[sid]));
                            }
                            cache[sid]
                        } else {
                            r
                        };
                        if d2 < rk * rk {
                            out.push(self.key(ai, slot, k));
                        }
                    }
                }
            }
            // Via centers of `pp` near the piece.
            let vradius = |c: f64| pn.hw + self.rb.via_out(pp, c) - TOL;
            let rv = vradius(hi);
            cache.clear();
            cache.resize(self.st.states.len(), f64::NAN);
            if let Some((x0, y0, x1, y1)) = self.grid.crange(&bb.expand(rv)) {
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let k = self.grid.cidx(x, y);
                        let d2 = point_seg_d2(self.grid.cell(x, y), a, b);
                        if d2 >= rv * rv {
                            continue;
                        }
                        let rk = if scoped {
                            let sid = if self.st.vst.is_empty() { 0 } else { self.st.vst[k] as usize };
                            if cache[sid].is_nan() {
                                cache[sid] = vradius(self.rb.c_cu(pn, sn, pp, self.st.states[sid]));
                            }
                            cache[sid]
                        } else {
                            rv
                        };
                        if d2 < rk * rk {
                            out.push(self.key(ai, self.slots.len(), k));
                        }
                    }
                }
            }
        }
    }

    /// Halo keys of a through via of `net` at `at` (a cell center, or anywhere with `offgrid`,
    /// which widens the track halo by the sampling margin).
    fn raster_via(&self, net: u32, at: P, offgrid: bool, out: &mut Vec<u64>) {
        let pn = self.rb.profile(net);
        let s_diag = self.grid.g * std::f64::consts::SQRT_2 / 2.0;
        let scoped = self.rb.scoped.positional();
        let sn = self.rb.st_via(pn, at, 0.0);
        let mut cache: Vec<f64> = Vec::new();
        for (ai, &pi) in self.active.iter().enumerate() {
            let pp = &self.rb.profiles[pi];
            let radius = |c: f64| {
                let mut r = self.rb.via_out(pn, c) + pp.hw - TOL;
                if offgrid {
                    r += delta(r, s_diag);
                }
                r
            };
            let hi = if scoped { self.rb.max_clear() } else { self.rb.c_cu(pn, sn, pp, St::default()) };
            let r = radius(hi);
            if let Some((i0, j0, i1, j1)) = self.grid.drange(&BoxF::of2(at, at).expand(r)) {
                for s in 0..self.slots.len() {
                    cache.clear();
                    cache.resize(self.st.states.len(), f64::NAN);
                    for j in j0..=j1 {
                        for i in i0..=i1 {
                            let k = self.grid.didx(i, j);
                            let d = self.grid.dpt(i, j).dist(at);
                            if d >= r {
                                continue;
                            }
                            let rk = if scoped {
                                let sid = if self.st.tst.is_empty() { 0 } else { self.st.tst[s][k] as usize };
                                if cache[sid].is_nan() {
                                    cache[sid] = radius(self.rb.c_cu(pn, sn, pp, self.st.states[sid]));
                                }
                                cache[sid]
                            } else {
                                r
                            };
                            if d < rk {
                                out.push(self.key(ai, s, k));
                            }
                        }
                    }
                }
            }
            let vradius = |c: f64| self.rb.via_via(pn, pp, c) - TOL;
            let rv = vradius(hi);
            cache.clear();
            cache.resize(self.st.states.len(), f64::NAN);
            if let Some((x0, y0, x1, y1)) = self.grid.crange(&BoxF::of2(at, at).expand(rv)) {
                for yy in y0..=y1 {
                    for xx in x0..=x1 {
                        let k = self.grid.cidx(xx, yy);
                        let d = self.grid.cell(xx, yy).dist(at);
                        if d >= rv {
                            continue;
                        }
                        let rk = if scoped {
                            let sid = if self.st.vst.is_empty() { 0 } else { self.st.vst[k] as usize };
                            if cache[sid].is_nan() {
                                cache[sid] = vradius(self.rb.c_cu(pn, sn, pp, self.st.states[sid]));
                            }
                            cache[sid]
                        } else {
                            rv
                        };
                        if d < rk {
                            out.push(self.key(ai, self.slots.len(), k));
                        }
                    }
                }
            }
        }
    }

    /// Straight runs of a wire: (slot, from, to) and via centers.
    pub fn wire_geometry(&self, w: &Wire) -> WireGeometry {
        let mut segs = Vec::new();
        let mut vias = Vec::new();
        if let Some(g) = &w.geo {
            for k in 1..g.len() {
                let ((s0, a), (s1, b)) = (g[k - 1], g[k]);
                if s0 != s1 {
                    vias.push(a);
                } else if a != b {
                    segs.push((s0, a, b));
                }
            }
            if g.len() == 1 {
                segs.push((g[0].0, g[0].1, g[0].1));
            }
            return (segs, vias);
        }
        let n = &w.nodes;
        if let (Some(st), Some(&first)) = (&w.start, n.first()) {
            let (sl, _, _) = self.unpack(first);
            let mut prev = self.node_pos(first);
            let mut rev = Vec::new();
            for &q in st {
                rev.push((sl, q, prev));
                prev = q;
            }
            segs.extend(rev.into_iter().rev());
        }
        let mut i = 0;
        while i + 1 < n.len() {
            let (s0, x0, y0) = self.unpack(n[i]);
            let (s1, x1, y1) = self.unpack(n[i + 1]);
            if s0 != s1 {
                vias.push(self.grid.cell(x0, y0));
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
        if let (Some(en), Some(&last)) = (&w.end, n.last()) {
            let (sl, _, _) = self.unpack(last);
            let mut prev = self.node_pos(last);
            for &q in en {
                segs.push((sl, prev, q));
                prev = q;
            }
        }
        (segs, vias)
    }

    /// Claims the halo of every wire of a net.
    pub fn commit(&mut self, nr: &mut NetRoute) {
        let mut keys = Vec::new();
        let mut bbox = BoxF::EMPTY;
        let pr = self.rb.profile(nr.net);
        let (hw, rv) = (pr.hw, pr.rv);
        for w in &nr.wires {
            let (segs, vias) = self.wire_geometry(w);
            let n_start = w.start.as_ref().map_or(0, Vec::len);
            let n_end = w.end.as_ref().map_or(0, Vec::len);
            let geo = w.geo.is_some();
            for (k, (s, a, b)) in segs.iter().enumerate() {
                let off = geo || k < n_start || k + n_end >= segs.len();
                self.raster_seg(nr.net, *s, *a, *b, off, &mut keys);
                bbox.add(*a);
                bbox.add(*b);
                let item = super::index::Item::Seg { net: nr.net, layer: self.slots[*s] as u8, a: *a, b: *b };
                let id = self.dynamic.insert(item, BoxF::of2(*a, *b).expand(hw));
                self.mark(id, off);
                nr.dyn_ids.push(id);
            }
            for at in vias {
                self.raster_via(nr.net, at, geo, &mut keys);
                let item = super::index::Item::Via { net: nr.net, at };
                let id = self.dynamic.insert(item, BoxF::of2(at, at).expand(rv));
                self.mark(id, geo);
                nr.dyn_ids.push(id);
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

    /// Records whether item `id` of `dynamic` is off the grid.
    fn mark(&mut self, id: u32, off: bool) {
        let i = id as usize;
        if self.offgrid.len() <= i {
            self.offgrid.resize(i + 1, false);
        }
        self.offgrid[i] = off;
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
        for id in nr.dyn_ids.drain(..) {
            self.dynamic.remove(id);
        }
    }

    /// Points where a committed net overlaps another net's halo: (slot or `None` for via
    /// cells, index).
    pub fn conflicts(&self, nr: &NetRoute) -> Vec<Conflict> {
        let ai = self.act[nr.prof];
        let mut out = Vec::new();
        for w in &nr.wires {
            if w.geo.is_some() {
                self.geo_conflicts(nr.net, w, &mut out);
                continue;
            }
            // Stubs are off the grid: the halos see them only at the grid's samples, so a stub
            // can come too close to another net's stub (or gridless wire) between samples.
            // Checked exactly against those; grid copper is in the halos already.
            if w.start.is_some() || w.end.is_some() {
                let (segs, _) = self.wire_geometry(w);
                let n_start = w.start.as_ref().map_or(0, Vec::len);
                let n_end = w.end.as_ref().map_or(0, Vec::len);
                let stubs: Vec<(usize, P, P)> = segs
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k < n_start || k + n_end >= segs.len())
                    .map(|(_, s)| *s)
                    .collect();
                self.hits(nr.net, &stubs, &[], true, &mut |slot, p, _| {
                    if let Some(s) = slot {
                        out.push((Some(s), self.near_dpt(p)));
                    }
                });
            }
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

    /// Exact conflicts of a gridless wire with the committed routing of other nets: the
    /// double-grid point (or via cell) nearest to each place where it comes too close.
    fn geo_conflicts(&self, net: u32, w: &Wire, out: &mut Vec<Conflict>) {
        let near_d = |q: P| self.near_dpt(q);
        let near_c = |q: P| -> usize {
            let g = self.grid.g;
            let x = (((q.x - self.grid.ox) / g).round() as i32).clamp(0, self.grid.w - 1);
            let y = (((q.y - self.grid.oy) / g).round() as i32).clamp(0, self.grid.h - 1);
            self.grid.cidx(x, y)
        };
        self.geo_hits(net, w, &mut |slot, p, _| match slot {
            Some(s) => out.push((Some(s), near_d(p))),
            None => out.push((None, near_c(p))),
        });
    }

    /// Index of the double-grid point nearest to `q`.
    fn near_dpt(&self, q: P) -> usize {
        let h = self.grid.g / 2.0;
        let i = (((q.x - self.grid.ox) / h).round() as i32).clamp(0, self.grid.dw() - 1);
        let j = (((q.y - self.grid.oy) / h).round() as i32).clamp(0, self.grid.dh() - 1);
        self.grid.didx(i, j)
    }

    /// Whether the stub `stub` from grid node `node` of `net` keeps clear of the committed
    /// routing of other nets (exact).
    fn stub_clear(&self, net: u32, node: u32, stub: &[P]) -> bool {
        let (s, _, _) = self.unpack(node);
        let mut prev = self.node_pos(node);
        let mut segs = Vec::new();
        for &q in stub {
            segs.push((s, prev, q));
            prev = q;
        }
        let mut clear = true;
        self.hits(net, &segs, &[], false, &mut |_, _, _| clear = false);
        clear
    }

    /// The nets whose committed routing a gridless wire of `net` comes too close to.
    pub fn geo_blockers(&self, net: u32, w: &Wire) -> Vec<u32> {
        let mut out = Vec::new();
        self.geo_hits(net, w, &mut |_, _, m| out.push(m));
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Calls `f(slot or None for a via, where, other net)` for every place where the gridless
    /// wire `w` of `net` comes closer to the committed routing of another net than the rules
    /// allow (exact).
    fn geo_hits(&self, net: u32, w: &Wire, f: &mut dyn FnMut(Option<usize>, P, u32)) {
        let (segs, vias) = self.wire_geometry(w);
        self.hits(net, &segs, &vias, false, f);
    }

    /// Calls `f(slot or None for a via, where, other net)` for every place where the
    /// segments `segs` (slot, from, to) and vias `vias` of `net` come closer to the committed
    /// routing of another net than the rules allow (exact; only its off-grid copper with
    /// `offgrid`).
    fn hits(
        &self,
        net: u32,
        segs: &[(usize, P, P)],
        vias: &[P],
        offgrid: bool,
        f: &mut dyn FnMut(Option<usize>, P, u32),
    ) {
        let skip = |id: u32| offgrid && !self.offgrid.get(id as usize).copied().unwrap_or(false);
        use super::index::Item;
        let rb = self.rb;
        let pr = rb.profile(net);
        for &(s, a, b) in segs {
            let layer = self.slots[s];
            let mine = if rb.scoped.on { rb.pieces(pr, layer, a, b, pr.hw) } else { vec![(a, b, St::default())] };
            let q = BoxF::of2(a, b).expand(pr.hw + self.dynamic.reach);
            for (id, it) in self.dynamic.query(&q) {
                if skip(id) {
                    continue;
                }
                let hit = match it {
                    Item::Seg { net: m, layer: l, a: c, b: d } if m != net && l as usize == layer => {
                        let po = rb.profile(m);
                        rb.segs_conflict(pr, layer, &mine, 0.0, po, c, d, po.hw).then(|| (closest_on(a, b, c, d), m))
                    }
                    Item::Via { net: m, at } if m != net => {
                        let po = rb.profile(m);
                        rb.seg_via_conflict(pr, &mine, 0.0, po, at).then(|| (super::geo::project(at, a, b), m))
                    }
                    _ => None,
                };
                if let Some((p, m)) = hit {
                    f(Some(s), p, m);
                }
            }
        }
        for &at in vias {
            let st = rb.st_via(pr, at, 0.0);
            let q = BoxF::of2(at, at).expand(pr.rv + self.dynamic.reach);
            for (id, it) in self.dynamic.query(&q) {
                if skip(id) {
                    continue;
                }
                let hit = match it {
                    Item::Seg { net: m, layer, a, b } if m != net => {
                        let po = rb.profile(m);
                        rb.via_seg_conflict(pr, at, st, po, layer as usize, a, b, po.hw).then_some(m)
                    }
                    Item::Via { net: m, at: o } if m != net => {
                        let po = rb.profile(m);
                        let req = rb.via_via(pr, po, rb.c_cu(pr, st, po, rb.st_via(po, o, 0.0)));
                        (at.dist(o) < req - TOL).then_some(m)
                    }
                    _ => None,
                };
                if let Some(m) = hit {
                    f(None, at, m);
                }
            }
        }
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
                    stubs.push(Access { node, stub: Some(vec![anchor]), cost: (len / self.grid.g) as f32 + 0.5 });
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
        &self,
        sc: &mut Scratch,
        net: u32,
        prof: usize,
        sources: &[(u32, f32)],
        targets: &[u32],
        mode: Mode,
    ) -> Result<Option<Vec<u32>>, Stop> {
        let pr = &self.rb.profiles[prof];
        let own_min = 2.0 * pr.dr + self.rb.h2h - TOL;
        if sources.is_empty() || targets.is_empty() {
            return Ok(None);
        }
        sc.stamp = sc.stamp.wrapping_add(1);
        if sc.stamp == 0 {
            sc.seen.iter_mut().for_each(|v| *v = 0);
            sc.tgt.iter_mut().for_each(|v| *v = 0);
            sc.stamp = 1;
        }
        let stamp = sc.stamp;
        // Negotiation passes other nets' routing, so no path exists when the static graph
        // separates the ends; neither does one for the stricter searches.
        if mode != Mode::Explain && !self.comps.may_reach(&self.grid, &self.st, prof, net, sources, targets) {
            return Ok(None);
        }
        if mode == Mode::Hard && self.pocket(sc, net, prof, sources, targets) {
            return Ok(None);
        }
        let mut tb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &t in targets {
            sc.tgt[t as usize] = stamp;
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
        // In negotiation the congestion near the targets is a cost every path pays, which
        // the distance alone does not see: costs to the targets settled backwards around them
        // raise the estimate (`near_costs`).
        let mut lb = std::mem::take(&mut sc.lb);
        // Set once the search has run long enough to be worth it (`near_costs`).
        let mut floor = 0.0f32;
        let heur = |lb: &NearCosts, floor: f32, n: usize, x: i32, y: i32| -> f32 {
            let dx = (tb.0 - x).max(x - tb.2).max(0) as f32;
            let dy = (tb.1 - y).max(y - tb.3).max(0) as f32;
            let (a, b) = if dx > dy { (dx, dy) } else { (dy, dx) };
            let h = (a + (std::f32::consts::SQRT_2 - 1.0) * b) * w;
            if floor > 0.0 {
                let d = if lb.seen[n] == lb.stamp { lb.dist[n].min(floor) } else { floor };
                h.max(d)
            } else {
                h
            }
        };
        sc.heap.clear();
        for &(n, c0) in sources {
            let (s, x, y) = self.unpack(n);
            let k = self.grid.didx(2 * x, 2 * y);
            let occ = self.occ_t[ai][s][k] as f32;
            if mode == Mode::Hard && occ > 0.0 {
                continue;
            }
            let c = c0 + self.hist_t[s][k] + pres * occ;
            let i = n as usize;
            if sc.seen[i] == stamp && sc.gc[i] <= c {
                continue;
            }
            sc.seen[i] = stamp;
            sc.gc[i] = c;
            sc.parent[i] = u32::MAX;
            sc.dir[i] = NONE;
            sc.heap.push(c + heur(&lb, floor, i, x, y), n);
        }
        let cells = self.cells();
        let ns = self.slots.len();
        let (gw, gh) = (self.grid.w, self.grid.h);
        let dw = self.grid.dw() as isize;
        // Index offsets of each direction's move midpoint and end on the double grid, and of
        // its end cell.
        let mid_off: [isize; 8] = DIRS.map(|(dx, dy)| dy as isize * dw + dx as isize);
        let cell_off: [isize; 8] = DIRS.map(|(dx, dy)| dy as isize * gw as isize + dx as isize);
        // Step costs per direction on each slot (diagonal, wrong way, along).
        let base_cost: Vec<[f32; 8]> = (0..ns)
            .map(|s| {
                DIRS.map(|(dx, dy)| {
                    if dx != 0 && dy != 0 {
                        std::f32::consts::SQRT_2 * self.costs.diag
                    } else if (self.pref[s] == 0 && dy != 0) || (self.pref[s] == 1 && dx != 0) {
                        self.costs.wrong_way
                    } else {
                        1.0
                    }
                })
            })
            .collect();
        let bend_cost = [0.0, self.costs.bend45, self.costs.bend90];
        let mut pops = 0usize;
        let block_pen = 60.0f32;
        while let Some((f, u)) = sc.heap.pop() {
            let ui = u as usize;
            let (s, x, y) = self.unpack(u);
            let gu = sc.gc[ui];
            if f > gu + heur(&lb, floor, ui, x, y) + 1e-3 {
                continue; // stale
            }
            if sc.tgt[ui] == stamp {
                let mut path = vec![u];
                let mut c = u;
                while sc.parent[c as usize] != u32::MAX {
                    c = sc.parent[c as usize];
                    path.push(c);
                }
                path.reverse();
                sc.lb = lb;
                return Ok(Some(path));
            }
            pops += 1;
            if pops & 4095 == 0 && (self.stop)() {
                sc.lb = lb;
                return Err(Stop);
            }
            if pops > self.max_expansions {
                sc.lb = lb;
                return Ok(None);
            }
            // A long search late in negotiation: raise the estimate by the costs around the
            // targets. Entries already queued keep their lower keys, which only makes them come
            // out earlier; a node found cheaper later is expanded again, so paths stay optimal.
            if pops == 2 * NEAR_COSTS && pres >= NEAR_COSTS_PRES && mode != Mode::Hard {
                floor = self.near_costs(&mut lb, net, prof, targets, pres, NEAR_COSTS);
            }
            let du = sc.dir[ui];
            let tmap = &self.st.track[prof][s][..];
            let occ_s = &self.occ_t[ai][s][..];
            let hist_s = &self.hist_t[s][..];
            let k0 = self.grid.didx(2 * x, 2 * y) as isize;
            let c0i = (s * cells) as isize + self.grid.cidx(x, y) as isize;
            let interior = x > 0 && y > 0 && x + 1 < gw && y + 1 < gh;
            for (nd, &(dx, dy)) in DIRS.iter().enumerate() {
                let t = if du == NONE { 0 } else { turn(du, nd as u8) };
                if t >= 3 {
                    continue;
                }
                if !interior {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= gw || ny >= gh {
                        continue;
                    }
                }
                let km = (k0 + mid_off[nd]) as usize;
                let kq = (k0 + 2 * mid_off[nd]) as usize;
                let mut pen = 0.0;
                if !ok(tmap[km], net) || !ok(tmap[kq], net) {
                    if mode != Mode::Explain {
                        continue;
                    }
                    pen += block_pen;
                }
                let occ = (occ_s[km] + occ_s[kq]) as f32;
                if occ > 0.0 {
                    match mode {
                        Mode::Hard => continue,
                        Mode::Explain => pen += block_pen,
                        Mode::Negotiate(_) => {}
                    }
                }
                let base = base_cost[s][nd];
                let hist = hist_s[km] + hist_s[kq];
                let bend = bend_cost[t as usize];
                let c = gu + (base + hist) * (1.0 + pres * occ) + bend + pen;
                let vi = (c0i + cell_off[nd]) as usize;
                if sc.seen[vi] == stamp && sc.gc[vi] <= c {
                    continue;
                }
                sc.seen[vi] = stamp;
                sc.gc[vi] = c;
                sc.parent[vi] = u;
                sc.dir[vi] = nd as u8;
                sc.heap.push(c + heur(&lb, floor, vi, x + dx, y + dy), vi as u32);
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
                // Hole-to-hole with the net's own vias from earlier wires.
                if mode != Mode::Explain
                    && sc.own_vias.iter().any(|&(vx, vy)| {
                        let (dx, dy) = ((vx - x) as f64, (vy - y) as f64);
                        (vx, vy) != (x, y) && (dx * dx + dy * dy).sqrt() * self.grid.g < own_min
                    })
                {
                    continue;
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
                let k = k0 as usize;
                for s2 in 0..ns {
                    if s2 == s {
                        continue;
                    }
                    if !ok(self.st.track[prof][s2][k], net) && mode != Mode::Explain {
                        continue;
                    }
                    let v = (s2 * cells) as u32 + cell as u32;
                    let vi = v as usize;
                    if sc.seen[vi] == stamp && sc.gc[vi] <= c0 {
                        continue;
                    }
                    sc.seen[vi] = stamp;
                    sc.gc[vi] = c0;
                    sc.parent[vi] = u;
                    sc.dir[vi] = NONE;
                    sc.heap.push(c0 + heur(&lb, floor, vi, x, y), v);
                }
            }
        }
        sc.lb = lb;
        Ok(None)
    }

    /// Costs to `targets` of a negotiation search (present factor `pres`) from the nodes
    /// around them: a backward Dijkstra over the moves the search may make, on its costs
    /// without bends (so never more than what the search pays), settling up to `limit` nodes.
    /// Returns the cost of the last node settled: every node not settled costs at least that
    /// much to reach a target. The exact costs are left in `lb` (by stamp); the search's
    /// estimate at a node is the larger of the distance and the cost found (capped at the
    /// returned floor), still a lower bound and consistent.
    fn near_costs(&self, lb: &mut NearCosts, net: u32, prof: usize, targets: &[u32], pres: f32, limit: usize) -> f32 {
        lb.stamp = lb.stamp.wrapping_add(1);
        if lb.stamp == 0 {
            lb.seen.iter_mut().for_each(|v| *v = 0);
            lb.stamp = 1;
        }
        let stamp = lb.stamp;
        let ai = self.act[prof];
        let cells = self.cells();
        let ns = self.slots.len();
        let (gw, gh) = (self.grid.w, self.grid.h);
        lb.heap.clear();
        for &t in targets {
            let i = t as usize;
            if lb.seen[i] != stamp {
                lb.seen[i] = stamp;
                lb.dist[i] = 0.0;
                lb.heap.push(0.0, t);
            }
        }
        let base = |s: usize, dx: i32, dy: i32| -> f32 {
            if dx != 0 && dy != 0 {
                std::f32::consts::SQRT_2 * self.costs.diag
            } else if (self.pref[s] == 0 && dy != 0) || (self.pref[s] == 1 && dx != 0) {
                self.costs.wrong_way
            } else {
                1.0
            }
        };
        let mut settled = 0usize;
        while let Some((d, v)) = lb.heap.pop() {
            let vi = v as usize;
            if d > lb.dist[vi] {
                continue;
            }
            settled += 1;
            if settled >= limit {
                return d;
            }
            let (s, x, y) = self.unpack(v);
            let tmap = &self.st.track[prof][s];
            let occ_s = &self.occ_t[ai][s];
            let hist_s = &self.hist_t[s];
            let kq = self.grid.didx(2 * x, 2 * y);
            if !ok(tmap[kq], net) {
                continue;
            }
            // Moves u -> v along the layer: u = v - (dx, dy), midpoint between them.
            for &(dx, dy) in DIRS.iter() {
                let (ux, uy) = (x - dx, y - dy);
                if ux < 0 || uy < 0 || ux >= gw || uy >= gh {
                    continue;
                }
                let km = self.grid.didx(2 * x - dx, 2 * y - dy);
                if !ok(tmap[km], net) {
                    continue;
                }
                let occ = (occ_s[km] + occ_s[kq]) as f32;
                let c = d + (base(s, dx, dy) + hist_s[km] + hist_s[kq]) * (1.0 + pres * occ);
                let ui = s * cells + self.grid.cidx(ux, uy);
                if lb.seen[ui] == stamp && lb.dist[ui] <= c {
                    continue;
                }
                lb.seen[ui] = stamp;
                lb.dist[ui] = c;
                lb.heap.push(c, ui as u32);
            }
            // Vias u (another slot, same cell) -> v.
            if self.vias_allowed {
                let cell = self.grid.cidx(x, y);
                if !ok(self.st.via[prof][cell], net) {
                    continue;
                }
                let occ = self.occ_v[ai][cell] as f32;
                let c = d + (self.costs.via + self.hist_v[cell]) * (1.0 + pres * occ);
                for s2 in (0..ns).filter(|&s2| s2 != s) {
                    let ui = s2 * cells + cell;
                    if lb.seen[ui] == stamp && lb.dist[ui] <= c {
                        continue;
                    }
                    lb.seen[ui] = stamp;
                    lb.dist[ui] = c;
                    lb.heap.push(c, ui as u32);
                }
            }
        }
        // Everything reachable was settled: nodes not seen cannot reach a target at all.
        f32::INFINITY
    }

    /// Whether the targets of a `Mode::Hard` search lie in a small pocket that no source
    /// reaches: everything from which a target can be entered (static maps and other nets'
    /// routing as the search sees them, ignoring its bend limit and the spacing of the net's
    /// own vias, so a superset of what it reaches) is visited backwards from the targets, up to
    /// `POCKET` nodes. A failing search would otherwise visit every node reachable from the
    /// sources before giving up, typically the whole board around a walled-in pad.
    fn pocket(&self, sc: &mut Scratch, net: u32, prof: usize, sources: &[(u32, f32)], targets: &[u32]) -> bool {
        sc.rstamp = sc.rstamp.wrapping_add(1);
        if sc.rstamp == 0 {
            sc.rseen.iter_mut().for_each(|v| *v = 0);
            sc.rsrc.iter_mut().for_each(|v| *v = 0);
            sc.rstamp = 1;
        }
        let stamp = sc.rstamp;
        let ai = self.act[prof];
        let cells = self.cells() as u32;
        let ns = self.slots.len();
        // Legal for the net, and also not near other nets' routing. A move along a layer
        // needs its midpoint and end free; a via needs its cell free and its end legal.
        let legal = |s: usize, i: i32, j: i32| ok(self.st.track[prof][s][self.grid.didx(i, j)], net);
        let free = |s: usize, i: i32, j: i32| legal(s, i, j) && self.occ_t[ai][s][self.grid.didx(i, j)] == 0;
        for &(n, _) in sources {
            let (s, x, y) = self.unpack(n);
            if self.occ_t[ai][s][self.grid.didx(2 * x, 2 * y)] == 0 {
                sc.rsrc[n as usize] = stamp;
            }
        }
        let mut queue: Vec<u32> = Vec::new();
        for &t in targets {
            let (s, x, y) = self.unpack(t);
            if sc.rsrc[t as usize] == stamp {
                return false;
            }
            if legal(s, 2 * x, 2 * y) && sc.rseen[t as usize] != stamp {
                sc.rseen[t as usize] = stamp;
                queue.push(t);
            }
        }
        let mut head = 0;
        while head < queue.len() {
            if queue.len() > POCKET {
                return false;
            }
            let v = queue[head];
            head += 1;
            let (s, x, y) = self.unpack(v);
            let mut reach = |u: u32, enter: bool, queue: &mut Vec<u32>| -> bool {
                if sc.rsrc[u as usize] == stamp {
                    return true;
                }
                if enter && sc.rseen[u as usize] != stamp {
                    sc.rseen[u as usize] = stamp;
                    queue.push(u);
                }
                false
            };
            for &(dx, dy) in DIRS.iter().filter(|_| free(s, 2 * x, 2 * y)) {
                let (ux, uy) = (x - dx, y - dy);
                if ux < 0 || uy < 0 || ux >= self.grid.w || uy >= self.grid.h || !free(s, 2 * ux + dx, 2 * uy + dy) {
                    continue;
                }
                let u = (s as u32) * cells + self.grid.cidx(ux, uy) as u32;
                if reach(u, legal(s, 2 * ux, 2 * uy), &mut queue) {
                    return false;
                }
            }
            if self.vias_allowed {
                let cell = self.grid.cidx(x, y);
                if ok(self.st.via[prof][cell], net) && self.occ_v[ai][cell] == 0 {
                    for s2 in (0..ns).filter(|&s2| s2 != s) {
                        let u = (s2 as u32) * cells + cell as u32;
                        if reach(u, legal(s2, 2 * x, 2 * y), &mut queue) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Occupancy keys (as in `NetRoute::keys`) along the cheapest path of connection `ci` of
    /// `nr` when everything is passable at a penalty: matching them against other nets' keys
    /// tells which nets are in the way.
    pub fn blocking_keys(&self, sc: &mut Scratch, nr: &NetRoute, ci: usize) -> Result<Vec<u64>, Stop> {
        let c = &nr.conns[ci];
        let sources: Vec<(u32, f32)> = nr.islands[c.a].access.iter().map(|a| (a.node, a.cost)).collect();
        let targets: Vec<u32> = nr.islands[c.b].access.iter().map(|a| a.node).collect();
        let Some(path) = self.search(sc, nr.net, nr.prof, &sources, &targets, Mode::Explain)? else {
            return Ok(vec![]);
        };
        let ai = self.act[nr.prof];
        let ns = self.slots.len();
        let mut keys = Vec::new();
        for w in path.windows(2) {
            let (s0, x0, y0) = self.unpack(w[0]);
            let (s1, x1, y1) = self.unpack(w[1]);
            if s0 != s1 {
                keys.push(self.key(ai, ns, self.grid.cidx(x0, y0)));
                continue;
            }
            keys.push(self.key(ai, s0, self.grid.didx(x0 + x1, y0 + y1)));
            keys.push(self.key(ai, s0, self.grid.didx(2 * x1, 2 * y1)));
        }
        keys.sort_unstable();
        keys.dedup();
        Ok(keys)
    }

    /// Routes every connection of a net (its wires are replaced, except, in negotiation, the
    /// ones `nr.keep` marks that still join its pads: see [`kept_wires`]). Connections whose
    /// islands are already joined by earlier or kept wires are skipped.
    pub fn route_net(&self, sc: &mut Scratch, nr: &mut NetRoute, mode: Mode) -> Result<(), Stop> {
        let keep = std::mem::take(&mut nr.keep);
        if self.gridless {
            return super::expansion::route_net(self, sc, nr, mode);
        }
        let old = std::mem::take(&mut nr.wires);
        sc.own_vias.clear();
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
        // Kept wires (partial rip-up): their islands start joined, their nodes in the trees.
        if keep.len() == old.len() && matches!(mode, Mode::Negotiate(_)) {
            let kept = kept_wires(nr, old, &keep);
            let cells = self.cells() as u32;
            for (isls, w) in kept {
                let r0 = find(&mut parent, isls[0]);
                for &i in &isls[1..] {
                    let (ra, rb) = (find(&mut parent, r0), find(&mut parent, i));
                    if ra != rb {
                        let (ma, mb) = (std::mem::take(&mut members[rb]), std::mem::take(&mut tree[rb]));
                        members[ra].extend(ma);
                        tree[ra].extend(mb);
                        parent[rb] = ra;
                    }
                }
                let r = find(&mut parent, r0);
                tree[r].extend(w.nodes.iter().copied());
                for q in w.nodes.windows(2) {
                    if q[0] / cells != q[1] / cells {
                        let (_, x, y) = self.unpack(q[0]);
                        sc.own_vias.push((x, y));
                    }
                }
                nr.wires.push(w);
            }
        }
        for ci in 0..nr.conns.len() {
            if matches!(nr.failed[ci], Some(FailKind::Static | FailKind::NoAccess | FailKind::Drc)) {
                continue;
            }
            nr.failed[ci] = None;
            let (ra, rb) = (find(&mut parent, nr.conns[ci].a), find(&mut parent, nr.conns[ci].b));
            if ra == rb {
                continue;
            }
            let mut src: BTreeMap<u32, (f32, Option<&Stub>)> = BTreeMap::new();
            for &isl in &members[ra] {
                for a in &nr.islands[isl].access {
                    let e = src.entry(a.node).or_insert((a.cost, a.stub.as_ref()));
                    if a.cost < e.0 {
                        *e = (a.cost, a.stub.as_ref());
                    }
                }
            }
            for &t in &tree[ra] {
                src.insert(t, (0.0, None));
            }
            let mut dst: BTreeMap<u32, Option<&Stub>> = BTreeMap::new();
            for &isl in &members[rb] {
                for a in &nr.islands[isl].access {
                    let better = match dst.get(&a.node) {
                        None => true,
                        Some(s) => s.is_some() && a.stub.is_none(),
                    };
                    if better {
                        dst.insert(a.node, a.stub.as_ref());
                    }
                }
            }
            for &t in &tree[rb] {
                dst.insert(t, None);
            }
            // Searches that may not share keep their stubs clear of other nets' routing too
            // (the halos see a stub only at the grid's samples).
            let had = !src.is_empty() && !dst.is_empty();
            if mode == Mode::Hard {
                src.retain(|&n, (_, st)| st.is_none_or(|st| self.stub_clear(nr.net, n, st)));
                dst.retain(|&n, st| st.is_none_or(|st| self.stub_clear(nr.net, n, st)));
            }
            if src.is_empty() || dst.is_empty() {
                nr.failed[ci] = Some(if had { FailKind::Congestion } else { FailKind::NoAccess });
                continue;
            }
            let sources: Vec<(u32, f32)> = src.iter().map(|(k, v)| (*k, v.0)).collect();
            let targets: Vec<u32> = dst.keys().copied().collect();
            match self.search(sc, nr.net, nr.prof, &sources, &targets, mode)? {
                Some(path) => {
                    let start = src[&path[0]].1.cloned();
                    let end = dst[path.last().expect("path")].cloned();
                    let (ma, mb) = (std::mem::take(&mut members[rb]), std::mem::take(&mut tree[rb]));
                    members[ra].extend(ma);
                    tree[ra].extend(mb);
                    tree[ra].extend(path.iter().copied());
                    parent[rb] = ra;
                    let cells = self.cells() as u32;
                    for w in path.windows(2) {
                        if w[0] / cells != w[1] / cells {
                            let (_, x, y) = self.unpack(w[0]);
                            sc.own_vias.push((x, y));
                        }
                    }
                    nr.wires.push(Wire { conn: ci, nodes: path, start, end, geo: None });
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

/// The wires of `old` marked in `keep` that still join islands of `nr` on their own: both
/// ends on an island's access node (with the wire's stub) or on a node of another such wire
/// (dropped until that holds), and each group of wires so joined reaching an island. Returns, per wire, the islands its group
/// joins (one wire of a group carries them, the others an island of the group), in `old`'s
/// order.
fn kept_wires(nr: &NetRoute, old: Vec<Wire>, keep: &[bool]) -> Vec<(Vec<usize>, Wire)> {
    // Access nodes: (island, stub) per node.
    let mut access: BTreeMap<u32, Vec<(usize, Option<&Stub>)>> = BTreeMap::new();
    for (i, isl) in nr.islands.iter().enumerate() {
        for a in &isl.access {
            access.entry(a.node).or_default().push((i, a.stub.as_ref()));
        }
    }
    // The islands an end of a wire reaches: an access at its node whose stub (if any) is the
    // wire's.
    let ends = |q: u32, st: Option<&Stub>| -> Vec<usize> {
        access.get(&q).map_or(vec![], |v| v.iter().filter(|(_, s)| s.is_none() || *s == st).map(|(i, _)| *i).collect())
    };
    let mut kept: Vec<Wire> = old
        .into_iter()
        .zip(keep)
        .filter(|(w, k)| **k && w.geo.is_none() && w.nodes.len() > 1)
        .map(|(w, _)| w)
        .collect();
    loop {
        let mut on: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (j, w) in kept.iter().enumerate() {
            for &q in &w.nodes {
                let v = on.entry(q).or_default();
                if v.last() != Some(&j) {
                    v.push(j);
                }
            }
        }
        let held = |j: usize, q: u32, st: Option<&Stub>| {
            !ends(q, st).is_empty() || on.get(&q).is_some_and(|v| v.iter().any(|&o| o != j))
        };
        let before = kept.len();
        let ok: Vec<bool> = kept
            .iter()
            .enumerate()
            .map(|(j, w)| {
                held(j, w.nodes[0], w.start.as_ref()) && held(j, *w.nodes.last().expect("nodes"), w.end.as_ref())
            })
            .collect();
        let mut it = ok.into_iter();
        kept.retain(|_| it.next().expect("flag"));
        if kept.len() == before {
            break;
        }
    }
    // Groups: union-find over the wires, joined where they share a node.
    let m = kept.len();
    let mut parent: Vec<usize> = (0..m).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut first: BTreeMap<u32, usize> = BTreeMap::new();
    for (j, w) in kept.iter().enumerate() {
        for &q in &w.nodes {
            let o = *first.entry(q).or_insert(j);
            let (a, b) = (find(&mut parent, o), find(&mut parent, j));
            if a != b {
                parent[b.max(a)] = a.min(b);
            }
        }
    }
    let mut isls: Vec<Vec<usize>> = vec![Vec::new(); m];
    for (j, w) in kept.iter().enumerate() {
        let r = find(&mut parent, j);
        isls[r].extend(ends(w.nodes[0], w.start.as_ref()));
        isls[r].extend(ends(*w.nodes.last().expect("nodes"), w.end.as_ref()));
    }
    for v in &mut isls {
        v.sort_unstable();
        v.dedup();
    }
    let roots: Vec<usize> = (0..m).map(|j| find(&mut parent, j)).collect();
    kept.into_iter()
        .enumerate()
        .filter(|(j, _)| !isls[roots[*j]].is_empty())
        .map(|(j, w)| {
            let r = roots[j];
            let v = if r == j { isls[r].clone() } else { vec![isls[r][0]] };
            (v, w)
        })
        .collect()
}

/// The point of segment `a`–`b` closest to segment `c`–`d`.
fn closest_on(a: P, b: P, c: P, d: P) -> P {
    let cands = [a, b, super::geo::project(c, a, b), super::geo::project(d, a, b)];
    let mut best = cands[0];
    let mut bd = f64::MAX;
    for q in cands {
        let e = point_seg_d2(q, c, d);
        if e < bd {
            bd = e;
            best = q;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_wires_must_still_reach_the_pads() {
        let isl = |node: u32| Island { terms: vec![], access: vec![Access { node, stub: None, cost: 0.0 }] };
        let wire = |conn: usize, nodes: Vec<u32>| Wire { conn, nodes, start: None, end: None, geo: None };
        let nr = NetRoute { islands: vec![isl(1), isl(10), isl(20)], ..NetRoute::default() };
        // w0 joins islands 0 and 1; w1 starts on w0 and ends on island 2.
        let old = || vec![wire(0, vec![1, 2, 3, 10]), wire(1, vec![3, 4, 20])];
        let groups = |keep: &[bool]| -> Vec<(Vec<usize>, usize)> {
            kept_wires(&nr, old(), keep).into_iter().map(|(i, w)| (i, w.conn)).collect()
        };
        assert_eq!(groups(&[true, true]), vec![(vec![0, 1, 2], 0), (vec![0], 1)]);
        assert_eq!(groups(&[true, false]), vec![(vec![0, 1], 0)]);
        // Without w0, w1 hangs from nothing.
        assert_eq!(groups(&[false, true]), vec![]);
    }

    #[test]
    fn queue_pops_like_a_binary_heap() {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        let mut rng = Rng(7);
        let mut q = Queue::default();
        let mut h: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        for round in 0..200 {
            for _ in 0..(round % 7) {
                let cost = (rng.next() % 50) as f32 * 0.25;
                let node = (rng.next() % 1000) as u32;
                q.push(cost, node);
                h.push(Reverse((cost.to_bits(), node)));
            }
            for _ in 0..(round % 3) {
                let a = q.pop();
                let b = h.pop().map(|Reverse((c, n))| (f32::from_bits(c), n));
                assert_eq!(a, b);
            }
        }
        while let Some(Reverse((c, n))) = h.pop() {
            assert_eq!(q.pop(), Some((f32::from_bits(c), n)));
        }
        assert_eq!(q.pop(), None);
    }
}
