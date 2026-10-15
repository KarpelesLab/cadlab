//! Graded rectilinear mesh lines for FDTD (one axis at a time).
//!
//! Fixed lines (metal edges, layer boundaries, ports, the domain bounds) are kept; lines closer
//! than a minimum spacing are merged into one (a hard line wins, else the middle one of the
//! cluster); the gaps between fixed lines are filled with cells no larger than the gap's maximum
//! (λ/N in the material) whose sizes change by at most a grading ratio from one cell to the
//! next, growing away from the fixed lines. Integer nanometers throughout; floating point only
//! inside the gap filling, rounded once per line. Deterministic.

/// A fixed line: coordinate (nm) and whether it is hard (never merged away: domain bounds,
/// ports, layer surfaces).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fixed {
    /// Coordinate.
    pub at: i64,
    /// Never moved by merging.
    pub hard: bool,
}

impl Fixed {
    /// A soft line (metal edge).
    pub fn soft(at: i64) -> Fixed {
        Fixed { at, hard: false }
    }
    /// A hard line.
    pub fn hard(at: i64) -> Fixed {
        Fixed { at, hard: true }
    }
}

/// Merges fixed lines closer than `min` (to the first line of their cluster): a cluster keeps
/// its hard lines (all of them), else its middle line. Lines outside `[lo, hi]` are dropped;
/// `lo` and `hi` are always kept.
pub fn merge(mut lines: Vec<Fixed>, lo: i64, hi: i64, min: i64) -> Vec<i64> {
    lines.retain(|f| f.at > lo && f.at < hi);
    lines.push(Fixed::hard(lo));
    lines.push(Fixed::hard(hi));
    lines.sort();
    lines.dedup_by(|a, b| {
        if a.at == b.at {
            b.hard |= a.hard;
            true
        } else {
            false
        }
    });
    let mut out: Vec<i64> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let start = lines[i].at;
        let mut j = i + 1;
        while j < lines.len() && lines[j].at - start < min && !(lines[j].hard && lines[i..j].iter().any(|f| f.hard)) {
            j += 1;
        }
        let cluster = &lines[i..j];
        match cluster.iter().find(|f| f.hard) {
            Some(h) => out.push(h.at),
            None => out.push(cluster[cluster.len() / 2].at),
        }
        i = j;
    }
    // A soft line kept too close to a following hard line (a cluster cannot hold two hard lines,
    // and a hard line ends a soft cluster only past `min`): drop soft lines within `min` of a
    // hard neighbour.
    let hard: Vec<i64> = lines.iter().filter(|f| f.hard).map(|f| f.at).collect();
    out.retain(|&x| hard.binary_search(&x).is_ok() || hard.iter().all(|&h| (h - x).abs() >= min));
    out.dedup();
    out
}

/// Cell sizes (floats, summing to `len`) filling a gap of `len` between two fixed lines whose
/// outside neighbouring cells are `left` and `right`, each cell at most `max` and growing by at
/// most `ratio` from its neighbour.
fn fill_gap(len: f64, left: f64, right: f64, max: f64, ratio: f64) -> Vec<f64> {
    if len <= max && len <= left * ratio && len <= right * ratio {
        return vec![len];
    }
    let (mut l, mut r) = (Vec::<f64>::new(), Vec::<f64>::new());
    let (mut nl, mut nr) = ((left * ratio).min(max), (right * ratio).min(max));
    let mut sum = 0.0;
    let mut last_left = true;
    while sum < len {
        if nl <= nr {
            l.push(nl);
            sum += nl;
            nl = (nl * ratio).min(max);
            last_left = true;
        } else {
            r.push(nr);
            sum += nr;
            nr = (nr * ratio).min(max);
            last_left = false;
        }
    }
    let join = |l: &[f64], r: &[f64]| -> Vec<f64> { l.iter().copied().chain(r.iter().rev().copied()).collect() };
    // `n` cells scaled down to fit, or `n − 1` scaled up (when that keeps every cell within
    // `max`): whichever needs the smaller change.
    let down = len / sum;
    let fewer = if last_left { join(&l[..l.len() - 1], &r) } else { join(&l, &r[..r.len() - 1]) };
    let fewer_sum: f64 = fewer.iter().sum();
    if fewer_sum > 0.0 {
        let up = len / fewer_sum;
        let biggest = fewer.iter().copied().fold(0.0, f64::max) * up;
        if biggest <= max * (1.0 + 1e-9) && up - 1.0 < 1.0 / down - 1.0 {
            return fewer.into_iter().map(|c| c * up).collect();
        }
    }
    join(&l, &r).into_iter().map(|c| c * down).collect()
}

