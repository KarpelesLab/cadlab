//! Thermal relief of pads in pours, end to end (docs/BOARD.md, "Thermal relief"; DECISIONS D109):
//! what a zone made with minimal arguments gives each kind of pad on every layer, the DRC
//! findings for solid, nominally relieved and starved pads, the repair command, the fab check
//! and the exported copper.

use cadlab::board::thermal::{self, Joint, PadClass};
use cadlab::command::{Registry, RunOptions, Session};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// A four-layer board: a 1x04 pin header (J1, pin 1 on GND), a regulator in SOT-223 whose tab
/// is GND (U1), a QFN-16 with its exposed pad on GND (U2), two 0402 capacitors to GND (C1, C2)
/// and a GND via. No zones yet.
fn setup() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    let pins = |names: &[(&str, &str)]| -> Value {
        names
            .iter()
            .enumerate()
            .map(|(i, (n, k))| json!({"number": (i + 1).to_string(), "name": n, "kind": k}))
            .collect()
    };
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "connector", "mpn": "HDR-1x04", "package": "PinHeader 1x04",
               "pins": pins(&[("GND", "passive"), ("A", "passive"), ("B", "passive"), ("VIN", "passive")])}),
    );
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ldo", "mpn": "REG-223", "package": "SOT-223",
               "pins": pins(&[("IN", "power_in"), ("ADJ", "passive"), ("OUT", "power_out"), ("TAB", "power_in")])}),
    );
    let mut qfn: Vec<(String, &str)> = (1..=16).map(|i| (format!("P{i}"), "passive")).collect();
    qfn.push(("EP".into(), "power_in"));
    let qfn: Vec<(&str, &str)> = qfn.iter().map(|(n, k)| (n.as_str(), *k)).collect();
    exec(
        &r,
        &mut s,
        "part.create",
        json!({"category": "ic", "mpn": "QFN-IC", "package": "QFN-16 3x3mm P0.5mm EP1.7mm", "pins": pins(&qfn)}),
    );
    exec(&r, &mut s, "circuit.add", json!({"part": "HDR-1x04"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "REG-223"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "QFN-IC"}));
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402", "count": 2}));
    exec(
        &r,
        &mut s,
        "net.connect",
        json!({"net": "GND", "pins": ["J1.1", "U1.TAB", "U2.EP", "U2.P1", "C1.2", "C2.2"]}),
    );
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["J1.4", "U1.IN", "C1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "VOUT", "pins": ["U1.OUT", "C2.1", "U2.P2"]}));
    exec(&r, &mut s, "board.setup", json!({"layers": 4}));
    exec(&r, &mut s, "board.outline", json!({"width": "40mm", "height": "30mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "J1", "at": ["6mm", "15mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["18mm", "15mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U2", "at": ["30mm", "20mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["12mm", "24mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["24mm", "8mm"]}));
    exec(&r, &mut s, "via.add", json!({"at": ["34mm", "6mm"], "net": "GND"}));
    (dir, r, s)
}

fn joints(s: &Session) -> Vec<Joint> {
    let p = s.project.as_ref().unwrap();
    thermal::joints(p, &cadlab::board::copper_items(p))
}

fn joint<'a>(js: &'a [Joint], pad: &str, layer: &str) -> &'a Joint {
    js.iter()
        .find(|j| format!("{}.{}", j.refdes, j.number) == pad && j.layer == layer)
        .unwrap_or_else(|| panic!("no joint for {pad} on {layer}"))
}

