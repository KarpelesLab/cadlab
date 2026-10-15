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
    // R1: top, 90°; pad 2 custom: the anchor rectangle and the polygon primitive united (Y up).
    let r1 = &b.footprints["R1"];
    assert_eq!((r1.at, r1.rotation, r1.side), (pt(10.0, 10.0), Angle::DEG_90, BoardSide::Top));
    let fp = crate::board::footprint_for(&p, "R1").unwrap();
    assert_eq!(fp.name, "R_Custom");
    let p1 = &fp.pads[0];
    assert_eq!((p1.at, p1.rotation), (pt(-0.5, 0.0), Angle::ZERO));
    assert_eq!(p1.shape, PadShape::RoundRect { w: Nm(600_000), h: Nm(500_000), r: Nm(125_000) });
    assert_eq!(p1.paste, None, "F.Paste: opening equal to the pad");
    let p2 = &fp.pads[1];
    let outline = vec![pt(-0.2, -0.2), pt(-0.2, 0.2), pt(0.6, -0.2), pt(0.6, 0.2)];
    let PadShape::Polygon { points } = &p2.shape else { panic!("{:?}", p2.shape) };
    let mut sorted = points.clone();
    sorted.sort();
    assert_eq!(sorted, outline);
    assert_eq!(p2.at, pt(0.5, 0.0));
    assert_eq!(p2.paste, Some(Paste::None));
    assert_eq!(fp.courtyard, vec![pt(-1.5, 1.0), pt(1.5, 1.0), pt(1.5, -1.0), pt(-1.5, -1.0)]);
    assert_eq!(fp.graphics.len(), 1, "silk lines joined");
    // U1: bottom, KiCad 180° = cadlab 0°, locked; paste windows on pad 3; an oval hole kept as a slot.
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
    assert_eq!(fu.pads[3].slot, Some((Nm(600_000), Nm(1_200_000))), "the oval hole is a slot");
    assert_eq!(fu.mount, Mount::Tht, "no attribute: from the pads");
    assert_eq!(fu.courtyard.len(), 4);
    assert!(fu.graphics.iter().any(|g| matches!(g.geometry, GraphicGeometry::Circle { filled: true, .. })));
    // Board pads land where KiCad has them: U1.1 at KiCad (121, 60) (180° turns local -1 to +1).
    let pads = crate::board::placed_pads(&p);
    let u1p1 = pads.iter().find(|x| x.refdes == "U1" && x.number == "1").unwrap();
    assert_eq!(u1p1.center, pt(21.0, 10.0));
    assert_eq!(u1p1.layers, ["B.Cu"]);
    let slot = pads.iter().find(|x| x.refdes == "U1" && x.number == "4").unwrap().slot.unwrap();
    assert_eq!(slot.0.x, slot.1.x, "a vertical slot");
    assert_eq!((slot.0.y - slot.1.y).abs(), Nm(600_000));
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
    // The logo's filled silkscreen polygon stays an area (a 0-wide outline would print nothing).
    assert!(
        b.graphics.iter().any(|g| g.layer == "F.SilkS"
            && matches!(&g.kind, GraphicKind::Polygon { points, width } if points.len() == 3 && *width == Nm::ZERO)),
        "logo polygon filled: {:?}",
        b.graphics
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
    // dimension, track on silkscreen.
    assert_eq!(r.not_imported, 5, "{d:#?}");
    // The copper drawing is board copper.
    assert!(b.graphics.iter().any(|g| g.layer == "F.Cu" && matches!(g.kind, GraphicKind::Line { .. })));
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

/// A small board exercising the corpus-driven import features: a ring-shaped custom pad around
/// a hole, a circle courtyard, a keep-out frame (outline with a hole), board-only stitching-via
/// and mouse-bite footprints, local settings, a net tie and a text variable.
const FEATURES: &str = r#"(kicad_pcb (version 20240108) (generator pcbnew)
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (37 "F.SilkS" user) (44 "Edge.Cuts" user)
    (47 "F.CrtYd" user))
  (setup (pad_to_mask_clearance 0.05))
  (net 0 "") (net 1 "GND") (net 2 "A")
  (footprint "Mount:Ring" (layer "F.Cu") (at 110 60)
    (property "Reference" "H5") (property "Value" "M2.5")
    (fp_circle (center 0 0) (end 3.2 0) (stroke (width 0.05)) (fill none) (layer "F.CrtYd"))
    (pad "" np_thru_hole circle (at 0 0) (size 3.7 3.7) (drill 3.7) (layers "*.Cu" "*.Mask"))
    (pad "1" smd custom (at 2.425 0) (size 1.15 1.15) (layers "F.Cu" "F.Mask") (net 1 "GND")
      (solder_mask_margin 0.1) (clearance 0.2) (zone_connect 0)
      (options (clearance outline) (anchor circle))
      (primitives (gr_circle (center -2.425 0) (end 0 0) (width 1.15) (fill no)))))
  (footprint "Jumper:NetTie" (layer "F.Cu") (at 120 60)
    (property "Reference" "NT1") (property "Value" "NetTie")
    (net_tie_pad_groups "1, 2") (thermal_width 0.5) (thermal_gap 0.6)
    (pad "1" smd rect (at -0.5 0) (size 0.6 0.6) (layers "F.Cu") (net 1 "GND")
      (thermal_bridge_width 0.3) (thermal_gap 0.4) (thermal_bridge_angle 90))
    (pad "2" smd rect (at 0.5 0) (size 0.6 0.6) (layers "F.Cu") (net 2 "A")
      (thermal_width 0.25) (thermal_bridge_angle 0)))
  (footprint "Stitch:Via" (layer "F.Cu") (at 105 55)
    (property "Reference" "STITCH01") (attr through_hole board_only)
    (pad "1" thru_hole circle (at 0 0) (size 0.6 0.6) (drill 0.3) (layers "*.Cu") (net 1 "GND")))
  (footprint "Stitch:Via" (layer "F.Cu") (at 106 55)
    (property "Reference" "STITCH01") (attr through_hole board_only)
    (pad "1" thru_hole circle (at 0 0) (size 0.6 0.6) (drill 0.3) (layers "*.Cu") (net 1 "GND")))
  (footprint "Panel:MouseBite" (layer "F.Cu") (at 125 52)
    (property "Reference" "mouse-bite-1")
    (pad "" np_thru_hole circle (at -0.5 0) (size 0.5 0.5) (drill 0.5) (layers "*.Cu" "*.Mask"))
    (pad "" np_thru_hole circle (at 0.5 0) (size 0.5 0.5) (drill 0.5) (layers "*.Cu" "*.Mask")))
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
  (gr_text "rev ${VERSION}" (at 115 68) (layer "F.SilkS") (effects (font (size 1 1))))
  (zone (net 0) (net_name "") (layers "F&B.Cu") (name "frame")
    (keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour allowed) (footprints allowed))
    (polygon (pts (xy 100 50) (xy 130 50) (xy 130 70) (xy 100 70)))
    (polygon (pts (xy 101 51) (xy 101 69) (xy 129 69) (xy 129 51)))))"#;

