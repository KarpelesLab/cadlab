//! Manufacturing outputs (M4): Gerber X2/X3, XNC drill, pick-and-place, IPC-D-356A.
//! Structural self-checks (always on), golden files (`CADLAB_BLESS=1`), determinism.

mod common;
mod fab_support;

use std::collections::BTreeMap;
use std::path::Path;

use cadlab::fabout::{self, Options, OutFile};
use fab_support::{check_gerber, check_ipc356, check_xnc, exec, tiny_board};
use serde_json::json;

fn options() -> Options {
    // A fixed version keeps the golden files stable across releases.
    Options { version: "test".into(), ..Options::default() }
}

fn by_name(files: &[OutFile]) -> BTreeMap<&str, &OutFile> {
    files.iter().map(|f| (f.name.as_str(), f)).collect()
}

#[test]
fn golden_files() {
    let (_d, _r, s) = tiny_board();
    let p = s.project.as_ref().unwrap();
    let mut files = fabout::all(p, &options());
    files.extend(fabout::drill_gerbers(p, &options()));
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/fab");
    for f in &files {
        // The CSV uses CRLF (RFC 4180); the repository stores LF (.gitattributes).
        common::golden::assert_golden(&golden.join(&f.name), &f.content.replace("\r\n", "\n"));
    }
    if std::env::var("CADLAB_BLESS").is_err() {
        let mut expected: Vec<String> = std::fs::read_dir(&golden)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        expected.sort();
        let mut names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        names.sort();
        assert_eq!(names, expected);
    }
}

#[test]
fn deterministic() {
    let (_d, _r, s) = tiny_board();
    let p = s.project.as_ref().unwrap();
    assert_eq!(fabout::all(p, &options()), fabout::all(p, &options()));
    let (_d2, _r2, s2) = tiny_board();
    assert_eq!(fabout::all(p, &options()), fabout::all(s2.project.as_ref().unwrap(), &options()));
}