fn findings(o: &Value, code: &str) -> Vec<String> {
    o["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == code)
        .map(|d| d["message"].as_str().unwrap().to_string())
        .collect()
}

/// The zone an agent makes with the fewest arguments: every solder pad relieved by four
/// spokes on every layer (inner planes too), the exposed pad and the tab solid on purpose, the
/// via solid, and no thermal finding from the DRC.
#[test]
fn minimal_zone_relieves_solder_pads_on_every_layer() {
    let (_d, r, mut s) = setup();
    let o = exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"], "outline": "board"}),
    );
    assert_eq!(o["output"]["zones"][0]["pads"], "thermal");
    assert!(findings(&o, "zone.solid_pads").is_empty());
    let js = joints(&s);
    // The through-hole pin: four spokes on each of the four layers.
    for layer in ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"] {
        let j = joint(&js, "J1.1", layer);
        assert_eq!((j.class, j.spokes, j.planned), (PadClass::Through, 4, 4), "{layer}");
        assert!(j.solid(67).is_none() && j.share() < 50, "{layer}: {} %", j.share());
    }
    // SMD solder pads: relieved.
    for pad in ["C1.2", "C2.2"] {
        let j = joint(&js, pad, "F.Cu");
        assert_eq!(j.class, PadClass::Smd);
        assert!(j.spokes >= 2 && j.solid(67).is_none(), "{pad}: {j:?}");
    }
    // Heat pads: the generated footprints say solid themselves.
    // (The QFN's exposed pad is solid too, but its own pins shut it in: the pour reaches it
    // through pin 1 only.)
    let j = joint(&js, "U1.4", "F.Cu");
    assert_eq!(j.class, PadClass::Heat);
    assert!(j.own && j.share() > 90, "{j:?}");
    let ep = joint(&js, "U2.17", "F.Cu");
    assert!(ep.class == PadClass::Heat && ep.own, "{ep:?}");
    let o = exec(&r, &mut s, "drc.run", json!({}));
    for code in ["drc.solid_pad", "drc.thermal_nominal", "drc.starved_thermal", "drc.thermal_size"] {
        assert!(findings(&o, code).is_empty(), "{code}: {:?}", findings(&o, code));
    }
    // The via sits solidly in the pour on every layer: the fill has no gap around it.
    let p = s.project.as_ref().unwrap();
    let fills = cadlab::board::zones::fill_zones(p, &cadlab::board::base_copper_items(p));
    assert_eq!(fills.len(), 4);
    let ring = cadlab::geom::poly::Circle::new(cadlab::geom::poly::Point::new(34_000_000, 6_000_000), 350_000)
        .to_ring(cadlab::board::COPPER_TOL)
        .unwrap();
    for f in &fills {
        let ring = cadlab::geom::poly::Polygon::new(ring.clone(), vec![]);
        assert!(cadlab::geom::poly::contains(&f.fill, &ring), "via not solid on {}", f.layer);
    }
    // The census: nothing solid that is not meant, nothing starved, a quarter of the pads inner.
    let o = exec(&r, &mut s, "zone.fill", json!({}));
    let c = &o["output"]["pads"];
    assert_eq!(
        (c["solid_through"].as_u64(), c["solid_smd"].as_u64(), c["starved"].as_u64()),
        (Some(0), Some(0), Some(0))
    );
    assert_eq!(c["heat_solid"], 1);
    assert_eq!(c["inner"], 2, "the header pin on In1.Cu and In2.Cu");
    assert!(findings(&o, "zone.solid_pads").is_empty());
}

