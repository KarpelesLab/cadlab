//! IPC-7351B land pattern calculation.
//!
//! For a lead row, with L = lead span (toe to toe), T = terminal length, W = terminal width:
//!
//! ```text
//! Z = Lmin + 2·Jt + √(CL² + F² + P²)        outer pad edge to outer pad edge
//! G = Smax − 2·Jh − √(CS² + F² + P²)        inner pad edge to inner pad edge
//! X = Wmin + 2·Js + √(CW² + F² + P²)        pad width
//! ```
//!
//! where Jt/Jh/Js are the toe/heel/side solder fillet goals for the lead family and density
//! level, C* are tolerance ranges, F is the board fabrication tolerance and P the placement
//! tolerance. S = L − 2T is the heel-to-heel span; its tolerance is taken as the RMS value
//! CS = √(CL² + 2·CT²) (the two terminals vary independently), centered on the worst-case range.

use crate::units::Nm;

use super::{Density, Dim, GenOptions};

/// Fillet goals and courtyard excess for one lead family, per density level `[Most, Nominal, Least]`, in µm.
///
/// Values follow the IPC-7351B fillet tables. **Check them against the standard before relying on
/// them for production**; they are kept in this one table so that is a single review.
pub(crate) struct Fillets {
    pub toe: [i64; 3],
    pub heel: [i64; 3],
    pub side: [i64; 3],
    pub courtyard: [i64; 3],
}

/// Rectangular / square-end chip components, 0603 (1608 metric) and larger.
pub(crate) const CHIP: Fillets = Fillets {
    toe: [550, 350, 150],
    heel: [0, 0, 0],
    side: [50, 0, -50],
    courtyard: [500, 250, 100],
};

/// Chip components smaller than 0603 (0402, 0201, 01005).
pub(crate) const CHIP_SMALL: Fillets = Fillets {
    toe: [300, 200, 100],
    heel: [0, 0, 0],
    side: [50, 0, -50],
    courtyard: [200, 150, 100],
};

/// Gull-wing leads, pitch > 0.625 mm.
pub(crate) const GULLWING: Fillets = Fillets {
    toe: [550, 350, 150],
    heel: [450, 350, 250],
    side: [50, 30, 10],
    courtyard: [500, 250, 100],
};

/// Gull-wing leads, pitch ≤ 0.625 mm.
pub(crate) const GULLWING_FINE: Fillets = Fillets {
    toe: [550, 350, 150],
    heel: [450, 350, 250],
    side: [10, -20, -40],
    courtyard: [500, 250, 100],
};

/// No-lead packages (QFN, DFN/SON): terminals flush with the body edge.
pub(crate) const NOLEAD: Fillets = Fillets {
    toe: [400, 300, 200],
    heel: [0, 0, 0],
    side: [-40, -40, -40],
    courtyard: [500, 250, 100],
};

impl Fillets {
    fn pick(v: [i64; 3], d: Density) -> Nm {
        Nm::from_um(v[d as usize])
    }

    pub fn courtyard(&self, d: Density) -> Nm {
        Self::pick(self.courtyard, d)
    }
}

/// Pads for one lead row: pad length along the lead axis, pad width, and the distance from the
/// package center to the pad center.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowPads {
    pub length: Nm,
    pub width: Nm,
    pub center: Nm,
}

fn rss(vals: &[Nm]) -> Nm {
    let s: f64 = vals.iter().map(|v| (v.0 as f64).powi(2)).sum();
    Nm(s.sqrt().ceil() as i64)
}

fn round_up(v: Nm, grid: Nm) -> Nm {
    if grid.0 <= 1 {
        return v;
    }
    Nm(v.0.div_euclid(grid.0) * grid.0 + if v.0.rem_euclid(grid.0) == 0 { 0 } else { grid.0 })
}

fn round_down(v: Nm, grid: Nm) -> Nm {
    if grid.0 <= 1 {
        return v;
    }
    Nm(v.0.div_euclid(grid.0) * grid.0)
}

/// Computes the pads of a lead row.
pub(crate) fn row_pads(span: Dim, terminal: Dim, width: Dim, f: &Fillets, opts: &GenOptions) -> RowPads {
    let d = opts.density;
    let (jt, jh, js) = (
        Fillets::pick(f.toe, d),
        Fillets::pick(f.heel, d),
        Fillets::pick(f.side, d),
    );
    let (fab, place) = (opts.fab_tolerance, opts.placement_tolerance);

    let cl = span.tol();
    let cw = width.tol();
    let ct = terminal.tol();
    let s_min = span.min - terminal.max * 2;
    let s_max = span.max - terminal.min * 2;
    let cs_worst = s_max - s_min;
    let cs = rss(&[cl, ct, ct]);
    let s_max_rms = s_max - Nm((cs_worst.0 - cs.0).max(0) / 2);

    let z = span.min + jt * 2 + rss(&[cl, fab, place]);
    let g = s_max_rms - jh * 2 - rss(&[cs, fab, place]);
    let x = width.min + js * 2 + rss(&[cw, fab, place]);

    let z = round_up(z, opts.rounding);
    let mut g = round_down(g, opts.rounding);
    // Pads of opposite rows must not merge or overlap.
    if g < opts.min_pad_gap {
        g = opts.min_pad_gap;
    }
    let x = round_up(x, opts.rounding);
    let length = Nm((z.0 - g.0) / 2);
    let center = Nm((z.0 + g.0) / 4);
    RowPads {
        length,
        width: x,
        center,
    }
}

/// Limits a pad width so neighbouring pads in a row keep `min_gap` between them.
pub(crate) fn clamp_to_pitch(width: Nm, pitch: Nm, min_gap: Nm) -> Nm {
    width.min(pitch - min_gap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::landpattern::Dim;

    fn mm(v: f64) -> Nm {
        Nm((v * 1e6).round() as i64)
    }

    fn dim(min: f64, max: f64) -> Dim {
        Dim {
            min: mm(min),
            max: mm(max),
        }
    }

    #[test]
    fn soic8_nominal() {
        // JEDEC MS-012: span 5.80–6.20, terminal 0.40–1.27, lead width 0.31–0.51.
        let r = row_pads(
            dim(5.8, 6.2),
            dim(0.4, 1.27),
            dim(0.31, 0.51),
            &GULLWING,
            &GenOptions::default(),
        );
        // Z = 6.91 (rounded up), G = 2.98 (rounded down): the center falls between grid steps.
        assert_eq!(
            r,
            RowPads {
                length: mm(1.965),
                width: mm(0.58),
                center: mm(2.4725)
            }
        );
    }

    #[test]
    fn chip_0402_nominal() {
        let r = row_pads(
            dim(0.95, 1.05),
            dim(0.15, 0.35),
            dim(0.45, 0.55),
            &CHIP_SMALL,
            &GenOptions::default(),
        );
        assert_eq!(
            r,
            RowPads {
                length: mm(0.565),
                width: mm(0.57),
                center: mm(0.4525)
            }
        );
    }

    #[test]
    fn rounding() {
        assert_eq!(round_up(Nm(10_001), Nm(10_000)), Nm(20_000));
        assert_eq!(round_up(Nm(10_000), Nm(10_000)), Nm(10_000));
        assert_eq!(round_down(Nm(19_999), Nm(10_000)), Nm(10_000));
        assert_eq!(round_down(Nm(-1), Nm(10_000)), Nm(-10_000));
    }
}
