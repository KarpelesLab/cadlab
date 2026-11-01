//! Differential pairs in spread placement: orientation of flow-through parts, and whether a
//! placement makes a pair cross (D71).
//!
//! A pair leaves a package side outward, its positive net on one hand of the direction of
//! travel; which hand is a property of the part (turning it turns the pads and the way out
//! together; only the other board side mirrors it). Two such sides join without crossing only
//! when their hands differ, wherever the parts sit: the check the router's "polarity" failure
//! makes by search. So on two ends with fixed hands no placement helps, and on a *flow-through*
//! part (a USB ESD array: each net on two pads, one side of the package facing each way, with
//! opposite hands) exactly one side suits each neighbour. That side must face it: turned the
//! other way the pair has to run around the package (where parts are close, it cannot), yet the
//! ratsnest barely notices (the links differ by a package's width). So the rotation is chosen
//! from the connections once every anchor is placed: fewest crossings, then the shortest links
//! from the sides that suit, then the pull of its other nets. Pads in a row with the part's center
//! on the row (a pin header) have no outward: a pair can leave them either way, so they suit any
//! hand. Improvement leaves such a part where it is, and never moves another pair end into more
//! crossings.

use super::{Engine, Kind, Point, Pose, dist};
use crate::model::Project;
use crate::model::part::Category;
use crate::units::Nm;
use std::collections::BTreeMap;

/// One place where a pair can leave a part.
#[derive(Clone, Copy)]
struct Exit {
    p: Point,
    n: Point,
    /// Which hand of the direction of travel the positive net is on, leaving (`None`: the pads
    /// have no outward, so either).
    hand: Option<bool>,
    /// The way out, a unit vector (`None`: no outward).
    out: Option<(f64, f64)>,
}

/// An end of a differential pair: a part with pads of both nets.
struct End {
    /// Index among the parts being placed (`None`: it stays where it is).
    part: Option<usize>,
    refdes: String,
    /// Its pads of both nets paired up.
    sides: Vec<Exit>,
    /// A package with a side per direction (not a connector's interleaved contacts).
    flow: bool,
}

/// The exits among a part's pads, its courtyard centered at `center`: each positive with its
/// nearest negative, kept when that positive is the negative's nearest too.
fn sides_of(center: Point, pp: &[Point], pn: &[Point]) -> Vec<Exit> {
    let nearest =
        |a: Point, of: &[Point]| of.iter().copied().min_by(|x, y| dist(a, *x).total_cmp(&dist(a, *y)).then(x.cmp(y)));
    pp.iter()
        .filter_map(|&a| {
            let b = nearest(a, pn)?;
            (nearest(b, pp) == Some(a)).then(|| {
                let (hand, out) = way_out(center, a, b);
                Exit { p: a, n: b, hand, out }
            })
        })
        .collect()
}

/// The way out of a pad pair: the hand of the positive pad, the direction perpendicular to the
/// line of the pads from the part's center towards their middle. None when the middle is on
/// the line through the center (within 0.1 mm): the pads have no outward.
fn way_out(center: Point, p: Point, n: Point) -> (Option<bool>, Option<(f64, f64)>) {
    let (dx, dy) = ((p.x.0 - n.x.0) as f64, (p.y.0 - n.y.0) as f64);
    let len = dx.hypot(dy);
    if len <= 0.0 {
        return (None, None);
    }
    let (mx, my) = ((p.x.0 + n.x.0) as f64 / 2.0 - center.x.0 as f64, (p.y.0 + n.y.0) as f64 / 2.0 - center.y.0 as f64);
    // Signed distance of the middle from the line through the center along the pads.
    let off = (mx * dy - my * dx) / len;
    if off.abs() < 100_000.0 {
        return (None, None);
    }
    let s = off.signum();
    (Some(off > 0.0), Some((s * dy / len, -s * dx / len)))
}

