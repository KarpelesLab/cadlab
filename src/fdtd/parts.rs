//! Board parts as lumped elements: two-pin resistors, capacitors and inductors in the region,
//! from their part values, placed across the gap between their two pads.
//!
//! A part's element is a series R–L–C: a resistor is its `resistance` (plus
//! its `esl`), a capacitor its `capacitance` with its `esr` and `esl`, an inductor its
//! `inductance` with its `esr`. Values come from the component's properties, else its part's
//! parameters. The element lies on the pads' copper layer along the axis joining the pad
//! centers: a box between the two pads' facing edges ([`PartBox`]), over the width where the pads
//! face each other and the layer's copper thickness.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, BTreeSet};

use crate::board::{self, PlacedPad};
use crate::diag::Diagnostic;
use crate::geom::poly::Rect;
use crate::model::Project;
use crate::model::part::{Category, ParamValue};
use crate::openems::Region;
use crate::refs::ObjectRef;
use crate::value::{Quantity, Unit};

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
    /// Mesh columns the element is spread over, in parallel across the pads (0: not placed).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub columns: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// A two-pin part with its values, before placement.
pub(crate) struct Candidate {
    pub(crate) info: LumpedPart,
    pads: [PlacedPad; 2],
    r: f64,
    l: f64,
    c: Option<f64>,
}

/// Values replacing a component's for one simulation: designator → `resistance`, `capacitance`,
/// `inductance`, `esr` or `esl` → value.
pub type Overrides = BTreeMap<String, BTreeMap<String, Quantity>>;

/// A parameter: this simulation's override, else the component's property, else the part's
/// parameter.
fn param(
    p: &Project,
    overrides: &Overrides,
    refdes: &str,
    params: &crate::model::part::Params,
    key: &str,
) -> Option<Quantity> {
    if let Some(v) = overrides.get(refdes).and_then(|m| m.get(key)) {
        return Some(*v);
    }
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
/// warnings for those that cannot be modeled. `kept`: with `only_selected`, the signal nets in the
/// model; a part on none of them is not part of this simulation (a note, not a warning).
pub(crate) fn candidates(
    p: &Project,
    window: Rect,
    kept: Option<&BTreeSet<String>>,
    overrides: &Overrides,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Candidate> {
    let pads = board::placed_pads(p);
    let lib = p.library();
    let mut out = Vec::new();
    let mut outside_selection: Vec<String> = Vec::new();
    for (refdes, comp) in &p.circuit().components {
        let Some(part) = lib.parts.get(&comp.part) else { continue };
        let main = match part.category {
            Category::Resistor => "resistance",
            Category::Capacitor => "capacitance",
            Category::Inductor => "inductance",
            _ => continue,
        };
        let mine: Vec<&PlacedPad> = pads.iter().filter(|q| q.refdes == *refdes).collect();
        let inside = mine
            .iter()
            .filter(|q| window.contains_point(crate::geom::poly::Point::new(q.center.x.0, q.center.y.0)))
            .count();
        if inside == 0 {
            continue;
        }
        if let Some(kept) = kept
            && !mine.iter().any(|q| q.net.as_ref().is_some_and(|n| kept.contains(n)))
        {
            outside_selection.push(refdes.clone());
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
        let get = |k: &str| param(p, overrides, refdes, &part.params, k);
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
                columns: 0,
            },
            pads: pads2,
            r: rv,
            l: lv,
            c: cv,
        });
    }
    if !outside_selection.is_empty() {
        diags.push(Diagnostic::info(
            "sim.parts_outside_selection",
            format!("not simulated, on none of the selected nets (`only_selected`): {}", outside_selection.join(", ")),
        ));
    }
    out
}

/// The modeled inductors and capacitors with nothing but their nominal value, for a note: at RF
/// the parasitics a datasheet gives (ESL, Q, self-resonance) move the result.
pub(crate) fn ideal(parts: &[LumpedPart]) -> Vec<String> {
    parts
        .iter()
        .filter(|q| {
            (q.capacitance.is_some() && q.inductance.is_none()) || (q.inductance.is_some() && q.resistance.is_none())
        })
        .map(|q| q.refdes.clone())
        .collect()
}

/// The copper layer of a pad (an SMD pad's own, a through-hole pad's footprint side).
fn pad_layer(q: &PlacedPad) -> String {
    if q.layers.len() == 1 { q.layers[0].clone() } else { board::outer_layer(q.side).to_string() }
}

/// A part's lumped element: a box between its two pads' copper (nm), `axis` joining the pads, with
/// the part's values. What the openEMS export writes and what `sim.sparams` hands ems.
pub(crate) struct PartBox {
    pub(crate) info: LumpedPart,
    pub(crate) axis: usize,
    pub(crate) lo: [i64; 3],
    pub(crate) hi: [i64; 3],
    pub(crate) r: f64,
    pub(crate) l: f64,
    pub(crate) c: Option<f64>,
}

/// The candidates as boxes on the region's mesh (a solver snaps the box to the same mesh lines as
/// the pads): both pads on one layer, facing along x or y, at least one cell between them.
pub(crate) fn boxes(r: &Region, parts: Vec<Candidate>, diags: &mut Vec<Diagnostic>) -> Vec<PartBox> {
    const HINT: &str = "use a finer mesh (`max_cell`, `min_cell`), or place it along x or y";
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
        let [p0, p1] = &c.pads;
        let (dx, dy) = ((p1.center.x.0 - p0.center.x.0).abs(), (p1.center.y.0 - p0.center.y.0).abs());
        let a = if dx >= dy { 0 } else { 1 };
        let b = 1 - a;
        let span = |q: &PlacedPad, ax: usize| {
            q.shape_on(&pad_layer(q)).bbox().map(|s| if ax == 0 { (s.min.x, s.max.x) } else { (s.min.y, s.max.y) })
        };
        let (Some(a0), Some(a1), Some(b0), Some(b1)) = (span(p0, a), span(p1, a), span(p0, b), span(p1, b)) else {
            diags.push(skipped(&refdes, "a pad has no copper shape".into(), HINT));
            continue;
        };
        let (o_lo, o_hi) = (b0.0.max(b1.0), b0.1.min(b1.1));
        if o_lo > o_hi {
            diags.push(skipped(&refdes, "its pads do not face each other along x or y".into(), HINT));
            continue;
        }
        // The gap along `a`, between the pads' facing edges; it must span a mesh cell.
        let (g_lo, g_hi) = if a0.0 <= a1.0 { (a0.1, a1.0) } else { (a1.1, a0.0) };
        let nearest = |v: i64| {
            let ls = &r.lines[a];
            (0..ls.len()).min_by_key(|&i| (ls[i] - v).abs()).unwrap_or(0)
        };
        if g_hi <= g_lo || nearest(g_lo) == nearest(g_hi) {
            diags.push(skipped(&refdes, "no mesh cell separates its pads' copper".into(), HINT));
            continue;
        }
        let (mut lo, mut hi) = ([0; 3], [0; 3]);
        (lo[a], hi[a]) = (g_lo, g_hi);
        (lo[b], hi[b]) = (o_lo, o_hi);
        (lo[2], hi[2]) = (layer.bottom, layer.top);
        out.push(PartBox { info: c.info, axis: a, lo, hi, r: c.r, l: c.l, c: c.c });
    }
    out
}