#[test]
fn corpus_features() {
    use crate::geom::poly;
    let pro = r#"{"text_variables": {"VERSION": "1.2"},
        "board": {"design_settings": {"rules": {"min_silk_clearance": 0.0}}}}"#;
    let (k, _) = rules::parse(Some(pro), None).unwrap();
    assert_eq!(k.rules["silk_to_pad"], Nm::ZERO, "a zero silk clearance is a value");
    let mut p = Project::new("t");
    let (r, d) = import(&mut p, FEATURES, &BoardImportOptions { rules: Some(k), ..opts() }).unwrap();
    let c = codes(&d);
    let b = p.board();
    // The ring pad: an annulus (the hole stays free) plus the anchor inside the ring.
    let ring = crate::board::placed_pads(&p).into_iter().find(|pp| pp.refdes == "H5" && pp.number == "1").unwrap();
    assert!(matches!(ring.pad.shape, PadShape::Polygon { .. }), "{:?}", ring.pad.shape);
    let area = poly::area2(&vec![ring.shape.clone()]) as f64 / 2.0 / 1e12;
    let annulus = std::f64::consts::PI * (3.0f64.powi(2) - 1.85f64.powi(2));
    assert!((area - annulus).abs() < 0.05, "ring area {area} mm², annulus {annulus} mm²");
    let hole_center: poly::Point = b.footprints["H5"].at.into();
    assert!(!poly::contains(&vec![ring.shape.clone()], &hole_center), "no copper over the hole");
    // Circle courtyard: one polygon around it, the drawing not kept as well.
    let h5 = crate::board::footprint_for(&p, "H5").unwrap();
    assert!(h5.courtyard.len() > 8, "a polygon around the circle");
    assert!(h5.graphics.iter().all(|g| g.layer != GraphicLayer::Courtyard));
    // Keep-out frame: one keep-out whose outline goes around the hole.
    assert_eq!(b.keepouts.len(), 1);
    let ko: poly::Ring = b.keepouts[0].outline.iter().map(|&q| q.into()).collect::<Vec<poly::Point>>().into();
    let area = (ko.signed_area2().abs() as f64) / 2.0 / 1e12;
    assert!((area - (600.0 - 28.0 * 18.0)).abs() < 1e-6, "frame area {area}");
    // Board-only footprints: stitching vias (one designator for both) as vias, mouse bites
    // (a designator cadlab cannot use) as holes.
    assert_eq!(b.vias.len(), 2);
    assert!(b.vias.iter().all(|v| v.net.as_deref() == Some("GND") && v.drill == Nm(300_000)));
    assert_eq!(b.holes.len(), 2, "{:?}", b.holes);
    assert!(b.holes.iter().all(|h| h.pad.is_none() && h.name.starts_with('H')));
    assert_eq!(r.holes, 2);
    // Text variables replaced.
    assert!(b.graphics.iter().any(|g| matches!(&g.kind, GraphicKind::Text { text, .. } if text == "rev 1.2")));
    for want in ["import.footprint_as_via", "import.footprint_as_holes", "import.zone_outlines", "import.text_variable"]
    {
        assert!(c.contains(&want), "{want} missing from {c:?}");
    }
    // Local settings: the board's mask expansion, the ring pad's mask margin, clearance and
    // zone connection.
    assert_eq!(b.rules.mask_expansion, Nm(50_000));
    let pad = &h5.pads.iter().find(|x| x.number == "1").unwrap().overrides;
    assert_eq!(pad.mask_margin, Some(Nm(100_000)));
    assert_eq!(pad.clearance, Some(Nm(200_000)));
    assert_eq!(pad.zone_connection, Some(PadConnection::None));
    // The net tie: its pads touch nothing else, but their nets are one group.
    let nt = crate::board::footprint_for(&p, "NT1").unwrap();
    assert_eq!(nt.net_ties, vec![vec!["1".to_string(), "2".to_string()]]);
    // Thermal reliefs: footprint and pad gaps and widths (both width tokens), a spoke angle
    // unless it is KiCad's default for the pad shape.
    assert_eq!((nt.overrides.thermal_spoke, nt.overrides.thermal_gap), (Some(Nm(500_000)), Some(Nm(600_000))));
    let (p1, p2) = (&nt.pads[0].overrides, &nt.pads[1].overrides);
    assert_eq!((p1.thermal_spoke, p1.thermal_gap, p1.thermal_angle), (Some(Nm(300_000)), Some(Nm(400_000)), None));
    assert_eq!((p2.thermal_spoke, p2.thermal_gap, p2.thermal_angle), (Some(Nm(250_000)), None, Some(Angle::ZERO)));
}

