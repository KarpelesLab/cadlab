//! `part.*`: parts in the project library.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::landpattern::{self, ChipKind, Density, GenOptions, PackageSpec};
use crate::model::Project;
use crate::model::footprint::Footprint;
use crate::model::part::{
    Category, FootprintRef, Origin, ParamValue, Params, Part, Pin, PinKind, Provenance, Side, slugify, valid_id,
};
use crate::partspec;
use crate::refs::ObjectRef;
use crate::symbolgen;

pub(crate) fn register(r: &mut Registry) {
    r.register::<Generic>()
        .register::<Create>()
        .register::<List>()
        .register::<Show>()
        .register::<Set>()
        .register::<Remove>()
        .register::<Search>();
}

/// Short description of a part.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PartSummary {
    /// Library ID.
    pub id: String,
    /// Category.
    pub category: Category,
    /// Description.
    pub description: String,
    /// Value (resistance, capacitance, ... or MPN).
    pub value: String,
    /// Manufacturer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// MPN (concrete parts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Preferred footprint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    /// Components using it.
    pub used_by: Vec<String>,
}

impl PartSummary {
    pub(crate) fn of(p: &Project, part: &Part) -> Self {
        let mut used_by: Vec<String> = p.circuit().using_part(&part.id).map(|(r, _)| r.clone()).collect();
        used_by.sort_by(|a, b| crate::model::sections::natural_cmp(a, b));
        PartSummary {
            id: part.id.clone(),
            category: part.category,
            description: part.description.clone(),
            value: part.value(),
            manufacturer: part.manufacturer.clone(),
            mpn: part.mpn.clone(),
            footprint: part.footprint().map(|f| f.footprint.clone()),
            used_by,
        }
    }

    fn line(&self) -> String {
        let mut s = format!("{}  {}", self.id, self.description);
        if let Some(m) = &self.mpn {
            s += &format!(" [{}{m}]", self.manufacturer.as_ref().map(|x| format!("{x} ")).unwrap_or_default());
        }
        if let Some(f) = &self.footprint {
            s += &format!("  ({f})");
        }
        if !self.used_by.is_empty() {
            s += &format!("  used by {}", self.used_by.join(", "));
        }
        s
    }
}

/// Adds `fp` to the library unless an identical one exists. An existing footprint with the same
/// name but different content is kept (projects stay stable when the generator changes); a note
/// is returned.
pub(crate) fn ensure_footprint(p: &mut Project, fp: Footprint) -> Option<Diagnostic> {
    let lib = p.library_mut();
    match lib.footprints.get(&fp.name) {
        None => {
            lib.footprints.insert(fp.name.clone(), fp);
            None
        }
        Some(existing) if *existing == fp => None,
        Some(_) => Some(
            Diagnostic::info(
                "footprint.kept_existing",
                format!("footprint `{}` already exists in the project and differs from a fresh generation; keeping the project's copy", fp.name),
            )
            .with_hint("run `footprint.generate` with `replace: true` to update it"),
        ),
    }
}

fn check_new_id(p: &Project, id: &str) -> Result<(), CommandError> {
    if !valid_id(id) {
        return Err(CommandError::invalid_args(
            "part.invalid_id",
            format!("invalid part id `{id}`: use letters, digits, `.`, `_`, `+`, `-`"),
        ));
    }
    if let Some(existing) = p.library().find_part_id_ci(id) {
        return Err(CommandError::conflict("part.exists", format!("part `{existing}` already exists"))
            .with_hint("pick another id, change it with `part.set`, or remove it with `part.remove`"));
    }
    Ok(())
}

/// Add a generic passive part from a short spec.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Generic {
    /// Spec: type, value and package, plus optional tolerance, rating, dielectric, color.
    /// Examples: "R 10k 1% 0402", "C 100nF 16V X7R 0402", "L 4.7uH 1A 0805", "LED red 0603".
    pub spec: String,
}

/// Result of `part.generic`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GenericResult {
    /// The part.
    #[serde(flatten)]
    pub part: PartSummary,
    /// False if the part already existed.
    pub created: bool,
}

