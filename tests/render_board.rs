//! Board rendering (M4): layer view, filters, highlight, realistic views, crop, markers.

use cadlab::command::{Registry, RunOptions, Session};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {}", f.error),
    }
}

/// The LDO + caps board from `tests/board.rs`, placed (R1 on the bottom) and partially routed.
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
    exec(&r, &mut s, "track.add", json!({"layer": "B.Cu", "points": [["7mm", "4mm"], ["14mm", "4mm"]], "net": "GND"}));
    (dir, r, s)
}

fn svg(r: &Registry, s: &mut Session, dir: &std::path::Path, name: &str, mut args: Value) -> String {
    args["path"] = json!(name);
    let o = exec(r, s, "render.board", args);
    assert_eq!(o["output"]["format"], "svg");
    std::fs::read_to_string(dir.join("p").join(name)).unwrap()
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[1..4], b"PNG");
    let be = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn renders_board_png_and_svg() {
    let (dir, r, mut s) = setup();
    let o = exec(&r, &mut s, "render.board", json!({}));
    assert_eq!(o["output"]["format"], "png");
    assert_eq!(o["output"]["png"], o["output"]["path"], "MCP attaches the image");
    let png = std::fs::read(dir.path().join("p/out/board.png")).unwrap();
    // 20 x 15 mm board + 1 mm margin each side, longest side ~1600 px.
    let (w, h) = png_size(&png);
    assert_eq!((w as f64, h as f64), (o["output"]["width"].as_f64().unwrap(), o["output"]["height"].as_f64().unwrap()));
    assert!((1590..=1610).contains(&w), "{w}");
    assert!((h as f64 / w as f64 - 17.0 / 22.0).abs() < 0.01);

    let o = exec(&r, &mut s, "render.board", json!({"path": "fixed.png", "px_per_mm": 10}));
    assert_eq!((o["output"]["width"].as_f64(), o["output"]["height"].as_f64()), (Some(220.0), Some(170.0)));

    // SVG is deterministic and in millimeters.
    let a = svg(&r, &mut s, dir.path(), "a.svg", json!({}));
    let b = svg(&r, &mut s, dir.path(), "b.svg", json!({}));
    assert_eq!(a, b);
    assert!(a.contains(r#"width="22mm" height="17mm""#), "{}", &a[..200]);
    assert!(a.contains("rgba(210,60,60,"), "front copper");
    assert!(a.contains("rgba(61,127,217,"), "back copper");
    assert!(a.contains("#e6d200"), "outline");
    assert!(a.contains("#e8d34a"), "bottom silkscreen (R1)");
    assert!(a.contains(r##"fill="#0e1116""##), "drill holes");
}

#[test]
fn layer_filters_and_highlight_change_output() {
    let (dir, r, mut s) = setup();
    let all = svg(&r, &mut s, dir.path(), "all.svg", json!({}));
    let front = svg(&r, &mut s, dir.path(), "front.svg", json!({"layers": ["F.Cu", "Edge.Cuts"], "ratsnest": false}));
    assert_ne!(all, front);
    assert!(front.contains("rgba(210,60,60,"));
    assert!(!front.contains("rgba(61,127,217,"), "no back copper");
    assert!(!front.contains("#f0f0f0"), "no silkscreen");
    let fab = svg(&r, &mut s, dir.path(), "fab.svg", json!({"layers": ["F.Fab", "F.CrtYd"]}));
    assert!(fab.contains("#a8a8a8") && fab.contains("#e040e0"));

    let hl = svg(&r, &mut s, dir.path(), "hl.svg", json!({"highlight": ["GND"]}));
    assert_ne!(all, hl);
    let hl_u1 = svg(&r, &mut s, dir.path(), "hl_u1.svg", json!({"highlight": ["U1"]}));
    assert_ne!(hl, hl_u1);

    let no_rats = svg(&r, &mut s, dir.path(), "norats.svg", json!({"ratsnest": false}));
    assert_ne!(all, no_rats);

    let f = r.execute(&mut s, "render.board", json!({"layers": ["F.Cuu"]}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "render.unknown_layer");
    let f = r.execute(&mut s, "render.board", json!({"highlight": ["GNDD"]}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "render.unknown_highlight");
}

#[test]
fn realistic_views_crop_and_markers() {
    let (dir, r, mut s) = setup();
    let top = svg(&r, &mut s, dir.path(), "top.svg", json!({"realistic": "top"}));
    assert!(top.contains("#1e5a2c"), "green mask");
    assert!(top.contains("#d9b35b"), "gold pads");
    let bottom = svg(&r, &mut s, dir.path(), "bottom.svg", json!({"realistic": "bottom"}));
    assert_ne!(top, bottom);
    assert!(bottom.contains("#d9b35b"), "R1's pads are exposed on the bottom");
    let o = exec(&r, &mut s, "render.board", json!({"path": "bottom.png", "realistic": "bottom"}));
    assert_eq!(o["output"]["format"], "png");

    // Board preferences drive the colors.
    exec(&r, &mut s, "board.setup", json!({"finish": ["HASL lead-free"], "mask_color": ["black"]}));
    let hasl = svg(&r, &mut s, dir.path(), "hasl.svg", json!({"realistic": "top"}));
    assert!(hasl.contains("#c6cbd1") && hasl.contains("#141414"));

    // Crop around a component, with a marker.
    let o = exec(
        &r,
        &mut s,
        "render.board",
        json!({"path": "u1.svg", "around": "U1", "margin": "1mm",
        "markers": [{"at": ["10mm", "7.5mm"], "label": "clearance"}]}),
    );
    let (w, h) = (o["output"]["width"].as_f64().unwrap(), o["output"]["height"].as_f64().unwrap());
    assert!(w < 10.0 && h < 10.0, "{w} x {h}");
    let u1 = std::fs::read_to_string(dir.path().join("p/u1.svg")).unwrap();
    assert!(u1.contains("#ff5a1f"), "marker");
    let f = r.execute(&mut s, "render.board", json!({"around": "C9"}), RunOptions::default()).unwrap_err();
    assert_eq!(f.error.diagnostic.code, "render.not_placed");
}
