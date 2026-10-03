//! Fab profiles (M4): `fab.list/show/check/compare/export`, DRC targets, profile BOM layouts.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::sync::Arc;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::fab::{Profiles, RotationOffset};
use cadlab::model::board::Track;
use cadlab::supplier::catalog::Catalog;
use cadlab::supplier::{Candidate, Suppliers};
use cadlab::{Angle, Nm, Point};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// The LDO + caps circuit of `tests/board.rs`, placed on a 20 x 15 mm board and fully routed
/// (as in `tests/drc.rs`).
fn routed() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "manufacturer": "Diodes", "package": "SOT-23-5",
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
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "7.5mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "7.5mm"], "rotation": 90}));
    let track = |r: &Registry, s: &mut Session, layer: &str, points: Value, net: Option<&str>| {
        let mut a = json!({"layer": layer, "points": points});
        if let Some(n) = net {
            a["net"] = json!(n);
        }
        exec(r, s, "track.add", a);
    };
    track(&r, &mut s, "F.Cu", json!(["U1.1", ["7.5mm", "8.45mm"], ["7.5mm", "6.55mm"], "U1.3"]), None);
    track(&r, &mut s, "F.Cu", json!(["C1.1", ["5mm", "6.55mm"], ["7.5mm", "6.55mm"]]), Some("VIN"));
    track(&r, &mut s, "F.Cu", json!(["U1.5", ["14mm", "8.45mm"], ["14mm", "7.0475mm"], "C2.1"]), None);
    for at in [["8.8475mm", "7.5mm"], ["5mm", "8.8mm"], ["16.5mm", "7.5mm"]] {
        exec(&r, &mut s, "via.add", json!({"at": at, "net": "GND"}));
    }
    track(&r, &mut s, "B.Cu", json!([["5mm", "8.8mm"], ["8.8475mm", "7.5mm"], ["16.5mm", "7.5mm"]]), Some("GND"));
    track(&r, &mut s, "F.Cu", json!(["C1.2", ["5mm", "8.8mm"]]), None);
    track(&r, &mut s, "F.Cu", json!([["16.5mm", "7.5mm"], ["16.5mm", "7.9525mm"], "C2.2"]), Some("GND"));
    (dir, r, s)
}

/// Adds an unconnected-net track of `width` mm in a free corner of the board.
fn thin_track(s: &mut Session, width_um: i64) {
    let p = s.project.as_mut().unwrap();
    let id = p.alloc_id();
    p.board_mut().tracks.push(Track {
        id,
        layer: "F.Cu".into(),
        width: Nm::from_um(width_um),
        net: Some("3V3".into()),
        start: Point::new(Nm::from_mm(2), Nm::from_mm(2)),
        end: Point::new(Nm::from_mm(6), Nm::from_mm(2)),
        mid: None,
        locked: false,
    });
}

