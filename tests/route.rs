//! M5 autorouter: route test boards to completion with zero DRC errors, determinism,
//! cancellation and budget, rip-up.

mod common;

use std::time::Instant;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::units::Nm;
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn new_project(r: &Registry, s: &mut Session) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    exec(r, s, "project.new", json!({"path": dir.path().join("p")}));
    dir
}

/// The LDO + caps board of `tests/board.rs`.
fn ldo_board(r: &Registry, s: &mut Session) {
    exec(
        r,
        s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}),
    );
    exec(r, s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(r, s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(r, s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(r, s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(r, s, "board.outline", json!({"width": "20mm", "height": "15mm", "corner_radius": "1mm"}));
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(r, s, "place.set", json!({"refdes": "C1", "at": ["5mm", "9mm"], "rotation": 90}));
    exec(r, s, "place.set", json!({"refdes": "C2", "at": ["15mm", "9mm"], "rotation": 90}));
}

fn drc_errors(s: &Session) -> Vec<String> {
    cadlab::drc::check(s.project.as_ref().unwrap())
        .into_iter()
        .filter(|d| d.severity == cadlab::Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

/// Renders the board to `$CADLAB_ROUTE_RENDER/<name>.png` when that variable is set.
fn render(r: &Registry, s: &mut Session, name: &str) {
    if let Some(dir) = std::env::var_os("CADLAB_ROUTE_RENDER") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        exec(r, s, "render.board", json!({"path": path, "px_per_mm": 40}));
    }
}

fn report(name: &str, o: &Value, ms: u128) {
    let st = &o["output"]["stats"];
    eprintln!(
        "{name}: {}/{} routed ({}%), {} vias, {} tracks, length {}, {} iterations, overuse {}, grid {}, {ms} ms",
        st["routed"],
        st["connections"],
        st["completion"],
        st["vias"],
        st["tracks"],
        st["length"],
        st["iterations"],
        st["overuse"],
        st["grid"]
    );
    for c in o["output"]["connections"].as_array().unwrap() {
        if c["status"] != "routed" {
            eprintln!("  failed: {c}");
        }
    }
}

#[test]
fn routes_ldo_board() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    let t = Instant::now();
    let o = exec(&r, &mut s, "route.all", json!({}));
    report("ldo", &o, t.elapsed().as_millis());
    render(&r, &mut s, "ldo");
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["unrouted"], 0);
    assert_eq!(st["output"]["completion"], 100.0);
}

#[test]
fn routes_attiny_board_auto_placed() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    common::boards::build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "40mm", "height": "30mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1.5mm"}));
    let t = Instant::now();
    let o = exec(&r, &mut s, "route.all", json!({"seed": 1}));
    report("attiny-auto", &o, t.elapsed().as_millis());
    render(&r, &mut s, "attiny-auto");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
}

#[test]
fn routes_attiny_board_hand_placed_with_power_class() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    common::boards::build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "32mm", "height": "24mm", "corner_radius": "1mm"}));
    for (refdes, at, rot) in [
        ("J1", ["4mm", "12mm"], 0),
        ("U1", ["10mm", "17mm"], 0),
        ("C1", ["7mm", "20mm"], 90),
        ("C2", ["13.5mm", "20mm"], 90),
        ("U2", ["17mm", "12mm"], 0),
        ("C3", ["17mm", "17mm"], 0),
        ("R1", ["22mm", "17mm"], 0),
        ("J2", ["27mm", "12mm"], 0),
        ("R2", ["13mm", "7mm"], 0),
        ("D1", ["17mm", "5mm"], 0),
    ] {
        exec(&r, &mut s, "place.set", json!({"refdes": refdes, "at": at, "rotation": rot}));
    }
    exec(&r, &mut s, "netclass.set", json!({"name": "power", "track_width": "0.5mm", "clearance": "0.25mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["GND", "VBUS", "3V3"], "class": "power"}));
    let t = Instant::now();
    let o = exec(&r, &mut s, "route.all", json!({}));
    report("attiny-hand-power", &o, t.elapsed().as_millis());
    render(&r, &mut s, "attiny-hand-power");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    let b = s.project.as_ref().unwrap().board();
    assert!(b.tracks.iter().filter(|t| t.net.as_deref() == Some("GND")).all(|t| t.width == cadlab::Nm::from_um(500)));
    // Ripping and routing a net class.
    exec(&r, &mut s, "route.rip", json!({"nets": ["GND", "VBUS", "3V3"]}));
    let o = exec(&r, &mut s, "route.nets", json!({"nets": ["class:power"]}));
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    let nets: std::collections::BTreeSet<&str> =
        o["output"]["connections"].as_array().unwrap().iter().map(|c| c["net"].as_str().unwrap()).collect();
    assert_eq!(nets, ["3V3", "GND", "VBUS"].into_iter().collect());
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    let b = s.project.as_ref().unwrap().board();
    assert!(b.tracks.iter().filter(|t| t.net.as_deref() == Some("SCK")).all(|t| t.width == cadlab::Nm::from_um(250)));
}

fn numbered_pins(n: usize) -> Value {
    Value::Array(
        (1..=n).map(|i| json!({"number": i.to_string(), "name": format!("P{i}"), "kind": "passive"})).collect(),
    )
}

/// A dense synthetic board: an LQFP-32 between a 2x8 header and a SOIC-16, 32 two-pin nets
/// with crossing buses.
fn dense_board(r: &Registry, s: &mut Session) {
    exec(
        r,
        s,
        "part.create",
        json!({"id": "MCU32", "category": "mcu", "package": "LQFP-32", "pins": numbered_pins(32)}),
    );
    exec(r, s, "part.create", json!({"id": "IO16", "category": "ic", "package": "SOIC-16", "pins": numbered_pins(16)}));
    exec(
        r,
        s,
        "part.create",
        json!({"id": "HDR16", "category": "connector", "package": "PinHeader 2x08", "pins": numbered_pins(16)}),
    );
    exec(r, s, "circuit.add", json!({"part": "MCU32", "refdes": "U1"}));
    exec(r, s, "circuit.add", json!({"part": "IO16", "refdes": "U2"}));
    exec(r, s, "circuit.add", json!({"part": "HDR16", "refdes": "J1"}));
    for i in 1..=8 {
        exec(r, s, "net.connect", json!({"net": format!("A{i}"), "pins": [format!("U1.{i}"), format!("J1.{i}")]}));
        exec(
            r,
            s,
            "net.connect",
            json!({"net": format!("B{i}"), "pins": [format!("U1.{}", 8 + i), format!("U2.{i}")]}),
        );
        exec(
            r,
            s,
            "net.connect",
            json!({"net": format!("C{i}"), "pins": [format!("U1.{}", 16 + i), format!("U2.{}", 17 - i)]}),
        );
        exec(
            r,
            s,
            "net.connect",
            json!({"net": format!("D{i}"), "pins": [format!("U1.{}", 24 + i), format!("J1.{}", 8 + i)]}),
        );
    }
    exec(r, s, "board.outline", json!({"width": "40mm", "height": "30mm"}));
    exec(r, s, "place.set", json!({"refdes": "J1", "at": ["6mm", "15mm"]}));
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["20mm", "15mm"]}));
    exec(r, s, "place.set", json!({"refdes": "U2", "at": ["32mm", "15mm"]}));
}

#[test]
fn routes_dense_board() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    dense_board(&r, &mut s);
    let t = Instant::now();
    let o = exec(&r, &mut s, "route.all", json!({"seed": 7}));
    report("dense", &o, t.elapsed().as_millis());
    render(&r, &mut s, "dense");
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
    assert!(o["output"]["stats"]["completion"].as_f64().unwrap() >= 90.0);
}

fn copper(s: &Session) -> (Value, Value) {
    let b = s.project.as_ref().unwrap().board();
    (serde_json::to_value(&b.tracks).unwrap(), serde_json::to_value(&b.vias).unwrap())
}

#[test]
fn routing_is_deterministic() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for _ in 0..2 {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        dense_board(&r, &mut s);
        let o = exec(&r, &mut s, "route.all", json!({"seed": 42, "effort": "low"}));
        results.push((copper(&s), o["output"].clone()));
    }
    assert!(results[0].0.0.as_array().is_some_and(|a| !a.is_empty()));
    assert_eq!(results[0], results[1]);
}

struct Collect(std::sync::Mutex<Vec<String>>);

impl cadlab::command::Progress for Collect {
    fn report(&self, _: u64, _: Option<u64>, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

#[test]
fn cancellation_progress_and_budget() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    dense_board(&r, &mut s);
    let before = copper(&s);

    // Cancelled: an error, and the project is unchanged.
    let cancel = cadlab::command::CancelToken::new();
    cancel.cancel();
    let progress = Collect(Default::default());
    let f = r
        .execute_with(&mut s, "route.all", json!({}), RunOptions::default(), &progress, &cancel)
        .expect_err("cancelled");
    assert_eq!(f.error.diagnostic.code, "cancelled");
    assert_eq!(copper(&s), before);

    // Progress is reported.
    let progress = Collect(Default::default());
    r.execute_with(
        &mut s,
        "route.all",
        json!({"effort": "low"}),
        RunOptions { dry_run: true },
        &progress,
        &cadlab::command::CancelToken::new(),
    )
    .unwrap();
    let msgs = progress.0.into_inner().unwrap();
    assert!(msgs.iter().any(|m| m.starts_with("iteration 1")), "{msgs:?}");
    assert_eq!(msgs.last().map(String::as_str), Some("done"));

    // A tiny budget returns a partial but DRC-clean result.
    let o = exec(&r, &mut s, "route.all", json!({"budget_ms": 1}));
    let st = &o["output"]["stats"];
    eprintln!("budget 1 ms: {st}");
    assert!(st["completion"].as_f64().unwrap() < 100.0);
    assert_eq!(st["budget_exhausted"], true);
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
    // A work budget cuts the run at the same point every time, and with any number of threads;
    // what it reports reproduces it.
    let cut = |s: &mut Session, threads: usize| {
        exec(&r, s, "route.rip", json!({"all": true}));
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let o = pool.install(|| exec(&r, s, "route.all", json!({"work_budget": 3000})));
        // Without the IDs, which go on counting after each rip.
        let (mut tracks, mut vias) = copper(s);
        for v in tracks.as_array_mut().unwrap().iter_mut().chain(vias.as_array_mut().unwrap()) {
            v.as_object_mut().unwrap().remove("id");
        }
        ((tracks, vias), o["output"]["stats"].clone())
    };
    let (c1, st1) = cut(&mut s, 1);
    assert_eq!(st1["budget_exhausted"], true, "{st1}");
    assert!(st1["work"].as_u64().unwrap() > 0, "{st1}");
    for threads in [1, 4] {
        let (c, st) = cut(&mut s, threads);
        assert_eq!(c, c1);
        assert_eq!(st, st1);
    }
    // A clock and a count cannot both limit a run: the result would depend on the clock.
    let f = r
        .execute(&mut s, "route.all", json!({"budget": "1s", "work_budget": 5}), RunOptions::default())
        .expect_err("both budgets");
    assert_eq!(f.error.diagnostic.code, "route.budget");
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    let o = exec(&r, &mut s, "route.all", json!({"budget_ms": 1}));
    assert!(o["diagnostics"].to_string().contains("route.budget_cut"), "{o}");
    // Ripping the partial result and routing again completes the board.
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    let o = exec(&r, &mut s, "route.all", json!({}));
    assert_eq!(o["output"]["stats"]["failed"], 0);
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["completion"], 100.0);
}

