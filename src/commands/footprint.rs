//! `footprint.*`: land patterns in the project library.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::geom::BBox;
use crate::landpattern::{self, ChipKind, Density, GenOptions, PackageSpec};
use crate::model::Project;
use crate::model::footprint::{Footprint, Mount};
use crate::model::part::valid_id;
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<Generate>().register::<List>().register::<Show>().register::<Remove>();
}

/// Short description of a footprint.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FootprintSummary {
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// SMD or THT.
    pub mount: Mount,
    /// Number of pads.
    pub pads: usize,
    /// Courtyard size (width, height).
    pub courtyard: (Nm, Nm),
    /// Parts using it.
    pub used_by: Vec<String>,
}

impl FootprintSummary {
    fn of(p: &Project, f: &Footprint) -> Self {
        let cy = BBox::of_points(f.courtyard.iter().copied())
            .map(|b| (b.width(), b.height()))
            .unwrap_or((Nm::ZERO, Nm::ZERO));
        let used_by = p
            .library()
            .parts
            .values()
            .filter(|part| part.footprints.iter().any(|r| r.footprint == f.name))
            .map(|part| part.id.clone())
            .collect();
        FootprintSummary {
            name: f.name.clone(),
            description: f.description.clone(),
            mount: f.mount,
            pads: f.pads.len(),
            courtyard: cy,
            used_by,
        }
    }

    fn line(&self) -> String {
        let mut s = format!(
            "{}  {} pads, courtyard {} x {}  {}",
            self.name, self.pads, self.courtyard.0, self.courtyard.1, self.description
        );
        if !self.used_by.is_empty() {
            s += &format!("  used by {}", self.used_by.join(", "));
        }
        s
    }
}

/// Generate a footprint (IPC-7351B) from a package name or datasheet dimensions.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Generate {
    /// Package name: "0402", "SOT-23-5", "SOIC-8", "TSSOP-20", "LQFP-48",
    /// "QFN-32 5x5mm P0.5mm EP3.1mm", "PinHeader 1x04".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Explicit dimensions from the datasheet, instead of `package`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<PackageSpec>,
    /// Body type for two-terminal chips (affects name and height): resistor, capacitor, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChipKind>,
    /// IPC density level (default: nominal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<Density>,
    /// Store under this name instead of the IPC name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Overwrite an existing footprint with the same name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// Result of `footprint.generate`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GenerateResult {
    /// The footprint.
    #[serde(flatten)]
    pub footprint: FootprintSummary,
    /// `created`, `replaced` or `unchanged`.
    pub status: String,
}

impl Command for Generate {
    const NAME: &'static str = "footprint.generate";
    const SUMMARY: &'static str = "Generate an IPC-7351B footprint from a package name or datasheet dimensions";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["package"];
    type Output = GenerateResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<GenerateResult, CommandError> {
        let spec = match (&self.package, self.spec) {
            (Some(name), None) => landpattern::packages::parse(name, self.kind.unwrap_or_default())
                .map_err(|e| CommandError::invalid_args("footprint.unknown_package", e))?,
            (None, Some(spec)) => spec,
            _ => {
                return Err(CommandError::invalid_args("footprint.missing_package", "give either `package` or `spec`"));
            }
        };
        let opts = GenOptions { density: self.density.unwrap_or_default(), ..Default::default() };
        let mut fp = landpattern::generate(&spec, &opts)
            .map_err(|e| CommandError::invalid_args("footprint.invalid", e.to_string()))?;
        if let Some(n) = self.name {
            if !valid_id(&n) {
                return Err(CommandError::invalid_args(
                    "footprint.invalid_name",
                    format!("invalid footprint name `{n}`"),
                ));
            }
            fp.name = n;
        }
        let p = ctx.project_mut()?;
        let existing = p.library().find_footprint_ci(&fp.name).map(String::from);
        let status = match existing {
            Some(name) if p.library().footprints[&name] == fp => "unchanged",
            Some(name) if !self.replace => {
                return Err(CommandError::conflict(
                    "footprint.exists",
                    format!("footprint `{name}` already exists with different content"),
                )
                .with_hint("pass `replace: true` to overwrite it, or `name` to store under another name"));
            }
            Some(name) => {
                p.library_mut().footprints.remove(&name);
                p.library_mut().footprints.insert(fp.name.clone(), fp.clone());
                "replaced"
            }
            None => {
                p.library_mut().footprints.insert(fp.name.clone(), fp.clone());
                "created"
            }
        };
        let p = ctx.project()?;
        Ok(GenerateResult {
            footprint: FootprintSummary::of(p, &p.library().footprints[&fp.name]),
            status: status.into(),
        })
    }

    fn summarize(o: &GenerateResult) -> String {
        format!("{}: {}", o.status, o.footprint.line())
    }
}

/// List footprints in the project library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

/// Footprints.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FootprintList {
    /// Footprints.
    pub footprints: Vec<FootprintSummary>,
}

impl Command for List {
    const NAME: &'static str = "footprint.list";
    const SUMMARY: &'static str = "List footprints in the project library";
    const KIND: CommandKind = CommandKind::Query;
    type Output = FootprintList;

    fn run(self, ctx: &mut Context<'_>) -> Result<FootprintList, CommandError> {
        let p = ctx.project()?;
        Ok(FootprintList { footprints: p.library().footprints.values().map(|f| FootprintSummary::of(p, f)).collect() })
    }

    fn summarize(o: &FootprintList) -> String {
        if o.footprints.is_empty() {
            return "no footprints".into();
        }
        o.footprints.iter().map(FootprintSummary::line).collect::<Vec<_>>().join("\n")
    }
}

/// Show a footprint in full: pads and graphics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Show {
    /// Footprint name.
    pub name: String,
}

impl Command for Show {
    const NAME: &'static str = "footprint.show";
    const SUMMARY: &'static str = "Show a footprint: pads, courtyard, graphics";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Footprint;

    fn run(self, ctx: &mut Context<'_>) -> Result<Footprint, CommandError> {
        Ok(util::footprint(ctx.project()?, &self.name)?.clone())
    }

    fn summarize(f: &Footprint) -> String {
        let mut s = format!("{} ({:?}): {}", f.name, f.mount, f.description);
        for p in &f.pads {
            let (w, h) = p.shape.size();
            s += &format!("\n  pad {:>3} at ({}, {}) {} x {}", p.number, p.at.x, p.at.y, w, h);
        }
        s
    }
}

/// Remove an unused footprint.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    /// Footprint name.
    pub name: String,
}

/// Result of `footprint.remove`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Removed {
    /// Removed footprint.
    pub name: String,
}

impl Command for Remove {
    const NAME: &'static str = "footprint.remove";
    const SUMMARY: &'static str = "Remove a footprint no part uses";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let p = ctx.project()?;
        let f = util::footprint(p, &self.name)?;
        let summary = FootprintSummary::of(p, f);
        if !summary.used_by.is_empty() {
            return Err(CommandError::conflict(
                "footprint.in_use",
                format!("footprint `{}` is used by {}", f.name, summary.used_by.join(", ")),
            ));
        }
        let name = f.name.clone();
        ctx.project_mut()?.library_mut().footprints.remove(&name);
        Ok(Removed { name })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed footprint {}", o.name)
    }
}
