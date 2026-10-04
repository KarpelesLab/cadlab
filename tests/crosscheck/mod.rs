//! Boards shared by the cross-check oracle tests (`tests/drc_crosscheck.rs`,
//! `tests/gerber_crosscheck.rs`): a clean routed LDO board with a bottom GND pour, and copies of
//! it with one deliberate defect each (docs/TESTING.md, "Cross-checks").

#![allow(dead_code, missing_docs)]

use std::path::{Path, PathBuf};

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::Project;
use serde_json::{Value, json};

pub fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

pub fn mm(x: f64, y: f64) -> Value {
    json!([format!("{x}mm"), format!("{y}mm")])
}

/// The LDO + two capacitors circuit on a 20 × 15 mm board with rounded corners, a `power` net
/// class (0.4 mm tracks) for VIN and GND.
///
/// U1 pads: 1 VIN (8.85, 8.45), 2 GND (8.85, 7.5), 3 EN (8.85, 6.55), 4 NC (11.15, 6.55),
/// 5 VOUT (11.15, 8.45). C1 (180°): C1.1 VIN (5.45, 10), C1.2 GND (4.55, 10).
/// C2: C2.1 3V3 (14.55, 10), C2.2 GND (15.45, 10).
pub fn ldo() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "xc"}));
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
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "10mm"], "rotation": 180}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "10mm"]}));
    (dir, r, s)
}

pub fn track(r: &Registry, s: &mut Session, layer: &str, pts: Vec<Value>) {
    exec(r, s, "track.add", json!({"layer": layer, "points": pts}));
}

pub fn net_track(r: &Registry, s: &mut Session, layer: &str, net: &str, width: Option<&str>, pts: Vec<Value>) {
    let mut a = json!({"layer": layer, "net": net, "points": pts});
    if let Some(w) = width {
        a["width"] = json!(w);
    }
    exec(r, s, "track.add", a);
}

/// Routes the LDO board: VIN and 3V3 on top, GND through vias and the bottom layer, and a GND
/// pour on the bottom.
pub fn route_all(r: &Registry, s: &mut Session) {
    // VIN: U1.1 and U1.3 joined by a spine at x = 7, then up to C1.1.
    track(r, s, "F.Cu", vec![json!("U1.1"), mm(7.0, 8.45), mm(7.0, 6.55), json!("U1.3")]);
    track(r, s, "F.Cu", vec![json!("C1.1"), mm(7.0, 10.0), mm(7.0, 8.45)]);
    // 3V3.
    track(r, s, "F.Cu", vec![json!("U1.5"), mm(13.0, 8.45), mm(13.0, 10.0), json!("C2.1")]);
    // GND: each pad to a via, vias joined on B.Cu.
    for at in [mm(7.95, 7.5), mm(3.5, 10.0), mm(16.5, 10.0)] {
        exec(r, s, "via.add", json!({"at": at, "net": "GND"}));
    }
    track(r, s, "F.Cu", vec![json!("U1.2"), mm(7.95, 7.5)]);
    track(r, s, "F.Cu", vec![json!("C1.2"), mm(3.5, 10.0)]);
    track(r, s, "F.Cu", vec![json!("C2.2"), mm(16.5, 10.0)]);
    net_track(r, s, "B.Cu", "GND", None, vec![mm(7.95, 7.5), mm(3.5, 7.5), mm(3.5, 10.0)]);
    net_track(r, s, "B.Cu", "GND", None, vec![mm(7.95, 7.5), mm(7.95, 4.0), mm(16.5, 4.0), mm(16.5, 10.0)]);
    exec(
        r,
        s,
        "zone.add",
        json!({"name": "GND_bottom", "net": "GND", "layers": ["B.Cu"],
               "outline": {"rect": {"from": ["1mm", "1mm"], "to": ["19mm", "14mm"]}}}),
    );
}

/// A cross-check board: a name, what it should show, and how it is built on top of the placed
/// LDO board.
pub struct Case {
    pub name: &'static str,
    /// The defect, in words.
    pub what: &'static str,
    /// cadlab codes this board must report (besides what the clean board reports: nothing).
    pub expect: &'static [&'static str],
    pub build: fn(&Registry, &mut Session),
}

