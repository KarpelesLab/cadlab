//! M2: nets, ERC and netlist export, driven through the command registry.

mod common;

use std::path::Path;

use cadlab::command::{Registry, RunOptions, Session};
use common::boards::{build_board, pins};
use common::golden::assert_golden;
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

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

fn new_project(r: &Registry) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    exec(r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "attiny-blinky"}));
    (dir, s)
}

/// M2 exit: a complete MCU board described with commands, ERC clean, netlist exported.
#[test]
fn attiny_board_erc_clean_and_netlist() {
    let r = Registry::with_builtins();
    let (dir, mut s) = new_project(&r);
    build_board(&r, &mut s);

    // First ERC: VBUS comes from a connector and is not marked driven; PB4 is floating.
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    let c = codes(&o);
    assert!(c.contains(&"erc.power_not_driven".to_string()), "{c:?}");
    assert!(c.contains(&"erc.unconnected".to_string()), "{c:?}");
    let pnd = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "erc.power_not_driven").unwrap();
    assert!(pnd["message"].as_str().unwrap().contains("VBUS"));
    assert!(pnd["hint"].as_str().unwrap().contains("--driven"), "connector-aware hint");
    // GND has only ground power pins: no error for it.
    assert!(!o["diagnostics"].as_array().unwrap().iter().any(|d| d["message"].as_str().unwrap().contains("`GND`")));

    exec(&r, &mut s, "net.set", json!({"nets": ["VBUS"], "driven": true}));
    exec(&r, &mut s, "net.no_connect", json!({"pins": ["U2.PB4"]}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert_eq!(o["output"], json!({"errors": 0, "warnings": 0}), "{:?}", o["diagnostics"]);
    assert_eq!(o["summary"], "ERC clean");

    // Net classes.
    exec(&r, &mut s, "netclass.set", json!({"name": "power", "track_width": "0.5mm", "clearance": "0.2mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["VBUS", "3V3", "GND"], "class": "power"}));

    // Netlists.
    exec(&r, &mut s, "circuit.export", json!({"path": "out/board.net"}));
    exec(&r, &mut s, "circuit.export", json!({"path": "out/board.json", "format": "json"}));
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/attiny85");
    assert_golden(&golden.join("board.net"), &std::fs::read_to_string(dir.path().join("p/out/board.net")).unwrap());
    let summary = exec(&r, &mut s, "circuit.summary", json!({}));
    assert_golden(&golden.join("summary.txt"), summary["output"]["text"].as_str().unwrap());
    let j: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("p/out/board.json")).unwrap()).unwrap();
    assert_eq!(j["components"].as_array().unwrap().len(), 10);
    assert_eq!(j["nets"].as_array().unwrap().len(), 9);

    // Saved circuit file is stable and readable.
    s.save().unwrap();
    assert_golden(&golden.join("circuit.json"), &std::fs::read_to_string(dir.path().join("p/circuit.json")).unwrap());
    let (s2, _) = Session::open(&dir.path().join("p")).unwrap();
    assert_eq!(s2.project, s.project);
}

fn mcu_with_port(r: &Registry, s: &mut Session) {
    let mut pin_list: Vec<(String, String, &str)> =
        vec![("1".into(), "VDD".into(), "power_in"), ("2".into(), "VSS".into(), "power_in")];
    for i in 0..8 {
        pin_list.push(((3 + i).to_string(), format!("PA{i}"), "bidirectional"));
    }
    let p: Vec<(&str, &str, &str)> = pin_list.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), *c)).collect();
    exec(r, s, "part.create", json!({"id": "MCU8", "category": "mcu", "package": "SOIC-16W", "pins": pins(&p)}));
    exec(r, s, "part.create", json!({"id": "HDR8", "category": "connector", "package": "PinHeader 1x08"}));
    exec(r, s, "circuit.add", json!({"part": "MCU8"}));
    exec(r, s, "circuit.add", json!({"part": "HDR8"}));
}

