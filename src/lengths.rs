//! Differential pairs and length matching: the rules a pair is routed with, routed lengths
//! (tracks along arcs plus vias through the stackup) along each net's signal path between its
//! end components, with the branches off it reported as stubs ([`path`]), coupled sections, the
//! DRC checks for pairs and length groups, and pair detection from net names. See
//! docs/ROUTER.md ("Differential pairs", "Length tuning") and DECISIONS D38, D55.
//!
//! Measured lengths are computed with floats and rounded to whole nanometers; nothing here is
//! stored.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{self as geo, CopperItem};
use crate::diag::Diagnostic;
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{Stackup, Track};
use crate::model::circuit::{Circuit, DiffPair, LengthGroup};
use crate::refs::ObjectRef;
use crate::units::Nm;

pub mod path;
pub use path::{DEFAULT_MAX_STUB, Ends, SignalPath, Stub, signal_path, signal_paths};

/// How far a coupled section's gap may be from the pair's gap before the DRC warns: 10 % of
/// the gap, at least [`GAP_TOLERANCE_MIN`].
pub const GAP_TOLERANCE: f64 = 0.10;
/// Smallest gap deviation the DRC reports.
pub const GAP_TOLERANCE_MIN: Nm = Nm(5_000);
/// Two parallel segments of a pair count as coupled when their edge-to-edge distance is at
/// most this many times the pair's gap.
pub const COUPLED_RANGE: f64 = 1.5;
/// Largest angle (as the sine) between two segments still counted as parallel (about 0.6°).
const PARALLEL_SIN: f64 = 0.01;
/// Arcs are measured for coupling as chords of at most this angle (degrees), so that concentric
/// arcs of a pair give parallel chords.
const ARC_STEP_DEG: f64 = 5.0;

/// The geometry a differential pair is routed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairRules {
    /// Track width of both nets: the class's `diff_pair_width`, else its `track_width`, else
    /// the rules'.
    pub width: Nm,
    /// Edge-to-edge gap between the two tracks: the class's `diff_pair_gap`, else the
    /// clearance.
    pub gap: Nm,
    /// Clearance of the pair's copper to other nets (the larger of both nets' clearances).
    pub clearance: Nm,
    /// Whether the class sets `diff_pair_width` and `diff_pair_gap`.
    pub from_class: bool,
}

/// The rules of a pair (see [`PairRules`]).
pub fn pair_rules(p: &Project, d: &DiffPair) -> PairRules {
    let c = p.circuit();
    let r = &p.board().rules;
    let class = c.diffpair_class(d);
    let net_clear = |n: &str| {
        c.nets
            .get(n)
            .and_then(|x| x.class.as_ref())
            .and_then(|k| c.netclasses.get(k))
            .and_then(|k| k.clearance)
            .unwrap_or(r.clearance)
    };
    let clearance = net_clear(&d.p).max(net_clear(&d.n)).max(class.and_then(|k| k.clearance).unwrap_or(Nm::ZERO));
    PairRules {
        width: class.and_then(|k| k.diff_pair_width.or(k.track_width)).unwrap_or(r.track_width),
        gap: class.and_then(|k| k.diff_pair_gap).unwrap_or(clearance),
        clearance,
        from_class: class.is_some_and(|k| k.diff_pair_width.is_some() && k.diff_pair_gap.is_some()),
    }
}

/// Track width of a net that belongs to a differential pair whose class sets
/// `diff_pair_width` (the router and the DRC use it instead of the class track width).
pub fn pair_width(c: &Circuit, net: &str) -> Option<Nm> {
    let (_, d) = c.diffpair_of(net)?;
    c.diffpair_class(d)?.diff_pair_width
}

/// The gap allowed between two nets when they form a differential pair (`None` otherwise):
/// the pair's `diff_pair_gap`. The DRC uses the smaller of it and the clearance between them.
pub fn pair_gap(c: &Circuit, a: &str, b: &str) -> Option<Nm> {
    let (_, d) = c.diffpair_of(a)?;
    if !((d.p == a && d.n == b) || (d.p == b && d.n == a)) {
        return None;
    }
    c.diffpair_class(d)?.diff_pair_gap
}

