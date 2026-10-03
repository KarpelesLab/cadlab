//! M4 placement: mounting holes, outline cutouts, automatic placement by groups and the
//! placement helpers (near, align, distribute).

mod common;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::Project;
use cadlab::{Diagnostic, Nm, Point};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd).error.diagnostic.code.to_string()
}

fn project(s: &Session) -> &Project {
    s.project.as_ref().unwrap()
}

fn drc(s: &Session) -> Vec<Diagnostic> {
    cadlab::drc::check(project(s))
}

/// DRC findings about placement (overlaps, outside the board, keep-outs, edges).
fn placement_errors(s: &Session) -> Vec<String> {
    const CODES: &[&str] = &[
        "drc.courtyard_overlap",
        "drc.footprint_outside",
        "drc.outside_board",
        "drc.copper_to_edge",
        "drc.keepout",
        "drc.clearance",
        "drc.short",
    ];
    drc(s).iter().filter(|d| CODES.contains(&d.code.as_ref())).map(|d| format!("{}: {}", d.code, d.message)).collect()
}

/// The ATtiny85 board of the schematic tests, with a 40 x 30 mm outline.
fn attiny() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    common::boards::build_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "40mm", "height": "30mm"}));
    (dir, r, s)
}

fn pad_center(p: &Project, refdes: &str, pad: &str) -> Point {
    cadlab::board::placed_pads(p).into_iter().find(|pp| pp.refdes == refdes && pp.number == pad).unwrap().center
}

fn dist_mm(a: Point, b: Point) -> f64 {
    let (dx, dy) = ((a.x.0 - b.x.0) as f64, (a.y.0 - b.y.0) as f64);
    (dx * dx + dy * dy).sqrt() / 1e6
}

fn ratsnest_mm(s: &Session) -> f64 {
    cadlab::board::ratsnest(project(s)).iter().map(|l| l.length.0 as f64 / 1e6).sum()
}

fn render(r: &Registry, s: &mut Session, name: &str) {
    if let Ok(dir) = std::env::var("CADLAB_PLACEMENT_RENDER") {
        let path = std::path::Path::new(&dir).join(name);
        exec(r, s, "render.board", json!({"path": path, "layers": ["F.Cu", "F.SilkS", "F.CrtYd", "Edge.Cuts"]}));
    }
}

#[test]
fn auto_groups_on_attiny_board() {
    let (_d, r, mut s) = attiny();
    let o = exec(&r, &mut s, "place.auto", json!({}));
    assert_eq!(o["output"]["placements"].as_object().unwrap().len(), 10, "{o}");
    assert!(o["output"].get("unplaced").is_none(), "{o}");
    render(&r, &mut s, "attiny_groups.png");
    let errors = placement_errors(&s);
    assert!(errors.is_empty(), "{errors:#?}");
    let groups = ratsnest_mm(&s);
    let p = project(&s);
    // Decoupling: a 3V3 capacitor right at the ATtiny's VCC pin, the input capacitor at the LDO.
    let vcc = pad_center(p, "U2", "8");
    let near_vcc = ["C2", "C3"].iter().map(|c| dist_mm(pad_center(p, c, "1"), vcc)).fold(f64::MAX, f64::min);
    assert!(near_vcc < 3.0, "3V3 capacitor {near_vcc:.2} mm from U2.VCC");
    let vin = pad_center(p, "U1", "1");
    let c1 = dist_mm(pad_center(p, "C1", "1"), vin);
    assert!(c1 < 3.0, "C1 {c1:.2} mm from U1.VIN");
    // Connectors near an edge.
    for j in ["J1", "J2"] {
        let at = p.board().footprints[j].at;
        let (x, y) = (at.x.0 as f64 / 1e6, at.y.0 as f64 / 1e6);
        let edge = x.min(40.0 - x).min(y).min(30.0 - y);
        assert!(edge < 6.0, "{j} at ({x}, {y}) is {edge} mm from the edge");
    }
    // Same result every time.
    let first = p.board().footprints.clone();

    // Compare with rows.
    let (_d2, r2, mut s2) = attiny();
    exec(&r2, &mut s2, "place.auto", json!({"strategy": "rows"}));
    render(&r2, &mut s2, "attiny_rows.png");
    let rows = ratsnest_mm(&s2);
    eprintln!("ratsnest: groups {groups:.1} mm, rows {rows:.1} mm; U2.VCC cap {near_vcc:.2} mm, C1 {c1:.2} mm");
    assert!(groups < rows * 0.8, "groups {groups:.1} mm vs rows {rows:.1} mm");

    let (_d3, r3, mut s3) = attiny();
    exec(&r3, &mut s3, "place.auto", json!({"strategy": "groups"}));
    assert_eq!(project(&s3).board().footprints, first, "deterministic");
}

