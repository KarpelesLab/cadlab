//! IPC-D-356A bare-board electrical test netlist.
//!
//! Fixed-column 80-character records. Header parameters (`P` records): job, units
//! (`CUST 1`: millimeters, coordinates and sizes in 0.001 mm), version and image. Test points:
//!
//! | Columns | Field |
//! |---|---|
//! | 1-3 | operation: `317` plated hole feature, `327` surface feature, `367` non-plated hole |
//! | 4-17 | net name (`N/C` when unconnected; long names via `NNAMEn` parameters) |
//! | 21-26, 27, 28-31 | designator, `-`, pin |
//! | 32 | `M`: mid-net point (vias) |
//! | 33-37, 38 | `D` + hole diameter, `P` plated / `U` unplated |
//! | 39-41 | `A` + access layer: `00` both sides, `01` top, `NN` bottom (last copper layer) |
//! | 42-49, 50-57 | `X`/`Y` + sign + 6 digits |
//! | 58-62, 63-67 | `X`/`Y` + 4 digits: feature size (unrotated) |
//! | 68-71 | `R` + rotation in degrees |
//! | 73-74 | `S` + mask: `0` none over the feature, `3` both sides (tented via) |
//!
//! The standard itself is sold by IPC; this follows the published record layout as used across
//! the industry. Coordinates are board coordinates, aligned with the Gerber files.

use super::{FileKind, Options, OutFile, file_name, pad_rotation};
use crate::board::{self, via_layers};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::BoardSide;
use crate::model::footprint::PadKind;
use crate::units::{Angle, Nm};

/// A record under construction: 80 columns of spaces.
struct Rec([u8; 80]);

impl Rec {
    fn new(op: &str) -> Self {
        let mut r = Rec([b' '; 80]);
        r.put(1, op, 3);
        r
    }

    /// Writes `s` at 1-based column `col`, truncated to `width`.
    fn put(&mut self, col: usize, s: &str, width: usize) {
        for (i, b) in s.bytes().filter(|b| (32..127).contains(b)).take(width).enumerate() {
            self.0[col - 1 + i] = b;
        }
    }

    fn finish(self) -> String {
        String::from_utf8_lossy(&self.0).trim_end().to_string()
    }
}

/// 0.001 mm units, clamped to `digits` digits.
fn units(nm: i64, digits: u32) -> i64 {
    let v = ((nm as f64) / 1000.0).round() as i64;
    v.clamp(-(10_i64.pow(digits) - 1), 10_i64.pow(digits) - 1)
}

fn coord(axis: char, nm: i64) -> String {
    let v = units(nm, 6);
    format!("{axis}{}{:06}", if v < 0 { '-' } else { '+' }, v.abs())
}

struct Feature<'a> {
    op: &'a str,
    net: String,
    refdes: &'a str,
    pin: &'a str,
    mid: bool,
    hole: Option<(Nm, bool)>,
    access: usize,
    at: Point,
    size: (Nm, Nm),
    rotation: Angle,
    mask: u8,
}

fn record(f: &Feature<'_>) -> String {
    let mut r = Rec::new(f.op);
    r.put(4, &f.net, 14);
    r.put(21, f.refdes, 6);
    r.put(27, "-", 1);
    r.put(28, f.pin, 4);
    if f.mid {
        r.put(32, "M", 1);
    }
    if let Some((d, plated)) = f.hole {
        r.put(33, &format!("D{:04}", units(d.0, 4)), 5);
        r.put(38, if plated { "P" } else { "U" }, 1);
    }
    r.put(39, &format!("A{:02}", f.access), 3);
    r.put(42, &coord('X', f.at.x.0), 8);
    r.put(50, &coord('Y', f.at.y.0), 8);
    r.put(58, &format!("X{:04}", units(f.size.0.0, 4)), 5);
    r.put(63, &format!("Y{:04}", units(f.size.1.0, 4)), 5);
    let deg = (f.rotation.normalized().0 as f64 / 1000.0).round() as i64 % 360;
    r.put(68, &format!("R{deg:03}"), 4);
    r.put(73, &format!("S{}", f.mask), 2);
    r.finish()
}

