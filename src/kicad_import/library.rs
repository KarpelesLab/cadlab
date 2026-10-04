//! The user's own KiCad library files → cadlab footprints and parts (DECISIONS D41).
//!
//! - **Footprints** (`.kicad_mod`, one per file; a `.pretty` directory holds many) go through the
//!   same conversion as the footprints embedded in boards (`board.import_kicad`): pads of every
//!   shape (custom pads as polygon pads; trapezoids, chamfers and slots approximated and
//!   reported), drills, silkscreen, fab and courtyard drawings. What a cadlab footprint cannot
//!   hold (texts, drawings on other layers, board edges, 3D model paths, local margins) is
//!   reported with a diagnostic.
//! - **Symbols** (`.kicad_sym`, many per file) become parts: pins (number, name, electrical type,
//!   the side of the body they are on, unit, alternate functions), the category from the
//!   reference prefix, and the fields (Value, Footprint, Datasheet, Description, MPN and
//!   manufacturer, other fields as parameters). Derived symbols (`extends`) take the pins of
//!   their root symbol. The drawing is not converted: cadlab symbols are generated from their
//!   pins (`symbolgen`), keeping the side each pin had in KiCad. Power symbols are net labels in
//!   cadlab, not parts, and are skipped.
//!
//! Written from KiCad's published S-expression format documentation and from files `kicad-cli`
//! writes (`sym upgrade`, `fp upgrade`); it shares no code with KiCad, and KiCad's own libraries
//! are never read (DECISIONS D7): these are the user's files, given by path, and the user's
//! license is recorded in the provenance.

use std::collections::{BTreeMap, BTreeSet};

use super::footprint::{convert_library, footprint_name};
use super::{MIN_VERSION, Notes, deg, invalid};
use crate::diag::Diagnostic;
use crate::model::footprint::Footprint;
use crate::model::part::{
    Category, FootprintRef, Origin, ParamValue, Params, Part, Pin, PinAlternate, PinKind, Provenance, Side, slugify,
    valid_id,
};
use crate::model::sections::natural_cmp;
use crate::netlist::import::{ImportError, pin_kind};
use crate::refs::ObjectRef;
use crate::sexpr::{self, Sexpr};
use crate::symbolgen;

/// A footprint read from a library file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFootprint {
    /// Name in the KiCad library (the file name without `.kicad_mod`).
    pub kicad_name: String,
    /// The converted footprint, named after the KiCad name (made a valid ID).
    pub footprint: Footprint,
}

/// Footprints read from one or more `.kicad_mod` files.
#[derive(Clone, Debug, Default)]
pub struct FootprintSet {
    /// Footprints, in file name order.
    pub footprints: Vec<LibraryFootprint>,
    /// Diagnostics (repeated notes aggregated over all files).
    pub diagnostics: Vec<Diagnostic>,
    /// Drawings, pads and settings not imported.
    pub not_imported: usize,
}

/// One `.kicad_mod` file: the KiCad footprint name (file name without extension) and the text.
#[derive(Clone, Debug)]
pub struct FootprintFile {
    /// Footprint name in the KiCad library.
    pub name: String,
    /// File content.
    pub text: String,
}

/// Provenance of an imported item.
fn provenance(what: &str, name: &str, source: &str, license: Option<&str>) -> Provenance {
    Provenance {
        origin: Origin::Import,
        detail: Some(format!("KiCad {what} `{name}` from {source}")),
        license: license.map(str::to_string),
    }
}

/// Checks the head and format version of a library file.
fn check_format(root: &Sexpr, head: &str, what: &str, upgrade: &str) -> Result<(), ImportError> {
    if root.head() == Some("module") {
        return Err(invalid(
            "import.kicad_version",
            format!("the {what} is in the KiCad 5 format (`(module`)"),
            format!("upgrade it first: `{upgrade}`, or open and save it in KiCad 6 or later"),
        ));
    }
    if root.head() != Some(head) {
        return Err(invalid(
            "import.not_kicad_library",
            format!("the file starts with `({}`, not `({head}`", root.head().unwrap_or("")),
            format!("give a KiCad {what} file"),
        ));
    }
    if let Some(v) = root.child_value("version").and_then(|v| v.parse::<u32>().ok())
        && v < MIN_VERSION
    {
        return Err(invalid(
            "import.kicad_version",
            format!("{what} format version {v} is older than KiCad 6 ({MIN_VERSION})"),
            format!("upgrade it first: `{upgrade}`"),
        ));
    }
    Ok(())
}

