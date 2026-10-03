//! M4 zones: copper pours with fill, keep-outs, connectivity through pours.

use cadlab::command::{Registry, RunOptions, Session};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> cadlab::command::Failure {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd)
}

/// The LDO + caps board of `tests/board.rs`, placed.
fn setup() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm", "corner_radius": "1mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "7.5mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "7.5mm"], "rotation": 90}));
    (dir, r, s)
}

fn gnd_lines(r: &Registry, s: &mut Session) -> usize {
    exec(r, s, "board.ratsnest", json!({"net": "GND"}))["output"]["lines"].as_array().unwrap().len()
}

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

/// Every zone island keeps the clearance from every other-net item on its layer.
fn assert_clearances(s: &Session) -> usize {
    use cadlab::board::{ItemRef, copper_items};
    use cadlab::geom::poly;
    let p = s.project.as_ref().unwrap();
    let clearance = p.board().rules.clearance.0;
    let items = copper_items(p);
    let mut checked = 0;
    for z in items.iter().filter(|i| matches!(i.item, ItemRef::Zone(..))) {
        for o in items.iter().filter(|o| o.net != z.net && o.layers.iter().any(|l| z.layers.contains(l))) {
            assert!(!poly::distance_less_than(&z.shape, &o.shape, clearance), "{} too close to {}", z.item, o.item);
            checked += 1;
        }
    }
    checked
}

#[test]
fn gnd_pours_connect_pads() {
    let (dir, r, mut s) = setup();
    assert_eq!(gnd_lines(&r, &mut s), 2);

    // Bottom pour: the SMD pads are on top, so nothing of GND touches it yet.
    let o = exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND_bottom", "net": "GND", "layers": ["B.Cu"], "outline": "board"}),
    );
    assert_eq!(o["output"]["zones"][0]["outline"].as_array().unwrap().len(), 4, "board bounding box");
    let o = exec(&r, &mut s, "zone.fill", json!({}));
    assert_eq!(codes(&o), ["zone.empty"]);
    assert_eq!(o["output"]["fills"][0]["islands"], 0);
    assert_eq!(gnd_lines(&r, &mut s), 2);

    // A via next to each GND pad, joined by a short top track: the pour connects them.
    for (pin, at) in [("C1.2", ["5mm", "9.3mm"]), ("C2.2", ["15mm", "9.3mm"]), ("U1.2", ["7.3mm", "7.5mm"])] {
        exec(&r, &mut s, "via.add", json!({"at": at, "net": "GND"}));
        exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": [pin, at]}));
    }
    let o = exec(&r, &mut s, "zone.fill", json!({"name": "gnd_bottom"}));
    assert_eq!(codes(&o), Vec::<String>::new());
    let f = &o["output"]["fills"][0];
    assert_eq!(
        (f["zone"].as_str(), f["layer"].as_str(), f["islands"].as_u64()),
        (Some("GND_bottom"), Some("B.Cu"), Some(1))
    );
    let area = f["area_mm2"].as_f64().unwrap();
    // 20 x 15 board less the 0.3 mm edge clearance, rounded corners and via clearances: nothing else on B.Cu.
    assert!(area > 19.3 * 14.3 - 1.0 && area < 19.4 * 14.4, "{area}");
    assert_eq!(gnd_lines(&r, &mut s), 0, "GND joined through the bottom pour");
    let o = exec(&r, &mut s, "board.ratsnest", json!({}));
    assert_eq!(o["output"]["lines"].as_array().unwrap().len(), 3, "VIN 2 + 3V3 1 unchanged");
    assert_eq!(assert_clearances(&s), 0, "no other-net copper on B.Cu");

    // A keep-out across the middle splits the bottom pour (warned), keeping both halves (each has a via).
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "slot", "layers": ["B.Cu"], "no_pours": true,
        "outline": {"rect": {"from": ["11mm", "-1mm"], "to": ["12mm", "16mm"]}}}),
    );
    let o = exec(&r, &mut s, "zone.fill", json!({}));
    assert_eq!(codes(&o), ["zone.split"]);
    assert_eq!(o["output"]["fills"][0]["islands"], 2);
    assert_eq!(gnd_lines(&r, &mut s), 1, "C2.2's half is cut off from C1.2 and U1.2");
    exec(&r, &mut s, "keepout.remove", json!({"name": "slot"}));
    assert_eq!(gnd_lines(&r, &mut s), 0);

    // Top pour instead: thermal spokes connect the pads directly.
    exec(&r, &mut s, "zone.remove", json!({"name": "GND_bottom"}));
    exec(&r, &mut s, "track.remove", json!({"net": "GND"}));
    let vias: Vec<String> =
        s.project.as_ref().unwrap().board().vias.iter().map(|v| format!("via#{}", v.id.0)).collect();
    exec(&r, &mut s, "via.remove", json!({"ids": vias}));
    assert_eq!(gnd_lines(&r, &mut s), 2);
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND_top", "net": "GND", "layers": ["F.Cu"], "outline": "board", "priority": 1}),
    );
    let o = exec(&r, &mut s, "zone.fill", json!({}));
    assert_eq!(codes(&o), Vec::<String>::new());
    assert_eq!(gnd_lines(&r, &mut s), 0, "GND joined through the top pour's thermal spokes");
    assert!(assert_clearances(&s) > 0);
    // Pads not connected: the pour floats and is removed.
    exec(&r, &mut s, "zone.set", json!({"name": "GND_top", "pads": "none"}));
    let o = exec(&r, &mut s, "zone.fill", json!({}));
    assert_eq!(codes(&o), ["zone.empty"]);
    assert_eq!(gnd_lines(&r, &mut s), 2);
    exec(&r, &mut s, "zone.set", json!({"name": "GND_top", "pads": "solid"}));
    assert_eq!(gnd_lines(&r, &mut s), 0);
    assert!(assert_clearances(&s) > 0);

    let o = exec(&r, &mut s, "zone.list", json!({}));
    assert_eq!(o["output"]["zones"][0]["pads"], "solid");
    assert_eq!(o["output"]["zones"][0]["priority"], 1);
    s.save().unwrap();
    let (s2, _) = Session::open(&dir.path().join("p")).unwrap();
    assert_eq!(s2.project, s.project);
}

