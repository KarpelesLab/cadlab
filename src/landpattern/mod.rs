//! Footprint (land pattern) generator, following IPC-7351B.
//!
//! A [`PackageSpec`] describes a package from its datasheet dimensions (with tolerances); the
//! generator computes pads, courtyard, silkscreen and fabrication outline, and names the result
//! with the IPC-7351 naming convention (`RESC1005X40N`, `SOIC127P600X175-8N`,
//! `QFN50P500X500X90-33N`). [`packages::parse`] turns common package names (`0402`, `SOT-23-5`,
//! `SOIC-8`, `QFN-32 5x5mm P0.5mm EP3.1mm`) into specs.
//!
//! Coordinates are IPC zero orientation, origin at the package center, Y up.

mod draw;
mod ipc;
pub mod packages;

pub use packages::chip_codes;

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::geom::Point;
use crate::model::footprint::{Body, Footprint, Mount, Pad, PadKind, PadShape, Paste};
use crate::units::{Nm, UnitError};

/// IPC-7351 density level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    /// Level A: maximum land protrusion, for hand soldering, wave soldering, high shock.
    Most = 0,
    /// Level B: nominal; the right choice for most reflow-soldered boards.
    #[default]
    Nominal = 1,
    /// Level C: minimal lands, for high-density boards.
    Least = 2,
}

impl Density {
    /// IPC name suffix.
    pub const fn suffix(self) -> char {
        match self {
            Density::Most => 'M',
            Density::Nominal => 'N',
            Density::Least => 'L',
        }
    }
}

/// A toleranced dimension. Written `"1.0±0.05mm"`, `"0.15..0.35mm"` or `"1.0mm"` (exact).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dim {
    /// Minimum.
    pub min: Nm,
    /// Maximum.
    pub max: Nm,
}

impl Dim {
    /// Exact value.
    pub const fn exact(v: Nm) -> Self {
        Dim { min: v, max: v }
    }

    /// Nominal ± tolerance.
    pub fn tol(&self) -> Nm {
        self.max - self.min
    }

    /// Midpoint.
    pub fn nominal(&self) -> Nm {
        Nm((self.min.0 + self.max.0) / 2)
    }

    /// From nominal and symmetric tolerance.
    pub fn plus_minus(nominal: Nm, tol: Nm) -> Self {
        Dim {
            min: nominal - tol,
            max: nominal + tol,
        }
    }

    /// Parses `1.0±0.05mm`, `1.0+-0.05mm`, `0.15..0.35mm`, `1.0mm`. A unit on one side applies to
    /// both.
    pub fn parse(s: &str) -> Result<Dim, UnitError> {
        let s = s.trim();
        let with_unit = |a: &str, b: &str| -> Result<(Nm, Nm), UnitError> {
            let (a, b) = (a.trim(), b.trim());
            match (Nm::parse(a), Nm::parse(b)) {
                (Ok(x), Ok(y)) => Ok((x, y)),
                (Err(UnitError::MissingUnit(_)), Ok(_)) => {
                    let unit = b.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-');
                    Ok((Nm::parse(&format!("{a}{unit}"))?, Nm::parse(b)?))
                }
                (Ok(_), Err(UnitError::MissingUnit(_))) => {
                    let unit = a.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-');
                    Ok((Nm::parse(a)?, Nm::parse(&format!("{b}{unit}"))?))
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        };
        if let Some((n, t)) = s.split_once('±').or_else(|| s.split_once("+-")) {
            let (n, t) = with_unit(n, t)?;
            return Ok(Dim::plus_minus(n, t.abs()));
        }
        if let Some((a, b)) = s.split_once("..") {
            let (a, b) = with_unit(a, b)?;
            return Ok(Dim {
                min: a.min(b),
                max: a.max(b),
            });
        }
        Ok(Dim::exact(Nm::parse(s)?))
    }
}

impl fmt::Display for Dim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}..{}", self.min, self.max)
        }
    }
}

