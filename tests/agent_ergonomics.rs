//! One test per friction point fixed after the agent session (docs/TESTING.md, "Agent sessions";
//! the whole session is tests/agent_session.rs).

use std::path::Path;
use std::process::{Command, Output};

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

fn diag<'a>(o: &'a Value, code: &str) -> Option<&'a Value> {
    o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == code)
}

fn new_project(r: &Registry) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    exec(r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    (dir, s)
}

fn cadlab(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cadlab"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
        .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
        .env("XDG_DATA_HOME", cwd.join(".no-data"))
        .env_remove("CADLAB_CATALOGS")
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

const SUPPLY: &str = r#"[{"number":"1","name":"VBUS","kind":"power_out"},{"number":"2","name":"VBUS","kind":"power_out"},{"number":"3","name":"GND"}]"#;

/// A connector's two VBUS contacts are one driver; two different sources still conflict.
#[test]
fn erc_counts_same_named_outputs_of_one_part_once() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let pins: Value = serde_json::from_str(SUPPLY).unwrap();
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "SRC", "category": "connector", "pins": pins, "package": "PinHeader 1x03"}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "SRC", "refdes": "J1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0402"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VBUS", "pins": ["J1.VBUS", "R1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["J1.GND", "R1.2"]}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert!(diag(&o, "erc.output_conflict").is_none(), "{}", o["diagnostics"]);

    exec(&r, &mut s, "circuit.add", json!({"part": "SRC", "refdes": "J2"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VBUS", "pins": ["J2.VBUS"]}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    let d = diag(&o, "erc.output_conflict").expect("two connectors driving VBUS");
    assert!(d["message"].as_str().unwrap().contains("J2.1"), "{d}");
}

/// A part created without any footprint source says it cannot be placed, and how to fix it.
#[test]
fn part_without_footprint_warns() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let o = exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "USBC", "category": "connector", "pins": [{"number": "A9", "name": "VBUS"}]}),
    );
    let d = diag(&o, "part.no_footprint").expect("warning");
    assert!(d["hint"].as_str().unwrap().contains("footprint.import_kicad"));
    let show = exec(&r, &mut s, "part.show", json!({"id": "USBC"}));
    assert!(show["summary"].as_str().unwrap().contains("footprint: none"), "{}", show["summary"]);

    // With a package: no warning.
    let o = exec(&r, &mut s, "part.create", json!({"id": "HDR", "category": "connector", "package": "PinHeader 1x02"}));
    assert!(diag(&o, "part.no_footprint").is_none());
}

/// Summaries print enum values as they are typed (`power_out`, `led`), not Rust names.
#[test]
fn part_show_uses_argument_names() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let pins: Value = serde_json::from_str(SUPPLY).unwrap();
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "SRC", "category": "connector", "pins": pins, "package": "PinHeader 1x03"}),
    );
    let o = exec(&r, &mut s, "part.show", json!({"id": "SRC"}));
    let summary = o["summary"].as_str().unwrap();
    assert!(summary.starts_with("SRC (connector)"), "{summary}");
    assert!(summary.contains("power_out") && !summary.contains("PowerOut"), "{summary}");
    exec(&r, &mut s, "part.generic", json!({"spec": "LED red 0603"}));
    let o = exec(&r, &mut s, "part.show", json!({"id": "LED_red_0603"}));
    assert!(o["summary"].as_str().unwrap().contains("(led)"));
}

/// Unknown package names come with a hint naming the explicit-dimension argument and the
/// KiCad import path, for both commands that take a package.
#[test]
fn unknown_package_has_a_hint() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let f = fail(&r, &mut s, "footprint.generate", json!({"package": "USB-C"}));
    let e = serde_json::to_value(&f.error).unwrap();
    assert_eq!(e["code"], "footprint.unknown_package");
    assert!(e["message"].as_str().unwrap().contains("TQFP-N"), "TQFP is accepted, so listed");
    let hint = e["hint"].as_str().unwrap();
    assert!(hint.contains("`spec`") && hint.contains("footprint.import_kicad"), "{hint}");

    let f = fail(
        &r,
        &mut s,
        "part.create",
        json!({"id": "X", "category": "ic", "package": "HC-49", "pins": [{"number": "1"}]}),
    );
    let e = serde_json::to_value(&f.error).unwrap();
    assert!(e["hint"].as_str().unwrap().contains("`package_spec`"), "{e}");
}

/// Route budgets take a unit like every other quantity; `budget_ms` still works.
#[test]
fn route_budget_with_units() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    exec(&r, &mut s, "board.outline", json!({"width": "10mm", "height": "10mm"}));
    exec(&r, &mut s, "route.all", json!({"budget": "5s"}));
    exec(&r, &mut s, "route.all", json!({"budget_ms": 5000}));
    let e = serde_json::to_value(&fail(&r, &mut s, "route.all", json!({"budget": "5"})).error).unwrap();
    assert_eq!(e["code"], "route.budget");
    assert!(e["message"].as_str().unwrap().contains("no unit"), "{e}");
    let e =
        serde_json::to_value(&fail(&r, &mut s, "route.all", json!({"budget": "5s", "budget_ms": 5})).error).unwrap();
    assert_eq!(e["code"], "route.budget");
}