/// Height of the center of every copper layer below the top surface, from the stackup (the
/// dielectrics in effect, see [`Stackup::effective_dielectrics`]).
pub fn layer_depths(s: &Stackup) -> Vec<Nm> {
    let n = s.copper_layers.max(1) as usize;
    let (diel, _) = s.effective_dielectrics();
    let cu = |i: usize| if i == 0 || i + 1 == n { s.outer_copper } else { s.inner_copper };
    let mut z = vec![Nm(cu(0).0 / 2)];
    for i in 1..n {
        let d = diel.get(i - 1).map_or(Nm::ZERO, |d| d.thickness);
        z.push(Nm(z[i - 1].0 + cu(i - 1).0 / 2 + d.0 + cu(i).0 / 2));
    }
    z
}

/// Routed length of a net.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetLength {
    /// Track length, along arcs.
    pub tracks: Nm,
    /// Via length: for each via, the distance through the board between the outermost layers
    /// where the net's tracks meet it (layer centers, from the stackup).
    pub vias: Nm,
    /// `tracks + vias`.
    pub total: Nm,
}

/// Routed length of all of `net`'s copper, branches included (see [`NetLength`]; pairs and
/// length groups use [`signal_length`] / [`pair_paths`]).
pub fn net_length(p: &Project, net: &str) -> NetLength {
    let b = p.board();
    let tracks: Vec<&Track> = b.tracks.iter().filter(|t| t.net.as_deref() == Some(net)).collect();
    let t: f64 = tracks.iter().map(|t| geo::track_length(t).0 as f64).sum();
    let names = b.stackup.copper_names();
    let depth = layer_depths(&b.stackup);
    let mut v = 0i64;
    for via in b.vias.iter().filter(|v| v.net.as_deref() == Some(net)) {
        let r = via.diameter.0 as f64 / 2.0;
        let near = |q: Point| ((q.x.0 - via.at.x.0) as f64).hypot((q.y.0 - via.at.y.0) as f64) <= r.max(1.0);
        let layers: BTreeSet<usize> = tracks
            .iter()
            .filter(|t| near(t.start) || near(t.end))
            .filter_map(|t| names.iter().position(|n| *n == t.layer))
            .collect();
        if let (Some(&lo), Some(&hi)) = (layers.first(), layers.last()) {
            v += depth[hi].0 - depth[lo].0;
        }
    }
    let tracks = Nm(t.round() as i64);
    NetLength { tracks, vias: Nm(v), total: Nm(tracks.0 + v) }
}

/// A straight piece of a track (an arc gives several chords), for coupling analysis.
#[derive(Clone, Copy, Debug)]
struct Piece {
    a: (f64, f64),
    b: (f64, f64),
    width: f64,
    track: u64,
}

fn pieces(t: &Track) -> Vec<Piece> {
    let f = |q: Point| (q.x.0 as f64, q.y.0 as f64);
    let mk = |a, b| Piece { a, b, width: t.width.0 as f64, track: t.id.0 };
    let Some(m) = t.mid else { return vec![mk(f(t.start), f(t.end))] };
    let ((ax, ay), (mx, my), (bx, by)) = (f(t.start), f(m), f(t.end));
    let d = 2.0 * (ax * (my - by) + mx * (by - ay) + bx * (ay - my));
    if d.abs() < 1e-6 {
        return vec![mk(f(t.start), f(t.end))];
    }
    let (a2, m2, b2) = (ax * ax + ay * ay, mx * mx + my * my, bx * bx + by * by);
    let cx = (a2 * (my - by) + m2 * (by - ay) + b2 * (ay - my)) / d;
    let cy = (a2 * (bx - mx) + m2 * (ax - bx) + b2 * (mx - ax)) / d;
    let r = (ax - cx).hypot(ay - cy);
    let ang = |x: f64, y: f64| (y - cy).atan2(x - cx);
    let tau = std::f64::consts::TAU;
    let (t0, tm, t1) = (ang(ax, ay), ang(mx, my), ang(bx, by));
    let ccw = (tm - t0).rem_euclid(tau) < (t1 - t0).rem_euclid(tau);
    let sweep = if ccw { (t1 - t0).rem_euclid(tau) } else { -(t0 - t1).rem_euclid(tau) };
    let k = ((sweep.abs().to_degrees() / ARC_STEP_DEG).ceil() as usize).max(1);
    let at = |i: usize| {
        if i == 0 {
            (ax, ay)
        } else if i == k {
            (bx, by)
        } else {
            let a = t0 + sweep * i as f64 / k as f64;
            (cx + r * a.cos(), cy + r * a.sin())
        }
    };
    (0..k).map(|i| mk(at(i), at(i + 1))).collect()
}