/// The ends of the pair with nets `np`, `nn`, as placed now.
fn ends(e: &Engine, np: usize, nn: usize) -> Vec<End> {
    let mut out = Vec::new();
    let is_flow = |sides: usize, ic: bool| ic && sides >= 2;
    for (i, part) in e.parts.iter().enumerate() {
        let Some(pose) = e.poses[i].filter(|_| part.kind != Kind::Passive) else { continue };
        let pick = |net: usize| -> Vec<Point> {
            part.pads.iter().enumerate().filter(|(_, x)| x.1 == Some(net)).map(|(k, _)| part.pad_at(&pose, k)).collect()
        };
        let (pp, pn) = (pick(np), pick(nn));
        if pp.is_empty() || pn.is_empty() {
            continue;
        }
        let sides = sides_of(part.rect(&pose).center(), &pp, &pn);
        let flow = is_flow(sides.len(), part.kind == Kind::Ic);
        out.push(End { part: Some(i), refdes: part.refdes.clone(), sides, flow });
    }
    let mut fixed: BTreeMap<&str, (Vec<Point>, Vec<Point>)> = BTreeMap::new();
    for ((r, _), (at, net)) in &e.fixed {
        if *net == Some(np) {
            fixed.entry(r).or_default().0.push(*at);
        } else if *net == Some(nn) {
            fixed.entry(r).or_default().1.push(*at);
        }
    }
    for (r, (pp, pn)) in fixed {
        if pp.is_empty() || pn.is_empty() {
            continue;
        }
        let part = e.p.circuit().components.get(r).and_then(|c| e.p.library().parts.get(&c.part));
        let ic = part.is_some_and(|x| x.category != Category::Connector && x.symbol.pins.len() > 2);
        let center = super::courtyard_box(e.p, r).map_or_else(|| centroid(&pp, &pn), |b| b.center());
        let sides = sides_of(center, &pp, &pn);
        let flow = is_flow(sides.len(), ic);
        out.push(End { part: None, refdes: r.to_string(), sides, flow });
    }
    out
}

/// Mean of the pads.
fn centroid(pp: &[Point], pn: &[Point]) -> Point {
    let k = (pp.len() + pn.len()).max(1) as i64;
    let (x, y) = pp.iter().chain(pn).fold((0, 0), |a, q| (a.0 + q.x.0, a.1 + q.y.0));
    Point::new(Nm(x / k), Nm(y / k))
}

/// How a link from one exit to another costs: crossed or not, how far the way out turns from
/// the way to the other end (see [`Exit::turn`]), and its length.
#[derive(Clone, Copy, Default, PartialEq)]
struct LinkCost {
    crossed: usize,
    turn: f64,
    len: f64,
}

impl LinkCost {
    fn better(self, o: LinkCost) -> bool {
        (self.crossed, self.turn, self.len) < (o.crossed, o.turn, o.len)
    }

    fn plus(self, o: LinkCost) -> LinkCost {
        LinkCost { crossed: self.crossed + o.crossed, turn: self.turn + o.turn, len: self.len + o.len }
    }
}

impl Exit {
    /// How much a pair from here to `to` has to turn back: 0 when `to` lies straight ahead, 2 when
    /// it lies right behind, divided by the distance in mm (at least 1): a turn around the
    /// package needs room for the pair, which a far neighbour leaves and a near one does not.
    fn turn(&self, to: Point) -> f64 {
        let Some((ox, oy)) = self.out else { return 0.0 };
        let (mx, my) = ((self.p.x.0 + self.n.x.0) as f64 / 2.0, (self.p.y.0 + self.n.y.0) as f64 / 2.0);
        let (dx, dy) = (to.x.0 as f64 - mx, to.y.0 as f64 - my);
        let d = dx.hypot(dy);
        if d <= 0.0 {
            return 0.0;
        }
        (1.0 - (dx * ox + dy * oy) / d) / (d / 1e6).max(1.0)
    }

    fn middle(&self) -> Point {
        Point::new(Nm((self.p.x.0 + self.n.x.0) / 2), Nm((self.p.y.0 + self.n.y.0) / 2))
    }
}

/// The best link from an exit of a part to another end. Two exits join uncrossed when their hands
/// differ: leaving one with the positive net on the left is arriving at the other with it on
/// the left, which is leaving that one with it on the right.
fn link(side: Exit, other: &End) -> LinkCost {
    let mut best: Option<LinkCost> = None;
    for x in &other.sides {
        let crossed = matches!((side.hand, x.hand), (Some(a), Some(b)) if a == b);
        let c = LinkCost {
            crossed: usize::from(crossed),
            turn: side.turn(x.middle()) + x.turn(side.middle()),
            len: dist(side.p, x.p) + dist(side.n, x.n),
        };
        if best.is_none_or(|b| c.better(b)) {
            best = Some(c);
        }
    }
    best.unwrap_or_default()
}

/// How a flow-through end `x` links to the pair's `others`. Two others each get a side,
/// whichever way round is better; any other number of them takes its best side.
fn flow_cost(x: &End, others: &[&End]) -> LinkCost {
    if let ([y, z], [a, b, ..]) = (others, &x.sides[..]) {
        let one = link(*a, y).plus(link(*b, z));
        let two = link(*b, y).plus(link(*a, z));
        return if two.better(one) { two } else { one };
    }
    let mut total = LinkCost::default();
    for o in others {
        let mut best: Option<LinkCost> = None;
        for &s in &x.sides {
            let c = link(s, o);
            if best.is_none_or(|b| c.better(b)) {
                best = Some(c);
            }
        }
        total = total.plus(best.unwrap_or_default());
    }
    total
}

