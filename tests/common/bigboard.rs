//! A synthetic large board for performance tests and benchmarks (deterministic, seeded).
//!
//! [`build`] with [`Spec::default`]: a 160 × 100 mm four-layer board with ~500 components (20
//! QFP/QFN ICs in a 5 × 4 grid of cells, ~470 0402/0603 passives around and under them, 12
//! 2 × 10 pin headers along the top and bottom edges), ~1400 nets, a few thousand tracks and vias
//! (escape fanouts of every IC and passive pad, L-shaped bottom-layer routes for most local nets)
//! and four inner-layer pours (GND on In1.Cu; 3V3, 1V8 and 5V on In2.Cu). The routing is not
//! clean (routes cross), which is fine for timing: DRC then also has violations to report.
//!
//! [`timings`] measures every heavy operation on it. Used by `tests/perf.rs` (`#[ignore]`d
//! timings, equivalence checks on small variants) and `examples/bigboard.rs`
//! (`cargo run --release --example bigboard [runs]`).

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::Project;
use cadlab::model::board::{BoardSide, PadConnection, PlacedFootprint, Track, Via, Zone};
use cadlab::model::circuit::{Net, PinRef};
use cadlab::{Angle, Nm, Point};
use serde_json::{Value, json};

/// Board size and seed.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    /// IC cells across (32 mm each).
    pub cols: usize,
    /// IC cells down (21.5 mm each, plus 7 mm connector strips at the top and bottom).
    pub rows: usize,
    /// Pin headers along each of the top and bottom edges.
    pub headers_per_edge: usize,
    /// Passives per cell (top side; bottom-side ones under the ICs come on top of that).
    pub passives_per_cell: usize,
    /// Fraction (per mille) of local nets left unrouted.
    pub unrouted_permille: u64,
    /// Seed.
    pub seed: u64,
}

impl Default for Spec {
    fn default() -> Self {
        Spec { cols: 5, rows: 4, headers_per_edge: 6, passives_per_cell: 22, unrouted_permille: 200, seed: 1 }
    }
}

impl Spec {
    /// A small variant (2 × 1 cells) for equivalence tests.
    pub fn small(seed: u64) -> Self {
        Spec { cols: 2, rows: 1, headers_per_edge: 1, passives_per_cell: 10, unrouted_permille: 300, seed }
    }
}

/// Small deterministic LCG (Knuth's MMIX constants).
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    /// Uniform in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// µm → Nm point.
fn um(x: i64, y: i64) -> Point {
    Point::new(Nm(x * 1000), Nm(y * 1000))
}

const CELL_W: i64 = 32_000;
const CELL_H: i64 = 21_500;
const STRIP: i64 = 7_000;

struct Ic {
    part: &'static str,
    package: &'static str,
    pins: usize,
    /// Half-size of the pad ring (µm), outer edge.
    outer: i64,
    /// Half-size of the free area inside the pad ring (µm), for bottom-side passives.
    inner: i64,
}

const ICS: [Ic; 3] = [
    Ic { part: "BIG_QFP100", package: "LQFP-100", pins: 100, outer: 8_400, inner: 5_000 },
    Ic { part: "BIG_QFP64", package: "LQFP-64", pins: 64, outer: 6_000, inner: 3_000 },
    Ic { part: "BIG_QFN48", package: "QFN-48 7x7mm P0.5mm EP5.1mm", pins: 49, outer: 4_000, inner: 0 },
];

const PASSIVES: [(&str, u64); 4] =
    [("C 100nF 16V X7R 0402", 25), ("R 10k 1% 0402", 50), ("C 1uF 16V X5R 0603", 10), ("LED red 0603", 15)];

