//! Common package names → [`PackageSpec`]s with typical (JEDEC / EIA) dimensions.
//!
//! These are *typical* dimensions for each package family. When a datasheet gives different
//! values, describe the package with an explicit [`PackageSpec`] instead.
//!
//! Accepted names (case-insensitive, `-`, `_` and spaces interchangeable):
//! - chips: `0201` `0402` `0603` `0805` `1206` `1210` `1812` `2010` `2512`, or metric `1005M`,
//!   `1608metric`, ...
//! - `SOT-23` (3 pins), `SOT-23-5`, `SOT-23-6`
//! - `SOIC-8/14/16`, `SOIC-16W/20W/24W/28W`, `TSSOP-8..28`, `MSOP-8`, `MSOP-10`
//! - `LQFP-32/44/48/64/100/144` (also `TQFP-`)
//! - `QFN-32 5x5mm P0.5mm EP3.1mm`, `DFN-8 3x3mm P0.65mm EP1.6x2.4mm` (parametric; optional
//!   `L0.4mm` terminal length, `b0.25mm` width, `H0.8mm` height)
//! - `PinHeader 1x04`, `PinHeader 2x05 P2.54mm`
//! - `SOT-223` (also `SOT-223-3/-4`, `TO-261`, `TO-261AA`): 3 leads + tab (pad 4)
//! - `DPAK` / `TO-252` (`TO-252-2/-3`, `TO-252AA`), `D2PAK` / `TO-263` (`TO-263-2/-3`, `TO-263AB`,
//!   `DDPAK`): leads 1 and 3, tab = pad 2 (the cut middle lead it is connected to)
//! - `SOD-123`, `SOD-323` (`SC-76`), `SOD-523` (`SC-79`), `SOD-123F` (`SOD-123FL`)
//! - `MINIMELF` (`DO-213AA`, `SOD-80`), `MELF` (`DO-213AB`); `SMA`, `SMB`, `SMC`
//!   (`DO-214AC/AA/AB`). Diodes, pad 1 = cathode, whatever `kind` is passed.
//! - `DIP-N` / `PDIP-N` (300 mil rows), `DIP-NW` (600 mil rows)
//! - `BGA-64 8x8 P0.8mm 6x6mm` (parametric): pin count, ball grid `columns x rows`, pitch, body
//!   size; optional `B0.45mm` ball diameter (default from the pitch), `H1.2mm` height (default
//!   1.2 mm), `VOID4x4` depopulated center block (`columns x rows`). The pin count must equal the
//!   populated balls.

use super::{ChipKind, Dim, ExposedPad, PackageSpec, SodLead};
use crate::units::Nm;

fn um(v: i64) -> Nm {
    Nm::from_um(v)
}

fn range(min_um: i64, max_um: i64) -> Dim {
    Dim { min: um(min_um), max: um(max_um) }
}

fn pm(nom_um: i64, tol_um: i64) -> Dim {
    Dim::plus_minus(um(nom_um), um(tol_um))
}

/// EIA chip size: (imperial, metric, L, L tol, W, W tol, T min, T max, height resistor, height other), µm.
type ChipSize = (&'static str, &'static str, i64, i64, i64, i64, i64, i64, i64, i64);

const CHIPS: &[ChipSize] = &[
    ("0201", "0603", 600, 30, 300, 30, 100, 200, 260, 330),
    ("0402", "1005", 1000, 50, 500, 50, 150, 350, 400, 550),
    ("0603", "1608", 1600, 100, 800, 100, 200, 500, 550, 870),
    ("0805", "2012", 2000, 100, 1250, 100, 250, 650, 700, 1350),
    ("1206", "3216", 3200, 150, 1600, 150, 250, 750, 700, 1800),
    ("1210", "3225", 3200, 200, 2500, 200, 250, 750, 700, 2700),
    ("1812", "4532", 4500, 200, 3200, 200, 250, 850, 700, 2000),
    ("2010", "5025", 5000, 200, 2500, 200, 350, 850, 700, 700),
    ("2512", "6332", 6300, 200, 3200, 200, 350, 850, 700, 700),
];

