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

use super::{ChipKind, Dim, ExposedPad, PackageSpec};
use crate::units::Nm;

fn um(v: i64) -> Nm {
    Nm::from_um(v)
}

fn range(min_um: i64, max_um: i64) -> Dim {
    Dim {
        min: um(min_um),
        max: um(max_um),
    }
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

/// Imperial chip codes, for recognizing package tokens in part specs.
pub fn chip_codes() -> impl Iterator<Item = &'static str> {
    CHIPS.iter().map(|c| c.0)
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
            "unknown package `{name}`; known: chip sizes (0402, 0603, ...), SOT-23[-5|-6], SOIC-N, SOIC-NW, TSSOP-N, \
             MSOP-8/10, LQFP-N, `QFN-32 5x5mm P0.5mm EP3.1mm`, `DFN-8 3x3mm P0.65mm EP1.6x2.4mm`, `PinHeader 1x04`; \
             or give explicit dimensions"
        )
    };

    // Chips: imperial, or metric with an M/METRIC suffix.
    let chip_code = n
        .strip_prefix('R')
        .or_else(|| n.strip_prefix('C'))
        .or_else(|| n.strip_prefix('L'))
        .unwrap_or(&n);
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

    match n.as_str() {
        "SOT-23" | "SOT-23-3" | "SOT23" | "SOT23-3" | "TO-236" => {
            return Ok(sot23(
                3,
                range(2100, 2640),
                range(1200, 1400),
                um(1120),
                Some(3),
                vec![2, 4, 6],
            ));
        }
        "SOT-23-5" | "SOT23-5" | "SOT-25" | "SOT-753" => {
            return Ok(sot23(
                5,
                range(2600, 3000),
                range(1500, 1700),
                um(1450),
                Some(3),
                vec![5],
            ));
        }
        "SOT-23-6" | "SOT23-6" | "SOT-26" => {
            return Ok(sot23(6, range(2600, 3000), range(1500, 1700), um(1450), None, vec![]));
        }
        _ => {}
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

    if let Some(rest) = n
        .strip_prefix("PINHEADER-")
        .or_else(|| n.strip_prefix("HEADER-"))
        .or_else(|| n.strip_prefix("PINSOCKET-"))
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
        let (drill, pad) = if pitch < um(2000) {
            (um(700), um(1200))
        } else {
            (um(1000), um(1700))
        };
        return Ok(PackageSpec::PinHeader {
            rows,
            pins_per_row,
            pitch,
            drill,
            pad,
            height: um(2500),
        });
    }

    Err(unknown())
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
    let pins: u32 = tokens
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| format!("`{orig}`: missing pin count"))?;
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
    let exposed_pad = ep.map(|(w, l)| ExposedPad {
        width: w,
        length: l,
        number: None,
    });
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
