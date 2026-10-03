//! `.kicad_pcb` export, checked by KiCad itself (docs/TESTING.md, "Oracles").
//!
//! Without `CADLAB_ORACLES=1` only the format tests run. With it, `kicad-cli` (path from
//! `CADLAB_ORACLE_KICAD_CLI` or `PATH`) loads the exported boards and runs DRC, IPC-D-356 and
//! Gerber export on them.
//!
//! The violation-by-violation comparison with cadlab's own DRC is in `tests/drc_crosscheck.rs`,
//! the raster comparison of both tools' Gerbers in `tests/gerber_crosscheck.rs`.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::geom::Point;
use cadlab::model::board::{PadConnection, Zone};
use cadlab::units::Nm;
use common::oracle::{self, Oracle};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// The LDO + two capacitors circuit from `tests/board.rs`, on a 20 × 15 mm board, with a
/// `power` net class for VIN and GND.
fn ldo() -> (tempfile::TempDir, Registry, Session) {
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
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1"]}));
    exec(
        &r,
        &mut s,
        "netclass.set",
        json!({"name": "power", "track_width": "0.4mm", "via_drill": "0.3mm", "via_diameter": "0.7mm"}),
    );
    exec(&r, &mut s, "net.set", json!({"nets": ["VIN", "GND"], "class": "power"}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm", "corner_radius": "1mm"}));
    // U1 pads: 1 VIN (8.85, 8.45), 2 GND (8.85, 7.5), 3 EN (8.85, 6.55), 4 NC (11.15, 6.55), 5 VOUT (11.15, 8.45).
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    // C1 rotated: C1.1 (VIN) at (5.45, 10), C1.2 (GND) at (4.55, 10).
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "10mm"], "rotation": 180}));
    // C2.1 (3V3) at (14.55, 10), C2.2 (GND) at (15.45, 10).
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "10mm"]}));
    (dir, r, s)
}

fn mm(x: f64, y: f64) -> Value {
    json!([format!("{x}mm"), format!("{y}mm")])
}

/// Fully routes the LDO board: VIN and 3V3 on top, GND through vias and the bottom layer, plus a
/// GND pour on the bottom.
fn route_all(r: &Registry, s: &mut Session) {
    let t = |s: &mut Session, layer: &str, pts: Vec<Value>| {
        exec(r, s, "track.add", json!({"layer": layer, "points": pts}));
    };
    // VIN: U1.1 and U1.3 joined by a spine at x = 7, then up to C1.1.
    t(s, "F.Cu", vec![json!("U1.1"), mm(7.0, 8.45), mm(7.0, 6.55), json!("U1.3")]);
    t(s, "F.Cu", vec![json!("C1.1"), mm(7.0, 10.0), mm(7.0, 8.45)]);
    // 3V3.
    t(s, "F.Cu", vec![json!("U1.5"), mm(13.0, 8.45), mm(13.0, 10.0), json!("C2.1")]);
    // GND: each pad to a via, vias joined on B.Cu.
    for at in [mm(7.95, 7.5), mm(3.5, 10.0), mm(16.5, 10.0)] {
        exec(r, s, "via.add", json!({"at": at, "net": "GND"}));
    }
    t(s, "F.Cu", vec![json!("U1.2"), mm(7.95, 7.5)]);
    t(s, "F.Cu", vec![json!("C1.2"), mm(3.5, 10.0)]);
    t(s, "F.Cu", vec![json!("C2.2"), mm(16.5, 10.0)]);
    exec(
        r,
        s,
        "track.add",
        json!({"layer": "B.Cu", "net": "GND", "points": [mm(7.95, 7.5), mm(3.5, 7.5), mm(3.5, 10.0)]}),
    );
    exec(
        r,
        s,
        "track.add",
        json!({"layer": "B.Cu", "net": "GND", "points": [mm(7.95, 7.5), mm(7.95, 4.0), mm(16.5, 4.0), mm(16.5, 10.0)]}),
    );
    // Bottom GND pour (written directly: zone commands are another workstream).
    let p = s.project.as_mut().unwrap();
    let id = p.alloc_id();
    let n = |v: f64| Nm((v * 1e6) as i64);
    p.board_mut().zones.push(Zone {
        id,
        name: "GND_bottom".into(),
        net: Some("GND".into()),
        layers: vec!["B.Cu".into()],
        outline: [(1.0, 1.0), (19.0, 1.0), (19.0, 14.0), (1.0, 14.0)]
            .into_iter()
            .map(|(x, y)| Point::new(n(x), n(y)))
            .collect(),
        priority: 0,
        clearance: None,
        min_width: None,
        pads: PadConnection::Thermal,
        thermal_gap: None,
        thermal_spoke: None,
    });
}