/// Adds a 2-pin 0402 capacitor `refdes` with its pins on `nets`, placed at `at`.
fn add_cap(r: &Registry, s: &mut Session, refdes: &str, at: Value, nets: [&str; 2]) {
    exec(r, s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "refdes": refdes}));
    for (i, n) in nets.iter().enumerate() {
        exec(r, s, "net.connect", json!({"net": n, "pins": [format!("{refdes}.{}", i + 1)]}));
    }
    exec(r, s, "place.set", json!({"refdes": refdes, "at": at}));
}

/// The clean board plus geometry the defect boards do not exercise: a QFN with an exposed pad
/// (paste windows) at 45°, a through-hole header on the bottom side, 0402s at 30° (top) and
/// 135° (bottom), and an arc track. Nothing is connected to the new parts.
pub fn features(r: &Registry, s: &mut Session) {
    route_all(r, s);
    let qfn = json!({
        "category": "ic", "mpn": "TEST-QFN", "package": "QFN-16 3x3mm P0.5mm EP1.7mm",
        "pins": (1..=17).map(|i| json!({"number": i.to_string(), "name": format!("P{i}")})).collect::<Vec<_>>()
    });
    exec(r, s, "part.create", qfn);
    exec(r, s, "circuit.add", json!({"part": "TEST-QFN", "refdes": "U2"}));
    exec(r, s, "place.set", json!({"refdes": "U2", "at": mm(4.0, 3.6), "rotation": 45}));
    exec(
        r,
        s,
        "part.create",
        json!({"id": "HDR-1x02", "category": "connector", "package": "PinHeader 1x02",
               "pins": [{"number": "1", "name": "A", "kind": "passive"}, {"number": "2", "name": "B", "kind": "passive"}]}),
    );
    exec(r, s, "circuit.add", json!({"part": "HDR-1x02", "refdes": "J1"}));
    exec(r, s, "place.set", json!({"refdes": "J1", "at": mm(12.0, 2.0), "rotation": 90, "side": "bottom"}));
    // J1.1 joins the bottom GND pour through a thermal relief.
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["J1.1"]}));
    exec(r, s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "refdes": "C3"}));
    exec(r, s, "place.set", json!({"refdes": "C3", "at": mm(17.5, 6.0), "rotation": 30}));
    exec(r, s, "circuit.add", json!({"part": "C 100nF 16V X7R 0402", "refdes": "C4"}));
    exec(r, s, "place.set", json!({"refdes": "C4", "at": mm(2.5, 7.0), "rotation": 135, "side": "bottom"}));
    // An arc from C1.1 bulging up to the VIN corner at (7, 10).
    let o = exec(r, s, "track.add", json!({"layer": "F.Cu", "points": ["C1.1", mm(7.0, 10.0)]}));
    let id = o["output"]["tracks"][0]["id"].as_u64().unwrap();
    let p = s.project.as_mut().unwrap();
    let t = p.board_mut().tracks.iter_mut().find(|t| t.id.0 == id).unwrap();
    let n = |v: f64| cadlab::units::Nm((v * 1e6).round() as i64);
    t.start = cadlab::geom::Point::new(n(5.45), n(10.0));
    t.mid = Some(cadlab::geom::Point::new(n(6.225), n(10.8)));
}

