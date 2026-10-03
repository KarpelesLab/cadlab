//! M4 DRC: a small routed board that is clean, then deliberate violations of every rule.

use std::collections::BTreeSet;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::board::{BoardGraphic, GraphicKind, Keepout, Track, Via};
use cadlab::{Diagnostic, Nm, ObjectRef, Point, Severity};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn mm(x: f64, y: f64) -> Point {
    Point::new(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64))
}

/// The LDO + caps circuit of `tests/board.rs`.
fn setup() -> (tempfile::TempDir, Registry, Session) {
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
    (dir, r, s)
}

/// The circuit placed on a 20 x 15 mm board and fully routed (GND through the bottom layer).
fn routed() -> (tempfile::TempDir, Registry, Session) {
    let (d, r, mut s) = setup();
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
    // VIN around U1.2 on the left, then to C1.1.
    track(&r, &mut s, "F.Cu", json!(["U1.1", ["7.5mm", "8.45mm"], ["7.5mm", "6.55mm"], "U1.3"]), None);
    track(&r, &mut s, "F.Cu", json!(["C1.1", ["5mm", "6.55mm"], ["7.5mm", "6.55mm"]]), Some("VIN"));
    // 3V3.
    track(&r, &mut s, "F.Cu", json!(["U1.5", ["14mm", "8.45mm"], ["14mm", "7.0475mm"], "C2.1"]), None);
    // GND: via in U1.2, bottom layer to vias next to C1.2 and C2.2.
    for at in [["8.8475mm", "7.5mm"], ["5mm", "8.8mm"], ["16.5mm", "7.5mm"]] {
        exec(&r, &mut s, "via.add", json!({"at": at, "net": "GND"}));
    }
    track(&r, &mut s, "B.Cu", json!([["5mm", "8.8mm"], ["8.8475mm", "7.5mm"], ["16.5mm", "7.5mm"]]), Some("GND"));
    track(&r, &mut s, "F.Cu", json!(["C1.2", ["5mm", "8.8mm"]]), None);
    track(&r, &mut s, "F.Cu", json!([["16.5mm", "7.5mm"], ["16.5mm", "7.9525mm"], "C2.2"]), Some("GND"));
    (d, r, s)
}

fn drc(s: &Session) -> Vec<Diagnostic> {
    cadlab::drc::check(s.project.as_ref().unwrap())
}

fn codes(d: &[Diagnostic]) -> BTreeSet<String> {
    d.iter().map(|d| d.code.to_string()).collect()
}

fn find<'a>(d: &'a [Diagnostic], code: &str, subject: &str) -> &'a Diagnostic {
    let want = ObjectRef::parse(subject).unwrap();
    d.iter()
        .find(|x| x.code == code && x.subjects.contains(&want))
        .unwrap_or_else(|| panic!("no {code} about {subject} in:\n{}", text(d)))
}

fn text(d: &[Diagnostic]) -> String {
    d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n")
}

fn project(s: &mut Session) -> &mut cadlab::model::Project {
    s.project.as_mut().unwrap()
}

fn add_track(s: &mut Session, layer: &str, a: Point, b: Point, width: f64, net: Option<&str>) -> u64 {
    let p = project(s);
    let id = p.alloc_id();
    p.board_mut().tracks.push(Track {
        id,
        layer: layer.into(),
        width: Nm((width * 1e6) as i64),
        net: net.map(Into::into),
        start: a,
        end: b,
        mid: None,
        locked: false,
    });
    id.0
}

fn add_via(s: &mut Session, at: Point, drill: f64, diameter: f64, net: Option<&str>) -> u64 {
    let p = project(s);
    let id = p.alloc_id();
    p.board_mut().vias.push(Via {
        id,
        at,
        drill: Nm((drill * 1e6) as i64),
        diameter: Nm((diameter * 1e6) as i64),
        net: net.map(Into::into),
        from: "F.Cu".into(),
        to: "B.Cu".into(),
        locked: false,
    });
    id.0
}

#[test]
fn routed_board_is_clean() {
    let (_d, r, mut s) = routed();
    let d = drc(&s);
    assert!(d.is_empty(), "expected a clean board:\n{}", text(&d));
    let o = r.execute(&mut s, "drc.run", json!({}), RunOptions::default()).unwrap();
    let v = serde_json::to_value(&o).unwrap();
    assert_eq!(v["output"], json!({"errors": 0, "warnings": 0}));
}

