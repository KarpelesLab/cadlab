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
