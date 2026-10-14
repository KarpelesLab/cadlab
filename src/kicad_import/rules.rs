//! Design rules from KiCad project files: the `.kicad_pro` project (board minimums, net classes
//! and their assignments) and the `.kicad_dru` custom rules.
//!
//! Read from the JSON project file as KiCad writes it (keys observed in files KiCad 8 to 10 and
//! cadlab's own export write) and from the custom rules syntax KiCad documents; no KiCad code is
//! involved (DECISIONS D7).
//!
//! Mapping to cadlab (`board.rules`, net classes):
//!
//! | KiCad | cadlab |
//! |---|---|
//! | `min_clearance`, Default class clearance | `clearance` (the larger of both) |
//! | Default class track width, via drill, via diameter | `track_width`, `via_drill`, `via_diameter` |
//! | `min_track_width` | `min_track_width` |
//! | `min_via_annular_width` | `min_annular_ring` |
//! | `min_through_hole_diameter` | `min_drill` |
//! | `min_hole_to_hole` | `hole_to_hole` |
//! | `min_copper_edge_clearance` | `copper_to_edge` |
//! | `defaults.silk_line_width` | `min_silk_width` |
//! | `min_silk_clearance` | `silk_to_pad` (zero included: overlaps only) |
//! | other classes | net classes (values equal to the board defaults are inherited) |
//! | `netclass_patterns`, `netclass_assignments` | `net.class` of matching circuit nets |
//! | `.kicad_dru` rules without condition (`clearance`, `track_width`, `hole_size`, `annular_width`, `hole_to_hole`, `edge_clearance`) | the board rule, when stricter |
//! | `.kicad_dru` `A.NetClass == 'X'` rules (`track_width`, `clearance`) | the class value, when stricter |
//! | `.kicad_dru` rules with geometric conditions ([`condition_scope`]: item type, layer, `memberOfFootprint`, `intersectsCourtyard`, `enclosedByArea`, `intersectsArea` / `insideArea`) with `clearance` / `track_width` minimums or `disallow` (`track`, `via`, `pad`, `zone`, `graphic`) | custom rules (`board.custom_rules`), in file order |
//! | conditions on `B` (the other item of a pair, [`condition_scopes`]) | the custom rule's `with` scope (clearance only) |
//! | rule `severity` (`error`, `warning`, `ignore`; `exclusion` as `ignore`) | the custom rule's `severity` (rules without a condition become custom rules to keep it) |
//!
//! A zero KiCad minimum means "no minimum": the cadlab value is kept. Everything else is reported
//! (`import.rule_unsupported`), never dropped silently.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ImportError, invalid};
use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::board::{CustomRule, ItemKind, RuleScope, RuleSeverity, Rules};

use crate::model::circuit::NetClass;
use crate::refs::ObjectRef;
use crate::sexpr;
use crate::units::Nm;

/// Rules read from KiCad files, before they are applied to a project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KicadRules {
    /// Board rule values by cadlab field name ([`crate::model::board::RULE_FIELDS`]).
    pub rules: BTreeMap<String, Nm>,
    /// Net classes other than `Default`, in file order, with the values the file gives.
    pub classes: Vec<(String, NetClass)>,
    /// Net name patterns (`*` and `?` wildcards) → class, in file order (explicit assignments
    /// first).
    pub patterns: Vec<(String, String)>,
    /// Project text variables (`${NAME}` in texts), substituted into board texts on import:
    /// cadlab texts have no variables.
    pub text_variables: BTreeMap<String, String>,
    /// Custom rules with geometric conditions (`.kicad_dru`), in file order.
    pub custom: Vec<CustomRule>,
}

/// What a rules import changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RulesReport {
    /// Board rules set, by field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rules: BTreeMap<String, Nm>,
    /// Net classes created or updated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub netclasses: Vec<String>,
    /// Circuit nets given a class.
    #[serde(default)]
    pub assigned: usize,
}

/// Length from a JSON number in millimeters.
fn json_mm(v: Option<&Value>) -> Option<Nm> {
    let f = v?.as_f64()?;
    f.is_finite().then(|| Nm((f * 1e6).round() as i64))
}

