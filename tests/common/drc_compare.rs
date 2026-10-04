//! KiCad DRC vs cadlab DRC, finding by finding (docs/TESTING.md, "Cross-checks"): shared by the
//! cross-check boards (`tests/drc_crosscheck.rs`) and the open-source corpus (`tests/corpus.rs`).
//!
//! KiCad violation types are mapped to cadlab codes ([`TYPE_MAP`]) and the two sets are matched
//! one to one:
//!
//! - **rule:** the mapped code;
//! - **items:** KiCad item UUIDs map back to cadlab objects through a UUID table (the exporter's
//!   `KicadExport::uuids` or the importer's `BoardImportReport::uuids`); KiCad's items (minus
//!   board edges) must be a subset of the cadlab finding's subjects (cadlab also names e.g. the
//!   keep-out area);
//! - **value:** the measured distance or size (KiCad's "actual 0.1000 mm") within
//!   [`VALUE_TOL`];
//! - **location:** KiCad's JSON gives no marker position, only item anchors, so every KiCad item
//!   anchor, converted with `Frame::from_kicad`, must be within [`POS_TOL`] of the cadlab object
//!   (pad center, track start, via center, footprint origin).
//!
//! Every unmatched finding on either side must be covered by an [`Allow`] entry with a reason.
//! KiCad runs as an external process (DECISIONS D7).

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cadlab::geom::Point;
use cadlab::kicad_pcb::Frame;
use cadlab::model::Project;
use cadlab::units::Nm;
use cadlab::{Diagnostic, ObjectRef};
use serde_json::Value;

use crate::common::oracle;

/// KiCad violation type → the cadlab codes it corresponds to.
pub const TYPE_MAP: &[(&str, &[&str])] = &[
    ("clearance", &["drc.clearance"]),
    ("shorting_items", &["drc.short"]),
    ("tracks_crossing", &["drc.short"]),
    ("track_width", &["drc.track_width", "drc.track_width_class"]),
    ("annular_width", &["drc.via_annular_ring", "drc.pad_annular_ring"]),
    ("drill_out_of_range", &["drc.via_drill", "drc.pad_drill"]),
    ("hole_to_hole", &["drc.hole_to_hole"]),
    ("holes_co_located", &["drc.hole_to_hole"]),
    ("missing_footprint", &["drc.unplaced"]),
    ("copper_edge_clearance", &["drc.copper_to_edge", "drc.outside_board"]),
    ("courtyards_overlap", &["drc.courtyard_overlap"]),
    ("items_not_allowed", &["drc.keepout"]),
    ("silk_over_copper", &["drc.silk_over_pad"]),
    ("unconnected_items", &["drc.unrouted"]),
];

/// Measured values (mm) may differ by this much: cadlab approximates arcs outward by up to
/// 1 µm per shape (`drc::TOLERANCE` = 2 µm), KiCad prints 0.1 µm.
pub const VALUE_TOL: f64 = 0.0025;

/// KiCad item anchors must map back to cadlab's positions within this (nm). The frame offset
/// is exact (a whole number of millimeters for cadlab's exports, KiCad's own coordinates for
/// imported boards), so the mapping is exact.
pub const POS_TOL: i64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Kicad,
    Cadlab,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Kicad => "KiCad",
            Side::Cadlab => "cadlab",
        }
    }
}

/// A known semantic difference: a finding on one side that the other side does not have.
#[derive(Clone, Debug)]
pub struct Allow<'a> {
    /// Board (case or project) name, or `None` for any board.
    pub case: Option<&'a str>,
    pub side: Side,
    /// KiCad type or cadlab code.
    pub rule: &'a str,
    /// Only findings for which this holds.
    pub when: Option<fn(&Finding) -> bool>,
    /// At most this many findings per board (`None`: any number).
    pub max: Option<usize>,
    /// Only findings naming one of these objects (empty: any).
    pub involving: &'a [String],
    pub reason: &'a str,
}

