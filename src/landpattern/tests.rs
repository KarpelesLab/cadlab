use super::packages::parse;
use super::*;
use crate::geom::BBox;
use crate::model::footprint::{GraphicGeometry, GraphicLayer};

fn um(v: i64) -> Nm {
    Nm::from_um(v)
}

fn gen_named(name: &str, kind: ChipKind) -> Footprint {
    let spec = parse(name, kind).unwrap_or_else(|e| panic!("{name}: {e}"));
    generate(&spec, &GenOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn pad<'a>(fp: &'a Footprint, n: &str) -> &'a Pad {
    fp.pads.iter().find(|p| p.number == n).unwrap_or_else(|| panic!("no pad {n}"))
}

#[test]
fn dims_parse() {
    assert_eq!(Dim::parse("1.0±0.05mm").unwrap(), Dim { min: um(950), max: um(1050) });
    assert_eq!(Dim::parse("1.0+-0.05mm").unwrap(), Dim { min: um(950), max: um(1050) });
    assert_eq!(Dim::parse("0.15..0.35mm").unwrap(), Dim { min: um(150), max: um(350) });
    assert_eq!(Dim::parse("0.15mm..0.35mm").unwrap(), Dim { min: um(150), max: um(350) });
    assert_eq!(Dim::parse("1mm").unwrap(), Dim::exact(um(1000)));
    assert!(Dim::parse("1.0").is_err());
    assert_eq!(Dim::parse("0.95..1.05mm").unwrap().to_string(), "0.95mm..1.05mm");
}

#[test]
fn ipc_names() {
    assert_eq!(gen_named("0402", ChipKind::Resistor).name, "RESC1005X40N");
    assert_eq!(gen_named("0603", ChipKind::Capacitor).name, "CAPC1608X87N");
    assert_eq!(gen_named("1608M", ChipKind::Capacitor).name, "CAPC1608X87N");
    assert_eq!(gen_named("0805", ChipKind::Led).name, "LEDC2012X135N");
    assert_eq!(gen_named("SOIC-8", ChipKind::Resistor).name, "SOIC127P600X175-8N");
    assert_eq!(gen_named("TSSOP-20", ChipKind::Resistor).name, "SOP65P640X120-20N");
    assert_eq!(gen_named("SOT-23-5", ChipKind::Resistor).name, "SOT95P280X145-5N");
    assert_eq!(gen_named("SOT-25", ChipKind::Resistor).name, "SOT95P280X145-5N");
    assert_eq!(gen_named("SC-74A", ChipKind::Resistor).name, "SOT95P280X145-5N");
    assert_eq!(gen_named("TO-236-3", ChipKind::Resistor).name, "SOT95P237X112-3N");
    assert_eq!(gen_named("LQFP-48", ChipKind::Resistor).name, "QFP50P900X900X160-48N");
    assert_eq!(gen_named("QFN-32 5x5mm P0.5mm EP3.1mm", ChipKind::Resistor).name, "QFN50P500X500X90-33N");
    assert_eq!(gen_named("DFN-8 2x3mm P0.5mm EP0.9x2.4mm H0.8mm", ChipKind::Resistor).name, "SON50P300X200X80-9N");
    assert_eq!(gen_named("PinHeader 1x04", ChipKind::Resistor).name, "PinHeader_1x04_P2.54mm");
}

#[test]
fn soic8_matches_hand_calculation() {
    let fp = gen_named("SOIC-8", ChipKind::Resistor);
    assert_eq!(fp.pads.len(), 8);
    let p1 = pad(&fp, "1");
    // IPC zero orientation: pin 1 top-left; rows along Y at 1.27 mm pitch.
    assert_eq!(p1.at, Point::new(-Nm(2_472_500), um(1905)));
    assert_eq!(p1.shape.size(), (um(1965), um(580)));
    assert_eq!(pad(&fp, "4").at, Point::new(-Nm(2_472_500), -um(1905)));
    assert_eq!(pad(&fp, "5").at, Point::new(Nm(2_472_500), -um(1905)));
    assert_eq!(pad(&fp, "8").at, Point::new(Nm(2_472_500), um(1905)));
}

