//! M1 flow through the command registry: generic and concrete parts, components, BOM, export.

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

fn ldo_args() -> Value {
    json!({
        "category": "ldo",
        "manufacturer": "Diodes",
        "mpn": "AP2112K-3.3TRG1",
        "package": "SOT-23-5",
        "params": {"voltage_out": "3.3V", "current_out": "600mA"},
        "pins": [
            {"number": "1", "name": "VIN", "kind": "power_in"},
            {"number": "2", "name": "GND", "kind": "power_in"},
            {"number": "3", "name": "EN", "kind": "input"},
            {"number": "4", "name": "NC", "kind": "no_connect"},
            {"number": "5", "name": "VOUT", "kind": "power_out"}
        ]
    })
}

fn setup() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    (dir, r, s)
}

#[test]
fn generic_parts_and_components() {
    let (_d, r, mut s) = setup();
    let o = exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402", "count": 3}));
    assert_eq!(o["output"]["refdes"], json!(["R1", "R2", "R3"]));
    assert_eq!(o["output"]["part_created"], true);
    // Same spec in another spelling reuses the part.
    let o = exec(&r, &mut s, "circuit.add", json!({"part": "resistor 0402 10k 1%"}));
    assert_eq!(o["output"]["part"], "R_10k_1pct_0402");
    assert_eq!(o["output"]["part_created"], false);
    assert_eq!(o["output"]["refdes"], json!(["R4"]));

    let o = exec(
        &r,
        &mut s,
        "circuit.add",
        json!({"part": "R_10k_1pct_0402", "refdes": "r10"}),
    );
    assert_eq!(o["output"]["refdes"], json!(["R10"]));
    let f = fail(
        &r,
        &mut s,
        "circuit.add",
        json!({"part": "R_10k_1pct_0402", "refdes": "R10"}),
    );
    assert_eq!(f.error.diagnostic.code, "circuit.refdes_taken");
    assert_eq!(f.error.diagnostic.hint.as_deref(), Some("next free: R11"));

    let f = fail(&r, &mut s, "circuit.add", json!({"part": "R_10k_1pct_0403"}));
    assert_eq!(f.error.diagnostic.code, "part.not_found");
    assert_eq!(
        f.error.diagnostic.hint.as_deref(),
        Some("did you mean `R_10k_1pct_0402`?")
    );

    let o = exec(&r, &mut s, "circuit.list", json!({}));
    let refs: Vec<&str> = o["output"]["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["refdes"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["R1", "R2", "R3", "R4", "R10"]);

    // Part in use cannot be removed.
    let f = fail(&r, &mut s, "part.remove", json!({"id": "R_10k_1pct_0402"}));
    assert_eq!(f.error.diagnostic.code, "part.in_use");
    exec(
        &r,
        &mut s,
        "circuit.remove",
        json!({"refdes": ["R1", "r2", "R3", "R4", "R10"]}),
    );
    exec(&r, &mut s, "part.remove", json!({"id": "R_10k_1pct_0402"}));
    // The footprint stays until removed explicitly.
    exec(&r, &mut s, "footprint.remove", json!({"name": "RESC1005X40N"}));
}

#[test]
fn concrete_part_with_generated_symbol_and_footprint() {
    let (_d, r, mut s) = setup();
    let o = exec(&r, &mut s, "part.create", ldo_args());
    assert_eq!(o["output"]["id"], "AP2112K-3.3TRG1");
    assert_eq!(o["output"]["footprint"], "SOT95P280X145-5N");
    let shown = exec(&r, &mut s, "part.show", json!({"id": "mpn:AP2112K-3.3TRG1"}));
    let pins = shown["output"]["part"]["symbol"]["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 5);
    assert!(pins.iter().all(|p| p.get("at").is_some()), "symbol pins are placed");

    // Pin pointing at a missing pad is rejected.
    let mut bad = ldo_args();
    bad["mpn"] = json!("X1");
    bad["pin_map"] = json!({"5": ["6"]});
    let f = fail(&r, &mut s, "part.create", bad);
    assert_eq!(f.error.diagnostic.code, "part.pin_without_pad");

    // Unmapped pads warn: a QFN exposed pad without a pin.
    let qfn = json!({
        "category": "ic", "mpn": "TEST-QFN", "package": "QFN-16 3x3mm P0.5mm EP1.7mm",
        "pins": (1..=16).map(|i| json!({"number": i.to_string(), "name": format!("P{i}")})).collect::<Vec<_>>()
    });
    let o = exec(&r, &mut s, "part.create", qfn);
    let diags = o["diagnostics"].as_array().unwrap();
    assert!(diags.iter().any(|d| d["code"] == "part.unconnected_pad"), "{diags:?}");

    // Duplicate id.
    let f = fail(&r, &mut s, "part.create", ldo_args());
    assert_eq!(f.error.diagnostic.code, "part.exists");
}

#[test]
fn bom_lines_dnp_approve_replace_export() {
    let (d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "circuit.add",
        json!({"part": "C 100nF 16V X7R 0402", "count": 2}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 0402", "count": 2}));
    exec(&r, &mut s, "part.create", ldo_args());
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "bom.dnp", json!({"refdes": ["R2"]}));
    exec(
        &r,
        &mut s,
        "bom.approve",
        json!({"part": "C_100nF_16V_X7R_0402", "add": ["CL05B104KO5NNNC"], "manufacturer": "Samsung"}),
    );
    exec(
        &r,
        &mut s,
        "bom.note",
        json!({"part": "AP2112K-3.3TRG1", "note": "any 3.3V SOT-23-5 LDO, same pinout"}),
    );

    let o = exec(&r, &mut s, "bom.list", json!({}));
    let lines = o["output"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["refdes"], json!(["C1", "C2"]));
    assert_eq!(lines[1]["refdes"], json!(["R1"]));
    assert_eq!(lines[1]["dnp"], json!(["R2"]));
    assert_eq!(lines[2]["value"], "AP2112K-3.3TRG1");
    assert_eq!(o["output"]["placements"], 4);
    assert_eq!(o["output"]["unsourced"], json!(["R_10k_0402"]));

    let o = exec(&r, &mut s, "bom.export", json!({"path": "out/bom.csv"}));
    assert_eq!(o["output"]["lines"], 3);
    let csv = std::fs::read_to_string(d.path().join("p/out/bom.csv")).unwrap();
    assert!(csv.starts_with("Line,Quantity,Designators,"));
    assert!(csv.contains("1,2,\"C1,C2\",100nF,"));
    assert!(csv.contains("Samsung CL05B104KO5NNNC"));

    let o = exec(
        &r,
        &mut s,
        "bom.export",
        json!({"path": "out/pcbway.csv", "format": "pcbway"}),
    );
    assert!(
        o["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bom.no_mpn")
    );
    let csv = std::fs::read_to_string(d.path().join("p/out/pcbway.csv")).unwrap();
    assert!(csv.contains(",Samsung,CL05B104KO5NNNC,"), "{csv}");
    assert!(!csv.contains("R2"), "DNP components are not in fab BOMs");

    // Replace the generic resistor with a concrete one.
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "resistor", "mpn": "RC0402FR-0710KL", "manufacturer": "Yageo", "package": "0402", "params": {"resistance": "10k", "tolerance": "1%"}}),
    );
    let o = exec(
        &r,
        &mut s,
        "bom.replace",
        json!({"from": "R_10k_0402", "to": "RC0402FR-0710KL"}),
    );
    assert_eq!(o["output"]["refdes"], json!(["R1", "R2"]));
    let o = exec(&r, &mut s, "bom.list", json!({}));
    assert_eq!(o["output"]["unsourced"], json!([]));

    // Everything persists.
    s.save().unwrap();
    let (s2, _) = Session::open(&d.path().join("p")).unwrap();
    assert_eq!(s2.project, s.project);
}

