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
    // Its nearest node column on mesh coordinates: no mesh edge lies inside a barrel between
    // mesh lines, so without it the via conducts nothing in openEMS (D74).
    let (xl, yl) = (lines(&xml, "XLines"), lines(&xml, "YLines"));
    let near = |ls: &[f64], v: f64| ls.iter().copied().min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs())).unwrap();
    let (nx, ny) = (near(&xl, 8.0), near(&yl, 4.0));
    assert!(
        xml.contains(&format!(
            "<Box Priority=\"10\">\n            <P1 X=\"{nx}\" Y=\"{ny}\" Z=\"0\"/>\n            <P2 X=\"{nx}\" Y=\"{ny}\" Z=\"1.57\"/>"
        )),
        "via node column at {nx}, {ny}"
    );
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
    assert_eq!(codes(&o), ["openems.mur_static_field", "openems.no_ports"]);
    let xml = std::fs::read_to_string(out.join("via_only.xml")).unwrap();
    assert!(!xml.contains("<LinPoly"), "sheet copper has no thickness");
    // Default margin: three stack heights (3 × 1.465 mm with sheet copper) around the 0.6 mm via
    // pad (a polygon just outside the circle).
    assert_eq!(o["output"]["window"], json!([["3.304024mm", "-0.695732mm"], ["12.695mm", "8.695732mm"]]));
    assert!(codes(&o).contains(&"openems.no_ports".to_string()));
    let py = std::fs::read_to_string(out.join("via_only.py")).unwrap();
    assert!(py.contains("BOUNDARY = ['MUR'] * 6"));
    // A Mur wall gets eight cells of clearance too (D74); `f_min` above 0 has no static-field note.
    let keep = lines(&xml, "YLines");
    assert!(keep[0] < -0.695733 && keep[8] <= -0.695732 && keep[9] > -0.695733, "{keep:?}");
    let o = exec(
        &r,
        &mut s,
        "export.openems",
        json!({"dir": out, "name": "via_only", "f_min": "1GHz", "f_max": "20GHz", "around": [format!("via#{via}")],
            "copper": "sheet", "boundary": "mur"}),
    );
    assert_eq!(codes(&o), ["openems.no_ports"]);
    // The via board's edge is one cell from the wall of its 4 mm region: warned when kept there,
    // for the PML and for Mur (D74).
    for boundary in ["pml", "mur"] {
        let o = exec(
            &r,
            &mut s,
            "export.openems",
            json!({"dir": out, "name": "near", "f_min": "1GHz", "f_max": "4GHz", "nets": ["RF"], "margin": "4mm",
                "pml_outside": false, "boundary": boundary}),
        );
        assert!(codes(&o).contains(&"openems.edge_near_boundary".to_string()), "{boundary}: {}", o["diagnostics"]);
    }

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

    // Oracle: the driver run by openEMS (the absorbing layer beyond the region: D74, with it in the
    // region's outer cells this board's edge one cell from the wall diverges) passes the signal at
    // 1 GHz and agrees with `sim.sparams` on the same options.
    let Some(_openems) = oracle::optional(Oracle::OpenEms) else { return };
    let python = std::env::var("CADLAB_ORACLE_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("openems_via");
    let _ = std::fs::remove_dir_all(&out);
    let common = via_oracle_args(&out);
    let mut args = common.clone();
    args["name"] = json!("oracle");
    let o = exec(&r, &mut s, "export.openems", args);
    // `sim.sparams` first: a failure there shows before openEMS has run.
    let read = |f: &str| cadlab::touchstone::parse(&std::fs::read_to_string(out.join(f)).unwrap(), Some(2)).unwrap();
    let mut args = common.clone();
    args["name"] = json!("cadlab");
    exec(&r, &mut s, "sim.sparams", args);
    let cl = read("cadlab.s2p");
    let script = out.join("oracle.py");
    oracle::run(Path::new(&python), &[script.to_str().unwrap()]);
    let ts = o["output"]["touchstone"].as_str().unwrap().to_string();
    let sum =
        exec(&r, &mut s, "touchstone.summary", json!({"path": ts, "frequencies": ["1GHz"], "params": ["S11", "S21"]}));
    let p = &sum["output"]["params"];
    let (s11, s21) = (p[0]["at"][0]["db"].as_f64().unwrap(), p[1]["at"][0]["db"].as_f64().unwrap());
    let oe = read("oracle.s2p");
    let db = |v: &cadlab::touchstone::Complex| 20.0 * c(v).abs().log10();
    let mut md = String::new();
    let mut fails = Vec::new();
    assert_eq!(oe.freqs, cl.freqs);
    md += "\n#### Via transition\n\n| f (GHz) | |S21| openEMS (dB) | |S21| cadlab (dB) | |S11| openEMS (dB) | |S11| cadlab (dB) |\n|---|---|---|---|---|\n";
    let mut worst = 0.0f64;
    for ((f, a), b) in oe.freqs.iter().zip(&oe.data).zip(&cl.data) {
        md += &format!(
            "| {:.2} | {:.3} | {:.3} | {:.2} | {:.2} |\n",
            f / 1e9,
            db(&a[2]),
            db(&b[2]),
            db(&a[0]),
            db(&b[0])
        );
        if *f >= 0.5e9 {
            let d = (db(&a[2]) - db(&b[2])).abs();
            worst = worst.max(d);
            // (Written so that a NaN fails: `max` drops it.)
            if d.partial_cmp(&0.25) != Some(std::cmp::Ordering::Less) {
                fails.push(format!("{f}: |S21| {} dB against openEMS's {} dB", db(&b[2]), db(&a[2])));
            }
        }
    }
    md += &format!("\nFrom 0.5 GHz: |S21| within {worst:.2} dB of cadlab's.\n");
    std::fs::write(out.join("summary_via.md"), &md).unwrap();
    eprintln!("{md}");
    assert!(s21 > -3.0 && s11 < -6.0, "S11 {s11} dB, S21 {s21} dB at 1 GHz\n{md}");
    assert!(fails.is_empty(), "{fails:?}\n{md}");
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

/// The options the oracle's via transition runs on, in openEMS and in `sim.sparams`.
fn via_oracle_args(out: &Path) -> Value {
    json!({"dir": out, "f_max": "4GHz", "points": 9, "nets": ["RF"], "margin": "4mm",
        "pml_outside": true, "end_criteria": "40dB", "ports": [{"at": "J1.1"}, {"at": "J2.1", "layer": "B.Cu"}]})
}

/// The options the oracle's lossy line runs on: `msl` is sheet copper between microstrip ports
/// 4 mm long (openEMS's microstrip ports measure zero-thickness strips), else solid copper
/// between lumped ports at the track's ends.
fn line_oracle_args(out: &Path, track: &str, msl: bool) -> Value {
    let ports = if msl {
        json!([{"at": track, "kind": "msl", "length": "4mm"}, {"at": track, "end": "end", "kind": "msl", "length": "4mm"}])
    } else {
        json!([{"at": track}, {"at": track, "end": "end"}])
    };
    json!({"dir": out, "around": [track], "copper": if msl { "sheet" } else { "solid" }, "thirds": true,
        "ports": ports, "pml_outside": false,
        "f_max": "6GHz", "points": 13, "margin": "5mm", "air": "5mm", "max_cell": "0.5mm", "pml_cells": 6,
        "end_criteria": "50dB"})
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

fn lines(xml: &str, tag: &str) -> Vec<f64> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let v = xml.split(&open).nth(1).unwrap().split(&close).next().unwrap();
    v.split(',').map(|a| a.parse().unwrap()).collect()
}

/// The absorbing layer beyond the region (D74): `pml_cells` more lines past the window on x and
/// y (z is air), the window as asked; what ends inside the window stays there, what reaches its
/// side runs to the wall. `pml_outside: false` keeps the layer in the region's outer cells.
#[test]
fn pml_outside_the_region() {
    let r = Registry::with_builtins();
    let (dir, mut s, id) = microstrip_board(&r, 24);
    let out = dir.path().join("sim");
    let track = format!("track#{id}");
    // A window across the board: x 8 .. 20 inside the line, y 8 .. 16 inside the 30 mm outline.
    let args = |name: &str, extra: Value| {
        let mut a = json!({"dir": out, "name": name, "f_max": "6GHz", "copper": "sheet", "perfect_copper": true,
            "window": [["8mm", "8mm"], ["20mm", "16mm"]], "air": "5mm"});
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    };
    let inner = exec(&r, &mut s, "export.openems", args("inner", json!({"pml_outside": false})));
    let outer = exec(&r, &mut s, "export.openems", args("outer", json!({"pml_cells": 6})));
    assert_eq!(inner["output"]["window"], outer["output"]["window"]);
    let (a, b) = (
        std::fs::read_to_string(out.join("inner.xml")).unwrap(),
        std::fs::read_to_string(out.join("outer.xml")).unwrap(),
    );
    let (ax, bx) = (lines(&a, "XLines"), lines(&b, "XLines"));
    assert_eq!((ax[0], ax[ax.len() - 1]), (8.0, 20.0));
    assert_eq!(bx.len(), ax.len() + 12, "six cells on each side");
    assert_eq!(&bx[6..bx.len() - 6], &ax[..], "the region's lines are unchanged");
    let (ay, by) = (lines(&a, "YLines"), lines(&b, "YLines"));
    assert_eq!(by.len(), ay.len() + 12);
    assert_eq!(lines(&a, "ZLines"), lines(&b, "ZLines"));
    // The laminate and the plane reach the wall; the line runs through the x walls.
    let wall = format!("X=\"{}\" Y=\"{}\"", bx[0], by[0]);
    let diel = b.split("Name=\"dielectric_1\"").nth(1).unwrap().split("</Material>").next().unwrap();
    assert!(diel.contains(&wall), "{diel}");
    assert!(!a.contains(&wall), "{a}");
    let strip = b.split("Name=\"copper_RF\"").nth(1).unwrap().split("</Metal>").next().unwrap();
    assert!(strip.contains(&format!("X=\"{}\" Y=\"11.5\"", bx[0])), "{strip}");
    assert!(strip.contains(&format!("X=\"{}\" Y=\"12.5\"", bx[bx.len() - 1])), "{strip}");

    // A window past the outline: the board's edge stays at the outline, out of the layer.
    let args = json!({"dir": out, "name": "edge", "f_max": "6GHz", "copper": "sheet", "perfect_copper": true,
        "around": [track], "margin": "20mm", "air": "5mm"});
    exec(&r, &mut s, "export.openems", args);
    let e = std::fs::read_to_string(out.join("edge.xml")).unwrap();
    let diel = e.split("Name=\"dielectric_1\"").nth(1).unwrap().split("</Material>").next().unwrap();
    assert!(diel.contains("X=\"0\" Y=\"0\"") && diel.contains("X=\"34\" Y=\"30\""), "{diel}");
}

/// Two-pin parts in the export (D74): by `sim.sparams`'s selection rules, as `LumpedElement`s in
/// the gap between the pads' copper; a resistance alone is a plain element, a reactance a series
/// one, 0 Ω a short; `parts: false` leaves them out, a part without a value is reported.
#[test]
fn parts_in_the_export() {
    let r = Registry::with_builtins();
    let (dir, mut s, t) = part_board(&r, "resistor", json!({"resistance": "50"}), 4);
    let out = dir.path().join("sim");
    let args = |name: &str, extra: Value| {
        let mut a = part_args(&out, t);
        a["name"] = json!(name);
        for (k, v) in extra.as_object().unwrap() {
            a[k] = v.clone();
        }
        a
    };
    let read = |name: &str| std::fs::read_to_string(out.join(format!("{name}.xml"))).unwrap();
    let o = exec(&r, &mut s, "export.openems", args("res", json!({})));
    assert_eq!(o["output"]["parts"], json!([{"refdes": "U1", "pads": ["U1.1", "U1.2"], "resistance": "50Ω"}]));
    assert!(!codes(&o).contains(&"sim.part_not_modeled".to_string()));
    let xml = read("res");
    let el = xml.split("<LumpedElement ").nth(1).unwrap().split("</LumpedElement>").next().unwrap();
    assert!(el.contains("Name=\"part_U1\" Direction=\"0\" Caps=\"1\" R=\"5.000000e1\">"), "{el}");
    // The part is at x = 10 mm, pads 0.9 mm wide at ±0.8 mm: the gap 9.65 .. 10.35; across, 1 mm.
    assert!(
        el.contains("<P1 X=\"9.65\" Y=\"9.5\" Z=\"0.5\"/>") && el.contains("<P2 X=\"10.35\" Y=\"10.5\" Z=\"0.5\"/>"),
        "{el}"
    );
    assert!(xml.contains("<!-- U1.1 to U1.2 -->"), "{xml}");

    let o = exec(&r, &mut s, "export.openems", args("none", json!({"parts": false})));
    assert!(o["output"].get("parts").is_none(), "{}", o["output"]);
    assert!(!read("none").contains("LumpedElement"));

    // A component property overrides the part; zero is a short (metal).
    exec(&r, &mut s, "part.set", json!({"id": "DUT", "params": {"resistance": "0"}}));
    exec(&r, &mut s, "export.openems", args("short", json!({})));
    let xml = read("short");
    assert!(xml.contains("<Metal ID=") && xml.contains("Name=\"part_U1\"") && !xml.contains("LumpedElement"), "{xml}");

    exec(&r, &mut s, "part.set", json!({"id": "DUT", "params": {"resistance": null}}));
    let o = exec(&r, &mut s, "export.openems", args("noval", json!({})));
    assert!(codes(&o).contains(&"sim.part_not_modeled".to_string()), "{}", o["diagnostics"]);
    assert!(!read("noval").contains("part_U1"));

    // A capacitor with ESR and ESL: one series element.
    let (dir2, mut s2, t2) =
        part_board(&r, "capacitor", json!({"capacitance": "1pF", "esr": "0.1", "esl": "0.5nH"}), 4);
    let out2 = dir2.path().join("sim");
    let o = exec(&r, &mut s2, "export.openems", part_args(&out2, t2));
    assert_eq!(o["output"]["parts"][0]["capacitance"], "1pF");
    let xml = std::fs::read_to_string(out2.join("dut.xml")).unwrap();
    assert!(xml.contains("Caps=\"1\" R=\"1.000000e-1\" C=\"1.000000e-12\" L=\"5.000000e-10\" LEtype=\"1\">"), "{xml}");

    // A part inside the PML, when it is kept in the region: warned.
    let o = exec(
        &r,
        &mut s2,
        "export.openems",
        json!({"dir": out2, "name": "in_pml", "f_max": "6GHz", "copper": "sheet", "perfect_copper": true,
            "window": [["9mm", "5mm"], ["20mm", "15mm"]], "air": "4mm", "pml_outside": false, "pml_cells": 4}),
    );
    assert!(codes(&o).contains(&"openems.part_in_pml".to_string()), "{}", o["diagnostics"]);
}

/// The board of the parts' tests: a two-layer board, 0.5 mm of εr 4 (lossless) over a ground
/// pour, a 1 mm microstrip on F.Cu on both sides of a 0603 part `U1` (pads 0.9 × 1 mm at ±0.8 mm)
/// at x = `len` + 6 mm, y = 10 mm. Returns the project directory, the session and the two
/// tracks' IDs.
fn part_board(r: &Registry, category: &str, params: Value, len: i64) -> (tempfile::TempDir, Session, [u64; 2]) {
    let (dir, mut s) = new_project(r, "parts");
    let s_ = &mut s;
    let mm = |v: f64| format!("{v}mm");
    exec(r, s_, "board.setup", json!({"layers": 2}));
    exec(r, s_, "board.dielectric", json!({"thickness": "0.5mm", "er": "4", "loss_tangent": "0"}));
    let width = 2 * len + 12;
    exec(r, s_, "board.outline", json!({"width": mm(width as f64), "height": "20mm"}));
    exec(
        r,
        s_,
        "footprint.create",
        json!({"name": "P0603", "pads": [
            {"number": "1", "at": ["-0.8mm", "0mm"], "size": ["0.9mm", "1mm"], "shape": "rect"},
            {"number": "2", "at": ["0.8mm", "0mm"], "size": ["0.9mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s_,
        "footprint.create",
        json!({"name": "TP1", "pads": [
        {"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "shape": "rect"}]}),
    );
    exec(
        r,
        s_,
        "part.create",
        json!({"id": "DUT", "category": category, "footprint": "P0603", "params": params,
            "pins": [{"number": "1", "name": "1"}, {"number": "2", "name": "2"}]}),
    );
    exec(
        r,
        s_,
        "part.create",
        json!({"id": "TP", "category": "connector", "footprint": "TP1",
        "pins": [{"number": "1", "name": "1"}]}),
    );
    exec(r, s_, "circuit.add", json!({"part": "DUT", "refdes": "U1"}));
    exec(r, s_, "circuit.add", json!({"part": "TP", "count": 3}));
    exec(r, s_, "net.connect", json!({"net": "A", "pins": ["U1.1", "J1.1"]}));
    exec(r, s_, "net.connect", json!({"net": "B", "pins": ["U1.2", "J2.1"]}));
    exec(r, s_, "net.connect", json!({"net": "GND", "pins": ["J3.1"]}));
    let x = (len + 6) as f64;
    exec(r, s_, "place.set", json!({"refdes": "U1", "at": [mm(x), "10mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J1", "at": ["2mm", "18mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J2", "at": [mm(width as f64 - 2.0), "18mm"]}));
    exec(r, s_, "place.set", json!({"refdes": "J3", "at": ["2mm", "2mm"]}));
    let mut track = |a: f64, b: f64, net: &str| {
        let o = exec(
            r,
            s_,
            "track.add",
            json!({"layer": "F.Cu", "points": [[mm(a), "10mm"], [mm(b), "10mm"]], "width": "1mm", "net": net}),
        );
        o["output"]["tracks"][0]["id"].as_u64().unwrap()
    };
    let (t1, t2) = (track(x - len as f64 - 1.0, x - 0.8, "A"), track(x + 0.8, x + len as f64 + 1.0, "B"));
    exec(r, &mut s, "via.add", json!({"at": ["2mm", "2mm"], "net": "GND", "drill": "0.3mm", "diameter": "0.6mm"}));
    exec(r, &mut s, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    (dir, s, [t1, t2])
}

fn part_args(dir: &Path, t: [u64; 2]) -> Value {
    json!({"dir": dir, "name": "dut", "around": [format!("track#{}", t[0]), format!("track#{}", t[1])],
        "copper": "sheet", "perfect_copper": true, "f_max": "6GHz", "points": 13, "margin": "4mm", "air": "4mm",
        "max_cell": "0.5mm", "pml_cells": 6, "thirds": true, "end_criteria": "50dB",
        "ports": [{"at": format!("track#{}", t[0]), "kind": "msl", "length": "3mm"},
                  {"at": format!("track#{}", t[1]), "end": "end", "kind": "msl", "length": "3mm"}]})
}

/// With openEMS installed: a series resistor and a series capacitor on a microstrip, exported and
/// run by openEMS, against `sim.sparams` on the same options (PML beyond the region in both) and
/// against circuit theory, S21 = 2 Z₀ / (2 Z₀ + Z). The tables go to `summary_parts.md`.
#[test]
fn lumped_parts_oracle() {
    let Some(_openems) = oracle::optional(Oracle::OpenEms) else { return };
    let python = std::env::var("CADLAB_ORACLE_PYTHON").unwrap_or_else(|_| "python3".into());
    let r = Registry::with_builtins();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("openems_parts");
    let _ = std::fs::remove_dir_all(&out);
    let cases = [
        ("series_r", "resistor", json!({"resistance": "50"}), None),
        ("series_c", "capacitor", json!({"capacitance": "1pF"}), Some(1e-12)),
    ];
    let mut md = String::new();
    let mut fails = Vec::new();
    for (name, category, params, cap) in cases {
        let (_dir, mut s, t) = part_board(&r, category, params, 6);
        let mut a = part_args(&out, t);
        a["name"] = json!(name);
        let o = exec(&r, &mut s, "export.openems", a.clone());
        assert_eq!(o["output"]["parts"].as_array().unwrap().len(), 1, "{}", o["output"]);
        a["pml_outside"] = json!(true);
        let read = |file: String| {
            cadlab::touchstone::parse(&std::fs::read_to_string(out.join(file)).unwrap(), Some(2)).unwrap()
        };
        // `sim.sparams` first: a failure there shows before openEMS has run.
        a["name"] = json!(format!("{name}_cadlab"));
        exec(&r, &mut s, "sim.sparams", a.clone());
        let cl = read(format!("{name}_cadlab.s2p"));
        oracle::run(Path::new(&python), &[out.join(format!("{name}.py")).to_str().unwrap()]);
        let oe = read(format!("{name}.s2p"));
        assert_eq!(oe.freqs, cl.freqs, "{name}");
        md +=
            &format!("\n#### {name}\n\n| f (GHz) | |S21| openEMS | |S21| cadlab | |S21| theory |\n|---|---|---|---|\n");
        for (i, (f, x)) in oe.freqs.iter().zip(&oe.data).enumerate() {
            let z = match cap {
                None => C64::new(50.0, 0.0),
                Some(c) => C64::new(0.0, -1.0 / (2.0 * std::f64::consts::PI * f * c)),
            };
            let ideal = (C64::new(100.0, 0.0) / (C64::new(100.0, 0.0) + z)).abs();
            let sa = c(&x[2]).abs();
            let sb = c(&cl.data[i][2]).abs();
            md += &format!("| {:.2} | {sa:.4} | {sb:.4} | {ideal:.4} |\n", f / 1e9);
            if !(0.5e9..=4.5e9).contains(f) {
                continue;
            }
            // (Written so that a NaN fails.)
            if (sa - sb).abs().partial_cmp(&0.03) != Some(std::cmp::Ordering::Less) {
                fails.push(format!("{name} {f}: openEMS {sa} against sim.sparams {sb}"));
            }
            if (sa - ideal).abs() > 0.03 {
                fails.push(format!("{name} {f}: openEMS {sa} against theory {ideal}"));
            }
        }
    }
    std::fs::write(out.join("summary_parts.md"), &md).unwrap();
    eprintln!("{md}");
    assert!(fails.is_empty(), "{fails:?}\n{md}");
}

/// With openEMS installed (`CADLAB_ORACLES=1`, the `openEMS` executable or
/// `CADLAB_ORACLE_OPENEMS`, and `CADLAB_ORACLE_PYTHON` with openEMS's Python modules): a lossy
/// microstrip (Djordjevic–Sarkar laminate, 58 MS/m copper) exported and simulated by openEMS,
/// against `sim.sparams` on the same region and mesh options, twice: sheet copper between
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
    let cases = [
        ("msl", "sheet copper, microstrip ports", true, 3.6e9),
        ("lumped", "solid copper, lumped ports", false, 2.6e9),
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
    for (name, title, msl, z_until) in cases {
        let common = line_oracle_args(&out, &track, msl);
        let mut args = common.clone();
        args["name"] = json!(name);
        exec(&r, &mut s, "export.openems", args);
        let read = |file: String| {
            cadlab::touchstone::parse(&std::fs::read_to_string(out.join(file)).unwrap(), Some(2)).unwrap()
        };
        // `sim.sparams` first: a failure there shows before openEMS has run.
        let mut args = common.clone();
        args["name"] = json!(format!("{name}_cadlab"));
        exec(&r, &mut s, "sim.sparams", args);
        let cl = read(format!("{name}_cadlab.s2p"));
        oracle::run(Path::new(&python), &[out.join(format!("{name}.py")).to_str().unwrap()]);
        let oe = read(format!("{name}.s2p"));
        assert_eq!(oe.freqs, cl.freqs, "{name}");
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
            // |S21| within 15 % and 0.025 dB (the coarse runs' noise on a lossless line); a NaN fails.
            let d = (db(&a[2]) - db(&b[2])).abs();
            loss = loss.max(d / db(&b[2]).abs());
            if d.partial_cmp(&(0.15 * db(&b[2]).abs() + 0.025)) != Some(std::cmp::Ordering::Less) {
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
        if !(z_rel < 0.05 && z_2d < 0.05 && s11 < 0.1) {
            fails.push(format!("{name}: Z₀ {z_rel} / {z_2d}, |S11| {s11}"));
        }
    }
    std::fs::write(out.join("summary.md"), &md).unwrap();
    eprintln!("{md}");
    assert!(fails.is_empty(), "{fails:?}\n{md}");
}

// ---------------------------------------------------------------------------------------------
// ems against openEMS: time, memory and accuracy per case of the oracle's tables (D103).

/// The cases of [`solver_comparison`]: the oracle's two lossy lines, its via transition and its
/// two lumped parts, and the via transition once more with the thirds rule stated (`via_same`).
/// The oracle's via case names no `thirds`, which `sim.sparams` has on by default and
/// `export.openems` off: its two solvers run on two meshes (D103). `via_same` gives both the
/// rule, and so the same lines; it is last, so that a failure of openEMS on a mesh the oracle
/// has not run costs the table nothing (it is written after every case).
const CMP_CASES: [&str; 6] = ["line_msl", "line_lumped", "via", "series_r", "series_c", "via_same"];

/// A case's board and the options both solvers run it on (the oracle tests' own; `name` is the
/// caller's).
fn comparison_case(r: &Registry, case: &str, out: &Path) -> (tempfile::TempDir, Session, Value) {
    match case {
        "line_msl" | "line_lumped" => {
            let (dir, s, id) = microstrip_board(r, 24);
            let args = line_oracle_args(out, &format!("track#{id}"), case == "line_msl");
            (dir, s, args)
        }
        "via" | "via_same" => {
            let (dir, mut s) = new_project(r, "rf");
            via_transition(r, &mut s);
            let mut args = via_oracle_args(out);
            if case == "via_same" {
                args["thirds"] = json!(true);
            }
            (dir, s, args)
        }
        "series_r" | "series_c" => {
            let (category, params) = if case == "series_r" {
                ("resistor", json!({"resistance": "50"}))
            } else {
                ("capacitor", json!({"capacitance": "1pF"}))
            };
            let (dir, s, t) = part_board(r, category, params, 6);
            let mut args = part_args(out, t);
            args["pml_outside"] = json!(true);
            (dir, s, args)
        }
        other => panic!("unknown comparison case {other:?}"),
    }
}

/// `args` with the members of `extra` set.
fn with(mut args: Value, extra: &Value) -> Value {
    for (k, v) in extra.as_object().into_iter().flatten() {
        args[k] = v.clone();
    }
    args
}

/// The peak resident memory of this process (kB): `VmHWM` (0 where `/proc` has none).
fn peak_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let line = status.lines().find(|l| l.starts_with("VmHWM:"));
    line.and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// What a measured process reports: this prefix, then one JSON object, on a line of its output.
const CMP_MARK: &str = "CADLAB_CMP ";

/// One `sim.sparams` run in a process of its own, for [`solver_comparison`] (which starts this
/// test with `CADLAB_CMP_CASE`, the output directory `CADLAB_CMP_DIR`, the file name
/// `CADLAB_CMP_NAME`, `CADLAB_CMP_THREADS` and, for another mesh, options as JSON in
/// `CADLAB_CMP_ARGS`): a process's peak memory is the largest run it ever made, and a time taken
/// beside another test's run is not the solver's. Prints the command's time, the time stepping's
/// share of it (from the progress reports), the mesh, the steps and the process's peak before and
/// after the run. Does nothing without `CADLAB_CMP_CASE`.
#[test]
#[ignore]
fn comparison_ems_run() {
    let Ok(case) = std::env::var("CADLAB_CMP_CASE") else { return };
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
    let out = std::path::PathBuf::from(var("CADLAB_CMP_DIR"));
    let r = Registry::with_builtins();
    let (_dir, mut s, mut args) = comparison_case(&r, &case, &out);
    if let Ok(extra) = std::env::var("CADLAB_CMP_ARGS") {
        args = with(args, &serde_json::from_str(&extra).expect("CADLAB_CMP_ARGS is JSON"));
    }
    args["name"] = json!(var("CADLAB_CMP_NAME"));
    args["threads"] = json!(var("CADLAB_CMP_THREADS").parse::<u32>().expect("CADLAB_CMP_THREADS"));
    let before = peak_kb();
    let stamps = Stamps(std::sync::Mutex::new(Vec::new()));
    let t = std::time::Instant::now();
    let o = r
        .execute_with(&mut s, "sim.sparams", args, RunOptions::default(), &stamps, &cadlab::command::CancelToken::new())
        .unwrap_or_else(|f| panic!("sim.sparams failed: {}", f.error));
    let seconds = t.elapsed().as_secs_f64();
    let o = serde_json::to_value(&o).unwrap();
    let x = &o["output"];
    let runs = x["runs"].as_array().unwrap();
    // A port's time stepping: from its first progress report to its last, scaled to all its steps.
    let stamps = stamps.0.into_inner().unwrap();
    let engine: f64 = (1..=runs.len())
        .map(|port| {
            let of: Vec<&(String, u64, std::time::Instant)> =
                stamps.iter().filter(|s| s.0.starts_with(&format!("port {port}/"))).collect();
            match (of.first(), of.last()) {
                (Some(a), Some(b)) if b.1 > a.1 => (b.2 - a.2).as_secs_f64() * b.1 as f64 / (b.1 - a.1) as f64,
                _ => f64::NAN,
            }
        })
        .sum();
    let report = json!({
        "seconds": seconds,
        "engine_seconds": engine.is_finite().then_some(engine),
        "cells": x["mesh"]["cells"],
        "steps": runs.iter().map(|r| r["steps"].clone()).collect::<Vec<_>>(),
        "converged": runs.iter().all(|r| r["converged"] == true),
        "timestep": x["timestep"],
        "conformal_edges": x["conformal_edges"],
        "solver": x["solver"],
        "before_kb": before,
        "peak_kb": peak_kb(),
    });
    println!("\n{CMP_MARK}{report}");
    // The run against its reference, for a run made by hand with other options.
    let ts = cadlab::touchstone::parse(&std::fs::read_to_string(x["touchstone"].as_str().unwrap()).unwrap(), Some(2));
    match (case.as_str(), ts) {
        ("line_msl" | "line_lumped", Ok(ts)) => {
            let (a, rows) = line_accuracy(&ts, case == "line_msl");
            println!(
                "loss against D52 from 0.5 GHz: largest {:.4} dB, RMS {:.1} %, mean {:+.1} %; Z₀ {:.1} %",
                a[0], a[1], a[2], a[3]
            );
            for (f, want, got) in rows {
                println!("  {:.2} GHz: D52 {want:.4} dB, this run {got:.4} dB", f / 1e9);
            }
        }
        ("series_r" | "series_c", Ok(ts)) => {
            let worst = part_accuracy(&ts, (case == "series_c").then_some(1e-12));
            println!(
                "|S21| against circuit theory, 0.5 to 4.5 GHz: within {:.4} of the ideal, {:.4} of the part between \
                 its lines",
                worst[0], worst[1]
            );
            for (f, s) in ts.freqs.iter().zip(&ts.data) {
                println!("  {:.2} GHz: |S21| {:.4}, |S11| {:.4}", f / 1e9, c(&s[2]).abs(), c(&s[0]).abs());
            }
        }
        _ => {}
    }
}

/// The progress reports of a `sim.sparams` run with the time of each: message, steps done, when.
struct Stamps(std::sync::Mutex<Vec<(String, u64, std::time::Instant)>>);

impl cadlab::command::Progress for Stamps {
    fn report(&self, done: u64, _: Option<u64>, message: &str) {
        self.0.lock().unwrap().push((message.to_string(), done, std::time::Instant::now()));
    }
}

/// The Python program that runs an exported driver for [`solver_comparison`]: the driver's own
/// imports first (the process's peak before any simulation), then the driver as `__main__`, then
/// the times and the peak resident set (its own and its children's) as JSON in a file.
const CMP_WRAPPER: &str = r#"import json, os, resource, runpy, sys, time
t0 = time.time()
import numpy  # noqa: the driver's imports
import CSXCAD  # noqa
import openEMS  # noqa
t1 = time.time()
kb = lambda who: resource.getrusage(who).ru_maxrss
before = kb(resource.RUSAGE_SELF)
script, report = sys.argv[1], sys.argv[2]
sys.argv = [script]
runpy.run_path(script, run_name='__main__')
t2 = time.time()
with open(report, 'w') as f:
    json.dump({'import_seconds': t1 - t0, 'seconds': t2 - t0, 'before_kb': before,
               'peak_kb': max(kb(resource.RUSAGE_SELF), kb(resource.RUSAGE_CHILDREN))}, f)
"#;

/// A measured run of either solver.
struct Measured {
    threads: u32,
    /// The whole run: `sim.sparams` as a command, or the driver's process with its imports.
    seconds: f64,
    /// The time stepping alone: openEMS's own account (the sum of its "Time for N iterations"
    /// lines); for ems, each port from its first progress report to its last, scaled to its steps.
    engine_seconds: Option<f64>,
    /// The solver's own count (openEMS counts the mesh lines, ems the cells between them).
    cells: Option<u64>,
    steps: Vec<u64>,
    /// The time step, seconds.
    timestep: Option<f64>,
    /// The process's peak resident set, and what it was before the run (MB).
    peak_mb: f64,
    before_mb: f64,
    /// The Touchstone file's text.
    text: String,
    note: String,
}

impl Measured {
    fn data(&self) -> cadlab::touchstone::Touchstone {
        cadlab::touchstone::parse(&self.text, Some(2)).unwrap()
    }
}

/// Runs a case with ems in a process of its own ([`comparison_ems_run`]).
fn measure_ems(case: &str, out: &Path, name: &str, threads: u32, extra: Option<&Value>) -> Measured {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["comparison_ems_run", "--exact", "--ignored", "--nocapture", "--test-threads", "1"])
        .env("CADLAB_CMP_CASE", case)
        .env("CADLAB_CMP_DIR", out)
        .env("CADLAB_CMP_NAME", name)
        .env("CADLAB_CMP_THREADS", threads.to_string())
        .env("RAYON_NUM_THREADS", threads.to_string());
    match extra {
        Some(e) => cmd.env("CADLAB_CMP_ARGS", e.to_string()),
        None => cmd.env_remove("CADLAB_CMP_ARGS"),
    };
    let o = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{case} with ems failed:\n{stdout}\n{}", String::from_utf8_lossy(&o.stderr));
    let line = stdout.split(CMP_MARK).nth(1).and_then(|l| l.lines().next()).expect("the run's report");
    let v: Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["converged"], true, "{case} with ems did not converge: {v}");
    Measured {
        threads,
        seconds: v["seconds"].as_f64().unwrap(),
        engine_seconds: v["engine_seconds"].as_f64(),
        cells: v["cells"].as_u64(),
        steps: v["steps"].as_array().unwrap().iter().filter_map(Value::as_u64).collect(),
        timestep: v["timestep"].as_f64(),
        peak_mb: v["peak_kb"].as_f64().unwrap() / 1024.0,
        before_mb: v["before_kb"].as_f64().unwrap() / 1024.0,
        text: std::fs::read_to_string(out.join(format!("{name}.s2p"))).unwrap(),
        note: v["solver"].as_str().unwrap_or("ems").to_string(),
    }
}

/// The number that follows `key` in `line`.
fn number_after(line: &str, key: &str) -> Option<f64> {
    let rest = line.split(key).nth(1)?.trim_start();
    let end =
        rest.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Runs an exported driver (`name`.py in `out`) with openEMS on `threads` threads, through
/// [`CMP_WRAPPER`]. openEMS's console output is kept as `name`.log; its engine's time, steps and
/// cells are read from the lines it prints at the end of each run ("Time for N iterations with C
/// cells : T sec"), and are missing from the result if it prints something else.
fn measure_openems(python: &str, out: &Path, name: &str, threads: u32) -> Measured {
    let wrapper = out.join("measure.py");
    std::fs::write(&wrapper, CMP_WRAPPER).unwrap();
    let report = out.join(format!("{name}.measure.json"));
    let o = std::process::Command::new(python)
        .arg(&wrapper)
        .arg(out.join(format!("{name}.py")))
        .arg(&report)
        .env("CADLAB_OPENEMS_THREADS", threads.to_string())
        .env("OMP_NUM_THREADS", threads.to_string())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    std::fs::write(out.join(format!("{name}.log")), &stdout).unwrap();
    assert!(o.status.success(), "{name} with openEMS failed:\n{stdout}\n{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let ends: Vec<&str> = stdout.lines().filter(|l| l.contains("Time for ") && l.contains(" iterations")).collect();
    let engine: Vec<f64> = ends.iter().filter_map(|l| number_after(l, " : ")).collect();
    let mut note: Vec<String> =
        stdout.lines().filter(|l| l.contains("number of threads")).map(|l| l.trim().to_string()).collect();
    note.dedup();
    // (Printed once per run; in full precision by the first only.)
    let timestep = stdout.lines().filter_map(|l| number_after(l, "FDTD timestep is: ")).find(|t| *t > 0.0);
    Measured {
        threads,
        seconds: v["seconds"].as_f64().unwrap(),
        engine_seconds: (!engine.is_empty() && engine.len() == ends.len()).then(|| engine.iter().sum()),
        cells: ends.first().and_then(|l| number_after(l, " with ")).map(|c| c as u64),
        steps: ends.iter().filter_map(|l| number_after(l, "Time for ")).map(|n| n as u64).collect(),
        timestep,
        peak_mb: v["peak_kb"].as_f64().unwrap() / 1024.0,
        before_mb: v["before_kb"].as_f64().unwrap() / 1024.0,
        text: std::fs::read_to_string(out.join(format!("{name}.s2p"))).unwrap(),
        note: note.join("; "),
    }
}

/// A lossy line's result against the line's own model, D52 (the 2D field solver's matrices of
/// the 1 mm strip of 35 µm copper on 0.5 mm of the Djordjevic–Sarkar laminate, with the skin
/// effect: no FDTD in it), from 0.5 GHz. The loss is the line's without its mismatch,
/// −10 log₁₀(|S21|² / (1 − |S11|²)), against 8.686 α l; Z₀ is the ABCD matrix's below the first
/// half-wave resonance (`z_until`), against the field solver's for the strip the model has (zero
/// thickness with sheets, 35 µm solid). Returns the largest difference of the loss (dB), its
/// RMS relative to the model's (%), its mean relative to the model's (%, signed), the largest
/// difference of Z₀ (%), and the loss per frequency.
fn line_accuracy(ts: &cadlab::touchstone::Touchstone, msl: bool) -> ([f64; 4], Vec<(f64, f64, f64)>) {
    use cadlab::electrical::fieldsolver::{Accuracy, LossTangents, Structure};
    use cadlab::electrical::loss::{self, COPPER_CONDUCTIVITY, Conductor, Roughness};
    let strip = |t: f64| Structure {
        model: cadlab::electrical::Model::Microstrip,
        h: 0.5,
        er: 4.0,
        h2: 0.0,
        er2: 4.0,
        t,
        etch: 0.0,
        broadside: None,
        coplanar_gap: None,
        mask: None,
    };
    let tans = LossTangents { below: 0.02, ref_freq: [Some(1e9), None, None], ..LossTangents::default() };
    let m = strip(0.035).line_matrices(1.0, None, false, &tans, Accuracy::default()).unwrap();
    let cond = Conductor {
        sigma: COPPER_CONDUCTIVITY,
        roughness: Roughness::Smooth,
        r_dc: 1.0 / (COPPER_CONDUCTIVITY * 1e-3 * 35e-6),
    };
    let z_sheet = strip(0.0).line(1.0, Accuracy::default()).unwrap().z0;
    // Between the reference planes of the microstrip ports (2 mm inside each end), or the ends.
    let (l, z_until) = if msl { (0.020, 3.6e9) } else { (0.024, 2.6e9) };
    let (mut worst, mut sq, mut mean, mut z_worst, mut n) = (0.0f64, 0.0, 0.0, 0.0f64, 0);
    let mut rows = Vec::new();
    for (f, s) in ts.freqs.iter().zip(&ts.data) {
        let p = loss::at(&m, *f, &cond);
        let want = 20.0 * std::f64::consts::LOG10_E * p.alpha * l;
        let (s11, s21) = (c(&s[0]).abs(), c(&s[2]).abs());
        let got = -10.0 * (s21 * s21 / (1.0 - s11 * s11)).log10();
        rows.push((*f, want, got));
        if *f < 0.5e9 {
            continue;
        }
        worst = worst.max((got - want).abs());
        sq += ((got - want) / want).powi(2);
        mean += (got - want) / want;
        n += 1;
        if *f <= z_until {
            let z = line_from_s(s, 50.0).0.re;
            let z_ref = if msl { z_sheet } else { p.z0.re };
            z_worst = z_worst.max((z - z_ref).abs() / z_ref);
        }
    }
    let n = n.max(1) as f64;
    ([worst, 100.0 * (sq / n).sqrt(), 100.0 * mean / n, 100.0 * z_worst], rows)
}

/// A series part's |S21| from 0.5 to 4.5 GHz against circuit theory, twice: the largest
/// difference from the ideal, 2 Z₀ / (2 Z₀ + Z) (what the oracle test holds both solvers to), and
/// from the part between the lines it sits on. The ideal takes the board's lines for 50 Ω; the
/// 1 mm strip on 0.5 mm of εr 4 is the 2D field solver's 50.8 Ω, and a 50 Ω part between 5.15 mm
/// of it on each side (from a port's reference plane, the middle of its 3 mm, to the pad's edge
/// at the gap) reads up to 0.004 above the ideal at 4.5 GHz, which is as much as the solvers
/// differ by. The second reference is that cascade, line, part, line, in 50 Ω: circuit theory on
/// the field solver's Z₀ and εeff, no FDTD in it.
fn part_accuracy(ts: &cadlab::touchstone::Touchstone, cap: Option<f64>) -> [f64; 2] {
    use cadlab::electrical::fieldsolver::{Accuracy, Structure};
    let strip = Structure {
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
    let line = strip.line(1.0, Accuracy::default()).unwrap();
    let (zl, length) = (C64::new(line.z0, 0.0), 5.15e-3);
    let k = |x: f64| C64::new(x, 0.0);
    let mut worst = [0.0f64; 2];
    for (f, s) in ts.freqs.iter().zip(&ts.data).filter(|(f, _)| (0.5e9..=4.5e9).contains(*f)) {
        let z = match cap {
            None => k(50.0),
            Some(cap) => C64::new(0.0, -1.0 / (2.0 * std::f64::consts::PI * f * cap)),
        };
        let got = c(&s[2]).abs();
        let ideal = (k(100.0) / (k(100.0) + z)).abs();
        // ABCD of line, series Z, line: [[c, j Zl s], [j s / Zl, c]] on each side.
        let theta = 2.0 * std::f64::consts::PI * f * line.er_eff.sqrt() / cadlab::openems::C0 * length;
        let (cs, js) = (k(theta.cos()), C64::new(0.0, theta.sin()));
        let side = [[cs, js * zl], [js / zl, cs]];
        let mul = |a: [[C64; 2]; 2], b: [[C64; 2]; 2]| {
            [
                [a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]],
                [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]],
            ]
        };
        let m = mul(mul(side, [[k(1.0), z], [k(0.0), k(1.0)]]), side);
        let on_lines = (k(2.0) / (m[0][0] + m[0][1] / k(50.0) + m[1][0] * k(50.0) + m[1][1])).abs();
        worst[0] = worst[0].max((got - ideal).abs());
        worst[1] = worst[1].max((got - on_lines).abs());
    }
    worst
}

/// The largest difference of |S21| (dB) between two results, from 0.5 GHz.
fn s21_distance(a: &cadlab::touchstone::Touchstone, b: &cadlab::touchstone::Touchstone) -> f64 {
    assert_eq!(a.freqs, b.freqs);
    let db = |v: &cadlab::touchstone::Complex| 20.0 * c(v).abs().log10();
    let from = a.freqs.iter().zip(a.data.iter().zip(&b.data)).filter(|(f, _)| **f >= 0.5e9);
    from.map(|(_, (x, y))| (db(&x[2]) - db(&y[2])).abs()).fold(0.0, f64::max)
}

/// The largest difference of |S21| between two results, from 0.5 to 4.5 GHz.
fn magnitude_distance(a: &cadlab::touchstone::Touchstone, b: &cadlab::touchstone::Touchstone) -> f64 {
    assert_eq!(a.freqs, b.freqs);
    let band = a.freqs.iter().zip(a.data.iter().zip(&b.data)).filter(|(f, _)| (0.5e9..=4.5e9).contains(*f));
    band.map(|(_, (x, y))| (c(&x[2]).abs() - c(&y[2]).abs()).abs()).fold(0.0, f64::max)
}

/// The finer mesh asked for a case: `fine` is options per case name, or those of `via_same` alone.
fn fine_of<'a>(fine: &'a Value, case: &str) -> Option<&'a Value> {
    let per_case = fine.as_object().is_some_and(|o| o.keys().all(|k| CMP_CASES.contains(&k.as_str())));
    if per_case { fine.get(case) } else { (case == "via_same").then_some(fine) }
}

/// ems and openEMS side by side on the oracle's five cases (and [`CMP_CASES`]' sixth), on this machine: time, steps, cells,
/// peak memory, and how far each is from a reference that is neither of them (D52's model of the
/// line, circuit theory for the parts; the via has none, so the two are held to each other and,
/// with `CADLAB_CMP_FINE`, to each solver's run on a finer mesh). Each run is a process of its
/// own, one at a time, on a stated number of threads (`CADLAB_CMP_THREADS`, a list; default 1
/// and the machine's count): ems through [`comparison_ems_run`], openEMS through the exported
/// driver with `CADLAB_OPENEMS_THREADS`. `CADLAB_CMP_CASES` (a list) limits the cases;
/// `CADLAB_CMP_FINE` is JSON options for a finer mesh of `via_same` (`{"max_cell": "0.4mm"}`), or
/// such options per case (`{"via_same": {..}, "series_r": {..}}`), run by both solvers on the
/// largest thread count. Without openEMS (`CADLAB_ORACLES=1` and
/// `CADLAB_ORACLE_OPENEMS`) the table has ems's rows only. The table goes to
/// `$CARGO_TARGET_TMPDIR/openems_comparison/comparison.md` (and `.json`), with openEMS's console
/// output beside it; nothing is asserted but that every run ends and converges: the tolerances
/// are the oracle tests'.
#[test]
#[ignore]
fn solver_comparison() {
    let python = oracle::optional(Oracle::OpenEms)
        .map(|_| std::env::var("CADLAB_ORACLE_PYTHON").unwrap_or_else(|_| "python3".into()));
    let r = Registry::with_builtins();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("openems_comparison");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let list = |k: &str| std::env::var(k).ok().map(|v| v.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>());
    let mut threads: Vec<u32> = match list("CADLAB_CMP_THREADS") {
        Some(l) => l.iter().map(|t| t.parse().expect("CADLAB_CMP_THREADS")).collect(),
        None => vec![1, cpus],
    };
    threads.dedup();
    let cases: Vec<&str> = match list("CADLAB_CMP_CASES") {
        Some(l) => CMP_CASES.into_iter().filter(|c| l.iter().any(|x| x == c)).collect(),
        None => CMP_CASES.to_vec(),
    };
    let fine = std::env::var("CADLAB_CMP_FINE").ok().filter(|v| !v.trim().is_empty());
    let fine: Option<Value> = fine.map(|v| serde_json::from_str(&v).expect("CADLAB_CMP_FINE is JSON"));
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|c| {
            c.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split(": ").nth(1)).map(String::from)
        })
        .unwrap_or_else(|| "unknown CPU".into());

    let mut md = String::new();
    let mut time = String::from(
        "| Case | Solver | Threads | Cells | Steps per port | Time step (ps) | Whole run (s) | Engine alone (s) | Mcells/s | Peak memory (MB) | Before the run (MB) |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
    );
    let mut acc = String::from("| Case | Quantity (reference) | ems | openEMS | Nearer |\n|---|---|---|---|---|\n");
    let mut detail = String::new();
    let mut summary = format!(
        "| Case | Whole run on {} thread(s), ems / openEMS (s) | Each at its fastest (threads), ems / openEMS (s) | Peak memory, ems / openEMS (MB) | Of it the run, ems / openEMS (MB) |\n|---|---|---|---|---|\n",
        threads[0]
    );
    let mut notes = Vec::new();
    let mut record = Vec::new();
    for case in cases {
        let (_dir, mut s, args) = comparison_case(&r, case, &out);
        let mut ems = Vec::new();
        let mut oe = Vec::new();
        for &n in &threads {
            eprintln!("{case}: ems, {n} thread(s)");
            ems.push(measure_ems(case, &out, &format!("{case}_ems_t{n}"), n, None));
            if let Some(python) = &python {
                eprintln!("{case}: openEMS, {n} thread(s)");
                let name = format!("{case}_openems_t{n}");
                exec(&r, &mut s, "export.openems", with(args.clone(), &json!({"name": name})));
                oe.push(measure_openems(python, &out, &name, n));
            }
        }
        let ems_version = ems[0].note.clone();
        for (solver, runs) in [("ems", &ems), ("openEMS", &oe)] {
            for m in runs {
                let steps: u64 = m.steps.iter().sum();
                let rate = m.cells.map(|c| c as f64 * steps as f64 / m.engine_seconds.unwrap_or(m.seconds) / 1e6);
                time += &format!(
                    "| {case} | {solver} | {} | {} | {} | {} | {:.1} | {} | {} | {:.0} | {:.0} |\n",
                    m.threads,
                    m.cells.map_or("?".into(), |c| c.to_string()),
                    m.steps.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
                    m.timestep.map_or("?".into(), |t| format!("{:.4}", t * 1e12)),
                    m.seconds,
                    m.engine_seconds.map_or("".into(), |e| format!("{e:.1}")),
                    rate.map_or("?".into(), |r| format!("{r:.0}")),
                    m.peak_mb,
                    m.before_mb
                );
                record.push(json!({"case": case, "solver": solver, "threads": m.threads, "cells": m.cells,
                    "steps": m.steps, "timestep": m.timestep, "seconds": m.seconds, "engine_seconds": m.engine_seconds,
                    "peak_mb": m.peak_mb, "before_mb": m.before_mb}));
            }
            if runs.len() > 1 {
                let same = runs.iter().all(|m| m.text == runs[0].text);
                notes.push(format!(
                    "{case}, {solver}: the Touchstone file is {} on {} threads.",
                    if same { "the same bytes" } else { "not the same" },
                    runs.iter().map(|m| m.threads.to_string()).collect::<Vec<_>>().join(" and ")
                ));
            }
        }
        if case == "via" && !oe.is_empty() {
            notes.push(
                "via: the oracle's case names no `thirds`, on by default in `sim.sparams` and off in the export: \
                 the two solvers run on two meshes there. `via_same` states the rule for both."
                    .into(),
            );
        }
        if let Some(m) = oe.iter().find(|m| !m.note.is_empty()) {
            notes.push(format!("{case}, openEMS says of its threads: {}", m.note));
        }
        // The case in one row: each solver on one thread and at its fastest, and its memory.
        if !oe.is_empty() {
            let best = |runs: &[Measured]| runs.iter().min_by(|a, b| a.seconds.total_cmp(&b.seconds)).unwrap().threads;
            let at = |runs: &[Measured], n: u32| runs.iter().find(|m| m.threads == n).map_or(f64::NAN, |m| m.seconds);
            let peak = |runs: &[Measured]| runs.iter().map(|m| m.peak_mb).fold(0.0, f64::max);
            let run = |runs: &[Measured]| runs.iter().map(|m| m.peak_mb - m.before_mb).fold(0.0, f64::max);
            let (be, bo) = (best(&ems), best(&oe));
            summary += &format!(
                "| {case} | {:.1} / {:.1} ({:.1}×) | {:.1} ({be}) / {:.1} ({bo}) ({:.1}×) | {:.0} / {:.0} | {:.0} / {:.0} |\n",
                at(&ems, threads[0]),
                at(&oe, threads[0]),
                at(&oe, threads[0]) / at(&ems, threads[0]),
                at(&ems, be),
                at(&oe, bo),
                at(&oe, bo) / at(&ems, be),
                peak(&ems),
                peak(&oe),
                run(&ems),
                run(&oe)
            );
        }
        // Accuracy, on the largest thread count's results.
        let (e, o) = (ems.last().unwrap().data(), oe.last().map(Measured::data));
        let nearer = |a: f64, b: Option<f64>| match b {
            None => "",
            Some(b) if a < b => "ems",
            Some(b) if b < a => "openEMS",
            _ => "equal",
        };
        let cell = |v: Option<f64>, digits: usize| v.map_or(String::new(), |v| format!("{v:.digits$}"));
        match case {
            "line_msl" | "line_lumped" => {
                let msl = case == "line_msl";
                let (ea, er) = line_accuracy(&e, msl);
                let oa = o.as_ref().map(|o| line_accuracy(o, msl));
                let names = [
                    ("loss from 0.5 GHz, largest difference, dB (D52)", 4),
                    ("loss from 0.5 GHz, RMS of the relative difference, % (D52)", 1),
                    ("loss from 0.5 GHz, mean relative difference, % (D52)", 1),
                    ("Z₀, largest difference, % (2D field solver)", 1),
                ];
                // The first frequency, 1 % of f_max, is below what the tests hold: on its own row.
                {
                    let (f, want, got) = er[0];
                    let theirs = oa.as_ref().map(|o| (o.1[0].2 - want).abs());
                    acc += &format!(
                        "| {case} | loss at {:.2} GHz, difference, dB (D52: {want:.4} dB) | {:.4} | {} | {} |\n",
                        f / 1e9,
                        (got - want).abs(),
                        cell(theirs, 4),
                        nearer((got - want).abs(), theirs)
                    );
                }
                for (i, (name, digits)) in names.into_iter().enumerate() {
                    let b = oa.as_ref().map(|o| o.0[i]);
                    let near = nearer(ea[i].abs(), b.map(f64::abs));
                    acc += &format!("| {case} | {name} | {:.digits$} | {} | {near} |\n", ea[i], cell(b, digits));
                }
                detail += &format!(
                    "\n#### {case}: the line's loss without its mismatch, dB\n\n| f (GHz) | D52 | ems | openEMS |\n|---|---|---|---|\n"
                );
                for (i, (f, want, got)) in er.iter().enumerate() {
                    let theirs = oa.as_ref().map(|o| o.1[i].2);
                    detail += &format!("| {:.2} | {want:.4} | {got:.4} | {} |\n", f / 1e9, cell(theirs, 4));
                }
            }
            "series_r" | "series_c" => {
                let cap = (case == "series_c").then_some(1e-12);
                let (a, b) = (part_accuracy(&e, cap), o.as_ref().map(|o| part_accuracy(o, cap)));
                let references = [
                    "circuit theory, the part between ideal 50 Ω lines",
                    "circuit theory, the part between its lines as the 2D field solver has them",
                ];
                for (i, reference) in references.into_iter().enumerate() {
                    let theirs = b.map(|b| b[i]);
                    acc += &format!(
                        "| {case} | \\|S21\\|, largest difference, 0.5 to 4.5 GHz ({reference}) | {:.4} | {} | {} |\n",
                        a[i],
                        cell(theirs, 4),
                        nearer(a[i], theirs)
                    );
                }
            }
            _ => {
                if let Some(o) = &o {
                    acc += &format!(
                        "| {case} | \\|S21\\|, largest difference between the two, dB (no reference) | {:.3} | | |\n",
                        s21_distance(&e, o)
                    );
                }
            }
        }
        // Against each solver's run on a finer mesh, where one is asked for: in dB for the lines
        // and the via, as a magnitude from 0.5 to 4.5 GHz for the parts.
        let part = case.starts_with("series");
        let apart = |a: &cadlab::touchstone::Touchstone, b: &cadlab::touchstone::Touchstone| {
            if part { magnitude_distance(a, b) } else { s21_distance(a, b) }
        };
        let (unit, digits) = if part { ("0.5 to 4.5 GHz", 4) } else { ("dB", 3) };
        if let Some(fine) = fine.as_ref().and_then(|f| fine_of(f, case)) {
            let n = *threads.last().unwrap();
            eprintln!("{case}: ems on the finer mesh, {n} thread(s)");
            let fe = measure_ems(case, &out, &format!("{case}_fine_ems"), n, Some(fine));
            let fo = python.as_ref().map(|python| {
                eprintln!("{case}: openEMS on the finer mesh, {n} thread(s)");
                let name = format!("{case}_fine_openems");
                exec(&r, &mut s, "export.openems", with(with(args.clone(), fine), &json!({"name": name})));
                measure_openems(python, &out, &name, n)
            });
            for (solver, m) in [("ems", Some(&fe)), ("openEMS", fo.as_ref())] {
                let Some(m) = m else { continue };
                let steps: u64 = m.steps.iter().sum();
                let rate = m.cells.map(|c| c as f64 * steps as f64 / m.engine_seconds.unwrap_or(m.seconds) / 1e6);
                time += &format!(
                    "| {case}, finer mesh | {solver} | {} | {} | {} | {} | {:.1} | {} | {} | {:.0} | {:.0} |\n",
                    m.threads,
                    m.cells.map_or("?".into(), |c| c.to_string()),
                    m.steps.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
                    m.timestep.map_or("?".into(), |t| format!("{:.4}", t * 1e12)),
                    m.seconds,
                    m.engine_seconds.map_or("".into(), |e| format!("{e:.1}")),
                    rate.map_or("?".into(), |r| format!("{r:.0}")),
                    m.peak_mb,
                    m.before_mb
                );
                let reference = m.data();
                let (a, b) = (apart(&e, &reference), o.as_ref().map(|o| apart(o, &reference)));
                acc += &format!(
                    "| {case} | \\|S21\\|, largest difference, {unit} ({solver} on the finer mesh, {fine}) | {a:.digits$} | {} | {} |\n",
                    cell(b, digits),
                    nearer(a, b)
                );
            }
            if let Some(fo) = &fo {
                acc += &format!(
                    "| {case} | \\|S21\\|, the two finer meshes apart, {unit} | {:.digits$} | | |\n",
                    apart(&fe.data(), &fo.data())
                );
            }
        }
        // The table so far, after every case.
        md.clear();
        md += &format!(
            "Machine: {cpu}, {cpus} logical CPUs. {ems_version}{}. One run each, one process per run, one at a time.\n\
             Whole run: `sim.sparams` as a command (model, both ports' runs, spectra, file) for ems; the driver's process \
             for openEMS (Python and its modules, geometry, both ports' runs, port calculation, file). Engine alone: the time \
             stepping, as openEMS prints it at the end of each run and as ems's progress reports time it. Mcells/s: \
             cells × steps over the engine's time. Cells: each solver's own count (openEMS counts the mesh lines, ems the cells \
             between them; with the absorbing layer beyond the region openEMS's mesh has those cells too). Before the \
             run: the test's process with the board for ems, Python with numpy, CSXCAD and openEMS imported for openEMS; \
             \"of it the run\" is the peak less that.\n\n",
            match (&python, std::env::var("OPENEMS_PROJECT_REV")) {
                (Some(_), Ok(rev)) => format!(", openEMS-Project {rev}"),
                (Some(_), Err(_)) => ", openEMS".into(),
                (None, _) => ", openEMS not installed".into(),
            }
        );
        if python.is_some() {
            md += &summary;
            md += "\n#### Every run\n\n";
        }
        md += &time;
        md += "\n#### Accuracy against references that are neither solver\n\n";
        md += &acc;
        md += &detail;
        md += "\n";
        for n in &notes {
            md += &format!("- {n}\n");
        }
        std::fs::write(out.join("comparison.md"), &md).unwrap();
        std::fs::write(out.join("comparison.json"), serde_json::to_string_pretty(&record).unwrap()).unwrap();
    }
    eprintln!("{md}");
}
