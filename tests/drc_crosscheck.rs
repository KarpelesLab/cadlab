//! Cross-check: cadlab's DRC against KiCad's on the same boards (docs/TESTING.md, "Cross-checks").
//!
//! Each board of `crosscheck::cases()` (a clean routed board with a pour, then one deliberate
//! violation per rule) is checked by `cadlab::drc::check` and by `kicad-cli pcb drc` on its
//! `.kicad_pcb` export. KiCad violation types are mapped to cadlab codes ([`TYPE_MAP`]) and the
//! two sets are compared finding by finding:
//!
//! - **rule:** the mapped code;
//! - **items:** KiCad item UUIDs map back to cadlab objects through the exporter's UUID table;
//!   KiCad's items (minus board edges) must be a subset of the cadlab finding's subjects (cadlab
//!   also names e.g. the keep-out area);
//! - **value:** the measured distance or size (KiCad's "actual 0.1000 mm") within
//!   [`VALUE_TOL`];
//! - **location:** KiCad's JSON gives no marker position, only item anchors, so every KiCad item
//!   anchor, converted with `Frame::from_kicad`, must be within [`POS_TOL`] of the cadlab object
//!   (pad center, track start, via center, footprint origin).
//!
//! Every unmatched finding on either side must be covered by an [`ALLOW`] entry with a reason;
//! every entry must be used. Runs only with `CADLAB_ORACLES=1` (KiCad is an external process,
//! DECISIONS D7). Reports are kept in `$CARGO_TARGET_TMPDIR/drc_crosscheck/<case>/`.

mod common;
mod crosscheck;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cadlab::geom::Point;
use cadlab::kicad_pcb::Frame;
use cadlab::model::Project;
use cadlab::units::Nm;
use cadlab::{Diagnostic, ObjectRef};
use common::oracle::{self, Oracle};
use serde_json::Value;

/// KiCad violation type → the cadlab codes it corresponds to.
const TYPE_MAP: &[(&str, &[&str])] = &[
    ("clearance", &["drc.clearance"]),
    ("shorting_items", &["drc.short"]),
    ("tracks_crossing", &["drc.short"]),
    ("track_width", &["drc.track_width", "drc.track_width_class"]),
    ("annular_width", &["drc.via_annular_ring", "drc.pad_annular_ring"]),
    ("drill_out_of_range", &["drc.via_drill", "drc.pad_drill"]),
    ("hole_to_hole", &["drc.hole_to_hole"]),
    ("copper_edge_clearance", &["drc.copper_to_edge", "drc.outside_board"]),
    ("courtyards_overlap", &["drc.courtyard_overlap"]),
    ("items_not_allowed", &["drc.keepout"]),
    ("silk_over_copper", &["drc.silk_over_pad"]),
    ("unconnected_items", &["drc.unrouted"]),
];

/// Measured values (mm) may differ by this much: cadlab approximates arcs outward by up to
/// 1 µm per shape (`drc::TOLERANCE` = 2 µm), KiCad prints 0.1 µm.
const VALUE_TOL: f64 = 0.0025;

/// KiCad item anchors must map back to cadlab's positions within this (nm). The frame offset
/// is a whole number of millimeters, so the mapping is exact.
const POS_TOL: i64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Kicad,
    Cadlab,
}

/// A known semantic difference: a finding on one side that the other side does not have.
struct Allow {
    /// Case name, or `None` for any board.
    case: Option<&'static str>,
    side: Side,
    /// KiCad type or cadlab code.
    rule: &'static str,
    /// Only findings for which this holds.
    when: Option<fn(&Finding) -> bool>,
    reason: &'static str,
}

/// The finding names a footprint text field (`text:U1/Reference`).
fn names_reference(f: &Finding) -> bool {
    f.items.iter().any(|l| l.starts_with("text:"))
}

/// The finding is between two pads of the same footprint (`U2.3` and `U2.17`).
fn same_footprint_pads(f: &Finding) -> bool {
    let owners: BTreeSet<Option<&str>> = f.items.iter().map(|l| l.split_once('.').map(|(r, _)| r)).collect();
    f.items.len() == 2 && owners.len() == 1 && !owners.contains(&None)
}