fn codes(o: &Value, severity: &str) -> Vec<String> {
    o["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == severity)
        .map(|d| d["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn list_and_show() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let o = exec(&r, &mut s, "fab.list", json!({}));
    let ids: Vec<&str> =
        o["output"]["profiles"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    for id in ["generic", "jlcpcb", "pcbway"] {
        assert!(ids.contains(&id), "{ids:?}");
    }
    let o = exec(&r, &mut s, "fab.show", json!({"fab": "jlcpcb"}));
    let p = &o["output"]["profile"];
    assert_eq!(p["verified_at"], "2026-10-04");
    assert!(p["sources"][0].as_str().unwrap().starts_with("https://jlcpcb.com/"));
    assert_eq!(p["process"][0]["min_track"], "0.1mm");
    assert_eq!(p["assembly"]["cpl"]["columns"][1]["header"], "Mid X");
    assert!(o["output"]["unverified"].as_array().unwrap().iter().any(|u| u == "assembly.cpl.origin"));

    let f = r.execute(&mut s, "fab.show", json!({"fab": "jlcpbc"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "fab.unknown");
    assert!(f.error.diagnostic.hint.as_deref().unwrap_or_default().contains("jlcpcb"));
}

#[test]
fn profiles_are_sourced_and_consistent() {
    let ps = Profiles::builtin();
    assert!(ps.warnings.is_empty());
    for (p, _) in ps.profiles.values() {
        for q in &p.processes {
            // Every limit used by fab.check is in the profile for both fabs.
            if p.id != "generic" {
                assert!(q.min_track.is_some() && q.min_space.is_some() && q.min_drill.is_some(), "{} {}", p.id, q.id);
                assert!(q.copper_to_edge.is_some() && q.max_size.is_some(), "{} {}", p.id, q.id);
            }
            for url in q.cite.values() {
                assert!(p.sources.contains(url), "{}: cited {url} is not in sources", p.id);
            }
        }
    }
    // Layer counts select processes.
    let j = ps.get("jlcpcb").unwrap();
    assert_eq!(j.process_for(2).unwrap().id, "two-layer");
    assert_eq!(j.process_for(4).unwrap().id, "four-layer");
    assert_eq!(j.process_for(8).unwrap().id, "multilayer");
    assert!(j.process_for(3).is_none());
}

#[test]
fn clean_board_passes_both_fabs() {
    let (_d, r, mut s) = routed();
    assert!(cadlab::drc::check(s.project.as_ref().unwrap()).is_empty());
    for fab in ["jlcpcb", "pcbway", "generic"] {
        let o = exec(&r, &mut s, "fab.check", json!({"fab": fab}));
        assert_eq!(o["output"]["errors"], 0, "{fab}: {:#}", o["diagnostics"]);
        assert!(o["output"]["process"].is_string());
        // No suppliers in a fresh session; generated footprints draw 0.12 mm silk lines, below
        // the fabs' 0.15 mm minimum.
        let mut w = codes(&o, "warning");
        w.sort();
        assert_eq!(w, ["fab.no_suppliers", "fab.silk_width"], "{fab}: {:#}", o["diagnostics"]);
    }
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "jlcpcb", "parts": false}));
    assert_eq!(o["output"]["process"], "two-layer");
    assert!(!codes(&o, "warning").contains(&"fab.no_suppliers".to_string()));

    let f =
        r.execute(&mut s, "fab.check", json!({"fab": "jlcpcb", "process": "hdi"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "fab.unknown_process");
}

#[test]
fn thin_tracks_fail_where_the_minimum_is_larger() {
    let (_d, r, mut s) = routed();
    thin_track(&mut s, 100);
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "generic", "parts": false}));
    assert!(codes(&o, "error").contains(&"fab.track_width".to_string()), "{:#}", o["diagnostics"]);
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "fab.track_width").unwrap();
    assert!(d["message"].as_str().unwrap().contains("minimum is 0.15mm"), "{d}");
    assert!(d["hint"].is_string());
    for fab in ["jlcpcb", "pcbway"] {
        let o = exec(&r, &mut s, "fab.check", json!({"fab": fab, "parts": false}));
        assert_eq!(o["output"]["errors"], 0, "0.1 mm is {fab}'s minimum: {:#}", o["diagnostics"]);
    }
    // The project's own rules are untouched.
    assert_eq!(s.project.as_ref().unwrap().board().rules.min_track_width, Nm::from_um(150));

    let o = exec(&r, &mut s, "fab.compare", json!({"fabs": ["jlcpcb", "pcbway", "generic"]}));
    let rows = o["output"]["fabs"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["fab"], "jlcpcb");
    assert_eq!(rows[0]["feasible"], true);
    assert_eq!(rows[1]["feasible"], true);
    assert_eq!(rows[2]["feasible"], false);
    assert_eq!(rows[2]["failing"], json!(["fab.track_width"]));

    // 0.09 mm is below both fabs' 2-layer minimum.
    thin_track(&mut s, 90);
    let o = exec(&r, &mut s, "fab.compare", json!({}));
    let rows = o["output"]["fabs"].as_array().unwrap();
    assert!(rows.iter().all(|r| r["feasible"] == false && r["failing"] == json!(["fab.track_width"])), "{rows:#?}");
}

#[test]
fn board_options_size_and_assembly_side() {
    let (_d, r, mut s) = routed();
    exec(&r, &mut s, "board.setup", json!({"thickness": "1.5mm", "finish": ["Gold plating", "ENIG"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "7.5mm"], "rotation": 90, "side": "bottom"}));
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "jlcpcb", "parts": false}));
    let errors = codes(&o, "error");
    assert!(errors.contains(&"fab.thickness".to_string()), "{errors:?}");
    assert_eq!(o["output"]["choices"]["finish"], "ENIG", "first offered preference");
    // Bottom-side assembly is offered by both.
    assert!(!errors.contains(&"fab.assembly_side".to_string()));
    let mut ps = Profiles::builtin();
    let p = &mut ps.profiles.get_mut("jlcpcb").unwrap().0;
    p.assembly.as_mut().unwrap().sides = vec![cadlab::fab::Side::Top];
    let rep = cadlab::fab::check::check(s.project.as_ref().unwrap(), p, None);
    assert!(rep.diagnostics.iter().any(|d| d.code == "fab.assembly_side"), "{:#?}", rep.diagnostics);
}

