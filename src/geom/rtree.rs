//! A static, packed R-tree over integer bounding boxes (sort-tile-recursive bulk loading).
//!
//! Built once from a list of boxes, then queried for the entries whose box meets a query box
//! (touching counts, as [`Rect::intersects`]). The tree only skips boxes that cannot meet the
//! query, so a query finds exactly the entries a linear scan would; results are returned
//! sorted by entry index, so they never depend on the tree's shape. Construction is
//! deterministic (stable sorts with the entry index as the last key).
//!
//! Used for candidate searches over shapes (DRC pairs, keep-away culling in zone fill); see
//! `docs/BOARD.md`, "Performance" and DECISIONS D42.

use polyclip::Rect;

/// Children per node.
const FANOUT: usize = 16;

/// A node: its box and the range of its children in the level below (entries for the leaf
/// level).
#[derive(Clone, Copy, Debug)]
struct Node {
    bbox: Rect,
    start: u32,
    end: u32,
}

/// A packed R-tree of boxes, each tagged with the index it was given at construction.
#[derive(Clone, Debug, Default)]
pub struct RTree {
    /// Entries in packed order: (box, index).
    entries: Vec<(Rect, u32)>,
    /// Node levels, bottom (parents of entries) to top (a single root).
    levels: Vec<Vec<Node>>,
}

/// Sort-tile-recursive order of `items` (by center x into vertical slices, by center y
/// within each slice), stable with the original position as the last key.
fn str_order<T>(items: &mut [T], bbox: impl Fn(&T) -> Rect) {
    let n = items.len();
    if n <= FANOUT {
        return;
    }
    let cx = |r: &Rect| r.min.x as i128 + r.max.x as i128;
    let cy = |r: &Rect| r.min.y as i128 + r.max.y as i128;
    items.sort_by_key(|t| cx(&bbox(t)));
    let leaves = n.div_ceil(FANOUT);
    let slices = (leaves as f64).sqrt().ceil() as usize;
    let per_slice = slices.max(1) * FANOUT;
    for chunk in items.chunks_mut(per_slice) {
        chunk.sort_by_key(|t| cy(&bbox(t)));
    }
}

fn cover(boxes: impl Iterator<Item = Rect>) -> Rect {
    boxes.reduce(|a, b| a.union(&b)).expect("non-empty node")
}

impl RTree {
    /// Builds the tree; `None` boxes (empty shapes) are left out and never found.
    pub fn new(boxes: impl IntoIterator<Item = Option<Rect>>) -> RTree {
        let mut entries: Vec<(Rect, u32)> =
            boxes.into_iter().enumerate().filter_map(|(i, b)| b.map(|b| (b, i as u32))).collect();
        // Stable sorts on a list in index order: ties keep index order.
        str_order(&mut entries, |e| e.0);
        let mut levels: Vec<Vec<Node>> = Vec::new();
        if entries.is_empty() {
            return RTree { entries, levels };
        }
        let mut below: Vec<Rect> = entries.iter().map(|e| e.0).collect();
        loop {
            let mut nodes: Vec<Node> = below
                .chunks(FANOUT)
                .enumerate()
                .map(|(k, c)| Node {
                    bbox: cover(c.iter().copied()),
                    start: (k * FANOUT) as u32,
                    end: (k * FANOUT + c.len()) as u32,
                })
                .collect();
            if nodes.len() > 1 {
                str_order(&mut nodes, |n| n.bbox);
            }
            below = nodes.iter().map(|n| n.bbox).collect();
            let top = nodes.len() == 1;
            levels.push(nodes);
            if top {
                break;
            }
        }
        RTree { entries, levels }
    }

    /// Number of entries (boxes given as `None` excluded).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the tree has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Box covering every entry.
    pub fn bbox(&self) -> Option<Rect> {
        self.levels.last().map(|l| l[0].bbox)
    }

    /// Calls `f` with the index of every entry whose box meets `q`, in tree order (not
    /// sorted; use [`RTree::query`] for a sorted list).
    pub fn visit(&self, q: &Rect, mut f: impl FnMut(u32)) {
        let Some(top) = self.levels.len().checked_sub(1) else { return };
        // (level, node index); level `usize::MAX` never occurs.
        let mut stack: Vec<(usize, u32)> = vec![(top, 0)];
        while let Some((level, k)) = stack.pop() {
            let node = &self.levels[level][k as usize];
            if !node.bbox.intersects(q) {
                continue;
            }
            if level == 0 {
                for e in &self.entries[node.start as usize..node.end as usize] {
                    if e.0.intersects(q) {
                        f(e.1);
                    }
                }
            } else {
                for c in node.start..node.end {
                    stack.push((level - 1, c));
                }
            }
        }
    }

