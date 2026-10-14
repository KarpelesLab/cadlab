//! Large-board performance (roadmap "Performance"): equivalence of the indexed connectivity,
//! ratsnest and zone fill with the straightforward versions on generated boards, and
//! `#[ignore]`d timings on the 500-component synthetic board and, when `CADLAB_CORPUS_DIR` is
//! set, the corpus boards (`cargo test --release --test perf -- --ignored --nocapture`, or
//! `cargo run --release --example bigboard [corpus]`). The timing test compares the synthetic
//! board with the committed baseline `tests/golden/bench/perf.json` and fails only on large
//! regressions (`tests/common/bench.rs`, DECISIONS D43); the `bench` CI job runs it.

use std::collections::BTreeMap;

use cadlab::board::{self, CopperItem, ItemRef, RatLine};
use cadlab::geom::poly;
use cadlab::{Nm, Point};

#[path = "common/bench.rs"]
mod bench;
#[path = "common/bigboard.rs"]
mod bigboard;
#[path = "common/fill_reference.rs"]
mod fill_reference;

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

/// The fills as comparable values.
fn fill_values(fills: &[board::zones::ZoneFill]) -> Vec<(String, String, Nm, &poly::PolygonSet, Option<String>)> {
    fills.iter().map(|f| (f.name.clone(), f.layer.clone(), f.clearance, &f.fill, f.error.clone())).collect()
}

/// Adds what exercises every per-zone culling path to a generated board: small zones of
/// several nets and priorities spread over a layer (like teardrops), a pour keep-out, and
/// non-plated holes.
fn add_culling_cases(p: &mut cadlab::model::Project) {
    use cadlab::model::board::{Hole, Keepout, PadConnection, Zone};
    let ring = board::contour_ring(&p.board().outline.contours[0], board::zones::FILL_TOL);
    let bb = poly::Rect::of_points(&ring).unwrap();
    let (x0, y0, x1, y1) = (bb.min.x, bb.min.y, bb.max.x, bb.max.y);
    let at = |fx: i64, fy: i64| Point::new(Nm(x0 + (x1 - x0) * fx / 100), Nm(y0 + (y1 - y0) * fy / 100));
    let nets = ["GND", "3V3", "1V8"];
    for k in 0..24i64 {
        let (fx, fy) = (5 + (k % 6) * 15, 10 + (k / 6) * 20);
        let (a, b) = (at(fx, fy), at(fx + 9, fy + 12));
        let id = p.alloc_id();
        p.board_mut().zones.push(Zone {
            id,
            name: format!("small{k}"),
            net: Some(nets[k as usize % 3].into()),
            layers: vec![if k % 2 == 0 { "In1.Cu" } else { "In2.Cu" }.into()],
            outline: vec![a, Point::new(b.x, a.y), b, Point::new(a.x, b.y)],
            priority: (k % 3) as u32,
            clearance: None,
            min_width: None,
            pads: if k % 4 == 0 { PadConnection::None } else { PadConnection::Thermal },
            thermal_gap: None,
            thermal_spoke: None,
            thermal_angle: None,
            thermal_spokes: None,
        });
    }
    let id = p.alloc_id();
    p.board_mut().keepouts.push(Keepout {
        id,
        name: "K".into(),
        layers: vec![],
        outline: vec![at(40, 40), at(48, 40), at(48, 47), at(40, 47)],
        no_tracks: false,
        no_vias: false,
        no_pours: true,
        no_footprints: false,
    });
    for (k, (fx, fy)) in [(3, 3), (97, 3), (50, 55), (75, 30)].into_iter().enumerate() {
        let id = p.alloc_id();
        p.board_mut().holes.push(Hole {
            id,
            name: format!("MH{k}"),
            at: at(fx, fy),
            drill: Nm::from_um(3_200),
            pad: None,
            net: None,
        });
    }
    // D40 inputs the fill depends on: local clearances and zone connections of footprints and
    // pads, a custom rule, a tht_thermal zone and copper text.
    use cadlab::model::board::{BoardGraphic, CustomRule, GraphicKind, ItemKind, RuleScope};
    let names: Vec<String> = p.library().footprints.keys().cloned().collect();
    for (i, n) in names.iter().enumerate() {
        let f = p.library_mut().footprints.get_mut(n).unwrap();
        match i % 3 {
            0 => f.overrides.clearance = Some(Nm::from_um(450)),
            1 => f.overrides.zone_connection = Some(PadConnection::Solid),
            _ => {
                if let Some(pad) = f.pads.first_mut() {
                    pad.overrides.zone_connection = Some(PadConnection::None);
                    pad.overrides.clearance = Some(Nm::from_um(120));
                }
            }
        }
    }
    p.board_mut().custom_rules.push(CustomRule {
        clearance: Some(Nm::from_um(350)),
        ..CustomRule::new("vias", RuleScope { kinds: vec![ItemKind::Via], ..Default::default() })
    });
    if let Some(z) = p.board_mut().zones.iter_mut().find(|z| z.name == "small1") {
        z.pads = PadConnection::ThtThermal;
    }
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "In1.Cu".into(),
        kind: GraphicKind::Text {
            text: "D40".into(),
            at: at(30, 50),
            size: Nm::from_mm(2),
            rotation: Default::default(),
        },
    });
}

