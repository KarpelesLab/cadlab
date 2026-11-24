use super::*;
use crate::geom::Point;
use crate::geom::poly::Location;
use crate::model::board::{Contour, Keepout, Segment, Track, Via};

const MM: i64 = 1_000_000;

fn pp(x: i64, y: i64) -> poly::Point {
    poly::Point::new(x, y)
}

fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<poly::Point> {
    vec![pp(x0, y0), pp(x1, y0), pp(x1, y1), pp(x0, y1)]
}

fn pad(net: &str, cx: i64, cy: i64, hw: i64, hh: i64) -> CopperItem {
    CopperItem {
        item: ItemRef::Pad("U1".into(), format!("{cx}")),
        net: Some(net.into()),
        layers: vec!["F.Cu".into()],
        shape: vec![rect(cx - hw, cy - hh, cx + hw, cy + hh)],
        anchor: Point::new(Nm(cx), Nm(cy)),
        local: Default::default(),
    }
}

fn track(net: &str, a: (i64, i64), b: (i64, i64), w: i64) -> CopperItem {
    let t = Track {
        id: ObjectId(1),
        layer: "F.Cu".into(),
        width: Nm(w),
        net: Some(net.into()),
        start: Point::new(Nm(a.0), Nm(a.1)),
        end: Point::new(Nm(b.0), Nm(b.1)),
        mid: None,
        locked: false,
    };
    CopperItem {
        item: ItemRef::Track(t.id),
        net: t.net.clone(),
        layers: vec!["F.Cu".into()],
        shape: crate::board::track_shape(&t),
        anchor: t.start,
        local: Default::default(),
    }
}

fn via(net: &str, x: i64, y: i64) -> CopperItem {
    let v = Via {
        id: ObjectId(2),
        at: Point::new(Nm(x), Nm(y)),
        drill: Nm(300_000),
        diameter: Nm(600_000),
        net: Some(net.into()),
        from: "F.Cu".into(),
        to: "B.Cu".into(),
        locked: false,
    };
    CopperItem {
        item: ItemRef::Via(v.id),
        net: v.net.clone(),
        layers: vec!["F.Cu".into()],
        shape: vec![crate::board::via_shape(&v)],
        anchor: v.at,
        local: Default::default(),
    }
}

fn params(pads: PadConnection) -> ZoneParams {
    ZoneParams {
        clearance: Nm(200_000),
        min_width: Nm(200_000),
        pads,
        thermal_gap: Nm(300_000),
        thermal_spoke: Nm(250_000),
        thermal_angle: None,
        thermal_spokes: 4,
        spoke_floor: None,
        min_spoke: Nm::ZERO,
    }
}

fn input<'a>(
    net: Option<&'a str>,
    outline: &'a [poly::Point],
    items: &'a [(CopperItem, Nm)],
    prm: ZoneParams,
) -> LayerInput<'a> {
    LayerInput {
        net,
        outline,
        board: None,
        items: items.iter().map(|(i, c)| (i, *c)).collect(),
        keepaway: vec![],
        params: prm,
    }
}

fn run(outline: &[poly::Point], items: &[(CopperItem, Nm)], prm: ZoneParams) -> PolygonSet {
    let inp = input(Some("GND"), outline, items, prm);
    let a = fill_layer(&inp).unwrap();
    assert_eq!(a, fill_layer(&inp).unwrap(), "deterministic");
    poly::check_canonical(&a, true).unwrap();
    a
}

fn inside(s: &PolygonSet, x: i64, y: i64) -> bool {
    poly::locate(s, pp(x, y)) == Location::Inside
}

#[test]
fn clearance_respected() {
    let outline = square(0, 0, 20 * MM, 20 * MM);
    let rotated = CopperItem {
        shape: vec![Polygon::new(
            vec![pp(14 * MM, 5 * MM), pp(15 * MM, 6 * MM), pp(14 * MM, 7 * MM), pp(13 * MM, 6 * MM)],
            vec![],
        )],
        ..pad("A", 14 * MM, 6 * MM, 0, 0)
    };
    let items = vec![
        (pad("A", 5 * MM, 5 * MM, 600_000, 400_000), Nm::ZERO),
        (track("B", (2 * MM, 15 * MM), (18 * MM, 11 * MM), 250_000), Nm::ZERO),
        (via("C", 10 * MM, 3 * MM), Nm(400_000)), // its net class clearance wins
        (rotated, Nm::ZERO),
        (via("GND", 3 * MM, 18 * MM), Nm::ZERO),
    ];
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    assert!(!fill.is_empty());
    for (it, c) in &items[..4] {
        let c = (*c).max(Nm(200_000)).0;
        assert!(!poly::distance_less_than(&fill, &it.shape, c), "{} closer than {c}", it.item);
        assert!(poly::distance_less_than(&fill, &it.shape, c + 5_000), "{} the fill hugs it", it.item);
    }
    // A same-net via sits in solid copper.
    assert!(poly::contains(&fill, &items[4].0.shape));
}

