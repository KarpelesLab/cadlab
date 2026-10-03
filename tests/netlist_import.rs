//! KiCad netlist import (`circuit.import`, M7): round trips through cadlab's own KiCad netlist
//! export, part resolution, KiCad conventions (sheet paths, power flags, unconnected pins) and
//! errors. The KiCad oracle round trip (schematic → `kicad-cli` netlist → import) is in
//! `tests/kicad_oracle.rs`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use cadlab::command::{Registry, RunOptions, Session};
use common::boards::{build_board, build_stm32_board};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn new_project(r: &Registry, dir: &std::path::Path, name: &str) -> Session {
    let mut s = Session::new();
    exec(r, &mut s, "project.new", json!({"path": dir.join(name), "name": name}));
    s
}

type Connectivity = BTreeMap<String, BTreeSet<String>>;

fn connectivity(s: &Session) -> Connectivity {
    let c = s.project.as_ref().unwrap().circuit();
    c.nets.iter().map(|(n, net)| (n.clone(), net.pins.iter().map(ToString::to_string).collect())).collect()
}

/// Designator → (value, footprint, MPN).
fn components(s: &Session) -> BTreeMap<String, (String, Option<String>, Option<String>)> {
    let p = s.project.as_ref().unwrap();
    p.circuit()
        .components
        .iter()
        .map(|(r, c)| {
            let part = &p.library().parts[&c.part];
            (r.clone(), (part.value(), part.footprint().map(|f| f.footprint.clone()), part.mpn.clone()))
        })
        .collect()
}

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