#[test]
fn zone_fills_match_reference() {
    for seed in 1..=2 {
        let (_dir, _r, mut s) = bigboard::build(bigboard::Spec::small(seed));
        let p = s.project.as_mut().unwrap();
        if seed == 2 {
            add_culling_cases(p);
        }
        let base = board::base_copper_items(p);
        let fills = board::zones::fill_zones_uncached(p, &base);
        let want = fill_reference::fill_zones(p, &base);
        let n = fills.iter().filter(|f| !f.fill.is_empty()).count();
        eprintln!("seed {seed}: {n} of {} zone layers filled", fills.len());
        assert!(n >= 3, "seed {seed}: {n} pours filled");
        assert_eq!(fill_values(&fills), fill_values(&want), "zone fills, seed {seed}");
    }
}

/// Zone fills of the large board and of the corpus boards (`CADLAB_CORPUS_DIR`) against the
/// reference. Slow in debug builds.
#[test]
#[ignore = "slow; run in release with --ignored"]
fn large_zone_fills_match_reference() {
    let (_dir, _r, mut s) = bigboard::build(bigboard::Spec::default());
    let p = s.project.as_mut().unwrap();
    add_culling_cases(p);
    let mut boards = vec![("synthetic".to_string(), p.clone())];
    boards.extend(bigboard::corpus_boards(&[]).into_iter().map(|(n, p, _)| (n, p)));
    for (name, p) in boards {
        let base = board::base_copper_items(&p);
        let fills = board::zones::fill_zones_uncached(&p, &base);
        assert_eq!(fill_values(&fills), fill_values(&fill_reference::fill_zones(&p, &base)), "{name}");
    }
}

#[test]
fn timing_rule() {
    let base: BTreeMap<String, f64> =
        [("a", 100.0), ("b", 1000.0), ("c", 2000.0), ("d", 10.0)].into_iter().map(|(n, t)| (n.into(), t)).collect();
    let run = |times: [f64; 4]| {
        let m: Vec<(String, f64)> = ["a", "b", "c", "d"].into_iter().zip(times).map(|(n, t)| (n.into(), t)).collect();
        let (v, _) = bench::check_timings_with(&m, &base, true);
        v.iter().filter(|v| v.failure.is_some()).map(|v| v.name.clone()).collect::<Vec<_>>()
    };
    // A machine twice as slow: nothing fails.
    assert!(run([200.0, 2000.0, 4000.0, 20.0]).is_empty());
    // One step 4× slower than the others: fails.
    assert_eq!(run([100.0, 4000.0, 2000.0, 10.0]), ["b"]);
    // Tiny steps never fail, however slow (under the noise floor).
    assert!(run([100.0, 1000.0, 2000.0, 200.0]).is_empty());
    // Everything 10× slower: the absolute limit.
    assert_eq!(run([1000.0, 10_000.0, 20_000.0, 100.0]), ["a", "b", "c"]);
    // A step without a baseline is reported, not checked.
    let (v, _) = bench::check_timings_with(&[("new".into(), 1e6)], &base, true);
    assert!(v[0].failure.is_none() && v[0].baseline_ms.is_none());
}

