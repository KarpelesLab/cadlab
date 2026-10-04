//! Length tuning with meanders (docs/ROUTER.md, "Length tuning").
//!
//! Length is added to a net by replacing parts of its straight tracks with *bumps*: trombone
//! (U-shaped bumps on one side), accordion (U-shaped bumps alternating sides) or sawtooth
//! (triangular teeth), with chamfered or (option `arcs`) round corners. Bumps are placed along
//! the longest straight tracks first, at a pitch of twice the leg spacing; each is checked
//! exactly against other nets' copper (clearance) and the net's own other copper (clearance as
//! spacing), and its amplitude is halved until it fits. The last bump's amplitude is solved
//! from the closed form of its added length, so the target is met to the nanometer (before
//! rounding of the vertices). A differential pair is tuned as a pair: the bumps are drawn on the
//! centerline of a coupled straight stretch and offset to both tracks (concentric arcs, mitered
//! chamfers), which adds the same length to both nets. Intra-pair skew is compensated by small
//! 45° triangular bumps on the shorter net, pointing away from its partner, on the coupled
//! stretches nearest to the bends where it lost length. Everything is DRC-checked at the end;
//! a member whose meanders the DRC flags is put back as it was and reported.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::ObjectId;
use crate::lengths::{self, COUPLED_RANGE};
use crate::model::Project;
use crate::model::board::Track;
use crate::model::circuit::DiffPair;
use crate::units::Nm;

use super::RouteError;
use super::arcs::{SAGITTA, chords};
use super::geo::{P, Shape, seg_seg_dist};
use super::index::{Checker, Index, Item, TOL};
use super::model::{ObKind, RouterBoard};

/// Meander shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MeanderStyle {
    /// U-shaped bumps on one side of the track (the default).
    #[default]
    Trombone,
    /// U-shaped bumps alternating sides.
    Accordion,
    /// Triangular teeth.
    Sawtooth,
}

/// Meander parameters.
#[derive(Clone, Debug, Default)]
pub struct TuneOptions {
    /// Shape.
    pub style: MeanderStyle,
    /// Largest bump height (default 1 mm, at least 2.5 spacings).
    pub amplitude: Option<Nm>,
    /// Center distance between neighboring legs of a net (default 4 track widths, at least
    /// width + clearance); a pair's legs keep this between its tracks and more than 1.5 gaps.
    pub spacing: Option<Nm>,
    /// Corner size: chamfer leg or arc radius (default a quarter of the spacing).
    pub corner: Option<Nm>,
    /// Round corners (arcs) instead of chamfers (trombone and accordion).
    pub arcs: bool,
    /// Compensate the intra-pair skew of differential pairs (default in `tune`: on).
    pub skew: bool,
}

/// Result of [`tune`]: tracks to add (IDs allocated in order from the project's allocator)
/// and to remove, with what was achieved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TuneResult {
    /// New tracks.
    pub tracks: Vec<Track>,
    /// Tracks replaced by meanders.
    pub removed_tracks: Vec<ObjectId>,
    /// Length groups.
    pub groups: Vec<GroupTuned>,
    /// Differential pairs whose skew was looked at.
    pub pairs: Vec<SkewTuned>,
}

/// A length group after tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GroupTuned {
    /// Group name.
    pub name: String,
    /// Length aimed at (the target, else the longest member).
    pub target: Option<Nm>,
    /// Allowed range.
    pub min: Option<Nm>,
    /// Allowed range.
    pub max: Option<Nm>,
    /// Members.
    pub members: Vec<MemberTuned>,
}

/// A member after tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemberTuned {
    /// Net or pair.
    pub name: String,
    /// `net` or `pair`.
    pub kind: String,
    /// Length before.
    pub before: Option<Nm>,
    /// Length after.
    pub after: Option<Nm>,
    /// Signed distance to the allowed range after tuning (0: within it; negative: short).
    pub error: Option<Nm>,
    /// Bumps added.
    pub meanders: usize,
    /// Why the member is not within range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A pair's skew after compensation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkewTuned {
    /// Pair name.
    pub name: String,
    /// Skew before.
    pub before: Nm,
    /// Skew after.
    pub after: Nm,
    /// The pair's limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_skew: Option<Nm>,
    /// Bumps added to the shorter net.
    pub bumps: usize,
    /// Why skew is left (when over the limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Skew under which a pair is left alone (nm).
const SKEW_IGNORE: f64 = 2_000.0;
/// `8 − 4√2`: length lost by the four chamfers of a U bump, per unit of chamfer.
const K_CHAMFER: f64 = 8.0 - 4.0 * std::f64::consts::SQRT_2;
/// `8 − 2π`: length lost by the four quarter arcs of a U bump, per unit of radius.
const K_ARC: f64 = 8.0 - 2.0 * std::f64::consts::PI;
/// Added length of a 45° triangular skew bump per unit of height: `2√2 − 2`.
const K_TRI: f64 = 2.0 * std::f64::consts::SQRT_2 - 2.0;

// ---------------------------------------------------------------------------------------------
// Geometry

/// A straight segment or a circular arc (center, counter-clockwise).
#[derive(Clone, Copy, Debug)]
struct Prim {
    a: P,
    b: P,
    arc: Option<(P, bool)>,
}

fn add(a: P, b: P, s: f64) -> P {
    P::new(a.x + b.x * s, a.y + b.y * s)
}

fn unit(a: P, b: P) -> P {
    let l = a.dist(b).max(1e-9);
    P::new((b.x - a.x) / l, (b.y - a.y) / l)
}

fn left(u: P) -> P {
    P::new(-u.y, u.x)
}

fn dot(a: P, b: P) -> f64 {
    a.x * b.x + a.y * b.y
}

fn cross(a: P, b: P) -> f64 {
    a.x * b.y - a.y * b.x
}

impl Prim {
    fn line(a: P, b: P) -> Prim {
        Prim { a, b, arc: None }
    }

