//! openEMS export (`export.openems`) and Touchstone summaries (`touchstone.summary`), through the
//! command registry. The openEMS oracle (optional) runs the exported driver.

mod common;

use std::path::Path;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::electrical::complex::C64;
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

/// A 4-layer board: a 50 Ω microstrip on F.Cu from J1 to a via, continuing on B.Cu to J2 (on
/// the bottom side), over GND planes on both inner layers, with return vias beside the signal
/// via. Returns the B.Cu track's and the signal via's IDs.
fn via_transition(r: &Registry, s: &mut Session) -> (u64, u64) {
    exec(r, s, "board.setup", json!({"layers": 4, "thickness": "1.6mm"}));
    exec(
        r,
        s,
        "board.dielectric",
        json!({"thickness": "0.2mm", "er": "4.1", "loss_tangent": "0.02", "material": "prepreg"}),
    );
    exec(r, s, "board.dielectric", json!({"gap": 2, "thickness": "1.065mm", "er": "4.5", "material": "core"}));
    exec(r, s, "board.outline", json!({"width": "16mm", "height": "8mm"}));
    exec(
        r,
        s,
        "footprint.create",
        json!({"name": "RF_EDGE", "pads": [
            {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "0.6mm"], "shape": "rect"},
            {"number": "2", "at": ["0mm", "1.5mm"], "size": ["1mm", "1mm"], "shape": "rect"},
            {"number": "3", "at": ["0mm", "-1.5mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s,
        "part.create",
        json!({"id": "RF_CONN", "category": "connector", "footprint": "RF_EDGE",
            "pins": [{"number": "1", "name": "SIG"}, {"number": "2", "name": "GND"}], "pin_map": {"2": ["2", "3"]}}),
    );
    exec(r, s, "circuit.add", json!({"part": "RF_CONN", "count": 2}));
    exec(r, s, "net.connect", json!({"net": "RF", "pins": ["J1.1", "J2.1"]}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["J1.2", "J2.2"]}));
    exec(r, s, "place.set", json!({"refdes": "J1", "at": ["2mm", "4mm"]}));
    exec(r, s, "place.set", json!({"refdes": "J2", "at": ["14mm", "4mm"], "side": "bottom"}));
    exec(
        r,
        s,
        "track.add",
        json!({"layer": "F.Cu", "points": [["2mm", "4mm"], ["8mm", "4mm"]], "width": "0.36mm", "net": "RF"}),
    );
    let o = exec(
        r,
        s,
        "track.add",
        json!({"layer": "B.Cu", "points": [["8mm", "4mm"], ["14mm", "4mm"]], "width": "0.36mm", "net": "RF"}),
    );
    let bottom = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    let o = exec(r, s, "via.add", json!({"at": ["8mm", "4mm"], "net": "RF", "drill": "0.3mm", "diameter": "0.6mm"}));
    let via = o["output"]["vias"][0]["id"].as_u64().unwrap();
    for at in
        [["8mm", "5.2mm"], ["8mm", "2.8mm"], ["2mm", "5.5mm"], ["2mm", "2.5mm"], ["14mm", "5.5mm"], ["14mm", "2.5mm"]]
    {
        exec(r, s, "via.add", json!({"at": at, "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    }
    exec(r, s, "zone.add", json!({"name": "GND_in", "net": "GND", "layers": ["In1.Cu", "In2.Cu"], "outline": "board"}));
    (bottom, via)
}

fn new_project(r: &Registry, name: &str) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    exec(r, &mut s, "project.new", json!({"path": dir.path().join(name), "name": name}));
    (dir, s)
}

#[test]
fn via_transition_export() {
    let r = Registry::with_builtins();
    let (dir, mut s) = new_project(&r, "rf");
    let (bottom, via) = via_transition(&r, &mut s);
    let out = dir.path().join("sim");
    let args = json!({
        "dir": out, "name": "via", "f_max": "10GHz", "nets": ["RF"], "margin": "4mm",
        "ports": [{"at": "J1.1"}, {"at": format!("track#{bottom}"), "end": "end", "kind": "msl", "length": "3mm"}]
    });
    let o = exec(&r, &mut s, "export.openems", args.clone());
    let x = &o["output"];
    assert_eq!(x["files"].as_array().unwrap().len(), 2);
    assert!(x["touchstone"].as_str().unwrap().ends_with("via.s2p"), "{x}");
    assert_eq!(x["nets"], json!(["GND", "RF"]));
    // Port 1: the pad, over the first inner layer; port 2: the bottom track's end, over In2.Cu.
    let p = &x["ports"];
    assert_eq!((p[0]["layer"].as_str(), p[0]["reference"].as_str()), (Some("F.Cu"), Some("In1.Cu")));
    assert_eq!(p[0]["width"], "0.6mm", "the pad's narrower side");
    assert_eq!((p[1]["layer"].as_str(), p[1]["reference"].as_str()), (Some("B.Cu"), Some("In2.Cu")));
    assert_eq!(p[1]["reference_plane"], json!(["12.5mm", "4mm"]));
    assert_eq!(x["excitation"], json!({"f0": "5GHz", "fc": "5GHz"}));
    // λ/20 in εr 4.5 at 10 GHz: 29.979 mm / √4.5 / 20 = 0.70662 mm.
    assert_eq!(x["mesh"]["max_cell"], "0.706618mm");
    assert_eq!(codes(&o), ["openems.msl_solid_copper"]);

    let xml = std::fs::read_to_string(out.join("via.xml")).unwrap();
    let py = std::fs::read_to_string(out.join("via.py")).unwrap();
    // Stack: B.Cu 0..0.035, prepreg, In2 0.235..0.2525, core, In1 1.3175..1.335, prepreg, F.Cu 1.535..1.57.
    // Lossy: Djordjevic–Sarkar anchored at f₀ as Debye terms.
    assert!(xml.contains("<DebyeMaterial ID=\"0\" Name=\"dielectric_1\">\n        <Property Epsilon=\""), "{xml}");
    assert!(xml.contains("gap 1: 0.2 thick, εr 4.1, tan δ 0.02, prepreg -->"), "{xml}");
    assert!(xml.contains("Hz; at 5000000000 Hz: εr 4.100000, tan δ 0.020000 -->"), "{xml}");
    assert!(xml.contains("Name=\"copper_GND\"") && xml.contains("Name=\"copper_RF\""));
    assert!(xml.contains("<Cylinder Priority=\"10\" Radius=\"0.15\">\n            <P1 X=\"8\" Y=\"4\" Z=\"0\"/>\n            <P2 X=\"8\" Y=\"4\" Z=\"1.57\"/>"), "via barrel");
    assert!(
        xml.contains("<Polygon Priority=\"11\" Elevation=\"1.3175\" NormDir=\"2\""),
        "In1 plane (its lower conducting sheet), opened around the via"
    );
    for z in ["0", "0.035", "0.235", "0.2525", "1.3175", "1.335", "1.535", "1.57"] {
        let zl = xml.split("<ZLines>").nth(1).unwrap().split("</ZLines>").next().unwrap();
        assert!(zl.split(',').any(|v| v == z), "z line {z} in {zl}");
    }
    assert!(py.contains("'kind': 'lumped', 'z0': 50, 'start': [2, 3.7, 1.335], 'stop': [2, 4.3, 1.535]"), "{py}");
    assert!(
        py.contains("'kind': 'msl', 'z0': 50, 'start': [14, 3.82, 0.035], 'stop': [11, 4.18, 0.235], 'prop': 'x'"),
        "{py}"
    );
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/openems");
    assert_golden(&golden.join("via.xml"), &xml);
    assert_golden(&golden.join("via.py"), &py);

    // Deterministic.
    exec(&r, &mut s, "export.openems", args);
    assert_eq!(std::fs::read_to_string(out.join("via.xml")).unwrap(), xml);

    // A window around the via only, sheet copper, MUR boundaries; the ports' tracks are
    // outside, so no ports (warned).
    let o = exec(
        &r,
        &mut s,
        "export.openems",
        json!({"dir": out, "name": "via_only", "f_max": "20GHz", "around": [format!("via#{via}")], "copper": "sheet", "boundary": "mur"}),
    );
    assert_eq!(codes(&o), ["openems.no_ports"]);
    let xml = std::fs::read_to_string(out.join("via_only.xml")).unwrap();
    assert!(!xml.contains("<LinPoly"), "sheet copper has no thickness");
    // Default margin: three stack heights (3 × 1.465 mm with sheet copper) around the 0.6 mm via
    // pad (a polygon just outside the circle).
    assert_eq!(o["output"]["window"], json!([["3.304024mm", "-0.695732mm"], ["12.695mm", "8.695732mm"]]));
    let py = std::fs::read_to_string(out.join("via_only.py")).unwrap();
    assert!(py.contains("BOUNDARY = ['MUR'] * 6"));

    // Errors.
    assert_eq!(fail(&r, &mut s, "export.openems", json!({"f_max": "10GHz"})), "openems.no_region");
    assert_eq!(fail(&r, &mut s, "export.openems", json!({"f_max": "10GHz", "nets": ["NOPE"]})), "openems.unknown_net");
    assert_eq!(
        fail(&r, &mut s, "export.openems", json!({"f_max": "10GHz", "nets": ["RF"], "ports": [{"at": "J1.9"}]})),
        "openems.unknown_pad"
    );
    assert_eq!(
        fail(
            &r,
            &mut s,
            "export.openems",
            json!({"f_max": "10GHz", "nets": ["RF"], "ports": [{"at": "J1.1", "kind": "msl"}]})
        ),
        "openems.msl_needs_track"
    );
    assert_eq!(fail(&r, &mut s, "export.openems", json!({"f_max": "1V", "nets": ["RF"]})), "value.wrong_unit");
    assert_eq!(
        fail(
            &r,
            &mut s,
            "export.openems",
            json!({"f_max": "10GHz", "nets": ["RF"], "ports": [{"at": "J1.1", "reference": "F.Cu"}]})
        ),
        "openems.bad_reference"
    );

    // Oracle: run the driver with openEMS and check the transition passes the signal at 1 GHz.
    // Opt-in (`CADLAB_OPENEMS_VIA=1`): openEMS diverges on this board (a late-time instability
    // that needs the GND planes, with perfect copper and constant-κ dielectrics as well, so not
    // D68's materials; the built-in solver passes it), and the run takes 25 minutes in CI.
    let Some(_openems) = oracle::optional(Oracle::OpenEms) else { return };
    if std::env::var("CADLAB_OPENEMS_VIA").is_err() {
        eprintln!("skipping the openEMS via run (set CADLAB_OPENEMS_VIA=1)");
        return;
    }
    let python = std::env::var("CADLAB_ORACLE_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("openems_via");
    let _ = std::fs::remove_dir_all(&out);
    let o = exec(
        &r,
        &mut s,
        "export.openems",
        json!({"dir": out, "name": "oracle", "f_max": "4GHz", "nets": ["RF"], "margin": "4mm",
            "ports": [{"at": "J1.1"}, {"at": "J2.1", "layer": "B.Cu"}]}),
    );
    let script = out.join("oracle.py");
    oracle::run(Path::new(&python), &[script.to_str().unwrap()]);
    let ts = o["output"]["touchstone"].as_str().unwrap().to_string();
    let sum =
        exec(&r, &mut s, "touchstone.summary", json!({"path": ts, "frequencies": ["1GHz"], "params": ["S11", "S21"]}));
    let p = &sum["output"]["params"];
    let (s11, s21) = (p[0]["at"][0]["db"].as_f64().unwrap(), p[1]["at"][0]["db"].as_f64().unwrap());
    assert!(s21 > -3.0 && s11 < -6.0, "S11 {s11} dB, S21 {s21} dB at 1 GHz");
}

#[test]
fn stackup_loss_tangent() {
    let r = Registry::with_builtins();
    let (_dir, mut s) = new_project(&r, "lt");
    let o = exec(&r, &mut s, "board.dielectric", json!({"er": "4.3", "loss_tangent": "0.015"}));
    assert_eq!(o["output"]["dielectrics"][0]["loss_tangent"], "0.015");
    // 0 is a lossless dielectric (stored), not "unset" (which assumes FR-4's 0.02 for losses).
    let o = exec(&r, &mut s, "board.dielectric", json!({"loss_tangent": "0"}));
    assert_eq!(o["output"]["dielectrics"][0]["loss_tangent"], "0");
    assert_eq!(fail(&r, &mut s, "board.dielectric", json!({"loss_tangent": "1.5"})), "value.out_of_range");
}

#[test]
fn touchstone_summary() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let dir = tempfile::tempdir().unwrap();
    // A lossy 2-port line, version 1.1, MA format; S21 falls with frequency.
    let path = dir.path().join("line.s2p");
    std::fs::write(
        &path,
        "! test line\n# GHz S MA R 50\n1 0.1 -90 0.95 -30 0.95 -30 0.1 -90\n2 0.2 -100 0.8 -60 0.8 -60 0.2 -100\n3 0.3 -110 0.5 -90 0.5 -90 0.3 -110\n",
    )
    .unwrap();
    let o = exec(
        &r,
        &mut s,
        "touchstone.summary",
        json!({"path": path, "frequencies": ["1.5GHz"], "params": ["S21", "S11"]}),
    );
    let x = &o["output"];
    assert_eq!((x["ports"].as_u64(), x["version"].as_str(), x["points"].as_u64()), (Some(2), Some("1.1"), Some(3)));
    assert_eq!(x["band"], json!([1e9, 3e9]));
    let s21 = &x["params"][0];
    assert_eq!(s21["name"], "S21");
    assert_eq!(s21["min"]["freq"], 3e9);
    assert_eq!(s21["min"]["db"], -6.021);
    assert_eq!(s21["max"]["db"], -0.446);
    assert_eq!(x["params"][1]["max"]["mag"], 0.3);
    assert_eq!(s21["at"][0]["freq"], 1.5e9);

    // Version 2.0, per-port references; default parameters (all four).
    let path2 = dir.path().join("two.s2p");
    std::fs::write(
        &path2,
        "[Version] 2.0\n# Hz S RI R 50\n[Number of Ports] 2\n[Two-Port Data Order] 12_21\n[Number of Frequencies] 2\n[Reference] 50 75\n[Network Data]\n1e6 0 0 1 0 1 0 0 0\n2e6 0 0 0 1 0 1 0 0\n[End]\n",
    )
    .unwrap();
    let o = exec(&r, &mut s, "touchstone.summary", json!({"path": path2}));
    let x = &o["output"];
    assert_eq!(x["reference"], json!([50.0, 75.0]));
    let names: Vec<&str> = x["params"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["S11", "S12", "S21", "S22"]);
    assert_eq!(x["params"][2]["max"]["db"], 0.0);

    // Errors.
    assert_eq!(
        fail(&r, &mut s, "touchstone.summary", json!({"path": path, "params": ["S31"]})),
        "touchstone.bad_param"
    );
    assert_eq!(
        fail(&r, &mut s, "touchstone.summary", json!({"path": path, "frequencies": ["5GHz"]})),
        "touchstone.out_of_band"
    );
    let bad = dir.path().join("x.txt");
    std::fs::write(&bad, "# Hz S RI\n1 0 0\n").unwrap();
    assert_eq!(fail(&r, &mut s, "touchstone.summary", json!({"path": bad})), "touchstone.no_ports");
    let o = exec(&r, &mut s, "touchstone.summary", json!({"path": bad, "ports": 1}));
    assert_eq!(o["output"]["params"][0]["name"], "S11");
}

/// Solder mask (both outer layers, open at the pads) and a dispersive dielectric in the export:
/// a mask material with its primitives over the laminate and the copper, air in the pad openings,
/// mesh lines at the mask surfaces, and εr / κ of the Djordjevic–Sarkar model at f₀.
#[test]
fn mask_and_dispersion_export() {
    let r = Registry::with_builtins();
    let (dir, mut s) = new_project(&r, "rf");
    let (bottom, _) = via_transition(&r, &mut s);
    exec(&r, &mut s, "board.dielectric", json!({"gap": 1, "ref_freq": "1GHz"}));
    let out = dir.path().join("sim");
    let args = json!({
        "dir": out, "name": "mask", "f_max": "10GHz", "nets": ["RF"], "margin": "4mm",
        "mask": "20um", "mask_copper": "15um", "mask_er": "3.5", "mask_loss_tangent": "0.02",
        "ports": [{"at": "J1.1"}, {"at": format!("track#{bottom}"), "end": "end", "kind": "msl", "length": "3mm"}]
    });
    let o = exec(&r, &mut s, "export.openems", args);
    assert_eq!(codes(&o), ["openems.msl_solid_copper"]);
    // The mask's outer surfaces bound the board before the air: 1.585 mm above, −0.015 below.
    let z = &o["output"]["z_range"];
    let air = 29.979_245_8 / 4.0;
    let mm = |v: &Value| v.as_str().unwrap().trim_end_matches("mm").parse::<f64>().unwrap();
    assert!((mm(&z[1]) - (1.585 + air)).abs() < 1e-3 && (mm(&z[0]) + 0.015 + air).abs() < 1e-3, "{z}");
    let xml = std::fs::read_to_string(out.join("mask.xml")).unwrap();
    assert!(xml.contains("<DebyeMaterial ID=\"3\" Name=\"solder_mask\">\n        <Property Epsilon=\""), "{xml}");
    assert!(xml.contains("solder mask: εr 3.5, tan δ 0.02 -->"), "{xml}");
    assert!(xml.contains("Name=\"mask_openings\">\n        <Property Epsilon=\"1\"/>"));
    // Over the laminate (top 1.535 + 0.02) and over the copper (top 1.57 + 0.015, bottom 0 − 0.015).
    assert!(xml.contains("Z=\"1.535\"/>") && xml.contains("Z=\"1.555\"/>"));
    assert!(xml.contains("Priority=\"7\" Elevation=\"1.57\" Length=\"0.015\""), "mask over the F.Cu track");
    assert!(xml.contains("Priority=\"7\" Elevation=\"-0.015\" Length=\"0.015\""), "mask under the B.Cu track");
    let zl = xml.split("<ZLines>").nth(1).unwrap().split("</ZLines>").next().unwrap();
    for z in ["-0.015", "0.015", "1.555", "1.585"] {
        assert!(zl.split(',').any(|v| v == z), "z line {z} in {zl}");
    }
    // The pads are open: air primitives over J1.1 (top) and J2.1 (bottom side).
    let openings = xml.split("Name=\"mask_openings\"").nth(1).unwrap().split("</Material>").next().unwrap();
    assert!(openings.contains("Priority=\"8\""), "{openings}");
    // The prepreg is dispersive: εr at f₀ = 5 GHz below its 1 GHz value; ε∞ below both.
    let gap1 = xml.split("<!-- gap 1:").nth(1).unwrap().split("</DebyeMaterial>").next().unwrap();
    assert!(gap1.contains("tan δ 0.02 at 1GHz, prepreg -->"), "{gap1}");
    let field =
        |s: &str, key: &str| -> f64 { s.split(key).nth(1).unwrap().split(['"', ',']).next().unwrap().parse().unwrap() };
    let eps = field(gap1, "at 5000000000 Hz: εr ");
    assert!(eps < 4.1 && eps > 4.0, "{eps}");
    let eps_inf = field(gap1, "<Property Epsilon=\"");
    assert!(eps_inf > 1.0 && eps_inf < eps, "{eps_inf}");
    // Errors: options without the thickness, a non-positive thickness.
    let bad = json!({"dir": out, "f_max": "10GHz", "nets": ["RF"], "mask_er": "3.5"});
    assert_eq!(fail(&r, &mut s, "export.openems", bad), "openems.bad_mask");
    let bad = json!({"dir": out, "f_max": "10GHz", "nets": ["RF"], "mask": "0mm"});
    assert_eq!(fail(&r, &mut s, "export.openems", bad), "openems.bad_mask");
}

// ---------------------------------------------------------------------------------------------
// openEMS oracle: the exported lossy microstrip simulated by openEMS against `sim.sparams` and
// the 2D field solver.

fn c(v: &cadlab::touchstone::Complex) -> C64 {
    C64::new(v.re, v.im)
}

/// ABCD of a 2-port from its S-parameters (reference `zr`): the line's Z₀ = √(B/C) and γl =
/// acosh(A).
fn line_from_s(s: &[cadlab::touchstone::Complex], zr: f64) -> (C64, C64) {
    let (s11, s12, s21, s22) = (c(&s[0]), c(&s[1]), c(&s[2]), c(&s[3]));
    let k = |x: f64| C64::new(x, 0.0);
    let (one, two, z) = (k(1.0), k(2.0), k(zr));
    let a = ((one + s11) * (one - s22) + s12 * s21) / (two * s21);
    let b = z * ((one + s11) * (one + s22) - s12 * s21) / (two * s21);
    let cc = ((one - s11) * (one - s22) - s12 * s21) / (two * s21 * z);
    let mut z0 = (b / cc).sqrt();
    if z0.re < 0.0 {
        z0 = k(0.0) - z0;
    }
    let mut gl = (a + (a * a - one).sqrt()).ln();
    if gl.im < 0.0 {
        gl = k(0.0) - gl;
    }
    (z0, gl)
}

/// A two-layer board with a straight microstrip on F.Cu (1 mm wide, `len_mm` long, from (5, 12)
/// mm) over a GND pour on B.Cu: 0.5 mm of a Djordjevic–Sarkar laminate (εr 4, tan δ 0.02 at 1
/// GHz). Returns the project directory, the session and the track's ID.
fn microstrip_board(r: &Registry, len_mm: i64) -> (tempfile::TempDir, Session, u64) {
    let (dir, mut s) = new_project(r, "line");
    let s_ = &mut s;
    exec(r, s_, "board.setup", json!({"layers": 2}));
    exec(
        r,
        s_,
        "board.dielectric",
        json!({"thickness": "0.5mm", "er": "4", "loss_tangent": "0.02", "ref_freq": "1GHz"}),
    );
    exec(r, s_, "board.outline", json!({"width": format!("{}mm", len_mm + 10), "height": "30mm"}));
    exec(
        r,
        s_,
        "footprint.create",
        json!({"name": "TP2", "pads": [
            {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"},
            {"number": "2", "at": ["2mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s_,
        "part.create",
        json!({"id": "TP", "category": "connector", "footprint": "TP2",
            "pins": [{"number": "1", "name": "SIG"}, {"number": "2", "name": "GND"}]}),
    );
    exec(r, s_, "circuit.add", json!({"part": "TP", "count": 2}));
    exec(r, s_, "net.connect", json!({"net": "RF", "pins": ["J1.1", "J2.1"]}));
    exec(r, s_, "net.connect", json!({"net": "GND", "pins": ["J1.2", "J2.2"]}));
    exec(r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "27mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J2", "at": [format!("{}mm", len_mm + 5), "27mm"]}));
    let o = exec(
        r,
        s_,
        "track.add",
        json!({"layer": "F.Cu", "points": [["5mm", "12mm"], [format!("{}mm", len_mm + 5), "12mm"]], "width": "1mm", "net": "RF"}),
    );
    let id = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    exec(r, s_, "via.add", json!({"at": ["2mm", "22mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(r, s_, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    (dir, s, id)
}

/// The lossy model in the export: the Djordjevic–Sarkar laminate as a `DebyeMaterial`, copper as
/// conducting sheets only (on both surfaces of solid copper, no perfect conductor between), and
/// the warning for openEMS microstrip ports on solid copper.
#[test]
fn lossy_export() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = microstrip_board(&r, 24);
    let out = dir.path().join("sim");
    let args = json!({"dir": out, "name": "solid", "around": [format!("track#{id}")], "f_max": "6GHz",
        "margin": "5mm", "roughness": "1um",
        "ports": [{"at": format!("track#{id}"), "kind": "msl", "length": "4mm"}]});
    let o = exec(&r, &mut s, "export.openems", args);
    assert_eq!(codes(&o), ["openems.msl_solid_copper"]);
    let xml = std::fs::read_to_string(out.join("solid.xml")).unwrap();
    assert!(xml.starts_with("<?xml") && xml.contains("\n<ContinuousStructure CoordSystem=\"0\">\n"), "{xml}");
    assert!(xml.contains("<DebyeMaterial ID=\"0\" Name=\"dielectric_1\">"), "{xml}");
    assert!(xml.contains("EpsilonDelta_1=\"") && xml.contains("EpsilonRelaxTime_1=\""));
    // 58 MS/m over K² (Hammerstad–Jensen, 1 µm RMS at 3 GHz: δ = 1.2 µm, K = 1.49).
    let sheet = xml.split("Name=\"copper_RF_F.Cu\"").nth(1).unwrap().split("</ConductingSheet>").next().unwrap();
    let sigma: f64 = sheet.split("Conductivity=\"").nth(1).unwrap().split('"').next().unwrap().parse().unwrap();
    assert!(sigma > 2.55e7 && sigma < 2.7e7, "{sigma}");
    assert!(sheet.contains("Thickness=\"3.500000e-5\""), "{sheet}");
    assert!(sheet.contains("Elevation=\"0.535\"") && sheet.contains("Elevation=\"0.57\""), "both surfaces: {sheet}");
    assert!(!xml.contains("Name=\"copper_RF\""), "no perfect conductor between the sheets: {xml}");
    // Sheet copper: the conducting sheets alone (no metal for the strip), no warning.
    let args = json!({"dir": out, "name": "sheet", "around": [format!("track#{id}")], "f_max": "6GHz",
        "margin": "5mm", "copper": "sheet", "ports": [{"at": format!("track#{id}"), "kind": "msl", "length": "4mm"}]});
    let o = exec(&r, &mut s, "export.openems", args);
    assert_eq!(codes(&o), Vec::<String>::new());
    let xml = std::fs::read_to_string(out.join("sheet.xml")).unwrap();
    assert!(xml.contains("Name=\"copper_RF_F.Cu\" Conductivity=\"5.800000e7\" Thickness=\"3.500000e-5\""), "{xml}");
    assert!(!xml.contains("Name=\"copper_RF\""), "{xml}");
    // Perfect copper: metals only; a lossless laminate is a plain material.
    exec(&r, &mut s, "board.dielectric", json!({"loss_tangent": "0"}));
    let args = json!({"dir": out, "name": "pec", "around": [format!("track#{id}")], "f_max": "6GHz",
        "margin": "5mm", "copper": "sheet", "perfect_copper": true});
    exec(&r, &mut s, "export.openems", args);
    let xml = std::fs::read_to_string(out.join("pec.xml")).unwrap();
    assert!(!xml.contains("ConductingSheet") && !xml.contains("DebyeMaterial"), "{xml}");
    assert!(xml.contains("<Material ID=\"0\" Name=\"dielectric_1\">\n        <Property Epsilon=\"4\"/>"), "{xml}");
    let bad = json!({"dir": out, "f_max": "6GHz", "around": [format!("track#{id}")], "perfect_copper": true,
        "conductivity": "58M"});
    assert_eq!(fail(&r, &mut s, "export.openems", bad), "openems.perfect_copper");
}

/// With openEMS installed (`CADLAB_ORACLES=1`, the `openEMS` executable or
/// `CADLAB_ORACLE_OPENEMS`, and `CADLAB_ORACLE_PYTHON` with openEMS's Python modules): a lossy
/// microstrip (Djordjevic–Sarkar laminate, 58 MS/m copper) exported and simulated by openEMS,
/// against the built-in solver on the same region and mesh options, twice: sheet copper between
/// microstrip ports (20 mm between the reference planes), and solid copper between lumped ports
/// at the track's ends. Checked: |S21| (the loss), the line's Z₀ from the ABCD matrix (below the
/// first half-wave resonance, where it is well conditioned; for the microstrip ports also
/// against the 2D field solver) and openEMS's |S11|. The tables go to
/// `$CARGO_TARGET_TMPDIR/openems_oracle/summary.md`.
#[test]
fn lossy_microstrip_oracle() {
    let Some(_openems) = oracle::optional(Oracle::OpenEms) else { return };
    let r = Registry::with_builtins();
    let (_dir, mut s, id) = microstrip_board(&r, 24);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("openems_oracle");
    let _ = std::fs::remove_dir_all(&out);
    let track = format!("track#{id}");
    // openEMS's microstrip ports measure zero-thickness strips: sheet copper with them.
    let cases = [
        (
            "msl",
            "sheet copper, microstrip ports",
            json!([{"at": track, "kind": "msl", "length": "4mm"}, {"at": track, "end": "end", "kind": "msl", "length": "4mm"}]),
            "sheet",
            3.6e9,
        ),
        ("lumped", "solid copper, lumped ports", json!([{"at": track}, {"at": track, "end": "end"}]), "solid", 2.6e9),
    ];
    // The 2D field solver's Z₀ of the zero-thickness strip (εr at 1 GHz).
    let st = cadlab::electrical::fieldsolver::Structure {
        model: cadlab::electrical::Model::Microstrip,
        h: 0.5,
        er: 4.0,
        h2: 0.0,
        er2: 4.0,
        t: 0.0,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    };
    let z2d = st.line(1.0, cadlab::electrical::fieldsolver::Accuracy::default()).unwrap().z0;
    let python = std::env::var("CADLAB_ORACLE_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut md = String::new();
    let mut fails = Vec::new();
    for (name, title, ports, copper, z_until) in cases {
        let common = json!({"dir": out, "around": [track], "copper": copper, "thirds": true, "ports": ports,
            "f_max": "6GHz", "points": 13, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm", "pml_cells": 6,
            "end_criteria": "50dB"});
        let mut args = common.clone();
        args["name"] = json!(name);
        exec(&r, &mut s, "export.openems", args);
        oracle::run(Path::new(&python), &[out.join(format!("{name}.py")).to_str().unwrap()]);
        let mut args = common;
        args["name"] = json!(format!("{name}_builtin"));
        exec(&r, &mut s, "sim.sparams", args);
        let read = |file: String| {
            cadlab::touchstone::parse(&std::fs::read_to_string(out.join(file)).unwrap(), Some(2)).unwrap()
        };
        let (oe, cl) = (read(format!("{name}.s2p")), read(format!("{name}_builtin.s2p")));
        assert_eq!(oe.freqs, cl.freqs);
        md += &format!(
            "\n#### {title}\n\n| f (GHz) | \\|S21\\| openEMS (dB) | \\|S21\\| cadlab (dB) | Z₀ openEMS | Z₀ cadlab | \\|S11\\| openEMS |\n\
             |---|---|---|---|---|---|\n"
        );
        let (mut loss, mut z_rel, mut z_2d, mut s11) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for ((f, a), b) in oe.freqs.iter().zip(&oe.data).zip(&cl.data) {
            let db = |v: &cadlab::touchstone::Complex| 20.0 * c(v).abs().log10();
            let ((za, _), (zb, _)) = (line_from_s(a, 50.0), line_from_s(b, 50.0));
            md += &format!(
                "| {:.2} | {:.4} | {:.4} | {:.2} | {:.2} | {:.4} |\n",
                f / 1e9,
                db(&a[2]),
                db(&b[2]),
                za.re,
                zb.re,
                c(&a[0]).abs()
            );
            if *f < 0.5e9 {
                continue;
            }
            // |S21| within 15 % and 0.025 dB (the coarse runs' noise on a lossless line).
            let d = (db(&a[2]) - db(&b[2])).abs();
            loss = loss.max(d / db(&b[2]).abs());
            if d > 0.15 * db(&b[2]).abs() + 0.025 {
                fails.push(format!("{name} {f}: |S21| {} vs {} dB", db(&a[2]), db(&b[2])));
            }
            s11 = s11.max(c(&a[0]).abs());
            if *f <= z_until {
                z_rel = z_rel.max((za.re - zb.re).abs() / zb.re);
                if name == "msl" {
                    z_2d = z_2d.max((za.re - z2d).abs() / z2d);
                }
            }
        }
        md += &format!(
            "\nFrom 0.5 GHz: |S21| within {:.1} % of cadlab's; Z₀ (to {:.1} GHz) within {:.1} % of cadlab's{}; openEMS |S11| at most {s11:.3}.\n",
            100.0 * loss,
            z_until / 1e9,
            100.0 * z_rel,
            if name == "msl" {
                format!(" and {:.1} % of the 2D solver's ({z2d:.2} Ω)", 100.0 * z_2d)
            } else {
                String::new()
            }
        );
        if z_rel >= 0.05 || z_2d >= 0.05 || s11 >= 0.1 {
            fails.push(format!("{name}: Z₀ {z_rel} / {z_2d}, |S11| {s11}"));
        }
    }
    std::fs::write(out.join("summary.md"), &md).unwrap();
    eprintln!("{md}");
    assert!(fails.is_empty(), "{fails:?}\n{md}");
}
