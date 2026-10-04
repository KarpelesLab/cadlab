//! Router benchmark suite (docs/ROUTER.md, "Benchmarks"): deterministic generated boards of
//! increasing difficulty and a runner measuring completion, vias, wirelength, track segments,
//! sharp corners, runtime and DRC errors, for cadlab's router and (optionally) freerouting run
//! as an external process on the exported DSN (`CADLAB_ORACLE_FREEROUTING=/path/freerouting.jar`).
//!
//! Used by `examples/route_bench.rs` (`cargo run --release --example route_bench [filter]`) and
//! `tests/route_bench.rs` (`#[ignore]`d).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::Project;
use serde_json::{Value, json};

#[path = "bigboard.rs"]
pub mod bigboard;
#[path = "mod.rs"]
pub mod common;

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn new_project() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("bench")}));
    (dir, r, s)
}

fn mm(x: f64, y: f64) -> Value {
    json!([format!("{x}mm"), format!("{y}mm")])
}

fn numbered_pins(n: usize) -> Value {
    Value::Array(
        (1..=n).map(|i| json!({"number": i.to_string(), "name": format!("P{i}"), "kind": "passive"})).collect(),
    )
}

/// Small deterministic LCG.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize
    }
}

/// Fine-pitch rules: 0.1 mm tracks and clearance, 0.45 / 0.2 mm vias.
fn fine_rules(r: &Registry, s: &mut Session) {
    exec(
        r,
        s,
        "board.rules",
        json!({"clearance": "0.1mm", "track_width": "0.1mm", "min_track_width": "0.1mm", "via_drill": "0.2mm",
               "via_diameter": "0.45mm", "min_drill": "0.2mm", "min_annular_ring": "0.1mm", "hole_to_hole": "0.25mm",
               "copper_to_edge": "0.3mm"}),
    );
}

/// A benchmark board.
pub struct Case {
    /// Short name.
    pub name: &'static str,
    /// What it is.
    pub what: &'static str,
    /// Builds the board (unrouted) in a new project.
    pub build: fn() -> (tempfile::TempDir, Registry, Session),
    /// Also compared with freerouting (left out where it takes far too long).
    pub freerouting: bool,
}

/// The suite, easiest first.
pub fn cases() -> Vec<Case> {
    vec![
        Case { name: "attiny-2l", what: "ATtiny85 board, auto-placed, 2 layers", build: attiny, freerouting: true },
        Case {
            name: "soic24-2l",
            what: "24x SOIC-16 grid with permuted buses, 2 layers",
            build: soic_grid,
            freerouting: true,
        },
        Case {
            name: "stm32-2l",
            what: "STM32F103 LQFP-48 board, auto-placed, 2 layers",
            build: || stm32(2),
            freerouting: true,
        },
        Case {
            name: "stm32-4l",
            what: "STM32F103 LQFP-48 board, auto-placed, 4 layers",
            build: || stm32(4),
            freerouting: true,
        },
        Case {
            name: "qfp-qfn-4l",
            what: "LQFP-100, LQFP-64, QFN-48 + passives both sides, pours, 4 layers",
            build: qfp_qfn,
            freerouting: true,
        },
        Case {
            name: "fine-4l",
            what: "QFN-48 0.5 mm, QFN-40 0.4 mm, 3x TSSOP; 0.1 mm rules, 4 layers",
            build: fine_pitch,
            freerouting: true,
        },
        Case {
            name: "bga144-4l",
            what: "BGA-144 0.8 mm + 4 headers; 0.1 mm rules, 4 layers",
            build: bga,
            freerouting: true,
        },
        Case {
            name: "big-4l",
            what: "the 160 x 100 mm, ~500-part synthetic board (bigboard.rs), routing removed",
            build: big,
            freerouting: false,
        },
    ]
}

fn attiny() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    common::boards::build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "40mm", "height": "30mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1.5mm"}));
    (d, r, s)
}

