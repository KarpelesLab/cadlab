//! `drc.*`: design rule check.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::{Diagnostic, Severity};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Run>().register::<Waive>().register::<Unwaive>().register::<Waivers>();
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
/// `targets`. Findings waived with `drc.waive` are left out (and counted); `severity`, `codes`,
/// `limit` and `offset` narrow what is listed, never the counts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// Unrouted connections: `auto` (default: listed one by one up to 10, else summarized in
    /// one finding, which keeps a check before routing readable), `each`, or `summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unrouted: Option<UnroutedReport>,
    /// List only findings at least this severe: `error`, `warning` or `info` (default: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    /// List only findings whose code is one of these or starts with one of them followed by a
    /// dot or an underscore (`drc.clearance`, `fab`, `fab.silk`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codes: Vec<String>,
    /// List at most this many findings (after `offset`); the output's `by_code` still counts
    /// them all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Skip this many of the selected findings (pagination with `limit`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: usize,
    /// Also list the waived findings (as info, prefixed `[waived]`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub show_waived: bool,
}

/// DRC result; the findings themselves are the command's diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DrcReport {
    /// Number of errors (a summary of unrouted connections counts once; waived findings do not
    /// count).
    pub errors: usize,
    /// Number of warnings (waived findings do not count).
    pub warnings: usize,
    /// Connections not routed yet (also when they are summarized in one finding).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unrouted: usize,
    /// Findings covered by a waiver (`drc.waivers` lists the waivers).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waived: usize,
    /// Errors and warnings per code (waived ones left out), errors first, most first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_code: Vec<CodeCount>,
    /// Findings listed (after `severity`, `codes`, `offset` and `limit`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub listed: usize,
    /// Findings selected by `severity` and `codes` but not listed (`offset`, `limit`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub not_listed: usize,
}

/// Number of findings of one code.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CodeCount {
    /// Finding code.
    pub code: String,
    /// Severity of the findings.
    pub severity: Severity,
    /// How many.
    pub count: usize,
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
        // Waivers.
        let waivers = &ctx.project()?.board().drc_waivers;
        let mut used = vec![false; waivers.len()];
        let mut waived = Vec::new();
        let mut kept = Vec::new();
        for d in diags {
            let subjects: Vec<String> = d.subjects.iter().map(ToString::to_string).collect();
            match waivers.iter().position(|w| w.covers(&d.code, &subjects)) {
                Some(i) if d.severity != Severity::Info => {
                    used[i] = true;
                    waived.push(d);
                }
                _ => kept.push(d),
            }
        }
        let mut notes = Vec::new();
        for (w, used) in waivers.iter().zip(&used) {
            if !used {
                let what =
                    if w.subjects.is_empty() { String::new() } else { format!(" for {}", w.subjects.join(", ")) };
                notes.push(
                    Diagnostic::info(
                        "drc.waiver_unused",
                        format!("the waiver of `{}`{what} matches no finding", w.code),
                    )
                    .with_hint(format!("remove it if the cause is gone: `drc.unwaive {}`", w.code)),
                );
            }
        }
        let errors = kept.iter().filter(|d| d.severity == Severity::Error).count();
        let warnings = kept.iter().filter(|d| d.severity == Severity::Warning).count();
        let mut by: std::collections::BTreeMap<(String, Severity), usize> = std::collections::BTreeMap::new();
        for d in kept.iter().filter(|d| d.severity != Severity::Info) {
            *by.entry((d.code.to_string(), d.severity)).or_default() += 1;
        }
        let mut by_code: Vec<CodeCount> =
            by.into_iter().map(|((code, severity), count)| CodeCount { code, severity, count }).collect();
        by_code.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.count.cmp(&a.count)).then(a.code.cmp(&b.code)));
        let waived_count = waived.len();
        if self.show_waived {
            for mut d in waived {
                d.severity = Severity::Info;
                d.message = format!("[waived] {}", d.message);
                kept.push(d);
            }
        }
        kept.extend(notes);
        // Selection.
        let selected: Vec<Diagnostic> = kept
            .into_iter()
            .filter(|d| self.severity.is_none_or(|s| d.severity >= s))
            .filter(|d| self.codes.is_empty() || self.codes.iter().any(|c| code_matches(&d.code, c)))
            .collect();
        let total = selected.len();
        let listed: Vec<Diagnostic> =
            selected.into_iter().skip(self.offset).take(self.limit.unwrap_or(usize::MAX)).collect();
        let n = listed.len();
        for d in listed {
            ctx.report(d);
        }
        Ok(DrcReport {
            errors,
            warnings,
            unrouted: count,
            waived: waived_count,
            by_code,
            listed: n,
            not_listed: total - n,
        })
    }

    fn summarize(o: &DrcReport) -> String {
        let mut s = if o.errors == 0 && o.warnings == 0 {
            "DRC clean".to_string()
        } else if o.unrouted > 0 {
            format!("DRC: {} error(s), {} warning(s); {} unrouted connection(s)", o.errors, o.warnings, o.unrouted)
        } else {
            format!("DRC: {} error(s), {} warning(s)", o.errors, o.warnings)
        };
        if o.waived > 0 {
            s += &format!(" ({} waived)", o.waived);
        }
        // Many findings: the counts per code tell where to look first.
        if o.errors + o.warnings > UNROUTED_AUTO_LIMIT && o.by_code.len() > 1 {
            let list: Vec<String> = o.by_code.iter().map(|c| format!("{} {}", c.code, c.count)).collect();
            s += &format!("\n  by code: {}", list.join(", "));
        }
        if o.not_listed > 0 {
            s += &format!(
                "\n  listed {} finding(s), {} more not listed (`offset`/`limit`, or narrow with `codes`/`severity`)",
                o.listed, o.not_listed
            );
        }
        s
    }
}

