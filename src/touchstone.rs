//! Touchstone network parameter files (`.sNp`): reader, writer and S-parameter summaries.
//!
//! Implemented from the published IBIS Open Forum specifications: "Touchstone File Format
//! Specification" version 1.1 (2002) and version 2.0 (2009). Supported:
//!
//! - version 1.0/1.1: option line (`# GHz S MA R 50`, any order, defaults GHz/S/MA/R 50), data
//!   wrapped over any number of lines, the 2-port order N11 N21 N12 N22, 2-port noise data
//!   (detected by a frequency not above the previous one); Z and Y data are normalized to `R`;
//! - version 2.0 (and 2.1 files using only 2.0 keywords): `[Version]`, `[Number of Ports]`,
//!   `[Two-Port Data Order]`, `[Number of Frequencies]`, `[Number of Noise Frequencies]`,
//!   `[Reference]` (per port), `[Matrix Format]` (`Full`, `Lower`, `Upper`), `[Network Data]`,
//!   `[Noise Data]`, `[Begin Information]`/`[End Information]` (skipped), `[End]`; Z and Y data
//!   are in ohms and siemens;
//! - parameters S, Y, Z, H, G; formats RI, MA, DB; frequency units Hz, kHz, MHz, GHz.
//!
//! Mixed-mode data (`[Mixed-Mode Order]`) is refused. S-parameters are available for S files
//! and, by conversion with the reference impedances, Y and Z files.

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A complex number (re, im).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    /// Real part.
    pub re: f64,
    /// Imaginary part.
    pub im: f64,
}

impl Complex {
    /// `re + j im`.
    pub const fn new(re: f64, im: f64) -> Complex {
        Complex { re, im }
    }
    /// From magnitude and angle in degrees.
    pub fn polar_deg(mag: f64, deg: f64) -> Complex {
        let a = deg.to_radians();
        Complex::new(mag * a.cos(), mag * a.sin())
    }
    /// Magnitude.
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    /// Magnitude in dB (20 log10).
    pub fn db(self) -> f64 {
        20.0 * self.abs().log10()
    }
    /// Angle in degrees (−180, 180].
    pub fn arg_deg(self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }
    fn mul(self, o: Complex) -> Complex {
        Complex::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
    fn scale(self, k: f64) -> Complex {
        Complex::new(self.re * k, self.im * k)
    }
    fn div(self, o: Complex) -> Complex {
        let d = o.re * o.re + o.im * o.im;
        Complex::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}

/// Network parameter type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Parameter {
    /// Scattering.
    S,
    /// Admittance.
    Y,
    /// Impedance.
    Z,
    /// Hybrid-h (2-port).
    H,
    /// Hybrid-g (2-port).
    G,
}

/// Data format of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Format {
    /// Real, imaginary.
    RI,
    /// Magnitude, angle (degrees).
    MA,
    /// Magnitude in dB, angle (degrees).
    DB,
}

/// One noise parameter point (2-port files).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Noise {
    /// Frequency (Hz).
    pub freq: f64,
    /// Minimum noise figure (dB).
    pub nf_min_db: f64,
    /// Optimum source reflection coefficient.
    pub gamma_opt: Complex,
    /// Effective noise resistance (ohms).
    pub rn: f64,
}

/// A parse error, with the line it was found on (1-based; 0: the file as a whole).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code (`touchstone.syntax`, ...).
    pub code: &'static str,
    /// Line number.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 { write!(f, "line {}: {}", self.line, self.message) } else { f.write_str(&self.message) }
    }
}

fn err(code: &'static str, line: usize, message: impl Into<String>) -> Error {
    Error { code, line, message: message.into() }
}

