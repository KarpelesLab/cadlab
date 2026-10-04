//! SPICE netlist export in the ngspice dialect (`export.spice`).
//!
//! Written from the SPICE netlist conventions documented in the ngspice manual (element
//! lines, `.model`, `.include`, `.control`); no code from ngspice or any other simulator is
//! used (DECISIONS D7, D29). Mapping, by part category:
//!
//! | Category | Element |
//! |---|---|
//! | resistor, capacitor, inductor | `R`, `C`, `L` with the part's `resistance`/`capacitance`/`inductance` |
//! | ferrite bead, fuse | `R` of 1 mΩ (DC approximation, warning) |
//! | diode, LED | `D anode cathode model`; without a `spice_model`, a default `.model ... D` (warning) |
//! | BJT, MOSFET | `Q c b e model` / `M d g s s model`, with the part's `spice_model` (else a placeholder) |
//! | everything else with pins (ICs, regulators, crystals, ...) | `X<ref> nodes... subckt` with the part's `spice_model` |
//! | connectors, test points, mechanical parts, switches | omitted (comment) |
//!
//! A component without a usable model is written as a commented-out placeholder line and a
//! `spice.no_model` warning, so the rest of the circuit still simulates. Part parameters (or
//! component properties, which win) `spice_model`, `spice_lib` (a file to `.include`) and
//! `spice_pins` (subcircuit pin order, as part pin numbers or names) supply real models.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::circuit::PinRef;
use crate::model::part::{Category, Part};
use crate::model::sections::natural_cmp;
use crate::refs::ObjectRef;
use crate::value::{Quantity, Unit};

/// Export options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Ground (node `0`): this net, else `GND`, else the first net named like ground.
    pub ground: Option<String>,
    /// Add a DC voltage source to ground for every net with a voltage (`net.set --voltage`) and
    /// for driven nets whose name gives one (`3V3`, `+5V`, `VCC_1V8`).
    pub supplies: bool,
    /// Analysis lines written as-is before `.end` (`.op`, `.tran 1u 10m`).
    pub analysis: Vec<String>,
    /// Lines of an ngspice `.control` block (`op`, `print v(out)`).
    pub control: Vec<String>,
    /// Also write DNP components.
    pub include_dnp: bool,
}

/// Why the export failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpiceError {
    /// Stable code.
    pub code: &'static str,
    /// Message.
    pub message: String,
    /// Hint.
    pub hint: String,
}

/// An exported netlist.
#[derive(Clone, Debug, Default)]
pub struct Netlist {
    /// The netlist text.
    pub text: String,
    /// Elements written (not commented out).
    pub elements: usize,
    /// Components written as placeholders (no model).
    pub placeholders: Vec<String>,
    /// Components omitted (connectors, switches, ...).
    pub omitted: Vec<String>,
    /// Supply sources added: net and voltage.
    pub sources: Vec<(String, Quantity)>,
    /// Net name → SPICE node.
    pub nodes: BTreeMap<String, String>,
    /// Warnings and notes.
    pub diagnostics: Vec<Diagnostic>,
}

/// A value in SPICE notation: engineering suffixes, `Meg` for 10⁶ (SPICE reads `M` as milli).
pub fn spice_number(q: &Quantity) -> String {
    let s = q.with_unit(Unit::Ohm).display_bare();
    match s.strip_suffix('M') {
        Some(n) => format!("{n}Meg"),
        None => s,
    }
}

/// The voltage a rail name implies: `3V3` → 3.3 V, `+5V` / `5V0` → 5 V, `VCC_1V8` → 1.8 V,
/// `12V` → 12 V, `-12V` → −12 V. Tokens are split on `_`, `/`, `.` and spaces.
pub fn rail_voltage(name: &str) -> Option<Quantity> {
    for tok in name.split(['_', '/', ' ', '.', '(', ')']).rev() {
        let (neg, t) = match tok.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, tok.strip_prefix('+').unwrap_or(tok)),
        };
        let t = t.to_ascii_uppercase();
        let Some(vpos) = t.find('V') else { continue };
        let (int, frac) = (&t[..vpos], &t[vpos + 1..]);
        if int.is_empty() || !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let digits = format!("{int}{frac}");
        let m: i64 = digits.parse().ok()?;
        let q = Quantity::new(if neg { -m } else { m }, -(frac.len() as i8), Unit::Volt);
        return Some(q);
    }
    None
}