fn stm32(layers: u8) -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    common::boards::build_stm32_board(&r, &mut s);
    exec(&r, &mut s, "board.setup", json!({"layers": layers}));
    exec(&r, &mut s, "board.outline", json!({"width": "60mm", "height": "45mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1mm"}));
    (d, r, s)
}

/// `cols` × `rows` SOIC-16s, each joined to its right neighbor by a permuted 6-bit bus and to the
/// one below by one net, plus GND and VCC on every IC (as in `tests/route.rs`).
fn soic_grid() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    let (cols, rows) = (6, 4);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "IO16", "category": "ic", "package": "SOIC-16", "pins": numbered_pins(16)}),
    );
    let (px, py) = (14.0, 12.0);
    exec(
        &r,
        &mut s,
        "board.outline",
        json!({"width": format!("{}mm", cols as f64 * px + 6.0), "height": format!("{}mm", rows as f64 * py + 6.0)}),
    );
    let name = |c: usize, rr: usize| format!("U{}", rr * cols + c + 1);
    for rr in 0..rows {
        for c in 0..cols {
            exec(&r, &mut s, "circuit.add", json!({"part": "IO16", "refdes": name(c, rr)}));
            let at = mm(3.0 + px * (c as f64 + 0.5), 3.0 + py * (rr as f64 + 0.5));
            exec(&r, &mut s, "place.set", json!({"refdes": name(c, rr), "at": at}));
        }
    }
    let mut rng = Lcg(1u64.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407));
    let mut n = 0;
    for rr in 0..rows {
        for c in 0..cols {
            if c + 1 < cols {
                let mut perm: Vec<usize> = (0..6).collect();
                for i in (1..6).rev() {
                    perm.swap(i, rng.next() % (i + 1));
                }
                for (k, p) in perm.iter().enumerate() {
                    n += 1;
                    let pins = [format!("{}.{}", name(c, rr), 10 + k), format!("{}.{}", name(c + 1, rr), 2 + p)];
                    exec(&r, &mut s, "net.connect", json!({"net": format!("N{n}"), "pins": pins}));
                }
            }
            if rr + 1 < rows {
                n += 1;
                let pins = [format!("{}.1", name(c, rr)), format!("{}.9", name(c, rr + 1))];
                exec(&r, &mut s, "net.connect", json!({"net": format!("N{n}"), "pins": pins}));
            }
        }
    }
    let all = |pin: usize| -> Vec<String> {
        (0..rows)
            .flat_map(|rr| (0..cols).map(move |c| (c, rr)))
            .map(|(c, rr)| format!("{}.{pin}", name(c, rr)))
            .collect()
    };
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": all(8)}));
    exec(&r, &mut s, "net.connect", json!({"net": "VCC", "pins": all(16)}));
    (d, r, s)
}

/// Three cells of the synthetic large board (`bigboard.rs`: LQFP-100, LQFP-64, QFN-48 with
/// 0402/0603 passives on both sides, two headers, GND/3V3/1V8/5V pours on the inner layers),
/// with its pre-made routing removed.
fn qfp_qfn() -> (tempfile::TempDir, Registry, Session) {
    let spec =
        bigboard::Spec { cols: 3, rows: 1, headers_per_edge: 1, passives_per_cell: 14, unrouted_permille: 0, seed: 3 };
    let (d, r, mut s) = bigboard::build(spec);
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    (d, r, s)
}

/// The full synthetic large board (`bigboard.rs`) with its pre-made routing removed.
fn big() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = bigboard::build(bigboard::Spec { unrouted_permille: 0, ..Default::default() });
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    (d, r, s)
}