impl Serialize for Dim {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Dim {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        Dim::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Dim {
    fn schema_name() -> Cow<'static, str> {
        "Dim".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Toleranced dimension from the datasheet: \"1.0±0.05mm\", \"0.15..0.35mm\" (min..max) or \"1.0mm\"."
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// Body type of a two-terminal chip, for the IPC name prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChipKind {
    /// Resistor (RESC).
    #[default]
    Resistor,
    /// Capacitor (CAPC).
    Capacitor,
    /// Inductor (INDC).
    Inductor,
    /// LED (LEDC); polarized.
    Led,
    /// Diode (DIOC); polarized.
    Diode,
    /// Fuse (FUSC).
    Fuse,
}

impl ChipKind {
    fn prefix(self) -> &'static str {
        match self {
            ChipKind::Resistor => "RESC",
            ChipKind::Capacitor => "CAPC",
            ChipKind::Inductor => "INDC",
            ChipKind::Led => "LEDC",
            ChipKind::Diode => "DIOC",
            ChipKind::Fuse => "FUSC",
        }
    }

    fn polarized(self) -> bool {
        matches!(self, ChipKind::Led | ChipKind::Diode)
    }
}

/// Exposed (thermal) pad under the package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExposedPad {
    /// Size along X.
    pub width: Nm,
    /// Size along Y.
    pub length: Nm,
    /// Pad number (default: one more than the last pin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
}

/// Package description, from datasheet dimensions.
///
/// Axis conventions (IPC zero orientation): for two-row packages the pin rows run along Y and
/// leads point along ±X; `span` is the toe-to-toe lead span across the rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "family", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageSpec {
    /// Two-terminal chip (resistor, capacitor, inductor, LED...): 0402, 0603, ...
    Chip {
        /// Body type.
        #[serde(default)]
        kind: ChipKind,
        /// Body length, terminal end to terminal end.
        length: Dim,
        /// Body width.
        width: Dim,
        /// Terminal (end cap) length.
        terminal: Dim,
        /// Maximum height.
        height: Nm,
    },
    /// Gull-wing leads on two sides: SOIC, SOP, TSSOP, MSOP, SOT-23.
    GullWing {
        /// IPC family name: `SOIC`, `SOP`, `SOT`.
        #[serde(default = "default_sop")]
        name: String,
        /// Number of pins.
        pins: u32,
        /// Pin pitch.
        pitch: Nm,
        /// Lead span, toe to toe (E).
        span: Dim,
        /// Body width across the rows (E1).
        body_width: Dim,
        /// Body length along the rows (D).
        body_length: Dim,
        /// Foot length (L).
        terminal: Dim,
        /// Lead width (b).
        lead_width: Dim,
        /// Maximum height (A).
        height: Nm,
        /// Lead positions per side when some are unpopulated (SOT-23-3/5): slots are numbered
        /// counter-clockwise from top-left like pins; pins are numbered over the remaining slots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slots_per_side: Option<u32>,
        /// Empty slots, 1-based.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        missing: Vec<u32>,
        /// Exposed pad.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposed_pad: Option<ExposedPad>,
    },
    /// Gull-wing leads on four sides: QFP, LQFP, TQFP.
    Qfp {
        /// Number of pins (multiple of 4).
        pins: u32,
        /// Pin pitch.
        pitch: Nm,
        /// Lead span, toe to toe, along X.
        span_x: Dim,
        /// Lead span along Y.
        span_y: Dim,
        /// Body size along X.
        body_x: Dim,
        /// Body size along Y.
        body_y: Dim,
        /// Foot length (L).
        terminal: Dim,
        /// Lead width (b).
        lead_width: Dim,
        /// Maximum height.
        height: Nm,
        /// Exposed pad.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposed_pad: Option<ExposedPad>,
    },
    /// No-lead terminals on two sides: DFN, SON.
    Dfn {
        /// Number of pins (excluding the exposed pad).
        pins: u32,
        /// Pin pitch.
        pitch: Nm,
        /// Body size across the rows (terminals at its edges).
        body_width: Dim,
        /// Body size along the rows.
        body_length: Dim,
        /// Terminal length (L).
        terminal: Dim,
        /// Terminal width (b).
        lead_width: Dim,
        /// Maximum height.
        height: Nm,
        /// Exposed pad.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposed_pad: Option<ExposedPad>,
    },
    /// No-lead terminals on four sides: QFN.
    Qfn {
        /// Number of pins (multiple of 4, excluding the exposed pad).
        pins: u32,
        /// Pin pitch.
        pitch: Nm,
        /// Body size along X.
        body_x: Dim,
        /// Body size along Y.
        body_y: Dim,
        /// Terminal length (L).
        terminal: Dim,
        /// Terminal width (b).
        lead_width: Dim,
        /// Maximum height.
        height: Nm,
        /// Exposed pad.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposed_pad: Option<ExposedPad>,
    },
    /// Through-hole pin header or socket.
    PinHeader {
        /// Rows (1 or 2).
        rows: u32,
        /// Pins per row.
        pins_per_row: u32,
        /// Pitch.
        #[serde(default = "default_header_pitch")]
        pitch: Nm,
        /// Finished hole diameter.
        #[serde(default = "default_header_drill")]
        drill: Nm,
        /// Pad diameter.
        #[serde(default = "default_header_pad")]
        pad: Nm,
        /// Plastic body height.
        #[serde(default = "default_header_height")]
        height: Nm,
    },
}

