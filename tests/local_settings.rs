//! Local pad and footprint settings, net ties, pads on the back, copper drawings, slots and
//! custom rules (DECISIONS D40): through the commands, the DRC, zone fills and the outputs, and
//! back through the KiCad export and import.

use cadlab::board::{self, ItemRef};
use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::board::{BoardGraphic, GraphicKind, PadConnection, Track};
use cadlab::model::footprint::{Graphic, GraphicGeometry, GraphicLayer, PadKind, Paste};
use cadlab::{Diagnostic, Nm, ObjectRef, Point};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

fn fails(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> String {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(_) => panic!("{cmd} should fail"),
        Err(f) => f.error.diagnostic.code.to_string(),
    }
}

fn mm(x: f64, y: f64) -> Point {
    Point::new(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64))
}

fn project(s: &mut Session) -> &mut cadlab::model::Project {
    s.project.as_mut().unwrap()
}

/// An LDO and two 0402 capacitors on a 20 x 15 mm board, placed, not routed.
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
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "7.5mm"]}));
    (dir, r, s)
}

fn drc(s: &Session) -> Vec<Diagnostic> {
    cadlab::drc::check(s.project.as_ref().unwrap())
}

fn of<'a>(d: &'a [Diagnostic], code: &str, subject: &str) -> Vec<&'a Diagnostic> {
    let want = ObjectRef::parse(subject).unwrap();
    d.iter().filter(|x| x.code == code && x.subjects.contains(&want)).collect()
}

fn footprint_name(s: &mut Session, refdes: &str) -> String {
    board::footprint_for(project(s), refdes).unwrap().name.clone()
}

/// Board box of a placed pad.
fn pad_box(s: &mut Session, refdes: &str, number: &str) -> (Point, Point) {
    let pp = board::placed_pads(project(s)).into_iter().find(|x| x.refdes == refdes && x.number == number).unwrap();
    let b = cadlab::geom::poly::Geometry::bbox(&pp.shape).unwrap();
    (Point::new(Nm(b.min.x), Nm(b.min.y)), Point::new(Nm(b.max.x), Nm(b.max.y)))
}

fn add_track(s: &mut Session, layer: &str, a: Point, b: Point, width: Nm, net: &str) -> u64 {
    let p = project(s);
    let id = p.alloc_id();
    p.board_mut().tracks.push(Track {
        id,
        layer: layer.into(),
        width,
        net: Some(net.into()),
        start: a,
        end: b,
        mid: None,
        locked: false,
    });
    id.0
}