/// A Touchstone file's content.
#[derive(Clone, Debug, PartialEq)]
pub struct Touchstone {
    /// Version: `1.1` (also for 1.0 files) or the `[Version]` given (`2.0`).
    pub version: String,
    /// Number of ports.
    pub ports: usize,
    /// Parameter type.
    pub parameter: Parameter,
    /// Data format the file used.
    pub format: Format,
    /// Reference impedance of every port (ohms).
    pub reference: Vec<f64>,
    /// Frequencies (Hz), increasing.
    pub freqs: Vec<f64>,
    /// Per frequency, the N×N matrix row-major (`[i * N + j]` = parameter (i+1, j+1)). Z and
    /// Y are in ohms and siemens (version 1 data denormalized).
    pub data: Vec<Vec<Complex>>,
    /// Noise parameters (2-port).
    pub noise: Vec<Noise>,
    /// Things the reader tolerated (unknown keywords).
    pub warnings: Vec<String>,
}

/// The port count from a file name's extension (`.s2p` → 2, `.S4P` → 4).
pub fn ports_from_extension(name: &str) -> Option<usize> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    let mid = ext.strip_prefix(|c: char| "syzhg".contains(c))?.strip_suffix('p')?;
    mid.parse().ok().filter(|&n| n > 0)
}

struct Opts {
    unit: f64,
    parameter: Parameter,
    format: Format,
    r: f64,
}

fn option_line(line: &str, n: usize) -> Result<Opts, Error> {
    let mut o = Opts { unit: 1e9, parameter: Parameter::S, format: Format::MA, r: 50.0 };
    let toks: Vec<String> = line.trim_start_matches('#').split_whitespace().map(str::to_ascii_uppercase).collect();
    let mut i = 0;
    while i < toks.len() {
        match toks[i].as_str() {
            "HZ" => o.unit = 1.0,
            "KHZ" => o.unit = 1e3,
            "MHZ" => o.unit = 1e6,
            "GHZ" => o.unit = 1e9,
            "S" => o.parameter = Parameter::S,
            "Y" => o.parameter = Parameter::Y,
            "Z" => o.parameter = Parameter::Z,
            "H" => o.parameter = Parameter::H,
            "G" => o.parameter = Parameter::G,
            "RI" => o.format = Format::RI,
            "MA" => o.format = Format::MA,
            "DB" => o.format = Format::DB,
            "R" => {
                i += 1;
                o.r = toks.get(i).and_then(|t| t.parse::<f64>().ok()).filter(|r| *r > 0.0).ok_or_else(|| {
                    err("touchstone.bad_option", n, "`R` must be followed by a positive reference impedance")
                })?;
            }
            t => return Err(err("touchstone.bad_option", n, format!("unknown option `{t}` in the option line"))),
        }
        i += 1;
    }
    Ok(o)
}

fn value(f: Format, a: f64, b: f64) -> Complex {
    match f {
        Format::RI => Complex::new(a, b),
        Format::MA => Complex::polar_deg(a, b),
        Format::DB => Complex::polar_deg(10f64.powf(a / 20.0), b),
    }
}

