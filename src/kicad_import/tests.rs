//! Unit tests of the KiCad board importer on hand-written files in the documented syntax
//! (KiCad 6 to 9 net tables and older field syntax; KiCad 10 nets by name are covered by the
//! oracle round trip in `tests/kicad_pcb_import.rs`).

use super::*;
use crate::model::footprint::Paste;
use crate::units::mm as mmu;

fn codes(d: &[Diagnostic]) -> Vec<&str> {
    d.iter().map(|d| d.code.as_ref()).collect()
}

fn pt(x: f64, y: f64) -> Point {
    Point::new(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64))
}

/// A 30 × 20 mm two-layer board at (100, 50)–(130, 70) with a circular cutout, a mounting hole,
/// a logo, R1 (top, 90°, with a custom pad), U1 (bottom), copper and areas.
const BOARD: &str = r#"(kicad_pcb (version 20221018) (generator pcbnew)
  (general (thickness 1.2))
  (layers (0 "F.Cu" signal) (1 "In1.Cu" signal) (2 "In2.Cu" signal) (31 "B.Cu" signal)
    (36 "B.SilkS" user "B.Silkscreen") (37 "F.SilkS" user "F.Silkscreen") (44 "Edge.Cuts" user)
    (46 "B.CrtYd" user) (47 "F.CrtYd" user) (48 "B.Fab" user) (49 "F.Fab" user) (40 "Dwgs.User" user))
  (setup (stackup (layer "F.SilkS" (type "Top Silk Screen") (color "White"))
      (layer "F.Mask" (type "Top Solder Mask") (color "Black"))
      (layer "F.Cu" (type "copper") (thickness 0.07))
      (layer "dielectric 1" (type "core") (thickness 1.0))
      (layer "In1.Cu" (type "copper") (thickness 0.0175))
      (copper_finish "ENIG"))
    (pad_to_mask_clearance 0))
  (net 0 "")
  (net 1 "GND")
  (net 2 "/SIG")
  (net 3 "unconnected-(U1-Pad2)")
  (footprint "Resistor_SMD:R_Custom" (layer "F.Cu") (at 110 60 90)
    (property "Reference" "R1") (property "Value" "10k") (property "MPN" "RC0402")
    (attr smd)
    (fp_line (start -1 -0.5) (end 1 -0.5) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
    (fp_line (start 1 -0.5) (end 1 0.5) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
    (fp_line (start -1.5 -1) (end 1.5 -1) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_line (start 1.5 -1) (end 1.5 1) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_line (start 1.5 1) (end -1.5 1) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_line (start -1.5 1) (end -1.5 -1) (stroke (width 0.05) (type solid)) (layer "F.CrtYd"))
    (fp_text user "${REFERENCE}" (at 0 0 90) (layer "F.Fab"))
    (pad "1" smd roundrect (at -0.5 0 90) (size 0.6 0.5) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25)
      (net 2 "/SIG") (pinfunction "A"))
    (pad "2" smd custom (at 0.5 0 90) (size 0.4 0.4) (layers "F.Cu" "F.Mask") (net 1 "GND")
      (options (clearance outline) (anchor rect))
      (primitives (gr_poly (pts (xy 0 -0.2) (xy 0.6 -0.2) (xy 0.6 0.2) (xy 0 0.2)) (width 0) (fill yes)))))
  (footprint "Package:QFN_like" locked (layer "B.Cu") (at 120 60 180)
    (fp_text reference "U1" (at 0 0) (layer "B.SilkS")) (fp_text value "CHIP" (at 0 0) (layer "B.Fab"))
    (fp_rect (start -2 -2) (end 2 2) (stroke (width 0.05)) (layer "B.CrtYd"))
    (fp_circle (center 1 1) (end 1.2 1) (stroke (width 0.1)) (fill solid) (layer "B.SilkS"))
    (fp_line (start 0 0) (end 1 0) (stroke (width 0.1)) (layer "F.SilkS"))
    (pad "1" smd rect (at -1 0) (size 0.5 0.3) (layers "B.Cu" "B.Paste" "B.Mask") (net 1 "GND"))
    (pad "2" smd rect (at 1 0) (size 0.5 0.3) (layers "B.Cu" "B.Mask") (net 3 "unconnected-(U1-Pad2)"))
    (pad "3" smd rect (at 0 1) (size 1.2 1.2) (layers "B.Cu" "B.Mask") (net 1 "GND"))
    (pad "" smd rect (at -0.3 1) (size 0.5 0.5) (layers "B.Paste"))
    (pad "" smd rect (at 0.3 1) (size 0.5 0.5) (layers "B.Paste"))
    (pad "4" thru_hole oval (at 0 -1) (size 1 1.6) (drill oval 0.6 1.2) (layers "*.Cu" "*.Mask") (net 1 "GND")))
  (footprint "MountingHole:MountingHole_3.2mm_M3_Pad" (layer "F.Cu") (at 104 54)
    (property "Reference" "H1") (property "Value" "MountingHole")
    (fp_circle (center 0 0) (end 3 0) (stroke (width 0.15)) (layer "Cmts.User"))
    (pad "1" thru_hole circle (at 0 0) (size 6 6) (drill 3.2) (layers "*.Cu" "*.Mask") (net 1 "GND")))
  (footprint "Logo:Logo" (layer "F.Cu") (at 125 66)
    (property "Reference" "G***")
    (fp_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (stroke (width 0)) (fill solid) (layer "F.SilkS")))
  (footprint "Resistor_SMD:R_Custom" (layer "F.Cu") (at 112 64)
    (property "Reference" "REF**") (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu")))
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
  (gr_circle (center 115 55) (end 116 55) (stroke (width 0.1)) (layer "Edge.Cuts"))
  (gr_line (start 101 69) (end 105 69) (stroke (width 0.15)) (layer "F.SilkS"))
  (gr_line (start 105 69) (end 105 68) (stroke (width 0.15)) (layer "F.SilkS"))
  (gr_text "v1.0" (at 108 68 90) (layer "B.SilkS") (effects (font (size 1.5 1.5) (thickness 0.2)) (justify mirror)))
  (gr_line (start 102 52) (end 103 52) (stroke (width 0.2)) (layer "F.Cu"))
  (dimension (type aligned) (layer "Dwgs.User"))
  (segment (start 109.5 60.5) (end 109.5 66) (width 0.25) (layer "F.Cu") (net 2) (uuid "t1"))
  (arc (start 120 60) (mid 121 61) (end 122 60) (width 0.3) (layer "B.Cu") (net 1) (uuid "t2"))
  (segment locked (start 1 1) (end 2 2) (width 0.2) (layer "F.SilkS") (net 0))
  (via (at 109.5 66) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 2) (uuid "v1"))
  (via blind (at 112 66) (size 0.5) (drill 0.2) (layers "F.Cu" "In1.Cu") (net 0))
  (zone (net 1) (net_name "GND") (layers "In1.Cu" "In2.Cu") (uuid "z1") (name "gnd") (hatch edge 0.5)
    (priority 2) (connect_pads yes (clearance 0.3)) (min_thickness 0.25)
    (fill yes (thermal_gap 0.4) (thermal_bridge_width 0.35))
    (polygon (pts (xy 100 50) (xy 130 50) (xy 130 70) (xy 100 70)))
    (filled_polygon (layer "In1.Cu") (pts (xy 100 50) (xy 130 50) (xy 130 70))))
  (zone (net 0) (net_name "") (layers "*.Cu") (uuid "k1") (hatch edge 0.5) (connect_pads (clearance 0))
    (min_thickness 0.25)
    (keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour allowed) (footprints not_allowed))
    (polygon (pts (xy 126 51) (xy 129 51) (xy 129 54) (xy 126 54))))
  (zone (net 0) (net_name "") (layer "F.Cu") (name "ko2") (hatch edge 0.5)
    (keepout (tracks allowed) (vias allowed) (pads allowed) (copperpour not_allowed) (footprints allowed))
    (polygon (pts (xy 101 51) (xy 102 51) (xy 102 52))))
)"#;

