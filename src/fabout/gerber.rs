//! Low-level Gerber X2 writer (Ucamco "The Gerber Layer Format Specification", revision 2026.05).
//!
//! Format 4.6 in millimeters (`%FSLAX46Y46*%`, `%MOMM*%`): one coordinate unit is 1e-6 mm, which
//! is exactly one nanometer, so board coordinates are written without rounding. Apertures are
//! collected while the body is built and written in the header, each preceded by its
//! `.AperFunction` (spec 5.6.10), so every aperture is defined before use. Object attributes
//! (`.N`, `.P`, `.C`, `.Cxxx`, spec 5.6.13-5.6.16) are emitted only when they change.
//! Arcs use multi-quadrant mode (`G75`) and are always written as two halves through their
//! midpoint, which keeps them numerically stable (spec 4.7.2).

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A point in nanometers (= Gerber 4.6 mm coordinate units).
pub type Xy = (i64, i64);

/// Formats nanometers as a millimeter decimal with up to 6 decimals (`1.2`, `-0.000001`, `0`).
pub fn mm(nm: i64) -> String {
    let sign = if nm < 0 { "-" } else { "" };
    let a = nm.unsigned_abs();
    let (int, frac) = (a / 1_000_000, a % 1_000_000);
    if frac == 0 {
        return format!("{sign}{int}");
    }
    let f = format!("{frac:06}");
    format!("{sign}{int}.{}", f.trim_end_matches('0'))
}