/// Builds the board in a new project under a temporary directory.
pub fn build(spec: Spec) -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("big")}));
    let mut rng = Lcg::new(spec.seed);
    let (cols, rows) = (spec.cols as i64, spec.rows as i64);
    let (bw, bh) = (cols * CELL_W, rows * CELL_H + 2 * STRIP);

    // Parts.
    for ic in &ICS {
        let pins: Vec<Value> = (1..=ic.pins)
            .map(|i| json!({"number": i.to_string(), "name": format!("P{i}"), "kind": "bidirectional"}))
            .collect();
        exec(&r, &mut s, "part.create", json!({"id": ic.part, "category": "mcu", "package": ic.package, "pins": pins}));
    }
    let hdr_pins: Vec<Value> =
        (1..=20).map(|i| json!({"number": i.to_string(), "name": format!("H{i}"), "kind": "passive"})).collect();
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "BIG_HDR", "category": "connector", "package": "PinHeader 2x10", "pins": hdr_pins}),
    );
    exec(
        &r,
        &mut s,
        "board.outline",
        json!({"width": format!("{}mm", bw / 1000), "height": format!("{}mm", bh as f64 / 1000.0)}),
    );

    // Components: one IC per cell, passives (type drawn per slot), headers.
    let ncells = spec.cols * spec.rows;
    let mut ic_of_cell: Vec<String> = Vec::new();
    for c in 0..ncells {
        let o = exec(&r, &mut s, "circuit.add", json!({"part": ICS[c % 3].part}));
        ic_of_cell.push(o["output"]["refdes"][0].as_str().unwrap().to_string());
    }
    let mut headers = Vec::new();
    let nhdr = 2 * spec.headers_per_edge;
    if nhdr > 0 {
        let o = exec(&r, &mut s, "circuit.add", json!({"part": "BIG_HDR", "count": nhdr}));
        headers = o["output"]["refdes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    }
    // Passive slots per cell: top side around the IC, bottom side inside its pad ring.
    let mut slots: Vec<(usize, Point, BoardSide)> = Vec::new();
    for c in 0..ncells {
        let (cx, cy) = cell_center(c, spec.cols);
        let ic = &ICS[c % 3];
        let mut top = Vec::new();
        let keep = ic.outer + 2_600;
        let mut y = -CELL_H / 2 + 1_400;
        while y <= CELL_H / 2 - 1_400 {
            let mut x = -CELL_W / 2 + 2_000;
            while x <= CELL_W / 2 - 2_000 {
                if x.abs() > keep || y.abs() > keep {
                    top.push(um(cx + x, cy + y));
                }
                x += 3_200;
            }
            y += 2_400;
        }
        // Spread the chosen slots over the cell.
        let take = spec.passives_per_cell.min(top.len());
        for k in 0..take {
            slots.push((c, top[k * top.len() / take], BoardSide::Top));
        }
        if ic.inner > 1_500 {
            let mut y = -ic.inner + 1_000;
            while y <= ic.inner - 1_000 {
                let mut x = -ic.inner + 1_600;
                while x <= ic.inner - 1_600 {
                    slots.push((c, um(cx + x, cy + y), BoardSide::Bottom));
                    x += 3_200;
                }
                y += 2_400;
            }
        }
    }
    let total: u64 = PASSIVES.iter().map(|p| p.1).sum();
    let kinds: Vec<usize> = slots
        .iter()
        .map(|_| {
            let mut v = rng.below(total);
            let mut k = 0;
            while v >= PASSIVES[k].1 {
                v -= PASSIVES[k].1;
                k += 1;
            }
            k
        })
        .collect();
    let mut passive_refs: Vec<String> = vec![String::new(); slots.len()];
    for (k, (name, _)) in PASSIVES.iter().enumerate() {
        let idx: Vec<usize> = (0..slots.len()).filter(|&i| kinds[i] == k).collect();
        if idx.is_empty() {
            continue;
        }
        let o = exec(&r, &mut s, "circuit.add", json!({"part": name, "count": idx.len()}));
        for (i, v) in idx.iter().zip(o["output"]["refdes"].as_array().unwrap()) {
            passive_refs[*i] = v.as_str().unwrap().to_string();
        }
    }

    let p = s.project.as_mut().unwrap();
    {
        let b = p.board_mut();
        b.stackup.copper_layers = 4;
        b.rules.clearance = Nm::from_um(150);
        b.rules.track_width = Nm::from_um(150);
        b.rules.min_track_width = Nm::from_um(120);
        b.rules.via_drill = Nm::from_um(250);
        b.rules.via_diameter = Nm::from_um(500);
        b.rules.min_drill = Nm::from_um(200);
        b.rules.min_annular_ring = Nm::from_um(100);
        b.rules.hole_to_hole = Nm::from_um(250);
    }
    // Placement.
    let place = |p: &mut Project, r: &str, at: Point, rot: Angle, side: BoardSide| {
        p.board_mut()
            .footprints
            .insert(r.to_string(), PlacedFootprint { at, rotation: rot, side, locked: false, footprint: None });
    };
    for (c, r) in ic_of_cell.iter().enumerate() {
        let (cx, cy) = cell_center(c, spec.cols);
        place(p, r, um(cx, cy), Angle::ZERO, BoardSide::Top);
    }
    for (i, r) in headers.iter().enumerate() {
        let (edge, k) = (i / spec.headers_per_edge.max(1), (i % spec.headers_per_edge.max(1)) as i64);
        let pitch = bw / spec.headers_per_edge.max(1) as i64;
        let x = pitch * k + pitch / 2;
        let y = if edge == 0 { STRIP / 2 } else { bh - STRIP / 2 };
        place(p, r, um(x, y), Angle::DEG_90, BoardSide::Top);
    }
    for (i, (_, at, side)) in slots.iter().enumerate() {
        let rot = if rng.below(2) == 0 { Angle::ZERO } else { Angle::DEG_180 };
        place(p, &passive_refs[i], *at, rot, *side);
    }

    // Nets.
    let power = |c: usize| if (c % spec.cols) * 5 < spec.cols * 3 { "3V3" } else { "1V8" };
    let mut nets: BTreeMap<String, BTreeSet<PinRef>> = BTreeMap::new();
    let mut add = |net: &str, r: &str, pin: &str| {
        nets.entry(net.to_string()).or_default().insert(PinRef::new(r, pin));
    };
    // IC pins: every 10th GND, every 10th (offset 5) power, the QFN pad GND, the rest signals.
    let mut free_signal: Vec<Vec<String>> = vec![Vec::new(); ncells];
    for (c, r) in ic_of_cell.iter().enumerate() {
        let ic = &ICS[c % 3];
        for i in 1..=ic.pins {
            let pin = i.to_string();
            if i.is_multiple_of(10) || (ic.pins == 49 && i == 49) {
                add("GND", r, &pin);
            } else if i % 10 == 5 {
                add(power(c), r, &pin);
            } else {
                free_signal[c].push(pin);
            }
        }
    }
    let mut next_sig = |c: usize| -> Option<String> {
        let v = &mut free_signal[c];
        if v.is_empty() { None } else { Some(v.remove(v.len() / 2)) }
    };
    let mut n = 0usize;
    // Passives.
    let mut leds: Vec<(usize, String)> = Vec::new();
    for (i, (c, _, _)) in slots.iter().enumerate() {
        let r = &passive_refs[i];
        match kinds[i] {
            0 | 2 => {
                add(power(*c), r, "1");
                add("GND", r, "2");
            }
            3 => leds.push((*c, r.clone())),
            _ => {}
        }
    }
    for (i, (c, _, _)) in slots.iter().enumerate() {
        if kinds[i] != 1 {
            continue;
        }
        let r = &passive_refs[i];
        let name = format!("S{n}");
        n += 1;
        if let Some(pin) = next_sig(*c) {
            add(&name, &ic_of_cell[*c], &pin);
        }
        add(&name, r, "1");
        let name2 = format!("S{n}");
        n += 1;
        add(&name2, r, "2");
        if let Some(k) = leds.iter().position(|(lc, _)| lc == c) {
            let (_, led) = leds.remove(k);
            add(&name2, &led, "1");
            add("GND", &led, "2");
        } else if i.is_multiple_of(3)
            && let Some(pin) = next_sig(*c)
        {
            add(&name2, &ic_of_cell[*c], &pin);
        }
    }
    for (_, led) in leds {
        add("GND", &led, "2");
        let name = format!("S{n}");
        n += 1;
        add(&name, &led, "1");
    }
    // Headers: 1 GND, 2 5V, odd pins to IC signals round robin, even pins alone.
    for (h, r) in headers.iter().enumerate() {
        add("GND", r, "1");
        add("5V", r, "2");
        for pin in 3..=20 {
            let name = format!("S{n}");
            n += 1;
            add(&name, r, &pin.to_string());
            let c = (h * 7 + pin) % ncells;
            if pin % 2 == 1
                && let Some(sig) = next_sig(c)
            {
                add(&name, &ic_of_cell[c], &sig);
            }
        }
    }
    // Remaining IC signals: one in ten joins the next cell's IC (buses), else alone.
    for c in 0..ncells {
        while let Some(pin) = next_sig(c) {
            let name = format!("S{n}");
            n += 1;
            add(&name, &ic_of_cell[c], &pin);
            if n.is_multiple_of(10)
                && let Some(other) = next_sig((c + 1) % ncells)
            {
                add(&name, &ic_of_cell[(c + 1) % ncells], &other);
            }
        }
    }
    for (name, pins) in nets {
        let id = p.alloc_id();
        p.circuit_mut().nets.insert(name, Net { pins, ..Net::new(id) });
    }

    // Copper: fanout of every netted pad to a via, then L-shaped bottom routes between the vias of
    // nets whose ends lie in one cell.
    let pads = cadlab::board::placed_pads(p);
    let centers: BTreeMap<&str, Point> = p.board().footprints.iter().map(|(r, f)| (r.as_str(), f.at)).collect();
    let mut tracks: Vec<(String, Option<String>, Point, Point)> = Vec::new();
    let mut vias: Vec<(Point, Option<String>)> = Vec::new();
    let mut ends: BTreeMap<String, Vec<Point>> = BTreeMap::new();
    let mut stagger = 0i64;
    for pp in &pads {
        let Some(net) = &pp.net else { continue };
        if pp.hole.is_some() {
            ends.entry(net.clone()).or_default().push(pp.center);
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for q in &pp.shape.outer.0 {
            (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
        }
        let fc = centers[pp.refdes.as_str()];
        let (dx, dy) = (pp.center.x.0 - fc.x.0, pp.center.y.0 - fc.y.0);
        let is_ic = pp.refdes.starts_with('U');
        if is_ic && dx.abs() < 1_000_000 && dy.abs() < 1_000_000 {
            // Exposed pad: vias straight under it.
            vias.push((pp.center, Some(net.clone())));
            continue;
        }
        let to = if is_ic {
            stagger += 1;
            let reach = 600_000 + (stagger % 3) * 750_000;
            if dx.abs() > dy.abs() {
                let x = if dx > 0 { x1 + reach } else { x0 - reach };
                Point::new(Nm(x), pp.center.y)
            } else {
                let y = if dy > 0 { y1 + reach } else { y0 - reach };
                Point::new(pp.center.x, Nm(y))
            }
        } else {
            // Passive pads: pin 1 up, pin 2 down (relative to the board).
            let y = if pp.number == "1" { pp.center.y.0 + 950_000 } else { pp.center.y.0 - 950_000 };
            Point::new(pp.center.x, Nm(y))
        };
        let layer = if pp.side == BoardSide::Top { "F.Cu" } else { "B.Cu" };
        tracks.push((layer.into(), Some(net.clone()), pp.center, to));
        vias.push((to, Some(net.clone())));
        ends.entry(net.clone()).or_default().push(to);
    }
    let cell_of = |q: Point| ((q.x.0 / 1000) / CELL_W, (q.y.0 / 1000 - STRIP) / CELL_H);
    for (net, pts) in &ends {
        if pts.len() < 2 || ["GND", "3V3", "1V8", "5V"].contains(&net.as_str()) {
            continue;
        }
        if rng.below(1000) < spec.unrouted_permille {
            continue;
        }
        let c = cell_of(pts[0]);
        if pts.iter().any(|q| cell_of(*q) != c) {
            continue;
        }
        let mut pts = pts.clone();
        pts.sort_by_key(|q| (q.x, q.y));
        for w in pts.windows(2) {
            let corner = Point::new(w[1].x, w[0].y);
            if corner != w[0] {
                tracks.push(("B.Cu".into(), Some(net.clone()), w[0], corner));
            }
            if corner != w[1] {
                tracks.push(("B.Cu".into(), Some(net.clone()), corner, w[1]));
            }
        }
    }
    let rules = p.board().rules.clone();
    for (layer, net, a, b) in tracks {
        let id = p.alloc_id();
        p.board_mut().tracks.push(Track {
            id,
            layer,
            width: rules.track_width,
            net,
            start: a,
            end: b,
            mid: None,
            locked: false,
        });
    }
    for (at, net) in vias {
        let id = p.alloc_id();
        p.board_mut().vias.push(Via {
            id,
            at,
            drill: rules.via_drill,
            diameter: rules.via_diameter,
            net,
            from: "F.Cu".into(),
            to: "B.Cu".into(),
            locked: false,
        });
    }

    // Pours: GND on In1.Cu; 3V3 / 1V8 split and a 5V strip on In2.Cu.
    let split = (cols * 3 / 5).max(1) * CELL_W;
    let rect = |x0: i64, y0: i64, x1: i64, y1: i64| vec![um(x0, y0), um(x1, y0), um(x1, y1), um(x0, y1)];
    let zones = [
        ("GND_In1", "GND", "In1.Cu", rect(0, 0, bw, bh)),
        ("3V3_In2", "3V3", "In2.Cu", rect(0, STRIP, split, bh)),
        ("1V8_In2", "1V8", "In2.Cu", rect(split, STRIP, bw, bh)),
        ("5V_In2", "5V", "In2.Cu", rect(0, 0, bw, STRIP)),
    ];
    for (name, net, layer, outline) in zones {
        let id = p.alloc_id();
        p.board_mut().zones.push(Zone {
            id,
            name: name.into(),
            net: Some(net.into()),
            layers: vec![layer.into()],
            outline,
            priority: 0,
            clearance: None,
            min_width: None,
            pads: PadConnection::Thermal,
            thermal_gap: None,
            thermal_spoke: None,
        });
    }
    s.mark_dirty();
    (dir, r, s)
}

fn cell_center(c: usize, cols: usize) -> (i64, i64) {
    let (col, row) = ((c % cols) as i64, (c / cols) as i64);
    (col * CELL_W + CELL_W / 2, STRIP + row * CELL_H + CELL_H / 2)
}

/// Counts for reports.
pub fn stats(p: &Project) -> String {
    let b = p.board();
    format!(
        "{} components, {} nets, {} pads, {} tracks, {} vias, {} zones",
        p.circuit().components.len(),
        p.circuit().nets.len(),
        cadlab::board::placed_pads(p).len(),
        b.tracks.len(),
        b.vias.len(),
        b.zones.len()
    )
}

/// One measured step.
pub struct Timing {
    /// What was measured.
    pub name: &'static str,
    /// Best of the runs.
    pub time: Duration,
    /// Result size, for the report.
    pub note: String,
}

fn best<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut last = None;
    for _ in 0..runs.max(1) {
        cadlab::board::zones::clear_fill_cache();
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed());
        last = Some(v);
    }
    (best, last.unwrap())
}

/// Times every heavy operation on the board of `spec`. Each step runs `runs` times with the
/// in-process zone fill cache cleared first (a cold run, as a fresh CLI process sees it) and
/// the best time is kept; the "fills cached" steps show repeated queries in one session.
/// Returns the board summary and the rows.
pub fn timings(spec: Spec, runs: usize) -> (String, Vec<Timing>) {
    use cadlab::board::{self, zones};
    let mut out = Vec::new();
    let mut row = |name: &'static str, time: Duration, note: String| out.push(Timing { name, time, note });
    let t = Instant::now();
    let (dir, r, mut s) = build(spec);
    row("generate", t.elapsed(), String::new());
    let root = dir.path().join("big");
    let summary = stats(s.project.as_ref().unwrap());
    let (d, _) = best(runs, || exec(&r, &mut s, "project.save", json!({})));
    row("project.save", d, String::new());
    let (d, p) = best(runs, || Project::load(&root).unwrap());
    row("Project::load", d, String::new());
    let p = &p;
    let (d, pads) = best(runs, || board::placed_pads(p));
    row("placed_pads", d, format!("{} pads", pads.len()));
    let (d, base) = best(runs, || board::base_copper_items(p));
    row("base_copper_items", d, format!("{} items", base.len()));
    let (d, fills) = best(runs, || zones::fill_zones_uncached(p, &base));
    let n: usize = fills.iter().map(|f| f.fill.len()).sum();
    row("zone fill (4 inner pours)", d, format!("{} fills, {n} islands", fills.len()));
    let (d, items) = best(runs, || board::copper_items(p));
    row("copper_items (with fill)", d, format!("{} items", items.len()));
    let (d, isl) = best(runs, || board::islands(&items));
    let n = isl.iter().enumerate().filter(|(i, k)| i == *k).count();
    row("islands", d, format!("{n} islands"));
    let (d, rats) = best(runs, || board::ratsnest_items(&items));
    row("ratsnest_items (islands + MST)", d, format!("{} lines", rats.len()));
    let (d, _) = best(runs, || board::ratsnest(p));
    row("ratsnest (with fill)", d, String::new());
    let (d, diags) = best(runs, || cadlab::drc::check(p));
    row("drc::check", d, format!("{} diagnostics", diags.len()));
    let out_dir = dir.path().join("out");
    let png = out_dir.join("board.png");
    let (d, _) = best(runs, || exec(&r, &mut s, "render.board", json!({"path": png})));
    row("render.board (PNG)", d, String::new());
    let (d, _) = best(runs, || exec(&r, &mut s, "export.gerber", json!({"dir": out_dir.join("gerber")})));
    row("export.gerber", d, String::new());
    let pcb = out_dir.join("kicad/big.kicad_pcb");
    let (d, _) = best(runs, || exec(&r, &mut s, "board.export_kicad", json!({"path": pcb})));
    row("board.export_kicad", d, String::new());
    let (d, _) = best(runs, || exec(&r, &mut s, "render.schematic", json!({"path": out_dir.join("sch.png")})));
    row("render.schematic (layout+PNG)", d, String::new());
    // Fills cached in process: repeated queries in one MCP or batch session.
    let _ = board::copper_items(s.project.as_ref().unwrap());
    for (name, cmd, args) in [
        ("board.ratsnest (fills cached)", "board.ratsnest", json!({})),
        ("drc.run (fills cached)", "drc.run", json!({})),
        ("render.board (fills cached)", "render.board", json!({"path": out_dir.join("board2.png")})),
        ("export.gerber (fills cached)", "export.gerber", json!({"dir": out_dir.join("gerber2")})),
    ] {
        let mut d = Duration::MAX;
        for _ in 0..runs.max(1) {
            let t = Instant::now();
            exec(&r, &mut s, cmd, args.clone());
            d = d.min(t.elapsed());
        }
        row(name, d, String::new());
    }
    (summary, out)
}

/// The timings as text, one line per step.
pub fn report(summary: &str, rows: &[Timing]) -> String {
    let mut s = format!("board: {summary}\n");
    for t in rows {
        s.push_str(&format!("{:<34} {:>9.1} ms  {}\n", t.name, t.time.as_secs_f64() * 1e3, t.note));
    }
    s
}
