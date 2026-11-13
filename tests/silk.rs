//! Reference designators on the silkscreen (DECISIONS D82): the DRC rules `drc.silk_edge` and
//! `drc.silk_overlap`, the arrangement pass (`silk.arrange`, run by `place.auto`), the manual
//! overrides (`silk.set`, `silk.reset`, `silk.list`), and every output printing a designator
//! at the same place.

mod common;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::board::{BoardGraphic, GraphicKind, RefText};
use cadlab::{Diagnostic, Nm, ObjectRef, Point, Severity};
use common::silk;
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

/// An LDO and two 0402 capacitors on a 20 x 15 mm board; nothing placed.
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
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    (dir, r, s)
}

fn project(s: &Session) -> &cadlab::model::Project {
    s.project.as_ref().unwrap()
}

/// The silkscreen findings of the DRC.
fn findings(s: &Session) -> Vec<Diagnostic> {
    cadlab::drc::check(project(s)).into_iter().filter(|d| d.code.starts_with("drc.silk_")).collect()
}

fn text(d: &[Diagnostic]) -> String {
    d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n")
}

fn find<'a>(d: &'a [Diagnostic], code: &str, subjects: &[&str]) -> &'a Diagnostic {
    let want: Vec<ObjectRef> = subjects.iter().map(|s| ObjectRef::parse(s).unwrap()).collect();
    d.iter()
        .find(|x| x.code == code && want.iter().all(|w| x.subjects.contains(w)))
        .unwrap_or_else(|| panic!("no {code} about {subjects:?} in:\n{}", text(d)))
}

fn ref_text(s: &Session, refdes: &str) -> Option<RefText> {
    project(s).board().footprints[refdes].ref_text.clone()
}

/// `drc.silk_edge`: a designator across the board edge, one too close to it, one over a
/// cutout, and a footprint drawing at the edge; warnings, waivable, and a zero rule only
/// forbids crossing.
#[test]
fn silkscreen_at_the_board_edge() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "10mm"]}));
    // C1 near the top edge: its designator, above the courtyard, is printed across the edge.
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "13.9mm"]}));
    let d = findings(&s);
    let over = find(&d, "drc.silk_edge", &["text:C1/Reference"]);
    assert_eq!(over.severity, Severity::Warning);
    assert!(over.message.contains("reference text of C1 crosses the board edge"), "{}", over.message);
    assert!(over.hint.as_deref().unwrap().contains("silk.arrange"), "{over:?}");
    assert_eq!(d.len(), 1, "{}", text(&d));

    // Inside the board but 0.125 mm from the edge: the capitals end at 14.8 mm, the stroke
    // 0.075 mm farther.
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": ["5mm", "14.3mm"]}));
    let d = findings(&s);
    let near = find(&d, "drc.silk_edge", &["text:C1/Reference"]);
    assert!(near.message.starts_with("reference text of C1 is 0.12"), "{}", near.message);
    assert!(near.message.ends_with("mm from the board edge, minimum is 0.15mm"), "{}", near.message);
    // The rule is the board's: a smaller minimum accepts it, zero still refuses crossing.
    let o = exec(&r, &mut s, "board.rules", json!({"silk_to_edge": "0.1mm"}));
    assert_eq!(o["output"]["changed"][0]["field"], "silk_to_edge", "{o}");
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));
    exec(&r, &mut s, "board.rules", json!({"silk_to_edge": "0mm"}));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": ["5mm", "14.4mm"]}));
    assert!(findings(&s).is_empty(), "the stroke ends 0.025 mm from the edge: {}", text(&findings(&s)));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": ["5mm", "14.6mm"]}));
    assert!(find(&findings(&s), "drc.silk_edge", &["text:C1/Reference"]).message.contains("crosses"));
    exec(&r, &mut s, "board.rules", json!({"silk_to_edge": "0.15mm"}));

    // A waiver takes it out of the warnings, as for any finding.
    let o = exec(&r, &mut s, "drc.run", json!({"codes": ["drc.silk"]}));
    assert_eq!(o["output"]["warnings"], 1, "{o}");
    exec(
        &r,
        &mut s,
        "drc.waive",
        json!({"code": "drc.silk_edge", "subjects": ["text:C1/Reference"], "reason": "test"}),
    );
    let o = exec(&r, &mut s, "drc.run", json!({"codes": ["drc.silk"]}));
    assert_eq!((&o["output"]["warnings"], &o["output"]["waived"]), (&json!(0), &json!(1)), "{o}");
    exec(&r, &mut s, "drc.unwaive", json!({"code": "drc.silk_edge"}));
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["C1"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "10mm"]}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));

    // A cutout is an edge too: one under C2's designator.
    exec(&r, &mut s, "board.cutout", json!({"rect": {"from": ["14mm", "11mm"], "to": ["16mm", "13mm"]}}));
    let d = findings(&s);
    assert!(find(&d, "drc.silk_edge", &["text:C2/Reference"]).message.contains("crosses"), "{}", text(&d));
    exec(&r, &mut s, "board.cutout_remove", json!({"index": 1}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));

    // Footprint drawings: U1's outline 0.05 mm from the left edge is one finding naming U1.
    let x0 = cadlab::board::silk::silk_shapes(project(&s))
        .iter()
        .find(|x| x.label == "U1")
        .and_then(|x| x.bbox)
        .unwrap()
        .min
        .x;
    let shift = Nm(50_000 - x0);
    exec(&r, &mut s, "place.move", json!({"refdes": ["U1"], "by": [shift, "0mm"]}));
    let d = findings(&s);
    let drawing = find(&d, "drc.silk_edge", &["U1"]);
    assert!(drawing.message.starts_with("silkscreen of U1 is 0.0"), "{}", drawing.message);
    assert!(drawing.message.ends_with("mm from the board edge, minimum is 0.15mm"), "{}", drawing.message);
    assert!(drawing.hint.as_deref().unwrap().contains("drc.waive drc.silk_edge"), "{drawing:?}");
}