/// Committed timings of the synthetic board (`tests/golden/bench/perf.json`).
#[derive(serde::Serialize, serde::Deserialize)]
struct PerfBaseline {
    /// How the baseline was measured.
    measured: String,
    /// Best time of each step (ms).
    steps: BTreeMap<String, f64>,
}

/// Timings of every heavy step on the synthetic board, compared with `tests/golden/bench/perf.json`
/// under the generous rule of `tests/common/bench.rs` (D43): run in release by the `bench` CI job
/// (`RAYON_NUM_THREADS=1`), which uploads `$CARGO_TARGET_TMPDIR/bench/perf.{json,md}`.
/// `CADLAB_BLESS=1` rewrites the baseline. Corpus boards (`CADLAB_CORPUS_DIR`) are timed and
/// reported, not compared.
#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn big_board_timings() {
    let (summary, rows) = bigboard::timings(bigboard::Spec::default(), 3);
    println!("{}", bigboard::report(&summary, &rows));
    let get = |name: &str| rows.iter().find(|t| t.name == name).unwrap();
    assert!(get("islands").note.ends_with("islands"));
    assert!(rows.iter().all(|t| t.time.as_secs() < 60), "something is pathologically slow");
    let measured: Vec<(String, f64)> = rows.iter().map(|t| (t.name.to_string(), t.time.as_secs_f64() * 1e3)).collect();
    let baseline: Option<PerfBaseline> = bench::read_baseline("perf.json");
    let bless = bench::blessing();
    assert!(bless || baseline.is_some(), "tests/golden/bench/perf.json missing: run with CADLAB_BLESS=1");
    let (verdicts, median) =
        bench::check_timings(&measured, &baseline.as_ref().map(|b| b.steps.clone()).unwrap_or_default());
    let mut markdown =
        format!("## Large-board timings\n\nboard: {summary}\n\n{}", bench::timing_table(&verdicts, median));
    let mut corpus = Vec::new();
    // Corpus boards, when fetched (`CADLAB_CORPUS_DIR`, scripts/fetch-corpus.sh).
    for (name, rows) in bigboard::corpus_timings(&[], 1) {
        println!("{}", bigboard::report(&format!("{name}: {}", rows[0].note), &rows));
        assert!(rows.iter().all(|t| t.time.as_secs() < 60), "{name}: something is pathologically slow");
        let steps: BTreeMap<&str, f64> = rows.iter().map(|t| (t.name, t.time.as_secs_f64() * 1e3)).collect();
        markdown.push_str(&format!("\n### {name}\n\n{}\n", bigboard::report(&rows[0].note, &rows)));
        corpus.push(serde_json::json!({"name": name, "steps": steps}));
    }
    let failures: Vec<String> =
        verdicts.iter().filter_map(|v| v.failure.as_ref().map(|f| format!("{}: {f}", v.name))).collect();
    let report = serde_json::json!({
        "profile": bench::profile(),
        "rayon_threads": std::env::var("RAYON_NUM_THREADS").ok(),
        "board": summary,
        "steps": verdicts,
        "median_ratio": median,
        "corpus": corpus,
        "failures": failures,
    });
    bench::write_report("perf", &report, &markdown);
    println!("{markdown}");
    if bless {
        let measured_how = format!(
            "{} build, RAYON_NUM_THREADS={}, best of 3",
            bench::profile(),
            std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "unset".into())
        );
        let steps = measured.into_iter().map(|(n, ms)| (n, (ms * 10.0).round() / 10.0)).collect();
        bench::write_baseline("perf.json", &PerfBaseline { measured: measured_how, steps });
        return;
    }
    assert!(failures.is_empty(), "performance regressions:\n{}", failures.join("\n"));
}