    /// Sweep angle (radians, positive) of an arc.
    fn sweep(&self) -> f64 {
        let Some((c, ccw)) = self.arc else { return 0.0 };
        let (t0, t1) = ((self.a.y - c.y).atan2(self.a.x - c.x), (self.b.y - c.y).atan2(self.b.x - c.x));
        let tau = std::f64::consts::TAU;
        if ccw { (t1 - t0).rem_euclid(tau) } else { (t0 - t1).rem_euclid(tau) }
    }

    fn len(&self) -> f64 {
        match self.arc {
            None => self.a.dist(self.b),
            Some((c, _)) => c.dist(self.a) * self.sweep(),
        }
    }

    /// The arc's midpoint.
    fn mid(&self) -> Option<P> {
        let (c, ccw) = self.arc?;
        let t0 = (self.a.y - c.y).atan2(self.a.x - c.x);
        let t = t0 + if ccw { 1.0 } else { -1.0 } * self.sweep() / 2.0;
        let r = c.dist(self.a);
        Some(P::new(c.x + r * t.cos(), c.y + r * t.sin()))
    }

    /// Chords for exact checks (an arc as chords within [`SAGITTA`]).
    fn chords(&self) -> Vec<P> {
        match self.mid() {
            Some(m) => chords(self.a, m, self.b),
            None => vec![self.a, self.b],
        }
    }

    /// Offset by `d` to the left of the travel direction (arcs stay concentric).
    fn offset(&self, d: f64) -> Prim {
        match self.arc {
            None => {
                let n = left(unit(self.a, self.b));
                Prim::line(add(self.a, n, d), add(self.b, n, d))
            }
            Some((c, ccw)) => {
                let r = c.dist(self.a);
                let r2 = if ccw { r - d } else { r + d };
                let k = r2 / r;
                let sc = |q: P| P::new(c.x + (q.x - c.x) * k, c.y + (q.y - c.y) * k);
                Prim { a: sc(self.a), b: sc(self.b), arc: Some((c, ccw)) }
            }
        }
    }
}

/// Offsets a connected chain of primitives by `d` (left positive), mitering line-to-line
/// corners. `None` when a piece would invert or vanish.
fn offset_chain(chain: &[Prim], d: f64) -> Option<Vec<Prim>> {
    let mut out: Vec<Prim> = chain.iter().map(|p| p.offset(d)).collect();
    for (o, p) in out.iter().zip(chain) {
        if let Some((c, _)) = p.arc
            && c.dist(o.a) < 1_000.0
        {
            return None;
        }
    }
    for i in 0..out.len().saturating_sub(1) {
        if out[i].arc.is_none() && out[i + 1].arc.is_none() {
            let (u1, u2) = (unit(chain[i].a, chain[i].b), unit(chain[i + 1].a, chain[i + 1].b));
            let den = cross(u1, u2);
            if den.abs() > 1e-9 {
                let (p1, p2) = (out[i].a, out[i + 1].a);
                let t = cross(P::new(p2.x - p1.x, p2.y - p1.y), u2) / den;
                let x = add(p1, u1, t);
                out[i].b = x;
                out[i + 1].a = x;
            }
        }
    }
    for (o, p) in out.iter().zip(chain) {
        if p.arc.is_none() && (dot(unit(o.a, o.b), unit(p.a, p.b)) <= 0.0 || o.a.dist(o.b) < 1.0) {
            return None;
        }
    }
    Some(out)
}

/// A bump in local coordinates (`t` along the track, `h` to its side), with where it starts
/// and ends along the track and the length it adds.
struct Bump {
    prims: Vec<Prim>,
    t0: f64,
    t1: f64,
    added: f64,
}

/// U bump: legs at `x` and `x + s`, height `amp`, corners `m` (chamfer leg or arc radius).
fn u_bump(x: f64, s: f64, amp: f64, m: f64, arcs: bool) -> Bump {
    let m = m.min(amp / 2.0).min(s / 2.0);
    let q = P::new;
    let prims = if arcs && m > 1.0 {
        vec![
            Prim { a: q(x - m, 0.0), b: q(x, m), arc: Some((q(x - m, m), true)) },
            Prim::line(q(x, m), q(x, amp - m)),
            Prim { a: q(x, amp - m), b: q(x + m, amp), arc: Some((q(x + m, amp - m), false)) },
            Prim::line(q(x + m, amp), q(x + s - m, amp)),
            Prim { a: q(x + s - m, amp), b: q(x + s, amp - m), arc: Some((q(x + s - m, amp - m), false)) },
            Prim::line(q(x + s, amp - m), q(x + s, m)),
            Prim { a: q(x + s, m), b: q(x + s + m, 0.0), arc: Some((q(x + s + m, m), true)) },
        ]
    } else {
        let pts = [
            q(x - m, 0.0),
            q(x, m),
            q(x, amp - m),
            q(x + m, amp),
            q(x + s - m, amp),
            q(x + s, amp - m),
            q(x + s, m),
            q(x + s + m, 0.0),
        ];
        pts.windows(2).map(|w| Prim::line(w[0], w[1])).collect()
    };
    let prims: Vec<Prim> = prims.into_iter().filter(|p| p.a.dist(p.b) > 0.5).collect();
    let added = prims.iter().map(Prim::len).sum::<f64>() - (s + 2.0 * m);
    Bump { prims, t0: x - m, t1: x + s + m, added }
}

/// Height of a U bump adding `rest`.
fn u_amp(rest: f64, s: f64, m: f64, arcs: bool) -> f64 {
    let m = m.min(s / 2.0);
    let k = if arcs { K_ARC } else { K_CHAMFER };
    let a = (rest + k * m) / 2.0;
    if a >= 2.0 * m { a } else { rest / (2.0 - k / 2.0) }
}

/// Sawtooth tooth: from `x` to `x + s`, apex at height `amp`.
fn tooth(x: f64, s: f64, amp: f64) -> Bump {
    let (a, m, b) = (P::new(x, 0.0), P::new(x + s / 2.0, amp), P::new(x + s, 0.0));
    Bump { prims: vec![Prim::line(a, m), Prim::line(m, b)], t0: x, t1: x + s, added: 2.0 * (s / 2.0).hypot(amp) - s }
}

fn tooth_amp(rest: f64, s: f64) -> f64 {
    (((rest + s) / 2.0).powi(2) - (s / 2.0).powi(2)).max(0.0).sqrt()
}