/// Adds the generic part for `spec` if missing; returns its ID and whether it was created.
pub(crate) fn add_generic(ctx: &mut Context<'_>, spec: &str) -> Result<(String, bool), CommandError> {
    let s = partspec::parse(spec).map_err(|e| {
        CommandError::invalid_args("part.invalid_spec", e)
            .with_hint("examples: \"R 10k 1% 0402\", \"C 100nF 16V X7R 0402\"")
    })?;
    let id = s.id();
    if let Some(existing) = ctx.project()?.library().find_part_id_ci(&id) {
        return Ok((existing.to_string(), false));
    }
    let (part, fp) = s.build(&GenOptions::default()).map_err(|e| CommandError::invalid_args("part.invalid_spec", e))?;
    let p = ctx.project_mut()?;
    let note = ensure_footprint(p, fp);
    p.library_mut().parts.insert(id.clone(), part);
    if let Some(n) = note {
        ctx.report(n);
    }
    Ok((id, true))
}

impl Command for Generic {
    const NAME: &'static str = "part.generic";
    const SUMMARY: &'static str = "Add a generic passive part (\"R 10k 1% 0402\") with generated symbol and footprint";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["spec"];
    type Output = GenericResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<GenericResult, CommandError> {
        let (id, created) = add_generic(ctx, &self.spec)?;
        let p = ctx.project()?;
        Ok(GenericResult { part: PartSummary::of(p, &p.library().parts[&id]), created })
    }

    fn summarize(o: &GenericResult) -> String {
        format!("{} {}", if o.created { "added" } else { "exists:" }, o.part.line())
    }
}

/// A pin, as given to `part.create`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinInput {
    /// Pin number as in the datasheet (`1`, `A3`, `EP`).
    pub number: String,
    /// Pin name (`VIN`, `PA9`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Electrical type (default: passive).
    #[serde(default)]
    pub kind: PinKind,
    /// Functional group for symbol layout (`power`, `PORTA`, `USB`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Force a symbol side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

/// Create a part: concrete (with MPN) or custom. Generates the symbol from the pin list, and the
/// footprint from a package name or explicit dimensions.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// Library ID (default: derived from the MPN).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Category.
    pub category: Category,
    /// One-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Manufacturer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Manufacturer part number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Parameters: {"voltage_out": "3.3V", "current_out": "600mA", ...}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// Pins (number, name, kind). Optional for two-terminal parts and connectors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<PinInput>,
    /// Package name to generate the footprint from ("SOT-23-5", "SOIC-8", "QFN-32 5x5mm P0.5mm EP3.1mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Explicit package dimensions from the datasheet, instead of `package`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_spec: Option<PackageSpec>,
    /// Use an existing library footprint instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    /// Pin number → pad numbers, where they differ (e.g. {"9": ["9", "EP"]}).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pin_map: BTreeMap<String, Vec<String>>,
    /// Datasheet URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasheet: Option<String>,
    /// IPC density level for the generated footprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    /// Look the MPN up at the configured suppliers and fill in what is missing: manufacturer,
    /// description, parameters, package, datasheet. Pins still come from `pins`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill_from_suppliers: bool,
}