fn export(r: &Registry, s: &mut Session, name: &str) -> PathBuf {
    let o = exec(r, s, "board.export_kicad", json!({"path": format!("kicad/{name}")}));
    assert_eq!(o["output"]["warnings"], Value::Null, "no export warnings: {o}");
    PathBuf::from(o["output"]["pcb"].as_str().unwrap())
}

/// Directory kept after the test run, for later comparisons.
fn keep_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("kicad_pcb_oracle").join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Keeps the exported files and the DRC report.
fn keep(pcb: &Path, name: &str) {
    let d = keep_dir(name);
    for ext in ["kicad_pcb", "kicad_pro", "kicad_dru", "drc.json"] {
        std::fs::copy(pcb.with_extension(ext), d.join(format!("board.{ext}"))).unwrap();
    }
}

/// Runs KiCad DRC (zones refilled, all severities, no schematic parity) and returns the report.
fn kicad_drc(cli: &Path, pcb: &Path) -> Value {
    let out = pcb.with_extension("drc.json");
    oracle::run(
        cli,
        &[
            "pcb",
            "drc",
            "--format",
            "json",
            "--severity-all",
            "--refill-zones",
            "-o",
            out.to_str().unwrap(),
            pcb.to_str().unwrap(),
        ],
    );
    serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap()
}

/// `type (severity): description [items]` for every violation.
fn violations(report: &Value) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for x in report["violations"].as_array().unwrap() {
        let items: Vec<&str> =
            x["items"].as_array().unwrap().iter().filter_map(|i| i["description"].as_str()).collect();
        v.push((
            x["type"].as_str().unwrap().to_string(),
            format!("{} ({}): {} {:?}", x["type"], x["severity"], x["description"], items),
        ));
    }
    for x in report["unconnected_items"].as_array().unwrap() {
        v.push(("unconnected_items".into(), format!("unconnected: {x}")));
    }
    v
}

/// KiCad DRC items that are expected on any cadlab export, with the reason each is acceptable.
const ALLOWLIST: &[(&str, &str)] = &[(
    "lib_footprint_issues",
    "footprints are embedded in the board; there is no `cadlab` KiCad footprint library to \
     configure (cadlab never ships or converts KiCad libraries, D7)",
)];