/// 45° triangular skew bump of height `h` starting at `x`.
fn tri(x: f64, h: f64) -> Bump {
    let (a, m, b) = (P::new(x, 0.0), P::new(x + h, h), P::new(x + 2.0 * h, 0.0));
    Bump { prims: vec![Prim::line(a, m), Prim::line(m, b)], t0: x, t1: x + 2.0 * h, added: K_TRI * h }
}

/// A local frame along a track: origin, direction and side.
#[derive(Clone, Copy)]
struct Frame {
    o: P,
    u: P,
    side: f64,
}

impl Frame {
    fn at(&self, t: f64, h: f64) -> P {
        let n = left(self.u);
        P::new(self.o.x + self.u.x * t + n.x * h * self.side, self.o.y + self.u.y * t + n.y * h * self.side)
    }

    fn map(&self, p: &Prim) -> Prim {
        let g = |q: P| self.at(q.x, q.y);
        Prim { a: g(p.a), b: g(p.b), arc: p.arc.map(|(c, ccw)| (g(c), if self.side < 0.0 { !ccw } else { ccw })) }
    }
}

/// The chain along a track from `t0` to `t1` with bumps (sorted, inside the range).
fn chain_with(f: &Frame, t0: f64, t1: f64, bumps: &[Bump], sides: &[f64]) -> Vec<Prim> {
    let mut out = Vec::new();
    let mut t = t0;
    for (b, &sd) in bumps.iter().zip(sides) {
        let fr = Frame { side: f.side * sd, ..*f };
        if b.t0 > t + 0.5 {
            out.push(Prim::line(f.at(t, 0.0), f.at(b.t0, 0.0)));
        }
        out.extend(b.prims.iter().map(|p| fr.map(p)));
        t = b.t1;
    }
    if t1 > t + 0.5 {
        out.push(Prim::line(f.at(t, 0.0), f.at(t1, 0.0)));
    }
    out
}

fn to_track(p: &Prim, like: &Track) -> Track {
    Track {
        id: ObjectId(0),
        start: p.a.to_point(),
        end: p.b.to_point(),
        mid: p.mid().map(P::to_point),
        locked: false,
        ..like.clone()
    }
}

// ---------------------------------------------------------------------------------------------
// The tuner

/// Parameters in nanometers for a net or pair.
#[derive(Clone, Copy, Debug)]
struct Params {
    amp: f64,
    s: f64,
    m: f64,
}

/// Board context for checks: the router's board and index of the working project.
struct Ctx {
    rb: RouterBoard,
    index: Index,
}

impl Ctx {
    fn new(q: &Project) -> Ctx {
        let items = crate::board::copper_items(q);
        let isl = crate::board::islands(&items);
        let rb = RouterBoard::build(q, &items, &isl);
        let mut index = Index::new(rb.bbox.expand(2_000_000.0), 500_000.0, super::reach(&rb));
        for (i, ob) in rb.obstacles.iter().enumerate() {
            index.insert(Item::Static(i as u32), ob.bbox);
        }
        Ctx { rb, index }
    }

    fn ck(&self) -> Checker<'_> {
        Checker { rb: &self.rb, index: &self.index }
    }

    fn layer(&self, name: &str) -> Option<usize> {
        self.rb.layer_names.iter().position(|n| n == name)
    }
}

/// Copper of one net on a layer, for spacing checks: chords with their half widths, and pad
/// and via shapes.
struct Own {
    segs: Vec<(P, P, f64)>,
    shapes: Vec<Shape>,
}

fn own_copper(q: &Project, ctx: &Ctx, net: &str, layer: &str, skip: &BTreeSet<ObjectId>) -> Own {
    let mut segs = Vec::new();
    for t in
        q.board().tracks.iter().filter(|t| t.net.as_deref() == Some(net) && t.layer == layer && !skip.contains(&t.id))
    {
        let (a, b) = (P::of(t.start), P::of(t.end));
        let pts = match t.mid {
            Some(m) => chords(a, P::of(m), b),
            None => vec![a, b],
        };
        segs.extend(pts.windows(2).map(|w| (w[0], w[1], t.width.0 as f64 / 2.0)));
    }
    let mut shapes = Vec::new();
    let id = ctx.rb.net_ids.get(net).copied();
    let bit = ctx.layer(layer).map_or(0, |l| 1u64 << l.min(63));
    for ob in &ctx.rb.obstacles {
        if ob.net == id && id.is_some() && ob.tracks & bit != 0 {
            let is_via = matches!(ob.shape, Shape::Capsule { a, b, .. } if a == b) && ob.kind == ObKind::Copper;
            if ob.kind == ObKind::Pad || is_via {
                shapes.push(ob.shape.clone());
            }
        }
    }
    Own { segs, shapes }
}

impl Own {
    /// Whether a chord of half width `hw` keeps `space` from this copper.
    fn clear(&self, a: P, b: P, hw: f64, space: f64) -> bool {
        self.segs.iter().all(|&(c, d, h)| seg_seg_dist(a, b, c, d) >= hw + h + space - TOL)
            && self.shapes.iter().all(|s| s.dist_seg(a, b) >= hw + space - TOL)
    }
}

/// A straight track of a net.
fn straight(q: &Project, net: &str) -> Vec<Track> {
    let mut v: Vec<Track> = q
        .board()
        .tracks
        .iter()
        .filter(|t| t.net.as_deref() == Some(net) && t.mid.is_none() && !t.locked)
        .cloned()
        .collect();
    v.sort_by(|a, b| crate::board::track_length(b).cmp(&crate::board::track_length(a)).then(a.id.cmp(&b.id)));
    v
}

/// A coupled stretch of a pair: straight tracks of both nets, parallel at about the gap.
struct Stretch {
    /// Track of the first net (the one the frame follows) and of the other.
    ta: Track,
    tb: Track,
    /// Frame along `ta` (origin at its start), `side` = +1.
    f: Frame,
    /// Signed offset of `tb`'s line from `ta`'s (left positive).
    delta: f64,
    /// Overlap of both along `ta`.
    lo: f64,
    hi: f64,
}

