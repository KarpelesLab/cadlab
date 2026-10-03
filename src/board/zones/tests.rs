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
    }
}

fn params(pads: PadConnection) -> ZoneParams {
    ZoneParams {
        clearance: Nm(200_000),
        min_width: Nm(200_000),
        pads,
        thermal_gap: Nm(300_000),
        thermal_spoke: Nm(250_000),
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