#[test]
fn unrouted_unplaced_and_no_outline() {
    let (_d, r, mut s) = setup();
    let d = drc(&s);
    assert_eq!(codes(&d), BTreeSet::from(["drc.no_outline".to_string(), "drc.unplaced".to_string()]));
    let u = find(&d, "drc.unplaced", "C2");
    assert_eq!(u.severity, Severity::Warning);
    let o = r.execute(&mut s, "drc.run", json!({}), RunOptions::default()).unwrap();
    assert!(o.diagnostics.iter().any(|d| d.code == "drc.no_outline" && d.severity == Severity::Error));

    // Placed but not routed: one error per missing connection, with both ends.
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.auto", json!({}));
    let d = drc(&s);
    let unrouted: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.unrouted").collect();
    assert_eq!(unrouted.len(), 5, "{}", text(&d));
    let gnd = find(&d, "drc.unrouted", "net:3V3");
    assert!(gnd.message.contains("U1.5") && gnd.message.contains("C2.1"), "{}", gnd.message);
    assert_eq!(gnd.severity, Severity::Error);
    assert!(gnd.location.is_some() && gnd.hint.is_some());
}

#[test]
fn copper_rules() {
    let (_d, _r, mut s) = routed();
    // 3V3 track 0.1 mm from the VIN track at x = 7.5 mm.
    let near = add_track(&mut s, "F.Cu", mm(7.15, 7.0), mm(7.15, 8.0), 0.25, Some("3V3"));
    // Crossing tracks of two nets, and copper without a net touching a net.
    let gnd = add_track(&mut s, "F.Cu", mm(2.0, 2.0), mm(4.0, 2.0), 0.25, Some("GND"));
    let vin = add_track(&mut s, "F.Cu", mm(3.0, 1.0), mm(3.0, 3.0), 0.25, Some("VIN"));
    let orphan = add_track(&mut s, "F.Cu", mm(16.0, 12.0), mm(18.0, 12.0), 0.25, None);
    let vin2 = add_track(&mut s, "F.Cu", mm(17.0, 11.0), mm(17.0, 13.0), 0.25, Some("VIN"));
    // The same crossing on different layers is fine.
    add_track(&mut s, "B.Cu", mm(12.0, 2.0), mm(14.0, 2.0), 0.25, Some("GND"));
    add_track(&mut s, "F.Cu", mm(13.0, 1.0), mm(13.0, 3.0), 0.25, Some("VIN"));
    // Too thin; too close to the edge; outside.
    let thin = add_track(&mut s, "B.Cu", mm(2.0, 5.0), mm(4.0, 5.0), 0.1, Some("GND"));
    let edge = add_track(&mut s, "B.Cu", mm(0.2, 4.0), mm(0.2, 5.0), 0.25, Some("GND"));
    let out = add_via(&mut s, mm(-1.0, 5.0), 0.3, 0.6, Some("GND"));
    // Via sizes; two holes 0.4 mm apart edge to edge.
    let small_drill = add_via(&mut s, mm(2.0, 10.0), 0.2, 0.6, Some("GND"));
    let thin_ring = add_via(&mut s, mm(4.0, 10.0), 0.3, 0.45, Some("GND"));
    let h1 = add_via(&mut s, mm(2.0, 13.0), 0.3, 0.6, Some("GND"));
    let h2 = add_via(&mut s, mm(2.7, 13.0), 0.3, 0.6, Some("GND"));

    let d = drc(&s);
    let t = |id: u64| format!("track#{id}");
    let v = |id: u64| format!("via#{id}");
    let c = find(&d, "drc.clearance", &t(near));
    assert_eq!(c.severity, Severity::Error);
    assert!(c.message.contains("0.1mm apart") && c.message.contains("clearance is 0.2mm"), "{}", c.message);
    assert!(c.subjects.contains(&ObjectRef::Net("VIN".into())));
    let sh = find(&d, "drc.short", &t(gnd));
    assert!(sh.subjects.contains(&ObjectRef::parse(&t(vin)).unwrap()));
    let loc = sh.location.unwrap();
    assert!((loc.x.0 - 3_000_000).abs() < 200_000 && (loc.y.0 - 2_000_000).abs() < 200_000, "{loc:?}");
    let sh = find(&d, "drc.short", &t(orphan));
    assert!(sh.message.contains("has no net"), "{}", sh.message);
    assert!(sh.subjects.contains(&ObjectRef::parse(&t(vin2)).unwrap()));
    assert_eq!(d.iter().filter(|x| x.code == "drc.short").count(), 2, "{}", text(&d));
    find(&d, "drc.track_width", &t(thin));
    let e = find(&d, "drc.copper_to_edge", &t(edge));
    assert!(e.message.contains("0.075mm"), "{}", e.message);
    find(&d, "drc.outside_board", &v(out));
    find(&d, "drc.via_drill", &v(small_drill));
    find(&d, "drc.via_annular_ring", &v(thin_ring));
    let hh = find(&d, "drc.hole_to_hole", &v(h1));
    assert!(hh.subjects.contains(&ObjectRef::parse(&v(h2)).unwrap()));
    assert!(hh.message.contains("0.4mm apart"), "{}", hh.message);
    // The isolated GND vias are also unrouted.
    find(&d, "drc.unrouted", &v(h1));

    // Ordering is by code, then location.
    let keys: Vec<(String, Option<(Nm, Nm)>)> =
        d.iter().map(|x| (x.code.to_string(), x.location.map(|l| (l.x, l.y)))).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(drc(&s), d, "deterministic");
}