/// Package names that share one land pattern, canonical name first. Distributors use all of
/// them (DigiKey lists AP2112K as `SOT-25`). Thin variants (TSOT) share the footprint.
const ALIASES: &[&[&str]] = &[
    &["SOT-23", "SOT-23-3", "TO-236", "TO-236-3", "TO-236AB"],
    &["SOT-23-5", "SOT-25", "SOT-753", "SC-74A", "TSOT-23-5", "SOT-23-5L"],
    &["SOT-23-6", "SOT-26", "SOT-457", "SC-74", "TSOT-23-6", "SOT-23-6L"],
    &["SOT-223", "SOT-223-3", "SOT-223-4", "TO-261", "TO-261-4", "TO-261AA"],
    &["TO-252", "DPAK", "TO-252-2", "TO-252-3", "TO-252AA", "DPAK-3"],
    &["TO-263", "D2PAK", "TO-263-2", "TO-263-3", "TO-263AB", "DDPAK", "D2PAK-3"],
    &["SOD-123"],
    &["SOD-123F", "SOD-123FL"],
    &["SOD-323", "SC-76"],
    &["SOD-523", "SC-79"],
    &["MINIMELF", "MINI-MELF", "DO-213AA", "SOD-80"],
    &["MELF", "DO-213AB"],
    &["SMA", "DO-214AC"],
    &["SMB", "DO-214AA"],
    &["SMC", "DO-214AB"],
];

fn alnum_key(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// Canonical name of a package: `SOT-25` → `SOT-23-5`. Unknown names are returned as given.
pub fn canonical(name: &str) -> &str {
    let k = alnum_key(name);
    ALIASES.iter().find(|g| g.iter().any(|a| alnum_key(a) == k)).map_or(name, |g| g[0])
}

/// Comparison key for package names: aliases and spelling variants compare equal
/// (`SOT-25`, `sot23-5`, `SOT-23-5`).
pub fn package_key(name: &str) -> String {
    alnum_key(canonical(name))
}

/// Imperial chip codes, for recognizing package tokens in part specs.
pub fn chip_codes() -> impl Iterator<Item = &'static str> {
    CHIPS.iter().map(|c| c.0)
}

/// Imperial chip code for a metric one: `1005` → `0402` (EIA sizes the generator knows).
pub fn imperial_from_metric(metric: &str) -> Option<&'static str> {
    CHIPS.iter().find(|c| c.1 == metric || (c.0 == "2512" && metric == "6432")).map(|c| c.0)
}

/// Normalizes a name: uppercase, separators to `-`.
fn norm(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c == '_' || c == ' ' || c == '-' {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c.to_ascii_uppercase());
        }
    }
    out.trim_matches('-').to_string()
}

/// Parses a length token that may omit `mm`.
fn len_mm(s: &str) -> Option<Nm> {
    let s = s.trim().to_ascii_lowercase();
    let s = s.strip_suffix("mm").unwrap_or(&s);
    Nm::parse(&format!("{s}mm")).ok()
}