/// A stretch where the two nets of a pair run side by side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CoupledSection {
    /// Copper layer.
    pub layer: String,
    /// Length along the positive net.
    pub length: Nm,
    /// Edge-to-edge gap.
    pub gap: Nm,
    /// Track widths (positive, negative net).
    pub widths: (Nm, Nm),
    /// Middle of the section.
    pub at: Point,
    /// Tracks involved (positive, negative net).
    pub tracks: (u64, u64),
}

/// How a routed pair is coupled.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Coupling {
    /// Routed length of the positive net along its signal path (all its copper when the
    /// path's ends are not connected yet).
    pub length_p: NetLength,
    /// Routed length of the negative net along its signal path.
    pub length_n: NetLength,
    /// `|length_p − length_n|` (totals, vias included).
    pub skew: Nm,
    /// Track length of the positive net's signal path running coupled.
    pub coupled_p: Nm,
    /// Track length of the negative net's signal path running coupled.
    pub coupled_n: Nm,
    /// Largest uncoupled track length along the two signal paths.
    pub uncoupled: Nm,
    /// The coupled sections (pairs of parallel segments).
    pub sections: Vec<CoupledSection>,
    /// The positive net's signal path (with its stubs), once its ends are connected.
    pub path_p: Option<SignalPath>,
    /// The negative net's signal path.
    pub path_n: Option<SignalPath>,
}

/// The end components a pair's length is measured between: its own `from`/`to`, else those
/// of a length group it is in.
pub fn pair_ends<'a>(c: &'a Circuit, d: &'a DiffPair) -> Ends<'a> {
    let name = c.diffpairs.iter().find(|(_, x)| x.p == d.p && x.n == d.n).map(|(k, _)| k.as_str());
    let group = name.and_then(|n| c.length_groups.values().find(|g| g.members.iter().any(|m| m == n)));
    Ends {
        from: d.from.as_deref().or(group.and_then(|g| g.from.as_deref())),
        to: d.to.as_deref().or(group.and_then(|g| g.to.as_deref())),
    }
}

/// The end components a net's length is measured between: those of a length group it is in.
pub fn net_ends<'a>(c: &'a Circuit, net: &str) -> Ends<'a> {
    let g = c.length_groups.values().find(|g| g.members.iter().any(|m| m == net));
    Ends { from: g.and_then(|g| g.from.as_deref()), to: g.and_then(|g| g.to.as_deref()) }
}

/// The signal paths of a pair's two nets (see [`signal_paths`]).
pub fn pair_paths(p: &Project, d: &DiffPair) -> (Option<SignalPath>, Option<SignalPath>) {
    let mut v = signal_paths(p, &[&d.p, &d.n], pair_ends(p.circuit(), d));
    let n = v.pop().flatten();
    (v.pop().flatten(), n)
}

/// A net's length along its signal path (all its copper when its ends are not connected).
pub fn signal_length(p: &Project, net: &str) -> NetLength {
    match signal_path(p, net, net_ends(p.circuit(), net)) {
        Some(sp) => sp.length,
        None => net_length(p, net),
    }
}

/// Length covered by a union of intervals.
fn union_len(mut iv: Vec<(f64, f64)>) -> f64 {
    iv.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut total = 0.0;
    let mut cur: Option<(f64, f64)> = None;
    for (lo, hi) in iv {
        match &mut cur {
            Some((_, e)) if lo <= *e => *e = e.max(hi),
            _ => {
                if let Some((s, e)) = cur {
                    total += e - s;
                }
                cur = Some((lo, hi));
            }
        }
    }
    if let Some((s, e)) = cur {
        total += e - s;
    }
    total
}

