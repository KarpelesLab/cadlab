//! `bom.*`: the bill of materials and its sourcing overlay.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{part, util};
use crate::bom::{self, BomRow, CsvFormat};
use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::diag::Diagnostic;
use crate::model::sections::ApprovedPart;
use crate::refs::ObjectRef;

pub(crate) fn register(r: &mut Registry) {
    r.register::<List>()
        .register::<Approve>()
        .register::<Dnp>()
        .register::<Note>()
        .register::<Replace>()
        .register::<Export>()
        .register::<Resolve>()
        .register::<Check>()
        .register::<Cost>()
        .register::<Substitutes>();
}

/// Show the BOM: one line per part, with quantities and designators.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

/// The BOM.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BomList {
    /// Lines.
    pub lines: Vec<BomRow>,
    /// Total components to place.
    pub placements: usize,
    /// Lines without an orderable MPN (generic parts with no approved candidate).
    pub unsourced: Vec<String>,
}

fn bom_list(ctx: &Context<'_>) -> Result<BomList, CommandError> {
    let lines = bom::rows(ctx.project()?);
    let placements = lines.iter().map(|l| l.quantity).sum();
    let unsourced =
        lines.iter().filter(|l| l.quantity > 0 && l.order_mpn().is_none()).map(|l| l.part.clone()).collect();
    Ok(BomList { lines, placements, unsourced })
}

impl Command for List {
    const NAME: &'static str = "bom.list";
    const SUMMARY: &'static str = "Show the BOM: one line per part, with quantities and designators";
    const KIND: CommandKind = CommandKind::Query;
    type Output = BomList;

    fn run(self, ctx: &mut Context<'_>) -> Result<BomList, CommandError> {
        bom_list(ctx)
    }

    fn summarize(o: &BomList) -> String {
        if o.lines.is_empty() {
            return "empty BOM (add components with `circuit.add`)".into();
        }
        let mut s = String::new();
        for l in &o.lines {
            let order = match l.order_mpn() {
                Some((m, p)) => format!("{}{p}", m.map(|m| format!("{m} ")).unwrap_or_default()),
                None => "(generic, no MPN yet)".into(),
            };
            s += &format!("{:>3} x {:<10} {:<28} {}", l.quantity, l.value, l.refdes.join(","), order);
            if !l.dnp.is_empty() {
                s += &format!("  DNP: {}", l.dnp.join(","));
            }
            s.push('\n');
        }
        s += &format!("{} line(s), {} placement(s)", o.lines.len(), o.placements);
        if !o.unsourced.is_empty() {
            s += &format!(", {} without MPN", o.unsourced.len());
        }
        s
    }
}

/// Add or remove approved manufacturer parts for a BOM line: alternates for a concrete part,
/// candidates for a generic one.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Approve {
    /// Part ID.
    pub part: String,
    /// MPNs to add, in order of preference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    /// Manufacturer of the added MPNs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// MPNs to remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

/// Approved MPNs of a line.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Approved {
    /// Part ID.
    pub part: String,
    /// Approved manufacturer parts, in order.
    pub approved: Vec<ApprovedPart>,
}

impl Command for Approve {
    const NAME: &'static str = "bom.approve";
    const SUMMARY: &'static str = "Add or remove approved MPNs (alternates or candidates) for a part";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["part"];
    type Output = Approved;

    fn run(self, ctx: &mut Context<'_>) -> Result<Approved, CommandError> {
        let id = util::part_in(ctx, &self.part)?.id.clone();
        let line = ctx.project_mut()?.bom_mut().lines.entry(id.clone()).or_default();
        for m in &self.remove {
            line.approved.retain(|a| !a.mpn.eq_ignore_ascii_case(m));
        }
        for m in &self.add {
            let m = m.trim();
            if m.is_empty() || line.approved.iter().any(|a| a.mpn.eq_ignore_ascii_case(m)) {
                continue;
            }
            line.approved.push(ApprovedPart { manufacturer: self.manufacturer.clone(), mpn: m.to_string() });
        }
        let approved = line.approved.clone();
        prune_line(ctx, &id)?;
        Ok(Approved { part: id, approved })
    }