/// Fills `create` from the best supplier listing of its MPN.
fn fill_from_suppliers(ctx: &mut Context<'_>, create: &mut Create) -> Result<(), CommandError> {
    require_suppliers(ctx)?;
    let mpn = create.mpn.clone().ok_or_else(|| {
        CommandError::invalid_args("part.missing_mpn", "`fill_from_suppliers` needs an `mpn` to look up")
    })?;
    let mut r = ctx.session.suppliers.lookup(&mpn, &[]);
    report_provider_errors(ctx, &r.errors);
    crate::supplier::SearchQuery::default().rank(&mut r.candidates);
    let Some(c) = r.candidates.into_iter().next() else {
        return Err(CommandError::not_found("supplier.mpn_not_found", format!("no supplier lists `{mpn}`"))
            .with_hint("check the MPN, or create the part without `fill_from_suppliers`"));
    };
    create.manufacturer = create.manufacturer.take().or(c.manufacturer);
    if create.description.is_none() && !c.description.is_empty() {
        create.description = Some(c.description);
    }
    create.datasheet = create.datasheet.take().or(c.datasheet);
    for (k, v) in c.params.0 {
        create.params.entry(k).or_insert_with(|| v.to_string());
    }
    if create.package.is_none() && create.package_spec.is_none() && create.footprint.is_none() {
        // Use the supplier's package name if the generator knows it.
        if let Some(p) = c.package.filter(|p| landpattern::packages::parse(p, chip_kind(create.category)).is_ok()) {
            create.package = Some(p);
        }
    }
    Ok(())
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

impl Command for Create {
    const NAME: &'static str = "part.create";
    const SUMMARY: &'static str = "Create a part from a pin list and a package; symbol and footprint are generated";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = PartSummary;

    fn run(mut self, ctx: &mut Context<'_>) -> Result<PartSummary, CommandError> {
        if self.fill_from_suppliers {
            fill_from_suppliers(ctx, &mut self)?;
        }
        let id = match (&self.id, &self.mpn) {
            (Some(id), _) => id.clone(),
            (None, Some(mpn)) => slugify(mpn),
            (None, None) => {
                return Err(CommandError::invalid_args("part.missing_id", "give an `id` or an `mpn`"));
            }
        };
        check_new_id(ctx.project()?, &id)?;

        let mut params = Params::default();
        for (k, v) in &self.params {
            params
                .set(k, v)
                .map_err(|e| CommandError::invalid_args("part.invalid_param", format!("parameter `{k}`: {e}")))?;
        }

        // Footprint: existing, generated from a name, or from explicit dimensions.
        let sources = [self.footprint.is_some(), self.package.is_some(), self.package_spec.is_some()];
        if sources.iter().filter(|s| **s).count() > 1 {
            return Err(CommandError::invalid_args(
                "part.footprint_ambiguous",
                "give only one of `footprint`, `package`, `package_spec`",
            ));
        }
        let opts = GenOptions { density: self.density.unwrap_or_default(), ..Default::default() };
        let generated = match (&self.package, &self.package_spec) {
            (Some(name), _) => {
                let spec = landpattern::packages::parse(name, chip_kind(self.category))
                    .map_err(|e| CommandError::invalid_args("footprint.unknown_package", e))?;
                if params.get("package").is_none() {
                    params.insert("package", ParamValue::Text(name.clone()));
                }
                Some(spec)
            }
            (None, Some(spec)) => Some(spec.clone()),
            _ => None,
        };
        let footprint: Option<Footprint> = match (&generated, &self.footprint) {
            (Some(spec), _) => Some(
                landpattern::generate(spec, &opts)
                    .map_err(|e| CommandError::invalid_args("footprint.invalid", e.to_string()))?,
            ),
            (None, Some(name)) => Some(util::footprint(ctx.project()?, name)?.clone()),
            (None, None) => None,
        };

        // Pins.
        let mut pins: Vec<Pin> = self
            .pins
            .iter()
            .map(|p| Pin {
                number: p.number.trim().to_string(),
                name: p.name.clone().unwrap_or_default(),
                kind: p.kind,
                group: p.group.clone(),
                side: p.side,
                at: None,
            })
            .collect();
        if pins.is_empty() {
            let two_terminal = symbolgen::style_for(self.category, 2) != crate::model::part::SymbolStyle::Box;
            if two_terminal {
                pins = symbolgen::two_terminal_pins(self.category);
            } else if matches!(self.category, Category::Connector | Category::TestPoint | Category::Mechanical)
                && let Some(fp) = &footprint
            {
                pins = fp.pad_numbers().into_iter().map(|n| Pin::new(n, "", PinKind::Passive)).collect();
            } else {
                return Err(CommandError::invalid_args("part.missing_pins", "this part needs a `pins` list")
                    .with_hint("give each pin's number, name and kind, as in the datasheet pinout table"));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &pins {
            if p.number.is_empty() || !seen.insert(p.number.clone()) {
                return Err(CommandError::invalid_args(
                    "part.duplicate_pin",
                    format!("pin numbers must be unique and non-empty (`{}`)", p.number),
                ));
            }
        }

        // Pin ↔ pad consistency.
        let fref =
            footprint.as_ref().map(|fp| FootprintRef { footprint: fp.name.clone(), pin_map: self.pin_map.clone() });
        if let (Some(fp), Some(fr)) = (&footprint, &fref) {
            let pads = fp.pad_numbers();
            for k in fr.pin_map.keys() {
                if !seen.contains(k) {
                    return Err(CommandError::invalid_args(
                        "part.pin_map",
                        format!("`pin_map` mentions unknown pin `{k}`"),
                    ));
                }
            }
            let mut used = std::collections::BTreeSet::new();
            for p in &pins {
                for pad in fr.pads_for(&p.number) {
                    if !pads.contains(&pad.as_str()) {
                        return Err(CommandError::invalid_args(
                            "part.pin_without_pad",
                            format!(
                                "pin {} maps to pad `{pad}`, which footprint `{}` does not have",
                                p.number, fp.name
                            ),
                        )
                        .with_hint(format!("footprint pads: {}; use `pin_map` to map pins to pads", pads.join(", "))));
                    }
                    used.insert(pad);
                }
            }
            for pad in pads {
                if !used.contains(pad) {
                    ctx.report(
                        Diagnostic::warning(
                            "part.unconnected_pad",
                            format!("pad {pad} of `{}` is not mapped to any pin", fp.name),
                        )
                        .with_subject(ObjectRef::Part { scheme: "local".into(), id: id.clone() })
                        .with_hint(
                            "map it with `pin_map` (an exposed pad usually belongs to GND), or ignore if intentional",
                        ),
                    );
                }
            }
        }

        let symbol = symbolgen::generate(self.category, pins);
        let description = self.description.clone().unwrap_or_else(|| match (&self.manufacturer, &self.mpn) {
            (_, Some(m)) => format!("{} {m}", self.category.label()),
            _ => self.category.label().to_string(),
        });
        let part = Part {
            id: id.clone(),
            category: self.category,
            description,
            manufacturer: self.manufacturer.clone(),
            mpn: self.mpn.clone(),
            params,
            symbol,
            footprints: fref.into_iter().collect(),
            datasheet: self.datasheet.clone(),
            provenance: Provenance { origin: Origin::Manual, detail: None, license: None },
        };
        let p = ctx.project_mut()?;
        let note = match (generated, footprint) {
            (Some(_), Some(fp)) => ensure_footprint(p, fp),
            _ => None,
        };
        p.library_mut().parts.insert(id.clone(), part);
        if let Some(n) = note {
            ctx.report(n);
        }
        let p = ctx.project()?;
        Ok(PartSummary::of(p, &p.library().parts[&id]))
    }

    fn summarize(o: &PartSummary) -> String {
        format!("created {}", o.line())
    }
}

/// List parts in the project library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    /// Only this category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Text to look for in ID, description, manufacturer or MPN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// Parts found.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PartList {
    /// Parts.
    pub parts: Vec<PartSummary>,
}

impl Command for List {
    const NAME: &'static str = "part.list";
    const SUMMARY: &'static str = "List parts in the project library";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["query"];
    type Output = PartList;

    fn run(self, ctx: &mut Context<'_>) -> Result<PartList, CommandError> {
        let p = ctx.project()?;
        let q = self.query.as_deref().map(str::to_lowercase);
        let parts = p
            .library()
            .parts
            .values()
            .filter(|part| self.category.is_none_or(|c| part.category == c))
            .filter(|part| {
                q.as_ref().is_none_or(|q| {
                    [Some(&part.id), Some(&part.description), part.manufacturer.as_ref(), part.mpn.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|s| s.to_lowercase().contains(q))
                })
            })
            .map(|part| PartSummary::of(p, part))
            .collect();
        Ok(PartList { parts })
    }

    fn summarize(o: &PartList) -> String {
        if o.parts.is_empty() {
            return "no parts".into();
        }
        o.parts.iter().map(PartSummary::line).collect::<Vec<_>>().join("\n")
    }
}

/// Show a part in full: parameters, pins, footprints.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Part ID (or MPN).
    pub id: String,
}

/// Full part plus usage.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PartDetail {
    /// The part.
    pub part: Part,
    /// Components using it.
    pub used_by: Vec<String>,
}