#[test]
fn local_clearances_and_custom_rules() {
    let (_d, r, mut s) = setup();
    // A 3V3 track 0.15 mm right of C1.1 (VIN): below the 0.2 mm rules clearance.
    let (lo, hi) = pad_box(&mut s, "C1", "1");
    let x = hi.x.0 as f64 / 1e6 + 0.15 + 0.1;
    let t = add_track(
        &mut s,
        "F.Cu",
        mm(x, lo.y.0 as f64 / 1e6 - 1.0),
        mm(x, hi.y.0 as f64 / 1e6 + 1.0),
        Nm(200_000),
        "3V3",
    );
    let track = format!("track#{t}");
    assert_eq!(of(&drc(&s), "drc.clearance", &track).len(), 1);

    // A local clearance on the capacitor's pad 1 replaces the net clearance, both ways.
    let cap = footprint_name(&mut s, "C1");
    let o = exec(&r, &mut s, "footprint.set", json!({"name": cap, "pads": ["1"], "clearance": "0.1mm"}));
    assert_eq!(o["output"]["pads"][0]["clearance"], "0.1mm");
    assert!(of(&drc(&s), "drc.clearance", &track).is_empty());
    // ... never below the rules' floor.
    exec(&r, &mut s, "board.rules", json!({"min_clearance": "0.18mm"}));
    let d = drc(&s);
    let c = of(&d, "drc.clearance", &track);
    assert_eq!(c.len(), 1);
    assert!(c[0].message.contains("clearance is 0.18mm"), "{}", c[0].message);
    exec(&r, &mut s, "board.rules", json!({"min_clearance": "0mm"}));
    // A footprint-level clearance is the pads' default; the pad's own wins.
    exec(&r, &mut s, "footprint.set", json!({"name": cap, "clearance": "0.3mm"}));
    assert!(of(&drc(&s), "drc.clearance", &track).is_empty(), "pad 1 keeps its 0.1 mm");
    exec(&r, &mut s, "footprint.set", json!({"name": cap, "pads": ["1"], "unset": ["clearance"]}));
    let d = drc(&s);
    assert!(of(&d, "drc.clearance", &track)[0].message.contains("clearance is 0.3mm"));
    exec(&r, &mut s, "footprint.set", json!({"name": cap, "unset": ["clearance"]}));

    // Custom rules: tracks in C1's courtyard may come to 0.1 mm; the last matching rule wins.
    assert_eq!(fails(&r, &mut s, "board.custom_rule", json!({"name": "x"})), "board.invalid_custom_rule");
    assert_eq!(
        fails(&r, &mut s, "board.custom_rule", json!({"name": "x", "clearance": "1mm", "in_area": "nowhere"})),
        "keepout.not_found"
    );
    exec(
        &r,
        &mut s,
        "board.custom_rule",
        json!({"name": "neckdown", "kinds": ["track"], "layer": "F.Cu", "in_courtyard": "C1", "clearance": "0.1mm", "track_width": "0.25mm"}),
    );
    let d = drc(&s);
    assert!(of(&d, "drc.clearance", &track).is_empty());
    let w = of(&d, "drc.track_width", &track);
    assert_eq!(w.len(), 1, "the rule's track width");
    assert!(w[0].message.contains("custom rule `neckdown` asks for 0.25mm"), "{}", w[0].message);
    exec(
        &r,
        &mut s,
        "board.custom_rule",
        json!({"name": "pads", "kinds": ["pad"], "footprint": cap, "clearance": "0.3mm"}),
    );
    let d = drc(&s);
    assert!(of(&d, "drc.clearance", &track)[0].message.contains("clearance is 0.3mm"), "the later rule wins");
    let o = exec(&r, &mut s, "board.custom_rules", json!({}));
    assert_eq!(o["output"]["rules"].as_array().unwrap().len(), 2);
    exec(&r, &mut s, "board.custom_rule_remove", json!({"name": "pads"}));
    exec(&r, &mut s, "board.custom_rule_remove", json!({"name": "neckdown"}));
    assert_eq!(of(&drc(&s), "drc.clearance", &track).len(), 1);

    // A rule area forbids nothing and scopes rules.
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "breakout", "outline": {"rect": {"from": ["0mm", "0mm"], "to": ["20mm", "15mm"]}}, "rule_area": true}),
    );
    exec(
        &r,
        &mut s,
        "board.custom_rule",
        json!({"name": "area", "in_area": "breakout", "kinds": ["track"], "clearance": "0.12mm"}),
    );
    let d = drc(&s);
    assert!(of(&d, "drc.clearance", &track).is_empty());
    assert!(d.iter().all(|x| x.code != "drc.keepout"), "a rule area forbids nothing");
}

#[test]
fn zone_connections_and_local_clearances_in_fills() {
    let (_d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "gnd", "net": "GND", "layers": ["F.Cu"], "outline": "board", "pads": "thermal"}),
    );
    let p = project(&mut s);
    let fill = |p: &cadlab::model::Project| {
        board::zones::fill_zones_uncached(p, &board::base_copper_items(p))
            .iter()
            .map(|f| board::zones::area_mm2(&f.fill))
            .sum::<f64>()
    };
    let thermal = fill(p);
    let cap = footprint_name(&mut s, "C1");
    // C1.2 and C2.2 (GND) solid: more copper.
    exec(&r, &mut s, "footprint.set", json!({"name": cap, "pads": ["2"], "zone_connection": "solid"}));
    let solid = fill(project(&mut s));
    assert!(solid > thermal + 0.1, "solid {solid} vs thermal {thermal}");
    // A large local clearance on the VIN pad 1 keeps the pour further away.
    exec(&r, &mut s, "footprint.set", json!({"name": cap, "pads": ["1"], "clearance": "1mm"}));
    let wide = fill(project(&mut s));
    assert!(wide < solid - 0.5, "wide {wide} vs solid {solid}");
    // The fill keeps the DRC clean.
    let d = drc(&s);
    assert!(d.iter().all(|x| x.code != "drc.clearance" && x.code != "drc.short"), "{d:#?}");
    // tht_thermal: SMD pads solid.
    exec(
        &r,
        &mut s,
        "footprint.set",
        json!({"name": cap, "unset": ["zone_connection", "clearance"], "pads": ["1", "2"]}),
    );
    exec(&r, &mut s, "zone.set", json!({"name": "gnd", "pads": "solid"}));
    let solid = fill(project(&mut s));
    exec(&r, &mut s, "zone.set", json!({"name": "gnd", "pads": "tht_thermal"}));
    let tht = fill(project(&mut s));
    assert!((tht - solid).abs() < 1e-6, "SMD pads are solid with tht_thermal: {tht} vs {solid}");
}