/// Trapezoid, chamfered and drill-offset pads on a top footprint (90°) and a bottom one, each
/// next to a custom pad of the same outline (or a pad without offset at the copper center): the
/// conversion custom pads already go through.
const PAD_SHAPES: &str = r#"(kicad_pcb (version 20240108) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "")
  (footprint "Test:Shapes" (layer "F.Cu") (at 110 60 90)
    (property "Reference" "J1")
    PADS)
  (footprint "Test:Shapes" (layer "B.Cu") (at 120 60)
    (property "Reference" "J2")
    PADS)
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
)"#;

fn pad_shapes_board(side: &str) -> String {
    let (cu, mask, a) = if side == "F" { ("F.Cu", "F.Mask", " 90") } else { ("B.Cu", "B.Mask", "") };
    let custom = |n: &str, at: &str, pts: &str| {
        format!(
            "(pad \"{n}\" smd custom (at {at}{a}) (size 0.01 0.01) (layers \"{cu}\" \"{mask}\")
      (options (clearance outline) (anchor circle))
      (primitives (gr_poly (pts {pts}) (width 0) (fill yes))))"
        )
    };
    [
        format!("(pad \"1\" smd trapezoid (at -2 0{a}) (size 1 0.6) (rect_delta 0.2 0) (layers \"{cu}\" \"{mask}\"))"),
        custom("2", "-2 2", "(xy -0.5 0.4) (xy -0.5 -0.4) (xy 0.5 -0.2) (xy 0.5 0.2)"),
        format!("(pad \"3\" smd trapezoid (at -2 4{a}) (size 1 0.6) (rect_delta 0 0.3) (layers \"{cu}\" \"{mask}\"))"),
        custom("4", "-2 6", "(xy -0.65 0.3) (xy -0.35 -0.3) (xy 0.35 -0.3) (xy 0.65 0.3)"),
        format!(
            "(pad \"5\" smd roundrect (at 0 0{a}) (size 1 0.8) (layers \"{cu}\" \"{mask}\") (roundrect_rratio 0) (chamfer_ratio 0.25) (chamfer top_left bottom_right))"
        ),
        custom("6", "0 2", "(xy -0.3 -0.4) (xy 0.5 -0.4) (xy 0.5 0.2) (xy 0.3 0.4) (xy -0.5 0.4) (xy -0.5 -0.2)"),
        format!(
            "(pad \"7\" smd roundrect (at 0 4{a}) (size 1 0.8) (layers \"{cu}\" \"{mask}\") (roundrect_rratio 0.25) (chamfer_ratio 0.25) (chamfer top_right))"
        ),
        format!("(pad \"8\" thru_hole rect (at 2 0{a}) (size 1.5 1) (drill 0.6 (offset 0.3 0.1)) (layers \"*.Cu\" \"*.Mask\"))"),
        format!("(pad \"9\" thru_hole rect (at 2 0{a}) (size 1.5 1) (drill 0.6) (layers \"*.Cu\" \"*.Mask\"))"),
        format!("(pad \"10\" smd rect (at 2.3 0.1{a}) (size 1.5 1) (layers \"{cu}\" \"{mask}\"))"),
    ]
    .join("\n    ")
}