fn stretches(q: &Project, a: &str, b: &str, gap: f64, width: f64) -> Vec<Stretch> {
    let tol = (gap * lengths::GAP_TOLERANCE).max(lengths::GAP_TOLERANCE_MIN.0 as f64);
    let mut out = Vec::new();
    for ta in straight(q, a) {
        let (pa, pb) = (P::of(ta.start), P::of(ta.end));
        let la = pa.dist(pb);
        if la < 1.0 {
            continue;
        }
        let u = unit(pa, pb);
        for tb in straight(q, b).into_iter().filter(|t| t.layer == ta.layer) {
            let (qa, qb) = (P::of(tb.start), P::of(tb.end));
            if qa.dist(qb) < 1.0 || cross(u, unit(qa, qb)).abs() > 1e-3 {
                continue;
            }
            let delta = cross(u, P::new(qa.x - pa.x, qa.y - pa.y));
            if ((delta.abs() - width) - gap).abs() > tol {
                continue;
            }
            let t = |x: P| dot(P::new(x.x - pa.x, x.y - pa.y), u);
            let (lo, hi) = (t(qa).min(t(qb)).max(0.0), t(qa).max(t(qb)).min(la));
            if hi - lo < width {
                continue;
            }
            out.push(Stretch { f: Frame { o: pa, u, side: 1.0 }, ta: ta.clone(), tb, delta, lo, hi });
        }
    }
    out.sort_by(|x, y| (y.hi - y.lo).total_cmp(&(x.hi - x.lo)).then(x.ta.id.cmp(&y.ta.id)));
    out
}

struct Tuner<'a> {
    q: Project,
    opts: &'a TuneOptions,
}

