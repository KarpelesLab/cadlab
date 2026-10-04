//! Equivalence of the indexed connectivity and ratsnest with the straightforward versions they
//! replaced (kept here as references), on randomized copper.

use super::*;
use polyclip::{Boolean, FillRule, Op};

/// The previous `islands`: sweep over bounding boxes, every overlapping pair tested.
fn islands_reference(items: &[CopperItem]) -> Vec<usize> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        r
    }
    let touches = |a: &CopperItem, b: &CopperItem| {
        a.layers.iter().any(|l| b.layers.contains(l)) && polyclip::intersects(&a.shape, &b.shape)
    };
    let boxes: Vec<Option<polyclip::Rect>> = items.iter().map(|it| polyclip::Geometry::bbox(&it.shape)).collect();
    let mut order: Vec<(polyclip::Rect, usize)> =
        boxes.iter().enumerate().filter_map(|(i, b)| b.map(|b| (b, i))).collect();
    order.sort_by_key(|(b, i)| (b.min.x, *i));
    for (k, (bi, i)) in order.iter().enumerate() {
        for (bj, j) in &order[k + 1..] {
            if bj.min.x > bi.max.x {
                break;
            }
            if bi.intersects(bj) && touches(&items[*i], &items[*j]) {
                let (a, b) = (find(&mut parent, *i), find(&mut parent, *j));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
            }
        }
    }
    (0..n).map(|i| find(&mut parent, i)).collect()
}

/// The previous ratsnest: Prim's algorithm scanning every (tree island, other island) pair of
/// anchors at each step.
fn ratsnest_reference(items: &[CopperItem], isl: &[usize]) -> Vec<RatLine> {
    let mut by_net: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(n) = &it.net
            && !matches!(it.item, ItemRef::Track(_) | ItemRef::Zone(..))
        {
            by_net.entry(n.as_str()).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for (net, idx) in by_net {
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in idx {
            groups.entry(isl[i]).or_default().push(i);
        }
        let groups: Vec<Vec<usize>> = groups.into_values().collect();
        if groups.len() < 2 {
            continue;
        }
        let mut in_tree = vec![false; groups.len()];
        in_tree[0] = true;
        for _ in 1..groups.len() {
            let mut best: Option<(i64, usize, usize, usize)> = None;
            for (_, g) in groups.iter().enumerate().filter(|(gi, _)| in_tree[*gi]) {
                for (hj, h) in groups.iter().enumerate().filter(|(hj, _)| !in_tree[*hj]) {
                    for &a in g {
                        for &b in h {
                            let d = dist(items[a].anchor, items[b].anchor);
                            if best.is_none_or(|x| d < x.0) {
                                best = Some((d, a, b, hj));
                            }
                        }
                    }
                }
            }
            let Some((d, a, b, hj)) = best else { break };
            in_tree[hj] = true;
            out.push(RatLine {
                net: net.to_string(),
                from: items[a].item.to_string(),
                from_at: items[a].anchor,
                to: items[b].item.to_string(),
                to_at: items[b].anchor,
                length: Nm(d),
            });
        }
    }
    out
}

struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: i64) -> i64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n.max(1) as u64) as i64
    }
}

const LAYERS: [&str; 3] = ["F.Cu", "In1.Cu", "B.Cu"];

fn pick_net(r: &mut Lcg) -> Option<String> {
    match r.below(8) {
        0 => None,
        k => Some(format!("N{}", k % 5)),
    }
}