#[test]
fn bus_connect_and_errors() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    mcu_with_port(&r, &mut s);
    let o = exec(&r, &mut s, "net.connect", json!({"net": "DATA[0..7]", "pins": ["U1.PA0..PA7", "J1.1..8"]}));
    assert_eq!(o["output"]["nets"].as_array().unwrap().len(), 8);
    let o = exec(&r, &mut s, "net.show", json!({"net": "DATA3"}));
    let p: Vec<&str> = o["output"]["pins"].as_array().unwrap().iter().map(|x| x["pin"].as_str().unwrap()).collect();
    // Pins are stored by number: PA3 is pin 6.
    assert_eq!(p, ["J1.4", "U1.6"]);

    // Width mismatch.
    let f = fail(&r, &mut s, "net.connect", json!({"net": "X[0..3]", "pins": ["U1.PA0..PA2"]}));
    assert_eq!(f.error.diagnostic.code, "net.bus_width");
    // Moving a pin to another net needs merge.
    let f = fail(&r, &mut s, "net.connect", json!({"net": "DATA0", "pins": ["U1.PA1"]}));
    assert_eq!(f.error.diagnostic.code, "net.would_merge");
    let o = exec(&r, &mut s, "net.connect", json!({"net": "DATA0", "pins": ["U1.PA1"], "merge": true}));
    assert_eq!(o["output"]["merged"], json!(["DATA1"]));
    // Pin errors with suggestions.
    let f = fail(&r, &mut s, "net.connect", json!({"net": "N", "pins": ["U1.PA9"]}));
    assert_eq!(f.error.diagnostic.code, "pin.not_found");
    let f = fail(&r, &mut s, "net.connect", json!({"net": "N", "pins": ["U9.1"]}));
    assert_eq!(f.error.diagnostic.code, "component.not_found");
    // By-name connects every pin with that name; ground-only power net is fine.
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.VSS"]}));

    // No-connect on a connected pin is refused; disconnect then mark.
    let f = fail(&r, &mut s, "net.no_connect", json!({"pins": ["U1.PA2"]}));
    assert_eq!(f.error.diagnostic.code, "net.pin_connected");
    exec(&r, &mut s, "net.disconnect", json!({"pins": ["U1.PA2"]}));
    exec(&r, &mut s, "net.no_connect", json!({"pins": ["U1.PA2"]}));

    // Rename component: nets follow.
    exec(&r, &mut s, "circuit.rename", json!({"from": "J1", "to": "J5"}));
    let o = exec(&r, &mut s, "net.show", json!({"net": "DATA7"}));
    assert!(o["output"]["pins"].as_array().unwrap().iter().any(|p| p["pin"] == "J5.8"));
    // Removing a component detaches it; nets left empty disappear.
    exec(&r, &mut s, "circuit.remove", json!({"refdes": ["J5"]}));
    let o = exec(&r, &mut s, "net.list", json!({}));
    assert!(
        o["output"]["nets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| { !n["pins"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().starts_with("J5")) })
    );
    // Single-pin nets are warned about.
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert!(codes(&o).contains(&"erc.single_pin_net".to_string()));
}

#[test]
fn erc_output_conflicts() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "BUF", "category": "ic", "package": "SOT-23-5",
        "pins": pins(&[("1", "A", "input"), ("2", "GND", "power_in"), ("3", "B", "input"), ("4", "Y", "output"), ("5", "VCC", "power_in")])}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "BUF", "count": 2}));
    exec(&r, &mut s, "net.connect", json!({"net": "Y", "pins": ["U1.Y", "U2.Y"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "IN", "pins": ["U1.A", "U2.A"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "VCC", "pins": ["U1.VCC", "U2.VCC"]}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    let c = codes(&o);
    assert!(c.contains(&"erc.output_conflict".to_string()), "{c:?}");
    assert!(c.contains(&"erc.input_not_driven".to_string()), "{c:?}");
    assert!(c.contains(&"erc.power_not_driven".to_string()), "{c:?}");
    assert!(c.contains(&"erc.power_unconnected".to_string()), "GND pins unconnected: {c:?}");
    // bom.replace with a part lacking pins drops those connections.
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "BUF4", "category": "ic", "package": "SOT-23-5",
        "pins": pins(&[("1", "A", "input"), ("2", "GND", "power_in"), ("3", "B", "input"), ("5", "VCC", "power_in")]),
        "pin_map": {}}),
    );
    let o = exec(&r, &mut s, "bom.replace", json!({"from": "BUF", "to": "BUF4", "refdes": ["U2"]}));
    assert!(codes(&o).contains(&"bom.pins_differ".to_string()));
    let o = exec(&r, &mut s, "net.show", json!({"net": "Y"}));
    assert_eq!(o["output"]["pins"].as_array().unwrap().len(), 1);
}