#[test]
fn rip_nets_connection() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);

    // One net only.
    let o = exec(&r, &mut s, "route.nets", json!({"nets": ["3V3"]}));
    assert_eq!(o["output"]["stats"]["connections"], 1);
    assert_eq!(o["output"]["connections"][0]["status"], "routed");
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["unrouted"], 4);
    assert_eq!(st["output"]["unrouted_by_net"], json!({"GND": 2, "VIN": 2}));
    assert!(!st["output"]["problem_areas"].as_array().unwrap().is_empty());

    // One connection, by pin names.
    let o = exec(&r, &mut s, "route.connection", json!({"from": "U1.VIN", "to": "U1.EN"}));
    assert_eq!(o["output"]["stats"]["routed"], 1);
    assert_eq!(o["output"]["connections"][0]["from"], "U1.1");
    let o = exec(&r, &mut s, "route.connection", json!({"from": "U1.VIN", "to": "U1.EN"}));
    assert_eq!(o["output"]["connections"], json!([]), "already connected");
    let f = r
        .execute(&mut s, "route.connection", json!({"from": "U1.VIN", "to": "C2.1"}), RunOptions::default())
        .expect_err("different nets");
    assert_eq!(f.error.diagnostic.code, "route.invalid_endpoint");

    // Everything else; existing copper is kept.
    let (tracks_before, _) = copper(&s);
    exec(&r, &mut s, "route.all", json!({}));
    let (tracks, _) = copper(&s);
    for t in tracks_before.as_array().unwrap() {
        assert!(tracks.as_array().unwrap().contains(t), "existing track kept");
    }
    assert_eq!(drc_errors(&s), Vec::<String>::new());

    // Rip one net, keeping locked tracks.
    let gnd_id = {
        let p = s.project.as_mut().unwrap();
        let t = p.board_mut().tracks.iter_mut().find(|t| t.net.as_deref() == Some("GND")).unwrap();
        t.locked = true;
        t.id
    };
    let o = exec(&r, &mut s, "route.rip", json!({"nets": ["GND"]}));
    assert!(o["output"]["tracks"].as_u64().unwrap() > 0);
    assert_eq!(o["output"]["locked_kept"], 1);
    let b = s.project.as_ref().unwrap().board();
    assert!(b.tracks.iter().filter(|t| t.net.as_deref() == Some("GND")).all(|t| t.id == gnd_id));
    assert!(b.tracks.iter().any(|t| t.net.as_deref() == Some("VIN")));
    let f = r.execute(&mut s, "route.rip", json!({}), RunOptions::default()).expect_err("nothing to rip");
    assert_eq!(f.error.diagnostic.code, "route.rip_what");
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    let b = s.project.as_ref().unwrap().board();
    assert_eq!(b.tracks.len(), 1, "only the locked track is left");
    assert!(b.vias.is_empty());
}

#[test]
fn failures_explain_what_blocks() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    // A wall of keep-out between U1 and C2 on every layer.
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "wall", "outline": {"rect": {"from": ["12.5mm", "-1mm"], "to": ["13.5mm", "16mm"]}},
               "no_tracks": true, "no_vias": true}),
    );
    let o = exec(&r, &mut s, "route.all", json!({}));
    report("wall", &o, 0);
    let failed: Vec<&Value> =
        o["output"]["connections"].as_array().unwrap().iter().filter(|c| c["status"] == "failed").collect();
    assert_eq!(failed.len(), 2, "C2's two connections");
    for c in &failed {
        let reason = c["reason"].as_str().unwrap();
        assert!(reason.contains("keep-out `wall`"), "{reason}");
        assert!(c["at"].is_array());
        assert!(!c["hints"].as_array().unwrap().is_empty());
    }
    assert_eq!(o["diagnostics"][0]["code"], "route.incomplete");
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());

    let f = r.execute(&mut s, "route.all", json!({"layers": ["In1.Cu"]}), RunOptions::default()).expect_err("layer");
    assert_eq!(f.error.diagnostic.code, "route.invalid_layer");
}

/// An LQFP-48 (0.5 mm pitch) with default rules (0.25 mm tracks): most of its pads are off the
/// 0.225 mm grid and need escapes. Pins go to four 1x12 headers, one per side.
fn lqfp_board(r: &Registry, s: &mut Session) {
    exec(
        r,
        s,
        "part.create",
        json!({"id": "MCU48", "category": "mcu", "package": "LQFP-48", "pins": numbered_pins(48)}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"id": "HDR12", "category": "connector", "package": "PinHeader 1x12", "pins": numbered_pins(12)}),
    );
    exec(r, s, "circuit.add", json!({"part": "MCU48", "refdes": "U1"}));
    exec(r, s, "board.outline", json!({"width": "60mm", "height": "60mm"}));
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["30mm", "30mm"]}));
    for (k, (x, y, rot)) in [(6, 30, 0), (30, 6, 90), (54, 30, 0), (30, 54, 90)].iter().enumerate() {
        let j = format!("J{}", k + 1);
        exec(r, s, "circuit.add", json!({"part": "HDR12", "refdes": j}));
        exec(r, s, "place.set", json!({"refdes": j, "at": [format!("{x}mm"), format!("{y}mm")], "rotation": rot}));
        for i in 1..=12 {
            let pin = k * 12 + i;
            exec(
                r,
                s,
                "net.connect",
                json!({"net": format!("P{pin}"), "pins": [format!("U1.{pin}"), format!("{j}.{i}")]}),
            );
        }
    }
}

fn completion(o: &Value) -> f64 {
    o["output"]["stats"]["completion"].as_f64().unwrap()
}

#[test]
fn fine_pitch_escapes() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    // The grid search alone, without and with escapes; then the default (`auto`: the gridless
    // search reaches the off-grid pads the grid cannot) without escapes.
    for (fanout, router) in [(false, "grid"), (true, "grid"), (false, "auto")] {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        lqfp_board(&r, &mut s);
        let o = exec(&r, &mut s, "route.all", json!({"fanout": fanout, "router": router}));
        report(&format!("lqfp48 fanout={fanout} router={router}"), &o, 0);
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new());
        let st = &o["output"]["stats"];
        results.push((completion(&o), st["escapes"].as_u64().unwrap(), st["grid"].as_str().unwrap().to_string()));
    }
    assert_eq!(results[0].1, 0);
    assert!(results[1].1 > 10, "{results:?}");
    // Without escapes the grid search does not reach every pad at its own pitch: the run is made
    // again on finer grids (D84), or stays incomplete.
    assert!(results[0].0 < 100.0 || results[0].2 != "0.225mm", "escapes help the grid: {results:?}");
    assert_eq!((results[1].0, results[1].2.as_str()), (100.0, "0.225mm"), "{results:?}");
    assert_eq!(results[2].0, 100.0, "the gridless search needs no escapes: {results:?}");
}

/// A BGA-64 (8 × 8, 0.8 mm) with every ball on a net to a header pin, on 4 layers with
/// fine-pitch rules.
fn bga_board(r: &Registry, s: &mut Session) {
    let balls: Vec<String> = "ABCDEFGH".chars().flat_map(|c| (1..=8).map(move |i| format!("{c}{i}"))).collect();
    let pins: Vec<Value> = balls.iter().map(|b| json!({"number": b, "name": b, "kind": "passive"})).collect();
    exec(
        r,
        s,
        "part.create",
        json!({"id": "BGA64", "category": "ic", "package": "BGA-64 8x8 P0.8mm 7x7mm", "pins": pins}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"id": "HDR32", "category": "connector", "package": "PinHeader 2x16 P1.27mm", "pins": numbered_pins(32)}),
    );
    exec(r, s, "board.setup", json!({"layers": 4}));
    exec(
        r,
        s,
        "board.rules",
        json!({"clearance": "0.1mm", "track_width": "0.1mm", "min_track_width": "0.1mm", "via_drill": "0.2mm",
               "via_diameter": "0.45mm", "min_drill": "0.2mm", "min_annular_ring": "0.1mm", "hole_to_hole": "0.25mm"}),
    );
    exec(r, s, "board.outline", json!({"width": "36mm", "height": "30mm"}));
    exec(r, s, "circuit.add", json!({"part": "BGA64", "refdes": "U1"}));
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["18mm", "15mm"]}));
    for (j, x) in [("J1", 7), ("J2", 29)] {
        exec(r, s, "circuit.add", json!({"part": "HDR32", "refdes": j}));
        exec(r, s, "place.set", json!({"refdes": j, "at": [format!("{x}mm"), "15mm"]}));
    }
    let mut used = [0, 0];
    for b in &balls {
        let col: usize = b[1..].parse().unwrap();
        let side = usize::from(col > 4);
        used[side] += 1;
        let (j, pin) = (["J1", "J2"][side], used[side]);
        exec(r, s, "net.connect", json!({"net": format!("N{b}"), "pins": [format!("U1.{b}"), format!("{j}.{pin}")]}));
    }
}

#[test]
fn bga_fanout_is_drc_clean() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    bga_board(&r, &mut s);
    let f = r.execute(&mut s, "route.fanout", json!({"refdes": ["U9"]}), RunOptions::default()).expect_err("unknown");
    assert_eq!(f.error.diagnostic.code, "component.not_found");
    let o = exec(&r, &mut s, "route.fanout", json!({"refdes": ["U1"]}));
    eprintln!("{}", o["summary"]);
    // 8 x 8 with room for a track between balls: the two outer rings escape on top, the 4 x 4
    // core gets dog bones.
    assert_eq!(o["output"]["vias"], 16);
    assert_eq!(o["output"]["tracks"], 16);
    let p = s.project.as_ref().unwrap();
    let pads = cadlab::board::placed_pads(p);
    for v in &p.board().vias {
        for pp in pads.iter().filter(|pp| pp.refdes == "U1") {
            let (dx, dy) = ((v.at.x.0 - pp.center.x.0) as f64, (v.at.y.0 - pp.center.y.0) as f64);
            assert!((dx * dx + dy * dy).sqrt() > 400_000.0, "via in pad {}", pp.number);
        }
    }
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
    // Routing on top of the fanout completes the board.
    // A budget slow CI runners (debug build, Windows) cannot exhaust: the result is deterministic.
    // Default effort: with the 0.30 mm lands of the IPC-7351B table (D78; 0.32 mm before) the
    // few iterations of `low` leave one connection of 64 unrouted.
    let o = exec(&r, &mut s, "route.all", json!({"budget": "600s"}));
    report("bga64", &o, 0);
    render(&r, &mut s, "bga64");
    assert_eq!(completion(&o), 100.0);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
}