/// Length in a custom rule: `0.2mm`, `8mil`, or a bare number in millimeters.
fn dru_len(s: &str) -> Option<Nm> {
    Nm::parse(s).or_else(|_| Nm::parse(&format!("{s}mm"))).ok()
}

/// Reads the project file and the custom rules (either may be absent).
pub fn parse(pro: Option<&str>, dru: Option<&str>) -> Result<(KicadRules, Vec<Diagnostic>), ImportError> {
    let mut k = KicadRules::default();
    let mut diags = Vec::new();
    if let Some(text) = pro {
        parse_pro(text, &mut k, &mut diags)?;
    }
    if let Some(text) = dru {
        parse_dru(text, &mut k, &mut diags)?;
    }
    Ok((k, diags))
}

fn parse_pro(text: &str, k: &mut KicadRules, diags: &mut Vec<Diagnostic>) -> Result<(), ImportError> {
    let pro: Value = serde_json::from_str(text).map_err(|e| {
        invalid(
            "import.parse",
            format!("the KiCad project file is not valid JSON: {e}"),
            "give the `.kicad_pro` file KiCad saved next to the board",
        )
    })?;
    if let Some(m) = pro["text_variables"].as_object() {
        for (k2, v) in m {
            if let Some(v) = v.as_str() {
                k.text_variables.insert(k2.clone(), v.to_string());
            }
        }
    }
    let ds = &pro["board"]["design_settings"];
    let r = &ds["rules"];
    let mut set = |field: &str, v: Option<Nm>, kicad: &str, diags: &mut Vec<Diagnostic>| match v {
        Some(v) if v.0 > 0 => {
            k.rules.insert(field.to_string(), v);
        }
        Some(_) => diags.push(
            Diagnostic::info(
                "import.rule_zero",
                format!("KiCad sets no minimum for `{kicad}`; cadlab keeps its `{field}`"),
            )
            .with_subject(ObjectRef::Named { kind: "rule".into(), name: field.into() })
            .with_hint(format!("set it with `board.rules --{field} <length>` if the design needs a different value")),
        ),
        None => {}
    };
    set("min_track_width", json_mm(r.get("min_track_width")), "min_track_width", diags);
    set("min_annular_ring", json_mm(r.get("min_via_annular_width")), "min_via_annular_width", diags);
    set("min_drill", json_mm(r.get("min_through_hole_diameter")), "min_through_hole_diameter", diags);
    set("hole_to_hole", json_mm(r.get("min_hole_to_hole")), "min_hole_to_hole", diags);
    set("copper_to_edge", json_mm(r.get("min_copper_edge_clearance")), "min_copper_edge_clearance", diags);
    set("min_silk_width", json_mm(ds["defaults"].get("silk_line_width")), "defaults.silk_line_width", diags);
    // Zero is a real value here: silkscreen may come up to pads but not over them (cadlab's
    // silk-to-pad check then reports overlaps only), as KiCad's silk_over_copper does.
    if let Some(v) = json_mm(r.get("min_silk_clearance")).filter(|v| v.0 >= 0) {
        k.rules.insert("silk_to_pad".into(), v);
    }
    let min_clearance = json_mm(r.get("min_clearance"));

    let ns = &pro["net_settings"];
    let mut default_clearance = None;
    for c in ns["classes"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(name) = c["name"].as_str() else { continue };
        let nc = NetClass {
            description: None,
            track_width: json_mm(c.get("track_width")).filter(|v| v.0 > 0),
            clearance: json_mm(c.get("clearance")).filter(|v| v.0 > 0),
            via_drill: json_mm(c.get("via_drill")).filter(|v| v.0 > 0),
            via_diameter: json_mm(c.get("via_diameter")).filter(|v| v.0 > 0),
            diff_pair_width: json_mm(c.get("diff_pair_width")).filter(|v| v.0 > 0),
            diff_pair_gap: json_mm(c.get("diff_pair_gap")).filter(|v| v.0 > 0),
            // KiCad keeps impedance targets in its calculator, not in net classes.
            impedance: None,
            diff_impedance: None,
        };
        if name == "Default" {
            default_clearance = nc.clearance;
            for (f, v) in
                [("track_width", nc.track_width), ("via_drill", nc.via_drill), ("via_diameter", nc.via_diameter)]
            {
                if let Some(v) = v {
                    k.rules.insert(f.into(), v);
                }
            }
        } else {
            k.classes.push((name.to_string(), nc));
        }
        // KiCad 6 lists member nets in the class itself.
        for n in c["nets"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(n) = n.as_str() {
                k.patterns.push((n.to_string(), name.to_string()));
            }
        }
    }
    let clearance = match (min_clearance.filter(|v| v.0 > 0), default_clearance) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    if let Some(c) = clearance {
        k.rules.insert("clearance".into(), c);
    }
    // Explicit assignments (net → class or classes) come before patterns.
    if let Some(m) = ns["netclass_assignments"].as_object() {
        for (net, v) in m {
            let class = match v {
                Value::String(s) => Some(s.clone()),
                Value::Array(a) => a.iter().find_map(|x| x.as_str()).map(String::from),
                _ => None,
            };
            if let Some(c) = class {
                k.patterns.insert(0, (net.clone(), c));
            }
        }
    }
    for p in ns["netclass_patterns"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        if let (Some(c), Some(pat)) = (p["netclass"].as_str(), p["pattern"].as_str()) {
            k.patterns.push((pat.to_string(), c.to_string()));
        }
    }
    Ok(())
}