#[test]
fn export_is_deterministic_and_well_formed() {
    let (_d, r, mut s) = ldo();
    route_all(&r, &mut s);
    exec(&r, &mut s, "place.flip", json!({"refdes": ["C2"]}));
    let p = s.project.as_ref().unwrap();
    let a = cadlab::kicad_pcb::export(p, "x");
    let b = cadlab::kicad_pcb::export(&p.clone(), "x");
    assert_eq!(a, b, "deterministic");
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    assert_eq!(a.footprints, 3);

    // Balanced parentheses outside strings, and one top-level expression.
    let (mut depth, mut in_str, mut esc, mut closed_top) = (0i64, false, false, 0);
    for c in a.pcb.chars() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                assert!(depth >= 0, "unbalanced");
                if depth == 0 {
                    closed_top += 1;
                }
            }
            _ => {}
        }
    }
    assert_eq!((depth, in_str, closed_top), (0, false, 1));
    assert!(a.pcb.starts_with("(kicad_pcb (version 20240108) (generator \"cadlab\")"));
    for section in [
        "\n  (general",
        "\n  (layers",
        "\n  (setup",
        "\n  (net 0 \"\")",
        "\n  (net 1 \"3V3\")",
        "\n  (footprint \"cadlab:SOT95P280X145-5N\" (layer \"F.Cu\")",
        "\n  (footprint \"cadlab:CAPC1005X55N\" (layer \"B.Cu\")",
        "\n  (gr_arc ",
        "\n  (gr_line ",
        "\n  (segment ",
        "\n  (via ",
        "\n  (zone ",
        "(roundrect_rratio 0.25)",
        "(layers \"B.Cu\" \"B.Mask\" \"B.Paste\")",
        "(pinfunction \"VOUT\") (pintype \"power_out\")",
    ] {
        assert!(a.pcb.contains(section), "missing `{section}`:\n{}", a.pcb);
    }
    // UUIDs are unique, and each maps back to the cadlab object it was written for.
    let uuids: Vec<&str> = a.pcb.match_indices("(uuid \"").map(|(i, _)| &a.pcb[i + 7..i + 43]).collect();
    let set: BTreeSet<&str> = uuids.iter().copied().collect();
    assert_eq!(set.len(), uuids.len(), "duplicate uuid");
    assert_eq!(set, a.uuids.keys().map(String::as_str).collect::<BTreeSet<_>>(), "every uuid is labelled");
    let labels: BTreeSet<&str> = a.uuids.values().map(String::as_str).collect();
    for l in ["U1", "U1.5", "C2.1", "track#13", "edge", "text:U1/Reference"] {
        assert!(labels.contains(l), "no uuid for {l}: {labels:?}");
    }
    assert!(labels.iter().any(|l| l.starts_with("via#")) && labels.iter().any(|l| l.starts_with("zone#")));
    // Cross-check regressions (tests/gerber_crosscheck.rs, tests/drc_crosscheck.rs):
    // the zone carries the thermal settings cadlab fills with (gap = clearance 0.2 mm, spoke =
    // the GND class width 0.4 mm), not KiCad's 0.5 mm defaults;
    assert!(a.pcb.contains("(fill (thermal_gap 0.2) (thermal_bridge_width 0.4))"), "zone thermal settings");
    // the reference sits where cadlab's legend prints it: upright, 0.3 mm above the courtyard
    // (U1: courtyard 1.71 mm above the origin, text 1 mm tall);
    assert!(a.pcb.contains("(property \"Reference\" \"U1\" (at 0 -2.51 0) (layer \"F.SilkS\")"), "U1 reference");
    // no derived minimum via diameter (cadlab checks drill and annular ring only).
    let pro: Value = serde_json::from_str(&a.project).unwrap();
    assert_eq!(pro["board"]["design_settings"]["rules"]["min_via_diameter"], 0.0);

    // Project file: valid JSON with the net class and its pattern; width rule in the .kicad_dru.
    let pro: Value = serde_json::from_str(&a.project).unwrap();
    let classes = pro["net_settings"]["classes"].as_array().unwrap();
    assert_eq!(classes.iter().map(|c| c["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Default", "power"]);
    assert_eq!(classes[1]["track_width"], 0.4);
    assert_eq!(
        pro["net_settings"]["netclass_patterns"],
        json!([{"netclass": "power", "pattern": "GND"}, {"netclass": "power", "pattern": "VIN"}])
    );
    assert!(a.rules.contains("(condition \"A.NetClass == 'power'\")"));
    assert!(a.rules.contains("(constraint track_width (min 0.4mm))"));

    // The command writes the three files.
    let pcb = export(&r, &mut s, "board");
    assert_eq!(
        std::fs::read_to_string(&pcb).unwrap(),
        cadlab::kicad_pcb::export(s.project.as_ref().unwrap(), "board").pcb
    );
    assert!(pcb.with_extension("kicad_pro").is_file());
    assert!(pcb.with_extension("kicad_dru").is_file());
}

#[test]
fn kicad_drc_clean_board() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (_d, r, mut s) = ldo();
    route_all(&r, &mut s);
    let o = exec(&r, &mut s, "board.ratsnest", json!({}));
    assert_eq!(o["output"]["lines"], json!([]), "cadlab sees the board fully routed");
    let pcb = export(&r, &mut s, "clean");
    let report = kicad_drc(&cli, &pcb);
    keep(&pcb, "clean");
    let unexpected: Vec<String> = violations(&report)
        .into_iter()
        .filter(|(ty, _)| !ALLOWLIST.iter().any(|(a, _)| a == ty))
        .map(|(_, d)| d)
        .collect();
    assert!(unexpected.is_empty(), "KiCad DRC on the routed board:\n{}", unexpected.join("\n"));
}