#[test]
fn trapezoid_chamfered_and_drill_offset_pads() {
    use crate::model::footprint::Corner;
    let text = PAD_SHAPES.replacen("PADS", &pad_shapes_board("F"), 1).replacen("PADS", &pad_shapes_board("B"), 1);
    let mut p = Project::new("t");
    let (_, d) = import(&mut p, &text, &opts()).unwrap();
    assert!(!codes(&d).contains(&"import.pad_approximated"), "{:?}", codes(&d));
    let pads = crate::board::placed_pads(&p);
    let get = |r: &str, n: &str| pads.iter().find(|pp| pp.refdes == r && pp.number == n).unwrap();
    // The board copper of a pad relative to its center, as sorted vertices.
    let rel = |pp: &crate::board::PlacedPad| {
        let mut v: Vec<(i64, i64)> =
            pp.shape.outer.0.iter().map(|q| (q.x - pp.center.x.0, q.y - pp.center.y.0)).collect();
        v.sort();
        v
    };
    let close = |a: &[(i64, i64)], b: &[(i64, i64)]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x.0 - y.0).abs() <= 2 && (x.1 - y.1).abs() <= 2)
    };
    for r in ["J1", "J2"] {
        for (shape, custom) in [("1", "2"), ("3", "4"), ("5", "6")] {
            let (a, b) = (rel(get(r, shape)), rel(get(r, custom)));
            assert!(close(&a, &b), "{r}.{shape} {a:?} vs {r}.{custom} {b:?}");
        }
        // The drill offset moves the copper: pad 8's copper is pad 10's, its hole pad 9's.
        let (p8, p9, p10) = (get(r, "8"), get(r, "9"), get(r, "10"));
        assert_eq!(p8.hole_at, p9.hole_at, "{r}");
        assert_eq!(p8.center, p10.center, "{r}");
        assert!(close(&rel(p8), &rel(p10)), "{r}");
        assert_ne!(p8.center, p8.hole_at);
    }
    // The model: shapes in cadlab pad orientation (Y up; the bottom side mirrored in X).
    let j1 = crate::board::footprint_for(&p, "J1").unwrap();
    let j2 = crate::board::footprint_for(&p, "J2").unwrap();
    let pad = |f: &crate::model::footprint::Footprint, n: &str| f.pads.iter().find(|x| x.number == n).unwrap().clone();
    let trapezoid = |dx: i64, dy: i64| PadShape::Trapezoid { w: mmu(1), h: Nm(600_000), dx: Nm(dx), dy: Nm(dy) };
    assert_eq!(pad(j1, "1").shape, trapezoid(200_000, 0));
    assert_eq!(pad(j2, "1").shape, trapezoid(-200_000, 0));
    assert_eq!(pad(j1, "3").shape, trapezoid(0, 300_000));
    let chamfered = |corners: Vec<Corner>, r: i64| PadShape::ChamferedRect {
        w: mmu(1),
        h: Nm(800_000),
        r: Nm(r),
        chamfer: Nm(200_000),
        corners,
    };
    assert_eq!(pad(j1, "5").shape, chamfered(vec![Corner::TopLeft, Corner::BottomRight], 0));
    assert_eq!(pad(j2, "5").shape, chamfered(vec![Corner::TopRight, Corner::BottomLeft], 0));
    assert_eq!(pad(j1, "7").shape, chamfered(vec![Corner::TopRight], 200_000));
    let p8 = pad(j1, "8");
    assert_eq!(p8.drill_offset, Some(pt(-0.3, 0.1)));
    assert_eq!(p8.hole_center(), pad(j1, "9").at);

    // Consumers: Gerber macros, IPC-2581 primitives, drill files at the hole, DSN, DRC.
    let files = crate::fabout::all(&p, &Default::default());
    let file = |f: &str| files.iter().find(|x| x.function.starts_with(f)).map(|x| x.content.clone()).unwrap();
    let top = file("Copper,L1");
    assert!(top.contains("Pad trapezoid 1x0.6 delta 0.2 0"), "{top}");
    assert!(top.contains("Pad chamfered 1x0.8 r0 chamfer 0.2 TLBR"), "{top}");
    let bottom = file("Copper,L2");
    assert!(bottom.contains("Pad trapezoid 1x0.6 delta 0.2 0"), "mirrored back on the bottom: {bottom}");
    let ipc = crate::fabout::ipc2581::document(&p, &Default::default()).content;
    assert!(ipc.contains("<RectCham"), "{ipc}");
    let hole = |r: &str| get(r, "8").hole_at;
    let holes = crate::fabout::holes(&p);
    for r in ["J1", "J2"] {
        let h = holes.iter().find(|h| h.pad == Some((r.to_string(), "8".to_string()))).unwrap();
        assert_eq!(h.at, hole(r));
    }
    let dsn = crate::specctra::export::export(&p, "t", &Default::default()).dsn.write();
    assert!(dsn.contains("poly_"), "{dsn}");
    let drc = crate::drc::check(&p);
    // The offset hole is 0.1 mm closer to one long edge: (1 − 0.6) / 2 − 0.1.
    let rings: Vec<&str> =
        drc.iter().filter(|d| d.code == "drc.pad_annular_ring").map(|d| d.message.as_str()).collect();
    assert_eq!(rings.len(), 2, "{rings:?}");
    assert!(rings.iter().all(|m| m.contains("annular ring 0.1mm")), "{rings:?}");

    // Exported back and imported again: the same land patterns and board copper.
    let out = crate::kicad_pcb::to_kicad_pcb(&p);
    // As written in the source on both sides (the bottom side mirrors back).
    assert_eq!(out.matches("(rect_delta 0.2 0)").count(), 2, "{out}");
    assert!(out.contains("(chamfer_ratio 0.25) (chamfer top_left bottom_right)"), "{out}");
    assert!(out.contains("(drill 0.6 (offset 0.3 0.1))"), "{out}");
    let mut q = Project::new("t");
    let (_, d) = import(&mut q, &out, &opts()).unwrap();
    assert!(!codes(&d).contains(&"import.pad_approximated"), "{:?}", codes(&d));
    for r in ["J1", "J2"] {
        let (a, b) = (crate::board::footprint_for(&p, r).unwrap(), crate::board::footprint_for(&q, r).unwrap());
        assert!(footprint::equivalent(a, b), "{r}: {:?}\n{:?}", a.pads, b.pads);
    }
    let again = crate::board::placed_pads(&q);
    for pp in &pads {
        let qq = again.iter().find(|x| x.refdes == pp.refdes && x.number == pp.number).unwrap();
        assert_eq!((pp.center, pp.hole_at), (qq.center, qq.hole_at), "{}.{}", pp.refdes, pp.number);
        assert!(close(&rel(pp), &rel(qq)), "{}.{}", pp.refdes, pp.number);
    }
}