#[test]
fn auto_respects_locked_cutouts_holes_and_keepouts() {
    let (_d, r, mut s) = attiny();
    exec(&r, &mut s, "place.set", json!({"refdes": "U2", "at": ["20mm", "15mm"], "rotation": 90}));
    exec(&r, &mut s, "place.lock", json!({"refdes": ["U2"]}));
    exec(&r, &mut s, "board.cutout", json!({"rect": {"from": ["5mm", "10mm"], "to": ["12mm", "20mm"]}}));
    exec(&r, &mut s, "board.cutout", json!({"circle": {"center": ["32mm", "8mm"], "diameter": "5mm"}}));
    for (x, y) in [(3.5, 3.5), (36.5, 3.5), (3.5, 26.5), (36.5, 26.5)] {
        exec(&r, &mut s, "board.hole", json!({"at": [format!("{x}mm"), format!("{y}mm")], "drill": "3.2mm"}));
    }
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "antenna", "outline": [["28mm", "20mm"], ["40mm", "20mm"], ["40mm", "30mm"], ["28mm", "30mm"]], "no_footprints": true}),
    );
    let o = exec(&r, &mut s, "place.auto", json!({"replace": true}));
    assert!(o["output"]["placements"].get("U2").is_none(), "locked part not moved");
    assert_eq!(o["output"]["placements"].as_object().unwrap().len(), 9, "{o}");
    render(&r, &mut s, "attiny_obstacles.png");
    let p = project(&s);
    assert_eq!(p.board().footprints["U2"].at, Point::new(Nm(20_000_000), Nm(15_000_000)));
    let errors = placement_errors(&s);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn holes_round_trip_and_reach_every_output() {
    let (d, r, mut s) = attiny();
    let o = exec(&r, &mut s, "board.hole", json!({"at": ["3.5mm", "3.5mm"], "drill": "3.2mm"}));
    assert_eq!(o["output"]["holes"][0]["name"], "H1");
    let o = exec(
        &r,
        &mut s,
        "board.hole",
        json!({"at": ["36.5mm", "3.5mm"], "drill": "3.2mm", "pad": "6mm", "net": "GND"}),
    );
    assert_eq!(o["output"]["holes"][1]["name"], "H2");
    assert_eq!(o["output"]["holes"][1]["net"], "GND");
    assert_eq!(
        fail(&r, &mut s, "board.hole", json!({"at": ["5mm", "5mm"], "drill": "3mm", "net": "GND"})),
        "board.invalid_hole"
    );
    assert_eq!(
        fail(&r, &mut s, "board.hole", json!({"at": ["5mm", "5mm"], "drill": "3mm", "pad": "2mm"})),
        "board.invalid_hole"
    );
    assert_eq!(
        fail(&r, &mut s, "board.hole", json!({"at": ["5mm", "5mm"], "drill": "3mm", "name": "U1"})),
        "board.duplicate_hole"
    );
    exec(&r, &mut s, "board.cutout", json!({"polygon": [["15mm", "12mm"], ["25mm", "12mm"], ["20mm", "18mm"]]}));
    exec(&r, &mut s, "place.auto", json!({}));
    assert!(placement_errors(&s).is_empty(), "{:#?}", placement_errors(&s));
    let info = exec(&r, &mut s, "board.info", json!({}));
    assert_eq!(info["output"]["holes"], 2);
    assert_eq!(info["output"]["cutouts"], 1);

    // Saved and loaded back.
    exec(&r, &mut s, "project.save", json!({}));
    let loaded = Project::load(&d.path().join("p")).unwrap();
    assert_eq!(loaded.board().holes, project(&s).board().holes);
    assert_eq!(loaded.board().outline, project(&s).board().outline);

    // DRC: the plated hole is GND copper (unrouted), a hole under a courtyard is reported.
    let d = drc(&s);
    assert!(d.iter().any(|x| x.code == "drc.unrouted" && x.message.contains("H2.1")), "{d:#?}");
    exec(&r, &mut s, "place.set", json!({"refdes": "U2", "at": ["4mm", "4mm"]}));
    let d = drc(&s);
    assert!(d.iter().any(|x| x.code == "drc.courtyard_overlap" && x.message.contains("hole H1")), "{d:#?}");
    exec(&r, &mut s, "place.remove", json!({"refdes": ["U2"]}));
    exec(&r, &mut s, "board.hole", json!({"at": ["50mm", "5mm"], "drill": "2mm", "name": "OUT"}));
    let d = drc(&s);
    assert!(d.iter().any(|x| x.code == "drc.outside_board" && x.message.contains("hole OUT")), "{d:#?}");
    exec(&r, &mut s, "board.hole_remove", json!({"holes": ["OUT"]}));

    // Drill file: both holes, mechanical.
    let p = project(&s);
    let holes: Vec<_> = cadlab::fabout::holes(p)
        .into_iter()
        .filter(|h| h.pad.as_ref().is_some_and(|(r, _)| r.starts_with('H')))
        .collect();
    assert_eq!(holes.len(), 2);
    assert!(holes.iter().all(|h| h.kind == cadlab::fabout::HoleKind::Mechanical));
    assert_eq!(holes.iter().filter(|h| h.plated).count(), 1);

    // Gerbers: the plated pad on copper, the cutout on the profile.
    let files = cadlab::fabout::gerbers(p, &Default::default());
    let text = |pred: &dyn Fn(&str) -> bool| {
        files.iter().filter(|f| pred(&f.name)).map(|f| f.content.clone()).collect::<Vec<_>>().join("\n")
    };
    let top = text(&|n| n.contains("F_Cu") || n.ends_with(".GTL"));
    assert!(top.contains("H2"), "plated hole on top copper");
    let profile = text(&|n| n.contains("Edge") || n.ends_with(".GKO") || n.ends_with(".GM1"));
    assert!(
        profile.contains("X15000000Y12000000") || profile.contains("X150000Y120000") || profile.contains("15000000"),
        "{profile}"
    );

    // Rendering and KiCad export.
    let o = exec(&r, &mut s, "render.board", json!({"path": d_path(&d, "b.svg")}));
    let svg = std::fs::read_to_string(o["output"]["path"].as_str().unwrap()).unwrap_or_default();
    assert!(svg.contains("<circle"), "holes drawn");
    let pcb = cadlab::kicad_pcb::to_kicad_pcb(project(&s));
    assert!(pcb.contains("\"cadlab:MountingHole\""), "hole footprints");
    assert!(pcb.contains("np_thru_hole circle") && pcb.contains("thru_hole circle"));
    assert!(pcb.contains("(net ") && pcb.contains("\"GND\""));
    assert_eq!(pcb.matches("(layer \"Edge.Cuts\")").count(), 4 + 3, "outline + triangle cutout");

    // Removal.
    let o = exec(&r, &mut s, "board.hole_remove", json!({"holes": ["H1", "h2"]}));
    assert_eq!(o["output"]["holes"], json!([]));
    assert_eq!(fail(&r, &mut s, "board.hole_remove", json!({"holes": ["H1"]})), "board.hole_not_found");
}