#[test]
fn footprint_generate_command() {
    let (_d, r, mut s) = setup();
    let o = exec(&r, &mut s, "footprint.generate", json!({"package": "SOIC-8"}));
    assert_eq!(o["output"]["name"], "SOIC127P600X175-8N");
    assert_eq!(o["output"]["status"], "created");
    let o = exec(&r, &mut s, "footprint.generate", json!({"package": "SOIC-8"}));
    assert_eq!(o["output"]["status"], "unchanged");
    let o = exec(
        &r,
        &mut s,
        "footprint.generate",
        json!({"package": "SOIC-8", "density": "most"}),
    );
    assert_eq!(o["output"]["name"], "SOIC127P600X175-8M");
    // From datasheet dimensions.
    let o = exec(
        &r,
        &mut s,
        "footprint.generate",
        json!({"spec": {
            "family": "chip", "kind": "capacitor", "length": "2.0±0.1mm", "width": "1.25±0.1mm",
            "terminal": "0.25..0.75mm", "height": "1.25mm"
        }}),
    );
    assert_eq!(o["output"]["name"], "CAPC2012X125N");
    let f = fail(&r, &mut s, "footprint.generate", json!({"package": "SOIC-9"}));
    assert_eq!(f.error.diagnostic.code, "footprint.unknown_package");
}
