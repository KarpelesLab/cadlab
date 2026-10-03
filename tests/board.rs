//! M4 foundation: board setup, placement, manual routing, ratsnest.

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

fn setup() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    exec(&r, &mut s, "part.create", json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}));
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    (dir, r, s)
}

#[test]
fn outline_place_route_ratsnest() {
    let (dir, r, mut s) = setup();
    let o = exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm", "corner_radius": "1mm"}));
    assert_eq!(o["output"]["size"], json!(["20mm", "15mm"]));
    assert_eq!(o["output"]["unplaced"], json!(["C1", "C2", "U1"]));

    let o = exec(&r, &mut s, "place.auto", json!({}));
    assert_eq!(o["output"]["placements"].as_object().unwrap().len(), 3);
    let o = exec(&r, &mut s, "board.info", json!({}));
    assert_eq!(o["output"]["placed"], 3);
    // VIN: U1.1, U1.3, C1.1 -> 2 links; GND: 3 pads -> 2; 3V3: 2 pads -> 1.
    assert_eq!(o["output"]["unrouted"], 5);

    // Place explicitly and route U1.VOUT -> C2.1 on top copper.
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "9mm"], "rotation": 90}));
    let o = exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", ["13mm", "9mm"], "C2.1"]}));
    assert_eq!(o["output"]["tracks"].as_array().unwrap().len(), 2);
    assert_eq!(o["output"]["tracks"][0]["net"], "3V3", "net inferred from the pins");
    assert_eq!(o["output"]["tracks"][0]["width"], "0.25mm", "rules default width");
    let o = exec(&r, &mut s, "board.ratsnest", json!({"net": "3V3"}));
    assert_eq!(o["output"]["lines"], json!([]), "3V3 routed");
    let o = exec(&r, &mut s, "board.info", json!({}));
    assert_eq!(o["output"]["unrouted"], 4);

    // Joining two nets with one track is refused.
    let f = fail(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", "U1.VIN"]}));
    assert_eq!(f.error.diagnostic.code, "track.short");
    let f = fail(&r, &mut s, "track.add", json!({"layer": "In1.Cu", "points": [["0mm", "0mm"], ["1mm", "0mm"]]}));
    assert_eq!(f.error.diagnostic.code, "track.invalid_layer");

    // Vias: defaults from rules; net class overrides widths.
    exec(&r, &mut s, "netclass.set", json!({"name": "power", "track_width": "0.5mm", "via_drill": "0.4mm", "via_diameter": "0.8mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["GND"], "class": "power"}));
    let o = exec(&r, &mut s, "via.add", json!({"at": ["5mm", "5mm"], "net": "GND"}));
    assert_eq!(o["output"]["vias"][0]["diameter"], "0.8mm");
    let o = exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "points": [["5mm", "5mm"], ["8mm", "5mm"]], "net": "GND"}));
    assert_eq!(o["output"]["tracks"][0]["width"], "0.5mm");
    let id = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    exec(&r, &mut s, "track.remove", json!({"ids": [format!("track#{id}")]}));
    exec(&r, &mut s, "via.remove", json!({"ids": ["via#".to_string() + &(id - 1).to_string()]}));

    // Locks.
    exec(&r, &mut s, "place.lock", json!({"refdes": ["U1"]}));
    let f = fail(&r, &mut s, "place.move", json!({"refdes": ["U1"], "by": ["1mm", "0mm"]}));
    assert_eq!(f.error.diagnostic.code, "place.locked");
    exec(&r, &mut s, "place.lock", json!({"refdes": ["U1"], "locked": false}));

    // Renaming and removing components keeps the board consistent.
    exec(&r, &mut s, "circuit.rename", json!({"from": "C2", "to": "C5"}));
    let o = exec(&r, &mut s, "place.list", json!({}));
    assert!(o["output"]["placements"].get("C5").is_some());
    exec(&r, &mut s, "net.rename", json!({"from": "3V3", "to": "VOUT"}));
    let o = exec(&r, &mut s, "track.list", json!({"net": "VOUT"}));
    assert_eq!(o["output"]["tracks"].as_array().unwrap().len(), 2);

    // Persisted.
    s.save().unwrap();
    let (s2, _) = Session::open(&dir.path().join("p")).unwrap();
    assert_eq!(s2.project, s.project);
}

#[test]
fn bottom_side_mirrors_pads() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "10mm"]}));
    let top = s.project.as_ref().map(cadlab::board::placed_pads).unwrap();
    let p1_top = top.iter().find(|p| p.refdes == "U1" && p.number == "1").unwrap().center;
    exec(&r, &mut s, "place.flip", json!({"refdes": ["U1"]}));
    let bottom = s.project.as_ref().map(cadlab::board::placed_pads).unwrap();
    let p1 = bottom.iter().find(|p| p.refdes == "U1" && p.number == "1").unwrap();
    assert_eq!(p1.center.y, p1_top.y);
    assert_eq!(p1.center.x.0 - 10_000_000, -(p1_top.x.0 - 10_000_000), "mirrored across the footprint's Y axis");
    assert_eq!(p1.layers, ["B.Cu"]);
    assert_eq!(p1.net.as_deref(), Some("VIN"));
}

#[test]
fn board_setup_and_rules() {
    let (_d, r, mut s) = setup();
    let o = exec(&r, &mut s, "board.setup", json!({"layers": 4, "finish": ["ENIG"]}));
    assert_eq!(o["output"]["layers"], json!(["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]));
    let f = fail(&r, &mut s, "board.setup", json!({"layers": 3}));
    assert_eq!(f.error.diagnostic.code, "board.invalid_layers");
    let o = exec(&r, &mut s, "board.rules", json!({"clearance": "0.15mm"}));
    assert_eq!(o["output"]["clearance"], "0.15mm");
    let f = fail(&r, &mut s, "board.rules", json!({"via_drill": "0.7mm"}));
    assert_eq!(f.error.diagnostic.code, "board.invalid_rule");
    let f = fail(&r, &mut s, "place.auto", json!({}));
    assert_eq!(f.error.diagnostic.code, "board.no_outline");
    exec(&r, &mut s, "board.outline", json!({"diameter": "30mm"}));
    exec(&r, &mut s, "board.outline", json!({"polygon": [["0mm", "0mm"], ["10mm", "0mm"], ["5mm", "8mm"]]}));
}