/// `drc.silk_overlap`: a designator on another footprint's drawing and on its own, two
/// designators on each other, a designator on a board drawing.
#[test]
fn silkscreen_texts_over_other_silkscreen() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["4mm", "4mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["16mm", "4mm"]}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));

    // C1's designator on U1's outline.
    let on_u1 = cadlab::board::silk::silk_shapes(project(&s)).iter().find(|x| x.label == "U1").unwrap().bbox.unwrap();
    let top = Point::new(Nm((on_u1.min.x + on_u1.max.x) / 2), Nm(on_u1.max.y));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": top}));
    let d = findings(&s);
    let hit = find(&d, "drc.silk_overlap", &["text:C1/Reference", "U1"]);
    assert_eq!(hit.severity, Severity::Warning);
    assert!(
        hit.message.contains("reference text of C1 overlaps the silkscreen of U1, minimum is 0.15mm"),
        "{}",
        hit.message
    );
    // U1's own designator there is as unreadable (D83; KiCad reports it too).
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["C1"]}));
    exec(&r, &mut s, "silk.set", json!({"refdes": "U1", "at": top}));
    let d = findings(&s);
    let own = find(&d, "drc.silk_overlap", &["text:U1/Reference", "U1"]);
    assert!(own.message.contains("reference text of U1 overlaps the silkscreen of U1"), "{}", own.message);
    assert_eq!(d.len(), 1, "{}", text(&d));
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["U1"]}));

    // Two designators 0.1 mm apart: cap height 1 mm, strokes 0.15 mm, centers 1.25 mm apart.
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": ["4mm", "11mm"]}));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C2", "at": ["4mm", "12.25mm"]}));
    let d = findings(&s);
    let pair = find(&d, "drc.silk_overlap", &["text:C1/Reference", "text:C2/Reference"]);
    assert!(pair.message.starts_with("reference text of C1 is 0."), "{}", pair.message);
    assert!(pair.message.ends_with("mm from the reference text of C2, minimum is 0.15mm"), "{}", pair.message);
    assert_eq!(d.len(), 1, "one finding per pair: {}", text(&d));
    // Touching is accepted with a zero rule, overlapping never.
    exec(&r, &mut s, "board.rules", json!({"silk_clearance": "0mm"}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C2", "at": ["4mm", "11.5mm"]}));
    assert!(find(&findings(&s), "drc.silk_overlap", &["text:C1/Reference"]).message.contains("overlaps"));
    exec(&r, &mut s, "board.rules", json!({"silk_clearance": "0.15mm"}));
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["C1", "C2"]}));

    // A board drawing under C2's designator, and a board text next to C1's.
    let p = s.project.as_mut().unwrap();
    let at = cadlab::board::silk::ref_text(p, "C2").unwrap().at;
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "F.SilkS".into(),
        kind: GraphicKind::Line { points: vec![at + mm(-2.0, 0.0), at + mm(2.0, 0.0)], width: Nm::from_um(150) },
    });
    let line = format!("graphic#{}", id.0);
    let at = cadlab::board::silk::ref_text(p, "C1").unwrap().at;
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "F.SilkS".into(),
        kind: GraphicKind::Text {
            text: "V1".into(),
            at,
            size: Nm::from_um(1000),
            rotation: Default::default(),
            style: Default::default(),
        },
    });
    let label = format!("graphic#{}", id.0);
    let d = findings(&s);
    find(&d, "drc.silk_overlap", &["text:C2/Reference", &line]);
    find(&d, "drc.silk_overlap", &["text:C1/Reference", &label]);
    // On the other side of the board nothing overlaps.
    exec(&r, &mut s, "place.flip", json!({"refdes": ["C1", "C2"]}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));
}