/// The clean board with local settings (DECISIONS D40): mask and paste margins on the board and
/// the capacitors' footprint, a header (bottom side, unconnected but for J1.1 on GND) with a
/// slotted pad, paste-in-hole and a solid zone connection, and U1.4 (no net) keeping a 0.3 mm
/// local clearance that a VIN track 0.2 mm away violates.
pub fn local_settings(r: &Registry, s: &mut Session) {
    route_all(r, s);
    let fp = |s: &mut Session, refdes: &str| {
        cadlab::board::footprint_for(s.project.as_ref().unwrap(), refdes).unwrap().name.clone()
    };
    exec(r, s, "board.rules", json!({"mask_expansion": "0.03mm", "paste_margin": "-0.02mm"}));
    let cap = fp(s, "C1");
    exec(r, s, "footprint.set", json!({"name": cap, "mask_margin": "0.06mm", "paste_ratio": "-0.1"}));
    exec(
        r,
        s,
        "part.create",
        json!({"id": "HDR-1x02", "category": "connector", "package": "PinHeader 1x02",
               "pins": [{"number": "1", "name": "A", "kind": "passive"}, {"number": "2", "name": "B", "kind": "passive"}]}),
    );
    exec(r, s, "circuit.add", json!({"part": "HDR-1x02", "refdes": "J1"}));
    exec(r, s, "place.set", json!({"refdes": "J1", "at": mm(12.0, 2.0), "rotation": 90, "side": "bottom"}));
    exec(r, s, "net.connect", json!({"net": "GND", "pins": ["J1.1"]}));
    let hdr = fp(s, "J1");
    exec(
        r,
        s,
        "footprint.set",
        json!({"name": hdr, "pads": ["1"], "slot": ["1.2mm", "0.8mm"], "zone_connection": "solid"}),
    );
    exec(r, s, "footprint.set", json!({"name": hdr, "pads": ["2"], "paste": "pad", "mask_margin": "-0.05mm"}));
    let ldo = fp(s, "U1");
    exec(r, s, "footprint.set", json!({"name": ldo, "pads": ["4"], "clearance": "0.3mm"}));
    // U1.4: 1.405 × 0.57 mm at (11.1525, 6.55), right edge 11.855; a 0.4 mm VIN track from U1.3
    // under U1 and up 0.2 mm right of U1.4 (left edge 12.055).
    track(r, s, "F.Cu", vec![json!("U1.3"), mm(8.85, 5.5), mm(12.255, 5.5), mm(12.255, 6.55)]);
}

/// Pushes a silkscreen line (board graphics have no command yet).
fn silk_line(s: &mut Session, from: (f64, f64), to: (f64, f64)) {
    use cadlab::geom::Point;
    use cadlab::model::board::{BoardGraphic, GraphicKind};
    use cadlab::units::Nm;
    let n = |v: f64| Nm((v * 1e6).round() as i64);
    let p = s.project.as_mut().unwrap();
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "F.SilkS".into(),
        kind: GraphicKind::Line {
            points: vec![Point::new(n(from.0), n(from.1)), Point::new(n(to.0), n(to.1))],
            width: Nm::from_um(150),
        },
    });
}