/// A copper bridge between the capacitor's pads.
fn bridge(s: &mut Session, refdes: &str) {
    let name = footprint_name(s, refdes);
    let p = project(s);
    let f = p.library_mut().footprints.get_mut(&name).unwrap();
    let (a, b) = (f.pads[0].at, f.pads[1].at);
    f.graphics.push(Graphic::new(GraphicLayer::Copper, Nm(150_000), GraphicGeometry::Path { points: vec![a, b] }));
}

#[test]
fn net_ties() {
    let (_d, r, mut s) = setup();
    bridge(&mut s, "C1");
    let d = drc(&s);
    let shorts: Vec<&Diagnostic> = d.iter().filter(|x| x.code == "drc.short").collect();
    // C1 and C2 share the footprint: each bridge (on its first pad's net) shorts the other pad.
    assert_eq!(shorts.len(), 2, "{shorts:#?}");
    assert!(shorts.iter().any(|x| x.subjects.contains(&ObjectRef::Name("C1".into()))), "the footprint is named");
    let cap = footprint_name(&mut s, "C1");
    let o = exec(&r, &mut s, "footprint.set", json!({"name": cap, "net_ties": [["1", "2"]]}));
    assert_eq!(o["output"]["net_ties"], json!([["1", "2"]]));
    let d = drc(&s);
    assert!(d.iter().all(|x| x.code != "drc.short"), "{d:#?}");
    // The nets stay distinct: the bridge does not route VIN to GND.
    let items = board::copper_items(project(&mut s));
    let isl = board::islands(&items);
    let idx = |it: &ItemRef| items.iter().position(|x| &x.item == it).unwrap();
    let (p1, p2) = (idx(&ItemRef::Pad("C1".into(), "1".into())), idx(&ItemRef::Pad("C1".into(), "2".into())));
    assert_ne!(isl[p1], isl[p2]);
    // Other copper touching a tied pad is still a short.
    let (lo, hi) = pad_box(&mut s, "C1", "1");
    let y = (lo.y.0 + hi.y.0) as f64 / 2e6;
    let t = add_track(
        &mut s,
        "F.Cu",
        mm(lo.x.0 as f64 / 1e6 - 1.0, y),
        mm(lo.x.0 as f64 / 1e6 + 0.1, y),
        Nm(200_000),
        "3V3",
    );
    assert_eq!(of(&drc(&s), "drc.short", &format!("track#{t}")).len(), 1);
    assert_eq!(
        fails(&r, &mut s, "footprint.set", json!({"name": cap, "net_ties": [["1", "9"]]})),
        "footprint.pad_not_found"
    );
}