/// Fills the gaps between sorted fixed lines. `max_for(a, b)` is the largest cell allowed
/// between fixed lines `a` and `b`. Gaps are refilled a few times with the neighbouring cells
/// actually generated, so grading holds across fixed lines too.
pub fn smooth(fixed: &[i64], max_for: &dyn Fn(i64, i64) -> i64, ratio: f64) -> Vec<i64> {
    if fixed.len() < 2 {
        return fixed.to_vec();
    }
    let gaps: Vec<(i64, i64)> = fixed.windows(2).map(|w| (w[0], w[1])).collect();
    let maxes: Vec<f64> = gaps.iter().map(|&(a, b)| max_for(a, b).max(1) as f64).collect();
    // First and last cell of every gap (initially: the gap itself, capped).
    let mut ends: Vec<(f64, f64)> =
        gaps.iter().zip(&maxes).map(|(&(a, b), &m)| ((b - a) as f64).min(m)).map(|c| (c, c)).collect();
    let mut cells: Vec<Vec<f64>> = Vec::new();
    for _ in 0..6 {
        cells = gaps
            .iter()
            .enumerate()
            .map(|(k, &(a, b))| {
                let left = if k > 0 { ends[k - 1].1 } else { maxes[k] };
                let right = if k + 1 < gaps.len() { ends[k + 1].0 } else { maxes[k] };
                fill_gap((b - a) as f64, left, right, maxes[k], ratio)
            })
            .collect();
        let next: Vec<(f64, f64)> = cells.iter().map(|c| (c[0], *c.last().unwrap())).collect();
        if next == ends {
            break;
        }
        ends = next;
    }
    let mut out = vec![fixed[0]];
    for (k, &(a, b)) in gaps.iter().enumerate() {
        let mut acc = 0.0;
        let c = &cells[k];
        for cell in &c[..c.len() - 1] {
            acc += cell;
            let x = a + acc.round() as i64;
            if x > *out.last().unwrap() && x < b {
                out.push(x);
            }
        }
        out.push(b);
    }
    out
}

/// Largest and smallest spacing of sorted lines (0 for fewer than two lines).
pub fn spacing(lines: &[i64]) -> (i64, i64) {
    let d: Vec<i64> = lines.windows(2).map(|w| w[1] - w[0]).collect();
    (d.iter().copied().max().unwrap_or(0), d.iter().copied().min().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(lines: &[i64], max: i64, ratio: f64) {
        let d: Vec<i64> = lines.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(d.iter().all(|&c| c > 0), "increasing: {lines:?}");
        assert!(d.iter().all(|&c| c <= max + 1), "max {max}: {d:?}");
        for w in d.windows(2) {
            let r = w[0].max(w[1]) as f64 / w[0].min(w[1]) as f64;
            assert!(r <= ratio * 1.02, "grading {r} in {d:?}");
        }
    }

    #[test]
    fn merges_close_lines_keeping_hard_ones() {
        let lines = vec![Fixed::soft(100), Fixed::soft(104), Fixed::soft(108), Fixed::hard(110), Fixed::soft(500)];
        assert_eq!(merge(lines, 0, 1000, 20), vec![0, 110, 500, 1000]);
        let lines = vec![Fixed::soft(100), Fixed::soft(105), Fixed::soft(109)];
        assert_eq!(merge(lines, 0, 1000, 20), vec![0, 105, 1000]);
        // Two hard lines closer than `min` both stay.
        let lines = vec![Fixed::hard(100), Fixed::hard(105)];
        assert_eq!(merge(lines, 0, 1000, 20), vec![0, 100, 105, 1000]);
        // Out of range lines are dropped.
        assert_eq!(merge(vec![Fixed::soft(-5), Fixed::soft(2000)], 0, 1000, 20), vec![0, 1000]);
    }

    #[test]
    fn uniform_gap_is_split_evenly() {
        let l = smooth(&[0, 10_000], &|_, _| 1_000, 1.5);
        assert_eq!(l.len(), 11);
        check(&l, 1_000, 1.5);
    }

    #[test]
    fn grades_away_from_fine_features() {
        // A 35 µm copper layer between two thick regions.
        let fixed = [0, 1_000_000, 1_035_000, 2_000_000];
        let l = smooth(&fixed, &|_, _| 200_000, 1.5);
        for f in fixed {
            assert!(l.contains(&f), "{f} kept");
        }
        check(&l, 200_000, 1.5);
        assert!(l.len() < 40, "{}", l.len());
    }

    #[test]
    fn deterministic() {
        let fixed = [0, 13, 999_917, 1_000_000, 7_654_321];
        let a = smooth(&fixed, &|a, _| if a < 1_000_000 { 50_000 } else { 300_000 }, 1.4);
        let b = smooth(&fixed, &|a, _| if a < 1_000_000 { 50_000 } else { 300_000 }, 1.4);
        assert_eq!(a, b);
        check(&a, 300_000, 1.4);
    }
}
