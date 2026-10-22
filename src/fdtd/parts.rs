//! Board parts as lumped elements: two-pin resistors, capacitors and inductors in the region,
//! from their part values, placed across the gap between their two pads.
//!
//! A part's element is a series R–L–C ([`LumpedElement`]): a resistor is its `resistance` (plus
//! its `esl`), a capacitor its `capacitance` with its `esr` and `esl`, an inductor its
//! `inductance` with its `esr`. Values come from the component's properties, else its part's
//! parameters. The element lies on the pads' copper layer along the axis joining the pad
//! centers, in the mesh cells between the two pads' copper (the edges that are not copper), over
//! the nodes where the pads face each other and every copper surface node of the layer (both
//! faces of solid copper).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{self, PlacedPad};
use crate::diag::Diagnostic;
use crate::geom::poly::Rect;
use crate::model::Project;
use crate::model::part::{Category, ParamValue};
use crate::openems::Region;
use crate::refs::ObjectRef;
use crate::value::{Quantity, Unit};

use super::engine::{LumpedElement, Structure};

/// A board part simulated as a lumped element.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LumpedPart {
    /// Component reference.
    pub refdes: String,
    /// Its two pads (`R1.1`, `R1.2`).
    pub pads: [String; 2],
    /// Series resistance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resistance: Option<Quantity>,
    /// Series inductance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inductance: Option<Quantity>,
    /// Series capacitance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacitance: Option<Quantity>,
}

/// A two-pin part with its values, before placement.
pub(crate) struct Candidate {
    pub(crate) info: LumpedPart,
    pads: [PlacedPad; 2],
    r: f64,
    l: f64,
    c: Option<f64>,
}

/// A parameter: the component's property, else the part's parameter.
fn param(p: &Project, refdes: &str, params: &crate::model::part::Params, key: &str) -> Option<Quantity> {
    let prop = p.circuit().components.get(refdes).and_then(|c| c.properties.get(key));
    if let Some(v) = prop.and_then(|v| ParamValue::parse(key, v).ok()) {
        return v.quantity().copied();
    }
    params.get(key).and_then(|v| v.quantity()).copied()
}

fn skipped(refdes: &str, why: String, hint: &str) -> Diagnostic {
    Diagnostic::warning("sim.part_not_modeled", format!("{refdes}: {why}; not simulated (its pads stay open)"))
        .with_subject(ObjectRef::Name(refdes.to_string()))
        .with_hint(hint)
}

/// The two-pin resistors, capacitors and inductors with a pad in `window`, with their values;
/// warnings for those that cannot be modeled.
pub(crate) fn candidates(p: &Project, window: Rect, diags: &mut Vec<Diagnostic>) -> Vec<Candidate> {
    let pads = board::placed_pads(p);
    let lib = p.library();
    let mut out = Vec::new();
    for (refdes, comp) in &p.circuit().components {
        let Some(part) = lib.parts.get(&comp.part) else { continue };
        let main = match part.category {
            Category::Resistor => "resistance",
            Category::Capacitor => "capacitance",
            Category::Inductor => "inductance",
            _ => continue,
        };
        let mine: Vec<&PlacedPad> = pads.iter().filter(|q| q.refdes == *refdes).collect();
        let inside = mine.iter().filter(|q| window.contains_point(super::pt(q.center.x.0, q.center.y.0))).count();
        if inside == 0 {
            continue;
        }
        if p.bom().dnp.contains(refdes) {
            diags.push(
                Diagnostic::info("sim.part_not_populated", format!("{refdes} is not populated (DNP): left open"))
                    .with_subject(ObjectRef::Name(refdes.clone())),
            );
            continue;
        }
        if mine.len() != 2 {
            diags.push(skipped(
                refdes,
                format!("{} pads (two expected)", mine.len()),
                "only two-pad parts are modeled",
            ));
            continue;
        }
        if inside < 2 {
            diags.push(skipped(
                refdes,
                "one of its pads is outside the region".into(),
                "include the part with `around` or grow `margin`",
            ));
            continue;
        }
        let get = |k: &str| param(p, refdes, &part.params, k);
        let Some(value) = get(main) else {
            diags.push(skipped(
                refdes,
                format!("its part `{}` has no `{main}`", part.id),
                &format!("set it: `part.set {} --params {main}=...`", part.id),
            ));
            continue;
        };
        let (esr, esl) = (get("esr"), get("esl"));
        let (r, l, c) = match part.category {
            Category::Resistor => (Some(value), esl, None),
            Category::Capacitor => (esr, esl, Some(value)),
            _ => (esr, Some(value), None),
        };
        let f = |q: Option<Quantity>| q.map_or(0.0, |q| q.to_f64());
        let (rv, lv, cv) = (f(r), f(l), c.map(|q| q.to_f64()));
        if rv < 0.0 || lv < 0.0 || cv.is_some_and(|c| c <= 0.0) || !rv.is_finite() || !lv.is_finite() {
            diags.push(skipped(
                refdes,
                "negative or zero values".into(),
                "check the part's resistance, capacitance, inductance, esr and esl",
            ));
            continue;
        }
        let mut two: Vec<PlacedPad> = mine.into_iter().cloned().collect();
        two.sort_by(|a, b| crate::model::sections::natural_cmp(&a.number, &b.number));
        let pads2 = [two[0].clone(), two[1].clone()];
        let unit = |q: Option<Quantity>, u: Unit| q.map(|q| q.with_unit(u));
        out.push(Candidate {
            info: LumpedPart {
                refdes: refdes.clone(),
                pads: [format!("{refdes}.{}", pads2[0].number), format!("{refdes}.{}", pads2[1].number)],
                resistance: unit(r, Unit::Ohm),
                inductance: unit(l, Unit::Henry),
                capacitance: unit(c, Unit::Farad),
            },
            pads: pads2,
            r: rv,
            l: lv,
            c: cv,
        });
    }
    out
}