impl Command for Show {
    const NAME: &'static str = "part.show";
    const SUMMARY: &'static str = "Show a part: parameters, pins, footprints";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["id"];
    type Output = PartDetail;

    fn run(self, ctx: &mut Context<'_>) -> Result<PartDetail, CommandError> {
        let p = ctx.project()?;
        let part = util::part(p, &self.id).map_err(|e| util::with_library_hint(ctx, e, &self.id))?;
        Ok(PartDetail { used_by: PartSummary::of(p, part).used_by, part: part.clone() })
    }

    fn summarize(o: &PartDetail) -> String {
        let p = &o.part;
        let mut s = format!("{} ({:?}): {}", p.id, p.category, p.description);
        if let Some(m) = &p.mpn {
            s += &format!("\n  mpn: {}{m}", p.manufacturer.as_ref().map(|x| format!("{x} ")).unwrap_or_default());
        }
        for (k, v) in &p.params.0 {
            s += &format!("\n  {k}: {v}");
        }
        for f in &p.footprints {
            s += &format!("\n  footprint: {}", f.footprint);
        }
        s += "\n  pins:";
        for pin in &p.symbol.pins {
            s += &format!("\n    {:>4} {:<12} {:?}", pin.number, pin.name, pin.kind);
        }
        if !o.used_by.is_empty() {
            s += &format!("\n  used by: {}", o.used_by.join(", "));
        }
        s
    }
}

