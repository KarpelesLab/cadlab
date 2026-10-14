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