#[test]
fn sot23_pin_slots() {
    let fp = gen_named("SOT-23", ChipKind::Resistor);
    let ys: Vec<(String, Nm, Nm)> = fp.pads.iter().map(|p| (p.number.clone(), p.at.x, p.at.y)).collect();
    assert_eq!(ys.len(), 3);
    assert!(ys[0].1 < Nm::ZERO && ys[0].2 == um(950)); // pin 1 top-left
    assert!(ys[1].1 < Nm::ZERO && ys[1].2 == -um(950)); // pin 2 bottom-left
    assert!(ys[2].1 > Nm::ZERO && ys[2].2 == Nm::ZERO); // pin 3 right middle

    let fp = gen_named("SOT-23-5", ChipKind::Resistor);
    assert_eq!(pad(&fp, "4").at.y, -um(950));
    assert_eq!(pad(&fp, "5").at.y, um(950));
    assert!(pad(&fp, "5").at.x > Nm::ZERO);
}

#[test]
fn quad_numbering_counter_clockwise() {
    let fp = gen_named("LQFP-48", ChipKind::Resistor);
    let (p1, p12, p13, p25, p37) = (pad(&fp, "1"), pad(&fp, "12"), pad(&fp, "13"), pad(&fp, "25"), pad(&fp, "37"));
    assert!(p1.at.x < Nm::ZERO && p1.at.y > Nm::ZERO); // left side, top
    assert!(p12.at.x < Nm::ZERO && p12.at.y < Nm::ZERO); // left side, bottom
    assert!(p13.at.y < Nm::ZERO && p13.at.x < Nm::ZERO); // bottom side, left
    assert!(p25.at.x > Nm::ZERO && p25.at.y < Nm::ZERO); // right side, bottom
    assert!(p37.at.y > Nm::ZERO && p37.at.x > Nm::ZERO); // top side, right
    // Bottom/top pads are vertical.
    let (w, h) = p13.shape.size();
    assert!(h > w);
    // Neighbours keep the minimum gap.
    let gap = (pad(&fp, "2").at.y - p1.at.y).abs() - p1.shape.size().1;
    assert!(gap >= GenOptions::default().min_pad_gap, "gap {gap}");
}

#[test]
fn exposed_pad_and_paste() {
    let fp = gen_named("QFN-32 5x5mm P0.5mm EP3.1mm", ChipKind::Resistor);
    let ep = pad(&fp, "33");
    assert_eq!(ep.at, Point::ORIGIN);
    assert_eq!(ep.shape.size(), (um(3100), um(3100)));
    match &ep.paste {
        Some(Paste::Windows { size, at }) => {
            assert_eq!(at.len(), 16);
            let coverage = (size.0.0 as f64 * size.1.0 as f64 * 16.0) / (3100e3 * 3100e3);
            assert!((0.5..0.65).contains(&coverage), "coverage {coverage}");
        }
        other => panic!("expected windows, got {other:?}"),
    }
}

