//! M8 advanced electrical: stackup dielectrics, impedance calculator, IPC-2152 current checks,
//! SPICE export (ngspice oracle) and the design lint, through the command registry.

mod common;

use std::path::Path;

use cadlab::command::{Registry, RunOptions, Session};
use common::boards::{build_board, pins};
use common::golden::assert_golden;
use common::oracle::{self, Oracle};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd).error.diagnostic.code.to_string()
}

fn codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

fn new_project(r: &Registry, name: &str) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    exec(r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": name}));
    (dir, s)
}

/// "50.12Ω" → 50.12
fn ohms(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches('Ω').parse().unwrap()
}

/// "0.352mm" → 0.352
fn mm(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches("mm").parse().unwrap()
}

#[test]
fn stackup_and_impedance() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r, "rf");

    // Default two-layer board: dielectrics assumed (1.6 mm minus copper, εr 4.5).
    let o = exec(&r, &mut s, "board.stackup", json!({}));
    assert_eq!(o["output"]["assumed"], true);
    assert_eq!(o["output"]["dielectrics"][0]["thickness"], "1.53mm");
    assert!(codes(&o).contains(&"board.stackup_assumed".to_string()));
    let o = exec(&r, &mut s, "impedance.calc", json!({"width": "0.3mm"}));
    assert!(codes(&o).contains(&"impedance.stackup_assumed".to_string()));
    assert_eq!(o["output"]["geometry"]["model"], "microstrip");

    // A four-layer stackup (0.2 mm prepreg, 1.065 mm core).
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    exec(&r, &mut s, "board.dielectric", json!({"thickness": "0.2mm", "er": "4.4", "material": "prepreg"}));
    let o = exec(
        &r,
        &mut s,
        "board.dielectric",
        json!({"gap": 2, "thickness": "1.065mm", "er": "4.6", "material": "core"}),
    );
    assert_eq!(o["output"]["assumed"], false);
    assert_eq!(o["output"]["layers_thickness"], "1.57mm");
    assert!(codes(&o).is_empty(), "{:?}", o["diagnostics"]);
    assert_eq!(o["output"]["copper"][1]["model"], "stripline");
    assert_eq!(fail(&r, &mut s, "board.dielectric", json!({"gap": 4, "er": "4"})), "board.invalid_gap");
    assert_eq!(fail(&r, &mut s, "board.dielectric", json!({"gap": 1, "er": "0.5"})), "board.invalid_dielectric");

    // 50 Ω microstrip on F.Cu, written into a net class.
    let o = exec(&r, &mut s, "impedance.solve", json!({"target": "50ohm", "layer": "F.Cu", "netclass": "rf"}));
    let out = &o["output"];
    let w = mm(&out["width"]);
    assert!((0.3..0.42).contains(&w), "{out}");
    assert!((ohms(&out["z0"]) - 50.0).abs() < 0.3, "{out}");
    assert!(codes(&o).is_empty(), "{:?}", o["diagnostics"]);
    // Cross-check against the IPC-2141 microstrip estimate (±8 %).
    let ipc = 87.0 / (4.4f64 + 1.41).sqrt() * (5.98 * 0.2 / (0.8 * w + 0.035)).ln();
    assert!((ipc - 50.0).abs() < 4.0, "IPC-2141 gives {ipc} at {w} mm");
    let class = exec(&r, &mut s, "netclass.show", json!({"name": "rf"}));
    assert_eq!(class["output"]["track_width"], out["width"]);
    assert_eq!(class["output"]["impedance"], "50Ω");

    // 90 Ω differential pair, 0.15 mm gap.
    let o = exec(&r, &mut s, "impedance.solve", json!({"target": "90", "gap": "0.15mm", "netclass": "usb"}));
    assert!((ohms(&o["output"]["zdiff"]) - 90.0).abs() < 0.5, "{}", o["output"]);
    let class = exec(&r, &mut s, "netclass.show", json!({"name": "usb"}));
    assert_eq!(class["output"]["diff_pair_gap"], "0.15mm");
    assert_eq!(class["output"]["diff_pair_width"], o["output"]["width"]);
    assert_eq!(class["output"]["diff_impedance"], "90Ω");
    // Gap from the class.
    let o2 = exec(&r, &mut s, "impedance.solve", json!({"target": "90", "differential": true, "netclass": "usb"}));
    assert_eq!(o2["output"]["width"], o["output"]["width"]);

    // Stripline on In1.Cu between F.Cu and In2.Cu: narrower than the microstrip for 50 Ω.
    let o = exec(&r, &mut s, "impedance.solve", json!({"target": "50", "layer": "In1.Cu"}));
    assert_eq!(o["output"]["geometry"]["model"], "stripline");
    assert_eq!(o["output"]["geometry"]["references"], json!(["F.Cu", "In2.Cu"]));
    assert!(mm(&o["output"]["width"]) < w);

    // Explicit geometry: Pozar's example 3.7 (εr 2.2, 1.59 mm, 4.9 mm wide is 50 Ω).
    let o =
        exec(&r, &mut s, "impedance.calc", json!({"width": "4.9mm", "height": "1.59mm", "er": "2.2", "copper": "0mm"}));
    assert!((ohms(&o["output"]["z0"]) - 50.0).abs() < 0.75, "{}", o["output"]);
    assert!(o["output"].get("layer").is_none());

    // Errors.
    assert_eq!(fail(&r, &mut s, "impedance.calc", json!({"width": "0.2mm", "layer": "In5.Cu"})), "layer.not_found");
    assert_eq!(fail(&r, &mut s, "impedance.solve", json!({"target": "500"})), "impedance.unreachable");
    assert_eq!(fail(&r, &mut s, "impedance.solve", json!({"target": "2A"})), "value.wrong_unit");
    assert_eq!(fail(&r, &mut s, "impedance.solve", json!({"target": "90", "differential": true})), "impedance.no_gap");
    assert_eq!(
        fail(&r, &mut s, "impedance.calc", json!({"width": "0.2mm", "model": "embedded_microstrip"})),
        "impedance.invalid_geometry"
    );
    let o = exec(
        &r,
        &mut s,
        "impedance.calc",
        json!({"width": "0.2mm", "model": "embedded_microstrip", "height2": "0.1mm"}),
    );
    assert_eq!(o["output"]["geometry"]["model"], "embedded_microstrip");

    // Changing the layer count drops the dielectrics.
    let o = exec(&r, &mut s, "board.setup", json!({"layers": 6}));
    assert!(codes(&o).contains(&"board.dielectrics_cleared".to_string()));
}