/// Fine-pitch parts on a small four-layer board: signals of a QFN-48 (0.5 mm) go to a QFN-40
/// (0.4 mm) and three TSSOPs (0.65 mm) in a scrambled order, plus GND and VCC on all of them.
fn fine_pitch() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    let parts = [
        ("FP_QFN48", "QFN-48 7x7mm P0.5mm EP5.1mm", 49),
        ("FP_QFN40", "QFN-40 5x5mm P0.4mm EP3.5mm", 41),
        ("FP_TSSOP28", "TSSOP-28", 28),
        ("FP_TSSOP20", "TSSOP-20", 20),
    ];
    for (id, pkg, n) in parts {
        exec(&r, &mut s, "part.create", json!({"id": id, "category": "ic", "package": pkg, "pins": numbered_pins(n)}));
    }
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    fine_rules(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "34mm", "height": "28mm", "corner_radius": "1mm"}));
    let comps = [
        ("U1", "FP_QFN48", 17.0, 14.0, 0, 49),
        ("U2", "FP_QFN40", 6.5, 7.0, 0, 41),
        ("U3", "FP_TSSOP28", 28.0, 8.0, 90, 28),
        ("U4", "FP_TSSOP28", 28.0, 20.5, 90, 28),
        ("U5", "FP_TSSOP20", 6.5, 20.5, 0, 20),
    ];
    for (rd, part, x, y, rot, _) in comps {
        exec(&r, &mut s, "circuit.add", json!({"part": part, "refdes": rd}));
        exec(&r, &mut s, "place.set", json!({"refdes": rd, "at": mm(x, y), "rotation": rot}));
    }
    exec(&r, &mut s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "count": 5}));
    for (i, (x, y)) in [(12.0, 10.0), (3.0, 12.0), (24.5, 3.5), (24.5, 25.5), (11.0, 24.0)].iter().enumerate() {
        exec(&r, &mut s, "place.set", json!({"refdes": format!("C{}", i + 1), "at": mm(*x, *y), "rotation": 90}));
    }
    let mut gnd = Vec::new();
    let mut vcc = Vec::new();
    let mut free: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (rd, _, _, _, _, n) in comps {
        for p in 1..=n {
            if p % 8 == 0 || (n > 40 && p == n) {
                gnd.push(format!("{rd}.{p}"));
            } else if p % 8 == 4 {
                vcc.push(format!("{rd}.{p}"));
            } else {
                free.entry(rd).or_default().push(p);
            }
        }
    }
    for i in 1..=5 {
        gnd.push(format!("C{i}.2"));
        vcc.push(format!("C{i}.1"));
    }
    let mut rng = Lcg(7);
    // U1's pins by side (counter-clockwise from pin 1 on the left): left → U2/U5, bottom → U2/U3,
    // right → U3/U4, top → U4/U5; the targets take the pins in a shuffled order.
    let targets = [["U2", "U5"], ["U2", "U3"], ["U3", "U4"], ["U4", "U5"]];
    let u1 = free.remove("U1").unwrap();
    let mut n = 0;
    for p in u1 {
        let side = ((p - 1) / 12).min(3);
        let t = targets[side][rng.next() % 2];
        let pool = free.get_mut(t).unwrap();
        if pool.is_empty() {
            continue;
        }
        let k = rng.next() % pool.len().min(6);
        let q = pool.remove(k);
        n += 1;
        exec(
            &r,
            &mut s,
            "net.connect",
            json!({"net": format!("S{n}"), "pins": [format!("U1.{p}"), format!("{t}.{q}")]}),
        );
    }
    // Some links between the small parts.
    for (a, b) in [("U2", "U5"), ("U3", "U4"), ("U2", "U3"), ("U5", "U4")] {
        for _ in 0..4 {
            let (Some(p), Some(q)) = (free.get_mut(a).unwrap().pop(), free.get_mut(b).unwrap().pop()) else { break };
            n += 1;
            exec(
                &r,
                &mut s,
                "net.connect",
                json!({"net": format!("S{n}"), "pins": [format!("{a}.{p}"), format!("{b}.{q}")]}),
            );
        }
    }
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": gnd}));
    exec(&r, &mut s, "net.connect", json!({"net": "VCC", "pins": vcc}));
    (d, r, s)
}

