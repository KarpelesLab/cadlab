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
/// IPC-7351B itself is not freely readable, so every row below names what it was compared with
/// and how far that goes (D78, docs/PARTS.md "Sources of the fillet tables"). The sources:
///
/// - **IPC-img**: page images of IPC-7351B (June 2010) Tables 3-2, 3-3, 3-5 and 3-6, posted on the
///   PCB Libraries forum by the author of IPC's land pattern calculator (topics 2587 and 2588).
/// - **IPC-3-11**: a page image of the table "Inward Flat Ribbon L and Gull-Wing Leads (Tantalum
///   Capacitors)" of the earlier IPC-7351 (there Table 3-11; Table 3-13 in revision B), posted in
///   forum topic 1314.
/// - **LE-2016**: PCB Libraries, "Library Expert Surface Mount Families" (updated 2016-02-25,
///   archived 2016-04-16): the per-family tables of the tool that was IPC's calculator. It prints
///   0.00 where IPC has a negative goal (seen on the rows that IPC-img and IPC-3-11 also give)
///   and 0.12 where IPC has a 0.1 mm courtyard (IPC-3-11, Hausherr), so it cannot confirm
///   those cells.
/// - **KEMET**: KEMET T491 datasheet (T2005_T491, 2026-07-08), Table 2 "Land Dimensions/Courtyard":
///   lands for density levels A/B/C with reference to IPC-7351, from which the goals follow.
/// - **Hausherr**: T. Hausherr, "PCB Design Perfection Starts in the CAD Library", parts 6, 9 and
///   12 (Mentor Graphics blog, 2010–2011, archived): courtyard excess 0.5/0.25/0.1 mm and the BGA
///   tables.
/// - **TI**: Texas Instruments SPRACA4 "AM57xx BGA PCB Design" (2017), Table 1.
///
/// The round-off printed with these tables in LE-2016 is 0.01 mm, the generator's default.
pub(crate) struct Fillets {
    pub toe: [i64; 3],
    pub heel: [i64; 3],
    pub side: [i64; 3],
    pub courtyard: [i64; 3],
}

/// Rectangular / square-end chip components, 0603 (1608 metric) and larger. IPC-7351B Table 3-5:
/// equal to IPC-img.
pub(crate) const CHIP: Fillets =
    Fillets { toe: [550, 350, 150], heel: [0, 0, 0], side: [50, 0, -50], courtyard: [500, 250, 100] };

/// Chip components smaller than 0603 (0402, 0201, 01005). IPC-7351B Table 3-6: equal to IPC-img.
pub(crate) const CHIP_SMALL: Fillets =
    Fillets { toe: [300, 200, 100], heel: [0, 0, 0], side: [50, 0, -50], courtyard: [200, 150, 100] };

/// Gull-wing leads, pitch > 0.625 mm. IPC-7351B Table 3-2: equal to IPC-img.
///
/// Also used for the leads and the tab of SOT-223/DPAK/D2PAK. IPC-7351B has a separate
/// Table 3-14 "Flat Lug Leads"; LE-2016 prints it as one table with these same numbers
/// ("Gull-Wing and Flat Lug Leads": 0.55/0.35/0.15, 0.45/0.35/0.25, 0.05/0.03/0.01). One source.
pub(crate) const GULLWING: Fillets =
    Fillets { toe: [550, 350, 150], heel: [450, 350, 250], side: [50, 30, 10], courtyard: [500, 250, 100] };

/// Gull-wing leads, pitch ≤ 0.625 mm. IPC-7351B Table 3-3: equal to IPC-img.
pub(crate) const GULLWING_FINE: Fillets =
    Fillets { toe: [550, 350, 150], heel: [450, 350, 250], side: [10, -20, -40], courtyard: [500, 250, 100] };

/// No-lead packages (QFN, DFN/SON): terminals flush with the body edge.
pub(crate) const NOLEAD: Fillets =
    Fillets { toe: [400, 300, 200], heel: [0, 0, 0], side: [-40, -40, -40], courtyard: [500, 250, 100] };

/// Flat (ribbon) leads that leave the body without bending down: SOD-123F, SOD-523 (IPC `SODFL`).
/// IPC-7351B Table 3-22 "Small Outline Components, Flat Lead".
///
/// LE-2016 gives toe 0.30/0.20/0.10, heel 0, side 0.05/0.00/0.00, courtyard 0.20/0.15/0.12: equal
/// here except the Least side goal (−0.05 here) and the Least courtyard (0.10 here), the two cells
/// that source cannot confirm. **Those two are unchecked**; everything else has this one source.
pub(crate) const FLAT: Fillets =
    Fillets { toe: [300, 200, 100], heel: [0, 0, 0], side: [50, 0, -50], courtyard: [200, 150, 100] };