#[test]
fn shove_reroutes_blocking_nets() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    // A VIN track across the whole board between U1 and C2: 3V3 (U1.5 to C2.1) cannot pass on F.Cu.
    exec(
        &r,
        &mut s,
        "track.add",
        json!({"net": "VIN", "layer": "F.Cu", "points": [["12.5mm", "0.6mm"], ["12.5mm", "14.4mm"]]}),
    );
    let o = exec(
        &r,
        &mut s,
        "route.connection",
        json!({"from": "U1.VOUT", "to": "C2.1", "layers": ["F.Cu"], "shove": false}),
    );
    assert_eq!(o["output"]["stats"]["failed"], 1);
    assert!(o["output"]["connections"][0]["subjects"].as_array().unwrap().contains(&json!("net:VIN")), "{o}");
    let o = exec(&r, &mut s, "route.connection", json!({"from": "U1.VOUT", "to": "C2.1", "layers": ["F.Cu"]}));
    eprintln!("{}", o["summary"]);
    assert_eq!(o["output"]["connections"][0]["status"], "routed");
    assert_eq!(o["output"]["rerouted"], json!(["VIN"]));
    let b = s.project.as_ref().unwrap().board();
    assert!(!b.tracks.iter().any(|t| t.start.x == cadlab::Nm::from_um(12_500) && t.end.x == t.start.x));
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["unrouted_by_net"].get("3V3"), None);
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn shove_pushes_vias_apart() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    // A VIN wall across the board between U1 and C2 with a gap between two vias too narrow
    // for 3V3 (U1.5 to C2.1) on F.Cu: push-and-shove moves the vias apart.
    exec(
        &r,
        &mut s,
        "track.add",
        json!({"net": "VIN", "layer": "F.Cu", "points": [["12.5mm", "0.6mm"], ["12.5mm", "8.6mm"]]}),
    );
    exec(&r, &mut s, "via.add", json!({"net": "VIN", "at": ["12.5mm", "8.6mm"]}));
    exec(
        &r,
        &mut s,
        "track.add",
        json!({"net": "VIN", "layer": "F.Cu", "points": [["12.5mm", "9.4mm"], ["12.5mm", "14.4mm"]]}),
    );
    exec(&r, &mut s, "via.add", json!({"net": "VIN", "at": ["12.5mm", "9.4mm"]}));
    drc_clean(&s);
    let vin_before = unrouted(&r, &mut s, "VIN");
    let o = exec(&r, &mut s, "route.connection", json!({"from": "U1.VOUT", "to": "C2.1", "layers": ["F.Cu"]}));
    eprintln!("{}", o["summary"]);
    assert_eq!(o["output"]["connections"][0]["status"], "routed");
    assert_eq!(o["output"]["rerouted"], json!(["VIN"]));
    render(&r, &mut s, "shove-vias");
    drc_clean(&s);
    assert_eq!(unrouted(&r, &mut s, "3V3"), 0);
    assert_eq!(unrouted(&r, &mut s, "VIN"), vin_before);
    // The vias moved apart, the walls still reach the board edges.
    let b = s.project.as_ref().unwrap().board();
    assert_eq!(b.vias.len(), 2);
    let ys: Vec<i64> = b.vias.iter().map(|v| v.at.y.0).collect();
    assert!((ys[0] - ys[1]).abs() > 1_200_000, "room for the track between the vias: {ys:?}");
    for end in [cadlab::Nm::from_um(600), cadlab::Nm::from_um(14_400)] {
        assert!(b.tracks.iter().any(|t| t.net.as_deref() == Some("VIN") && (t.start.y == end || t.end.y == end)));
    }
}

#[test]
fn arc_corners_are_drc_clean_and_exported() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let dir = new_project(&r, &mut s);
    common::boards::build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "40mm", "height": "30mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1.5mm"}));
    let o = exec(&r, &mut s, "route.all", json!({"seed": 1, "arcs": true, "arc_radius": "0.8mm"}));
    report("attiny arcs", &o, 0);
    assert_eq!(completion(&o), 100.0);
    drc_clean(&s);
    render(&r, &mut s, "arcs");
    let b = s.project.as_ref().unwrap().board();
    let arcs = b.tracks.iter().filter(|t| t.mid.is_some()).count();
    assert!(arcs > 5, "{arcs} arcs");
    let total: i64 = b.tracks.iter().map(|t| cadlab::board::track_length(t).0).sum();
    // route.status measures arcs along the arc.
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["length"], json!(cadlab::Nm(total)));
    // Exported as true arcs.
    exec(&r, &mut s, "export.gerber", json!({"dir": dir.path().join("gbr")}));
    let mut circular = false;
    for e in std::fs::read_dir(dir.path().join("gbr")).unwrap() {
        let text = std::fs::read_to_string(e.unwrap().path()).unwrap_or_default();
        circular |= text.contains("Copper") && (text.contains("G02*") || text.contains("G03*"));
    }
    assert!(circular, "copper layers with circular interpolation");
    exec(&r, &mut s, "board.export_kicad", json!({"path": dir.path().join("k/board")}));
    let pcb = std::fs::read_to_string(dir.path().join("k/board.kicad_pcb")).unwrap();
    assert!(pcb.contains("(arc (start"), "KiCad arcs");
}

#[test]
fn identical_for_any_thread_count() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let res = pool.install(|| {
            let mut s = Session::new();
            let _d = new_project(&r, &mut s);
            generated_board(&r, &mut s, 4, 2, 2, 3);
            let o = exec(&r, &mut s, "route.all", json!({"seed": 5, "effort": "low"}));
            (copper(&s), o["output"].clone())
        });
        results.push(res);
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn any_angle_shortcuts() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    dense_board(&r, &mut s);
    let o = exec(&r, &mut s, "route.all", json!({"seed": 7, "any_angle": true}));
    report("dense any-angle", &o, 0);
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
    let b = s.project.as_ref().unwrap().board();
    let any = b
        .tracks
        .iter()
        .filter(|t| {
            let (dx, dy) = ((t.end.x.0 - t.start.x.0).abs(), (t.end.y.0 - t.start.y.0).abs());
            dx > 2 && dy > 2 && (dx - dy).abs() > 2
        })
        .count();
    assert!(any > 0, "some segment at another angle than 0/45/90");
}

/// A generated board: `cols` × `rows` SOIC-16s, each joined to its right neighbor by a
/// permuted 6-bit bus and to the one below by one net, plus GND and VCC on every IC.
fn generated_board(r: &Registry, s: &mut Session, cols: usize, rows: usize, layers: u8, seed: u64) {
    exec(r, s, "part.create", json!({"id": "IO16", "category": "ic", "package": "SOIC-16", "pins": numbered_pins(16)}));
    exec(r, s, "board.setup", json!({"layers": layers}));
    let (px, py) = (14.0, 12.0);
    let w = cols as f64 * px + 6.0;
    let h = rows as f64 * py + 6.0;
    exec(r, s, "board.outline", json!({"width": format!("{w}mm"), "height": format!("{h}mm")}));
    let name = |c: usize, rr: usize| format!("U{}", rr * cols + c + 1);
    for rr in 0..rows {
        for c in 0..cols {
            exec(r, s, "circuit.add", json!({"part": "IO16", "refdes": name(c, rr)}));
            let at = [format!("{}mm", 3.0 + px * (c as f64 + 0.5)), format!("{}mm", 3.0 + py * (rr as f64 + 0.5))];
            exec(r, s, "place.set", json!({"refdes": name(c, rr), "at": at}));
        }
    }
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let mut n = 0;
    for rr in 0..rows {
        for c in 0..cols {
            if c + 1 < cols {
                let mut perm: Vec<usize> = (0..6).collect();
                for i in (1..6).rev() {
                    perm.swap(i, next() % (i + 1));
                }
                for (k, p) in perm.iter().enumerate() {
                    n += 1;
                    let pins = [format!("{}.{}", name(c, rr), 10 + k), format!("{}.{}", name(c + 1, rr), 2 + p)];
                    exec(r, s, "net.connect", json!({"net": format!("N{n}"), "pins": pins}));
                }
            }
            if rr + 1 < rows {
                n += 1;
                let pins = [format!("{}.1", name(c, rr)), format!("{}.9", name(c, rr + 1))];
                exec(r, s, "net.connect", json!({"net": format!("N{n}"), "pins": pins}));
            }
        }
    }
    let all = |pin: usize| -> Vec<String> {
        (0..rows)
            .flat_map(|rr| (0..cols).map(move |c| (c, rr)))
            .map(|(c, rr)| format!("{}.{pin}", name(c, rr)))
            .collect()
    };
    exec(r, s, "net.connect", json!({"net": "GND", "pins": all(8)}));
    exec(r, s, "net.connect", json!({"net": "VCC", "pins": all(16)}));
}

type Builder = Box<dyn Fn(&Registry, &mut Session)>;

/// Router benchmark on generated boards (release build recommended):
/// `cargo test --release --test route -- --ignored --nocapture`. Results go in docs/ROUTER.md.
#[test]
#[ignore]
fn bench_generated_boards() {
    let r = Registry::with_builtins();
    eprintln!("| board | layers | nets | connections | completion | vias | length | iterations | time |");
    eprintln!("|---|---|---|---|---|---|---|---|---|");
    let mut cases: Vec<(String, Builder, u8)> = vec![
        ("ldo (3 parts)".into(), Box::new(ldo_board), 2),
        ("dense (LQFP-32, SOIC-16, 2x8)".into(), Box::new(dense_board), 2),
    ];
    for (c, rr, l) in [(3, 2, 2), (4, 3, 2), (6, 4, 2), (6, 4, 4)] {
        cases.push((
            format!("{}x SOIC-16 grid", c * rr),
            Box::new(move |r: &Registry, s: &mut Session| generated_board(r, s, c, rr, l, 1)),
            l,
        ));
    }
    for (name, build, layers) in cases {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        build(&r, &mut s);
        let nets = s.project.as_ref().unwrap().circuit().nets.len();
        let t = Instant::now();
        let o = exec(&r, &mut s, "route.all", json!({"budget_ms": 120000}));
        let ms = t.elapsed().as_millis();
        let st = &o["output"]["stats"];
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new(), "{name}");
        eprintln!(
            "| {name} | {layers} | {nets} | {} | {}% | {} | {} | {} | {ms} ms |",
            st["connections"],
            st["completion"],
            st["vias"],
            st["length"].as_str().unwrap(),
            st["iterations"]
        );
        render(&r, &mut s, &format!("bench-{}", name.split_whitespace().next().unwrap()));
    }
}