#[test]
fn silkscreen_clears_pads() {
    let opts = GenOptions::default();
    for name in [
        "0402",
        "0805",
        "SOIC-8",
        "SOT-23-5",
        "LQFP-48",
        "QFN-32 5x5mm P0.5mm EP3.1mm",
        "PinHeader 2x05",
        "SOT-223",
        "DPAK",
        "D2PAK",
        "SOD-123",
        "SOD-523",
        "SOD-123F",
        "MINIMELF",
        "SMA",
        "SMC",
        "DIP-8",
        "DIP-28W",
        "BGA-64 8x8 P0.8mm 6x6mm",
    ] {
        let fp = gen_named(name, ChipKind::Led);
        for g in &fp.graphics {
            let GraphicGeometry::Path { points } = &g.geometry else {
                continue;
            };
            for pt in points {
                for p in &fp.pads {
                    let (w, h) = p.shape.size();
                    let dx = (pt.x - p.at.x).abs().0 - w.0 / 2;
                    let dy = (pt.y - p.at.y).abs().0 - h.0 / 2;
                    let clear = dx.max(dy);
                    assert!(
                        clear >= opts.silk_clearance.0 + opts.silk_width.0 / 2 - 2,
                        "{name}: silk point {pt:?} too close to pad {} ({clear} nm)",
                        p.number
                    );
                }
            }
        }
        // Courtyard contains every pad.
        let cy = BBox::of_points(fp.courtyard.iter().copied()).unwrap();
        for p in &fp.pads {
            assert!(cy.contains(p.at), "{name}: pad {} outside courtyard", p.number);
        }
    }
}

#[test]
fn pin_header_layout() {
    let fp = gen_named("PinHeader 2x03", ChipKind::Resistor);
    assert_eq!(fp.mount, Mount::Tht);
    assert_eq!(pad(&fp, "1").at, Point::new(-um(1270), um(2540)));
    assert_eq!(pad(&fp, "2").at, Point::new(um(1270), um(2540)));
    assert_eq!(pad(&fp, "6").at, Point::new(um(1270), -um(2540)));
    assert!(matches!(pad(&fp, "1").shape, PadShape::Rect { .. }));
    assert!(matches!(pad(&fp, "2").shape, PadShape::Circle { .. }));
}

#[test]
fn density_changes_pads_and_name() {
    let spec = parse("SOIC-8", ChipKind::Resistor).unwrap();
    let most = generate(&spec, &GenOptions { density: Density::Most, ..Default::default() }).unwrap();
    let least = generate(&spec, &GenOptions { density: Density::Least, ..Default::default() }).unwrap();
    assert_eq!(most.name, "SOIC127P600X175-8M");
    assert_eq!(least.name, "SOIC127P600X175-8L");
    assert!(most.pads[0].shape.size().0 > least.pads[0].shape.size().0);
}

#[test]
fn errors() {
    assert!(parse("FOO-12", ChipKind::Resistor).is_err());
    assert!(parse("QFN-32 P0.5mm", ChipKind::Resistor).unwrap_err().contains("body size"));
    let mut spec = parse("SOIC-8", ChipKind::Resistor).unwrap();
    if let PackageSpec::GullWing { pins, .. } = &mut spec {
        *pins = 7;
    }
    assert!(generate(&spec, &GenOptions::default()).is_err());
}

#[test]
fn spec_serde_roundtrip() {
    let spec = parse("QFN-32 5x5mm P0.5mm EP3.1mm", ChipKind::Resistor).unwrap();
    let j = serde_json::to_string(&spec).unwrap();
    assert!(j.contains("\"family\":\"qfn\""), "{j}");
    assert_eq!(serde_json::from_str::<PackageSpec>(&j).unwrap(), spec);
}

#[test]
fn ipc_names_new_families() {
    let cases = [
        ("SOT-223", "SOT230P700X180-4N"),
        ("TO-261", "SOT230P700X180-4N"),
        ("SOT-223-4", "SOT230P700X180-4N"),
        ("DPAK", "TO229P990X238-3N"),
        ("TO-252-3", "TO229P990X238-3N"),
        ("D2PAK", "TO254P1525X483-3N"),
        ("TO-263AB", "TO254P1525X483-3N"),
        ("SOD-123", "SOD3715X135N"),
        ("sod123", "SOD3715X135N"),
        ("SOD-123F", "SODFL3716X110N"),
        ("SOD-323", "SOD2512X110N"),
        ("SC-79", "SODFL1608X70N"),
        ("MiniMELF", "DIOMELF3515N"),
        ("DO-213AB", "DIOMELF5025N"),
        ("SMA", "DIOM5226X230N"),
        ("DO-214AA", "DIOM5436X244N"),
        ("SMC", "DIOM7959X262N"),
        ("DIP-8", "DIP254P762X533-8"),
        ("PDIP-14", "DIP254P762X533-14"),
        ("DIP-28W", "DIP254P1524X533-28"),
        ("BGA-64 8x8 P0.8mm 6x6mm", "BGA64C80P8X8_600X600X120N"),
        ("BGA-60 8x8 P0.8mm 6x6mm VOID2x2 H1.0mm", "BGA60C80P8X8_600X600X100N"),
    ];
    for (pkg, name) in cases {
        // Package names fix the body type (diodes) regardless of the requested kind.
        assert_eq!(gen_named(pkg, ChipKind::Resistor).name, name, "{pkg}");
    }
}