/// `drc.silk_overlap` between drawings (D83): a footprint's drawings against a board drawing
/// and against another footprint's, never two board drawings; and what lies wholly beyond the
/// board edge is reported for a designator only.
#[test]
fn silkscreen_drawings_of_different_owners() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "circuit.add", json!({"part": "AP2112K-3.3TRG1"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U2", "at": ["4mm", "11mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["4mm", "4mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["16mm", "4mm"]}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));
    // Silkscreen against silkscreen and against the edge (not against pads).
    let findings = |s: &Session| -> Vec<Diagnostic> {
        findings(s).into_iter().filter(|d| d.code != "drc.silk_over_pad").collect()
    };
    let shape = |s: &Session, label: &str| {
        cadlab::board::silk::silk_shapes(project(s)).iter().find(|x| x.label == label).unwrap().bbox.unwrap()
    };
    let u1 = shape(&s, "U1");
    let line = |s: &mut Session, a: Point, b: Point| {
        let p = s.project.as_mut().unwrap();
        let id = p.alloc_id();
        p.board_mut().graphics.push(BoardGraphic {
            id,
            layer: "F.SilkS".into(),
            kind: GraphicKind::Line { points: vec![a, b], width: Nm::from_um(150) },
        });
        format!("graphic#{}", id.0)
    };
    let n = |v: i64| Nm(v);

    // A board line through U1's outline, and a second board line across the first.
    let y = n((u1.min.y + u1.max.y) / 2);
    let across = line(&mut s, Point::new(n(u1.min.x - 1_000_000), y), Point::new(n(u1.max.x + 1_000_000), y));
    line(
        &mut s,
        Point::new(n(u1.max.x + 500_000), y - Nm(500_000)),
        Point::new(n(u1.max.x + 500_000), y + Nm(500_000)),
    );
    let d = findings(&s);
    let hit = find(&d, "drc.silk_overlap", &["U1", &across]);
    assert_eq!(hit.severity, Severity::Warning);
    assert_eq!(hit.message, format!("silkscreen of U1 overlaps the silkscreen of {across}, minimum is 0.15mm"));
    assert!(hit.hint.as_deref().unwrap().contains("drc.waive"), "{hit:?}");
    assert_eq!(d.len(), 1, "board drawings are not checked against each other: {}", text(&d));
    s.project.as_mut().unwrap().board_mut().graphics.clear();

    // A board line 0.1 mm above U1's drawing: too close at 0.15 mm, accepted when touching is.
    let y = n(u1.max.y + 100_000 + 75_000);
    let near = line(&mut s, Point::new(n(u1.min.x), y), Point::new(n(u1.max.x), y));
    let d = findings(&s);
    let hit = find(&d, "drc.silk_overlap", &["U1", &near]);
    assert!(hit.message.starts_with("silkscreen of U1 is 0.1mm from the silkscreen of"), "{}", hit.message);
    exec(&r, &mut s, "board.rules", json!({"silk_clearance": "0mm"}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));
    exec(&r, &mut s, "board.rules", json!({"silk_clearance": "0.15mm"}));
    s.project.as_mut().unwrap().board_mut().graphics.clear();

    // Two footprints: U2's drawing on U1's outline is one finding naming both; on the other
    // side of the board nothing prints over anything.
    let u2 = shape(&s, "U2");
    let by = Point::new(n(u1.min.x - u2.min.x + 300_000), n(u1.max.y - (u2.min.y + u2.max.y) / 2));
    exec(&r, &mut s, "place.move", json!({"refdes": ["U2"], "by": by}));
    exec(&r, &mut s, "silk.set", json!({"refdes": "U2", "hidden": true}));
    exec(&r, &mut s, "silk.set", json!({"refdes": "U1", "hidden": true}));
    let d = findings(&s);
    let hit = find(&d, "drc.silk_overlap", &["U1", "U2"]);
    assert_eq!(hit.message, "silkscreen of U1 overlaps the silkscreen of U2, minimum is 0.15mm");
    assert!(hit.hint.as_deref().unwrap().contains("place.move"), "{hit:?}");
    assert_eq!(d.len(), 1, "{}", text(&d));
    exec(&r, &mut s, "place.flip", json!({"refdes": ["U2"]}));
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));

    // Beyond the board edge and clear of it: a drawing and a board text are not printed and
    // not cut, so they are not reported; a designator there is missing from the legend.
    let out = Point::new(Nm::from_mm(-5), Nm::from_mm(5));
    line(&mut s, out, out + mm(2.0, 0.0));
    let p = s.project.as_mut().unwrap();
    let id = p.alloc_id();
    p.board_mut().graphics.push(BoardGraphic {
        id,
        layer: "F.SilkS".into(),
        kind: GraphicKind::Text {
            text: "note".into(),
            at: out + mm(0.0, 3.0),
            size: Nm::from_um(1000),
            rotation: Default::default(),
            style: Default::default(),
        },
    });
    assert!(findings(&s).is_empty(), "{}", text(&findings(&s)));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C2", "at": out + mm(0.0, -3.0)}));
    let d = findings(&s);
    assert_eq!(find(&d, "drc.silk_edge", &["text:C2/Reference"]).message, "reference text of C2 is off the board");
    assert_eq!(d.len(), 1, "{}", text(&d));
}