/// Escapes a string for use as an attribute field (spec 3.4.3/3.4.4): `%`, `*`, `,`, `\` and
/// anything outside printable ASCII become `\uXXXX` (or `\UXXXXXXXX`).
pub fn field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '*' | ',' | '\\') || !(' '..='~').contains(&c) {
            let v = c as u32;
            if v <= 0xFFFF {
                let _ = write!(out, "\\u{v:04X}");
            } else {
                let _ = write!(out, "\\U{v:08X}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// File polarity (`.FilePolarity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// The image is material.
    Positive,
    /// The image is the absence of material (solder mask openings).
    Negative,
}

/// Plot mode of the graphics state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Linear,
    Cw,
    Ccw,
}

/// A Gerber X2 file under construction.
pub struct Gerber {
    file_attrs: Vec<String>,
    polarity: Polarity,
    /// Macro definitions in order of first use, and their bodies (for deduplication).
    macros: Vec<(String, String)>,
    /// Aperture (template, function) in order of first use; D-code = 10 + index.
    apertures: Vec<(String, Option<String>)>,
    lookup: BTreeMap<(String, Option<String>), usize>,
    body: String,
    current: Option<usize>,
    mode: Option<Mode>,
    pos: Option<Xy>,
    /// Current object attributes (name → value).
    obj: BTreeMap<String, String>,
}

impl Gerber {
    /// A new file. `software_version` goes into `.GenerationSoftware`; `file_function` is the
    /// `.FileFunction` value (`Copper,L1,Top`).
    pub fn new(software_version: &str, file_function: &str, polarity: Polarity) -> Self {
        Gerber {
            file_attrs: vec![
                format!("%TF.GenerationSoftware,cadlab,cadlab,{}*%", field(software_version)),
                "%TF.SameCoordinates*%".into(),
                format!("%TF.FileFunction,{file_function}*%"),
                format!("%TF.FilePolarity,{}*%", if polarity == Polarity::Positive { "Positive" } else { "Negative" }),
                "%TF.Part,Single*%".into(),
            ],
            polarity,
            macros: Vec::new(),
            apertures: Vec::new(),
            lookup: BTreeMap::new(),
            body: String::new(),
            current: None,
            mode: None,
            pos: None,
            obj: BTreeMap::new(),
        }
    }

    /// File polarity.
    pub fn polarity(&self) -> Polarity {
        self.polarity
    }

    /// Defines (or reuses) an aperture macro with a fixed body and returns its name. Bodies are
    /// primitives separated by `*` (without the final `*%`).
    pub fn macro_def(&mut self, body: String) -> String {
        if let Some((name, _)) = self.macros.iter().find(|(_, b)| *b == body) {
            return name.clone();
        }
        let name = format!("Shape{}", self.macros.len() + 1);
        self.macros.push((name.clone(), body));
        name
    }

    /// Returns the D-code of an aperture template (`C,0.5`, `R,1X0.6`, `Shape3`), defining it
    /// with the given `.AperFunction` if new.
    pub fn aperture(&mut self, template: &str, function: Option<&str>) -> usize {
        let key = (template.to_string(), function.map(String::from));
        if let Some(&i) = self.lookup.get(&key) {
            return i + 10;
        }
        let i = self.apertures.len();
        self.apertures.push(key.clone());
        self.lookup.insert(key, i);
        i + 10
    }

    fn select(&mut self, d: usize) {
        if self.current != Some(d) {
            let _ = writeln!(self.body, "D{d}*");
            self.current = Some(d);
        }
    }

    fn set_mode(&mut self, m: Mode) {
        if self.mode != Some(m) {
            self.body.push_str(match m {
                Mode::Linear => "G01*\n",
                Mode::Cw => "G02*\n",
                Mode::Ccw => "G03*\n",
            });
            self.mode = Some(m);
        }
    }

    /// Sets the object attributes for the following objects: every attribute not listed is
    /// deleted (`%TD.X*%`), changed ones are (re)defined with `%TO%`.
    pub fn attrs(&mut self, list: &[(&str, String)]) {
        let want: BTreeMap<String, String> = list.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let stale: Vec<String> = self.obj.keys().filter(|k| !want.contains_key(*k)).cloned().collect();
        for k in stale {
            let _ = writeln!(self.body, "%TD{k}*%");
            self.obj.remove(&k);
        }
        for (k, v) in list {
            if self.obj.get(*k) != Some(v) {
                if v.is_empty() {
                    let _ = writeln!(self.body, "%TO{k},*%");
                } else {
                    let _ = writeln!(self.body, "%TO{k},{v}*%");
                }
                self.obj.insert(k.to_string(), v.clone());
            }
        }
    }

    /// Writes a comment line (`G04`). The text must not contain `*` or `%`.
    pub fn comment(&mut self, text: &str) {
        let t: String = text.chars().filter(|c| *c != '*' && *c != '%').collect();
        let _ = writeln!(self.body, "G04 {t}*");
    }

    fn xy(p: Xy) -> String {
        format!("X{}Y{}", p.0, p.1)
    }

    /// Flashes aperture `d` at `p` (D03).
    pub fn flash(&mut self, d: usize, p: Xy) {
        self.select(d);
        let _ = writeln!(self.body, "{}D03*", Self::xy(p));
        self.pos = Some(p);
    }

    fn move_to(&mut self, p: Xy) {
        if self.pos != Some(p) {
            let _ = writeln!(self.body, "{}D02*", Self::xy(p));
            self.pos = Some(p);
        }
    }

    fn line_to(&mut self, p: Xy) {
        self.set_mode(Mode::Linear);
        let _ = writeln!(self.body, "{}D01*", Self::xy(p));
        self.pos = Some(p);
    }

    /// Draws an open polyline with aperture `d`.
    pub fn polyline(&mut self, d: usize, pts: &[Xy]) {
        let Some(&first) = pts.first() else { return };
        self.select(d);
        self.move_to(first);
        if pts.len() == 1 {
            // A zero-length draw makes a dot of the aperture's size.
            self.line_to(first);
        }
        for &p in &pts[1..] {
            self.line_to(p);
        }
    }

    /// Draws a path of segments with aperture `d`, starting at `start`.
    pub fn path(&mut self, d: usize, start: Xy, segs: &[Seg]) {
        self.select(d);
        self.move_to(start);
        self.segments(segs);
    }

    fn segments(&mut self, segs: &[Seg]) {
        for s in segs {
            match *s {
                Seg::Line(p) => self.line_to(p),
                Seg::Arc { center, end, ccw } => self.arc_to(center, end, ccw),
            }
        }
    }

    /// Circular segment from the current point to `end` around `center` (G75 multi-quadrant).
    fn arc_to(&mut self, center: Xy, end: Xy, ccw: bool) {
        let start = self.pos.expect("arc needs a current point");
        // Arcs shorter than 2 µm are unstable (spec 4.7.2): draw them as lines.
        let (dx, dy) = ((end.0 - start.0) as f64, (end.1 - start.1) as f64);
        if (dx * dx + dy * dy).sqrt() < 2_000.0 && start != end {
            self.line_to(end);
            return;
        }
        self.set_mode(if ccw { Mode::Ccw } else { Mode::Cw });
        let _ = writeln!(self.body, "{}I{}J{}D01*", Self::xy(end), center.0 - start.0, center.1 - start.1);
        self.pos = Some(end);
    }

    /// A region (G36/G37) made of closed contours (each with no holes: fracture first), with
    /// the given `.AperFunction` attached to it.
    pub fn region(&mut self, contours: &[Vec<Xy>], function: Option<&str>) {
        let contours: Vec<&Vec<Xy>> = contours.iter().filter(|c| c.len() >= 3).collect();
        if contours.is_empty() {
            return;
        }
        if let Some(f) = function {
            let _ = writeln!(self.body, "%TA.AperFunction,{f}*%");
        }
        self.body.push_str("G36*\n");
        for c in contours {
            let _ = writeln!(self.body, "{}D02*", Self::xy(c[0]));
            self.pos = Some(c[0]);
            for &p in &c[1..] {
                self.line_to(p);
            }
            if c.last() != c.first() {
                self.line_to(c[0]);
            }
        }
        self.body.push_str("G37*\n");
        if function.is_some() {
            self.body.push_str("%TD.AperFunction*%\n");
        }
    }

    /// The complete file.
    pub fn finish(mut self) -> String {
        if !self.obj.is_empty() {
            self.body.push_str("%TD*%\n");
        }
        let mut out = String::new();
        for a in &self.file_attrs {
            out.push_str(a);
            out.push('\n');
        }
        out.push_str("%FSLAX46Y46*%\n%MOMM*%\n%LPD*%\n");
        for (name, body) in &self.macros {
            let _ = writeln!(out, "%AM{name}*\n{body}*%");
        }
        let mut cur_fn: Option<&str> = None;
        for (i, (tpl, f)) in self.apertures.iter().enumerate() {
            if f.as_deref() != cur_fn {
                match f {
                    Some(f) => {
                        let _ = writeln!(out, "%TA.AperFunction,{f}*%");
                    }
                    None => out.push_str("%TD.AperFunction*%\n"),
                }
                cur_fn = f.as_deref();
            }
            let _ = writeln!(out, "%ADD{}{tpl}*%", i + 10);
        }
        if cur_fn.is_some() {
            out.push_str("%TD.AperFunction*%\n");
        }
        out.push_str("G75*\n");
        out.push_str(&self.body);
        out.push_str("M02*\n");
        out
    }
}

/// A path segment from the current point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seg {
    /// Straight segment to a point.
    Line(Xy),
    /// Circular arc to `end` around `center`.
    Arc {
        /// Center.
        center: Xy,
        /// End point.
        end: Xy,
        /// Counter-clockwise.
        ccw: bool,
    },
}