fn default_sop() -> String {
    "SOP".into()
}
fn default_header_pitch() -> Nm {
    Nm::from_um(2540)
}
fn default_header_drill() -> Nm {
    Nm::from_um(1000)
}
fn default_header_pad() -> Nm {
    Nm::from_um(1700)
}
fn default_header_height() -> Nm {
    Nm::from_um(2500)
}

/// Generator settings. Defaults: nominal density, F = 0.05 mm, P = 0.025 mm, 0.01 mm rounding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GenOptions {
    /// IPC density level.
    pub density: Density,
    /// Board fabrication tolerance (F).
    pub fab_tolerance: Nm,
    /// Part placement tolerance (P).
    pub placement_tolerance: Nm,
    /// Pad dimension rounding grid.
    pub rounding: Nm,
    /// Minimum copper gap between neighbouring pads.
    pub min_pad_gap: Nm,
    /// Corner radius as a fraction of the shorter pad side, in percent (0 = sharp rectangles).
    pub corner_ratio_pct: u32,
    /// Maximum corner radius.
    pub max_corner_radius: Nm,
    /// Silkscreen line width.
    pub silk_width: Nm,
    /// Clearance between silkscreen and pad copper.
    pub silk_clearance: Nm,
    /// Fabrication outline line width.
    pub fab_width: Nm,
    /// Courtyard line width.
    pub courtyard_width: Nm,
    /// Courtyard rounding grid.
    pub courtyard_grid: Nm,
    /// Solder paste coverage of exposed pads, in percent.
    pub ep_paste_pct: u32,
}

impl Default for GenOptions {
    fn default() -> Self {
        GenOptions {
            density: Density::Nominal,
            fab_tolerance: Nm::from_um(50),
            placement_tolerance: Nm::from_um(25),
            rounding: Nm::from_um(10),
            min_pad_gap: Nm::from_um(150),
            corner_ratio_pct: 25,
            max_corner_radius: Nm::from_um(250),
            silk_width: Nm::from_um(120),
            silk_clearance: Nm::from_um(150),
            fab_width: Nm::from_um(100),
            courtyard_width: Nm::from_um(50),
            courtyard_grid: Nm::from_um(10),
            ep_paste_pct: 60,
        }
    }
}

/// Error generating a footprint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenError {
    /// Inconsistent or impossible dimensions.
    #[error("invalid package: {0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> GenError {
    GenError::Invalid(msg.into())
}

/// Dimension in hundredths of a millimeter, for IPC names.
fn hmm(v: Nm) -> i64 {
    (v.0 + 5_000) / 10_000
}

/// Dimension in tenths of a millimeter, truncated, for chip names (0805: 2.0 × 1.25 → `2012`).
fn tmm(v: Nm) -> i64 {
    v.0 / 100_000
}