/// Parses a package name. `kind` sets the chip body type for two-terminal chips.
pub fn parse(name: &str, kind: ChipKind) -> Result<PackageSpec, String> {
    let n = norm(name);
    let unknown = || {
        format!(
            "unknown package `{name}`; known: chip sizes (0402, 0603, ...), SOT-23[-5|-6], SOT-223, DPAK, D2PAK, \
             SOIC-N, SOIC-NW, TSSOP-N, MSOP-8/10, LQFP-N, SOD-123/123F/323/523, MINIMELF, MELF, SMA, SMB, SMC, \
             DIP-N, DIP-NW, `QFN-32 5x5mm P0.5mm EP3.1mm`, `DFN-8 3x3mm P0.65mm EP1.6x2.4mm`, \
             `BGA-64 8x8 P0.8mm 6x6mm`, `PinHeader 1x04`; or give explicit dimensions"
        )
    };

    // Chips: imperial, or metric with an M/METRIC suffix.
    let chip_code = n.strip_prefix('R').or_else(|| n.strip_prefix('C')).or_else(|| n.strip_prefix('L')).unwrap_or(&n);
    let metric = chip_code
        .strip_suffix("-METRIC")
        .or_else(|| chip_code.strip_suffix("METRIC"))
        .or_else(|| chip_code.strip_suffix('M'));
    if let Some(c) = CHIPS.iter().find(|c| match metric {
        Some(m) => c.1 == m || (c.0 == "2512" && m == "6432"),
        None => c.0 == chip_code,
    }) {
        let (_, _, l, lt, w, wt, tmin, tmax, hr, ho) = *c;
        let height = um(if kind == ChipKind::Resistor { hr } else { ho });
        return Ok(PackageSpec::Chip {
            kind,
            length: pm(l, lt),
            width: pm(w, wt),
            terminal: range(tmin, tmax),
            height,
        });
    }

    match norm(canonical(name)).as_str() {
        "SOT-23" => {
            return Ok(sot23(3, range(2100, 2640), range(1200, 1400), um(1120), Some(3), vec![2, 4, 6]));
        }
        "SOT-23-5" => {
            return Ok(sot23(5, range(2600, 3000), range(1500, 1700), um(1450), Some(3), vec![5]));
        }
        "SOT-23-6" => {
            return Ok(sot23(6, range(2600, 3000), range(1500, 1700), um(1450), None, vec![]));
        }
        // JEDEC TO-261AA.
        "SOT-223" => {
            return Ok(PackageSpec::Tab {
                name: "SOT".into(),
                leads: 3,
                pitch: um(2300),
                span: range(6700, 7300),
                body_width: range(3300, 3700),
                body_length: range(6300, 6700),
                terminal: range(700, 1100),
                lead_width: range(600, 880),
                tab_width: range(2900, 3180),
                tab_terminal: range(700, 1100),
                tab_protrusion: None,
                height: um(1800),
                missing: vec![],
                tab_number: None,
            });
        }
        // JEDEC TO-252AA.
        "TO-252" => {
            return Ok(PackageSpec::Tab {
                name: "TO".into(),
                leads: 3,
                pitch: um(2290),
                span: range(9400, 10400),
                body_width: range(5970, 6220),
                body_length: range(6350, 6730),
                terminal: range(1400, 1780),
                lead_width: range(630, 890),
                tab_width: range(4950, 5460),
                tab_terminal: range(5200, 5800),
                tab_protrusion: Some(range(890, 1270)),
                height: um(2380),
                missing: vec![2],
                tab_number: Some(2),
            });
        }
        // JEDEC TO-263AB.
        "TO-263" => {
            return Ok(PackageSpec::Tab {
                name: "TO".into(),
                leads: 3,
                pitch: um(2540),
                span: range(14610, 15880),
                body_width: range(8380, 9650),
                body_length: range(9650, 10670),
                terminal: range(1780, 2790),
                lead_width: range(510, 990),
                tab_width: range(9650, 10670),
                tab_terminal: range(7000, 7500),
                tab_protrusion: Some(range(1000, 1600)),
                height: um(4830),
                missing: vec![2],
                tab_number: Some(2),
            });
        }
        "SOD-123" => {
            return Ok(sod(SodLead::GullWing, (3550, 3850), (2550, 2850), (1400, 1700), (250, 450), (500, 700), 1350));
        }
        "SOD-123F" => {
            return Ok(sod(SodLead::Flat, (3500, 3900), (2500, 2900), (1500, 1700), (500, 900), (750, 950), 1100));
        }
        "SOD-323" => {
            return Ok(sod(SodLead::GullWing, (2300, 2700), (1600, 1800), (1150, 1350), (200, 450), (250, 400), 1100));
        }
        "SOD-523" => {
            return Ok(sod(SodLead::Flat, (1500, 1700), (1100, 1300), (700, 900), (150, 350), (250, 350), 700));
        }
        "MINIMELF" => {
            return Ok(PackageSpec::Melf {
                kind: ChipKind::Diode,
                length: range(3300, 3700),
                diameter: range(1400, 1600),
                terminal: range(250, 500),
            });
        }
        "MELF" => {
            return Ok(PackageSpec::Melf {
                kind: ChipKind::Diode,
                length: range(4800, 5200),
                diameter: range(2410, 2670),
                terminal: range(450, 650),
            });
        }
        "SMA" => return Ok(molded((4900, 5500), (4000, 4600), (2400, 2800), (1250, 1650), 2300)),
        "SMB" => return Ok(molded((5210, 5590), (4060, 4570), (3300, 3940), (1960, 2210), 2440)),
        "SMC" => return Ok(molded((7750, 8130), (6600, 7110), (5590, 6220), (2900, 3200), 2620)),
        _ => {}
    }

    if let Some(rest) = n.strip_prefix("DIP-").or_else(|| n.strip_prefix("PDIP-")) {
        let (wide, count) = match rest.strip_suffix('W') {
            Some(c) => (true, c),
            None => (false, rest),
        };
        let pins: u32 = count.parse().map_err(|_| unknown())?;
        if pins < 4 || !pins.is_multiple_of(2) || pins > 64 {
            return Err(unknown());
        }
        return Ok(dip(pins, wide));
    }

    if n.starts_with("BGA-") {
        return parametric_bga(name, &n).map_err(|e| format!("{e}; example: `BGA-64 8x8 P0.8mm 6x6mm`"));
    }

    if let Some(rest) = n.strip_prefix("SOIC-") {
        let (wide, count) = match rest.strip_suffix('W') {
            Some(c) => (true, c),
            None => (false, rest),
        };
        let pins: u32 = count.parse().map_err(|_| unknown())?;
        let body_length = match (wide, pins) {
            (false, 8) => range(4800, 5000),
            (false, 14) => range(8550, 8750),
            (false, 16) => range(9800, 10000),
            (true, 16) => range(10100, 10500),
            (true, 20) => range(12600, 13000),
            (true, 24) => range(15200, 15600),
            (true, 28) => range(17700, 18100),
            _ => return Err(unknown()),
        };
        let (span, body_width, height) = if wide {
            (range(10000, 10650), range(7400, 7600), um(2650))
        } else {
            (range(5800, 6200), range(3800, 4000), um(1750))
        };
        return Ok(PackageSpec::GullWing {
            name: "SOIC".into(),
            pins,
            pitch: um(1270),
            span,
            body_width,
            body_length,
            terminal: range(400, 1270),
            lead_width: range(310, 510),
            height,
            slots_per_side: None,
            missing: vec![],
            exposed_pad: None,
        });
    }

    if let Some(count) = n.strip_prefix("TSSOP-") {
        let pins: u32 = count.parse().map_err(|_| unknown())?;
        let body_length = match pins {
            8 => range(2900, 3100),
            14 | 16 => range(4900, 5100),
            20 => range(6400, 6600),
            24 => range(7700, 7900),
            28 => range(9600, 9800),
            _ => return Err(unknown()),
        };
        return Ok(PackageSpec::GullWing {
            name: "SOP".into(),
            pins,
            pitch: um(650),
            span: range(6200, 6600),
            body_width: range(4300, 4500),
            body_length,
            terminal: range(450, 750),
            lead_width: range(190, 300),
            height: um(1200),
            slots_per_side: None,
            missing: vec![],
            exposed_pad: None,
        });
    }

    if let Some(count) = n.strip_prefix("MSOP-") {
        let (pins, pitch, lead_width) = match count {
            "8" => (8, um(650), range(220, 380)),
            "10" => (10, um(500), range(170, 270)),
            _ => return Err(unknown()),
        };
        return Ok(PackageSpec::GullWing {
            name: "SOP".into(),
            pins,
            pitch,
            span: range(4750, 5050),
            body_width: range(2900, 3100),
            body_length: range(2900, 3100),
            terminal: range(400, 700),
            lead_width,
            height: um(1100),
            slots_per_side: None,
            missing: vec![],
            exposed_pad: None,
        });
    }

    if let Some(count) = n.strip_prefix("LQFP-").or_else(|| n.strip_prefix("TQFP-")) {
        let pins: u32 = count.parse().map_err(|_| unknown())?;
        let (body, span, pitch, b) = match pins {
            32 => (7000, 9000, 800, range(300, 450)),
            44 => (10000, 12000, 800, range(300, 450)),
            48 => (7000, 9000, 500, range(170, 270)),
            64 => (10000, 12000, 500, range(170, 270)),
            100 => (14000, 16000, 500, range(170, 270)),
            144 => (20000, 22000, 500, range(170, 270)),
            _ => return Err(unknown()),
        };
        let height = if n.starts_with("TQFP") { um(1200) } else { um(1600) };
        return Ok(PackageSpec::Qfp {
            pins,
            pitch: um(pitch),
            span_x: pm(span, 200),
            span_y: pm(span, 200),
            body_x: pm(body, 100),
            body_y: pm(body, 100),
            terminal: range(450, 750),
            lead_width: b,
            height,
            exposed_pad: None,
        });
    }

    if n.starts_with("QFN-") || n.starts_with("DFN-") || n.starts_with("SON-") {
        return parametric_nolead(name, &n).map_err(|e| format!("{e}; example: `QFN-32 5x5mm P0.5mm EP3.1mm`"));
    }

    if let Some(rest) =
        n.strip_prefix("PINHEADER-").or_else(|| n.strip_prefix("HEADER-")).or_else(|| n.strip_prefix("PINSOCKET-"))
    {
        let mut parts = rest.split('-');
        let geometry = parts.next().unwrap_or("");
        let (r, c) = geometry.split_once('X').ok_or_else(unknown)?;
        let rows: u32 = r.parse().map_err(|_| unknown())?;
        let pins_per_row: u32 = c.parse().map_err(|_| unknown())?;
        let mut pitch = um(2540);
        for p in parts {
            if let Some(v) = p.strip_prefix('P').and_then(len_mm) {
                pitch = v;
            }
        }
        let (drill, pad) = if pitch < um(2000) { (um(700), um(1200)) } else { (um(1000), um(1700)) };
        return Ok(PackageSpec::PinHeader { rows, pins_per_row, pitch, drill, pad, height: um(2500) });
    }

    Err(unknown())
}