/// Parses a Touchstone file. `ports` is required for version 1 files (from the extension, see
/// [`ports_from_extension`]); version 2 files give it themselves (a different `ports` is an
/// error).
pub fn parse(text: &str, ports: Option<usize>) -> Result<Touchstone, Error> {
    // Tokens of the data sections, with their line numbers.
    let mut version = "1.1".to_string();
    let mut v2 = false;
    let mut opts: Option<Opts> = None;
    let mut n_ports = ports;
    let mut two_port_12_21 = false;
    let mut two_port_order_seen = false;
    let mut n_freq: Option<usize> = None;
    let mut n_noise: Option<usize> = None;
    let mut reference: Vec<f64> = Vec::new();
    let mut reference_open = false;
    let mut matrix = "FULL".to_string();
    let mut warnings = Vec::new();
    #[derive(PartialEq, Clone, Copy)]
    enum Section {
        Header,
        Network,
        Noise,
        Info,
        End,
    }
    let mut section = Section::Header;
    let mut net_tokens: Vec<(f64, usize)> = Vec::new();
    let mut noise_tokens: Vec<(f64, usize)> = Vec::new();
    let mut first_content = true;
    for (k, raw) in text.lines().enumerate() {
        let ln = k + 1;
        let line = raw.split('!').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if section == Section::Info {
            if line.to_ascii_uppercase().starts_with("[END INFORMATION]") {
                section = Section::Header;
            }
            continue;
        }
        if section == Section::End {
            return Err(err("touchstone.syntax", ln, "content after [End]"));
        }
        if line.starts_with('[') {
            let close = line.find(']').ok_or_else(|| err("touchstone.syntax", ln, "unterminated keyword"))?;
            let kw = line[1..close].trim().to_ascii_uppercase();
            let kw = kw.split_whitespace().collect::<Vec<_>>().join(" ");
            let rest = line[close + 1..].trim();
            if first_content && kw != "VERSION" {
                return Err(err("touchstone.syntax", ln, "a version 2 file starts with [Version]"));
            }
            first_content = false;
            reference_open = false;
            match kw.as_str() {
                "VERSION" => {
                    let v: f64 = rest.parse().map_err(|_| err("touchstone.syntax", ln, "bad [Version]"))?;
                    if v < 2.0 {
                        return Err(err("touchstone.syntax", ln, "[Version] is 2.0 or later"));
                    }
                    version = rest.to_string();
                    v2 = true;
                }
                "NUMBER OF PORTS" => {
                    let n: usize = rest
                        .parse()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| err("touchstone.syntax", ln, "bad [Number of Ports]"))?;
                    if let Some(p) = ports
                        && p != n
                    {
                        return Err(err(
                            "touchstone.port_mismatch",
                            ln,
                            format!("the file has {n} ports, {p} were expected (file extension)"),
                        ));
                    }
                    n_ports = Some(n);
                }
                "TWO-PORT DATA ORDER" => {
                    two_port_order_seen = true;
                    two_port_12_21 = match rest {
                        "12_21" => true,
                        "21_12" => false,
                        _ => return Err(err("touchstone.syntax", ln, "[Two-Port Data Order] is 12_21 or 21_12")),
                    };
                }
                "NUMBER OF FREQUENCIES" => {
                    n_freq =
                        Some(rest.parse().map_err(|_| err("touchstone.syntax", ln, "bad [Number of Frequencies]"))?);
                }
                "NUMBER OF NOISE FREQUENCIES" => {
                    n_noise = Some(
                        rest.parse().map_err(|_| err("touchstone.syntax", ln, "bad [Number of Noise Frequencies]"))?,
                    );
                }
                "REFERENCE" => {
                    for t in rest.split_whitespace() {
                        reference.push(t.parse().map_err(|_| err("touchstone.syntax", ln, "bad [Reference] value"))?);
                    }
                    reference_open = true;
                }
                "MATRIX FORMAT" => {
                    matrix = rest.to_ascii_uppercase();
                    if !["FULL", "LOWER", "UPPER"].contains(&matrix.as_str()) {
                        return Err(err("touchstone.syntax", ln, "[Matrix Format] is Full, Lower or Upper"));
                    }
                }
                "MIXED-MODE ORDER" => {
                    return Err(err("touchstone.unsupported", ln, "mixed-mode data is not supported"));
                }
                "NETWORK DATA" => section = Section::Network,
                "NOISE DATA" => section = Section::Noise,
                "BEGIN INFORMATION" => section = Section::Info,
                "END" => section = Section::End,
                other => warnings.push(format!("line {ln}: keyword [{other}] ignored")),
            }
            continue;
        }
        first_content = false;
        if line.starts_with('#') {
            if opts.is_none() {
                opts = Some(option_line(line, ln)?);
            }
            continue;
        }
        if reference_open && section == Section::Header {
            for t in line.split_whitespace() {
                reference.push(t.parse().map_err(|_| err("touchstone.syntax", ln, "bad [Reference] value"))?);
            }
            continue;
        }
        if v2 && section == Section::Header {
            return Err(err("touchstone.syntax", ln, "data before [Network Data]"));
        }
        let into = if section == Section::Noise { &mut noise_tokens } else { &mut net_tokens };
        for t in line.split_whitespace() {
            let v: f64 = t.parse().map_err(|_| err("touchstone.syntax", ln, format!("`{t}` is not a number")))?;
            into.push((v, ln));
        }
    }
    if v2 && section != Section::End {
        warnings.push("no [End] keyword".into());
    }
    let opts = opts.unwrap_or(Opts { unit: 1e9, parameter: Parameter::S, format: Format::MA, r: 50.0 });
    let n = n_ports.ok_or_else(|| {
        err("touchstone.no_ports", 0, "the number of ports is unknown (a version 1 file is named .sNp)")
    })?;
    if v2 && n == 2 && !two_port_order_seen {
        warnings.push("2-port file without [Two-Port Data Order]: 21_12 assumed".into());
    }
    if matches!(opts.parameter, Parameter::H | Parameter::G) && n != 2 {
        return Err(err("touchstone.syntax", 0, "H and G parameters are defined for 2-port networks only"));
    }
    if reference.is_empty() {
        reference = vec![opts.r; n];
    } else if reference.len() != n {
        return Err(err("touchstone.syntax", 0, format!("[Reference] has {} values for {n} ports", reference.len())));
    }
    // Entries per frequency.
    let cells: Vec<(usize, usize)> = (0..n)
        .flat_map(|i| (0..n).map(move |j| (i, j)))
        .filter(|&(i, j)| match matrix.as_str() {
            "LOWER" => j <= i,
            "UPPER" => j >= i,
            _ => true,
        })
        .collect();
    let per = 1 + 2 * cells.len();
    let mut freqs = Vec::new();
    let mut data = Vec::new();
    let mut noise_v1: Vec<(f64, usize)> = Vec::new();
    let mut i = 0;
    while i < net_tokens.len() {
        let (f, ln) = net_tokens[i];
        if !v2 && n == 2 && freqs.last().is_some_and(|&last: &f64| f * opts.unit <= last) {
            noise_v1 = net_tokens[i..].to_vec();
            break;
        }
        if i + per > net_tokens.len() {
            return Err(err(
                "touchstone.truncated",
                ln,
                format!("incomplete data at frequency {f}: {} values for {per}", net_tokens.len() - i),
            ));
        }
        let fh = f * opts.unit;
        if freqs.last().is_some_and(|&last| fh <= last) {
            return Err(err("touchstone.syntax", ln, "frequencies must increase"));
        }
        let mut m = vec![Complex::default(); n * n];
        for (c, &(r, col)) in cells.iter().enumerate() {
            let (a, b) = (net_tokens[i + 1 + 2 * c].0, net_tokens[i + 2 + 2 * c].0);
            // The 2-port order N11 N21 N12 N22 (version 1, and 21_12) is column-major.
            let (r, col) = if n == 2 && !two_port_12_21 { (col, r) } else { (r, col) };
            m[r * n + col] = value(opts.format, a, b);
        }
        if matrix != "FULL" {
            for r in 0..n {
                for col in 0..n {
                    let (a, b) = if matrix == "LOWER" { (r, col) } else { (col, r) };
                    if b > a {
                        m[a * n + b] = m[b * n + a];
                    }
                }
            }
        }
        // Version 1 Z and Y data are normalized to R.
        if !v2 {
            let k = match opts.parameter {
                Parameter::Z => opts.r,
                Parameter::Y => 1.0 / opts.r,
                _ => 1.0,
            };
            for v in &mut m {
                *v = v.scale(k);
            }
        }
        freqs.push(fh);
        data.push(m);
        i += per;
    }
    if let Some(nf) = n_freq
        && nf != freqs.len()
    {
        return Err(err(
            "touchstone.count_mismatch",
            0,
            format!("[Number of Frequencies] is {nf}, the file has {}", freqs.len()),
        ));
    }
    let noise_src = if v2 { noise_tokens } else { noise_v1 };
    let mut noise = Vec::new();
    for c in noise_src.chunks(5) {
        if c.len() < 5 {
            return Err(err("touchstone.truncated", c[0].1, "incomplete noise data (5 values per frequency)"));
        }
        let rn = if v2 { c[4].0 } else { c[4].0 * opts.r };
        noise.push(Noise {
            freq: c[0].0 * opts.unit,
            nf_min_db: c[1].0,
            gamma_opt: Complex::polar_deg(c[2].0, c[3].0),
            rn,
        });
    }
    if let Some(nn) = n_noise
        && nn != noise.len()
    {
        return Err(err(
            "touchstone.count_mismatch",
            0,
            format!("[Number of Noise Frequencies] is {nn}, the file has {}", noise.len()),
        ));
    }
    if freqs.is_empty() {
        return Err(err("touchstone.empty", 0, "no network data"));
    }
    Ok(Touchstone {
        version,
        ports: n,
        parameter: opts.parameter,
        format: opts.format,
        reference,
        freqs,
        data,
        noise,
        warnings,
    })
}