impl PackageSpec {
    /// IPC-7351 name at the given density.
    pub fn ipc_name(&self, density: Density) -> String {
        let d = density.suffix();
        match self {
            PackageSpec::Chip {
                kind,
                length,
                width,
                height,
                ..
            } => {
                format!(
                    "{}{:02}{:02}X{}{d}",
                    kind.prefix(),
                    tmm(length.nominal()),
                    tmm(width.nominal()),
                    hmm(*height)
                )
            }
            PackageSpec::GullWing {
                name,
                pins,
                pitch,
                span,
                height,
                exposed_pad,
                ..
            } => {
                let ep = u32::from(exposed_pad.is_some());
                format!(
                    "{name}{}P{}X{}-{}{d}",
                    hmm(*pitch),
                    hmm(span.nominal()),
                    hmm(*height),
                    pins + ep
                )
            }
            PackageSpec::Qfp {
                pins,
                pitch,
                span_x,
                span_y,
                height,
                exposed_pad,
                ..
            } => {
                let ep = u32::from(exposed_pad.is_some());
                format!(
                    "QFP{}P{}X{}X{}-{}{d}",
                    hmm(*pitch),
                    hmm(span_x.nominal()),
                    hmm(span_y.nominal()),
                    hmm(*height),
                    pins + ep
                )
            }
            PackageSpec::Dfn {
                pins,
                pitch,
                body_width,
                body_length,
                height,
                exposed_pad,
                ..
            } => {
                let ep = u32::from(exposed_pad.is_some());
                format!(
                    "SON{}P{}X{}X{}-{}{d}",
                    hmm(*pitch),
                    hmm(body_length.nominal()),
                    hmm(body_width.nominal()),
                    hmm(*height),
                    pins + ep
                )
            }
            PackageSpec::Qfn {
                pins,
                pitch,
                body_x,
                body_y,
                height,
                exposed_pad,
                ..
            } => {
                let ep = u32::from(exposed_pad.is_some());
                format!(
                    "QFN{}P{}X{}X{}-{}{d}",
                    hmm(*pitch),
                    hmm(body_x.nominal()),
                    hmm(body_y.nominal()),
                    hmm(*height),
                    pins + ep
                )
            }
            PackageSpec::PinHeader {
                rows,
                pins_per_row,
                pitch,
                ..
            } => {
                format!(
                    "PinHeader_{rows}x{pins_per_row:02}_P{}mm",
                    pitch.display_in(crate::units::LengthUnit::Mm).trim_end_matches("mm")
                )
            }
        }
    }

    /// Total number of numbered pads (pins plus exposed pad).
    pub fn pad_count(&self) -> u32 {
        match self {
            PackageSpec::Chip { .. } => 2,
            PackageSpec::GullWing { pins, exposed_pad, .. }
            | PackageSpec::Qfp { pins, exposed_pad, .. }
            | PackageSpec::Dfn { pins, exposed_pad, .. }
            | PackageSpec::Qfn { pins, exposed_pad, .. } => pins + u32::from(exposed_pad.is_some()),
            PackageSpec::PinHeader { rows, pins_per_row, .. } => rows * pins_per_row,
        }
    }
}