/// The finding names a footprint text field (`text:U1/Reference`).
pub fn names_reference(f: &Finding) -> bool {
    f.items.iter().any(|l| l.starts_with("text:"))
}

/// The finding is between two pads of the same footprint (`U2.3` and `U2.17`).
pub fn same_footprint_pads(f: &Finding) -> bool {
    let owners: BTreeSet<Option<&str>> = f.items.iter().map(|l| l.split_once('.').map(|(r, _)| r)).collect();
    f.items.len() == 2 && owners.len() == 1 && !owners.contains(&None)
}

/// Allowances shared by every comparison (the cross-check boards and the corpus).
pub const ALLOW: &[Allow<'static>] = &[
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "clearance",
        when: Some(same_footprint_pads),
        max: None,
        involving: &[],
        reason: "cadlab does not check pads of one footprint against each other (docs/BOARD.md, drc.clearance): \
                 their spacing is the footprint's, not the layout's; KiCad checks them (e.g. QFN pins 0.16 mm from \
                 the exposed pad against a 0.2 mm clearance)",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "lib_footprint_issues",
        when: None,
        max: None,
        involving: &[],
        reason: "footprints are embedded in the board; there is no `cadlab` KiCad footprint library to configure \
                 (cadlab never ships or converts KiCad libraries, D7)",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "track_dangling",
        when: None,
        max: None,
        involving: &[],
        reason: "cadlab has no dangling-end rule: connectivity is checked between pads and vias (drc.unrouted); \
                 the defect boards add stubs that end in free space",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "via_dangling",
        when: None,
        max: None,
        involving: &[],
        reason: "same as track_dangling: a via joined to its net only through a pour on one layer is connected \
                 for cadlab",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "silk_over_copper",
        when: Some(names_reference),
        max: None,
        involving: &[],
        reason: "reference designators are generated by cadlab's legend writer (the export puts KiCad's Reference \
                 field at the same place) and clipped at mask openings there; they are not design objects, so \
                 cadlab's silk_over_pad checks footprint and board silkscreen only",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "silk_overlap",
        when: None,
        max: None,
        involving: &[],
        reason: "cadlab has no silkscreen-to-silkscreen clearance rule",
    },
    Allow {
        case: None,
        side: Side::Cadlab,
        rule: "drc.via_size_class",
        when: None,
        max: None,
        involving: &[],
        reason: "KiCad uses net class via sizes only as defaults for new vias and never checks existing ones; cadlab \
                 warns when a via is smaller than its class asks for (D26)",
    },
];

#[derive(Debug)]
pub struct Finding {
    /// cadlab code (KiCad types mapped through TYPE_MAP; unmapped types kept as is).
    pub code: String,
    /// KiCad type, for KiCad findings.
    pub kicad_type: Option<String>,
    pub items: BTreeSet<String>,
    /// KiCad item anchors in cadlab coordinates, by label.
    pub anchors: Vec<(String, Point)>,
    pub value: Option<f64>,
    pub text: String,
}

impl Finding {
    /// The rule an allowance names: the KiCad type or the cadlab code.
    pub fn rule(&self, side: Side) -> &str {
        match side {
            Side::Kicad => self.kicad_type.as_deref().unwrap_or_default(),
            Side::Cadlab => self.code.as_str(),
        }
    }
}

/// The first `<number>mm` after `marker` in `s`.
pub fn mm_after(s: &str, marker: &str) -> Option<f64> {
    let rest = &s[s.find(marker)? + marker.len()..];
    let tok = rest.split_whitespace().find(|t| t.trim_end_matches([',', ')', ';']).ends_with("mm"))?;
    tok.trim_end_matches([',', ')', ';']).trim_end_matches("mm").trim().parse().ok()
}

/// KiCad's measured value: `... actual 0.1562 mm)`.
pub fn kicad_actual(desc: &str) -> Option<f64> {
    let rest = &desc[desc.find("actual ")? + 7..];
    rest.split_whitespace().next()?.parse().ok()
}