#[test]
fn sot223_matches_hand_calculation() {
    // Symmetric: span 6.70..7.30, L 0.70..1.10, b 0.60..0.88, tab 2.90..3.18, gull-wing nominal.
    // Z = 6.70 + 0.70 + √(0.6² + F² + P²) = 8.01; G = 4.08 (see ipc.rs) → 1.965 long at ±3.0225.
    // X = 0.60 + 0.06 + √(0.28² + F² + P²) = 0.95 (lead); 2.90 + 0.06 + 0.29 = 3.25 (tab).
    let fp = gen_named("SOT-223", ChipKind::Resistor);
    let numbers: Vec<&str> = fp.pads.iter().map(|p| p.number.as_str()).collect();
    assert_eq!(numbers, ["1", "2", "3", "4"]);
    assert_eq!(pad(&fp, "1").at, Point::new(-Nm(3_022_500), um(2300)));
    assert_eq!(pad(&fp, "3").at, Point::new(-Nm(3_022_500), -um(2300)));
    assert_eq!(pad(&fp, "1").shape.size(), (um(1965), um(950)));
    assert_eq!(pad(&fp, "4").at, Point::new(Nm(3_022_500), Nm::ZERO));
    assert_eq!(pad(&fp, "4").shape.size(), (um(1965), um(3250)));
}

#[test]
fn dpak_matches_hand_calculation() {
    // Body 5.97..6.22 along X (half 3.0475), tab protrudes 0.89..1.27: tab end 3.9375..4.3175
    // (nominal 4.1275) from the body center; the lead toes are then at 9.40..10.40 − 4.1275.
    // Leads (L 1.40..1.78, b 0.63..0.89) on the virtual span 10.545..12.545:
    //   Z/2 = 6.625, G/2 = 3.31 → 3.315 long at −4.9675, X = 0.96.
    // Tab (5.20..5.80 long, 4.95..5.46 wide) on the virtual span 7.875..8.635:
    //   Z/2 = 4.67, G/2 = −2.01 (past the center) → 6.68 long at +1.33, X = 5.53.
    let fp = gen_named("DPAK", ChipKind::Resistor);
    let numbers: Vec<&str> = fp.pads.iter().map(|p| p.number.as_str()).collect();
    assert_eq!(numbers, ["1", "3", "2"], "leads 1 and 3, tab = 2");
    assert_eq!(pad(&fp, "1").at, Point::new(-Nm(4_967_500), um(2290)));
    assert_eq!(pad(&fp, "3").at, Point::new(-Nm(4_967_500), -um(2290)));
    assert_eq!(pad(&fp, "1").shape.size(), (um(3315), um(960)));
    assert_eq!(pad(&fp, "2").at, Point::new(um(1330), Nm::ZERO));
    assert_eq!(pad(&fp, "2").shape.size(), (um(6680), um(5530)));
    // The protruding tab is drawn on the fab layer.
    let fab = fp.graphics.iter().filter(|g| g.layer == GraphicLayer::Fab).count();
    assert_eq!(fab, 2);
    // Lead and tab pads keep the minimum gap: lead inner edge −3.31, tab inner edge −2.01.
    let gap = (pad(&fp, "2").at.x - um(3340)) - (pad(&fp, "1").at.x + Nm(1_657_500));
    assert_eq!(gap, um(1300));
}