fn sanitize(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        match c {
            c if c.is_ascii_alphanumeric() || c == '_' => s.push(c),
            '+' => s.push_str("_P"),
            '-' => s.push_str("_N"),
            _ => s.push('_'),
        }
    }
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, 'N');
    }
    s
}

/// Picks a name not yet used (case-insensitively: SPICE names are case-insensitive).
fn unique(base: String, used: &mut BTreeSet<String>) -> String {
    let mut name = base.clone();
    let mut i = 2;
    while !used.insert(name.to_ascii_lowercase()) {
        name = format!("{base}_{i}");
        i += 1;
    }
    name
}

fn is_ground_name(n: &str) -> bool {
    crate::symbolgen::is_ground(n) && !n.eq_ignore_ascii_case("EP") && !n.eq_ignore_ascii_case("PAD")
}

/// Text parameter of a part, overridden by the component property of the same name.
fn text_param(p: &Project, refdes: &str, part: &Part, key: &str) -> Option<String> {
    if let Some(v) = p.circuit().components.get(refdes).and_then(|c| c.properties.get(key)) {
        return Some(v.clone());
    }
    part.params.get(key).map(|v| v.to_string())
}

/// SPICE node names.
struct Nodes<'a> {
    index: BTreeMap<&'a PinRef, &'a str>,
    /// Net → node.
    names: BTreeMap<String, String>,
    used: BTreeSet<String>,
    /// Unconnected pins given a node.
    nc: usize,
}

impl Nodes<'_> {
    /// Node of a pin: its net's, or a fresh one for an unconnected pin.
    fn node(&mut self, pin: &PinRef) -> String {
        match self.index.get(pin) {
            Some(n) => self.names[*n].clone(),
            None => {
                self.nc += 1;
                unique(sanitize(&format!("NC_{}_{}", pin.refdes, pin.pin)), &mut self.used)
            }
        }
    }
}

