//! What KiCad does with silkscreen, measured with `kicad-cli` (docs/TESTING.md, "Silkscreen";
//! DECISIONS D83): where it draws texts, and which silkscreen pairs its DRC reports from which
//! distance. Nothing here is read from KiCad's sources: two probe boards are written for the
//! purpose and KiCad's answers are compared with cadlab's on the same files.
//!
//! **Texts** (`kicad_text_geometry`). cadlab draws board texts and the silkscreen texts of
//! imported footprints itself (anchor, justification, lines, character width, stroke, slant,
//! mirroring, knockout), in its own stroke font. A board of probe texts on a grid is plotted by
//! `kicad-cli pcb export gerbers`, the strokes of each grid cell are read back from the Gerber
//! file, and their extent around the text's anchor is compared with the extent of what cadlab
//! draws after importing the same file.
//!
//! - Conventions (where the anchor is for each justification, the cap height, the line pitch,
//!   the slant, the widest stroke, mirroring, rotation, the knockout box, where a footprint's
//!   text goes when the footprint is turned or on the bottom) must agree: within
//!   [`CONVENTION_TOL`] vertically ([`KNOCKOUT_TOL`] for knockout boxes), and one font unit
//!   more in all ([`FONT_UNIT`]: KiCad's letters end a unit inside cadlab's).
//! - Glyphs differ, the fonts being two: each character's extent and advance are in the table
//!   (`$CARGO_TARGET_TMPDIR/kicad_text/table.txt`) for the record, and whole strings must come
//!   out within a tenth of each other's width.
//!
//! **Rules** (`kicad_silk_rules`). A board of probe cells, each holding one pair of silkscreen
//! items (or one item at the board edge) at a known distance, is run through `kicad-cli pcb drc`
//! and through cadlab's DRC at silkscreen clearances of 0 and 0.15 mm. The two must report the
//! same cells, except where they are known to differ, each for a reason the test names and
//! requires to still hold ([`known_rule_difference`]).
//!
//! Both need `CADLAB_ORACLES=1` and `kicad-cli`; without, cadlab's side of both boards runs
//! (`probe_board_imports`, `rule_probe_board_imports`) and the Gerber reader is tested.

mod common;
#[path = "common/drc_compare.rs"]
mod drc_compare;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cadlab::kicad_import::{self, BoardImportOptions};
use cadlab::kicad_pcb::Frame;
use cadlab::model::Project;
use cadlab::model::board::GraphicKind;
use cadlab::{Nm, Point};
use common::oracle::{self, Oracle};

/// Grid cell (mm): each probe has a cell to itself, its anchor in the middle.
const CELL: (f64, f64) = (48.0, 16.0);
const COLUMNS: usize = 5;
/// Stroke width of the calibration line (no probe uses it).
const CAL_WIDTH: f64 = 0.777;