/// The class a custom rule condition restricts to: `A.NetClass == 'X'`, `A.hasNetclass('X')`.
fn condition_class(cond: &str) -> Option<String> {
    let c = cond.trim();
    let quoted = |rest: &str| {
        let rest = rest.trim_start();
        let q = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let body = &rest[1..];
        let end = body.find(q)?;
        let tail = body[end + 1..].trim();
        (tail.is_empty() || tail == ")").then(|| body[..end].to_string())
    };
    for pre in ["A.NetClass ==", "A.NetClass==", "B.NetClass ==", "B.NetClass=="] {
        if let Some(rest) = c.strip_prefix(pre) {
            return quoted(rest);
        }
    }
    for pre in ["A.hasNetclass(", "B.hasNetclass("] {
        if let Some(rest) = c.strip_prefix(pre) {
            return quoted(rest);
        }
    }
    None
}

/// The scope of a geometric custom rule condition: terms joined by `&&`, each one of
/// `A.Type == 'Pad'` (`Track`, `Via`, `Zone`), `A.Type != 'Zone'`, `A.Layer == 'F.Cu'`,
/// `A.memberOfFootprint('lib:name*')`, `A.intersectsCourtyard('...')` and
/// `A.enclosedByArea('name')`. A footprint pattern loses its library prefix (cadlab footprint
/// names have none); a pattern without one matches designators too. Anything else is an error
/// naming the term.
pub fn condition_scope(cond: &str) -> Result<RuleScope, String> {
    let mut s = RuleScope::default();
    let quoted = |t: &str| -> Option<String> {
        let t = t.trim();
        let q = t.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let body = &t[1..];
        let end = body.find(q)?;
        body[end + 1..].trim().is_empty().then(|| body[..end].to_string())
    };
    let call = |t: &str, f: &str| -> Option<String> {
        let rest = t.strip_prefix(f)?.trim_start().strip_prefix('(')?;
        quoted(rest.trim_end().strip_suffix(')')?)
    };
    let kind_of = |v: &str| match v {
        "Pad" => Some(ItemKind::Pad),
        "Track" => Some(ItemKind::Track),
        "Via" => Some(ItemKind::Via),
        "Zone" => Some(ItemKind::Zone),
        "Graphic" => Some(ItemKind::Graphic),
        _ => None,
    };
    let fp_pattern = |v: String| v.rsplit_once(':').map(|(_, n)| n.to_string()).unwrap_or(v);
    for term in cond.split("&&").map(str::trim) {
        let unsupported = || format!("condition term `{term}` is not supported");
        // Alternatives of one kind: `(A.Type == 'Pad' || A.Type == 'Via')`, or one footprint
        // pattern with and without its library (`(A.memberOfFootprint('lib:X') ||
        // A.memberOfFootprint('X'))`, as cadlab's export writes it).
        if let Some(inner) = term.strip_prefix('(').and_then(|t| t.strip_suffix(')'))
            && inner.contains("||")
        {
            let alts: Vec<RuleScope> = inner
                .split("||")
                .map(|a| condition_scope(a.trim()))
                .collect::<Result<_, _>>()
                .map_err(|_| unsupported())?;
            let first = alts[0].clone();
            let same_but_kinds =
                alts.iter().all(|a| RuleScope { kinds: Vec::new(), ..a.clone() } == RuleScope::default());
            if same_but_kinds && alts.iter().all(|a| !a.kinds.is_empty()) {
                let mut kinds: Vec<ItemKind> = alts.iter().flat_map(|a| a.kinds.clone()).collect();
                kinds.sort();
                kinds.dedup();
                s.kinds = if s.kinds.is_empty() {
                    kinds
                } else {
                    s.kinds.into_iter().filter(|x| kinds.contains(x)).collect()
                };
            } else if alts.iter().all(|a| *a == first) {
                s.footprint = first.footprint.or(s.footprint);
                s.in_courtyard = first.in_courtyard.or(s.in_courtyard);
                s.in_area = first.in_area.or(s.in_area);
                s.layer = first.layer.or(s.layer);
            } else {
                return Err(unsupported());
            }
            continue;
        }
        let (lhs, op, rhs) = match (term.split_once("=="), term.split_once("!=")) {
            (Some((l, r)), _) => (l.trim(), "==", r),
            (None, Some((l, r))) => (l.trim(), "!=", r),
            (None, None) => ("", "", ""),
        };
        if lhs == "A.Type" {
            let v = quoted(rhs).ok_or_else(unsupported)?;
            let k = kind_of(&v).ok_or_else(unsupported)?;
            let all = [ItemKind::Pad, ItemKind::Track, ItemKind::Via, ItemKind::Zone, ItemKind::Graphic];
            let set: Vec<ItemKind> = if op == "==" { vec![k] } else { all.into_iter().filter(|x| *x != k).collect() };
            s.kinds = if s.kinds.is_empty() { set } else { s.kinds.into_iter().filter(|x| set.contains(x)).collect() };
            if s.kinds.is_empty() {
                return Err(format!("condition `{cond}` selects nothing"));
            }
        } else if lhs == "A.Layer" && op == "==" {
            let v = quoted(rhs).ok_or_else(unsupported)?;
            if !v.ends_with(".Cu") || s.layer.is_some() {
                return Err(unsupported());
            }
            s.layer = Some(v);
        } else if let Some(v) = call(term, "A.memberOfFootprint") {
            s.footprint = Some(fp_pattern(v));
        } else if let Some(v) = call(term, "A.intersectsCourtyard") {
            s.in_courtyard = Some(fp_pattern(v));
        } else if let Some(v) = call(term, "A.enclosedByArea") {
            s.in_area = Some(v);
        } else if let Some(v) = call(term, "A.intersectsArea").or_else(|| call(term, "A.insideArea")) {
            // `insideArea` is the older name of `intersectsArea` (KiCad's documentation).
            s.touches_area = Some(v);
        } else {
            return Err(unsupported());
        }
    }
    Ok(s)
}

