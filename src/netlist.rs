//! Netlist export: KiCad netlist (`.net`, S-expression format version "E") for interoperability
//! and oracle tests, and a plain JSON netlist.
//!
//! The KiCad writer follows the format as documented and as seen in files KiCad writes; it shares
//! no code with KiCad (DECISIONS D7). Output is deterministic: no dates, sorted entries.

use std::fmt::Write as _;

use serde::Serialize;

use crate::model::Project;
use crate::model::part::{Part, PinKind};
use crate::model::sections::natural_cmp;

/// Library name used for cadlab parts and footprints in exported files.
pub const LIB: &str = "cadlab";

fn q(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// KiCad electrical type names.
pub fn kicad_pin_type(k: PinKind) -> &'static str {
    match k {
        PinKind::Input => "input",
        PinKind::Output => "output",
        PinKind::Bidirectional => "bidirectional",
        PinKind::TriState => "tri_state",
        PinKind::Passive => "passive",
        PinKind::PowerIn => "power_in",
        PinKind::PowerOut => "power_out",
        PinKind::OpenCollector => "open_collector",
        PinKind::OpenEmitter => "open_emitter",
        PinKind::NoConnect => "no_connect",
        PinKind::Unspecified => "unspecified",
    }
}

fn pin_info<'a>(part: Option<&'a Part>, number: &str) -> (&'a str, PinKind) {
    part.and_then(|p| p.symbol.pins.iter().find(|s| s.number == number))
        .map_or(("", PinKind::Unspecified), |s| (s.name.as_str(), s.kind))
}

/// KiCad netlist (`.net`).
pub fn kicad(p: &Project) -> String {
    let c = p.circuit();
    let lib = p.library();
    let mut s = String::new();
    s += "(export (version \"E\")\n";
    let _ = writeln!(
        s,
        "  (design\n    (source {})\n    (tool {}))",
        q(&p.manifest().name),
        q(&format!("cadlab {}", env!("CARGO_PKG_VERSION")))
    );

    s += "  (components";
    for refdes in c.refdes_sorted() {
        let comp = &c.components[refdes];
        let part = lib.parts.get(&comp.part);
        let value = part.map(Part::value).unwrap_or_else(|| comp.part.clone());
        let _ = write!(s, "\n    (comp (ref {})\n      (value {})", q(refdes), q(&value));
        if let Some(fp) = part.and_then(Part::footprint) {
            let _ = write!(s, "\n      (footprint {})", q(&format!("{LIB}:{}", fp.footprint)));
        }
        if let Some(ds) = part.and_then(|p| p.datasheet.as_deref()) {
            let _ = write!(s, "\n      (datasheet {})", q(ds));
        }
        let desc = part.map(|p| p.description.as_str()).unwrap_or("");
        let _ = write!(s, "\n      (libsource (lib {}) (part {}) (description {}))", q(LIB), q(&comp.part), q(desc));
        if let Some(p) = part {
            if let Some(m) = &p.manufacturer {
                let _ = write!(s, "\n      (property (name \"Manufacturer\") (value {}))", q(m));
            }
            if let Some(m) = &p.mpn {
                let _ = write!(s, "\n      (property (name \"MPN\") (value {}))", q(m));
            }
        }
        if p.bom().dnp.contains(refdes) {
            s += "\n      (property (name \"dnp\") (value \"\"))";
        }
        s += "\n      (sheetpath (names \"/\") (tstamps \"/\")))";
    }
    s += ")\n";

    s += "  (libparts";
    let mut used: Vec<&str> = c.components.values().map(|x| x.part.as_str()).collect();
    used.sort();
    used.dedup();
    for id in used {
        let Some(part) = lib.parts.get(id) else { continue };
        let _ =
            write!(s, "\n    (libpart (lib {}) (part {})\n      (description {})", q(LIB), q(id), q(&part.description));
        let _ = write!(
            s,
            "\n      (fields\n        (field (name \"Reference\") {})\n        (field (name \"Value\") {}))",
            q(part.category.refdes_prefix()),
            q(&part.value())
        );
        s += "\n      (pins";
        let mut pins: Vec<_> = part.symbol.pins.iter().collect();
        pins.sort_by(|a, b| natural_cmp(&a.number, &b.number));
        for pin in pins {
            let name = if pin.name.is_empty() { "~" } else { pin.name.as_str() };
            let _ = write!(
                s,
                "\n        (pin (num {}) (name {}) (type {}))",
                q(&pin.number),
                q(name),
                q(kicad_pin_type(pin.kind))
            );
        }
        s += "))";
    }
    s += ")\n";

    s += "  (nets";
    for (code, (name, net)) in c.nets.iter().enumerate() {
        let _ = write!(s, "\n    (net (code {}) (name {})", q(&(code + 1).to_string()), q(name));
        for pin in &net.pins {
            let part = c.components.get(&pin.refdes).and_then(|x| lib.parts.get(&x.part));
            let (fname, kind) = pin_info(part, &pin.pin);
            let _ = write!(s, "\n      (node (ref {}) (pin {})", q(&pin.refdes), q(&pin.pin));
            if !fname.is_empty() {
                let _ = write!(s, " (pinfunction {})", q(fname));
            }
            let _ = write!(s, " (pintype {}))", q(kicad_pin_type(kind)));
        }
        s += ")";
    }
    s += "))\n";
    s
}