#[test]
fn net_class_rules() {
    let (_d, r, mut s) = routed();
    // A wider class clearance applies to its nets, and a class width flags narrower tracks.
    exec(&r, &mut s, "netclass.set", json!({"name": "hv", "clearance": "0.6mm", "track_width": "0.3mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["3V3"], "class": "hv"}));
    let d = drc(&s);
    // Pads of one footprint (U1.5 next to U1.4) are not checked against each other, but the
    // 3V3 track at x = 14 mm is 0.59 mm from C2.2 (GND).
    let c = d.iter().find(|x| x.code == "drc.clearance").unwrap_or_else(|| panic!("{}", text(&d)));
    assert!(c.message.contains("clearance is 0.6mm"), "{}", c.message);
    assert!(c.subjects.contains(&ObjectRef::parse("C2.2").unwrap()), "{}", c);
    let w = d.iter().filter(|x| x.code == "drc.track_width_class").collect::<Vec<_>>();
    assert_eq!(w.len(), 3, "the three 3V3 segments are 0.25 mm: {}", text(&d));
    assert!(w.iter().all(|x| x.severity == Severity::Warning));
}

#[test]
fn placement_rules() {
    let (_d, r, mut s) = routed();
    // Two caps with overlapping courtyards on top, a third under them on the bottom.
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 5}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C3", "at": ["3mm", "12mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C4", "at": ["3.8mm", "12mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C5", "at": ["3.4mm", "12mm"], "rotation": 90, "side": "bottom"}));
    // Partly off the board.
    exec(&r, &mut s, "place.set", json!({"refdes": "C6", "at": ["19.9mm", "2mm"]}));
    let d = drc(&s);
    let ov: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.courtyard_overlap").collect();
    assert_eq!(ov.len(), 1, "{}", text(&d));
    assert!(ov[0].subjects.contains(&ObjectRef::Name("C3".into())));
    assert!(ov[0].subjects.contains(&ObjectRef::Name("C4".into())));
    find(&d, "drc.footprint_outside", "C6");
    find(&d, "drc.outside_board", "C6.2");
    find(&d, "drc.unplaced", "C7");
    assert!(!codes(&d).contains("drc.short"), "{}", text(&d));
}

#[test]
fn silk_keepout_and_through_hole_rules() {
    let (_d, r, mut s) = routed();
    // A header footprint with a 0.25 mm hole in a 0.4 mm pad.
    exec(
        &r,
        &mut s,
        "footprint.generate",
        json!({"name": "HDR_BAD",
        "spec": {"family": "pin_header", "rows": 1, "pins_per_row": 2, "drill": "0.25mm", "pad": "0.4mm"}}),
    );
    exec(&r, &mut s, "part.create", json!({"id": "HDR2", "category": "connector", "package": "PinHeader 1x02"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "HDR2"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["17mm", "3mm"]}));
    project(&mut s).board_mut().footprints.get_mut("J1").unwrap().footprint = Some("HDR_BAD".into());

    // Silkscreen line across C2.1.
    let p = project(&mut s);
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "F.SilkS".into(),
        kind: GraphicKind::Line { points: vec![mm(14.5, 7.05), mm(15.5, 7.05)], width: Nm::from_um(150) },
    });
    let silk_id = id.0;

    // A keep-out forbidding everything, with a track, a via and a footprint inside.
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "refdes": "C7"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C7", "at": ["12mm", "12.5mm"]}));
    let p = project(&mut s);
    let id = p.alloc_id();
    p.board_mut().keepouts.push(Keepout {
        id,
        name: "antenna".into(),
        layers: vec![],
        outline: vec![mm(9.0, 11.5), mm(13.0, 11.5), mm(13.0, 14.5), mm(9.0, 14.5)],
        no_tracks: true,
        no_vias: true,
        no_pours: true,
        no_footprints: true,
    });
    let kt = add_track(&mut s, "B.Cu", mm(9.5, 14.2), mm(12.5, 14.2), 0.25, None);
    let kv = add_via(&mut s, mm(9.6, 12.3), 0.3, 0.6, None);
    // A keep-out limited to the bottom layer does not affect top tracks.
    let p = project(&mut s);
    let id = p.alloc_id();
    p.board_mut().keepouts.push(Keepout {
        id,
        name: "bottom".into(),
        layers: vec!["B.Cu".into()],
        outline: vec![mm(18.0, 9.0), mm(19.5, 9.0), mm(19.5, 11.0), mm(18.0, 11.0)],
        no_tracks: true,
        no_vias: false,
        no_pours: false,
        no_footprints: false,
    });
    add_track(&mut s, "F.Cu", mm(18.5, 10.0), mm(19.0, 10.0), 0.25, None);

    let d = drc(&s);
    let pd = find(&d, "drc.pad_drill", "J1.1");
    assert!(pd.message.contains("0.25mm"), "{}", pd.message);
    find(&d, "drc.pad_annular_ring", "J1.2");
    let silk = find(&d, "drc.silk_over_pad", &format!("graphic#{silk_id}"));
    assert_eq!(silk.severity, Severity::Warning);
    assert!(silk.subjects.contains(&ObjectRef::parse("C2.1").unwrap()) && silk.message.contains("overlaps"));
    let k: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.keepout").collect();
    assert_eq!(k.len(), 3, "{}", text(&d));
    find(&d, "drc.keepout", &format!("track#{kt}"));
    find(&d, "drc.keepout", &format!("via#{kv}"));
    let c7 = find(&d, "drc.keepout", "C7");
    assert!(c7.subjects.contains(&ObjectRef::Named { kind: "keepout".into(), name: "antenna".into() }));
}