#[test]
fn back_pads_slots_mask_and_paste() {
    let (_d, r, mut s) = setup();
    let ldo = footprint_name(&mut s, "U1");
    // U1.4 (NC) on the back, tented.
    exec(&r, &mut s, "footprint.set", json!({"name": ldo, "pads": ["4"], "back": true, "mask": "none"}));
    let pads = board::placed_pads(project(&mut s));
    let nc = pads.iter().find(|x| x.refdes == "U1" && x.number == "4").unwrap();
    assert_eq!(nc.layers, ["B.Cu"]);
    assert_eq!(fails(&r, &mut s, "footprint.set", json!({"name": ldo, "back": true})), "footprint.pads_required");

    // A header with a slotted pad 1 and paste-in-hole on pad 2.
    exec(&r, &mut s, "part.create", json!({"id": "HDR2", "category": "connector", "package": "PinHeader 1x02"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "HDR2"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["4mm", "3mm"]}));
    let hdr = footprint_name(&mut s, "J1");
    exec(&r, &mut s, "footprint.set", json!({"name": hdr, "pads": ["1"], "slot": ["1.5mm", "0.6mm"]}));
    exec(&r, &mut s, "footprint.set", json!({"name": hdr, "pads": ["2"], "paste": "pad"}));
    let f = board::footprint_for(project(&mut s), "J1").unwrap().clone();
    assert_eq!(f.pads[0].kind, PadKind::Tht { drill: Nm(600_000) });
    assert_eq!(f.pads[0].slot, Some((Nm(1_500_000), Nm(600_000))));
    assert_eq!(f.pads[1].paste, Some(Paste::Pad));

    // Board mask and paste settings, a footprint mask margin, the minimum web.
    exec(
        &r,
        &mut s,
        "board.rules",
        json!({"mask_expansion": "0.05mm", "paste_ratio": "-0.1", "mask_min_web": "0.3mm"}),
    );
    let cap = footprint_name(&mut s, "C1");
    exec(&r, &mut s, "footprint.set", json!({"name": cap, "mask_margin": "0mm"}));
    let p = project(&mut s).clone();
    let files = cadlab::fabout::gerbers(&p, &Default::default());
    let file = |suffix: &str| files.iter().find(|f| f.name.ends_with(suffix)).unwrap().content.clone();
    let bmask = file("-B_Mask.gbr");
    let bcu = file("-B_Cu.gbr");
    assert!(bcu.contains("U1,4"), "the back pad is on B.Cu");
    assert!(!bmask.contains("%TO.C,U1*%"), "tented: no opening\n{bmask}");
    let fmask = file("-F_Mask.gbr");
    assert!(fmask.contains("merged across mask narrower than the minimum web"), "SOT-23 pads 0.3 mm apart merge");
    let fpaste = file("-F_Paste.gbr");
    assert!(fpaste.contains("%TO.C,J1*%"), "paste in hole on J1.2:\n{fpaste}");
    // The slot is routed in the drill file, and drawn in the X2 drill file.
    let drills = cadlab::fabout::excellon::drills(&p, &Default::default());
    let pth = &drills.iter().find(|f| f.name.ends_with("-PTH.drl")).unwrap().content;
    assert!(pth.contains("M15\nG01X") && pth.contains("M16\nG05\n"), "{pth}");
    let x2 = cadlab::fabout::drill_gerbers(&p, &Default::default());
    assert!(x2[0].content.contains("D01*"), "the slot is a draw");
    // DRC: the slot's annular ring along its length.
    exec(&r, &mut s, "board.rules", json!({"min_annular_ring": "0.25mm"}));
    let d = drc(&s);
    assert!(!of(&d, "drc.pad_annular_ring", "J1.1").is_empty(), "{d:#?}");
}

#[test]
fn copper_drawings_on_the_board() {
    let (_d, r, mut s) = setup();
    let _ = &r;
    let p = project(&mut s);
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "F.Cu".into(),
        kind: GraphicKind::Text {
            text: "V1".into(),
            at: mm(10.0, 2.0),
            size: Nm(1_000_000),
            rotation: Default::default(),
        },
    });
    let id2 = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id: id2,
        layer: "B.Cu".into(),
        kind: GraphicKind::Polygon { points: vec![mm(2.0, 12.0), mm(4.0, 12.0), mm(4.0, 13.0)], width: Nm::ZERO },
    });
    let items = board::copper_items(p);
    assert!(items.iter().any(|x| x.item == ItemRef::Graphic(id) && x.net.is_none()));
    // A track over the text: no-net copper touching a net.
    add_track(&mut s, "F.Cu", mm(9.0, 2.0), mm(11.0, 2.0), Nm(200_000), "VIN");
    let d = drc(&s);
    assert_eq!(of(&d, "drc.short", &format!("graphic#{}", id.0)).len(), 1, "{d:#?}");
    let files = cadlab::fabout::gerbers(project(&mut s), &Default::default());
    let bcu = &files.iter().find(|f| f.name.ends_with("-B_Cu.gbr")).unwrap().content;
    assert!(bcu.contains("G36*"), "the polygon is a region:\n{bcu}");
}