/// Writes the netlist.
pub fn export(p: &Project, opts: &Options) -> Result<Netlist, SpiceError> {
    let c = p.circuit();
    let lib = p.library();
    let ground = match &opts.ground {
        Some(g) => {
            let g = g.strip_prefix("net:").unwrap_or(g);
            if !c.nets.contains_key(g) {
                return Err(SpiceError {
                    code: "spice.ground_not_found",
                    message: format!("no net `{g}` for the ground node"),
                    hint: "list nets with `net.list`".into(),
                });
            }
            Some(g.to_string())
        }
        None => {
            c.nets.keys().find(|n| n.as_str() == "GND").or_else(|| c.nets.keys().find(|n| is_ground_name(n))).cloned()
        }
    };
    let mut out = Netlist::default();
    if ground.is_none() {
        out.diagnostics.push(
            Diagnostic::warning("spice.no_ground", "no ground net found: nothing is connected to node 0")
                .with_hint("name the ground net GND, or give `ground`; SPICE needs a DC path to node 0"),
        );
    }

    // Nodes.
    let mut nodes = Nodes { index: c.pin_index(), names: BTreeMap::new(), used: BTreeSet::from(["0".into()]), nc: 0 };
    for name in c.nets.keys() {
        let node =
            if Some(name) == ground.as_ref() { "0".to_string() } else { unique(sanitize(name), &mut nodes.used) };
        nodes.names.insert(name.clone(), node);
    }

    let mut body = String::new();
    let mut models: BTreeMap<String, String> = BTreeMap::new();
    let mut includes: BTreeSet<String> = BTreeSet::new();
    let mut elem_names = BTreeSet::new();
    let mut refs: Vec<&String> = c.components.keys().collect();
    refs.sort_by(|a, b| natural_cmp(a, b));
    for refdes in refs {
        let comp = &c.components[refdes];
        if p.bom().dnp.contains(refdes) && !opts.include_dnp {
            let _ = writeln!(body, "* {refdes}: not populated (DNP)");
            out.omitted.push(refdes.clone());
            continue;
        }
        let Some(part) = lib.parts.get(&comp.part) else {
            out.diagnostics.push(
                Diagnostic::error("spice.missing_part", format!("{refdes} uses part `{}`, missing", comp.part))
                    .with_subject(ObjectRef::Name(refdes.clone())),
            );
            continue;
        };
        let subject = ObjectRef::Name(refdes.clone());
        let model_name = text_param(p, refdes, part, "spice_model");
        if let Some(l) = text_param(p, refdes, part, "spice_lib") {
            includes.insert(l);
        }
        let pin = |key: &[&str], fallback: Option<&str>| -> Option<PinRef> {
            part.symbol
                .pins
                .iter()
                .find(|s| key.iter().any(|k| s.name.eq_ignore_ascii_case(k)))
                .or_else(|| fallback.and_then(|f| part.symbol.pins.iter().find(|s| s.number == f)))
                .map(|s| PinRef::new(refdes, &s.number))
        };
        let two = |nodes: &mut Nodes| {
            let mut pins: Vec<&str> = part.symbol.pins.iter().map(|s| s.number.as_str()).collect();
            pins.sort_by(|a, b| natural_cmp(a, b));
            (pins.len() == 2)
                .then(|| (nodes.node(&PinRef::new(refdes, pins[0])), nodes.node(&PinRef::new(refdes, pins[1]))))
        };
        let elem = |letter: char, used: &mut BTreeSet<String>| {
            let base = if refdes.starts_with(letter) { refdes.clone() } else { format!("{letter}{refdes}") };
            unique(base, used)
        };
        let placeholder = |out: &mut Netlist, body: &mut String, line: String, why: String| {
            let _ = writeln!(body, "* {line}");
            out.placeholders.push(refdes.clone());
            out.diagnostics.push(
                Diagnostic::warning("spice.no_model", format!("{refdes} ({}): {why}; written as a comment", part.id))
                    .with_subject(subject.clone())
                    .with_hint(format!(
                        "set the part's SPICE model: `part.set {} --params spice_model=NAME --params spice_lib=FILE` \
                         (and spice_pins=\"...\" for the subcircuit pin order)",
                        part.id
                    )),
            );
        };
        match part.category {
            Category::Resistor | Category::Capacitor | Category::Inductor => {
                let (letter, key) = match part.category {
                    Category::Resistor => ('R', "resistance"),
                    Category::Capacitor => ('C', "capacitance"),
                    _ => ('L', "inductance"),
                };
                let Some((a, b)) = two(&mut nodes) else {
                    placeholder(&mut out, &mut body, refdes.to_string(), "not a two-pin part".into());
                    continue;
                };
                let name = elem(letter, &mut elem_names);
                match part.params.get(key).and_then(|v| v.quantity()) {
                    Some(v) => {
                        let _ = writeln!(body, "{name} {a} {b} {}", spice_number(v));
                        out.elements += 1;
                    }
                    None => placeholder(&mut out, &mut body, format!("{name} {a} {b} ?"), format!("no `{key}`")),
                }
            }
            Category::FerriteBead | Category::Fuse => {
                let Some((a, b)) = two(&mut nodes) else {
                    placeholder(&mut out, &mut body, refdes.clone(), "not a two-pin part".into());
                    continue;
                };
                let name = elem('R', &mut elem_names);
                let _ = writeln!(body, "{name} {a} {b} 1m");
                out.elements += 1;
                out.diagnostics.push(
                    Diagnostic::info(
                        "spice.approximated",
                        format!("{refdes} ({}) is modeled as a 1 mΩ resistor (DC only)", part.category.label()),
                    )
                    .with_subject(subject.clone()),
                );
            }
            Category::Diode | Category::Led => {
                let (Some(a), Some(k)) = (pin(&["A", "ANODE"], Some("2")), pin(&["K", "C", "CATHODE"], Some("1")))
                else {
                    placeholder(&mut out, &mut body, refdes.clone(), "anode/cathode pins not found".into());
                    continue;
                };
                let (na, nk) = (nodes.node(&a), nodes.node(&k));
                let name = elem('D', &mut elem_names);
                let model = match &model_name {
                    Some(m) => m.clone(),
                    None => {
                        let m = format!("D_{}", sanitize(&part.id));
                        models.insert(m.clone(), format!(".model {m} D"));
                        out.diagnostics.push(
                            Diagnostic::warning(
                                "spice.default_model",
                                format!("{refdes} ({}) uses SPICE's default diode model", part.id),
                            )
                            .with_subject(subject.clone())
                            .with_hint(format!(
                                "set a real model: `part.set {} --params spice_model=NAME --params spice_lib=FILE`",
                                part.id
                            )),
                        );
                        m
                    }
                };
                let _ = writeln!(body, "{name} {na} {nk} {model}");
                out.elements += 1;
            }
            Category::TransistorBjt | Category::Mosfet => {
                let bjt = part.category == Category::TransistorBjt;
                let names: [&[&str]; 3] = if bjt {
                    [&["C", "COLLECTOR"], &["B", "BASE"], &["E", "EMITTER"]]
                } else {
                    [&["D", "DRAIN"], &["G", "GATE"], &["S", "SOURCE"]]
                };
                let pins: Vec<Option<PinRef>> = names.iter().map(|n| pin(n, None)).collect();
                let letter = if bjt { 'Q' } else { 'M' };
                let name = elem(letter, &mut elem_names);
                if pins.iter().any(Option::is_none) {
                    placeholder(&mut out, &mut body, name, "terminal pins not found by name".into());
                    continue;
                }
                let terms: Vec<String> = pins.iter().flatten().map(|p| nodes.node(p)).collect();
                let Some(model) = &model_name else {
                    placeholder(
                        &mut out,
                        &mut body,
                        format!("{name} {} ?", terms.join(" ")),
                        "no `spice_model`".into(),
                    );
                    continue;
                };
                let bulk = if bjt {
                    String::new()
                } else {
                    let b = pin(&["B", "BULK", "BODY"], None).map(|b| nodes.node(&b));
                    format!(" {}", b.unwrap_or_else(|| terms[2].clone()))
                };
                let _ = writeln!(body, "{name} {}{bulk} {model}", terms.join(" "));
                out.elements += 1;
            }
            Category::Connector | Category::TestPoint | Category::Mechanical | Category::Switch => {
                let _ = writeln!(body, "* {refdes}: {} (not simulated)", part.category.label());
                out.omitted.push(refdes.clone());
            }
            _ => {
                let order: Vec<String> = match text_param(p, refdes, part, "spice_pins") {
                    Some(list) => {
                        let mut v = Vec::new();
                        for key in list.split([' ', ',']).filter(|s| !s.is_empty()) {
                            match part.symbol.pin(key) {
                                Some(sp) => v.push(sp.number.clone()),
                                None => {
                                    out.diagnostics.push(
                                        Diagnostic::error(
                                            "spice.bad_pin_order",
                                            format!("{refdes}: spice_pins names `{key}`, not a pin of {}", part.id),
                                        )
                                        .with_subject(subject.clone())
                                        .with_hint("list the part's pin numbers or names in subcircuit order"),
                                    );
                                }
                            }
                        }
                        v
                    }
                    None => {
                        let mut v: Vec<String> = part.symbol.pins.iter().map(|s| s.number.clone()).collect();
                        v.sort_by(|a, b| natural_cmp(a, b));
                        v
                    }
                };
                let name = elem('X', &mut elem_names);
                let terms: Vec<String> = order.iter().map(|n| nodes.node(&PinRef::new(refdes, n))).collect();
                match &model_name {
                    Some(m) => {
                        let _ = writeln!(body, "{name} {} {m}", terms.join(" "));
                        out.elements += 1;
                    }
                    None => placeholder(
                        &mut out,
                        &mut body,
                        format!("{name} {} {}", terms.join(" "), sanitize(&part.id)),
                        "no `spice_model` (subcircuit)".into(),
                    ),
                }
            }
        }
    }

    // Supplies.
    let mut sources = String::new();
    if opts.supplies {
        for (name, net) in &c.nets {
            if Some(name) == ground.as_ref() {
                continue;
            }
            let v = net.voltage.or_else(|| if net.driven { rail_voltage(name) } else { None });
            match v {
                Some(v) => {
                    let node = &nodes.names[name];
                    let src = unique(format!("V{node}"), &mut elem_names);
                    let _ = writeln!(sources, "{src} {node} 0 DC {}", spice_number(&v));
                    out.sources.push((name.clone(), v));
                    out.elements += 1;
                }
                None if net.driven => out.diagnostics.push(
                    Diagnostic::warning(
                        "spice.supply_unknown",
                        format!("driven net `{name}` has no voltage: no source added"),
                    )
                    .with_subject(ObjectRef::Net(name.clone()))
                    .with_hint(format!("set it: `net.set {name} --voltage 3.3V`")),
                ),
                None => {}
            }
        }
    }

    let mut t = String::new();
    let _ = writeln!(t, "* {} (cadlab SPICE netlist, ngspice dialect)", p.manifest().name);
    for i in &includes {
        let _ = writeln!(t, ".include \"{i}\"");
    }
    if !sources.is_empty() {
        t.push_str("* supplies\n");
        t.push_str(&sources);
    }
    t.push_str(&body);
    for m in models.values() {
        let _ = writeln!(t, "{m}");
    }
    for a in &opts.analysis {
        let _ = writeln!(t, "{a}");
    }
    if !opts.control.is_empty() {
        t.push_str(".control\n");
        for l in &opts.control {
            let _ = writeln!(t, "{l}");
        }
        t.push_str(".endc\n");
    }
    t.push_str(".end\n");
    if nodes.nc > 0 {
        out.diagnostics.push(Diagnostic::info(
            "spice.unconnected_pins",
            format!("{} unconnected pin(s) got their own node", nodes.nc),
        ));
    }
    out.nodes = nodes.names;
    out.text = t;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let q = |s: &str| Quantity::parse(s).unwrap();
        assert_eq!(spice_number(&q("10k")), "10k");
        assert_eq!(spice_number(&q("4.7uF")), "4.7u");
        assert_eq!(spice_number(&q("1MΩ")), "1Meg");
        assert_eq!(spice_number(&q("2.2M")), "2.2Meg");
        assert_eq!(spice_number(&q("100nF")), "100n");
        assert_eq!(spice_number(&q("3.3V")), "3.3");
        assert_eq!(spice_number(&q("0.5")), "500m");
    }

    #[test]
    fn rails() {
        let v = |s: &str| rail_voltage(s).map(|q| q.to_string());
        assert_eq!(v("3V3").as_deref(), Some("3.3V"));
        assert_eq!(v("+5V").as_deref(), Some("5V"));
        assert_eq!(v("VCC_1V8").as_deref(), Some("1.8V"));
        assert_eq!(v("12V").as_deref(), Some("12V"));
        assert_eq!(v("-12V").as_deref(), Some("-12V"));
        assert_eq!(v("5V0").as_deref(), Some("5V"));
        assert_eq!(v("VBUS"), None);
        assert_eq!(v("GND"), None);
        assert_eq!(v("VDD"), None);
    }

    #[test]
    fn node_names() {
        let mut used = BTreeSet::from(["0".to_string()]);
        assert_eq!(unique(sanitize("D+"), &mut used), "D_P");
        assert_eq!(unique(sanitize("D-"), &mut used), "D_N");
        assert_eq!(unique(sanitize("3V3"), &mut used), "N3V3");
        assert_eq!(unique(sanitize("/usb/x"), &mut used), "_usb_x");
        assert_eq!(unique(sanitize("a"), &mut used), "a");
        assert_eq!(unique(sanitize("A"), &mut used), "A_2");
    }
}
