//! `touchstone.summary`: read a Touchstone file (`.sNp`, versions 1.1 and 2.0) and summarize its
//! S-parameters, to check simulation or measurement results (`docs/ELECTRICAL.md`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::touchstone::{self, Complex, Parameter};
use crate::value::{Quantity, Unit};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Summary>();
}

/// Summarize a Touchstone file's S-parameters: magnitude (dB) and phase at given frequencies,
/// and the best and worst values over the band. Y and Z files are converted with their
/// reference impedances. Needs no project.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    /// The file (`.s2p`, ...; relative to the project when one is open).
    pub path: PathBuf,
    /// Frequencies to report ("1GHz", "2.4GHz"), interpolated between the file's points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frequencies: Vec<Quantity>,
    /// Parameters to report (`S21`, `S11`; `S1,10` for ports past 9). Default: all for up to
    /// four ports, else the reflections and the transmissions from port 1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<String>,
    /// Number of ports of a version 1 file whose extension does not give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<usize>,
}

/// A parameter at one frequency.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Point {
    /// Frequency (Hz).
    pub freq: f64,
    /// Magnitude in dB (20 log10 |S|).
    pub db: f64,
    /// Magnitude.
    pub mag: f64,
    /// Phase in degrees.
    pub phase: f64,
}

/// Summary of one parameter.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ParamSummary {
    /// Name (`S21`).
    pub name: String,
    /// Smallest magnitude over the band.
    pub min: Point,
    /// Largest magnitude over the band.
    pub max: Point,
    /// Values at the requested frequencies.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<Point>,
}

/// Result of `touchstone.summary`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TouchstoneSummary {
    /// File read.
    pub path: String,
    /// Touchstone version (`1.1` for version 1 files).
    pub version: String,
    /// Number of ports.
    pub ports: usize,
    /// Parameter type in the file (S, Y, Z, H, G); summaries are S-parameters.
    pub parameter: Parameter,
    /// Reference impedance per port (Ω).
    pub reference: Vec<f64>,
    /// Number of frequency points.
    pub points: usize,
    /// First and last frequency (Hz).
    pub band: [f64; 2],
    /// Number of noise parameter points.
    #[serde(skip_serializing_if = "is_zero")]
    pub noise_points: usize,
    /// The parameters.
    pub params: Vec<ParamSummary>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn round(v: f64, decimals: i32) -> f64 {
    let k = 10f64.powi(decimals);
    let r = (v * k).round() / k;
    if r == 0.0 { 0.0 } else { r }
}

fn point(f: f64, c: Complex) -> Point {
    Point { freq: f, db: round(c.db().max(-999.0), 3), mag: round(c.abs(), 6), phase: round(c.arg_deg(), 2) }
}

/// `S21` → (1, 0); `S1,10` → (0, 9).
fn param_index(s: &str, n: usize) -> Option<(usize, usize)> {
    let rest = s.trim().strip_prefix(['S', 's'])?;
    let (i, j) = match rest.split_once(',') {
        Some((a, b)) => (a.trim().parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?),
        None if rest.len() == 2 && n < 10 => (rest[..1].parse().ok()?, rest[1..].parse().ok()?),
        None => return None,
    };
    (i >= 1 && j >= 1 && i <= n && j <= n).then(|| (i - 1, j - 1))
}

fn name(i: usize, j: usize, n: usize) -> String {
    if n < 10 { format!("S{}{}", i + 1, j + 1) } else { format!("S{},{}", i + 1, j + 1) }
}

impl Command for Summary {
    const NAME: &'static str = "touchstone.summary";
    const SUMMARY: &'static str =
        "Summarize a Touchstone (.sNp) file: |S| in dB and phase at frequencies, band extremes";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = TouchstoneSummary;