/// Generates the footprint for `spec`.
pub fn generate(spec: &PackageSpec, opts: &GenOptions) -> Result<Footprint, GenError> {
    let name = spec.ipc_name(opts.density);
    let mut fp = match spec {
        PackageSpec::Chip {
            kind,
            length,
            width,
            terminal,
            height,
        } => {
            let f = if length.nominal() < Nm::from_um(1500) {
                &ipc::CHIP_SMALL
            } else {
                &ipc::CHIP
            };
            check_dims(&[("length", length), ("width", width), ("terminal", terminal)])?;
            let row = ipc::row_pads(*length, *terminal, *width, f, opts);
            let pads = vec![
                smd_pad("1", Point::new(-row.center, Nm::ZERO), row.length, row.width, opts),
                smd_pad("2", Point::new(row.center, Nm::ZERO), row.length, row.width, opts),
            ];
            let body = Body {
                width: length.nominal(),
                length: width.nominal(),
                height: *height,
            };
            draw::finish(
                name,
                Mount::Smd,
                pads,
                body,
                f.courtyard(opts.density),
                kind.polarized(),
                opts,
            )
        }
        PackageSpec::GullWing {
            pins,
            pitch,
            span,
            body_width,
            body_length,
            terminal,
            lead_width,
            height,
            slots_per_side,
            missing,
            exposed_pad,
            ..
        } => {
            check_dims(&[
                ("span", span),
                ("body_width", body_width),
                ("terminal", terminal),
                ("lead_width", lead_width),
            ])?;
            let f = if *pitch <= Nm::from_um(625) {
                &ipc::GULLWING_FINE
            } else {
                &ipc::GULLWING
            };
            let row = ipc::row_pads(*span, *terminal, *lead_width, f, opts);
            let width = ipc::clamp_to_pitch(row.width, *pitch, opts.min_pad_gap);
            let per_side = match slots_per_side {
                Some(n) => *n,
                None => {
                    if !pins.is_multiple_of(2) {
                        return Err(invalid(
                            "a two-row package needs an even pin count, or `slots_per_side`",
                        ));
                    }
                    pins / 2
                }
            };
            if per_side * 2 - missing.len() as u32 != *pins {
                return Err(invalid(format!(
                    "{pins} pins do not fit {per_side} slots per side with {} missing",
                    missing.len()
                )));
            }
            let mut pads = dual_row(per_side, *pitch, row.center, row.length, width, missing, opts);
            if let Some(ep) = exposed_pad {
                pads.push(exposed(ep, pins + 1, opts));
            }
            let body = Body {
                width: body_width.nominal(),
                length: body_length.nominal(),
                height: *height,
            };
            draw::finish(name, Mount::Smd, pads, body, f.courtyard(opts.density), true, opts)
        }
        PackageSpec::Dfn {
            pins,
            pitch,
            body_width,
            body_length,
            terminal,
            lead_width,
            height,
            exposed_pad,
        } => {
            check_dims(&[
                ("body_width", body_width),
                ("terminal", terminal),
                ("lead_width", lead_width),
            ])?;
            if !pins.is_multiple_of(2) {
                return Err(invalid("a DFN needs an even pin count"));
            }
            let f = &ipc::NOLEAD;
            let row = ipc::row_pads(*body_width, *terminal, *lead_width, f, opts);
            let width = ipc::clamp_to_pitch(row.width, *pitch, opts.min_pad_gap);
            let mut pads = dual_row(pins / 2, *pitch, row.center, row.length, width, &[], opts);
            if let Some(ep) = exposed_pad {
                pads.push(exposed(ep, pins + 1, opts));
            }
            let body = Body {
                width: body_width.nominal(),
                length: body_length.nominal(),
                height: *height,
            };
            draw::finish(name, Mount::Smd, pads, body, f.courtyard(opts.density), true, opts)
        }
        PackageSpec::Qfp {
            pins,
            pitch,
            span_x,
            span_y,
            body_x,
            body_y,
            terminal,
            lead_width,
            height,
            exposed_pad,
        } => {
            check_dims(&[
                ("span_x", span_x),
                ("span_y", span_y),
                ("terminal", terminal),
                ("lead_width", lead_width),
            ])?;
            let f = if *pitch <= Nm::from_um(625) {
                &ipc::GULLWING_FINE
            } else {
                &ipc::GULLWING
            };
            let rx = ipc::row_pads(*span_x, *terminal, *lead_width, f, opts);
            let ry = ipc::row_pads(*span_y, *terminal, *lead_width, f, opts);
            let mut pads = quad(*pins, *pitch, rx, ry, opts)?;
            if let Some(ep) = exposed_pad {
                pads.push(exposed(ep, pins + 1, opts));
            }
            let body = Body {
                width: body_x.nominal(),
                length: body_y.nominal(),
                height: *height,
            };
            draw::finish(name, Mount::Smd, pads, body, f.courtyard(opts.density), true, opts)
        }
        PackageSpec::Qfn {
            pins,
            pitch,
            body_x,
            body_y,
            terminal,
            lead_width,
            height,
            exposed_pad,
        } => {
            check_dims(&[
                ("body_x", body_x),
                ("body_y", body_y),
                ("terminal", terminal),
                ("lead_width", lead_width),
            ])?;
            let f = &ipc::NOLEAD;
            let rx = ipc::row_pads(*body_x, *terminal, *lead_width, f, opts);
            let ry = ipc::row_pads(*body_y, *terminal, *lead_width, f, opts);
            let mut pads = quad(*pins, *pitch, rx, ry, opts)?;
            if let Some(ep) = exposed_pad {
                pads.push(exposed(ep, pins + 1, opts));
            }
            let body = Body {
                width: body_x.nominal(),
                length: body_y.nominal(),
                height: *height,
            };
            draw::finish(name, Mount::Smd, pads, body, f.courtyard(opts.density), true, opts)
        }
        PackageSpec::PinHeader {
            rows,
            pins_per_row,
            pitch,
            drill,
            pad,
            height,
        } => {
            if !(1..=2).contains(rows) || *pins_per_row == 0 {
                return Err(invalid("pin headers have 1 or 2 rows and at least one pin per row"));
            }
            if *drill >= *pad {
                return Err(invalid("the pad must be larger than the drill"));
            }
            let mut pads = Vec::new();
            let x0 = Nm(-(pitch.0 * (*rows as i64 - 1)) / 2);
            let y0 = Nm(pitch.0 * (*pins_per_row as i64 - 1) / 2);
            for i in 0..*pins_per_row {
                for r in 0..*rows {
                    let n = i * rows + r + 1;
                    let at = Point::new(x0 + *pitch * r as i64, y0 - *pitch * i as i64);
                    let shape = if n == 1 {
                        PadShape::Rect { w: *pad, h: *pad }
                    } else {
                        PadShape::Circle { d: *pad }
                    };
                    pads.push(Pad {
                        number: n.to_string(),
                        at,
                        rotation: Default::default(),
                        shape,
                        kind: PadKind::Tht { drill: *drill },
                        paste: Some(Paste::None),
                    });
                }
            }
            let body = Body {
                width: *pitch * *rows as i64,
                length: *pitch * *pins_per_row as i64,
                height: *height,
            };
            draw::finish(name, Mount::Tht, pads, body, Nm::from_um(250), false, opts)
        }
    }?;
    fp.generator = Some(serde_json::to_value(spec).expect("spec serializes"));
    fp.description = describe(spec, opts.density);
    Ok(fp)
}