/// KiCad 9 pad stacks on a four-layer board: a `front_inner_back` pad (rectangle on top,
/// circle inside, oval below) and a `custom` one (a circle per inner layer) on a top footprint,
/// and the same footprint flipped to the bottom side (its stack keyed by board layers, so its
/// own side's shape is `B.Cu`'s and its inner layers are reversed). A track of another net on
/// `In1.Cu` passes where the top shape would be (clear of the inner circle), the same track on
/// `F.Cu` hits it, and a pour on `In2.Cu` fills up to the inner circle.
const PAD_STACKS: &str = r#"(kicad_pcb (version 20241229) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (1 "In1.Cu" signal) (2 "In2.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "")
  (net 1 "A")
  (net 2 "B")
  (footprint "Test:Stack" (layer "F.Cu") (at 110 60)
    (property "Reference" "J1")
    (pad "1" thru_hole rect (at 0 0) (size 2 2) (drill 0.8) (layers "*.Cu" "*.Mask") (net 1 "A")
      (padstack (mode front_inner_back)
        (layer "Inner" (shape circle) (size 1.2 1.2))
        (layer "B.Cu" (shape oval) (size 2 1.5))))
    (pad "2" thru_hole circle (at 4 0) (size 1.7 1.7) (drill 0.8) (layers "*.Cu" "*.Mask")
      (padstack (mode custom)
        (layer "In1.Cu" (shape circle) (size 1 1))
        (layer "In2.Cu" (shape circle) (size 1.4 1.4))
        (layer "B.Cu" (shape circle) (size 1.7 1.7))))
    (pad "3" smd rect (at 8 0) (size 1 1) (layers "F.Cu" "F.Mask")
      (padstack (mode front_inner_back) (layer "B.Cu" (shape circle) (size 0.8 0.8)))))
  (footprint "Test:Stack" (layer "B.Cu") (at 120 60)
    (property "Reference" "J2")
    (pad "1" thru_hole oval (at 0 0) (size 2 1.5) (drill 0.8) (layers "*.Cu" "*.Mask")
      (padstack (mode front_inner_back)
        (layer "Inner" (shape circle) (size 1.2 1.2))
        (layer "B.Cu" (shape rect) (size 2 2))))
    (pad "2" thru_hole circle (at -4 0) (size 1.7 1.7) (drill 0.8) (layers "*.Cu" "*.Mask")
      (padstack (mode custom)
        (layer "In1.Cu" (shape circle) (size 1.4 1.4))
        (layer "In2.Cu" (shape circle) (size 1 1))
        (layer "B.Cu" (shape circle) (size 1.7 1.7))))
    (pad "3" smd circle (at -8 0) (size 0.8 0.8) (layers "B.Cu" "B.Mask")
      (padstack (mode front_inner_back) (layer "B.Cu" (shape rect) (size 1 1)))))
  (segment (start 108 61) (end 111.5 61) (width 0.2) (layer "In1.Cu") (net 2) (uuid "t1"))
  (segment (start 108 61) (end 111.5 61) (width 0.2) (layer "F.Cu") (net 2) (uuid "t2"))
  (zone (net 0) (net_name "") (layers "In2.Cu") (uuid "z1") (name "pour") (hatch edge 0.5)
    (connect_pads (clearance 0.2)) (min_thickness 0.2) (fill yes)
    (polygon (pts (xy 106 56) (xy 116 56) (xy 116 64) (xy 106 64))))
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
)"#;