/// The boards: the clean one first, then one deliberate violation per rule. Defects are
/// connected to a pad where possible (KiCad reports dangling ends and unconnected track
/// islands, see the allowlists).
pub fn cases() -> Vec<Case> {
    vec![
        Case { name: "clean", what: "fully routed, bottom GND pour", expect: &[], build: route_all },
        Case {
            name: "features",
            what: "clean, plus a QFN at 45° with paste windows, a bottom THT header, parts at 30° and 135° (bottom), an arc track",
            expect: &[],
            build: features,
        },
        Case {
            name: "local_settings",
            what: "mask/paste margins, a slot, paste-in-hole, a solid pad connection, and a track 0.2 mm from a pad keeping 0.3 mm",
            expect: &["drc.clearance"],
            build: local_settings,
        },
        Case {
            name: "clearance_track_track",
            what: "a VIN track 0.1 mm from the 3V3 track",
            expect: &["drc.clearance"],
            build: |r, s| {
                route_all(r, s);
                // 3V3 spine at x = 13 (0.25 mm, left edge 12.875); VIN is 0.4 mm (right edge 12.775).
                track(r, s, "F.Cu", vec![json!("C1.1"), mm(5.45, 11.5), mm(12.575, 11.5), mm(12.575, 9.0)]);
            },
        },
        Case {
            name: "clearance_track_pad",
            what: "a 3V3 track 0.1 mm from U1.4 (NC, no net)",
            expect: &["drc.clearance"],
            build: |r, s| {
                route_all(r, s);
                // U1.4: 1.405 × 0.57 mm at (11.1525, 6.55), right edge 11.855.
                track(r, s, "F.Cu", vec![json!("U1.5"), mm(12.08, 8.45), mm(12.08, 5.5)]);
            },
        },
        Case {
            name: "clearance_pad_via",
            what: "a GND via 0.1 mm from C2.1 (3V3)",
            expect: &["drc.clearance"],
            build: |r, s| {
                route_all(r, s);
                // C2.1: 0.565 × 0.57 mm at (14.5475, 10), top edge 10.285; via radius 0.3.
                exec(
                    r,
                    s,
                    "via.add",
                    json!({"at": mm(14.5475, 10.685), "net": "GND", "diameter": "0.6mm", "drill": "0.3mm"}),
                );
            },
        },
        Case {
            name: "short",
            what: "a GND track across the 3V3 track",
            expect: &["drc.short", "drc.clearance"],
            build: |r, s| {
                route_all(r, s);
                track(r, s, "F.Cu", vec![json!("C2.2"), mm(15.45, 9.0), mm(12.0, 9.0)]);
            },
        },
        Case {
            name: "track_width_class",
            what: "a VIN track narrower than the power class",
            expect: &["drc.track_width_class"],
            build: |r, s| {
                route_all(r, s);
                net_track(r, s, "F.Cu", "VIN", Some("0.2mm"), vec![mm(7.0, 10.0), mm(7.0, 12.0)]);
            },
        },
        Case {
            name: "track_width_min",
            what: "a VIN track narrower than the board minimum",
            expect: &["drc.track_width"],
            build: |r, s| {
                route_all(r, s);
                net_track(r, s, "F.Cu", "VIN", Some("0.1mm"), vec![mm(7.0, 10.0), mm(7.0, 12.0)]);
            },
        },
        Case {
            name: "via_annular_ring",
            what: "a GND via with a 0.075 mm ring",
            expect: &["drc.via_annular_ring"],
            build: |r, s| {
                route_all(r, s);
                exec(
                    r,
                    s,
                    "via.add",
                    json!({"at": mm(3.5, 12.0), "net": "GND", "diameter": "0.45mm", "drill": "0.3mm"}),
                );
                net_track(r, s, "F.Cu", "GND", None, vec![mm(3.5, 10.0), mm(3.5, 12.0)]);
            },
        },
        Case {
            name: "via_drill",
            what: "a GND via with a 0.2 mm drill",
            expect: &["drc.via_drill"],
            build: |r, s| {
                route_all(r, s);
                exec(
                    r,
                    s,
                    "via.add",
                    json!({"at": mm(3.5, 12.0), "net": "GND", "diameter": "0.6mm", "drill": "0.2mm"}),
                );
                net_track(r, s, "F.Cu", "GND", None, vec![mm(3.5, 10.0), mm(3.5, 12.0)]);
            },
        },
        Case {
            name: "pad_drill_and_ring",
            what: "a header with 0.25 mm holes in 0.4 mm pads",
            expect: &["drc.pad_drill", "drc.pad_annular_ring"],
            build: |r, s| {
                route_all(r, s);
                exec(
                    r,
                    s,
                    "footprint.generate",
                    json!({"name": "HDR_BAD",
                    "spec": {"family": "pin_header", "rows": 1, "pins_per_row": 2, "drill": "0.25mm", "pad": "0.4mm"}}),
                );
                exec(r, s, "part.create", json!({"id": "HDR2", "category": "connector", "package": "PinHeader 1x02"}));
                exec(r, s, "circuit.add", json!({"part": "HDR2", "refdes": "J1"}));
                exec(r, s, "place.set", json!({"refdes": "J1", "at": mm(12.0, 2.5), "rotation": 90}));
                let p = s.project.as_mut().unwrap();
                p.board_mut().footprints.get_mut("J1").unwrap().footprint = Some("HDR_BAD".into());
            },
        },
        Case {
            name: "hole_to_hole",
            what: "two GND vias whose holes are 0.3 mm apart",
            expect: &["drc.hole_to_hole"],
            build: |r, s| {
                route_all(r, s);
                exec(r, s, "via.add", json!({"at": mm(3.5, 12.0), "net": "GND"}));
                exec(r, s, "via.add", json!({"at": mm(4.1, 12.0), "net": "GND"}));
                net_track(r, s, "F.Cu", "GND", None, vec![mm(3.5, 10.0), mm(3.5, 12.0), mm(4.1, 12.0)]);
            },
        },
        Case {
            name: "copper_to_edge",
            what: "a 3V3 track 0.1 mm from the top edge",
            expect: &["drc.copper_to_edge"],
            build: |r, s| {
                route_all(r, s);
                track(r, s, "F.Cu", vec![json!("C2.1"), mm(14.55, 14.775), mm(12.0, 14.775)]);
            },
        },
        Case {
            name: "courtyard_overlap",
            what: "a third capacitor overlapping C2's courtyard",
            expect: &["drc.courtyard_overlap"],
            build: |r, s| {
                route_all(r, s);
                // 0402 courtyards are ±0.44 mm tall: 0.8 mm apart they overlap by 0.08 mm, and
                // the pads stay 0.23 mm apart.
                add_cap(r, s, "C3", mm(15.0, 10.8), ["3V3", "GND"]);
                track(r, s, "F.Cu", vec![json!("C3.1"), json!("C2.1")]);
                track(r, s, "F.Cu", vec![json!("C3.2"), json!("C2.2")]);
            },
        },
        Case {
            name: "silk_over_pad",
            what: "a silkscreen line across C2.1",
            expect: &["drc.silk_over_pad"],
            build: |r, s| {
                route_all(r, s);
                silk_line(s, (14.0, 10.0), (14.3, 10.0));
            },
        },
        Case {
            name: "unrouted",
            what: "the 3V3 tracks removed",
            expect: &["drc.unrouted"],
            build: |r, s| {
                route_all(r, s);
                let p = s.project.as_mut().unwrap();
                p.board_mut().tracks.retain(|t| t.net.as_deref() != Some("3V3"));
            },
        },
        Case {
            name: "keepout_tracks",
            what: "a keep-out over the 3V3 track (tracks forbidden)",
            expect: &["drc.keepout"],
            build: |r, s| {
                route_all(r, s);
                exec(
                    r,
                    s,
                    "keepout.add",
                    json!({"name": "ko", "layers": ["F.Cu"], "no_tracks": true,
                           "outline": {"rect": {"from": ["12.5mm", "8.8mm"], "to": ["13.5mm", "9.6mm"]}}}),
                );
            },
        },
        Case {
            name: "keepout_footprint",
            what: "a keep-out forbidding footprints over C1",
            expect: &["drc.keepout"],
            build: |r, s| {
                route_all(r, s);
                exec(
                    r,
                    s,
                    "keepout.add",
                    json!({"name": "nofp", "layers": ["F.Cu"], "no_footprints": true,
                           "outline": {"rect": {"from": ["4.8mm", "9mm"], "to": ["6mm", "11mm"]}}}),
                );
            },
        },
        Case {
            name: "netless_track",
            what: "a track without a net from a GND via to a new via (semantic difference)",
            expect: &["drc.short"],
            build: |r, s| {
                route_all(r, s);
                exec(r, s, "via.add", json!({"at": mm(3.5, 12.0), "net": "GND"}));
                track(r, s, "F.Cu", vec![mm(3.5, 10.0), mm(3.5, 12.0)]);
            },
        },
    ]
}

/// Builds a case on a fresh placed LDO board.
pub fn build(case: &Case) -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = ldo();
    (case.build)(&r, &mut s);
    (d, r, s)
}

pub fn project(s: &Session) -> &Project {
    s.project.as_ref().unwrap()
}

/// Writes the `.kicad_pcb`/`.kicad_pro`/`.kicad_dru` of the session's project into `dir`.
pub fn export_kicad(p: &Project, dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let e = cadlab::kicad_pcb::export(p, "board");
    assert!(e.warnings.is_empty(), "export warnings: {:?}", e.warnings);
    let pcb = dir.join("board.kicad_pcb");
    std::fs::write(&pcb, &e.pcb).unwrap();
    std::fs::write(dir.join("board.kicad_pro"), &e.project).unwrap();
    std::fs::write(dir.join("board.kicad_dru"), &e.rules).unwrap();
    pcb
}

/// Directory kept after the run (`$CARGO_TARGET_TMPDIR/<suite>/<case>`), emptied first.
pub fn keep_dir(suite: &str, name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(suite).join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