/// A BGA-144 (12 × 12, 0.8 mm pitch) in the middle of a four-layer board with a 2 × 15 1.27 mm
/// header on each side: every signal ball goes to the header on its side (in order along the
/// side, outer rows first), plus GND and VCC balls with two header pins each.
fn bga() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = new_project();
    let rows = "ABCDEFGHJKLM";
    let balls: Vec<String> = rows.chars().flat_map(|rn| (1..=12).map(move |c| format!("{rn}{c}"))).collect();
    let pins: Vec<Value> = balls.iter().map(|b| json!({"number": b, "name": b, "kind": "bidirectional"})).collect();
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "BGA144", "category": "ic", "package": "BGA-144 12x12 P0.8mm 10x10mm", "pins": pins}),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "HDR30", "category": "connector", "package": "PinHeader 2x15 P1.27mm", "pins": numbered_pins(30)}),
    );
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    fine_rules(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "50mm", "height": "50mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "BGA144", "refdes": "U1"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": mm(25.0, 25.0)}));
    // Headers: left, bottom, right, top.
    for (i, (x, y, rot)) in [(9.0, 25.0, 0), (25.0, 9.0, 90), (41.0, 25.0, 0), (25.0, 41.0, 90)].iter().enumerate() {
        let rd = format!("J{}", i + 1);
        exec(&r, &mut s, "circuit.add", json!({"part": "HDR30", "refdes": rd}));
        exec(&r, &mut s, "place.set", json!({"refdes": rd, "at": mm(*x, *y), "rotation": rot}));
    }
    exec(&r, &mut s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "count": 4}));
    for (i, (x, y)) in [(18.0, 18.0), (32.0, 18.0), (32.0, 32.0), (18.0, 32.0)].iter().enumerate() {
        exec(&r, &mut s, "place.set", json!({"refdes": format!("C{}", i + 1), "at": mm(*x, *y), "rotation": 45}));
    }
    let p = s.project.as_ref().unwrap();
    let pads = cadlab::board::placed_pads(p);
    let center = (25_000_000i64, 25_000_000i64);
    let mut gnd: Vec<String> = Vec::new();
    let mut vcc: Vec<String> = Vec::new();
    // (side, ring, position along the side, ball).
    let mut sig: Vec<(usize, i64, i64, String)> = Vec::new();
    for pp in pads.iter().filter(|pp| pp.refdes == "U1") {
        let (ri, ci) = (rows.find(&pp.number[..1]).unwrap() as i64, pp.number[1..].parse::<i64>().unwrap() - 1);
        let (dx, dy) = (pp.center.x.0 - center.0, pp.center.y.0 - center.1);
        let ring = ri.min(ci).min(11 - ri).min(11 - ci);
        if (ri + 2 * ci) % 9 == 4 && ring > 0 {
            gnd.push(format!("U1.{}", pp.number));
            continue;
        }
        if (2 * ri + ci) % 11 == 3 && ring > 0 {
            vcc.push(format!("U1.{}", pp.number));
            continue;
        }
        let side = if dx.abs() >= dy.abs() {
            if dx < 0 { 0 } else { 2 }
        } else if dy < 0 {
            1
        } else {
            3
        };
        let along = if side % 2 == 0 { dy } else { dx };
        sig.push((side, ring, along, pp.number.clone()));
    }
    sig.sort();
    let mut n = 0;
    for side in 0..4 {
        let hdr = format!("J{}", side + 1);
        let mut on_side: Vec<&(usize, i64, i64, String)> = sig.iter().filter(|s| s.0 == side).collect();
        // Outer ring first, then by position: header pins 3.. in that order.
        on_side.sort_by_key(|s| (s.1, s.2));
        for (k, s0) in on_side.iter().take(28).enumerate() {
            n += 1;
            exec(
                &r,
                &mut s,
                "net.connect",
                json!({"net": format!("B{n}"), "pins": [format!("U1.{}", s0.3), format!("{hdr}.{}", k + 3)]}),
            );
        }
        gnd.push(format!("{hdr}.1"));
        vcc.push(format!("{hdr}.2"));
    }
    for i in 1..=4 {
        gnd.push(format!("C{i}.2"));
        vcc.push(format!("C{i}.1"));
    }
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": gnd}));
    exec(&r, &mut s, "net.connect", json!({"net": "VCC", "pins": vcc}));
    (d, r, s)
}

