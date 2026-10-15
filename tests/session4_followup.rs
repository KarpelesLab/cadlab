//! Agent session 4's open items (docs/TESTING.md, "Session 4 follow-up"), one focused test
//! each: KiCad DRC exclusions as waivers, "keep together" placement hints, `place.near` on a
//! routed board, `route.nets` shoving for leftovers, export and listing output, supplier part
//! numbers from netlist fields.

use std::path::Path;
use std::sync::Arc;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::geom::Point;
use cadlab::supplier::catalog::Catalog;
use cadlab::supplier::{Candidate, Suppliers};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args.clone(), RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} {args} failed: {}", f.error),
    }
}

fn err(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    match r.execute(s, cmd, args.clone(), RunOptions::default()) {
        Ok(o) => panic!("{cmd} {args} should fail: {}", serde_json::to_value(&o).unwrap()),
        Err(f) => f.error.diagnostic.code.to_string(),
    }
}

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().into_iter().flatten().filter_map(|d| d["code"].as_str().map(String::from)).collect()
}

fn diag<'a>(o: &'a Value, code: &str) -> &'a Value {
    o["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["code"] == code)
        .unwrap_or_else(|| panic!("no {code} in {o}"))
}

fn mm(x: f64, y: f64) -> Value {
    json!([format!("{x}mm"), format!("{y}mm")])
}

fn new_project(r: &Registry, dir: &Path, name: &str) -> Session {
    let mut s = Session::new();
    exec(r, &mut s, "project.new", json!({"path": dir.join(name)}));
    s
}

/// Center of the pad of `refdes` on `net`.
fn pad(s: &Session, refdes: &str, net: &str) -> Point {
    cadlab::board::placed_pads(s.project.as_ref().unwrap())
        .into_iter()
        .find(|p| p.refdes == refdes && p.net.as_deref() == Some(net))
        .unwrap_or_else(|| panic!("no pad of {refdes} on {net}"))
        .center
}

fn dist_mm(a: Point, b: Point) -> f64 {
    let (dx, dy) = ((a.x.0 - b.x.0) as f64, (a.y.0 - b.y.0) as f64);
    (dx * dx + dy * dy).sqrt() / 1e6
}

// ---- KiCad DRC exclusions --------------------------------------------------------------------

