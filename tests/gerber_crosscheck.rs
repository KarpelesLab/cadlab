//! Cross-check: cadlab's Gerber and drill files against KiCad's for the same boards
//! (docs/TESTING.md, "Cross-checks").
//!
//! For each board of `crosscheck::cases()`, cadlab's Gerbers (`fabout`) and KiCad's
//! (`kicad-cli pcb export gerbers/drill` on the `.kicad_pcb` export, zones refilled by KiCad,
//! coordinates relative to the auxiliary origin, which the export puts at cadlab's origin) are
//! rendered by gerbv over the same window and compared pixel by pixel, layer by layer. Drill
//! files are compared hit by hit. Runs only with `CADLAB_ORACLES=1`; KiCad and gerbv are
//! external processes (DECISIONS D7). Files and renders are kept in
//! `$CARGO_TARGET_TMPDIR/gerber_crosscheck/<case>/`.
#![cfg(feature = "png")]

mod common;
mod crosscheck;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cadlab::fabout::{self, Options};
use cadlab::model::Project;
use cadlab::model::board::BoardSide;
use common::oracle::{self, Oracle};

/// Render resolution: 1000 dpi, 25.4 µm per pixel.
const DPI: f64 = 1000.0;
/// Margin around the board outline's bounding box (mm).
const MARGIN_MM: f64 = 1.0;

struct Image {
    w: usize,
    h: usize,
    lit: Vec<bool>,
}