#[test]
fn kicad_drc_finds_deliberate_violations() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (_d, r, mut s) = ldo();
    // Only 3V3 is routed: VIN and GND stay unrouted.
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.5", mm(13.0, 8.45), mm(13.0, 10.0), "C2.1"]}));
    // Clearance: a VIN track 0.1 mm from the 3V3 track (0.25 mm wide at x = 13).
    exec(
        &r,
        &mut s,
        "track.add",
        json!({"layer": "F.Cu", "net": "VIN", "width": "0.4mm", "points": [mm(13.425, 8.6), mm(13.425, 9.6)]}),
    );
    // Narrower than the power class (0.4 mm) but above the board minimum (0.15 mm).
    exec(
        &r,
        &mut s,
        "track.add",
        json!({"layer": "B.Cu", "net": "GND", "width": "0.2mm", "points": [mm(5.0, 4.0), mm(9.0, 4.0)]}),
    );
    // Copper 0.1 mm from the board edge (rule: 0.3 mm).
    exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "net": "VIN", "points": [mm(5.0, 0.225), mm(9.0, 0.225)]}));
    let pcb = export(&r, &mut s, "violations");
    let report = kicad_drc(&cli, &pcb);
    keep(&pcb, "violations");
    let found: BTreeSet<String> = violations(&report).into_iter().map(|(t, _)| t).collect();
    for expected in ["clearance", "track_width", "copper_edge_clearance", "unconnected_items"] {
        assert!(found.contains(expected), "KiCad did not report `{expected}`; found {found:?}");
    }
    // The width violation is the GND track, from the net class rule.
    let width: Vec<&Value> =
        report["violations"].as_array().unwrap().iter().filter(|v| v["type"] == "track_width").collect();
    assert_eq!(width.len(), 1, "{width:?}");
    assert!(width[0]["items"][0]["description"].as_str().unwrap().contains("[GND]"), "{width:?}");
    // VIN and GND are unrouted (the stray VIN/GND tracks are extra islands); 3V3 is routed.
    let unconnected = report["unconnected_items"].to_string();
    for net in ["[VIN]", "[GND]"] {
        assert!(unconnected.contains(net), "{net} not reported unconnected: {unconnected}");
    }
    assert!(!unconnected.contains("[3V3]"), "3V3 is routed: {unconnected}");
}

/// A pad record of KiCad's IPC-D-356 export: refdes, number, center and size in 0.0001 in,
/// rotation in degrees.
#[derive(Debug)]
struct IpcPad(String, String, (i64, i64), (i64, i64), i64);

fn ipc356_pads(text: &str) -> Vec<IpcPad> {
    let num = |s: &str, at: &mut usize, len: usize| -> i64 {
        let v: i64 = s[*at..*at + len].parse().unwrap();
        *at += len;
        v
    };
    text.lines()
        .filter(|l| l.starts_with("327") || l.starts_with("317"))
        .map(|l| {
            let refdes = l[20..26].trim().to_string();
            let pin = l[27..31].trim().to_string();
            let mut i = l[31..].find("X+").or_else(|| l[31..].find("X-")).unwrap() + 32;
            let x = num(l, &mut i, 7);
            i += 1;
            let y = num(l, &mut i, 7);
            i += 1;
            let w = num(l, &mut i, 4);
            i += 1;
            let h = num(l, &mut i, 4);
            i += 1;
            let rot = num(l, &mut i, 3);
            IpcPad(refdes, pin, (x, y), (w, h), rot)
        })
        .collect()
}

#[test]
fn kicad_sees_pads_where_cadlab_puts_them() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (_d, r, mut s) = ldo();
    // Arbitrary angles and both sides.
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"], "rotation": 30, "side": "bottom"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "10mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "10mm"], "rotation": 135, "side": "bottom"}));
    let pcb = export(&r, &mut s, "pads");
    let ipc = pcb.with_extension("ipc");
    oracle::run(&cli, &["pcb", "export", "ipcd356", "-o", ipc.to_str().unwrap(), pcb.to_str().unwrap()]);
    let kicad = ipc356_pads(&std::fs::read_to_string(&ipc).unwrap());
    let p = s.project.as_ref().unwrap();
    let frame = cadlab::kicad_pcb::Frame::of(p);
    let ours = cadlab::board::placed_pads(p);
    assert_eq!(kicad.len(), ours.len(), "{kicad:?}");
    for pp in &ours {
        let k = kicad
            .iter()
            .find(|k| k.0 == pp.refdes && k.1 == pp.number)
            .unwrap_or_else(|| panic!("{}.{} missing from {kicad:?}", pp.refdes, pp.number));
        // IPC-D-356: 0.0001 in, Y up, relative to the auxiliary axis origin, which the export
        // puts at cadlab's origin: the numbers are cadlab coordinates.
        assert_eq!(frame.from_kicad(frame.to_kicad(pp.center).0, frame.to_kicad(pp.center).1), pp.center);
        let to_tenth_mil = |v: Nm| (v.0 as f64 / 2540.0).round() as i64;
        let (ex, ey) = (to_tenth_mil(pp.center.x), to_tenth_mil(pp.center.y));
        assert!(
            (k.2.0 - ex).abs() <= 1 && (k.2.1 - ey).abs() <= 1,
            "{}.{}: KiCad {:?}, cadlab {:?}",
            pp.refdes,
            pp.number,
            k.2,
            (ex, ey)
        );
        let (w, h) = pp.pad.shape.size();
        assert_eq!(k.3, (to_tenth_mil(w), to_tenth_mil(h)), "{}.{} size", pp.refdes, pp.number);
        // Pad angle (two-fold symmetric shapes: compare modulo 180°). cadlab's absolute angle is
        // the footprint rotation, plus the pad's own (minus for mirrored bottom-side parts).
        let pf = &p.board().footprints[&pp.refdes];
        let ours_deg = (pf.rotation.0 / 1000) as i64;
        let kicad_ccw = (360 - k.4).rem_euclid(360);
        assert!(
            (kicad_ccw - ours_deg).rem_euclid(180) == 0 || (k.4 - ours_deg).rem_euclid(180) == 0,
            "{}.{} rotation: KiCad R{}, cadlab {ours_deg}",
            pp.refdes,
            pp.number,
            k.4
        );
    }
}