#[test]
fn kicad_round_trip() {
    let (_d, r, mut s) = setup();
    bridge(&mut s, "C1");
    let cap = footprint_name(&mut s, "C1");
    let ldo = footprint_name(&mut s, "U1");
    exec(
        &r,
        &mut s,
        "footprint.set",
        json!({"name": cap, "net_ties": [["1", "2"]], "mask_margin": "0.05mm", "paste_ratio": "-0.05"}),
    );
    exec(
        &r,
        &mut s,
        "footprint.set",
        json!({"name": cap, "pads": ["1"], "clearance": "0.1mm", "zone_connection": "solid", "paste_margin": "-0.03mm"}),
    );
    exec(&r, &mut s, "footprint.set", json!({"name": ldo, "pads": ["4"], "back": true, "mask": "none"}));
    exec(&r, &mut s, "part.create", json!({"id": "HDR2", "category": "connector", "package": "PinHeader 1x02"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "HDR2"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["4mm", "3mm"]}));
    let hdr = footprint_name(&mut s, "J1");
    exec(&r, &mut s, "footprint.set", json!({"name": hdr, "pads": ["1"], "slot": ["0.6mm", "1mm"], "paste": "pad"}));
    exec(
        &r,
        &mut s,
        "board.rules",
        json!({"mask_expansion": "0.04mm", "mask_min_web": "0.1mm", "paste_margin": "-0.02mm", "paste_ratio": "-0.03"}),
    );
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "gnd", "net": "GND", "layers": ["B.Cu"], "outline": "board", "pads": "tht_thermal"}),
    );
    exec(
        &r,
        &mut s,
        "keepout.add",
        json!({"name": "area", "outline": {"rect": {"from": ["1mm", "1mm"], "to": ["8mm", "6mm"]}}, "rule_area": true}),
    );
    exec(
        &r,
        &mut s,
        "board.custom_rule",
        json!({"name": "a", "kinds": ["track", "via"], "in_area": "area", "clearance": "0.15mm"}),
    );
    exec(
        &r,
        &mut s,
        "board.custom_rule",
        json!({"name": "b", "kinds": ["pad"], "footprint": cap, "layer": "F.Cu", "clearance": "0.12mm", "track_width": "0.2mm"}),
    );
    exec(&r, &mut s, "board.custom_rule", json!({"name": "c", "in_courtyard": "U1", "clearance": "0.11mm"}));
    let p = project(&mut s);
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "B.Cu".into(),
        kind: GraphicKind::Polygon { points: vec![mm(2.0, 12.0), mm(4.0, 12.0), mm(4.0, 13.0)], width: Nm::ZERO },
    });
    let before = p.clone();
    let ex = cadlab::kicad_pcb::export(&before, "t");
    assert!(ex.pcb.contains("(net_tie_pad_groups \"1,2\")"), "{}", ex.pcb);
    assert!(ex.pcb.contains("(drill oval 0.6 1)"));
    assert!(ex.pcb.contains("(solder_mask_min_width 0.1)"));
    assert!(ex.rules.contains("A.enclosedByArea('area')"), "{}", ex.rules);

    // Back into a copy with the board cleared: the same footprints, rules and settings.
    let mut q = before.clone();
    let rules = cadlab::kicad_import::rules::parse(Some(&ex.project), Some(&ex.rules)).unwrap().0;
    let opts = cadlab::kicad_import::BoardImportOptions { replace: true, rules: Some(rules), ..Default::default() };
    let (_, diags) = cadlab::kicad_import::import(&mut q, &ex.pcb, &opts).unwrap();
    let bad: Vec<&Diagnostic> =
        diags.iter().filter(|d| d.code == "import.rule_unsupported" || d.code == "import.pad_unsupported").collect();
    assert!(bad.is_empty(), "{bad:#?}");
    for refdes in ["C1", "U1", "J1"] {
        assert_eq!(board::footprint_for(&q, refdes), board::footprint_for(&before, refdes), "{refdes}");
    }
    let (a, b) = (&before.board().rules, &q.board().rules);
    assert_eq!(
        (a.mask_expansion, a.mask_min_web, a.paste_margin, a.paste_ratio),
        (b.mask_expansion, b.mask_min_web, b.paste_margin, b.paste_ratio)
    );
    assert_eq!(q.board().custom_rules, before.board().custom_rules);
    assert_eq!(q.board().zones[0].pads, PadConnection::ThtThermal);
    assert!(q.board().keepouts.iter().any(|k| k.name == "area"), "the rule area a rule refers to is kept");
    assert!(q.board().graphics.iter().any(|g| g.layer == "B.Cu" && matches!(g.kind, GraphicKind::Polygon { .. })));
    let codes = |p: &cadlab::model::Project| {
        let mut v: Vec<(String, Option<(Nm, Nm)>)> =
            cadlab::drc::check(p).into_iter().map(|d| (d.code.to_string(), d.location.map(|l| (l.x, l.y)))).collect();
        v.sort();
        v
    };
    assert_eq!(codes(&q), codes(&before));
}