impl Image {
    fn load(path: &Path) -> Image {
        let pm = tiny_skia::Pixmap::load_png(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let lit = pm.data().chunks(4).map(|p| p[0].max(p[1]).max(p[2]) > 127).collect();
        Image { w: pm.width() as usize, h: pm.height() as usize, lit }
    }

    fn at(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.lit[y as usize * self.w + x as usize]
    }

    /// Whether every pixel within `r` of (x, y) has the value `v`.
    fn uniform(&self, x: i64, y: i64, r: i64, v: bool) -> bool {
        (-r..=r).all(|dy| (-r..=r).all(|dx| self.at(x + dx, y + dy) == v))
    }
}

/// Pixel comparison of two renders of the same window.
#[derive(Debug, Default)]
struct Diff {
    /// Lit pixels in each image.
    lit: (usize, usize),
    /// Pixels lit in exactly one image.
    xor: usize,
    /// XOR pixels that are not within `r` pixels of an edge in the other image (real
    /// differences, not rasterization or arc-approximation jitter).
    core: usize,
    /// Bounding box (px) of the core differences.
    bbox: Option<(usize, usize, usize, usize)>,
}

fn compare(a: &Image, b: &Image, r: i64, skip: &dyn Fn(usize, usize) -> bool) -> Diff {
    assert_eq!((a.w, a.h), (b.w, b.h), "renders of different sizes");
    let mut d = Diff::default();
    for y in 0..a.h {
        for x in 0..a.w {
            if skip(x, y) {
                continue;
            }
            let (va, vb) = (a.lit[y * a.w + x], b.lit[y * a.w + x]);
            d.lit.0 += va as usize;
            d.lit.1 += vb as usize;
            if va == vb {
                continue;
            }
            d.xor += 1;
            let (xi, yi) = (x as i64, y as i64);
            // A pixel lit only in `a` is a real difference if `a` is solid around it and `b`
            // is empty around it (and vice versa).
            if a.uniform(xi, yi, r, va) && b.uniform(xi, yi, r, vb) {
                d.core += 1;
                d.bbox = Some(match d.bbox {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    d
}

/// Render window: lower-left corner and size in mm.
struct Window {
    x0: f64,
    y0: f64,
    w: f64,
    h: f64,
}

impl Window {
    fn of(p: &Project) -> Window {
        let c = &p.board().outline.contours[0];
        let ring = cadlab::board::contour_ring(c, cadlab::board::COPPER_TOL);
        let xs = ring.iter().map(|q| q.x);
        let ys = ring.iter().map(|q| q.y);
        let (x0, x1) = (xs.clone().min().unwrap() as f64 / 1e6, xs.max().unwrap() as f64 / 1e6);
        let (y0, y1) = (ys.clone().min().unwrap() as f64 / 1e6, ys.max().unwrap() as f64 / 1e6);
        Window { x0: x0 - MARGIN_MM, y0: y0 - MARGIN_MM, w: x1 - x0 + 2.0 * MARGIN_MM, h: y1 - y0 + 2.0 * MARGIN_MM }
    }

    /// Pixel (column, row) of a board point in mm.
    fn px(&self, x: f64, y: f64) -> (f64, f64) {
        let s = DPI / 25.4;
        ((x - self.x0) * s, (self.y0 + self.h - y) * s)
    }
}

fn render(gerbv: &Path, win: &Window, file: &Path, out: &Path) -> Image {
    let inch = |v: f64| v / 25.4;
    let o = Command::new(gerbv)
        .arg("--export=png")
        .arg(format!("--dpi={DPI}"))
        .arg(format!("--origin={:.6}x{:.6}", inch(win.x0), inch(win.y0)))
        .arg(format!("--window_inch={:.6}x{:.6}", inch(win.w), inch(win.h)))
        .args(["--border=0", "--background=#000000", "--foreground=#FFFFFFFF"])
        .arg(format!("--output={}", out.display()))
        .arg(file)
        .output()
        .expect("running gerbv");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "gerbv failed on {}: {stderr}", file.display());
    // gerbv guesses that a Gerber file without any aperture is RS-274D; both tools write empty
    // layers that way (header, no objects, M02).
    let empty = !std::fs::read_to_string(file).unwrap().contains("%ADD");
    let noise = |l: &str| oracle::gerbv_noise(l) || (empty && l.contains("Most likely found a RS-274D file"));
    assert!(stderr.lines().all(noise), "gerbv diagnostics for {}:\n{stderr}", file.display());
    Image::load(out)
}

/// Writes cadlab's Gerbers and drill files into `dir/cadlab`.
fn cadlab_outputs(p: &Project, dir: &Path) -> PathBuf {
    let out = dir.join("cadlab");
    std::fs::create_dir_all(&out).unwrap();
    let o = Options::default();
    let mut files = fabout::gerbers(p, &o);
    files.extend(fabout::excellon::drills(p, &o));
    for f in &files {
        std::fs::write(out.join(&f.name), &f.content).unwrap();
    }
    out
}

/// Exports the board for KiCad and writes KiCad's Gerbers and drill files into `dir/kicad`.
fn kicad_outputs(cli: &Path, p: &Project, dir: &Path) -> PathBuf {
    let pcb = crosscheck::export_kicad(p, &dir.join("kicad_pcb"));
    let out = dir.join("kicad");
    std::fs::create_dir_all(&out).unwrap();
    let o = format!("{}/", out.display());
    oracle::run(
        cli,
        &[
            "pcb",
            "export",
            "gerbers",
            "--layers",
            "F.Cu,B.Cu,F.Mask,B.Mask,F.Paste,B.Paste,F.SilkS,B.SilkS,Edge.Cuts",
            "--use-drill-file-origin",
            // KiCad refills the zones with its own algorithm before plotting.
            "--check-zones",
            "-o",
            &o,
            pcb.to_str().unwrap(),
        ],
    );
    oracle::run(
        cli,
        &[
            "pcb",
            "export",
            "drill",
            "--drill-origin",
            "plot",
            "--excellon-separate-th",
            "-o",
            &o,
            pcb.to_str().unwrap(),
        ],
    );
    out
}

fn find(dir: &Path, suffix: &str) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(suffix))
        .unwrap_or_else(|| panic!("no *{suffix} in {}", dir.display()))
}

/// How a layer is compared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Copper, mask, paste: same image.
    Exact,
    /// Board profile: cadlab draws it with a 0.1 mm aperture, the export with 0.05 mm Edge.Cuts
    /// lines; the 1 px edge tolerance absorbs the 25 µm per side, so the center lines must agree.
    Profile,
    /// Silkscreen: compared outside mask openings (cadlab clips the legend there; KiCad does
    /// not without `--subtract-soldermask`, whose clear-polarity output gerbv's PNG export does
    /// not render faithfully) and outside reference designators (different stroke fonts),
    /// which must only be inked in both.
    Silk,
}

/// Real differences allowed per layer (mm²): about 15 pixels at 1000 dpi.
const CORE_TOL_MM2: f64 = 0.01;
/// Allowed difference of the total image area: 0.5 % plus [`CORE_TOL_MM2`] (catches uniform
/// growth or shrinkage smaller than a pixel, e.g. a mask expansion mismatch).
const AREA_TOL: f64 = 0.005;

/// (layer, cadlab file suffix, KiCad file suffix, comparison).
const COMPARED: &[(&str, &str, &str, Mode)] = &[
    ("F.Cu", "-F_Cu.gbr", "-F_Cu.gtl", Mode::Exact),
    ("B.Cu", "-B_Cu.gbr", "-B_Cu.gbl", Mode::Exact),
    ("F.Mask", "-F_Mask.gbr", "-F_Mask.gts", Mode::Exact),
    ("B.Mask", "-B_Mask.gbr", "-B_Mask.gbs", Mode::Exact),
    ("F.Paste", "-F_Paste.gbr", "-F_Paste.gtp", Mode::Exact),
    ("B.Paste", "-B_Paste.gbr", "-B_Paste.gbp", Mode::Exact),
    ("F.SilkS", "-F_SilkS.gbr", "-F_Silkscreen.gto", Mode::Silk),
    ("B.SilkS", "-B_SilkS.gbr", "-B_Silkscreen.gbo", Mode::Silk),
    ("Edge.Cuts", "-Edge_Cuts.gbr", "-Edge_Cuts.gm1", Mode::Profile),
];

/// Reference designator boxes on a side, in pixels (x0, y0, x1, y1).
fn refdes_boxes(p: &Project, win: &Window, side: BoardSide) -> Vec<(f64, f64, f64, f64)> {
    let mut out = Vec::new();
    for (r, pf) in &p.board().footprints {
        if pf.side != side {
            continue;
        }
        let Some(t) = fabout::refdes_text(p, r) else { continue };
        let (cx, cy) = (t.at.x.0 as f64 / 1e6, t.at.y.0 as f64 / 1e6);
        let size = t.size.0 as f64 / 1e6;
        let (hw, hh) = (r.len() as f64 * size * 0.6 + size * 0.3, size * 0.9);
        let (x0, y0) = win.px(cx - hw, cy + hh);
        let (x1, y1) = win.px(cx + hw, cy - hh);
        out.push((x0, y0, x1, y1));
    }
    out
}

fn inside(b: &(f64, f64, f64, f64), x: usize, y: usize) -> bool {
    let (x, y) = (x as f64, y as f64);
    x >= b.0 && x <= b.2 && y >= b.1 && y <= b.3
}

/// A drill hit: plated, hole function, diameter (µm), position (µm).
type Hit = (bool, String, i64, i64, i64);

/// Reads the hits of an Excellon/XNC file written in decimal millimeters (both tools' format).
fn drill_hits(path: &Path) -> Vec<Hit> {
    let text = std::fs::read_to_string(path).unwrap();
    let plated = !text.contains("TF.FileFunction,NonPlated");
    let um =
        |s: &str| (s.parse::<f64>().unwrap_or_else(|_| panic!("{s} in {}", path.display())) * 1000.0).round() as i64;
    let mut tools: BTreeMap<u32, (String, i64)> = BTreeMap::new();
    let mut function = String::new();
    let mut current = None;
    let mut header = true;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(f) = line.strip_prefix("; #@! TA.AperFunction,") {
            function = f.rsplit(',').next().unwrap().to_string();
        } else if line == "%" {
            header = false;
        } else if let Some(t) = line.strip_prefix('T') {
            let (num, dia) = match t.split_once('C') {
                Some((n, d)) => (n, Some(d)),
                None => (t, None),
            };
            let n: u32 = num.parse().unwrap();
            if header {
                tools.insert(n, (function.clone(), um(dia.unwrap())));
            } else {
                current = Some(n);
            }
        } else if let Some(xy) = line.strip_prefix('X') {
            let (x, y) = xy.split_once('Y').unwrap();
            let (f, d) = tools[&current.expect("hit before tool selection")].clone();
            out.push((plated, f, d, um(x), um(y)));
        }
    }
    out
}

fn all_hits(dir: &Path) -> Vec<Hit> {
    let mut hits: Vec<Hit> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "drl"))
        .flat_map(|p| drill_hits(&p))
        .collect();
    hits.sort();
    hits
}

