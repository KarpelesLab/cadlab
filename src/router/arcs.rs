//! Arc corners: every bend of a routed polyline (not a via, junction or terminal) becomes a
//! circular arc tangent to both segments, as large as the rules allow, for smooth or RF nets.
//!
//! At a corner `B` between `A`–`B` and `B`–`C` with interior angle `φ`, an arc of radius `r`
//! touches the segments at distance `t = r / tan(φ/2)` from `B`; `t` may use half of a
//! segment shared with another rounded corner and all of one that ends at a pinned vertex.
//! The radius starts at the largest that fits (capped by the `arc_radius` option) and halves
//! until the arc passes the exact clearance check (chords of at most [`SAGITTA`] with that much
//! extra clearance, so the true arc keeps the rules), down to the track width. The straight
//! parts left are pieces of segments already checked. Arcs are stored as the track's midpoint
//! (`Track::mid`), which the Gerber, KiCad and IPC-2581 writers export as true arcs.

use super::geo::{BoxF, P};
use super::index::{Checker, Index, Item};
use super::post::{NetGeom, Path};

/// Largest distance (nm) between an arc and the chords standing for it in checks.
pub(crate) const SAGITTA: f64 = 100.0;

/// Chords of the arc from `a` through `m` to `b` with at most [`SAGITTA`] deviation (points,
/// both ends included); a straight segment when the points are collinear.
pub(crate) fn chords(a: P, m: P, b: P) -> Vec<P> {
    let Some((c, r)) = circle(a, m, b) else { return vec![a, b] };
    let ang = |q: P| (q.y - c.y).atan2(q.x - c.x);
    let (t0, tm, t1) = (ang(a), ang(m), ang(b));
    let tau = std::f64::consts::TAU;
    // Sweep from t0 to t1 passing tm.
    let ccw = (tm - t0).rem_euclid(tau) < (t1 - t0).rem_euclid(tau);
    let sweep = if ccw { (t1 - t0).rem_euclid(tau) } else { -(t0 - t1).rem_euclid(tau) };
    let step = 2.0 * (1.0 - SAGITTA / r).clamp(-1.0, 1.0).acos();
    let n = ((sweep.abs() / step.max(1e-6)).ceil() as usize).clamp(1, 4096);
    let mut out = vec![a];
    for k in 1..n {
        let t = t0 + sweep * k as f64 / n as f64;
        out.push(P::new(c.x + r * t.cos(), c.y + r * t.sin()));
    }
    out.push(b);
    out
}

/// Circle through three points (center, radius), `None` when they are collinear.
fn circle(a: P, m: P, b: P) -> Option<(P, f64)> {
    let d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
    if d.abs() < 1e-3 {
        return None;
    }
    let (a2, m2, b2) = (a.x * a.x + a.y * a.y, m.x * m.x + m.y * m.y, b.x * b.x + b.y * b.y);
    let c = P::new(
        (a2 * (m.y - b.y) + m2 * (b.y - a.y) + b2 * (a.y - m.y)) / d,
        (a2 * (b.x - m.x) + m2 * (a.x - b.x) + b2 * (m.x - a.x)) / d,
    );
    Some((c, c.dist(a)))
}

