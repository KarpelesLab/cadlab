//! KiCad netlist import (DECISIONS D13): circuits come into cadlab as netlists, never as
//! `.kicad_sch` files.
//!
//! The reader follows the S-expression netlist format ("E") as documented by KiCad and as seen in
//! files `kicad-cli sch export netlist` writes; it shares no code with KiCad (DECISIONS D7).
//! [`parse_kicad`] reads a file into a [`KicadNetlist`], [`import`] adds it to a project:
//!
//! - **Components** keep their designators. Each is matched to a library part, in order: the
//!   symbol's library part name as a project part ID (with the same value), the `MPN` field, then
//!   value and footprint name. Otherwise passives (R, C, L, FB, D, LED) become generic parts when
//!   value and footprint name give a spec (`R 10k 0402`), and everything else gets a part built
//!   from the netlist (pins, names and types from the `libparts` section, footprint generated when
//!   the footprint name is a package cadlab knows, MPN and manufacturer from fields). Parts left
//!   without a footprint are placeholders, reported per component (`import.placeholder_part`).
//! - **Nets** keep their names, without KiCad's root sheet prefix (`/LED` → `LED`; sub-sheet
//!   paths stay, `/power/VIN` → `power/VIN`). Single-pin `unconnected-(...)` nets, which KiCad
//!   writes for every unconnected pin, are skipped. Net classes are kept when the project
//!   defines a class of that name.
//! - Power symbols and flags (`#PWR`, `#FLG` designators) are not components. `kicad-cli` leaves
//!   them out of netlists altogether, so a `PWR_FLAG` cannot be recovered: nets powered from a
//!   connector must be marked again (`net.set <net> --driven`).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diag::Diagnostic;
use crate::landpattern::{self, ChipKind, GenOptions};
use crate::model::Project;
use crate::model::circuit::valid_refdes;
use crate::model::footprint::Footprint;
use crate::model::part::{
    Category, FootprintRef, Origin, ParamValue, Params, Part, Pin, PinKind, Provenance, slugify, valid_id,
};
use crate::model::sections::{Component, Net, PinRef, natural_cmp};
use crate::refs::ObjectRef;
use crate::sexpr::{self, Sexpr};

/// A KiCad netlist, as read from the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KicadNetlist {
    /// `(design (source ...))`: the schematic it came from.
    pub source: Option<String>,
    /// `(design (tool ...))`: the program that wrote it.
    pub tool: Option<String>,
    /// Components, in file order (power symbols included).
    pub components: Vec<NetlistComponent>,
    /// Library parts (symbols): pins with names and types.
    pub libparts: Vec<LibPart>,
    /// Nets, in file order.
    pub nets: Vec<NetlistNet>,
}

/// A component (`comp`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetlistComponent {
    /// Reference designator, as written.
    pub refdes: String,
    /// Value.
    pub value: String,
    /// Footprint, `lib:name`.
    pub footprint: Option<String>,
    /// Datasheet URL.
    pub datasheet: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Symbol library (`libsource lib`).
    pub lib: Option<String>,
    /// Symbol name (`libsource part`).
    pub part: Option<String>,
    /// Other fields and properties (MPN, Manufacturer, user fields), by name.
    pub fields: BTreeMap<String, String>,
    /// Marked do-not-populate.
    pub dnp: bool,
}

/// A library part (`libpart`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibPart {
    /// Library.
    pub lib: String,
    /// Part (symbol) name.
    pub part: String,
    /// Description.
    pub description: Option<String>,
    /// Pins.
    pub pins: Vec<NetlistPin>,
}

/// A pin of a library part.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetlistPin {
    /// Pin number.
    pub number: String,
    /// Pin name (empty when unnamed).
    pub name: String,
    /// Electrical type, when known.
    pub kind: Option<PinKind>,
}

/// A net.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetlistNet {
    /// Net code.
    pub code: String,
    /// Name, as written (`/LED`, `GND`, `Net-(D1-K)`).
    pub name: String,
    /// Net class, if not the default.
    pub class: Option<String>,
    /// Connected pins.
    pub nodes: Vec<Node>,
}

/// A pin on a net.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// Reference designator, as written.
    pub refdes: String,
    /// Pin number.
    pub pin: String,
    /// Pin name (`pinfunction`).
    pub function: Option<String>,
    /// Pin type (`pintype`).
    pub kind: Option<PinKind>,
}

/// How a component's part was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// A project part whose ID is the symbol's library part name (and with the same value).
    Existing,
    /// A project part with the component's MPN.
    Mpn,
    /// A project part with the same value and footprint name.
    ValueFootprint,
    /// A generic part from value and package (`R 10k 0402`), existing or created.
    Generic,
    /// A part created from the netlist: pins, a footprint, and the MPN when the netlist has one.
    Created,
    /// A part created from the netlist without a footprint: it cannot be placed yet.
    Placeholder,
}