/// Drill coordinates may differ by this much (µm): KiCad writes millimeters with three decimals,
/// cadlab with four.
const HIT_TOL_UM: i64 = 1;

/// Whether two hit lists match one to one: same plating, function and diameter, positions
/// within [`HIT_TOL_UM`].
fn same_hits(a: &[Hit], b: &[Hit]) -> bool {
    let mut used = vec![false; b.len()];
    a.len() == b.len()
        && a.iter().all(|h| {
            let hit = b.iter().enumerate().position(|(i, k)| {
                !used[i]
                    && (h.0, &h.1, h.2) == (k.0, &k.1, k.2)
                    && (h.3 - k.3).abs() <= HIT_TOL_UM
                    && (h.4 - k.4).abs() <= HIT_TOL_UM
            });
            hit.map(|i| used[i] = true).is_some()
        })
}

#[test]
fn kicad_and_cadlab_gerbers_agree() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let Some(gerbv) = oracle::require(Oracle::Gerbv) else { return };
    let px_mm2 = (25.4 / DPI) * (25.4 / DPI);
    let mut problems = Vec::new();
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s);
        let dir = crosscheck::keep_dir("gerber_crosscheck", case.name);
        let ours = cadlab_outputs(p, &dir);
        let theirs = kicad_outputs(&cli, p, &dir);
        let win = Window::of(p);
        let mut log = vec![format!("=== {}", case.name)];
        let mut masks: BTreeMap<&str, Image> = BTreeMap::new();
        for (layer, a, b, mode) in COMPARED {
            let ia = render(&gerbv, &win, &find(&ours, a), &dir.join(format!("{layer}.cadlab.png")));
            let ib = render(&gerbv, &win, &find(&theirs, b), &dir.join(format!("{layer}.kicad.png")));
            let side = if layer.starts_with("B.") { BoardSide::Bottom } else { BoardSide::Top };
            let boxes = if *mode == Mode::Silk { refdes_boxes(p, &win, side) } else { Vec::new() };
            let mask = masks.get(if side == BoardSide::Top { "F.Mask" } else { "B.Mask" });
            let skip = |x: usize, y: usize| match mode {
                Mode::Silk => {
                    boxes.iter().any(|b| inside(b, x, y))
                        || mask.is_some_and(|m| !m.uniform(x as i64, y as i64, 2, false))
                }
                _ => false,
            };
            let d = compare(&ia, &ib, 1, &skip);
            let mm2 = |n: usize| n as f64 * px_mm2;
            log.push(format!(
                "  {layer:9} area {:8.3} / {:8.3} mm²  xor {:6.3} mm²  differences {:6.3} mm²",
                mm2(d.lit.0),
                mm2(d.lit.1),
                mm2(d.xor),
                mm2(d.core)
            ));
            if mm2(d.core) > CORE_TOL_MM2 {
                let (x0, y0, x1, y1) = d.bbox.unwrap();
                let mm = |v: usize| v as f64 * 25.4 / DPI;
                problems.push(format!(
                    "{}: {layer}: {:.3} mm² differ, within x {:.2}..{:.2} mm, y {:.2}..{:.2} mm (renders in {})",
                    case.name,
                    mm2(d.core),
                    win.x0 + mm(x0),
                    win.x0 + mm(x1),
                    win.y0 + win.h - mm(y1),
                    win.y0 + win.h - mm(y0),
                    dir.display()
                ));
            }
            if *mode == Mode::Exact && (mm2(d.lit.0) - mm2(d.lit.1)).abs() > AREA_TOL * mm2(d.lit.1) + CORE_TOL_MM2 {
                problems.push(format!(
                    "{}: {layer}: area {:.3} mm² (cadlab) vs {:.3} mm² (KiCad)",
                    case.name,
                    mm2(d.lit.0),
                    mm2(d.lit.1)
                ));
            }
            // Reference designators: inked by both, wherever they are not clipped.
            for b in &boxes {
                let ink = |img: &Image| {
                    (b.1.max(0.0) as usize..(b.3 as usize).min(img.h))
                        .any(|y| (b.0.max(0.0) as usize..(b.2 as usize).min(img.w)).any(|x| img.lit[y * img.w + x]))
                };
                if ink(&ia) != ink(&ib) {
                    problems
                        .push(format!("{}: {layer}: a reference designator is printed by only one tool", case.name));
                }
            }
            if layer.ends_with(".Mask") {
                masks.insert(layer, ia);
            }
        }
        // Drill files: same hits, diameters, plating and functions.
        let (ha, hb) = (all_hits(&ours), all_hits(&theirs));
        log.push(format!("  drill     {} / {} hits", ha.len(), hb.len()));
        if !same_hits(&ha, &hb) {
            problems.push(format!("{}: drill hits differ:\n  cadlab {ha:?}\n  KiCad  {hb:?}", case.name));
        }
        eprintln!("{}", log.join("\n"));
    }
    assert!(problems.is_empty(), "Gerber cross-check differences:\n{}", problems.join("\n"));
}

#[test]
fn drill_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.drl");
    std::fs::write(
        &f,
        "M48\n; #@! TF.FileFunction,Plated,1,2,PTH\nMETRIC\n; #@! TA.AperFunction,Plated,PTH,ViaDrill\nT1C0.300\n%\nG05\nT1\nX3.5Y10.0\nM30\n",
    )
    .unwrap();
    assert_eq!(drill_hits(&f), vec![(true, "ViaDrill".to_string(), 300, 3500, 10000)]);
}