#[test]
fn drc_targets_add_warnings_only() {
    let (_d, r, mut s) = routed();
    s.project.as_mut().unwrap().manifest_mut().targets = vec!["generic".into(), "nope".into()];
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert_eq!(o["output"]["errors"], 0);
    assert!(codes(&o, "warning").contains(&"fab.unknown_target".to_string()));
    // A track the generic profile cannot make: a DRC error from the project's rules, and a
    // target warning.
    thin_track(&mut s, 100);
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert_eq!(codes(&o, "error"), ["drc.track_width"]);
    let w = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "fab.track_width").unwrap();
    assert_eq!(w["severity"], "warning");
    assert!(w["message"].as_str().unwrap().starts_with("[generic] "));
}

fn lcsc() -> Suppliers {
    let c: Candidate = serde_json::from_value(json!({
        "sku": "C51118", "manufacturer": "Diodes", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
        "stock": 12000, "prices": [{"qty": 1, "price": "0.12 USD"}], "lifecycle": "active"
    }))
    .unwrap();
    Suppliers::new().with(Arc::new(Catalog::from_parts("lcsc", vec![c])))
}

fn read_dir(dir: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
        })
        .collect()
}

#[test]
fn export_bundle_deterministic_zip_and_lock() {
    let (d, r, mut s) = routed();
    s.suppliers = lcsc();
    let o = exec(&r, &mut s, "fab.export", json!({"fab": "jlcpcb"}));
    assert_eq!(o["output"]["process"], "two-layer");
    let a = read_dir(&d.path().join("p/out/fab/jlcpcb"));
    exec(&r, &mut s, "fab.export", json!({"fab": "jlcpcb", "dir": "out/again"}));
    let b = read_dir(&d.path().join("p/out/again"));
    assert_eq!(a, b, "exports are byte-identical");
    let names: Vec<&str> = a.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "fab-lock.json",
            "p-BOM-jlcpcb.csv",
            "p-CPL-jlcpcb.csv",
            "p-PTH.drl",
            "p-jlcpcb.zip",
            "p.GBL",
            "p.GBO",
            "p.GBP",
            "p.GBS",
            "p.GKO",
            "p.GTL",
            "p.GTO",
            "p.GTP",
            "p.GTS",
        ]
    );

    // The zip holds exactly the fabrication files, flat, with the same contents.
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(a["p-jlcpcb.zip"].clone())).unwrap();
    let mut in_zip = Vec::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        assert_eq!(&buf, &a[f.name()], "{}", f.name());
        in_zip.push(f.name().to_string());
    }
    in_zip.sort();
    let mut fab_files: Vec<String> =
        a.keys().filter(|n| n.starts_with("p.G") || n.ends_with(".drl")).cloned().collect();
    fab_files.sort();
    assert_eq!(in_zip, fab_files);

    // BOM and CPL in JLCPCB's layouts.
    let bom = String::from_utf8(a["p-BOM-jlcpcb.csv"].clone()).unwrap();
    assert!(bom.starts_with("Comment,Designator,Footprint,JLCPCB Part #\r\n"), "{bom}");
    assert!(bom.contains("AP2112K-3.3TRG1,U1,SOT-23-5,C51118\r\n"), "{bom}");
    let cpl = String::from_utf8(a["p-CPL-jlcpcb.csv"].clone()).unwrap();
    assert!(cpl.starts_with("Designator,Mid X,Mid Y,Layer,Rotation\r\n"), "{cpl}");
    assert!(cpl.contains("U1,10mm,7.5mm,Top,0\r\n"), "{cpl}");
    assert!(cpl.contains("C1,5mm,7.5mm,Top,90\r\n"), "{cpl}");

    // The lock: profile, process, file hashes, chosen parts.
    let lock: Value = serde_json::from_slice(&a["fab-lock.json"]).unwrap();
    assert_eq!(lock["lock_version"], 1);
    assert_eq!(lock["profile"]["id"], "jlcpcb");
    assert_eq!(lock["profile"]["verified_at"], "2026-10-04");
    assert_eq!(lock["profile"]["source"], "builtin");
    assert_eq!(
        lock["process"],
        json!({"id": "two-layer", "layers": 2, "thickness": "1.6mm", "outer_copper": "0.035mm"})
    );
    let files = lock["files"].as_array().unwrap();
    assert_eq!(files.len(), a.len() - 1, "every file but the lock itself");
    for f in files {
        let name = f["name"].as_str().unwrap();
        assert_eq!(f["sha256"], cadlab::fab::sha256_hex(&a[name]), "{name}");
        assert_eq!(f["bytes"], a[name].len());
    }
    let u1 = lock["bom"].as_array().unwrap().iter().find(|l| l["designators"] == json!(["U1"])).unwrap();
    assert_eq!(u1["mpn"], "AP2112K-3.3TRG1");
    assert_eq!(u1["sku"], "C51118");
    assert_eq!(u1["provider"], "lcsc");
    assert_eq!(u1["status"], "ok");
    let caps = lock["bom"].as_array().unwrap().iter().find(|l| l["designators"] == json!(["C1", "C2"])).unwrap();
    assert_eq!(caps["status"], "no_mpn");
    assert!(caps.get("sku").is_none());

    // PCBWay: generic (KiCad-style) names, its own BOM layout, no SKU column.
    exec(&r, &mut s, "fab.export", json!({"fab": "pcbway"}));
    let w = read_dir(&d.path().join("p/out/fab/pcbway"));
    assert!(w.contains_key("p-F_Cu.gbr") && w.contains_key("p-pcbway.zip") && w.contains_key("p-PTH.drl"));
    let bom = String::from_utf8(w["p-BOM-pcbway.csv"].clone()).unwrap();
    assert!(bom.starts_with("Item #,Designator,Qty,Manufacturer,Mfg Part #,"), "{bom}");
    assert!(bom.contains(",U1,1,Diodes,AP2112K-3.3TRG1,"), "{bom}");
}