#[test]
fn thermal_spokes_and_gap() {
    let outline = square(0, 0, 10 * MM, 10 * MM);
    let (c, h) = (5 * MM, 500_000);
    let items = vec![(pad("GND", c, c, h, h), Nm::ZERO)];
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    assert_eq!(fill.len(), 1);
    assert!(poly::intersects(&fill, &items[0].0.shape), "connected through spokes");
    let mid = h + 150_000; // inside the 0.3 mm gap
    for (dx, dy) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
        assert!(inside(&fill, c + dx * mid, c + dy * mid), "spoke ({dx}, {dy})");
    }
    // Diagonal parts of the gap are empty.
    assert!(!inside(&fill, c + mid, c + mid));
    assert!(!inside(&fill, c - mid, c - mid));
    // Solid: no gap at all.
    let solid = run(&outline, &items, params(PadConnection::Solid));
    assert!(inside(&solid, c + mid, c + mid));
    // Whole square, less the four corners rounded by the min-width opening (r = 0.1 mm).
    let full = 2 * 100 * MM as i128 * MM as i128;
    let corners = 2 * (4.0 * (1.0 - std::f64::consts::FRAC_PI_4) * 1e10) as i128;
    assert!((poly::area2(&solid) - (full - corners)).abs() < full / 10_000);
    // None: clearance gap; with nothing else of the net the whole pour is unconnected.
    let none = run(&outline, &items, params(PadConnection::None));
    assert!(none.is_empty(), "unconnected copper removed");
    let with_via = [items[0].clone(), (via("GND", MM, MM), Nm::ZERO)];
    let none = run(&outline, &with_via, params(PadConnection::None));
    assert!(!none.is_empty());
    assert!(!poly::distance_less_than(&none, &items[0].0.shape, 200_000));
    assert!(!inside(&none, c + h + 100_000, c));
}

#[test]
fn spoke_angle_count_and_pad_overrides() {
    use crate::board::PadThermal;
    use crate::units::Angle;
    let outline = square(0, 0, 10 * MM, 10 * MM);
    let (c, h) = (5 * MM, 500_000);
    let mid = h + 150_000; // inside the 0.3 mm gap
    let with = |t: PadThermal| {
        let mut it = pad("GND", c, c, h, h);
        it.local.thermal = Some(t);
        vec![(it, Nm::ZERO)]
    };
    // A round pad defaults to diagonal spokes (here a square pad marked round: the spoke
    // direction is what is checked).
    let round = with(PadThermal { round: true, ..Default::default() });
    let fill = run(&outline, &round, params(PadConnection::Thermal));
    assert!(inside(&fill, c + mid, c + mid), "45° spoke");
    assert!(inside(&fill, c - mid, c - mid), "225° spoke");
    assert!(!inside(&fill, c + mid, c), "no axis spoke");
    assert!(poly::intersects(&fill, &round[0].0.shape), "connected");
    // The pad's orientation turns the spokes: a pad at 45° with the default 90° is diagonal.
    let turned = with(PadThermal { rotation: Angle(45_000), ..Default::default() });
    let f2 = run(&outline, &turned, params(PadConnection::Thermal));
    assert!(inside(&f2, c - mid, c + mid), "135° spoke");
    // The zone's angle applies to pads without one; a pad's own angle wins.
    let mut prm = params(PadConnection::Thermal);
    prm.thermal_angle = Some(Angle(45_000));
    let plain = with(PadThermal::default());
    assert!(inside(&run(&outline, &plain, prm), c + mid, c + mid));
    let own = with(PadThermal { angle: Some(Angle::ZERO), ..Default::default() });
    let f3 = run(&outline, &own, prm);
    assert!(inside(&f3, c + mid, c) && !inside(&f3, c + mid, c + mid));
    // Two spokes: +y and -y only (first at the default 90°).
    let mut two = params(PadConnection::Thermal);
    two.thermal_spokes = 2;
    let f4 = run(&outline, &plain, two);
    assert!(inside(&f4, c, c + mid) && inside(&f4, c, c - mid));
    assert!(!inside(&f4, c + mid, c) && !inside(&f4, c - mid, c));
    let four = run(&outline, &plain, params(PadConnection::Thermal));
    assert!(poly::area2(&f4) < poly::area2(&four));
    // A pad's own gap and spoke width.
    let wide = with(PadThermal { gap: Some(Nm(600_000)), spoke: Some(Nm(400_000)), ..Default::default() });
    let f5 = run(&outline, &wide, params(PadConnection::Thermal));
    assert!(!inside(&f5, c + h + 400_000, c + h + 400_000), "inside the 0.6 mm gap");
    assert!(inside(&f5, c + h + 300_000, c + 150_000), "a 0.4 mm spoke");
    assert!(inside(&four, c + h + 400_000, c + h + 400_000), "outside the zone's 0.3 mm gap");
}