#[test]
fn gerber_structure() {
    let (_d, _r, s) = tiny_board();
    let p = s.project.as_ref().unwrap();
    let mut files = fabout::gerbers(p, &options());
    files.extend(fabout::drill_gerbers(p, &options()));
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "tiny-F_Cu.gbr",
            "tiny-B_Cu.gbr",
            "tiny-F_Mask.gbr",
            "tiny-B_Mask.gbr",
            "tiny-F_Paste.gbr",
            "tiny-B_Paste.gbr",
            "tiny-F_SilkS.gbr",
            "tiny-B_SilkS.gbr",
            "tiny-Edge_Cuts.gbr",
            "tiny-F_Component.gbr",
            "tiny-B_Component.gbr",
            "tiny-PTH-drl.gbr",
        ]
    );
    let mut infos = BTreeMap::new();
    for f in &files {
        let info = check_gerber(&f.name, &f.content);
        assert_eq!(info.file_attrs[".FileFunction"], f.function, "{}", f.name);
        assert_eq!(info.file_attrs[".GenerationSoftware"], "cadlab,cadlab,test");
        assert!(info.file_attrs.contains_key(".SameCoordinates"));
        // Every aperture has a function.
        for (d, (tpl, func)) in &info.apertures {
            assert!(func.is_some(), "{}: D{d} {tpl} has no .AperFunction", f.name);
        }
        infos.insert(f.name.clone(), info);
    }
    let pads = cadlab::board::placed_pads(p);
    let count = |layer: &str| pads.iter().filter(|pp| pp.layers.iter().any(|l| l == layer)).count();

    // Copper: every pad and via flashed, with net/pin/component attributes; tracks drawn.
    let top = &infos["tiny-F_Cu.gbr"];
    assert_eq!(top.file_attrs[".FilePolarity"], "Positive");
    let flashes: usize = top.flashes.values().sum();
    assert_eq!(flashes, count("F.Cu") + 2, "pads + 2 vias");
    assert!(top.object_attrs.is_superset(&[".N", ".P", ".C"].map(String::from).into()));
    let u1_5 = top.flash_list.iter().find(|(_, _, a)| a.get(".P").is_some_and(|v| v == "U1,5")).unwrap();
    assert_eq!(u1_5.2[".N"], "3V3");
    assert_eq!(u1_5.2[".C"], "U1");
    let fun = |info: &fab_support::GerberInfo, d: u32| info.apertures[&d].1.clone().unwrap();
    assert!(top.flash_list.iter().any(|(_, d, _)| fun(top, *d) == "ViaPad"));
    assert!(top.flash_list.iter().any(|(_, d, _)| fun(top, *d) == "SMDPad,CuDef"));
    assert!(top.flash_list.iter().any(|(_, d, _)| fun(top, *d) == "ComponentPad"));
    assert!(top.draws >= 1);
    // C1 at 45° needs macro apertures.
    assert!(top.apertures.values().any(|(t, _)| t.starts_with("Shape")));
    let bottom = &infos["tiny-B_Cu.gbr"];
    assert_eq!(bottom.file_attrs[".FileFunction"], "Copper,L2,Bot");
    assert_eq!(bottom.flashes.values().sum::<usize>(), count("B.Cu") + 2);
    assert_eq!(bottom.arcs, 2, "the arc track, as two halves");

    // Masks are negative, openings for every pad on that side; vias tented.
    let fm = &infos["tiny-F_Mask.gbr"];
    assert_eq!(fm.file_attrs[".FilePolarity"], "Negative");
    let top_smd = pads.iter().filter(|pp| pp.layers == ["F.Cu"]).count();
    let tht = pads.iter().filter(|pp| pp.hole.is_some()).count();
    assert_eq!(fm.flashes.values().sum::<usize>(), top_smd + tht);
    assert!(fm.apertures.values().all(|(_, f)| f.as_deref() == Some("Material")));

    // Paste: SMD only (exposed pads would use windows).
    assert_eq!(infos["tiny-F_Paste.gbr"].flashes.values().sum::<usize>(), top_smd);
    assert_eq!(infos["tiny-B_Paste.gbr"].flashes.values().sum::<usize>(), 2, "C2 on the bottom");

    // Legend: footprint silk, refdes and the board text.
    let silk = &infos["tiny-F_SilkS.gbr"];
    assert!(silk.draws > 20, "strokes of text");
    assert_eq!(silk.regions, 1, "the line across U1 is clipped at the pad openings");

    // Profile: rounded rectangle, four corners as two arcs each.
    let prof = &infos["tiny-Edge_Cuts.gbr"];
    assert_eq!(prof.file_attrs[".FileFunction"], "Profile,NP");
    assert_eq!(prof.arcs, 8);
    assert_eq!(prof.apertures.values().next().unwrap().1.as_deref(), Some("Profile"));

    // X3: populated parts only (C1 is DNP), with component attributes and pins.
    let comp = &infos["tiny-F_Component.gbr"];
    let mains: Vec<&BTreeMap<String, String>> =
        comp.flash_list.iter().filter(|(_, d, _)| fun(comp, *d) == "ComponentMain").map(|(_, _, a)| a).collect();
    let refs: Vec<&str> = mains.iter().map(|a| a[".C"].as_str()).collect();
    assert_eq!(refs, ["J1", "U1"]);
    assert_eq!(mains[1][".CMPN"], "AP2112K-3.3TRG1");
    assert_eq!(mains[1][".CMnt"], "SMD");
    assert_eq!(mains[0][".CRot"], "90");
    let bcomp = &infos["tiny-B_Component.gbr"];
    assert_eq!(bcomp.file_attrs[".FileFunction"], "Component,L2,Bot");
    let c2 = bcomp.flash_list.iter().find(|(_, d, _)| fun(bcomp, *d) == "ComponentMain").unwrap();
    assert_eq!(c2.2[".CRot"], "270", "bottom: placement rotation + 180°");

    // X2 drill: vias and component holes, separate tools.
    let drl = &infos["tiny-PTH-drl.gbr"];
    assert_eq!(drl.file_attrs[".FileFunction"], "Plated,1,2,PTH,Drill");
    assert_eq!(drl.flashes.values().sum::<usize>(), 2 + tht);
}