/// A zone set to solid: the command says what it costs, the DRC reports every solder pad (the
/// through-hole pin once, with its layers, inner ones included), names the tombstoning risk of
/// the two-pad parts, and stays quiet for the exposed pad, the tab and the via. `zone.relieve`
/// then brings the board to relieved pads and the findings go.
#[test]
fn solid_zone_is_reported_and_repaired() {
    let (_d, r, mut s) = setup();
    let o = exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"], "outline": "board",
               "pads": "solid"}),
    );
    assert_eq!(findings(&o, "zone.solid_pads").len(), 1);
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let solid = findings(&o, "drc.solid_pad");
    assert_eq!(solid.len(), 4, "{solid:#?}");
    let pin = solid.iter().find(|m| m.contains("J1.1")).expect("header pin");
    assert!(pin.contains("through-hole pin") && pin.contains("F.Cu, In1.Cu, In2.Cu, B.Cu"), "{pin}");
    let cap = solid.iter().find(|m| m.contains("C1.2")).expect("capacitor");
    assert!(cap.contains("tombstoning") && cap.contains("C1.1"), "{cap}");
    assert!(solid.iter().any(|m| m.contains("U2.1 ")), "{solid:#?}");
    assert!(!solid.iter().any(|m| m.contains("U1.4") || m.contains("U2.17")), "heat pads are quiet: {solid:#?}");
    let d = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "drc.solid_pad").unwrap();
    assert_eq!(d["severity"], "warning");
    assert!(d["hint"].as_str().unwrap().contains("zone.relieve"));
    let subjects: Vec<&str> = d["subjects"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(subjects.contains(&"zone:GND") && subjects.iter().any(|s| s.contains('.')), "{subjects:?}");
    // zone.fill and zone.thermals count them.
    let o = exec(&r, &mut s, "zone.fill", json!({}));
    assert_eq!(o["output"]["pads"]["solid_through"], 4);
    assert_eq!(o["output"]["pads"]["inner_solid"], 2);
    assert_eq!(findings(&o, "zone.solid_pads").len(), 1);
    let o = exec(&r, &mut s, "zone.thermals", json!({}));
    assert!(o["output"]["pads"].as_array().unwrap().iter().all(|j| j["state"] == "solid"));
    // The fab check carries them too, as warnings: the export is not blocked.
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "generic"}));
    assert_eq!(findings(&o, "fab.solid_pad").len(), 4);
    // Repair.
    let o = exec(&r, &mut s, "zone.relieve", json!({}));
    let out = &o["output"];
    assert_eq!(out["zones"], json!([["GND", "solid"]]));
    assert_eq!((out["before"]["solid_through"].as_u64(), out["after"]["solid_through"].as_u64()), (Some(4), Some(0)));
    assert_eq!(out["after"]["solid_smd"], 0);
    assert_eq!(out["after"]["heat_solid"], 1, "the tab stays solid");
    assert_eq!(out["left"], json!([]));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert!(findings(&o, "drc.solid_pad").is_empty());
    assert!(findings(&o, "drc.starved_thermal").is_empty());
    // One undo step takes the whole repair back.
    exec(&r, &mut s, "history.undo", json!({}));
    assert_eq!(exec(&r, &mut s, "zone.list", json!({}))["output"]["zones"][0]["pads"], "solid");
}

/// A pad that is solid on purpose: said on the pad, the check is quiet; a waiver on the zone
/// quiets the rest, in the DRC and in the fab check.
#[test]
fn solid_on_purpose_is_quiet() {
    let (_d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["F.Cu"], "outline": "board", "pads": "solid"}),
    );
    let fp = {
        let p = s.project.as_ref().unwrap();
        cadlab::board::footprint_for(p, "J1").unwrap().name.clone()
    };
    exec(&r, &mut s, "footprint.set", json!({"name": fp, "pads": ["1"], "zone_connection": "solid"}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let solid = findings(&o, "drc.solid_pad");
    assert!(!solid.iter().any(|m| m.contains("J1.1")), "{solid:#?}");
    assert_eq!(solid.len(), 3);
    exec(
        &r,
        &mut s,
        "drc.waive",
        json!({"code": "drc.solid_pad", "subjects": ["zone:GND"], "reason": "reflow only, preheated"}),
    );
    let o = exec(&r, &mut s, "drc.run", json!({}));
    assert!(findings(&o, "drc.solid_pad").is_empty());
    assert_eq!(o["output"]["waived"], 3, "{}", o["output"]);
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "generic"}));
    assert!(findings(&o, "fab.solid_pad").is_empty());
    // zone.relieve keeps a deliberate pad setting when asked to, and removes it otherwise.
    let o = exec(&r, &mut s, "zone.relieve", json!({"keep_own": true}));
    assert_eq!(o["output"]["removed"], json!([]));
    let js = joints(&s);
    assert!(joint(&js, "J1.1", "F.Cu").solid(67).is_some());
    let o = exec(&r, &mut s, "zone.relieve", json!({}));
    assert_eq!(o["output"]["removed"].as_array().map_or(0, Vec::len), 1, "{}", o["output"]);
    let js = joints(&s);
    assert!(joint(&js, "J1.1", "F.Cu").solid(67).is_none());
}