/// Solves `a x = b` for square complex matrices (row-major, size n), by Gauss-Jordan
/// elimination with partial pivoting; `None` when singular.
fn solve(mut a: Vec<Complex>, mut b: Vec<Complex>, n: usize) -> Option<Vec<Complex>> {
    for c in 0..n {
        let p = (c..n).max_by(|&x, &y| a[x * n + c].abs().total_cmp(&a[y * n + c].abs()))?;
        if a[p * n + c].abs() < 1e-300 {
            return None;
        }
        if p != c {
            for k in 0..n {
                a.swap(p * n + k, c * n + k);
                b.swap(p * n + k, c * n + k);
            }
        }
        let piv = a[c * n + c];
        for k in 0..n {
            a[c * n + k] = a[c * n + k].div(piv);
            b[c * n + k] = b[c * n + k].div(piv);
        }
        for r in 0..n {
            if r != c {
                let f = a[r * n + c];
                if f == Complex::default() {
                    continue;
                }
                for k in 0..n {
                    a[r * n + k] = a[r * n + k].sub(f.mul(a[c * n + k]));
                    b[r * n + k] = b[r * n + k].sub(f.mul(b[c * n + k]));
                }
            }
        }
    }
    Some(b)
}

impl Touchstone {
    /// S-parameters per frequency (row-major N×N), converting Z and Y data with the reference
    /// impedances: with z = R^−½ Z R^−½, S = (z + I)^−1 (z − I); with y = R^½ Y R^½,
    /// S = (I + y)^−1 (I − y). (Diagonal real references: both factors commute with the
    /// normalization, so the left inverse equals the usual right one.)
    pub fn s_params(&self) -> Result<Vec<Vec<Complex>>, Error> {
        let n = self.ports;
        let sq: Vec<f64> = self.reference.iter().map(|r| r.sqrt()).collect();
        let eye = |i: usize, j: usize| if i == j { Complex::new(1.0, 0.0) } else { Complex::default() };
        match self.parameter {
            Parameter::S => Ok(self.data.clone()),
            Parameter::Z | Parameter::Y => {
                let mut out = Vec::with_capacity(self.data.len());
                for (k, m) in self.data.iter().enumerate() {
                    let norm: Vec<Complex> = (0..n * n)
                        .map(|x| {
                            let (i, j) = (x / n, x % n);
                            if self.parameter == Parameter::Z {
                                m[x].scale(1.0 / (sq[i] * sq[j]))
                            } else {
                                m[x].scale(sq[i] * sq[j])
                            }
                        })
                        .collect();
                    // Z: (z + I) S = z − I; Y: (I + y) S = I − y.
                    let a: Vec<Complex> = (0..n * n).map(|x| eye(x / n, x % n).add(norm[x])).collect();
                    let b: Vec<Complex> = (0..n * n)
                        .map(|x| match self.parameter {
                            Parameter::Z => norm[x].sub(eye(x / n, x % n)),
                            _ => eye(x / n, x % n).sub(norm[x]),
                        })
                        .collect();
                    let s = solve(a, b, n).ok_or_else(|| {
                        err(
                            "touchstone.singular",
                            0,
                            format!("cannot convert to S-parameters at {} Hz (singular matrix)", self.freqs[k]),
                        )
                    })?;
                    out.push(s);
                }
                Ok(out)
            }
            Parameter::H | Parameter::G => {
                Err(err("touchstone.unsupported", 0, "H and G parameters are not converted to S-parameters"))
            }
        }
    }