/// The fab-derived "comfortable" rules keep silk limits as published, so generated footprints
/// pass; a silk clearance that a footprint's own drawing violates gets a hint that fits.
#[test]
fn generated_footprints_pass_fab_silk_rules() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "20mm"}));
    exec(&r, &mut s, "board.rules", json!({"fab": "jlcpcb"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0402"}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "LDO", "category": "ldo", "package": "SOT-23-5",
        "pins": [{"number": "1"}, {"number": "2"}, {"number": "3"}, {"number": "4"}, {"number": "5"}]}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "LDO", "refdes": "U1"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "10mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R1", "at": ["4mm", "4mm"]}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert!(diag(&o, "drc.silk_over_pad").is_none(), "{}", o["diagnostics"]);

    exec(&r, &mut s, "board.rules", json!({"silk_to_pad": "0.3mm"}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let d = diag(&o, "drc.silk_over_pad").expect("own silk now too close");
    assert!(d["hint"].as_str().unwrap().contains("footprint's own silkscreen"), "{d}");
}

/// Every "no supplier" diagnostic gives the same, complete set of options.
#[test]
fn no_supplier_hints_agree() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "20mm"}));
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "jlcpcb"}));
    let d = diag(&o, "fab.no_suppliers").expect("no suppliers in a fresh session");
    assert_eq!(d["hint"], cadlab::supplier::NO_SUPPLIERS_HINT);
    let e = serde_json::to_value(&fail(&r, &mut s, "part.search", json!({"query": "LDO"})).error).unwrap();
    assert_eq!(e["hint"], cadlab::supplier::NO_SUPPLIERS_HINT);
}