fn opts() -> BoardImportOptions {
    BoardImportOptions { file_name: "t.kicad_pcb".into(), ..Default::default() }
}

#[test]
fn imports_a_hand_written_board() {
    let mut p = Project::new("t");
    let (r, d) = import(&mut p, BOARD, &opts()).unwrap();
    let b = p.board();
    // Setup.
    assert_eq!((r.copper_layers, r.thickness), (4, Nm::from_um(1200)));
    assert_eq!(b.stackup.outer_copper, Nm::from_um(70));
    assert_eq!(b.stackup.inner_copper, Nm(17_500));
    assert_eq!(b.stackup.finish, ["ENIG"]);
    assert_eq!(b.stackup.mask_color, ["Black"]);
    // Origin: no aux axis, so the outline's lower-left corner (100, 70).
    assert_eq!(r.origin, (mmu(100), mmu(70)));
    // Outline: the rectangle, and the circle as a cutout.
    assert_eq!(b.outline.contours.len(), 2);
    assert_eq!(b.outline.contours[0].start, pt(0.0, 20.0));
    assert!(matches!(b.outline.contours[1].segments[0], Segment::Arc { .. }));
    // R1: top, 90°; pad 2 custom → bounding rectangle 0.8 × 0.4 shifted by 0.2 mm along the pad.
    let r1 = &b.footprints["R1"];
    assert_eq!((r1.at, r1.rotation, r1.side), (pt(10.0, 10.0), Angle::DEG_90, BoardSide::Top));
    let fp = crate::board::footprint_for(&p, "R1").unwrap();
    assert_eq!(fp.name, "R_Custom");
    let p1 = &fp.pads[0];
    assert_eq!((p1.at, p1.rotation), (pt(-0.5, 0.0), Angle::ZERO));
    assert_eq!(p1.shape, PadShape::RoundRect { w: Nm(600_000), h: Nm(500_000), r: Nm(125_000) });
    assert_eq!(p1.paste, None, "F.Paste: opening equal to the pad");
    let p2 = &fp.pads[1];
    assert_eq!(p2.shape, PadShape::Rect { w: Nm(800_000), h: Nm(400_000) });
    assert_eq!(p2.at, pt(0.7, 0.0));
    assert_eq!(p2.paste, Some(Paste::None));
    assert_eq!(fp.courtyard, vec![pt(-1.5, 1.0), pt(1.5, 1.0), pt(1.5, -1.0), pt(-1.5, -1.0)]);
    assert_eq!(fp.graphics.len(), 1, "silk lines joined");
    // U1: bottom, KiCad 180° = cadlab 0°, locked; paste windows on pad 3; oval slot drilled round.
    let u1 = &b.footprints["U1"];
    assert_eq!((u1.rotation, u1.side, u1.locked), (Angle::ZERO, BoardSide::Bottom, true));
    let fu = crate::board::footprint_for(&p, "U1").unwrap();
    assert_eq!(fu.pads.len(), 4);
    assert_eq!(fu.pads[0].at, pt(-1.0, 0.0), "bottom: local (x, y) kept");
    assert_eq!(fu.pads[0].paste, None);
    match &fu.pads[2].paste {
        Some(Paste::Windows { size, at }) => {
            assert_eq!(*size, (Nm(500_000), Nm(500_000)));
            assert_eq!(at, &vec![pt(-0.3, 0.0), pt(0.3, 0.0)]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fu.pads[3].kind, PadKind::Tht { drill: Nm(600_000) });
    assert_eq!(fu.mount, Mount::Tht, "no attribute: from the pads");
    assert_eq!(fu.courtyard.len(), 4);
    assert!(fu.graphics.iter().any(|g| matches!(g.geometry, GraphicGeometry::Circle { filled: true, .. })));
    // Board pads land where KiCad has them: U1.1 at KiCad (121, 60) (180° turns local -1 to +1).
    let pads = crate::board::placed_pads(&p);
    let u1p1 = pads.iter().find(|x| x.refdes == "U1" && x.number == "1").unwrap();
    assert_eq!(u1p1.center, pt(21.0, 10.0));
    assert_eq!(u1p1.layers, ["B.Cu"]);
    let r1p1 = pads.iter().find(|x| x.refdes == "R1" && x.number == "1").unwrap();
    assert_eq!(r1p1.center, pt(10.0, 9.5), "KiCad (110, 60.5)");
    // The mounting hole.
    assert_eq!(b.holes.len(), 1);
    let h = &b.holes[0];
    assert_eq!(
        (h.name.as_str(), h.at, h.drill, h.pad, h.net.as_deref()),
        ("H1", pt(4.0, 16.0), Nm(3_200_000), Some(mmu(6)), Some("GND"))
    );
    // Circuit built from the board: nets named without the root sheet prefix; the single-pad
    // unconnected net is no net.
    assert_eq!(r.circuit, CircuitSource::Built);
    let nets: Vec<&String> = p.circuit().nets.keys().collect();
    assert_eq!(nets, ["GND", "SIG"]);
    assert_eq!(p.circuit().components["R1"].properties.get("MPN"), None, "MPN goes to the part");
    let part = &p.library().parts[&p.circuit().components["R1"].part];
    assert_eq!(part.mpn.as_deref(), Some("RC0402"));
    assert_eq!(part.symbol.pins.iter().map(|x| x.label()).collect::<Vec<_>>(), ["A", "2"]);
    // Copper.
    assert_eq!(b.tracks.len(), 2);
    assert_eq!(b.tracks[0].net.as_deref(), Some("SIG"));
    assert_eq!(b.tracks[1].mid, Some(pt(21.0, 9.0)));
    assert_eq!(b.vias.len(), 2);
    assert_eq!((b.vias[1].from.as_str(), b.vias[1].to.as_str(), b.vias[1].net.as_deref()), ("F.Cu", "In1.Cu", None));
    // Zone and keep-outs.
    let z = &b.zones[0];
    assert_eq!((z.name.as_str(), z.priority, z.pads), ("gnd", 2, PadConnection::Solid));
    assert_eq!(z.layers, ["In1.Cu", "In2.Cu"]);
    assert_eq!(
        (z.clearance, z.thermal_gap, z.thermal_spoke),
        (Some(Nm(300_000)), Some(Nm(400_000)), Some(Nm(350_000)))
    );
    assert_eq!(z.min_width, Some(Nm(250_000)));
    assert_eq!(b.keepouts.len(), 2);
    assert!(b.keepouts[0].layers.is_empty(), "every copper layer");
    assert!(b.keepouts[0].no_tracks && b.keepouts[0].no_vias && b.keepouts[0].no_footprints && !b.keepouts[0].no_pours);
    assert_eq!(b.keepouts[0].name, "keepout");
    assert_eq!(b.keepouts[1].layers, ["F.Cu"]);
    // Graphics: a joined silk polyline, the text, the logo.
    let lines: Vec<&BoardGraphic> = b.graphics.iter().filter(|g| matches!(g.kind, GraphicKind::Line { .. })).collect();
    assert!(
        lines
            .iter()
            .any(|g| matches!(&g.kind, GraphicKind::Line { points, .. } if points.len() == 3 && g.layer == "F.SilkS"))
    );
    assert!(b.graphics.iter().any(|g| matches!(&g.kind, GraphicKind::Text { text, rotation, size, .. }
        if text == "v1.0" && *rotation == Angle::DEG_90 && *size == Nm(1_500_000))));
    assert!(
        lines.iter().any(|g| matches!(&g.kind, GraphicKind::Line { points, .. } if points.len() == 4)),
        "logo polygon closed"
    );
    // UUIDs.
    assert_eq!(r.uuids["t1"], format!("track#{}", b.tracks[0].id.0));
    assert_eq!(r.uuids["z1"], format!("zone#{}", z.id.0));
    assert_eq!(r.uuids["k1"], "keepout:keepout");
    // Everything not imported is reported.
    let c = codes(&d);
    for want in [
        "import.invalid_refdes",
        "import.footprint_layer",
        "import.pad_approximated",
        "import.copper_drawing",
        "import.unsupported_item",
        "import.track_layer",
        "import.zone_fills",
        "import.footprint_text",
        "import.footprint_as_graphics",
        "import.stackup_dielectric",
    ] {
        assert!(c.contains(&want), "{want} missing from {c:?}");
    }
    // invalid designator, F.SilkS line on a bottom footprint, Cmts.User circle of the hole,
    // copper drawing, dimension, track on silkscreen.
    assert_eq!(r.not_imported, 6, "{d:#?}");
}

#[test]
fn errors_and_conflicts() {
    let mut p = Project::new("t");
    let e = import(&mut p, "(kicad_sch (version 20231120))", &opts()).unwrap_err();
    assert_eq!(e.code, "import.not_kicad_pcb");
    let e = import(&mut p, "(kicad_pcb (version 20171130))", &opts()).unwrap_err();
    assert_eq!(e.code, "import.kicad_version");
    let e = import(&mut p, "(kicad_pcb (version 20240108)", &opts()).unwrap_err();
    assert_eq!(e.code, "import.parse");
    import(&mut p, BOARD, &opts()).unwrap();
    let e = import(&mut p, BOARD, &opts()).unwrap_err();
    assert_eq!((e.code, e.kind), ("import.board_not_empty", ImportErrorKind::Conflict));
    // Replace keeps the circuit, matched by designator this time.
    let (r, _) = import(&mut p, BOARD, &BoardImportOptions { replace: true, ..opts() }).unwrap();
    assert_eq!(r.circuit, CircuitSource::Matched);
    assert!(r.library_footprints.is_empty(), "the same footprints are reused");
}

#[test]
fn matches_an_existing_circuit_and_reports_differences() {
    let mut p = Project::new("t");
    import(&mut p, BOARD, &opts()).unwrap();
    // The circuit says R1.2 is on SIG2 instead of GND.
    let c = p.circuit_mut();
    c.nets.get_mut("GND").unwrap().pins.retain(|x| x.to_string() != "R1.2");
    let id = crate::id::ObjectId(9999);
    c.nets.insert(
        "SIG2".into(),
        crate::model::circuit::Net {
            id,
            pins: [PinRef::new("R1", "2")].into_iter().collect(),
            class: None,
            driven: false,
            voltage: None,
            current: None,
            temp_rise: None,
        },
    );
    *p.board_mut() = Board::default();
    let (_, d) = import(&mut p, BOARD, &opts()).unwrap();
    assert!(codes(&d).contains(&"import.net_conflict"), "{:?}", codes(&d));
    assert!(codes(&d).contains(&"import.net_mismatch"), "{:?}", codes(&d));
    // A footprint whose designator the circuit lacks.
    p.circuit_mut().components.remove("U1");
    *p.board_mut() = Board::default();
    let (r, d) = import(&mut p, BOARD, &opts()).unwrap();
    assert!(codes(&d).contains(&"import.component_not_in_circuit"));
    assert!(!p.board().footprints.contains_key("U1"));
    assert_eq!(r.footprints, 1);
}

#[test]
fn origins() {
    let mut p = Project::new("t");
    let board = BOARD.replace("(pad_to_mask_clearance 0)", "(aux_axis_origin 90 80)");
    let (r, _) = import(&mut p, &board, &opts()).unwrap();
    assert_eq!(r.origin, (mmu(90), mmu(80)));
    let mut p = Project::new("t");
    let (r, _) = import(&mut p, &board, &BoardImportOptions { origin: OriginMode::Page, ..opts() }).unwrap();
    assert_eq!(r.origin, (Nm::ZERO, Nm::ZERO));
    assert_eq!(p.board().footprints["R1"].at, pt(110.0, -60.0));
}

use crate::model::board::BoardSide;
use crate::model::footprint::{GraphicGeometry, Mount};
use crate::model::sections::PinRef;
