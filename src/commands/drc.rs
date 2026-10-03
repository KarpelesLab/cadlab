//! `drc.*`: design rule check.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::{Diagnostic, Severity};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Run>();
}

/// Fab compatibility for the manifest's `targets`: what `fab.check` finds for each target fab
/// (board and assembly constraints, not parts availability), as warnings only, prefixed with
/// the fab ID. Targets add checks; they never make DRC fail (DECISIONS D12).
fn target_warnings(p: &crate::model::Project) -> Vec<Diagnostic> {
    let targets = &p.manifest().targets;
    if targets.is_empty() {
        return Vec::new();
    }
    let profiles = crate::fab::Profiles::load();
    let mut out = Vec::new();
    for t in targets {
        let Some(profile) = profiles.get(t) else {
            out.push(
                Diagnostic::warning("fab.unknown_target", format!("target `{t}` is not a known fab profile"))
                    .with_hint(format!("profiles: {}; edit targets in cadlab.toml", profiles.ids().join(", "))),
            );
            continue;
        };
        for mut d in crate::fab::check::check(p, profile, None).diagnostics {
            if d.severity == Severity::Info {
                continue;
            }
            d.severity = Severity::Warning;
            d.message = format!("[{t}] {}", d.message);
            out.push(d);
        }
    }
    out
}

/// Run the design rule check on the board, plus fab compatibility warnings for the manifest's
/// `targets`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Run {}

/// DRC result; the findings themselves are the command's diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DrcReport {
    /// Number of errors.
    pub errors: usize,
    /// Number of warnings.
    pub warnings: usize,
}

impl Command for Run {
    const NAME: &'static str = "drc.run";
    const SUMMARY: &'static str = "Design rule check: clearance, shorts, widths, drills, annular rings, edges, courtyards, silk, keep-outs, unrouted";
    const KIND: CommandKind = CommandKind::Query;
    type Output = DrcReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<DrcReport, CommandError> {
        let mut diags = crate::drc::check(ctx.project()?);
        diags.extend(target_warnings(ctx.project()?));
        let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
        let warnings = diags.iter().filter(|d| d.severity == Severity::Warning).count();
        for d in diags {
            ctx.report(d);
        }
        Ok(DrcReport { errors, warnings })
    }

    fn summarize(o: &DrcReport) -> String {
        if o.errors == 0 && o.warnings == 0 {
            "DRC clean".into()
        } else {
            format!("DRC: {} error(s), {} warning(s)", o.errors, o.warnings)
        }
    }
}