impl Tuner<'_> {
    fn params(&self, w: f64, c: f64) -> Params {
        let s = self.opts.spacing.map_or((4.0 * w).max(w + c), |x| x.0 as f64).max(w + 1_000.0);
        let m = self.opts.corner.map_or(s / 4.0, |x| x.0 as f64).min(s / 2.0).max(0.0);
        let amp = self.opts.amplitude.map_or((2.5 * s).max(1_000_000.0), |x| x.0 as f64).max(2.0 * m);
        Params { amp, s, m }
    }

    /// Replaces track `old` by the chain (keeping its layer, width and net); returns the new
    /// track IDs.
    fn replace(&mut self, old: &Track, chain: &[Prim]) -> Vec<ObjectId> {
        let first = P::of(old.start);
        // The chain runs from the track's start to its end.
        let mut pts: Vec<Prim> = chain.to_vec();
        if pts.first().is_some_and(|p| p.a.dist(first) > 1.0) {
            pts.insert(0, Prim::line(first, pts[0].a));
        }
        let end = P::of(old.end);
        if pts.last().is_some_and(|p| p.b.dist(end) > 1.0) {
            let lb = pts[pts.len() - 1].b;
            pts.push(Prim::line(lb, end));
        }
        let b = self.q.board_mut();
        b.tracks.retain(|t| t.id != old.id);
        let mut ids = Vec::new();
        for p in &pts {
            let mut t = to_track(p, old);
            if t.start == t.end {
                continue;
            }
            t.id = self.q.alloc_id();
            ids.push(t.id);
            self.q.board_mut().tracks.push(t);
        }
        ids
    }

    /// Adds `need` (nm) to a net with meanders. Returns (added, bumps).
    fn tune_net(&mut self, net: &str, need: f64) -> (f64, usize) {
        let ctx = Ctx::new(&self.q);
        let Some(&id) = ctx.rb.net_ids.get(net) else { return (0.0, 0) };
        let pr = ctx.rb.profile(id).clone();
        let (hw, c) = (pr.hw, pr.c);
        let par = self.params(2.0 * hw, c);
        let mut added = 0.0;
        let mut count = 0;
        let mut changed: Vec<(Track, Vec<Prim>)> = Vec::new();
        let mut pref = 1.0;
        for t in straight(&self.q, net) {
            if need - added <= 0.5 {
                break;
            }
            let Some(layer) = ctx.layer(&t.layer) else { continue };
            let (a, b) = (P::of(t.start), P::of(t.end));
            let len = a.dist(b);
            let f = Frame { o: a, u: unit(a, b), side: 1.0 };
            let own = own_copper(&self.q, &ctx, net, &t.layer, &BTreeSet::from([t.id]));
            let ck = ctx.ck();
            let legal = |bp: &[Prim]| {
                bp.iter().all(|p| {
                    let extra = if p.arc.is_some() { SAGITTA } else { 0.0 };
                    p.chords().windows(2).all(|w| {
                        ck.seg_ext(layer, w[0], w[1], id, extra, None).is_none() && own.clear(w[0], w[1], hw, c + extra)
                    })
                })
            };
            let (s, m) = (par.s, par.m);
            let margin = s.max(4.0 * hw);
            let mut bumps: Vec<Bump> = Vec::new();
            let mut sides: Vec<f64> = Vec::new();
            let saw = self.opts.style == MeanderStyle::Sawtooth;
            let pitch = if saw { s } else { 2.0 * s };
            let mut x = margin + if saw { 0.0 } else { m };
            let mut placed_here = 0usize;
            while x + if saw { s } else { s + m } <= len - margin && need - added > 0.5 {
                let rest = need - added;
                let first = match self.opts.style {
                    MeanderStyle::Accordion => {
                        if placed_here.is_multiple_of(2) {
                            pref
                        } else {
                            -pref
                        }
                    }
                    _ => pref,
                };
                let mut done = false;
                for side in [first, -first] {
                    let exact = if saw { tooth_amp(rest, s) } else { u_amp(rest, s, m, self.opts.arcs) };
                    let mut amp = exact.min(par.amp);
                    while amp > 1.0 {
                        let bump = if saw { tooth(x, s, amp) } else { u_bump(x, s, amp, m, self.opts.arcs) };
                        let fr = Frame { side, ..f };
                        let g: Vec<Prim> = bump.prims.iter().map(|p| fr.map(p)).collect();
                        if legal(&g) {
                            added += bump.added;
                            bumps.push(bump);
                            sides.push(side);
                            done = true;
                            break;
                        }
                        amp /= 2.0;
                        if amp < 2.0 * hw.max(m) {
                            break;
                        }
                    }
                    if done {
                        if self.opts.style != MeanderStyle::Accordion {
                            pref = side;
                        }
                        break;
                    }
                }
                if done {
                    placed_here += 1;
                }
                x += pitch;
            }
            if bumps.is_empty() {
                continue;
            }
            count += bumps.len();
            let chain = chain_with(&f, 0.0, len, &bumps, &sides);
            changed.push((t, chain));
        }
        for (t, chain) in changed {
            self.replace(&t, &chain);
        }
        (added, count)
    }

    /// Adds `need` (nm) to both nets of a pair with coupled meanders. Returns (added, bumps).
    fn tune_pair(&mut self, d: &DiffPair, need: f64) -> (f64, usize) {
        let r = lengths::pair_rules(&self.q, d);
        let (w, gap, cl) = (r.width.0 as f64, r.gap.0 as f64, r.clearance.0 as f64);
        let gmin = gap.min(cl);
        let ctx = Ctx::new(&self.q);
        let (Some(&pn), Some(&nn)) = (ctx.rb.net_ids.get(&d.p), ctx.rb.net_ids.get(&d.n)) else { return (0.0, 0) };
        let base = self.params(w, cl);
        let mut added = 0.0;
        let mut count = 0;
        let mut pref = 1.0;
        let mut used: BTreeSet<ObjectId> = BTreeSet::new();
        let mut changes: Vec<(Track, Vec<Prim>, Track, Vec<Prim>)> = Vec::new();
        for st in stretches(&self.q, &d.p, &d.n, gap, w) {
            if need - added <= 0.5 {
                break;
            }
            if used.contains(&st.ta.id) || used.contains(&st.tb.id) {
                continue;
            }
            let Some(layer) = ctx.layer(&st.ta.layer) else { continue };
            let half = st.delta.abs() / 2.0;
            // Centerline frame: the first net is at −delta/2 from it (left positive).
            let cf = Frame { o: add(st.f.o, left(st.f.u), st.delta / 2.0), ..st.f };
            let da = -st.delta / 2.0; // offset of the first net from the centerline
            let s = 2.0 * half + base.s.max(w + COUPLED_RANGE * 1.1 * gap);
            let m = if self.opts.arcs { base.m.max(half + w) } else { base.m.max(1.2 * half) }.min(s / 2.0);
            let par = Params { amp: base.amp.max(2.0 * m + w), s, m };
            let skip = BTreeSet::from([st.ta.id, st.tb.id]);
            let own_a = own_copper(&self.q, &ctx, &d.p, &st.ta.layer, &skip);
            let own_b = own_copper(&self.q, &ctx, &d.n, &st.ta.layer, &skip);
            let ck = ctx.ck();
            let margin = s.max(4.0 * w) + w;
            let lead = half.max(w);
            let legal = |cl_chain: &[Prim]| -> Option<(Vec<Prim>, Vec<Prim>)> {
                let pa = offset_chain(cl_chain, da)?;
                let pb = offset_chain(cl_chain, -da)?;
                let ok = |chain: &[Prim], net: u32, other: u32, own: &Own, partner: &Own| {
                    chain.iter().all(|p| {
                        let extra = if p.arc.is_some() { SAGITTA } else { 0.0 };
                        p.chords().windows(2).all(|x| {
                            ck.seg_ext(layer, x[0], x[1], net, extra, Some(other)).is_none()
                                && own.clear(x[0], x[1], w / 2.0, cl + extra)
                                && partner.clear(x[0], x[1], w / 2.0, gmin + extra)
                        })
                    })
                };
                // Between the new tracks: the gap holds by construction; check anyway.
                let ca: Vec<(P, P)> =
                    pa.iter().flat_map(|p| p.chords().windows(2).map(|x| (x[0], x[1])).collect::<Vec<_>>()).collect();
                let cb: Vec<(P, P)> =
                    pb.iter().flat_map(|p| p.chords().windows(2).map(|x| (x[0], x[1])).collect::<Vec<_>>()).collect();
                let apart = ca.iter().all(|&(a0, a1)| {
                    cb.iter().all(|&(b0, b1)| seg_seg_dist(a0, a1, b0, b1) >= w + gmin - TOL - 2.0 * SAGITTA)
                });
                (apart && ok(&pa, pn, nn, &own_a, &own_b) && ok(&pb, nn, pn, &own_b, &own_a)).then_some((pa, pb))
            };
            let mut bumps: Vec<Bump> = Vec::new();
            let mut sides: Vec<f64> = Vec::new();
            let mut x = st.lo + margin + m;
            while x + s + m <= st.hi - margin && need - added > 0.5 {
                let rest = need - added;
                let first =
                    if self.opts.style == MeanderStyle::Accordion && bumps.len() % 2 == 1 { -pref } else { pref };
                let mut done = false;
                for side in [first, -first] {
                    let mut amp = u_amp(rest, s, m, self.opts.arcs).min(par.amp);
                    while amp > 1.0 {
                        let bump = u_bump(x, s, amp, m, self.opts.arcs);
                        let mut trial: Vec<Bump> =
                            bumps.iter().map(|b| Bump { prims: b.prims.clone(), ..*b }).collect();
                        let mut tsides = sides.clone();
                        trial.push(Bump { prims: bump.prims.clone(), ..bump });
                        tsides.push(side);
                        let chain = chain_with(&cf, trial[0].t0 - lead, bump.t1 + lead, &trial, &tsides);
                        if legal(&chain).is_some() {
                            added += bump.added;
                            bumps.push(bump);
                            sides.push(side);
                            done = true;
                            break;
                        }
                        amp /= 2.0;
                        if amp < 2.0 * m + w {
                            break;
                        }
                    }
                    if done {
                        if self.opts.style != MeanderStyle::Accordion {
                            pref = side;
                        }
                        break;
                    }
                }
                x += 2.0 * s;
            }
            if bumps.is_empty() {
                continue;
            }
            let (t0, t1) = (bumps[0].t0 - lead, bumps[bumps.len() - 1].t1 + lead);
            let chain = chain_with(&cf, t0, t1, &bumps, &sides);
            let Some((pa, pb)) = legal(&chain) else { continue };
            count += bumps.len();
            used.insert(st.ta.id);
            used.insert(st.tb.id);
            changes.push((st.ta.clone(), pa, st.tb.clone(), pb));
        }
        for (ta, pa, tb, pb) in changes {
            self.replace(&ta, &pa);
            // The other track may run the other way: the chain follows the first one.
            let (s, e) = (P::of(tb.start), P::of(tb.end));
            let u = unit(pa[0].a, pa[pa.len() - 1].b);
            let tb2 = if dot(unit(s, e), u) < 0.0 {
                Track { start: tb.end, end: tb.start, mid: None, ..tb.clone() }
            } else {
                tb.clone()
            };
            self.replace(&tb2, &pb);
        }
        (added, count)
    }

    /// Compensates a pair's skew with small bumps on the shorter net.
    fn compensate(&mut self, name: &str, d: &DiffPair) -> SkewTuned {
        let r = lengths::pair_rules(&self.q, d);
        let before = lengths::coupling(&self.q, d, r.gap).skew;
        let mut out =
            SkewTuned { name: name.to_string(), before, after: before, max_skew: d.max_skew, bumps: 0, reason: None };
        let (lp, ln) = (lengths::net_length(&self.q, &d.p).total, lengths::net_length(&self.q, &d.n).total);
        let need = (lp.0 - ln.0).abs() as f64;
        if need <= SKEW_IGNORE {
            return out;
        }
        let (short, long) = if lp < ln { (&d.p, &d.n) } else { (&d.n, &d.p) };
        let (w, gap, cl) = (r.width.0 as f64, r.gap.0 as f64, r.clearance.0 as f64);
        let gmin = gap.min(cl);
        let ctx = Ctx::new(&self.q);
        let (Some(&sn), Some(&ln_)) = (ctx.rb.net_ids.get(short.as_str()), ctx.rb.net_ids.get(long.as_str())) else {
            return out;
        };
        // Bends where the shorter net is the inner track.
        let tracks: Vec<Track> =
            self.q.board().tracks.iter().filter(|t| t.net.as_deref() == Some(short.as_str())).cloned().collect();
        let partner: Vec<(String, P, P)> = self
            .q
            .board()
            .tracks
            .iter()
            .filter(|t| t.net.as_deref() == Some(long.as_str()))
            .map(|t| (t.layer.clone(), P::of(t.start), P::of(t.end)))
            .collect();
        let mut inner: Vec<P> = Vec::new();
        for (i, a) in tracks.iter().enumerate() {
            for b in &tracks[i + 1..] {
                if a.layer != b.layer || a.mid.is_some() || b.mid.is_some() {
                    continue;
                }
                let (a0, a1, b0, b1) = (P::of(a.start), P::of(a.end), P::of(b.start), P::of(b.end));
                let shared = [(a0, a1, b0, b1), (a0, a1, b1, b0), (a1, a0, b0, b1), (a1, a0, b1, b0)]
                    .into_iter()
                    .find(|(v, _, w0, _)| v.dist(*w0) < 1.0);
                let Some((v, ea, _, eb)) = shared else { continue };
                let (e1, e2) = (unit(v, ea), unit(v, eb));
                if dot(e1, e2) < -0.999 {
                    continue; // straight
                }
                let bis = unit(P::new(0.0, 0.0), P::new(e1.x + e2.x, e1.y + e2.y));
                let near = partner
                    .iter()
                    .filter(|(l, ..)| *l == a.layer)
                    .map(|(_, p0, p1)| super::geo::project(v, *p0, *p1))
                    .min_by(|x, y| x.dist(v).total_cmp(&y.dist(v)));
                if let Some(nq) = near
                    && nq.dist(v) < 3.0 * (w + gap)
                    && dot(P::new(nq.x - v.x, nq.y - v.y), bis) < 0.0
                {
                    inner.push(v);
                }
            }
        }
        let mut cands = stretches(&self.q, short, long, gap, w);
        let key = |st: &Stretch| {
            let c = st.f.at((st.lo + st.hi) / 2.0, 0.0);
            inner.iter().map(|v| v.dist(c)).fold(f64::INFINITY, f64::min)
        };
        if !inner.is_empty() {
            cands.sort_by(|a, b| key(a).total_cmp(&key(b)).then(a.ta.id.cmp(&b.ta.id)));
        }
        let hmax = w.max(gap);
        let mut added = 0.0;
        let mut changes: Vec<(Track, Vec<Prim>)> = Vec::new();
        let mut used: BTreeSet<ObjectId> = BTreeSet::new();
        for st in &cands {
            if need - added <= 0.5 {
                break;
            }
            if used.contains(&st.ta.id) {
                continue;
            }
            let Some(layer) = ctx.layer(&st.ta.layer) else { continue };
            // Outward: away from the partner.
            let side = if st.delta > 0.0 { -1.0 } else { 1.0 };
            let f = Frame { side, ..st.f };
            let skip = BTreeSet::from([st.ta.id]);
            let own = own_copper(&self.q, &ctx, short, &st.ta.layer, &skip);
            let other = own_copper(&self.q, &ctx, long, &st.ta.layer, &BTreeSet::new());
            let ck = ctx.ck();
            let legal = |g: &[Prim]| {
                g.iter().all(|p| {
                    ck.seg_ext(layer, p.a, p.b, sn, 0.0, Some(ln_)).is_none()
                        && own.clear(p.a, p.b, w / 2.0, cl)
                        && other.clear(p.a, p.b, w / 2.0, gmin)
                })
            };
            let margin = w + gap;
            let mut bumps: Vec<Bump> = Vec::new();
            let mut x = st.lo + margin;
            // Bumps from the start of the stretch on; where one does not fit (at any height
            // down to a quarter width), slide along.
            while need - added > 0.5 {
                let rest = need - added;
                let h0 = (rest / K_TRI).min(hmax);
                if x + 2.0 * h0.min(w / 4.0) > st.hi - margin {
                    break;
                }
                let mut h = h0;
                let mut ok = false;
                while h > 1.0 && (h >= w / 4.0 || h == h0) {
                    if x + 2.0 * h <= st.hi - margin {
                        let b = tri(x, h);
                        let g: Vec<Prim> = b.prims.iter().map(|p| f.map(p)).collect();
                        if legal(&g) {
                            added += b.added;
                            x = b.t1 + hmax.max(w);
                            bumps.push(b);
                            ok = true;
                            break;
                        }
                    }
                    h /= 2.0;
                }
                if !ok {
                    x += w.max(gap) / 2.0;
                }
            }
            if bumps.is_empty() {
                continue;
            }
            used.insert(st.ta.id);
            out.bumps += bumps.len();
            let len = P::of(st.ta.start).dist(P::of(st.ta.end));
            let ones = vec![1.0; bumps.len()];
            changes.push((st.ta.clone(), chain_with(&f, 0.0, len, &bumps, &ones)));
        }
        for (t, chain) in changes {
            self.replace(&t, &chain);
        }
        out.after = lengths::coupling(&self.q, d, r.gap).skew;
        if d.max_skew.is_some_and(|m| out.after > m) {
            out.reason = Some(format!(
                "no room for skew bumps beside {short}: {} left (move nearby copper or route the pair again)",
                out.after
            ));
        }
        out
    }
}