/// Molded body with inward L-bend (J-like) leads folded under the body: SMA/SMB/SMC (DO-214),
/// molded tantalum. IPC-7351B Table 3-13 "Inward Flat Ribbon L-Leads". IPC calls the fillet at
/// the outer bend the *heel* (0.8/0.5/0.2, "to find Z") and the one at the lead tip under the body
/// the *toe* (0.25/0.15/0.07, "to find G"); here `toe` is always the outer (Z) goal and `heel` the
/// inner (G) goal, so the two IPC rows appear swapped.
///
/// Checked, two independent sources: IPC-3-11 has exactly these goals, side 0.01/−0.05/−0.10
/// and courtyard 0.5/0.25/0.1; the KEMET lands are reproduced to 0.02 mm by these goals with
/// F = P = 0.1 mm (test `molded_tantalum_matches_kemet_t491`), the negative side goals included.
/// LE-2016 agrees on toe and heel. KEMET's Least courtyard is 0.12 mm, not 0.1.
pub(crate) const MOLDED: Fillets =
    Fillets { toe: [800, 500, 200], heel: [250, 150, 70], side: [10, -50, -100], courtyard: [500, 250, 100] };

/// Cylindrical end caps: MELF, MiniMELF (DO-213AB/AA). IPC-7351B Table 3-7.
///
/// One source: LE-2016 has toe 0.60/0.40/0.20, heel 0.20/0.10/0.02, side 0.10/0.05/0.01 and
/// courtyard 0.50/0.25/0.12 (the 2024 edition of that document has the same goals). The Least
/// courtyard of 0.1 mm is the general IPC-7351B value from Hausherr. Not confirmed by a second,
/// independent source: Diodes' MELF lands (AP02001) fall between the Nominal and Most patterns.
pub(crate) const MELF: Fillets =
    Fillets { toe: [600, 400, 200], heel: [200, 100, 20], side: [100, 50, 10], courtyard: [500, 250, 100] };

/// BGA with collapsing balls, IPC-7351B Table 14-5 "Land Approximation (mm) for Collapsible Solder
/// Balls": `(nominal ball diameter, nominal land diameter)` in µm. The land is the ball reduced
/// by 25 % (0.55 mm and larger), 20 % (0.25–0.50 mm) or 15 % (smaller), rounded to 0.05 mm in
/// the first two groups. It does not depend on the density level chosen for the footprint: the
/// table itself assigns level A, B or C by ball size.
///
/// Checked, two independent sources, for 0.75 to 0.20 mm: TI Table 1 and Hausherr part 9
/// Table 1 have the same eleven rows. The 0.17 and 0.15 mm rows are in Hausherr only.
/// Both print a "land variation" range around the nominal land; TI says to design to the nominal
/// diameter, Hausherr that IPC's calculator takes the upper end. The nominal is used here.
pub(crate) const BGA_COLLAPSING: &[(i64, i64)] = &[
    (750, 550),
    (650, 500),
    (600, 450),
    (550, 400),
    (500, 400),
    (450, 350),
    (400, 300),
    (350, 300),
    (300, 250),
    (250, 200),
    (200, 170),
    (170, 150),
    (150, 130),
];

/// Reduction for ball sizes between the rows of [`BGA_COLLAPSING`]: `(smallest nominal ball
/// diameter in µm, reduction in %)`, largest balls first. A ball between two groups takes the
/// reduction of the smaller group (cadlab's choice; the table has no rule for it).
pub(crate) const BGA_REDUCTION: &[(i64, i64)] = &[(550, 25), (250, 20), (0, 15)];

/// BGA courtyard excess `[Most, Nominal, Least]`, µm.
///
/// **Unchecked.** Hausherr parts 6 and 9 give 2.0/1.0/0.5 mm for levels A/B/C, one author, and
/// there the level follows the ball size (above 0.50 mm, 0.50–0.25 mm, below 0.25 mm) so that
/// rework tools fit; here it follows the density level chosen for the footprint. LE-2016 has
/// 1.0/0.5/0.25.
pub(crate) const BGA_COURTYARD: [i64; 3] = [2000, 1000, 500];