/// A part used by imported components.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedPart {
    /// Part ID in the project library.
    pub part: String,
    /// How it was found.
    pub resolution: Resolution,
    /// Whether the import added it to the library.
    pub created: bool,
    /// Components using it.
    pub refdes: Vec<String>,
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportReport {
    /// Schematic the netlist was exported from, if given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Components added.
    pub components: usize,
    /// Nets added or extended.
    pub nets: usize,
    /// Parts used, by resolution then ID.
    pub parts: Vec<ImportedPart>,
    /// Components removed first (`replace`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub replaced: usize,
    /// Single-pin `unconnected-(...)` nets skipped.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unconnected_skipped: usize,
    /// Power symbols and flags skipped (`#PWR01`, `#FLG01`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub power_symbols: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Kind of import failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportErrorKind {
    /// The file is not a usable netlist.
    Invalid,
    /// The netlist conflicts with the project.
    Conflict,
}

/// An import failure, with a stable code and a hint.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ImportError {
    /// Kind.
    pub kind: ImportErrorKind,
    /// Stable code (`import.parse`, `import.refdes_taken`, ...).
    pub code: &'static str,
    /// Message.
    pub message: String,
    /// How to fix it.
    pub hint: String,
    /// Objects concerned.
    pub subjects: Vec<ObjectRef>,
}

fn invalid(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> ImportError {
    ImportError { kind: ImportErrorKind::Invalid, code, message: message.into(), hint: hint.into(), subjects: vec![] }
}

/// KiCad pin type name → pin kind (`free` is treated as passive). A `+no_connect` suffix, which
/// KiCad adds to unconnected pins, is ignored.
pub fn pin_kind(t: &str) -> Option<PinKind> {
    let t = t.split('+').next().unwrap_or(t);
    Some(match t {
        "input" => PinKind::Input,
        "output" => PinKind::Output,
        "bidirectional" => PinKind::Bidirectional,
        "tri_state" => PinKind::TriState,
        "passive" | "free" => PinKind::Passive,
        "power_in" => PinKind::PowerIn,
        "power_out" => PinKind::PowerOut,
        "open_collector" => PinKind::OpenCollector,
        "open_emitter" => PinKind::OpenEmitter,
        "no_connect" => PinKind::NoConnect,
        "unspecified" => PinKind::Unspecified,
        _ => return None,
    })
}

/// Fields that are not carried over as component properties.
pub(crate) const STANDARD_FIELDS: &[&str] = &[
    "reference",
    "value",
    "footprint",
    "datasheet",
    "description",
    "sheetname",
    "sheetfile",
    "dnp",
    "exclude_from_bom",
    "exclude_from_board",
    "exclude_from_sim",
];

/// Field names holding a manufacturer part number (case-insensitive).
const MPN_FIELDS: &[&str] = &[
    "mpn",
    "manufacturer part number",
    "manufacturer_part_number",
    "manufacturer pn",
    "mfr part number",
    "mfr. part number",
    "mfr_pn",
    "mfr pn",
    "mfg part number",
    "mfg_pn",
    "part number",
];

/// Field names holding the manufacturer (case-insensitive).
const MANUFACTURER_FIELDS: &[&str] = &["manufacturer", "manufacturer_name", "mfr", "mfg"];

impl NetlistComponent {
    fn field(&self, names: &[&str]) -> Option<(&str, &str)> {
        names.iter().find_map(|n| {
            self.fields
                .iter()
                .find(|(k, v)| k.eq_ignore_ascii_case(n) && !v.trim().is_empty())
                .map(|(k, v)| (k.as_str(), v.trim()))
        })
    }

    /// Manufacturer part number, from the first MPN-like field.
    pub fn mpn(&self) -> Option<&str> {
        self.field(MPN_FIELDS).map(|f| f.1)
    }

    /// Manufacturer, from the first manufacturer field.
    pub fn manufacturer(&self) -> Option<&str> {
        self.field(MANUFACTURER_FIELDS).map(|f| f.1)
    }

    /// Footprint name without its library (`cadlab:CAPC1005X55N` → `CAPC1005X55N`).
    pub fn footprint_name(&self) -> Option<&str> {
        let f = self.footprint.as_deref()?.trim();
        let name = f.split_once(':').map_or(f, |(_, n)| n);
        (!name.is_empty()).then_some(name)
    }
}

fn text(x: Option<&str>) -> Option<String> {
    x.map(str::trim).filter(|s| !s.is_empty() && *s != "~").map(String::from)
}

/// Reads a KiCad netlist (`kicad-cli sch export netlist`, S-expression format).
pub fn parse_kicad(input: &str) -> Result<KicadNetlist, ImportError> {
    let root = sexpr::parse(input).map_err(|e| {
        invalid(
            "import.parse",
            format!("not a readable netlist: {e}"),
            "export the netlist with `kicad-cli sch export netlist` (KiCad S-expression format)",
        )
    })?;
    if root.head() != Some("export") {
        return Err(invalid(
            "import.not_kicad_netlist",
            format!("the file starts with `({}`, not `(export`", root.head().unwrap_or("")),
            "give a KiCad netlist (`kicad-cli sch export netlist --format kicadsexpr`); other formats are not read",
        ));
    }
    let mut nl = KicadNetlist::default();
    if let Some(d) = root.get("design") {
        nl.source = text(d.child_value("source"));
        nl.tool = text(d.child_value("tool"));
    }
    for c in root.get("components").map(|c| c.all("comp").collect::<Vec<_>>()).unwrap_or_default() {
        let Some(refdes) = text(c.child_value("ref")) else {
            return Err(invalid("import.parse", "a component has no `ref`", "check the netlist file is complete"));
        };
        let mut comp = NetlistComponent {
            refdes,
            value: text(c.child_value("value")).unwrap_or_default(),
            footprint: text(c.child_value("footprint")),
            datasheet: text(c.child_value("datasheet")),
            description: text(c.child_value("description")),
            ..Default::default()
        };
        if let Some(ls) = c.get("libsource") {
            comp.lib = text(ls.child_value("lib"));
            comp.part = text(ls.child_value("part"));
            if comp.description.is_none() {
                comp.description = text(ls.child_value("description"));
            }
        }
        // `(fields (field (name "MPN") "value"))` and `(property (name "MPN") (value "value"))`.
        let mut named: Vec<(String, Option<String>)> = Vec::new();
        if let Some(fs) = c.get("fields") {
            for f in fs.all("field") {
                let Some(name) = f.child_value("name") else { continue };
                named.push((name.to_string(), f.items().iter().skip(1).find_map(Sexpr::atom).map(String::from)));
            }
        }
        for p in c.all("property") {
            let Some(name) = p.child_value("name") else { continue };
            named.push((name.to_string(), p.child_value("value").map(String::from)));
        }
        for (name, value) in named {
            let lower = name.to_ascii_lowercase();
            if lower == "dnp" {
                comp.dnp = !matches!(value.as_deref().map(str::trim), Some("0" | "no" | "false"));
                continue;
            }
            if STANDARD_FIELDS.contains(&lower.as_str()) || lower.starts_with("ki_") {
                continue;
            }
            if let Some(v) = text(value.as_deref()) {
                comp.fields.entry(name).or_insert(v);
            }
        }
        nl.components.push(comp);
    }
    for lp in root.get("libparts").map(|c| c.all("libpart").collect::<Vec<_>>()).unwrap_or_default() {
        let mut part = LibPart {
            lib: text(lp.child_value("lib")).unwrap_or_default(),
            part: text(lp.child_value("part")).unwrap_or_default(),
            description: text(lp.child_value("description")),
            pins: Vec::new(),
        };
        if let Some(pins) = lp.get("pins") {
            for p in pins.all("pin") {
                let Some(number) = text(p.child_value("num")) else { continue };
                let name = text(p.child_value("name")).filter(|n| *n != number).unwrap_or_default();
                part.pins.push(NetlistPin { number, name, kind: p.child_value("type").and_then(pin_kind) });
            }
        }
        nl.libparts.push(part);
    }
    for n in root.get("nets").map(|c| c.all("net").collect::<Vec<_>>()).unwrap_or_default() {
        let mut net = NetlistNet {
            code: text(n.child_value("code")).unwrap_or_default(),
            name: n.child_value("name").unwrap_or_default().to_string(),
            class: text(n.child_value("class")),
            nodes: Vec::new(),
        };
        for node in n.all("node") {
            let (Some(refdes), Some(pin)) = (text(node.child_value("ref")), text(node.child_value("pin"))) else {
                continue;
            };
            net.nodes.push(Node {
                refdes,
                pin,
                function: text(node.child_value("pinfunction")),
                kind: node.child_value("pintype").and_then(pin_kind),
            });
        }
        nl.nets.push(net);
    }
    Ok(nl)
}

/// Import options.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Remove every component and net first (board placements of components that come back
    /// are kept).
    pub replace: bool,
    /// Name of the imported file, recorded in the provenance of created parts.
    pub file_name: String,
}

