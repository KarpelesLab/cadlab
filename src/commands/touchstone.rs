//! `touchstone.summary`: read a Touchstone file (`.sNp`, versions 1.1 and 2.0) and summarize its
//! S-parameters, to check simulation or measurement results (`docs/ELECTRICAL.md`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::touchstone::{self, Checks, Complex, Parameter, Reflection};
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
    /// Band of interest: the best and worst values are over these two frequencies ("2.4GHz",
    /// "2.4835GHz") instead of the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<[Quantity; 2]>,
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
    /// For a reflection (S11, S22): the impedance looking into the port, VSWR, return loss and
    /// mismatch loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflection: Option<Reflection>,
    /// For a transmission (S21) at a requested frequency: the loss with the mismatch at the driven
    /// port taken out, −10 log10(|S21|² / (1 − |S11|²)) in dB: what the network dissipates and
    /// radiates, or sends to other ports. |S21| between two terminations mixes it with the
    /// reflection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_loss_db: Option<f64>,
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
    /// The band of the best and worst values, when `band` narrowed it (Hz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<[f64; 2]>,
    /// The parameters.
    pub params: Vec<ParamSummary>,
    /// Passivity and reciprocity of the data over the whole file.
    pub checks: Checks,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn round(v: f64, decimals: i32) -> f64 {
    let k = 10f64.powi(decimals);
    let r = (v * k).round() / k;
    if r == 0.0 { 0.0 } else { r }
}

fn point(f: f64, c: Complex, z0: Option<f64>) -> Point {
    Point {
        freq: f,
        db: round(c.db().max(-999.0), 3),
        mag: round(c.abs(), 6),
        phase: round(c.arg_deg(), 2),
        reflection: z0.map(|z| touchstone::reflection(c, z)),
        available_loss_db: None,
    }
}

/// What to summarize: frequencies to report, the band of the extremes, reference impedances.
pub(crate) struct View<'a> {
    pub(crate) at: &'a [f64],
    pub(crate) band: Option<[f64; 2]>,
    pub(crate) reference: &'a [f64],
}

/// Summary of S(i+1, j+1) of an `n`-port: extremes (over `view.band`, its ends interpolated, else
/// the whole file) and values at `view.at` (Hz, inside the file's band).
pub(crate) fn summarize(
    freqs: &[f64],
    s: &[Vec<Complex>],
    i: usize,
    j: usize,
    n: usize,
    view: &View<'_>,
) -> ParamSummary {
    let idx = i * n + j;
    let z0 = (i == j).then(|| view.reference.get(i).or(view.reference.first()).copied().unwrap_or(50.0));
    let mut pts: Vec<Point> = Vec::new();
    if let Some([lo, hi]) = view.band {
        for f in [lo, hi] {
            pts.extend(touchstone::interpolate(freqs, s, f, idx).map(|c| point(f, c, z0)));
        }
    }
    let inside = |f: f64| view.band.is_none_or(|[lo, hi]| f >= lo && f <= hi);
    pts.extend(freqs.iter().zip(s).filter(|(f, _)| inside(**f)).map(|(&f, m)| point(f, m[idx], z0)));
    let min = pts.iter().min_by(|a, b| a.mag.total_cmp(&b.mag)).unwrap().clone();
    let max = pts.iter().max_by(|a, b| a.mag.total_cmp(&b.mag)).unwrap().clone();
    let at = view
        .at
        .iter()
        .filter_map(|&f| {
            let c = touchstone::interpolate(freqs, s, f, idx)?;
            let mut pt = point(f, c, z0);
            if i != j {
                let g = touchstone::interpolate(freqs, s, f, j * n + j)?.abs();
                let available = 1.0 - g * g;
                if available > 0.0 && c.abs() > 0.0 {
                    pt.available_loss_db = Some(round(-10.0 * (c.abs() * c.abs() / available).log10(), 3));
                }
            }
            Some(pt)
        })
        .collect();
    ParamSummary { name: name(i, j, n), min, max, at }
}

/// A frequency as text ("2.44GHz").
pub(crate) fn hz(f: f64) -> String {
    Quantity::from_f64(f, 0, Unit::Hertz).map_or_else(|| format!("{f} Hz"), |q| q.to_string())
}

/// `band` as Hz, checked against the file's or simulation's band.
pub(crate) fn band_hz(band: Option<[Quantity; 2]>, lo: f64, hi: f64) -> Result<Option<[f64; 2]>, CommandError> {
    let Some([a, b]) = band else { return Ok(None) };
    let a = crate::commands::electrical::expect_unit(a, Unit::Hertz, "band")?.to_f64();
    let b = crate::commands::electrical::expect_unit(b, Unit::Hertz, "band")?.to_f64();
    if a >= b || a < lo || b > hi {
        return Err(CommandError::invalid_args(
            "touchstone.out_of_band",
            format!("`band` must be two increasing frequencies inside {} .. {}", hz(lo), hz(hi)),
        )
        .with_hint("choose frequencies inside the band"));
    }
    Ok(Some([a, b]))
}