/// Two resistors joined by net `SIG` on a 30 × 20 mm board, ready for tracks.
fn two_resistor_board(r: &Registry, s: &mut Session) {
    exec(r, s, "circuit.add", json!({"part": "R 10k 1% 0603", "count": 2}));
    exec(r, s, "net.connect", json!({"net": "SIG", "pins": ["R1.1", "R2.1"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["R1.2", "R2.2"]}));
    exec(r, s, "board.outline", json!({"width": "30mm", "height": "20mm"}));
    exec(r, s, "place.set", json!({"refdes": "R1", "at": ["5mm", "10mm"]}));
    exec(r, s, "place.set", json!({"refdes": "R2", "at": ["25mm", "10mm"]}));
}

#[test]
fn current_and_impedance_drc() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r, "power");
    two_resistor_board(&r, &mut s);
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["R1.1", "R2.1"], "width": "0.25mm"}));

    // No current: no current check.
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert!(!codes(&o).contains(&"drc.current_width".to_string()));

    let o = exec(&r, &mut s, "net.set", json!({"nets": ["SIG"], "current": "3A", "temp_rise": "10C"}));
    assert_eq!(o["output"]["nets"][0]["current"], "3A");
    assert_eq!(o["output"]["nets"][0]["temp_rise"], "10°C");
    assert_eq!(fail(&r, &mut s, "net.set", json!({"nets": ["SIG"], "current": "3V"})), "net.invalid_value");
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "drc.current_width").expect("warning");
    assert_eq!(d["severity"], "warning");
    assert!(d["subjects"].as_array().unwrap().contains(&json!("net:SIG")), "{d}");
    assert!(d["hint"].as_str().unwrap().contains("current.width"));

    // IPC-2152 fit: 3 A, 10 °C, 35 µm → about 2.09 mm.
    let o = exec(&r, &mut s, "current.width", json!({"net": "SIG", "netclass": "power"}));
    let w = &o["output"]["widths"];
    assert_eq!(w.as_array().unwrap().len(), 2);
    assert!((2.0..2.2).contains(&mm(&w[0]["width"])), "{w}");
    let class = exec(&r, &mut s, "netclass.show", json!({"name": "power"}));
    assert_eq!(class["output"]["track_width"], w[0]["width"]);
    // IPC-2221 external: k = 0.048 → less width than IPC-2152's conservative chart at 3 A.
    let o = exec(&r, &mut s, "current.width", json!({"current": "3A", "layer": "F.Cu", "method": "ipc2221"}));
    assert!(mm(&o["output"]["widths"][0]["width"]) < 2.0);
    assert_eq!(fail(&r, &mut s, "current.width", json!({})), "current.missing");
    assert_eq!(fail(&r, &mut s, "current.width", json!({"current": "1A", "layer": "X.Cu"})), "layer.not_found");

    // Wide enough: clean. Clearing the current removes the check.
    exec(
        &r,
        &mut s,
        "track.add",
        json!({"layer": "B.Cu", "points": [["5mm", "5mm"], ["25mm", "5mm"]], "width": "2.2mm", "net": "SIG"}),
    );
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let n = o["diagnostics"].as_array().unwrap().iter().filter(|d| d["code"] == "drc.current_width").count();
    assert_eq!(n, 1, "only the F.Cu track is too narrow");
    exec(&r, &mut s, "net.set", json!({"nets": ["SIG"], "current": ""}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert!(!codes(&o).contains(&"drc.current_width".to_string()));

    // Impedance target on the class: a 0.25 mm track over 1.53 mm of FR-4 is far from 50 Ω.
    exec(&r, &mut s, "netclass.set", json!({"name": "rf", "impedance": "50"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["SIG"], "class": "rf"}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "drc.impedance").expect("warning");
    assert!(d["message"].as_str().unwrap().contains("assumed stackup"), "{d}");
}

/// A 5 V divider with an LED and an IC without a SPICE model.
fn divider(r: &Registry, s: &mut Session) {
    let steps = [
        (
            "part.create",
            json!({"id": "PWR", "category": "connector", "package": "PinHeader 1x02",
            "pins": pins(&[("1", "VIN", "passive"), ("2", "GND", "passive")])}),
        ),
        (
            "part.create",
            json!({"id": "OPAMP", "category": "ic", "package": "SOIC-8",
            "pins": pins(&[("1", "OUT", "output"), ("2", "IN-", "input"), ("3", "IN+", "input"), ("4", "V-", "power_in"),
                           ("5", "NC1", "no_connect"), ("6", "NC2", "no_connect"), ("7", "NC3", "no_connect"), ("8", "V+", "power_in")])}),
        ),
        ("circuit.add", json!({"part": "PWR", "refdes": "J1"})),
        ("circuit.add", json!({"part": "R 10k 1% 0402", "count": 2})),
        ("circuit.add", json!({"part": "C 100nF 16V X7R 0402"})),
        ("circuit.add", json!({"part": "R 1k 1% 0402"})),
        ("circuit.add", json!({"part": "LED red 0603"})),
        ("circuit.add", json!({"part": "OPAMP"})),
        ("net.connect", json!({"net": "VIN", "pins": ["J1.VIN", "R1.1", "R3.1", "U1.V+"]})),
        ("net.connect", json!({"net": "OUT", "pins": ["R1.2", "R2.1", "C1.1", "U1.IN+"]})),
        ("net.connect", json!({"net": "GND", "pins": ["J1.GND", "R2.2", "C1.2", "D1.K", "U1.V-"]})),
        ("net.connect", json!({"net": "LED_A", "pins": ["R3.2", "D1.A"]})),
        ("net.connect", json!({"net": "BUF", "pins": ["U1.OUT", "U1.IN-"]})),
        ("net.set", json!({"nets": ["VIN"], "driven": true, "voltage": "5V"})),
    ];
    for (cmd, args) in steps {
        exec(r, s, cmd, args);
    }
}

#[test]
fn spice_export_divider() {
    let r = Registry::with_builtins();
    let (dir, mut s) = new_project(&r, "divider");
    divider(&r, &mut s);
    let path = dir.path().join("divider.cir");
    let o =
        exec(&r, &mut s, "export.spice", json!({"path": path, "supplies": true, "control": ["op", "print v(out)"]}));
    let out = &o["output"];
    assert_eq!(out["placeholders"], json!(["U1"]));
    assert_eq!(out["omitted"], json!(["J1"]));
    assert_eq!(out["sources"], json!({"VIN": "5V"}));
    assert_eq!(out["nodes"]["GND"], "0");
    let c = codes(&o);
    assert!(c.contains(&"spice.no_model".to_string()) && c.contains(&"spice.default_model".to_string()), "{c:?}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("VVIN VIN 0 DC 5\n"), "{text}");
    assert!(text.contains("R1 VIN OUT 10k\n") && text.contains("C1 OUT 0 100n\n"), "{text}");
    assert!(text.contains("D1 LED_A 0 D_"), "anode first: {text}");
    assert!(text.contains("* XU1 BUF BUF OUT 0 NC_U1_5 NC_U1_6 NC_U1_7 VIN OPAMP\n"), "{text}");
    assert_golden(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/spice/divider.cir"), &text);

    // With a model, the IC becomes a subcircuit call and its library is included.
    exec(
        &r,
        &mut s,
        "part.set",
        json!({"id": "OPAMP", "params": {"spice_model": "LM358", "spice_lib": "models/lm358.lib", "spice_pins": "IN+ IN- V+ V- OUT"}}),
    );
    let o = exec(&r, &mut s, "export.spice", json!({"path": path}));
    assert!(o["output"].get("placeholders").is_none(), "{}", o["output"]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(".include \"models/lm358.lib\"\n"), "{text}");
    assert!(text.contains("XU1 OUT BUF VIN 0 BUF LM358\n"), "{text}");
    assert!(!text.contains("VVIN"), "no supplies unless asked");
    assert_eq!(fail(&r, &mut s, "export.spice", json!({"ground": "NOPE"})), "spice.ground_not_found");

    // Oracle: ngspice computes the divider's operating point.
    let Some(ngspice) = oracle::require(Oracle::Ngspice) else { return };
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/spice/divider.cir");
    let stdout = oracle::run(&ngspice, &["-b", golden.to_str().unwrap()]);
    let v = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("v(out) = "))
        .unwrap_or_else(|| panic!("no v(out) in ngspice output:\n{stdout}"));
    let v: f64 = v.trim().parse().unwrap();
    assert!((v - 2.5).abs() < 1e-6, "v(out) = {v}");
}

#[test]
fn lint_findings() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r, "lint");
    let steps = [
        (
            "part.create",
            json!({"id": "MCU", "category": "mcu", "package": "SOIC-8",
            "pins": pins(&[("1", "VDD", "power_in"), ("2", "PB0/SDA", "bidirectional"), ("3", "PB1/SCL", "bidirectional"),
                           ("4", "GND", "power_in"), ("5", "USB_DP", "bidirectional"), ("6", "USB_DM", "bidirectional"),
                           ("7", "EN", "input"), ("8", "CLKOUT", "output")])}),
        ),
        (
            "part.create",
            json!({"id": "SENSOR", "category": "ic", "package": "SOT-23-5",
            "pins": pins(&[("1", "VDD", "power_in"), ("2", "GND", "power_in"), ("3", "SDA", "bidirectional"),
                           ("4", "SCL", "input"), ("5", "CLKIN", "input")])}),
        ),
        (
            "part.create",
            json!({"id": "USBC", "category": "connector", "package": "PinHeader 1x04",
            "pins": pins(&[("1", "VBUS", "passive"), ("2", "D-", "passive"), ("3", "D+", "passive"), ("4", "GND", "passive")])}),
        ),
        ("circuit.add", json!({"part": "MCU"})),
        ("circuit.add", json!({"part": "SENSOR"})),
        ("circuit.add", json!({"part": "USBC", "refdes": "J1"})),
        ("circuit.add", json!({"part": "R 4.7k 1% 0402"})),
        ("circuit.add", json!({"part": "C 100nF 16V X7R 0402"})),
        ("net.connect", json!({"net": "3V3", "pins": ["U1.VDD", "U2.VDD", "R1.1", "J1.VBUS"]})),
        ("net.connect", json!({"net": "GND", "pins": ["U1.GND", "U2.GND", "J1.GND"]})),
        ("net.connect", json!({"net": "I2C_SDA", "pins": ["U1.2", "U2.SDA", "R1.2"]})),
        ("net.connect", json!({"net": "I2C_SCL", "pins": ["U1.3", "U2.SCL"]})),
        ("net.connect", json!({"net": "USB_D+", "pins": ["U1.USB_DP", "J1.D+"]})),
        ("net.connect", json!({"net": "USB_D-", "pins": ["U1.USB_DM", "J1.D-"]})),
        ("net.connect", json!({"net": "MCLK", "pins": ["U1.CLKOUT", "U2.CLKIN"]})),
        ("net.connect", json!({"net": "EN_FLT", "pins": ["U1.EN", "C1.1"]})),
        ("net.connect", json!({"net": "GND", "pins": ["C1.2"]})),
    ];
    for (cmd, args) in steps {
        exec(&r, &mut s, cmd, args);
    }
    let o = exec(&r, &mut s, "circuit.lint", json!({}));
    let c = codes(&o);
    let count = |code: &str| c.iter().filter(|x| *x == code).count();
    // Both ICs draw from 3V3 with no capacitor to ground.
    assert_eq!(count("lint.missing_decoupling"), 2, "{c:?}");
    // SDA has a pull-up, SCL does not.
    assert_eq!(count("lint.i2c_pullup"), 1, "{c:?}");
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "lint.i2c_pullup").unwrap();
    assert_eq!(d["subjects"], json!(["net:I2C_SCL"]));
    assert_eq!(count("lint.usb_esd"), 2, "{c:?}");
    assert_eq!(count("lint.clock_termination"), 1, "{c:?}");
    assert_eq!(count("lint.floating_input"), 1, "EN sees only a capacitor: {c:?}");
    assert_eq!(o["output"], json!({"warnings": 6, "notes": 1}));
    for d in o["diagnostics"].as_array().unwrap() {
        assert!(d["hint"].as_str().is_some_and(|h| !h.is_empty()), "{d}");
        assert!(!d["subjects"].as_array().unwrap().is_empty(), "{d}");
    }

    // Fixes: decoupling, SCL pull-up, an ESD array, EN pulled up.
    let fixes = [
        ("circuit.add", json!({"part": "C 100nF 16V X7R 0402", "count": 2})),
        ("net.connect", json!({"net": "3V3", "pins": ["C2.1", "C3.1"]})),
        ("net.connect", json!({"net": "GND", "pins": ["C2.2", "C3.2"]})),
        ("circuit.add", json!({"part": "R 4.7k 1% 0402", "count": 2})),
        ("net.connect", json!({"net": "I2C_SCL", "pins": ["R2.2"]})),
        ("net.connect", json!({"net": "EN_FLT", "pins": ["R3.2"]})),
        ("net.connect", json!({"net": "3V3", "pins": ["R2.1", "R3.1"]})),
        (
            "part.create",
            json!({"id": "USBLC6", "category": "ic", "description": "USB ESD protection array", "package": "SOT-23-5",
            "pins": pins(&[("1", "IO1", "passive"), ("2", "GND", "power_in"), ("3", "IO2", "passive"), ("4", "NC", "no_connect"), ("5", "VBUS", "passive")])}),
        ),
        ("circuit.add", json!({"part": "USBLC6"})),
        ("net.connect", json!({"net": "USB_D+", "pins": ["U3.IO1"]})),
        ("net.connect", json!({"net": "USB_D-", "pins": ["U3.IO2"]})),
        ("net.connect", json!({"net": "GND", "pins": ["U3.GND"]})),
        ("net.connect", json!({"net": "3V3", "pins": ["U3.VBUS"]})),
    ];
    for (cmd, args) in fixes {
        exec(&r, &mut s, cmd, args);
    }
    let o = exec(&r, &mut s, "circuit.lint", json!({}));
    assert_eq!(codes(&o), ["lint.clock_termination"], "{:?}", o["diagnostics"]);

    // ERC with lint includes the lint findings.
    let o = exec(&r, &mut s, "circuit.erc", json!({"lint": true}));
    assert!(codes(&o).contains(&"lint.clock_termination".to_string()));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert!(!codes(&o).iter().any(|c| c.starts_with("lint.")));

    // An unconnected input is a floating input.
    exec(&r, &mut s, "net.disconnect", json!({"pins": ["U2.CLKIN"]}));
    let o = exec(&r, &mut s, "circuit.lint", json!({}));
    assert!(codes(&o).contains(&"lint.floating_input".to_string()));
}

#[test]
fn attiny_board_lint_clean() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r, "attiny");
    build_board(&r, &mut s);
    let o = exec(&r, &mut s, "circuit.lint", json!({}));
    assert_eq!(o["summary"], "lint clean", "{:?}", o["diagnostics"]);
}
