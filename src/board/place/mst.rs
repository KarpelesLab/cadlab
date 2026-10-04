//! Minimum spanning tree length over points (the placement ratsnest cost), computed with
//! Prim's algorithm in one pass per step (DECISIONS D42): the points are picked in the same
//! order with the same link lengths as the plain two-pass loop, so the floating-point total is
//! bit-identical.

use crate::geom::Point;

/// Euclidean distance (the placement's metric).
pub(super) fn dist(a: Point, b: Point) -> f64 {
    let (dx, dy) = ((a.x.0 - b.x.0) as f64, (a.y.0 - b.y.0) as f64);
    (dx * dx + dy * dy).sqrt()
}

/// Length of the minimum spanning tree of `pts`: Prim's algorithm from `pts[0]`, adding at
/// each step the closest remaining point (lowest index among equal distances) and summing
/// the link lengths in that order.
pub(super) fn mst_length(pts: &[Point]) -> f64 {
    fused(pts)
}

/// Prim's algorithm with the next-point search fused into the update pass, over a shrinking
/// list of the remaining points: the same choices (smallest link, then lowest index) and the
/// same link lengths as the straightforward loop (kept in the tests), in one pass per step
/// instead of two over all points.
fn fused(pts: &[Point]) -> f64 {
    if pts.len() < 2 {
        return 0.0;
    }
    let xs: Vec<i64> = pts.iter().map(|p| p.x.0).collect();
    let ys: Vec<i64> = pts.iter().map(|p| p.y.0).collect();
    let mut best = vec![f64::INFINITY; pts.len()];
    let mut rest: Vec<u32> = (1..pts.len() as u32).collect();
    let mut u = 0usize;
    let mut total = 0.0;
    loop {
        // `best[u]` is 0 for the first point, as in the plain loop.
        total += if u == 0 { 0.0 } else { best[u] };
        if rest.is_empty() {
            return total;
        }
        let (ux, uy) = (xs[u], ys[u]);
        let mut next = usize::MAX;
        let mut next_best = f64::INFINITY;
        for &v in &rest {
            let v = v as usize;
            let (dx, dy) = ((ux - xs[v]) as f64, (uy - ys[v]) as f64);
            let d = (dx * dx + dy * dy).sqrt();
            let b = if d < best[v] { d } else { best[v] };
            best[v] = b;
            if next == usize::MAX || b < next_best || (b == next_best && v < next) {
                next = v;
                next_best = b;
            }
        }
        let k = rest.iter().position(|&v| v as usize == next).expect("remaining");
        rest.swap_remove(k);
        u = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Nm;

    /// The straightforward version.
    fn plain(pts: &[Point]) -> f64 {
        if pts.len() < 2 {
            return 0.0;
        }
        let mut best = vec![f64::INFINITY; pts.len()];
        let mut done = vec![false; pts.len()];
        best[0] = 0.0;
        let mut total = 0.0;
        for _ in 0..pts.len() {
            let mut u = usize::MAX;
            for v in 0..pts.len() {
                if !done[v] && (u == usize::MAX || best[v] < best[u]) {
                    u = v;
                }
            }
            done[u] = true;
            total += best[u];
            for v in 0..pts.len() {
                if !done[v] {
                    let d = dist(pts[u], pts[v]);
                    if d < best[v] {
                        best[v] = d;
                    }
                }
            }
        }
        total
    }

    fn points(n: usize, seed: u64, spread: i64) -> Vec<Point> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |m: i64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % m as u64) as i64
        };
        (0..n)
            .map(|_| {
                // Clustered, with duplicates and points on shared lines (ties).
                let c = next(5) * spread;
                Point::new(Nm(c + next(spread) / 250 * 250), Nm(next(spread) / 500 * 500))
            })
            .collect()
    }

    /// `cargo test --release --lib mst::tests::timing -- --ignored --nocapture`
    #[test]
    #[ignore = "timing"]
    fn timing() {
        for n in [64, 129, 184, 403] {
            let pts = points(n, 9, 40_000_000);
            for (name, f) in [("plain", plain as fn(&[Point]) -> f64), ("fused", fused)] {
                let t = std::time::Instant::now();
                for _ in 0..2000 {
                    std::hint::black_box(f(std::hint::black_box(&pts)));
                }
                eprintln!("n {n} {name} {:?}", t.elapsed() / 2000);
            }
        }
    }

    #[test]
    fn fused_matches_plain_bit_for_bit() {
        for (n, seed, spread) in
            [(48, 1, 1_000_000), (60, 2, 10_000), (200, 3, 5_000_000), (700, 4, 40_000_000), (513, 5, 2_000)]
        {
            let pts = points(n, seed, spread);
            assert_eq!(fused(&pts).to_bits(), plain(&pts).to_bits(), "n {n}");
        }
        // All points equal, and on a line.
        let same = vec![Point::new(Nm(5), Nm(7)); 100];
        assert_eq!(fused(&same).to_bits(), plain(&same).to_bits());
        let line: Vec<Point> = (0..300).map(|i| Point::new(Nm((i * 7919) % 300 * 1000), Nm(0))).collect();
        assert_eq!(fused(&line).to_bits(), plain(&line).to_bits());
    }
}