/// Measured outcome of one routing run.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    /// Router name.
    pub router: String,
    /// Connections (pads − 1 per net).
    pub connections: u64,
    /// Unrouted connections left.
    pub unrouted: u64,
    /// Routed share in percent.
    pub completion: f64,
    /// Vias.
    pub vias: usize,
    /// Track segments.
    pub segments: usize,
    /// Total track length (mm).
    pub length_mm: f64,
    /// Corners of 90° or sharper between two track segments of a net on a layer.
    pub sharp_corners: usize,
    /// Track segments that are not horizontal, vertical or 45°.
    pub any_angle: usize,
    /// Wall-clock time (ms).
    pub ms: u128,
    /// DRC errors other than unrouted connections.
    pub drc_errors: usize,
}

/// Counts corners of 90° or sharper (two segments of one net and layer meeting at a point
/// nothing else touches) and segments that are not octilinear.
fn shape_counts(p: &Project) -> (usize, usize) {
    type Key = (String, Option<String>, i64, i64);
    let mut ends: BTreeMap<Key, Vec<(f64, f64)>> = BTreeMap::new();
    let mut any = 0;
    for t in &p.board().tracks {
        let (dx, dy) = ((t.end.x.0 - t.start.x.0) as f64, (t.end.y.0 - t.start.y.0) as f64);
        if dx.abs() > 2.0 && dy.abs() > 2.0 && (dx.abs() - dy.abs()).abs() > 2.0 {
            any += 1;
        }
        for (at, d) in [(t.start, (dx, dy)), (t.end, (-dx, -dy))] {
            ends.entry((t.layer.clone(), t.net.clone(), at.x.0, at.y.0)).or_default().push(d);
        }
    }
    let vias: std::collections::BTreeSet<(i64, i64)> = p.board().vias.iter().map(|v| (v.at.x.0, v.at.y.0)).collect();
    let mut sharp = 0;
    for ((_, _, x, y), ds) in &ends {
        if ds.len() != 2 || vias.contains(&(*x, *y)) {
            continue;
        }
        let (a, b) = (ds[0], ds[1]);
        let (la, lb) = ((a.0 * a.0 + a.1 * a.1).sqrt(), (b.0 * b.0 + b.1 * b.1).sqrt());
        if la < 1.0 || lb < 1.0 {
            continue;
        }
        // Angle between the two outgoing directions: 180° = straight, ≤ 90° = sharp corner.
        let cos = (a.0 * b.0 + a.1 * b.1) / (la * lb);
        if cos > -1e-3 {
            sharp += 1;
        }
    }
    (sharp, any)
}

fn drc_errors(p: &Project) -> usize {
    cadlab::drc::check(p)
        .into_iter()
        .filter(|d| d.severity == cadlab::Severity::Error && d.code != "drc.unrouted")
        .count()
}

/// Metrics of the routed board in the session.
pub fn measure(router: &str, r: &Registry, s: &mut Session, ms: u128, drc_before: usize) -> Metrics {
    let st = exec(r, s, "route.status", json!({}));
    let o = &st["output"];
    let p = s.project.as_ref().unwrap();
    let (sharp, any) = shape_counts(p);
    let length: f64 = p.board().tracks.iter().map(|t| cadlab::board::track_length(t).0 as f64).sum();
    Metrics {
        router: router.into(),
        connections: o["connections"].as_u64().unwrap(),
        unrouted: o["unrouted"].as_u64().unwrap(),
        completion: o["completion"].as_f64().unwrap(),
        vias: p.board().vias.len(),
        segments: p.board().tracks.len(),
        length_mm: length / 1e6,
        sharp_corners: sharp,
        any_angle: any,
        ms,
        drc_errors: drc_errors(p).saturating_sub(drc_before),
    }
}