/// A zone that leaves the spoke width to cadlab gives small pads narrower spokes: half the
/// pad's shorter side, never below the floor; a width set on the zone or the pad stays.
#[test]
fn default_spokes_fit_small_pads() {
    let mut prm = params(PadConnection::Thermal);
    prm.thermal_spoke = Nm(500_000);
    prm.spoke_floor = Some(Nm(150_000));
    // 2 x 2 mm: the zone's width. 0.6 x 0.5 mm: half the shorter side. 0.25 x 0.8 mm: the floor.
    assert_eq!(Relief::of(&pad("GND", 0, 0, MM, MM), &prm).w, 500_000);
    assert_eq!(Relief::of(&pad("GND", 0, 0, 300_000, 250_000), &prm).w, 250_000);
    assert_eq!(Relief::of(&pad("GND", 0, 0, 125_000, 400_000), &prm).w, 150_000);
    // Rotated by 45 degrees the pad is as narrow as before.
    let mut turned = pad("GND", 0, 0, 300_000, 250_000);
    let h = std::f64::consts::FRAC_1_SQRT_2;
    let turn = |x: f64, y: f64| pp(((x - y) * h).round() as i64, ((x + y) * h).round() as i64);
    turned.shape =
        vec![Polygon::new(vec![turn(-3e5, -2.5e5), turn(3e5, -2.5e5), turn(3e5, 2.5e5), turn(-3e5, 2.5e5)], vec![])];
    assert!((short_side(&turned.shape) - 500_000).abs() <= 2, "vertex rounding only");
    assert_eq!(Relief::of(&turned, &prm).w, 250_000);
    assert_eq!((fitted_spoke(499_999), fitted_spoke(500_001), fitted_spoke(250_000)), (250_000, 250_000, 125_000));
    // Mirrored (a footprint on the bottom side), its outline runs clockwise: the same pad.
    let mut mirrored = pad("GND", 0, 0, 300_000, 250_000);
    mirrored.shape[0].outer.0.reverse();
    assert_eq!(short_side(&mirrored.shape), 500_000);
    assert_eq!(Relief::of(&mirrored, &prm).w, 250_000);
    // A width given on the zone, or on the pad, is used as it is.
    prm.spoke_floor = None;
    assert_eq!(Relief::of(&pad("GND", 0, 0, 125_000, 400_000), &prm).w, 500_000);
    prm.spoke_floor = Some(Nm(150_000));
    let mut own = pad("GND", 0, 0, 125_000, 400_000);
    own.local.thermal = Some(crate::board::PadThermal { spoke: Some(Nm(300_000)), ..Default::default() });
    assert_eq!(Relief::of(&own, &prm).w, 300_000);
    // The small pad in a pour: four spokes reach it and leave most of its outline free.
    let items = [(pad("GND", 5 * MM, 5 * MM, 300_000, 250_000), Nm(200_000))];
    let fill = run(&square(0, 0, 10 * MM, 10 * MM), &items, prm);
    assert!(inside(&fill, 5 * MM + 450_000, 5 * MM), "spoke");
    assert!(!inside(&fill, 5 * MM + 450_000, 5 * MM + 200_000), "gap beside the spoke");
}

#[test]
fn spokes_blocked_by_obstacles_are_dropped() {
    let outline = square(0, 0, 10 * MM, 10 * MM);
    let c = 5 * MM;
    // A foreign track right of the pad: the +x spoke would violate its clearance.
    let items = vec![
        (pad("GND", c, c, 500_000, 500_000), Nm::ZERO),
        (track("A", (c + 900_000, 2 * MM), (c + 900_000, 8 * MM), 200_000), Nm::ZERO),
    ];
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    assert!(!poly::distance_less_than(&fill, &items[1].0.shape, 200_000));
    assert!(inside(&fill, c - 650_000, c), "-x spoke kept");
    assert!(!inside(&fill, c + 650_000, c), "+x spoke dropped");
}

/// A row of same-net pads, each inside its neighbors' thermal gaps, whose spokes across the row
/// are blocked (foreign tracks above and below): the inner pads are joined through the spokes
/// running pad to pad from the outer pads, which reach the pour (resistor arrays, corpus
/// `buspirate5-rev10`).
#[test]
fn spokes_between_pads_of_a_row_carry_the_connection() {
    let outline = square(0, 0, 10 * MM, 10 * MM);
    let c = 5 * MM;
    let row: Vec<CopperItem> =
        [-750_000, -250_000, 250_000, 750_000].iter().map(|dx| pad("GND", c + dx, c, 150_000, 250_000)).collect();
    let mut items: Vec<(CopperItem, Nm)> = row.iter().map(|p| (p.clone(), Nm::ZERO)).collect();
    items.push((track("A", (MM, c + 700_000), (9 * MM, c + 700_000), 200_000), Nm::ZERO));
    items.push((track("A", (MM, c - 700_000), (9 * MM, c - 700_000), 200_000), Nm::ZERO));
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    for p in &row {
        assert!(poly::intersects(&fill, &p.shape), "{} joined", p.item);
    }
    assert!(inside(&fill, c, c), "copper between the inner pads");
    for (t, _) in &items[4..] {
        assert!(!poly::distance_less_than(&fill, &t.shape, 200_000));
    }
    // Without the outer pads' outward spokes (their ends blocked too), nothing joins the row.
    items.push((track("A", (c - 1_300_000, MM), (c - 1_300_000, 9 * MM), 200_000), Nm::ZERO));
    items.push((track("A", (c + 1_300_000, MM), (c + 1_300_000, 9 * MM), 200_000), Nm::ZERO));
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    for p in &row {
        assert!(!poly::intersects(&fill, &p.shape), "{} left alone", p.item);
    }
}