/// Reads footprint files. `source` names them in provenance and messages (`MyLib.pretty`).
/// With one file, an unreadable file is an error; with several, it is skipped with a warning.
pub fn read_footprints(
    files: &[FootprintFile],
    source: &str,
    license: Option<&str>,
) -> Result<FootprintSet, ImportError> {
    let mut notes = Notes::default();
    let mut out = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for f in files {
        let parsed = sexpr::parse(&f.text)
            .map_err(|e| {
                invalid(
                    "import.parse",
                    format!("{}.kicad_mod is not a readable KiCad footprint: {e}", f.name),
                    "give a `.kicad_mod` file saved by KiCad 6 or later",
                )
            })
            .and_then(|root| {
                check_format(&root, "footprint", "footprint", "kicad-cli fp upgrade <dir.pretty>")?;
                Ok(root)
            });
        let root = match parsed {
            Ok(r) => r,
            Err(e) if files.len() == 1 => return Err(e),
            Err(e) => {
                notes.not_imported(
                    Diagnostic::warning("import.footprint_unreadable", format!("{}; not imported", e.message))
                        .with_subject(ObjectRef::Named { kind: "footprint".into(), name: f.name.clone() })
                        .with_hint(e.hint),
                );
                continue;
            }
        };
        let mut name = footprint_name(&f.name, "footprint");
        if !names.insert(name.to_ascii_lowercase()) {
            let base = name.clone();
            name = (2..).map(|i| format!("{base}_{i}")).find(|n| !names.contains(&n.to_ascii_lowercase())).unwrap();
            names.insert(name.to_ascii_lowercase());
            notes.push(
                Diagnostic::warning(
                    "import.name_changed",
                    format!("footprints `{base}` and `{}` have the same cadlab name; the second is `{name}`", f.name),
                )
                .with_subject(ObjectRef::Named { kind: "footprint".into(), name: name.clone() })
                .with_hint("rename the footprint files so their names differ in more than case and punctuation"),
            );
        }
        let mut fp = convert_library(&root, &name, &mut notes);
        fp.provenance = Some(provenance("footprint", &f.name, source, license));
        out.push(LibraryFootprint { kicad_name: f.name.clone(), footprint: fp });
    }
    let not_imported = notes.skipped;
    Ok(FootprintSet { footprints: out, diagnostics: notes.finish(), not_imported })
}

/// Whether two footprints are the same land pattern: names, descriptions and provenance aside,
/// pads and drawings in any order, within the few nanometers rotated board exports leave (the
/// test board import uses to reuse project footprints).
pub fn same_land_pattern(a: &Footprint, b: &Footprint) -> bool {
    super::footprint::equivalent(a, b)
}

/// Options for reading a symbol library.
#[derive(Clone, Debug, Default)]
pub struct SymbolOptions {
    /// Only these symbols (KiCad names); all when empty.
    pub only: Vec<String>,
    /// Category of every part, instead of guessing it from the reference prefix.
    pub category: Option<Category>,
    /// License of the library, recorded in provenance.
    pub license: Option<String>,
    /// File name, for provenance and messages.
    pub source: String,
}

/// A symbol converted to a part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibrarySymbol {
    /// Symbol name in the KiCad library.
    pub kicad_name: String,
    /// The part.
    pub part: Part,
}

/// A symbol that was not imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedSymbol {
    /// Symbol name in the KiCad library.
    pub kicad_name: String,
    /// Why (the code of the diagnostic that explains it).
    pub reason: &'static str,
}