/// Net name in cadlab: KiCad's root sheet prefix removed, `{slash}` unescaped.
pub fn net_name(kicad: &str, code: &str) -> String {
    let n = kicad.strip_prefix('/').unwrap_or(kicad).replace("{slash}", "/");
    if n.trim().is_empty() { format!("Net-{code}") } else { n }
}

/// Category for a component, from its designator prefix, refined by the description (cadlab's
/// own exports start descriptions with the category label: "LDO regulator AMS1117-3.3").
fn category_for(refdes: &str, description: Option<&str>, hints: &str) -> Category {
    const ALL: &[Category] = &[
        Category::Resistor,
        Category::Capacitor,
        Category::Inductor,
        Category::FerriteBead,
        Category::Diode,
        Category::Led,
        Category::TransistorBjt,
        Category::Mosfet,
        Category::Ldo,
        Category::Regulator,
        Category::Mcu,
        Category::Ic,
        Category::Connector,
        Category::Crystal,
        Category::Oscillator,
        Category::Switch,
        Category::Fuse,
        Category::TestPoint,
        Category::Mechanical,
    ];
    let prefix = refdes.trim_end_matches(|c: char| c.is_ascii_digit());
    if let Some(d) = description
        && let Some(c) = ALL.iter().find(|c| d.starts_with(&format!("{} ", c.label())) && c.refdes_prefix() == prefix)
    {
        return *c;
    }
    let h = hints.to_ascii_uppercase();
    match prefix {
        "R" => Category::Resistor,
        "C" => Category::Capacitor,
        "L" => Category::Inductor,
        "FB" => Category::FerriteBead,
        "LED" => Category::Led,
        "D" if h.contains("LED") => Category::Led,
        "D" => Category::Diode,
        "Q" if h.contains("FET") || h.contains("MOS") => Category::Mosfet,
        "Q" => Category::TransistorBjt,
        "U" | "IC" => Category::Ic,
        "J" | "P" | "CN" | "CON" => Category::Connector,
        "Y" => Category::Crystal,
        "SW" | "S" | "BTN" => Category::Switch,
        "F" => Category::Fuse,
        "TP" => Category::TestPoint,
        "H" | "MH" | "FID" => Category::Mechanical,
        _ => Category::Other,
    }
}