/// Center and direction of the arc through `a`, `m`, `b`, or `None` when they are collinear.
pub fn arc_center(a: Xy, m: Xy, b: Xy) -> Option<(Xy, bool)> {
    let f = |p: Xy| (p.0 as f64, p.1 as f64);
    let ((ax, ay), (mx, my), (bx, by)) = (f(a), f(m), f(b));
    let d = 2.0 * (ax * (my - by) + mx * (by - ay) + bx * (ay - my));
    if d.abs() < 1e-6 {
        return None;
    }
    let (a2, m2, b2) = (ax * ax + ay * ay, mx * mx + my * my, bx * bx + by * by);
    let cx = (a2 * (my - by) + m2 * (by - ay) + b2 * (ay - my)) / d;
    let cy = (a2 * (bx - mx) + m2 * (ax - bx) + b2 * (mx - ax)) / d;
    // Counter-clockwise when a → m → b turns left.
    let cross = (mx - ax) * (by - my) - (my - ay) * (bx - mx);
    Some(((cx.round() as i64, cy.round() as i64), cross > 0.0))
}

/// Segments for the arc from `a` through `m` to `b`: two halves through the midpoint (stable
/// even for near-full arcs), or straight lines if the points are collinear.
pub fn arc_segs(a: Xy, m: Xy, b: Xy) -> Vec<Seg> {
    match arc_center(a, m, b) {
        Some((center, ccw)) => vec![Seg::Arc { center, end: m, ccw }, Seg::Arc { center, end: b, ccw }],
        None => vec![Seg::Line(m), Seg::Line(b)],
    }
}