/// Land diameter for a collapsing BGA ball of nominal diameter `ball`: the [`BGA_COLLAPSING`] row
/// of a standard ball size, else the reduction of its group rounded like the table (to 0.05 mm
/// from 0.25 mm up, to 0.01 mm below).
pub(crate) fn bga_land(ball: Nm) -> Nm {
    if let Some((_, land)) = BGA_COLLAPSING.iter().find(|(b, _)| ball.0 == b * 1000) {
        return Nm::from_um(*land);
    }
    let pct = BGA_REDUCTION.iter().find(|(min, _)| ball.0 >= min * 1000).map_or(15, |r| r.1);
    let d = ball.0 * (100 - pct) / 100;
    let g = if ball.0 >= 250_000 { 50_000 } else { 10_000 };
    Nm((d + g / 2).div_euclid(g) * g)
}

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

/// Z (outer), G (inner, not clamped) and X of a lead row, rounded to the grid.
fn zgx(span: Dim, terminal: Dim, width: Dim, f: &Fillets, opts: &GenOptions) -> (Nm, Nm, Nm) {
    let d = opts.density;
    let (jt, jh, js) = (Fillets::pick(f.toe, d), Fillets::pick(f.heel, d), Fillets::pick(f.side, d));
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

    (round_up(z, opts.rounding), round_down(g, opts.rounding), round_up(x, opts.rounding))
}

/// Computes the pads of a lead row.
pub(crate) fn row_pads(span: Dim, terminal: Dim, width: Dim, f: &Fillets, opts: &GenOptions) -> RowPads {
    let (z, mut g, x) = zgx(span, terminal, width, f, opts);
    // Pads of opposite rows must not merge or overlap.
    if g < opts.min_pad_gap {
        g = opts.min_pad_gap;
    }
    let length = Nm((z.0 - g.0) / 2);
    let center = Nm((z.0 + g.0) / 4);
    RowPads { length, width: x, center }
}

/// Pad on one side of an asymmetric package (SOT-223, DPAK): edges as distances from the origin
/// along the lead axis, and pad width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SidePad {
    /// Outer pad edge (toe side).
    pub outer: Nm,
    /// Inner pad edge; may be negative when the pad extends past the origin (DPAK tab).
    pub inner: Nm,
    pub width: Nm,
}

