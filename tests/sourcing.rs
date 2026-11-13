//! Part research against a test catalog (tests/fixtures/catalog.json; stock and prices are made up).

use std::path::Path;
use std::sync::Arc;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::supplier::Suppliers;
use cadlab::supplier::catalog::Catalog;
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn setup() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let catalog = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog.json");
    s.suppliers = Suppliers::new().with(Arc::new(Catalog::lazy(catalog)));
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    (dir, r, s)
}

fn mpns(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|c| c["mpn"].as_str().unwrap().to_string()).collect()
}

/// The M1 exit (docs/ROADMAP.md): from "a 3.3 V LDO, 500 mA, SOT-23-5, in stock" to a concrete
/// part in the BOM with symbol and footprint attached, through the command registry with the
/// offline catalog as the only provider (no network). The agent's part is the choice among the
/// candidates and the pin list from the datasheet; everything else comes from the commands.
#[test]
fn ldo_from_requirement_to_bom() {
    let (dir, r, mut s) = setup();
    let o = exec(
        &r,
        &mut s,
        "part.search",
        json!({"query": "LDO", "package": "SOT-23-5", "params": {"voltage_out": "3.3V", "current_out": ">=500mA"}, "in_stock": true}),
    );
    // XC6206 (200 mA, SOT-23), the obsolete part and the out-of-stock one are excluded; cheapest first.
    assert_eq!(mpns(&o["output"]["candidates"]), ["ME6211C33M5G-N", "AP2112K-3.3TRG1"]);
    // Every candidate meets the requirement as stated, from the catalog file.
    for c in o["output"]["candidates"].as_array().unwrap() {
        assert_eq!(
            (&c["provider"], &c["package"], &c["params"]["voltage_out"]),
            (&json!("testcat"), &json!("SOT-23-5"), &json!("3.3V"))
        );
        assert!(c["stock"].as_u64().unwrap() > 0 && c["lifecycle"] == "active", "{c}");
        let ma: f64 = c["params"]["current_out"].as_str().unwrap().trim_end_matches("mA").parse().unwrap();
        assert!(ma >= 500.0, "{c}");
    }

    let pins = json!([
        {"number": "1", "name": "VIN", "kind": "power_in"},
        {"number": "2", "name": "GND", "kind": "power_in"},
        {"number": "3", "name": "EN", "kind": "input"},
        {"number": "4", "name": "NC", "kind": "no_connect"},
        {"number": "5", "name": "VOUT", "kind": "power_out"}
    ]);
    let o = exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "pins": pins, "fill_from_suppliers": true}),
    );
    assert_eq!(o["output"]["manufacturer"], "Diodes");
    assert_eq!(o["output"]["footprint"], "SOT95P280X145-5N");
    let shown = exec(&r, &mut s, "part.show", json!({"id": "AP2112K-3.3TRG1"}));
    assert_eq!(shown["output"]["part"]["params"]["current_out"], "600mA");
    assert_eq!(shown["output"]["part"]["datasheet"], "https://example.com/ap2112.pdf");

    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    let o = exec(&r, &mut s, "bom.list", json!({}));
    assert_eq!(o["output"]["lines"][0]["refdes"], json!(["U1"]));
    assert_eq!(o["output"]["lines"][0]["footprint"], "SOT95P280X145-5N");

    // The BOM has one line and it is concrete: a manufacturer and an MPN, nothing left to source.
    let lines = o["output"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!((&lines[0]["manufacturer"], &lines[0]["mpn"]), (&json!("Diodes"), &json!("AP2112K-3.3TRG1")));
    assert_eq!((&lines[0]["package"], &lines[0]["quantity"]), (&json!("SOT-23-5"), &json!(1)));
    assert_eq!(o["output"]["unsourced"], json!([]));
    // ... and the catalog has it in stock for the build.
    let check = exec(&r, &mut s, "bom.check", json!({}));
    assert_eq!(check["output"]["lines"][0]["status"], "ok", "{check}");
    assert!(check["diagnostics"].as_array().unwrap().iter().all(|d| d["severity"] != "error"), "{check}");

    // Symbol, footprint and pin↔pad map, in the project saved to disk and opened again.
    s.save().unwrap();
    let (s2, _) = Session::open(&dir.path().join("p")).unwrap();
    let p = s2.project.as_ref().unwrap();
    let comp = &p.circuit().components["U1"];
    let part = &p.library().parts[&comp.part];
    assert!(part.is_concrete());
    let names: Vec<(&str, &str)> =
        part.symbol.pins.iter().map(|pin| (pin.number.as_str(), pin.name.as_str())).collect();
    assert_eq!(names, [("1", "VIN"), ("2", "GND"), ("3", "EN"), ("4", "NC"), ("5", "VOUT")]);
    assert!(part.symbol.pins.iter().all(|pin| pin.at.is_some() && pin.side.is_some()), "symbol pins are drawn");
    assert_eq!(part.footprints.len(), 1);
    let fp = &p.library().footprints[&part.footprints[0].footprint];
    assert_eq!(fp.name, "SOT95P280X145-5N");
    let pads: Vec<&str> = fp.pads.iter().map(|pad| pad.number.as_str()).collect();
    assert_eq!(pads, ["1", "2", "3", "4", "5"]);
    // Each pin lands on exactly one pad of the footprint, each pad takes exactly one pin.
    let mut used = std::collections::BTreeSet::new();
    for pin in &part.symbol.pins {
        let on = part.footprints[0].pads_for(&pin.number);
        assert_eq!(on.len(), 1, "pin {} on pads {on:?}", pin.number);
        assert!(pads.contains(&on[0].as_str()), "pin {} on a pad the footprint lacks: {on:?}", pin.number);
        assert!(used.insert(on[0].clone()), "pad {} takes two pins", on[0]);
    }
    assert_eq!(used.len(), pads.len());
    // Pin 1 (VIN) is where SOT-23-5 has it: the first pad of the three-lead side.
    let at = |n: &str| fp.pads.iter().find(|pad| pad.number == n).unwrap().at;
    assert!(at("1").x == at("2").x && at("2").x == at("3").x && at("4").x == at("5").x && at("1").x < at("5").x);
    // Both render as drawings.
    exec(&r, &mut s, "render.symbol", json!({"part": "AP2112K-3.3TRG1", "path": "sym.svg"}));
    exec(&r, &mut s, "render.footprint", json!({"name": "AP2112K-3.3TRG1", "path": "fp.svg"}));
    assert!(dir.path().join("p/sym.svg").is_file() && dir.path().join("p/fp.svg").is_file());
}