fn chip_kind(c: Category) -> ChipKind {
    match c {
        Category::Capacitor => ChipKind::Capacitor,
        Category::Inductor | Category::FerriteBead => ChipKind::Inductor,
        Category::Led => ChipKind::Led,
        Category::Diode => ChipKind::Diode,
        Category::Fuse => ChipKind::Fuse,
        _ => ChipKind::Resistor,
    }
}

/// Imperial chip size named by a footprint: IPC-7351 names (`RESC1005X40N` → `0402`) or
/// names containing the size (`R_0603_1608Metric` → `0603`).
fn chip_size(footprint: &str) -> Option<String> {
    let up = footprint.to_ascii_uppercase();
    for pre in ["RESC", "CAPC", "INDC", "LEDC", "DIOC", "FUSC"] {
        if let Some(rest) = up.strip_prefix(pre)
            && let Some(m) = rest.get(..4)
            && m.chars().all(|c| c.is_ascii_digit())
        {
            return landpattern::packages::imperial_from_metric(m).map(String::from);
        }
    }
    for t in up.split(['_', '-', ' ', '.']) {
        if landpattern::packages::chip_codes().any(|c| c == t) {
            return Some(t.to_string());
        }
        if let Some(m) = t.strip_suffix("METRIC") {
            return landpattern::packages::imperial_from_metric(m).map(String::from);
        }
    }
    None
}

/// The generic spec letter for a category, if it has generic parts.
fn generic_prefix(c: Category) -> Option<&'static str> {
    Some(match c {
        Category::Resistor => "R",
        Category::Capacitor => "C",
        Category::Inductor => "L",
        Category::FerriteBead => "FB",
        Category::Led => "LED",
        Category::Diode => "D",
        _ => return None,
    })
}

/// Pins of one component seen on nets: number → (name, type).
type NodePins = BTreeMap<String, (Option<String>, Option<PinKind>)>;

/// Pins of a component: the library part's pins, plus any pin seen on a net.
fn component_pins(lp: Option<&LibPart>, nodes: &NodePins) -> Vec<Pin> {
    let mut pins: BTreeMap<String, Pin> = BTreeMap::new();
    if let Some(lp) = lp {
        for p in &lp.pins {
            pins.entry(p.number.clone())
                .or_insert_with(|| Pin::new(p.number.clone(), p.name.clone(), p.kind.unwrap_or(PinKind::Passive)));
        }
    }
    for (num, (function, kind)) in nodes {
        pins.entry(num.clone()).or_insert_with(|| {
            let name = function.clone().filter(|f| f != num && f != "~").unwrap_or_default();
            Pin::new(num.clone(), name, kind.unwrap_or(PinKind::Passive))
        });
    }
    let mut v: Vec<Pin> = pins.into_values().collect();
    v.sort_by(|a, b| natural_cmp(&a.number, &b.number));
    v
}

/// Whether `part` has every pin in `needed`.
fn has_pins(part: &Part, needed: &BTreeSet<String>) -> bool {
    needed.iter().all(|n| part.symbol.pins.iter().any(|p| &p.number == n))
}

/// Components that will share one created part.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    category: Category,
    symbol: Option<(String, String)>,
    value: String,
    footprint: Option<String>,
    mpn: Option<String>,
    manufacturer: Option<String>,
}

