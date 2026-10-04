//! M5 autorouter: route test boards to completion with zero DRC errors, determinism,
//! cancellation and budget, rip-up.

mod common;

use std::time::Instant;

use cadlab::command::{Registry, RunOptions, Session};
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
    for fanout in [false, true] {
        let mut s = Session::new();
        let _d = new_project(&r, &mut s);
        lqfp_board(&r, &mut s);
        let o = exec(&r, &mut s, "route.all", json!({"fanout": fanout}));
        report(&format!("lqfp48 fanout={fanout}"), &o, 0);
        let errors: Vec<String> = drc_errors(&s).into_iter().filter(|e| !e.starts_with("drc.unrouted")).collect();
        assert_eq!(errors, Vec::<String>::new());
        results.push((completion(&o), o["output"]["stats"]["escapes"].as_u64().unwrap()));
    }
    assert_eq!(results[0].1, 0);
    assert!(results[1].1 > 10, "{results:?}");
    assert!(results[1].0 > results[0].0, "escapes help: {results:?}");
    assert_eq!(results[1].0, 100.0);
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
    let o = exec(&r, &mut s, "route.all", json!({"effort": "low"}));
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
