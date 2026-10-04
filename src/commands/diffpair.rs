//! `diffpair.*` and `lengthgroup.*`: differential pairs and length-matching groups
//! (`crate::lengths`, docs/ROUTER.md). Routing them is `route.diffpair` and `route.tune`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::lengths::{self, GroupStatus, PairRules, PairSuggestion};
use crate::model::Project;
use crate::model::circuit::{DiffPair, LengthGroup};
use crate::refs::ObjectRef;
use crate::suggest::did_you_mean;
use crate::units::Nm;

pub(crate) fn register(r: &mut Registry) {
    r.register::<PairAdd>()
        .register::<PairRemove>()
        .register::<PairList>()
        .register::<PairSuggest>()
        .register::<GroupSet>()
        .register::<GroupRemove>()
        .register::<GroupList>();
}

/// Default length group tolerance when none is given: 0.1 mm.
pub const DEFAULT_TOLERANCE: Nm = Nm(100_000);

fn pair_ref(name: &str) -> ObjectRef {
    ObjectRef::Named { kind: "diffpair".into(), name: name.to_string() }
}

fn pair_not_found(p: &Project, name: &str) -> CommandError {
    let s = did_you_mean(name, p.circuit().diffpairs.keys().map(String::as_str), 3);
    CommandError::not_found("diffpair.not_found", format!("no differential pair `{name}`"))
        .with_subject(pair_ref(name))
        .with_suggestions(&s)
        .with_hint_if_none("list pairs with diffpair.list; define one with diffpair.add")
}

/// A differential pair with its rules and, once routed, its measured geometry.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PairInfo {
    /// Name.
    pub name: String,
    /// The definition.
    #[serde(flatten)]
    pub pair: DiffPair,
    /// Width, gap and clearance it routes with.
    pub rules: PairRules,
    /// Routed length of the positive net (tracks along arcs, plus vias through the stackup).
    pub length_p: Nm,
    /// Routed length of the negative net.
    pub length_n: Nm,
    /// Length difference.
    pub skew: Nm,
    /// Coupled track length (the smaller of the two nets').
    pub coupled: Nm,
    /// Largest uncoupled track length.
    pub uncoupled: Nm,
}

fn pair_info(p: &Project, name: &str) -> PairInfo {
    let d = p.circuit().diffpairs[name].clone();
    let rules = lengths::pair_rules(p, &d);
    let cp = lengths::coupling(p, &d, rules.gap);
    PairInfo {
        name: name.to_string(),
        rules,
        length_p: cp.length_p.total,
        length_n: cp.length_n.total,
        skew: cp.skew,
        coupled: cp.coupled_p.min(cp.coupled_n),
        uncoupled: cp.uncoupled,
        pair: d,
    }
}

fn pair_line(i: &PairInfo) -> String {
    let mut s = format!(
        "{}: {} / {}, {} wide, {} gap{}",
        i.name,
        i.pair.p,
        i.pair.n,
        i.rules.width,
        i.rules.gap,
        i.pair.class.as_ref().map(|c| format!(" (class {c})")).unwrap_or_default()
    );
    if let Some(m) = i.pair.max_skew {
        s.push_str(&format!(", max skew {m}"));
    }
    if let Some(m) = i.pair.max_uncoupled {
        s.push_str(&format!(", max uncoupled {m}"));
    }
    if i.length_p > Nm::ZERO || i.length_n > Nm::ZERO {
        s.push_str(&format!(
            "; routed {} / {}, skew {}, coupled {}, uncoupled {}",
            i.length_p, i.length_n, i.skew, i.coupled, i.uncoupled
        ));
    }
    s
}

/// Define a differential pair (or update one): two nets routed as coupled traces at the
/// width and gap of a net class (`diff_pair_width`, `diff_pair_gap`, e.g. from
/// `impedance.solve --gap`), with optional skew and uncoupled-length limits checked by the DRC.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PairAdd {
    /// Positive net ("USB_D+", "USB_DP").
    pub p: String,
    /// Negative net ("USB_D-", "USB_DM").
    pub n: String,
    /// Pair name (default: the common prefix of the two nets, "USB_D").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Net class with the pair's width, gap and impedance (default: the positive net's class).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Largest allowed length difference between the two nets ("0.1mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_skew: Option<Nm>,
    /// Largest allowed uncoupled length of either net ("3mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uncoupled: Option<Nm>,
}