    fn summarize(o: &Approved) -> String {
        if o.approved.is_empty() {
            return format!("{}: no approved MPNs", o.part);
        }
        let list: Vec<String> = o.approved.iter().map(|a| a.mpn.clone()).collect();
        format!("{}: approved {}", o.part, list.join(", "))
    }
}

/// Drops an empty overlay line so `bom.json` only holds meaningful data.
fn prune_line(ctx: &mut Context<'_>, id: &str) -> Result<(), CommandError> {
    let bom = ctx.project_mut()?.bom_mut();
    if bom.lines.get(id).is_some_and(|l| l.approved.is_empty() && l.notes.is_none()) {
        bom.lines.remove(id);
    }
    Ok(())
}

/// Mark components as do-not-populate (or populate them again).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dnp {
    /// Reference designators.
    pub refdes: Vec<String>,
    /// True to mark DNP, false to populate again.
    #[serde(default = "yes")]
    pub dnp: bool,
}

fn yes() -> bool {
    true
}

/// Components marked DNP after the change.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DnpList {
    /// All DNP components.
    pub dnp: Vec<String>,
}

impl Command for Dnp {
    const NAME: &'static str = "bom.dnp";
    const SUMMARY: &'static str = "Mark components do-not-populate (they stay on the board)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = DnpList;

    fn run(self, ctx: &mut Context<'_>) -> Result<DnpList, CommandError> {
        let mut keys = Vec::new();
        for r in &self.refdes {
            keys.push(util::refdes_key(ctx.project()?, r)?);
        }
        let bom = ctx.project_mut()?.bom_mut();
        for k in keys {
            if self.dnp {
                bom.dnp.insert(k);
            } else {
                bom.dnp.remove(&k);
            }
        }
        let mut dnp: Vec<String> = bom.dnp.iter().cloned().collect();
        dnp.sort_by(|a, b| crate::model::sections::natural_cmp(a, b));
        Ok(DnpList { dnp })
    }

    fn summarize(o: &DnpList) -> String {
        if o.dnp.is_empty() { "no DNP components".into() } else { format!("DNP: {}", o.dnp.join(", ")) }
    }
}

/// Set the purchasing/assembly note of a BOM line.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// Part ID.
    pub part: String,
    /// Note text; empty clears it.
    pub note: String,
}

impl Command for Note {
    const NAME: &'static str = "bom.note";
    const SUMMARY: &'static str = "Set a purchasing or assembly note on a BOM line";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["part", "note"];
    type Output = Approved;

    fn run(self, ctx: &mut Context<'_>) -> Result<Approved, CommandError> {
        let id = util::part_in(ctx, &self.part)?.id.clone();
        let line = ctx.project_mut()?.bom_mut().lines.entry(id.clone()).or_default();
        line.notes = (!self.note.trim().is_empty()).then(|| self.note.trim().to_string());
        let approved = line.approved.clone();
        prune_line(ctx, &id)?;
        Ok(Approved { part: id, approved })
    }

    fn summarize(o: &Approved) -> String {
        format!("note set on {}", o.part)
    }
}

/// Switch components from one part to another (e.g. a generic part to a concrete one).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Replace {
    /// Current part ID.
    pub from: String,
    /// New part: library ID, MPN, or generic spec.
    pub to: String,
    /// Only these components (default: all using `from`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refdes: Vec<String>,
}

/// Result of `bom.replace`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Replaced {
    /// Components switched.
    pub refdes: Vec<String>,
    /// Old part.
    pub from: String,
    /// New part.
    pub to: String,
}

impl Command for Replace {
    const NAME: &'static str = "bom.replace";
    const SUMMARY: &'static str = "Switch components from one part to another, checking pin compatibility";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["from", "to"];
    type Output = Replaced;