/// Pad for one side whose terminal toe lies `toe` from the origin: the Z/G/X equations applied to
/// the symmetric package of span `2·toe`, without the minimum-gap clamp (the caller checks the gap
/// against the opposite side, which may differ).
pub(crate) fn side_pad(toe: Dim, terminal: Dim, width: Dim, f: &Fillets, opts: &GenOptions) -> SidePad {
    let span = Dim { min: toe.min * 2, max: toe.max * 2 };
    let (z, g, x) = zgx(span, terminal, width, f, opts);
    SidePad { outer: Nm(z.0 / 2), inner: Nm(g.0.div_euclid(2)), width: x }
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
        Dim { min: mm(min), max: mm(max) }
    }

    #[test]
    fn soic8_nominal() {
        // JEDEC MS-012: span 5.80–6.20, terminal 0.40–1.27, lead width 0.31–0.51.
        let r = row_pads(dim(5.8, 6.2), dim(0.4, 1.27), dim(0.31, 0.51), &GULLWING, &GenOptions::default());
        // Z = 6.91 (rounded up), G = 2.98 (rounded down): the center falls between grid steps.
        assert_eq!(r, RowPads { length: mm(1.965), width: mm(0.58), center: mm(2.4725) });
    }

    #[test]
    fn chip_0402_nominal() {
        let r = row_pads(dim(0.95, 1.05), dim(0.15, 0.35), dim(0.45, 0.55), &CHIP_SMALL, &GenOptions::default());
        assert_eq!(r, RowPads { length: mm(0.565), width: mm(0.57), center: mm(0.4525) });
    }

    #[test]
    fn sot223_side_matches_symmetric_row() {
        // A symmetric package computed side by side gives the same pads as the row equations.
        let opts = GenOptions::default();
        let r = row_pads(dim(6.7, 7.3), dim(0.7, 1.1), dim(0.6, 0.88), &GULLWING, &opts);
        let s = side_pad(dim(3.35, 3.65), dim(0.7, 1.1), dim(0.6, 0.88), &GULLWING, &opts);
        assert_eq!(s.outer - s.inner, r.length);
        assert_eq!(Nm((s.outer.0 + s.inner.0) / 2), r.center);
        assert_eq!(s.width, r.width);
    }

    /// IPC-7351B Table 14-5 as printed by TI SPRACA4 Table 1 ("IPC-7351A for NSMD Pads") and by
    /// Hausherr's part 9 Table 1 (0.75 to 0.20 mm in both; 0.17 and 0.15 mm in the latter only).
    #[test]
    fn bga_land_table() {
        // (ball, land, reduction in %, rounding grid), µm.
        let rows = [
            (750, 550, 25, 50),
            (650, 500, 25, 50),
            (600, 450, 25, 50),
            (550, 400, 25, 50),
            (500, 400, 20, 50),
            (450, 350, 20, 50),
            (400, 300, 20, 50),
            (350, 300, 20, 50),
            (300, 250, 20, 50),
            (250, 200, 20, 50),
            (200, 170, 15, 10),
            (170, 150, 15, 10),
            (150, 130, 15, 10),
        ];
        assert_eq!(rows.len(), BGA_COLLAPSING.len());
        for (ball, land, pct, grid) in rows {
            assert_eq!(bga_land(Nm::from_um(ball)), Nm::from_um(land), "ball {ball} µm");
            // The table is the stated reduction rounded to the grid; only the 0.17 mm row is
            // not (15 % gives 0.1445).
            let computed = (ball * (100 - pct) * 10 + grid * 500).div_euclid(grid * 1000) * grid;
            if ball != 170 {
                assert_eq!(computed, land, "ball {ball} µm");
            }
        }
        // TI SPRACA4: the AM572x has 0.5 mm balls, "the recommendation for the ball pad size is 0.4mm".
        assert_eq!(bga_land(mm(0.5)), mm(0.4));
        // Ball sizes between the rows: the reduction of the group, rounded the same way.
        assert_eq!(bga_land(mm(0.42)), mm(0.35)); // 20 %: 0.336
        assert_eq!(bga_land(mm(0.52)), mm(0.40)); // 20 %: 0.416
        assert_eq!(bga_land(mm(0.70)), mm(0.55)); // 25 %: 0.525
        assert_eq!(bga_land(mm(0.22)), mm(0.19)); // 15 %: 0.187
        // Always 10 to 30 % smaller than the ball. (Not monotonic at the group steps: the table
        // itself has 0.40 for both 0.50 and 0.55 mm.)
        for ball in (100..=1000).step_by(10).map(Nm::from_um) {
            let land = bga_land(ball);
            assert!(land.0 * 10 <= ball.0 * 9 && land.0 * 10 >= ball.0 * 7, "ball {ball} → {land}");
        }
    }

    /// The fillet rows as compared with their sources (see [`Fillets`]): a change here needs a
    /// source.
    #[test]
    fn fillet_rows_pinned() {
        let row = |f: &Fillets| (f.toe, f.heel, f.side, f.courtyard);
        // IPC-7351B Tables 3-5, 3-6, 3-2 and 3-3 (page images).
        assert_eq!(row(&CHIP), ([550, 350, 150], [0, 0, 0], [50, 0, -50], [500, 250, 100]));
        assert_eq!(row(&CHIP_SMALL), ([300, 200, 100], [0, 0, 0], [50, 0, -50], [200, 150, 100]));
        assert_eq!(row(&GULLWING), ([550, 350, 150], [450, 350, 250], [50, 30, 10], [500, 250, 100]));
        assert_eq!(row(&GULLWING_FINE), ([550, 350, 150], [450, 350, 250], [10, -20, -40], [500, 250, 100]));
        // Table 3-22, flat lead: Library Expert 2016 (Least side and Least courtyard unconfirmed).
        assert_eq!(row(&FLAT), ([300, 200, 100], [0, 0, 0], [50, 0, -50], [200, 150, 100]));
        // Table 3-13, molded body: IPC-7351 page image and KEMET T491 (toe and heel swapped, see MOLDED).
        assert_eq!(row(&MOLDED), ([800, 500, 200], [250, 150, 70], [10, -50, -100], [500, 250, 100]));
        // Table 3-7, MELF: Library Expert 2016.
        assert_eq!(row(&MELF), ([600, 400, 200], [200, 100, 20], [100, 50, 10], [500, 250, 100]));
        assert_eq!(BGA_REDUCTION, &[(550, 25), (250, 20), (0, 15)]);
        assert_eq!(BGA_COURTYARD, [2000, 1000, 500]);
    }

    #[test]
    fn rounding() {
        assert_eq!(round_up(Nm(10_001), Nm(10_000)), Nm(20_000));
        assert_eq!(round_up(Nm(10_000), Nm(10_000)), Nm(10_000));
        assert_eq!(round_down(Nm(19_999), Nm(10_000)), Nm(10_000));
        assert_eq!(round_down(Nm(-1), Nm(10_000)), Nm(-10_000));
    }
}