/// A change made by the tuner, for the final DRC check: the nets touched and their tracks
/// before.
struct Unit {
    /// Group index and member index, or `None` for a pair's skew.
    member: Option<(usize, usize)>,
    pair: Option<usize>,
    nets: Vec<String>,
    before: Vec<Track>,
    ids: BTreeSet<u64>,
}

/// Tunes the length groups (`group`: one of them, else all) and the skew of differential
/// pairs (those in the groups, or every pair when no group is named), with meanders. The
/// project is not modified; the result says what to add and remove and what was achieved.
pub fn tune(p: &Project, group: Option<&str>, opts: &TuneOptions) -> Result<TuneResult, RouteError> {
    let c = p.circuit();
    let groups: Vec<String> = match group {
        Some(g) if c.length_groups.contains_key(g) => vec![g.to_string()],
        Some(g) => return Err(RouteError::UnknownGroup(g.to_string())),
        None => c.length_groups.keys().cloned().collect(),
    };
    let pair_names: Vec<String> = if group.is_some() {
        let mut v: Vec<String> = groups
            .iter()
            .flat_map(|g| c.length_groups[g].members.iter().filter(|m| c.diffpairs.contains_key(*m)).cloned())
            .collect();
        v.sort();
        v.dedup();
        v
    } else {
        c.diffpairs.keys().cloned().collect()
    };
    let mut t = Tuner { q: p.clone(), opts };
    let mut units: Vec<Unit> = Vec::new();
    let snapshot = |q: &Project, nets: &[String]| -> Vec<Track> {
        q.board().tracks.iter().filter(|t| t.net.as_ref().is_some_and(|n| nets.contains(n))).cloned().collect()
    };
    let ids_of = |q: &Project| -> BTreeSet<u64> { q.board().tracks.iter().map(|t| t.id.0).collect() };
    let mut skews: Vec<SkewTuned> = Vec::new();
    if opts.skew {
        for (k, name) in pair_names.iter().enumerate() {
            let d = c.diffpairs[name].clone();
            let nets = vec![d.p.clone(), d.n.clone()];
            let before = snapshot(&t.q, &nets);
            let old = ids_of(&t.q);
            let st = t.compensate(name, &d);
            let ids = ids_of(&t.q).difference(&old).copied().collect();
            units.push(Unit { member: None, pair: Some(k), nets, before, ids });
            skews.push(st);
        }
    }
    let mut out_groups: Vec<GroupTuned> = Vec::new();
    for (gi, gname) in groups.iter().enumerate() {
        let g = &c.length_groups[gname];
        let unrouted = lengths::unrouted_nets(&crate::board::copper_items(&t.q));
        let st = lengths::group_status(&t.q, gname, g, &unrouted);
        let aim = st.target;
        let mut members = Vec::new();
        for (mi, m) in st.members.iter().enumerate() {
            let mut mt = MemberTuned {
                name: m.name.clone(),
                kind: m.kind.clone(),
                before: m.length,
                after: m.length,
                error: m.error,
                meanders: 0,
                reason: None,
            };
            if m.length.is_none() {
                mt.reason = Some("not fully routed".into());
            }
            if let (Some(len), Some(err), Some(aim)) = (m.length, m.error, aim)
                && err < Nm::ZERO
            {
                let need = (aim.0 - len.0) as f64;
                let nets = match c.diffpairs.get(&m.name) {
                    Some(d) => vec![d.p.clone(), d.n.clone()],
                    None => vec![m.name.clone()],
                };
                let before = snapshot(&t.q, &nets);
                let old = ids_of(&t.q);
                let (_, bumps) = match c.diffpairs.get(&m.name) {
                    Some(d) => t.tune_pair(&d.clone(), need),
                    None => t.tune_net(&m.name, need),
                };
                mt.meanders = bumps;
                let ids = ids_of(&t.q).difference(&old).copied().collect();
                units.push(Unit { member: Some((gi, mi)), pair: None, nets, before, ids });
            }
            members.push(mt);
        }
        out_groups.push(GroupTuned { name: gname.clone(), target: st.target, min: st.min, max: st.max, members });
    }
    // DRC: a unit whose new copper the DRC flags is put back (with later units on its nets).
    let mut reverted: BTreeMap<usize, String> = BTreeMap::new();
    for _ in 0..4 {
        let diags = crate::drc::check(&t.q);
        let mut bad: BTreeMap<usize, String> = BTreeMap::new();
        for dg in diags.iter().filter(|d| d.severity == crate::diag::Severity::Error && d.code != "drc.unrouted") {
            for s in &dg.subjects {
                if let crate::refs::ObjectRef::Item { kind, index } = s
                    && kind == "track"
                    && let Some(ui) = units.iter().position(|u| u.ids.contains(index))
                {
                    bad.entry(ui).or_insert_with(|| format!("{}: {}", dg.code, dg.message));
                }
            }
        }
        if bad.is_empty() {
            break;
        }
        let first = *bad.keys().next().expect("bad unit");
        let nets: BTreeSet<String> = bad.keys().flat_map(|&u| units[u].nets.clone()).collect();
        // Restore every net touched by a bad unit to its state before the earliest unit
        // (from the first bad one on) that touched it.
        let mut restored: BTreeSet<String> = BTreeSet::new();
        for (ui, unit) in units.iter_mut().enumerate().skip(first) {
            let touches: Vec<String> = unit.nets.iter().filter(|n| nets.contains(*n)).cloned().collect();
            if touches.is_empty() {
                continue;
            }
            for n in &touches {
                if restored.insert(n.clone()) {
                    let b = t.q.board_mut();
                    b.tracks.retain(|x| x.net.as_ref() != Some(n));
                    let back: Vec<Track> = unit.before.iter().filter(|x| x.net.as_ref() == Some(n)).cloned().collect();
                    b.tracks.extend(back);
                }
            }
            let why =
                bad.get(&ui).cloned().unwrap_or_else(|| "reverted with an earlier change on the same nets".into());
            reverted.insert(ui, why);
            unit.ids.clear();
        }
    }
    // Report.
    let unrouted = lengths::unrouted_nets(&crate::board::copper_items(&t.q));
    for (gi, gt) in out_groups.iter_mut().enumerate() {
        let gname = gt.name.clone();
        let st = lengths::group_status(&t.q, &gname, &c.length_groups[&gname], &unrouted);
        gt.target = st.target;
        gt.min = st.min;
        gt.max = st.max;
        for (mi, (mt, ms)) in gt.members.iter_mut().zip(&st.members).enumerate() {
            mt.after = ms.length;
            mt.error = ms.error;
            if let Some(ui) = units.iter().position(|u| u.member == Some((gi, mi)))
                && let Some(why) = reverted.get(&ui)
            {
                mt.meanders = 0;
                mt.reason = Some(format!("meanders removed after the DRC check ({why})"));
            } else if ms.error.is_some_and(|e| e < Nm::ZERO) && mt.reason.is_none() {
                mt.reason = Some("no room left for meanders along its straight tracks (raise the amplitude, lower the spacing, or move nearby copper)".into());
            } else if ms.error.is_some_and(|e| e > Nm::ZERO) && mt.reason.is_none() {
                mt.reason =
                    Some("longer than allowed: tuning only adds length (route it shorter or raise the target)".into());
            }
        }
    }
    for (k, sk) in skews.iter_mut().enumerate() {
        let d = &c.diffpairs[&sk.name];
        sk.after = lengths::coupling(&t.q, d, lengths::pair_rules(&t.q, d).gap).skew;
        if let Some(ui) = units.iter().position(|u| u.pair == Some(k))
            && let Some(why) = reverted.get(&ui)
        {
            sk.bumps = 0;
            sk.reason = Some(format!("skew bumps removed after the DRC check ({why})"));
        }
    }
    // The difference to the project, renumbered from its allocator.
    let old: BTreeSet<ObjectId> = p.board().tracks.iter().map(|x| x.id).collect();
    let new: BTreeSet<ObjectId> = t.q.board().tracks.iter().map(|x| x.id).collect();
    let mut alloc = p.clone();
    let mut tracks: Vec<Track> = t.q.board().tracks.iter().filter(|x| !old.contains(&x.id)).cloned().collect();
    for x in &mut tracks {
        x.id = alloc.alloc_id();
    }
    Ok(TuneResult { tracks, removed_tracks: old.difference(&new).copied().collect(), groups: out_groups, pairs: skews })
}

