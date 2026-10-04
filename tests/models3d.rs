//! 3D models on footprints (M9, DECISIONS D36): attaching STL / OBJ / glTF files decoded by the
//! oxideav-mesh3d crates, the 3D render using them instead of generated bodies, STEP (faceted
//! B-rep) and IDF exports, storage, undo, dry runs and shared libraries.
//!
//! Every model here is a small box written by the tests themselves (no third-party models).
//! `CADLAB_ORACLE_FREECAD` (with `CADLAB_ORACLES=1`) also checks that FreeCAD reads the model
//! body as a closed solid with the right volume.

#![cfg(all(feature = "models3d", feature = "png"))]

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use cadlab::command::{Failure, Registry, RunOptions, Session};
use cadlab::mcad;
use cadlab::model::Project;
use cadlab::model::board::BoardSide;
use cadlab::render::board3d::{Image3d, Options3d, render};
use cadlab::userlib::{Libraries, UserLibrary};
use common::oracle::{self, Oracle};
use serde_json::{Value, json};

fn exec(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Value {
    match r.execute(s, cmd, args, RunOptions::default()) {
        Ok(o) => serde_json::to_value(&o).unwrap(),
        Err(f) => panic!("{cmd} failed: {} ({:?})", f.error, f.error.diagnostic.hint),
    }
}

fn fail(r: &Registry, s: &mut Session, cmd: &str, args: Value) -> Failure {
    r.execute(s, cmd, args, RunOptions::default()).expect_err(cmd)
}

// ---------------------------------------------------------------------------------------------
// Fixtures: a box as STL (ASCII), OBJ and glTF (embedded buffer, Y up, meters).

/// Triangles of an axis-aligned box (Z up), wound counter-clockwise seen from outside.
fn box_tris(min: [f64; 3], max: [f64; 3]) -> Vec<[[f64; 3]; 3]> {
    let c = |i: usize, j: usize, k: usize| [[min[0], max[0]][i], [min[1], max[1]][j], [min[2], max[2]][k]];
    let quads = [
        [c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0)],
        [c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1)],
        [c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1)],
        [c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0)],
        [c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0)],
        [c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1)],
    ];
    quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect()
}

fn stl(tris: &[[[f64; 3]; 3]]) -> String {
    let mut s = String::from("solid fixture\n");
    for t in tris {
        s.push_str("  facet normal 0 0 0\n    outer loop\n");
        for v in t {
            s.push_str(&format!("      vertex {} {} {}\n", v[0], v[1], v[2]));
        }
        s.push_str("    endloop\n  endfacet\n");
    }
    s.push_str("endsolid fixture\n");
    s
}

/// OBJ in millimeters with Z up (attached with `unit: mm, up: z`).
fn obj(tris: &[[[f64; 3]; 3]]) -> String {
    let mut s = String::from("# fixture\no box\n");
    for t in tris {
        for v in t {
            s.push_str(&format!("v {} {} {}\n", v[0], v[1], v[2]));
        }
    }
    for i in 0..tris.len() {
        s.push_str(&format!("f {} {} {}\n", 3 * i + 1, 3 * i + 2, 3 * i + 3));
    }
    s
}

/// glTF 2.0 with one red box, in meters with Y up (board Z = glTF Y, board Y = -glTF Z).
fn gltf(tris: &[[[f64; 3]; 3]], rgba: [f64; 4]) -> String {
    let mut buf = Vec::new();
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in tris.iter().flatten() {
        let g = [(v[0] / 1000.0) as f32, (v[2] / 1000.0) as f32, (-v[1] / 1000.0) as f32];
        for i in 0..3 {
            lo[i] = lo[i].min(g[i]);
            hi[i] = hi[i].max(g[i]);
            buf.extend_from_slice(&g[i].to_le_bytes());
        }
    }
    let n = tris.len() * 3;
    let b64 = cadlab::model::model3d::base64_encode(&buf);
    json!({
        "asset": {"version": "2.0", "generator": "cadlab tests"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": rgba}}],
        "buffers": [{"byteLength": buf.len(), "uri": format!("data:application/octet-stream;base64,{b64}")}],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": buf.len(), "target": 34962}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": n, "type": "VEC3", "min": lo, "max": hi}]
    })
    .to_string()
}

// ---------------------------------------------------------------------------------------------
// Board: U1 (SOT-23-5) on top, R1 (0603) on the bottom.