/// SOD from (min, max) µm ranges: span, body length (X), body width (Y), foot, lead width; height.
fn sod(
    lead: SodLead,
    span: (i64, i64),
    body_length: (i64, i64),
    body_width: (i64, i64),
    terminal: (i64, i64),
    lead_width: (i64, i64),
    height: i64,
) -> PackageSpec {
    let r = |v: (i64, i64)| range(v.0, v.1);
    PackageSpec::Sod {
        lead,
        span: r(span),
        body_length: r(body_length),
        body_width: r(body_width),
        terminal: r(terminal),
        lead_width: r(lead_width),
        height: um(height),
    }
}

/// DO-214 molded diode from (min, max) µm ranges: overall length, body length, width, terminal
/// width; height. The terminal length under the body is 0.76–1.52 mm for all three sizes.
fn molded(
    length: (i64, i64),
    body_length: (i64, i64),
    width: (i64, i64),
    lead_width: (i64, i64),
    height: i64,
) -> PackageSpec {
    let r = |v: (i64, i64)| range(v.0, v.1);
    PackageSpec::Molded {
        kind: ChipKind::Diode,
        length: r(length),
        body_length: r(body_length),
        width: r(width),
        terminal: range(760, 1520),
        lead_width: r(lead_width),
        height: um(height),
    }
}