/// Warnings when the data cannot be physical. `prefix`: the code's group; `hint`: what to do.
pub(crate) fn check_diagnostics(c: &Checks, prefix: &str, hint: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if c.max_power > 1.02 {
        out.push(
            Diagnostic::warning(
                format!("{prefix}.not_passive"),
                format!(
                    "the network gains power: Σ|Sij|² reaches {} at {} (a passive network stays at or below 1)",
                    c.max_power,
                    hz(c.max_power_freq)
                ),
            )
            .with_hint(hint.to_string()),
        );
    }
    if c.max_asymmetry > 0.03 {
        out.push(
            Diagnostic::warning(
                format!("{prefix}.not_reciprocal"),
                format!(
                    "S is not symmetric: |Sij − Sji| reaches {} at {} (a board network is reciprocal)",
                    c.max_asymmetry,
                    hz(c.max_asymmetry_freq)
                ),
            )
            .with_hint(hint.to_string()),
        );
    }
    out
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

/// One point as text; a reflection adds the impedance and VSWR.
pub(crate) fn point_text(a: &Point) -> String {
    let mut s = format!("{}: {:.2} dB, {:.1}°", hz(a.freq), a.db, a.phase);
    if let Some(r) = &a.reflection {
        if let (Some(re), Some(im)) = (r.z_re, r.z_im) {
            s.push_str(&format!(", Z {re:.1} {} j{:.1} Ω", if im < 0.0 { "-" } else { "+" }, im.abs()));
        }
        if let Some(v) = r.vswr {
            s.push_str(&format!(", VSWR {v:.2}"));
        }
    }
    if let Some(l) = a.available_loss_db {
        s.push_str(&format!(" ({l:.2} dB without the mismatch)"));
    }
    s
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
        let band = band_hz(self.band, ts.freqs[0], *ts.freqs.last().unwrap())?;
        let view = View { at: &freqs, band, reference: &ts.reference };
        let params = wanted.iter().map(|&(i, j)| summarize(&ts.freqs, &s, i, j, n, &view)).collect();
        let checks = touchstone::checks(&ts.freqs, &s, n);
        for d in check_diagnostics(
            &checks,
            "touchstone",
            "measured data can be slightly off; a simulation: check its convergence, the absorbing layer's distance and \
             the mesh",
        ) {
            ctx.report(d);
        }
        Ok(TouchstoneSummary {
            path: path.display().to_string(),
            version: ts.version.clone(),
            ports: n,
            parameter: ts.parameter,
            reference: ts.reference.clone(),
            points: ts.freqs.len(),
            band: [ts.freqs[0], *ts.freqs.last().unwrap()],
            noise_points: ts.noise.len(),
            over: band,
            params,
            checks,
        })
    }

    fn summarize(o: &TouchstoneSummary) -> String {
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
                s.push_str(&format!("\n    {}", point_text(a)));
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

    #[test]
    fn reflections_mean_what_they_say() {
        // Γ = 1/3 on 50 Ω is 100 Ω, VSWR 2, 9.54 dB return loss, 0.51 dB mismatch loss.
        let r = touchstone::reflection(Complex::new(1.0 / 3.0, 0.0), 50.0);
        assert_eq!((r.z_re, r.z_im), (Some(100.0), Some(0.0)));
        assert_eq!(r.vswr, Some(2.0));
        assert!((r.return_loss_db - 9.542).abs() < 1e-3, "{r:?}");
        assert!((r.mismatch_loss_db.unwrap() - 0.5115).abs() < 1e-3, "{r:?}");
        // An open: no impedance, no VSWR.
        let r = touchstone::reflection(Complex::new(1.0, 0.0), 50.0);
        assert_eq!((r.z_re, r.vswr, r.mismatch_loss_db), (None, None, None));
        // j50 Ω.
        let g = Complex::new(0.0, 1.0);
        let r = touchstone::reflection(g, 50.0);
        assert_eq!((r.z_re, r.z_im), (Some(0.0), Some(50.0)));
    }

    #[test]
    fn checks_flag_gain_and_asymmetry() {
        let c = |re: f64| Complex::new(re, 0.0);
        let freqs = [1e9, 2e9];
        let good = vec![c(0.1), c(0.9), c(0.9), c(0.1)];
        let bad = vec![c(0.1), c(1.2), c(0.9), c(0.1)];
        let k = touchstone::checks(&freqs, &[good.clone(), good.clone()], 2);
        assert!(check_diagnostics(&k, "sim", "h").is_empty(), "{k:?}");
        let k = touchstone::checks(&freqs, &[good, bad], 2);
        assert_eq!(k.max_power_freq, 2e9);
        assert_eq!(k.max_asymmetry, 0.3);
        let codes: Vec<String> = check_diagnostics(&k, "sim", "h").iter().map(|d| d.code.to_string()).collect();
        assert_eq!(codes, ["sim.not_passive", "sim.not_reciprocal"]);
    }
}