#[test]
fn drill_and_netlists() {
    let (_d, _r, s) = tiny_board();
    let p = s.project.as_ref().unwrap();
    let files = fabout::all(p, &options());
    let f = by_name(&files);

    let (tools, hits, attrs) = check_xnc("PTH", &f["tiny-PTH.drl"].content);
    assert!(attrs.contains(&"TF.FileFunction,Plated,1,2,PTH,Drill".to_string()));
    assert!(attrs.contains(&"TA.AperFunction,ViaDrill".to_string()));
    assert!(attrs.contains(&"TA.AperFunction,ComponentDrill".to_string()));
    assert_eq!(tools["T01"], 0.3, "via drill from the rules");
    assert_eq!(hits.values().sum::<usize>(), 4, "2 vias + 2 header pins");
    assert!(!f.contains_key("tiny-NPTH.drl"), "no non-plated holes");

    // Pick and place: DNP C1 excluded, origin at the outline's lower-left.
    let pnp = &f["tiny-pos.csv"].content;
    assert!(pnp.ends_with("\r\n") && !pnp.replace("\r\n", "").contains('\n'), "CRLF line endings");
    let lines: Vec<&str> = pnp.lines().collect();
    assert_eq!(lines[0], "Designator,Value,Package,Footprint,X (mm),Y (mm),Rotation,Side");
    assert_eq!(lines.len(), 4);
    assert!(lines[1].starts_with("C2,1uF,0402,"), "{}", lines[1]);
    assert!(lines[1].ends_with(",18,9,90,bottom"), "{}", lines[1]);
    assert!(lines[2].starts_with("J1,"));
    assert!(lines[3].starts_with("U1,") && lines[3].ends_with(",12,7.5,0,top"), "{}", lines[3]);

    // IPC-D-356A: every pad and via.
    let recs = check_ipc356(&f["tiny.d356"].content);
    let pads = cadlab::board::placed_pads(p);
    assert_eq!(recs.len(), pads.len() + 2);
    let u1_5 = recs.iter().find(|r| r.2 == "U1" && r.3 == "5").unwrap();
    assert_eq!((u1_5.0.as_str(), u1_5.1.as_str(), u1_5.4.as_str()), ("327", "3V3", "A01"));
    let c2_1 = recs.iter().find(|r| r.2 == "C2" && r.3 == "1").unwrap();
    assert_eq!(c2_1.4, "A02", "bottom side access");
    let j1 = recs.iter().find(|r| r.2 == "J1" && r.3 == "2").unwrap();
    assert_eq!((j1.0.as_str(), j1.1.as_str(), j1.4.as_str()), ("317", "GND", "A00"));
    let nc = recs.iter().find(|r| r.2 == "U1" && r.3 == "4").unwrap();
    assert_eq!(nc.1, "N/C");
    assert_eq!(recs.iter().filter(|r| r.2 == "VIA").count(), 2);
}

#[test]
fn export_commands() {
    let (dir, r, mut s) = tiny_board();
    let o = exec(&r, &mut s, "export.all", json!({}));
    let files = o["output"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 11 + 1 + 2 + 1, "Gerber, drill, pick-and-place, IPC-D-356A, IPC-2581");
    let root = dir.path().join("p");
    for f in files {
        let path = Path::new(f["path"].as_str().unwrap());
        assert!(path.starts_with(root.join("out/fab")), "{}", path.display());
        assert!(path.is_file());
    }
    let o = exec(&r, &mut s, "export.drill", json!({"dir": "drill", "gerber": true}));
    let names: Vec<&str> =
        o["output"]["files"].as_array().unwrap().iter().map(|f| f["function"].as_str().unwrap()).collect();
    assert_eq!(names, ["Plated,1,2,PTH", "Plated,1,2,PTH,Drill"]);
    assert!(root.join("drill/tiny-PTH.drl").is_file());
    let o = exec(&r, &mut s, "export.pnp", json!({"path": "assembly/pos.csv"}));
    assert_eq!(o["output"]["files"][0]["function"], "PickPlace");
    assert!(root.join("assembly/pos.csv").is_file());
    let o = exec(&r, &mut s, "export.ipc356", json!({}));
    assert!(
        Path::new(o["output"]["files"][0]["path"].as_str().unwrap())
            .ends_with(Path::new("out").join("fab").join("tiny.d356"))
    );
    let o = exec(&r, &mut s, "export.gerber", json!({"dir": "g", "mask_expansion": "0.05mm"}));
    assert_eq!(o["output"]["files"].as_array().unwrap().len(), 11);
    let mask = std::fs::read_to_string(root.join("g/tiny-F_Mask.gbr")).unwrap();
    check_gerber("mask", &mask);
    // Grown by an exact offset: the square pad gets corners rounded by the expansion.
    assert!(mask.contains("Pad 1.8x1.8 r0.05") && mask.contains("%ADD12C,1.8*%"), "grown pad apertures:\n{mask}");

    // Unplaced parts are reported.
    exec(&r, &mut s, "circuit.add", json!({"part": "C 1uF 16V X5R 0402"}));
    let out = r.execute(&mut s, "export.pnp", json!({}), cadlab::command::RunOptions::default()).unwrap();
    let v = serde_json::to_value(&out).unwrap();
    assert!(v.to_string().contains("export.unplaced"), "{v}");
}
