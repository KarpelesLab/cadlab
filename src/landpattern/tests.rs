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
    // A1 top-left, H8 bottom-right; ball 0.40 mm → land 0.30 mm (IPC-7351B Table 14-5).
    assert_eq!(pad(&fp, "A1").at, Point::new(-um(2800), um(2800)));
    assert_eq!(pad(&fp, "A8").at, Point::new(um(2800), um(2800)));
    assert_eq!(pad(&fp, "H8").at, Point::new(um(2800), -um(2800)));
    assert_eq!(pad(&fp, "A1").shape, PadShape::Circle { d: um(300) });
    // The land does not follow the footprint's density level or rounding grid, only the ball.
    let spec = parse("BGA-64 8x8 P0.8mm 6x6mm", ChipKind::Resistor).unwrap();
    for density in [Density::Most, Density::Least] {
        let opts = GenOptions { density, rounding: um(1), ..GenOptions::default() };
        assert_eq!(pad(&generate(&spec, &opts).unwrap(), "A1").shape, PadShape::Circle { d: um(300) });
    }
    // TI SPRACA4 (AM572x, 0.8 mm pitch, 0.5 mm balls): 0.4 mm lands.
    let fp = gen_named("BGA-64 8x8 P0.8mm 7x7mm B0.5mm", ChipKind::Resistor);
    assert_eq!(pad(&fp, "A1").shape, PadShape::Circle { d: um(400) });
    let fp = gen_named("BGA-64 8x8 P0.8mm 6x6mm", ChipKind::Resistor);
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

/// Z (outer edge to outer edge), G (inner edge to inner edge) and X (pad width) of a
/// two-terminal footprint.
fn zgx(fp: &Footprint) -> (Nm, Nm, Nm) {
    let (p1, p2) = (pad(fp, "1"), pad(fp, "2"));
    let (len, width) = p1.shape.size();
    assert_eq!(p2.shape.size(), (len, width));
    let span = p2.at.x - p1.at.x;
    (span + len, span - len, width)
}

/// Courtyard size along X and Y.
fn courtyard_size(fp: &Footprint) -> (Nm, Nm) {
    let cy = BBox::of_points(fp.courtyard.iter().copied()).unwrap();
    (cy.max.x - cy.min.x, cy.max.y - cy.min.y)
}

#[track_caller]
fn assert_close(what: &str, got: Nm, want_um: i64, tol_um: i64) {
    let d = (got - um(want_um)).abs();
    assert!(d <= um(tol_um), "{what}: {got}, expected {} within {}", um(want_um), um(tol_um));
}

/// Molded tantalum (inward flat ribbon L leads, IPC-7351B Table 3-13) against KEMET's lands.
///
/// Source: KEMET T491 datasheet T2005_T491 (2026-07-08), "Dimensions" (L, W, H, F = termination
/// width ±0.1, S = termination length ±0.3) and Table 2 "Land Dimensions/Courtyard", whose
/// density levels A/B/C refer to IPC-7351: W = land width, L = land length, S = gap between the
/// lands, V1 × V2 = courtyard. KEMET's numbers follow from F = P = 0.1 mm, set here.
/// Tolerance: 0.02 mm on Z = 2L + S, G = S and X = W (0.01 mm rounding on each side).
#[test]
fn molded_tantalum_matches_kemet_t491() {
    // (case, [L, ±, W, ±, H, F, S], then per level A, B, C: [W, L, S, V1, V2]), µm.
    let cases = [
        (
            "C 6032-28",
            [6000, 300, 3200, 300, 2500, 2200, 1300],
            [[2350, 2770, 2370, 8920, 4500], [2230, 2370, 2570, 7820, 4000], [2130, 1990, 2730, 6960, 3740]],
        ),
        (
            "D 7343-31",
            [7300, 300, 4300, 300, 2800, 2400, 1300],
            [[2550, 2770, 3670, 10220, 5600], [2430, 2370, 3870, 9120, 5100], [2330, 1990, 4030, 8260, 4840]],
        ),
    ];
    for (case, [l, lt, w, wt, h, f, s], lands) in cases {
        let spec = PackageSpec::Molded {
            kind: ChipKind::Capacitor,
            length: Dim::plus_minus(um(l), um(lt)),
            body_length: Dim::plus_minus(um(l), um(lt)),
            width: Dim::plus_minus(um(w), um(wt)),
            terminal: Dim::plus_minus(um(s), um(300)),
            lead_width: Dim::plus_minus(um(f), um(100)),
            height: um(h + 300),
        };
        for (density, [kw, kl, ks, v1, v2]) in [Density::Most, Density::Nominal, Density::Least].into_iter().zip(lands)
        {
            let opts =
                GenOptions { density, fab_tolerance: um(100), placement_tolerance: um(100), ..GenOptions::default() };
            let fp = generate(&spec, &opts).unwrap();
            let (z, g, x) = zgx(&fp);
            let what = format!("{case} {density:?}");
            assert_close(&format!("{what} Z"), z, 2 * kl + ks, 20);
            assert_close(&format!("{what} G"), g, ks, 20);
            assert_close(&format!("{what} X"), x, kw, 20);
            // Courtyard: KEMET's is the lands and the largest body plus 0.5 / 0.25 / 0.12 mm;
            // ours is around the nominal body, and 0.1 mm at Least (IPC-7351B). Tolerance
            // 0.05 mm along the lands; across them the body tolerance comes off KEMET's V2.
            let (cx, cy) = courtyard_size(&fp);
            assert_close(&format!("{what} courtyard V1"), cx, v1, 50);
            assert_close(&format!("{what} courtyard V2"), cy, v2 - wt, 50);
        }
    }
}