/// Random copper on a coarse grid (so anchors tie in distance and shapes touch exactly), with a
/// few large pours (hundreds of vertices, holes, islands in holes) to exercise the prepared path.
fn random_items(seed: u64, n: usize) -> Vec<CopperItem> {
    let mut r = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let g = 250_000i64;
    let at = |r: &mut Lcg| Point::new(Nm(r.below(80) * g), Nm(r.below(60) * g));
    let mut out = Vec::new();
    for k in 0..n {
        let net = pick_net(&mut r);
        match r.below(10) {
            0..=3 => {
                let c = at(&mut r);
                let (hw, hh) = (g / 2 + r.below(3) * g / 2, g / 2 + r.below(3) * g / 2);
                let shape = PadShape::RoundRect { w: Nm(2 * hw), h: Nm(2 * hh), r: Nm(r.below(2) * g / 4) };
                let local = pad_shape_local(&shape).unwrap();
                let moved: Vec<polyclip::Point> =
                    local.outer.0.iter().map(|q| polyclip::Point::new(q.x + c.x.0, q.y + c.y.0)).collect();
                let layers = if r.below(4) == 0 {
                    LAYERS.iter().map(|l| l.to_string()).collect()
                } else {
                    vec![LAYERS[r.below(3) as usize].to_string()]
                };
                out.push(CopperItem {
                    item: ItemRef::Pad(format!("U{}", k % 7), format!("{k}")),
                    net,
                    layers,
                    shape: vec![Polygon::new(moved, vec![])],
                    anchor: c,
                });
            }
            4..=6 => {
                let (a, b) = (at(&mut r), at(&mut r));
                let b = if r.below(2) == 0 { Point::new(b.x, a.y) } else { b };
                let t = Track {
                    id: ObjectId(k as u64),
                    layer: LAYERS[r.below(3) as usize].into(),
                    width: Nm(g / 2 + r.below(2) * g / 2),
                    net,
                    start: a,
                    end: b,
                    mid: None,
                    locked: false,
                };
                out.push(CopperItem {
                    item: ItemRef::Track(t.id),
                    net: t.net.clone(),
                    layers: vec![t.layer.clone()],
                    shape: track_shape(&t),
                    anchor: t.start,
                });
            }
            7 | 8 => {
                let v = Via {
                    id: ObjectId(k as u64),
                    at: at(&mut r),
                    drill: Nm(g),
                    diameter: Nm(2 * g),
                    net,
                    from: "F.Cu".into(),
                    to: "B.Cu".into(),
                    locked: false,
                };
                out.push(CopperItem {
                    item: ItemRef::Via(v.id),
                    net: v.net.clone(),
                    layers: LAYERS.iter().map(|l| l.to_string()).collect(),
                    shape: vec![via_shape(&v)],
                    anchor: v.at,
                });
            }
            _ => {
                // A pour: a rectangle minus round holes, plus a small island inside one hole.
                let c = at(&mut r);
                let (w, h) = ((8 + r.below(30)) * g, (8 + r.below(30)) * g);
                let rect = polyclip::Ring::from([
                    (c.x.0, c.y.0),
                    (c.x.0 + w, c.y.0),
                    (c.x.0 + w, c.y.0 + h),
                    (c.x.0, c.y.0 + h),
                ]);
                let tol = ArcTol::new(200, Side::Outside);
                let mut holes = Vec::new();
                for _ in 0..6 {
                    let hc = polyclip::Point::new(c.x.0 + r.below(w / g) * g, c.y.0 + r.below(h / g) * g);
                    holes.push(Polygon::new(
                        Circle { center: hc, radius: g + r.below(3) * g }.to_ring(tol).unwrap(),
                        vec![],
                    ));
                }
                let mut set = Boolean::new()
                    .subject(&rect, FillRule::NonZero)
                    .clip(&holes, FillRule::NonZero)
                    .op(Op::Difference)
                    .execute()
                    .unwrap();
                if let Some(h) = holes.first() {
                    let hc = h.outer.0[0];
                    let island = Circle { center: polyclip::Point::new(hc.x - g, hc.y), radius: g / 3 };
                    set.push(Polygon::new(island.to_ring(tol).unwrap(), vec![]));
                }
                let layer = LAYERS[r.below(3) as usize].to_string();
                for (i, poly) in set.into_iter().enumerate() {
                    let anchor = Point::new(Nm(poly.outer.0[0].x), Nm(poly.outer.0[0].y));
                    out.push(CopperItem {
                        item: ItemRef::Zone(ObjectId(k as u64), layer.clone(), i),
                        net: net.clone(),
                        layers: vec![layer.clone()],
                        shape: vec![poly],
                        anchor,
                    });
                }
            }
        }
    }
    out
}

#[test]
fn islands_and_ratsnest_match_references() {
    let (mut prepared_used, mut lines, mut joined) = (0, 0, 0);
    for seed in 0..24u64 {
        let items = random_items(seed, 60 + 15 * seed as usize);
        prepared_used +=
            items.iter().filter(|it| prepared::segment_count(&it.shape) >= prepared::PREPARE_MIN_SEGMENTS).count();
        let isl = islands(&items);
        assert_eq!(isl, islands_reference(&items), "islands, seed {seed}");
        joined += isl.iter().enumerate().filter(|(i, k)| *i != **k).count();
        lines += ratsnest_from(&items, &isl).len();
        assert_eq!(ratsnest_from(&items, &isl), ratsnest_reference(&items, &isl), "ratsnest, seed {seed}");
        assert_eq!(ratsnest_items(&items), ratsnest_reference(&items, &isl), "ratsnest_items, seed {seed}");
    }
    assert!(prepared_used > 10, "the tests should exercise prepared shapes ({prepared_used})");
    assert!(lines > 200 && joined > 500, "non-trivial connectivity ({lines} lines, {joined} joined items)");
}

#[test]
fn islands_with_many_layer_names() {
    // More than 128 distinct layer names: layer sets are compared by name.
    let mut items = random_items(7, 120);
    for (k, it) in items.iter_mut().enumerate() {
        it.layers.push(format!("User{}", k * 2));
    }
    let mut extra = random_items(8, 60);
    for (k, it) in extra.iter_mut().enumerate() {
        it.layers = vec![format!("User{}", k * 2)];
    }
    items.extend(extra);
    assert_eq!(islands(&items), islands_reference(&items));
}

#[test]
fn pad_nets_follow_the_first_net() {
    use crate::model::circuit::{Net, PinRef};
    let mut p = Project::new("t");
    let c = p.circuit_mut();
    // A pin listed in two nets (an inconsistent circuit): the first net in name order wins,
    // as with `Circuit::net_of`.
    for (name, id) in [("B", 1), ("A", 2)] {
        let pins = [PinRef::new("U1", "1"), PinRef::new("U2", "3")].into_iter().collect();
        c.nets.insert(name.into(), Net { pins, ..Net::new(ObjectId(id)) });
    }
    let index = pin_nets(&p, None);
    assert_eq!(index["U1"]["1"], "A");
    assert_eq!(index["U2"]["3"], p.circuit().net_of(&PinRef::new("U2", "3")).unwrap());
    assert_eq!(pin_nets(&p, Some("U2")).into_keys().collect::<Vec<_>>(), ["U2"]);
}