/// The scopes of a geometric condition: the terms on `A` (as [`condition_scope`]) and, when
/// there are terms on `B` (the other item of a pair), those as a second scope. A term naming
/// both items is an error.
pub fn condition_scopes(cond: &str) -> Result<(RuleScope, Option<RuleScope>), String> {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for term in cond.split("&&").map(str::trim) {
        let (on_a, on_b) = (term.contains("A."), term.contains("B."));
        match (on_a, on_b) {
            (true, true) => return Err(format!("condition term `{term}` relates both items")),
            (false, true) => b.push(term.replace("B.", "A.")),
            _ => a.push(term.to_string()),
        }
    }
    let sa = if a.is_empty() { RuleScope::default() } else { condition_scope(&a.join(" && "))? };
    let sb = if b.is_empty() { None } else { Some(condition_scope(&b.join(" && "))?) };
    Ok((sa, sb))
}

/// A `disallow` constraint's item types as cadlab item kinds; `None` names the first type
/// without a counterpart.
fn disallow_kinds(con: &sexpr::Sexpr) -> Result<Vec<ItemKind>, String> {
    let mut kinds = Vec::new();
    for t in con.items().iter().skip(2).filter_map(|x| x.atom()) {
        let k = match t {
            "track" => ItemKind::Track,
            "via" => ItemKind::Via,
            "pad" => ItemKind::Pad,
            "zone" => ItemKind::Zone,
            "graphic" => ItemKind::Graphic,
            other => return Err(format!("disallowed type `{other}` has no cadlab counterpart")),
        };
        if !kinds.contains(&k) {
            kinds.push(k);
        }
    }
    if kinds.is_empty() {
        return Err("`disallow` names no type".into());
    }
    kinds.sort();
    Ok(kinds)
}