/// DO-214 molded diodes against Diodes Incorporated's suggested pad layouts.
///
/// Source: Diodes Inc. AP02001 "Package Outline Dimensions / Suggested Pad Layout (Based on
/// IPC-7351A)", Rev. 81 (2014-11): SMA, SMB, SMC outlines (E = overall length, C = lead width,
/// H = terminal length 0.76..1.52, B = body length, A = body width, J = height) and pads
/// (X1 = Z, G, Y = X). Those layouts are rounded to 0.1 mm and are not the Nominal calculation:
/// tolerance 0.3 mm on Z, 0.5 mm on G (the wide terminal tolerance moves G most), 0.25 mm on X.
#[test]
fn molded_diodes_near_diodes_ap02001() {
    // (name, E, B, A, C, J max, pads [Z, G, X]), µm.
    let cases = [
        ("SMA", (4800, 5590), (4000, 4600), (2290, 2920), (1270, 1630), 2300, [6500, 1500, 1700]),
        ("SMB", (5000, 5590), (4060, 4570), (3300, 3940), (1960, 2210), 2500, [6800, 1800, 2300]),
        ("SMC", (7750, 8130), (6600, 7110), (5590, 6220), (2750, 3180), 2500, [9400, 4400, 3300]),
    ];
    let r = |v: (i64, i64)| Dim { min: um(v.0), max: um(v.1) };
    for (name, e, b, a, c, j, [pz, pg, px]) in cases {
        let spec = PackageSpec::Molded {
            kind: ChipKind::Diode,
            length: r(e),
            body_length: r(b),
            width: r(a),
            terminal: r((760, 1520)),
            lead_width: r(c),
            height: um(j),
        };
        let (z, g, x) = zgx(&generate(&spec, &GenOptions::default()).unwrap());
        assert_close(&format!("{name} Z"), z, pz, 300);
        assert_close(&format!("{name} G"), g, pg, 500);
        assert_close(&format!("{name} X"), x, px, 250);
    }
}

/// MELF and MiniMELF (IPC-7351B Table 3-7) against Diodes Incorporated's suggested pad layouts.
///
/// Source: Diodes Inc. AP02001 Rev. 81 (2014-11, "Based on IPC-7351A"): MiniMELF A 3.30..3.70,
/// B 1.30..1.60, C 0.28..0.50, pads X1 4.70, G 2.10, Y 1.70; MELF A 4.80..5.20, B 2.40..2.60,
/// C 0.55 typ, pads X1 6.30, G 3.30, Y 2.70. Their pad width is our Nominal one (0.02 mm
/// tolerance: side goal 0.05); their Z and G lie between our Nominal and Most patterns, which
/// is asserted as such (0.01 mm slack), so this does not confirm the toe and heel rows.
#[test]
fn melf_brackets_diodes_ap02001() {
    let cases = [
        ("MiniMELF", (3300, 3700), (1300, 1600), (280, 500), [4700, 2100, 1700]),
        ("MELF", (4800, 5200), (2400, 2600), (550, 550), [6300, 3300, 2700]),
    ];
    let r = |v: (i64, i64)| Dim { min: um(v.0), max: um(v.1) };
    for (name, a, b, c, [pz, pg, px]) in cases {
        let spec = PackageSpec::Melf { kind: ChipKind::Diode, length: r(a), diameter: r(b), terminal: r(c) };
        let at = |density| zgx(&generate(&spec, &GenOptions { density, ..GenOptions::default() }).unwrap());
        let (zn, gn, xn) = at(Density::Nominal);
        let (zm, gm, _) = at(Density::Most);
        assert_close(&format!("{name} X"), xn, px, 20);
        assert!(zn <= um(pz + 10) && um(pz - 10) <= zm, "{name} Z: {zn} (nominal) .. {zm} (most) around {}", um(pz));
        assert!(gm <= um(pg + 10) && um(pg - 10) <= gn, "{name} G: {gm} (most) .. {gn} (nominal) around {}", um(pg));
    }
}