#[test]
fn per_layer_pad_stacks() {
    use crate::board::{ItemRef, PlacedPad};
    use crate::model::footprint::{PadStack, StackLayer};
    let mut p = Project::new("t");
    let (_, d) = import(&mut p, PAD_STACKS, &opts()).unwrap();
    assert!(!codes(&d).contains(&"import.pad_approximated"), "{:?}", codes(&d));
    let circle = |d: f64| PadShape::Circle { d: Nm((d * 1e6).round() as i64) };
    // The model: the footprint's side first, layers counted from it; both placements alike.
    let want1 = PadStack {
        inner: Some(circle(1.2)),
        back: Some(PadShape::Oval { w: mmu(2), h: Nm(1_500_000) }),
        ..Default::default()
    };
    let want2 = PadStack { layers: [(1, circle(1.0)), (2, circle(1.4))].into(), ..Default::default() };
    for r in ["J1", "J2"] {
        let f = crate::board::footprint_for(&p, r).unwrap();
        let pad = |n: &str| f.pads.iter().find(|x| x.number == n).unwrap();
        assert_eq!(pad("1").shape, PadShape::Rect { w: mmu(2), h: mmu(2) }, "{r}");
        assert_eq!(pad("1").stack.as_ref(), Some(&want1), "{r}");
        assert_eq!(pad("2").shape, circle(1.7), "{r}");
        assert_eq!(pad("2").stack.as_ref(), Some(&want2), "{r}");
        assert_eq!(pad("2").shape_on(StackLayer::Inner(2)), &circle(1.4));
        assert_eq!(pad("1").size(), (mmu(2), mmu(2)));
        // An SMD pad takes its stack's entry for its own copper layer (`B.Cu` on the bottom).
        assert_eq!(pad("3").shape, PadShape::Rect { w: mmu(1), h: mmu(1) }, "{r}");
        assert!(pad("3").stack.is_none());
    }
    // Board copper per layer: J1 counts from the top, J2 from the bottom.
    let pads = crate::board::placed_pads(&p);
    let get = |r: &str, n: &str| pads.iter().find(|pp| pp.refdes == r && pp.number == n).unwrap();
    let width = |pp: &PlacedPad, l: &str| {
        let xs: Vec<i64> = pp.shape_on(l).outer.0.iter().map(|q| q.x).collect();
        xs.iter().max().unwrap() - xs.iter().min().unwrap()
    };
    let near = |a: i64, b: f64| (a - (b * 1e6) as i64).abs() <= 2_000;
    let (j1, j2) = (get("J1", "1"), get("J2", "1"));
    for pp in [j1, j2] {
        assert!(near(width(pp, "F.Cu"), 2.0) && near(width(pp, "B.Cu"), 2.0));
        assert!(near(width(pp, "In1.Cu"), 1.2) && near(width(pp, "In2.Cu"), 1.2));
    }
    // The top footprint's front is the rectangle, the bottom one's is on B.Cu.
    assert_eq!(j1.pad_shape_on("F.Cu"), &PadShape::Rect { w: mmu(2), h: mmu(2) });
    assert_eq!(j2.pad_shape_on("B.Cu"), &PadShape::Rect { w: mmu(2), h: mmu(2) });
    assert!(matches!(j2.pad_shape_on("F.Cu"), PadShape::Oval { .. }));
    assert!(near(width(get("J1", "2"), "In1.Cu"), 1.0) && near(width(get("J1", "2"), "In2.Cu"), 1.4));
    assert!(near(width(get("J2", "2"), "In1.Cu"), 1.4) && near(width(get("J2", "2"), "In2.Cu"), 1.0));

    // Copper items: one per shape, joined into one island through the hole.
    let items = crate::board::copper_items(&p);
    let isl = crate::board::islands(&items);
    let of = |r: &str, n: &str| -> Vec<usize> {
        (0..items.len()).filter(|&i| items[i].item == ItemRef::Pad(r.into(), n.into())).collect()
    };
    let j1i = of("J1", "1");
    assert_eq!(j1i.len(), 3);
    assert_eq!(items[j1i[1]].layers, vec!["In1.Cu".to_string(), "In2.Cu".to_string()]);
    assert!(j1i.iter().all(|&i| isl[i] == isl[j1i[0]]));
    assert_eq!(of("J1", "2").len(), 3, "F.Cu and B.Cu alike, two inner circles");

    // DRC: the F.Cu track hits the rectangle, the In1.Cu one clears the inner circle.
    let track = |l: &str| p.board().tracks.iter().find(|t| t.layer == l).unwrap().id.0;
    let drc = crate::drc::check(&p);
    let about = |id: u64| {
        drc.iter()
            .filter(|d| d.severity == crate::diag::Severity::Error && d.message.contains(&format!("track#{id}")))
            .count()
    };
    assert!(about(track("F.Cu")) > 0, "{drc:?}");
    assert_eq!(about(track("In1.Cu")), 0, "{drc:?}");
    // The annular ring is the narrowest layer's: pad 2's 1 mm inner circle on its 0.8 mm hole.
    let rings: Vec<&str> =
        drc.iter().filter(|d| d.code == "drc.pad_annular_ring").map(|d| d.message.as_str()).collect();
    assert_eq!(rings.len(), 2, "{rings:?}");
    assert!(rings.iter().all(|m| m.contains(".2 annular ring 0.1mm")), "{rings:?}");

    // Zone fill on In2.Cu reaches inside the top rectangle's outline, outside the inner circle.
    let probe = |x: i64, y: i64| {
        use crate::geom::poly::{Point as P, Polygon};
        let s = 50_000;
        Polygon::new(
            vec![P::new(x - s, y - s), P::new(x + s, y - s), P::new(x + s, y + s), P::new(x - s, y + s)],
            vec![],
        )
    };
    let fill: Vec<_> = items
        .iter()
        .filter(|it| matches!(&it.item, ItemRef::Zone(_, l, _) if l == "In2.Cu"))
        .flat_map(|it| it.shape.clone())
        .collect();
    let (cx, cy) = (j1.center.x.0, j1.center.y.0);
    assert!(fill.iter().any(|pg| crate::geom::poly::contains(pg, &probe(cx + 920_000, cy))), "no fill at the pad");
    assert!(!fill.iter().any(|pg| crate::geom::poly::intersects(pg, &probe(cx + 600_000, cy))));

    // Outputs: Gerber flashes per layer, IPC-2581 per layer, DSN padstacks per layer.
    let files = crate::fabout::all(&p, &Default::default());
    let file = |f: &str| files.iter().find(|x| x.function.starts_with(f)).map(|x| x.content.clone()).unwrap();
    let (l1, l2, l4) = (file("Copper,L1"), file("Copper,L2"), file("Copper,L4"));
    assert!(l1.contains("R,2X2") && l1.contains("O,2X1.5") && !l1.contains("C,1.2*"), "{l1}");
    assert!(l2.contains("C,1.2*") && l2.contains("C,1*") && l2.contains("C,1.4*") && !l2.contains("R,2X2"), "{l2}");
    assert!(l4.contains("R,2X2") && l4.contains("O,2X1.5"), "{l4}");
    let ipc = crate::fabout::ipc2581::document(&p, &Default::default()).content;
    assert!(ipc.contains("<Circle diameter=\"1.2\"/>") && ipc.contains("<LayerFeature layerRef=\"In1.Cu\">"));
    let dsn = crate::specctra::export::export(&p, "t", &Default::default()).dsn.write();
    for s in ["(rect F.Cu -1000 -1000 1000 1000)", "(circle In1.Cu 1200)", "(path B.Cu 1500 -250 0 250 0)"] {
        assert!(dsn.contains(s), "{s}: {dsn}");
    }
    assert!(dsn.contains("(circle In1.Cu 1000)") && dsn.contains("(circle In2.Cu 1400)"), "{dsn}");
    assert!(dsn.contains("(padstack tht_d800_rect_2000x2000_stack_"), "{dsn}");

    // Exported back (stacks keyed by board layer again) and imported: the same footprints.
    let out = crate::kicad_pcb::to_kicad_pcb(&p);
    assert_eq!(out.matches("(padstack (mode front_inner_back)").count(), 2, "{out}");
    assert_eq!(out.matches("(padstack (mode custom)").count(), 2, "{out}");
    assert!(out.contains("(layer \"Inner\" (shape circle) (size 1.2 1.2))"), "{out}");
    let mut q = Project::new("t");
    let (_, d) = import(&mut q, &out, &opts()).unwrap();
    assert!(!codes(&d).contains(&"import.pad_approximated"), "{:?}", codes(&d));
    for r in ["J1", "J2"] {
        let (a, b) = (crate::board::footprint_for(&p, r).unwrap(), crate::board::footprint_for(&q, r).unwrap());
        assert!(footprint::equivalent(a, b), "{r}: {:?}\n{:?}", a.pads, b.pads);
    }
    // The project format keeps the stack.
    let f = crate::board::footprint_for(&p, "J1").unwrap();
    let json = serde_json::to_string(f).unwrap();
    let back: crate::model::footprint::Footprint = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, f);
}

