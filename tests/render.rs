//! Rendering through the command registry (M3).

use cadlab::command::{Registry, RunOptions, Session};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

#[test]
fn renders_schematic_symbol_footprint() {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "pins": [{"number": "1", "name": "VIN", "kind": "power_in"}, {"number": "2", "name": "GND", "kind": "power_in"},
                 {"number": "3", "name": "EN", "kind": "input"}, {"number": "4", "name": "NC", "kind": "no_connect"},
                 {"number": "5", "name": "VOUT", "kind": "power_out"}]}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0402"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1", "R1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));

    let o = exec(&r, &mut s, "render.schematic", json!({}));
    assert_eq!(o["output"]["format"], "png");
    assert_eq!(o["output"]["png"], o["output"]["path"]);
    let png = std::fs::read(dir.path().join("p/out/schematic.png")).unwrap();
    assert_eq!(&png[1..4], b"PNG");

    // SVG output is deterministic.
    exec(&r, &mut s, "render.schematic", json!({"path": "a.svg"}));
    exec(&r, &mut s, "render.schematic", json!({"path": "b.svg"}));
    let a = std::fs::read_to_string(dir.path().join("p/a.svg")).unwrap();
    assert_eq!(a, std::fs::read_to_string(dir.path().join("p/b.svg")).unwrap());
    assert!(a.contains("viewBox=\"0 0 297 210\""), "fits on A4");

    exec(&r, &mut s, "render.symbol", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "render.footprint", json!({"name": "AP2112K-3.3TRG1"}));
    assert!(dir.path().join("p/out/footprint-SOT95P280X145-5N.png").is_file());
    let f = r.execute(&mut s, "render.schematic", json!({"path": "x.bmp"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "render.format");

    // Hints: pin U1 somewhere, then release it.
    exec(&r, &mut s, "schematic.place", json!({"refdes": "U1", "at": ["150mm", "100mm"], "rot": 0}));
    let o = exec(&r, &mut s, "render.schematic", json!({"path": "c.svg"}));
    assert_eq!(o["output"]["format"], "svg");
    exec(&r, &mut s, "schematic.unplace", json!({}));
    assert!(s.project.as_ref().unwrap().schematic().is_none());
}
