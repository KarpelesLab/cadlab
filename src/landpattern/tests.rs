use super::packages::parse;
use super::*;
use crate::geom::BBox;
use crate::model::footprint::GraphicGeometry;

fn um(v: i64) -> Nm {
    Nm::from_um(v)
}

fn gen_named(name: &str, kind: ChipKind) -> Footprint {
    let spec = parse(name, kind).unwrap_or_else(|e| panic!("{name}: {e}"));
    generate(&spec, &GenOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn pad<'a>(fp: &'a Footprint, n: &str) -> &'a Pad {
    fp.pads
        .iter()
        .find(|p| p.number == n)
        .unwrap_or_else(|| panic!("no pad {n}"))
}

#[test]
fn dims_parse() {
    assert_eq!(
        Dim::parse("1.0±0.05mm").unwrap(),
        Dim {
            min: um(950),
            max: um(1050)
        }
    );
    assert_eq!(
        Dim::parse("1.0+-0.05mm").unwrap(),
        Dim {
            min: um(950),
            max: um(1050)
        }
    );
    assert_eq!(
        Dim::parse("0.15..0.35mm").unwrap(),
        Dim {
            min: um(150),
            max: um(350)
        }
    );
    assert_eq!(
        Dim::parse("0.15mm..0.35mm").unwrap(),
        Dim {
            min: um(150),
            max: um(350)
        }
    );
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
    assert_eq!(gen_named("LQFP-48", ChipKind::Resistor).name, "QFP50P900X900X160-48N");
    assert_eq!(
        gen_named("QFN-32 5x5mm P0.5mm EP3.1mm", ChipKind::Resistor).name,
        "QFN50P500X500X90-33N"
    );
    assert_eq!(
        gen_named("DFN-8 2x3mm P0.5mm EP0.9x2.4mm H0.8mm", ChipKind::Resistor).name,
        "SON50P300X200X80-9N"
    );
    assert_eq!(
        gen_named("PinHeader 1x04", ChipKind::Resistor).name,
        "PinHeader_1x04_P2.54mm"
    );
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
    let (p1, p12, p13, p25, p37) = (
        pad(&fp, "1"),
        pad(&fp, "12"),
        pad(&fp, "13"),
        pad(&fp, "25"),
        pad(&fp, "37"),
    );
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
    let most = generate(
        &spec,
        &GenOptions {
            density: Density::Most,
            ..Default::default()
        },
    )
    .unwrap();
    let least = generate(
        &spec,
        &GenOptions {
            density: Density::Least,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(most.name, "SOIC127P600X175-8M");
    assert_eq!(least.name, "SOIC127P600X175-8L");
    assert!(most.pads[0].shape.size().0 > least.pads[0].shape.size().0);
}

#[test]
fn errors() {
    assert!(parse("FOO-12", ChipKind::Resistor).is_err());
    assert!(
        parse("QFN-32 P0.5mm", ChipKind::Resistor)
            .unwrap_err()
            .contains("body size")
    );
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