const ALLOW: &[Allow] = &[
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "clearance",
        when: Some(same_footprint_pads),
        reason: "cadlab does not check pads of one footprint against each other (docs/BOARD.md, drc.clearance): \
                 their spacing is the footprint's, not the layout's; KiCad checks them (e.g. QFN pins 0.16 mm from \
                 the exposed pad against a 0.2 mm clearance)",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "lib_footprint_issues",
        when: None,
        reason: "footprints are embedded in the board; there is no `cadlab` KiCad footprint library to configure \
                 (cadlab never ships or converts KiCad libraries, D7)",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "track_dangling",
        when: None,
        reason: "cadlab has no dangling-end rule: connectivity is checked between pads and vias (drc.unrouted); \
                 the defect boards add stubs that end in free space",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "via_dangling",
        when: None,
        reason: "same as track_dangling: a via joined to its net only through a pour on one layer is connected \
                 for cadlab",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "silk_over_copper",
        when: Some(names_reference),
        reason: "reference designators are generated by cadlab's legend writer (the export puts KiCad's Reference \
                 field at the same place) and clipped at mask openings there; they are not design objects, so \
                 cadlab's silk_over_pad checks footprint and board silkscreen only",
    },
    Allow {
        case: None,
        side: Side::Kicad,
        rule: "silk_overlap",
        when: None,
        reason: "cadlab has no silkscreen-to-silkscreen clearance rule",
    },
    Allow {
        case: None,
        side: Side::Cadlab,
        rule: "drc.via_size_class",
        when: None,
        reason: "KiCad uses net class via sizes only as defaults for new vias and never checks existing ones; cadlab \
                 warns when a via is smaller than its class asks for (D26)",
    },
    Allow {
        case: Some("netless_track"),
        side: Side::Cadlab,
        rule: "drc.short",
        when: None,
        reason: "copper without a net touching a net is a short for cadlab (the design must say what each track \
                 carries); KiCad gives such a track the net it touches when it loads the board",
    },
    Allow {
        case: Some("netless_track"),
        side: Side::Kicad,
        rule: "track_width",
        when: None,
        reason: "KiCad assigned the netless 0.25 mm track to GND (see the drc.short entry), whose power class asks \
                 for 0.4 mm; cadlab checks the class width of a track's own net, and it has none",
    },
];

#[derive(Debug)]
struct Finding {
    /// cadlab code (KiCad types mapped through TYPE_MAP; unmapped types kept as is).
    code: String,
    /// KiCad type, for KiCad findings.
    kicad_type: Option<String>,
    items: BTreeSet<String>,
    /// KiCad item anchors in cadlab coordinates, by label.
    anchors: Vec<(String, Point)>,
    value: Option<f64>,
    text: String,
}

/// The first `<number>mm` after `marker` in `s`.
fn mm_after(s: &str, marker: &str) -> Option<f64> {
    let rest = &s[s.find(marker)? + marker.len()..];
    let tok = rest.split_whitespace().find(|t| t.trim_end_matches([',', ')', ';']).ends_with("mm"))?;
    tok.trim_end_matches([',', ')', ';']).trim_end_matches("mm").trim().parse().ok()
}