/// Routes a case with cadlab (`route.all`, seed 1, `budget_ms`), optionally rendering the result
/// to `render/<name>.png`. `CADLAB_BENCH_VERBOSE` prints the router's stats and failures.
/// `CADLAB_BENCH_ROUTER` (`grid`, `gridless`, `auto`) picks the search.
pub fn run_cadlab(case: &Case, budget_ms: u64, render: Option<&Path>) -> Metrics {
    let (_d, r, mut s) = (case.build)();
    let before = drc_errors(s.project.as_ref().unwrap());
    let t = Instant::now();
    let mut args = json!({"budget_ms": budget_ms, "seed": 1});
    if let Ok(router) = std::env::var("CADLAB_BENCH_ROUTER") {
        args["router"] = router.into();
    }
    let o = exec(&r, &mut s, "route.all", args);
    let ms = t.elapsed().as_millis();
    if std::env::var_os("CADLAB_BENCH_VERBOSE").is_some() {
        eprintln!("{}: {}", case.name, o["output"]["stats"]);
        for c in o["output"]["connections"].as_array().unwrap().iter().filter(|c| c["status"] != "routed") {
            eprintln!("  {} {} -> {}: {}", c["net"], c["from"], c["to"], c["reason"]);
        }
    }
    if let Some(dir) = render {
        let path = dir.join(format!("{}.png", case.name));
        exec(&r, &mut s, "render.board", json!({"path": path, "px_per_mm": 30}));
    }
    measure("cadlab", &r, &mut s, ms, before)
}

/// The freerouting jar or launcher from `CADLAB_ORACLE_FREEROUTING`, if set.
pub fn freerouting_tool() -> Option<PathBuf> {
    std::env::var_os("CADLAB_ORACLE_FREEROUTING").map(PathBuf::from).filter(|p| p.exists())
}

/// Routes a case with freerouting (external process, `-mp 20`, one thread; a jar runs with
/// `$JAVA_HOME/bin/java` when `JAVA_HOME` is set) through the DSN/SES exchange and measures the imported result with cadlab's DRC.
pub fn run_freerouting(case: &Case, tool: &Path) -> Metrics {
    let (d, r, mut s) = (case.build)();
    let before = drc_errors(s.project.as_ref().unwrap());
    let dsn = d.path().join(format!("{}.dsn", case.name));
    let ses = d.path().join(format!("{}.ses", case.name));
    exec(&r, &mut s, "export.dsn", json!({"path": dsn}));
    let (program, mut args): (PathBuf, Vec<String>) = if tool.extension().is_some_and(|e| e == "jar") {
        let java = std::env::var_os("JAVA_HOME").map_or(PathBuf::from("java"), |h| PathBuf::from(h).join("bin/java"));
        (java, vec!["-jar".into(), tool.display().to_string()])
    } else {
        (tool.to_path_buf(), vec![])
    };
    args.extend(
        [
            "-de",
            &dsn.display().to_string(),
            "-do",
            &ses.display().to_string(),
            "-mp",
            "20",
            "-mt",
            "1",
            "--gui.enabled=false",
        ]
        .map(String::from),
    );
    let t = Instant::now();
    let out = std::process::Command::new(&program).args(&args).output().expect("running freerouting");
    let ms = t.elapsed().as_millis();
    if !ses.is_file() {
        eprintln!("freerouting wrote no session for {}:\n{}", case.name, String::from_utf8_lossy(&out.stderr));
        return Metrics { router: "freerouting (failed)".into(), ms, ..Default::default() };
    }
    exec(&r, &mut s, "route.import_ses", json!({"path": ses}));
    measure("freerouting", &r, &mut s, ms, before)
}

/// Markdown table header.
pub const HEADER: &str = "| board | router | connections | completion | vias | length | segments | sharp corners | time | DRC errors |\n|---|---|---|---|---|---|---|---|---|---|";

/// One table row.
pub fn row(case: &Case, m: &Metrics) -> String {
    format!(
        "| {} | {} | {} | {:.1}% | {} | {:.1} mm | {} | {} | {} ms | {} |",
        case.name,
        m.router,
        m.connections,
        m.completion,
        m.vias,
        m.length_mm,
        m.segments,
        m.sharp_corners,
        m.ms,
        m.drc_errors
    )
}