/// A KiCad project whose `.kicad_pro` excludes findings: the one cadlab finds too becomes a
/// waiver (comment as reason), the others are reported by cause.
#[test]
fn kicad_drc_exclusions_become_waivers() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "src");
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0603", "count": 2}));
    exec(&r, &mut s, "net.connect", json!({"net": "A", "pins": ["R1.1", "R2.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "B", "pins": ["R1.2", "R2.2"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    // Courtyards overlap (0.8 mm apart vertically, 1.5 mm tall), pads do not.
    exec(&r, &mut s, "place.set", json!({"refdes": "R1", "at": mm(10.0, 7.0)}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R2", "at": mm(10.0, 8.2)}));
    let d = exec(&r, &mut s, "drc.run", json!({"codes": ["drc.courtyard_overlap"]}));
    assert_eq!(d["output"]["listed"], 1, "{d}");
    let out = cadlab::kicad_pcb::export(s.project.as_ref().unwrap(), "x");
    let uuid = |label: &str| out.uuids.iter().find(|(_, l)| *l == label).map(|(u, _)| u.clone()).unwrap();
    let kdir = dir.path().join("kicad");
    std::fs::create_dir_all(&kdir).unwrap();
    std::fs::write(kdir.join("x.kicad_pcb"), &out.pcb).unwrap();
    let mut pro: Value = serde_json::from_str(&out.project).unwrap();
    let (r1, r2) = (uuid("R1"), uuid("R2"));
    pro["board"]["design_settings"]["drc_exclusions"] = json!([
        [format!("courtyards_overlap|10000000|7000000|{r1}|{r2}"), "tight on purpose"],
        format!("npth_inside_courtyard|1|2|{r1}|00000000-0000-0000-0000-000000000000"),
        format!("clearance|1|2|{r1}|{r2}"),
        "hole_clearance|1|2|11111111-2222-3333-4444-555555555555|00000000-0000-0000-0000-000000000000",
    ]);
    std::fs::write(kdir.join("x.kicad_pro"), serde_json::to_string_pretty(&pro).unwrap()).unwrap();

    let mut s = new_project(&r, dir.path(), "dst");
    let o = exec(&r, &mut s, "board.import_kicad", json!({"path": kdir.join("x.kicad_pcb")}));
    assert_eq!(o["output"]["drc_exclusions"], json!({"read": 4, "waived": 1, "not_matched": 3}), "{o}");
    let notes: Vec<&str> = o["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "import.drc_exclusion")
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(notes.iter().any(|m| m.contains("`npth_inside_courtyard`") && m.contains("no check of that type")));
    assert!(notes.iter().any(|m| m.contains("`clearance`") && m.contains("does not find")));
    assert!(notes.iter().any(|m| m.contains("`hole_clearance`") && m.contains("left out")));
    let w = exec(&r, &mut s, "drc.waivers", json!({}));
    assert_eq!(
        w["output"]["waivers"],
        json!([{"code": "drc.courtyard_overlap", "subjects": ["R1", "R2"],
                "reason": "KiCad DRC exclusion (courtyards_overlap): tight on purpose"}])
    );
    let d = exec(&r, &mut s, "drc.run", json!({}));
    assert_eq!(d["output"]["waived"], 1, "{d}");
    assert!(!codes(&d).contains(&"drc.courtyard_overlap".to_string()), "{d}");
    assert!(!codes(&d).contains(&"drc.waiver_unused".to_string()), "{d}");
}

/// A real project's exclusions (the corpus's Cynthion, skipped without `CADLAB_CORPUS_DIR`):
/// its 5 courtyard overlaps and 3 copper-to-edge findings become waivers; the footprint library
/// mismatches and holes in courtyards (no cadlab check) are reported.
#[test]
fn corpus_drc_exclusions() {
    let Some(root) = std::env::var_os("CADLAB_CORPUS_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipping: set CADLAB_CORPUS_DIR to the directory filled by scripts/fetch-corpus.sh");
        return;
    };
    let pcb = root.join("cynthion/cynthion.kicad_pcb");
    if !pcb.is_file() {
        eprintln!("skipping: cynthion is not in {}", root.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    let o = exec(&r, &mut s, "board.import_kicad", json!({"path": pcb}));
    assert_eq!(o["output"]["drc_exclusions"], json!({"read": 13, "waived": 8, "not_matched": 5}), "{o}");
    let w = exec(&r, &mut s, "drc.waivers", json!({}));
    let codes: Vec<&str> =
        w["output"]["waivers"].as_array().unwrap().iter().map(|w| w["code"].as_str().unwrap()).collect();
    assert_eq!(codes.iter().filter(|c| **c == "drc.courtyard_overlap").count(), 5, "{w}");
    assert_eq!(codes.iter().filter(|c| **c == "drc.copper_to_edge").count(), 3, "{w}");
}

// ---- Placement: keep-together hints, place.near on a routed board -----------------------------

/// A 5 V input with an LDO and a power LED: the LED's resistor is pulled to +5V and the LED to
/// GND; `place.hint R1 --near D1` keeps the resistor at the LED.
fn led_board(r: &Registry, s: &mut Session) {
    let pins = |list: &[(&str, &str, &str)]| -> Value {
        list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect()
    };
    exec(
        r,
        s,
        "part.create",
        json!({"id": "PWR_HDR", "category": "connector", "package": "PinHeader 1x02",
            "pins": pins(&[("1", "VBUS", "passive"), ("2", "GND", "passive")])}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
            "pins": pins(&[("1", "VIN", "power_in"), ("2", "GND", "power_in"), ("3", "EN", "input"),
                ("4", "NC", "no_connect"), ("5", "VOUT", "power_out")])}),
    );
    exec(r, s, "circuit.add", json!({"part": "PWR_HDR", "refdes": "J1"}));
    exec(r, s, "circuit.add", json!({"part": "AP2112K-3.3TRG1", "refdes": "U1"}));
    exec(r, s, "circuit.add", json!({"part": "C 1uF 16V X5R 0603", "count": 2}));
    exec(r, s, "circuit.add", json!({"part": "R 1k 1% 0603", "refdes": "R1"}));
    exec(r, s, "circuit.add", json!({"part": "LED red 0603", "refdes": "D1"}));
    exec(r, s, "net.connect", json!({"net": "+5V", "pins": ["J1.VBUS", "U1.VIN", "U1.EN", "C1.1", "R1.1"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["J1.GND", "U1.GND", "C1.2", "C2.2", "D1.K"]}));
    exec(r, s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(r, s, "net.connect", json!({"net": "PWR_LED", "pins": ["R1.2", "D1.A"]}));
    exec(r, s, "board.outline", json!({"width": "40mm", "height": "30mm"}));
}

#[test]
fn keep_together_hint_holds_a_part_next_to_another() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    led_board(&r, &mut s);
    // The LED goes to an edge, as indicators do. Without a hint its resistor stays with +5V,
    // far from it (9 mm here; 15 mm in the session).
    exec(&r, &mut s, "place.hint", json!({"refdes": ["D1"], "edge": "top"}));
    exec(&r, &mut s, "place.auto", json!({}));
    let apart = dist_mm(pad(&s, "R1", "PWR_LED"), pad(&s, "D1", "PWR_LED"));
    assert!(apart > 5.0, "{apart:.2} mm");
    let o = exec(&r, &mut s, "place.hint", json!({"refdes": ["R1"], "near": "d1"}));
    assert_eq!(o["output"]["hints"]["R1"], json!({"near": "D1"}), "{o}");
    let h = r.execute(&mut s, "place.hint", json!({}), RunOptions::default()).unwrap();
    assert!(h.summary.contains("R1: near D1") && h.summary.contains("D1: edge top"), "{}", h.summary);
    exec(&r, &mut s, "place.auto", json!({"replace": true}));
    let d = dist_mm(pad(&s, "R1", "PWR_LED"), pad(&s, "D1", "PWR_LED"));
    assert!(d < 3.0, "R1.2 is {d:.2} mm from D1.A");
    // Deterministic.
    let first = s.project.as_ref().unwrap().board().footprints.clone();
    exec(&r, &mut s, "place.auto", json!({"replace": true}));
    assert_eq!(s.project.as_ref().unwrap().board().footprints, first);

    // Mistakes are refused with a code and a way out.
    assert_eq!(err(&r, &mut s, "place.hint", json!({"refdes": ["R1"], "near": "R1"})), "place.hint_near_self");
    assert_eq!(err(&r, &mut s, "place.hint", json!({"refdes": ["D1"], "near": "R1"})), "place.hint_near_loop");
    assert_eq!(
        err(&r, &mut s, "place.hint", json!({"refdes": ["R1"], "near": "D1", "clear": true})),
        "place.hint_conflict"
    );
    assert_eq!(err(&r, &mut s, "place.hint", json!({"refdes": ["R1"], "near": "D7"})), "component.not_found");
    // Renaming the LED follows; removing it drops the hint.
    exec(&r, &mut s, "circuit.rename", json!({"from": "D1", "to": "D9"}));
    let o = exec(&r, &mut s, "place.hint", json!({}));
    assert_eq!(o["output"]["hints"]["R1"], json!({"near": "D9"}), "{o}");
    exec(&r, &mut s, "circuit.remove", json!({"refdes": ["D9"]}));
    let o = exec(&r, &mut s, "place.hint", json!({}));
    assert!(o["output"]["hints"].get("R1").is_none(), "{o}");
}

/// Board F.Cu covered by GND tracks except a slot for the LED: no room for the resistor's
/// courtyard clear of them within reach, so `place.near` lets the body cover them and keeps the
/// pads clear, and says so.
#[test]
fn place_near_finds_room_on_a_routed_board() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0603", "refdes": "R1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "LED red 0603", "refdes": "D1"}));
    exec(&r, &mut s, "net.connect", json!({"net": "PWR_LED", "pins": ["R1.2", "D1.A"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["D1.K"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "+5V", "pins": ["R1.1"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "D1", "at": mm(10.0, 7.5)}));
    let track = |s: &mut Session, a: Value, b: Value| {
        exec(&r, s, "track.add", json!({"layer": "F.Cu", "net": "GND", "width": "0.25mm", "points": [a, b]}));
    };
    for y in [1.5, 3.5, 5.5, 9.5, 11.5, 13.5] {
        track(&mut s, mm(0.5, y), mm(19.5, y));
    }
    track(&mut s, mm(0.5, 7.5), mm(7.0, 7.5));
    track(&mut s, mm(13.0, 7.5), mm(19.5, 7.5));
    let o = exec(&r, &mut s, "place.near", json!({"refdes": "R1", "target": "D1.A"}));
    let d = diag(&o, "place.over_routing");
    assert!(d["message"].as_str().unwrap().contains("GND"), "{d}");
    assert!(d["subjects"].as_array().unwrap().contains(&json!("net:GND")), "{d}");
    // The pads keep their clearance: no short, no clearance error.
    let drc = exec(&r, &mut s, "drc.run", json!({"codes": ["drc.clearance", "drc.short", "drc.courtyard"]}));
    assert_eq!(drc["output"]["listed"].as_u64().unwrap_or(0), 0, "{drc}");
    assert!(dist_mm(pad(&s, "R1", "PWR_LED"), pad(&s, "D1", "PWR_LED")) < 5.0);
}

// ---- Routing: route.nets shoves for leftovers ------------------------------------------------

/// Net A must cross net B's routing, a wall across the board on F.Cu (routing limited to F.Cu):
/// `route.nets A` rips up and reroutes B around it, as `route.connection` does; without `shove`
/// the failure names `route.connection`.
#[test]
fn route_nets_shoves_what_it_cannot_route() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0603", "count": 4}));
    exec(&r, &mut s, "net.connect", json!({"net": "A", "pins": ["R1.1", "R2.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "B", "pins": ["R3.1", "R3.2", "R4.1", "R4.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "N1", "pins": ["R1.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "N2", "pins": ["R2.2"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R1", "at": mm(10.0, 12.0)}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R2", "at": mm(10.0, 3.0)}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R3", "at": mm(1.6, 7.5)}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R4", "at": mm(18.4, 7.5)}));
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["R3.1", "R3.2", "R4.1", "R4.2"]}));
    let before = s.project.clone();

    let o = exec(&r, &mut s, "route.nets", json!({"nets": ["A"], "layers": ["F.Cu"], "shove": false, "budget": "20s"}));
    assert_eq!(o["output"]["stats"]["failed"], 1, "{o}");
    let c = &o["output"]["connections"][0];
    let hint = c["hints"][0].as_str().unwrap();
    assert!(
        hint.starts_with("`route.connection R1.1 R2.1`") || hint.starts_with("`route.connection R2.1 R1.1`"),
        "{c}"
    );
    assert!(diag(&o, "route.incomplete")["hint"].as_str().unwrap().contains("route.connection"), "{o}");

    s.project = before;
    let o = exec(&r, &mut s, "route.nets", json!({"nets": ["A"], "layers": ["F.Cu"], "budget": "30s"}));
    assert_eq!(o["output"]["stats"]["failed"], 0, "{o}");
    assert_eq!(o["output"]["rerouted"], json!(["B"]), "{o}");
    let st = exec(&r, &mut s, "route.status", json!({}));
    assert_eq!(st["output"]["unrouted"], 0, "B is routed again: {st}");
    let drc = exec(&r, &mut s, "drc.run", json!({"severity": "error"}));
    assert_eq!(drc["output"]["errors"], 0, "{drc}");
}

// ---- Output: fab.export, circuit.list, BOM wording --------------------------------------------

/// A small routed board for the export and listing checks.
fn routed(r: &Registry, dir: &Path) -> Session {
    let mut s = new_project(r, dir, "p");
    led_board(r, &mut s);
    exec(r, &mut s, "place.auto", json!({}));
    let o = exec(r, &mut s, "route.all", json!({"budget": "30s"}));
    assert_eq!(o["output"]["stats"]["failed"], 0, "{o}");
    s
}

#[test]
fn fab_export_text_is_compact_and_relative() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = routed(&r, dir.path());
    let o = r.execute(&mut s, "fab.export", json!({"fab": "jlcpcb", "force": true}), RunOptions::default()).unwrap();
    let v = serde_json::to_value(&o).unwrap();
    assert_eq!(v["output"]["dir"], json!(Path::new("out/fab/jlcpcb").display().to_string()));
    for f in v["output"]["files"].as_array().unwrap() {
        let p = f["path"].as_str().unwrap();
        assert!(Path::new(p).is_relative(), "{p}");
        assert!(dir.path().join("p").join(p).is_file(), "{p}");
    }
    // One header line, then names only, a few per line; no absolute paths.
    assert!(o.summary.starts_with("jlcpcb ("), "{}", o.summary);
    assert!(o.summary.lines().next().unwrap().ends_with(&Path::new("out/fab/jlcpcb").display().to_string()));
    assert!(!o.summary.contains(&dir.path().display().to_string()), "{}", o.summary);
    assert!(o.summary.lines().all(|l| l.len() < 200), "{}", o.summary);
    assert!(o.summary.contains("fab-lock.json"), "{}", o.summary);
}

/// An imported board's placement keeps its own footprint until the part changes: `circuit.list`
/// shows that one, and the part's.
#[test]
fn circuit_list_shows_the_placed_footprint() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0603", "refdes": "R1"}));
    exec(&r, &mut s, "part.generic", json!({"spec": "R 1k 1% 0805"}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R1", "at": mm(10.0, 7.5)}));
    let other = s
        .project
        .as_ref()
        .unwrap()
        .library()
        .parts
        .get("R_1k_1pct_0805")
        .and_then(|p| p.footprint())
        .map(|f| f.footprint.clone())
        .unwrap();
    s.project.as_mut().unwrap().board_mut().footprints.get_mut("R1").unwrap().footprint = Some(other.clone());
    let o = r.execute(&mut s, "circuit.list", json!({}), RunOptions::default()).unwrap();
    let v = serde_json::to_value(&o).unwrap();
    let c = &v["output"]["components"][0];
    assert_eq!(c["footprint"], json!(other), "{c}");
    assert_eq!(c["part_footprint"], json!("RESC1608X55N"), "{c}");
    assert!(o.summary.contains(&format!("({other}; the part's: RESC1608X55N)")), "{}", o.summary);
}

/// A KiCad netlist with an `LCSC` field on one line and a part cadlab cannot resolve.
const LCSC_NET: &str = r#"(export (version "E")
  (design (source "/p/p.kicad_sch") (tool "Eeschema 8.0.0"))
  (components
    (comp (ref "R1") (value "10k") (footprint "Resistor_SMD:R_0402_1005Metric")
      (fields (field (name "LCSC") "C25744"))
      (libsource (lib "Device") (part "R") (description "Resistor")) (tstamps "r1"))
    (comp (ref "R2") (value "10k") (footprint "Resistor_SMD:R_0402_1005Metric")
      (fields (field (name "LCSC") "C25744"))
      (libsource (lib "Device") (part "R") (description "Resistor")) (tstamps "r2"))
    (comp (ref "U1") (value "MYSTERY") (footprint "Weird:Thing")
      (fields (field (name "LCSC Part #") "C999999"))
      (libsource (lib "Custom") (part "MYSTERY") (description "Something")) (tstamps "u1")))
  (libparts
    (libpart (lib "Device") (part "R") (description "Resistor")
      (pins (pin (num "1") (name "~") (type "passive")) (pin (num "2") (name "~") (type "passive"))))
    (libpart (lib "Custom") (part "MYSTERY") (description "Something")
      (pins (pin (num "1") (name "A") (type "passive")) (pin (num "2") (name "B") (type "passive")))))
  (nets
    (net (code "1") (name "A") (node (ref "R1") (pin "1") (pintype "passive")) (node (ref "U1") (pin "1") (pintype "passive")))
    (net (code "2") (name "B") (node (ref "R1") (pin "2") (pintype "passive")) (node (ref "R2") (pin "1") (pintype "passive")))
    (net (code "3") (name "C") (node (ref "R2") (pin "2") (pintype "passive")) (node (ref "U1") (pin "2") (pintype "passive")))))
"#;

#[test]
fn bom_names_placeholders_and_supplier_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    let path = dir.path().join("x.net");
    std::fs::write(&path, LCSC_NET).unwrap();
    exec(&r, &mut s, "circuit.import", json!({"path": path}));
    let o = r.execute(&mut s, "bom.list", json!({}), RunOptions::default()).unwrap();
    let v = serde_json::to_value(&o).unwrap();
    let lines = v["output"]["lines"].as_array().unwrap();
    let r_line = lines.iter().find(|l| l["refdes"] == json!(["R1", "R2"])).unwrap();
    assert_eq!(r_line["generic"], true);
    assert_eq!(r_line["skus"], json!({"lcsc": "C25744"}));
    let u_line = lines.iter().find(|l| l["refdes"] == json!(["U1"])).unwrap();
    assert_eq!(u_line["skus"], json!({"lcsc": "C999999"}));
    assert!(o.summary.contains("(generic, no MPN yet) [lcsc C25744]"), "{}", o.summary);
    assert!(o.summary.contains("(placeholder: no footprint or MPN yet) [lcsc C999999]"), "{}", o.summary);
}

/// Supplier part numbers from netlist fields fill the fab's catalog column (no supplier
/// configured), and are the first substitute candidate when the catalog has them.
#[test]
fn netlist_supplier_numbers_reach_the_fab_bom_and_substitutes() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    let path = dir.path().join("x.net");
    std::fs::write(
        &path,
        LCSC_NET.replace("(footprint \"Weird:Thing\")", "(footprint \"Resistor_SMD:R_0402_1005Metric\")"),
    )
    .unwrap();
    exec(&r, &mut s, "circuit.import", json!({"path": path}));
    // The mystery part gets a footprint so it can be placed.
    let fp = s.project.as_ref().unwrap().library().parts["R_10k_0402"].footprint().unwrap().footprint.clone();
    exec(&r, &mut s, "part.set", json!({"id": "MYSTERY", "footprint": fp}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.auto", json!({}));
    exec(&r, &mut s, "route.all", json!({"budget": "20s"}));

    let o = exec(&r, &mut s, "fab.export", json!({"fab": "jlcpcb", "force": true}));
    let d = diag(&o, "fab.sku_from_fields");
    assert!(d["message"].as_str().unwrap().contains("C25744"), "{d}");
    let out = dir.path().join("p/out/fab/jlcpcb");
    let bom = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().to_lowercase().contains("bom"))
        .unwrap();
    let csv = std::fs::read_to_string(&bom).unwrap();
    assert!(csv.contains("C25744") && csv.contains("C999999"), "{csv}");
    let lock: Value = serde_json::from_str(&std::fs::read_to_string(out.join("fab-lock.json")).unwrap()).unwrap();
    let pick = lock["bom"].as_array().unwrap().iter().find(|l| l["designators"] == json!(["R1", "R2"])).unwrap();
    assert_eq!((pick["sku"].clone(), pick["sku_from_fields"].clone()), (json!("C25744"), json!(true)), "{pick}");
    // `bom.export` in the JLCPCB layout carries them too.
    exec(&r, &mut s, "bom.export", json!({"path": "j.csv", "format": "jlcpcb"}));
    assert!(std::fs::read_to_string(dir.path().join("p/j.csv")).unwrap().contains("C25744"));

    // With the catalog configured, the field's part is the first substitute candidate for the
    // line without an MPN, and fab.substitute takes it by default.
    let c = |v: Value| -> Candidate { serde_json::from_value(v).unwrap() };
    let lcsc = vec![
        c(json!({"sku": "C25744", "manufacturer": "Uniroyal", "mpn": "0402WGF1002TCE", "category": "resistor",
            "package": "0402", "params": {"resistance": "10k", "tolerance": "1%"}, "stock": 100000,
            "prices": [{"qty": 1, "price": "0.001 USD"}], "lifecycle": "active"})),
        c(json!({"sku": "C11702", "manufacturer": "Yageo", "mpn": "RC0402FR-0710KL", "category": "resistor",
            "package": "0402", "params": {"resistance": "10k", "tolerance": "1%"}, "stock": 100000,
            "prices": [{"qty": 1, "price": "0.0005 USD"}], "lifecycle": "active"})),
    ];
    s.suppliers = Suppliers::new().with(Arc::new(Catalog::from_parts("lcsc", lcsc)));
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "jlcpcb"}));
    let line = o["output"]["substitutes"].as_array().unwrap().iter().find(|l| l["part"] == "R_10k_0402");
    if let Some(line) = line {
        assert_eq!(line["candidates"][0]["basis"], "field", "{line}");
        assert_eq!(line["candidates"][0]["offer"]["sku"], "C25744", "{line}");
    }
    let o = exec(&r, &mut s, "fab.substitute", json!({"fab": "jlcpcb", "part": "R_10k_0402"}));
    assert_eq!(o["output"]["substitution"]["mpn"], "0402WGF1002TCE", "{o}");
    assert_eq!(o["output"]["substitution"]["basis"], "field", "{o}");
}
