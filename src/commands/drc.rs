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

/// How `drc.run` reports connections that are not routed yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnroutedReport {
    /// One finding per connection up to 10 of them, else one summary (default).
    #[default]
    Auto,
    /// One `drc.unrouted` finding per connection, with both ends.
    Each,
    /// One `drc.unrouted` finding for all of them, with the count per net.
    Summary,
}

/// Connections listed one by one under `auto`; more are summarized.
pub const UNROUTED_AUTO_LIMIT: usize = 10;

/// Run the design rule check on the board, plus fab compatibility warnings for the manifest's
/// `targets`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// Unrouted connections: `auto` (default: listed one by one up to 10, else summarized in
    /// one finding, which keeps a check before routing readable), `each`, or `summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrouted: Option<UnroutedReport>,
}

/// DRC result; the findings themselves are the command's diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DrcReport {
    /// Number of errors (a summary of unrouted connections counts once).
    pub errors: usize,
    /// Number of warnings.
    pub warnings: usize,
    /// Connections not routed yet (also when they are summarized in one finding).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unrouted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// One `drc.unrouted` finding standing for `each`, the per-connection findings: the count per
/// net (most first, then by name), the nets as subjects, the first connection's location.
fn unrouted_summary(each: &[Diagnostic]) -> Diagnostic {
    let mut per_net: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for d in each {
        if let Some(crate::refs::ObjectRef::Net(n)) = d.subjects.first() {
            *per_net.entry(n.clone()).or_default() += 1;
        }
    }
    let mut nets: Vec<(String, usize)> = per_net.into_iter().collect();
    nets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| crate::model::sections::natural_cmp(&a.0, &b.0)));
    const SHOWN: usize = 8;
    let mut list: Vec<String> = nets.iter().take(SHOWN).map(|(n, c)| format!("{n} {c}")).collect();
    if nets.len() > SHOWN {
        list.push(format!("{} more net(s)", nets.len() - SHOWN));
    }
    let mut d = Diagnostic::error(
        "drc.unrouted",
        format!("{} connection(s) on {} net(s) are not routed: {}", each.len(), nets.len(), list.join(", ")),
    )
    .with_hint(
        "route them (`route.all`; `route.status` shows progress); `drc.run` with `unrouted: each` \
         (`cadlab drc --unrouted each`) lists every connection with both ends",
    );
    for (n, _) in &nets {
        d = d.with_subject(crate::refs::ObjectRef::Net(n.clone()));
    }
    if let Some(at) = each.first().and_then(|x| x.location) {
        d = d.at(at);
    }
    d
}

impl Command for Run {
    const NAME: &'static str = "drc.run";
    const SUMMARY: &'static str = "Design rule check: clearance, shorts, widths, drills, annular rings, edges, courtyards, silk, keep-outs, unrouted";
    const KIND: CommandKind = CommandKind::Query;
    type Output = DrcReport;

    fn run(self, ctx: &mut Context<'_>) -> Result<DrcReport, CommandError> {
        let mut diags = crate::drc::check(ctx.project()?);
        let (unrouted, mut diags): (Vec<Diagnostic>, Vec<Diagnostic>) =
            diags.drain(..).partition(|d| d.code == "drc.unrouted");
        let count = unrouted.len();
        let summarize = match self.unrouted.unwrap_or_default() {
            UnroutedReport::Auto => count > UNROUTED_AUTO_LIMIT,
            UnroutedReport::Each => false,
            UnroutedReport::Summary => count > 0,
        };
        if summarize {
            diags.push(unrouted_summary(&unrouted));
        } else {
            diags.extend(unrouted);
        }
        diags.extend(target_warnings(ctx.project()?));
        let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
        let warnings = diags.iter().filter(|d| d.severity == Severity::Warning).count();
        for d in diags {
            ctx.report(d);
        }
        Ok(DrcReport { errors, warnings, unrouted: count })
    }

    fn summarize(o: &DrcReport) -> String {
        if o.errors == 0 && o.warnings == 0 {
            "DRC clean".into()
        } else if o.unrouted > 0 {
            format!("DRC: {} error(s), {} warning(s); {} unrouted connection(s)", o.errors, o.warnings, o.unrouted)
        } else {
            format!("DRC: {} error(s), {} warning(s)", o.errors, o.warnings)
        }
    }
}
