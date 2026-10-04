//! Raster comparison of Gerber files and hit-by-hit comparison of drill files, shared by the
//! cross-check boards (`tests/gerber_crosscheck.rs`) and the open-source corpus
//! (`tests/corpus.rs`); docs/TESTING.md, "Cross-checks".
//!
//! gerbv renders each layer of both file sets at [`DPI`] over the same window (the outline's
//! bounding box plus [`MARGIN_MM`]) and the images are XORed. A differing pixel counts only when
//! both images are uniform within 1 px around it (rasterization and arc-approximation jitter is
//! not a difference). gerbv and KiCad are external processes (DECISIONS D7).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cadlab::fabout::{self, Options};
use cadlab::model::Project;
use cadlab::model::board::BoardSide;

use crate::common::oracle;

/// Render resolution: 1000 dpi, 25.4 µm per pixel.
pub const DPI: f64 = 1000.0;
/// Margin around the board outline's bounding box (mm).
pub const MARGIN_MM: f64 = 1.0;
/// Area of one pixel (mm²).
pub const PX_MM2: f64 = (25.4 / DPI) * (25.4 / DPI);

pub struct Image {
    pub w: usize,
    pub h: usize,
    pub lit: Vec<bool>,
}

impl Image {
    pub fn load(path: &Path) -> Image {
        let pm = tiny_skia::Pixmap::load_png(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let lit = pm.data().chunks(4).map(|p| p[0].max(p[1]).max(p[2]) > 127).collect();
        Image { w: pm.width() as usize, h: pm.height() as usize, lit }
    }

    fn at(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.lit[y as usize * self.w + x as usize]
    }

    /// Whether every pixel within `r` of (x, y) has the value `v`.
    pub fn uniform(&self, x: i64, y: i64, r: i64, v: bool) -> bool {
        (-r..=r).all(|dy| (-r..=r).all(|dx| self.at(x + dx, y + dy) == v))
    }
}

/// Pixel comparison of two renders of the same window.
#[derive(Debug, Default)]
pub struct Diff {
    /// Lit pixels in each image.
    pub lit: (usize, usize),
    /// Pixels lit in exactly one image.
    pub xor: usize,
    /// XOR pixels that are not within `r` pixels of an edge in the other image (real
    /// differences, not rasterization or arc-approximation jitter).
    pub core: usize,
    /// Bounding box (px) of the core differences.
    pub bbox: Option<(usize, usize, usize, usize)>,
}

pub fn compare(a: &Image, b: &Image, r: i64, skip: &dyn Fn(usize, usize) -> bool) -> Diff {
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
pub struct Window {
    pub x0: f64,
    pub y0: f64,
    pub w: f64,
    pub h: f64,
}

impl Window {
    /// The bounding box of the board's outline (every contour) plus [`MARGIN_MM`].
    pub fn of(p: &Project) -> Window {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for c in &p.board().outline.contours {
            for q in cadlab::board::contour_ring(c, cadlab::board::COPPER_TOL) {
                let (x, y) = (q.x as f64 / 1e6, q.y as f64 / 1e6);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
        Window { x0: x0 - MARGIN_MM, y0: y0 - MARGIN_MM, w: x1 - x0 + 2.0 * MARGIN_MM, h: y1 - y0 + 2.0 * MARGIN_MM }
    }

    /// Pixel (column, row) of a board point in mm.
    pub fn px(&self, x: f64, y: f64) -> (f64, f64) {
        let s = DPI / 25.4;
        ((x - self.x0) * s, (self.y0 + self.h - y) * s)
    }

    /// Board point (mm) of a pixel.
    pub fn mm(&self, col: usize, row: usize) -> (f64, f64) {
        let s = 25.4 / DPI;
        (self.x0 + col as f64 * s, self.y0 + self.h - row as f64 * s)
    }
}

pub fn render(gerbv: &Path, win: &Window, file: &Path, out: &Path) -> Image {
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
    // gerbv guesses that a Gerber file without any aperture is RS-274D (and misses apertures);
    // both tools write empty layers that way (header, no objects, M02), and KiCad writes
    // region-only layers (merged mask openings) without apertures too.
    let empty = !std::fs::read_to_string(file).unwrap().contains("%ADD");
    let noise = |l: &str| {
        oracle::gerbv_noise(l)
            || (empty
                && (l.contains("Most likely found a RS-274D file") || l.contains("Missing apertures/drill sizes")))
    };
    assert!(stderr.lines().all(noise), "gerbv diagnostics for {}:\n{stderr}", file.display());
    Image::load(out)
}

/// Writes cadlab's Gerbers and drill files into `dir/cadlab`.
pub fn cadlab_outputs(p: &Project, dir: &Path) -> PathBuf {
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

/// Runs KiCad's Gerber export of `layers` (comma-separated) and its drill export on `pcb` into
/// `out`, both relative to the auxiliary (drill/place file) origin. `extra` is added to the
/// Gerber export's arguments (`--check-zones` makes KiCad refill the zones with its own algorithm first).
pub fn kicad_outputs(cli: &Path, pcb: &Path, out: &Path, layers: &str, extra: &[&str]) {
    std::fs::create_dir_all(out).unwrap();
    let o = format!("{}/", out.display());
    let mut args = vec!["pcb", "export", "gerbers", "--layers", layers, "--use-drill-file-origin"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["-o", &o, pcb.to_str().unwrap()]);
    oracle::run(cli, &args);
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
}

pub fn find(dir: &Path, suffix: &str) -> PathBuf {
    try_find(dir, suffix).unwrap_or_else(|| panic!("no *{suffix} in {}", dir.display()))
}

pub fn try_find(dir: &Path, suffix: &str) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(suffix))
        .collect();
    v.sort();
    v.into_iter().next()
}

/// How a layer is compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
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
pub const CORE_TOL_MM2: f64 = 0.01;
/// Allowed difference of the total image area: 0.5 % plus [`CORE_TOL_MM2`] (catches uniform
/// growth or shrinkage smaller than a pixel, e.g. a mask expansion mismatch).
pub const AREA_TOL: f64 = 0.005;

/// Reference designator boxes on a side, in pixels (x0, y0, x1, y1).
pub fn refdes_boxes(p: &Project, win: &Window, side: BoardSide) -> Vec<(f64, f64, f64, f64)> {
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

/// A layer to compare: cadlab's file, KiCad's file, the comparison mode.
pub struct LayerSpec {
    pub layer: String,
    pub ours: PathBuf,
    pub theirs: PathBuf,
    pub mode: Mode,
}

/// The comparison of one layer.
#[derive(Debug)]
pub struct LayerResult {
    pub layer: String,
    pub mode: Mode,
    /// Lit area (mm²): cadlab, KiCad.
    pub area: (f64, f64),
    /// Area lit in exactly one render (mm²).
    pub xor: f64,
    /// Real differences (mm²).
    pub core: f64,
    /// Where the real differences are (mm): x0, x1, y0, y1.
    pub bbox_mm: Option<(f64, f64, f64, f64)>,
    /// Reference designators inked by only one tool (silkscreen).
    pub refdes_mismatches: usize,
}

impl LayerResult {
    /// The cross-check thresholds: at most [`CORE_TOL_MM2`] of real differences and, for
    /// [`Mode::Exact`], total areas within [`AREA_TOL`].
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.core > CORE_TOL_MM2 {
            let (x0, x1, y0, y1) = self.bbox_mm.unwrap();
            v.push(format!(
                "{}: {:.3} mm² differ, within x {x0:.2}..{x1:.2} mm, y {y0:.2}..{y1:.2} mm",
                self.layer, self.core
            ));
        }
        if self.mode == Mode::Exact && !self.areas_agree() {
            v.push(format!("{}: area {:.3} mm² (cadlab) vs {:.3} mm² (KiCad)", self.layer, self.area.0, self.area.1));
        }
        if self.refdes_mismatches > 0 {
            v.push(format!(
                "{}: {} reference designators printed by only one tool",
                self.layer, self.refdes_mismatches
            ));
        }
        v
    }

    pub fn areas_agree(&self) -> bool {
        (self.area.0 - self.area.1).abs() <= AREA_TOL * self.area.1 + CORE_TOL_MM2
    }
}

/// Renders and compares each layer (mask layers before the silkscreen of their side, whose
/// comparison skips mask openings). Renders are kept in `dir`.
pub fn compare_layers(
    gerbv: &Path,
    p: &Project,
    win: &Window,
    dir: &Path,
    specs: &[LayerSpec],
    check_refdes: bool,
) -> Vec<LayerResult> {
    let mut masks: BTreeMap<BoardSide, Image> = BTreeMap::new();
    let mut out = Vec::new();
    for s in specs {
        let ia = render(gerbv, win, &s.ours, &dir.join(format!("{}.cadlab.png", s.layer)));
        let ib = render(gerbv, win, &s.theirs, &dir.join(format!("{}.kicad.png", s.layer)));
        let side = if s.layer.starts_with("B.") { BoardSide::Bottom } else { BoardSide::Top };
        let boxes = if s.mode == Mode::Silk { refdes_boxes(p, win, side) } else { Vec::new() };
        let mask = masks.get(&side);
        let skip = |x: usize, y: usize| match s.mode {
            Mode::Silk => {
                boxes.iter().any(|b| inside(b, x, y)) || mask.is_some_and(|m| !m.uniform(x as i64, y as i64, 2, false))
            }
            _ => false,
        };
        let d = compare(&ia, &ib, 1, &skip);
        let mm2 = |n: usize| n as f64 * PX_MM2;
        let bbox_mm = d.bbox.map(|(x0, y0, x1, y1)| {
            let (a, b) = (win.mm(x0, y1), win.mm(x1, y0));
            (a.0, b.0, a.1, b.1)
        });
        // Reference designators: inked by both, wherever they are not clipped.
        let mut refdes_mismatches = 0;
        if check_refdes {
            for b in &boxes {
                let ink = |img: &Image| {
                    (b.1.max(0.0) as usize..(b.3 as usize).min(img.h))
                        .any(|y| (b.0.max(0.0) as usize..(b.2 as usize).min(img.w)).any(|x| img.lit[y * img.w + x]))
                };
                if ink(&ia) != ink(&ib) {
                    refdes_mismatches += 1;
                }
            }
        }
        eprintln!(
            "  {:9} area {:9.3} / {:9.3} mm²  xor {:7.3} mm²  differences {:7.3} mm²",
            s.layer,
            mm2(d.lit.0),
            mm2(d.lit.1),
            mm2(d.xor),
            mm2(d.core)
        );
        out.push(LayerResult {
            layer: s.layer.clone(),
            mode: s.mode,
            area: (mm2(d.lit.0), mm2(d.lit.1)),
            xor: mm2(d.xor),
            core: mm2(d.core),
            bbox_mm,
            refdes_mismatches,
        });
        if s.layer.ends_with(".Mask") {
            masks.insert(side, ia);
        }
    }
    out
}

/// A drill hit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hit {
    pub plated: bool,
    /// Gerber X2 hole function (`ViaDrill`, `ComponentDrill`, `MechanicalDrill`).
    pub function: String,
    /// Diameter (µm).
    pub dia: i64,
    /// Position (µm); the middle of a routed slot.
    pub x: i64,
    pub y: i64,
    /// A routed slot (`G85`), not a round hole.
    pub slot: bool,
}

/// Reads the hits of an Excellon/XNC file written in decimal millimeters (both tools' format).
/// A slot (`X..Y..G85X..Y..`, or routed: `G00X..Y..`, `M15`, `G01X..Y..`, `M16`) is one hit at
/// its middle, marked as a slot.
pub fn drill_hits(path: &Path) -> Vec<Hit> {
    let text = std::fs::read_to_string(path).unwrap();
    let plated = !text.contains("TF.FileFunction,NonPlated");
    let um =
        |s: &str| (s.parse::<f64>().unwrap_or_else(|_| panic!("{s} in {}", path.display())) * 1000.0).round() as i64;
    let xy = |s: &str| -> Option<(i64, i64)> {
        let (x, y) = s.split_once('Y')?;
        Some((um(x), um(y)))
    };
    let mut tools: BTreeMap<u32, (String, i64)> = BTreeMap::new();
    let mut function = String::new();
    let mut current = None;
    let mut header = true;
    let mut route_from: Option<(i64, i64)> = None;
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
        } else if let Some(rest) = line.strip_prefix("G00X") {
            // Route mode (XNC): `G00` to the slot start, `M15`, `G01` to its end, `M16`.
            route_from = xy(rest);
        } else if let Some(rest) = line.strip_prefix("G01X") {
            let (f, d) = tools[&current.expect("route before tool selection")].clone();
            if let (Some((x0, y0)), Some((x1, y1))) = (route_from, xy(rest)) {
                out.push(Hit { plated, function: f, dia: d, x: (x0 + x1) / 2, y: (y0 + y1) / 2, slot: true });
            }
            route_from = xy(rest);
        } else if let Some(rest) = line.strip_prefix('X') {
            let (f, d) = tools[&current.expect("hit before tool selection")].clone();
            let hit = |x, y, slot| Hit { plated, function: f.clone(), dia: d, x, y, slot };
            match rest.split_once("G85X") {
                Some((a, b)) => {
                    let (Some((x0, y0)), Some((x1, y1))) = (xy(a), xy(b)) else { continue };
                    out.push(hit((x0 + x1) / 2, (y0 + y1) / 2, true));
                }
                None => {
                    let Some((x, y)) = xy(rest) else { continue };
                    out.push(hit(x, y, false));
                }
            }
        }
    }
    out
}