#[test]
fn isolated_islands_removed() {
    let outline = square(0, 0, 20 * MM, 10 * MM);
    // A foreign track splits the zone; only the left half has GND copper.
    let items = vec![
        (track("A", (10 * MM, -MM), (10 * MM, 11 * MM), 300_000), Nm::ZERO),
        (via("GND", 4 * MM, 5 * MM), Nm::ZERO),
    ];
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    assert_eq!(fill.len(), 1);
    assert!(fill[0].bbox().unwrap().max.x < 10 * MM);
    // Netless zones keep every island and treat every item as an obstacle.
    let f = fill_layer(&input(None, &outline, &items, params(PadConnection::Thermal))).unwrap();
    assert_eq!(f.len(), 2);
    assert!(!poly::distance_less_than(&f, &items[1].0.shape, 200_000));
}

#[test]
fn min_width_enforced() {
    let outline = square(0, 0, 20 * MM, 10 * MM);
    // Two foreign pads leave a 0.15 mm neck (after clearance) at x = 10 mm, y = 2 mm.
    let neck = 150_000 + 2 * 200_000;
    let items = vec![
        (pad("A", 10 * MM, 2 * MM - neck / 2 - 2 * MM, MM, 2 * MM), Nm::ZERO),
        (pad("A", 10 * MM, 2 * MM + neck / 2 + 4 * MM, MM, 4 * MM), Nm::ZERO),
        (via("GND", 3 * MM, 5 * MM), Nm::ZERO),
        (via("GND", 17 * MM, 5 * MM), Nm::ZERO),
    ];
    let mut prm = params(PadConnection::Thermal);
    prm.min_width = Nm::ZERO;
    let thin = run(&outline, &items, prm);
    assert_eq!(thin.len(), 1, "without a min width the neck joins both sides");
    assert!(inside(&thin, 10 * MM, 2 * MM));
    let fill = run(&outline, &items, params(PadConnection::Thermal));
    assert_eq!(fill.len(), 2, "the neck is narrower than 0.2 mm and goes");
    assert!(!inside(&fill, 10 * MM, 2 * MM));
    // Opening by just under half the min width leaves the fill (nearly) unchanged.
    let again = poly::opening(&fill, 99_000, FILL_TOL).unwrap();
    let lost = poly::area2(&fill) - poly::area2(&again);
    assert!(lost * 1000 < poly::area2(&fill), "no sub-min-width features left");
}

fn zone(id: u64, name: &str, layers: &[&str], outline: Vec<Point>, priority: u32, clearance: Option<Nm>) -> Zone {
    Zone {
        id: ObjectId(id),
        name: name.into(),
        net: Some(name.into()),
        layers: layers.iter().map(|l| l.to_string()).collect(),
        outline,
        priority,
        clearance,
        min_width: None,
        pads: PadConnection::Thermal,
        thermal_gap: None,
        thermal_spoke: None,
        thermal_angle: None,
        thermal_spokes: None,
    }
}

#[test]
fn project_fill_edges_priorities_keepouts() {
    let mut p = Project::new("t");
    let b = p.board_mut();
    let pt = |x: i64, y: i64| Point::new(Nm(x * MM), Nm(y * MM));
    let line = |x, y| Segment::Line { to: pt(x, y) };
    b.outline.contours = vec![
        Contour { start: pt(0, 0), segments: vec![line(30, 0), line(30, 20), line(0, 20), line(0, 0)] },
        // A round cutout: copper keeps copper_to_edge from it.
        Contour {
            start: pt(25, 10),
            segments: vec![
                Segment::Arc { mid: pt(23, 12), to: pt(21, 10) },
                Segment::Arc { mid: pt(23, 8), to: pt(25, 10) },
            ],
        },
    ];
    let big = vec![pt(-5, -5), pt(35, -5), pt(35, 25), pt(-5, 25)];
    b.zones.push(zone(10, "GND", &["F.Cu", "B.Cu"], big, 0, None));
    let small = vec![pt(2, 2), pt(10, 2), pt(10, 10), pt(2, 10)];
    b.zones.push(zone(11, "VCC", &["F.Cu"], small, 5, Some(Nm::from_um(500))));
    b.keepouts.push(Keepout {
        id: ObjectId(12),
        name: "ant".into(),
        layers: vec!["B.Cu".into()],
        outline: vec![pt(12, 12), pt(18, 12), pt(18, 18), pt(12, 18)],
        no_tracks: false,
        no_vias: false,
        no_pours: true,
        no_footprints: false,
        allow_nets: vec![],
    });
    for (i, (net, x, y)) in [("GND", 15, 5), ("VCC", 5, 5)].into_iter().enumerate() {
        b.vias.push(Via {
            id: ObjectId(20 + i as u64),
            at: pt(x, y),
            drill: Nm(300_000),
            diameter: Nm(600_000),
            net: Some(net.into()),
            from: "F.Cu".into(),
            to: "B.Cu".into(),
            locked: false,
        });
    }
    let base = crate::board::base_copper_items(&p);
    let fills = fill_zones(&p, &base);
    let names: Vec<(&str, &str)> = fills.iter().map(|f| (f.name.as_str(), f.layer.as_str())).collect();
    assert_eq!(names, [("GND", "F.Cu"), ("GND", "B.Cu"), ("VCC", "F.Cu")], "board order");
    assert!(fills.iter().all(|f| f.error.is_none() && !f.fill.is_empty()));
    let (gnd_f, gnd_b, vcc) = (&fills[0].fill, &fills[1].fill, &fills[2].fill);
    // The higher priority VCC pour is avoided by GND with the larger clearance.
    assert!(!poly::distance_less_than(gnd_f, vcc, 500_000));
    assert!(poly::distance_less_than(gnd_f, vcc, 520_000));
    // Edge clearance to the outline and the cutout.
    // Inside the true circle, so a distance to it is at most the distance to the true cutout.
    let cut = Circle::new(pp(23 * MM, 10 * MM), 2 * MM).to_ring(ArcTol::new(1, Side::Inside)).unwrap();
    let edge = p.board().rules.copper_to_edge.0;
    for f in [gnd_f, gnd_b] {
        assert!(!poly::distance_less_than(f, &cut, edge));
        let bb = f.bbox().unwrap();
        assert!(bb.min.x >= edge && bb.max.x <= 30 * MM - edge);
    }
    // The keep-out applies to B.Cu only.
    assert!(!inside(gnd_b, 15 * MM, 15 * MM));
    assert!(inside(gnd_f, 15 * MM, 15 * MM));
    // Copper items: one per island.
    let items = crate::board::copper_items(&p);
    let zi: Vec<&CopperItem> = items.iter().filter(|i| matches!(i.item, ItemRef::Zone(..))).collect();
    assert_eq!(zi.len(), 3);
    assert_eq!(zi[0].item.to_string(), "zone#10@F.Cu/0");
}