/// The copper layer of a pad (an SMD pad's own, a through-hole pad's footprint side).
fn pad_layer(q: &PlacedPad) -> String {
    if q.layers.len() == 1 { q.layers[0].clone() } else { board::outer_layer(q.side).to_string() }
}

/// Places the candidates' elements in the voxelized structure (after the copper). Returns the
/// parts modeled.
pub(crate) fn place(
    s: &mut Structure,
    r: &Region,
    parts: Vec<Candidate>,
    diags: &mut Vec<Diagnostic>,
) -> Vec<LumpedPart> {
    let mut out = Vec::new();
    for c in parts {
        let refdes = c.info.refdes.clone();
        let (la, lb) = (pad_layer(&c.pads[0]), pad_layer(&c.pads[1]));
        let layer = match r.layers.iter().find(|l| l.name == la) {
            Some(l) if la == lb => l,
            _ => {
                diags.push(skipped(
                    &refdes,
                    "its pads are on different copper layers".into(),
                    "only parts with both pads on one copper layer are modeled",
                ));
                continue;
            }
        };
        match element(s, layer, &c) {
            Ok(e) => {
                s.element(LumpedElement { r: c.r, l: c.l, c: c.c, ..e });
                out.push(c.info);
            }
            Err(why) => diags.push(skipped(
                &refdes,
                why,
                "use a finer mesh (`max_cell`, `min_cell`), keep its nets' copper (`only_selected`), or place it \
                 along x or y",
            )),
        }
    }
    out
}

/// The element's box between the two pads' copper.
fn element(s: &Structure, layer: &crate::openems::Layer, c: &Candidate) -> Result<LumpedElement, String> {
    let [p0, p1] = &c.pads;
    let (dx, dy) = ((p1.center.x.0 - p0.center.x.0).abs(), (p1.center.y.0 - p0.center.y.0).abs());
    let a = if dx >= dy { 0 } else { 1 };
    let b = 1 - a;
    let coord = |q: &PlacedPad, ax: usize| if ax == 0 { q.center.x.0 } else { q.center.y.0 };
    let bbox = |q: &PlacedPad| {
        let l = pad_layer(q);
        q.shape_on(&l).bbox()
    };
    let (Some(b0), Some(b1)) = (bbox(p0), bbox(p1)) else { return Err("a pad has no copper shape".into()) };
    let span = |r: Rect, ax: usize| if ax == 0 { (r.min.x, r.max.x) } else { (r.min.y, r.max.y) };
    let (s0, s1) = (span(b0, b), span(b1, b));
    let (o_lo, o_hi) = (s0.0.max(s1.0), s0.1.min(s1.1));
    if o_lo > o_hi {
        return Err("its pads do not face each other along x or y".into());
    }
    let (from, to) = {
        let (u, v) = (s.node(a, coord(p0, a)), s.node(a, coord(p1, a)));
        (u.min(v), u.max(v))
    };
    let (k0, k1) = {
        let (u, v) = (s.node(2, layer.bottom), s.node(2, layer.top));
        (u.min(v), u.max(v))
    };
    let at = |ia: usize, ib: usize, k: usize| {
        let mut q = [0; 3];
        (q[a], q[b], q[2]) = (ia, ib, k);
        q
    };
    // The run of edges that are not copper between the pads, on one column.
    let gap = |ib: usize, k: usize| -> Option<(usize, usize)> {
        let mut i = from;
        while i < to && s.is_pec(a, at(i, ib, k)) {
            i += 1;
        }
        let start = i;
        while i < to && !s.is_pec(a, at(i, ib, k)) {
            i += 1;
        }
        let end = i;
        (end > start
            && (end..to).all(|j| s.is_pec(a, at(j, ib, k)))
            && s.touches_pec(at(start, ib, k))
            && s.touches_pec(at(end, ib, k)))
        .then_some((start, end))
    };
    let mid = s.node(b, o_lo + (o_hi - o_lo) / 2);
    let Some(run) = gap(mid, k1) else {
        return Err("no mesh cell separates its pads' copper, or a pad has no copper in the region".into());
    };
    // Both faces of solid copper when the gap is the same there, else the top.
    let ks = if (k0..=k1).all(|k| gap(mid, k) == Some(run)) { k0 } else { k1 };
    // Columns across where the pads face each other with the same gap, contiguous around the middle.
    let lines = &s.lines()[b];
    let ok = |ib: usize| lines[ib] >= o_lo && lines[ib] <= o_hi && (ks..=k1).all(|k| gap(ib, k) == Some(run));
    let (mut lo_b, mut hi_b) = (mid, mid);
    while lo_b > 0 && ok(lo_b - 1) {
        lo_b -= 1;
    }
    while hi_b + 1 < lines.len() && ok(hi_b + 1) {
        hi_b += 1;
    }
    let (mut lo, mut hi) = ([0; 3], [0; 3]);
    (lo[a], hi[a]) = run;
    (lo[b], hi[b]) = (lo_b, hi_b);
    (lo[2], hi[2]) = (ks, k1);
    Ok(LumpedElement { axis: a, lo, hi, r: 0.0, l: 0.0, c: None })
}