#[test]
fn search_filters_and_errors() {
    let (_d, r, mut s) = setup();
    let o =
        exec(&r, &mut s, "part.search", json!({"query": "", "category": "resistor", "params": {"tolerance": "<=1%"}}));
    assert_eq!(mpns(&o["output"]["candidates"]), ["0402WGF1002TCE", "RC0402FR-0710KL"]);
    let o = exec(&r, &mut s, "part.search", json!({"query": "LDO", "max_price": "0.04 USD", "include_obsolete": true}));
    // In stock first (even obsolete), then out of stock.
    assert_eq!(mpns(&o["output"]["candidates"]), ["XC6206P332MR", "OLD3300", "AP2112K-3.3TRG1-OUT"]);
    let f = r
        .execute(&mut s, "part.search", json!({"params": {"resistance": ">=1uF"}}), RunOptions::default())
        .unwrap_err();
    assert_eq!(f.error.diagnostic.code, "part.invalid_filter");

    let mut empty = Session::new();
    let f = r.execute(&mut empty, "part.search", json!({"query": "x"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "supplier.none");
}

#[test]
fn resolve_check_cost() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402", "count": 4}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "count": 10}));
    exec(&r, &mut s, "circuit.add", json!({"part": "LED red 0603"}));

    // Proposals only.
    let undo_before = s.history.undo_len();
    let o = exec(&r, &mut s, "bom.resolve", json!({}));
    let lines = o["output"]["lines"].as_array().unwrap();
    let by = |id: &str| lines.iter().find(|l| l["part"] == id).unwrap().clone();
    // 1% or better only; 5% part excluded. Capacitors: >= 16 V, enough stock for 10 (the 40-stock Murata qualifies,
    // the 10 V one does not).
    assert_eq!(mpns(&by("R_10k_1pct_0402")["candidates"]), ["0402WGF1002TCE", "RC0402FR-0710KL"]);
    assert_eq!(mpns(&by("C_100nF_16V_X7R_0402")["candidates"]), ["GRM155R71C104KA88D", "CL05B104KO5NNNC"]);
    assert_eq!(o["output"]["unresolved"], json!(["LED_red_0603"]));
    assert!(o["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "bom.unresolved"));
    assert_eq!(s.history.undo_len(), undo_before, "proposals do not change the project");

    // For 10 boards the 40-unit capacitor stock is not enough: the other one wins.
    let o = exec(&r, &mut s, "bom.resolve", json!({"boards": 10, "apply": true}));
    let lines = o["output"]["lines"].as_array().unwrap();
    let c = lines.iter().find(|l| l["part"] == "C_100nF_16V_X7R_0402").unwrap();
    assert_eq!(c["candidates"][0]["mpn"], "CL05B104KO5NNNC");
    assert_eq!(c["applied"], true);

    // Check for 10 boards: LED has no MPN -> error diagnostic.
    let o = exec(&r, &mut s, "bom.check", json!({"boards": 10}));
    let statuses: Vec<(String, String)> = o["output"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["part"].as_str().unwrap().to_string(), l["status"].as_str().unwrap().to_string()))
        .collect();
    assert!(statuses.contains(&("LED_red_0603".into(), "no_mpn".into())), "{statuses:?}");
    assert!(statuses.contains(&("C_100nF_16V_X7R_0402".into(), "ok".into())), "{statuses:?}");
    assert!(o["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "bom.no_mpn" && d["severity"] == "error"));

    // Cost for 1 board: 4 × 0.001 + 10 × 0.003 = 0.034 USD (LED unpriced).
    let o = exec(&r, &mut s, "bom.cost", json!({"boards": 1}));
    assert_eq!(o["output"]["totals"], json!(["0.034 USD"]));
    assert!(o["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "bom.incomplete_cost"));
    // 1000 boards: the line approved 0402WGF1002TCE only (cheapest at small quantity), so its single price
    // applies even though RC0402FR has a 1000+ break: 4000 × 0.001 = 4.00 USD.
    let o = exec(&r, &mut s, "bom.cost", json!({"boards": 1000}));
    let res = o["output"]["lines"].as_array().unwrap().iter().find(|l| l["part"] == "R_10k_1pct_0402").unwrap().clone();
    assert_eq!(res["extended"], "4.00 USD");
}