/// Spokes that do not survive: tracks of another net on three sides of the header pin leave it
/// one spoke (`drc.starved_thermal`, a warning naming pad and zone); on all four sides it is
/// cut off its pour and the DRC also reports the connection open.
#[test]
fn starved_thermal() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "zone.add", json!({"name": "GND", "net": "GND", "layers": ["B.Cu"], "outline": "board"}));
    let at = joint(&joints(&s), "J1.1", "B.Cu").at;
    let (x, y) = (at.x.0, at.y.0);
    let mm = |v: i64| format!("{v}nm");
    let d = 1_500_000;
    // A U of VIN track around the pin: left, top and right.
    let u = [(x - d, y - d), (x - d, y + d), (x + d, y + d), (x + d, y - d)];
    let pts: Vec<Value> = u.iter().map(|(a, b)| json!([mm(*a), mm(*b)])).collect();
    exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "net": "VIN", "points": pts}));
    let js = joints(&s);
    let j = joint(&js, "J1.1", "B.Cu");
    assert_eq!((j.spokes, j.planned), (1, 4), "{j:?}");
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let starved = findings(&o, "drc.starved_thermal");
    assert_eq!(starved.len(), 1, "{starved:#?}");
    assert!(starved[0].contains("J1.1") && starved[0].contains("1 on B.Cu of 4"), "{}", starved[0]);
    let diag = o["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "drc.starved_thermal").unwrap();
    assert_eq!(diag["severity"], "warning");
    assert!(diag["subjects"].as_array().unwrap().iter().any(|s| s == "zone:GND"));
    assert!(diag["hint"].as_str().unwrap().contains("thermal-angle"));
    assert_eq!(findings(&exec(&r, &mut s, "fab.check", json!({"fab": "generic"})), "fab.starved_thermal").len(), 1);
    // A minimum of one accepts it; zero turns the check off.
    exec(&r, &mut s, "board.rules", json!({"min_thermal_spokes": 1}));
    assert!(findings(&exec(&r, &mut s, "drc.run", json!({})), "drc.starved_thermal").is_empty());
    exec(&r, &mut s, "board.rules", json!({"min_thermal_spokes": 2}));
    // Close the U: no spoke is left, the pad is cut off its pour.
    let pts = json!([[mm(x + d), mm(y - d)], [mm(x - d), mm(y - d)]]);
    exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "net": "VIN", "points": pts}));
    let js = joints(&s);
    assert!(!js.iter().any(|j| j.refdes == "J1" && j.number == "1" && j.spokes > 0), "{js:#?}");
    let o = exec(&r, &mut s, "drc.run", json!({"unrouted": "each"}));
    assert!(findings(&o, "drc.unrouted").iter().any(|m| m.contains("J1.1")), "{:#?}", findings(&o, "drc.unrouted"));
}