/// Change part fields. Only given fields change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Set {
    /// Part ID.
    pub id: String,
    /// New description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Manufacturer (empty string clears).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// MPN (empty string clears, making the part generic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Datasheet URL (empty string clears).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasheet: Option<String>,
    /// Parameters to set; null removes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Option<String>>,
    /// Preferred footprint (must exist in the library and have pads for every pin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
}

impl Command for Set {
    const NAME: &'static str = "part.set";
    const SUMMARY: &'static str = "Change a part's description, MPN, parameters or footprint";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["id"];
    type Output = PartSummary;

    fn run(self, ctx: &mut Context<'_>) -> Result<PartSummary, CommandError> {
        let p = ctx.project()?;
        let id = util::part(p, &self.id)?.id.clone();
        let fp = match &self.footprint {
            Some(name) => {
                let fp = util::footprint(p, name)?;
                let part = &p.library().parts[&id];
                let pads = fp.pad_numbers();
                for pin in &part.symbol.pins {
                    if !pads.contains(&pin.number.as_str()) {
                        return Err(CommandError::invalid_args(
                            "part.pin_without_pad",
                            format!("footprint `{}` has no pad `{}` for pin {}", fp.name, pin.number, pin.label()),
                        ));
                    }
                }
                Some(fp.name.clone())
            }
            None => None,
        };
        let mut parsed = Vec::new();
        for (k, v) in &self.params {
            let v = v
                .as_ref()
                .map(|v| ParamValue::parse(k, v))
                .transpose()
                .map_err(|e| CommandError::invalid_args("part.invalid_param", format!("parameter `{k}`: {e}")))?;
            parsed.push((k.clone(), v));
        }
        let part = ctx.project_mut()?.library_mut().parts.get_mut(&id).expect("found above");
        let opt = |s: String| (!s.is_empty()).then_some(s);
        if let Some(d) = self.description {
            part.description = d;
        }
        if let Some(m) = self.manufacturer {
            part.manufacturer = opt(m);
        }
        if let Some(m) = self.mpn {
            part.mpn = opt(m);
        }
        if let Some(d) = self.datasheet {
            part.datasheet = opt(d);
        }
        for (k, v) in parsed {
            match v {
                Some(v) => part.params.insert(k, v),
                None => {
                    part.params.0.remove(&k);
                }
            }
        }
        if let Some(name) = fp {
            part.footprints.retain(|f| f.footprint != name);
            part.footprints.insert(0, FootprintRef::new(name));
        }
        let p = ctx.project()?;
        Ok(PartSummary::of(p, &p.library().parts[&id]))
    }

    fn summarize(o: &PartSummary) -> String {
        format!("updated {}", o.line())
    }
}

/// Remove an unused part from the library.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Part ID.
    pub id: String,
}

/// Result of `part.remove`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Removed {
    /// Removed part ID.
    pub id: String,
}

impl Command for Remove {
    const NAME: &'static str = "part.remove";
    const SUMMARY: &'static str = "Remove an unused part from the library";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["id"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let p = ctx.project()?;
        let part = util::part(p, &self.id)?;
        let id = part.id.clone();
        let users = PartSummary::of(p, part).used_by;
        if !users.is_empty() {
            return Err(CommandError::conflict("part.in_use", format!("part `{id}` is used by {}", users.join(", ")))
                .with_hint(
                    "remove those components (`circuit.remove`) or switch them to another part (`bom.replace`)",
                ));
        }
        let p = ctx.project_mut()?;
        p.library_mut().parts.remove(&id);
        p.bom_mut().lines.remove(&id);
        Ok(Removed { id })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed part {}", o.id)
    }
}