/// A few thousand tracks: the spatial index keeps the check fast. Timing is asserted in release
/// builds only (`cargo test --release --test drc`).
#[test]
fn many_tracks_performance() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "board.outline", json!({"width": "100mm", "height": "100mm"}));
    let mut n = 0;
    for row in 0..50 {
        for col in 0..30 {
            let (x, y) = (2.0 + col as f64 * 3.2, 2.0 + row as f64 * 1.9);
            let net = format!("N{}", row % 7);
            add_track(&mut s, "F.Cu", mm(x, y), mm(x + 2.9, y), 0.2, Some(&net));
            let net = format!("N{}", col % 5);
            add_track(&mut s, "B.Cu", mm(y, x), mm(y, x + 2.9), 0.2, Some(&net));
            n += 2;
        }
    }
    // A sprinkling of vias, some of them too close to tracks of other nets.
    for i in 0..200 {
        add_via(&mut s, mm(3.0 + (i % 20) as f64 * 4.7, 3.0 + (i / 20) as f64 * 9.1), 0.3, 0.6, Some("N1"));
    }
    let t0 = std::time::Instant::now();
    let d = drc(&s);
    let dt = t0.elapsed();
    eprintln!("DRC of {n} tracks and 200 vias: {} findings in {dt:?}", d.len());
    assert!(d.iter().any(|x| x.code == "drc.short"));
    assert!(d.iter().any(|x| x.code == "drc.unplaced"));
    #[cfg(not(debug_assertions))]
    assert!(dt < std::time::Duration::from_secs(1), "DRC took {dt:?}");
}

