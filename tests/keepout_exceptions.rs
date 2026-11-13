//! Net exceptions of keep-outs (D76) in the formats that carry keep-outs but have no per-net
//! exception: Specctra DSN, IDF, IDX and KiCad. Each writes the keep-out as it is without the
//! exception (stricter, never looser) and says so with `export.keepout_net_exception`.

use cadlab::command::{Registry, RunOptions, Session};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// The LDO board with two keep-outs: `feed` forbids everything on every layer except for net
/// 3V3, `plain` forbids tracks on the top layer for every net.
fn board(dir: &std::path::Path) -> (Registry, Session) {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.join("p")}));
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
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "9mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "9mm"], "rotation": 90}));
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "feed", "outline": {"rect": {"from": ["12mm", "1mm"], "to": ["13.5mm", "5mm"]}},
               "allow_nets": ["3V3"]}),
    );
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "plain", "layers": ["F.Cu"], "no_tracks": true,
               "outline": {"rect": {"from": ["2mm", "1mm"], "to": ["4mm", "3mm"]}}}),
    );
    (r, s)
}

/// The `export.keepout_net_exception` diagnostics of a result: one, about `feed` and 3V3.
fn assert_reported(o: &Value, what: &str) {
    let d: Vec<&Value> =
        o["diagnostics"].as_array().unwrap().iter().filter(|d| d["code"] == "export.keepout_net_exception").collect();
    assert_eq!(d.len(), 1, "{what}: {}", o["diagnostics"]);
    let d = d[0];
    assert_eq!(d["severity"], "warning", "{what}");
    let subjects = d["subjects"].to_string();
    assert!(subjects.contains("feed") && subjects.contains("3V3"), "{what}: {subjects}");
    assert!(d["message"].as_str().unwrap().contains("keep-out `feed` excepts net 3V3"), "{what}: {d}");
    assert!(d["hint"].as_str().unwrap().contains("keepout.set feed --forbid-nets"), "{what}: {d}");
}

fn assert_not_reported(o: &Value, what: &str) {
    let d = o["diagnostics"].as_array().unwrap();
    assert!(d.iter().all(|d| d["code"] != "export.keepout_net_exception"), "{what}: {}", o["diagnostics"]);
}

fn read(path: &Value) -> String {
    std::fs::read_to_string(path.as_str().unwrap()).unwrap()
}

#[test]
fn dsn_keeps_the_keepout_for_every_net() {
    let d = tempfile::tempdir().unwrap();
    let (r, mut s) = board(d.path());
    let o = exec(&r, &mut s, "export.dsn", json!({}));
    assert_reported(&o, "export.dsn");
    let with = read(&o["output"]["path"]);
    let dsn = cadlab::specctra::dsn::Dsn::parse(&with).unwrap();
    let feed: Vec<_> = dsn.keepouts.iter().filter(|k| k.name == "feed").collect();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].kind, cadlab::specctra::dsn::KeepoutKind::All, "tracks and vias, for every net");
    // The file is the one written without the exception.
    exec(&r, &mut s, "keepout.set", json!({"name": "feed", "forbid_nets": ["3V3"]}));
    let o = exec(&r, &mut s, "export.dsn", json!({}));
    assert_not_reported(&o, "export.dsn without exceptions");
    assert_eq!(read(&o["output"]["path"]), with);
}

#[test]
fn idf_and_idx_keep_the_keepout_for_every_net() {
    let d = tempfile::tempdir().unwrap();
    let (r, mut s) = board(d.path());
    let emn = |o: &Value| {
        let f = o["output"]["files"].as_array().unwrap().iter().find(|f| f["function"] == "IdfBoard").unwrap();
        read(&f["path"])
    };
    let o = exec(&r, &mut s, "export.idf", json!({}));
    assert_reported(&o, "export.idf");
    let idf = emn(&o);
    assert!(idf.contains(".ROUTE_KEEPOUT") && idf.contains(".VIA_KEEPOUT"));
    let o = exec(&r, &mut s, "export.idx", json!({}));
    assert_reported(&o, "export.idx");
    let idx = read(&o["output"]["path"]);
    // The exception is named in the description of the copper items, not of the component one.
    for what in ["no tracks", "no vias", "no copper pours"] {
        assert!(idx.contains(&format!("{what} on all copper layers, except net 3V3")), "{what}");
    }
    assert!(idx.contains("no tracks on F.Cu<"), "`plain` has no exception");
    assert!(!idx.contains("no components, except"));

    // A footprint-only keep-out has nothing to except in either file; without the exception
    // the IDF is the same and the IDX differs by the descriptions only.
    exec(&r, &mut s, "keepout.set", json!({"name": "feed", "forbid_nets": ["3V3"]}));
    let o = exec(&r, &mut s, "export.idf", json!({}));
    assert_not_reported(&o, "export.idf without exceptions");
    assert_eq!(emn(&o), idf);
    let o = exec(&r, &mut s, "export.idx", json!({}));
    assert_not_reported(&o, "export.idx without exceptions");
    assert_eq!(read(&o["output"]["path"]), idx.replace(", except net 3V3", ""));
}

#[test]
fn kicad_rule_area_without_the_exception() {
    let d = tempfile::tempdir().unwrap();
    let (r, mut s) = board(d.path());
    let o = exec(&r, &mut s, "board.export_kicad", json!({"path": d.path().join("k").join("b.kicad_pcb")}));
    assert_reported(&o, "board.export_kicad");
    let warnings = o["output"]["warnings"].to_string();
    assert!(warnings.contains("keep-out `feed`") && warnings.contains("3V3"), "{warnings}");
    let pcb = read(&o["output"]["pcb"]);
    assert!(
        pcb.contains(
            "(keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour not_allowed) (footprints not_allowed))"
        ),
        "the rule area forbids what the keep-out forbids"
    );
    // Read back: the keep-out, without the exception KiCad's file cannot hold.
    let mut s2 = Session::new();
    exec(&r, &mut s2, "project.new", json!({"path": d.path().join("q")}));
    exec(&r, &mut s2, "board.import_kicad", json!({"path": o["output"]["pcb"]}));
    let b = s2.project.as_ref().unwrap().board();
    let feed = b.keepouts.iter().find(|k| k.name == "feed").expect("feed");
    assert!(feed.no_tracks && feed.no_vias && feed.no_pours && feed.no_footprints);
    assert!(feed.allow_nets.is_empty());

    exec(&r, &mut s, "keepout.set", json!({"name": "feed", "forbid_nets": ["3V3"]}));
    let o2 = exec(&r, &mut s, "board.export_kicad", json!({"path": d.path().join("k2").join("b.kicad_pcb")}));
    assert_not_reported(&o2, "board.export_kicad without exceptions");
    assert_eq!(read(&o2["output"]["pcb"]), pcb, "the same file");
}