impl Command for PairAdd {
    const NAME: &'static str = "diffpair.add";
    const SUMMARY: &'static str = "Define a differential pair: two nets routed coupled at a class's width and gap";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["p", "n"];
    type Output = PairInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<PairInfo, CommandError> {
        let p = ctx.project()?;
        let pn = super::net::net_name(p, &self.p)?;
        let nn = super::net::net_name(p, &self.n)?;
        if pn == nn {
            return Err(CommandError::invalid_args("diffpair.same_net", "the two nets of a pair must differ")
                .with_hint("give the positive and the negative net"));
        }
        for (label, v) in [("max_skew", self.max_skew), ("max_uncoupled", self.max_uncoupled)] {
            if v.is_some_and(|v| v < Nm::ZERO) {
                return Err(CommandError::invalid_args(
                    "diffpair.invalid_value",
                    format!("`{label}` cannot be negative"),
                ));
            }
        }
        let c = p.circuit();
        if let Some(cl) = &self.class
            && !c.netclasses.contains_key(cl)
        {
            let s = did_you_mean(cl, c.netclasses.keys().map(String::as_str), 3);
            return Err(CommandError::not_found("netclass.not_found", format!("no net class `{cl}`"))
                .with_suggestions(&s)
                .with_hint_if_none("create it with netclass.set or impedance.solve --netclass"));
        }
        // An existing pair of these nets is updated; a net may be in one pair only.
        let existing = c.diffpairs.iter().find(|(_, d)| d.p == pn && d.n == nn).map(|(k, _)| k.clone());
        for net in [&pn, &nn] {
            if let Some((k, _)) = c.diffpair_of(net)
                && Some(k) != existing.as_deref()
            {
                return Err(CommandError::conflict(
                    "diffpair.net_in_pair",
                    format!("net {net} is already in differential pair {k}"),
                )
                .with_subject(pair_ref(k))
                .with_hint(format!("remove that pair first (diffpair.remove {k})")));
            }
        }
        let name = match (&self.name, &existing) {
            (Some(n), _) => n.trim().to_string(),
            (None, Some(e)) => e.clone(),
            (None, None) => {
                let base = lengths::default_pair_name(&pn, &nn);
                if c.nets.contains_key(&base) || c.diffpairs.contains_key(&base) {
                    format!("{base}_pair")
                } else {
                    base
                }
            }
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(CommandError::invalid_args("diffpair.invalid_name", "pair names have no spaces"));
        }
        if c.nets.contains_key(&name) {
            return Err(CommandError::conflict(
                "diffpair.name_taken",
                format!("`{name}` is a net name; length groups could not tell them apart"),
            )
            .with_hint("give the pair another name"));
        }
        if c.diffpairs.contains_key(&name) && existing.as_deref() != Some(name.as_str()) {
            return Err(CommandError::conflict("diffpair.exists", format!("a differential pair `{name}` exists"))
                .with_subject(pair_ref(&name))
                .with_hint("give another name, or remove it first"));
        }
        let mut d = existing.as_ref().map(|e| c.diffpairs[e].clone()).unwrap_or(DiffPair {
            p: pn,
            n: nn,
            class: None,
            max_skew: None,
            max_uncoupled: None,
        });
        if self.class.is_some() {
            d.class = self.class;
        }
        if self.max_skew.is_some() {
            d.max_skew = self.max_skew;
        }
        if self.max_uncoupled.is_some() {
            d.max_uncoupled = self.max_uncoupled;
        }
        let cm = ctx.project_mut()?.circuit_mut();
        if let Some(e) = &existing
            && *e != name
        {
            cm.diffpairs.remove(e);
            for g in cm.length_groups.values_mut() {
                for m in &mut g.members {
                    if m == e {
                        *m = name.clone();
                    }
                }
            }
        }
        cm.diffpairs.insert(name.clone(), d);
        let info = pair_info(ctx.project()?, &name);
        if !info.rules.from_class {
            ctx.report(
                Diagnostic::info(
                    "diffpair.no_class_rules",
                    format!(
                        "pair {name} has no class with diff_pair_width and diff_pair_gap: it routes {} wide with a {} gap",
                        info.rules.width, info.rules.gap
                    ),
                )
                .with_subject(pair_ref(&name))
                .with_hint("solve them for a differential impedance: impedance.solve --target 90ohm --gap 0.15mm --netclass usb, then net.set --class usb"),
            );
        }
        Ok(info)
    }

    fn summarize(o: &PairInfo) -> String {
        pair_line(o)
    }
}

/// Remove a differential pair definition (the nets and their routing stay).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PairRemove {
    /// Pair name.
    pub name: String,
}