fn diag_codes(o: &Value) -> Vec<String> {
    o["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

#[test]
fn rule_presets_change_what_drc_accepts() {
    let (_d, r, mut s) = routed();
    // IPC class 3: 0.25 mm annular ring; the three 0.6/0.3 mm vias (0.15 mm ring) now fail.
    let o = exec(&r, &mut s, "board.rules", json!({"preset": "ipc3"}));
    assert_eq!(o["output"]["ipc_class"], 3);
    assert_eq!(o["output"]["min_annular_ring"], "0.25mm");
    assert_eq!(o["output"]["via_diameter"], "0.8mm");
    let changed: Vec<&str> =
        o["output"]["changed"].as_array().unwrap().iter().map(|c| c["field"].as_str().unwrap()).collect();
    assert_eq!(changed, ["via_diameter", "min_annular_ring"]);
    let d = drc(&s);
    let rings: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.via_annular_ring").collect();
    assert_eq!(rings.len(), 3, "{}", text(&d));
    assert!(rings.iter().all(|x| x.severity == Severity::Error && x.message.contains("minimum 0.25mm")));
    // A new via takes the class 3 default size and passes.
    exec(&r, &mut s, "via.add", json!({"at": ["2mm", "13mm"], "net": "GND"}));
    let d = drc(&s);
    assert_eq!(d.iter().filter(|x| x.code == "drc.via_annular_ring").count(), 3, "{}", text(&d));
    exec(&r, &mut s, "history.undo", json!({}));
    // Back to class 2: clean again.
    exec(&r, &mut s, "board.rules", json!({"preset": "ipc2"}));
    assert!(drc(&s).is_empty(), "{}", text(&drc(&s)));
    // A preset and explicit values: the explicit ones win.
    let o = exec(&r, &mut s, "board.rules", json!({"preset": "ipc3", "min_annular_ring": "0.15mm"}));
    assert_eq!(o["output"]["min_annular_ring"], "0.15mm");
    assert!(drc(&s).is_empty(), "{}", text(&drc(&s)));
}

#[test]
fn rules_derived_from_a_fab_profile() {
    let (_d, r, mut s) = routed();
    // Preview with dry run: nothing changes.
    let before = project(&mut s).board().rules.clone();
    let o = r
        .execute(&mut s, "board.rules", json!({"fab": "jlcpcb", "margin": "tightest"}), RunOptions { dry_run: true })
        .unwrap();
    let v = serde_json::to_value(&o).unwrap();
    assert_eq!(v["output"]["min_track_width"], "0.1mm");
    assert_eq!(project(&mut s).board().rules, before);

    let o = exec(&r, &mut s, "board.rules", json!({"fab": "jlcpcb", "margin": "tightest"}));
    let out = &o["output"];
    assert_eq!(out["derived_from"], "jlcpcb two-layer");
    for (k, v) in [
        ("min_track_width", "0.1mm"),
        ("clearance", "0.1mm"),
        ("track_width", "0.1mm"),
        ("min_drill", "0.15mm"),
        ("min_annular_ring", "0.05mm"),
        ("via_drill", "0.15mm"),
        ("via_diameter", "0.25mm"),
        ("hole_to_hole", "0.2mm"),
        ("copper_to_edge", "0.2mm"),
    ] {
        assert_eq!(out[k], v, "{k}");
    }
    let track = out["derived"].as_array().unwrap().iter().find(|d| d["field"] == "min_track_width").unwrap().clone();
    assert_eq!(track, json!({"field": "min_track_width", "value": "0.1mm", "from": "min_track", "limit": "0.1mm"}));
    assert!(drc(&s).is_empty(), "the board is wider than the limits: {}", text(&drc(&s)));

    // Comfortable (default): minimums +25 % rounded up to 10 um, default track/via not below class 2.
    let o = exec(&r, &mut s, "board.rules", json!({"fab": "jlcpcb"}));
    let out = &o["output"];
    for (k, v) in [
        ("min_track_width", "0.13mm"),
        ("clearance", "0.13mm"),
        ("track_width", "0.25mm"),
        ("min_drill", "0.19mm"),
        ("min_annular_ring", "0.07mm"),
        ("via_drill", "0.3mm"),
        ("via_diameter", "0.6mm"),
        ("hole_to_hole", "0.25mm"),
    ] {
        assert_eq!(out[k], v, "{k}");
    }
    // Explicit values win over derived ones.
    let o = exec(&r, &mut s, "board.rules", json!({"fab": "pcbway", "clearance": "0.2mm"}));
    assert_eq!(o["output"]["clearance"], "0.2mm");
    assert_eq!(o["output"]["derived_from"].as_str().unwrap().split(' ').next(), Some("pcbway"));

    // Only numbers are stored: the rules have no reference to the fab.
    let rules = serde_json::to_value(&project(&mut s).board().rules).unwrap();
    let keys: Vec<&str> = rules.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want: Vec<&str> = cadlab::model::board::RULE_FIELDS.to_vec();
    want.push("ipc_class");
    want.sort();
    let mut keys = keys;
    keys.sort();
    assert_eq!(keys, want);

    let f = r.execute(&mut s, "board.rules", json!({"margin": "tightest"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "board.invalid_rule");
    let f = r.execute(&mut s, "board.rules", json!({"fab": "nofab"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "fab.unknown");
    let f = r
        .execute(&mut s, "board.rules", json!({"fab": "jlcpcb", "process": "nope"}), RunOptions::default())
        .unwrap_err();
    assert_eq!(f.error.diagnostic.code, "fab.unknown_process");
}

#[test]
fn net_class_view_and_per_class_via_rules() {
    let (_d, r, mut s) = routed();
    exec(&r, &mut s, "netclass.set", json!({"name": "gnd", "via_drill": "0.4mm", "via_diameter": "0.8mm"}));
    exec(&r, &mut s, "net.set", json!({"nets": ["GND"], "class": "gnd"}));
    let o = exec(&r, &mut s, "netclass.show", json!({"name": "gnd"}));
    let e = &o["output"]["effective"];
    assert_eq!(e["via_diameter"], "0.8mm");
    assert_eq!(e["track_width"], "0.25mm");
    assert_eq!(e["inherited"], json!(["track_width", "clearance"]));
    assert_eq!(o["output"]["nets"], json!(["GND"]));
    // The existing 0.6/0.3 mm GND vias are smaller than the class asks: warnings, not errors.
    let d = drc(&s);
    let w: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.via_size_class").collect();
    assert_eq!(w.len(), 3, "{}", text(&d));
    assert!(w.iter().all(|x| x.severity == Severity::Warning && x.message.contains("asks for 0.8mm/0.4mm")));
    // Board rules changes show through inherited values.
    exec(&r, &mut s, "board.rules", json!({"track_width": "0.3mm"}));
    let o = exec(&r, &mut s, "netclass.list", json!({}));
    assert_eq!(o["output"]["classes"][0]["effective"]["track_width"], "0.3mm");

    // A class below the board minimums is reported when set, and by the DRC.
    let o = exec(&r, &mut s, "netclass.set", json!({"name": "thin", "track_width": "0.1mm", "via_diameter": "0.5mm"}));
    assert_eq!(diag_codes(&o), ["drc.netclass_rule", "drc.netclass_rule"]);
    let d = drc(&s);
    let n: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.netclass_rule").collect();
    assert_eq!(n.len(), 2, "{}", text(&d));
    assert!(n[0].subjects.contains(&ObjectRef::Name("thin".into())) && n[0].hint.is_some());
    // Lowering the board minimums (as a fab derivation would) clears them.
    let o = exec(&r, &mut s, "board.rules", json!({"fab": "jlcpcb", "margin": "tightest"}));
    assert!(diag_codes(&o).is_empty(), "{o}");
    assert!(!codes(&drc(&s)).contains("drc.netclass_rule"));
    let f = r.execute(&mut s, "netclass.show", json!({"name": "gn"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "netclass.not_found");
}