/// Ratsnest lines of a net.
fn unrouted(r: &Registry, s: &mut Session, net: &str) -> u64 {
    let st = exec(r, s, "route.status", json!({}));
    st["output"]["unrouted_by_net"].get(net).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn drc_clean(s: &Session) {
    let errors: Vec<String> = drc_errors(s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn route_track_shoves_and_walks_around() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    // A free VIN track along y = 3 mm, then a GND track just above it: VIN is pushed down.
    exec(&r, &mut s, "track.add", json!({"net": "VIN", "layer": "F.Cu", "points": [["3mm", "3mm"], ["17mm", "3mm"]]}));
    let vin_before = unrouted(&r, &mut s, "VIN");
    let strict = r
        .execute(
            &mut s,
            "route.track",
            json!({"net": "GND", "layer": "F.Cu", "points": [["6mm", "3.1mm"], ["14mm", "3.1mm"]], "mode": "strict"}),
            RunOptions::default(),
        )
        .unwrap_err();
    assert_eq!(strict.error.diagnostic.code, "route.track_blocked", "{}", strict.error);
    assert!(strict.error.diagnostic.subjects.contains(&cadlab::ObjectRef::Net("VIN".into())), "{}", strict.error);
    let o = exec(
        &r,
        &mut s,
        "route.track",
        json!({"net": "GND", "layer": "F.Cu", "points": [["6mm", "3.1mm"], ["14mm", "3.1mm"]]}),
    );
    eprintln!("{}", o["summary"]);
    assert_eq!(o["output"]["shoved"], json!(["VIN"]));
    assert_eq!(o["output"]["segments"], 1, "the GND track stays where it was asked");
    drc_clean(&s);
    assert_eq!(unrouted(&r, &mut s, "VIN"), vin_before, "the shoved track keeps its connections");
    let b = s.project.as_ref().unwrap().board();
    let vin: Vec<_> = b.tracks.iter().filter(|t| t.net.as_deref() == Some("VIN")).collect();
    assert!(vin.len() > 1, "VIN was bent around the GND track");
    assert!(vin.iter().any(|t| t.start.y < cadlab::Nm::from_um(3_000)), "pushed downwards");
    render(&r, &mut s, "route-track-shove");
    // Undo, lock VIN: shove mode now walks GND around it instead (VIN never moves).
    exec(&r, &mut s, "history.undo", json!({}));
    for t in s.project.as_mut().unwrap().board_mut().tracks.iter_mut() {
        if t.net.as_deref() == Some("VIN") {
            t.locked = true;
        }
    }
    let locked: Vec<_> = s.project.as_ref().unwrap().board().tracks.clone();
    let err = r
        .execute(
            &mut s,
            "route.track",
            json!({"net": "GND", "layer": "F.Cu", "points": [["6mm", "3.1mm"], ["14mm", "3.1mm"]]}),
            RunOptions::default(),
        )
        .unwrap_err();
    eprintln!("{}", err.error);
    assert_eq!(err.error.diagnostic.code, "route.track_blocked");
    assert!(err.error.diagnostic.hint.as_deref().unwrap().contains("unlock"), "{}", err.error);
    // Ends clear of it: the GND track walks around the locked VIN track's end.
    let o = exec(
        &r,
        &mut s,
        "route.track",
        json!({"net": "GND", "layer": "F.Cu", "points": [["14mm", "3.6mm"], ["18mm", "2.4mm"]]}),
    );
    eprintln!("{}", o["summary"]);
    assert_eq!(o["output"]["shoved"], json!(null));
    assert_eq!(o["output"]["walked"], true);
    let b = s.project.as_ref().unwrap().board();
    for t in locked.iter().filter(|t| t.locked) {
        assert!(b.tracks.contains(t), "locked tracks never move");
    }
    drc_clean(&s);
    // Walkaround: a track crossing pads of other nets detours around them.
    let o = exec(
        &r,
        &mut s,
        "route.track",
        json!({"net": "GND", "layer": "F.Cu", "points": [["2mm", "9mm"], ["8mm", "9mm"]], "mode": "walkaround"}),
    );
    eprintln!("{}", o["summary"]);
    assert_eq!(o["output"]["walked"], true);
    drc_clean(&s);
    render(&r, &mut s, "route-track-walk");
}

/// The LQFP-48 board of [`lqfp_board`] with the MCU turned by `rotation` degrees: at 30° no pad
/// lies on the routing grid or along a grid line.
fn rotated_lqfp_board(r: &Registry, s: &mut Session, rotation: i32) {
    lqfp_board(r, s);
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["30mm", "30mm"], "rotation": rotation}));
}

#[test]
fn gridless_search_reaches_a_rotated_part() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for router in ["grid", "auto"] {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        rotated_lqfp_board(&r, &mut s, 30);
        let o = exec(&r, &mut s, "route.all", json!({"router": router, "seed": 1}));
        report(&format!("lqfp48 at 30° router={router}"), &o, 0);
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new(), "{router}");
        results.push(completion(&o));
    }
    assert!(results[1] >= results[0], "auto is never worse than the grid: {results:?}");
    assert_eq!(results[1], 100.0, "{results:?}");
}

#[test]
fn gridless_search_is_identical_for_any_thread_count() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let res = pool.install(|| {
            let mut s = Session::new();
            let _d = new_project(&r, &mut s);
            generated_board(&r, &mut s, 3, 2, 2, 3);
            let o = exec(&r, &mut s, "route.all", json!({"seed": 5, "router": "gridless"}));
            report(&format!("gridless {threads} threads"), &o, 0);
            let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
            assert_eq!(errors, Vec::<String>::new());
            (copper(&s), o["output"].clone())
        });
        results.push(res);
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0].1["stats"]["completion"], 100.0);
}

/// A QFN-40 (0.4 mm pitch) whose pins go to four 1x10 headers on 4 layers, with 0.25 mm tracks and
/// 0.19 mm clearance: the class width does not fit between the pads (0.2 mm minimum width).
fn qfn_neck_board(r: &Registry, s: &mut Session) {
    exec(
        r,
        s,
        "part.create",
        json!({"id": "Q40", "category": "ic", "package": "QFN-40 5x5mm P0.4mm EP3.5mm", "pins": numbered_pins(41)}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"id": "HDR10", "category": "connector", "package": "PinHeader 1x10", "pins": numbered_pins(10)}),
    );
    exec(r, s, "board.rules", json!({"track_width": "0.25mm", "clearance": "0.19mm", "min_track_width": "0.2mm"}));
    exec(r, s, "board.setup", json!({"layers": 4}));
    exec(r, s, "circuit.add", json!({"part": "Q40", "refdes": "U1"}));
    exec(r, s, "board.outline", json!({"width": "50mm", "height": "50mm"}));
    exec(r, s, "place.set", json!({"refdes": "U1", "at": ["25mm", "25mm"]}));
    for (k, (x, y, rot)) in [(6, 25, 0), (25, 6, 90), (44, 25, 0), (25, 44, 90)].iter().enumerate() {
        let j = format!("J{}", k + 1);
        exec(r, s, "circuit.add", json!({"part": "HDR10", "refdes": j}));
        exec(r, s, "place.set", json!({"refdes": j, "at": [format!("{x}mm"), format!("{y}mm")], "rotation": rot}));
        for i in 1..=10 {
            let pin = k * 10 + i;
            exec(
                r,
                s,
                "net.connect",
                json!({"net": format!("P{pin}"), "pins": [format!("U1.{pin}"), format!("{j}.{i}")]}),
            );
        }
    }
}

#[test]
fn neck_down_out_of_a_fine_pitch_qfn() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for neck in [false, true] {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        qfn_neck_board(&r, &mut s);
        let o = exec(&r, &mut s, "route.all", json!({"neck": neck, "seed": 1}));
        report(&format!("qfn40 neck={neck}"), &o, 0);
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new());
        let b = s.project.as_ref().unwrap().board();
        let narrow = b.tracks.iter().filter(|t| t.width < Nm::from_um(250)).count();
        let wide = b.tracks.iter().filter(|t| t.width == Nm::from_um(250)).count();
        assert!(b.tracks.iter().all(|t| t.width >= Nm::from_um(200)), "never under min_track_width");
        results.push((completion(&o), narrow, wide));
    }
    assert_eq!(results[0].1, 0, "no neck-downs when off: {results:?}");
    assert!(results[0].0 < 50.0, "the class width does not leave the pads: {results:?}");
    assert!(results[1].0 >= 70.0, "necked down, most pins get out: {results:?}");
    assert!(results[1].1 > 0 && results[1].2 > 0, "necked near the QFN, widened elsewhere: {results:?}");
}