    /// Writes the data in RI format: version 1.1 when every port has the same reference, else
    /// 2.0 (Z and Y data normalized for version 1 as the specification requires). Noise data
    /// is written for 2-port files.
    pub fn write(&self) -> String {
        let n = self.ports;
        let v2 = self.reference.iter().any(|r| *r != self.reference[0]);
        let mut o = String::new();
        let p = format!("{:?}", self.parameter);
        if v2 {
            o.push_str("[Version] 2.0\n");
        }
        let _ = writeln!(o, "# Hz {p} RI R {}", self.reference[0]);
        if v2 {
            let _ = writeln!(o, "[Number of Ports] {n}");
            if n == 2 {
                o.push_str("[Two-Port Data Order] 21_12\n");
            }
            let _ = writeln!(o, "[Number of Frequencies] {}", self.freqs.len());
            if !self.noise.is_empty() {
                let _ = writeln!(o, "[Number of Noise Frequencies] {}", self.noise.len());
            }
            let r: Vec<String> = self.reference.iter().map(|r| format!("{r}")).collect();
            let _ = writeln!(o, "[Reference] {}", r.join(" "));
            o.push_str("[Network Data]\n");
        }
        let k = match (v2, self.parameter) {
            (false, Parameter::Z) => 1.0 / self.reference[0],
            (false, Parameter::Y) => self.reference[0],
            _ => 1.0,
        };
        let pair = |c: Complex| format!("{} {}", c.re * k, c.im * k);
        for (f, m) in self.freqs.iter().zip(&self.data) {
            if n == 2 {
                let _ = writeln!(o, "{f} {} {} {} {}", pair(m[0]), pair(m[2]), pair(m[1]), pair(m[3]));
                continue;
            }
            for i in 0..n {
                for c in (0..n).step_by(4) {
                    let lead = if i == 0 && c == 0 { f.to_string() } else { " ".into() };
                    let vals: Vec<String> = (c..(c + 4).min(n)).map(|j| pair(m[i * n + j])).collect();
                    let _ = writeln!(o, "{lead} {}", vals.join(" "));
                }
            }
        }
        if !self.noise.is_empty() {
            if v2 {
                o.push_str("[Noise Data]\n");
            }
            for z in &self.noise {
                let rn = if v2 { z.rn } else { z.rn / self.reference[0] };
                let _ =
                    writeln!(o, "{} {} {} {} {}", z.freq, z.nf_min_db, z.gamma_opt.abs(), z.gamma_opt.arg_deg(), rn);
            }
        }
        if v2 {
            o.push_str("[End]\n");
        }
        o
    }
}