/// Flat-lead SOD (IPC-7351B Table 3-22) against Diodes Incorporated's suggested pad layout.
///
/// Source: Diodes Inc. AP02001 Rev. 81 (2014-11, "Based on IPC-7351A"), SOD123F: He 3.30..3.70,
/// b 0.80..1.35, L2 0.35..0.85, D 1.70..1.90, E 2.60..2.80, A max 1.15; pads X1 (Z) 4.20,
/// G 1.52, Y (width) 1.80. Their Z lies between our Nominal (toe 0.2) and Most (toe 0.3)
/// patterns, asserted as such. The wide tolerances of this outline (b ±0.275, L2 ±0.25) leave G
/// and the width loose: tolerance 0.4 mm on G and 0.45 mm on the width at Nominal.
#[test]
fn flat_lead_sod_near_diodes_ap02001() {
    let r = |min, max| Dim { min: um(min), max: um(max) };
    let spec = PackageSpec::Sod {
        lead: SodLead::Flat,
        span: r(3300, 3700),
        body_length: r(2600, 2800),
        body_width: r(1700, 1900),
        terminal: r(350, 850),
        lead_width: r(800, 1350),
        height: um(1150),
    };
    let at = |density| zgx(&generate(&spec, &GenOptions { density, ..GenOptions::default() }).unwrap());
    let (zn, gn, xn) = at(Density::Nominal);
    let (zm, _, _) = at(Density::Most);
    assert!(zn <= um(4200) && um(4200) <= zm, "Z: {zn} (nominal) .. {zm} (most) around 4.2mm");
    assert_close("G", gn, 1520, 400);
    assert_close("X", xn, 1800, 450);
    // The built-in SOD-123F (typical dimensions) gives the same Z within 0.15 mm.
    let (z, _, _) = zgx(&gen_named("SOD-123F", ChipKind::Diode));
    assert_close("SOD-123F Z", z, 4200, 150);
}

/// DPAK and D2PAK (leads and flat lug tab on the gull-wing goals, IPC-7351B Table 3-14) against
/// Diodes Incorporated's suggested pad layouts.
///
/// Source: Diodes Inc. AP02001 Rev. 81 (2014-11, "Based on IPC-7351A"). TO252 (DPAK): lead pads
/// X 1.06 wide, Y 2.60 long, C 4.572 apart; tab pad X1 5.632 wide, Y1 5.70 long; Y2 10.70 overall.
/// TO263AB (D2PAK): X 1.10, Y 3.50, C 5.08, X1 10.41, Y1 7.01, Y2 15.99.
/// Widths and lead positions agree: tolerance on widths 0.15 mm (DPAK) and 0.35 mm (D2PAK, whose
/// built-in tab width range 9.65..10.67 is wider than this layout assumes), on the lead spacing
/// 0.01 mm. Lengths do not: the IPC toe and heel goals (0.35 mm each at Nominal, plus
/// tolerances) give longer pads than these layouts; asserted only as "not shorter, and at most
/// this much longer": for the DPAK 0.8 mm on a lead, 1.0 mm on the tab and 0.7 mm overall, for
/// the D2PAK 0.9, 1.6 and 0.9 mm (its built-in tab length 7.0..7.5 is longer than the D1
/// 6.55..6.95 of this outline).
#[test]
fn dpak_near_diodes_ap02001() {
    // (name, lead width, lead length, lead spacing, tab width, tab length, overall, width tolerance,
    // how much longer the lead, the tab and the whole pattern may be), µm.
    let cases = [
        ("DPAK", 1060, 2600, 4572, 5632, 5700, 10700, 150, [800, 1000, 700]),
        ("D2PAK", 1100, 3500, 5080, 10410, 7010, 15990, 350, [900, 1600, 900]),
    ];
    for (name, lw, ll, c, tw, tl, overall, wtol, longer) in cases {
        let fp = gen_named(name, ChipKind::Resistor);
        let (l1, l3, tab) = (pad(&fp, "1"), pad(&fp, "3"), pad(&fp, "2"));
        let (len, width) = l1.shape.size();
        let (tab_len, tab_width) = tab.shape.size();
        assert_close(&format!("{name} lead width"), width, lw, wtol);
        assert_close(&format!("{name} tab width"), tab_width, tw, wtol);
        assert_close(&format!("{name} lead spacing"), l1.at.y - l3.at.y, c, 10);
        let extent = (tab.at.x + Nm(tab_len.0 / 2)) - (l1.at.x - Nm(len.0 / 2));
        let lengths = [("lead length", len, ll), ("tab length", tab_len, tl), ("overall length", extent, overall)];
        for ((what, got, want), longer) in lengths.into_iter().zip(longer) {
            assert!(got >= um(want) && got <= um(want + longer), "{name} {what}: {got}, layout {}", um(want));
        }
    }
}
