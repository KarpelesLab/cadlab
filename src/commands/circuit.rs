//! `circuit.*`: components (nets arrive in M2).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{part, util};
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::model::sections::{Component, natural_cmp};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Add>().register::<Remove>().register::<List>();
}

fn valid_refdes(s: &str) -> bool {
    let letters = s.trim_end_matches(|c: char| c.is_ascii_digit());
    !letters.is_empty() && letters.len() < s.len() && letters.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

/// Add components: one or more instances of a part.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Add {
    /// Part ID or MPN from the library, or a generic spec ("R 10k 1% 0402") which adds the part
    /// to the library if needed.
    pub part: String,
    /// How many.
    #[serde(default = "one")]
    pub count: u32,
    /// Reference designator (only with count 1). Default: next free for the part's category (`R3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refdes: Option<String>,
    /// Free-form properties, e.g. {"function": "power LED"}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

fn one() -> u32 {
    1
}

/// Result of `circuit.add`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Added {
    /// New reference designators.
    pub refdes: Vec<String>,
    /// Part used.
    pub part: String,
    /// Whether the part was created from a generic spec.
    pub part_created: bool,
}

impl Command for Add {
    const NAME: &'static str = "circuit.add";
    const SUMMARY: &'static str = "Add components (instances of a library part, or of a generic spec)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["part"];
    type Output = Added;

    fn run(self, ctx: &mut Context<'_>) -> Result<Added, CommandError> {
        if self.count == 0 || self.count > 1000 {
            return Err(CommandError::invalid_args(
                "circuit.invalid_count",
                "count must be between 1 and 1000",
            ));
        }
        if self.refdes.is_some() && self.count != 1 {
            return Err(CommandError::invalid_args(
                "circuit.refdes_with_count",
                "`refdes` can only be given with count 1",
            ));
        }
        // Library part, else a generic spec.
        let (id, created) = match util::part(ctx.project()?, &self.part) {
            Ok(p) => (p.id.clone(), false),
            Err(not_found) => {
                if crate::partspec::parse(&self.part).is_ok() {
                    part::add_generic(ctx, &self.part)?
                } else {
                    return Err(not_found);
                }
            }
        };
        let prefix = ctx.project()?.library().parts[&id].category.refdes_prefix();
        let mut added = Vec::new();
        for _ in 0..self.count {
            let p = ctx.project_mut()?;
            let refdes = match &self.refdes {
                Some(r) => {
                    let r = r.trim().to_uppercase();
                    if !valid_refdes(&r) {
                        return Err(CommandError::invalid_args(
                            "circuit.invalid_refdes",
                            format!("`{r}` is not a reference designator (letters then a number: R1, U12, SW3)"),
                        ));
                    }
                    if p.circuit().components.contains_key(&r) {
                        return Err(
                            CommandError::conflict("circuit.refdes_taken", format!("`{r}` already exists"))
                                .with_hint(format!("next free: {}", p.circuit().next_refdes(prefix))),
                        );
                    }
                    r
                }
                None => p.circuit().next_refdes(prefix),
            };
            let id_obj = p.alloc_id();
            p.circuit_mut().components.insert(
                refdes.clone(),
                Component {
                    id: id_obj,
                    part: id.clone(),
                    properties: self.properties.clone(),
                },
            );
            added.push(refdes);
        }
        Ok(Added {
            refdes: added,
            part: id,
            part_created: created,
        })
    }

    fn summarize(o: &Added) -> String {
        format!(
            "added {} ({}){}",
            o.refdes.join(", "),
            o.part,
            if o.part_created { ", new part" } else { "" }
        )
    }
}

/// Remove components.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Reference designators.
    pub refdes: Vec<String>,
}

/// Result of `circuit.remove`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Removed {
    /// Removed components.
    pub refdes: Vec<String>,
}

impl Command for Remove {
    const NAME: &'static str = "circuit.remove";
    const SUMMARY: &'static str = "Remove components";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let mut keys = Vec::new();
        for r in &self.refdes {
            keys.push(util::refdes_key(ctx.project()?, r)?);
        }
        let p = ctx.project_mut()?;
        for k in &keys {
            p.circuit_mut().components.remove(k);
            p.bom_mut().dnp.remove(k);
        }
        Ok(Removed { refdes: keys })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed {}", o.refdes.join(", "))
    }
}

/// List components.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {
    /// Only components using this part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

/// A component, for listings.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ComponentInfo {
    /// Reference designator.
    pub refdes: String,
    /// Part ID.
    pub part: String,
    /// Value.
    pub value: String,
    /// Footprint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<String>,
    /// Do not populate.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dnp: bool,
    /// Properties.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

/// Components.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ComponentList {
    /// Components in natural order.
    pub components: Vec<ComponentInfo>,
}

impl Command for List {
    const NAME: &'static str = "circuit.list";
    const SUMMARY: &'static str = "List components";
    const KIND: CommandKind = CommandKind::Query;
    type Output = ComponentList;

    fn run(self, ctx: &mut Context<'_>) -> Result<ComponentList, CommandError> {
        let p = ctx.project()?;
        let filter = match &self.part {
            Some(id) => Some(util::part(p, id)?.id.clone()),
            None => None,
        };
        let mut components: Vec<ComponentInfo> = p
            .circuit()
            .components
            .iter()
            .filter(|(_, c)| filter.as_ref().is_none_or(|f| *f == c.part))
            .map(|(r, c)| {
                let part = p.library().parts.get(&c.part);
                ComponentInfo {
                    refdes: r.clone(),
                    part: c.part.clone(),
                    value: part.map(|pt| pt.value()).unwrap_or_default(),
                    footprint: part.and_then(|pt| pt.footprint()).map(|f| f.footprint.clone()),
                    dnp: p.bom().dnp.contains(r),
                    properties: c.properties.clone(),
                }
            })
            .collect();
        components.sort_by(|a, b| natural_cmp(&a.refdes, &b.refdes));
        Ok(ComponentList { components })
    }

    fn summarize(o: &ComponentList) -> String {
        if o.components.is_empty() {
            return "no components".into();
        }
        o.components
            .iter()
            .map(|c| {
                format!(
                    "{:<6} {:<10} {}{}{}",
                    c.refdes,
                    c.value,
                    c.part,
                    c.footprint.as_ref().map(|f| format!("  ({f})")).unwrap_or_default(),
                    if c.dnp { "  DNP" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