/// Export → import into a fresh project → identical connectivity, values and designators.
fn round_trip(build: fn(&Registry, &mut Session)) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut a = new_project(&r, dir.path(), "a");
    build(&r, &mut a);
    exec(&r, &mut a, "bom.dnp", json!({"refdes": ["C1"]}));
    let net = dir.path().join("a.net");
    exec(&r, &mut a, "circuit.export", json!({"path": net}));

    let mut b = new_project(&r, dir.path(), "b");
    let o = exec(&r, &mut b, "circuit.import", json!({"path": net}));
    assert_eq!(connectivity(&a), connectivity(&b));
    assert_eq!(o["output"]["components"], a.project.as_ref().unwrap().circuit().components.len());
    assert_eq!(o["output"]["nets"], a.project.as_ref().unwrap().circuit().nets.len());
    let (ca, cb) = (components(&a), components(&b));
    assert_eq!(ca.keys().collect::<Vec<_>>(), cb.keys().collect::<Vec<_>>());
    for (k, (va, fa, ma)) in &ca {
        let (vb, fb, mb) = &cb[k];
        assert_eq!(va, vb, "{k} value");
        assert_eq!(ma, mb, "{k} MPN");
        // Generic passives and chip-size parts get the same generated footprint back.
        if fb.is_some() {
            assert_eq!(fa, fb, "{k} footprint");
        }
    }
    assert!(b.project.as_ref().unwrap().bom().dnp.contains("C1"));
    // Pin names and types come back from the libparts section.
    let (pa, pb) = (a.project.as_ref().unwrap(), b.project.as_ref().unwrap());
    for (refdes, comp) in &pa.circuit().components {
        let pins = |p: &cadlab::model::Project, part: &str| -> Vec<(String, String, String)> {
            p.library().parts[part]
                .symbol
                .pins
                .iter()
                .map(|q| (q.number.clone(), q.label().to_string(), format!("{:?}", q.kind)))
                .collect()
        };
        let mut x = pins(pa, &comp.part);
        let mut y = pins(pb, &pb.circuit().components[refdes].part);
        x.sort();
        y.sort();
        assert_eq!(x, y, "{refdes} pins");
    }
    // The re-exported nets section is the same, except that a pin name equal to the pin
    // number (`(pin "1") (pinfunction "1")`) is read as no name.
    exec(&r, &mut b, "circuit.export", json!({"path": dir.path().join("b.net")}));
    let ta = std::fs::read_to_string(&net).unwrap();
    let tb = std::fs::read_to_string(dir.path().join("b.net")).unwrap();
    let nets = |t: &str| {
        t[t.find("  (nets").unwrap()..]
            .lines()
            .map(|l| {
                let Some((_, rest)) = l.split_once("(pin \"") else { return l.to_string() };
                let num = &rest[..rest.find('"').unwrap()];
                l.replace(&format!(" (pinfunction \"{num}\")"), "")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(nets(&ta), nets(&tb));

    // Into the original project, replacing its circuit: every part is found again.
    let before = connectivity(&a);
    let o = exec(&r, &mut a, "circuit.import", json!({"path": net, "replace": true}));
    assert_eq!(connectivity(&a), before);
    for p in o["output"]["parts"].as_array().unwrap() {
        assert_eq!(p["resolution"], "existing", "{p}");
        assert_eq!(p["created"], false);
    }
    assert!(codes(&o).is_empty(), "{:#}", o["diagnostics"]);
}

#[test]
fn round_trip_attiny() {
    round_trip(build_board);
}

#[test]
fn round_trip_stm32() {
    round_trip(build_stm32_board);
}

/// A netlist in the shape `kicad-cli` writes for a KiCad project using KiCad's own symbols:
/// sheet paths, power symbols and flags, unconnected pins, MPN fields, a sub-sheet.
const KICAD_NET: &str = r##"(export (version "E")
  (design (source "/home/u/proj/proj.kicad_sch") (date "2026-10-04T10:00:00") (tool "Eeschema 9.0.0"))
  (components
    (comp (ref "R1") (value "10k") (footprint "Resistor_SMD:R_0402_1005Metric")
      (libsource (lib "Device") (part "R") (description "Resistor"))
      (fields (field (name "Footprint") "Resistor_SMD:R_0402_1005Metric") (field (name "Datasheet") "~"))
      (sheetpath (names "/") (tstamps "/")) (tstamps "a1"))
    (comp (ref "R2") (value "10k") (footprint "Resistor_SMD:R_0402_1005Metric")
      (libsource (lib "Device") (part "R") (description "Resistor"))
      (property (name "dnp") (value ""))
      (sheetpath (names "/") (tstamps "/")) (tstamps "a2"))
    (comp (ref "D1") (value "LED") (footprint "LED_SMD:LED_0603_1608Metric")
      (libsource (lib "Device") (part "LED") (description "Light emitting diode"))
      (sheetpath (names "/") (tstamps "/")) (tstamps "a3"))
    (comp (ref "U1") (value "AP2112K-3.3") (footprint "Package_TO_SOT_SMD:SOT-23-5")
      (libsource (lib "Regulator_Linear") (part "AP2112K-3.3") (description "600mA LDO"))
      (fields (field (name "MPN") "AP2112K-3.3TRG1") (field (name "Manufacturer") "Diodes Inc") (field (name "LCSC") "C51118"))
      (sheetpath (names "/power/") (tstamps "/b1/")) (tstamps "a4"))
    (comp (ref "J1") (value "Conn_01x03") (footprint "Connector_JST:JST_PH_B3B-PH-K_1x03_P2.00mm_Vertical")
      (libsource (lib "Connector_Generic") (part "Conn_01x03") (description "Generic connector"))
      (sheetpath (names "/") (tstamps "/")) (tstamps "a5"))
    (comp (ref "J2") (value "SENSE_OUT") (footprint "Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical")
      (libsource (lib "Connector_Generic") (part "Conn_01x02") (description "Generic connector"))
      (sheetpath (names "/") (tstamps "/")) (tstamps "a6"))
    (comp (ref "#PWR01") (value "GND") (libsource (lib "power") (part "GND")))
    (comp (ref "#FLG01") (value "PWR_FLAG") (libsource (lib "power") (part "PWR_FLAG"))))
  (libparts
    (libpart (lib "Device") (part "R") (description "Resistor")
      (pins (pin (num "1") (name "~") (type "passive")) (pin (num "2") (name "~") (type "passive"))))
    (libpart (lib "Device") (part "LED")
      (pins (pin (num "1") (name "K") (type "passive")) (pin (num "2") (name "A") (type "passive"))))
    (libpart (lib "Regulator_Linear") (part "AP2112K-3.3")
      (pins (pin (num "1") (name "VIN") (type "power_in")) (pin (num "2") (name "GND") (type "power_in"))
            (pin (num "3") (name "EN") (type "input")) (pin (num "4") (name "NC") (type "no_connect"))
            (pin (num "5") (name "VOUT") (type "power_out"))))
    (libpart (lib "Connector_Generic") (part "Conn_01x03")
      (pins (pin (num "1") (name "Pin_1") (type "passive")) (pin (num "2") (name "Pin_2") (type "passive"))
            (pin (num "3") (name "Pin_3") (type "passive")))))
  (nets
    (net (code "1") (name "GND") (class "Default")
      (node (ref "#PWR01") (pin "1") (pintype "power_in"))
      (node (ref "J1") (pin "2") (pinfunction "Pin_2") (pintype "passive"))
      (node (ref "U1") (pin "2") (pinfunction "GND") (pintype "power_in"))
      (node (ref "D1") (pin "1") (pinfunction "K") (pintype "passive"))
      (node (ref "J2") (pin "2") (pintype "passive")))
    (net (code "2") (name "/VBUS") (class "Default")
      (node (ref "#FLG01") (pin "1") (pintype "power_out"))
      (node (ref "J1") (pin "1") (pinfunction "Pin_1") (pintype "passive"))
      (node (ref "U1") (pin "1") (pinfunction "VIN") (pintype "power_in"))
      (node (ref "U1") (pin "3") (pinfunction "EN") (pintype "input")))
    (net (code "3") (name "/power/3V3") (class "Power")
      (node (ref "U1") (pin "5") (pinfunction "VOUT") (pintype "power_out"))
      (node (ref "R1") (pin "1") (pintype "passive"))
      (node (ref "R2") (pin "1") (pintype "passive")))
    (net (code "4") (name "Net-(D1-A)") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive"))
      (node (ref "D1") (pin "2") (pinfunction "A") (pintype "passive")))
    (net (code "5") (name "/SENSE") (class "Default")
      (node (ref "R2") (pin "2") (pintype "passive"))
      (node (ref "J1") (pin "3") (pinfunction "Pin_3") (pintype "passive"))
      (node (ref "J2") (pin "1") (pintype "passive")))
    (net (code "6") (name "unconnected-(U1-NC-Pad4)") (class "Default")
      (node (ref "U1") (pin "4") (pinfunction "NC") (pintype "no_connect+no_connect")))))
"##;

#[test]
fn kicad_project_netlist() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    let path = dir.path().join("kicad.net");
    std::fs::write(&path, KICAD_NET).unwrap();

    // Dry run: report, but no change.
    let o = r.execute(&mut s, "circuit.import", json!({"path": path}), RunOptions { dry_run: true }).unwrap();
    let o = serde_json::to_value(&o).unwrap();
    assert_eq!(o["output"]["components"], 6);
    assert!(s.project.as_ref().unwrap().circuit().components.is_empty());
    assert!(s.project.as_ref().unwrap().library().parts.is_empty());

    let o = exec(&r, &mut s, "circuit.import", json!({"path": path}));
    let out = &o["output"];
    assert_eq!(out["source"], "/home/u/proj/proj.kicad_sch");
    assert_eq!(out["components"], 6);
    assert_eq!(out["nets"], 5);
    assert_eq!(out["unconnected_skipped"], 1);
    assert_eq!(out["power_symbols"], 2);
    let parts: BTreeMap<String, (String, Vec<String>)> = out["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let refs = p["refdes"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
            (p["part"].as_str().unwrap().to_string(), (p["resolution"].as_str().unwrap().to_string(), refs))
        })
        .collect();
    assert_eq!(parts["R_10k_0402"], ("generic".into(), vec!["R1".into(), "R2".into()]));
    assert_eq!(parts["LED_0603"], ("generic".into(), vec!["D1".into()]));
    // MPN and a package cadlab can generate: a concrete part.
    assert_eq!(parts["AP2112K-3.3TRG1"], ("created".into(), vec!["U1".into()]));
    // No MPN, but a footprint cadlab can generate: a (generic) part like any other.
    assert_eq!(parts["SENSE_OUT"], ("created".into(), vec!["J2".into()]));
    // Footprint unknown to cadlab: a placeholder, which cannot be placed yet.
    assert_eq!(parts["Conn_01x03"], ("placeholder".into(), vec!["J1".into()]));
    let c = codes(&o);
    assert_eq!(c.iter().filter(|x| *x == "import.placeholder_part").count(), 1, "{c:?}");
    assert!(c.contains(&"import.unknown_netclass".to_string()), "{c:?}");

    let p = s.project.as_ref().unwrap();
    let circuit = p.circuit();
    let names: Vec<&str> = circuit.nets.keys().map(String::as_str).collect();
    assert_eq!(names, ["GND", "Net-(D1-A)", "SENSE", "VBUS", "power/3V3"]);
    // Power flags are not read (kicad-cli does not write them); nothing is marked driven.
    assert!(circuit.nets.values().all(|n| !n.driven));
    let u1 = &p.library().parts["AP2112K-3.3TRG1"];
    assert_eq!(u1.manufacturer.as_deref(), Some("Diodes Inc"));
    assert_eq!(u1.footprint().unwrap().footprint, "SOT95P280X145-5N");
    assert_eq!(u1.symbol.pin("3").unwrap().name, "EN");
    assert_eq!(format!("{:?}", u1.symbol.pin("5").unwrap().kind), "PowerOut");
    // Other fields stay on the component; MPN and manufacturer moved to the part.
    assert_eq!(circuit.components["U1"].properties, BTreeMap::from([("LCSC".to_string(), "C51118".to_string())]));
    assert!(p.bom().dnp.contains("R2"));
    assert_eq!(p.library().parts["Conn_01x03"].symbol.pins.len(), 3);

    // Again without `replace`: the designators are taken.
    let f = r.execute(&mut s, "circuit.import", json!({"path": path}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "import.refdes_taken");
    assert!(f.error.diagnostic.hint.as_deref().unwrap().contains("replace"));
    // With `replace`: the parts created the first time are found again.
    let o = exec(&r, &mut s, "circuit.import", json!({"path": path, "replace": true}));
    for p in o["output"]["parts"].as_array().unwrap() {
        assert_eq!(p["created"], false, "{p}");
    }
    assert_eq!(o["output"]["replaced"], 6);
}