    fn run(self, ctx: &mut Context<'_>) -> Result<Replaced, CommandError> {
        let from = util::part_in(ctx, &self.from)?.id.clone();
        let to = match util::part(ctx.project()?, &self.to) {
            Ok(p) => p.id.clone(),
            Err(e) if crate::partspec::parse(&self.to).is_ok() => {
                let _ = e;
                part::add_generic(ctx, &self.to)?.0
            }
            Err(e) => return Err(util::with_library_hint(ctx, e, &self.to)),
        };
        let p = ctx.project()?;
        let targets: Vec<String> = if self.refdes.is_empty() {
            p.circuit().using_part(&from).map(|(r, _)| r.clone()).collect()
        } else {
            let mut v = Vec::new();
            for r in &self.refdes {
                let k = util::refdes_key(p, r)?;
                if p.circuit().components[&k].part != from {
                    return Err(CommandError::invalid_args("bom.not_using_part", format!("{k} does not use `{from}`")));
                }
                v.push(k);
            }
            v
        };
        if targets.is_empty() {
            return Err(CommandError::new(
                ErrorKind::Conflict,
                "bom.part_unused",
                format!("no component uses `{from}`"),
            ));
        }
        // Pin compatibility: every pin of the old part should exist on the new one.
        let (old, new) = (&p.library().parts[&from], &p.library().parts[&to]);
        let missing: Vec<String> = old
            .symbol
            .pins
            .iter()
            .filter(|pin| new.symbol.pin(&pin.number).is_none())
            .map(|pin| format!("{} ({})", pin.number, pin.label()))
            .collect();
        let new_pins: std::collections::BTreeSet<String> = new.symbol.pins.iter().map(|p| p.number.clone()).collect();
        if !missing.is_empty() {
            ctx.report(
                Diagnostic::warning(
                    "bom.pins_differ",
                    format!("`{to}` lacks pins of `{from}`: {}", missing.join(", ")),
                )
                .with_subject(ObjectRef::Part { scheme: "local".into(), id: to.clone() })
                .with_hint("connections to those pins were dropped; check the pinouts and reconnect"),
            );
        }
        let p = ctx.project_mut()?;
        for r in &targets {
            p.circuit_mut().components.get_mut(r).expect("checked").part = to.clone();
            // Connections to pins the new part does not have are dropped (warned above).
            let gone: Vec<crate::model::circuit::PinRef> = p
                .circuit()
                .nets
                .values()
                .flat_map(|n| n.pins.iter())
                .filter(|pin| pin.refdes == *r && !new_pins.contains(&pin.pin))
                .cloned()
                .collect();
            crate::connect::disconnect(p.circuit_mut(), &gone);
            p.circuit_mut().no_connect.retain(|pin| pin.refdes != *r || new_pins.contains(&pin.pin));
        }
        let mut refdes = targets;
        refdes.sort_by(|a, b| crate::model::sections::natural_cmp(a, b));
        Ok(Replaced { refdes, from, to })
    }

    fn summarize(o: &Replaced) -> String {
        format!("{}: {} -> {}", o.refdes.join(", "), o.from, o.to)
    }
}

/// Write the BOM as CSV.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Output file (relative paths are relative to the project directory).
    pub path: PathBuf,
    /// Column layout: generic (all fields), jlcpcb or pcbway (the BOM layout of that fab profile).
    #[serde(default)]
    pub format: CsvFormat,
}

/// Result of `bom.export`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Exported {
    /// File written.
    pub path: String,
    /// Data lines written.
    pub lines: usize,
}

impl Command for Export {
    const NAME: &'static str = "bom.export";
    const SUMMARY: &'static str = "Write the BOM as CSV (generic, jlcpcb or pcbway layout)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Exported;

