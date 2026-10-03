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
        .register::<Export>();
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
    let unsourced = lines
        .iter()
        .filter(|l| l.quantity > 0 && l.order_mpn().is_none())
        .map(|l| l.part.clone())
        .collect();
    Ok(BomList {
        lines,
        placements,
        unsourced,
    })
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
            s += &format!(
                "{:>3} x {:<10} {:<28} {}",
                l.quantity,
                l.value,
                l.refdes.join(","),
                order
            );
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
        let id = util::part(ctx.project()?, &self.part)?.id.clone();
        let line = ctx.project_mut()?.bom_mut().lines.entry(id.clone()).or_default();
        for m in &self.remove {
            line.approved.retain(|a| !a.mpn.eq_ignore_ascii_case(m));
        }
        for m in &self.add {
            let m = m.trim();
            if m.is_empty() || line.approved.iter().any(|a| a.mpn.eq_ignore_ascii_case(m)) {
                continue;
            }
            line.approved.push(ApprovedPart {
                manufacturer: self.manufacturer.clone(),
                mpn: m.to_string(),
            });
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
    if bom
        .lines
        .get(id)
        .is_some_and(|l| l.approved.is_empty() && l.notes.is_none())
    {
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
        if o.dnp.is_empty() {
            "no DNP components".into()
        } else {
            format!("DNP: {}", o.dnp.join(", "))
        }
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
        let id = util::part(ctx.project()?, &self.part)?.id.clone();
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
        let from = util::part(ctx.project()?, &self.from)?.id.clone();
        let to = match util::part(ctx.project()?, &self.to) {
            Ok(p) => p.id.clone(),
            Err(e) if crate::partspec::parse(&self.to).is_ok() => {
                let _ = e;
                part::add_generic(ctx, &self.to)?.0
            }
            Err(e) => return Err(e),
        };
        let p = ctx.project()?;
        let targets: Vec<String> = if self.refdes.is_empty() {
            p.circuit().using_part(&from).map(|(r, _)| r.clone()).collect()
        } else {
            let mut v = Vec::new();
            for r in &self.refdes {
                let k = util::refdes_key(p, r)?;
                if p.circuit().components[&k].part != from {
                    return Err(CommandError::invalid_args(
                        "bom.not_using_part",
                        format!("{k} does not use `{from}`"),
                    ));
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
        if !missing.is_empty() {
            ctx.report(
                Diagnostic::warning(
                    "bom.pins_differ",
                    format!("`{to}` lacks pins of `{from}`: {}", missing.join(", ")),
                )
                .with_subject(ObjectRef::Part {
                    scheme: "local".into(),
                    id: to.clone(),
                })
                .with_hint("connections to those pins will be dropped when nets exist; check the pinouts"),
            );
        }
        let p = ctx.project_mut()?;
        for r in &targets {
            p.circuit_mut().components.get_mut(r).expect("checked").part = to.clone();
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
    /// Column layout: generic (all fields), jlcpcb or pcbway (their assembly BOM templates).
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
        let csv = bom::to_csv(&list.lines, self.format);
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
                        .with_subject(ObjectRef::Part {
                            scheme: "local".into(),
                            id: id.clone(),
                        })
                        .with_hint("approve one with `bom.approve`, or switch to a concrete part with `bom.replace`"),
                );
            }
        }
        Ok(Exported {
            path: path.display().to_string(),
            lines: csv.lines().count().saturating_sub(1),
        })
    }

    fn summarize(o: &Exported) -> String {
        format!("wrote {} ({} lines)", o.path, o.lines)
    }
}

fn io(path: &std::path::Path, e: std::io::Error) -> CommandError {
    crate::model::ModelError::Io {
        path: path.to_path_buf(),
        source: e,
    }
    .into()
}