fn ldo_board() -> (tempfile::TempDir, Registry, Session) {
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    s.libraries = Some(Libraries::new(vec![UserLibrary::new("user", dir.path().join("lib"))]));
    exec(&r, &mut s, "project.new", json!({"path": dir.path().join("p"), "name": "ldo"}));
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
    // Fixtures next to the project.
    let tall = box_tris([-1.5, -1.5, 0.0], [1.5, 1.5, 6.0]);
    let small = box_tris([-1.0, -0.5, 0.0], [1.0, 0.5, 3.0]);
    let models = dir.path().join("p").join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("tall.stl"), stl(&tall)).unwrap();
    std::fs::write(models.join("tall.obj"), obj(&tall)).unwrap();
    std::fs::write(models.join("red.gltf"), gltf(&small, [1.0, 0.0, 0.0, 1.0])).unwrap();
    std::fs::write(models.join("one.stl"), stl(&tall[..1])).unwrap();
    std::fs::write(models.join("junk.stl"), "this is not a model").unwrap();
    (dir, r, s)
}

fn project(s: &Session) -> &Project {
    s.project.as_ref().unwrap()
}

fn fp_of(s: &Session, refdes: &str) -> String {
    cadlab::board::footprint_for(project(s), refdes).unwrap().name.clone()
}

/// `CADLAB_RENDER3D_KEEP=<dir>` keeps the images for review.
fn keep(name: &str, img: &Image3d) {
    if let Some(dir) = std::env::var_os("CADLAB_RENDER3D_KEEP") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Path::new(&dir).join(name), img.to_png().unwrap()).unwrap();
    }
}

fn probe(img: &Image3d, p: [f64; 3]) -> [u8; 3] {
    let (x, y) = img.camera.project(p);
    img.pixel(x as u32, y as u32)
}

fn is_grey(c: [u8; 3]) -> bool {
    let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
    (r - g).abs() < 14 && (g - b).abs() < 14 && r > 60
}

fn is_red(c: [u8; 3]) -> bool {
    c[0] > 100 && (c[1] as i32) < 50 && (c[2] as i32) < 50
}

// ---------------------------------------------------------------------------------------------