/// A USB-C-like row of 0.5 mm-pitch pads (a shield pad above it) whose D+ and D− pads interleave
/// (`D+ D− D+ D−`), both nets also going to a header below: on two layers one net must change
/// layer under the row. The crossover planning gives the inner pad of one net a via with room
/// for the other net's pads to get past it, and the net's other pad a via of its own.
#[test]
fn usb_c_crossover_routes() {
    let r = Registry::with_builtins();
    let mut pins = numbered_pins(8);
    pins.as_array_mut().unwrap().push(json!({"number": "S1", "name": "SHIELD", "kind": "passive"}));
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(
        &r,
        &mut s,
        "footprint.create",
        json!({
            "name": "ROW8",
            "pads": [{"number": "1", "at": ["-1.75mm", "0mm"], "size": ["0.3mm", "0.8mm"], "shape": "rect",
                      "count": 8, "pitch": ["0.5mm", "0mm"]},
                     {"number": "S1", "at": ["0mm", "1.1mm"], "size": ["4.4mm", "0.6mm"], "shape": "rect"}]
        }),
    );
    exec(&r, &mut s, "part.create", json!({"id": "ROW8", "category": "connector", "footprint": "ROW8", "pins": pins}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "HDR2", "category": "connector", "package": "PinHeader 1x2", "pins": numbered_pins(2)}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "ROW8", "refdes": "J1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "HDR2", "refdes": "J2"}));
    exec(&r, &mut s, "net.connect", json!({"net": "DP", "pins": ["J1.3", "J1.5", "J2.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "DM", "pins": ["J1.4", "J1.6", "J2.2"]}));
    // The shield and the outer pins on a net of their own, already joined (the shield is part of
    // the footprint's copper on real receptacles).
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["J1.S1"]}));
    // USB tracks of 0.2 mm at 0.2 mm clearance, as on small USB-C boards.
    exec(&r, &mut s, "netclass.set", json!({"name": "usb", "track_width": "0.2mm", "clearance": "0.2mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["DP", "DM"], "class": "usb"}));
    exec(&r, &mut s, "board.outline", json!({"width": "16mm", "height": "12mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["8mm", "10mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J2", "at": ["8mm", "4mm"], "rotation": 90}));
    let o = exec(&r, &mut s, "route.all", json!({}));
    report("usb-c crossover", &o, 0);
    render(&r, &mut s, "usb_c_crossover");
    assert_eq!(completion(&o), 100.0, "{o}");
    assert!(o["output"]["stats"]["fanout_vias"].as_u64().unwrap() >= 1, "{o}");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
}

/// Scoped clearance rules (D57's open item): a QFN-40 at 0.4 mm pitch with a 0.25 mm class
/// clearance leaves no way out of its pads, not even necked down to the 0.2 mm minimum width;
/// custom rules allowing 0.15 mm for the QFN's pads and for tracks in its courtyard (as on
/// tinytapeout-demo) let the router out, and the result is DRC-clean under the full rule set.
#[test]
fn scoped_rules_let_tracks_out_of_a_fine_pitch_qfn() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for rules in [false, true] {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        qfn_neck_board(&r, &mut s);
        exec(
            &r,
            &mut s,
            "board.rules",
            json!({"track_width": "0.2mm", "clearance": "0.25mm", "min_clearance": "0.1mm"}),
        );
        if rules {
            exec(
                &r,
                &mut s,
                "board.custom_rule",
                json!({"name": "qfn tracks", "in_courtyard": "U1", "kinds": ["track", "via"], "clearance": "0.15mm"}),
            );
            exec(
                &r,
                &mut s,
                "board.custom_rule",
                json!({"name": "qfn pads", "kinds": ["pad"], "footprint": "U1", "clearance": "0.15mm"}),
            );
        }
        let o = exec(&r, &mut s, "route.all", json!({"seed": 1}));
        report(&format!("qfn40 scoped rules={rules}"), &o, 0);
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new(), "rules={rules}");
        assert_eq!(o["output"]["stats"]["drc_removed"], 0, "rules={rules}: {}", o["output"]["stats"]);
        results.push(completion(&o));
    }
    assert!(results[0] < 20.0, "the class clearance walls the pads in: {results:?}");
    assert!(results[1] >= 70.0, "the scoped rules let them out: {results:?}");
}

/// A scoped rule asking for more than the class (a high-voltage area, entirely-inside scope)
/// and the board's hole clearance from plated holes: the router keeps both, so the DRC removes
/// nothing; tracks crossing the area's boundary are split there, so the DRC decides each piece
/// as the router did.
#[test]
fn scoped_rules_asking_for_more_are_kept() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    generated_board(&r, &mut s, 3, 2, 2, 7);
    exec(&r, &mut s, "board.rules", json!({"hole_clearance": "0.35mm"}));
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "hv", "outline": {"rect": {"from": ["0mm", "0mm"], "to": ["24mm", "40mm"]}}, "rule_area": true}),
    );
    exec(&r, &mut s, "board.custom_rule", json!({"name": "hv", "in_area": "hv", "clearance": "0.4mm"}));
    let o = exec(&r, &mut s, "route.all", json!({"seed": 1}));
    report("scoped rules asking for more", &o, 0);
    let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(o["output"]["stats"]["drc_removed"], 0, "{}", o["output"]["stats"]);
    assert!(completion(&o) >= 90.0, "{}", o["output"]["stats"]);
    assert!(o["output"]["stats"]["vias"].as_u64().unwrap() > 0, "vias keep the hole clearance");
    // Tracks are split at the area's boundary: none reaches from well inside to well outside.
    let b = s.project.as_ref().unwrap().board();
    let x = |p: cadlab::Point| p.x.0 as f64 / 1e6;
    assert!(
        b.tracks.iter().all(|t| !(x(t.start).min(x(t.end)) < 23.0 && x(t.start).max(x(t.end)) > 25.0)),
        "a track crosses the area's boundary unsplit"
    );
}

/// Net exceptions of a keep-out (D76): the wall of `failures_explain_what_blocks` lets the
/// excepted nets through, in every search (grid, gridless rooms, with and without the gridless
/// refinement) and in `route.track`, and the DRC agrees; every other net still stops at it.
#[test]
fn keepout_net_exceptions() {
    let r = Registry::with_builtins();
    let wall = |allow: &[&str]| {
        json!({"name": "wall", "outline": {"rect": {"from": ["12.5mm", "-1mm"], "to": ["13.5mm", "16mm"]}},
               "no_tracks": true, "no_vias": true, "allow_nets": allow})
    };
    let failed = |o: &Value| -> Vec<(String, String, String)> {
        o["output"]["connections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["status"] == "failed")
            .map(|c| {
                let hints: Vec<&str> = c["hints"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
                (c["net"].as_str().unwrap().to_string(), c["reason"].as_str().unwrap().to_string(), hints.join("; "))
            })
            .collect()
    };
    let crossing = |s: &Session, net: &str| {
        let x = |p: cadlab::Point| p.x.0 as f64 / 1e6;
        s.project.as_ref().unwrap().board().tracks.iter().any(|t| {
            t.net.as_deref() == Some(net) && x(t.start).min(x(t.end)) < 13.5 && x(t.start).max(x(t.end)) > 12.5
        })
    };
    for (router, gridless) in [("auto", true), ("grid", false), ("grid", true), ("gridless", true)] {
        let what = format!("router {router}, gridless {gridless}");
        let opts = json!({"router": router, "gridless": gridless});
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        ldo_board(&r, &mut s);
        // One excepted net: 3V3 crosses, GND does not.
        let o = exec(&r, &mut s, "keepout.add", wall(&["3V3"]));
        assert_eq!(o["output"]["keepouts"][0]["allow_nets"], json!(["3V3"]));
        let o = exec(&r, &mut s, "route.all", opts.clone());
        report(&format!("wall except 3V3 ({what})"), &o, 0);
        let f = failed(&o);
        assert_eq!(f.len(), 1, "{what}: {f:?}");
        assert_eq!(f[0].0, "GND", "{what}");
        assert!(f[0].1.contains("keep-out `wall`"), "{what}: {}", f[0].1);
        assert!(f[0].2.contains("except net GND") && f[0].2.contains("keepout.set"), "{what}: {}", f[0].2);
        assert!(crossing(&s, "3V3"), "{what}: 3V3 goes through the wall");
        assert!(!crossing(&s, "GND"), "{what}: GND stays out of it");
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new(), "{what}");
        // Two excepted nets (points only a set of nets may use): everything routes.
        exec(&r, &mut s, "keepout.set", json!({"name": "wall", "allow_nets": ["GND"]}));
        let o = exec(&r, &mut s, "route.all", opts.clone());
        report(&format!("wall except 3V3 and GND ({what})"), &o, 0);
        assert_eq!(failed(&o), vec![], "{what}");
        assert_eq!(completion(&o), 100.0, "{what}");
        assert!(crossing(&s, "GND"), "{what}");
        assert_eq!(drc_errors(&s), Vec::<String>::new(), "{what}");
        // Without the exceptions the same copper is in the keep-out.
        exec(&r, &mut s, "keepout.set", json!({"name": "wall", "forbid_nets": ["3V3", "GND"]}));
        let errors = drc_errors(&s);
        assert!(!errors.is_empty() && errors.iter().all(|e| e.starts_with("drc.keepout")), "{what}: {errors:?}");
    }

    // Manual routing: a straight track through the wall, refused for VIN, placed for 3V3.
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    exec(&r, &mut s, "keepout.add", wall(&["3V3"]));
    let track = |net: &str, mode: &str| json!({"layer": "B.Cu", "net": net, "mode": mode, "points": [["11mm", "2mm"], ["15mm", "2mm"]]});
    for mode in ["strict", "walkaround", "shove"] {
        let f = r.execute(&mut s, "route.track", track("VIN", mode), RunOptions::default()).expect_err("blocked");
        assert_eq!(f.error.diagnostic.code, "route.track_blocked", "{mode}: {}", f.error);
        assert!(f.error.diagnostic.message.contains("keep-out `wall`"), "{mode}: {}", f.error);
    }
    assert!(s.project.as_ref().unwrap().board().tracks.is_empty());
    let o = exec(&r, &mut s, "route.track", track("3V3", "strict"));
    assert_eq!((o["output"]["segments"].as_u64(), o["output"]["walked"].as_bool()), (Some(1), Some(false)));
    assert!(drc_errors(&s).iter().all(|e| !e.starts_with("drc.keepout")));
}

/// Pours are filled again around new copper (D80). Six tracks between two rows of tall pads cut
/// the top-layer GND pour into strips closed at both ends, each holding the GND pad of one
/// capacitor: on the empty board the pour joins the three pads, so nothing is planned for GND.
/// The router sees the cut, plans GND again from what the fill still joins and routes it.
#[test]
fn pads_cut_off_from_their_pour_are_routed() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(
        &r,
        &mut s,
        "footprint.create",
        json!({
            "name": "WALL6",
            "pads": [{"number": "1", "at": ["0mm", "-5mm"], "size": ["1.2mm", "1.7mm"], "shape": "rect",
                      "count": 6, "pitch": ["0mm", "2mm"]}]
        }),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "WALL6", "category": "connector", "footprint": "WALL6", "pins": numbered_pins(6)}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "WALL6", "refdes": "J1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "WALL6", "refdes": "J2"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 3}));
    for i in 1..=6 {
        let mut pins = vec![format!("J1.{i}"), format!("J2.{i}")];
        if (2..=4).contains(&i) {
            pins.push(format!("C{}.1", i - 1));
        }
        exec(&r, &mut s, "net.connect", json!({"net": format!("S{i}"), "pins": pins}));
    }
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["C1.2", "C2.2", "C3.2"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "30mm", "height": "16mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["4mm", "8mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J2", "at": ["26mm", "8mm"]}));
    // Each capacitor between two tracks: pad 1 beside its own track, pad 2 (GND) in the strip.
    for (i, x) in [(1, "10mm"), (2, "15mm"), (3, "20mm")] {
        let y = format!("{}mm", 4 + 2 * i);
        exec(&r, &mut s, "place.set", json!({"refdes": format!("C{i}"), "at": [x, y]}));
    }
    exec(&r, &mut s, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["F.Cu"], "outline": "board"}));
    assert_eq!(unrouted(&r, &mut s, "GND"), 0, "the pour joins the GND pads of the empty board");
    let o = exec(&r, &mut s, "route.all", json!({"seed": 1}));
    report("pour cut", &o, 0);
    render(&r, &mut s, "pour_cut");
    assert_eq!(o["output"]["stats"]["failed"], 0, "{o}");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    // The pour alone no longer joins them: without GND's own routing they are apart.
    exec(&r, &mut s, "route.rip", json!({"nets": ["GND"]}));
    assert!(unrouted(&r, &mut s, "GND") >= 1, "the signal tracks were expected to cut the pour");
}

/// A USB-C-like row whose configuration pad sits between a no-connect pad and the pad whose
/// exit turns towards it around the crossover via (corne-cherry's CC1): it has to turn away
/// before that exit does, by less than the way to the next grid line, and at some offsets of
/// the row against the grid (0.12 mm here) no line lies in what is left. The pad then turns by
/// what it needs and reaches a cell further out, so it has an exit at every offset that needs
/// one (D84).
#[test]
fn pad_beside_a_crossover_exit_gets_out_at_any_grid_offset() {
    let r = Registry::with_builtins();
    for step in 2..=8 {
        let mut pins = numbered_pins(9);
        pins.as_array_mut().unwrap().push(json!({"number": "S1", "name": "SHIELD", "kind": "passive"}));
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        exec(
            &r,
            &mut s,
            "footprint.create",
            json!({
                "name": "ROW9",
                "pads": [{"number": "1", "at": ["-2mm", "0mm"], "size": ["0.3mm", "1.3mm"], "shape": "rect",
                          "count": 9, "pitch": ["0.5mm", "0mm"]},
                         {"number": "S1", "at": ["0mm", "1.4mm"], "size": ["4.9mm", "0.6mm"], "shape": "rect"}]
            }),
        );
        exec(
            &r,
            &mut s,
            "part.create",
            json!({"id": "ROW9", "category": "connector", "footprint": "ROW9", "pins": pins}),
        );
        exec(
            &r,
            &mut s,
            "part.create",
            json!({"id": "HDR8", "category": "connector", "package": "PinHeader 1x8", "pins": numbered_pins(8)}),
        );
        exec(&r, &mut s, "circuit.add", json!({"part": "ROW9", "refdes": "J1"}));
        exec(&r, &mut s, "circuit.add", json!({"part": "HDR8", "refdes": "J2"}));
        // Pads 2 and 8 stay unconnected; 3 is the configuration pad beside the crossing 4 to 7.
        exec(&r, &mut s, "net.connect", json!({"net": "VB", "pins": ["J1.1", "J2.1"]}));
        exec(&r, &mut s, "net.connect", json!({"net": "CC", "pins": ["J1.3", "J2.2"]}));
        exec(&r, &mut s, "net.connect", json!({"net": "DB", "pins": ["J1.4", "J1.6", "J2.3"]}));
        exec(&r, &mut s, "net.connect", json!({"net": "DA", "pins": ["J1.5", "J1.7", "J2.4"]}));
        // The header's pads share one line of the grid: they set its offset, not the row.
        exec(&r, &mut s, "net.connect", json!({"net": "E1", "pins": ["J2.5", "J2.6"]}));
        exec(&r, &mut s, "net.connect", json!({"net": "E2", "pins": ["J2.7", "J2.8"]}));
        exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["J1.S1"]}));
        exec(&r, &mut s, "board.rules", json!({"track_width": "0.2mm", "clearance": "0.2mm"}));
        exec(&r, &mut s, "netclass.set", json!({"name": "usb", "track_width": "0.25mm", "clearance": "0.2mm"}));
        exec(&r, &mut s, "net.set", json!({"nets": ["VB", "CC", "DA", "DB"], "class": "usb"}));
        exec(&r, &mut s, "board.outline", json!({"width": "30mm", "height": "26mm"}));
        let x = format!("{}um", 12_000 + 20 * step);
        exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": [x, "22mm"]}));
        exec(&r, &mut s, "place.set", json!({"refdes": "J2", "at": ["24mm", "13mm"]}));
        let f = cadlab::router::fanout(s.project.as_ref().unwrap(), &[], &[]).unwrap();
        assert!(f.pads.iter().any(|p| p == "J1.3"), "offset {} um: exits for {:?}", 20 * step, f.pads);
        let o = exec(&r, &mut s, "route.all", json!({"seed": 1}));
        report(&format!("row {} um off", 20 * step), &o, 0);
        assert_eq!(completion(&o), 100.0, "offset {} um: {o}", 20 * step);
        assert_eq!(drc_errors(&s), Vec::<String>::new(), "offset {} um", 20 * step);
    }
}

