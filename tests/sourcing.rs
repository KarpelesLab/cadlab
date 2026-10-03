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
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| c["mpn"].as_str().unwrap().to_string())
        .collect()
}

/// The M1 exit scenario: "a 3.3 V LDO, 500 mA, SOT-23-5, in stock" to a concrete part in the BOM.
#[test]
fn ldo_from_requirement_to_bom() {
    let (_d, r, mut s) = setup();
    let o = exec(
        &r,
        &mut s,
        "part.search",
        json!({"query": "LDO", "package": "SOT-23-5", "params": {"voltage_out": "3.3V", "current_out": ">=500mA"}, "in_stock": true}),
    );
    // XC6206 (200 mA, SOT-23), the obsolete part and the out-of-stock one are excluded; cheapest first.
    assert_eq!(mpns(&o["output"]["candidates"]), ["ME6211C33M5G-N", "AP2112K-3.3TRG1"]);

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
}

#[test]
fn search_filters_and_errors() {
    let (_d, r, mut s) = setup();
    let o = exec(
        &r,
        &mut s,
        "part.search",
        json!({"query": "", "category": "resistor", "params": {"tolerance": "<=1%"}}),
    );
    assert_eq!(mpns(&o["output"]["candidates"]), ["0402WGF1002TCE", "RC0402FR-0710KL"]);
    let o = exec(
        &r,
        &mut s,
        "part.search",
        json!({"query": "LDO", "max_price": "0.04 USD", "include_obsolete": true}),
    );
    // In stock first (even obsolete), then out of stock.
    assert_eq!(
        mpns(&o["output"]["candidates"]),
        ["XC6206P332MR", "OLD3300", "AP2112K-3.3TRG1-OUT"]
    );
    let f = r
        .execute(
            &mut s,
            "part.search",
            json!({"params": {"resistance": ">=1uF"}}),
            RunOptions::default(),
        )
        .unwrap_err();
    assert_eq!(f.error.diagnostic.code, "part.invalid_filter");

    let mut empty = Session::new();
    let f = r
        .execute(&mut empty, "part.search", json!({"query": "x"}), RunOptions::default())
        .unwrap_err();
    assert_eq!(f.error.diagnostic.code, "supplier.none");
}

#[test]
fn resolve_check_cost() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402", "count": 4}));
    exec(
        &r,
        &mut s,
        "circuit.add",
        json!({"part": "C 100nF 16V X7R 0402", "count": 10}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "LED red 0603"}));

    // Proposals only.
    let undo_before = s.history.undo_len();
    let o = exec(&r, &mut s, "bom.resolve", json!({}));
    let lines = o["output"]["lines"].as_array().unwrap();
    let by = |id: &str| lines.iter().find(|l| l["part"] == id).unwrap().clone();
    // 1% or better only; 5% part excluded. Capacitors: >= 16 V, enough stock for 10 (the 40-stock Murata qualifies,
    // the 10 V one does not).
    assert_eq!(
        mpns(&by("R_10k_1pct_0402")["candidates"]),
        ["0402WGF1002TCE", "RC0402FR-0710KL"]
    );
    assert_eq!(
        mpns(&by("C_100nF_16V_X7R_0402")["candidates"]),
        ["GRM155R71C104KA88D", "CL05B104KO5NNNC"]
    );
    assert_eq!(o["output"]["unresolved"], json!(["LED_red_0603"]));
    assert!(
        o["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bom.unresolved")
    );
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
        .map(|l| {
            (
                l["part"].as_str().unwrap().to_string(),
                l["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        statuses.contains(&("LED_red_0603".into(), "no_mpn".into())),
        "{statuses:?}"
    );
    assert!(
        statuses.contains(&("C_100nF_16V_X7R_0402".into(), "ok".into())),
        "{statuses:?}"
    );
    assert!(
        o["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bom.no_mpn" && d["severity"] == "error")
    );

    // Cost for 1 board: 4 × 0.001 + 10 × 0.003 = 0.034 USD (LED unpriced).
    let o = exec(&r, &mut s, "bom.cost", json!({"boards": 1}));
    assert_eq!(o["output"]["totals"], json!(["0.034 USD"]));
    assert!(
        o["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bom.incomplete_cost")
    );
    // 1000 boards: the line approved 0402WGF1002TCE only (cheapest at small quantity), so its single price
    // applies even though RC0402FR has a 1000+ break: 4000 × 0.001 = 4.00 USD.
    let o = exec(&r, &mut s, "bom.cost", json!({"boards": 1000}));
    let res = o["output"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["part"] == "R_10k_1pct_0402")
        .unwrap()
        .clone();
    assert_eq!(res["extended"], "4.00 USD");
}