/// Imports `nl` into the project. Returns the report and the diagnostics to show.
pub fn import(
    p: &mut Project,
    nl: &KicadNetlist,
    opts: &ImportOptions,
) -> Result<(ImportReport, Vec<Diagnostic>), ImportError> {
    let mut diags = Vec::new();
    let mut report = ImportReport { source: nl.source.clone(), ..Default::default() };

    // Components: designators checked first, so nothing changes on error.
    let mut comps: Vec<(String, &NetlistComponent)> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut invalid_refs = Vec::new();
    for c in &nl.components {
        if c.refdes.starts_with('#') {
            report.power_symbols += 1;
            continue;
        }
        let r = c.refdes.trim().to_uppercase();
        if !valid_refdes(&r) {
            invalid_refs.push(c.refdes.clone());
            continue;
        }
        if !seen.insert(r.clone()) {
            diags.push(
                Diagnostic::warning("import.duplicate_refdes", format!("`{r}` appears twice; the first one is kept"))
                    .with_subject(ObjectRef::Name(r.clone()))
                    .with_hint("annotate the schematic again in KiCad so designators are unique"),
            );
            continue;
        }
        comps.push((r, c));
    }
    if !invalid_refs.is_empty() {
        return Err(ImportError {
            subjects: invalid_refs.iter().map(|r| ObjectRef::Name(r.clone())).collect(),
            ..invalid(
                "import.invalid_refdes",
                format!("not reference designators (letters then a number): {}", invalid_refs.join(", ")),
                "annotate the schematic in KiCad so every symbol has a designator like R1 or U12, then export the netlist again",
            )
        });
    }
    if comps.is_empty() {
        return Err(invalid(
            "import.empty",
            "the netlist has no components",
            "export the netlist from a schematic that has symbols (power symbols are not components)",
        ));
    }
    if opts.replace {
        let keep: BTreeSet<&str> = comps.iter().map(|(r, _)| r.as_str()).collect();
        report.replaced = p.circuit().components.len();
        let c = p.circuit_mut();
        c.components.clear();
        c.nets.clear();
        c.no_connect.clear();
        c.instances.clear();
        p.bom_mut().dnp.clear();
        p.board_mut().footprints.retain(|r, _| keep.contains(r.as_str()));
    } else {
        let taken: Vec<&str> =
            comps.iter().map(|(r, _)| r.as_str()).filter(|r| p.circuit().components.contains_key(*r)).collect();
        if !taken.is_empty() {
            return Err(ImportError {
                kind: ImportErrorKind::Conflict,
                code: "import.refdes_taken",
                message: format!("the circuit already has {}", taken.join(", ")),
                hint: "pass `replace: true` to replace the circuit with the netlist, or rename or remove the existing components first".into(),
                subjects: taken.iter().map(|r| ObjectRef::Name((*r).to_string())).collect(),
            });
        }
    }

    // Pins seen on nets, per component: number → (name, type).
    let known: BTreeSet<&str> = comps.iter().map(|(r, _)| r.as_str()).collect();
    // Pins a matched part must have: those on nets that are kept (not on a skipped one-pin
    // `unconnected-(...)` net).
    let mut node_pins: BTreeMap<String, NodePins> = BTreeMap::new();
    let mut connected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for net in &nl.nets {
        let nodes: Vec<(String, &Node)> = net
            .nodes
            .iter()
            .map(|n| (n.refdes.trim().to_uppercase(), n))
            .filter(|(r, _)| known.contains(r.as_str()))
            .collect();
        let skipped = net.name.starts_with("unconnected-") && nodes.len() == 1;
        for (r, n) in nodes {
            if !skipped {
                connected.entry(r.clone()).or_default().insert(n.pin.clone());
            }
            node_pins.entry(r).or_default().entry(n.pin.clone()).or_insert((n.function.clone(), n.kind));
        }
    }
    let libpart = |c: &NetlistComponent| {
        nl.libparts.iter().find(|l| Some(&l.part) == c.part.as_ref() && Some(&l.lib) == c.lib.as_ref())
    };

    // Resolve each component to a part, or to a group that gets a created part.
    enum Pending {
        Part(String, Resolution, bool),
        Group(GroupKey),
    }
    let empty = BTreeMap::new();
    let mut resolved: Vec<(String, &NetlistComponent, Pending)> = Vec::new();
    for (r, c) in &comps {
        let needed: BTreeSet<String> = connected.get(r).cloned().unwrap_or_default();
        let lp = libpart(c);
        let lib = p.library();
        let fp_name = c.footprint_name();
        let reject = |part: &Part, how: &str, diags: &mut Vec<Diagnostic>| {
            let missing: Vec<&str> = needed
                .iter()
                .filter(|n| !part.symbol.pins.iter().any(|q| &q.number == *n))
                .map(String::as_str)
                .collect();
            diags.push(
                Diagnostic::warning(
                    "import.pin_mismatch",
                    format!(
                        "`{r}` matches library part `{}` by {how}, but the part has no pin(s) {}; not used",
                        part.id,
                        missing.join(", ")
                    ),
                )
                .with_subject(ObjectRef::Name(r.clone()))
                .with_subject(ObjectRef::Part { scheme: "local".into(), id: part.id.clone() })
                .with_hint("the schematic symbol and the library part number their pins differently; fix one of them, or use the part created by the import"),
            );
        };
        let mut found: Option<(String, Resolution)> = None;
        // 1. Symbol name as part ID, same value.
        if let Some(id) = c.part.as_deref()
            && let Some(part) = lib.parts.get(id).or_else(|| lib.find_part_id_ci(id).map(|k| &lib.parts[k]))
            && part.value().eq_ignore_ascii_case(&c.value)
        {
            if has_pins(part, &needed) {
                found = Some((part.id.clone(), Resolution::Existing));
            } else {
                reject(part, "name", &mut diags);
            }
        }
        // 2. MPN.
        if found.is_none()
            && let Some(mpn) = c.mpn()
            && let Some(part) =
                lib.parts.values().find(|pt| pt.mpn.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(mpn)))
        {
            if has_pins(part, &needed) {
                found = Some((part.id.clone(), Resolution::Mpn));
            } else {
                reject(part, "MPN", &mut diags);
            }
        }
        // 3. Value and footprint name.
        if found.is_none()
            && c.mpn().is_none()
            && let Some(fp) = fp_name
            && let Some(part) = lib.parts.values().find(|pt| {
                pt.value().eq_ignore_ascii_case(&c.value) && pt.footprint().is_some_and(|f| f.footprint == fp)
            })
        {
            if has_pins(part, &needed) {
                found = Some((part.id.clone(), Resolution::ValueFootprint));
            } else {
                reject(part, "value and footprint", &mut diags);
            }
        }
        if let Some((id, how)) = found {
            resolved.push((r.clone(), c, Pending::Part(id, how, false)));
            continue;
        }
        let hints = format!(
            "{} {} {}",
            c.value,
            c.part.as_deref().unwrap_or(""),
            lp.and_then(|l| l.description.as_deref()).unwrap_or("")
        );
        let category = category_for(r, c.description.as_deref(), &hints);
        // 4. Generic passive, when there is no MPN to keep.
        if c.mpn().is_none()
            && let Some(prefix) = generic_prefix(category)
            && let Some(size) = fp_name.and_then(chip_size)
        {
            let mut specs = vec![format!("{prefix} {} {size}", c.value)];
            if category == Category::Led {
                specs.push(format!("LED {size}"));
            }
            let generic = specs.iter().find_map(|s| crate::partspec::parse(s).ok());
            if let Some(spec) = generic {
                let id = spec.id();
                let existing = p.library().find_part_id_ci(&id).map(String::from);
                match existing {
                    Some(k) if has_pins(&p.library().parts[&k], &needed) => {
                        resolved.push((r.clone(), c, Pending::Part(k, Resolution::Generic, false)));
                        continue;
                    }
                    Some(_) => {}
                    None => {
                        if let Ok((part, fp)) = spec.build(&GenOptions::default())
                            && has_pins(&part, &needed)
                        {
                            add_footprint(p, fp, &mut diags);
                            p.library_mut().parts.insert(id.clone(), part);
                            resolved.push((r.clone(), c, Pending::Part(id, Resolution::Generic, true)));
                            continue;
                        }
                    }
                }
            }
        }
        // 5. A part made from the netlist.
        let key = GroupKey {
            category,
            symbol: c.lib.clone().zip(c.part.clone()),
            value: c.value.clone(),
            footprint: c.footprint.clone(),
            mpn: c.mpn().map(String::from),
            manufacturer: c.manufacturer().map(String::from),
        };
        resolved.push((r.clone(), c, Pending::Group(key)));
    }

    // Create one part per group, with the union of its components' pins.
    let mut groups: BTreeMap<GroupKey, Vec<(String, &NetlistComponent)>> = BTreeMap::new();
    for (r, c, pend) in &resolved {
        if let Pending::Group(k) = pend {
            groups.entry(k.clone()).or_default().push((r.clone(), c));
        }
    }
    let mut group_parts: BTreeMap<GroupKey, (String, Resolution, bool)> = BTreeMap::new();
    for (key, members) in &groups {
        let c = members[0].1;
        let mut pins: BTreeMap<String, Pin> = BTreeMap::new();
        for (r, m) in members {
            for pin in component_pins(libpart(m), node_pins.get(r).unwrap_or(&empty)) {
                pins.entry(pin.number.clone()).or_insert(pin);
            }
        }
        let mut pins: Vec<Pin> = pins.into_values().collect();
        pins.sort_by(|a, b| natural_cmp(&a.number, &b.number));
        if pins.is_empty() {
            // A symbol without pins (logo, mounting hole without pad): give it nothing to connect.
            pins.push(Pin::new("1", "", PinKind::Passive));
        }
        let made = create_part(p, key, c, libpart(c), pins, opts, &mut diags);
        group_parts.insert(key.clone(), made);
    }

    // Components.
    let mut used: BTreeMap<(String, Resolution, bool), Vec<String>> = BTreeMap::new();
    for (r, c, pend) in resolved {
        let (id, how, created) = match pend {
            Pending::Part(id, how, created) => (id, how, created),
            Pending::Group(k) => {
                let (id, how, created) = group_parts[&k].clone();
                if how == Resolution::Placeholder {
                    let why = match &c.footprint {
                        Some(f) => {
                            format!("its footprint `{f}` is not in the project and is no package cadlab can generate")
                        }
                        None => "the netlist gives no footprint".to_string(),
                    };
                    let generic = generic_prefix(k.category)
                        .map(|g| format!("`bom.replace {id} \"{g} <value> <package>\"` (a generic part), or "))
                        .unwrap_or_default();
                    diags.push(
                        Diagnostic::warning(
                            "import.placeholder_part",
                            format!("`{r}` (value `{}`) matched no library part and {why}; it uses the placeholder part `{id}`, which has no footprint", c.value),
                        )
                        .with_subject(ObjectRef::Name(r.clone()))
                        .with_subject(ObjectRef::Part { scheme: "local".into(), id: id.clone() })
                        .with_hint(format!(
                            "replace it: {generic}`part.create` (MPN, pins, package) then `bom.replace {id} <new part>`; or give it a footprint: `footprint.generate` (package name or dimensions), then `part.set {id} --footprint <name>`"
                        )),
                    );
                }
                (id, how, created)
            }
        };
        let mut properties = c.fields.clone();
        for k in [c.field(MPN_FIELDS).map(|f| f.0.to_string()), c.field(MANUFACTURER_FIELDS).map(|f| f.0.to_string())]
            .into_iter()
            .flatten()
        {
            properties.remove(&k);
        }
        let obj = p.alloc_id();
        p.circuit_mut().components.insert(r.clone(), Component { id: obj, part: id.clone(), block: None, properties });
        if c.dnp {
            p.bom_mut().dnp.insert(r.clone());
        }
        used.entry((id, how, created)).or_default().push(r);
    }
    report.components = comps.len();
    // One entry per part. A part created by this import and found again by later components
    // keeps the resolution it was created with.
    let mut by_part: BTreeMap<String, ImportedPart> = BTreeMap::new();
    for ((id, how, created), refs) in used {
        let e = by_part.entry(id.clone()).or_insert(ImportedPart {
            part: id,
            resolution: how,
            created: false,
            refdes: Vec::new(),
        });
        if created {
            e.resolution = how;
            e.created = true;
        }
        e.refdes.extend(refs);
    }
    let mut parts: Vec<ImportedPart> = by_part.into_values().collect();
    for ip in &mut parts {
        ip.refdes.sort_by(|a, b| natural_cmp(a, b));
    }
    parts.sort_by(|a, b| a.resolution.cmp(&b.resolution).then_with(|| a.part.cmp(&b.part)));
    report.parts = parts;

    // Nets.
    let mut names_used: BTreeSet<String> = BTreeSet::new();
    let mut unknown_classes: BTreeSet<String> = BTreeSet::new();
    for net in &nl.nets {
        let pins: BTreeSet<PinRef> = net
            .nodes
            .iter()
            .filter(|n| known.contains(n.refdes.trim().to_uppercase().as_str()))
            .map(|n| PinRef::new(n.refdes.trim().to_uppercase(), n.pin.clone()))
            .collect();
        if pins.is_empty() {
            continue;
        }
        if net.name.starts_with("unconnected-") && pins.len() == 1 {
            report.unconnected_skipped += 1;
            continue;
        }
        let mut name = net_name(&net.name, &net.code);
        if names_used.contains(&name) {
            name = net.name.clone();
            if names_used.contains(&name) {
                name = format!("{name}_{}", net.code);
            }
        }
        names_used.insert(name.clone());
        let class = match net.class.as_deref() {
            None | Some("Default") => None,
            Some(cl) if p.circuit().netclasses.contains_key(cl) => Some(cl.to_string()),
            Some(cl) => {
                unknown_classes.insert(cl.to_string());
                None
            }
        };
        let id = p.alloc_id();
        let c = p.circuit_mut();
        let entry = c.nets.entry(name).or_insert(Net::new(id));
        entry.pins.extend(pins);
        if entry.class.is_none() {
            entry.class = class;
        }
        report.nets += 1;
    }
    for cl in unknown_classes {
        diags.push(
            Diagnostic::info(
                "import.unknown_netclass",
                format!("net class `{cl}` is not defined in the project; its nets use the default rules"),
            )
            .with_subject(ObjectRef::Named { kind: "netclass".into(), name: cl.clone() })
            .with_hint(format!("define it with `netclass.set {cl}` and assign nets with `net.set`")),
        );
    }
    Ok((report, diags))
}