/// CLI forms an agent reaches for first: group help that says what is inside, `erc`/`lint`/`drc`
/// shortcuts, points as `x,y`, JSON from `@file`, global options under their own heading.
#[test]
fn cli_shortcuts_and_argument_forms() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let o = cadlab(root, &["--help"]);
    let help = text(&o);
    assert!(help.contains("Components, ERC, design lint"), "{help}");
    assert!(!help.contains("circuit commands"), "{help}");
    assert!(help.contains("erc ") && help.contains("alias of `circuit erc`"), "{help}");

    assert!(cadlab(root, &["project", "new", "p"]).status.success());
    let p = root.join("p");
    std::fs::write(root.join("pins.json"), r#"[{"number":"1","name":"A"},{"number":"2","name":"B"}]"#).unwrap();
    let pins = format!("@{}", root.join("pins.json").display());
    let o = cadlab(
        &p,
        &["part", "create", "--category", "connector", "--id", "H", "--package", "PinHeader 1x02", "--pins", &pins],
    );
    assert!(o.status.success(), "{}", text(&o));
    let o = cadlab(&p, &["part", "create", "--category", "connector", "--id", "H2", "--pins", "@missing.json"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("cannot read `missing.json`"), "{}", text(&o));

    assert!(cadlab(&p, &["circuit", "add", "H"]).status.success());
    assert!(cadlab(&p, &["board", "outline", "--width", "20mm", "--height", "20mm"]).status.success());
    let o = cadlab(&p, &["place", "set", "J1", "--at", "5mm,6mm"]);
    assert!(o.status.success(), "{}", text(&o));
    let o = cadlab(&p, &["place", "set", "J1", "--at", "5mm"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("expected 2 comma-separated values"), "{}", text(&o));
    let o = cadlab(&p, &["place", "set", "J1", "--at", r#"["7mm","6mm"]"#, "--json"]);
    assert!(o.status.success(), "{}", text(&o));

    // `erc` exits 0 when clean of errors (pins are unconnected: warnings only); `drc` alone runs
    // the check (unrouted nets are none: nothing is connected).
    let o = cadlab(&p, &["erc", "--json"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["command"], "circuit.erc");
    let o = cadlab(&p, &["lint", "--json"]);
    assert_eq!(serde_json::from_slice::<Value>(&o.stdout).unwrap()["command"], "circuit.lint");
    let o = cadlab(&p, &["drc", "--json"]);
    assert_eq!(serde_json::from_slice::<Value>(&o.stdout).unwrap()["command"], "drc.run", "{}", text(&o));

    let o = cadlab(&p, &["place", "set", "--help"]);
    let help = text(&o);
    assert!(help.contains("Global options:") && help.contains("--at <X,Y>"), "{help}");

    // Render output paths are positional for symbols and footprints too.
    let o = cadlab(&p, &["render", "footprint", "H", "out/h.svg"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(p.join("out/h.svg").is_file());
}

// ---------------------------------------------------------------------------------------------
// Session 2: what session 1 left open.

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session").join(name)
}

fn mm(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches("mm").parse().unwrap()
}

/// An MCU with three headers, a crystal and decoupling, on a board much larger than its parts.
fn spread_board(r: &Registry) -> (tempfile::TempDir, Session) {
    let (d, mut s) = new_project(r);
    let pins: Value = serde_json::from_str(&std::fs::read_to_string(fixture("atmega328p_pins.json")).unwrap()).unwrap();
    exec(r, &mut s, "part.create", json!({"id": "MCU", "category": "mcu", "package": "TQFP-32", "pins": pins}));
    let hdr = json!([{"number":"1","name":"A"},{"number":"2","name":"B"},{"number":"3","name":"C"},{"number":"4","name":"GND"}]);
    exec(
        r,
        &mut s,
        "part.create",
        json!({"id": "HDR", "category": "connector", "package": "PinHeader 1x04", "pins": hdr}),
    );
    let xtal = json!([{"number":"1","name":"X1"},{"number":"2","name":"GND"},{"number":"3","name":"X2"},{"number":"4","name":"GND"}]);
    exec(
        r,
        &mut s,
        "part.create",
        json!({"id": "XTAL", "category": "crystal", "package": "DFN-4 3.2x2.5mm P1.6mm", "pins": xtal}),
    );
    exec(r, &mut s, "circuit.add", json!({"part": "MCU", "refdes": "U1"}));
    exec(r, &mut s, "circuit.add", json!({"part": "XTAL", "refdes": "Y1"}));
    exec(r, &mut s, "circuit.add", json!({"part": "HDR", "count": 3}));
    exec(r, &mut s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "count": 3}));
    let mut nets = vec![
        json!({"net": "GND", "pins": ["U1.GND", "J1.GND", "J2.GND", "J3.GND", "Y1.GND", "C1.2", "C2.2", "C3.2"]}),
        json!({"net": "VCC", "pins": ["U1.VCC", "U1.AVCC", "C1.1", "C2.1", "C3.1"]}),
        json!({"net": "XTAL1", "pins": ["U1.XTAL1", "Y1.X1"]}),
        json!({"net": "XTAL2", "pins": ["U1.XTAL2", "Y1.X2"]}),
    ];
    for (j, sig) in [("J1", ["PD0", "PD1", "PD2"]), ("J2", ["PC0", "PC1", "PC2"]), ("J3", ["MOSI", "MISO", "SCK"])] {
        for (pin, s) in ["A", "B", "C"].iter().zip(sig) {
            nets.push(json!({"net": format!("{j}_{pin}"), "pins": [format!("{j}.{pin}"), format!("U1.{s}")]}));
        }
    }
    for n in nets {
        exec(r, &mut s, "net.connect", n);
    }
    exec(r, &mut s, "board.outline", json!({"width": "60mm", "height": "45mm"}));
    (d, s)
}

fn extent(o: &Value) -> (f64, f64) {
    let pts: Vec<(f64, f64)> =
        o["output"]["placements"].as_object().unwrap().values().map(|f| (mm(&f["at"][0]), mm(&f["at"][1]))).collect();
    let w = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max) - pts.iter().map(|p| p.0).fold(f64::MAX, f64::min);
    let h = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max) - pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    (w, h)
}

/// `place.auto` spreads groups over the board: the MCU central, headers at edges and apart,
/// the crystal next to the MCU; `compact` keeps the packed placement. Both deterministic and
/// free of placement errors.
#[test]
fn auto_placement_uses_the_board() {
    let r = Registry::with_builtins();
    let (_d, mut s) = spread_board(&r);
    let o = exec(&r, &mut s, "place.auto", json!({}));
    assert!(o["output"].get("unplaced").is_none(), "{o}");
    let at = |r: &str| {
        let f = &o["output"]["placements"][r];
        (mm(&f["at"][0]), mm(&f["at"][1]))
    };
    let (ux, uy) = at("U1");
    assert!((ux - 30.0).abs() < 6.0 && (uy - 22.5).abs() < 6.0, "MCU central: ({ux}, {uy})");
    let heads: Vec<(f64, f64)> = ["J1", "J2", "J3"].iter().map(|j| at(j)).collect();
    for (j, &(x, y)) in heads.iter().enumerate() {
        let edge = x.min(60.0 - x).min(y).min(45.0 - y);
        assert!(edge < 6.0, "J{} at ({x}, {y}) is {edge} mm from the edge", j + 1);
    }
    for i in 0..3 {
        for j in i + 1..3 {
            let d = ((heads[i].0 - heads[j].0).powi(2) + (heads[i].1 - heads[j].1).powi(2)).sqrt();
            assert!(d > 12.0, "headers J{} and J{} {d:.1} mm apart: {heads:?}", i + 1, j + 1);
        }
    }
    let (yx, yy) = at("Y1");
    let y_to_u = ((yx - ux).powi(2) + (yy - uy).powi(2)).sqrt();
    assert!(y_to_u < 10.0, "crystal {y_to_u:.1} mm from the MCU");
    let p = s.project.as_ref().unwrap();
    let bad: Vec<String> = cadlab::drc::check(p)
        .into_iter()
        .filter(|d| {
            ["drc.courtyard_overlap", "drc.footprint_outside", "drc.copper_to_edge", "drc.silk_over_pad"]
                .contains(&d.code.as_ref())
        })
        .map(|d| d.message)
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    let spread = extent(&o);

    let (_d2, mut s2) = spread_board(&r);
    let o2 = exec(&r, &mut s2, "place.auto", json!({}));
    assert_eq!(o2["output"]["placements"], o["output"]["placements"], "deterministic");

    let (_d3, mut s3) = spread_board(&r);
    let o3 = exec(&r, &mut s3, "place.auto", json!({"compact": true}));
    let compact = extent(&o3);
    assert!(
        spread.0 * spread.1 > compact.0 * compact.1 * 1.3,
        "spread {spread:?} vs compact {compact:?} (origin extents, mm)"
    );
}

/// Before routing, `drc.run` reports unrouted connections as one summary with the count per
/// net; `unrouted: each` lists them, and few are listed one by one anyway.
#[test]
fn drc_summarizes_unrouted_connections() {
    let r = Registry::with_builtins();
    let (_d, mut s) = spread_board(&r);
    exec(&r, &mut s, "place.auto", json!({}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let unrouted: Vec<&Value> =
        o["diagnostics"].as_array().unwrap().iter().filter(|d| d["code"] == "drc.unrouted").collect();
    assert_eq!(unrouted.len(), 1, "one summary: {o}");
    let n = o["output"]["unrouted"].as_u64().unwrap();
    assert!(n > 10, "{o}");
    let msg = unrouted[0]["message"].as_str().unwrap();
    assert!(msg.starts_with(&format!("{n} connection(s) on ")) && msg.contains("GND "), "{msg}");
    assert!(unrouted[0]["hint"].as_str().unwrap().contains("unrouted: each"), "{}", unrouted[0]);
    assert!(unrouted[0]["subjects"].as_array().unwrap().iter().any(|x| x == "net:GND"), "{}", unrouted[0]);
    assert_eq!(o["output"]["errors"], 1);

    let each = exec(&r, &mut s, "drc.run", json!({"unrouted": "each"}));
    let k = each["diagnostics"].as_array().unwrap().iter().filter(|d| d["code"] == "drc.unrouted").count();
    assert_eq!(k as u64, n);
    assert_eq!(each["output"]["errors"].as_u64().unwrap(), n);

    // Few connections: listed one by one by default, summarized on request.
    let (_d2, mut s2) = new_project(&r);
    exec(&r, &mut s2, "circuit.add", json!({"part": "R 1k 1% 0402", "count": 2}));
    exec(&r, &mut s2, "net.connect", json!({"net": "A", "pins": ["R1.1", "R2.1"]}));
    exec(&r, &mut s2, "board.outline", json!({"width": "20mm", "height": "20mm"}));
    exec(&r, &mut s2, "place.auto", json!({}));
    let o = exec(&r, &mut s2, "drc.run", json!({}));
    let d = diag(&o, "drc.unrouted").unwrap();
    assert!(d["message"].as_str().unwrap().contains("R1.1"), "{d}");
    let o = exec(&r, &mut s2, "drc.run", json!({"unrouted": "summary"}));
    let d = diag(&o, "drc.unrouted").unwrap();
    assert_eq!(d["message"], "1 connection(s) on 1 net(s) are not routed: A 1", "{d}");
}

/// Text `describe` lists the values of enum fields inside objects (`pins[].kind`).
#[test]
fn describe_shows_nested_enum_values() {
    let dir = tempfile::tempdir().unwrap();
    let o = cadlab(dir.path(), &["describe", "part.create"]);
    let t = text(&o);
    assert!(t.contains("pins[].kind: input|output|bidirectional|"), "{t}");
    assert!(t.contains("pins[].side: left|right|top|bottom"), "{t}");
    let o = cadlab(dir.path(), &["describe", "footprint.create"]);
    let t = text(&o);
    assert!(t.contains("pads[].shape: rect|roundrect|circle|oval"), "{t}");
    assert!(t.contains("pads[].layers: top|bottom|all|npth"), "{t}");
}

/// A `.kicad_mod` whose file name differs from the footprint inside is named after the
/// footprint inside, with a note; either name selects it; `name` overrides both.
#[test]
fn kicad_footprint_import_uses_the_internal_name() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let file = fixture("USB_C_6P.kicad_mod");
    let o = exec(&r, &mut s, "footprint.import_kicad", json!({"path": file, "license": "own work"}));
    let fp = &o["output"]["footprints"][0];
    assert_eq!(fp["name"], "USB_C_Receptacle_6P", "{o}");
    assert_eq!(fp["kicad_name"], "USB_C_6P", "{o}");
    let note = diag(&o, "import.name_from_file").expect("note");
    assert_eq!(note["severity"], "info");
    assert!(note["hint"].as_str().unwrap().contains("`name`"), "{note}");
    assert!(s.project.as_ref().unwrap().library().footprints.contains_key("USB_C_Receptacle_6P"));

    // Selecting by either name in a directory.
    let dir = tempfile::tempdir().unwrap();
    let pretty = dir.path().join("Mine.pretty");
    std::fs::create_dir(&pretty).unwrap();
    std::fs::copy(&file, pretty.join("USB_C_6P.kicad_mod")).unwrap();
    for n in ["USB_C_6P", "usb_c_receptacle_6p"] {
        let (_d2, mut s2) = new_project(&r);
        let o = exec(&r, &mut s2, "footprint.import_kicad", json!({"path": pretty, "footprint": [n]}));
        assert_eq!(o["output"]["footprints"][0]["name"], "USB_C_Receptacle_6P", "{n}: {o}");
    }
    let (_d3, mut s3) = new_project(&r);
    let e = fail(&r, &mut s3, "footprint.import_kicad", json!({"path": pretty, "footprint": ["USB_C_Receptacle"]}));
    assert_eq!(e.error.diagnostic.code, "import.footprint_not_found");

    // `name` overrides; it needs a single footprint and a valid name.
    let o = exec(&r, &mut s3, "footprint.import_kicad", json!({"path": file, "name": "USBC"}));
    assert_eq!(o["output"]["footprints"][0]["name"], "USBC", "{o}");
    assert!(diag(&o, "import.name_from_file").is_none(), "{o}");
    std::fs::copy(fixture("USB_C_6P.kicad_mod"), pretty.join("OTHER.kicad_mod")).unwrap();
    let e = fail(&r, &mut s3, "footprint.import_kicad", json!({"path": pretty, "name": "X"}));
    assert_eq!(e.error.diagnostic.code, "import.name_needs_one");
    let e = fail(&r, &mut s3, "footprint.import_kicad", json!({"path": file, "name": "bad name"}));
    assert_eq!(e.error.diagnostic.code, "import.invalid_name");
}

/// `footprint.create`: a land pattern from a pad list, with rows (`count`, `pitch`), holes,
/// generated silkscreen, pin-1 dot and courtyard; errors and warnings name the pad.
#[test]
fn footprint_create_from_a_pad_list() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let o = exec(
        &r,
        &mut s,
        "footprint.create",
        json!({
            "name": "CONN_5P",
            "description": "5 pin connector with pegs",
            "pads": [
                {"number": "1", "at": ["-2mm", "0mm"], "size": ["0.6mm", "1.5mm"], "count": 5, "pitch": ["1mm", "0mm"]},
                {"number": "MP", "at": ["-4mm", "-2.5mm"], "size": ["1.2mm", "1.8mm"], "shape": "rect", "count": 2, "pitch": ["8mm", "0mm"]},
                {"at": ["-3mm", "-1.5mm"], "drill": "0.8mm", "layers": "npth"},
                {"number": "6", "at": ["0mm", "-3mm"], "size": ["1.5mm", "1.5mm"], "drill": "0.9mm"}
            ],
            "body": {"width": "9mm", "length": "5mm", "height": "2mm"}
        }),
    );
    assert_eq!(o["output"]["status"], "created", "{o}");
    let p = s.project.as_ref().unwrap();
    let fp = &p.library().footprints["CONN_5P"];
    let numbers: Vec<&str> = fp.pads.iter().map(|x| x.number.as_str()).collect();
    assert_eq!(numbers, ["1", "2", "3", "4", "5", "MP", "MP", "", "6"]);
    assert_eq!(fp.pads[4].at, cadlab::Point::new(cadlab::Nm::from_um(2000), cadlab::Nm::ZERO));
    assert_eq!(fp.mount, cadlab::model::footprint::Mount::Tht);
    assert!(matches!(fp.pads[0].shape, cadlab::model::footprint::PadShape::RoundRect { .. }));
    assert!(matches!(fp.pads[8].shape, cadlab::model::footprint::PadShape::Circle { .. }));
    assert!(matches!(fp.pads[7].kind, cadlab::model::footprint::PadKind::Npth { .. }));
    // Courtyard around everything, silk with a pin-1 dot.
    let cy = cadlab::geom::BBox::of_points(fp.courtyard.iter().copied()).unwrap();
    for pd in &fp.pads {
        let (w, h) = pd.shape.size();
        assert!(cy.min.x <= pd.at.x - w / 2 && cy.max.x >= pd.at.x + w / 2, "{}", pd.number);
        assert!(cy.min.y <= pd.at.y - h / 2 && cy.max.y >= pd.at.y + h / 2, "{}", pd.number);
    }
    use cadlab::model::footprint::{GraphicGeometry, GraphicLayer};
    assert!(
        fp.graphics
            .iter()
            .any(|g| g.layer == GraphicLayer::Silk && matches!(g.geometry, GraphicGeometry::Circle { .. }))
    );
    assert!(fp.graphics.iter().any(|g| g.layer == GraphicLayer::Fab));

    // Usable right away; the generated silkscreen passes the fab's silk rules.
    let pins = json!([{"number":"1","name":"A"},{"number":"2","name":"B"},{"number":"3","name":"C"},{"number":"4","name":"D"},{"number":"5","name":"E"},{"number":"6","name":"F"}]);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "CONN", "category": "connector", "footprint": "CONN_5P", "pins": pins}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "CONN", "refdes": "J1"}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "20mm"}));
    exec(&r, &mut s, "board.rules", json!({"fab": "jlcpcb"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["10mm", "10mm"]}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let silk: Vec<&Value> =
        o["diagnostics"].as_array().unwrap().iter().filter(|d| d["code"].as_str().unwrap().contains("silk")).collect();
    assert!(silk.is_empty(), "{silk:#?}");

    // Same again: unchanged; different without `replace`: conflict.
    let same = json!({"name": "ONE", "pads": [{"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"]}]});
    exec(&r, &mut s, "footprint.create", same.clone());
    assert_eq!(exec(&r, &mut s, "footprint.create", same)["output"]["status"], "unchanged");
    let other = json!({"name": "ONE", "pads": [{"number": "1", "at": ["0mm", "0mm"], "size": ["2mm", "1mm"]}]});
    assert_eq!(fail(&r, &mut s, "footprint.create", other).error.diagnostic.code, "footprint.exists");

    // Errors name the pad; overlapping pads warn.
    for (pads, code, what) in [
        (json!([{"number": "1", "at": ["0mm", "0mm"]}]), "footprint.invalid_pad", "`size` is missing"),
        (
            json!([{"number": "2", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "drill": "1mm"}]),
            "footprint.invalid_pad",
            "annular ring",
        ),
        (
            json!([{"number": "3", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "count": 3}]),
            "footprint.invalid_pad",
            "`pitch`",
        ),
        (
            json!([{"number": "4", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"], "layers": "all"}]),
            "footprint.invalid_pad",
            "`drill`",
        ),
        (json!([]), "footprint.no_pads", ""),
    ] {
        let e = fail(&r, &mut s, "footprint.create", json!({"name": "BAD", "pads": pads}));
        assert_eq!(e.error.diagnostic.code, code, "{}", e.error);
        assert!(e.error.diagnostic.message.contains(what), "{}", e.error);
    }
    let o = exec(
        &r,
        &mut s,
        "footprint.create",
        json!({"name": "OVL", "pads": [{"number": "1", "at": ["0mm", "0mm"], "size": ["1mm", "1mm"]}, {"number": "2", "at": ["0.5mm", "0mm"], "size": ["1mm", "1mm"]}]}),
    );
    assert!(diag(&o, "footprint.pads_overlap").is_some(), "{o}");
}

// ---- Session 3 (docs/TESTING.md): session 2's open items and session 3's friction ----

/// `footprint.create` keeps datasheet pad coordinates with an offset body (`body.at`): fab
/// outline and courtyard follow the body; `origin: body` / `pads` move the origin instead.
#[test]
fn footprint_create_body_offset_and_origin() {
    use cadlab::model::footprint::{GraphicGeometry, GraphicLayer};
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let pads =
        json!([{"number": "1", "at": ["-1mm", "0mm"], "size": ["0.6mm", "1mm"], "count": 3, "pitch": ["1mm", "0mm"]}]);
    let body = json!({"width": "6mm", "length": "4mm", "height": "2mm", "at": ["0mm", "-2.5mm"]});
    exec(&r, &mut s, "footprint.create", json!({"name": "OFF", "pads": pads, "body": body}));
    let fab_box = |s: &Session, name: &str| {
        let fp = &s.project.as_ref().unwrap().library().footprints[name];
        let g = fp.graphics.iter().find(|g| g.layer == GraphicLayer::Fab).unwrap();
        let GraphicGeometry::Polygon { points } = &g.geometry else { panic!("fab outline") };
        (cadlab::geom::BBox::of_points(points.iter().copied()).unwrap(), fp.clone())
    };
    let um = cadlab::Nm::from_um;
    let (fab, fp) = fab_box(&s, "OFF");
    assert_eq!(fp.pads[0].at, cadlab::Point::new(um(-1000), cadlab::Nm::ZERO), "pads as given");
    assert_eq!((fab.min.y, fab.max.y), (um(-4500), um(-500)), "{fab:?}");
    assert_eq!(fp.body.unwrap().at.y, um(-2500));
    let cy = cadlab::geom::BBox::of_points(fp.courtyard.iter().copied()).unwrap();
    assert!(cy.min.y <= um(-4500) && cy.max.y >= um(500), "{cy:?}");

    // Origin at the body center: everything shifts by +2.5 mm in Y.
    exec(&r, &mut s, "footprint.create", json!({"name": "BODY", "pads": pads, "body": body, "origin": "body"}));
    let (fab, fp) = fab_box(&s, "BODY");
    assert_eq!(fp.pads[0].at.y, um(2500));
    assert!(fp.body.unwrap().at.is_origin());
    assert_eq!((fab.min.y, fab.max.y), (um(-2000), um(2000)));
    // Origin at the pads' center.
    exec(&r, &mut s, "footprint.create", json!({"name": "PADS", "pads": pads, "origin": "pads", "body": body}));
    let fp = &s.project.as_ref().unwrap().library().footprints["PADS"];
    assert_eq!(fp.pads[1].at, cadlab::Point::ORIGIN);
    let e = fail(&r, &mut s, "footprint.create", json!({"name": "X", "pads": pads, "origin": "body"}));
    assert_eq!(e.error.diagnostic.code, "footprint.origin_without_body");

    // `footprint show` prints the body and where it is.
    let dir = tempfile::tempdir().unwrap();
    let o = cadlab(dir.path(), &["project", "new", "p"]);
    assert!(o.status.success(), "{}", text(&o));
    let p = dir.path().join("p");
    let (pads_s, body_s) = (pads.to_string(), body.to_string());
    let o = cadlab(&p, &["footprint", "create", "OFF", "--pads", &pads_s, "--body", &body_s]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&cadlab(&p, &["footprint", "show", "OFF"]));
    assert!(t.contains("body 6mm x 4mm x 2mm at (0mm, -2.5mm)"), "{t}");
}

/// Placement hints: persisted per designator, an edge for a connector, an LED pulled to an
/// edge, renamed and removed with their component; `density` validated and used.
#[test]
fn placement_hints_and_density() {
    let r = Registry::with_builtins();
    let (_d, mut s) = spread_board(&r);
    exec(&r, &mut s, "circuit.add", json!({"part": "LED red 0603", "refdes": "D1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0402", "refdes": "R1"}));
    exec(&r, &mut s, "net.connect", json!({"net": "LED", "pins": ["U1.PD3", "R1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "LED_A", "pins": ["R1.2", "D1.A"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["D1.K"]}));
    let o = exec(&r, &mut s, "place.hint", json!({"refdes": ["J1"], "edge": "left"}));
    assert_eq!(o["output"]["hints"]["J1"]["edge"], "left");
    exec(&r, &mut s, "place.hint", json!({"refdes": ["J2,D1"], "edge": "top"}));
    let listed = exec(&r, &mut s, "place.hint", json!({}));
    assert_eq!(listed["output"]["hints"].as_object().unwrap().len(), 3, "{listed}");
    let o = exec(&r, &mut s, "place.auto", json!({}));
    let at = |o: &Value, r: &str| {
        let f = &o["output"]["placements"][r];
        (mm(&f["at"][0]), mm(&f["at"][1]))
    };
    assert!(at(&o, "J1").0 < 6.0, "J1 at the left edge: {:?}", at(&o, "J1"));
    assert!(at(&o, "J2").1 > 39.0, "J2 at the top edge: {:?}", at(&o, "J2"));
    assert!(at(&o, "D1").1 > 41.0, "D1 at the top edge: {:?}", at(&o, "D1"));
    // Again: the same placement (hints are data, the placement deterministic).
    let o2 = exec(&r, &mut s, "place.auto", json!({"replace": true}));
    assert_eq!(o2["output"]["placements"], o["output"]["placements"]);

    // Hints follow renames and removals; `clear` drops them.
    exec(&r, &mut s, "circuit.rename", json!({"from": "D1", "to": "D9"}));
    let listed = exec(&r, &mut s, "place.hint", json!({}));
    assert!(listed["output"]["hints"].get("D9").is_some(), "{listed}");
    exec(&r, &mut s, "circuit.remove", json!({"refdes": ["D9"]}));
    exec(&r, &mut s, "place.hint", json!({"refdes": ["J2"], "clear": true}));
    let listed = exec(&r, &mut s, "place.hint", json!({}));
    assert_eq!(listed["output"]["hints"], json!({"J1": {"edge": "left"}}));
    let e = fail(&r, &mut s, "place.hint", json!({"refdes": ["J1"], "edge": "top", "clear": true}));
    assert_eq!(e.error.diagnostic.code, "place.hint_conflict");
    let e = fail(&r, &mut s, "place.hint", json!({"refdes": ["J7"], "edge": "top"}));
    assert_eq!(e.error.diagnostic.code, "component.not_found", "{}", e.error);

    // Density: a percentage, not with `compact`; a lower one keeps groups closer.
    let e = fail(&r, &mut s, "place.auto", json!({"replace": true, "density": 5}));
    assert_eq!(e.error.diagnostic.code, "place.invalid_density");
    let e = fail(&r, &mut s, "place.auto", json!({"replace": true, "density": 50, "compact": true}));
    assert_eq!(e.error.diagnostic.code, "place.density_compact");
    let (_d2, mut s2) = spread_board(&r);
    let tight = exec(&r, &mut s2, "place.auto", json!({"density": 20}));
    let (_d3, mut s3) = spread_board(&r);
    let wide = exec(&r, &mut s3, "place.auto", json!({"density": 90}));
    let (t, w) = (extent(&tight), extent(&wide));
    assert!(t.0 * t.1 < w.0 * w.1, "density 20 {t:?} vs 90 {w:?}");
}

/// Unmapped shell pads get one warning with the pin to add; `part.create --replace` adds it,
/// and refuses to drop a pin that is connected.
#[test]
fn part_create_suggests_a_shield_pin_and_replaces() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let pads = json!([
        {"number": "1", "at": ["-1mm", "0mm"], "size": ["0.6mm", "1mm"], "count": 3, "pitch": ["1mm", "0mm"]},
        {"number": "S1", "at": ["-3mm", "-1mm"], "size": ["1mm", "2mm"], "drill": "0.6mm"},
        {"number": "S1", "at": ["3mm", "-1mm"], "size": ["1mm", "2mm"], "drill": "0.6mm"},
        {"number": "MH2", "at": ["0mm", "-3mm"], "size": ["1mm", "1mm"]}
    ]);
    exec(&r, &mut s, "footprint.create", json!({"name": "SHELL", "pads": pads}));
    let pins = json!([{"number": "1", "name": "A"}, {"number": "2", "name": "B"}, {"number": "3", "name": "C"}]);
    let o = exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "CONN", "category": "connector", "footprint": "SHELL", "pins": pins}),
    );
    let d = diag(&o, "part.unmapped_shield").expect("shield warning");
    assert!(d["message"].as_str().unwrap().contains("S1, MH2"), "{d}");
    let hint = d["hint"].as_str().unwrap();
    assert!(hint.contains(r#"`pin_map` {"SH": ["S1", "MH2"]}"#), "{hint}");
    assert!(hint.contains("--replace") && hint.contains("SHIELD"), "{hint}");
    assert!(diag(&o, "part.unconnected_pad").is_none(), "one warning: {o}");

    // The fix as hinted.
    let mut with = pins.as_array().unwrap().clone();
    with.push(json!({"number": "SH", "name": "SHIELD", "kind": "passive"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "CONN", "refdes": "J1"}));
    exec(&r, &mut s, "net.connect", json!({"net": "N3", "pins": ["J1.3"]}));
    let o = exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "CONN", "category": "connector", "footprint": "SHELL", "pins": with,
               "pin_map": {"SH": ["S1", "MH2"]}, "replace": true}),
    );
    assert!(o["diagnostics"].as_array().unwrap().is_empty(), "{o}");
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["J1.SHIELD"]}));
    // Dropping connected pin 3 is refused.
    let e = fail(
        &r,
        &mut s,
        "part.create",
        json!({"id": "CONN", "category": "connector", "footprint": "SHELL", "pins": [{"number": "1"}, {"number": "2"}],
               "replace": true}),
    );
    assert_eq!(e.error.diagnostic.code, "part.replace_drops_pins");
    assert!(e.error.diagnostic.message.contains("J1.3"), "{}", e.error);
    // Without `replace`, an existing ID is still a conflict.
    let e = fail(&r, &mut s, "part.create", json!({"id": "CONN", "category": "connector", "footprint": "SHELL"}));
    assert_eq!(e.error.diagnostic.code, "part.exists");
}