/// Later zones of a layer reuse earlier fills grown by their keep-away distance; the reuse is
/// keyed by fill and distance, so zones with different clearances get their own.
#[test]
fn grown_fills_match_fresh_offsets() {
    let mut p = Project::new("t");
    let b = p.board_mut();
    let pt = |x: i64, y: i64| Point::new(Nm(x * MM), Nm(y * MM));
    let line = |x, y| Segment::Line { to: pt(x, y) };
    b.outline.contours =
        vec![Contour { start: pt(0, 0), segments: vec![line(30, 0), line(30, 20), line(0, 20), line(0, 0)] }];
    let all = vec![pt(-5, -5), pt(35, -5), pt(35, 25), pt(-5, 25)];
    b.zones.push(zone(10, "A", &["F.Cu"], vec![pt(2, 2), pt(12, 2), pt(12, 18), pt(2, 18)], 3, None));
    let right = vec![pt(14, 2), pt(28, 2), pt(28, 18), pt(14, 18)];
    b.zones.push(zone(11, "B", &["F.Cu"], right, 2, Some(Nm::from_um(400))));
    b.zones.push(zone(12, "C", &["F.Cu"], all.clone(), 1, Some(Nm::from_um(300))));
    b.zones.push(zone(13, "D", &["F.Cu"], all, 0, None));
    for (i, (net, x, y)) in [("A", 5, 5), ("B", 20, 5), ("C", 13, 10), ("D", 13, 1)].into_iter().enumerate() {
        b.vias.push(Via {
            id: ObjectId(20 + i as u64),
            at: pt(x, y),
            drill: Nm(300_000),
            diameter: Nm(600_000),
            net: Some(net.into()),
            from: "F.Cu".into(),
            to: "B.Cu".into(),
            locked: false,
        });
    }
    let base = crate::board::base_copper_items(&p);
    let fills = fill_zones_uncached(&p, &base);
    assert_eq!(fills.len(), 4);
    assert!(fills.iter().all(|f| f.error.is_none() && !f.fill.is_empty()), "{fills:?}");
    // Reference: each lower zone filled with freshly grown higher-priority fills.
    let area = board_area(&p, p.board().rules.copper_to_edge).unwrap();
    let items_on: Vec<(&CopperItem, Nm)> =
        base.iter().filter(|it| it.layers.iter().any(|l| l == "F.Cu")).map(|it| (it, Nm::ZERO)).collect();
    for k in 1..fills.len() {
        let z = &p.board().zones[k];
        let prm = zone_params(&p, z);
        let mut keepaway = Vec::new();
        for f in &fills[..k] {
            let c = prm.clearance.max(f.clearance).0 + SAFETY;
            keepaway.extend(poly::offset(&f.fill, c, Join::Round, OBSTACLE_TOL).unwrap());
        }
        let outline: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
        let want = fill_layer(&LayerInput {
            net: z.net.as_deref(),
            outline: &outline,
            board: area.as_ref(),
            items: items_on.clone(),
            keepaway,
            params: prm,
        })
        .unwrap();
        assert_eq!(fills[k].fill, want, "zone {}", z.name);
    }
}