fn describe(spec: &PackageSpec, density: Density) -> String {
    let level = match density {
        Density::Most => "most",
        Density::Nominal => "nominal",
        Density::Least => "least",
    };
    let what = match spec {
        PackageSpec::Chip {
            kind, length, width, ..
        } => {
            let k = match kind {
                ChipKind::Resistor => "Resistor",
                ChipKind::Capacitor => "Capacitor",
                ChipKind::Inductor => "Inductor",
                ChipKind::Led => "LED",
                ChipKind::Diode => "Diode",
                ChipKind::Fuse => "Fuse",
            };
            format!("{k} chip {} x {}", length.nominal(), width.nominal())
        }
        PackageSpec::GullWing { name, pins, pitch, .. } => format!("{name} {pins} pins, pitch {pitch}"),
        PackageSpec::Qfp { pins, pitch, .. } => format!("QFP {pins} pins, pitch {pitch}"),
        PackageSpec::Dfn { pins, pitch, .. } => format!("DFN/SON {pins} pins, pitch {pitch}"),
        PackageSpec::Qfn { pins, pitch, .. } => format!("QFN {pins} pins, pitch {pitch}"),
        PackageSpec::PinHeader {
            rows,
            pins_per_row,
            pitch,
            ..
        } => format!("pin header {rows}x{pins_per_row}, pitch {pitch}"),
    };
    format!("{what}; IPC-7351B {level} density, generated by cadlab")
}

fn check_dims(dims: &[(&str, &Dim)]) -> Result<(), GenError> {
    for (name, d) in dims {
        if d.min.0 <= 0 {
            return Err(invalid(format!("`{name}` must be positive")));
        }
    }
    Ok(())
}

fn corner(w: Nm, h: Nm, opts: &GenOptions) -> Nm {
    let r = Nm(w.0.min(h.0) * opts.corner_ratio_pct as i64 / 100);
    r.min(opts.max_corner_radius)
}