/// Courtyards in pieces and on the other side, rule areas sharing a name, a designator with a
/// space (corpus: tinytapeout-demo's carrier, corne-cherry's key switches, sweep's power switch).
const PIECES: &str = r#"(kicad_pcb (version 20240108) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user) (46 "B.CrtYd" user) (47 "F.CrtYd" user))
  (net 0 "") (net 1 "A") (net 2 "B")
  (footprint "Test:Carrier" (layer "F.Cu") (at 110 60)
    (property "Reference" "J5")
    (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net 1 "A"))
    (fp_rect (start -2 -3) (end 2 3) (stroke (width 0.12)) (fill no) (layer "F.CrtYd"))
    (fp_rect (start 10 -1) (end 12 1) (stroke (width 0.12)) (fill no) (layer "F.CrtYd")))
  (footprint "Test:Switch" (layer "B.Cu") (at 118 66)
    (property "Reference" "SW1")
    (pad "1" thru_hole circle (at 0 0) (size 1.5 1.5) (drill 1) (layers "*.Cu" "*.Mask") (net 2 "B"))
    (fp_line (start -3 -3) (end 3 -3) (stroke (width 0.05)) (layer "F.CrtYd"))
    (fp_line (start 3 -3) (end 3 3) (stroke (width 0.05)) (layer "F.CrtYd"))
    (fp_line (start 3 3) (end -3 3) (stroke (width 0.05)) (layer "F.CrtYd"))
    (fp_line (start -3 3) (end -3 -3) (stroke (width 0.05)) (layer "F.CrtYd")))
  (footprint "Test:Diode" (layer "B.Cu") (at 118 64)
    (property "Reference" "D1")
    (pad "1" smd rect (at 0 0) (size 1 1) (layers "B.Cu") (net 2 "B"))
    (fp_rect (start -1 -1) (end 1 1) (stroke (width 0.05)) (fill no) (layer "B.CrtYd")))
  (footprint "Test:Spdt" (layer "B.Cu") (at 104 54)
    (property "Reference" "POWER SW")
    (pad "1" smd rect (at 0 0) (size 1 1) (layers "B.Cu") (net 1 "A"))
    (fp_rect (start -1 -1) (end 1 1) (stroke (width 0.05)) (fill no) (layer "B.CrtYd")))
  (zone (net 0) (net_name "") (layer "F.Cu") (name "X") (hatch edge 0.5)
    (keepout (tracks allowed) (vias allowed) (pads allowed) (copperpour allowed) (footprints allowed))
    (polygon (pts (xy 101 51) (xy 103 51) (xy 103 53) (xy 101 53))))
  (zone (net 0) (net_name "") (layer "F.Cu") (name "X") (hatch edge 0.5)
    (keepout (tracks allowed) (vias allowed) (pads allowed) (copperpour allowed) (footprints allowed))
    (polygon (pts (xy 105 51) (xy 107 51) (xy 107 53) (xy 105 53))))
  (gr_rect (start 100 50) (end 130 70) (stroke (width 0.1)) (fill none) (layer "Edge.Cuts"))
)"#;