/// Coupling of a pair's routed tracks with the given `gap` (see [`Coupling`]).
pub fn coupling(p: &Project, d: &DiffPair, gap: Nm) -> Coupling {
    let b = p.board();
    let of = |net: &str| -> Vec<(&Track, Vec<Piece>)> {
        b.tracks.iter().filter(|t| t.net.as_deref() == Some(net)).map(|t| (t, pieces(t))).collect()
    };
    let (tp, tn) = (of(&d.p), of(&d.n));
    let (path_p, path_n) = pair_paths(p, d);
    let range = gap.0 as f64 * COUPLED_RANGE + 1_000.0;
    // Per piece (net, track index, piece index): covered intervals along it.
    let mut cov_p: BTreeMap<(usize, usize), Vec<(f64, f64)>> = BTreeMap::new();
    let mut cov_n: BTreeMap<(usize, usize), Vec<(f64, f64)>> = BTreeMap::new();
    let mut sections = Vec::new();
    for (i, (ta, pa)) in tp.iter().enumerate() {
        for (j, (tb, pb)) in tn.iter().enumerate() {
            if ta.layer != tb.layer {
                continue;
            }
            for (ki, x) in pa.iter().enumerate() {
                let (ux, uy) = (x.b.0 - x.a.0, x.b.1 - x.a.1);
                let lx = ux.hypot(uy);
                if lx < 1.0 {
                    continue;
                }
                let (ux, uy) = (ux / lx, uy / lx);
                for (kj, y) in pb.iter().enumerate() {
                    let (vx, vy) = (y.b.0 - y.a.0, y.b.1 - y.a.1);
                    let ly = vx.hypot(vy);
                    if ly < 1.0 || (ux * vy - uy * vx).abs() / ly > PARALLEL_SIN {
                        continue;
                    }
                    let mid = ((y.a.0 + y.b.0) / 2.0, (y.a.1 + y.b.1) / 2.0);
                    let dl = (ux * (mid.1 - x.a.1) - uy * (mid.0 - x.a.0)).abs();
                    let eg = dl - (x.width + y.width) / 2.0;
                    if eg <= 0.0 || eg > range {
                        continue;
                    }
                    let t = |q: (f64, f64)| (q.0 - x.a.0) * ux + (q.1 - x.a.1) * uy;
                    let (s0, s1) = (t(y.a).min(t(y.b)), t(y.a).max(t(y.b)));
                    let (lo, hi) = (s0.max(0.0), s1.min(lx));
                    if hi - lo < 1.0 {
                        continue;
                    }
                    cov_p.entry((i, ki)).or_default().push((lo, hi));
                    // The same stretch along the negative net's piece.
                    let tb_ = |q: (f64, f64)| ((q.0 - y.a.0) * vx + (q.1 - y.a.1) * vy) / ly;
                    let pl = |s: f64| (x.a.0 + ux * s, x.a.1 + uy * s);
                    let (r0, r1) = (tb_(pl(lo)), tb_(pl(hi)));
                    cov_n.entry((j, kj)).or_default().push((r0.min(r1).max(0.0), r0.max(r1).min(ly)));
                    let c = (lo + hi) / 2.0;
                    sections.push(CoupledSection {
                        layer: ta.layer.clone(),
                        length: Nm((hi - lo).round() as i64),
                        gap: Nm(eg.round() as i64),
                        widths: (ta.width, tb.width),
                        at: Point::new(Nm((x.a.0 + ux * c).round() as i64), Nm((x.a.1 + uy * c).round() as i64)),
                        tracks: (x.track, y.track),
                    });
                }
            }
        }
    }
    // Coupled length along the signal path only (a stub running beside the partner does
    // not count).
    let coupled = |cov: BTreeMap<(usize, usize), Vec<(f64, f64)>>,
                   tr: &[(&Track, Vec<Piece>)],
                   path: &Option<SignalPath>|
     -> Nm {
        let total: f64 = cov
            .into_iter()
            .map(|((ti, _), iv)| {
                let t = tr[ti].0;
                match path {
                    None => union_len(iv),
                    Some(sp) if t.mid.is_some() => {
                        if sp.contains_track(t.id.0) {
                            union_len(iv)
                        } else {
                            0.0
                        }
                    }
                    Some(sp) => {
                        let on = sp.stretches(t.id.0);
                        let cut: Vec<(f64, f64)> = iv
                            .iter()
                            .flat_map(|&(a, b)| on.iter().map(move |&(c, e)| (a.max(c), b.min(e))))
                            .filter(|(a, b)| b > a)
                            .collect();
                        union_len(cut)
                    }
                }
            })
            .sum();
        Nm(total.round() as i64)
    };
    let (coupled_p, coupled_n) = (coupled(cov_p, &tp, &path_p), coupled(cov_n, &tn, &path_n));
    let len = |sp: &Option<SignalPath>, net: &str| sp.as_ref().map_or_else(|| net_length(p, net), |s| s.length);
    let (length_p, length_n) = (len(&path_p, &d.p), len(&path_n, &d.n));
    let uncoupled = Nm((length_p.tracks.0 - coupled_p.0).max(length_n.tracks.0 - coupled_n.0).max(0));
    Coupling {
        skew: Nm((length_p.total.0 - length_n.total.0).abs()),
        length_p,
        length_n,
        coupled_p,
        coupled_n,
        uncoupled,
        sections,
        path_p,
        path_n,
    }
}