/// Linear interpolation of row-major matrices at `f` (Hz) between the two nearest frequencies
/// (real and imaginary parts separately); `None` outside the file's range.
pub fn interpolate(freqs: &[f64], data: &[Vec<Complex>], f: f64, idx: usize) -> Option<Complex> {
    if freqs.is_empty() || f < freqs[0] || f > *freqs.last()? {
        return None;
    }
    let k = freqs.partition_point(|&x| x < f);
    if freqs[k] == f || k == 0 {
        return Some(data[k][idx]);
    }
    let (f0, f1) = (freqs[k - 1], freqs[k]);
    let t = (f - f0) / (f1 - f0);
    let (a, b) = (data[k - 1][idx], data[k][idx]);
    Some(a.add(b.sub(a).scale(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn extension() {
        assert_eq!(ports_from_extension("a/b/line.s2p"), Some(2));
        assert_eq!(ports_from_extension("X.S12P"), Some(12));
        assert_eq!(ports_from_extension("x.z1p"), Some(1));
        assert_eq!(ports_from_extension("x.txt"), None);
        assert_eq!(ports_from_extension("x.s0p"), None);
    }

    #[test]
    fn v1_two_port_ma_with_noise() {
        // The 2-port order is N11 N21 N12 N22; noise data starts when the frequency drops.
        let t = "! a comment\n# GHz S MA R 50\n1 0.5 -90 0.9 10 0.01 20 0.4 -45\n2 0.6 -100 ! inline\n 0.8 5 0.02 30 0.3 -50\n1 2.0 0.5 120 0.4\n2 2.5 0.4 130 0.5\n";
        let ts = parse(t, Some(2)).unwrap();
        assert_eq!(ts.version, "1.1");
        assert_eq!(ts.freqs, vec![1e9, 2e9]);
        let s21 = ts.data[0][2];
        assert!(close(s21.abs(), 0.9, 1e-12) && close(s21.arg_deg(), 10.0, 1e-9));
        let s12 = ts.data[0][1];
        assert!(close(s12.abs(), 0.01, 1e-12));
        assert_eq!(ts.noise.len(), 2);
        assert!(close(ts.noise[1].rn, 0.5 * 50.0, 1e-12));
        assert!(close(ts.noise[0].freq, 1e9, 0.0));
    }

    #[test]
    fn v1_defaults_and_db() {
        let t = "# \n100 -20 0\n";
        let ts = parse(t, Some(1)).unwrap();
        assert_eq!((ts.parameter, ts.format), (Parameter::S, Format::MA));
        assert_eq!(ts.freqs, vec![100e9]);
        let t = "# MHz S DB R 75\n100 -6.0206 180\n";
        let ts = parse(t, Some(1)).unwrap();
        assert!(close(ts.data[0][0].abs(), 0.5, 1e-5));
        assert!(close(ts.data[0][0].re, -0.5, 1e-5));
        assert_eq!(ts.reference, vec![75.0]);
    }

    #[test]
    fn v1_four_port_row_wrapping() {
        // 4-port: each matrix row on its own line.
        let mut t = String::from("# Hz S RI R 50\n");
        for f in [1.0, 2.0] {
            for i in 0..4 {
                let vals: Vec<String> = (0..4).map(|j| format!("{} {}", i * 4 + j, f)).collect();
                if i == 0 {
                    t += &format!("{f} {}\n", vals.join(" "));
                } else {
                    t += &format!("  {}\n", vals.join(" "));
                }
            }
        }
        let ts = parse(&t, Some(4)).unwrap();
        assert_eq!(ts.data.len(), 2);
        assert_eq!(ts.data[1][6], Complex::new(6.0, 2.0), "S23 row-major");
    }

    #[test]
    fn v1_z_is_normalized() {
        let t = "# Hz Z RI R 50\n1 2 0\n";
        let ts = parse(t, Some(1)).unwrap();
        assert_eq!(ts.data[0][0], Complex::new(100.0, 0.0));
        let s = ts.s_params().unwrap();
        assert!(close(s[0][0].re, 1.0 / 3.0, 1e-12), "(100-50)/(100+50)");
    }

    #[test]
    fn v2_full_and_lower() {
        let t = "[Version] 2.0\n# GHz S RI R 50\n[Number of Ports] 2\n[Two-Port Data Order] 12_21\n\
                 [Number of Frequencies] 1\n[Reference] 50\n 75\n[Network Data]\n1 0.1 0 0.2 0 0.3 0 0.4 0\n[End]\n";
        let ts = parse(t, None).unwrap();
        assert_eq!(ts.version, "2.0");
        assert_eq!(ts.reference, vec![50.0, 75.0]);
        assert_eq!(ts.data[0][1], Complex::new(0.2, 0.0), "12_21: S12 second");
        assert_eq!(ts.data[0][2], Complex::new(0.3, 0.0));
        let t = "[Version] 2.0\n# Hz Z RI\n[Number of Ports] 3\n[Number of Frequencies] 1\n[Matrix Format] Lower\n\
                 [Network Data]\n5 11 0\n 21 0 22 0\n 31 0 32 0 33 0\n[End]\n";
        let ts = parse(t, Some(3)).unwrap();
        assert_eq!(ts.data[0][1], Complex::new(21.0, 0.0), "upper filled from lower");
        assert_eq!(ts.data[0][5], Complex::new(32.0, 0.0));
        assert_eq!(ts.data[0][0], Complex::new(11.0, 0.0), "version 2 Z is not normalized");
    }

    #[test]
    fn v2_noise_and_info() {
        let t = "[Version] 2.0\n# GHz S MA R 50\n[Number of Ports] 2\n[Two-Port Data Order] 21_12\n\
                 [Number of Frequencies] 1\n[Number of Noise Frequencies] 1\n[Begin Information]\nanything [x]\n[End Information]\n\
                 [Network Data]\n1 0 0 1 0 1 0 0 0\n[Noise Data]\n1 1.5 0.3 45 12.5\n[End]\n";
        let ts = parse(t, None).unwrap();
        assert_eq!(ts.noise[0].rn, 12.5);
        assert!(close(ts.data[0][2].abs(), 1.0, 0.0));
    }

    #[test]
    fn errors() {
        let e = |t: &str, p| parse(t, p).unwrap_err().code;
        assert_eq!(e("# Hz S RI\n1 0 0\n", None), "touchstone.no_ports");
        assert_eq!(e("# Hz S RI\n1 0 0 0\n", Some(2)), "touchstone.truncated");
        assert_eq!(e("# Hz S RI\n2 0 0\n1 0 0\n", Some(1)), "touchstone.syntax");
        assert_eq!(e("# Hz Q RI\n1 0 0\n", Some(1)), "touchstone.bad_option");
        assert_eq!(
            e("[Version] 2.0\n# Hz S RI\n[Number of Ports] 2\n[Network Data]\n", Some(3)),
            "touchstone.port_mismatch"
        );
        assert_eq!(
            e(
                "[Version] 2.0\n# Hz S RI\n[Number of Ports] 1\n[Number of Frequencies] 2\n[Network Data]\n1 0 0\n[End]\n",
                None
            ),
            "touchstone.count_mismatch"
        );
        assert_eq!(e("[Version] 2.0\n[Number of Ports] 1\n[Mixed-Mode Order] D1,2\n", None), "touchstone.unsupported");
        assert_eq!(e("# Hz S RI\n1 x 0\n", Some(1)), "touchstone.syntax");
    }

    #[test]
    fn y_converts_to_s() {
        // A series admittance network of a matched 2-port: Y of a series 50 Ω resistor.
        let g = 1.0 / 50.0;
        let t = format!(
            "[Version] 2.0\n# Hz Y RI R 50\n[Number of Ports] 2\n[Two-Port Data Order] 21_12\n[Number of Frequencies] 1\n[Network Data]\n1 {g} 0 {} 0 {} 0 {g} 0\n[End]\n",
            -g, -g
        );
        let s = parse(&t, None).unwrap().s_params().unwrap();
        // Series R between 50 Ω ports: S11 = R/(R+100) = 1/3, S21 = 100/(R+100) = 2/3.
        assert!(close(s[0][0].re, 1.0 / 3.0, 1e-12) && close(s[0][2].re, 2.0 / 3.0, 1e-12), "{s:?}");
    }

    #[test]
    fn round_trip() {
        let t = "# GHz S MA R 50\n1 0.5 -90 0.9 10 0.01 20 0.4 -45\n2 0.6 -100 0.8 5 0.02 30 0.3 -50\n";
        let a = parse(t, Some(2)).unwrap();
        let b = parse(&a.write(), Some(2)).unwrap();
        for (x, y) in a.data.iter().flatten().zip(b.data.iter().flatten()) {
            assert!(close(x.re, y.re, 1e-12) && close(x.im, y.im, 1e-12));
        }
        let mut c = a.clone();
        c.reference = vec![50.0, 75.0];
        let d = parse(&c.write(), None).unwrap();
        assert_eq!(d.version, "2.0");
        assert_eq!(d.reference, vec![50.0, 75.0]);
        assert_eq!(d.data, c.data);
    }

    #[test]
    fn interpolates() {
        let f = [1.0, 3.0];
        let d = vec![vec![Complex::new(0.0, 0.0)], vec![Complex::new(1.0, -2.0)]];
        assert_eq!(interpolate(&f, &d, 2.0, 0), Some(Complex::new(0.5, -1.0)));
        assert_eq!(interpolate(&f, &d, 3.0, 0), Some(Complex::new(1.0, -2.0)));
        assert_eq!(interpolate(&f, &d, 4.0, 0), None);
    }
}