/// The in-process reuse of fills only answers for identical inputs: any change to the board,
/// the net classes or a net's class gives fresh fills.
#[test]
fn memo_follows_every_input() {
    use crate::model::circuit::{Net, NetClass};
    let mut p = Project::new("t");
    let pt = |x: i64, y: i64| Point::new(Nm(x * MM), Nm(y * MM));
    let line = |x, y| Segment::Line { to: pt(x, y) };
    let b = p.board_mut();
    b.outline.contours =
        vec![Contour { start: pt(0, 0), segments: vec![line(30, 0), line(30, 20), line(0, 20), line(0, 0)] }];
    b.zones.push(zone(10, "GND", &["F.Cu"], vec![pt(-5, -5), pt(35, -5), pt(35, 25), pt(-5, 25)], 0, None));
    for (i, (net, x, y)) in [("GND", 5, 5), ("SIG", 15, 10)].into_iter().enumerate() {
        b.vias.push(Via {
            id: ObjectId(20 + i as u64),
            at: pt(x, y),
            drill: Nm(300_000),
            diameter: Nm(600_000),
            net: Some(net.into()),
            from: "F.Cu".into(),
            to: "B.Cu".into(),
            locked: false,
        });
    }
    let check = |p: &Project, what: &str| {
        let base = crate::board::base_copper_items(p);
        let (cached, fresh) = (fill_zones(p, &base), fill_zones_uncached(p, &base));
        assert_eq!(cached.len(), fresh.len(), "{what}");
        for (a, b) in cached.iter().zip(&fresh) {
            assert_eq!((&a.fill, a.clearance), (&b.fill, b.clearance), "{what}");
        }
        cached[0].fill.clone()
    };
    let first = check(&p, "first");
    assert_eq!(check(&p, "unchanged"), first);
    let shared = p.clone();
    assert_eq!(check(&shared, "shared board"), first);
    p.board_mut().rules.clearance = Nm::from_um(600);
    let wider = check(&p, "rules");
    assert_ne!(wider, first);
    p.board_mut().vias[1].at = pt(20, 10);
    let moved = check(&p, "moved via");
    assert_ne!(moved, wider);
    let c = p.circuit_mut();
    c.netclasses.insert("wide".into(), NetClass { clearance: Some(Nm::from_um(900)), ..Default::default() });
    let mut sig = Net::new(ObjectId(30));
    sig.class = Some("wide".into());
    c.nets.insert("SIG".into(), sig);
    let classed = check(&p, "net class");
    assert_ne!(classed, moved);
    p.circuit_mut().netclasses.get_mut("wide").unwrap().clearance = Some(Nm::from_um(1_200));
    assert_ne!(check(&p, "class clearance"), classed);
}

/// The reliefs of pads whose surroundings are as they were in an earlier fill are taken from
/// it (`SpokeMemo`), and the fill is the one made from scratch, point for point: a grid of
/// pads with rows of four in each other's gaps, foreign tracks that come and go beside some
/// of them (whole spokes, trimmed spokes, spokes in the other directions, spokes from pad to
/// pad), filled in a sequence of boards that each differ from the last in one place.
#[test]
fn reliefs_reused_give_the_fill_made_from_scratch() {
    let outline = square(0, 0, 40 * MM, 40 * MM);
    let mut items: Vec<(CopperItem, Nm)> = Vec::new();
    for gx in 0..6 {
        for gy in 0..6 {
            let (cx, cy) = (4 * MM + gx * 6 * MM, 4 * MM + gy * 6 * MM);
            if (gx + gy) % 3 == 0 {
                for dx in [-750_000, -250_000, 250_000, 750_000] {
                    items.push((pad("GND", cx + dx, cy, 150_000, 250_000), Nm::ZERO));
                }
            } else {
                items.push((pad("GND", cx, cy, 500_000, 400_000), Nm::ZERO));
            }
        }
    }
    let pads = items.len();
    let prm = params(PadConnection::Thermal);
    let both = |items: &[(CopperItem, Nm)], what: &str| {
        let inp = input(Some("GND"), &outline, items, prm);
        // With the reliefs of earlier fills alone, and with the allowed area of a fill made
        // from the same copper of other nets too.
        let mut allowed = Stage::default();
        let (reused, fresh) = (fill_layer_with(&inp, true, None).unwrap(), fill_layer(&inp).unwrap());
        assert_eq!(reused, fresh, "{what}");
        assert_eq!(fill_layer_with(&inp, true, Some(&mut allowed)).unwrap(), fresh, "{what}, again");
        assert!(allowed.allowed.is_some(), "{what}: the allowed area is given back");
        assert_eq!(
            fill_layer_with(&inp, true, Some(&mut allowed)).unwrap(),
            fresh,
            "{what}, with the allowed area kept"
        );
        fresh
    };
    let bare = both(&items, "no track");
    // Tracks beside a pad, one more at a time: close enough to trim a spoke, then to drop it.
    let mut last = bare.clone();
    for k in 0..12i64 {
        let (cx, cy) = (4 * MM + (k % 6) * 6 * MM, 4 * MM + (k % 5) * 6 * MM);
        let off = 700_000 + 60_000 * k;
        items.push((track("A", (cx + off, cy - 2 * MM), (cx + off, cy + 2 * MM), 200_000), Nm::ZERO));
        if k % 2 == 0 {
            items.push((track("B", (cx - 2 * MM, cy + off), (cx + 2 * MM, cy + off), 200_000), Nm::ZERO));
        }
        let now = both(&items, &format!("track {k}"));
        assert_ne!(now, last, "track {k} changes the fill");
        last = now;
    }
    // And out again, the first first: the fill of each board is its own, whatever was filled before.
    while items.len() > pads {
        items.remove(pads);
        both(&items, "a track out");
    }
    assert_eq!(both(&items, "no track again"), bare);
    // A pad's own relief changes the pour and not the allowed area, which is taken from the
    // fill before: the fill is still the one made from scratch.
    for k in [0usize, 7, 20] {
        items[k].0.local.thermal =
            Some(crate::board::PadThermal { gap: Some(Nm(150_000)), spoke: Some(Nm(100_000)), ..Default::default() });
        assert_ne!(both(&items, &format!("pad {k} with a relief of its own")), bare);
    }
    items[3].0.local.connection = Some(PadConnection::Solid);
    both(&items, "a pad solid");
}