/// Nets with unrouted connections, from copper items.
pub fn unrouted_nets(items: &[CopperItem]) -> BTreeSet<String> {
    geo::ratsnest_items(items).into_iter().map(|l| l.net).collect()
}

/// Whether a net has pads and no unrouted connection.
fn complete(p: &Project, net: &str, unrouted: &BTreeSet<String>) -> bool {
    !unrouted.contains(net) && p.board().tracks.iter().any(|t| t.net.as_deref() == Some(net))
}

/// The length of a group member along its signal path: a net's, or a pair's mean (`None` when
/// the member is not fully routed or does not exist).
pub fn member_length(p: &Project, name: &str, unrouted: &BTreeSet<String>) -> Option<Nm> {
    let c = p.circuit();
    if let Some(d) = c.diffpairs.get(name) {
        if !complete(p, &d.p, unrouted) || !complete(p, &d.n, unrouted) {
            return None;
        }
        let (pp, pn) = pair_paths(p, d);
        let len = |sp: Option<SignalPath>, net: &str| sp.map_or_else(|| net_length(p, net), |s| s.length).total;
        let (a, b) = (len(pp, &d.p), len(pn, &d.n));
        return Some(Nm((a.0 + b.0 + 1) / 2));
    }
    (c.nets.contains_key(name) && complete(p, name, unrouted)).then(|| signal_length(p, name).total)
}

/// Where a length group stands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GroupStatus {
    /// Group name.
    pub name: String,
    /// The length every member aims at: the target, else the longest routed member.
    pub target: Option<Nm>,
    /// Allowed range (inclusive).
    pub min: Option<Nm>,
    /// Allowed range (inclusive).
    pub max: Option<Nm>,
    /// Per member.
    pub members: Vec<MemberStatus>,
}

/// A length group member's state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemberStatus {
    /// Net or pair name.
    pub name: String,
    /// `net` or `pair`.
    pub kind: String,
    /// Routed length (`None`: not fully routed).
    pub length: Option<Nm>,
    /// Signed distance to the allowed range (negative: too short); zero inside it.
    pub error: Option<Nm>,
}

/// Evaluates a length group.
pub fn group_status(p: &Project, name: &str, g: &LengthGroup, unrouted: &BTreeSet<String>) -> GroupStatus {
    let c = p.circuit();
    let lens: Vec<(String, String, Option<Nm>)> = g
        .members
        .iter()
        .map(|m| {
            let kind = if c.diffpairs.contains_key(m) { "pair" } else { "net" };
            (m.clone(), kind.to_string(), member_length(p, m, unrouted))
        })
        .collect();
    let longest = lens.iter().filter_map(|x| x.2).max();
    let target = g.target.or(longest);
    let (min, max) = match (g.target, target) {
        (Some(t), _) => (Some(Nm(t.0 - g.tolerance.0)), Some(Nm(t.0 + g.tolerance.0))),
        (None, Some(t)) => (Some(Nm(t.0 - g.tolerance.0)), Some(t)),
        _ => (None, None),
    };
    let members = lens
        .into_iter()
        .map(|(name, kind, length)| {
            let error = match (length, min, max) {
                (Some(l), Some(lo), Some(hi)) => Some(if l < lo {
                    Nm(l.0 - lo.0)
                } else if l > hi {
                    Nm(l.0 - hi.0)
                } else {
                    Nm::ZERO
                }),
                _ => None,
            };
            MemberStatus { name, kind, length, error }
        })
        .collect();
    GroupStatus { name: name.to_string(), target, min, max, members }
}

fn pair_subject(name: &str) -> ObjectRef {
    ObjectRef::Named { kind: "diffpair".into(), name: name.to_string() }
}