/// Board texts are drawn as their style says (D83): anchor, lines, character width, stroke,
/// slant, mirroring, knockout; the DRC measures those strokes and the KiCad export writes them.
#[test]
fn board_texts_follow_their_style() {
    use cadlab::model::board::{TextAlign, TextStyle, TextVAlign};
    let (_d, _r, mut s) = setup();
    let p = s.project.as_mut().unwrap();
    let at = mm(10.0, 7.0);
    let mut add = |text: &str, layer: &str, style: TextStyle| {
        let id = p.alloc_id();
        p.board_mut().graphics.push(BoardGraphic {
            id,
            layer: layer.into(),
            kind: GraphicKind::Text {
                text: text.into(),
                at,
                size: Nm::from_mm(1),
                rotation: Default::default(),
                style,
            },
        });
        let g = p.board().graphics.last().unwrap();
        (cadlab::board::text_ink(p, &g.layer, &g.kind).unwrap(), g.kind.clone())
    };
    let um = |v: i64| v as f64 / 1000.0;
    let plain = add("HELLO", "F.SilkS", TextStyle::default()).0.bbox().unwrap();
    // Centered both ways, capitals 1 mm high, the stroke an eighth of that but at least the
    // rules' minimum silkscreen width (0.15 mm).
    assert_eq!(um(plain.min.y - at.y.0), -575.0);
    assert_eq!(um(plain.max.y - at.y.0), 575.0);
    assert!((plain.min.x + plain.max.x - 2 * at.x.0).abs() < 300_000, "{plain:?}");
    // Left and bottom: the text starts at the anchor and stands on it.
    let left = TextStyle { align: TextAlign::Left, valign: TextVAlign::Bottom, ..Default::default() };
    let b = add("HELLO", "F.SilkS", left).0.bbox().unwrap();
    assert!(b.min.x >= at.x.0 - 75_000 && b.min.x < at.x.0 + 300_000, "{b:?}");
    assert_eq!(um(b.min.y - at.y.0), -75.0);
    assert!(((b.max.x - b.min.x) - (plain.max.x - plain.min.x)).abs() <= 2, "{b:?}");
    // Right and top: it ends at the anchor and hangs from it.
    let right = TextStyle { align: TextAlign::Right, valign: TextVAlign::Top, ..Default::default() };
    let b = add("HELLO", "F.SilkS", right).0.bbox().unwrap();
    assert!(b.max.x <= at.x.0 + 75_000 && b.max.x > at.x.0 - 300_000, "{b:?}");
    assert_eq!(um(b.max.y - at.y.0), 75.0);
    // On the bottom the same text runs the other way; unless it is not mirrored.
    let b = add("HELLO", "B.SilkS", left).0.bbox().unwrap();
    assert!(b.max.x <= at.x.0 + 75_000, "{b:?}");
    let unmirrored = TextStyle { mirror: Some(false), ..left };
    let b = add("HELLO", "B.SilkS", unmirrored).0.bbox().unwrap();
    assert!(b.min.x >= at.x.0 - 75_000, "{b:?}");
    // Two lines are one line pitch higher; twice the character width is twice as wide between
    // the stroke centers; a stroke of its own.
    let two = add("HELLO\nHELLO", "F.SilkS", TextStyle::default()).0.bbox().unwrap();
    assert!(two.max.y - two.min.y > plain.max.y - plain.min.y + 1_400_000, "{two:?}");
    assert!(((two.max.x - two.min.x) - (plain.max.x - plain.min.x)).abs() <= 2, "{two:?}");
    let wide = TextStyle { width: Some(Nm::from_mm(2)), thickness: Some(Nm::from_um(200)), ..Default::default() };
    let (ink, _) = add("HELLO", "F.SilkS", wide);
    assert_eq!(ink.width, Nm::from_um(200));
    let b = ink.bbox().unwrap();
    assert!(((b.max.x - b.min.x - 200_000) - 2 * (plain.max.x - plain.min.x - 150_000)).abs() <= 2, "{b:?}");
    // Italic leans to the right; a knockout prints the box and leaves the letters open.
    let b = add("HELLO", "F.SilkS", TextStyle { italic: true, ..left }).0.bbox().unwrap();
    assert!(b.max.x > plain.max.x - plain.min.x + at.x.0, "{b:?}");
    let (ink, _) = add("HELLO", "F.SilkS", TextStyle { knockout: true, ..Default::default() });
    let k = ink.bbox().unwrap();
    assert!(k.min.x < plain.min.x && k.max.x > plain.max.x && k.min.y < plain.min.y && k.max.y > plain.max.y);
    let area = |set: &cadlab::geom::poly::PolygonSet| -> f64 { cadlab::geom::poly::area2(set) as f64 };
    let (boxed, letters) = (area(&ink.knockout_box()), area(&ink.strokes()));
    assert!((area(&ink.region()) - (boxed - letters)).abs() < 1e-6 * boxed, "the box without the letters");

    // The KiCad export says how each is drawn, and the import reads it back the same.
    let p = s.project.as_ref().unwrap();
    let export = cadlab::kicad_pcb::export(p, "board");
    for want in [
        "(effects (font (size 1 1) (thickness 0.15)))",
        "(effects (font (size 1 1) (thickness 0.15)) (justify left bottom))",
        "(effects (font (size 1 1) (thickness 0.15)) (justify right top))",
        "(effects (font (size 1 1) (thickness 0.15)) (justify left bottom mirror))",
        "(effects (font (size 1 2) (thickness 0.2)))",
        "(effects (font (size 1 1) (thickness 0.15) (italic yes)) (justify left bottom))",
        "(layer \"F.SilkS\" knockout)",
        "\"HELLO\\nHELLO\"",
    ] {
        assert!(export.pcb.contains(want), "the export has {want}");
    }
    let mut q = cadlab::model::Project::new("back");
    cadlab::kicad_import::import(&mut q, &export.pcb, &Default::default()).unwrap();
    let kinds = |p: &cadlab::model::Project| -> Vec<GraphicKind> {
        p.board().graphics.iter().map(|g| g.kind.clone()).collect()
    };
    assert_eq!(kinds(&q), kinds(p));
}