#[test]
fn kicad_exports_gerbers() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (_d, r, mut s) = ldo();
    route_all(&r, &mut s);
    let pcb = export(&r, &mut s, "gerbers");
    let out = keep_dir("gerbers");
    for e in std::fs::read_dir(&out).unwrap().flatten() {
        std::fs::remove_file(e.path()).unwrap();
    }
    std::fs::copy(&pcb, out.join("board.kicad_pcb")).unwrap();
    let dir = format!("{}/", out.display());
    oracle::run(&cli, &["pcb", "export", "gerbers", "-o", &dir, pcb.to_str().unwrap()]);
    oracle::run(&cli, &["pcb", "export", "drill", "-o", &dir, pcb.to_str().unwrap()]);
    let files: BTreeSet<String> =
        std::fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    for suffix in [
        "-F_Cu.gtl",
        "-B_Cu.gbl",
        "-F_Mask.gts",
        "-B_Mask.gbs",
        "-F_Paste.gtp",
        "-F_Silkscreen.gto",
        "-Edge_Cuts.gm1",
        ".drl",
    ] {
        assert!(files.iter().any(|f| f.ends_with(suffix)), "no *{suffix} in {files:?}");
    }
    let top = files.iter().find(|f| f.ends_with("-F_Cu.gtl")).unwrap();
    let gtl = std::fs::read_to_string(out.join(top)).unwrap();
    // Copper on the top layer: pads flashed and tracks drawn, with net attributes.
    assert!(gtl.contains("D03*"), "no flashes");
    assert!(gtl.contains("D01*"), "no draws");
    for net in ["VIN", "GND", "3V3"] {
        assert!(gtl.contains(&format!("%TO.N,{net}*%")), "no {net} object in {top}");
    }
    let drl = files.iter().find(|f| f.ends_with(".drl")).unwrap();
    let drl = std::fs::read_to_string(out.join(drl)).unwrap();
    assert_eq!(drl.lines().filter(|l| l.starts_with('X')).count(), 3, "three via holes:\n{drl}");
}