fn parse_dru(text: &str, k: &mut KicadRules, diags: &mut Vec<Diagnostic>) -> Result<(), ImportError> {
    // Comment lines start with `#`; the file is a sequence of top-level expressions.
    let body: String = text.lines().filter(|l| !l.trim_start().starts_with('#')).map(|l| format!("{l}\n")).collect();
    let root = sexpr::parse(&format!("({body})")).map_err(|e| {
        invalid(
            "import.parse",
            format!("the custom rules file is not readable: {e}"),
            "check the `.kicad_dru` file with KiCad's custom rules editor",
        )
    })?;
    for rule in root.all("rule") {
        let name = rule.value().unwrap_or("").to_string();
        let unsupported = |why: String, diags: &mut Vec<Diagnostic>| {
            diags.push(
                Diagnostic::warning("import.rule_unsupported", format!("custom rule `{name}` not imported: {why}"))
                    .with_subject(ObjectRef::Named { kind: "rule".into(), name: name.clone() })
                    .with_hint(
                        "cadlab rules are board-wide values, net classes and custom rules scoped by item kind, layer, footprint, courtyard or area: express it with `board.rules`, `netclass.set` or `board.custom_rule` if it matters",
                    ),
            );
        };
        let severity = match rule.child_value("severity") {
            None | Some("error") => RuleSeverity::Error,
            Some("warning") => RuleSeverity::Warning,
            Some("ignore" | "exclusion") => RuleSeverity::Ignore,
            Some(sev) => {
                unsupported(format!("severity `{sev}` is not one of error, warning, ignore"), diags);
                continue;
            }
        };
        let cond = rule.child_value("condition");
        let layer = rule.child_value("layer");
        let class_cond = cond.and_then(condition_class);
        let disallows = rule.all("constraint").any(|c| c.value() == Some("disallow"));
        if class_cond.is_some() && (severity != RuleSeverity::Error || disallows) {
            unsupported("a severity or `disallow` on a net class condition".into(), diags);
            continue;
        }
        // Geometric conditions (item kind, layer, footprint, courtyard, area, the other item)
        // become a custom rule, as do rules with a severity or a `disallow`; net class
        // conditions tighten the class.
        let geometric = layer.is_some()
            || cond.is_some_and(|c| condition_class(c).is_none())
            || (class_cond.is_none() && (severity != RuleSeverity::Error || disallows));
        if geometric {
            let (mut scope, with) = match cond.map(condition_scopes).transpose() {
                Ok(s) => s.unwrap_or_default(),
                Err(why) => {
                    unsupported(why, diags);
                    continue;
                }
            };
            if let Some(l) = layer {
                if !(l.ends_with(".Cu") && scope.layer.as_ref().is_none_or(|x| x == l)) {
                    unsupported(format!("layer `{l}` is not one copper layer"), diags);
                    continue;
                }
                scope.layer = Some(l.to_string());
            }
            let mut cr = CustomRule::new(name.clone(), scope);
            cr.severity = severity;
            let paired = with.is_some();
            cr.with = with;
            for con in rule.all("constraint") {
                let kind = con.value().unwrap_or("");
                let min = con.child_value("min").and_then(dru_len);
                match (kind, min) {
                    ("clearance", Some(v)) => cr.clearance = Some(v),
                    ("track_width", Some(v)) if !paired => cr.track_width = Some(v),
                    ("disallow", _) if !paired => match disallow_kinds(con) {
                        Ok(k) => cr.disallow = k,
                        Err(why) => {
                            unsupported(why, diags);
                            continue;
                        }
                    },
                    _ => {
                        let why = if paired {
                            format!("constraint `{con}` with a condition on B (only clearances relate two items)")
                        } else {
                            format!("constraint `{con}` has no cadlab counterpart in a scoped rule")
                        };
                        unsupported(why, diags);
                        continue;
                    }
                }
                if con.child_value("max").is_some() {
                    diags.push(
                        Diagnostic::info(
                            "import.rule_partial",
                            format!(
                                "custom rule `{name}`: the {kind} maximum is not checked (cadlab rules are minimums)"
                            ),
                        )
                        .with_subject(ObjectRef::Named { kind: "rule".into(), name: name.clone() }),
                    );
                }
            }
            if cr.clearance.is_some() || cr.track_width.is_some() || !cr.disallow.is_empty() {
                k.custom.push(cr);
            }
            continue;
        }
        let class = class_cond;
        for con in rule.all("constraint") {
            let Some(kind) = con.value() else { continue };
            let min = con.child_value("min").and_then(dru_len);
            let field = match (&class, kind) {
                (None, "clearance") => Some("clearance"),
                (None, "track_width") => Some("min_track_width"),
                (None, "hole_size") => Some("min_drill"),
                (None, "annular_width") => Some("min_annular_ring"),
                (None, "hole_to_hole") => Some("hole_to_hole"),
                (None, "edge_clearance") => Some("copper_to_edge"),
                (Some(_), "track_width" | "clearance") => Some(kind),
                _ => None,
            };
            let (Some(field), Some(min)) = (field, min) else {
                unsupported(format!("constraint `{}` has no cadlab counterpart", con), diags);
                continue;
            };
            match &class {
                None => {
                    let e = k.rules.entry(field.to_string()).or_insert(min);
                    *e = (*e).max(min);
                }
                Some(cl) => {
                    let idx = match k.classes.iter().position(|(n, _)| n == cl) {
                        Some(i) => i,
                        None => {
                            k.classes.push((cl.clone(), NetClass::default()));
                            k.classes.len() - 1
                        }
                    };
                    let nc = &mut k.classes[idx].1;
                    let slot = if field == "track_width" { &mut nc.track_width } else { &mut nc.clearance };
                    if slot.is_none_or(|v| v < min) {
                        *slot = Some(min);
                    }
                }
            }
        }
    }
    Ok(())
}