/// `silk.arrange` moves designators off the edge and off other silkscreen, in a way that
/// depends on nothing but the board; texts set by hand stay; turning a part hands an arranged
/// text back to its default place.
#[test]
fn arrange_moves_designators_to_free_positions() {
    let (_d, r, mut s) = setup();
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7mm"]}));
    // C1 under the top edge; C2 right under U1, its designator on U1's drawing.
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "14mm"]}));
    let under = cadlab::board::place::courtyard_box(project(&s), "U1").unwrap();
    exec(
        &r,
        &mut s,
        "place.set",
        json!({"refdes": "C2", "at": [Nm((under.x0 + under.x1) / 2), Nm(under.y0 - 750_000)]}),
    );
    let d = findings(&s);
    find(&d, "drc.silk_edge", &["text:C1/Reference"]);
    find(&d, "drc.silk_overlap", &["text:C2/Reference", "U1"]);
    assert!(!silk::problems(project(&s), silk::KICAD_SILK_CLEARANCE).is_empty());

    let o = exec(&r, &mut s, "silk.arrange", json!({}));
    assert!(o["diagnostics"].as_array().unwrap().is_empty(), "{o}");
    assert_eq!(o["output"]["texts"], 3, "{o}");
    let moved: Vec<&String> = o["output"]["moved"].as_object().unwrap().keys().collect();
    assert_eq!(moved, ["C1", "C2"], "U1's designator was free above it: {o}");
    assert_eq!(o["output"]["moved"]["C1"]["origin"], "arranged", "{o}");
    assert_eq!(ref_text(&s, "U1"), None);
    silk::assert_legible(project(&s));
    // C1's designator went below it, the first free position after "above".
    let c1 = ref_text(&s, "C1").unwrap();
    assert!(c1.offset.unwrap().y < Nm(0) && c1.offset.unwrap().x == Nm(0) && !c1.manual, "{c1:?}");

    // Again: nothing moves. From scratch: the same result.
    let before = project(&s).board().footprints.clone();
    let again = exec(&r, &mut s, "silk.arrange", json!({}));
    assert_eq!(again["output"]["moved"], json!({}), "{again}");
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["C1", "C2", "U1"]}));
    exec(&r, &mut s, "silk.arrange", json!({}));
    assert_eq!(project(&s).board().footprints, before);

    // A text set by hand is never moved, even where it is clipped; the others keep clear of it.
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": ["5mm", "14.8mm"], "rotation": 0}));
    let by_hand = ref_text(&s, "C1").unwrap();
    assert!(by_hand.manual);
    let o = exec(&r, &mut s, "silk.arrange", json!({}));
    assert_eq!((&o["output"]["manual"], &o["output"]["texts"]), (&json!(1), &json!(2)), "{o}");
    assert_eq!(ref_text(&s, "C1").unwrap(), by_hand);
    find(&findings(&s), "drc.silk_edge", &["text:C1/Reference"]);
    let l = exec(&r, &mut s, "silk.list", json!({}));
    assert_eq!(l["output"]["texts"]["C1"]["origin"], "manual", "{l}");
    assert_eq!(l["output"]["texts"]["C2"]["origin"], "arranged", "{l}");
    assert_eq!(l["output"]["texts"]["U1"]["origin"], "default", "{l}");
    // Handed back, it is arranged again.
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["C1"]}));
    exec(&r, &mut s, "silk.arrange", json!({"refdes": ["C1"]}));
    assert_eq!(project(&s).board().footprints, before);

    // Moving a part takes its designator along; turning it drops an arranged position (it was
    // chosen for the other orientation), not one set by hand.
    let was = cadlab::board::silk::ref_text(project(&s), "C2").unwrap().at;
    exec(&r, &mut s, "place.move", json!({"refdes": ["C2"], "by": ["1mm", "0mm"]}));
    assert_eq!(cadlab::board::silk::ref_text(project(&s), "C2").unwrap().at, was + mm(1.0, 0.0));
    exec(&r, &mut s, "place.rotate", json!({"refdes": ["C2"]}));
    assert_eq!(ref_text(&s, "C2"), None);
    exec(&r, &mut s, "silk.set", json!({"refdes": "C2", "at": ["15mm", "3mm"], "rotation": 90, "size": "0.8mm"}));
    exec(&r, &mut s, "place.rotate", json!({"refdes": ["C2"]}));
    let kept = ref_text(&s, "C2").unwrap();
    assert_eq!((kept.rotation, kept.size, kept.manual), (cadlab::units::Angle::DEG_90, Some(Nm::from_um(800)), true));
}

