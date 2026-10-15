//! Static reachability on the routing grid: which grid nodes a net can reach from which,
//! ignoring other nets' routing.
//!
//! In negotiation every node the static maps let a net through is passable (other nets'
//! routing only costs), so a connection whose ends lie in different connected components of the
//! static graph has no path, and searching for one would visit every node reachable from its
//! sources first. The components are computed once per rule profile over the points that are
//! free for every net, by union-find; a net adds the points only it may use (near its own pads
//! and copper), merged per net on first use. The graph ignores the search's bend limit and the
//! hole-to-hole spacing between a net's own vias, so it can only join more nodes than the search
//! reaches: "different components" proves that no path exists, and skipping such a search
//! never changes a result.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use super::engine::DIRS;
use super::grid::{FREE, Grid, Statics, ok};

/// No label (a node that is not free for every net).
const NONE: u32 = u32::MAX;
/// Tag bit of a node without a label (only some net may use it).
const VIRTUAL: u64 = 1 << 40;

/// Per net: the double-grid points (slot, index) and the via cells only it may use.
type Owned = BTreeMap<u32, (Vec<(u32, u32)>, Vec<u32>)>;

/// Components of one profile's static graph.
pub(crate) struct ProfileComponents {
    /// Root node per node (`NONE` when not free for every net).
    label: Vec<u32>,
    /// Double-grid points (slot, index) and via cells usable by one net only, per net.
    owned: Owned,
    /// Merged components per net (lazily).
    nets: Mutex<BTreeMap<u32, Arc<NetComponents>>>,
}

/// A net's merges: tag → root tag (tags absent are their own root).
pub(crate) struct NetComponents {
    root: BTreeMap<u64, u64>,
}

/// Components per profile, built on first use.
pub(crate) struct Components {
    profiles: Vec<OnceLock<ProfileComponents>>,
}

fn find(p: &mut [u32], mut i: u32) -> u32 {
    while p[i as usize] != i {
        let g = p[p[i as usize] as usize];
        p[i as usize] = g;
        i = g;
    }
    i
}

fn union(p: &mut [u32], a: u32, b: u32) {
    let (ra, rb) = (find(p, a), find(p, b));
    if ra != rb {
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        p[hi as usize] = lo;
    }
}

fn find_tag(m: &mut BTreeMap<u64, u64>, t: u64) -> u64 {
    let mut r = t;
    while let Some(&q) = m.get(&r) {
        if q == r {
            break;
        }
        r = q;
    }
    // Path compression.
    let mut c = t;
    while c != r {
        let next = m.get(&c).copied().unwrap_or(c);
        m.insert(c, r);
        c = next;
    }
    r
}

fn union_tag(m: &mut BTreeMap<u64, u64>, a: u64, b: u64) {
    let (ra, rb) = (find_tag(m, a), find_tag(m, b));
    if ra != rb {
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        m.insert(hi, lo);
        m.entry(lo).or_insert(lo);
    }
}

impl Components {
    pub fn new(profiles: usize) -> Components {
        Components { profiles: (0..profiles).map(|_| OnceLock::new()).collect() }
    }

    /// Whether a search of `net` (profile `prof`) from `sources` may reach one of `targets`:
    /// `false` only when no path exists.
    pub fn may_reach(
        &self,
        grid: &Grid,
        st: &Statics,
        prof: usize,
        net: u32,
        sources: &[(u32, f32)],
        targets: &[u32],
    ) -> bool {
        let pc = self.profiles[prof].get_or_init(|| ProfileComponents::build(grid, st, prof));
        let nc = pc.net(grid, st, prof, net);
        let cells = grid.cells() as u32;
        let ns = st.track[prof].len() as u32;
        let state = |n: u32| -> u32 {
            let (s, c) = (n / cells, n % cells);
            let (x, y) = ((c % grid.w as u32) as i32, (c / grid.w as u32) as i32);
            st.track[prof][s as usize][grid.didx(2 * x, 2 * y)]
        };
        let tag = |n: u32| -> u64 {
            let l = pc.label[n as usize];
            if l == NONE { VIRTUAL | n as u64 } else { l as u64 }
        };
        let root = |t: u64| -> u64 {
            let mut r = t;
            while let Some(&q) = nc.root.get(&r) {
                if q == r {
                    break;
                }
                r = q;
            }
            r
        };
        let mut from: Vec<u64> = Vec::new();
        for &(n, _) in sources {
            if ok(state(n), net) {
                from.push(root(tag(n)));
                continue;
            }
            // A source the maps do not let through: what its first moves reach.
            let (s, c) = (n / cells, n % cells);
            let (x, y) = ((c % grid.w as u32) as i32, (c / grid.w as u32) as i32);
            let map = &st.track[prof][s as usize];
            for &(dx, dy) in &DIRS {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= grid.w || ny >= grid.h {
                    continue;
                }
                if ok(map[grid.didx(2 * x + dx, 2 * y + dy)], net) && ok(map[grid.didx(2 * nx, 2 * ny)], net) {
                    from.push(root(tag(s * cells + grid.cidx(nx, ny) as u32)));
                }
            }
            if ok(st.via[prof][c as usize], net) {
                for s2 in (0..ns).filter(|&s2| s2 != s) {
                    if ok(st.track[prof][s2 as usize][grid.didx(2 * x, 2 * y)], net) {
                        from.push(root(tag(s2 * cells + c)));
                    }
                }
            }
        }
        from.sort_unstable();
        from.dedup();
        targets.iter().any(|&t| ok(state(t), net) && from.binary_search(&root(tag(t))).is_ok())
    }
}

