//! Large-board performance (roadmap "Performance"): equivalence of the indexed connectivity and
//! ratsnest with the straightforward versions on generated boards, and `#[ignore]`d timings on
//! the 500-component synthetic board (`cargo test --release --test perf -- --ignored --nocapture`,
//! or `cargo run --release --example bigboard`).

use std::collections::BTreeMap;

use cadlab::board::{self, CopperItem, ItemRef, RatLine};
use cadlab::geom::poly;
use cadlab::{Nm, Point};

#[path = "common/bigboard.rs"]
mod bigboard;

/// Connectivity as it was computed before the indexed version: every pair of items with
/// overlapping bounding boxes and a shared layer tested with `intersects`.
fn islands_reference(items: &[CopperItem]) -> Vec<usize> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &[usize], mut i: usize) -> usize {
        while p[i] != i {
            i = p[i];
        }
        i
    }
    let boxes: Vec<Option<poly::Rect>> = items.iter().map(|it| poly::Geometry::bbox(&it.shape)).collect();
    for i in 0..n {
        for j in i + 1..n {
            let (Some(a), Some(b)) = (boxes[i], boxes[j]) else { continue };
            if a.intersects(&b)
                && items[i].layers.iter().any(|l| items[j].layers.contains(l))
                && poly::intersects(&items[i].shape, &items[j].shape)
            {
                let (x, y) = (find(&parent, i), find(&parent, j));
                if x != y {
                    parent[x.max(y)] = x.min(y);
                }
            }
        }
    }
    (0..n).map(|i| find(&parent, i)).collect()
}

fn dist(a: Point, b: Point) -> i64 {
    let (dx, dy) = ((a.x.0 - b.x.0) as f64, (a.y.0 - b.y.0) as f64);
    (dx * dx + dy * dy).sqrt().round() as i64
}

/// The ratsnest as it was computed before: Prim's algorithm scanning all anchor pairs between
/// the tree and every other island at each step.
fn ratsnest_reference(items: &[CopperItem], isl: &[usize]) -> Vec<RatLine> {
    let mut by_net: BTreeMap<&str, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(n) = &it.net
            && !matches!(it.item, ItemRef::Track(_) | ItemRef::Zone(..))
        {
            by_net.entry(n.as_str()).or_default().entry(isl[i]).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for (net, groups) in by_net {
        let groups: Vec<Vec<usize>> = groups.into_values().collect();
        let mut in_tree = vec![false; groups.len()];
        in_tree[0] = true;
        for _ in 1..groups.len() {
            let mut best: Option<(i64, usize, usize, usize)> = None;
            for g in (0..groups.len()).filter(|g| in_tree[*g]) {
                for h in (0..groups.len()).filter(|h| !in_tree[*h]) {
                    for &a in &groups[g] {
                        for &b in &groups[h] {
                            let d = dist(items[a].anchor, items[b].anchor);
                            if best.is_none_or(|x| d < x.0) {
                                best = Some((d, a, b, h));
                            }
                        }
                    }
                }
            }
            let (d, a, b, h) = best.unwrap();
            in_tree[h] = true;
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

#[test]
fn generated_boards_match_references() {
    for seed in 1..=2 {
        let (_dir, _r, s) = bigboard::build(bigboard::Spec::small(seed));
        let p = s.project.as_ref().unwrap();
        let items = board::copper_items(p);
        assert!(items.iter().any(|it| matches!(it.item, ItemRef::Zone(..))), "pours filled");
        let isl = board::islands(&items);
        assert_eq!(isl, islands_reference(&items), "islands, seed {seed}");
        let rats = board::ratsnest(p);
        assert!(!rats.is_empty(), "the generated boards are partly unrouted");
        assert_eq!(rats, ratsnest_reference(&items, &isl), "ratsnest, seed {seed}");
        assert_eq!(rats, board::ratsnest_from(&items, &isl));
    }
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn big_board_timings() {
    let (summary, rows) = bigboard::timings(bigboard::Spec::default(), 1);
    println!("{}", bigboard::report(&summary, &rows));
    let get = |name: &str| rows.iter().find(|t| t.name == name).unwrap();
    assert!(get("islands").note.ends_with("islands"));
    assert!(rows.iter().all(|t| t.time.as_secs() < 60), "something is pathologically slow");
}