/// Without a free position the designator is left out of the legend, with a warning naming it;
/// `place.auto` arranges by itself and reports the same.
#[test]
fn no_room_hides_the_designator_with_a_warning() {
    let (_d, r, mut s) = setup();
    // A board the LDO just fits on: no room for "U1" beside it or inside it.
    let b = {
        exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7mm"]}));
        cadlab::board::place::courtyard_box(project(&s), "U1").unwrap()
    };
    let (w, h) = (Nm(b.x1 - b.x0 + 1_600_000), Nm(b.y1 - b.y0 + 1_600_000));
    exec(&r, &mut s, "board.outline", json!({"width": w, "height": h}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": [Nm(w.0 / 2), Nm(h.0 / 2)]}));
    find(&findings(&s), "drc.silk_edge", &["text:U1/Reference"]);

    let o = exec(&r, &mut s, "silk.arrange", json!({}));
    let d = &o["diagnostics"][0];
    assert_eq!(
        (&d["code"], &d["severity"], &d["subjects"]),
        (&json!("silk.hidden"), &json!("warning"), &json!(["U1"]))
    );
    assert!(d["hint"].as_str().unwrap().contains("silk.set"), "{d}");
    assert_eq!(o["output"]["hidden"], json!(["U1"]), "{o}");
    assert!(ref_text(&s, "U1").unwrap().hidden);
    assert!(cadlab::board::silk::ref_text(project(&s), "U1").is_none());
    assert!(findings(&s).is_empty(), "nothing is printed clipped: {}", text(&findings(&s)));
    let l = exec(&r, &mut s, "silk.list", json!({}));
    assert_eq!(l["output"]["texts"]["U1"]["hidden"], true, "{l}");
    // No strokes for it in the legend, hidden in the KiCad export.
    let legend = |s: &Session| {
        let files = cadlab::fabout::gerbers(project(s), &cadlab::fabout::Options::default());
        files.into_iter().find(|f| f.name.ends_with("F_SilkS.gbr")).unwrap().content
    };
    let hidden = legend(&s);
    let pcb = cadlab::kicad_pcb::export(project(&s), "p").pcb;
    let field = pcb.lines().find(|l| l.contains("(property \"Reference\" \"U1\"")).unwrap();
    assert!(field.contains("(hide yes)"), "{field}");
    // By hand it can still be printed, where the designer accepts it.
    exec(&r, &mut s, "silk.set", json!({"refdes": "U1", "hidden": false}));
    assert!(legend(&s).len() > hidden.len());
    find(&findings(&s), "drc.silk_edge", &["text:U1/Reference"]);

    // `place.auto` on a roomy board: designators arranged, nothing reported.
    exec(&r, &mut s, "silk.reset", json!({"refdes": ["U1"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm"}));
    let o = exec(&r, &mut s, "place.auto", json!({"replace": true}));
    assert!(o["diagnostics"].as_array().unwrap().is_empty(), "{o}");
    silk::assert_legible(project(&s));
    assert!(silk::hidden(project(&s)).is_empty());
}

/// Every output prints a designator where `board::silk::ref_text` says: the Gerber legend,
/// IPC-2581, the render and the KiCad export (whose import gives the same text back).
#[test]
fn every_output_draws_the_same_text() {
    let (d, r, mut s) = setup();
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["4mm", "4mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["16mm", "4mm"], "side": "bottom"}));
    let outputs = |r: &Registry, s: &mut Session| -> Vec<String> {
        let o = cadlab::fabout::Options::default();
        let p = s.project.as_ref().unwrap();
        let mut v: Vec<String> = cadlab::fabout::gerbers(p, &o)
            .into_iter()
            .filter(|f| f.name.ends_with("_SilkS.gbr"))
            .map(|f| f.content)
            .collect();
        v.push(cadlab::fabout::ipc2581::document(p, &o).content);
        v.push(cadlab::kicad_pcb::export(p, "p").pcb);
        for side in ["top", "bottom"] {
            let path = d.path().join(format!("{side}.svg"));
            exec(r, s, "render.board", json!({"path": path, "realistic": side}));
            v.push(std::fs::read_to_string(path).unwrap());
        }
        v
    };
    let base = outputs(&r, &mut s);

    // A designator moved, turned and resized by hand: every output changes...
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "at": ["4mm", "8mm"], "rotation": 90, "size": "1.2mm"}));
    exec(&r, &mut s, "silk.set", json!({"refdes": "C2", "at": ["16mm", "8mm"], "rotation": 270}));
    let moved = outputs(&r, &mut s);
    for (i, (a, b)) in base.iter().zip(&moved).enumerate() {
        assert!(a != b, "output {i} does not follow the designators");
    }
    // ... to the strokes of `ref_text`: the top legend draws C1's first stroke where it says.
    let p = project(&s);
    let t = cadlab::board::silk::ref_text(p, "C1").unwrap();
    assert_eq!((t.at, t.size, t.width), (mm(4.0, 8.0), Nm::from_um(1200), Nm::from_um(180)));
    assert_eq!(cadlab::fabout::refdes_text(p, "C1"), Some(t));
    let first = t.paths("C1")[0][0];
    // Gerber coordinates are nanometers (format 4.6 in millimeters).
    let start = format!("X{}Y{}D02*", first.x.0, first.y.0);
    assert!(moved[0].contains(&start), "no stroke starting at {start} in the top legend");
    assert!(!base[0].contains(&start));
    // The KiCad export: position in the footprint's frame (C1 is turned by 90°), angle on the
    // board, height and stroke. KiCad centers a box around the text on the position, which
    // reaches farther below the baseline than above the capitals (D83): for this text turned a
    // quarter (its top to the left of the board, which is +Y of the footprint's KiCad frame),
    // the position is that much towards its baseline of the text's center at (4, 0).
    use cadlab::model::board::{TextAlign, TextVAlign};
    let pcb = &moved[3];
    let at = pcb.lines().position(|l| l.contains("(property \"Reference\" \"C1\"")).unwrap();
    let field: Vec<&str> = pcb.lines().skip(at).take(2).collect();
    let quarter = cadlab::units::Angle::DEG_90;
    let (h, w) = (Nm::from_um(1200), Nm::from_um(180));
    let centered = (TextAlign::Center, TextVAlign::Middle);
    let shift = cadlab::kicad_pcb::kicad_text_shift(h, w, centered, false, quarter, false);
    assert!(shift.x < Nm::ZERO && shift.x > Nm::from_um(-150) && shift.y == Nm::ZERO, "{shift:?}");
    let y = format!("{}", -shift.x.0 as f64 / 1e6);
    assert!(field[0].contains(&format!("(at 4 {y} 90) (layer \"F.SilkS\")")), "{field:?} for {y}");
    assert!(field[1].contains("(font (size 1.2 1.2) (thickness 0.18))"), "{field:?}");
    // C2 reads from top to bottom: KiCad would turn it upright unless told not to.
    let at = pcb.lines().position(|l| l.contains("(property \"Reference\" \"C2\"")).unwrap();
    let field: Vec<&str> = pcb.lines().skip(at).take(2).collect();
    assert!(
        field[0].contains("270) (unlocked yes) (layer \"B.SilkS\")") && field[1].contains("(justify mirror)"),
        "{field:?}"
    );

    // Imported back, the texts are where they were, set by hand; hidden ones stay hidden.
    exec(&r, &mut s, "silk.set", json!({"refdes": "U1", "hidden": true}));
    let p = project(&s).clone();
    let export = cadlab::kicad_pcb::export(&p, "p");
    let mut q = p.clone();
    *q.board_mut() = Default::default();
    cadlab::kicad_import::import(&mut q, &export.pcb, &Default::default()).unwrap();
    for refdes in ["C1", "C2", "U1"] {
        let (a, b) = (&p.board().footprints[refdes], &q.board().footprints[refdes]);
        assert_eq!(a.ref_text, b.ref_text, "{refdes}");
        assert!(b.ref_text.as_ref().unwrap().manual);
    }
    assert!(q.board().footprints["U1"].ref_text.as_ref().unwrap().hidden);
    // A fab that prints nothing under 1 mm says so for a smaller designator.
    exec(&r, &mut s, "silk.set", json!({"refdes": "C1", "size": "0.6mm"}));
    let o = exec(&r, &mut s, "fab.check", json!({"fab": "jlcpcb"}));
    let small: Vec<&Value> =
        o["diagnostics"].as_array().unwrap().iter().filter(|x| x["code"] == "fab.silk_height").collect();
    assert_eq!(small.len(), 1, "{o}");
    assert_eq!(small[0]["subjects"], json!(["C1"]), "{o}");
}