/// Whether `code` is `filter` or lies under it (`fab` and `fab.silk` select `fab.silk_width`).
fn code_matches(code: &str, filter: &str) -> bool {
    let filter = filter.trim_end_matches(['.', '*']);
    code == filter || code.strip_prefix(filter).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('_'))
}

/// Accept DRC findings on purpose: a finding with this code naming all the given subjects is
/// left out of `drc.run`'s errors and warnings (counted as waived). For deliberate departures
/// from the rules (an edge connector's courtyard over the board edge), not to hide problems.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Waive {
    /// Finding code, as `drc.run` prints it (`drc.footprint_outside`, `fab.silk_to_pad`).
    pub code: String,
    /// Subjects the finding must name (`J1`, `net:GND`, `graphic#208`), as printed in the
    /// finding's parentheses; none: every finding of the code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<String>,
    /// Why it is accepted (kept with the board, shown by `drc.waivers`).
    pub reason: String,
}

/// The board's waivers after a change.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WaiverList {
    /// All waivers.
    pub waivers: Vec<crate::model::board::DrcWaiver>,
}

impl Command for Waive {
    const NAME: &'static str = "drc.waive";
    const SUMMARY: &'static str =
        "Waive DRC findings on purpose (code, subjects, reason): left out of errors and warnings";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["code"];
    type Output = WaiverList;

    fn run(self, ctx: &mut Context<'_>) -> Result<WaiverList, CommandError> {
        let code = self.code.trim().to_string();
        if !code.contains('.') {
            return Err(CommandError::invalid_args("drc.waiver_code", format!("`{code}` is not a finding code"))
                .with_hint("give the code as `drc.run` prints it, e.g. `drc.footprint_outside`"));
        }
        if self.reason.trim().is_empty() {
            return Err(CommandError::invalid_args("drc.waiver_reason", "a waiver needs a reason")
                .with_hint("say why the finding is acceptable (`reason`)"));
        }
        let subjects: Vec<String> =
            self.subjects.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let w = crate::model::board::DrcWaiver { code, subjects, reason: self.reason.trim().to_string() };
        let board = ctx.project_mut()?.board_mut();
        match board.drc_waivers.iter_mut().find(|x| x.code == w.code && x.subjects == w.subjects) {
            Some(x) => x.reason = w.reason,
            None => board.drc_waivers.push(w),
        }
        Ok(WaiverList { waivers: board.drc_waivers.clone() })
    }

    fn summarize(o: &WaiverList) -> String {
        waivers_text(&o.waivers)
    }
}

/// Remove DRC waivers: those of a code (all, or only the one with exactly these subjects).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unwaive {
    /// Finding code of the waivers to remove.
    pub code: String,
    /// Only the waiver with exactly these subjects (default: every waiver of the code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subjects: Option<Vec<String>>,
}

impl Command for Unwaive {
    const NAME: &'static str = "drc.unwaive";
    const SUMMARY: &'static str = "Remove DRC waivers of a code";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["code"];
    type Output = WaiverList;

    fn run(self, ctx: &mut Context<'_>) -> Result<WaiverList, CommandError> {
        let board = ctx.project_mut()?.board_mut();
        let before = board.drc_waivers.len();
        board.drc_waivers.retain(|w| {
            !(w.code == self.code
                && self.subjects.as_ref().is_none_or(|s| {
                    s.len() == w.subjects.len()
                        && s.iter().all(|x| w.subjects.iter().any(|y| y.eq_ignore_ascii_case(x)))
                }))
        });
        if board.drc_waivers.len() == before {
            let codes: Vec<String> = board.drc_waivers.iter().map(|w| w.code.clone()).collect();
            let hint = if codes.is_empty() {
                "the board has no waivers".to_string()
            } else {
                format!("waived codes: {} (`drc.waivers` lists them)", codes.join(", "))
            };
            return Err(CommandError::not_found(
                "drc.waiver_not_found",
                format!("no waiver of `{}` with these subjects", self.code),
            )
            .with_hint(hint));
        }
        Ok(WaiverList { waivers: board.drc_waivers.clone() })
    }

    fn summarize(o: &WaiverList) -> String {
        waivers_text(&o.waivers)
    }
}

/// List the board's DRC waivers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Waivers {}

impl Command for Waivers {
    const NAME: &'static str = "drc.waivers";
    const SUMMARY: &'static str = "List the board's DRC waivers";
    const KIND: CommandKind = CommandKind::Query;
    type Output = WaiverList;

    fn run(self, ctx: &mut Context<'_>) -> Result<WaiverList, CommandError> {
        Ok(WaiverList { waivers: ctx.project()?.board().drc_waivers.clone() })
    }

    fn summarize(o: &WaiverList) -> String {
        waivers_text(&o.waivers)
    }
}

fn waivers_text(ws: &[crate::model::board::DrcWaiver]) -> String {
    if ws.is_empty() {
        return "no DRC waivers".into();
    }
    let mut s = format!("{} DRC waiver(s):", ws.len());
    for w in ws {
        let what = if w.subjects.is_empty() { "every finding".to_string() } else { w.subjects.join(", ") };
        s += &format!("\n  {} ({what}): {}", w.code, w.reason);
    }
    s
}