/// Relief in name only: spokes as wide as an 0402 pad's sides cover its outline
/// (`drc.thermal_nominal`); `zone.relieve` gives the pad a spoke of its own that fits.
#[test]
fn nominal_relief_and_sizes() {
    let (_d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["F.Cu"], "outline": "board", "thermal_spoke": "0.6mm",
               "thermal_gap": "0.2mm"}),
    );
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let nominal = findings(&o, "drc.thermal_nominal");
    assert!(nominal.iter().any(|m| m.contains("C1.2")) && nominal.iter().any(|m| m.contains("C2.2")), "{nominal:#?}");
    assert!(!nominal.iter().any(|m| m.contains("J1.1")), "a 1.7 mm pad is relieved by 0.6 mm spokes");
    let o = exec(&r, &mut s, "zone.relieve", json!({}));
    assert!(!o["output"]["narrowed"].as_array().unwrap().is_empty(), "{}", o["output"]);
    assert_eq!(o["output"]["after"]["solid_smd"], 0);
    assert!(findings(&exec(&r, &mut s, "drc.run", json!({})), "drc.thermal_nominal").is_empty());
    // A gap below the minimum clearance and a spoke below the minimum track width.
    exec(&r, &mut s, "zone.set", json!({"name": "GND", "thermal_gap": "0.05mm", "thermal_spoke": "0.1mm"}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let sizes = findings(&o, "drc.thermal_size");
    assert_eq!(sizes.len(), 2, "{sizes:#?}");
    assert!(sizes.iter().any(|m| m.contains("gap")) && sizes.iter().any(|m| m.contains("spoke width")));
}

/// A track wider than the pad makes it solid without any zone; a track of the pad's width does
/// not.
#[test]
fn wide_track_into_a_pad() {
    let (_d, r, mut s) = setup();
    let o = exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "width": "0.4mm", "points": ["C1.1", "U1.IN"]}));
    assert!(o["output"].is_object());
    assert!(findings(&exec(&r, &mut s, "drc.run", json!({})), "drc.solid_pad").is_empty());
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "width": "1.5mm", "points": ["C2.1", "U1.OUT"]}));
    let solid = findings(&exec(&r, &mut s, "drc.run", json!({})), "drc.solid_pad");
    assert!(solid.iter().any(|m| m.contains("C2.1") && m.contains("tracks wider than the pad")), "{solid:#?}");
}

/// The spokes of a power pin against its net's current: narrow spokes are reported with the
/// width IPC-2152 asks for.
#[test]
fn spokes_carry_the_current() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "zone.add", json!({"name": "GND", "net": "GND", "layers": ["F.Cu"], "outline": "board"}));
    assert!(findings(&exec(&r, &mut s, "drc.run", json!({})), "drc.current_spokes").is_empty());
    exec(&r, &mut s, "net.set", json!({"nets": ["GND"], "current": "8A"}));
    let o = exec(&r, &mut s, "drc.run", json!({}));
    let cur = findings(&o, "drc.current_spokes");
    assert_eq!(cur.len(), 1, "{cur:#?}");
    assert!(cur[0].contains("IPC-2152") && cur[0].contains("C1.2"), "{}", cur[0]);
    assert!(!cur[0].contains("U1.4"), "the solid tab carries it: {}", cur[0]);
}

/// The relief is in the exported copper: the Gerber of a relieved layer differs from the solid
/// one, and the KiCad export carries the zone's connection and the pads' own.
#[test]
fn outputs_carry_the_relief() {
    let (d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["F.Cu", "In1.Cu"], "outline": "board"}),
    );
    let out = d.path().join("gerber");
    exec(&r, &mut s, "export.gerber", json!({"dir": out}));
    let read = |dir: &std::path::Path, part: &str| -> String {
        let f = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().contains(part))
            .unwrap_or_else(|| panic!("no {part} file"));
        std::fs::read_to_string(f.path()).unwrap()
    };
    let relieved = read(&out, "In1_Cu");
    exec(&r, &mut s, "zone.set", json!({"name": "GND", "pads": "solid"}));
    let out2 = d.path().join("gerber-solid");
    exec(&r, &mut s, "export.gerber", json!({"dir": out2}));
    let solid = read(&out2, "In1_Cu");
    assert!(relieved.len() > solid.len() + 200, "the relief adds outline to the inner plane");
    exec(&r, &mut s, "zone.set", json!({"name": "GND", "pads": "thermal"}));
    let pcb = d.path().join("out.kicad_pcb");
    exec(&r, &mut s, "board.export_kicad", json!({"path": pcb}));
    let text = std::fs::read_to_string(&pcb).unwrap();
    assert!(
        text.contains("(connect_pads (clearance") || text.contains("(connect_pads\n"),
        "thermal is KiCad's default"
    );
    assert!(text.contains("(zone_connect 2)"), "the exposed pad and the tab are solid in KiCad too");
}