#[test]
fn zone_errors() {
    let (_d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "gnd", "layers": ["b.cu"],
        "outline": {"rect": {"from": ["0mm", "0mm"], "to": ["10mm", "10mm"]}}}),
    );
    let o = exec(&r, &mut s, "zone.list", json!({}));
    assert_eq!(o["output"]["zones"][0]["net"], "GND");
    assert_eq!(o["output"]["zones"][0]["layers"], json!(["B.Cu"]));
    let f = fail(&r, &mut s, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["F.Cu"], "outline": "board"}));
    assert_eq!(f.error.diagnostic.code, "zone.duplicate");
    let f = fail(&r, &mut s, "zone.add", json!({"name": "Z", "net": "GND", "layers": ["In1.Cu"], "outline": "board"}));
    assert_eq!(f.error.diagnostic.code, "zone.invalid_layer");
    let f = fail(&r, &mut s, "zone.add", json!({"name": "Z", "net": "GNDD", "layers": ["F.Cu"], "outline": "board"}));
    assert_eq!(f.error.diagnostic.code, "net.not_found");
    let f = fail(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "Z", "net": "GND", "layers": ["F.Cu"],
        "outline": [["0mm", "0mm"], ["1mm", "1mm"], ["2mm", "2mm"]]}),
    );
    assert_eq!(f.error.diagnostic.code, "zone.invalid_outline");
    let f = fail(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "Z", "net": "GND", "layers": ["F.Cu"], "outline": "board", "thermal_spoke": "0mm"}),
    );
    assert_eq!(f.error.diagnostic.code, "zone.invalid_value");
    let f = fail(&r, &mut s, "zone.remove", json!({"name": "GNX"}));
    assert_eq!(f.error.diagnostic.code, "zone.not_found");
    assert!(f.error.to_string().contains("GND") || format!("{:?}", f.error).contains("GND"), "suggests GND");
    let f = fail(&r, &mut s, "zone.fill", json!({"name": "nope"}));
    assert_eq!(f.error.diagnostic.code, "zone.not_found");
    exec(&r, &mut s, "zone.add", json!({"name": "V", "net": "VIN", "layers": ["F.Cu"], "outline": "board"}));
    let f = fail(&r, &mut s, "zone.set", json!({"name": "V", "rename": "gnd"}));
    assert_eq!(f.error.diagnostic.code, "zone.duplicate");
    let f = fail(&r, &mut s, "keepout.add", json!({"name": "k", "outline": "board", "no_vias": false}));
    assert_eq!(f.error.diagnostic.code, "keepout.nothing_forbidden");
    let o = exec(&r, &mut s, "keepout.add", json!({"name": "k", "outline": "board"}));
    let k = &o["output"]["keepouts"][0];
    assert_eq!(
        (k["no_tracks"].as_bool(), k["no_pours"].as_bool()),
        (Some(true), Some(true)),
        "all forbidden by default"
    );
    let f = fail(&r, &mut s, "keepout.add", json!({"name": "K", "outline": "board"}));
    assert_eq!(f.error.diagnostic.code, "keepout.duplicate");
    let f = fail(&r, &mut s, "keepout.remove", json!({"name": "x"}));
    assert_eq!(f.error.diagnostic.code, "keepout.not_found");
}