/// A QFN-40 (0.4 mm pitch, twice the 0.2 mm grid pitch) placed 0.09 mm and 0.07 mm off the
/// positions of four 1x12 headers, which have more pads: the grid's lines follow the QFN's rows,
/// whose pads would each have to turn onto a line otherwise (of tinytapeout-demo's QFN-80, 10
/// of 75 pads then got no exit), and the headers' pads are reached by stubs. The QFN needs no
/// escapes there, as where it shares the headers' lines (D84).
#[test]
fn grid_lines_follow_the_finest_rows() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "Q40", "category": "ic", "package": "QFN-40 5x5mm P0.4mm EP3.5mm", "pins": numbered_pins(41)}),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "HDR12", "category": "connector", "package": "PinHeader 1x12", "pins": numbered_pins(12)}),
    );
    exec(&r, &mut s, "board.rules", json!({"track_width": "0.2mm", "clearance": "0.18mm", "min_track_width": "0.2mm"}));
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    exec(&r, &mut s, "circuit.add", json!({"part": "Q40", "refdes": "U1"}));
    exec(&r, &mut s, "board.outline", json!({"width": "50mm", "height": "50mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["25.09mm", "25.07mm"]}));
    for (k, (x, y, rot)) in [(6, 25, 0), (25, 6, 90), (44, 25, 0), (25, 44, 90)].iter().enumerate() {
        let j = format!("J{}", k + 1);
        exec(&r, &mut s, "circuit.add", json!({"part": "HDR12", "refdes": j}));
        exec(
            &r,
            &mut s,
            "place.set",
            json!({"refdes": j, "at": [format!("{x}mm"), format!("{y}mm")], "rotation": rot}),
        );
        for i in 1..=10 {
            let pin = k * 10 + i;
            exec(
                &r,
                &mut s,
                "net.connect",
                json!({"net": format!("P{pin}"), "pins": [format!("U1.{pin}"), format!("{j}.{i}")]}),
            );
        }
        exec(
            &r,
            &mut s,
            "net.connect",
            json!({"net": format!("X{k}"), "pins": [format!("{j}.11"), format!("{j}.12")]}),
        );
    }
    let f = cadlab::router::fanout(s.project.as_ref().unwrap(), &["U1".to_string()], &[]).unwrap();
    assert!(f.tracks.is_empty(), "every pad of the QFN is on a grid line: escapes for {:?}", f.pads);
    // The same part where the headers' pads are (their lines were the grid's before): unchanged.
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["25mm", "25mm"]}));
    let f = cadlab::router::fanout(s.project.as_ref().unwrap(), &["U1".to_string()], &[]).unwrap();
    assert!(f.tracks.is_empty(), "{:?}", f.pads);
}

/// Two pads that only join through a slot between two keep-outs, 0.24 mm wide for a 0.2 mm
/// track and 0.07 mm off the lines of the default 0.2 mm grid: the grid search finds no way at
/// that pitch. A run that leaves connections unrouted inside its budget is made again on finer
/// grids, one of which has a line in the slot (D84). An explicit `grid` is used as given.
#[test]
fn finer_grids_after_a_run_that_leaves_connections() {
    let r = Registry::with_builtins();
    let board = |s: &mut Session| {
        let _d = new_project(&r, s);
        exec(
            &r,
            s,
            "part.create",
            json!({"id": "HDR1", "category": "connector", "package": "PinHeader 1x1", "pins": numbered_pins(1)}),
        );
        exec(&r, s, "circuit.add", json!({"part": "HDR1", "refdes": "J1"}));
        exec(&r, s, "circuit.add", json!({"part": "HDR1", "refdes": "J2"}));
        exec(&r, s, "net.connect", json!({"net": "N", "pins": ["J1.1", "J2.1"]}));
        exec(&r, s, "board.rules", json!({"track_width": "0.2mm", "clearance": "0.2mm"}));
        exec(&r, s, "board.outline", json!({"width": "20mm", "height": "12mm"}));
        exec(&r, s, "place.set", json!({"refdes": "J1", "at": ["10mm", "3mm"]}));
        exec(&r, s, "place.set", json!({"refdes": "J2", "at": ["10mm", "9mm"]}));
        // A wall across the board between the two pads, open from x = 9.95 to 10.19 mm.
        for (name, from, to) in [("left", "-1mm", "9.95mm"), ("right", "10.19mm", "21mm")] {
            exec(
                &r,
                s,
                "keepout.add",
                json!({"name": name, "outline": {"rect": {"from": [from, "5.5mm"], "to": [to, "6.5mm"]}},
                       "no_tracks": true, "no_vias": true}),
            );
        }
        _d
    };
    use cadlab::router::{Hooks, RouteOptions, Scope, SearchKind, route};
    let mut s = Session::new();
    let _d = board(&mut s);
    let p = s.project.as_ref().unwrap();
    let hooks = Hooks { progress: &|_, _, _| {}, cancelled: &|| false };
    let grid_only = RouteOptions { search: Some(SearchKind::Grid), ..Default::default() };
    let fixed = RouteOptions { grid: Some(Nm::from_um(200)), ..grid_only.clone() };
    let o = route(p, &Scope::All, &fixed, &hooks).unwrap();
    assert_eq!(o.stats.failed, 1, "no line of the 0.2 mm grid lies in the slot: {:?}", o.stats);
    assert_eq!(o.stats.grid, Nm::from_um(200));
    let o = route(p, &Scope::All, &grid_only, &hooks).unwrap();
    assert_eq!(o.stats.failed, 0, "{:?}", o.stats);
    assert!(o.stats.grid < Nm::from_um(200), "routed on a finer grid: {:?}", o.stats);
    let o = exec(&r, &mut s, "route.all", json!({"router": "grid"}));
    report("slot off the grid", &o, 0);
    assert_eq!(o["output"]["stats"]["failed"], 0, "{o}");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
}

// ---- `router: guide` (D85, D88) ----------------------------------------------------------

fn diag_codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