/// Skew compensation of the given pairs on `q` (in place), as `route.diffpair` does after
/// routing. Returns the reports.
pub(crate) fn compensate_pairs(q: &mut Project, names: &[String], opts: &TuneOptions) -> Vec<SkewTuned> {
    let c = q.circuit().clone();
    let mut t = Tuner { q: q.clone(), opts };
    let out = names.iter().filter_map(|n| c.diffpairs.get(n).map(|d| t.compensate(n, d))).collect();
    *q = t.q;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_len(c: &[Prim]) -> f64 {
        c.iter().map(Prim::len).sum()
    }

    #[test]
    fn bump_lengths_match_closed_forms() {
        for arcs in [false, true] {
            for rest in [10_000.0, 150_000.0, 900_000.0, 3_000_000.0] {
                let (s, m) = (400_000.0, 100_000.0);
                let a = u_amp(rest, s, m, arcs);
                let b = u_bump(0.0, s, a, m, arcs);
                assert!((b.added - rest).abs() < 1e-3, "arcs {arcs} rest {rest}: {} {}", b.added, a);
                assert!((chain_len(&b.prims) - (b.t1 - b.t0) - rest).abs() < 1e-3);
            }
        }
        let s = 300_000.0;
        let a = tooth_amp(500_000.0, s);
        assert!((tooth(0.0, s, a).added - 500_000.0).abs() < 1e-3);
        assert!((tri(0.0, 100_000.0).added - K_TRI * 100_000.0).abs() < 1e-6);
    }

    #[test]
    fn offsets_of_a_bump_average_to_the_centerline() {
        for arcs in [false, true] {
            let b = u_bump(1_000_000.0, 800_000.0, 600_000.0, 250_000.0, arcs);
            let f = Frame { o: P::new(0.0, 0.0), u: P::new(1.0, 0.0), side: 1.0 };
            let chain = chain_with(&f, 0.0, 3_000_000.0, &[b], &[1.0]);
            let l = offset_chain(&chain, 100_000.0).unwrap();
            let r = offset_chain(&chain, -100_000.0).unwrap();
            let (cl, ll, rl) = (chain_len(&chain), chain_len(&l), chain_len(&r));
            assert!(((ll + rl) / 2.0 - cl).abs() < 1.0, "arcs {arcs}: {ll} {rl} {cl}");
            // Balanced bends: both offsets have the same length.
            assert!((ll - rl).abs() < 1.0, "arcs {arcs}: {ll} {rl}");
            // Offsets stay connected.
            for w in l.windows(2) {
                assert!(w[0].b.dist(w[1].a) < 1e-6);
            }
        }
    }
}