/// One probe text.
#[derive(Clone, Debug)]
struct Probe {
    name: String,
    text: String,
    bottom: bool,
    /// Height, width, thickness (mm); no thickness: left out of the file.
    size: (f64, f64),
    thickness: Option<f64>,
    justify: &'static str,
    font_flags: &'static str,
    knockout: bool,
    angle: f64,
    /// A footprint text: the footprint's angle and side, the text at (3, 0) in its frame.
    footprint: Option<(f64, bool, &'static str)>,
}

impl Probe {
    fn new(name: &str, text: &str) -> Probe {
        Probe {
            name: name.to_string(),
            text: text.to_string(),
            bottom: false,
            size: (1.0, 1.0),
            thickness: Some(0.15),
            justify: "",
            font_flags: "",
            knockout: false,
            angle: 0.0,
            footprint: None,
        }
    }
    fn justify(mut self, j: &'static str) -> Probe {
        self.justify = j;
        self
    }
    fn size(mut self, h: f64, w: f64) -> Probe {
        self.size = (h, w);
        self
    }
    fn bottom(mut self) -> Probe {
        self.bottom = true;
        self
    }
    fn angle(mut self, a: f64) -> Probe {
        self.angle = a;
        self
    }
    fn flags(mut self, f: &'static str) -> Probe {
        self.font_flags = f;
        self
    }
    fn thickness(mut self, t: Option<f64>) -> Probe {
        self.thickness = t;
        self
    }
    fn knockout(mut self) -> Probe {
        self.knockout = true;
        self
    }
    fn in_footprint(mut self, angle: f64, bottom: bool, extra: &'static str) -> Probe {
        self.footprint = Some((angle, bottom, extra));
        self.bottom = bottom;
        self
    }
}

fn probes() -> Vec<Probe> {
    let p = Probe::new;
    let mut v = vec![
        // Anchor and justification.
        p("H center", "H"),
        p("H left bottom", "H").justify("left bottom"),
        p("H right top", "H").justify("right top"),
        p("H left", "H").justify("left"),
        p("H right", "H").justify("right"),
        p("H bottom", "H").justify("bottom"),
        p("H top", "H").justify("top"),
        p("Hg center", "Hg"),
        p("Hg bottom", "Hg").justify("bottom"),
        p("HELLO left bottom", "HELLO").justify("left bottom"),
        p("HELLO right", "HELLO").justify("right"),
        // Lines.
        p("2 lines center", "H\nH"),
        p("2 lines bottom", "H\nH").justify("bottom"),
        p("2 lines top", "H\nH").justify("top"),
        p("3 lines left bottom", "A\nBB\nC").justify("left bottom"),
        p("3 lines right", "A\nBB\nC").justify("right"),
        p("2 lines size 2", "H\nH").size(2.0, 2.0),
        // Size, stroke, slant.
        p("H 2x2", "H").size(2.0, 2.0),
        p("H h2 w1", "H").size(2.0, 1.0),
        p("H h1 w2", "H").size(1.0, 2.0),
        p("HELLO h1 w2 left", "HELLO").size(1.0, 2.0).justify("left"),
        p("H no thickness", "H").thickness(None),
        p("H 2x2 no thickness", "H").size(2.0, 2.0).thickness(None),
        p("H thick 0.3", "H").thickness(Some(0.3)),
        p("H bold 0.3", "H").thickness(Some(0.3)).flags(" (bold yes)"),
        p("HELLO bold", "HELLO").thickness(Some(0.2)).flags(" (bold yes)"),
        p("H italic", "H").flags(" (italic yes)"),
        p("HELLO italic left bottom", "HELLO").flags(" (italic yes)").justify("left bottom"),
        // Rotation and mirroring.
        p("HELLO left bottom 90", "HELLO").justify("left bottom").angle(90.0),
        p("HELLO left bottom 45", "HELLO").justify("left bottom").angle(45.0),
        p("HELLO left bottom 180", "HELLO").justify("left bottom").angle(180.0),
        p("L back mirror", "L").bottom().justify("mirror"),
        p("PULL back left mirror", "PULL").bottom().justify("left mirror"),
        p("PULL back left bottom mirror 90", "PULL").bottom().justify("left bottom mirror").angle(90.0),
        p("PULL back left bottom mirror 30", "PULL").bottom().justify("left bottom mirror").angle(30.0),
        p("PULL back left, not mirrored", "PULL").bottom().justify("left"),
        p("PULL front left mirror", "PULL").justify("left mirror"),
        // Knockout.
        p("HI knockout", "HI").knockout(),
        p("HI knockout 2x2 0.3", "HI").knockout().size(2.0, 2.0).thickness(Some(0.3)),
        p("HI knockout left bottom", "HI").knockout().justify("left bottom"),
        p("2 lines knockout", "HI\nHI").knockout(),
        // Footprint texts: footprint angle, side; the text at (3, 0) in the footprint's frame.
        p("fp 0, text 0", "FA").in_footprint(0.0, false, ""),
        p("fp 90, text 90", "FA").angle(90.0).in_footprint(90.0, false, ""),
        p("fp 90, text 0", "FA").in_footprint(90.0, false, ""),
        p("fp 180, text 180 left", "FA").angle(180.0).justify("left").in_footprint(180.0, false, ""),
        p("fp 180, text 180 left unlocked", "FA").angle(180.0).justify("left").in_footprint(
            180.0,
            false,
            " (unlocked yes)",
        ),
        p("fp 270, text 270 left", "FA").angle(270.0).justify("left").in_footprint(270.0, false, ""),
        p("fp 0 back, text 0 mirror left", "FA").justify("left mirror").in_footprint(0.0, true, ""),
        p("fp 90 back, text 90 mirror left", "FA").angle(90.0).justify("left mirror").in_footprint(90.0, true, ""),
        p("fp 180 back, text 180 mirror left", "FA").angle(180.0).justify("left mirror").in_footprint(180.0, true, ""),
        // Widths of whole strings.
        p("upper", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        p("lower", "abcdefghijklmnopqrstuvwxyz"),
        p("digits", "0123456789"),
        p("punct", "!#%&'()*+,-./:;<=>?@[]"),
        p("word GND", "GND"),
        p("word UART0CTS", "UART0CTS"),
        p("word BPIO3", "BPIO3"),
        p("word R12", "R12"),
        p("word C3", "C3"),
        p("word D2", "D2"),
        p("word LED705", "LED705"),
        p("word Tiny Tapeout", "Tiny Tapeout"),
        p("bars", "||").justify("left bottom"),
    ];
    // The box KiCad justifies, by character size and stroke: each corner and the middle.
    for (h, w, t) in [
        (2.0, 2.0, 0.15),
        (1.0, 1.0, 0.1),
        (1.0, 1.0, 0.25),
        (2.0, 2.0, 0.5),
        (2.0, 1.0, 0.15),
        (1.0, 2.0, 0.15),
        (3.0, 3.0, 0.3),
    ] {
        for j in ["left bottom", "right top", ""] {
            let name = format!("H {h}x{w} t{t} {}", if j.is_empty() { "center" } else { j });
            v.push(p(&name, "H").size(h, w).thickness(Some(t)).justify(j));
        }
    }
    for (h, t) in [(1.0, 0.25), (2.0, 0.15), (2.0, 0.5)] {
        v.push(p(&format!("2 lines {h} t{t}"), "H\nH").size(h, h).thickness(Some(t)));
    }
    // The widest stroke KiCad draws.
    v.push(p("H 1x2 t0.4", "H").size(1.0, 2.0).thickness(Some(0.4)));
    v.push(p("H 2x1 t0.4", "H").size(2.0, 1.0).thickness(Some(0.4)));
    v.push(p("H 1x1 t0.4 bold", "H").thickness(Some(0.4)).flags(" (bold yes)"));
    v.push(p("H 2x2 t0.6", "H").size(2.0, 2.0).thickness(Some(0.6)));
    v.push(p("H 2x2 t0.6 bold", "H").size(2.0, 2.0).thickness(Some(0.6)).flags(" (bold yes)"));
    // The knockout box: by size and stroke, and around letters lower than capitals.
    v.push(p("HI plain", "HI"));
    v.push(p("HI knockout t0.1", "HI").knockout().thickness(Some(0.1)));
    v.push(p("HI knockout 2x2 t0.15", "HI").knockout().size(2.0, 2.0));
    v.push(p("oo knockout", "oo").knockout());
    v.push(p("gg knockout", "gg").knockout());
    v.push(p("HI knockout 90", "HI").knockout().angle(90.0));
    // Italic by size.
    v.push(p("H italic 2x2", "H").size(2.0, 2.0).flags(" (italic yes)"));
    v.push(p("H italic left bottom", "H").flags(" (italic yes)").justify("left bottom"));
    // More footprint texts: angles around a quarter turn, a text on the bottom.
    v.push(p("fp 0, text 90", "FA").angle(90.0).in_footprint(0.0, false, ""));
    v.push(p("fp 0, text 270 left", "FA").angle(270.0).justify("left").in_footprint(0.0, false, ""));
    v.push(p("fp 0, text 135 left", "FA").angle(135.0).justify("left").in_footprint(0.0, false, ""));
    v.push(p("fp 0, text -45 left", "FA").angle(-45.0).justify("left").in_footprint(0.0, false, ""));
    v.push(p("fp 0 back, text 180 mirror left", "FA").angle(180.0).justify("left mirror").in_footprint(0.0, true, ""));
    v.push(p("fp 0 back, text 270 mirror left", "FA").angle(270.0).justify("left mirror").in_footprint(0.0, true, ""));
    // Each character: alone (its ink), and between two bars (its advance).
    for c in (33u8..=126).map(char::from) {
        v.push(p(&format!("char {c}"), &c.to_string()).justify("left bottom"));
        v.push(p(&format!("adv {c}"), &format!("|{c}|")).justify("left bottom"));
    }
    v
}

/// KiCad position (mm) of probe `i`'s anchor.
fn anchor(i: usize) -> (f64, f64) {
    (20.0 + CELL.0 * ((i % COLUMNS) as f64 + 0.5), 20.0 + CELL.1 * ((i / COLUMNS) as f64 + 0.5))
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

fn num(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The probe board: cadlab's export of an empty board (a file KiCad is known to load), with
/// the probe texts written by hand before its closing parenthesis.
fn probe_board(probes: &[Probe]) -> String {
    let p = Project::new("probe");
    let base = cadlab::kicad_pcb::export(&p, "probe").pcb;
    let end = base.trim_end().rfind(')').expect("the export ends with a parenthesis");
    let mut s = base[..end].to_string();
    for (layer, n) in [("F.SilkS", 0), ("B.SilkS", 1)] {
        s.push_str(&format!(
            "  (gr_line (start 1 1) (end 2 3) (stroke (width {CAL_WIDTH}) (type solid)) (layer \"{layer}\") (uuid \"00000000-0000-4000-8000-00000000000{n}\"))\n"
        ));
    }
    for (i, pr) in probes.iter().enumerate() {
        let (x, y) = anchor(i);
        let layer = if pr.bottom { "B.SilkS" } else { "F.SilkS" };
        let thickness = pr.thickness.map(|t| format!(" (thickness {})", num(t))).unwrap_or_default();
        let justify = if pr.justify.is_empty() { String::new() } else { format!(" (justify {})", pr.justify) };
        let effects = format!(
            "(effects (font (size {} {}){thickness}{}){justify})",
            num(pr.size.0),
            num(pr.size.1),
            pr.font_flags
        );
        let uuid = |k: usize| format!("\"00000000-0000-4000-9000-{:06}{k:06}\"", i);
        match pr.footprint {
            None => s.push_str(&format!(
                "  (gr_text {} (at {} {} {}) (layer \"{layer}\"{}) (uuid {})\n    {effects})\n",
                quoted(&pr.text),
                num(x),
                num(y),
                num(pr.angle),
                if pr.knockout { " knockout" } else { "" },
                uuid(0),
            )),
            Some((fp_angle, back, extra)) => {
                let cu = if back { "B" } else { "F" };
                s.push_str(&format!(
                    "  (footprint \"probe:P{i}\" (layer \"{cu}.Cu\") (uuid {}) (at {} {} {})\n",
                    uuid(1),
                    num(x),
                    num(y),
                    num(fp_angle)
                ));
                s.push_str(&format!(
                    "    (property \"Reference\" \"X{i}\" (at 0 -4 {}) (layer \"{cu}.SilkS\") (hide yes) (uuid {})\n      (effects (font (size 1 1) (thickness 0.15))))\n",
                    num(fp_angle),
                    uuid(2)
                ));
                s.push_str(&format!(
                    "    (fp_text user {} (at 3 0 {}){extra} (layer \"{layer}\") (uuid {})\n      {effects})\n",
                    quoted(&pr.text),
                    num(pr.angle),
                    uuid(3)
                ));
                s.push_str(&format!(
                    "    (pad \"1\" smd rect (at 0 0 {}) (size 0.5 0.5) (layers \"{cu}.Cu\" \"{cu}.Mask\") (uuid {}))\n  )\n",
                    num(fp_angle),
                    uuid(4)
                ));
            }
        }
    }
    s.push_str(")\n");
    s
}

/// Extent of ink: min x, max x, min y, max y.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Extent {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
}

impl Extent {
    fn add(e: &mut Option<Extent>, x: f64, y: f64, r: f64) {
        let n = Extent { x0: x - r, x1: x + r, y0: y - r, y1: y + r };
        *e = Some(match *e {
            None => n,
            Some(o) => Extent { x0: o.x0.min(n.x0), x1: o.x1.max(n.x1), y0: o.y0.min(n.y0), y1: o.y1.max(n.y1) },
        });
    }
    fn show(e: &Option<Extent>) -> String {
        match e {
            Some(e) => format!("x {:7.3} {:7.3}  y {:7.3} {:7.3}", e.x0, e.x1, e.y0, e.y1),
            None => format!("{:^36}", "-"),
        }
    }
    fn worst(a: &Extent, b: &Extent) -> f64 {
        [(a.x0 - b.x0), (a.x1 - b.x1), (a.y0 - b.y0), (a.y1 - b.y1)].iter().fold(0.0, |m: f64, d| m.max(d.abs()))
    }
}

type Xy = (f64, f64);

/// What a Gerber layer draws: strokes (ends and aperture diameter) and region vertices, in mm.
#[derive(Debug, Default, PartialEq)]
struct Drawn {
    strokes: Vec<(Xy, Xy, f64)>,
    region: Vec<(f64, f64)>,
    flashes: Vec<((f64, f64), f64)>,
    clear: bool,
}

/// Reads the draws of a Gerber file as KiCad writes them: `FSLAX..Y..` coordinates in mm,
/// circular apertures, `D01`/`D02`/`D03` with modal coordinates, `G36`/`G37` regions. Arcs
/// count by their ends.
fn read_gerber(text: &str) -> Drawn {
    let mut out = Drawn::default();
    let mut scale = 1e-6;
    let mut apertures: BTreeMap<u32, f64> = BTreeMap::new();
    let mut current = 0.0;
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let mut in_region = false;
    for raw in text.split('*') {
        let t = raw.trim_matches(|c: char| c == '%' || c.is_whitespace());
        if t.is_empty() || t.starts_with("G04") {
            continue;
        }
        if let Some(rest) = t.strip_prefix("FSLAX") {
            // `46Y46`: integer and decimal digits.
            if let Some(d) = rest.chars().nth(1).and_then(|c| c.to_digit(10)) {
                scale = 10f64.powi(-(d as i32));
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("ADD") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let shape = &rest[digits.len()..];
            let size = shape
                .split_once(',')
                .and_then(|(_, params)| params.split('X').next())
                .and_then(|d| d.parse::<f64>().ok())
                .unwrap_or(0.0);
            if let Ok(n) = digits.parse() {
                apertures.insert(n, size);
            }
            continue;
        }
        if t.starts_with("LPC") {
            out.clear = true;
            continue;
        }
        if t.starts_with("MO") || t.starts_with("LP") || t.starts_with('T') || t.starts_with("AM") {
            continue;
        }
        // Strip G codes in front of coordinates (`G01X...`), handling the region ones.
        let mut t = t;
        while t.starts_with('G') {
            let digits: String = t[1..].chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<u32>() {
                Ok(36) => in_region = true,
                Ok(37) => in_region = false,
                _ => {}
            }
            t = &t[1 + digits.len()..];
        }
        if t.is_empty() {
            continue;
        }
        // Fields: X, Y, I, J, D followed by a signed integer.
        let mut fields: BTreeMap<char, i64> = BTreeMap::new();
        let mut chars = t.char_indices().peekable();
        let mut ok = true;
        while let Some((i, c)) = chars.next() {
            if !"XYIJD".contains(c) {
                ok = false;
                break;
            }
            let start = i + 1;
            let mut end = start;
            while let Some(&(j, d)) = chars.peek() {
                if d.is_ascii_digit() || d == '-' || d == '+' {
                    end = j + 1;
                    chars.next();
                } else {
                    break;
                }
            }
            match t[start..end].parse::<i64>() {
                Ok(v) => {
                    fields.insert(c, v);
                }
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let (nx, ny) =
            (fields.get(&'X').map_or(x, |v| *v as f64 * scale), fields.get(&'Y').map_or(y, |v| *v as f64 * scale));
        match fields.get(&'D') {
            Some(&d) if d >= 10 => current = apertures.get(&(d as u32)).copied().unwrap_or(0.0),
            Some(1) | None if fields.contains_key(&'X') || fields.contains_key(&'Y') => {
                if in_region {
                    out.region.push((nx, ny));
                } else {
                    out.strokes.push(((x, y), (nx, ny), current));
                }
            }
            Some(2) if in_region => out.region.push((nx, ny)),
            Some(3) => out.flashes.push(((nx, ny), current)),
            _ => {}
        }
        (x, y) = (nx, ny);
    }
    out
}

/// The Gerber-to-KiCad mapping, from the calibration line (1, 1)–(2, 3): offset and whether Y
/// is flipped.
#[derive(Clone, Copy, Debug)]
struct Calibration {
    ox: f64,
    oy: f64,
    sy: f64,
}

impl Calibration {
    fn of(d: &Drawn) -> Option<Calibration> {
        let (a, b, _) = d.strokes.iter().find(|(_, _, w)| (w - CAL_WIDTH).abs() < 1e-6)?;
        let (p, q) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        let sy = if q.1 > p.1 { 1.0 } else { -1.0 };
        Some(Calibration { ox: p.0 - 1.0, oy: p.1 - sy * 1.0, sy })
    }
    /// KiCad position of a Gerber point.
    fn kicad(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (x - self.ox, self.sy * (y - self.oy))
    }
}

/// Per probe: the extent of the strokes (with their width) and of the regions KiCad plots in its
/// cell, relative to the anchor with Y up, and the stroke widths used.
fn kicad_extents(d: &Drawn, cal: &Calibration, n: usize) -> Vec<(Option<Extent>, Option<Extent>, Vec<f64>)> {
    let mut out = vec![(None, None, Vec::new()); n];
    let cell = |(x, y): (f64, f64)| -> Option<(usize, f64, f64)> {
        let (cx, cy) = (((x - 20.0) / CELL.0).floor(), ((y - 20.0) / CELL.1).floor());
        if cx < 0.0 || cy < 0.0 || cx >= COLUMNS as f64 {
            return None;
        }
        let i = cy as usize * COLUMNS + cx as usize;
        let (ax, ay) = anchor(i);
        (i < n).then_some((i, x - ax, -(y - ay)))
    };
    for (a, b, w) in &d.strokes {
        if (w - CAL_WIDTH).abs() < 1e-6 {
            continue;
        }
        for q in [a, b] {
            if let Some((i, x, y)) = cell(cal.kicad(*q)) {
                Extent::add(&mut out[i].0, x, y, w / 2.0);
                if !out[i].2.iter().any(|v: &f64| (v - w).abs() < 1e-9) {
                    out[i].2.push(*w);
                }
            }
        }
    }
    for q in &d.region {
        if let Some((i, x, y)) = cell(cal.kicad(*q)) {
            Extent::add(&mut out[i].1, x, y, 0.0);
        }
    }
    out
}

fn find(dir: &Path, suffix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(suffix)))
}

/// What cadlab draws for each probe after importing the board: extent of the ink (strokes with
/// their width, or the knockout region) relative to the anchor, Y up, and the stroke width.
fn cadlab_extents(pcb: &str, probes: &[Probe]) -> Vec<(Option<Extent>, f64)> {
    let mut p = Project::new("probe");
    let (report, _) =
        kicad_import::import(&mut p, pcb, &BoardImportOptions::default()).expect("the probe board imports");
    let frame = Frame { origin: report.origin };
    let rel = |q: Point, i: usize| {
        let (kx, ky) = frame.to_kicad(q);
        let (ax, ay) = anchor(i);
        (kx.0 as f64 / 1e6 - ax, -(ky.0 as f64 / 1e6 - ay))
    };
    let of_box = |b: Option<cadlab::geom::poly::Rect>, i: usize| {
        let mut e = None;
        if let Some(b) = b {
            for (x, y) in [(b.min.x, b.min.y), (b.max.x, b.max.y)] {
                let (rx, ry) = rel(Point::new(Nm(x), Nm(y)), i);
                Extent::add(&mut e, rx, ry, 0.0);
            }
        }
        e
    };
    let mut texts = p.board().graphics.iter().filter(|g| matches!(g.kind, GraphicKind::Text { .. }));
    let drawings = cadlab::board::silk::silk_shapes(&p);
    let mut out = Vec::new();
    for (i, pr) in probes.iter().enumerate() {
        if pr.footprint.is_some() {
            let label = format!("X{i}");
            let s = drawings.iter().find(|s| s.label == label);
            let width = p
                .library()
                .footprints
                .values()
                .flat_map(|f| f.graphics.iter())
                .map(|g| g.width.0 as f64 / 1e6)
                .next()
                .unwrap_or(0.0);
            out.push((of_box(s.and_then(|s| s.bbox), i), width));
            continue;
        }
        let g = texts.next().expect("a board text per probe");
        let GraphicKind::Text { text, .. } = &g.kind else { unreachable!() };
        assert_eq!(text, &pr.text, "probe {i} ({}) is the board text in file order", pr.name);
        let ink = cadlab::board::text_ink(&p, &g.layer, &g.kind).expect("a text");
        out.push((of_box(ink.bbox(), i), ink.width.0 as f64 / 1e6));
    }
    out
}

#[test]
fn gerber_reader() {
    let g = "%FSLAX46Y46*%\n%MOMM*%\n%LPD*%\nG01*\n%ADD10C,0.150000*%\n%ADD11C,0.777000*%\nD10*\n\
             X15000000Y-15000000D02*\nX15500000D01*\nG01X15500000Y-14000000D01*\nD11*\nX1000000Y-1000000D02*\n\
             X2000000Y-3000000D01*\nG36*\nX0Y0D02*\nG01*\nX1000000Y0D01*\nX1000000Y1000000D01*\nG37*\nM02*\n";
    let d = read_gerber(g);
    assert_eq!(d.strokes.len(), 3);
    assert_eq!(d.strokes[0], ((15.0, -15.0), (15.5, -15.0), 0.15));
    assert_eq!(d.strokes[1], ((15.5, -15.0), (15.5, -14.0), 0.15));
    assert_eq!(d.region, vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]);
    let cal = Calibration::of(&d).unwrap_or_else(|| panic!("the calibration line: {d:?}"));
    assert!(cal.ox.abs() < 1e-9 && cal.oy.abs() < 1e-9 && cal.sy == -1.0, "{cal:?}");
    assert_eq!(cal.kicad((15.0, -15.0)), (15.0, 15.0));
}

#[test]
fn probe_board_imports() {
    // The probe file is one cadlab reads: every board text in order, every footprint text drawn.
    let probes = probes();
    let pcb = probe_board(&probes);
    let ours = cadlab_extents(&pcb, &probes);
    assert_eq!(ours.len(), probes.len());
    for (pr, (e, w)) in probes.iter().zip(&ours) {
        assert!(e.is_some(), "{}: cadlab draws nothing", pr.name);
        if std::env::var_os("CADLAB_SHOW_PROBES").is_some() {
            let e = e.expect("drawn");
            eprintln!("{:<36} x {:.6} {:.6} y {:.6} {:.6}  {w:.3}", pr.name, e.x0, e.x1, e.y0, e.y1);
        }
    }
}

#[test]
fn kicad_text_geometry() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("kicad_text");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let probes = probes();
    let pcb_text = probe_board(&probes);
    let pcb = dir.join("probe.kicad_pcb");
    std::fs::write(&pcb, &pcb_text).unwrap();
    let out = format!("{}/", dir.join("gerber").display());
    std::fs::create_dir_all(dir.join("gerber")).unwrap();
    oracle::run(&cli, &["pcb", "export", "gerbers", "--layers", "F.SilkS,B.SilkS", "-o", &out, pcb.to_str().unwrap()]);
    let read = |suffixes: &[&str]| -> (Drawn, Calibration) {
        let f = suffixes
            .iter()
            .find_map(|s| find(&dir.join("gerber"), s))
            .unwrap_or_else(|| panic!("no Gerber file {suffixes:?} in {}", dir.join("gerber").display()));
        let d = read_gerber(&std::fs::read_to_string(&f).unwrap());
        let cal = Calibration::of(&d).unwrap_or_else(|| panic!("no calibration line in {}", f.display()));
        (d, cal)
    };
    let (front, fcal) = read(&["-F_Silkscreen.gbr", "-F_SilkS.gbr", "-F_Silkscreen.gto"]);
    let (back, bcal) = read(&["-B_Silkscreen.gbr", "-B_SilkS.gbr", "-B_Silkscreen.gbo"]);
    eprintln!(
        "front: {} strokes, {} region vertices, {fcal:?}; back: {} strokes, {} region vertices, {bcal:?}",
        front.strokes.len(),
        front.region.len(),
        back.strokes.len(),
        back.region.len()
    );
    let kf = kicad_extents(&front, &fcal, probes.len());
    let kb = kicad_extents(&back, &bcal, probes.len());
    let ours = cadlab_extents(&pcb_text, &probes);

    let mut table = String::new();
    table.push_str(&format!(
        "{:<36} {:<36}  {:<36}  {:>6} {:>6}  {:>6}\n",
        "probe (anchor at 0, 0; mm; Y up)", "KiCad ink", "cadlab ink", "KiCad", "cadlab", "worst"
    ));
    let mut widths = Vec::new();
    let mut problems = Vec::new();
    for (i, pr) in probes.iter().enumerate() {
        let (strokes, region, apertures) = if pr.bottom { &kb[i] } else { &kf[i] };
        let kicad = if pr.knockout && region.is_some() { region } else { strokes };
        let (cad, cad_w) = &ours[i];
        // Conventions: the probes whose letters end where KiCad's do, to a font unit.
        if pr.convention() {
            match (kicad, cad) {
                (Some(a), Some(b)) => {
                    let exact = if pr.knockout { KNOCKOUT_TOL } else { CONVENTION_TOL };
                    let tol = FONT_UNIT * pr.size.0.max(pr.size.1) + exact;
                    if Extent::worst(a, b) > tol {
                        problems.push(format!(
                            "{}: extents differ by {:.3} mm (over {tol:.3})",
                            pr.name,
                            Extent::worst(a, b)
                        ));
                    }
                    // Unturned capitals are as high in both fonts: the vertical placement is exact.
                    let upright = pr.angle == 0.0 && pr.footprint.is_none();
                    if upright && (a.y0 - b.y0).abs().max((a.y1 - b.y1).abs()) > exact {
                        problems.push(format!("{}: placed {:.3} mm higher", pr.name, b.y0 - a.y0));
                    }
                }
                _ => problems.push(format!("{}: nothing drawn by one tool", pr.name)),
            }
            if !pr.knockout && !apertures.iter().all(|a| (a - cad_w).abs() < 1e-6) {
                problems.push(format!("{}: KiCad strokes {apertures:?} mm, cadlab {cad_w} mm", pr.name));
            }
        }
        let worst = match (kicad, cad) {
            (Some(a), Some(b)) => format!("{:6.3}", Extent::worst(a, b)),
            _ => "     -".to_string(),
        };
        table.push_str(&format!(
            "{:<36} {}  {}  {:>6} {:>6.3}  {worst}{}\n",
            pr.name,
            Extent::show(kicad),
            Extent::show(cad),
            apertures.iter().map(|a| format!("{a:.3}")).collect::<Vec<_>>().join("/"),
            cad_w,
            if pr.knockout && strokes.is_some() {
                format!("   strokes: {}", Extent::show(strokes))
            } else {
                String::new()
            }
        ));
        if let (Some(a), Some(b)) = (kicad, cad)
            && (pr.name.starts_with("word") || ["upper", "lower", "digits", "punct"].contains(&pr.name.as_str()))
        {
            widths.push((pr.name.clone(), a.x1 - a.x0, b.x1 - b.x0));
        }
    }
    table.push_str("\nInk widths of whole strings (mm at 1 mm height): KiCad, cadlab, cadlab / KiCad\n");
    for (name, k, c) in &widths {
        table.push_str(&format!("{name:<20} {k:7.3} {c:7.3} {:6.3}\n", c / k));
    }
    std::fs::write(dir.join("table.txt"), &table).unwrap();
    eprintln!("{table}");
    // The fonts are two: whole strings come out within a tenth of each other's width.
    for (name, k, c) in &widths {
        if !(0.9..1.1).contains(&(c / k)) {
            problems.push(format!("{name}: {c:.3} mm wide in cadlab's font, {k:.3} mm in KiCad's"));
        }
    }
    assert!(problems.is_empty(), "text conventions differ from KiCad's:\n{}", problems.join("\n"));
}

/// Conventions must agree within this (mm): the Gerber coordinates fit cadlab's to a few
/// nanometers.
const CONVENTION_TOL: f64 = 0.001;
/// A knockout box must agree within this (mm): KiCad builds it around a polygon of the strokes,
/// whose round ends fall 1 to 4 µm short.
const KNOCKOUT_TOL: f64 = 0.006;
/// One unit of the fonts' 21-unit capital height, as a part of the character size: where
/// KiCad's letters end a unit inside cadlab's (most capitals, on each side).
const FONT_UNIT: f64 = 0.05;

impl Probe {
    /// Whether the probe measures a convention (placement, size, stroke) rather than glyphs.
    fn convention(&self) -> bool {
        let n = self.name.as_str();
        !(n.starts_with("char ") || n.starts_with("adv ") || n.starts_with("word ") || n == "bars")
            && !["upper", "lower", "digits", "punct"].contains(&n)
    }
}

// ---------------------------------------------------------------------------------------------
// Which silkscreen pairs KiCad's DRC reports, and from which distance

/// Rule probe cells are 10 mm squares; column 0 lies on the board's left edge (x = 0).
const RCELL: f64 = 10.0;
const RCOLUMNS: usize = 10;

/// One rule probe: a name and the KiCad items of its cell, written around the cell's center.
struct RuleProbe {
    name: String,
    /// At the board's left edge (column 0) rather than inside the board.
    edge: bool,
    items: Vec<Item>,
}

/// A probe item, in millimeters around the cell center (edge probes: X from the board edge),
/// Y up.
#[derive(Clone)]
enum Item {
    /// A board line: layer, ends, width.
    Line(&'static str, (f64, f64), (f64, f64), f64),
    /// A board text, 1 mm high, centered.
    Text(&'static str, &'static str, (f64, f64)),
    /// A footprint at a position, on the bottom or not, its reference at an offset and shown
    /// or hidden, with drawings in its frame.
    Footprint((f64, f64), bool, ((f64, f64), bool), Vec<FpItem>),
}

#[derive(Clone)]
enum FpItem {
    Line((f64, f64), (f64, f64), f64),
    /// Center, radius, width, filled.
    Circle((f64, f64), f64, f64, bool),
    /// Vertices, width, filled.
    Poly(Vec<(f64, f64)>, f64, bool),
    /// A user text, 1 mm high, centered.
    Text(&'static str, (f64, f64)),
}

/// A reference hidden, out of the way.
const HIDDEN: ((f64, f64), bool) = ((0.0, 4.0), false);

fn rule_probes() -> Vec<RuleProbe> {
    use FpItem as F;
    use Item::*;
    let mut v = Vec::new();
    let probe = |name: String, items: Vec<Item>| RuleProbe { name, edge: false, items };
    let board_line = || Line("F.SilkS", (-3.0, 0.0), (3.0, 0.0), 0.15);
    // A board line 0.15 mm wide against a footprint drawing 0.12 mm wide, `gap` apart
    // (negative: overlapping by that much).
    for gap_um in [-50, -35, -20, -10, -5, -2, 0, 2, 5, 10, 50, 100, 140, 148, 150, 152, 160, 200] {
        let gap = gap_um as f64 / 1000.0;
        let y = 0.075 + gap + 0.06;
        let fp = |items: Vec<FpItem>| Footprint((0.0, 0.0), false, HIDDEN, items);
        v.push(probe(
            format!("line / fp line, gap {gap_um} um"),
            vec![board_line(), fp(vec![F::Line((-3.0, y), (3.0, y), 0.12)])],
        ));
        v.push(probe(
            format!("line / fp ring, gap {gap_um} um"),
            vec![board_line(), fp(vec![F::Circle((0.0, y + 0.7), 0.7, 0.12, false)])],
        ));
        v.push(probe(
            format!("line / fp disc, gap {gap_um} um"),
            vec![board_line(), fp(vec![F::Circle((0.0, y + 0.7), 0.7, 0.12, true)])],
        ));
        v.push(probe(
            format!("line / fp polygon outline, gap {gap_um} um"),
            vec![
                board_line(),
                fp(vec![F::Poly(vec![(-2.0, y), (2.0, y), (2.0, y + 1.0), (-2.0, y + 1.0)], 0.12, false)]),
            ],
        ));
    }
    // Which pairs are checked at all: each pair overlaps by 0.05 mm, or a text lies on the item.
    let y = 0.075 - 0.05 + 0.06;
    let fp_at = |at: (f64, f64), reference: Option<(f64, f64)>, items: Vec<FpItem>| {
        Footprint(at, false, reference.map_or(HIDDEN, |q| (q, true)), items)
    };
    let fp_line = || F::Line((-3.0, 0.0), (3.0, 0.0), 0.12);
    let pairs: Vec<(&str, Vec<Item>)> = vec![
        ("board line / board line", vec![board_line(), Line("F.SilkS", (-3.0, 0.1), (3.0, 0.1), 0.15)]),
        (
            "two lines of one footprint",
            vec![fp_at((0.0, 0.0), None, vec![fp_line(), F::Line((-3.0, 0.07), (3.0, 0.07), 0.12)])],
        ),
        (
            "lines of two footprints",
            vec![fp_at((0.0, 0.0), None, vec![fp_line()]), fp_at((0.0, 0.07), None, vec![fp_line()])],
        ),
        (
            "fp line on the front / board line on the back",
            vec![Line("B.SilkS", (-3.0, 0.0), (3.0, 0.0), 0.15), fp_at((0.0, y), None, vec![fp_line()])],
        ),
        (
            "fp line on the back / board line on the back",
            vec![Line("B.SilkS", (-3.0, 0.0), (3.0, 0.0), 0.15), Footprint((0.0, y), true, HIDDEN, vec![fp_line()])],
        ),
        ("reference on its own footprint's line", vec![fp_at((0.0, 0.0), Some((0.0, 0.0)), vec![fp_line()])]),
        ("reference on a board line", vec![board_line(), fp_at((0.0, 0.0), Some((0.0, 0.0)), vec![])]),
        (
            "reference on another footprint's line",
            vec![fp_at((0.0, 0.0), None, vec![fp_line()]), fp_at((0.0, -1.0), Some((0.0, 1.0)), vec![])],
        ),
        (
            "hidden reference on a board line",
            vec![board_line(), Footprint((0.0, 0.0), false, ((0.0, 0.0), false), vec![])],
        ),
        (
            "fp text on its own footprint's line",
            vec![fp_at((0.0, 0.0), None, vec![fp_line(), F::Text("AB", (0.0, 0.0))])],
        ),
        (
            "fp text on another footprint's line",
            vec![fp_at((0.0, 0.0), None, vec![fp_line()]), fp_at((0.0, -1.0), None, vec![F::Text("AB", (0.0, 1.0))])],
        ),
        (
            "fp text on another footprint's text",
            vec![
                fp_at((0.0, 0.0), None, vec![F::Text("AB", (0.0, 0.0))]),
                fp_at((0.0, -1.0), None, vec![F::Text("AB", (0.3, 1.0))]),
            ],
        ),
        (
            "reference on a text of its own footprint",
            vec![fp_at((0.0, 0.0), Some((0.3, 0.0)), vec![F::Text("AB", (0.0, 0.0))])],
        ),
        (
            "two texts of one footprint",
            vec![fp_at((0.0, 0.0), None, vec![F::Text("AB", (0.0, 0.0)), F::Text("AB", (0.3, 0.0))])],
        ),
        ("board text on a board line", vec![board_line(), Text("F.SilkS", "AB", (0.0, 0.0))]),
        ("board text on a board text", vec![Text("F.SilkS", "AB", (0.0, 0.0)), Text("F.SilkS", "AB", (0.3, 0.0))]),
        ("board text on a fp line", vec![Text("F.SilkS", "AB", (0.0, 0.0)), fp_at((0.0, 0.0), None, vec![fp_line()])]),
        (
            "board text on the front / board line on the back",
            vec![Line("B.SilkS", (-3.0, 0.0), (3.0, 0.0), 0.15), Text("F.SilkS", "AB", (0.0, 0.0))],
        ),
        // Drawings on the Margin layer.
        ("silk line across a Margin line", vec![Line("Margin", (0.0, -3.0), (0.0, 3.0), 0.1), board_line()]),
        (
            "silk line 1 mm from a Margin line",
            vec![Line("Margin", (0.0, -3.0), (0.0, 3.0), 0.1), Line("F.SilkS", (1.0, -3.0), (1.0, 3.0), 0.15)],
        ),
    ];
    for (name, items) in pairs {
        v.push(probe(name.to_string(), items));
    }
    // The board edge at x = 0: a vertical board line whose ink is `gap` inside the board
    // (negative: across the edge), then wholly outside.
    let edge = |name: String, items: Vec<Item>| RuleProbe { name, edge: true, items };
    for gap_um in [-50, 0, 2, 5, 100, 148, 152, 200] {
        let x = 0.075 + gap_um as f64 / 1000.0;
        v.push(edge(format!("edge / line inside, gap {gap_um} um"), vec![Line("F.SilkS", (x, -3.0), (x, 3.0), 0.15)]));
    }
    for gap_um in [100, 152, 1000, 4000] {
        let x = -0.075 - gap_um as f64 / 1000.0;
        v.push(edge(format!("edge / line outside, gap {gap_um} um"), vec![Line("F.SilkS", (x, -3.0), (x, 3.0), 0.15)]));
    }
    v.push(edge(
        "edge / fp line inside, gap 100 um".into(),
        vec![Footprint((3.0, 0.0), false, HIDDEN, vec![F::Line((-2.825, -3.0), (-2.825, 3.0), 0.15)])],
    ));
    v.push(edge(
        "edge / fp line 1 mm outside".into(),
        vec![Footprint((3.0, 0.0), false, HIDDEN, vec![F::Line((-4.0, -3.0), (-4.0, 3.0), 0.15)])],
    ));
    v.push(edge(
        "edge / fp line across".into(),
        vec![Footprint((3.0, 0.0), false, HIDDEN, vec![F::Line((-5.0, 0.0), (-1.0, 0.0), 0.15)])],
    ));
    v.push(edge("edge / board text across".into(), vec![Text("F.SilkS", "ABAB", (0.0, 0.0))]));
    v.push(edge("edge / board text 3 mm outside".into(), vec![Text("F.SilkS", "AB", (-3.0, 0.0))]));
    v.push(edge("edge / reference across".into(), vec![Footprint((3.0, 0.0), false, ((-3.0, 0.0), true), vec![])]));
    v.push(edge(
        "edge / reference 3 mm outside".into(),
        vec![Footprint((3.0, 0.0), false, ((-6.0, 0.0), true), vec![])],
    ));
    v.push(edge(
        "edge / fp text across".into(),
        vec![Footprint((3.0, 0.0), false, HIDDEN, vec![F::Text("ABAB", (-3.0, 0.0))])],
    ));
    v
}

/// Center of rule probe `i` in cadlab coordinates (mm): edge probes in column 0 with the board
/// edge at x = 0 as their X origin, the others in columns 1 and up.
fn rule_cell(probes: &[RuleProbe], i: usize) -> (f64, f64) {
    let edges = probes[..i].iter().filter(|p| p.edge).count();
    let inner = i - edges;
    if probes[i].edge {
        (0.0, RCELL * (edges as f64 + 0.5))
    } else {
        (RCELL * ((inner % RCOLUMNS) as f64 + 1.5), RCELL * ((inner / RCOLUMNS) as f64 + 0.5))
    }
}

/// The probe a cadlab position belongs to.
fn rule_probe_at(probes: &[RuleProbe], x: f64, y: f64) -> Option<usize> {
    let row = (y / RCELL).floor();
    if row < 0.0 {
        return None;
    }
    let row = row as usize;
    if x < RCELL {
        return probes.iter().enumerate().filter(|(_, p)| p.edge).nth(row).map(|(i, _)| i);
    }
    let col = ((x / RCELL).floor() as usize).checked_sub(1)?;
    if col >= RCOLUMNS {
        return None;
    }
    probes.iter().enumerate().filter(|(_, p)| !p.edge).nth(row * RCOLUMNS + col).map(|(i, _)| i)
}

/// The rule probe board for a silkscreen clearance: cadlab's export of an empty board with
/// that rule (`.kicad_pcb`, `.kicad_pro`, `.kicad_dru`), the probe items written by hand.
fn rule_board(probes: &[RuleProbe], clearance: &str) -> (cadlab::kicad_pcb::KicadExport, Frame) {
    use cadlab::command::{Registry, RunOptions, Session};
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let r = Registry::with_builtins();
    let mut s = Session::new();
    let mut exec = |cmd: &str, args: serde_json::Value| {
        if let Err(f) = r.execute(&mut s, cmd, args, RunOptions::default()) {
            panic!("{cmd} failed: {}", f.error);
        }
    };
    exec("project.new", json!({"path": dir.path().join("p")}));
    let rows_inner = probes.iter().filter(|p| !p.edge).count().div_ceil(RCOLUMNS);
    let rows = rows_inner.max(probes.iter().filter(|p| p.edge).count());
    let (w, h) = (RCELL * (RCOLUMNS as f64 + 1.5), RCELL * rows as f64);
    exec("board.outline", json!({"width": format!("{w}mm"), "height": format!("{h}mm")}));
    exec("board.rules", json!({"silk_clearance": clearance, "silk_to_edge": clearance}));
    let p = s.project.as_ref().unwrap();
    let frame = Frame::of(p);
    let mut export = cadlab::kicad_pcb::export(p, "board");
    let end = export.pcb.trim_end().rfind(')').expect("the export ends with a parenthesis");
    let mut out = export.pcb[..end].to_string();
    let mut next = 0usize;
    let mut uuid = || {
        next += 1;
        format!("\"00000000-0000-4000-a000-{next:012}\"")
    };
    let mut fp_number = 0usize;
    for (i, pr) in probes.iter().enumerate() {
        let (cx, cy) = rule_cell(probes, i);
        // KiCad position of a point of the cell.
        let k = |(x, y): (f64, f64)| {
            let q =
                frame.to_kicad(Point::new(Nm(((cx + x) * 1e6).round() as i64), Nm(((cy + y) * 1e6).round() as i64)));
            format!("{} {}", num(q.0.0 as f64 / 1e6), num(q.1.0 as f64 / 1e6))
        };
        let font = "(effects (font (size 1 1) (thickness 0.15)))";
        for item in &pr.items {
            match item {
                Item::Line(layer, a, b, w) => out.push_str(&format!(
                    "  (gr_line (start {}) (end {}) (stroke (width {}) (type solid)) (layer \"{layer}\") (uuid {}))\n",
                    k(*a),
                    k(*b),
                    num(*w),
                    uuid()
                )),
                Item::Text(layer, text, at) => out.push_str(&format!(
                    "  (gr_text \"{text}\" (at {} 0) (layer \"{layer}\") (uuid {})\n    {font})\n",
                    k(*at),
                    uuid()
                )),
                Item::Footprint(at, back, reference, items) => {
                    fp_number += 1;
                    let side = if *back { "B" } else { "F" };
                    // Footprint-local KiCad coordinates: Y down (a footprint on the back is
                    // written as seen from the top, like KiCad does).
                    let l = |(x, y): (f64, f64)| format!("{} {}", num(x), num(-y));
                    let mirror = if *back { " (justify mirror)" } else { "" };
                    let font = format!("(effects (font (size 1 1) (thickness 0.15)){mirror})");
                    out.push_str(&format!(
                        "  (footprint \"probe:R{fp_number}\" (layer \"{side}.Cu\") (uuid {}) (at {})\n",
                        uuid(),
                        k(*at)
                    ));
                    out.push_str(&format!(
                        "    (property \"Reference\" \"X{fp_number}\" (at {} 0) (layer \"{side}.SilkS\"){} (uuid {})\n      {font})\n",
                        l(reference.0),
                        if reference.1 { "" } else { " (hide yes)" },
                        uuid()
                    ));
                    for it in items {
                        let stroke = |w: &f64| format!("(stroke (width {}) (type solid))", num(*w));
                        let fill = |f: &bool| if *f { "(fill solid)" } else { "(fill none)" };
                        match it {
                            FpItem::Line(a, b, w) => out.push_str(&format!(
                                "    (fp_line (start {}) (end {}) {} (layer \"{side}.SilkS\") (uuid {}))\n",
                                l(*a),
                                l(*b),
                                stroke(w),
                                uuid()
                            )),
                            FpItem::Circle(c, r, w, filled) => out.push_str(&format!(
                                "    (fp_circle (center {}) (end {}) {} {} (layer \"{side}.SilkS\") (uuid {}))\n",
                                l(*c),
                                l((c.0 + r, c.1)),
                                stroke(w),
                                fill(filled),
                                uuid()
                            )),
                            FpItem::Poly(pts, w, filled) => out.push_str(&format!(
                                "    (fp_poly (pts {}) {} {} (layer \"{side}.SilkS\") (uuid {}))\n",
                                pts.iter().map(|q| format!("(xy {})", l(*q))).collect::<Vec<_>>().join(" "),
                                stroke(w),
                                fill(filled),
                                uuid()
                            )),
                            FpItem::Text(text, at) => out.push_str(&format!(
                                "    (fp_text user \"{text}\" (at {} 0) (layer \"{side}.SilkS\") (uuid {})\n      {font})\n",
                                l(*at),
                                uuid()
                            )),
                        }
                    }
                    // A pad, 4 mm below, so the footprint is a part.
                    out.push_str(&format!(
                        "    (pad \"1\" smd rect (at 0 4) (size 0.5 0.5) (layers \"{side}.Cu\" \"{side}.Mask\") (uuid {}))\n  )\n",
                        uuid()
                    ));
                }
            }
        }
    }
    out.push_str(")\n");
    export.pcb = out;
    (export, frame)
}

/// Per probe, what each tool's DRC reports in its cell on the rule board with `clearance`:
/// KiCad's silkscreen violation types, cadlab's silkscreen codes.
fn rule_results(
    cli: Option<&Path>,
    dir: &Path,
    probes: &[RuleProbe],
    clearance: &str,
) -> Vec<(Vec<String>, Vec<String>)> {
    std::fs::create_dir_all(dir).unwrap();
    let (export, frame) = rule_board(probes, clearance);
    let pcb = dir.join("board.kicad_pcb");
    std::fs::write(&pcb, &export.pcb).unwrap();
    std::fs::write(dir.join("board.kicad_pro"), &export.project).unwrap();
    std::fs::write(dir.join("board.kicad_dru"), &export.rules).unwrap();
    let mut out = vec![(Vec::new(), Vec::new()); probes.len()];
    let report = match cli {
        Some(cli) => drc_compare::kicad_drc(cli, &pcb, &dir.join("drc.json"), &[]),
        None => serde_json::json!({"violations": []}),
    };
    for v in report["violations"].as_array().unwrap() {
        let ty = v["type"].as_str().unwrap_or_default();
        if !matches!(ty, "silk_overlap" | "silk_edge_clearance") {
            continue;
        }
        // The cell of the first item that is not the board edge.
        let cell = v["items"].as_array().unwrap().iter().find_map(|i| {
            let desc = i["description"].as_str().unwrap_or_default();
            if desc.contains("Edge.Cuts") {
                return None;
            }
            let (x, y) = (i["pos"]["x"].as_f64()?, i["pos"]["y"].as_f64()?);
            let q = frame.from_kicad(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64));
            rule_probe_at(probes, q.x.0 as f64 / 1e6, q.y.0 as f64 / 1e6)
        });
        match cell {
            Some(i) => out[i].0.push(if ty == "silk_overlap" { "overlap" } else { "edge" }.to_string()),
            None => eprintln!("a KiCad finding outside every probe cell: {v}"),
        }
    }
    // cadlab: the same files, imported.
    let (k, _) = cadlab::kicad_import::rules::parse(Some(&export.project), Some(&export.rules)).expect("the rules");
    let mut p = Project::new("probe");
    let opts = BoardImportOptions { rules: Some(k), ..Default::default() };
    kicad_import::import(&mut p, &export.pcb, &opts).expect("the rule probe board imports");
    for d in cadlab::drc::check(&p) {
        if !matches!(d.code.as_ref(), "drc.silk_edge" | "drc.silk_overlap") {
            continue;
        }
        let cell = d.location.and_then(|q| rule_probe_at(probes, q.x.0 as f64 / 1e6, q.y.0 as f64 / 1e6));
        match cell {
            Some(i) => out[i].1.push(d.code.trim_start_matches("drc.silk_").to_string()),
            None => eprintln!("a cadlab finding outside every probe cell: {d}"),
        }
    }
    for (a, b) in &mut out {
        a.sort();
        a.dedup();
        b.sort();
        b.dedup();
    }
    out
}

#[test]
fn rule_probe_board_imports() {
    // Every probe item lands in its own cell, and cadlab's DRC runs on the board.
    let probes = rule_probes();
    for i in 0..probes.len() {
        let (x, y) = rule_cell(&probes, i);
        assert_eq!(
            rule_probe_at(&probes, x + if probes[i].edge { 1.0 } else { 0.0 }, y),
            Some(i),
            "{}",
            probes[i].name
        );
        if probes[i].edge {
            assert_eq!(rule_probe_at(&probes, -4.0, y), Some(i), "{}", probes[i].name);
        }
    }
    let (export, _) = rule_board(&probes, "0.15mm");
    let mut p = Project::new("probe");
    let (report, _) = kicad_import::import(&mut p, &export.pcb, &BoardImportOptions::default()).expect("imports");
    let footprints = probes.iter().flat_map(|p| &p.items).filter(|i| matches!(i, Item::Footprint(..))).count();
    assert_eq!(report.footprints, footprints);
    let found = cadlab::drc::check(&p).iter().filter(|d| d.code.starts_with("drc.silk_")).count();
    assert!(found > 20, "{found} silkscreen findings");
    // cadlab's side of the table, without KiCad: every finding lies in a probe's cell.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("kicad_silk_rules_cadlab");
    let ours = rule_results(None, &dir, &probes, "0.15mm");
    let at = |name: &str| &ours[probes.iter().position(|p| p.name == name).expect(name)].1;
    assert_eq!(at("line / fp line, gap 100 um"), &["overlap"]);
    assert!(at("line / fp line, gap 200 um").is_empty());
    assert!(at("board line / board line").is_empty());
    assert_eq!(at("edge / line inside, gap 100 um"), &["edge"]);
    assert!(at("edge / line outside, gap 4000 um").is_empty());
    // With zero rules: overlapping and crossing only, beyond the 2 um tolerance.
    let zero = rule_results(None, &dir, &probes, "0mm");
    let at0 = |name: &str| &zero[probes.iter().position(|p| p.name == name).expect(name)].1;
    assert_eq!(at0("line / fp line, gap -5 um"), &["overlap"]);
    assert!(at0("line / fp line, gap -2 um").is_empty());
    assert!(at0("line / fp line, gap 0 um").is_empty());
    assert_eq!(at0("edge / line inside, gap -50 um"), &["edge"]);
    assert!(at0("edge / line inside, gap 0 um").is_empty());
    assert!(at0("edge / line outside, gap 100 um").is_empty());
    assert_eq!(at0("reference on its own footprint's line"), &["overlap"]);
    assert_eq!(at0("edge / reference 3 mm outside"), &["edge"]);
    assert!(at0("edge / board text 3 mm outside").is_empty());
    if std::env::var_os("CADLAB_SHOW_PROBES").is_some() {
        for ((p, (_, c)), (_, z)) in probes.iter().zip(&ours).zip(&zero) {
            eprintln!("{:<52} {:<12} {}", p.name, z.join("+"), c.join("+"));
        }
    }
}

#[test]
fn kicad_silk_rules() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("kicad_silk_rules");
    let _ = std::fs::remove_dir_all(&dir);
    let probes = rule_probes();
    let zero = rule_results(Some(&cli), &dir.join("clearance-0"), &probes, "0mm");
    let some = rule_results(Some(&cli), &dir.join("clearance-0.15"), &probes, "0.15mm");
    let mut table = format!(
        "{:<52} {:<22} {:<22}  {:<22} {:<22}\n",
        "probe", "KiCad, 0 mm", "cadlab, 0 mm", "KiCad, 0.15 mm", "cadlab, 0.15 mm"
    );
    let mut differ = 0;
    let mut problems = Vec::new();
    let mut seen: std::collections::BTreeSet<&str> = Default::default();
    for (i, pr) in probes.iter().enumerate() {
        let show = |v: &Vec<String>| if v.is_empty() { "-".to_string() } else { v.join("+") };
        let mut notes = Vec::new();
        for (clearance, (kicad, cadlab)) in [("0", &zero[i]), ("0.15", &some[i])] {
            if kicad == cadlab {
                continue;
            }
            match known_rule_difference(&pr.name, clearance, kicad, cadlab) {
                Some(why) => {
                    seen.insert(why);
                    notes.push(why);
                }
                None => problems.push(format!(
                    "{} at {clearance} mm: KiCad {}, cadlab {}",
                    pr.name,
                    show(kicad),
                    show(cadlab)
                )),
            }
        }
        differ += usize::from(!notes.is_empty());
        notes.dedup();
        table.push_str(&format!(
            "{:<52} {:<22} {:<22}  {:<22} {:<22}{}\n",
            pr.name,
            show(&zero[i].0),
            show(&zero[i].1),
            show(&some[i].0),
            show(&some[i].1),
            if notes.is_empty() { String::new() } else { format!("  << {}", notes.join("; ")) }
        ));
        // KiCad takes a circle at its radius: its findings on the circle probes are those of
        // a drawing half the stroke (0.06 mm) farther away.
        if let Some(gap) = circle_gap_um(&pr.name) {
            for (clearance_um, (kicad, _)) in [(0, &zero[i]), (150, &some[i])] {
                let want = gap + 60 < clearance_um;
                if kicad.is_empty() == want {
                    problems.push(format!("{}: KiCad at {clearance_um} um reports {}", pr.name, show(kicad)));
                }
            }
        }
    }
    table.push_str(&format!("\n{differ} of {} probes differ, each for a known reason\n", probes.len()));
    std::fs::write(dir.join("table.txt"), &table).unwrap();
    eprintln!("{table}");
    for why in [CIRCLE, TOLERANCE, OWN_TEXT, MARGIN, OFF_BOARD] {
        if !seen.contains(why) {
            problems.push(format!("no probe differs any more because: {why}"));
        }
    }
    assert!(problems.is_empty(), "silkscreen rules differ from KiCad's:\n{}", problems.join("\n"));
}

const CIRCLE: &str = "KiCad takes a circle at its radius, without its stroke";
const TOLERANCE: &str = "cadlab's 2 um tolerance (KiCad's is under 2 um)";
const OWN_TEXT: &str = "a footprint's texts are drawings of the footprint in cadlab, and its own drawings one item";
const MARGIN: &str = "KiCad also takes drawings on the Margin layer as board edges";
const OFF_BOARD: &str = "cadlab reports a designator wholly off the board (the legend lacks it)";

/// The gap of a probe whose footprint drawing is a circle, in micrometers.
fn circle_gap_um(name: &str) -> Option<i64> {
    let rest = name.strip_prefix("line / fp ring, gap ").or(name.strip_prefix("line / fp disc, gap "))?;
    rest.strip_suffix(" um")?.parse().ok()
}

/// Why the two tools are known to answer a probe differently at a clearance ("0" or "0.15"
/// mm), if they are: the reasons are the documented differences (docs/TESTING.md, D83).
fn known_rule_difference(name: &str, clearance: &str, kicad: &[String], cadlab: &[String]) -> Option<&'static str> {
    let only_kicad = !kicad.is_empty() && cadlab.is_empty();
    let only_cadlab = kicad.is_empty() && !cadlab.is_empty();
    if circle_gap_um(name).is_some() && only_cadlab {
        return Some(CIRCLE);
    }
    let at_tolerance = [
        ("line / fp line, gap -2 um", "0"),
        ("line / fp line, gap 148 um", "0.15"),
        ("edge / line inside, gap 148 um", "0.15"),
    ];
    if only_kicad && at_tolerance.contains(&(name, clearance)) {
        return Some(TOLERANCE);
    }
    if only_kicad && ["fp text on its own footprint's line", "two texts of one footprint"].contains(&name) {
        return Some(OWN_TEXT);
    }
    if only_kicad && name == "silk line across a Margin line" {
        return Some(MARGIN);
    }
    if only_cadlab && name == "edge / reference 3 mm outside" {
        return Some(OFF_BOARD);
    }
    None
}