fn other_errors(s: &Session) -> Vec<String> {
    drc_errors(s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect()
}

/// The guide crate routes a two-layer board through `route.all`: complete, clean by cadlab's
/// DRC, with stats in the native router's shape and progress from guide's phases.
#[test]
fn guide_routes_two_layer_boards() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    let progress = Collect(Default::default());
    let out = r
        .execute_with(
            &mut s,
            "route.all",
            json!({"router": "guide"}),
            RunOptions::default(),
            &progress,
            &cadlab::command::CancelToken::new(),
        )
        .unwrap();
    let o = serde_json::to_value(&out).unwrap();
    report("guide ldo", &o, 0);
    render(&r, &mut s, "guide_ldo");
    let st = &o["output"]["stats"];
    assert_eq!((st["connections"].as_u64(), st["routed"].as_u64(), st["failed"].as_u64()), (Some(5), Some(5), Some(0)));
    assert_eq!(st["completion"], 100.0);
    assert_eq!(st["tracks"].as_u64().unwrap() as usize, s.project.as_ref().unwrap().board().tracks.len());
    assert_eq!(st["vias"].as_u64().unwrap() as usize, s.project.as_ref().unwrap().board().vias.len());
    assert_eq!(st["grid"], "0.225mm");
    assert_eq!((st["drc_removed"].as_u64(), st["budget_exhausted"].as_bool()), (Some(0), Some(false)));
    assert!(st["work"].as_u64().unwrap() > 0, "guide's expanded nodes: {st}");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(diag_codes(&o), Vec::<String>::new());
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["unrouted"], 0);
    let msgs = progress.0.into_inner().unwrap();
    assert!(
        msgs.iter().any(|m| m.starts_with("guide: routing, ")) && msgs.iter().any(|m| m == "verifying"),
        "{msgs:?}"
    );
    assert_eq!(msgs.last().map(String::as_str), Some("done"));
    // Nothing left: nothing attempted, nothing added.
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    assert_eq!(
        (o["output"]["stats"]["connections"].as_u64(), o["output"]["stats"]["tracks"].as_u64()),
        (Some(0), Some(0))
    );

    // The ATtiny85 board, auto-placed.
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    common::boards::build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "40mm", "height": "30mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1.5mm"}));
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide attiny", &o, 0);
    render(&r, &mut s, "guide_attiny");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
}

/// A four-layer board (SOIC-16s with permuted buses, GND and VCC on every part).
#[test]
fn guide_routes_a_four_layer_board() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    generated_board(&r, &mut s, 2, 2, 4, 3);
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide 4 layers", &o, 0);
    render(&r, &mut s, "guide_4l");
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    let b = s.project.as_ref().unwrap().board();
    assert!(b.vias.iter().all(|v| (v.from.as_str(), v.to.as_str()) == ("F.Cu", "B.Cu")), "through vias only");
    // No via in a pad, as with the native router: guide places them on its own (40 of the 148
    // vias of the benchmark's soic24-2l) unless told not to (`vias_in_pads`), which the adapter
    // does.
    assert!(!b.vias.is_empty());
    let pads = cadlab::board::placed_pads(s.project.as_ref().unwrap());
    for v in &b.vias {
        let inside = pads
            .iter()
            .filter_map(|pp| Some((pp, pp.shape.bbox()?)))
            .find(|(_, bb)| (bb.min.x..=bb.max.x).contains(&v.at.x.0) && (bb.min.y..=bb.max.y).contains(&v.at.y.0));
        assert!(inside.is_none(), "via#{} is in pad {}.{}", v.id.0, inside.unwrap().0.refdes, inside.unwrap().0.number);
    }
}

/// Keep-outs: guide routes around one, and takes the nets a keep-out excepts (D76) through it.
#[test]
fn guide_keeps_out_of_keepouts() {
    let r = Registry::with_builtins();
    let crossing = |s: &Session, x0: f64, x1: f64, y0: f64, y1: f64| {
        let mm = |v: Nm| v.0 as f64 / 1e6;
        s.project.as_ref().unwrap().board().tracks.iter().any(|t| {
            let (xa, xb) = (mm(t.start.x).min(mm(t.end.x)), mm(t.start.x).max(mm(t.end.x)));
            let (ya, yb) = (mm(t.start.y).min(mm(t.end.y)), mm(t.start.y).max(mm(t.end.y)));
            xa < x1 && xb > x0 && ya < y1 && yb > y0
        })
    };
    // A keep-out between U1 and C2 on both layers that leaves a way around.
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "block", "outline": {"rect": {"from": ["12.5mm", "4mm"], "to": ["13.5mm", "11mm"]}},
               "no_tracks": true, "no_vias": true}),
    );
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide around a keep-out", &o, 0);
    render(&r, &mut s, "guide_keepout");
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    assert!(!crossing(&s, 12.5, 13.5, 4.0, 11.0));

    // A wall across the board excepting 3V3: guide takes 3V3 through it, as the native router
    // does (`keepout_net_exceptions`), and leaves GND, which is not excepted, open there, with
    // what it knows of why.
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "wall", "outline": {"rect": {"from": ["12.5mm", "-1mm"], "to": ["13.5mm", "16mm"]}},
               "no_tracks": true, "no_vias": true, "allow_nets": ["3V3"]}),
    );
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide at a wall with an exception", &o, 0);
    assert_eq!(diag_codes(&o), ["route.incomplete"], "{}", o["diagnostics"]);
    let failed: Vec<&Value> =
        o["output"]["connections"].as_array().unwrap().iter().filter(|c| c["status"] == "failed").collect();
    assert!(!failed.is_empty() && failed.iter().all(|c| c["net"] == "GND"), "{}", o["output"]["connections"]);
    for c in &failed {
        assert!(c["reason"].as_str().unwrap().contains("guide found no way from"), "{c}");
        assert!(c["subjects"].to_string().contains("GND"), "{c}");
        assert!(c["hints"][0].as_str().unwrap().starts_with("`route.connection "), "{c}");
        assert!(c["hints"].to_string().contains("native router"), "{c}");
    }
    assert_eq!(unrouted(&r, &mut s, "3V3"), 0);
    assert!(crossing(&s, 12.5, 13.5, -1.0, 16.0));
    assert_eq!(other_errors(&s), Vec::<String>::new());
}

/// Two nets between two pairs of 0402 resistors, as a differential pair: guide routes them
/// side by side at the pair's gap, and the report measures the result as the native one does.
#[test]
fn guide_routes_a_differential_pair() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "34mm", "height": "20mm"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 22R 1% 0402", "count": 4}));
    exec(&r, &mut s, "net.connect", json!({"net": "TX_P", "pins": ["R1.2", "R3.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "TX_N", "pins": ["R2.2", "R4.1"]}));
    for (refdes, x, y) in [("R1", 6.0, 10.6), ("R2", 6.0, 9.4), ("R3", 28.0, 10.6), ("R4", 28.0, 9.4)] {
        exec(&r, &mut s, "place.set", json!({"refdes": refdes, "at": [format!("{x}mm"), format!("{y}mm")]}));
    }
    exec(&r, &mut s, "netclass.set", json!({"name": "tx", "diff_pair_width": "0.2mm", "diff_pair_gap": "0.15mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["TX_P", "TX_N"], "class": "tx"}));
    exec(&r, &mut s, "diffpair.add", json!({"p": "TX_P", "n": "TX_N"}));
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide pair", &o, 0);
    render(&r, &mut s, "guide_pair");
    assert_eq!(o["output"]["stats"]["completion"], 100.0, "{}", o["output"]);
    assert_eq!(other_errors(&s), Vec::<String>::new());
    let pr = &o["output"]["pairs"][0];
    assert_eq!((pr["name"].as_str(), pr["status"].as_str()), (Some("TX"), Some("routed")), "{pr}");
    assert_eq!((pr["width"].as_str(), pr["gap"].as_str()), (Some("0.2mm"), Some("0.15mm")), "{pr}");
    assert_eq!((pr["connections"].as_u64(), pr["routed"].as_u64()), (Some(2), Some(2)), "{pr}");
    let mm = |v: &Value| Nm::parse(v.as_str().unwrap()).unwrap().0 as f64 / 1e6;
    assert!(mm(&pr["coupled"]) > 15.0, "most of the 22 mm between the parts runs coupled: {pr}");
    let p = s.project.as_ref().unwrap();
    assert!(p.board().tracks.iter().all(|t| t.width == Nm(200_000)), "the pair's width");
    let cp = cadlab::lengths::coupling(p, &p.circuit().diffpairs["TX"], Nm(150_000));
    assert!(!cp.sections.is_empty());
    // At the gap or a few nanometres over it: guide offsets the two tracks from a centreline on
    // its lattice (16 nm over here, where the native router is within 2 nm).
    for sec in &cp.sections {
        assert!((150_000..150_100).contains(&sec.gap.0), "{sec:?}");
    }
    // Uncoupled on request: the same two nets as any others.
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "pairs": false}));
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    assert_eq!(o["output"].get("pairs"), None, "{}", o["output"]);
}

/// Routing already there stays: guide takes it as fixed copper of its net and completes it,
/// and `route.nets` leaves the other nets alone.
#[test]
fn guide_completes_a_partial_routing() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    // 3V3 by hand, and a stub of VIN out of C1 with a via at its end.
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", ["13mm", "9mm"], "C2.1"]}));
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "net": "VIN", "points": ["C1.1", ["3mm", "7mm"]]}));
    exec(&r, &mut s, "via.add", json!({"at": ["3mm", "7mm"], "net": "VIN"}));
    let before = copper(&s);
    let o = exec(&r, &mut s, "route.nets", json!({"nets": ["VIN"], "router": "guide"}));
    report("guide partial, VIN", &o, 0);
    assert_eq!(o["output"]["stats"]["completion"], 100.0, "{}", o["output"]);
    assert!(o["output"]["connections"].as_array().unwrap().iter().all(|c| c["net"] == "VIN"));
    let b = s.project.as_ref().unwrap().board();
    let old: Vec<Value> = before.0.as_array().unwrap().clone();
    let now = serde_json::to_value(&b.tracks).unwrap();
    assert_eq!(now.as_array().unwrap()[..old.len()], old[..], "existing tracks are untouched");
    assert!(b.tracks.iter().skip(old.len()).all(|t| t.net.as_deref() == Some("VIN")), "only VIN was routed");
    assert_eq!(unrouted(&r, &mut s, "VIN"), 0);
    assert!(unrouted(&r, &mut s, "GND") > 0);
    assert_eq!(other_errors(&s), Vec::<String>::new());
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    assert!(o["output"]["connections"].as_array().unwrap().iter().all(|c| c["net"] == "GND"), "{}", o["output"]);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
}

/// guide has no threads: the same board gives the same copper and report on every run, inside
/// thread pools of any size, and so does the same seed.
#[test]
fn guide_is_identical_for_any_thread_count() {
    let r = Registry::with_builtins();
    let mut results = Vec::new();
    for threads in [1, 4, 1] {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
        let res = pool.install(|| {
            let mut s = Session::new();
            let _d = new_project(&r, &mut s);
            generated_board(&r, &mut s, 3, 2, 2, 3);
            let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
            let first = (copper(&s), o["output"].clone());
            exec(&r, &mut s, "route.rip", json!({"all": true}));
            let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "seed": 5}));
            assert_eq!(diag_codes(&o), Vec::<String>::new());
            (first, copper(&s), o["output"].clone())
        });
        results.push(res);
    }
    assert!(results[0].0.0.0.as_array().is_some_and(|a| !a.is_empty()));
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0], results[2]);
    // Another seed is another order of the nets: both complete, not the same copper.
    assert_eq!(results[0].2["stats"]["completion"], 100.0);
    assert_ne!(results[0].0.0, results[0].1);
}