/// KiCad's measured value: `... actual 0.1562 mm)`.
fn kicad_actual(desc: &str) -> Option<f64> {
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

fn cadlab_findings(p: &Project) -> Vec<Finding> {
    cadlab::drc::check(p)
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

fn kicad_findings(report: &Value, uuids: &BTreeMap<String, String>, frame: &Frame) -> Vec<Finding> {
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
            names.push(i["description"].as_str().unwrap_or_default().to_string());
            anchors.push((l.clone(), at));
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
    out
}

/// Whether a KiCad finding and a cadlab finding are the same.
fn same(k: &Finding, c: &Finding) -> bool {
    let ty = k.kicad_type.as_deref().unwrap_or_default();
    let codes: &[&str] = TYPE_MAP.iter().find(|(t, _)| *t == ty).map(|(_, c)| *c).unwrap_or(&[]);
    codes.contains(&c.code.as_str()) && !k.items.is_empty() && k.items.is_subset(&c.items)
}

/// Position of a labelled cadlab object, for the anchor check.
fn anchor_of(p: &Project, label: &str) -> Option<Point> {
    let b = p.board();
    if let Some(id) = label.strip_prefix("track#") {
        return b.tracks.iter().find(|t| t.id.0.to_string() == id).map(|t| t.start);
    }
    if let Some(id) = label.strip_prefix("via#") {
        return b.vias.iter().find(|v| v.id.0.to_string() == id).map(|v| v.at);
    }
    if let Some(pf) = b.footprints.get(label) {
        return Some(pf.at);
    }
    let (r, n) = label.split_once('.')?;
    cadlab::board::placed_pads(p).into_iter().find(|pp| pp.refdes == r && pp.number == n).map(|pp| pp.center)
}

fn allowed(case: &str, side: Side, f: &Finding) -> Option<usize> {
    let rule = match side {
        Side::Kicad => f.kicad_type.as_deref().unwrap_or_default(),
        Side::Cadlab => f.code.as_str(),
    };
    ALLOW.iter().position(|a| {
        a.side == side && a.rule == rule && a.case.is_none_or(|c| c == case) && a.when.is_none_or(|w| w(f))
    })
}

/// Marks an allowlist entry used; its reason, the first time.
fn why(a: usize, used: &mut [bool]) -> String {
    let first = !std::mem::replace(&mut used[a], true);
    if first { format!("\n      because: {}", ALLOW[a].reason) } else { String::new() }
}

fn kicad_drc(cli: &Path, pcb: &Path) -> Value {
    let out = pcb.with_file_name("drc.json");
    oracle::run(
        cli,
        &[
            "pcb",
            "drc",
            "--format",
            "json",
            "--severity-all",
            "--refill-zones",
            "-o",
            out.to_str().unwrap(),
            pcb.to_str().unwrap(),
        ],
    );
    serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap()
}

#[test]
fn kicad_and_cadlab_drc_agree() {
    let Some(cli) = oracle::require(Oracle::KicadCli) else { return };
    let mut problems = Vec::new();
    let mut used = vec![false; ALLOW.len()];
    for case in crosscheck::cases() {
        let (_d, _r, s) = crosscheck::build(&case);
        let p = crosscheck::project(&s);
        let dir = crosscheck::keep_dir("drc_crosscheck", case.name);
        let export = cadlab::kicad_pcb::export(p, "board");
        let pcb = crosscheck::export_kicad(p, &dir);
        let report = kicad_drc(&cli, &pcb);
        let frame = Frame::of(p);
        let ours = cadlab_findings(p);
        let theirs = kicad_findings(&report, &export.uuids, &frame);
        let mut log = vec![format!("=== {} ({})", case.name, case.what)];
        macro_rules! fail {
            ($msg:expr) => {{
                let msg: String = $msg;
                log.push(format!("  !! {msg}"));
                problems.push(format!("{}: {msg}", case.name));
            }};
        }

        // Every expected code is reported by cadlab.
        for code in case.expect {
            if !ours.iter().any(|f| f.code == *code) {
                fail!(format!("cadlab did not report {code}"));
            }
        }

        // One-to-one matching, KiCad findings first.
        let mut taken = vec![false; ours.len()];
        for k in &theirs {
            for (label, at) in &k.anchors {
                if let Some(want) = anchor_of(p, label) {
                    let d = (want.x.0 - at.x.0).abs().max((want.y.0 - at.y.0).abs());
                    if d > POS_TOL {
                        fail!(format!("KiCad puts {label} at {at:?}, cadlab at {want:?}"));
                    }
                }
            }
            let hit = ours.iter().enumerate().position(|(i, c)| !taken[i] && same(k, c));
            match hit {
                Some(i) => {
                    taken[i] = true;
                    let c = &ours[i];
                    log.push(format!("  ok  {}  <->  {}", c.text, k.text));
                    if let (Some(a), Some(b)) = (k.value, c.value)
                        && (a - b).abs() > VALUE_TOL
                    {
                        fail!(format!("measured {b} mm by cadlab, {a} mm by KiCad: {} / {}", c.text, k.text));
                    }
                }
                None => match allowed(case.name, Side::Kicad, k) {
                    Some(a) => {
                        log.push(format!("  allowed (KiCad only)  {}{}", k.text, why(a, &mut used)));
                    }
                    None => fail!(format!("only KiCad reports: {}", k.text)),
                },
            }
        }
        for (i, c) in ours.iter().enumerate() {
            if taken[i] {
                continue;
            }
            match allowed(case.name, Side::Cadlab, c) {
                Some(a) => {
                    log.push(format!("  allowed (cadlab only)  {}{}", c.text, why(a, &mut used)));
                }
                None => fail!(format!("only cadlab reports: {} {:?}", c.text, c.items)),
            }
        }
        eprintln!("{}", log.join("\n"));
    }
    for (a, u) in ALLOW.iter().zip(&used) {
        if !u {
            problems.push(format!("unused allowlist entry {:?} {} ({:?})", a.side, a.rule, a.case));
        }
    }
    assert!(problems.is_empty(), "DRC cross-check differences:\n{}", problems.join("\n"));
}

#[test]
fn value_parsing() {
    assert_eq!(mm_after("track#3 (net 3V3) and U1.4 (no net) are 0.1mm apart on F.Cu", " are "), Some(0.1));
    assert_eq!(mm_after("via#3 annular ring 0.075mm (0.45mm pad, 0.3mm drill)", "annular ring "), Some(0.075));
    assert_eq!(kicad_actual("Clearance violation (clearance 0.2000 mm; actual 0.1562 mm)"), Some(0.1562));
    assert_eq!(kicad_actual("Tracks crossing"), None);
}