    /// Indices of the entries whose box meets `q`, ascending.
    pub fn query(&self, q: &Rect) -> Vec<u32> {
        let mut out = Vec::new();
        self.visit(q, |i| out.push(i));
        out.sort_unstable();
        out
    }

    /// Whether any entry's box meets `q`.
    pub fn any(&self, q: &Rect) -> bool {
        let Some(top) = self.levels.len().checked_sub(1) else { return false };
        let mut stack: Vec<(usize, u32)> = vec![(top, 0)];
        while let Some((level, k)) = stack.pop() {
            let node = &self.levels[level][k as usize];
            if !node.bbox.intersects(q) {
                continue;
            }
            if level == 0 {
                if self.entries[node.start as usize..node.end as usize].iter().any(|e| e.0.intersects(q)) {
                    return true;
                }
            } else {
                stack.extend((node.start..node.end).map(|c| (level - 1, c)));
            }
        }
        false
    }

    /// Pairs `(i, j)`, `i < j`, of entries whose boxes, each grown by `grow` on every side,
    /// meet. Sorted.
    pub fn pairs(&self, grow: i64) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for &(b, i) in &self.entries {
            let q = b.expand(grow.saturating_mul(2));
            self.visit(&q, |j| {
                if i < j {
                    out.push((i, j));
                }
            });
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polyclip::Point;

    fn r(x0: i64, y0: i64, x1: i64, y1: i64) -> Rect {
        Rect::new(Point::new(x0, y0), Point::new(x1, y1))
    }

    /// Deterministic pseudo-random boxes, some empty, of mixed sizes.
    fn boxes(n: usize, seed: u64) -> Vec<Option<Rect>> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |m: i64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % m as u64) as i64
        };
        (0..n)
            .map(|_| {
                if next(20) == 0 {
                    return None;
                }
                let (x, y) = (next(1_000_000) - 500_000, next(600_000) - 300_000);
                let big = next(50) == 0;
                let (w, h) = if big { (next(400_000), next(400_000)) } else { (next(5_000), next(5_000)) };
                Some(r(x, y, x + w, y + h))
            })
            .collect()
    }

    #[test]
    fn queries_match_a_linear_scan() {
        for (n, seed) in [(0, 1), (1, 2), (15, 3), (16, 4), (17, 5), (300, 6), (5000, 7)] {
            let bx = boxes(n, seed);
            let t = RTree::new(bx.iter().copied());
            assert_eq!(t.len(), bx.iter().flatten().count());
            for q in boxes(200, seed + 100).into_iter().flatten().chain([r(-1, -1, 1, 1), r(i64::MIN / 4, 0, 0, 0)]) {
                let q = q.expand(3_000);
                let want: Vec<u32> =
                    (0..n as u32).filter(|&i| bx[i as usize].is_some_and(|b| b.intersects(&q))).collect();
                assert_eq!(t.query(&q), want);
                assert_eq!(t.any(&q), !want.is_empty());
            }
            for grow in [0, 1, 2_500] {
                let mut want = Vec::new();
                for i in 0..n {
                    for j in i + 1..n {
                        if let (Some(a), Some(b)) = (bx[i], bx[j])
                            && a.expand(grow).intersects(&b.expand(grow))
                        {
                            want.push((i as u32, j as u32));
                        }
                    }
                }
                assert_eq!(t.pairs(grow), want, "n {n}, grow {grow}");
            }
        }
    }

    #[test]
    fn touching_boxes_meet() {
        let t = RTree::new([Some(r(0, 0, 10, 10)), Some(r(10, 10, 20, 20)), None, Some(r(21, 0, 30, 10))]);
        assert_eq!(t.query(&r(10, 10, 10, 10)), vec![0, 1]);
        assert_eq!(t.pairs(0), vec![(0, 1)]);
        assert_eq!(t.pairs(1), vec![(0, 1), (1, 3)]);
        assert_eq!(t.bbox(), Some(r(0, 0, 30, 20)));
    }
}