/// Plastic DIP, JEDEC MS-001 (300 mil) / MS-011 (600 mil) typical body sizes.
fn dip(pins: u32, wide: bool) -> PackageSpec {
    let body_length = match (wide, pins) {
        (false, 8) => range(9020, 10160),
        (false, 14 | 16) => range(18670, 19690),
        (false, 18) => range(22350, 23370),
        (false, 20) => range(24890, 26920),
        (_, 24) => range(31120, 32130),
        (_, 28) => range(34670, 37400),
        (true, 32) => range(40500, 42500),
        (true, 40) => range(50300, 53200),
        (true, 48) => range(60600, 62600),
        // Others: the pin rows plus about 2.5 mm of body.
        _ => Dim::plus_minus(um(2540 * (pins as i64 / 2) + 1270), um(500)),
    };
    let (row_spacing, body_width) = if wide { (um(15240), range(13000, 14000)) } else { (um(7620), range(6100, 6600)) };
    PackageSpec::Dip {
        pins,
        pitch: um(2540),
        row_spacing,
        body_width,
        body_length,
        height: um(5330),
        drill: um(800),
        pad: um(1600),
    }
}

/// `BGA-64 8x8 P0.8mm 6x6mm [B0.4mm] [H1.2mm] [VOID2x2]`.
fn parametric_bga(orig: &str, n: &str) -> Result<PackageSpec, String> {
    let mut tokens = n.split('-').skip(1);
    let pins: u32 = tokens.next().and_then(|t| t.parse().ok()).ok_or_else(|| format!("`{orig}`: missing pin count"))?;
    let (mut grid, mut body, mut pitch, mut ball, mut height, mut void) = (None, None, None, None, None, None);
    let pair = |v: &str, what: &str| -> Result<(Nm, Nm), String> {
        let (a, b) = v.split_once('X').ok_or_else(|| format!("bad {what}"))?;
        Ok((len_mm(a).ok_or_else(|| format!("bad {what}"))?, len_mm(b).ok_or_else(|| format!("bad {what}"))?))
    };
    let count_pair = |v: &str, what: &str| -> Result<(u32, u32), String> {
        let (a, b) = v.split_once('X').ok_or_else(|| format!("bad {what}"))?;
        Ok((a.parse().map_err(|_| format!("bad {what}"))?, b.parse().map_err(|_| format!("bad {what}"))?))
    };
    for t in tokens {
        if let Some(v) = t.strip_prefix("VOID") {
            void = Some(count_pair(v, "VOID block")?);
        } else if let Some(v) = t.strip_prefix('P') {
            pitch = Some(len_mm(v).ok_or("bad pitch")?);
        } else if let Some(v) = t.strip_prefix('B') {
            ball = Some(len_mm(v).ok_or("bad ball diameter")?);
        } else if let Some(v) = t.strip_prefix('H') {
            height = Some(len_mm(v).ok_or("bad height")?);
        } else if t.contains('X') && grid.is_none() && !t.ends_with("MM") {
            grid = Some(count_pair(t, "ball grid")?);
        } else if t.contains('X') && body.is_none() {
            body = Some(pair(t, "body size")?);
        } else {
            return Err(format!("`{orig}`: unexpected `{t}`"));
        }
    }
    let (cols, rows) = grid.ok_or_else(|| format!("`{orig}`: missing ball grid (e.g. 8x8)"))?;
    let (bx, by) = body.ok_or_else(|| format!("`{orig}`: missing body size (e.g. 6x6mm)"))?;
    let pitch = pitch.ok_or_else(|| format!("`{orig}`: missing pitch (e.g. P0.8mm)"))?;
    // Typical JEDEC ball diameter for the pitch (MO-216/MO-275 style).
    let ball = ball.unwrap_or_else(|| match pitch.0 / 1000 {
        1270.. => um(750),
        1000.. => um(500),
        800.. => um(400),
        650.. => um(350),
        500.. => um(300),
        400.. => um(250),
        _ => Nm(pitch.0 / 2),
    });
    let spec = PackageSpec::Bga {
        rows,
        cols,
        pitch,
        ball: Dim::plus_minus(ball, um(50)),
        body_x: Dim::plus_minus(bx, um(100)),
        body_y: Dim::plus_minus(by, um(100)),
        height: height.unwrap_or(um(1200)),
        missing: vec![],
        missing_center: void.map(|(c, r)| [c, r]),
    };
    let balls = spec.pad_count();
    if balls != pins {
        return Err(format!(
            "`{orig}`: {pins} pins but {balls} balls are populated (use VOIDcxr or explicit `missing`)"
        ));
    }
    Ok(spec)
}