/// A pair's ends as placed now: every flow-through end against the others, or, without one, the
/// two ends against each other. With the designators where it must cross.
fn pair_cost(all: &[End]) -> (LinkCost, Vec<String>) {
    let mut total = LinkCost::default();
    let mut who = Vec::new();
    let flows: Vec<&End> = all.iter().filter(|x| x.flow).collect();
    for x in &flows {
        let others: Vec<&End> = all.iter().filter(|o| o.refdes != x.refdes).collect();
        let c = flow_cost(x, &others);
        total = total.plus(c);
        if c.crossed > 0 {
            who.push(x.refdes.clone());
        }
    }
    if let ([a, b], true) = (all, flows.is_empty()) {
        let mut best: Option<LinkCost> = None;
        for &s in &a.sides {
            let c = link(s, b);
            if best.is_none_or(|w| c.better(w)) {
                best = Some(c);
            }
        }
        if let Some(c) = best {
            total = total.plus(c);
            if c.crossed > 0 {
                who.extend([a.refdes.clone(), b.refdes.clone()]);
            }
        }
    }
    (total, who)
}

impl Engine<'_> {
    /// Crossings of all differential pairs as placed now.
    pub(super) fn pair_crossings(&self) -> usize {
        self.pair_nets.iter().map(|&(np, nn)| pair_cost(&ends(self, np, nn)).0.crossed).sum()
    }
}

/// Turns each flow-through part being placed so that the side suiting each neighbour faces it
/// (fewest crossings, then room to break out, then least turning, then shortest links and the
/// pull of its other nets), about its center, shifted up to 4 mm when that pose is taken or
/// leaves no room. Called with the anchors placed.
pub(super) fn orient_flow_parts(e: &mut Engine) {
    for k in 0..e.pair_nets.len() {
        let (np, nn) = e.pair_nets[k];
        let flows: Vec<usize> = ends(e, np, nn).iter().filter(|x| x.flow).filter_map(|x| x.part).collect();
        for i in flows {
            let Some(cur) = e.poses[i] else { continue };
            let center = e.parts[i].rect(&cur).center();
            // (crossings, no room, turning in hundredths, length and pull).
            let key = |e: &mut Engine, q: Pose| {
                let clear = e.zones_clear(i, &q);
                e.poses[i] = Some(q);
                let c = pair_cost(&ends(e, np, nn)).0;
                let pull = c.len + e.attraction(i, &q);
                e.poses[i] = Some(cur);
                (c.crossed, !clear, (c.turn * 100.0).round() as i64, pull)
            };
            let better = |a: &(usize, bool, i64, f64), b: &(usize, bool, i64, f64)| {
                (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)).then(a.3.total_cmp(&b.3)).is_lt()
            };
            let mut best = (key(e, cur), cur);
            for rot in 0..4u8 {
                let probe = Pose { rot, ..cur };
                let c = e.parts[i].rect(&probe).center();
                let at =
                    probe.at + Point::new(Nm(super::snap(center.x.0 - c.x.0)), Nm(super::snap(center.y.0 - c.y.0)));
                // The nearest seat of this rotation with room to break out.
                'seat: for d in [0, 250_000i64, 500_000, 1_000_000, 2_000_000, 3_000_000, 4_000_000] {
                    for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                        if d == 0 && (dx, dy) != (0, 0) {
                            continue;
                        }
                        let q = Pose { at: at + Point::new(Nm(dx * d), Nm(dy * d)), ..probe };
                        if !e.valid(i, &q) || !e.zones_clear(i, &q) {
                            continue;
                        }
                        let v = key(e, q);
                        if better(&v, &best.0) {
                            best = (v, q);
                        }
                        break 'seat;
                    }
                }
            }
            let q = best.1;
            e.poses[i] = Some(q);
            e.flow_part[i] = true;
            if let Some(room) = e.rooms[i] {
                e.rooms[i] = Some(room.shift(q.at - cur.at));
            }
        }
    }
}

/// The differential pairs that must cross as placed, by name, with the parts concerned: the
/// hands of their ends (see the module's notes) leave no way to keep both nets in order.
pub fn pair_crossings(p: &Project) -> Vec<(String, Vec<String>)> {
    let Ok(e) = Engine::new(p, &[], 0, false, &BTreeMap::new(), &BTreeMap::new()) else {
        return Vec::new();
    };
    let index: BTreeMap<&str, usize> = e.net_names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    let mut out = Vec::new();
    for (name, d) in &p.circuit().diffpairs {
        let (Some(&np), Some(&nn)) = (index.get(d.p.as_str()), index.get(d.n.as_str())) else { continue };
        let (c, who) = pair_cost(&ends(&e, np, nn));
        if c.crossed > 0 {
            out.push((name.clone(), who));
        }
    }
    out
}