/// What is left of a trimmed spoke is copper that can be made, and a spoke may be trimmed down
/// to that (D119): a spoke squeezed between two tracks of another net is kept where what is
/// left is as wide as the minimum track (`Relief::min`, from the rules), however much of the
/// spoke that is, and dropped where it is narrower; with no minimum, half the spoke decides.
#[test]
fn trimmed_spoke_is_no_narrower_than_a_track() {
    let outline = square(0, 0, 20 * MM, 20 * MM);
    let (cx, cy) = (10 * MM, 10 * MM);
    // A pad with a spoke of 0.4 mm to the right, and two tracks of another net that leave
    // 0.24 mm of copper between their clearances where it crosses the relief gap.
    let mut prm = params(PadConnection::Thermal);
    prm.thermal_spoke = Nm(400_000);
    prm.thermal_angle = Some(Angle::ZERO);
    prm.thermal_spokes = 1;
    let half = 120_000 + prm.clearance.0 + SAFETY + 100_000;
    let items = vec![
        (pad("GND", cx, cy, 500_000, 500_000), Nm::ZERO),
        (track("A", (cx + 700_000, cy + half), (cx + 3 * MM, cy + half), 200_000), Nm::ZERO),
        (track("A", (cx + 700_000, cy - half), (cx + 3 * MM, cy - half), 200_000), Nm::ZERO),
    ];
    let pad_shape = items[0].0.shape.clone();
    let joined = |prm: ZoneParams| {
        let fill = fill_layer(&input(Some("GND"), &outline, &items, prm)).unwrap();
        fill.iter().any(|q| poly::intersects(q, &pad_shape))
    };
    assert!(joined(prm), "half the spoke's width is enough where no minimum is asked");
    prm.min_spoke = Nm(200_000);
    assert!(joined(prm), "0.24 mm is a track of 0.2 mm");
    prm.min_spoke = Nm(320_000);
    assert!(!joined(prm), "0.24 mm is no track of 0.32 mm");
    // A wider spoke: 0.24 mm is less than half of 0.6 mm, and a track of 0.2 mm all the same.
    prm.thermal_spoke = Nm(600_000);
    prm.min_spoke = Nm::ZERO;
    assert!(!joined(prm), "less than half the spoke, with no minimum to go by");
    prm.min_spoke = Nm(200_000);
    assert!(joined(prm), "trimmed down to the minimum track width");
}

/// The pour before its islands are told is made from the allowed area, the settings and the
/// pads of the net alone (`Stage::before`): the one of an earlier fill made from the same is
/// taken, the islands told of it as of one made from scratch, and one made in another allowed
/// area is not.
#[test]
fn pour_of_an_earlier_fill_gives_the_fill_made_from_scratch() {
    let outline = square(0, 0, 20 * MM, 10 * MM);
    let prm = params(PadConnection::Thermal);
    // A foreign track splits the zone; the net has a pad on the left of it.
    let mut items = vec![
        (track("A", (10 * MM, -MM), (10 * MM, 11 * MM), 300_000), Nm::ZERO),
        (pad("GND", 4 * MM, 5 * MM, 500_000, 400_000), Nm::ZERO),
    ];
    let mut first = Stage::default();
    let left = fill_layer_with(&input(Some("GND"), &outline, &items, prm), true, Some(&mut first)).unwrap();
    assert_eq!(left, fill_layer(&input(Some("GND"), &outline, &items, prm)).unwrap());
    assert_eq!(left.len(), 1, "the right half is an island");
    let (area, allowed, whole) = (first.area.unwrap(), first.allowed.unwrap(), first.whole.unwrap());
    assert_eq!(whole.len(), 2, "both halves before the islands are told");
    // A via of the net on the right: the allowed area and the pads are as they were, and the
    // right half is no island now.
    items.push((via("GND", 16 * MM, 5 * MM), Nm::ZERO));
    let fresh = fill_layer(&input(Some("GND"), &outline, &items, prm)).unwrap();
    assert_eq!(fresh.len(), 2);
    let mut kept =
        Stage { area: Some(area), allowed: Some(allowed.clone()), before: Some((None, whole.clone())), whole: None };
    assert_eq!(fill_layer_with(&input(Some("GND"), &outline, &items, prm), true, Some(&mut kept)).unwrap(), fresh);
    assert_eq!(kept.whole.as_ref(), Some(&whole), "given back as it was");
    assert_eq!(kept.allowed.as_ref(), Some(&allowed));
    // With an allowed area that is not known to be the earlier one: told by comparing them.
    let mut kept = Stage { before: Some((Some(allowed.clone()), whole.clone())), ..Default::default() };
    assert_eq!(fill_layer_with(&input(Some("GND"), &outline, &items, prm), true, Some(&mut kept)).unwrap(), fresh);
    // It is the pour given that the fill is made of: one that only this test could give.
    let marker = vec![rect(MM, MM, 6 * MM, 9 * MM)];
    let mut kept = Stage { before: Some((Some(allowed.clone()), marker.clone())), ..Default::default() };
    assert_eq!(fill_layer_with(&input(Some("GND"), &outline, &items, prm), true, Some(&mut kept)).unwrap(), marker);
    // Another foreign track: another allowed area, and the pour is made anew.
    items.push((track("B", (2 * MM, 2 * MM), (6 * MM, 2 * MM), 200_000), Nm::ZERO));
    let other = fill_layer(&input(Some("GND"), &outline, &items, prm)).unwrap();
    assert!(other != fresh, "the track changes the fill");
    let mut kept = Stage { before: Some((Some(allowed), marker)), ..Default::default() };
    assert_eq!(fill_layer_with(&input(Some("GND"), &outline, &items, prm), true, Some(&mut kept)).unwrap(), other);
}