/// DRC checks of differential pairs and length groups (all warnings): `drc.diffpair_invalid`
/// (a pair naming a missing net), `drc.diffpair_gap` and `drc.diffpair_width` (coupled
/// sections whose gap or width differ from the pair's), `drc.diffpair_uncoupled` (uncoupled
/// length over `max_uncoupled`), `drc.diffpair_skew` (skew over `max_skew`) and
/// `drc.length_mismatch` (a length group member outside its range). Lengths are checked only
/// on fully routed nets.
pub fn checks(p: &Project, items: &[CopperItem]) -> Vec<Diagnostic> {
    let c = p.circuit();
    if c.diffpairs.is_empty() && c.length_groups.is_empty() {
        return vec![];
    }
    let unrouted = unrouted_nets(items);
    let mut out = Vec::new();
    for (name, d) in &c.diffpairs {
        let missing: Vec<&String> = [&d.p, &d.n].into_iter().filter(|n| !c.nets.contains_key(n.as_str())).collect();
        if !missing.is_empty() {
            out.push(
                Diagnostic::warning(
                    "drc.diffpair_invalid",
                    format!(
                        "differential pair {name}: no net {}",
                        missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                )
                .with_subject(pair_subject(name))
                .with_hint("fix it with diffpair.add (new nets) or remove it with diffpair.remove"),
            );
            continue;
        }
        let r = pair_rules(p, d);
        let cp = coupling(p, d, r.gap);
        let nets = || [ObjectRef::Net(d.p.clone()), ObjectRef::Net(d.n.clone())];
        // Gap and width along coupled sections: the worst per layer.
        let tol = (r.gap.0 as f64 * GAP_TOLERANCE).max(GAP_TOLERANCE_MIN.0 as f64);
        let mut worst_gap: BTreeMap<&str, &CoupledSection> = BTreeMap::new();
        let mut bad_width: BTreeMap<(&str, Nm), &CoupledSection> = BTreeMap::new();
        for s in &cp.sections {
            let dev = (s.gap.0 - r.gap.0).abs() as f64;
            if dev > tol {
                let e = worst_gap.entry(s.layer.as_str()).or_insert(s);
                if dev > (e.gap.0 - r.gap.0).abs() as f64 {
                    *e = s;
                }
            }
            for w in [s.widths.0, s.widths.1] {
                if (w.0 - r.width.0).abs() > 1_000 {
                    bad_width.entry((s.layer.as_str(), w)).or_insert(s);
                }
            }
        }
        for (layer, s) in worst_gap {
            let mut dg = Diagnostic::warning(
                "drc.diffpair_gap",
                format!(
                    "differential pair {name}: gap {} on {layer} over {} of coupled track, the pair asks for {}",
                    s.gap, s.length, r.gap
                ),
            )
            .at(s.at)
            .with_subject(pair_subject(name))
            .with_hint("rip and route the pair again (route.diffpair), or set the gap with netclass.set diff_pair_gap");
            dg.subjects.extend(nets());
            dg.subjects.push(ObjectRef::Item { kind: "track".into(), index: s.tracks.0 });
            dg.subjects.push(ObjectRef::Item { kind: "track".into(), index: s.tracks.1 });
            out.push(dg);
        }
        for ((layer, w), s) in bad_width {
            let mut dg = Diagnostic::warning(
                "drc.diffpair_width",
                format!(
                    "differential pair {name}: {w} wide track on {layer} in a coupled section, the pair asks for {}",
                    r.width
                ),
            )
            .at(s.at)
            .with_subject(pair_subject(name))
            .with_hint("rip and route the pair again (route.diffpair) to use the class's diff_pair_width");
            dg.subjects.extend(nets());
            out.push(dg);
        }
        let routed = complete(p, &d.p, &unrouted) && complete(p, &d.n, &unrouted);
        if let Some(lim) = d.max_uncoupled
            && cp.uncoupled > lim
            && (cp.length_p.tracks > Nm::ZERO || cp.length_n.tracks > Nm::ZERO)
        {
            let mut dg = Diagnostic::warning(
                "drc.diffpair_uncoupled",
                format!("differential pair {name}: {} of track runs uncoupled, the limit is {lim}", cp.uncoupled),
            )
            .with_subject(pair_subject(name))
            .with_hint("route the pair coupled (route.diffpair) with its pads closer together, or raise max_uncoupled (diffpair.add)");
            dg.subjects.extend(nets());
            out.push(dg);
        }
        if let Some(lim) = d.max_skew
            && routed
            && cp.skew > lim
        {
            let mut dg = Diagnostic::warning(
                "drc.diffpair_skew",
                format!(
                    "differential pair {name}: {} is {} and {} is {}, skew {} over the {lim} allowed",
                    d.p, cp.length_p.total, d.n, cp.length_n.total, cp.skew
                ),
            )
            .with_subject(pair_subject(name))
            .with_hint("compensate it with route.tune (small meanders on the shorter net)");
            dg.subjects.extend(nets());
            out.push(dg);
        }
        if routed {
            let group_limit =
                c.length_groups.values().filter(|g| g.members.iter().any(|m| m == name)).find_map(|g| g.max_stub);
            let lim = d.max_stub.or(group_limit).unwrap_or(DEFAULT_MAX_STUB);
            for sp in [&cp.path_p, &cp.path_n].into_iter().flatten() {
                out.extend(stub_warnings(sp, lim, "drc.diffpair_stub", &format!("differential pair {name}"), || {
                    let mut v = vec![pair_subject(name)];
                    v.extend(nets());
                    v
                }));
            }
        }
    }
    for (name, g) in &c.length_groups {
        let lim = g.max_stub.unwrap_or(DEFAULT_MAX_STUB);
        for m in g.members.iter().filter(|m| !c.diffpairs.contains_key(*m) && complete(p, m, &unrouted)) {
            let Some(sp) = signal_path(p, m, net_ends(c, m)) else { continue };
            out.extend(stub_warnings(&sp, lim, "drc.length_stub", &format!("length group {name}"), || {
                vec![ObjectRef::Named { kind: "lengthgroup".into(), name: name.clone() }, ObjectRef::Net(m.clone())]
            }));
        }
        let st = group_status(p, name, g, &unrouted);
        for m in &st.members {
            let (Some(len), Some(err)) = (m.length, m.error) else { continue };
            if err == Nm::ZERO {
                continue;
            }
            let want = match g.target {
                Some(t) => format!("{t} ±{}", g.tolerance),
                None => format!("within {} of the longest ({})", g.tolerance, st.target.unwrap_or_default()),
            };
            let what = if err < Nm::ZERO { "short" } else { "long" };
            let mut dg = Diagnostic::warning(
                "drc.length_mismatch",
                format!(
                    "length group {name}: {} {} is {len}, the group wants {want} ({} too {what})",
                    m.kind,
                    m.name,
                    err.abs()
                ),
            )
            .with_subject(ObjectRef::Named { kind: "lengthgroup".into(), name: name.clone() })
            .with_hint(if err < Nm::ZERO {
                "add meanders with route.tune".to_string()
            } else {
                "shorten the route (rip and route again) or raise the target; route.tune only adds length".to_string()
            });
            match c.diffpairs.get(&m.name) {
                Some(d) => {
                    dg.subjects.push(pair_subject(&m.name));
                    dg.subjects.push(ObjectRef::Net(d.p.clone()));
                    dg.subjects.push(ObjectRef::Net(d.n.clone()));
                }
                None => dg.subjects.push(ObjectRef::Net(m.name.clone())),
            }
            out.push(dg);
        }
    }
    out
}

/// Warnings for the stubs of a signal path longer than `lim`.
fn stub_warnings(
    sp: &SignalPath,
    lim: Nm,
    code: &'static str,
    what: &str,
    subjects: impl Fn() -> Vec<ObjectRef>,
) -> Vec<Diagnostic> {
    sp.stubs
        .iter()
        .filter(|s| s.length > lim)
        .map(|s| {
            let to = if s.pads.is_empty() { String::new() } else { format!(" to {}", s.pads.join(", ")) };
            let mut dg = Diagnostic::warning(
                code,
                format!(
                    "{what}: {} has a {} stub{to} off its signal path {} → {} (limit {lim})",
                    sp.net, s.length, sp.from, sp.to
                ),
            )
            .at(s.at)
            .with_hint(
                "place the part next to the line (or route the line through its pad), set the path's ends \
                 (diffpair.set --from --to, lengthgroup.set --from --to), or raise the limit (--max-stub)",
            );
            dg.subjects = subjects();
            dg.subjects.extend(s.pads.iter().filter_map(|x| {
                let (r, n) = x.split_once('.')?;
                Some(ObjectRef::Pin { component: r.to_string(), pin: n.to_string() })
            }));
            dg
        })
        .collect()
}

/// A differential pair found by net names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PairSuggestion {
    /// Suggested pair name.
    pub name: String,
    /// Positive net.
    pub p: String,
    /// Negative net.
    pub n: String,
    /// The naming convention matched (`DP/DM`, `_P/_N`, `+/-`, ...).
    pub rule: String,
}