/// Fill timing on a 100 x 100 mm board with a few hundred obstacles:
/// `cargo test --release --test zone fill_bench -- --ignored --nocapture`.
#[test]
#[ignore]
fn fill_bench() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 60}));
    let gnd: Vec<String> = (1..=60).map(|i| format!("C{i}.2")).collect();
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": gnd}));
    for i in 0..20 {
        exec(&r, &mut s, "net.connect", json!({"net": format!("N{i}"), "pins": [format!("C{}.1", i + 1)]}));
    }
    exec(&r, &mut s, "board.outline", json!({"width": "100mm", "height": "100mm", "corner_radius": "3mm"}));
    for i in 0..60 {
        let (x, y) = (8 + (i % 10) * 9, 10 + (i / 10) * 14);
        let at = [format!("{x}mm"), format!("{y}mm")];
        exec(&r, &mut s, "place.set", json!({"refdes": format!("C{}", i + 1), "at": at, "rotation": (i % 4) * 45}));
    }
    for i in 0..200 {
        let (x, y) = (5 + (i * 37) % 90, 5 + (i * 53) % 90);
        let net = format!("N{}", i % 20);
        exec(&r, &mut s, "via.add", json!({"at": [format!("{x}.5mm"), format!("{y}.25mm")], "net": net}));
        let layer = if i % 2 == 0 { "F.Cu" } else { "B.Cu" };
        let a = [format!("{}mm", 3 + (i * 7) % 94), format!("{}.6mm", 3 + (i * 11) % 94)];
        let b = [format!("{}mm", 3 + (i * 13) % 94), format!("{}.6mm", 3 + (i * 17) % 94)];
        exec(&r, &mut s, "track.add", json!({"layer": layer, "net": net, "points": [a, b]}));
    }
    // GND stitching vias, so the bottom pour has something to connect to.
    for i in 0..40 {
        let (x, y) = (6 + (i % 8) * 12, 4 + (i / 8) * 20);
        exec(&r, &mut s, "via.add", json!({"at": [format!("{x}mm"), format!("{y}mm")], "net": "GND"}));
    }
    exec(&r, &mut s, "zone.add", json!({"name": "GND", "net": "GND", "layers": ["F.Cu", "B.Cu"], "outline": "board"}));
    let p = s.project.as_ref().unwrap();
    let base = cadlab::board::base_copper_items(p);
    println!("{} base copper items", base.len());
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let fills = cadlab::board::zones::fill_zones_uncached(p, &base);
        let dt = t.elapsed();
        for f in &fills {
            let v: usize = f.fill.iter().map(|q| q.vertex_count()).sum();
            let a = cadlab::board::zones::area_mm2(&f.fill);
            println!("{} {}: {} islands, {v} vertices, {a:.1} mm2", f.name, f.layer, f.fill.len());
        }
        println!("fill_zones (2 layers): {dt:?}");
    }
    for what in ["memo miss", "memo hit"] {
        let t = std::time::Instant::now();
        cadlab::board::zones::fill_zones(p, &base);
        println!("fill_zones ({what}): {:?}", t.elapsed());
    }
    let t = std::time::Instant::now();
    let n = cadlab::board::ratsnest(p).len();
    println!("ratsnest with zones: {:?} ({n} lines)", t.elapsed());
}

/// DRC and zone fill agree: a filled pour causes no clearance or short findings, and pads joined
/// through it are not reported as unrouted.
#[test]
fn drc_accepts_filled_pours() {
    let (_dir, r, mut s) = setup();
    exec(&r, &mut s, "zone.add", json!({"name": "GND_top", "net": "GND", "layers": ["F.Cu"], "outline": "board"}));
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", "C2.1"]}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let c = codes(&o);
    assert!(!c.iter().any(|c| c == "drc.short" || c == "drc.clearance"), "{:?}", o["diagnostics"]);
    let unrouted: Vec<&Value> =
        o["diagnostics"].as_array().unwrap().iter().filter(|d| d["code"] == "drc.unrouted").collect();
    assert!(unrouted.iter().all(|d| !d["message"].as_str().unwrap().contains("GND")), "{unrouted:?}");
}