/// The measured quantity in a cadlab message.
fn cadlab_value(d: &Diagnostic) -> Option<f64> {
    let m = d.message.as_str();
    let marker = match d.code.as_ref() {
        "drc.clearance" | "drc.hole_to_hole" => " are ",
        "drc.copper_to_edge" => " is ",
        "drc.track_width" | "drc.track_width_class" => " is ",
        "drc.via_annular_ring" | "drc.pad_annular_ring" => "annular ring ",
        "drc.via_drill" | "drc.pad_drill" => "drill ",
        _ => return None,
    };
    mm_after(m, marker)
}

/// cadlab label of a diagnostic subject, or `None` for nets and layers.
fn label(r: &ObjectRef) -> Option<String> {
    match r {
        ObjectRef::Net(_) | ObjectRef::Layer(_) => None,
        // Zone islands (`zone#3@B.Cu/0`) are reported per zone by KiCad.
        ObjectRef::Name(n) if n.starts_with("zone#") => Some(n.split('@').next().unwrap().to_string()),
        r => Some(r.to_string()),
    }
}

/// cadlab's findings on `p`.
pub fn cadlab_findings(p: &Project) -> Vec<Finding> {
    findings_of(&cadlab::drc::check(p))
}

/// Findings from a list of cadlab diagnostics.
pub fn findings_of(diags: &[Diagnostic]) -> Vec<Finding> {
    diags
        .iter()
        .map(|d| Finding {
            code: d.code.to_string(),
            kicad_type: None,
            items: d.subjects.iter().filter_map(label).collect(),
            anchors: Vec::new(),
            value: cadlab_value(d),
            text: format!("{} ({:?}) {}", d.code, d.severity, d.message),
        })
        .collect()
}

/// KiCad's findings from a `pcb drc --format json` report, items mapped through `uuids`.
pub fn kicad_findings(report: &Value, uuids: &BTreeMap<String, String>, frame: &Frame) -> Vec<Finding> {
    let entry = |ty: &str, sev: &str, desc: &str, items: &[Value]| {
        let mut labels = BTreeSet::new();
        let mut anchors = Vec::new();
        let mut names = Vec::new();
        for i in items {
            let l = uuids
                .get(i["uuid"].as_str().unwrap_or_default())
                .cloned()
                .unwrap_or_else(|| format!("?{}", i["description"].as_str().unwrap_or_default()));
            let (x, y) = (i["pos"]["x"].as_f64().unwrap(), i["pos"]["y"].as_f64().unwrap());
            let at = frame.from_kicad(Nm((x * 1e6).round() as i64), Nm((y * 1e6).round() as i64));
            let desc = i["description"].as_str().unwrap_or_default();
            names.push(desc.to_string());
            // A footprint's label also names its drawings and fields (its courtyard, `Reference
            // field of U1`), anchored elsewhere: only the footprint itself is at its origin.
            if !(is_footprint_label(&l) && !desc.starts_with("Footprint ")) {
                anchors.push((l.clone(), at));
            }
            if l != "edge" {
                labels.insert(l);
            }
        }
        let code =
            TYPE_MAP.iter().find(|(t, _)| *t == ty).map(|(_, c)| c[0].to_string()).unwrap_or_else(|| ty.to_string());
        let value = kicad_actual(desc);
        Finding {
            code,
            kicad_type: Some(ty.to_string()),
            items: labels,
            anchors,
            value,
            text: format!("{ty} ({sev}) {desc} {names:?}"),
        }
    };
    let mut out = Vec::new();
    for v in report["violations"].as_array().unwrap() {
        out.push(entry(
            v["type"].as_str().unwrap(),
            v["severity"].as_str().unwrap_or_default(),
            v["description"].as_str().unwrap_or_default(),
            v["items"].as_array().unwrap(),
        ));
    }
    for v in report["unconnected_items"].as_array().unwrap() {
        out.push(entry("unconnected_items", "error", "", v["items"].as_array().unwrap()));
    }
    // With `--schematic-parity`: footprints of schematic symbols missing on the board
    // (`Missing footprint C8 (100nF)`, no items) are cadlab's unplaced components. Other parity
    // findings compare the board with KiCad's schematic, which cadlab only sees as a netlist.
    for v in report["schematic_parity"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let desc = v["description"].as_str().unwrap_or_default();
        if v["type"].as_str() == Some("missing_footprint")
            && let Some(r) = desc.strip_prefix("Missing footprint ").and_then(|s| s.split_whitespace().next())
        {
            let mut f = entry("missing_footprint", v["severity"].as_str().unwrap_or_default(), desc, &[]);
            f.items.insert(r.to_string());
            out.push(f);
        }
    }
    out
}