/// Segments of a full circle starting and ending at `(cx + r, cy)`.
pub fn circle_segs(c: Xy, r: i64) -> (Xy, Vec<Seg>) {
    let start = (c.0 + r, c.1);
    let half = (c.0 - r, c.1);
    (start, vec![Seg::Arc { center: c, end: half, ccw: true }, Seg::Arc { center: c, end: start, ccw: true }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_format() {
        assert_eq!(mm(1_200_000), "1.2");
        assert_eq!(mm(0), "0");
        assert_eq!(mm(-1), "-0.000001");
        assert_eq!(mm(25_400_000), "25.4");
        assert_eq!(field("a,b*c%d\\é"), "a\\u002Cb\\u002Ac\\u0025d\\u005C\\u00E9");
    }

    #[test]
    fn writer_structure() {
        let mut g = Gerber::new("0.0.1", "Copper,L1,Top", Polarity::Positive);
        let d = g.aperture("C,0.25", Some("Conductor"));
        assert_eq!(d, 10);
        assert_eq!(g.aperture("C,0.25", Some("Conductor")), 10);
        assert_eq!(g.aperture("C,0.25", Some("ViaPad")), 11);
        g.attrs(&[(".N", "GND".into())]);
        g.polyline(d, &[(0, 0), (1_000_000, 0)]);
        g.path(d, (0, 0), &arc_segs((0, 0), (1_000_000, 1_000_000), (2_000_000, 0)));
        g.region(&[vec![(0, 0), (10, 0), (10, 10)]], Some("Conductor"));
        let s = g.finish();
        assert!(s.starts_with("%TF.GenerationSoftware,cadlab,cadlab,0.0.1*%\n"));
        assert!(s.contains("%TA.AperFunction,Conductor*%\n%ADD10C,0.25*%\n%TA.AperFunction,ViaPad*%\n%ADD11C,0.25*%"));
        assert!(s.contains("G02*\nX1000000Y1000000I1000000J0D01*"), "{s}");
        assert!(s.contains("G36*\nX0Y0D02*\nG01*\nX10Y0D01*\nX10Y10D01*\nX0Y0D01*\nG37*"), "{s}");
        assert!(s.ends_with("%TD*%\nM02*\n"));
    }

    #[test]
    fn arc_direction() {
        // Upper half circle from (0,0) through (1,1) to (2,0) is clockwise.
        assert_eq!(arc_center((0, 0), (1, 1), (2, 0)), Some(((1, 0), false)));
        assert_eq!(arc_center((2, 0), (1, 1), (0, 0)), Some(((1, 0), true)));
        assert_eq!(arc_center((0, 0), (1, 0), (2, 0)), None);
    }
}