/// Suffix conventions, most specific first: (positive, negative).
const SUFFIXES: &[(&str, &str)] =
    &[("DP", "DM"), ("DP", "DN"), ("_P", "_N"), ("+", "-"), ("_POS", "_NEG"), ("P", "N"), ("P", "M")];

/// Pairs suggested by net names: `X_P`/`X_N`, `X+`/`X-`, `XDP`/`XDM` (or `DN`), `XP`/`XN`,
/// matched case-insensitively on the suffix with the same stem; nets already in a pair are
/// left out.
pub fn suggest_pairs(c: &Circuit) -> Vec<PairSuggestion> {
    let taken: BTreeSet<&str> = c.diffpairs.values().flat_map(|d| [d.p.as_str(), d.n.as_str()]).collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut names: BTreeSet<String> = c.diffpairs.keys().cloned().collect();
    let mut out = Vec::new();
    for net in c.nets.keys() {
        if taken.contains(net.as_str()) || used.contains(net) {
            continue;
        }
        for (pos, neg) in SUFFIXES {
            if net.len() <= pos.len() || !net.to_ascii_uppercase().ends_with(pos) {
                continue;
            }
            let stem = &net[..net.len() - pos.len()];
            // Single-letter suffixes need a stem of two characters or more.
            if pos.len() == 1 && pos.chars().all(|ch| ch.is_ascii_alphabetic()) && stem.len() < 2 {
                continue;
            }
            let lower = net[net.len() - pos.len()..].chars().any(|ch| ch.is_ascii_lowercase());
            let want = format!("{stem}{}", if lower { neg.to_ascii_lowercase() } else { neg.to_string() });
            let Some(partner) = c.nets.keys().find(|k| k.eq_ignore_ascii_case(&want)) else { continue };
            if partner == net || taken.contains(partner.as_str()) || used.contains(partner) {
                continue;
            }
            let base = default_pair_name(net, partner);
            let mut name = base.clone();
            let mut k = 2;
            while names.contains(&name) {
                name = format!("{base}_{k}");
                k += 1;
            }
            names.insert(name.clone());
            used.insert(net.clone());
            used.insert(partner.clone());
            out.push(PairSuggestion { name, p: net.clone(), n: partner.clone(), rule: format!("{pos}/{neg}") });
            break;
        }
    }
    out
}