#[test]
fn blocks_capture_and_instantiate() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    build_board(&r, &mut s);
    exec(&r, &mut s, "net.set", json!({"nets": ["VBUS"], "driven": true}));
    exec(&r, &mut s, "net.no_connect", json!({"pins": ["U2.PB4"]}));

    // LED + resistor: LED_A is internal, LED_DRIVE and GND leave the block.
    let o = exec(
        &r,
        &mut s,
        "block.create",
        json!({"name": "status_led", "components": ["R2", "D1"], "description": "LED with series resistor"}),
    );
    assert_eq!(o["output"]["ports"], json!(["GND", "LED_DRIVE"]));
    assert_eq!(o["output"]["internal_nets"], json!(["LED_A"]));

    let o = exec(
        &r,
        &mut s,
        "block.instantiate",
        json!({"block": "status_led", "instance": "LED2", "connect": {"LED_DRIVE": "LED2_DRIVE"}}),
    );
    assert_eq!(o["output"]["components"], json!({"D1": "D2", "R2": "R3"}));
    assert_eq!(o["output"]["nets"], json!({"GND": "GND", "LED_A": "LED2/LED_A", "LED_DRIVE": "LED2_DRIVE"}));
    let o = exec(&r, &mut s, "net.show", json!({"net": "LED2/LED_A"}));
    let p: Vec<&str> = o["output"]["pins"].as_array().unwrap().iter().map(|x| x["pin"].as_str().unwrap()).collect();
    assert_eq!(p, ["D2.2", "R3.2"]);

    // Drive it from the spare pin (its no-connect mark is cleared, with a note).
    let o = exec(&r, &mut s, "net.connect", json!({"net": "LED2_DRIVE", "pins": ["U2.PB4"]}));
    assert!(codes(&o).contains(&"net.nc_cleared".to_string()));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert_eq!(o["output"], json!({"errors": 0, "warnings": 0}), "{:?}", o["diagnostics"]);

    // Errors.
    let f = fail(&r, &mut s, "block.instantiate", json!({"block": "status_led", "instance": "LED2"}));
    assert_eq!(f.error.diagnostic.code, "block.instance_exists");
    let f = fail(
        &r,
        &mut s,
        "block.instantiate",
        json!({"block": "status_led", "instance": "X", "connect": {"LED_DRIV": "N"}}),
    );
    assert_eq!(f.error.diagnostic.hint.as_deref(), Some("did you mean `LED_DRIVE`?"));
    let f = fail(&r, &mut s, "block.instantiate", json!({"block": "status_lde", "instance": "X"}));
    assert_eq!(f.error.diagnostic.code, "block.not_found");

    // A block with instances cannot be removed until they are gone.
    let f = fail(&r, &mut s, "block.remove", json!({"name": "status_led"}));
    assert_eq!(f.error.diagnostic.code, "block.in_use");
    exec(&r, &mut s, "circuit.remove", json!({"refdes": ["R3", "D2"]}));
    let o = exec(&r, &mut s, "block.list", json!({}));
    assert_eq!(o["output"]["blocks"][0]["instances"], json!([]));
    exec(&r, &mut s, "block.remove", json!({"name": "status_led"}));

    // CLI-style alias works through the registry name; the block survives a save/load.
    exec(&r, &mut s, "block.create", json!({"name": "ldo", "components": ["U1", "C1", "C2"]}));
    s.save().unwrap();
    let root = s.root().unwrap().to_path_buf();
    let (s2, _) = Session::open(&root).unwrap();
    assert_eq!(s2.project.as_ref().unwrap().circuit().blocks["ldo"].ports, ["3V3", "GND", "VBUS"]);
}