#[test]
fn export_refuses_on_errors_unless_forced() {
    let (_d, r, mut s) = routed();
    thin_track(&mut s, 90);
    let f = r.execute(&mut s, "fab.export", json!({"fab": "jlcpcb"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "fab.check_failed");
    let o = exec(&r, &mut s, "fab.export", json!({"fab": "jlcpcb", "force": true}));
    assert!(codes(&o, "error").contains(&"fab.track_width".to_string()));
    assert!(o["output"]["files"].as_array().unwrap().len() > 5);
}

#[test]
fn cpl_rotation_offsets_are_applied_at_export_only() {
    let (_d, _r, s) = routed();
    let p = s.project.as_ref().unwrap();
    let mut a = Profiles::builtin().get("jlcpcb").unwrap().assembly.clone().unwrap();
    a.rotation_offsets.push(RotationOffset { package: "SOT-23*".into(), offset: Angle::from_deg(180), source: None });
    a.rotation_offsets.push(RotationOffset { package: "0402".into(), offset: Angle::from_deg(-90), source: None });
    let before = p.clone();
    let (cpl, applied) = cadlab::fab::export::cpl_csv(p, &a);
    assert!(cpl.contains("U1,10mm,7.5mm,Top,180\r\n"), "{cpl}");
    assert!(cpl.contains("C1,5mm,7.5mm,Top,0\r\n"), "{cpl}");
    assert!(cpl.contains("C2,15mm,7.5mm,Top,0\r\n"), "{cpl}");
    let applied: Vec<(&str, &str)> = applied.iter().map(|o| (o.designator.as_str(), o.pattern.as_str())).collect();
    assert_eq!(applied, [("C1", "0402"), ("C2", "0402"), ("U1", "SOT-23*")]);
    assert_eq!(p, &before, "nothing is stored in the project");
    assert_eq!(p.board().footprints["U1"].rotation, Angle::ZERO);
}

#[test]
fn bom_export_uses_profile_layouts() {
    let (d, r, mut s) = routed();
    exec(&r, &mut s, "bom.export", json!({"path": "out/j.csv", "format": "jlcpcb"}));
    let csv = std::fs::read_to_string(d.path().join("p/out/j.csv")).unwrap();
    assert!(csv.starts_with("Comment,Designator,Footprint,JLCPCB Part #\r\n"), "{csv}");
    assert!(csv.contains("\"C1,C2\""), "{csv}");
    let rows = cadlab::bom::rows(s.project.as_ref().unwrap());
    assert_eq!(cadlab::bom::to_csv(&rows, cadlab::bom::CsvFormat::Jlcpcb), csv);
}