/// The parts of earlier fills that a fill of another board takes (`LayerMemo`, `GrownMemo`)
/// give the fills made from scratch, board after board: a via of the pour's net that makes an
/// island a part of it and goes again, a track of another net inside one that is there (the
/// allowed area as it was), copper of another net far from a pour of higher priority (that
/// pour as it was, and grown as it was for the pour below it), a pour with nothing of its net
/// (empty, and kept so) that gets a via and loses it.
#[test]
fn parts_of_earlier_fills_give_the_fills_made_from_scratch() {
    let mut p = Project::new("t");
    let pt = |x: i64, y: i64| Point::new(Nm(x * MM), Nm(y * MM));
    let line = |x, y| Segment::Line { to: pt(x, y) };
    let b = p.board_mut();
    b.outline.contours =
        vec![Contour { start: pt(0, 0), segments: vec![line(40, 0), line(40, 20), line(0, 20), line(0, 0)] }];
    // Zone numbers that no other test has: the memo is by zone and layer, and of the process.
    b.zones.push(zone(9110, "GND", &["F.Cu"], vec![pt(-5, -5), pt(45, -5), pt(45, 25), pt(-5, 25)], 0, None));
    b.zones.push(zone(9111, "A", &["F.Cu"], vec![pt(2, 2), pt(8, 2), pt(8, 8), pt(2, 8)], 3, None));
    b.zones.push(zone(9112, "X", &["F.Cu"], vec![pt(9, 11), pt(13, 11), pt(13, 17), pt(9, 17)], 2, None));
    let via = |id: u64, net: &str, x: i64, y: i64| Via {
        id: ObjectId(id),
        at: pt(x, y),
        drill: Nm(300_000),
        diameter: Nm(600_000),
        net: Some(net.into()),
        from: "F.Cu".into(),
        to: "B.Cu".into(),
        locked: false,
    };
    let track = |id: u64, net: &str, a: Point, b: Point| Track {
        id: ObjectId(id),
        layer: "F.Cu".into(),
        width: Nm(300_000),
        net: Some(net.into()),
        start: a,
        end: b,
        mid: None,
        locked: false,
    };
    b.vias.push(via(9120, "GND", 15, 5));
    b.vias.push(via(9121, "A", 5, 5));
    // A track of another net from edge to edge: the pour's right half is an island.
    b.tracks.push(track(9130, "SIG", pt(22, -1), pt(22, 21)));
    let check = |p: &Project, what: &str| -> Vec<PolygonSet> {
        let base = crate::board::base_copper_items(p);
        let (kept, fresh) = (fill_zones(p, &base), fill_zones_uncached(p, &base));
        assert_eq!(kept.len(), fresh.len(), "{what}");
        for (a, b) in kept.iter().zip(&fresh) {
            assert!(a.error.is_none() && b.error.is_none(), "{what}: {:?} {:?}", a.error, b.error);
            assert_eq!(a.fill, b.fill, "{what}: {} on {}", a.name, a.layer);
        }
        kept.into_iter().map(|f| f.fill).collect()
    };
    let first = check(&p, "first");
    assert_eq!(first[0].len(), 1, "the right half is an island");
    assert!(!first[1].is_empty() && first[2].is_empty(), "A is filled, X has nothing of its net");
    p.board_mut().vias.push(via(9122, "GND", 30, 5));
    let both = check(&p, "a via of the net in the island");
    assert_eq!(both[0].len(), 2);
    assert_eq!((&both[1], &both[2]), (&first[1], &first[2]));
    p.board_mut().tracks.push(track(9131, "SIG", pt(22, 5), pt(22, 10)));
    assert_eq!(check(&p, "a track inside the track that is there"), both);
    p.board_mut().vias.push(via(9123, "SIG", 15, 15));
    let holed = check(&p, "a via of another net in the pour");
    assert!(holed[0] != both[0], "the pour keeps away from the via");
    assert_eq!(holed[1], both[1], "far from A");
    p.board_mut().vias.retain(|v| v.id != ObjectId(9122));
    let left = check(&p, "the via in the island out again");
    assert_eq!(left[0].len(), 1);
    p.board_mut().vias.push(via(9124, "X", 11, 14));
    let with_x = check(&p, "a via of X");
    assert!(!with_x[2].is_empty(), "X is filled round its via");
    assert!(with_x[0] != left[0], "and the pour below keeps away from it");
    p.board_mut().vias.retain(|v| v.id != ObjectId(9124));
    assert_eq!(check(&p, "the via of X out again"), left);
    p.board_mut().vias.retain(|v| v.id != ObjectId(9123));
    p.board_mut().tracks.retain(|t| t.id != ObjectId(9131));
    assert_eq!(check(&p, "as at first"), first);
}