/// SMD pad of length `len` along X and width `width` along Y (before rotation).
fn smd_pad(number: &str, at: Point, len: Nm, width: Nm, opts: &GenOptions) -> Pad {
    let shape = if opts.corner_ratio_pct == 0 {
        PadShape::Rect { w: len, h: width }
    } else {
        PadShape::RoundRect {
            w: len,
            h: width,
            r: corner(len, width, opts),
        }
    };
    Pad {
        number: number.to_string(),
        at,
        rotation: Default::default(),
        shape,
        kind: PadKind::Smd,
        paste: None,
    }
}

/// Two rows: pins along Y, left row top to bottom then right row bottom to top.
fn dual_row(per_side: u32, pitch: Nm, center: Nm, len: Nm, width: Nm, missing: &[u32], opts: &GenOptions) -> Vec<Pad> {
    let y0 = Nm(pitch.0 * (per_side as i64 - 1) / 2);
    let mut pads = Vec::new();
    let mut n = 0;
    for slot in 1..=per_side * 2 {
        if missing.contains(&slot) {
            continue;
        }
        n += 1;
        let (x, i) = if slot <= per_side {
            (-center, slot - 1)
        } else {
            (center, per_side * 2 - slot)
        };
        let at = Point::new(x, y0 - pitch * i as i64);
        pads.push(smd_pad(&n.to_string(), at, len, width, opts));
    }
    pads
}

/// Four sides, counter-clockwise from the top of the left side.
fn quad(pins: u32, pitch: Nm, rx: ipc::RowPads, ry: ipc::RowPads, opts: &GenOptions) -> Result<Vec<Pad>, GenError> {
    if !pins.is_multiple_of(4) || pins == 0 {
        return Err(invalid("quad packages need a pin count that is a multiple of 4"));
    }
    let per = pins / 4;
    let off = Nm(pitch.0 * (per as i64 - 1) / 2);
    let wx = ipc::clamp_to_pitch(rx.width, pitch, opts.min_pad_gap);
    let wy = ipc::clamp_to_pitch(ry.width, pitch, opts.min_pad_gap);
    let mut pads = Vec::new();
    for i in 0..pins {
        let side = i / per;
        let k = (i % per) as i64;
        let step = pitch * k;
        let pad = match side {
            0 => smd_pad("", Point::new(-rx.center, off - step), rx.length, wx, opts),
            1 => smd_pad("", Point::new(-off + step, -ry.center), wy, ry.length, opts),
            2 => smd_pad("", Point::new(rx.center, -off + step), rx.length, wx, opts),
            _ => smd_pad("", Point::new(off - step, ry.center), wy, ry.length, opts),
        };
        pads.push(Pad {
            number: (i + 1).to_string(),
            ..pad
        });
    }
    Ok(pads)
}

/// Exposed pad with a grid of paste windows covering `ep_paste_pct` of its area.
fn exposed(ep: &ExposedPad, default_number: u32, opts: &GenOptions) -> Pad {
    let number = ep.number.unwrap_or(default_number).to_string();
    let mut pad = smd_pad(&number, Point::ORIGIN, ep.width, ep.length, opts);
    // Windows of about 1 mm pitch, scaled so the total area matches the coverage target.
    let cells = |len: Nm| ((len.0 + 999_999) / 1_000_000).max(1);
    let (nx, ny) = (cells(ep.width), cells(ep.length));
    let scale = (opts.ep_paste_pct.min(100) as f64 / 100.0).sqrt();
    let (cw, ch) = (ep.width.0 / nx, ep.length.0 / ny);
    let size = (
        Nm((cw as f64 * scale) as i64 / 10_000 * 10_000),
        Nm((ch as f64 * scale) as i64 / 10_000 * 10_000),
    );
    let mut at = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            let x = -ep.width.0 / 2 + cw * i + cw / 2;
            let y = ep.length.0 / 2 - ch * j - ch / 2;
            at.push(Point::new(Nm(x), Nm(y)));
        }
    }
    pad.paste = Some(Paste::Windows { size, at });
    pad
}

#[cfg(test)]
mod tests;