fn sot23(pins: u32, span: Dim, body_width: Dim, height: Nm, slots: Option<u32>, missing: Vec<u32>) -> PackageSpec {
    PackageSpec::GullWing {
        name: "SOT".into(),
        pins,
        pitch: um(950),
        span,
        body_width,
        body_length: range(2800, 3040),
        terminal: range(300, 600),
        lead_width: range(300, 500),
        height,
        slots_per_side: slots,
        missing,
        exposed_pad: None,
    }
}

/// `QFN-32 5x5mm P0.5mm EP3.1mm [L0.4mm] [b0.25mm] [H0.9mm]`.
fn parametric_nolead(orig: &str, n: &str) -> Result<PackageSpec, String> {
    let mut tokens = n.split('-');
    let family = tokens.next().unwrap_or("");
    let pins: u32 = tokens.next().and_then(|t| t.parse().ok()).ok_or_else(|| format!("`{orig}`: missing pin count"))?;
    let (mut body, mut pitch, mut ep, mut terminal, mut lead_width, mut height) = (None, None, None, None, None, None);
    for t in tokens {
        if let Some(v) = t.strip_prefix("EP") {
            let (a, b) = v.split_once('X').unwrap_or((v, v));
            ep = Some((len_mm(a).ok_or("bad EP size")?, len_mm(b).ok_or("bad EP size")?));
        } else if let Some(v) = t.strip_prefix('P') {
            pitch = Some(len_mm(v).ok_or("bad pitch")?);
        } else if let Some(v) = t.strip_prefix('L') {
            terminal = Some(len_mm(v).ok_or("bad terminal length")?);
        } else if let Some(v) = t.strip_prefix('B') {
            lead_width = Some(len_mm(v).ok_or("bad terminal width")?);
        } else if let Some(v) = t.strip_prefix('H') {
            height = Some(len_mm(v).ok_or("bad height")?);
        } else if let Some((a, b)) = t.split_once('X') {
            body = Some((len_mm(a).ok_or("bad body size")?, len_mm(b).ok_or("bad body size")?));
        } else {
            return Err(format!("`{orig}`: unexpected `{t}`"));
        }
    }
    let (bx, by) = body.ok_or_else(|| format!("`{orig}`: missing body size (e.g. 5x5mm)"))?;
    let pitch = pitch.ok_or_else(|| format!("`{orig}`: missing pitch (e.g. P0.5mm)"))?;
    let tol = um(100);
    let terminal = terminal.map(|t| Dim::plus_minus(t, tol)).unwrap_or(range(300, 500));
    let lead_width = lead_width.map(|b| Dim::plus_minus(b, um(50))).unwrap_or_else(|| {
        if pitch <= um(400) {
            range(150, 250)
        } else if pitch <= um(500) {
            range(180, 300)
        } else {
            range(250, 350)
        }
    });
    let height = height.unwrap_or(um(900));
    let exposed_pad = ep.map(|(w, l)| ExposedPad { width: w, length: l, number: None });
    Ok(if family == "QFN" {
        PackageSpec::Qfn {
            pins,
            pitch,
            body_x: Dim::plus_minus(bx, tol),
            body_y: Dim::plus_minus(by, tol),
            terminal,
            lead_width,
            height,
            exposed_pad,
        }
    } else {
        PackageSpec::Dfn {
            pins,
            pitch,
            body_width: Dim::plus_minus(bx, tol),
            body_length: Dim::plus_minus(by, tol),
            terminal,
            lead_width,
            height,
            exposed_pad,
        }
    })
}