    fn run(self, ctx: &mut Context<'_>) -> Result<Exported, CommandError> {
        let list = bom_list(ctx)?;
        // Fab layouts come from the fab profiles, user overrides included.
        let profiles = self.format.profile_id().map(|_| crate::fab::Profiles::load());
        let layout = self
            .format
            .profile_id()
            .and_then(|id| profiles.as_ref()?.get(id)?.assembly.as_ref())
            .map(|a| a.bom.clone());
        let csv = match layout {
            Some(l) => crate::fab::export::bom_csv(&l, &list.lines, &Default::default()),
            None => bom::to_csv(&list.lines, self.format),
        };
        let path = match ctx.session.root() {
            Some(root) if self.path.is_relative() => root.join(&self.path),
            _ => self.path.clone(),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(&path, e))?;
        }
        std::fs::write(&path, &csv).map_err(|e| io(&path, e))?;
        if self.format != CsvFormat::Generic {
            for id in &list.unsourced {
                ctx.report(
                    Diagnostic::warning("bom.no_mpn", format!("`{id}` has no MPN; the fab will need one"))
                        .with_subject(ObjectRef::Part { scheme: "local".into(), id: id.clone() })
                        .with_hint("approve one with `bom.approve`, or switch to a concrete part with `bom.replace`"),
                );
            }
        }
        Ok(Exported { path: path.display().to_string(), lines: csv.lines().count().saturating_sub(1) })
    }

    fn summarize(o: &Exported) -> String {
        format!("wrote {} ({} lines)", o.path, o.lines)
    }
}

fn io(path: &std::path::Path, e: std::io::Error) -> CommandError {
    crate::model::ModelError::Io { path: path.to_path_buf(), source: e }.into()
}

fn boards_default() -> u64 {
    1
}

/// Find orderable candidates for generic BOM lines (those without an MPN or approved parts).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resolve {
    /// Number of boards to build (sets the stock needed).
    #[serde(default = "boards_default")]
    pub boards: u64,
    /// Add the best candidate of each line as an approved MPN.
    #[serde(default)]
    pub apply: bool,
    /// Candidates to propose per line.
    #[serde(default = "three")]
    pub candidates: usize,
    /// Also re-resolve lines that already have approved MPNs.
    #[serde(default)]
    pub all: bool,
    /// Only these providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

fn three() -> usize {
    3
}

/// Proposals for one line.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Proposal {
    /// Part ID.
    pub part: String,
    /// Candidates, best first.
    pub candidates: Vec<crate::supplier::Candidate>,
    /// Whether the first candidate was added as approved.
    pub applied: bool,
}

/// Result of `bom.resolve`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    /// Per-line proposals.
    pub lines: Vec<Proposal>,
    /// Lines with no candidate at all.
    pub unresolved: Vec<String>,
}

impl Command for Resolve {
    const NAME: &'static str = "bom.resolve";
    const SUMMARY: &'static str = "Find in-stock MPNs for generic BOM lines; with `apply`, approve the best";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = Resolution;

    fn run(self, ctx: &mut Context<'_>) -> Result<Resolution, CommandError> {
        part::require_suppliers(ctx)?;
        let p = ctx.project()?;
        let rows: Vec<BomRow> =
            bom::rows(p).into_iter().filter(|r| r.mpn.is_none() && (self.all || r.approved.is_empty())).collect();
        let mut lines = Vec::new();
        let mut errors = Vec::new();
        for row in rows {
            let part = &ctx.project()?.library().parts[&row.part];
            let mut q = crate::sourcing::query_for(part, row.quantity.max(1) as u64 * self.boards);
            q.limit = self.candidates.clamp(1, 20);
            let r = ctx.session.suppliers.search(&q, &self.providers);
            errors.extend(r.errors);
            lines.push(Proposal { part: row.part, candidates: r.candidates, applied: false });
        }
        errors.sort();
        errors.dedup();
        part::report_provider_errors(ctx, &errors);
        if self.apply {
            for l in &mut lines {
                if let Some(c) = l.candidates.first() {
                    let line = ctx.project_mut()?.bom_mut().lines.entry(l.part.clone()).or_default();
                    if !line.approved.iter().any(|a| a.mpn.eq_ignore_ascii_case(&c.mpn)) {
                        line.approved
                            .insert(0, ApprovedPart { manufacturer: c.manufacturer.clone(), mpn: c.mpn.clone() });
                    }
                    l.applied = true;
                }
            }
        }
        let unresolved = lines.iter().filter(|l| l.candidates.is_empty()).map(|l| l.part.clone()).collect::<Vec<_>>();
        for u in &unresolved {
            ctx.report(
                Diagnostic::warning("bom.unresolved", format!("no in-stock candidate for `{u}`"))
                    .with_subject(ObjectRef::Part {
                        scheme: "local".into(),
                        id: u.clone(),
                    })
                    .with_hint("the combination may not exist or be out of stock: relax one requirement (dielectric X7R→X5R, a larger package, voltage) with `part.set`, or search with `part.search`"),
            );
        }
        Ok(Resolution { lines, unresolved })
    }

