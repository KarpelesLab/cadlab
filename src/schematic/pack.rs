//! Skyline bin packing of rectangular groups onto sheets, in grid units.
//!
//! Coordinates are top-down (y grows from the top edge of the drawing area), as the sheet is
//! filled from the top-left. Each sheet may have one blocked rectangle (the title block).

/// A packed rectangle: sheet index, x and y of its top-left corner.
pub(crate) type Spot = (usize, i64, i64);

/// One sheet being filled: skyline segments `(x, width, y)`.
struct Bin {
    segs: Vec<(i64, i64, i64)>,
}

impl Bin {
    fn new(w: i64) -> Bin {
        Bin { segs: vec![(0, w, 0)] }
    }

    /// Lowest (then leftmost) top-left position for a `w`×`h` rectangle.
    fn find(&self, w: i64, h: i64, area: (i64, i64), blocked: Option<[i64; 4]>) -> Option<(i64, i64)> {
        let mut best: Option<(i64, i64)> = None;
        for &(x, _, _) in &self.segs {
            if x + w > area.0 {
                continue;
            }
            let y = self.segs.iter().filter(|(sx, sw, _)| *sx < x + w && sx + sw > x).map(|s| s.2).max().unwrap_or(0);
            if y + h > area.1 {
                continue;
            }
            if let Some(b) = blocked
                && x < b[2]
                && x + w > b[0]
                && y < b[3]
                && y + h > b[1]
            {
                continue;
            }
            if best.is_none_or(|(bx, by)| (y, x) < (by, bx)) {
                best = Some((x, y));
            }
        }
        best
    }

    fn place(&mut self, x: i64, y: i64, w: i64, h: i64) {
        let mut out = Vec::new();
        for &(sx, sw, sy) in &self.segs {
            let end = sx + sw;
            if end <= x || sx >= x + w {
                out.push((sx, sw, sy));
                continue;
            }
            if sx < x {
                out.push((sx, x - sx, sy));
            }
            if end > x + w {
                out.push((x + w, end - x - w, sy));
            }
        }
        out.push((x, w, y + h));
        out.sort();
        // Merge neighbours at the same height.
        let mut merged: Vec<(i64, i64, i64)> = Vec::new();
        for s in out {
            match merged.last_mut() {
                Some(m) if m.0 + m.1 == s.0 && m.2 == s.2 => m.1 += s.1,
                _ => merged.push(s),
            }
        }
        self.segs = merged;
    }

    fn height(&self) -> i64 {
        self.segs.iter().map(|s| s.2).max().unwrap_or(0)
    }
}

/// Packs `sizes` (w, h) into sheets of `area` (w, h), first fit in a few orders (tallest first,
/// largest first, widest first, by group then tallest, given order). Keeps the result with the
/// fewest sheets, then the fewest `groups` (items that belong together) spread over several
/// sheets, then the least used height. `None` if an item does not fit on an empty sheet or more
/// than `max_sheets` would be needed.
pub(crate) fn pack(
    sizes: &[(i64, i64)],
    groups: &[usize],
    area: (i64, i64),
    blocked: Option<[i64; 4]>,
    max_sheets: usize,
) -> Option<Vec<Spot>> {
    let n = sizes.len();
    let mut orders: Vec<Vec<usize>> = Vec::new();
    let by = |key: &dyn Fn(usize) -> i64| {
        let mut o: Vec<usize> = (0..n).collect();
        o.sort_by_key(|&i| (std::cmp::Reverse(key(i)), i));
        o
    };
    orders.push(by(&|i| sizes[i].1 * 1000 + sizes[i].0));
    orders.push(by(&|i| sizes[i].0 * sizes[i].1));
    orders.push(by(&|i| sizes[i].0 * 1000 + sizes[i].1));
    orders.push(by(&|i| -(groups.get(i).copied().unwrap_or(0) as i64) * 1_000_000 + sizes[i].1));
    orders.push((0..n).collect());
    let mut best: Option<((usize, usize, i64), Vec<Spot>)> = None;
    for order in orders {
        let mut bins: Vec<Bin> = Vec::new();
        let mut spots = vec![(0, 0, 0); n];
        let mut failed = false;
        for &i in &order {
            let (w, h) = sizes[i];
            let found = bins.iter().enumerate().find_map(|(b, bin)| bin.find(w, h, area, blocked).map(|p| (b, p)));
            let (b, (x, y)) = match found {
                Some(f) => f,
                None => {
                    let bin = Bin::new(area.0);
                    let Some(p) = bin.find(w, h, area, blocked) else {
                        failed = true;
                        break;
                    };
                    bins.push(bin);
                    (bins.len() - 1, p)
                }
            };
            bins[b].place(x, y, w, h);
            spots[i] = (b, x, y);
        }
        if failed || bins.len() > max_sheets {
            continue;
        }
        let mut sheets_of: std::collections::BTreeMap<usize, std::collections::BTreeSet<usize>> = Default::default();
        for (i, spot) in spots.iter().enumerate() {
            sheets_of.entry(groups.get(i).copied().unwrap_or(i + usize::MAX / 2)).or_default().insert(spot.0);
        }
        let split = sheets_of.values().filter(|s| s.len() > 1).count();
        let score = (bins.len(), split, bins.iter().map(Bin::height).sum::<i64>());
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, spots));
        }
    }
    best.map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_without_overlap() {
        let sizes = [(10, 4), (3, 9), (6, 6), (6, 2), (4, 4), (12, 3), (2, 2)];
        let spots = pack(&sizes, &[], (20, 14), Some([14, 11, 20, 14]), 1).unwrap();
        for (i, &(b, x, y)) in spots.iter().enumerate() {
            assert_eq!(b, 0);
            let (w, h) = sizes[i];
            assert!(x + w <= 20 && y + h <= 14);
            assert!(!(x < 20 && x + w > 14 && y < 14 && y + h > 11), "item {i} on the blocked area");
            for (j, &(_, x2, y2)) in spots.iter().enumerate().skip(i + 1) {
                let (w2, h2) = sizes[j];
                assert!(x + w <= x2 || x2 + w2 <= x || y + h <= y2 || y2 + h2 <= y, "{i} and {j} overlap");
            }
        }
        // Too big for one sheet: two sheets, or none when limited to one.
        let big = [(10, 10), (10, 10)];
        assert!(pack(&big, &[], (12, 12), None, 1).is_none());
        let s = pack(&big, &[], (12, 12), None, 4).unwrap();
        assert_ne!(s[0].0, s[1].0);
        assert!(pack(&[(20, 1)], &[], (12, 12), None, 4).is_none());
    }
}