/// Whether a KiCad finding and a cadlab finding are the same.
fn same(k: &Finding, c: &Finding) -> bool {
    let ty = k.kicad_type.as_deref().unwrap_or_default();
    let codes: &[&str] = TYPE_MAP.iter().find(|(t, _)| *t == ty).map(|(_, c)| *c).unwrap_or(&[]);
    codes.contains(&c.code.as_str()) && !k.items.is_empty() && k.items.is_subset(&c.items)
}

/// Whether a label names a footprint (`U1`) rather than a pad, track, via, zone, text...
fn is_footprint_label(l: &str) -> bool {
    !l.contains(['.', '#', ':', '?']) && l != "edge"
}

/// Positions a labelled cadlab object may be at, for the anchor check (several for pads: a
/// footprint may repeat a pad number, e.g. the shield tabs of a USB-C receptacle).
fn anchor_of(p: &Project, pads: &BTreeMap<(String, String), Vec<Point>>, label: &str) -> Vec<Point> {
    let b = p.board();
    if let Some(id) = label.strip_prefix("track#") {
        return b.tracks.iter().filter(|t| t.id.0.to_string() == id).map(|t| t.start).collect();
    }
    if let Some(id) = label.strip_prefix("via#") {
        return b.vias.iter().filter(|v| v.id.0.to_string() == id).map(|v| v.at).collect();
    }
    if let Some(pf) = b.footprints.get(label) {
        return vec![pf.at];
    }
    let Some((r, n)) = label.split_once('.') else { return Vec::new() };
    pads.get(&(r.to_string(), n.to_string())).cloned().unwrap_or_default()
}

/// Runs `kicad-cli pcb drc` on `pcb` (zones refilled, every severity) and returns the JSON
/// report, written next to `out`.
pub fn kicad_drc(cli: &Path, pcb: &Path, out: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["pcb", "drc", "--format", "json", "--severity-all", "--refill-zones"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["-o", out.to_str().unwrap(), pcb.to_str().unwrap()]);
    oracle::run(cli, &args);
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

/// What one board's comparison found.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Matched pairs.
    pub matched: usize,
    /// Findings reported by both tools, by KiCad type.
    pub matched_by_rule: BTreeMap<String, usize>,
    /// Unmatched findings covered by an allowance: (side, rule) → count.
    pub allowed: BTreeMap<(&'static str, String), usize>,
    /// Unmatched findings without an allowance, or other mismatches: (side, rule) → count.
    pub unexplained: BTreeMap<(&'static str, String), usize>,
    /// KiCad findings, cadlab findings.
    pub totals: (usize, usize),
    /// KiCad findings repeating, for another segment, a pair a cadlab finding names
    /// ([`PER_SEGMENT`]).
    pub per_segment: usize,
    /// cadlab findings of a rule KiCad reported [`KICAD_REPORT_CAP`] times (unverifiable).
    pub beyond_cap: usize,
}

/// KiCad's DRC report stops at this many findings of most types (observed with KiCad 10:
/// `lib_footprint_issues`, `track_width` and silkscreen types stop at exactly 199 on large boards;
/// `clearance` can go past it). A type reported exactly this many times is taken as truncated.
pub const KICAD_REPORT_CAP: usize = 199;

/// KiCad types reported once per segment of a drawing where cadlab reports once per owner.
pub const PER_SEGMENT: &[&str] = &["silk_over_copper"];

/// One board to compare.
pub struct Board<'a> {
    /// Case or project name (allowances may be limited to it).
    pub name: &'a str,
    /// Description for the log.
    pub what: &'a str,
    /// cadlab codes that must be reported.
    pub expect: &'a [&'a str],
    /// Log every matched pair (small boards) or only a summary.
    pub verbose: bool,
}