/// Where no spoke fits, relieving a solid pad takes its joint away: `zone.relieve` says which
/// pads (`cut`, `zone.relieve_cut`) instead of leaving them to be found later. The pin here is
/// fenced by another net on three sides and half of the fourth: the solid pour reaches it past
/// the end of the fence, where the spoke along the pad's axis has no room.
#[test]
fn relieve_reports_the_pads_it_cuts_off() {
    let (_d, r, mut s) = setup();
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["B.Cu"], "outline": "board", "pads": "solid"}),
    );
    let at = joint(&joints(&s), "J1.1", "B.Cu").at;
    let (x, y) = (at.x.0, at.y.0);
    let nm = |v: i64| format!("{v}nm");
    let d = 1_500_000;
    let fence = [(x + 300_000, y - d), (x - d, y - d), (x - d, y + d), (x + d, y + d), (x + d, y - d)];
    let pts: Vec<Value> = fence.iter().map(|(a, b)| json!([nm(*a), nm(*b)])).collect();
    exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "net": "VIN", "points": pts}));
    let before = joints(&s);
    assert!(joint(&before, "J1.1", "B.Cu").zone_contact.0 > 0, "the solid pour reaches the pin");
    let o = exec(&r, &mut s, "zone.relieve", json!({}));
    assert_eq!(o["output"]["cut"], json!(["J1.1 on B.Cu"]), "{}", o["output"]);
    let cut = findings(&o, "zone.relieve_cut");
    assert_eq!(cut.len(), 1);
    assert!(cut[0].contains("J1.1 on B.Cu"), "{}", cut[0]);
    // The pad is reported as one no spoke reaches, too.
    assert!(o["output"]["left"].as_array().unwrap().iter().any(|j| j["pad"] == "J1.1" && j["spokes"] == 0));
}

/// Parts on the bottom side are mirrored (their outlines run the other way): their pads are
/// classed, measured and relieved like those on top.
#[test]
fn bottom_side_parts() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "place.flip", json!({"refdes": ["C1", "C2", "U1"]}));
    exec(
        &r,
        &mut s,
        "zone.add",
        json!({"name": "GND", "net": "GND", "layers": ["B.Cu"], "outline": "board", "pads": "solid"}),
    );
    let js = joints(&s);
    for pad in ["C1.2", "C2.2"] {
        let j = joint(&js, pad, "B.Cu");
        assert_eq!(j.class, PadClass::Smd, "{pad}");
        assert!(j.perimeter.0 > 1_000_000 && j.solid(67).is_some(), "{pad}: {j:?}");
        assert!((400_000..700_000).contains(&j.short.0), "{pad}: short side {}", j.short);
    }
    assert_eq!(joint(&js, "U1.4", "B.Cu").class, PadClass::Heat);
    let classes = thermal::classes(&cadlab::board::placed_pads(s.project.as_ref().unwrap()));
    assert_eq!(classes[&("U1".to_string(), "1".to_string())], PadClass::Smd);
    let solid = findings(&exec(&r, &mut s, "drc.run", json!({})), "drc.solid_pad");
    assert!(solid.iter().any(|m| m.contains("C1.2")) && solid.iter().any(|m| m.contains("C2.2")), "{solid:#?}");
    assert!(!solid.iter().any(|m| m.contains("U1.4")), "the tab is a heat pad on either side: {solid:#?}");
    let o = exec(&r, &mut s, "zone.relieve", json!({}));
    assert_eq!(o["output"]["after"]["solid_smd"], 0, "{}", o["output"]);
    let js = joints(&s);
    assert!(joint(&js, "C1.2", "B.Cu").spokes >= 2 && joint(&js, "U1.4", "B.Cu").share() > 90);
}