/// Adds a footprint unless one with that name exists (the project's copy is kept).
fn add_footprint(p: &mut Project, fp: Footprint, diags: &mut Vec<Diagnostic>) {
    let lib = p.library_mut();
    match lib.footprints.get(&fp.name) {
        None => {
            lib.footprints.insert(fp.name.clone(), fp);
        }
        Some(existing) if *existing == fp => {}
        Some(_) => diags.push(
            Diagnostic::info(
                "footprint.kept_existing",
                format!("footprint `{}` already exists in the project and differs from a fresh generation; keeping the project's copy", fp.name),
            )
            .with_hint("run `footprint.generate` with `replace: true` to update it"),
        ),
    }
}

/// A footprint for a created part: the project footprint of that name, or one generated from
/// a package the name gives. Only footprints with a pad for every pin are used.
fn find_footprint(
    p: &mut Project,
    name: &str,
    category: Category,
    pins: &[Pin],
    diags: &mut Vec<Diagnostic>,
) -> Option<(String, Option<String>)> {
    let fits = |fp: &Footprint| {
        let pads = fp.pad_numbers();
        pins.iter().all(|pin| pads.contains(&pin.number.as_str()))
    };
    if let Some(fp) = p.library().footprints.get(name)
        && fits(fp)
    {
        return Some((fp.name.clone(), None));
    }
    let mut candidates = vec![name.to_string()];
    if pins.len() == 2
        && let Some(size) = chip_size(name)
    {
        candidates.push(size);
    }
    for pkg in candidates {
        let Ok(spec) = landpattern::packages::parse(&pkg, chip_kind(category)) else { continue };
        let Ok(fp) = landpattern::generate(&spec, &GenOptions::default()) else { continue };
        if let Some(existing) = p.library().footprints.get(&fp.name) {
            if fits(existing) {
                return Some((existing.name.clone(), Some(pkg)));
            }
            continue;
        }
        if fits(&fp) {
            let n = fp.name.clone();
            add_footprint(p, fp, diags);
            return Some((n, Some(pkg)));
        }
    }
    None
}

