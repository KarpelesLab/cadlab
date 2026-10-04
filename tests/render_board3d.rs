//! 3D board view (M9): determinism, pixel probes, options, and the STM32 board's timing.

mod common;

use cadlab::command::{Registry, RunOptions, Session};
use cadlab::model::board::BoardSide;
use cadlab::render::board3d::{Image3d, Options3d, render};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// `CADLAB_RENDER3D_KEEP=<dir>` keeps the images for review.
fn keep(name: &str, img: &Image3d) {
    if let Some(dir) = std::env::var_os("CADLAB_RENDER3D_KEEP") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(name), img.to_png().unwrap()).unwrap();
    }
}

/// The LDO + caps board of `tests/render_board.rs`: U1 (SOT-23-5) on top, R1 (0603) on the
/// bottom, a via and a mounting hole.
fn ldo_board() -> (tempfile::TempDir, Registry, Session) {
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
    exec(&r, &mut s, "circuit.add", json!({"part": "R 10k 1% 0603"}));
    exec(&r, &mut s, "net.connect", json!({"net": "VIN", "pins": ["U1.VIN", "U1.EN", "C1.1", "R1.1"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "GND", "pins": ["U1.GND", "C1.2", "C2.2"]}));
    exec(&r, &mut s, "net.connect", json!({"net": "3V3", "pins": ["U1.VOUT", "C2.1", "R1.2"]}));
    exec(&r, &mut s, "board.outline", json!({"width": "20mm", "height": "15mm", "corner_radius": "1mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "U1", "at": ["10mm", "7.5mm"]}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C1", "at": ["5mm", "9mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C2", "at": ["15mm", "9mm"], "rotation": 90}));
    exec(&r, &mut s, "place.set", json!({"refdes": "R1", "at": ["10mm", "3mm"], "side": "bottom"}));
    exec(&r, &mut s, "track.add", json!({"layer": "F.Cu", "points": ["U1.VOUT", ["13mm", "9mm"], "C2.1"]}));
    exec(&r, &mut s, "via.add", json!({"at": ["7mm", "4mm"], "net": "GND"}));
    exec(&r, &mut s, "board.hole", json!({"at": ["3mm", "3mm"], "drill": "2mm"}));
    (dir, r, s)
}

fn project(s: &Session) -> &cadlab::model::Project {
    s.project.as_ref().unwrap()
}

fn mm(v: cadlab::units::Nm) -> f64 {
    v.0 as f64 / 1e6
}

/// Pixel at the projection of a board point.
fn probe(img: &Image3d, p: [f64; 3]) -> [u8; 3] {
    let (x, y) = img.camera.project(p);
    img.pixel(x as u32, y as u32)
}

fn is_orange(c: [u8; 3]) -> bool {
    c[0] > 150 && c[0] as i32 - c[2] as i32 > 80 && c[1] < c[0]
}

fn is_dark(c: [u8; 3]) -> bool {
    c.iter().all(|&v| v < 70)
}

#[test]
fn deterministic_with_bodies_where_parts_are() {
    let (_d, _r, s) = ldo_board();
    let p = project(&s);
    let o = Options3d { size: 600, ..Default::default() };
    let a = render(p, &o).unwrap();
    let b = render(p, &o).unwrap();
    assert_eq!(a.to_png().unwrap(), b.to_png().unwrap(), "same input, same bytes");
    keep("ldo_iso.png", &a);
    assert_eq!(a.width.max(a.height), 600);

    // U1's molded body top (SOT-23-5, 1.45 mm max) is dark where it projects.
    let u1 = p.board().footprints["U1"].at;
    let top = [mm(u1.x), mm(u1.y), 1.4];
    assert!(is_dark(probe(&a, top)), "U1 body: {:?}", probe(&a, top));
    // Without components, the same spot shows the green mask (or a finish/silk color), not mold.
    let bare = render(p, &Options3d { components: false, ..o.clone() }).unwrap();
    keep("ldo_bare.png", &bare);
    let c = probe(&bare, [mm(u1.x) + 0.0, mm(u1.y) + 0.0, 0.0]);
    assert!(!is_dark(c), "{c:?}");
    // The board's green mask between parts.
    let c = probe(&a, [12.0, 12.0, 0.0]);
    assert!(c[1] > c[0] && c[1] > c[2], "mask green: {c:?}");

    // Highlight paints U1 orange.
    let hl = render(p, &Options3d { highlight: vec!["U1".into()], ..o.clone() }).unwrap();
    keep("ldo_hl.png", &hl);
    assert!(is_orange(probe(&hl, top)), "{:?}", probe(&hl, top));

    // The mounting hole is see-through when looking straight down: background color.
    let down = render(p, &Options3d { elevation: 90.0, components: false, ..o.clone() }).unwrap();
    keep("ldo_down.png", &down);
    let (_, y) = down.camera.project([3.0, 3.0, 0.0]);
    let bg = down.pixel(1, y as u32); // the background is a vertical gradient
    let hole = probe(&down, [3.0, 3.0, 0.0]);
    let diff: i32 = (0..3).map(|i| (hole[i] as i32 - bg[i] as i32).abs()).sum();
    assert!(diff < 30, "hole {hole:?} vs background {bg:?}");
}

#[test]
fn bottom_view_shows_bottom_parts() {
    let (_d, _r, s) = ldo_board();
    let p = project(&s);
    let t = mm(p.board().stackup.thickness);
    let o = Options3d { size: 600, side: BoardSide::Bottom, highlight: vec!["R1".into()], ..Default::default() };
    let img = render(p, &o).unwrap();
    keep("ldo_bottom.png", &img);
    let r1 = p.board().footprints["R1"].at;
    // R1 (0603, 0.45 mm high) hangs under the board.
    let c = probe(&img, [mm(r1.x), mm(r1.y), -t - 0.4]);
    assert!(is_orange(c), "R1 from below: {c:?}");
    // From above, R1 is hidden by the board.
    let top = render(p, &Options3d { side: BoardSide::Top, ..o.clone() }).unwrap();
    let c = probe(&top, [mm(r1.x), mm(r1.y), 0.0]);
    assert!(!is_orange(c), "{c:?}");
    assert_ne!(img.to_png().unwrap(), top.to_png().unwrap());
}

#[test]
fn command_writes_png_and_validates() {
    let (dir, r, mut s) = ldo_board();
    let o = exec(&r, &mut s, "render.board3d", json!({"size": 400}));
    assert_eq!(o["output"]["format"], "png");
    assert_eq!(o["output"]["png"], o["output"]["path"], "MCP attaches the image");
    let png = std::fs::read(dir.path().join("p/out/board3d.png")).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let o = exec(
        &r,
        &mut s,
        "render.board3d",
        json!({"path": "b.png", "view": "bottom", "azimuth": -30, "elevation": 60, "px_per_mm": 8}),
    );
    assert!(o["output"]["width"].as_f64().unwrap() > 100.0);
    for (args, code) in [
        (json!({"highlight": ["U9"]}), "render.unknown_highlight"),
        (json!({"elevation": 2}), "render.bad_angle"),
        (json!({"path": "x.svg"}), "render.format"),
    ] {
        let f = r.execute(&mut s, "render.board3d", args, RunOptions::default()).unwrap_err();
        assert_eq!(f.error.diagnostic.code, code);
    }
}

#[test]
fn stm32_board_renders_quickly() {
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let dir = tempfile::tempdir().unwrap();
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p")}));
    common::boards::build_stm32_board(&r, &mut s);
    exec(&r, &mut s, "board.outline", json!({"width": "60mm", "height": "45mm", "corner_radius": "2mm"}));
    exec(&r, &mut s, "place.auto", json!({"spacing": "1mm"}));
    exec(&r, &mut s, "place.set", json!({"refdes": "C9", "at": ["30mm", "40mm"], "side": "bottom"}));
    let p = project(&s);
    let start = std::time::Instant::now();
    let img = render(p, &Options3d::default()).unwrap();
    let took = start.elapsed();
    keep("stm32_iso.png", &img);
    eprintln!("stm32 3D view: {} x {} px in {took:?}", img.width, img.height);
    if !cfg!(debug_assertions) {
        assert!(took.as_secs_f64() < 2.0, "{took:?}");
    }
    // The LQFP-48's body top is dark at its center.
    let u1 = p.board().footprints["U1"].at;
    assert!(is_dark(probe(&img, [mm(u1.x), mm(u1.y), 1.5])));
    let bottom = render(p, &Options3d { side: BoardSide::Bottom, ..Default::default() }).unwrap();
    keep("stm32_bottom.png", &bottom);
}