impl ProfileComponents {
    fn build(grid: &Grid, st: &Statics, prof: usize) -> ProfileComponents {
        let maps = &st.track[prof];
        let ns = maps.len();
        let cells = grid.cells();
        let (w, h) = (grid.w, grid.h);
        let mut p: Vec<u32> = vec![NONE; ns * cells];
        let free = |s: usize, i: i32, j: i32| maps[s][grid.didx(i, j)] == FREE;
        for s in 0..ns {
            for y in 0..h {
                for x in 0..w {
                    if free(s, 2 * x, 2 * y) {
                        let u = s * cells + grid.cidx(x, y);
                        p[u] = u as u32;
                    }
                }
            }
        }
        for s in 0..ns {
            for y in 0..h {
                for x in 0..w {
                    if !free(s, 2 * x, 2 * y) {
                        continue;
                    }
                    let u = (s * cells + grid.cidx(x, y)) as u32;
                    for (dx, dy) in [(1, 0), (0, 1), (1, 1), (-1, 1)] {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx < 0 || nx >= w || ny >= h {
                            continue;
                        }
                        if free(s, 2 * x + dx, 2 * y + dy) && free(s, 2 * nx, 2 * ny) {
                            union(&mut p, u, (s * cells + grid.cidx(nx, ny)) as u32);
                        }
                    }
                }
            }
        }
        if ns > 1 {
            for y in 0..h {
                for x in 0..w {
                    let c = grid.cidx(x, y);
                    if st.via[prof][c] != FREE {
                        continue;
                    }
                    let mut first: Option<u32> = None;
                    for s in 0..ns {
                        if free(s, 2 * x, 2 * y) {
                            let u = (s * cells + c) as u32;
                            match first {
                                None => first = Some(u),
                                Some(f) => union(&mut p, f, u),
                            }
                        }
                    }
                }
            }
        }
        for u in 0..p.len() {
            if p[u] != NONE {
                let r = find(&mut p, u as u32);
                p[u] = r;
            }
        }
        // Points only one net may use.
        let mut owned = Owned::new();
        for (s, m) in maps.iter().enumerate() {
            for (k, &v) in m.iter().enumerate() {
                if v != FREE && v != super::grid::BLOCKED {
                    owned.entry(v - 1).or_default().0.push((s as u32, k as u32));
                }
            }
        }
        for (c, &v) in st.via[prof].iter().enumerate() {
            if v != FREE && v != super::grid::BLOCKED {
                owned.entry(v - 1).or_default().1.push(c as u32);
            }
        }
        ProfileComponents { label: p, owned, nets: Mutex::new(BTreeMap::new()) }
    }

    /// The merges of `net` (computed on first use).
    fn net(&self, grid: &Grid, st: &Statics, prof: usize, net: u32) -> Arc<NetComponents> {
        if let Some(n) = self.nets.lock().expect("components").get(&net) {
            return n.clone();
        }
        let n = Arc::new(self.build_net(grid, st, prof, net));
        self.nets.lock().expect("components").entry(net).or_insert(n).clone()
    }