    fn summarize(o: &Resolution) -> String {
        if o.lines.is_empty() {
            return "nothing to resolve: every line has an MPN".into();
        }
        o.lines
            .iter()
            .map(|l| match l.candidates.first() {
                Some(c) => format!(
                    "{}: {}{} {}{}",
                    l.part,
                    if l.applied { "approved " } else { "" },
                    c.manufacturer.as_deref().unwrap_or("?"),
                    c.mpn,
                    if l.candidates.len() > 1 { format!(" (+{} more)", l.candidates.len() - 1) } else { String::new() }
                ),
                None => format!("{}: no candidate", l.part),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Check stock and lifecycle of every BOM line for a build.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Number of boards to build.
    #[serde(default = "boards_default")]
    pub boards: u64,
    /// Only these providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

/// Sourcing per line.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Sourcing {
    /// Boards.
    pub boards: u64,
    /// Per line.
    pub lines: Vec<crate::sourcing::LineSourcing>,
    /// Total per currency (lines with an offer).
    pub totals: Vec<crate::supplier::Money>,
}

fn source_all(ctx: &mut Context<'_>, boards: u64, providers: &[String]) -> Result<Sourcing, CommandError> {
    part::require_suppliers(ctx)?;
    let rows: Vec<BomRow> = bom::rows(ctx.project()?).into_iter().filter(|r| r.quantity > 0).collect();
    let mut errors = Vec::new();
    let lines: Vec<_> = rows
        .iter()
        .map(|r| crate::sourcing::source_line(r, boards.max(1), &ctx.session.suppliers, providers, &mut errors))
        .collect();
    part::report_provider_errors(ctx, &errors);
    let totals = crate::sourcing::totals(&lines);
    Ok(Sourcing { boards: boards.max(1), lines, totals })
}

impl Command for Check {
    const NAME: &'static str = "bom.check";
    const SUMMARY: &'static str = "Check stock and lifecycle of every BOM line for a number of boards";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Sourcing;

    fn run(self, ctx: &mut Context<'_>) -> Result<Sourcing, CommandError> {
        use crate::sourcing::Availability as A;
        let s = source_all(ctx, self.boards, &self.providers)?;
        for l in &s.lines {
            let subject = ObjectRef::Part { scheme: "local".into(), id: l.part.clone() };
            let d = match l.status {
                A::Ok => continue,
                A::LowStock => Diagnostic::error(
                    "bom.low_stock",
                    format!(
                        "`{}`: need {}, best offer has {}",
                        l.part,
                        l.needed,
                        l.offer.as_ref().map_or(0, |o| o.stock)
                    ),
                )
                .with_hint("approve an alternate (`bom.approve`) or reduce the build"),
                A::EndOfLife => {
                    Diagnostic::warning("bom.end_of_life", format!("`{}`: only end-of-life offers", l.part))
                        .with_hint("find a replacement with `part.search` and `bom.replace`")
                }
                A::NotFound => {
                    Diagnostic::error("bom.not_found", format!("`{}`: no provider lists its MPN(s)", l.part))
                        .with_hint("check the MPN, or approve an alternate")
                }
                A::NoMpn => Diagnostic::error("bom.no_mpn", format!("`{}` has no MPN to order", l.part))
                    .with_hint("run `bom.resolve` or `bom.approve`"),
            };
            ctx.report(d.with_subject(subject));
        }
        Ok(s)
    }

    fn summarize(o: &Sourcing) -> String {
        let bad = o.lines.iter().filter(|l| l.status != crate::sourcing::Availability::Ok).count();
        let mut s: String = o
            .lines
            .iter()
            .map(|l| {
                format!(
                    "{:<28} need {:>6}  {:?}{}",
                    l.part,
                    l.needed,
                    l.status,
                    l.offer
                        .as_ref()
                        .map(|c| format!("  {}:{} stock {}", c.provider, c.sku, c.stock))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        s += &format!("\n{} line(s) checked for {} board(s), {} problem(s)", o.lines.len(), o.boards, bad);
        s
    }
}

/// Cost the BOM for a number of boards, from the cheapest in-stock offer of each line.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cost {
    /// Number of boards to build.
    #[serde(default = "boards_default")]
    pub boards: u64,
    /// Only these providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
}

impl Command for Cost {
    const NAME: &'static str = "bom.cost";
    const SUMMARY: &'static str = "Cost the BOM for a number of boards (cheapest in-stock offer per line)";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Sourcing;

    fn run(self, ctx: &mut Context<'_>) -> Result<Sourcing, CommandError> {
        let s = source_all(ctx, self.boards, &self.providers)?;
        let missing: Vec<&str> = s.lines.iter().filter(|l| l.extended.is_none()).map(|l| l.part.as_str()).collect();
        if !missing.is_empty() {
            ctx.report(
                Diagnostic::warning("bom.incomplete_cost", format!("no price for: {}", missing.join(", ")))
                    .with_hint("totals exclude these lines; see `bom.check`"),
            );
        }
        Ok(s)
    }

    fn summarize(o: &Sourcing) -> String {
        let mut s: String = o
            .lines
            .iter()
            .map(|l| match (&l.offer, &l.unit_price, &l.extended) {
                (Some(c), Some(u), Some(e)) => {
                    format!(
                        "{:<28} {:>6} x {:>12} = {:>12}  {}:{}",
                        l.part,
                        l.order_qty.unwrap_or(0),
                        u.to_string(),
                        e.to_string(),
                        c.provider,
                        c.sku
                    )
                }
                _ => format!("{:<28} no price ({:?})", l.part, l.status),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let totals: Vec<String> = o.totals.iter().map(ToString::to_string).collect();
        s += &format!(
            "\ntotal for {} board(s): {}",
            o.boards,
            if totals.is_empty() { "-".into() } else { totals.join(" + ") }
        );
        s
    }
}

/// Substitute candidates for BOM lines that are not available (not found, short of stock, end
/// of life, or without an MPN): drop-in replacements from providers' cross-reference data, and
/// for passives parts with the same package and value, equal or better tolerance and equal or
/// higher ratings; ranked deterministically (docs/PARTS.md, "Substitutes"). With `fab`, lines are
/// sourced at that fab's catalog providers and substitutions recorded in its `fab-lock.json` are
/// applied. Suggestions only: approve one for every fab with `bom.approve`, or for one fab with
/// `fab.substitute`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Substitutes {
    /// Fab profile whose parts catalog and lock apply (`jlcpcb`, `pcbway`, `generic`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fab: Option<String>,
    /// Number of boards to build (sets the stock needed).
    #[serde(default = "boards_default")]
    pub boards: u64,
    /// Candidates per line.
    #[serde(default = "three")]
    pub candidates: usize,
    /// Only these providers (without `fab`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    /// Only this line (part ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// With `fab`: directory of its `fab-lock.json` (default `out/fab/<fab>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
}

/// Result of `bom.substitutes`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubstituteReport {
    /// Fab profile, if one was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fab: Option<String>,
    /// Boards.
    pub boards: u64,
    /// Lines that need a substitute, in BOM order.
    pub lines: Vec<crate::substitute::LineSubstitutes>,
}

impl Command for Substitutes {
    const NAME: &'static str = "bom.substitutes";
    const SUMMARY: &'static str = "Ranked substitute candidates for unavailable BOM lines (optionally at a fab)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["fab"];
    type Output = SubstituteReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<SubstituteReport, CommandError> {
        part::require_suppliers(ctx)?;
        let limit = self.candidates.clamp(1, 20);
        let boards = self.boards.max(1);
        let mut rows: Vec<BomRow> = bom::rows(ctx.project()?).into_iter().filter(|r| r.quantity > 0).collect();
        if let Some(only) = &self.part {
            if !rows.iter().any(|r| &r.part == only) {
                let near = crate::suggest::did_you_mean(only, rows.iter().map(|r| r.part.as_str()), 3);
                return Err(CommandError::not_found("bom.line_not_found", format!("no BOM line for part `{only}`"))
                    .with_suggestions(&near)
                    .with_hint_if_none("bom.list lists the lines by part ID"));
            }
            rows.retain(|r| &r.part == only);
        }
        let lines = match &self.fab {
            Some(fab) => {
                let ps = super::fab::profiles(ctx);
                let (p, _) = super::fab::profile(&ps, fab)?;
                let dir = super::fab::out_dir(ctx, &p.id, self.dir.as_deref());
                let applied = super::fab::applied_substitutions(ctx, p, &dir);
                let project = ctx.project()?;
                let r = crate::fab::check::parts(project, &rows, p, &ctx.session.suppliers, boards, &applied, limit);
                let errors: Vec<String> =
                    r.diagnostics.iter().filter(|d| d.code == "supplier.error").map(|d| d.message.clone()).collect();
                part::report_provider_errors(ctx, &errors);
                r.substitutes
            }
            None => {
                let mut errors = Vec::new();
                let mut lines = Vec::new();
                for r in &rows {
                    let s =
                        crate::sourcing::source_line(r, boards, &ctx.session.suppliers, &self.providers, &mut errors);
                    if s.status == crate::sourcing::Availability::Ok {
                        continue;
                    }
                    let p = ctx.project()?;
                    let part = p.library().parts.get(&r.part);
                    lines.push(crate::substitute::for_line(
                        r,
                        part,
                        s.status,
                        s.needed,
                        &ctx.session.suppliers,
                        &self.providers,
                        limit,
                        &mut errors,
                    ));
                }
                errors.sort();
                errors.dedup();
                part::report_provider_errors(ctx, &errors);
                lines
            }
        };
        for l in &lines {
            if l.candidates.is_empty() {
                ctx.report(
                    Diagnostic::warning(
                        "bom.no_substitute",
                        format!("no substitute for `{}` ({:?})", l.part, l.status),
                    )
                    .with_subject(ObjectRef::Part { scheme: "local".into(), id: l.part.clone() })
                    .with_hint(l.note.clone().unwrap_or_else(|| "relax the part's requirements (part.set)".into())),
                );
            }
        }
        Ok(SubstituteReport { fab: self.fab.clone(), boards, lines })
    }

    fn summarize(o: &SubstituteReport) -> String {
        if o.lines.is_empty() {
            return format!("every line is available for {} board(s): no substitute needed", o.boards);
        }
        let mut s = String::new();
        for l in &o.lines {
            if !s.is_empty() {
                s.push('\n');
            }
            s += &format!("{} ({}, {:?}, need {}):", l.part, l.designators.join(","), l.status, l.needed);
            if l.candidates.is_empty() {
                s += &format!(" none: {}", l.note.as_deref().unwrap_or("-"));
            }
            for (i, c) in l.candidates.iter().enumerate() {
                s += &format!("\n  {}. {} [{:?}]", i + 1, crate::substitute::describe(c), c.basis);
            }
        }
        s
    }
}