/// A pair's default name: the common prefix of its nets without trailing separators
/// (`USB_DP`/`USB_DM` → `USB_D`), else both names joined.
pub fn default_pair_name(p: &str, n: &str) -> String {
    let common: String = p.chars().zip(n.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect();
    let t = common.trim_end_matches(['_', '-', '+', '/', '.', ' ']);
    if t.is_empty() { format!("{p}_{n}") } else { t.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ObjectId;
    use crate::model::circuit::Net;

    #[test]
    fn names_and_suggestions() {
        assert_eq!(default_pair_name("USB_DP", "USB_DM"), "USB_D");
        assert_eq!(default_pair_name("USB_D+", "USB_D-"), "USB_D");
        assert_eq!(default_pair_name("LVDS0_P", "LVDS0_N"), "LVDS0");
        assert_eq!(default_pair_name("A", "B"), "A_B");
        let mut c = Circuit::default();
        for (i, n) in ["USB_DP", "USB_DM", "CLK+", "CLK-", "TX0_P", "TX0_N", "GND", "VCCP", "SDA", "eth_rxp", "eth_rxn"]
            .into_iter()
            .enumerate()
        {
            c.nets.insert(n.into(), Net::new(ObjectId(i as u64 + 1)));
        }
        let s = suggest_pairs(&c);
        let got: Vec<(&str, &str, &str)> = s.iter().map(|x| (x.name.as_str(), x.p.as_str(), x.n.as_str())).collect();
        assert_eq!(
            got,
            [
                ("CLK", "CLK+", "CLK-"),
                ("TX0", "TX0_P", "TX0_N"),
                ("USB_D", "USB_DP", "USB_DM"),
                ("eth_rx", "eth_rxp", "eth_rxn")
            ]
        );
    }

    #[test]
    fn depths_and_unions() {
        let s = Stackup { copper_layers: 4, ..Default::default() };
        let z = layer_depths(&s);
        assert_eq!(z.len(), 4);
        // Outer layer centers: the board thickness minus one outer copper thickness.
        assert!((z[3].0 - z[0].0 - (s.thickness.0 - s.outer_copper.0)).abs() <= 3, "{z:?}");
        assert_eq!(union_len(vec![(0.0, 2.0), (1.0, 3.0), (5.0, 6.0)]), 4.0);
    }
}