/// One connection is refused with a code, the option concerned and what to do instead; options
/// guide has nothing for are named in a warning; `layers` and the budgets are kept; a cancelled
/// run changes nothing.
#[test]
fn guide_refuses_warns_and_keeps_its_limits() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    ldo_board(&r, &mut s);
    let conn = json!({"from": "U1.VOUT", "to": "C2.1", "router": "guide"});
    let f = r.execute(&mut s, "route.connection", conn, RunOptions::default()).expect_err("refused");
    let d = f.error.diagnostic.clone();
    assert_eq!(d.code, "route.guide_unsupported", "{}", f.error);
    assert!(d.hint.is_some(), "{d:?}");
    assert_eq!(d.subjects.iter().map(ToString::to_string).collect::<Vec<_>>(), ["option:router"]);
    assert!(s.project.as_ref().unwrap().board().tracks.is_empty());

    let cancel = cadlab::command::CancelToken::new();
    cancel.cancel();
    let f = r
        .execute_with(
            &mut s,
            "route.all",
            json!({"router": "guide"}),
            RunOptions::default(),
            &Collect(Default::default()),
            &cancel,
        )
        .expect_err("cancelled");
    assert_eq!(f.error.diagnostic.code, "cancelled");
    assert!(s.project.as_ref().unwrap().board().tracks.is_empty());

    // A work budget of one search node: the run stops there, the same on every run, and what
    // is left open says why. No time budget was given, so nothing depends on the clock.
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "work_budget": 1}));
    assert_eq!(o["output"]["stats"]["budget_exhausted"], true, "{}", o["output"]["stats"]);
    assert_eq!(diag_codes(&o), ["route.incomplete"]);
    let failed: Vec<&Value> =
        o["output"]["connections"].as_array().unwrap().iter().filter(|c| c["status"] == "failed").collect();
    assert!(!failed.is_empty());
    assert!(
        failed.iter().all(|c| c["reason"].as_str().unwrap().starts_with("the work budget was spent")),
        "{failed:?}"
    );
    let first = (copper(&s), o["output"].clone());
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "work_budget": 1}));
    assert_eq!(first, (copper(&s), o["output"].clone()));
    exec(&r, &mut s, "route.rip", json!({"all": true}));
    // A time budget that is over before the run starts: nothing routed, and the run says so.
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "budget_ms": 0}));
    assert_eq!(o["output"]["stats"]["routed"], 0, "{}", o["output"]["stats"]);
    assert!(diag_codes(&o).contains(&"route.budget_cut".to_string()), "{}", o["diagnostics"]);
    assert!(s.project.as_ref().unwrap().board().tracks.is_empty());

    // `layers`: the tracks keep to them. The parts are on the front, so the front alone routes
    // the board, without a via.
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "layers": ["F.Cu"]}));
    assert_eq!(o["output"]["stats"]["completion"], 100.0, "{}", o["output"]);
    let b = s.project.as_ref().unwrap().board();
    assert!(b.vias.is_empty() && b.tracks.iter().all(|t| t.layer == "F.Cu"));
    let f = r
        .execute(&mut s, "route.all", json!({"router": "guide", "layers": ["In9.Cu"]}), RunOptions::default())
        .expect_err("no such layer");
    assert_ne!(f.error.diagnostic.code, "route.guide_unsupported");
    exec(&r, &mut s, "route.rip", json!({"all": true}));

    let o = exec(&r, &mut s, "route.all", json!({"router": "guide", "seed": 3, "effort": "high", "arcs": true}));
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "route.guide_option_ignored");
    let d = d.unwrap_or_else(|| panic!("{}", o["diagnostics"]));
    let subjects = d["subjects"].to_string();
    assert!(["option:effort", "option:arcs"].iter().all(|o| subjects.contains(o)), "{d}");
    assert!(!subjects.contains("option:seed"), "the seed is guide's: {d}");
    assert_eq!(o["output"]["stats"]["completion"], 100.0);
    assert!(s.project.as_ref().unwrap().board().tracks.iter().all(|t| t.mid.is_none()), "no arcs from guide");
}

/// Pours with guide (the board of `pads_cut_off_from_their_pour_are_routed`): guide plans GND on
/// cadlab's fill of the zone, and what counts is the board as cadlab fills it again around the
/// new tracks, so the report is taken from that and GND's pads end up joined on it.
#[test]
fn guide_routes_a_board_with_a_pour() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(
        &r,
        &mut s,
        "footprint.create",
        json!({
            "name": "WALL6",
            "pads": [{"number": "1", "at": ["0mm", "-5mm"], "size": ["1.2mm", "1.7mm"], "shape": "rect",
                      "count": 6, "pitch": ["0mm", "2mm"]}]
        }),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "WALL6", "category": "connector", "footprint": "WALL6", "pins": numbered_pins(6)}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "WALL6", "refdes": "J1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "WALL6", "refdes": "J2"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 3}));
    for i in 1..=6 {
        let mut pins = vec![format!("J1.{i}"), format!("J2.{i}")];
        if (2..=4).contains(&i) {
            pins.push(format!("C{}.1", i - 1));
        }
        exec(&r, &mut s, "net.connect", json!({"net": format!("S{i}"), "pins": pins}));
    }
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["C1.2", "C2.2", "C3.2"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "30mm", "height": "16mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["4mm", "8mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J2", "at": ["26mm", "8mm"]}));
    for (i, x) in [(1, "10mm"), (2, "15mm"), (3, "20mm")] {
        let y = format!("{}mm", 4 + 2 * i);
        exec(&r, &mut s, "place.set", json!({"refdes": format!("C{i}"), "at": [x, y]}));
    }
    exec(&r, &mut s, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["F.Cu"], "outline": "board"}));
    assert_eq!(unrouted(&r, &mut s, "GND"), 0, "the pour joins the GND pads of the empty board");
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide pour", &o, 0);
    render(&r, &mut s, "guide_pour");
    assert_eq!(o["output"]["stats"]["failed"], 0, "{o}");
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["unrouted"], 0, "counted on the board with its pour filled again");
    // The signal tracks cut the pour that guide planned on (cadlab's fill of the empty board):
    // a second run, on the routed board and its fill, joined what was left.
    assert_eq!(o["output"]["stats"]["iterations"], 2, "{}", o["output"]["stats"]);
}

/// An unplated hole on the straight line between two pads, with no `hole_clearance` in the
/// rules: the DRC and guide then only forbid copper over the hole, and the native router keeps
/// the copper clearance from it all the same. The adapter gives the hole as copper of no net, so
/// guide's route keeps that distance too.
#[test]
fn guide_keeps_clear_of_unplated_holes() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "24mm", "height": "12mm"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 22R 1% 0402", "count": 2}));
    exec(&r, &mut s, "net.connect", json!({"net": "A", "pins": ["R1.2", "R2.1"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R1", "at": ["4mm", "6mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R2", "at": ["20mm", "6mm"]}));
    exec(&r, &mut s, "board.hole", json!({"at": ["12mm", "6.3mm"], "drill": "2mm"}));
    assert_eq!(s.project.as_ref().unwrap().board().rules.hole_clearance, Nm::ZERO);
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide past a hole", &o, 0);
    render(&r, &mut s, "guide_hole");
    assert_eq!(o["output"]["stats"]["completion"], 100.0, "{}", o["output"]);
    assert_eq!(drc_errors(&s), Vec::<String>::new());
    // Every track's edge is at least the clearance (0.2 mm) from the hole's (1 mm radius).
    let p = s.project.as_ref().unwrap();
    let clearance = p.board().rules.clearance.0 as f64;
    let (hx, hy) = (12e6, 6.3e6);
    for t in &p.board().tracks {
        let (ax, ay, bx, by) = (t.start.x.0 as f64, t.start.y.0 as f64, t.end.x.0 as f64, t.end.y.0 as f64);
        let (dx, dy) = (bx - ax, by - ay);
        let u = if dx == 0.0 && dy == 0.0 {
            0.0
        } else {
            (((hx - ax) * dx + (hy - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0)
        };
        let gap = (ax + u * dx - hx).hypot(ay + u * dy - hy) - 1e6 - t.width.0 as f64 / 2.0;
        assert!(gap >= clearance - 1.0, "track {} is {gap} nm from the hole", t.id.0);
    }
}

/// A length group with guide: four nets between two rows of resistors, the far row staggered
/// so that their straight lengths differ by 1.5 mm steps. The group goes to guide as a skew
/// limit (`LengthConstraint::max_skew`) and guide adds its meanders; the lengths are then
/// measured the way the DRC measures them (`lengthgroup.list`).
#[test]
fn guide_matches_a_length_group() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let _d = new_project(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "44mm", "height": "30mm"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 22R 1% 0402", "count": 8}));
    let members: Vec<String> = (0..4).map(|k| format!("DQ{k}")).collect();
    for k in 0..4 {
        let y = 5.0 + 6.0 * k as f64;
        let pins = [format!("R{}.2", k + 1), format!("R{}.1", k + 5)];
        exec(&r, &mut s, "net.connect", json!({"net": format!("DQ{k}"), "pins": pins}));
        exec(&r, &mut s, "place.set", json!({"refdes": format!("R{}", k + 1), "at": ["6mm", format!("{y}mm")]}));
        let x = 26.0 + 1.5 * k as f64;
        exec(
            &r,
            &mut s,
            "place.set",
            json!({"refdes": format!("R{}", k + 5), "at": [format!("{x}mm"), format!("{y}mm")]}),
        );
    }
    exec(&r, &mut s, "lengthgroup.set", json!({"name": "DQ", "members": members, "tolerance": "0.1mm"}));
    let o = exec(&r, &mut s, "route.all", json!({"router": "guide"}));
    report("guide length group", &o, 0);
    render(&r, &mut s, "guide_lengths");
    assert_eq!(o["output"]["stats"]["completion"], 100.0, "{}", o["output"]);
    assert_eq!(other_errors(&s), Vec::<String>::new());
    let o = exec(&r, &mut s, "lengthgroup.list", json!({}));
    let status = &o["output"]["groups"][0]["status"];
    let lengths: Vec<i64> = status["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| Nm::parse(m["length"].as_str().unwrap()).unwrap().0)
        .collect();
    let (short, long) = (*lengths.iter().min().unwrap(), *lengths.iter().max().unwrap());
    assert!(long > 23_000_000 && long - short <= 100_000, "{status}");
    // The three shorter nets were made longer: without the group they are straight tracks.
    let p = s.project.as_ref().unwrap();
    let segments = |net: &str| p.board().tracks.iter().filter(|t| t.net.as_deref() == Some(net)).count();
    assert!(segments("DQ0") > 1 && segments("DQ1") > 1 && segments("DQ2") > 1, "{status}");
    let mismatch: Vec<String> = cadlab::drc::check(s.project.as_ref().unwrap())
        .into_iter()
        .filter(|d| d.code.starts_with("drc.length"))
        .map(|d| d.message)
        .collect();
    assert_eq!(mismatch, Vec::<String>::new());
}