/// Symbols read from a `.kicad_sym`.
#[derive(Clone, Debug, Default)]
pub struct SymbolSet {
    /// Format version of the file.
    pub format_version: Option<u32>,
    /// Parts, in file order.
    pub parts: Vec<LibrarySymbol>,
    /// Symbols not imported.
    pub skipped: Vec<SkippedSymbol>,
    /// Diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Fields that are not carried over as parameters.
const STANDARD_FIELDS: &[&str] = &[
    "Reference",
    "Value",
    "Footprint",
    "Datasheet",
    "Description",
    "ki_description",
    "ki_keywords",
    "ki_fp_filters",
    "ki_locked",
];

/// Fields a derived symbol takes from its parent when it has none of its own (KiCad keeps the
/// other fields of a derived symbol separate from its parent's).
const INHERITED_FIELDS: &[&str] =
    &["Reference", "Value", "Footprint", "Datasheet", "Description", "ki_description", "ki_keywords", "ki_fp_filters"];

/// A field name reduced to lowercase letters and digits, for matching (`Mfr. Part #` → `mfrpart`).
fn norm(k: &str) -> String {
    k.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

fn is_mpn_field(k: &str) -> bool {
    matches!(
        norm(k).as_str(),
        "mpn"
            | "manufacturerpartnumber"
            | "manufacturerpn"
            | "mfrpartnumber"
            | "mfrpart"
            | "mfrpn"
            | "mfgpartnumber"
            | "mfgpn"
            | "manfpartnumber"
            | "partnumber"
    )
}

fn is_manufacturer_field(k: &str) -> bool {
    matches!(norm(k).as_str(), "manufacturer" | "manufacturername" | "mfr" | "mfg" | "manf" | "mfrname")
}

/// Distributor and assembler fields: supplier choices are made per fab at export (D12).
fn is_supplier_field(k: &str) -> bool {
    let n = norm(k);
    [
        "lcsc",
        "jlc",
        "digikey",
        "mouser",
        "farnell",
        "newark",
        "element14",
        "arrow",
        "tme",
        "rsonline",
        "rspart",
        "octopart",
        "supplier",
        "distributor",
    ]
    .iter()
    .any(|p| n.starts_with(p))
}

/// Category from the reference prefix, refined by the symbol's name, value, description and
/// keywords.
pub fn guess_category(reference: &str, text: &str) -> Category {
    let prefix: String =
        reference.trim_start_matches('#').chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>();
    let t = text.to_ascii_lowercase();
    let has = |w: &str| t.split(|c: char| !c.is_ascii_alphanumeric()).any(|x| x == w);
    match prefix.to_ascii_uppercase().as_str() {
        "R" => Category::Resistor,
        "C" => Category::Capacitor,
        "L" => Category::Inductor,
        "FB" => Category::FerriteBead,
        "LED" => Category::Led,
        "D" if has("led") => Category::Led,
        "D" => Category::Diode,
        "Q" | "T" if has("mosfet") || has("fet") || has("nmos") || has("pmos") => Category::Mosfet,
        "Q" | "T" => Category::TransistorBjt,
        "U" | "IC" if has("ldo") => Category::Ldo,
        "U" | "IC" if has("regulator") || has("converter") || has("buck") || has("boost") => Category::Regulator,
        "U" | "IC" if has("mcu") || has("microcontroller") => Category::Mcu,
        "U" | "IC" => Category::Ic,
        "J" | "P" | "CN" | "CON" => Category::Connector,
        "X" | "OSC" => Category::Oscillator,
        "Y" => Category::Crystal,
        "SW" | "S" | "BTN" => Category::Switch,
        "F" => Category::Fuse,
        "TP" => Category::TestPoint,
        "H" | "MH" | "FID" => Category::Mechanical,
        _ => Category::Other,
    }
}

/// Fields of a symbol, in file order: `(property "Name" "value" ...)`.
fn fields(e: &Sexpr) -> Vec<(String, String)> {
    e.all("property")
        .filter_map(|p| {
            let it = p.items();
            Some((it.get(1)?.atom()?.to_string(), it.get(2).and_then(Sexpr::atom).unwrap_or("").to_string()))
        })
        .collect()
}

/// A flag written as a bare atom (`hide`) or `(hide yes)`.
fn flag(e: &Sexpr, name: &str) -> bool {
    e.items().iter().skip(1).any(|c| c.atom() == Some(name))
        || e.get(name).is_some_and(|c| c.value().is_none_or(|v| v == "yes"))
}

/// Unit and body style of a sub-symbol `Name_<unit>_<style>`.
fn unit_style(name: &str) -> Option<(u32, u32)> {
    let mut it = name.rsplitn(3, '_');
    let style = it.next()?.parse().ok()?;
    let unit = it.next()?.parse().ok()?;
    it.next()?;
    Some((unit, style))
}

/// A pin as read, before units are known.
struct RawPin {
    pin: Pin,
    unit: u32,
    hidden: bool,
}

/// Side of the symbol body a pin is on, from its KiCad orientation (the direction from the
/// connection point toward the body).
fn side_of(angle: &str) -> Side {
    let a = deg(angle).map(|a| a.normalized().0).unwrap_or(0);
    match ((a + 45_000) / 90_000) % 4 {
        0 => Side::Left,
        1 => Side::Bottom,
        2 => Side::Right,
        _ => Side::Top,
    }
}

/// Reads the pins of a (root) symbol: every sub-symbol of body style 0 or 1 (style 2 is KiCad's
/// alternate De Morgan drawing of the same pins).
fn read_pins(sym: &Sexpr, subject: &ObjectRef, notes: &mut Notes) -> Vec<RawPin> {
    let mut out = Vec::new();
    let mut read = |holder: &Sexpr, unit: u32, notes: &mut Notes| {
        for p in holder.all("pin") {
            let it = p.items();
            let ty = it.get(1).and_then(Sexpr::atom).unwrap_or("");
            let kind = pin_kind(ty).unwrap_or_else(|| {
                notes.agg(
                    "import.symbol_pin_type",
                    ty,
                    Diagnostic::warning(
                        "import.symbol_pin_type",
                        format!("unknown pin type `{ty}` read as unspecified"),
                    )
                    .with_subject(subject.clone())
                    .with_hint("set the pin type in KiCad's symbol editor and import again"),
                );
                PinKind::Unspecified
            });
            let number = p.child_value("number").unwrap_or("").trim().to_string();
            let name = p.child_value("name").unwrap_or("").trim();
            let name = if name == "~" { "" } else { name }.to_string();
            if number.is_empty() {
                notes.not_imported(
                    Diagnostic::warning(
                        "import.symbol_pin_number",
                        format!("a pin of {subject} has no number (`{name}`); not imported"),
                    )
                    .with_subject(subject.clone())
                    .with_hint("number every pin in KiCad's symbol editor (pins map to footprint pads by number)"),
                );
                continue;
            }
            let side = p.get("at").and_then(|a| a.items().get(3)).and_then(Sexpr::atom).map_or(Side::Left, side_of);
            let mut alternates: Vec<PinAlternate> = p
                .all("alternate")
                .filter_map(|a| {
                    let it = a.items();
                    let n = it.get(1)?.atom()?.trim().to_string();
                    let k = it.get(2).and_then(Sexpr::atom).and_then(pin_kind).unwrap_or(PinKind::Unspecified);
                    (!n.is_empty()).then_some(PinAlternate { name: n, kind: k })
                })
                .collect();
            // By name, as KiCad saves them.
            alternates.sort_by(|a, b| natural_cmp(&a.name, &b.name));
            let mut pin = Pin::new(number, name, kind);
            pin.side = Some(side);
            pin.alternates = alternates;
            out.push(RawPin { pin, unit, hidden: flag(p, "hide") });
        }
    };
    read(sym, 0, notes);
    for sub in sym.all("symbol") {
        let name = sub.value().unwrap_or("");
        let (unit, style) = unit_style(name).unwrap_or((0, 0));
        if style > 1 {
            continue;
        }
        read(sub, unit, notes);
    }
    out
}

/// Merges pins read from all units: one pin per number. A number in several units (stacked or
/// repeated supply pins) is shared by them; a number reused with another name or type keeps the
/// first and is reported.
fn merge_pins(raw: Vec<RawPin>, subject: &ObjectRef, notes: &mut Notes) -> (Vec<Pin>, Vec<String>) {
    let units: BTreeSet<u32> = raw.iter().map(|r| r.unit).filter(|&u| u > 0).collect();
    let multi = units.len() > 1;
    let mut pins: Vec<Pin> = Vec::new();
    let mut visible: BTreeSet<String> = BTreeSet::new();
    for r in raw {
        if !r.hidden {
            visible.insert(r.pin.number.clone());
        }
        let unit = (multi && r.unit > 0).then_some(r.unit);
        match pins.iter_mut().find(|p| p.number == r.pin.number) {
            Some(p) => {
                if p.name != r.pin.name || p.kind != r.pin.kind {
                    notes.push(
                        Diagnostic::warning(
                            "import.symbol_pin_conflict",
                            format!(
                                "{subject} has two pins numbered {} (`{}`, `{}`); the first is kept",
                                p.number,
                                p.label(),
                                r.pin.label()
                            ),
                        )
                        .with_subject(subject.clone())
                        .with_hint("give each pin its own number in KiCad, or edit the part with `part.set`"),
                    );
                } else if p.unit != unit {
                    p.unit = None;
                }
            }
            None => {
                let mut pin = r.pin;
                pin.unit = unit;
                pins.push(pin);
            }
        }
    }
    pins.sort_by(|a, b| natural_cmp(&a.number, &b.number));
    // Hidden power inputs (every occurrence hidden: a hidden pin stacked on a visible one is
    // only a duplicate).
    let mut hidden_power: Vec<String> = Vec::new();
    for p in &pins {
        if p.kind == PinKind::PowerIn && !visible.contains(&p.number) && !hidden_power.iter().any(|h| h == p.label()) {
            hidden_power.push(p.label().to_string());
        }
    }
    (pins, hidden_power)
}

/// A footprint a part can use: its name in the target library and the footprint.
pub type FootprintLookup<'a> = dyn Fn(&str) -> Option<(String, Footprint)> + 'a;

/// Reads the symbols of a `.kicad_sym` into parts. `footprints` finds the footprint a symbol's
/// Footprint field names (by its cadlab name: `MyLib:SOT-23` → `SOT-23`) in the target library.
pub fn read_symbols(
    text: &str,
    opts: &SymbolOptions,
    footprints: &FootprintLookup<'_>,
) -> Result<SymbolSet, ImportError> {
    if text.trim_start().starts_with("EESchema-LIBRARY") {
        return Err(invalid(
            "import.kicad_version",
            "the symbol library is in the KiCad 5 `.lib` format",
            "convert it first: `kicad-cli sym upgrade <file.lib> -o <file.kicad_sym>`",
        ));
    }
    let root = sexpr::parse(text).map_err(|e| {
        invalid(
            "import.parse",
            format!("not a readable KiCad symbol library: {e}"),
            "give a `.kicad_sym` file saved by KiCad 6 or later",
        )
    })?;
    check_format(&root, "kicad_symbol_lib", "symbol library", "kicad-cli sym upgrade <file.kicad_sym>")?;
    let mut set =
        SymbolSet { format_version: root.child_value("version").and_then(|v| v.parse().ok()), ..Default::default() };
    let symbols: Vec<&Sexpr> = root.all("symbol").collect();
    let by_name: BTreeMap<&str, &Sexpr> = symbols.iter().filter_map(|s| Some((s.value()?, *s))).collect();

    // Selection.
    let mut wanted: Vec<&str> = Vec::new();
    for n in &opts.only {
        let found = by_name
            .keys()
            .find(|k| **k == n.as_str())
            .or_else(|| by_name.keys().find(|k| k.eq_ignore_ascii_case(n)))
            .copied();
        match found {
            Some(k) => wanted.push(k),
            None => {
                let sug = crate::suggest::did_you_mean(n, by_name.keys().copied(), 3);
                return Err(invalid(
                    "import.symbol_not_found",
                    format!("the library has no symbol `{n}`"),
                    if sug.is_empty() {
                        format!("symbols: {}", by_name.keys().copied().collect::<Vec<_>>().join(", "))
                    } else {
                        format!("did you mean {}?", sug.join(", "))
                    },
                ));
            }
        }
    }

    let mut notes = Notes::default();
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for sym in &symbols {
        let Some(name) = sym.value() else { continue };
        if !wanted.is_empty() && !wanted.contains(&name) {
            continue;
        }
        let subject = ObjectRef::Named { kind: "symbol".into(), name: name.to_string() };
        // Inheritance: the root symbol gives pins and units; fields come from the symbol, with
        // the standard ones inherited when missing.
        let mut chain: Vec<&Sexpr> = vec![sym];
        let mut cur: &Sexpr = sym;
        let mut broken: Option<String> = None;
        while let Some(parent) = cur.child_value("extends") {
            match by_name.get(parent) {
                Some(p) if !chain.iter().any(|c| std::ptr::eq(*c, *p)) => {
                    chain.push(p);
                    cur = p;
                }
                Some(_) => {
                    broken = Some(format!("{subject} extends itself through `{parent}`"));
                    break;
                }
                None => {
                    broken = Some(format!("{subject} extends `{parent}`, which is not in the library"));
                    break;
                }
            }
        }
        if let Some(msg) = broken {
            notes.push(
                Diagnostic::warning("import.symbol_extends", format!("{msg}; not imported"))
                    .with_subject(subject.clone())
                    .with_hint("import the library that holds the parent symbol, or flatten the symbol in KiCad"),
            );
            set.skipped.push(SkippedSymbol { kicad_name: name.to_string(), reason: "import.symbol_extends" });
            continue;
        }
        let root_sym = *chain.last().expect("chain starts with the symbol");
        let mut fmap: BTreeMap<String, String> = BTreeMap::new();
        let own = fields(sym);
        for (k, v) in &own {
            fmap.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for parent in chain.iter().skip(1) {
            for (k, v) in fields(parent) {
                if INHERITED_FIELDS.contains(&k.as_str()) {
                    fmap.entry(k).or_insert(v);
                }
            }
        }
        let field = |k: &str| fmap.get(k).map(|v| v.trim()).filter(|v| !v.is_empty() && *v != "~");

        if chain.iter().any(|s| s.get("power").is_some()) {
            notes.agg(
                "import.symbol_power",
                "power",
                Diagnostic::info(
                    "import.symbol_power",
                    "power symbols are not imported: in cadlab they are net names, not parts (supply and ground symbols are drawn from the nets on export)",
                )
                .with_subject(subject.clone())
                .with_hint("name the circuit's nets after them (`net.rename`) and mark supplies driven (`net.set --driven`)"),
            );
            set.skipped.push(SkippedSymbol { kicad_name: name.to_string(), reason: "import.symbol_power" });
            continue;
        }

        let raw = read_pins(root_sym, &subject, &mut notes);
        let (pins, hidden_power) = merge_pins(raw, &subject, &mut notes);
        if pins.is_empty() {
            notes.push(
                Diagnostic::warning(
                    "import.symbol_no_pins",
                    format!("{subject} has no pins (a drawing or logo); not imported"),
                )
                .with_subject(subject.clone())
                .with_hint("cadlab parts need pins; draw logos as board graphics"),
            );
            set.skipped.push(SkippedSymbol { kicad_name: name.to_string(), reason: "import.symbol_no_pins" });
            continue;
        }
        if !hidden_power.is_empty() {
            notes.push(
                Diagnostic::warning(
                    "import.symbol_hidden_power_pin",
                    format!(
                        "{subject} has hidden power input pins ({}): KiCad connects them to the net of that name implicitly, cadlab does not",
                        hidden_power.join(", ")
                    ),
                )
                .with_subject(subject.clone())
                .with_hint("connect these pins in the circuit explicitly (`net.connect`)"),
            );
        }

        // Identity and category.
        let value = field("Value").unwrap_or(name).to_string();
        let description = field("Description").or(field("ki_description")).map(str::to_string);
        let keywords = field("ki_keywords").unwrap_or("");
        let reference = field("Reference").unwrap_or("");
        let category = opts.category.unwrap_or_else(|| {
            guess_category(reference, &format!("{name} {value} {} {keywords}", description.as_deref().unwrap_or("")))
        });
        let mut id = if valid_id(name) { name.to_string() } else { slugify(name) };
        if !ids.insert(id.to_ascii_lowercase()) {
            let base = id.clone();
            id = (2..).map(|i| format!("{base}_{i}")).find(|n| !ids.contains(&n.to_ascii_lowercase())).unwrap();
            ids.insert(id.to_ascii_lowercase());
            notes.push(
                Diagnostic::warning(
                    "import.name_changed",
                    format!(
                        "symbol `{name}` has the same part ID as an earlier symbol (`{base}`); it is imported as `{id}`"
                    ),
                )
                .with_subject(subject.clone())
                .with_hint("rename the symbols so their names differ in more than case and punctuation"),
            );
        }

        // Fields.
        let mut params = Params::default();
        let value_param = match category {
            Category::Resistor => Some("resistance"),
            Category::Capacitor => Some("capacitance"),
            Category::Inductor => Some("inductance"),
            Category::FerriteBead => Some("impedance"),
            Category::Crystal | Category::Oscillator => Some("frequency"),
            _ => None,
        };
        // The value is a parameter when it reads as one (`10k`, `100nF`; an LED's color), not
        // when it repeats the symbol name (`R`, `LED`).
        if let Some(k) = value_param
            && let Ok(v @ ParamValue::Quantity(_)) = ParamValue::parse(k, &value)
        {
            params.insert(k, v);
        }
        const COLORS: &[&str] = &[
            "red", "green", "blue", "yellow", "white", "orange", "amber", "pink", "purple", "violet", "uv", "ir", "rgb",
        ];
        if category == Category::Led && COLORS.iter().any(|c| value.eq_ignore_ascii_case(c)) {
            params.insert("color", ParamValue::Text(value.to_ascii_lowercase()));
        }
        let (mut mpn, mut manufacturer) = (None, None);
        let mut supplier_fields = Vec::new();
        let mut sim_fields = Vec::new();
        for (k, v) in &own {
            let v = v.trim();
            if STANDARD_FIELDS.contains(&k.as_str()) || v.is_empty() || v == "~" {
                continue;
            }
            if is_mpn_field(k) {
                mpn.get_or_insert_with(|| v.to_string());
            } else if is_manufacturer_field(k) {
                manufacturer.get_or_insert_with(|| v.to_string());
            } else if is_supplier_field(k) {
                supplier_fields.push(k.clone());
            } else if k.starts_with("Sim.") || k.starts_with("Spice_") {
                sim_fields.push(k.clone());
            } else if k.starts_with("ki_") {
                continue;
            } else {
                let key = slugify(&k.to_ascii_lowercase()).replace(['.', '-', '+'], "_");
                match ParamValue::parse(&key, v) {
                    Ok(pv) => params.insert(key, pv),
                    Err(e) => notes.push(
                        Diagnostic::warning(
                            "import.symbol_field_value",
                            format!("field `{k}` of {subject}: `{v}` is not a valid `{key}` ({e}); not imported"),
                        )
                        .with_subject(subject.clone())
                        .with_hint(format!("set the parameter with `part.set {id}` (`params`: `{key}`)")),
                    ),
                }
            }
        }
        if !supplier_fields.is_empty() {
            notes.agg(
                "import.symbol_supplier_field",
                "supplier",
                Diagnostic::info(
                    "import.symbol_supplier_field",
                    format!(
                        "distributor and assembler fields ({}) are not imported: projects stay provider-agnostic and supplier parts are chosen per fab at export",
                        supplier_fields.join(", ")
                    ),
                )
                .with_subject(subject.clone())
                .with_hint("keep the MPN on the part; `fab.check` / `fab.export` pick the supplier's SKU (`fab-lock.json`)"),
            );
        }
        if !sim_fields.is_empty() {
            notes.agg(
                "import.symbol_sim_field",
                "sim",
                Diagnostic::info("import.symbol_sim_field", "KiCad simulation fields (`Sim.*`) are not imported")
                    .with_subject(subject.clone())
                    .with_hint("set `spice_model` / `spice_lib` / `spice_pins` parameters for `export.spice`"),
            );
        }

        // Footprint.
        let mut footprint_refs = Vec::new();
        if let Some(fpf) = field("Footprint") {
            let wanted = footprint_name(fpf, "footprint");
            match footprints(&wanted) {
                Some((fp_name, fp)) => {
                    let pads = fp.pad_numbers();
                    let missing: Vec<&str> =
                        pins.iter().map(|p| p.number.as_str()).filter(|n| !pads.contains(n)).collect();
                    if missing.is_empty() {
                        footprint_refs.push(FootprintRef::new(fp_name));
                    } else {
                        notes.push(
                            Diagnostic::warning(
                                "import.symbol_pin_without_pad",
                                format!(
                                    "{subject}: pins {} have no pad in footprint `{fp_name}`; the footprint is not attached",
                                    missing.join(", ")
                                ),
                            )
                            .with_subject(subject.clone())
                            .with_hint(format!(
                                "fix the pin or pad numbers in KiCad and import again, or recreate the part with `part.create --footprint {fp_name}` and a `pin_map`"
                            )),
                        );
                    }
                }
                None => notes.push(
                    Diagnostic::warning(
                        "import.symbol_footprint_missing",
                        format!("{subject} uses footprint `{fpf}`, which is not in the target library; the part has no footprint"),
                    )
                    .with_subject(subject.clone())
                    .with_hint(format!(
                        "import the footprint first (`footprint.import_kicad <dir.pretty>`, or pass `footprints`), or generate one (`footprint.generate`) and attach it (`part.set {id} --footprint <name>`)"
                    )),
                ),
            }
        }

        let symbol = symbolgen::generate(category, pins);
        let part = Part {
            id,
            category,
            description: description.unwrap_or_else(|| format!("{} {name}", category.label())),
            manufacturer,
            mpn,
            params,
            symbol,
            footprints: footprint_refs,
            datasheet: field("Datasheet").map(str::to_string),
            provenance: provenance("symbol", name, &opts.source, opts.license.as_deref()),
        };
        set.parts.push(LibrarySymbol { kicad_name: name.to_string(), part });
    }
    set.diagnostics = notes.finish();
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(guess_category("R", "R_Small"), Category::Resistor);
        assert_eq!(guess_category("D", "LED_Small LED"), Category::Led);
        assert_eq!(guess_category("Q", "IRLML6402 P-MOSFET"), Category::Mosfet);
        assert_eq!(guess_category("U", "AP2112K LDO regulator"), Category::Ldo);
        assert_eq!(guess_category("U", "NE555"), Category::Ic);
        assert_eq!(guess_category("#PWR", "GND"), Category::Other);
        assert_eq!(guess_category("J", "USB_C"), Category::Connector);
        assert_eq!(guess_category("Y", "Crystal"), Category::Crystal);
    }

    #[test]
    fn sides_and_units() {
        assert_eq!(side_of("0"), Side::Left);
        assert_eq!(side_of("90"), Side::Bottom);
        assert_eq!(side_of("180"), Side::Right);
        assert_eq!(side_of("270"), Side::Top);
        assert_eq!(unit_style("Op_Amp_2_1"), Some((2, 1)));
        assert_eq!(unit_style("cadlab:R_10k_0_1"), Some((0, 1)));
        assert_eq!(unit_style("X"), None);
    }

    #[test]
    fn field_names() {
        assert!(is_mpn_field("MPN"));
        assert!(is_mpn_field("Manufacturer Part Number"));
        assert!(is_mpn_field("Mfr. Part #"));
        assert!(is_manufacturer_field("Manufacturer"));
        assert!(is_supplier_field("LCSC Part"));
        assert!(is_supplier_field("DigiKey_PN"));
        assert!(!is_supplier_field("Tolerance"));
    }
}