#[test]
fn errors() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = new_project(&r, dir.path(), "p");
    let mut run = |text: &str| {
        let path = dir.path().join("x.net");
        std::fs::write(&path, text).unwrap();
        r.execute(&mut s, "circuit.import", json!({"path": path}), RunOptions::default()).unwrap_err().error
    };
    let e = run("(export (version \"E\") (components");
    assert_eq!(e.diagnostic.code, "import.parse");
    assert!(e.diagnostic.hint.is_some());
    assert_eq!(run("(kicad_sch (version 1))").diagnostic.code, "import.not_kicad_netlist");
    assert_eq!(run("(export (version \"E\") (components) (nets))").diagnostic.code, "import.empty");
    let e = run("(export (components (comp (ref \"R?\") (value \"1k\"))))");
    assert_eq!(e.diagnostic.code, "import.invalid_refdes");
    assert!(e.diagnostic.hint.as_deref().unwrap().contains("annotate"));
    let f = r.execute(&mut s, "circuit.import", json!({"path": "missing.net"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "io", "{}", f.error);
}

#[test]
fn library_parts_are_matched_by_mpn_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let path = dir.path().join("kicad.net");
    std::fs::write(&path, KICAD_NET).unwrap();
    let ldo = |id: &str, pins: Value| json!({"id": id, "category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5", "pins": pins});
    // The netlist's AP2112K: found by MPN.
    let mut s = new_project(&r, dir.path(), "p");
    let all = json!([{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
        {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
        {"number": "5", "name": "VOUT", "kind": "power_out"}]);
    exec(&r, &mut s, "part.create", ldo("LDO_A", all));
    let o = exec(&r, &mut s, "circuit.import", json!({"path": path}));
    let u1 = o["output"]["parts"].as_array().unwrap().iter().find(|p| p["refdes"][0] == "U1").unwrap().clone();
    assert_eq!(u1, json!({"part": "LDO_A", "resolution": "mpn", "created": false, "refdes": ["U1"]}));

    // A part with that MPN but missing a connected pin is not used.
    let mut s = new_project(&r, dir.path(), "q");
    let three = json!([{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
        {"number": "3", "name": "VOUT", "kind": "power_out"}]);
    let mut args = ldo("LDO_B", three);
    args["package"] = json!("SOT-23");
    exec(&r, &mut s, "part.create", args);
    let o = exec(&r, &mut s, "circuit.import", json!({"path": path}));
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "import.pin_mismatch").unwrap();
    assert!(d["message"].as_str().unwrap().contains("LDO_B"), "{d}");
    assert!(d["message"].as_str().unwrap().contains("pin(s) 5"), "{d}");
    assert_eq!(s.project.as_ref().unwrap().circuit().components["U1"].part, "AP2112K-3.3TRG1");
}