/// Removed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Removed {
    /// Name.
    pub name: String,
    /// Length groups it was removed from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
}

impl Command for PairRemove {
    const NAME: &'static str = "diffpair.remove";
    const SUMMARY: &'static str = "Remove a differential pair definition (nets and routing stay)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let p = ctx.project()?;
        if !p.circuit().diffpairs.contains_key(&self.name) {
            return Err(pair_not_found(p, &self.name));
        }
        let c = ctx.project_mut()?.circuit_mut();
        c.diffpairs.remove(&self.name);
        let mut groups = Vec::new();
        for (k, g) in c.length_groups.iter_mut() {
            let before = g.members.len();
            g.members.retain(|m| *m != self.name);
            if g.members.len() != before {
                groups.push(k.clone());
            }
        }
        Ok(Removed { name: self.name, groups })
    }

    fn summarize(o: &Removed) -> String {
        if o.groups.is_empty() {
            format!("removed pair {}", o.name)
        } else {
            format!("removed pair {} (and from length groups {})", o.name, o.groups.join(", "))
        }
    }
}

/// List differential pairs with their rules and measured lengths, skew and coupling.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PairList {}

/// Pairs.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PairListResult {
    /// Pairs.
    pub pairs: Vec<PairInfo>,
}

impl Command for PairList {
    const NAME: &'static str = "diffpair.list";
    const SUMMARY: &'static str = "List differential pairs: rules, routed lengths, skew, coupled/uncoupled length";
    const KIND: CommandKind = CommandKind::Query;
    type Output = PairListResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<PairListResult, CommandError> {
        let p = ctx.project()?;
        Ok(PairListResult { pairs: p.circuit().diffpairs.keys().map(|k| pair_info(p, k)).collect() })
    }

    fn summarize(o: &PairListResult) -> String {
        if o.pairs.is_empty() {
            "no differential pairs".into()
        } else {
            o.pairs.iter().map(pair_line).collect::<Vec<_>>().join("\n")
        }
    }
}

/// Suggest differential pairs from net names (`X_P`/`X_N`, `X+`/`X-`, `XDP`/`XDM`, `XP`/`XN`);
/// nets already in a pair are left out. Add them with diffpair.add.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PairSuggest {}

/// Suggestions.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Suggestions {
    /// Suggested pairs.
    pub pairs: Vec<PairSuggestion>,
}

impl Command for PairSuggest {
    const NAME: &'static str = "diffpair.suggest";
    const SUMMARY: &'static str = "Suggest differential pairs from net names (_P/_N, +/-, DP/DM)";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Suggestions;

    fn run(self, ctx: &mut Context<'_>) -> Result<Suggestions, CommandError> {
        Ok(Suggestions { pairs: lengths::suggest_pairs(ctx.project()?.circuit()) })
    }