/// `*` / `?` wildcard match (KiCad net class patterns).
pub fn glob_match(pattern: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Applies board rules and net classes. Class values equal to the (new) board default are left
/// unset, so the class inherits them.
pub fn apply(p: &mut Project, k: &KicadRules) -> (RulesReport, Vec<Diagnostic>) {
    let mut report = RulesReport::default();
    let diags = Vec::new();
    let mut rules: Rules = p.board().rules.clone();
    for (f, v) in &k.rules {
        if let Some(slot) = rules.length_mut(f) {
            *slot = *v;
            report.rules.insert(f.clone(), *v);
        }
    }
    p.board_mut().rules = rules.clone();
    p.board_mut().custom_rules = k.custom.clone();
    for (name, nc) in &k.classes {
        let keep = |v: Option<Nm>, d: Nm| v.filter(|v| *v != d);
        let old = p.circuit().netclasses.get(name);
        let class = NetClass {
            description: old.and_then(|c| c.description.clone()),
            impedance: old.and_then(|c| c.impedance),
            diff_impedance: old.and_then(|c| c.diff_impedance),
            track_width: keep(nc.track_width, rules.track_width),
            clearance: keep(nc.clearance, rules.clearance),
            via_drill: keep(nc.via_drill, rules.via_drill),
            via_diameter: keep(nc.via_diameter, rules.via_diameter),
            diff_pair_width: keep(nc.diff_pair_width, rules.track_width),
            diff_pair_gap: keep(nc.diff_pair_gap, rules.clearance),
        };
        p.circuit_mut().netclasses.insert(name.clone(), class);
        report.netclasses.push(name.clone());
    }
    (report, diags)
}

/// Gives circuit nets the class of the first matching pattern. `kicad_names` maps cadlab net
/// names back to the KiCad names (patterns are written against those).
pub fn assign(
    p: &mut Project,
    k: &KicadRules,
    kicad_names: &BTreeMap<String, String>,
    report: &mut RulesReport,
    diags: &mut Vec<Diagnostic>,
) {
    if k.patterns.is_empty() {
        return;
    }
    let mut unknown: BTreeMap<String, usize> = BTreeMap::new();
    let names: Vec<String> = p.circuit().nets.keys().cloned().collect();
    for n in names {
        let kicad = kicad_names.get(&n).cloned().unwrap_or_else(|| n.clone());
        let candidates = [kicad.clone(), format!("/{n}"), n.clone()];
        let Some((_, class)) = k.patterns.iter().find(|(pat, _)| candidates.iter().any(|c| glob_match(pat, c))) else {
            continue;
        };
        let class = if class == "Default" {
            None
        } else if p.circuit().netclasses.contains_key(class) {
            Some(class.clone())
        } else {
            *unknown.entry(class.clone()).or_default() += 1;
            continue;
        };
        let net = p.circuit_mut().nets.get_mut(&n).expect("net exists");
        if net.class != class {
            net.class = class;
            report.assigned += 1;
        }
    }
    for (c, n) in unknown {
        diags.push(
            Diagnostic::warning(
                "import.unknown_netclass",
                format!("{n} net(s) are assigned to net class `{c}`, which the project file does not define"),
            )
            .with_subject(ObjectRef::Named { kind: "netclass".into(), name: c.clone() })
            .with_hint(format!("define it with `netclass.set {c}` and assign the nets with `net.set`")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_and_conditions() {
        assert!(glob_match("/USB*", "/USB_D+"));
        assert!(glob_match("GND", "GND"));
        assert!(!glob_match("GND", "AGND"));
        assert!(glob_match("?GND", "AGND"));
        assert!(glob_match("*", ""));
        assert_eq!(condition_class("A.NetClass == 'power'").as_deref(), Some("power"));
        assert_eq!(condition_class("A.hasNetclass(\"hv\")").as_deref(), Some("hv"));
        assert_eq!(condition_class("A.Type == 'Via'"), None);
        assert_eq!(dru_len("0.4mm"), Some(Nm::from_um(400)));
        assert_eq!(dru_len("8mil"), Some(Nm::from_mil(8)));
        assert_eq!(dru_len("0.2"), Some(Nm::from_um(200)));
    }

    #[test]
    fn severities_disallow_and_conditions_on_b() {
        let dru = r#"(version 1)
(rule soft (severity warning) (condition "A.Type == 'Track' && B.Type == 'Pad' && B.Layer == 'F.Cu'") (constraint clearance (min 0.1mm)))
(rule off (severity ignore) (constraint clearance (min 0.3mm)))
(rule excl (severity exclusion) (condition "A.memberOfFootprint('J1')") (constraint clearance (min 0.2mm)))
(rule novias (condition "A.insideArea('rf')") (constraint disallow via track))
(rule both (condition "A.intersectsArea('x') && B.Type == 'Via'") (constraint track_width (min 0.3mm)))
(rule texts (condition "A.intersectsArea('x')") (constraint disallow text))
(rule cls (severity warning) (condition "A.NetClass == 'HV'") (constraint clearance (min 1mm)))
(rule mixed (condition "A.Net == B.Net") (constraint clearance (min 1mm)))"#;
        let (k, diags) = parse(None, Some(dru)).unwrap();
        let names: Vec<&str> = k.custom.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["soft", "off", "excl", "novias"]);
        let soft = &k.custom[0];
        assert_eq!(soft.severity, RuleSeverity::Warning);
        assert_eq!(soft.scope.kinds, [ItemKind::Track]);
        let with = soft.with.as_ref().unwrap();
        assert_eq!((with.kinds.as_slice(), with.layer.as_deref()), ([ItemKind::Pad].as_slice(), Some("F.Cu")));
        assert_eq!(k.custom[1].severity, RuleSeverity::Ignore);
        assert_eq!(k.custom[1].scope, RuleScope::default(), "no condition: everything");
        assert!(!k.rules.contains_key("clearance"), "an ignored rule does not tighten the board");
        assert_eq!(k.custom[2].severity, RuleSeverity::Ignore, "exclusions are not reported");
        let nv = &k.custom[3];
        assert_eq!(nv.disallow, [ItemKind::Track, ItemKind::Via]);
        assert_eq!(nv.scope.touches_area.as_deref(), Some("rf"));
        let unsupported: Vec<&str> = diags
            .iter()
            .filter(|d| d.code == "import.rule_unsupported")
            .map(|d| d.message.split('`').nth(1).unwrap_or(""))
            .collect();
        assert_eq!(unsupported, ["both", "texts", "cls", "mixed"]);
        assert!(k.classes.is_empty());
    }

    #[test]
    fn project_and_custom_rules() {
        let pro = r#"{"board": {"design_settings": {"defaults": {"silk_line_width": 0.12},
            "rules": {"min_clearance": 0.15, "min_track_width": 0.127, "min_via_annular_width": 0.1,
                      "min_through_hole_diameter": 0.25, "min_hole_to_hole": 0.0, "min_copper_edge_clearance": 0.4}}},
            "net_settings": {"classes": [
                {"name": "Default", "clearance": 0.2, "track_width": 0.25, "via_diameter": 0.6, "via_drill": 0.3},
                {"name": "Power", "clearance": 0.2, "track_width": 0.5, "via_diameter": 0.8, "via_drill": 0.4,
                 "diff_pair_width": null}],
             "netclass_patterns": [{"netclass": "Power", "pattern": "+*V"}],
             "netclass_assignments": {"/VBAT": "Power"}}}"#;
        let dru = "(version 1)\n# comment\n(rule \"w\" (condition \"A.NetClass == 'Power'\") (constraint track_width (min 0.6mm)))\n(rule g (constraint hole_to_hole (min 0.3mm)))\n(rule x (condition \"A.Type == 'Pad' && A.memberOfFootprint('Lib:QFN*')\") (constraint clearance (min 0.1mm)))\n(rule y (condition \"B.Type == 'Via'\") (constraint clearance (min 1mm)))";
        let (k, diags) = parse(Some(pro), Some(dru)).unwrap();
        assert_eq!(k.rules["clearance"], Nm::from_um(200));
        assert_eq!(k.rules["min_silk_width"], Nm::from_um(120));
        assert_eq!(k.rules["hole_to_hole"], Nm::from_um(300));
        assert_eq!(k.classes[0].1.track_width, Some(Nm::from_um(600)), "the larger of class and rule");
        assert_eq!(k.patterns[0], ("/VBAT".to_string(), "Power".to_string()));
        let codes: Vec<&str> = diags.iter().map(|d| d.code.as_ref()).collect();
        assert_eq!(codes, ["import.rule_zero"]);
        // The geometric rule is a custom rule on QFN pads.
        assert_eq!(k.custom.len(), 2);
        let x = &k.custom[0];
        assert_eq!((x.name.as_str(), x.clearance, x.track_width), ("x", Some(Nm::from_um(100)), None));
        assert_eq!(x.scope.kinds, [ItemKind::Pad]);
        assert_eq!(x.scope.footprint.as_deref(), Some("QFN*"), "library prefix dropped");
        // A condition on B only: anything against vias.
        let y = &k.custom[1];
        assert_eq!(y.scope, RuleScope::default());
        assert_eq!(y.with.as_ref().map(|w| w.kinds.clone()), Some(vec![ItemKind::Via]));

        let mut p = Project::new("t");
        let (rep, _) = apply(&mut p, &k);
        assert_eq!(rep.netclasses, ["Power"]);
        let c = &p.circuit().netclasses["Power"];
        assert_eq!(c.clearance, None, "equal to the board clearance: inherited");
        assert_eq!(c.via_drill, Some(Nm::from_um(400)));
        assert_eq!(p.board().rules.copper_to_edge, Nm::from_um(400));
    }
}