fn d_path(_d: &[Diagnostic], name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("cadlab-placement-{}-{name}", std::process::id()))
}

#[test]
fn cutouts_validate_and_remove() {
    let (_d, r, mut s) = attiny();
    let o = exec(&r, &mut s, "board.cutout", json!({"rect": {"from": ["10mm", "10mm"], "to": ["15mm", "12mm"]}}));
    assert_eq!(o["output"]["cutouts"][0]["index"], 1);
    assert_eq!(o["output"]["cutouts"][0]["min"], json!(["10mm", "10mm"]));
    exec(&r, &mut s, "board.cutout", json!({"circle": {"center": ["30mm", "15mm"], "diameter": "4mm"}}));
    assert_eq!(
        fail(&r, &mut s, "board.cutout", json!({"rect": {"from": ["35mm", "5mm"], "to": ["45mm", "8mm"]}})),
        "board.invalid_cutout"
    );
    assert_eq!(
        fail(&r, &mut s, "board.cutout", json!({"rect": {"from": ["11mm", "11mm"], "to": ["13mm", "14mm"]}})),
        "board.invalid_cutout"
    );
    assert_eq!(fail(&r, &mut s, "board.cutout", json!({})), "board.invalid_cutout");
    // Zone fill keeps out of the cutouts, DRC flags copper in them.
    exec(&r, &mut s, "zone.add", json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    exec(&r, &mut s, "zone.fill", json!({}));
    exec(&r, &mut s, "via.add", json!({"at": ["12mm", "11mm"], "net": "GND"}));
    let d = drc(&s);
    assert!(d.iter().any(|x| x.code == "drc.outside_board" && x.message.contains("via#")), "{d:#?}");
    let o = exec(&r, &mut s, "board.cutout_remove", json!({"index": 1}));
    assert_eq!(o["output"]["cutouts"].as_array().unwrap().len(), 1);
    assert_eq!(o["output"]["cutouts"][0]["index"], 1);
    assert_eq!(fail(&r, &mut s, "board.cutout_remove", json!({"index": 2})), "board.cutout_not_found");
}

#[test]
fn near_align_distribute() {
    let (_d, r, mut s) = attiny();
    exec(&r, &mut s, "place.set", json!({"refdes": "U2", "at": ["20mm", "15mm"]}));
    // A decoupling capacitor next to VCC.
    let o = exec(&r, &mut s, "place.near", json!({"refdes": "C3", "target": "U2.VCC"}));
    assert!(o["output"]["placements"]["C3"].is_object());
    let p = project(&s);
    let d = dist_mm(pad_center(p, "C3", "1"), pad_center(p, "U2", "8"));
    assert!(d < 2.5, "C3.1 {d:.2} mm from U2.VCC");
    assert!(placement_errors(&s).is_empty(), "{:#?}", placement_errors(&s));
    // Next to it on a given side, then next to a part.
    exec(&r, &mut s, "place.near", json!({"refdes": "C2", "target": "U2.VCC", "side": "above", "distance": "0.5mm"}));
    let p = project(&s);
    let u2 = cadlab::board::place::courtyard_box(p, "U2").unwrap();
    let c2 = cadlab::board::place::courtyard_box(p, "C2").unwrap();
    assert!(c2.y0 >= u2.y1 + 500_000, "C2 above U2 with 0.5 mm: {c2:?} vs {u2:?}");
    exec(&r, &mut s, "place.near", json!({"refdes": "R1", "target": "U2"}));
    assert!(placement_errors(&s).is_empty(), "{:#?}", placement_errors(&s));
    assert_eq!(fail(&r, &mut s, "place.near", json!({"refdes": "R2", "target": "J1.1"})), "place.not_placed");
    assert_eq!(fail(&r, &mut s, "place.near", json!({"refdes": "U2", "target": "U2"})), "place.invalid_target");

    // Align and distribute.
    for (c, x, y) in [("R1", 5.0, 5.0), ("R2", 9.0, 6.0), ("C1", 7.0, 7.0), ("D1", 15.0, 4.0)] {
        exec(&r, &mut s, "place.set", json!({"refdes": c, "at": [format!("{x}mm"), format!("{y}mm")]}));
    }
    let o = exec(&r, &mut s, "place.align", json!({"refdes": ["R1", "R2", "C1", "D1"], "axis": "y"}));
    for c in ["R1", "R2", "C1", "D1"] {
        assert_eq!(o["output"]["placements"][c]["at"][1], "5mm", "{o}");
    }
    let o = exec(&r, &mut s, "place.align", json!({"refdes": ["R1", "D1"], "axis": "y", "to": "8mm"}));
    assert_eq!(o["output"]["placements"]["D1"]["at"][1], "8mm");
    exec(&r, &mut s, "place.align", json!({"refdes": ["R1", "R2", "C1", "D1"], "axis": "y", "to": "center"}));
    assert_eq!(
        fail(&r, &mut s, "place.align", json!({"refdes": ["R1"], "axis": "x", "to": "middle"})),
        "place.invalid_align"
    );

    let o = exec(&r, &mut s, "place.distribute", json!({"refdes": ["R1", "R2", "C1", "D1"], "axis": "x"}));
    let p = project(&s);
    let boxes: Vec<_> =
        ["R1", "C1", "R2", "D1"].iter().map(|c| cadlab::board::place::courtyard_box(p, c).unwrap()).collect();
    let gaps: Vec<i64> = boxes.windows(2).map(|w| w[1].x0 - w[0].x1).collect();
    assert!(gaps.iter().all(|g| (g - gaps[0]).abs() <= 2), "equal gaps {gaps:?}: {o}");
    assert_eq!(p.board().footprints["R1"].at.x, Nm(5_000_000), "first stays");
    assert_eq!(p.board().footprints["D1"].at.x, Nm(15_000_000), "last stays");
    exec(&r, &mut s, "place.distribute", json!({"refdes": ["R1", "R2", "C1", "D1"], "axis": "x", "spacing": "1mm"}));
    let p = project(&s);
    let boxes: Vec<_> =
        ["R1", "C1", "R2", "D1"].iter().map(|c| cadlab::board::place::courtyard_box(p, c).unwrap()).collect();
    assert!(boxes.windows(2).all(|w| w[1].x0 - w[0].x1 == 1_000_000), "{boxes:?}");
    exec(&r, &mut s, "place.lock", json!({"refdes": ["D1"]}));
    assert_eq!(fail(&r, &mut s, "place.align", json!({"refdes": ["R1", "D1"], "axis": "x"})), "place.locked");
}