/// Length of the arc from `a` through `m` to `b` (nm).
#[cfg(test)]
fn arc_length(a: P, m: P, b: P) -> f64 {
    let pts = chords(a, m, b);
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

fn unit(a: P, b: P) -> (P, f64) {
    let l = a.dist(b);
    (P::new((b.x - a.x) / l.max(1e-9), (b.y - a.y) / l.max(1e-9)), l)
}

/// Rounds the corners of every path of a net (index entries of the net must be removed by the
/// caller). `min_r`: smallest radius worth an arc; `max_r`: the cap.
pub(crate) fn round(g: &mut NetGeom, net: u32, ck: &Checker<'_>, min_r: f64, max_r: f64) {
    for p in &mut g.paths {
        round_path(p, net, ck, min_r, max_r);
    }
}

fn round_path(p: &mut Path, net: u32, ck: &Checker<'_>, min_r: f64, max_r: f64) {
    let n = p.pts.len();
    if n < 3 {
        return;
    }
    // New polyline with arcs: points and, per segment, the arc midpoint.
    let mut pts: Vec<P> = vec![p.pts[0]];
    let mut mids: Vec<Option<P>> = Vec::new();
    // How far along each segment a rounded corner may reach from each end.
    for i in 1..n - 1 {
        let (a, b, c) = (p.pts[i - 1], p.pts[i], p.pts[i + 1]);
        let corner = (|| {
            if p.pinned[i] {
                return None;
            }
            let (u1, l1) = unit(b, a);
            let (u2, l2) = unit(b, c);
            let cos = (u1.x * u2.x + u1.y * u2.y).clamp(-1.0, 1.0);
            let phi = cos.acos();
            if phi > std::f64::consts::PI - 1e-3 || phi < 1e-3 {
                return None;
            }
            // What the previous corner left of the first segment, and half of the next one
            // (all of it when it ends at a pinned vertex).
            let used = pts.last().map_or(0.0, |q| q.dist(a).min(l1));
            let next = if p.pinned[i + 1] { l2 } else { l2 / 2.0 };
            let avail = (l1 - used).min(next) - 1.0;
            let tan = (phi / 2.0).tan();
            let mut r = (avail * tan).min(max_r);
            while r >= min_r {
                let t = r / tan;
                let t1 = P::new(b.x + u1.x * t, b.y + u1.y * t);
                let t2 = P::new(b.x + u2.x * t, b.y + u2.y * t);
                let bis = unit(P::new(0.0, 0.0), P::new(u1.x + u2.x, u1.y + u2.y)).0;
                let off = r / (phi / 2.0).sin() - r;
                let m = P::new(b.x + bis.x * off, b.y + bis.y * off);
                let ch = chords(t1, m, t2);
                if ch.windows(2).all(|s| ck.seg_margin(p.layer, s[0], s[1], net, SAGITTA).is_none()) {
                    return Some((t1, m, t2));
                }
                r /= 2.0;
            }
            None
        })();
        match corner {
            Some((t1, m, t2)) => {
                if pts.last().is_some_and(|q| q.dist(t1) > 1.0) {
                    pts.push(t1);
                    mids.push(None);
                }
                pts.push(t2);
                mids.push(Some(m));
            }
            None => {
                pts.push(b);
                mids.push(None);
            }
        }
    }
    let last = p.pts[n - 1];
    if pts.last().is_some_and(|q| q.dist(last) > 1.0) {
        pts.push(last);
        mids.push(None);
    } else if let Some(l) = pts.last_mut() {
        *l = last;
    }
    if mids.iter().any(Option::is_some) {
        p.pinned = (0..pts.len()).map(|k| k == 0 || k + 1 == pts.len()).collect();
        p.pts = pts;
        p.mids = mids;
    }
}

/// Index entries of an arc segment: its chords.
pub(crate) fn insert_arc(index: &mut Index, net: u32, layer: usize, a: P, m: P, b: P, hw: f64) -> Vec<u32> {
    chords(a, m, b)
        .windows(2)
        .map(|s| {
            index.insert(Item::Seg { net, layer: layer as u8, a: s[0], b: s[1] }, BoxF::of2(s[0], s[1]).expand(hw))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_follow_the_arc() {
        let (a, m, b) = (P::new(1e6, 0.0), P::new(1e6 * 0.5f64.sqrt(), 1e6 * 0.5f64.sqrt()), P::new(0.0, 1e6));
        let c = chords(a, m, b);
        assert!(c.len() > 10);
        for q in &c {
            assert!((q.dist(P::new(0.0, 0.0)) - 1e6).abs() < 1.0);
        }
        let l = arc_length(a, m, b);
        assert!((l - std::f64::consts::FRAC_PI_2 * 1e6).abs() < 100.0, "{l}");
        assert_eq!(chords(a, P::new(5e5, 5e5), b), vec![a, b], "collinear");
    }
}