/// A 4-layer board using every exported feature: QFN with paste windows (exposed pad), a
/// through-hole header on the bottom, an arc track, a blind via, locked items, zones with each
/// pad connection style, a keep-out, board text, a DNP part.
fn feature_board() -> (tempfile::TempDir, Registry, Session) {
    use cadlab::model::board::{BoardGraphic, GraphicKind, Keepout, Track};
    let (d, r, mut s) = ldo();
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    let qfn = json!({
        "category": "ic", "mpn": "TEST-QFN", "package": "QFN-16 3x3mm P0.5mm EP1.7mm",
        "pins": (1..=17).map(|i| json!({"number": i.to_string(), "name": format!("P{i}")})).collect::<Vec<_>>()
    });
    exec(&r, &mut s, "part.create", qfn);
    exec(&r, &mut s, "circuit.add", json!({"part": "TEST-QFN"}));
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "connector", "mpn": "HDR-1x04", "package": "PinHeader 1x04",
        "pins": (1..=4).map(|i| json!({"number": i.to_string(), "name": format!("P{i}"), "kind": "passive"})).collect::<Vec<_>>()}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "HDR-1x04"}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U2.17", "J1.1"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U2", "at": ["5mm", "4mm"], "rotation": 45}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["17mm", "7mm"], "side": "bottom"}));
    exec(&r, &mut s, "place.lock", json!({"refdes": ["J1"]}));
    exec(&r, &mut s, "bom.dnp", json!({"refdes": ["C2"]}));
    exec(&r, &mut s, "via.add", json!({"at": mm(12.0, 3.0), "net": "GND", "from": "F.Cu", "to": "In1.Cu"}));
    let p = s.project.as_mut().unwrap();
    let n = |v: f64| Nm((v * 1e6) as i64);
    let pt = |x: f64, y: f64| Point::new(n(x), n(y));
    let id = p.alloc_id();
    p.board_mut().tracks.push(Track {
        id,
        layer: "In2.Cu".into(),
        width: n(0.3),
        net: Some("3V3".into()),
        start: pt(2.0, 12.0),
        end: pt(6.0, 12.0),
        mid: Some(pt(4.0, 13.0)),
        locked: true,
    });
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| vec![pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)];
    for (i, (pads, layer)) in
        [(PadConnection::Solid, "In1.Cu"), (PadConnection::None, "In2.Cu")].into_iter().enumerate()
    {
        let id = p.alloc_id();
        p.board_mut().zones.push(Zone {
            id,
            name: format!("Z{i}"),
            net: Some("GND".into()),
            layers: vec![layer.into()],
            outline: rect(1.0, 1.0, 10.0, 6.0),
            priority: i as u32 + 1,
            clearance: Some(n(0.3)),
            min_width: Some(n(0.25)),
            pads,
            thermal_gap: Some(n(0.4)),
            thermal_spoke: Some(n(0.35)),
        });
    }
    let id = p.alloc_id();
    p.board_mut().keepouts.push(Keepout {
        id,
        name: "antenna".into(),
        layers: vec![],
        outline: rect(14.0, 11.0, 18.0, 14.0),
        no_tracks: true,
        no_vias: true,
        no_pours: true,
        no_footprints: false,
    });
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "B.SilkS".into(),
        kind: GraphicKind::Text {
            text: "cadlab \"test\"".into(),
            at: pt(10.0, 13.0),
            size: n(1.0),
            rotation: Default::default(),
        },
    });
    (d, r, s)
}

#[test]
fn kicad_loads_every_feature() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let (_d, r, mut s) = feature_board();
    route_all(&r, &mut s);
    let pcb = export(&r, &mut s, "features");
    let ours = std::fs::read_to_string(&pcb).unwrap();
    // KiCad re-saves the board in its current format; nothing may be lost on the way.
    let resaved = pcb.with_file_name("resaved.kicad_pcb");
    std::fs::copy(&pcb, &resaved).unwrap();
    oracle::run(&cli, &["pcb", "upgrade", "--force", resaved.to_str().unwrap()]);
    let theirs = std::fs::read_to_string(&resaved).unwrap();
    std::fs::write(keep_dir("features").join("board.kicad_pcb"), &ours).unwrap();
    std::fs::write(keep_dir("features").join("resaved.kicad_pcb"), &theirs).unwrap();
    assert!(theirs.contains("(generator \"pcbnew\")"), "KiCad re-saved it");
    for token in [
        "(footprint ",
        "(pad ",
        "(segment",
        "(arc",
        "(via",
        "blind",
        "(zone",
        "(keepout",
        "(tracks not_allowed)",
        "(copperpour not_allowed)",
        "(gr_text",
        "(gr_line",
        "(gr_arc",
        "(fp_line",
        "(fp_poly",
        "(fp_circle",
        "(pinfunction",
        "(roundrect_rratio",
        "(locked yes)",
        "(priority",
        "(connect_pads yes",
        "(connect_pads no",
        "(thermal_gap 0.4)",
        "(thermal_bridge_width 0.35)",
        "(min_thickness 0.25)",
        "(attr smd exclude_from_pos_files exclude_from_bom dnp)",
        "(attr through_hole)",
        "\"F.Paste\")",
    ] {
        let (a, b) = (ours.matches(token).count(), theirs.matches(token).count());
        assert!(a > 0, "the feature board has no `{token}`");
        assert_eq!(a, b, "`{token}`: {a} written, {b} after KiCad re-saved the board");
    }
}