/// Search suppliers and catalogs for orderable parts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// Keywords, all required: "LDO 3.3V", "AP2112", "USB-C receptacle".
    #[serde(default)]
    pub query: String,
    /// Category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Package ("SOT-23-5", "0402").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Parameter filters, value optionally prefixed by >=, <=, >, <:
    /// {"voltage_out": "3.3V", "current_out": ">=500mA", "voltage_in": "5V"}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// Quantity needed (stock check and price break).
    #[serde(default = "one_u64")]
    pub quantity: u64,
    /// Only parts with at least `quantity` in stock.
    #[serde(default)]
    pub in_stock: bool,
    /// Maximum unit price ("0.50 USD").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price: Option<String>,
    /// Include obsolete and last-time-buy parts.
    #[serde(default)]
    pub include_obsolete: bool,
    /// Maximum results.
    #[serde(default = "ten")]
    pub limit: usize,
    /// Only these providers (default: all configured).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

fn one_u64() -> u64 {
    1
}

fn ten() -> usize {
    10
}

pub(crate) fn require_suppliers(ctx: &Context<'_>) -> Result<(), CommandError> {
    if ctx.session.suppliers.is_empty() {
        return Err(CommandError::new(
            crate::command::ErrorKind::NotFound,
            "supplier.none",
            "no part suppliers or catalogs are configured",
        )
        .with_hint("add catalog files to ~/.config/cadlab/catalogs/ or list them in CADLAB_CATALOGS (docs/PARTS.md)"));
    }
    Ok(())
}

pub(crate) fn report_provider_errors(ctx: &mut Context<'_>, errors: &[String]) {
    for e in errors {
        ctx.report(
            Diagnostic::warning("supplier.error", e.clone()).with_hint("results from other providers are still shown"),
        );
    }
}

/// Search results.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchResults {
    /// Quantity the prices are for.
    pub quantity: u64,
    /// Candidates, best first (in stock, active, cheapest, most stock).
    pub candidates: Vec<crate::supplier::Candidate>,
}

impl Command for Search {
    const NAME: &'static str = "part.search";
    const SUMMARY: &'static str = "Search suppliers for orderable parts by keywords and parameter filters";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["query"];
    type Output = SearchResults;

    fn run(self, ctx: &mut Context<'_>) -> Result<SearchResults, CommandError> {
        require_suppliers(ctx)?;
        let mut filters = Vec::new();
        for (k, v) in &self.params {
            filters.push(
                crate::supplier::ParamFilter::parse(k, v)
                    .map_err(|e| CommandError::invalid_args("part.invalid_filter", format!("filter `{k}`: {e}")))?,
            );
        }
        let max_price = self
            .max_price
            .as_deref()
            .map(crate::supplier::Money::parse)
            .transpose()
            .map_err(|e| CommandError::invalid_args("part.invalid_price", e))?;
        let q = crate::supplier::SearchQuery {
            text: self.query.clone(),
            category: self.category,
            package: self.package.clone(),
            filters,
            quantity: self.quantity.max(1),
            in_stock: self.in_stock,
            max_price,
            include_obsolete: self.include_obsolete,
            limit: self.limit.clamp(1, 100),
        };
        let r = ctx.session.suppliers.search(&q, &self.providers);
        report_provider_errors(ctx, &r.errors);
        Ok(SearchResults { quantity: q.quantity, candidates: r.candidates })
    }

    fn summarize(o: &SearchResults) -> String {
        if o.candidates.is_empty() {
            return "no matching parts (relax filters, or check `in_stock`/`max_price`)".into();
        }
        o.candidates
            .iter()
            .map(|c| {
                format!(
                    "{}:{}  {} {}  {}  stock {}  {}  {}",
                    c.provider,
                    c.sku,
                    c.manufacturer.as_deref().unwrap_or("?"),
                    c.mpn,
                    c.package.as_deref().unwrap_or("-"),
                    c.stock,
                    match c.unit_price(o.quantity) {
                        Some(p) if c.moq > o.quantity => format!("{p}/u (MOQ {})", c.moq),
                        Some(p) => format!("{p}/u @{}", o.quantity),
                        None => "no price".into(),
                    },
                    c.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
