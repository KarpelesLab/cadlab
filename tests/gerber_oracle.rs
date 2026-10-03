//! gerbv oracle (docs/TESTING.md): every Gerber and XNC file of the tiny test board must parse
//! without diagnostics and render copper (or openings, holes, ink) where expected.
//! Runs only with `CADLAB_ORACLES=1`; gerbv is an external process (DECISIONS D7).
//!
//! gerbv exits 0 even on errors, so the test requires an empty stderr (gerbv reports problems
//! as `WARNING`/`CRITICAL` log lines there) and probes pixels of the rendered PNG.

mod common;
mod fab_support;

use std::path::Path;
use std::process::Command;

use cadlab::fabout::{self, Options};
use common::oracle::{Oracle, require};

/// Render window: origin (-2.54 mm, -2.54 mm), 1.2 × 0.8 inch at 1270 dpi = 50 px/mm.
const PX_PER_MM: f64 = 50.0;
const ORIGIN_MM: f64 = -2.54;
const HEIGHT_MM: f64 = 0.8 * 25.4;

/// (x mm, y mm, expect material).
type Probe = (f64, f64, bool);

struct Image {
    w: u32,
    h: u32,
    data: Vec<u8>,
}

impl Image {
    /// Number of lit pixels.
    fn ink(&self) -> usize {
        self.data.chunks(4).filter(|p| p[0].max(p[1]).max(p[2]) > 127).count()
    }

    /// Brightness (0-255) at board coordinates in mm: the maximum over a 3×3 pixel block.
    fn at(&self, x: f64, y: f64) -> u8 {
        let px = ((x - ORIGIN_MM) * PX_PER_MM).round() as i64;
        let py = ((HEIGHT_MM - (y - ORIGIN_MM)) * PX_PER_MM).round() as i64;
        let mut best = 0;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (u, v) = (px + dx, py + dy);
                assert!(
                    u >= 0 && v >= 0 && (u as u32) < self.w && (v as u32) < self.h,
                    "probe ({x}, {y}) outside image"
                );
                let i = ((v as u32 * self.w + u as u32) * 4) as usize;
                best = best.max(self.data[i].max(self.data[i + 1]).max(self.data[i + 2]));
            }
        }
        best
    }
}

#[cfg(feature = "png")]
fn decode(path: &Path) -> Image {
    let pm = tiny_skia::Pixmap::load_png(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Image { w: pm.width(), h: pm.height(), data: pm.data().to_vec() }
}

#[cfg(not(feature = "png"))]
fn decode(_path: &Path) -> Image {
    panic!("the gerbv oracle needs the `png` feature to decode renders")
}

fn render(gerbv: &Path, file: &Path, out: &Path) -> Image {
    let o = Command::new(gerbv)
        .args(["--export=png", "--dpi=1270", "--origin=-0.1x-0.1", "--window_inch=1.2x0.8", "--border=0"])
        .args(["--background=#000000", "--foreground=#FFFFFFFF"])
        .arg(format!("--output={}", out.display()))
        .arg(file)
        .output()
        .expect("running gerbv");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "gerbv failed on {}: {stderr}", file.display());
    assert!(stderr.lines().all(common::oracle::gerbv_noise), "gerbv diagnostics for {}:\n{stderr}", file.display());
    decode(out)
}

#[test]
fn gerbv_parses_and_renders() {
    let Some(gerbv) = require(Oracle::Gerbv) else { return };
    let (dir, _r, s) = fab_support::tiny_board();
    let p = s.project.as_ref().unwrap();
    let mut files = fabout::all(p, &Options::default());
    files.extend(fabout::drill_gerbers(p, &Options::default()));
    let out = dir.path().join("fab");
    std::fs::create_dir_all(&out).unwrap();
    for f in &files {
        std::fs::write(out.join(&f.name), &f.content).unwrap();
    }
    // (file, [(x mm, y mm, expect material)]).
    let probes: &[(&str, &[Probe])] = &[
        (
            "tiny-F_Cu.gbr",
            &[
                (10.8475, 8.45, true), // U1 pad 1 (rounded rectangle macro)
                (6.68, 10.18, true),   // C1 pad 1 at 45° (rotated macro)
                (15.5, 9.0, true),     // via pad
                (14.3, 8.72, true),    // track
                (1.73, 6.0, true),     // J1 pin 1 (square)
                (12.0, 7.5, false),    // between U1 pads
                (20.0, 12.0, false),
            ],
        ),
        (
            "tiny-B_Cu.gbr",
            &[
                (8.0, 4.0, true),       // arc track apex
                (8.0, 3.0, false),      // inside the arc
                (18.0, 9.4525, true),   // C2 pad 1 (bottom)
                (4.27, 6.0, true),      // J1 pin 2 (round)
                (10.8475, 8.45, false), // U1 is on top
            ],
        ),
        ("tiny-F_Mask.gbr", &[(10.8475, 8.45, true), (1.73, 6.0, true), (15.5, 9.0, false)]),
        ("tiny-B_Mask.gbr", &[(18.0, 9.4525, true), (4.27, 6.0, true), (6.0, 3.0, false)]),
        ("tiny-F_Paste.gbr", &[(10.8475, 8.45, true), (1.73, 6.0, false)]),
        ("tiny-B_Paste.gbr", &[(18.0, 8.5475, true), (10.8475, 8.45, false)]),
        ("tiny-F_SilkS.gbr", &[(9.5, 8.45, true), (10.8475, 8.45, false), (12.0, 8.45, true), (13.152, 8.45, false)]),
        ("tiny-B_SilkS.gbr", &[(10.8475, 8.45, false), (1.73, 6.0, false)]),
        (
            "tiny-Edge_Cuts.gbr",
            &[(12.5, 0.0, true), (0.0, 7.5, true), (25.0, 7.5, true), (12.5, 7.5, false), (0.05, 0.05, false)],
        ),
        ("tiny-F_Component.gbr", &[(12.0, 7.5, true), (3.0, 6.0, true), (18.0, 9.0, false)]),
        ("tiny-B_Component.gbr", &[(18.0, 9.0, true), (12.0, 7.5, false)]),
        ("tiny-PTH.drl", &[(15.5, 9.0, true), (6.0, 3.0, true), (1.73, 6.0, true), (3.0, 6.0, false)]),
        ("tiny-PTH-drl.gbr", &[(15.5, 9.0, true), (4.27, 6.0, true), (3.0, 6.0, false)]),
    ];
    for f in &files {
        let path = out.join(&f.name);
        if f.name.ends_with(".csv") || f.name.ends_with(".d356") {
            continue;
        }
        let img = render(&gerbv, &path, &out.join(format!("{}.png", f.name)));
        let Some((_, list)) = probes.iter().find(|(n, _)| *n == f.name) else { panic!("no probes for {}", f.name) };
        assert!(img.ink() > 0, "{}: empty render", f.name);
        for &(x, y, on) in *list {
            let v = img.at(x, y);
            assert_eq!(
                v > 127,
                on,
                "{} at ({x}, {y}): brightness {v}, expected {}",
                f.name,
                if on { "image" } else { "empty" }
            );
        }
    }
}