#[test]
fn two_terminal_diodes_match_hand_calculation() {
    // SMA (molded body, outer goal 0.5, inner 0.15, side −0.05): L 4.90..5.50, T 0.76..1.52,
    // W 1.25..1.65 → Z 6.51, G 2.00, X 1.56.
    let fp = gen_named("SMA", ChipKind::Resistor);
    assert_eq!(pad(&fp, "1").at, Point::new(-Nm(2_127_500), Nm::ZERO), "pad 1 = cathode on the left");
    assert_eq!(pad(&fp, "2").at, Point::new(Nm(2_127_500), Nm::ZERO));
    assert_eq!(pad(&fp, "1").shape.size(), (um(2255), um(1560)));
    // MiniMELF (MELF goals 0.4/0.1/0.05): L 3.30..3.70, T 0.25..0.50, D 1.40..1.60 → Z 4.51, G 2.28.
    let fp = gen_named("MINIMELF", ChipKind::Resistor);
    assert_eq!(pad(&fp, "1").at, Point::new(-Nm(1_697_500), Nm::ZERO));
    assert_eq!(pad(&fp, "1").shape.size(), (um(1115), um(1710)));
    // SOD-123 (gull-wing goals): span 3.55..3.85, L 0.25..0.45, b 0.50..0.70 → Z 4.56, G 2.09.
    let fp = gen_named("SOD-123", ChipKind::Resistor);
    assert_eq!(pad(&fp, "1").at, Point::new(-Nm(1_662_500), Nm::ZERO));
    assert_eq!(pad(&fp, "1").shape.size(), (um(1235), um(770)));
    // SOD-523 (flat lead goals 0.2/0/0): span 1.50..1.70, L 0.15..0.35, b 0.25..0.35 → Z 2.11, G 0.92.
    let fp = gen_named("SOD-523", ChipKind::Resistor);
    assert_eq!(pad(&fp, "1").at, Point::new(-Nm(757_500), Nm::ZERO));
    assert_eq!(pad(&fp, "1").shape.size(), (um(595), um(370)));
    // Polarized: pin-1 dot on the silkscreen, left of the cathode.
    for name in ["SMA", "MELF", "SOD-123", "SOD-523"] {
        let fp = gen_named(name, ChipKind::Resistor);
        let dot = fp.graphics.iter().find_map(|g| match g.geometry {
            GraphicGeometry::Circle { center, .. } if g.layer == GraphicLayer::Silk => Some(center),
            _ => None,
        });
        assert!(dot.is_some_and(|c| c.x < pad(&fp, "1").at.x), "{name}: no pin-1 dot left of the cathode");
    }
}

#[test]
fn molded_and_melf_kinds() {
    let spec = PackageSpec::Molded {
        kind: ChipKind::Capacitor,
        length: Dim::parse("3.2±0.2mm").unwrap(),
        body_length: Dim::parse("2.8±0.2mm").unwrap(),
        width: Dim::parse("1.6±0.2mm").unwrap(),
        terminal: Dim::parse("0.5..1.1mm").unwrap(),
        lead_width: Dim::parse("1.1..1.3mm").unwrap(),
        height: um(1800),
    };
    assert_eq!(generate(&spec, &GenOptions::default()).unwrap().name, "CAPMP3216X180N");
    let spec = PackageSpec::Melf {
        kind: ChipKind::Resistor,
        length: Dim::parse("5.8..6.0mm").unwrap(),
        diameter: Dim::parse("2.1..2.3mm").unwrap(),
        terminal: Dim::parse("0.4..0.6mm").unwrap(),
    };
    let fp = generate(&spec, &GenOptions::default()).unwrap();
    assert_eq!(fp.name, "RESMELF5922N");
    let polarized = fp.graphics.iter().any(|g| matches!(g.geometry, GraphicGeometry::Circle { .. }));
    assert!(!polarized, "resistors are not polarized");
}