/// Net names that fit the 14-column field as-is (printable ASCII, no spaces).
fn fits(name: &str) -> bool {
    name.len() <= 14 && name.bytes().all(|b| (33..127).contains(&b))
}

/// The IPC-D-356A netlist of the board.
pub fn netlist(p: &Project, o: &Options) -> OutFile {
    let n = p.board().stackup.copper_names().len();
    let mut long: Vec<String> = Vec::new();
    let mut net_field = |net: &Option<String>| -> String {
        match net {
            None => "N/C".into(),
            Some(s) if fits(s) => s.clone(),
            Some(s) => {
                let i = match long.iter().position(|x| x == s) {
                    Some(i) => i,
                    None => {
                        long.push(s.clone());
                        long.len() - 1
                    }
                };
                format!("NNAME{}", i + 1)
            }
        }
    };
    let mut records = Vec::new();
    for pp in board::placed_pads(p) {
        let rot = pad_rotation(&pp, super::fp_rotation(p, &pp.refdes));
        let (op, access) = match pp.pad.kind {
            PadKind::Smd => ("327", if pp.side == BoardSide::Top { 1 } else { n }),
            PadKind::Tht { .. } => ("317", 0),
            PadKind::Npth { .. } => ("367", 0),
        };
        records.push(record(&Feature {
            op,
            net: net_field(&pp.net),
            refdes: &pp.refdes,
            pin: &pp.number,
            mid: false,
            hole: pp.hole,
            access,
            at: pp.center,
            size: pp.pad.shape.size(),
            rotation: rot,
            mask: 0,
        }));
    }
    for v in &p.board().vias {
        let ls = via_layers(p, v);
        let through = ls.len() == n;
        let access = if through {
            0
        } else if ls.first().is_some_and(|l| l == "F.Cu") {
            1
        } else {
            n
        };
        records.push(record(&Feature {
            op: "317",
            net: net_field(&v.net),
            refdes: "VIA",
            pin: "",
            mid: true,
            hole: Some((v.drill, true)),
            access,
            at: v.at,
            size: (v.diameter, v.diameter),
            rotation: Angle::ZERO,
            mask: 3,
        }));
    }
    let mut s = String::new();
    s.push_str("C  IPC-D-356A bare board test netlist\n");
    s.push_str(&format!("C  generated by cadlab {}\n", o.version));
    let job: String = p.manifest().name.chars().filter(|c| (' '..='~').contains(c)).take(60).collect();
    s.push_str(&format!("P  JOB   {job}\n"));
    s.push_str("P  CODE  00\nP  UNITS CUST 1\nP  VER   IPC-D-356A\nP  IMAGE PRIMARY\n");
    for (i, name) in long.iter().enumerate() {
        let name: String = name.chars().filter(|c| (' '..='~').contains(c)).take(60).collect();
        let key = format!("NNAME{}", i + 1);
        s.push_str(&format!("P  {key:<6}{}{name}\n", if key.len() >= 6 { " " } else { "" }));
    }
    for r in records {
        s.push_str(&r);
        s.push('\n');
    }
    s.push_str("999\n");
    OutFile { name: file_name(&p.manifest().name, &FileKind::Ipc356), function: "TestNetlist".into(), content: s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_columns() {
        let r = record(&Feature {
            op: "317",
            net: "GND".into(),
            refdes: "U1",
            pin: "12",
            mid: false,
            hole: Some((Nm(800_000), true)),
            access: 0,
            at: Point::new(Nm(12_345_678), Nm(-2_000_000)),
            size: (Nm(1_500_000), Nm(1_500_000)),
            rotation: Angle::from_deg(90),
            mask: 0,
        });
        assert_eq!(r, "317GND              U1    -12   D0800PA00X+012346Y-002000X1500Y1500R090 S0");
        assert_eq!(&r[20..22], "U1", "designator at column 21");
        assert_eq!(&r[32..33], "D", "hole at column 33");
        assert_eq!(&r[41..49], "X+012346");
        assert_eq!(&r[72..74], "S0");
        assert!(fits("GND") && !fits("A VERY LONG NET NAME") && !fits("with space"));
    }
}