/// Creates the part for a group of components; returns its ID and resolution.
fn create_part(
    p: &mut Project,
    key: &GroupKey,
    c: &NetlistComponent,
    lp: Option<&LibPart>,
    pins: Vec<Pin>,
    opts: &ImportOptions,
    diags: &mut Vec<Diagnostic>,
) -> (String, Resolution, bool) {
    // ID from the MPN, else the value, else the symbol name.
    let name = key
        .mpn
        .as_deref()
        .or(Some(c.value.as_str()).filter(|v| !v.trim().is_empty()))
        .or(key.symbol.as_ref().map(|s| s.1.as_str()))
        .unwrap_or("part");
    let base = slugify(name);
    let base = if valid_id(&base) { base } else { "part".to_string() };
    let fp_name = c.footprint_name();
    let footprint = fp_name.and_then(|f| find_footprint(p, f, key.category, &pins, diags));
    let mut params = Params::default();
    if let Some((_, Some(pkg))) = &footprint {
        params.insert("package", ParamValue::Text(pkg.clone()));
    }
    let main = match key.category {
        Category::Resistor => Some("resistance"),
        Category::Capacitor => Some("capacitance"),
        Category::Inductor => Some("inductance"),
        Category::FerriteBead => Some("impedance"),
        Category::Crystal | Category::Oscillator => Some("frequency"),
        _ => None,
    };
    if let Some(k) = main
        && let Ok(v @ ParamValue::Quantity(_)) = ParamValue::parse(k, &c.value)
    {
        params.insert(k, v);
    }
    let description = c
        .description
        .clone()
        .or_else(|| lp.and_then(|l| l.description.clone()))
        .unwrap_or_else(|| format!("{} {}", key.category.label(), c.value).trim().to_string());
    let mut detail = format!("KiCad netlist {}", opts.file_name);
    if let Some((lib, part)) = &key.symbol {
        detail += &format!(", symbol {lib}:{part}");
    }
    if let Some(f) = &c.footprint {
        detail += &format!(", footprint {f}");
    }
    let how = if footprint.is_some() { Resolution::Created } else { Resolution::Placeholder };
    let symbol = crate::symbolgen::generate(key.category, pins);
    let mut part = Part {
        id: base.clone(),
        category: key.category,
        description,
        manufacturer: key.manufacturer.clone(),
        mpn: key.mpn.clone(),
        params,
        symbol,
        footprints: footprint.map(|(f, _)| FootprintRef::new(f)).into_iter().collect(),
        datasheet: c.datasheet.clone().filter(|d| d.contains("://")),
        provenance: Provenance { origin: Origin::Import, detail: Some(detail), license: None },
    };
    // A part an earlier import of the same netlist created is reused; any other part keeps its
    // ID and the new one gets a suffix.
    for n in 1.. {
        let id = match n {
            1 => base.clone(),
            2 => format!("{base}_kicad"),
            n => format!("{base}_kicad{n}"),
        };
        part.id = id.clone();
        match p.library().find_part_id_ci(&id) {
            Some(k) if p.library().parts[k] == part => return (k.to_string(), how, false),
            Some(_) => continue,
            None => {
                p.library_mut().parts.insert(id.clone(), part);
                return (id, how, true);
            }
        }
    }
    unreachable!("an unused part ID exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(net_name("/LED", "1"), "LED");
        assert_eq!(net_name("/blk/A{slash}B", "1"), "blk/A/B");
        assert_eq!(net_name("GND", "1"), "GND");
        assert_eq!(net_name("", "7"), "Net-7");
        assert_eq!(chip_size("CAPC1005X55N").as_deref(), Some("0402"));
        assert_eq!(chip_size("R_0603_1608Metric").as_deref(), Some("0603"));
        assert_eq!(chip_size("LED_1608Metric").as_deref(), Some("0603"));
        assert_eq!(chip_size("SOT-23-5"), None);
        assert_eq!(pin_kind("input+no_connect"), Some(PinKind::Input));
        assert_eq!(category_for("U3", Some("LDO regulator AMS1117"), ""), Category::Ldo);
        assert_eq!(category_for("D2", None, "LED_Small"), Category::Led);
        assert_eq!(category_for("Q1", None, "Q_NMOS_GSD"), Category::Mosfet);
        assert_eq!(category_for("ZZ1", None, ""), Category::Other);
    }
}