/// A buck converter: the output rail behind the inductor counts as driven (ERC), and its
/// output capacitor is looked for there, not on the switch node (lint).
#[test]
fn buck_output_behind_its_inductor() {
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let pins = json!([{"number":"1","name":"EN","kind":"input"},{"number":"2","name":"GND","kind":"power_in"},{"number":"3","name":"LX","kind":"power_out"},{"number":"4","name":"IN","kind":"power_in"},{"number":"5","name":"FB","kind":"input"}]);
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"id": "BUCK", "category": "regulator", "package": "SOT-23-5", "pins": pins}),
    );
    let load = json!([{"number":"1","name":"VDD","kind":"power_in"},{"number":"2","name":"GND","kind":"power_in"},{"number":"3","name":"IO"}]);
    exec(&r, &mut s, "part.create", json!({"id": "LOAD", "category": "ic", "package": "SOT-23", "pins": load}));
    exec(&r, &mut s, "circuit.add", json!({"part": "BUCK", "refdes": "U1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "LOAD", "refdes": "U2"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "L 2.2uH 2A 1210", "refdes": "L1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 10uF 10V X5R 0805", "count": 2}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "refdes": "C3"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402", "count": 2}));
    for (net, pins) in [
        ("VIN", json!(["U1.IN", "U1.EN", "C1.1"])),
        ("GND", json!(["U1.GND", "U2.GND", "C1.2", "R2.2", "C3.2"])),
        ("SW", json!(["U1.LX", "L1.1"])),
        ("+3V3", json!(["L1.2", "U2.VDD", "R1.1"])),
        ("FB", json!(["U1.FB", "R1.2", "R2.1"])),
    ] {
        exec(&r, &mut s, "net.connect", json!({"net": net, "pins": pins}));
    }
    exec(&r, &mut s, "net.connect", json!({"net": "IO", "pins": ["U2.IO", "C3.1"]}));
    exec(&r, &mut s, "net.set", json!({"nets": ["VIN"], "driven": true}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert!(diag(&o, "erc.power_not_driven").is_none(), "{}", o["diagnostics"]);
    // No output capacitor yet: lint names +3V3, not the switch node.
    let o = exec(&r, &mut s, "circuit.lint", json!({}));
    let found: Vec<&str> = o["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "lint.missing_decoupling")
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert!(found.iter().any(|m| m.contains("`+3V3`")), "{found:?}");
    assert!(!found.iter().any(|m| m.contains("`SW`")), "{found:?}");
    exec(&r, &mut s, "net.connect", json!({"net": "+3V3", "pins": ["C2.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["C2.2"]}));
    let o = exec(&r, &mut s, "circuit.lint", json!({}));
    assert!(diag(&o, "lint.missing_decoupling").is_none(), "{}", o["diagnostics"]);
    // Without the inductor's link, the rail is not driven.
    exec(&r, &mut s, "net.disconnect", json!({"pins": ["L1.2"]}));
    let o = exec(&r, &mut s, "circuit.erc", json!({}));
    assert!(diag(&o, "erc.power_not_driven").is_some(), "{}", o["diagnostics"]);
}

/// `board.fab_stackup` lists a fab's published stackups, applies one in one step; the fab check
/// accepts its copper, and the fab export names it in `fab-lock.json`.
#[test]
fn fab_stackup_applies_and_is_locked() {
    let r = Registry::with_builtins();
    let (d, mut s) = new_project(&r);
    // Without a name: the fab's stackups, listed (nothing changes).
    let before = s.project.as_ref().unwrap().board().stackup.clone();
    let o = exec(&r, &mut s, "board.fab_stackup", json!({"fab": "jlcpcb"}));
    let ids: Vec<&str> =
        o["output"]["stackups"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"JLC04161H-7628") && ids.len() >= 3, "{o}");
    let first = &o["output"]["stackups"][0];
    assert_eq!(first["layers"], 4, "{first}");
    assert_eq!(first["dielectrics"].as_array().unwrap().len(), 3, "{first}");
    assert!(first["apply"].as_str().unwrap().starts_with("board.fab_stackup jlcpcb JLC"), "{first}");
    assert!(o["output"].get("copper").is_none(), "{o}");
    assert_eq!(s.project.as_ref().unwrap().board().stackup, before);
    let o = cadlab(&d.path().join("p"), &["board", "fab-stackup", "jlcpcb"]);
    assert!(o.status.success() && text(&o).contains("JLC04161H-7628: 4 layers"), "{}", text(&o));
    let e = fail(&r, &mut s, "board.fab_stackup", json!({"fab": "jlcpcb", "stackup": "JLC04161H-762"}));
    assert_eq!(e.error.diagnostic.code, "board.unknown_stackup");
    let e = fail(&r, &mut s, "board.fab_stackup", json!({"fab": "oshpark", "stackup": "x"}));
    assert_eq!(e.error.diagnostic.code, "fab.no_stackups");
    let o = exec(&r, &mut s, "board.fab_stackup", json!({"fab": "jlcpcb", "stackup": "jlc04161h-3313"}));
    assert!(o["diagnostics"].as_array().unwrap().is_empty(), "{o}");
    let st = &s.project.as_ref().unwrap().board().stackup;
    assert_eq!(st.copper_layers, 4);
    assert_eq!(st.inner_copper, cadlab::Nm(15_200));
    assert_eq!(st.dielectrics[1].thickness, cadlab::Nm::from_um(1265));
    assert_eq!(st.dielectrics[0].er.to_string(), "4.1");

    exec(&r, &mut s, "circuit.add", json!({"part": "R 1k 1% 0402", "count": 2}));
    exec(&r, &mut s, "net.connect", json!({"net": "A", "pins": ["R1.1", "R2.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "B", "pins": ["R1.2", "R2.2"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "20mm"}));
    exec(&r, &mut s, "board.rules", json!({"fab": "jlcpcb"}));
    exec(&r, &mut s, "place.auto", json!({}));
    exec(&r, &mut s, "route.all", json!({}));
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "jlcpcb"}));
    assert!(diag(&o, "fab.copper").is_none(), "{}", o["diagnostics"]);
    let o = exec(&r, &mut s, "fab.export", json!({"fab": "jlcpcb", "dir": d.path().join("fab")}));
    assert_eq!(o["output"]["stackup"], "JLC04161H-3313", "{o}");
    let lock: Value =
        serde_json::from_str(&std::fs::read_to_string(d.path().join("fab/fab-lock.json")).unwrap()).unwrap();
    assert_eq!(lock["process"]["stackup"], "JLC04161H-3313");
    // A dielectric changed by hand no longer matches: nothing is claimed.
    exec(&r, &mut s, "board.dielectric", json!({"gap": 1, "thickness": "0.2mm"}));
    let o = exec(&r, &mut s, "fab.export", json!({"fab": "jlcpcb", "dir": d.path().join("fab")}));
    assert!(o["output"].get("stackup").is_none(), "{o}");
}

/// JSON-valued flags take a bare word as a string (`--outline board`).
#[test]
fn bare_words_for_json_flags() {
    let dir = tempfile::tempdir().unwrap();
    assert!(cadlab(dir.path(), &["project", "new", "p"]).status.success());
    let p = dir.path().join("p");
    for a in [
        &["board", "outline", "--width", "20mm", "--height", "20mm"][..],
        &["board", "setup", "--layers", "4"],
        &["circuit", "add", "R 1k 1% 0402"],
        &["net", "connect", "GND", "R1.1"],
    ] {
        assert!(cadlab(&p, a).status.success());
    }
    let o = cadlab(&p, &["zone", "add", "GND", "--net", "GND", "--layers", "In1.Cu", "--outline", "board"]);
    assert!(o.status.success(), "{}", text(&o));
    // Malformed JSON still says so.
    let o = cadlab(&p, &["zone", "add", "G2", "--net", "GND", "--layers", "In2.Cu", "--outline", "{\"rect\": "]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("expected JSON"), "{}", text(&o));
}

/// The generated pin-1 dot avoids every pad (a USB-C shell pad sat where it used to go).
#[test]
fn pin1_dot_avoids_other_pads() {
    use cadlab::model::footprint::{GraphicGeometry, GraphicLayer};
    let r = Registry::with_builtins();
    let (_d, mut s) = new_project(&r);
    let pads = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session3/usb_c_16p_pads.json"),
    )
    .unwrap();
    let pads: Value = serde_json::from_str(&pads).unwrap();
    let body = json!({"width": "8.94mm", "length": "7.3mm", "height": "3.2mm", "at": ["0mm", "-3.4mm"]});
    exec(&r, &mut s, "footprint.create", json!({"name": "USBC", "pads": pads, "body": body}));
    let fp = &s.project.as_ref().unwrap().library().footprints["USBC"];
    let (c, rad) = fp
        .graphics
        .iter()
        .find_map(|g| match g.geometry {
            GraphicGeometry::Circle { center, radius, filled: true } if g.layer == GraphicLayer::Silk => {
                Some((center, radius))
            }
            _ => None,
        })
        .expect("pin-1 dot");
    for p in &fp.pads {
        let (w, h) = p.shape.size();
        let dx = ((c.x - p.at.x).0.abs() - w.0 / 2 - rad.0).max(0);
        let dy = ((c.y - p.at.y).0.abs() - h.0 / 2 - rad.0).max(0);
        assert!(dx.max(dy) >= 150_000, "dot at {c:?} within 0.15 mm of pad {} at {:?}", p.number, p.at);
    }
}