    fn summarize(o: &Suggestions) -> String {
        if o.pairs.is_empty() {
            return "no differential pairs found by name".into();
        }
        o.pairs
            .iter()
            .map(|s| {
                format!(
                    "{}: {} / {} ({}): cadlab diffpair add {} {} --name {}",
                    s.name, s.p, s.n, s.rule, s.p, s.n, s.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Define (or replace) a length-matching group: nets and differential pairs whose routed
/// lengths must agree, either with a target (`target` ± `tolerance`) or with the longest
/// member (within `tolerance` below it). Checked by the DRC (`drc.length_mismatch`), tuned by
/// `route.tune`. Lengths include vias (through the stackup).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupSet {
    /// Group name ("DDR_DQ").
    pub name: String,
    /// Members: net names or differential pair names (a pair counts as the mean of its nets).
    pub members: Vec<String>,
    /// Absolute target length ("25mm"; default: match the longest member).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Nm>,
    /// Allowed deviation (default 0.1mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<Nm>,
}

/// A length group with where its members stand.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GroupInfo {
    /// The definition.
    pub group: LengthGroup,
    /// Members' routed lengths and errors.
    pub status: GroupStatus,
}

fn group_info(p: &Project, name: &str) -> GroupInfo {
    let g = p.circuit().length_groups[name].clone();
    let unrouted = lengths::unrouted_nets(&crate::board::copper_items(p));
    let status = lengths::group_status(p, name, &g, &unrouted);
    GroupInfo { group: g, status }
}

fn group_line(i: &GroupInfo) -> String {
    let g = &i.group;
    let want = match g.target {
        Some(t) => format!("{t} ±{}", g.tolerance),
        None => format!("match the longest within {}", g.tolerance),
    };
    let mut s = format!("{}: {want}", i.status.name);
    for m in &i.status.members {
        let len = m.length.map_or("not routed".to_string(), |l| l.to_string());
        let err = match m.error {
            Some(e) if e != Nm::ZERO => format!(" ({e} off)"),
            Some(_) => " (ok)".into(),
            None => String::new(),
        };
        s.push_str(&format!("\n  {} {}: {len}{err}", m.kind, m.name));
    }
    s
}

impl Command for GroupSet {
    const NAME: &'static str = "lengthgroup.set";
    const SUMMARY: &'static str = "Define a length-matching group of nets/pairs (target or match longest, tolerance)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name", "members"];
    type Output = GroupInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<GroupInfo, CommandError> {
        let name = self.name.trim().to_string();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(CommandError::invalid_args("lengthgroup.invalid_name", "group names have no spaces"));
        }
        if self.members.is_empty() {
            return Err(CommandError::invalid_args("lengthgroup.no_members", "give at least one member")
                .with_hint("members are net names or differential pair names"));
        }
        let tolerance = self.tolerance.unwrap_or(DEFAULT_TOLERANCE);
        if tolerance < Nm::ZERO || self.target.is_some_and(|t| t <= Nm::ZERO) {
            return Err(CommandError::invalid_args(
                "lengthgroup.invalid_value",
                "target must be positive and tolerance not negative",
            ));
        }
        let p = ctx.project()?;
        let c = p.circuit();
        let mut members: Vec<String> = Vec::new();
        for m in &self.members {
            let m = if c.diffpairs.contains_key(m) { m.clone() } else { super::net::net_name(p, m)? };
            if !members.contains(&m) {
                members.push(m);
            }
        }
        // A net may not be in the group both alone and through its pair.
        for m in &members {
            if let Some((k, _)) = c.diffpair_of(m)
                && members.iter().any(|x| x == k)
            {
                return Err(CommandError::invalid_args(
                    "lengthgroup.duplicate_member",
                    format!("net {m} is in the group alone and through pair {k}"),
                )
                .with_hint("list the pair or its nets, not both"));
            }
        }
        if self.target.is_none() && members.len() < 2 {
            return Err(CommandError::invalid_args(
                "lengthgroup.no_target",
                "a group without a target needs at least two members to match",
            )
            .with_hint("give a target length, or more members"));
        }
        ctx.project_mut()?
            .circuit_mut()
            .length_groups
            .insert(name.clone(), LengthGroup { members, target: self.target, tolerance });
        Ok(group_info(ctx.project()?, &name))
    }

    fn summarize(o: &GroupInfo) -> String {
        group_line(o)
    }
}

/// Remove a length group.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupRemove {
    /// Group name.
    pub name: String,
}

impl Command for GroupRemove {
    const NAME: &'static str = "lengthgroup.remove";
    const SUMMARY: &'static str = "Remove a length-matching group";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Removed;

    fn run(self, ctx: &mut Context<'_>) -> Result<Removed, CommandError> {
        let p = ctx.project()?;
        if !p.circuit().length_groups.contains_key(&self.name) {
            let s = did_you_mean(&self.name, p.circuit().length_groups.keys().map(String::as_str), 3);
            return Err(CommandError::not_found("lengthgroup.not_found", format!("no length group `{}`", self.name))
                .with_suggestions(&s)
                .with_hint_if_none("list groups with lengthgroup.list"));
        }
        ctx.project_mut()?.circuit_mut().length_groups.remove(&self.name);
        Ok(Removed { name: self.name, groups: vec![] })
    }

    fn summarize(o: &Removed) -> String {
        format!("removed length group {}", o.name)
    }
}

/// List length groups with their members' routed lengths and errors.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupList {}

/// Groups.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GroupListResult {
    /// Groups.
    pub groups: Vec<GroupInfo>,
}

impl Command for GroupList {
    const NAME: &'static str = "lengthgroup.list";
    const SUMMARY: &'static str = "List length groups with members' routed lengths and errors";
    const KIND: CommandKind = CommandKind::Query;
    type Output = GroupListResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<GroupListResult, CommandError> {
        let p = ctx.project()?;
        Ok(GroupListResult { groups: p.circuit().length_groups.keys().map(|k| group_info(p, k)).collect() })
    }

    fn summarize(o: &GroupListResult) -> String {
        if o.groups.is_empty() {
            "no length groups".into()
        } else {
            o.groups.iter().map(group_line).collect::<Vec<_>>().join("\n")
        }
    }
}
