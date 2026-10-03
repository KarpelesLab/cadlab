//! `drc.*`: design rule check.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Severity;

pub(crate) fn register(r: &mut Registry) {
    r.register::<Run>();
}

/// Run the design rule check on the board.
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
        let diags = crate::drc::check(ctx.project()?);
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