    fn run(self, ctx: &mut Context<'_>) -> Result<TouchstoneSummary, CommandError> {
        let path = match ctx.session.root() {
            Some(root) if self.path.is_relative() => root.join(&self.path),
            _ => self.path.clone(),
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CommandError::from(crate::model::ModelError::Io { path: path.clone(), source: e }))?;
        let ports = self.ports.or_else(|| touchstone::ports_from_extension(&path.to_string_lossy()));
        let ts = touchstone::parse(&text, ports).map_err(|e| {
            CommandError::invalid_args(e.code, format!("{}: {e}", path.display())).with_hint(match e.code {
                "touchstone.no_ports" => "name the file `.sNp` or give `ports`",
                "touchstone.unsupported" => "convert the file to single-ended S, Y or Z parameters",
                _ => "check the file against the Touchstone specification (version 1.1 or 2.0)",
            })
        })?;
        for w in &ts.warnings {
            ctx.report(Diagnostic::info("touchstone.tolerated", w.clone()));
        }
        let s = ts.s_params().map_err(|e| {
            CommandError::invalid_args(e.code, e.message)
                .with_hint("H and G files: convert them to S-parameters with the tool that wrote them")
        })?;
        let n = ts.ports;
        let wanted: Vec<(usize, usize)> = if self.params.is_empty() {
            if n <= 4 {
                (0..n).flat_map(|i| (0..n).map(move |j| (i, j))).collect()
            } else {
                let mut v: Vec<(usize, usize)> = (0..n).map(|i| (i, i)).collect();
                v.extend((1..n).map(|i| (i, 0)));
                v
            }
        } else {
            let mut v = Vec::new();
            for p in &self.params {
                v.push(param_index(p, n).ok_or_else(|| {
                    CommandError::invalid_args(
                        "touchstone.bad_param",
                        format!("`{p}` is not an S-parameter of this {n}-port file"),
                    )
                    .with_hint(if n < 10 { "use S11, S21, ..." } else { "use S1,1, S10,1, ..." })
                })?);
            }
            v
        };
        let mut freqs = Vec::new();
        for q in &self.frequencies {
            let q = crate::commands::electrical::expect_unit(*q, Unit::Hertz, "frequencies")?;
            let f = q.to_f64();
            if f < ts.freqs[0] || f > *ts.freqs.last().unwrap() {
                return Err(CommandError::invalid_args(
                    "touchstone.out_of_band",
                    format!("{q} is outside the file's band ({} Hz .. {} Hz)", ts.freqs[0], ts.freqs.last().unwrap()),
                )
                .with_hint("choose frequencies inside the band"));
            }
            freqs.push(f);
        }
        let params = wanted
            .iter()
            .map(|&(i, j)| {
                let idx = i * n + j;
                let pts: Vec<Point> = ts.freqs.iter().zip(&s).map(|(&f, m)| point(f, m[idx])).collect();
                let min = pts.iter().min_by(|a, b| a.mag.total_cmp(&b.mag)).unwrap().clone();
                let max = pts.iter().max_by(|a, b| a.mag.total_cmp(&b.mag)).unwrap().clone();
                let at = freqs
                    .iter()
                    .filter_map(|&f| touchstone::interpolate(&ts.freqs, &s, f, idx).map(|c| point(f, c)))
                    .collect();
                ParamSummary { name: name(i, j, n), min, max, at }
            })
            .collect();
        Ok(TouchstoneSummary {
            path: path.display().to_string(),
            version: ts.version.clone(),
            ports: n,
            parameter: ts.parameter,
            reference: ts.reference.clone(),
            points: ts.freqs.len(),
            band: [ts.freqs[0], *ts.freqs.last().unwrap()],
            noise_points: ts.noise.len(),
            params,
        })
    }

    fn summarize(o: &TouchstoneSummary) -> String {
        let hz = |f: f64| {
            let q = Quantity::from_f64(f, 0, Unit::Hertz).map(|q| q.to_string());
            q.unwrap_or_else(|| format!("{f} Hz"))
        };
        let mut s = format!(
            "{}: {}-port {:?}, Touchstone {}, {} points, {} .. {}",
            o.path,
            o.ports,
            o.parameter,
            o.version,
            o.points,
            hz(o.band[0]),
            hz(o.band[1])
        );
        for p in &o.params {
            s.push_str(&format!(
                "\n  {}: {:.2} dB at {} .. {:.2} dB at {}",
                p.name,
                p.min.db,
                hz(p.min.freq),
                p.max.db,
                hz(p.max.freq)
            ));
            for a in &p.at {
                s.push_str(&format!("\n    {}: {:.2} dB, {:.1}°", hz(a.freq), a.db, a.phase));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_names() {
        assert_eq!(param_index("S21", 2), Some((1, 0)));
        assert_eq!(param_index("s11", 2), Some((0, 0)));
        assert_eq!(param_index("S31", 2), None);
        assert_eq!(param_index("S1,10", 12), Some((0, 9)));
        assert_eq!(param_index("S110", 12), None);
        assert_eq!(name(9, 0, 12), "S10,1");
    }
}