#[test]
fn dip_layout() {
    let fp = gen_named("DIP-8", ChipKind::Resistor);
    assert_eq!(fp.mount, Mount::Tht);
    // Counter-clockwise from the top-left, 7.62 mm rows, 2.54 mm pitch.
    assert_eq!(pad(&fp, "1").at, Point::new(-um(3810), um(3810)));
    assert_eq!(pad(&fp, "4").at, Point::new(-um(3810), -um(3810)));
    assert_eq!(pad(&fp, "5").at, Point::new(um(3810), -um(3810)));
    assert_eq!(pad(&fp, "8").at, Point::new(um(3810), um(3810)));
    assert!(matches!(pad(&fp, "1").shape, PadShape::Rect { .. }));
    assert!(matches!(pad(&fp, "2").shape, PadShape::Circle { .. }));
    assert_eq!(pad(&fp, "2").kind, PadKind::Tht { drill: um(800) });
    let fp = gen_named("DIP-28W", ChipKind::Resistor);
    assert_eq!(pad(&fp, "1").at, Point::new(-um(7620), um(16510)));
    assert_eq!(pad(&fp, "28").at, Point::new(um(7620), um(16510)));
    assert!(parse("DIP-7", ChipKind::Resistor).is_err());
}

#[test]
fn bga_rows_and_lands() {
    assert_eq!(bga_row_name(0), "A");
    assert_eq!(bga_row_name(7), "H");
    assert_eq!(bga_row_name(8), "J"); // I skipped
    assert_eq!(bga_row_name(19), "Y");
    assert_eq!(bga_row_name(20), "AA");
    assert_eq!(bga_row_name(39), "AY");
    assert_eq!(bga_row_name(40), "BA");
    let used: String = (0..20).map(bga_row_name).collect();
    for c in ['I', 'O', 'Q', 'S', 'X', 'Z'] {
        assert!(!used.contains(c), "{c} used");
    }

    let fp = gen_named("BGA-64 8x8 P0.8mm 6x6mm", ChipKind::Resistor);
    assert_eq!(fp.pads.len(), 64);
    // A1 top-left, H8 bottom-right; ball 0.40 mm → land 0.32 mm (20 % reduction).
    assert_eq!(pad(&fp, "A1").at, Point::new(-um(2800), um(2800)));
    assert_eq!(pad(&fp, "A8").at, Point::new(um(2800), um(2800)));
    assert_eq!(pad(&fp, "H8").at, Point::new(um(2800), -um(2800)));
    assert_eq!(pad(&fp, "A1").shape, PadShape::Circle { d: um(320) });
    // Courtyard: 1.0 mm (nominal) around the 6 mm body.
    let cy = BBox::of_points(fp.courtyard.iter().copied()).unwrap();
    assert_eq!(cy.max, Point::new(um(4000), um(4000)));
    // Balls sit under the body: the silk outline moves out around them instead of vanishing,
    // and A1 gets the pin-1 dot.
    let silk_paths = fp.graphics.iter().filter(|g| matches!(g.geometry, GraphicGeometry::Path { .. })).count();
    assert_eq!(silk_paths, 1, "one closed silk outline");
    assert!(fp.graphics.iter().any(|g| matches!(g.geometry, GraphicGeometry::Circle { .. })));

    let fp = gen_named("BGA-60 8x8 P0.8mm 6x6mm VOID2x2", ChipKind::Resistor);
    assert_eq!(fp.pads.len(), 60);
    for n in ["D4", "D5", "E4", "E5"] {
        assert!(!fp.pads.iter().any(|p| p.number == n), "{n} should be depopulated");
    }
    assert!(parse("BGA-64 8x8 P0.8mm 6x6mm VOID2x2", ChipKind::Resistor).unwrap_err().contains("60 balls"));
    assert!(parse("BGA-64 8x8 6x6mm", ChipKind::Resistor).unwrap_err().contains("pitch"));

    let mut spec = parse("BGA-64 8x8 P0.8mm 6x6mm", ChipKind::Resistor).unwrap();
    if let PackageSpec::Bga { missing, .. } = &mut spec {
        *missing = vec!["a1".into(), "H8".into()];
    }
    assert_eq!(spec.pad_count(), 62);
    assert_eq!(spec.ipc_name(Density::Nominal), "BGA62C80P8X8_600X600X120N");
    let fp = generate(&spec, &GenOptions::default()).unwrap();
    assert!(!fp.pads.iter().any(|p| p.number == "A1" || p.number == "H8"));
    if let PackageSpec::Bga { missing, .. } = &mut spec {
        *missing = vec!["I1".into()];
    }
    assert!(generate(&spec, &GenOptions::default()).is_err());
}