    fn build_net(&self, grid: &Grid, st: &Statics, prof: usize, net: u32) -> NetComponents {
        let mut root: BTreeMap<u64, u64> = BTreeMap::new();
        let Some((points, vias)) = self.owned.get(&net) else { return NetComponents { root } };
        let maps = &st.track[prof];
        let ns = maps.len();
        let cells = grid.cells();
        let dw = grid.dw();
        let tag = |s: usize, x: i32, y: i32| -> u64 {
            let n = s * cells + grid.cidx(x, y);
            let l = self.label[n];
            if l == NONE { VIRTUAL | n as u64 } else { l as u64 }
        };
        let okp = |s: usize, i: i32, j: i32| ok(maps[s][grid.didx(i, j)], net);
        let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x < grid.w && y < grid.h;
        // Every slot's node at cell (x, y) the net may use, joined through a via there.
        let via_join = |root: &mut BTreeMap<u64, u64>, x: i32, y: i32| {
            if !ok(st.via[prof][grid.cidx(x, y)], net) {
                return;
            }
            let mut first: Option<u64> = None;
            for s in 0..ns {
                if okp(s, 2 * x, 2 * y) {
                    let t = tag(s, x, y);
                    match first {
                        None => first = Some(t),
                        Some(f) => union_tag(root, f, t),
                    }
                }
            }
        };
        for &(s, k) in points {
            let s = s as usize;
            let (i, j) = ((k as i32) % dw, (k as i32) / dw);
            match (i % 2, j % 2) {
                (0, 0) => {
                    let (x, y) = (i / 2, j / 2);
                    let t = tag(s, x, y);
                    root.entry(t).or_insert(t);
                    for &(dx, dy) in &DIRS {
                        let (nx, ny) = (x + dx, y + dy);
                        if inside(nx, ny) && okp(s, 2 * x + dx, 2 * y + dy) && okp(s, 2 * nx, 2 * ny) {
                            union_tag(&mut root, t, tag(s, nx, ny));
                        }
                    }
                    via_join(&mut root, x, y);
                }
                _ => {
                    // A move's midpoint: the moves through it.
                    let ends: Vec<((i32, i32), (i32, i32))> = match (i % 2, j % 2) {
                        (1, 0) => vec![((i - 1, j), (i + 1, j))],
                        (0, 1) => vec![((i, j - 1), (i, j + 1))],
                        _ => vec![((i - 1, j - 1), (i + 1, j + 1)), ((i - 1, j + 1), (i + 1, j - 1))],
                    };
                    for ((ai, aj), (bi, bj)) in ends {
                        let (a, b) = ((ai / 2, aj / 2), (bi / 2, bj / 2));
                        if inside(a.0, a.1) && inside(b.0, b.1) && okp(s, ai, aj) && okp(s, bi, bj) {
                            union_tag(&mut root, tag(s, a.0, a.1), tag(s, b.0, b.1));
                        }
                    }
                }
            }
        }
        for &c in vias {
            let (x, y) = ((c as i32) % grid.w, (c as i32) / grid.w);
            via_join(&mut root, x, y);
        }
        // Flatten.
        let keys: Vec<u64> = root.keys().copied().collect();
        for k in keys {
            let r = find_tag(&mut root, k);
            root.insert(k, r);
        }
        NetComponents { root }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::geo::{BoxF, P};
    use crate::router::grid::BLOCKED;

    /// Statics without scoped rules (the maps filled by the test).
    fn plain() -> Statics {
        Statics {
            track: vec![],
            via: vec![],
            tst: vec![],
            vst: vec![],
            states: vec![crate::router::scoped::St::default()],
        }
    }

    /// A 9 × 5 cell grid on two layers with a wall across layer 0 at x = 4 (cells and
    /// midpoints), and vias blocked: each side of the wall is its own component, except for
    /// the net owning the wall; through a free via cell, the other layer joins them.
    #[test]
    fn walls_separate_and_owned_points_join() {
        let grid = Grid::new(BoxF::of2(P::new(0.0, 0.0), P::new(8.0, 4.0)), 1.0, 0.0, 0.0);
        let (dw, dh) = (grid.dw(), grid.dh());
        let wall = |v: u32| {
            let mut m = vec![FREE; (dw * dh) as usize];
            for j in 0..dh {
                for i in 7..=9 {
                    m[grid.didx(2 * 4 + i - 8, j)] = v;
                }
            }
            m
        };
        let blocked_vias = vec![BLOCKED; grid.cells()];
        let st =
            Statics { track: vec![vec![wall(BLOCKED), wall(BLOCKED)]], via: vec![blocked_vias.clone()], ..plain() };
        let node = |s: u32, x: i32, y: i32| s * grid.cells() as u32 + grid.cidx(x, y) as u32;
        let (a, b) = (node(0, 1, 1), node(0, 7, 3));
        assert!(Components::new(1).may_reach(&grid, &st, 0, 5, &[(a, 0.0)], &[node(0, 2, 3)]));
        assert!(!Components::new(1).may_reach(&grid, &st, 0, 5, &[(a, 0.0)], &[b]));
        // The wall owned by net 3 (state 4): passable for it only.
        let st = Statics { track: vec![vec![wall(4), wall(BLOCKED)]], via: vec![blocked_vias.clone()], ..plain() };
        let c = Components::new(1);
        assert!(c.may_reach(&grid, &st, 0, 3, &[(a, 0.0)], &[b]));
        assert!(!c.may_reach(&grid, &st, 0, 5, &[(a, 0.0)], &[b]));
        // Layer 1 free and every via cell free: both sides join for every net.
        let st =
            Statics { track: vec![vec![wall(BLOCKED), wall(FREE)]], via: vec![vec![FREE; grid.cells()]], ..plain() };
        assert!(Components::new(1).may_reach(&grid, &st, 0, 5, &[(a, 0.0)], &[b]));
    }
}