/// JSON netlist.
#[derive(Serialize)]
pub struct JsonNetlist {
    /// Components in natural order.
    pub components: Vec<JsonComponent>,
    /// Nets by name order.
    pub nets: Vec<JsonNet>,
    /// Pins marked no-connect.
    pub no_connect: Vec<String>,
}

/// A component in the JSON netlist.
#[derive(Serialize)]
pub struct JsonComponent {
    /// Designator.
    pub refdes: String,
    /// Part ID.
    pub part: String,
    /// Value.
    pub value: String,
    /// Footprint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    /// Manufacturer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// MPN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
}

/// A net in the JSON netlist.
#[derive(Serialize)]
pub struct JsonNet {
    /// Name.
    pub name: String,
    /// Net class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Pins: `{"pin": "U1.1", "name": "VIN", "type": "power_in"}`.
    pub pins: Vec<serde_json::Value>,
}

/// JSON netlist.
pub fn json(p: &Project) -> JsonNetlist {
    let c = p.circuit();
    let lib = p.library();
    let components = c
        .refdes_sorted()
        .into_iter()
        .map(|r| {
            let comp = &c.components[r];
            let part = lib.parts.get(&comp.part);
            JsonComponent {
                refdes: r.to_string(),
                part: comp.part.clone(),
                value: part.map(Part::value).unwrap_or_default(),
                footprint: part.and_then(Part::footprint).map(|f| f.footprint.clone()),
                manufacturer: part.and_then(|p| p.manufacturer.clone()),
                mpn: part.and_then(|p| p.mpn.clone()),
            }
        })
        .collect();
    let nets = c
        .nets
        .iter()
        .map(|(name, net)| JsonNet {
            name: name.clone(),
            class: net.class.clone(),
            pins: net
                .pins
                .iter()
                .map(|pin| {
                    let part = c.components.get(&pin.refdes).and_then(|x| lib.parts.get(&x.part));
                    let (n, k) = pin_info(part, &pin.pin);
                    serde_json::json!({"pin": pin.to_string(), "name": n, "type": kicad_pin_type(k)})
                })
                .collect(),
        })
        .collect();
    JsonNetlist { components, nets, no_connect: c.no_connect.iter().map(ToString::to_string).collect() }
}

/// Compact text summary for humans and LLM context: components grouped by part, then nets.
pub fn summary(p: &Project) -> String {
    let c = p.circuit();
    let lib = p.library();
    let mut s = format!("{}: {} components, {} nets\n", p.manifest().name, c.components.len(), c.nets.len());
    s += "components:\n";
    for refdes in c.refdes_sorted() {
        let comp = &c.components[refdes];
        let part = lib.parts.get(&comp.part);
        let what = match part {
            Some(pt) => match &pt.mpn {
                Some(m) if pt.value() != *m => format!("{} ({m})", pt.value()),
                _ => pt.value(),
            },
            None => format!("missing part {}", comp.part),
        };
        let fp = part.and_then(Part::footprint).map(|f| format!(" [{}]", f.footprint)).unwrap_or_default();
        let _ = writeln!(s, "  {refdes}: {what}{fp}");
    }
    s += "nets:\n";
    for (name, net) in &c.nets {
        let pins: Vec<String> = net
            .pins
            .iter()
            .map(|pin| {
                let part = c.components.get(&pin.refdes).and_then(|x| lib.parts.get(&x.part));
                let (n, _) = pin_info(part, &pin.pin);
                if n.is_empty() || n == pin.pin { pin.to_string() } else { format!("{pin}({n})") }
            })
            .collect();
        let mut flags = String::new();
        if let Some(cl) = &net.class {
            let _ = write!(flags, " [{cl}]");
        }
        if net.driven {
            flags += " [driven]";
        }
        let _ = writeln!(s, "  {name}{flags}: {}", pins.join(" "));
    }
    if !c.no_connect.is_empty() {
        let nc: Vec<String> = c.no_connect.iter().map(ToString::to_string).collect();
        let _ = writeln!(s, "no-connect: {}", nc.join(" "));
    }
    s
}