#[test]
fn attach_formats_and_listing() {
    let (_d, r, mut s) = ldo_board();
    let u1 = fp_of(&s, "U1");
    let o = exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl"}));
    let out = &o["output"];
    assert_eq!(out["model"], json!({"file": "tall.stl"}));
    assert_eq!(out["facts"]["format"], "stl");
    assert_eq!(out["facts"]["triangles"], 12);
    assert_eq!(out["facts"]["min"], json!(["-1.5mm", "-1.5mm", "0mm"]));
    assert_eq!(out["facts"]["max"], json!(["1.5mm", "1.5mm", "6mm"]));
    // The model is far from SOT-23's body size? 3 mm vs 2.9 mm: no warning.
    assert!(!o.to_string().contains("model.size_mismatch"), "{o}");

    // OBJ (declared meters, Y up) with explicit unit and axis; glTF with its own (m, Y up).
    let o = exec(
        &r,
        &mut s,
        "footprint.model_set",
        json!({"footprint": u1, "file": "models/tall.obj", "unit": "mm", "up": "z", "offset": ["0mm", "0mm", "0.5mm"]}),
    );
    assert_eq!(o["output"]["facts"]["format"], "obj");
    assert_eq!(o["output"]["facts"]["max"], json!(["1.5mm", "1.5mm", "6.5mm"]));
    assert_eq!(o["output"]["pruned"], json!(["tall.stl"]), "the STL is no longer used");
    let o = exec(
        &r,
        &mut s,
        "footprint.model_set",
        json!({"footprint": u1, "file": "models/red.gltf", "rotation": [0, 0, 90]}),
    );
    assert_eq!(o["output"]["facts"]["format"], "gltf");
    // Rotated a quarter turn: 2 x 1 becomes 1 x 2; glTF meters became millimeters, Y up became Z.
    assert_eq!(o["output"]["facts"]["min"], json!(["-0.5mm", "-1mm", "0mm"]));
    assert_eq!(o["output"]["facts"]["max"], json!(["0.5mm", "1mm", "3mm"]));

    // A unit mistake is flagged: the OBJ read as meters is 1000 times too large.
    let o = exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.obj", "up": "z"}));
    assert!(o.to_string().contains("model.size_mismatch"), "{o}");

    // Part-specific model on R1's footprint, and the listing.
    let r1 = fp_of(&s, "R1");
    let part = project(&s).circuit().components["R1"].part.clone();
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": r1, "file": "models/red.gltf", "part": part}));
    let l = exec(&r, &mut s, "footprint.model_list", json!({}));
    let files: Vec<&str> =
        l["output"]["files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(files, ["red.gltf", "tall.obj"]);
    let uses = l["output"]["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[1]["part"], part);
    let fmts: Vec<&str> =
        l["output"]["formats"].as_array().unwrap().iter().map(|f| f["format"].as_str().unwrap()).collect();
    assert_eq!(fmts, ["gltf", "obj", "stl", "usdz"]);
    assert!(l["output"]["pending"].to_string().contains("step"));
    assert_eq!(cadlab::models3d::model_for(project(&s), "R1").unwrap().file, "red.gltf");
    assert!(cadlab::models3d::model_for(project(&s), "C1").is_none());

    // Reusing a library model by name; clearing.
    let c1 = fp_of(&s, "C1");
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": c1, "file": "red.gltf"}));
    let o = exec(&r, &mut s, "footprint.model_clear", json!({"footprint": c1}));
    assert!(o["output"]["pruned"].is_null(), "still used by R1's part");
    let f = fail(&r, &mut s, "footprint.model_clear", json!({"footprint": c1}));
    assert_eq!(f.error.diagnostic.code, "model.not_set");
}

#[test]
fn errors_and_unsupported_formats() {
    let (_d, r, mut s) = ldo_board();
    let u1 = fp_of(&s, "U1");
    // STEP and VRML: reported before any file access, with the formats available now.
    for file in ["models/U1.step", "models/U1.wrl"] {
        let f = fail(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": file}));
        let d = &f.error.diagnostic;
        assert_eq!(d.code, "model.unsupported_format", "{file}");
        let hint = d.hint.as_deref().unwrap();
        assert!(hint.contains(".stl") && hint.contains(".glb"), "{hint}");
        assert!(hint.contains("STEP and VRML arrive with the oxideav"), "{hint}");
    }
    let f = fail(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/nope.stl"}));
    assert_eq!(f.error.diagnostic.code, "model.file_not_found");
    // A broken file is rejected and leaves nothing behind.
    let f = fail(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/junk.stl"}));
    assert!(["model.invalid", "model.empty"].contains(&&*f.error.diagnostic.code), "{:?}", f.error.diagnostic);
    assert!(project(&s).library().models.is_empty());
    for (args, code) in [
        (json!({"footprint": u1, "file": "models/tall.stl", "scale": ["0"]}), "model.bad_scale"),
        (json!({"footprint": u1, "file": "models/tall.stl", "scale": [1, 2]}), "model.bad_scale"),
        (json!({"footprint": u1, "file": "models/tall.stl", "name": "bad name.stl"}), "model.invalid_name"),
        (json!({"footprint": "NOPE", "file": "models/tall.stl"}), "footprint.not_found"),
        (json!({"footprint": u1, "file": "models/tall.stl", "part": "R_10k_1pct_0603"}), "model.part_footprint"),
    ] {
        let f = fail(&r, &mut s, "footprint.model_set", args.clone());
        assert_eq!(f.error.diagnostic.code, code, "{args}");
    }
    // Same name, different content: a conflict unless replaced.
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl", "name": "m.stl"}));
    let f =
        fail(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/one.stl", "name": "m.stl"}));
    assert_eq!(f.error.diagnostic.code, "model.exists");
    exec(
        &r,
        &mut s,
        "footprint.model_set",
        json!({"footprint": u1, "file": "models/one.stl", "name": "m.stl", "replace": true}),
    );
}

#[test]
fn dry_run_undo_and_storage() {
    let (d, r, mut s) = ldo_board();
    let u1 = fp_of(&s, "U1");
    let before = project(&s).clone();
    let o = r
        .execute(
            &mut s,
            "footprint.model_set",
            json!({"footprint": u1, "file": "models/tall.stl"}),
            RunOptions { dry_run: true },
        )
        .unwrap();
    assert_eq!(o.output["facts"]["triangles"], 12, "the preview decodes the model");
    assert_eq!(project(&s), &before, "dry run leaves the project alone");

    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl", "scale": ["2.54"]}));
    s.save().unwrap();
    let root = d.path().join("p");
    let stored = std::fs::read(root.join("library/models/tall.stl")).unwrap();
    assert_eq!(stored, std::fs::read(root.join("models/tall.stl")).unwrap(), "stored as-is");
    let fp_json = std::fs::read_to_string(root.join(format!("library/footprints/{u1}.json"))).unwrap();
    assert!(fp_json.contains(r#""model": {"file": "tall.stl", "scale": ["2.54", "2.54", "2.54"]}"#), "{fp_json}");
    let loaded = Project::load(&root).unwrap();
    assert_eq!(&loaded, project(&s));
    // Packed form (undo snapshots, `project pack`) carries the bytes.
    assert_eq!(&Project::from_packed_str(&project(&s).to_packed_string()).unwrap(), project(&s));

    exec(&r, &mut s, "history.undo", json!({}));
    assert!(project(&s).library().models.is_empty());
    assert!(project(&s).library().footprints[&u1].model.is_none());
    s.save().unwrap();
    assert!(!root.join("library/models/tall.stl").exists(), "removed on save");
    exec(&r, &mut s, "history.redo", json!({}));
    assert_eq!(project(&s).library().models.len(), 1);
    // Removing the footprint's last user drops the file; regenerating keeps the model.
    exec(&r, &mut s, "footprint.generate", json!({"package": "SOT-23-5", "replace": true}));
    assert!(project(&s).library().footprints[&u1].model.is_some());
}

#[test]
fn render_uses_models_instead_of_bodies() {
    let (_d, r, mut s) = ldo_board();
    let (u1, r1) = (fp_of(&s, "U1"), fp_of(&s, "R1"));
    let o = Options3d { size: 600, ..Default::default() };
    let t = project(&s).board().stackup.thickness.0 as f64 / 1e6;
    let plain = render(project(&s), &o).unwrap();
    let plain_bottom = render(project(&s), &Options3d { side: BoardSide::Bottom, ..o.clone() }).unwrap();

    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl"}));
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": r1, "file": "models/red.gltf"}));
    let a = render(project(&s), &o).unwrap();
    let b = render(project(&s), &o).unwrap();
    assert_eq!(a.to_png().unwrap(), b.to_png().unwrap(), "same input, same bytes");
    assert!(a.model_errors.is_empty());
    keep("model_iso.png", &a);
    // U1 is now a 6 mm grey tower: a point 5.5 mm above it shows the model, not the board.
    let high = [10.0, 7.5, 5.5];
    assert!(is_grey(probe(&a, high)), "model: {:?}", probe(&a, high));
    assert_ne!(probe(&plain, high), probe(&a, high));
    // The generated SOT-23 body (dark, 1.45 mm) is gone: the top of the tower is grey too.
    assert!(is_grey(probe(&a, [10.0, 7.5, 6.0])));

    // R1's red box hangs 3 mm under the board, mirrored like other bottom parts.
    let bottom = render(project(&s), &Options3d { side: BoardSide::Bottom, ..o.clone() }).unwrap();
    keep("model_bottom.png", &bottom);
    let under = [10.0, 3.0, -t - 2.5];
    assert!(is_red(probe(&bottom, under)), "{:?}", probe(&bottom, under));
    assert!(!is_red(probe(&plain_bottom, under)));

    // A model that cannot be read falls back to the generated body, with a warning.
    s.project
        .as_mut()
        .unwrap()
        .library_mut()
        .models
        .insert("tall.stl".into(), cadlab::model::model3d::ModelData::new(b"garbage".to_vec()));
    let img = render(project(&s), &o).unwrap();
    assert_eq!(img.model_errors.len(), 1);
    assert_eq!(img.model_errors[0].0, "U1");
    let v = exec(&r, &mut s, "render.board3d", json!({"size": 300}));
    assert!(v.to_string().contains("using a generated body"), "{v}");
}

// ---------------------------------------------------------------------------------------------
// STEP and IDF.

fn parse_step(text: &str) -> BTreeMap<u64, String> {
    assert!(text.starts_with("ISO-10303-21;\nHEADER;\n") && text.ends_with("END-ISO-10303-21;\n"));
    let data = &text[text.find("DATA;\n").unwrap() + 6..text.rfind("ENDSEC;").unwrap()];
    let mut out = BTreeMap::new();
    for e in data.split(";\n").map(str::trim).filter(|e| !e.is_empty()) {
        let (id, body) = e.split_once('=').unwrap();
        out.insert(id.trim_start_matches('#').parse().unwrap(), body.to_string());
    }
    out
}

fn refs(body: &str) -> Vec<u64> {
    let mut v = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let j = (i + 1..b.len()).find(|&j| !b[j].is_ascii_digit()).unwrap_or(b.len());
            v.push(body[i + 1..j].parse().unwrap());
            i = j;
        } else {
            i += 1;
        }
    }
    v
}

fn kind(body: &str) -> &str {
    &body[..body.find('(').unwrap_or(body.len())]
}

fn mcad_options() -> mcad::Options {
    mcad::Options { version: "test".into(), ..mcad::Options::default() }
}

#[test]
fn step_writes_models_as_faceted_breps() {
    let (d, r, mut s) = ldo_board();
    let u1 = fp_of(&s, "U1");
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl"}));
    let p = project(&s);
    let out = mcad::step::export(p, &mcad_options()).unwrap();
    assert_eq!(out.content, mcad::step::export(p, &mcad_options()).unwrap().content, "deterministic");
    assert_eq!(out.model_bodies, 1);
    assert_eq!(out.bodies, 4);
    assert!(out.open_models.is_empty() && out.model_errors.is_empty());
    let ents = parse_step(&out.content);
    for (id, body) in &ents {
        for r in refs(body) {
            assert!(ents.contains_key(&r), "#{id} references undefined #{r}");
        }
    }
    let of = |k: &str| ents.iter().filter(|(_, b)| kind(b) == k).map(|(i, _)| *i).collect::<Vec<_>>();
    assert_eq!(of("FACETED_BREP").len(), 1);
    assert_eq!(of("FACETED_BREP_SHAPE_REPRESENTATION").len(), 1);
    assert_eq!(of("FACE_SURFACE").len(), 12);
    assert_eq!(of("MANIFOLD_SOLID_BREP").len(), 1 + 2, "board, and the 0402 and 0603 boxes");
    // The shell is closed: every poly-loop edge is used once in each direction.
    let brep = of("FACETED_BREP")[0];
    let shell = refs(&ents[&brep])[0];
    assert_eq!(kind(&ents[&shell]), "CLOSED_SHELL");
    let mut edges: BTreeMap<(u64, u64), u32> = BTreeMap::new();
    let mut pts = Vec::new();
    for face in refs(&ents[&shell]) {
        assert_eq!(kind(&ents[&face]), "FACE_SURFACE");
        let fr = refs(&ents[&face]);
        assert_eq!(kind(&ents[&fr[1]]), "PLANE");
        let lp = refs(&ents[&fr[0]])[0];
        assert_eq!(kind(&ents[&lp]), "POLY_LOOP");
        let v = refs(&ents[&lp]);
        assert_eq!(v.len(), 3);
        for k in 0..3 {
            *edges.entry((v[k], v[(k + 1) % 3])).or_default() += 1;
        }
        pts.extend(v);
    }
    for (&(a, b), &n) in &edges {
        assert_eq!((n, edges.get(&(b, a))), (1, Some(&1)), "edge #{a}-#{b}");
    }
    pts.sort();
    pts.dedup();
    assert_eq!(pts.len(), 8, "welded corners");
    let coords: Vec<String> = pts.iter().map(|i| ents[i].clone()).collect();
    assert!(coords.contains(&"CARTESIAN_POINT('',(-1.5,-1.5,0.))".to_string()), "{coords:?}");
    assert!(coords.contains(&"CARTESIAN_POINT('',(1.5,1.5,6.))".to_string()), "{coords:?}");
    // U1 is an instance of the model part.
    let nauo: Vec<&String> = of("NEXT_ASSEMBLY_USAGE_OCCURRENCE").iter().map(|i| &ents[i]).collect();
    assert!(nauo.iter().any(|b| b.starts_with("NEXT_ASSEMBLY_USAGE_OCCURRENCE('U1'")));
    assert!(ents.values().any(|b| b.starts_with("PRODUCT('") && b.contains("_tall.stl'")));

    // An open mesh is written as a surface model, with a note.
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/one.stl"}));
    let out = mcad::step::export(project(&s), &mcad_options()).unwrap();
    assert_eq!(out.open_models, ["one.stl"]);
    let ents = parse_step(&out.content);
    assert_eq!(ents.values().filter(|b| kind(b) == "SHELL_BASED_SURFACE_MODEL").count(), 1);
    assert_eq!(ents.values().filter(|b| kind(b) == "OPEN_SHELL").count(), 1);
    let v = exec(&r, &mut s, "export.step", json!({}));
    assert_eq!(v["output"]["model_bodies"], 1);
    assert!(v.to_string().contains("export.step_open_model"), "{v}");
    assert!(d.path().join("p/out/mcad/ldo.step").is_file());
}

#[test]
fn idf_uses_model_extent() {
    let (_d, r, mut s) = ldo_board();
    let r1 = fp_of(&s, "R1");
    // Off-center model: 2 x 1 mm, 3 mm high, moved 1 mm along X.
    exec(
        &r,
        &mut s,
        "footprint.model_set",
        json!({"footprint": r1, "file": "models/red.gltf", "offset": ["1mm", "0mm", "0mm"]}),
    );
    let out = mcad::idf::export(project(&s), &mcad_options()).unwrap();
    let lib = out.library;
    let entry = lib.split(".ELECTRICAL\n").find(|e| e.starts_with(&format!("\"{r1}\""))).unwrap();
    let lines: Vec<&str> = entry.lines().collect();
    assert!(lines[0].ends_with(" MM 3"), "{}", lines[0]);
    assert_eq!(&lines[1..6], ["0 0 -0.5 0", "0 2 -0.5 0", "0 2 0.5 0", "0 0 0.5 0", "0 0 -0.5 0"]);
}

#[test]
fn step_model_freecad_oracle() {
    let Some(freecad) = oracle::optional(Oracle::FreeCad) else { return };
    let (d, r, mut s) = ldo_board();
    let u1 = fp_of(&s, "U1");
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl"}));
    let out = mcad::step::export(project(&s), &mcad_options()).unwrap();
    let path = d.path().join("m.step");
    std::fs::write(&path, &out.content).unwrap();
    let script = d.path().join("check.py");
    std::fs::write(
        &script,
        format!(
            "import Part\ns = Part.read(r'{}')\nprint('VALID', s.isValid())\nfor so in s.Solids:\n    print('SOLID', so.isValid(), so.isClosed(), '%.6f' % so.Volume)\n",
            path.display()
        ),
    )
    .unwrap();
    let stdout = oracle::run(&freecad, &["-c", &format!("exec(open(r'{}').read())", script.display())]);
    assert!(stdout.contains("VALID True"), "{stdout}");
    let vols: Vec<f64> =
        stdout.lines().filter_map(|l| l.strip_prefix("SOLID True True ")).map(|v| v.trim().parse().unwrap()).collect();
    assert!(vols.iter().any(|v| (v - 54.0).abs() < 1e-6), "the 3 x 3 x 6 mm model solid: {stdout}");
}

// ---------------------------------------------------------------------------------------------
// Shared libraries carry model files with footprints and parts.

#[test]
fn library_publish_and_import_carry_models() {
    let (d, r, mut s) = ldo_board();
    let u1 = fp_of(&s, "U1");
    exec(&r, &mut s, "footprint.model_set", json!({"footprint": u1, "file": "models/tall.stl"}));
    let o = exec(&r, &mut s, "lib.publish", json!({"footprint": u1}));
    let items: Vec<String> = o["output"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| format!("{} {}", i["kind"].as_str().unwrap(), i["name"].as_str().unwrap()))
        .collect();
    assert_eq!(items, [format!("footprint {u1}"), "model tall.stl".to_string()]);
    assert!(d.path().join("lib/models/tall.stl").is_file());
    let l = exec(&r, &mut s, "lib.list", json!({"kind": "model"}));
    assert_eq!(l["output"]["items"][0]["name"], "tall.stl");

    // A fresh project imports the footprint with its model.
    let mut s2 = Session::new();
    s2.libraries = s.libraries.clone();
    exec(&r, &mut s2, "project.new", json!({"path": d.path().join("q"), "name": "q"}));
    let o = exec(&r, &mut s2, "lib.import", json!({"name": u1}));
    assert!(o.to_string().contains("tall.stl"), "{o}");
    let p2 = project(&s2);
    assert_eq!(p2.library().models["tall.stl"], project(&s).library().models["tall.stl"]);
    assert_eq!(p2.library().footprints[&u1].model, project(&s).library().footprints[&u1].model);

    // The library refuses to remove a model a footprint still uses.
    let f = fail(&r, &mut s, "lib.remove", json!({"name": "tall.stl", "kind": "model"}));
    assert_eq!(f.error.diagnostic.code, "lib.in_use");
}