pub fn all_hits(dir: &Path) -> Vec<Hit> {
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
pub const HIT_TOL_UM: i64 = 1;

/// Hits of `a` without a counterpart in `b` and of `b` without one in `a` (same plating and
/// diameter, positions within [`HIT_TOL_UM`], slots matching only slots, and the same Gerber X2
/// hole function unless `any_function`: the function is the writer's reading of what the hole is
/// for, and differs on imported boards, e.g. KiCad's component drill for the pad of a stitching-via
/// footprint that cadlab imports as a via).
pub fn unmatched_hits(a: &[Hit], b: &[Hit], any_function: bool) -> (Vec<Hit>, Vec<Hit>) {
    let mut used = vec![false; b.len()];
    let mut only_a = Vec::new();
    for h in a {
        let hit = b.iter().enumerate().position(|(i, k)| {
            !used[i]
                && h.plated == k.plated
                && h.dia == k.dia
                && h.slot == k.slot
                && (any_function || h.function == k.function)
                && (h.x - k.x).abs() <= HIT_TOL_UM
                && (h.y - k.y).abs() <= HIT_TOL_UM
        });
        match hit {
            Some(i) => used[i] = true,
            None => only_a.push(h.clone()),
        }
    }
    let only_b = b.iter().zip(&used).filter(|(_, u)| !**u).map(|(h, _)| h.clone()).collect();
    (only_a, only_b)
}

/// Whether two hit lists match one to one.
pub fn same_hits(a: &[Hit], b: &[Hit]) -> bool {
    let (x, y) = unmatched_hits(a, b, false);
    x.is_empty() && y.is_empty()
}

#[test]
fn drill_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.drl");
    std::fs::write(
        &f,
        "M48\n; #@! TF.FileFunction,Plated,1,2,PTH\nMETRIC\n; #@! TA.AperFunction,Plated,PTH,ViaDrill\nT1C0.300\n%\nG05\nT1\nX3.5Y10.0\nX1Y2G85X1Y3\nG00X5.0Y1.0\nM15\nG01X6.0Y1.0\nM16\nG05\nM30\n",
    )
    .unwrap();
    let h = |x, y, slot| Hit { plated: true, function: "ViaDrill".to_string(), dia: 300, x, y, slot };
    assert_eq!(drill_hits(&f), vec![h(3500, 10000, false), h(1000, 2500, true), h(5500, 1000, true)]);
}