/// Compares KiCad's DRC report with cadlab's findings `ours` on `p`. `uuids` maps KiCad UUIDs to
/// `p`'s objects and `frame` KiCad coordinates to `p`'s. `used[i]` counts the findings
/// `allow[i]` covered; differences are appended to `problems`.
#[allow(clippy::too_many_arguments)]
pub fn compare(
    b: &Board,
    p: &Project,
    ours: &[Finding],
    report: &Value,
    uuids: &BTreeMap<String, String>,
    frame: &Frame,
    allow: &[Allow],
    used: &mut [usize],
    problems: &mut Vec<String>,
) -> Outcome {
    let theirs = kicad_findings(report, uuids, frame);
    let mut pads: BTreeMap<(String, String), Vec<Point>> = BTreeMap::new();
    for pp in cadlab::board::placed_pads(p) {
        pads.entry((pp.refdes, pp.number)).or_default().push(pp.center);
    }
    let mut out = Outcome { totals: (theirs.len(), ours.len()), ..Default::default() };
    let mut log = vec![format!("=== {} ({})", b.name, b.what)];
    let mut board_used = vec![0usize; allow.len()];
    let mut fail = |log: &mut Vec<String>, out: &mut Outcome, side: &'static str, rule: &str, msg: String| {
        log.push(format!("  !! {msg}"));
        problems.push(format!("{}: {msg}", b.name));
        *out.unexplained.entry((side, rule.to_string())).or_default() += 1;
    };

    // Every expected code is reported by cadlab.
    for code in b.expect {
        if !ours.iter().any(|f| f.code == *code) {
            fail(&mut log, &mut out, "cadlab", code, format!("cadlab did not report {code}"));
        }
    }

    let allowed = |side: Side, f: &Finding, board_used: &mut [usize]| -> Option<usize> {
        let rule = f.rule(side);
        let i = allow.iter().enumerate().position(|(i, a)| {
            a.side == side
                && a.rule == rule
                && a.case.is_none_or(|c| c == b.name)
                && a.when.is_none_or(|w| w(f))
                && a.max.is_none_or(|m| board_used[i] < m)
                && (a.involving.is_empty() || f.items.iter().any(|x| a.involving.contains(x)))
        })?;
        board_used[i] += 1;
        Some(i)
    };
    let why = |i: usize, board_used: &[usize]| -> String {
        if board_used[i] == 1 { format!("\n      because: {}", allow[i].reason) } else { String::new() }
    };

    // One-to-one matching, KiCad findings first.
    let mut taken = vec![false; ours.len()];
    for k in &theirs {
        for (label, at) in &k.anchors {
            let dist = |w: &Point| (w.x.0 - at.x.0).abs().max((w.y.0 - at.y.0).abs());
            if let Some(want) = anchor_of(p, &pads, label).into_iter().min_by_key(dist)
                && dist(&want) > POS_TOL
            {
                fail(
                    &mut log,
                    &mut out,
                    "KiCad",
                    "anchor",
                    format!("KiCad puts {label} at {at:?}, cadlab at {want:?}"),
                );
            }
        }
        let hit = ours.iter().enumerate().position(|(i, c)| !taken[i] && same(k, c));
        match hit {
            Some(i) => {
                taken[i] = true;
                let c = &ours[i];
                out.matched += 1;
                *out.matched_by_rule.entry(k.rule(Side::Kicad).to_string()).or_default() += 1;
                if b.verbose {
                    log.push(format!("  ok  {}  <->  {}", c.text, k.text));
                }
                if let (Some(a), Some(v)) = (k.value, c.value)
                    && (a - v).abs() > VALUE_TOL
                {
                    fail(
                        &mut log,
                        &mut out,
                        "both",
                        &c.code,
                        format!("measured {v} mm by cadlab, {a} mm by KiCad: {} / {}", c.text, k.text),
                    );
                }
            }
            // KiCad reports silkscreen over copper per silkscreen segment, cadlab per footprint
            // silkscreen and pad: another segment of a pair cadlab reports is the same defect.
            None if PER_SEGMENT.contains(&k.rule(Side::Kicad)) && ours.iter().any(|c| same(k, c)) => {
                out.per_segment += 1;
                if b.verbose {
                    log.push(format!("  same defect (per segment)  {}", k.text));
                }
            }
            None => match allowed(Side::Kicad, k, &mut board_used) {
                Some(a) => {
                    *out.allowed.entry(("KiCad", k.rule(Side::Kicad).to_string())).or_default() += 1;
                    if b.verbose || board_used[a] == 1 {
                        log.push(format!("  allowed (KiCad only)  {}{}", k.text, why(a, &board_used)));
                    }
                }
                None => {
                    let rule = k.rule(Side::Kicad).to_string();
                    fail(&mut log, &mut out, "KiCad", &rule, format!("only KiCad reports: {}", k.text));
                }
            },
        }
    }
    // KiCad's report stops at KICAD_REPORT_CAP findings of a type: past it, cadlab's findings of
    // that rule cannot be checked.
    let capped: BTreeSet<&str> = TYPE_MAP
        .iter()
        .filter(|(t, _)| theirs.iter().filter(|k| k.kicad_type.as_deref() == Some(*t)).count() == KICAD_REPORT_CAP)
        .flat_map(|(_, codes)| codes.iter().copied())
        .collect();
    for (i, c) in ours.iter().enumerate() {
        if taken[i] {
            continue;
        }
        if capped.contains(c.code.as_str()) {
            out.beyond_cap += 1;
            continue;
        }
        match allowed(Side::Cadlab, c, &mut board_used) {
            Some(a) => {
                *out.allowed.entry(("cadlab", c.code.clone())).or_default() += 1;
                if b.verbose || board_used[a] == 1 {
                    log.push(format!("  allowed (cadlab only)  {}{}", c.text, why(a, &board_used)));
                }
            }
            None => {
                let code = c.code.clone();
                fail(&mut log, &mut out, "cadlab", &code, format!("only cadlab reports: {} {:?}", c.text, c.items));
            }
        }
    }
    for (u, n) in used.iter_mut().zip(&board_used) {
        *u += n;
    }
    if !b.verbose {
        log.push(format!(
            "  {} KiCad / {} cadlab findings, {} matched {:?}, allowed {:?}",
            out.totals.0, out.totals.1, out.matched, out.matched_by_rule, out.allowed
        ));
    }
    eprintln!("{}", log.join("\n"));
    out
}

/// Every allowlist entry must have been used.
pub fn unused_entries(allow: &[Allow], used: &[usize], problems: &mut Vec<String>) {
    for (a, u) in allow.iter().zip(used) {
        if *u == 0 {
            problems.push(format!("unused allowlist entry {:?} {} ({:?})", a.side, a.rule, a.case));
        }
    }
}

#[test]
fn value_parsing() {
    assert_eq!(mm_after("track#3 (net 3V3) and U1.4 (no net) are 0.1mm apart on F.Cu", " are "), Some(0.1));
    assert_eq!(mm_after("via#3 annular ring 0.075mm (0.45mm pad, 0.3mm drill)", "annular ring "), Some(0.075));
    assert_eq!(kicad_actual("Clearance violation (clearance 0.2000 mm; actual 0.1562 mm)"), Some(0.1562));
    assert_eq!(kicad_actual("Tracks crossing"), None);
}