#[test]
fn courtyard_pieces_shared_area_names_and_renamed_designators() {
    let dru = r#"(version 1)
(rule "in x" (condition "A.enclosedByArea('X')") (constraint clearance (min 0.1mm)))"#;
    let (k, _) = rules::parse(None, Some(dru)).unwrap();
    let mut p = Project::new("t");
    let (_, d) = import(&mut p, PIECES, &BoardImportOptions { rules: Some(k), ..opts() }).unwrap();
    let c = codes(&d);
    // Two separate courtyard rectangles: the larger is the courtyard, the other a piece.
    let j5 = crate::board::footprint_for(&p, "J5").unwrap();
    assert_eq!(j5.courtyard.len(), 4);
    let xs: Vec<i64> = j5.courtyard.iter().map(|q| q.x.0).collect();
    assert_eq!((xs.iter().min(), xs.iter().max()), (Some(&-2_000_000), Some(&2_000_000)));
    let pieces = crate::board::placed_courtyards(&p, "J5");
    assert_eq!(pieces.len(), 2);
    assert!(pieces.iter().all(|(_, s)| *s == BoardSide::Top));
    // A bottom footprint whose courtyard is on F.CrtYd: no generated courtyard, the drawing
    // on the top side; the bottom diode inside it does not overlap it.
    assert!(!c.contains(&"import.courtyard_generated"), "{c:?}");
    let sw = crate::board::footprint_for(&p, "SW1").unwrap();
    assert!(sw.courtyard.is_empty());
    let sides: Vec<BoardSide> = crate::board::placed_courtyards(&p, "SW1").into_iter().map(|(_, s)| s).collect();
    assert_eq!(sides, [BoardSide::Top]);
    let overlaps: Vec<String> = crate::drc::check(&p)
        .into_iter()
        .filter(|d| d.code == "drc.courtyard_overlap")
        .map(|d| d.message.to_string())
        .collect();
    assert!(overlaps.is_empty(), "{overlaps:?}");
    // The designator with a space is kept under a usable one.
    assert!(c.contains(&"import.refdes_renamed") && !c.contains(&"import.invalid_refdes"), "{c:?}");
    assert!(p.board().footprints.contains_key("POWERSW1"));
    // Both areas named X keep their rule.
    let names: Vec<(&str, Option<&str>)> =
        p.board().custom_rules.iter().map(|r| (r.name.as_str(), r.scope.in_area.as_deref())).collect();
    assert_eq!(names, [("in x", Some("X")), ("in x (X_2)", Some("X_2"))]);
}