#[test]
fn new_families_courtyard_and_pad_gaps() {
    let opts = GenOptions::default();
    let names = [
        "SOT-223",
        "DPAK",
        "D2PAK",
        "SOD-123",
        "SOD-123F",
        "SOD-323",
        "SOD-523",
        "MINIMELF",
        "MELF",
        "SMA",
        "SMB",
        "SMC",
        "DIP-8",
        "DIP-28W",
        "BGA-64 8x8 P0.8mm 6x6mm",
        "BGA-60 8x8 P0.8mm 6x6mm VOID2x2",
        "BGA-256 16x16 P1.0mm 17x17mm",
    ];
    for name in names {
        let fp = gen_named(name, ChipKind::Diode);
        let cy = BBox::of_points(fp.courtyard.iter().copied()).unwrap();
        for (i, p) in fp.pads.iter().enumerate() {
            let (w, h) = p.shape.size();
            for (sx, sy) in [(-1, -1), (-1, 1), (1, -1), (1, 1)] {
                let corner = Point::new(p.at.x + Nm(sx * w.0 / 2), p.at.y + Nm(sy * h.0 / 2));
                assert!(cy.contains(corner), "{name}: pad {} sticks out of the courtyard", p.number);
            }
            // Pads keep the minimum copper gap.
            for q in &fp.pads[i + 1..] {
                let (qw, qh) = q.shape.size();
                let dx = (p.at.x - q.at.x).abs().0 - (w.0 + qw.0) / 2;
                let dy = (p.at.y - q.at.y).abs().0 - (h.0 + qh.0) / 2;
                assert!(dx.max(dy) >= opts.min_pad_gap.0, "{name}: pads {} and {} too close", p.number, q.number);
            }
        }
    }
}

#[test]
fn new_spec_serde_roundtrip() {
    for (name, family) in [
        ("SOT-223", "tab"),
        ("DPAK", "tab"),
        ("SOD-123F", "sod"),
        ("SMB", "molded"),
        ("MINIMELF", "melf"),
        ("DIP-16", "dip"),
        ("BGA-60 8x8 P0.8mm 6x6mm VOID2x2", "bga"),
    ] {
        let spec = parse(name, ChipKind::Resistor).unwrap();
        let j = serde_json::to_string(&spec).unwrap();
        assert!(j.contains(&format!("\"family\":\"{family}\"")), "{j}");
        assert_eq!(serde_json::from_str::<PackageSpec>(&j).unwrap(), spec, "{name}");
    }
    // Defaults and unknown fields.
    let j = r#"{"family":"melf","length":"3.3..3.7mm","diameter":"1.4..1.6mm","terminal":"0.25..0.5mm"}"#;
    let spec = serde_json::from_str::<PackageSpec>(j).unwrap();
    assert!(matches!(spec, PackageSpec::Melf { kind: ChipKind::Diode, .. }));
    let j = r#"{"family":"melf","length":"3.5mm","diameter":"1.5mm","terminal":"0.3mm","colour":"red"}"#;
    assert!(serde_json::from_str::<PackageSpec>(j).is_err());
}
