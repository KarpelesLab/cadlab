//! Checks a project against a fab profile: can this fab make (and assemble) the board?
//!
//! [`check`] covers the board itself (process options, size, geometric limits through
//! [`crate::drc::check_limits`] with the profile's minimums, drills, annular rings, silkscreen)
//! and the assembly constraints. [`parts`] adds parts availability through the configured
//! suppliers. Nothing in the project is changed: the profile's limits go into a temporary rule
//! set. Diagnostics use `fab.*` codes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::export::Substitution;
use super::{FabProfile, Process, Side, chip_size};
use crate::board::{self as geo, COPPER_TOL};
use crate::bom::BomRow;
use crate::diag::{Diagnostic, Severity};
use crate::geom::Point;
use crate::model::Project;
use crate::model::board::{BoardSide, GraphicKind, Rules};
use crate::model::footprint::{GraphicGeometry, GraphicLayer, Mount, PadKind};
use crate::model::sections::natural_cmp;
use crate::refs::ObjectRef;
use crate::sourcing::{self, Availability};
use crate::substitute::LineSubstitutes;
use crate::supplier::Suppliers;
use crate::units::Nm;

/// Board options chosen for a fab: the first preference the process offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Choices {
    /// Surface finish (`None`: the fab's default, no preference given or none offered).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    /// Solder mask color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask_color: Option<String>,
    /// Silkscreen color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silk_color: Option<String>,
}

/// Result of checking a board against a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Process used, if one fits the layer count.
    pub process: Option<String>,
    /// Board options chosen.
    pub choices: Choices,
    /// Findings, `fab.*` codes, sorted by code then location.
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// Number of findings of a severity.
    pub fn count(&self, s: Severity) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == s).count()
    }
}

/// Picks the process: `id` when given (error if unknown), else the first offering the board's
/// layer count.
pub fn select_process<'a>(
    profile: &'a FabProfile,
    p: &Project,
    id: Option<&str>,
) -> Result<&'a Process, Box<Diagnostic>> {
    let layers = p.board().stackup.copper_layers;
    match id {
        Some(id) => profile.process(id).ok_or_else(|| {
            let ids: Vec<&str> = profile.processes.iter().map(|q| q.id.as_str()).collect();
            Box::new(
                Diagnostic::error("fab.unknown_process", format!("{} has no process `{id}`", profile.name))
                    .with_hint(format!("processes: {}", ids.join(", "))),
            )
        }),
        None => profile.process_for(layers).ok_or_else(|| {
            let mut all: Vec<u8> = profile.processes.iter().flat_map(|q| q.layers.iter().copied()).collect();
            all.sort();
            all.dedup();
            let all: Vec<String> = all.iter().map(u8::to_string).collect();
            Box::new(
                Diagnostic::error("fab.layers", format!("{} does not make {layers}-layer boards", profile.name))
                    .with_hint(format!("layer counts offered: {}; change it with board.setup", all.join(", "))),
            )
        }),
    }
}

/// Checks the board and assembly constraints (not parts availability: see [`parts`]).
pub fn check(p: &Project, profile: &FabProfile, process: Option<&str>) -> Report {
    let mut out = Vec::new();
    let proc_ = match select_process(profile, p, process) {
        Ok(q) => q,
        Err(d) => return Report { process: None, choices: Choices::default(), diagnostics: vec![*d] },
    };
    let ctx = Ctx { p, profile, process: proc_ };
    let choices = ctx.board_options(&mut out);
    ctx.board_size(&mut out);
    ctx.limits(&mut out);
    ctx.holes(&mut out);
    ctx.silk(&mut out);
    ctx.assembly(&mut out);
    sort(&mut out);
    Report { process: Some(proc_.id.clone()), choices, diagnostics: out }
}

fn sort(out: &mut [Diagnostic]) {
    out.sort_by(|a, b| {
        let loc = |d: &Diagnostic| d.location.map(|l| (l.x, l.y));
        (a.code.as_ref(), loc(a), &a.message).cmp(&(b.code.as_ref(), loc(b), &b.message))
    });
}

struct Ctx<'a> {
    p: &'a Project,
    profile: &'a FabProfile,
    process: &'a Process,
}

/// Case- and space-insensitive comparison of option names (`HASL lead-free` = `hasl-leadfree`).
fn same(a: &str, b: &str) -> bool {
    let k = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    k(a) == k(b)
}

impl Ctx<'_> {
    fn who(&self) -> String {
        format!("{} {}", self.profile.name, self.process.id)
    }

    fn hint_other(&self) -> String {
        format!("or choose another fab/process (fab.compare); limits from {}", self.profile.sources[0])
    }

    fn board_options(&self, out: &mut Vec<Diagnostic>) -> Choices {
        let s = &self.p.board().stackup;
        let q = self.process;
        let who = self.who();
        if !q.thickness.is_empty() && !q.thickness.contains(&s.thickness) {
            let opts: Vec<String> = q.thickness.iter().map(Nm::to_string).collect();
            out.push(
                Diagnostic::error("fab.thickness", format!("{who} does not offer a {} board thickness", s.thickness))
                    .with_hint(format!("offered: {}; change it with board.setup", opts.join(", "))),
            );
        }
        if !q.outer_copper.is_empty() && !q.outer_copper.contains(&s.outer_copper) {
            let opts: Vec<String> = q.outer_copper.iter().map(Nm::to_string).collect();
            out.push(
                Diagnostic::error("fab.copper", format!("{who} does not offer {} outer copper", s.outer_copper))
                    .with_hint(format!("offered (35 um = 1 oz): {}; change it with board.setup", opts.join(", "))),
            );
        }
        if s.copper_layers > 2 && !q.inner_copper.is_empty() && !q.inner_copper.contains(&s.inner_copper) {
            let opts: Vec<String> = q.inner_copper.iter().map(Nm::to_string).collect();
            out.push(
                Diagnostic::error("fab.copper", format!("{who} does not offer {} inner copper", s.inner_copper))
                    .with_hint(format!("offered (35 um = 1 oz): {}; change it with board.setup", opts.join(", "))),
            );
        }
        let mut pick = |what: &str, code: &'static str, prefs: &[String], offered: &[String]| -> Option<String> {
            if offered.is_empty() {
                return prefs.first().cloned();
            }
            let found = prefs.iter().find_map(|pref| offered.iter().find(|o| same(o, pref)).cloned());
            if found.is_none() && !prefs.is_empty() {
                out.push(
                    Diagnostic::warning(
                        code,
                        format!("{who} offers none of the {what} preferences ({})", prefs.join(", ")),
                    )
                    .with_hint(format!("offered: {}; the fab default is used", offered.join(", "))),
                );
            }
            found
        };
        Choices {
            finish: pick("finish", "fab.finish", &s.finish, &q.finishes),
            mask_color: pick("mask color", "fab.mask_color", &s.mask_color, &q.mask_colors),
            silk_color: pick("silkscreen color", "fab.silk_color", &s.silk_color, &q.silk_colors),
        }
    }

    fn board_size(&self, out: &mut Vec<Diagnostic>) {
        let Some(c) = self.p.board().outline.contours.first() else {
            out.push(
                Diagnostic::error("fab.no_outline", "the board has no outline")
                    .with_hint("draw one with board.outline before checking manufacturability"),
            );
            return;
        };
        let ring = geo::contour_ring(c, COPPER_TOL);
        let (Some(x0), Some(x1), Some(y0), Some(y1)) = (
            ring.iter().map(|q| q.x).min(),
            ring.iter().map(|q| q.x).max(),
            ring.iter().map(|q| q.y).min(),
            ring.iter().map(|q| q.y).max(),
        ) else {
            return;
        };
        let (w, h) = (Nm(x1 - x0), Nm(y1 - y0));
        let fits = |[a, b]: [Nm; 2]| (w <= a && h <= b) || (w <= b && h <= a);
        let covers = |[a, b]: [Nm; 2]| (w >= a && h >= b) || (w >= b && h >= a);
        if let Some(m) = self.process.max_size
            && !fits(m)
        {
            out.push(
                Diagnostic::error(
                    "fab.board_size",
                    format!("{w} x {h} board exceeds {}'s maximum {} x {}", self.who(), m[0], m[1]),
                )
                .with_hint(format!("make the board smaller (board.outline), {}", self.hint_other())),
            );
        }
        if let Some(m) = self.process.min_size
            && !covers(m)
        {
            out.push(
                Diagnostic::error(
                    "fab.board_size",
                    format!("{w} x {h} board is below {}'s minimum {} x {}", self.who(), m[0], m[1]),
                )
                .with_hint("make the board larger or panelize it"),
            );
        }
    }

    /// Clearance, track width, hole-to-hole, copper-to-edge and silk-to-pad through the DRC with
    /// a temporary rule set holding the profile's minimums.
    fn limits(&self, out: &mut Vec<Diagnostic>) {
        let q = self.process;
        let z = Nm::ZERO;
        let rules = Rules {
            clearance: q.min_space.unwrap_or(z),
            min_track_width: q.min_track.unwrap_or(z),
            min_drill: z,
            min_annular_ring: z,
            hole_to_hole: q.hole_to_hole.unwrap_or(z),
            copper_to_edge: q.copper_to_edge.unwrap_or(z),
            silk_to_pad: q.silk_to_pad.unwrap_or(z),
            ..Rules::default()
        };
        let who = self.who();
        for mut d in crate::drc::check_limits(self.p, &rules) {
            let (code, hint) = match d.code.as_ref() {
                "drc.clearance" if q.min_space.is_some() => ("fab.clearance", "move or reroute one of them"),
                "drc.track_width" if q.min_track.is_some() => ("fab.track_width", "use a wider track"),
                "drc.hole_to_hole" if q.hole_to_hole.is_some() => ("fab.hole_to_hole", "move the holes apart"),
                "drc.copper_to_edge" if q.copper_to_edge.is_some() => {
                    ("fab.copper_to_edge", "move the copper away from the board edge")
                }
                "drc.silk_over_pad" if q.silk_to_pad.is_some() => {
                    ("fab.silk_to_pad", "move the silkscreen away; the fab clips it at pads")
                }
                // Shorts and copper outside the board are design errors that drc.run reports.
                _ => continue,
            };
            d.code = code.into();
            d.message = format!("{} ({who})", d.message);
            d.hint = Some(format!("{hint}, {}", self.hint_other()));
            out.push(d);
        }
    }

    /// Drill sizes, annular rings and pad hole-to-hole.
    fn holes(&self, out: &mut Vec<Diagnostic>) {
        let q = self.process;
        let who = self.who();
        let board = self.p.board();
        for v in &board.vias {
            let subj = |d: Diagnostic| {
                let d = d.with_subject(ObjectRef::Item { kind: "via".into(), index: v.id.0 }).at(v.at);
                match &v.net {
                    Some(n) => d.with_subject(ObjectRef::Net(n.clone())),
                    None => d,
                }
            };
            if let Some(m) = q.min_drill
                && v.drill < m
            {
                out.push(subj(
                    Diagnostic::error(
                        "fab.drill",
                        format!("via#{} drill {} is below {who}'s minimum {m}", v.id.0, v.drill),
                    )
                    .with_hint(format!("use a larger via drill, {}", self.hint_other())),
                ));
            }
            let ring = Nm((v.diameter.0 - v.drill.0) / 2);
            if let Some(m) = q.min_via_ring
                && ring < m
            {
                out.push(subj(
                    Diagnostic::error(
                        "fab.annular_ring",
                        format!("via#{} annular ring {ring} is below {who}'s minimum {m}", v.id.0),
                    )
                    .with_hint(format!("use a larger via diameter, {}", self.hint_other())),
                ));
            }
            if let Some(m) = q.max_drill
                && v.drill > m
            {
                out.push(subj(Diagnostic::warning(
                    "fab.max_drill",
                    format!("via#{} drill {} is above {who}'s largest drill {m}", v.id.0, v.drill),
                )));
            }
        }
        let pads = geo::placed_pads(self.p);
        let mut pad_holes: Vec<(Point, Nm, String, Vec<ObjectRef>)> = Vec::new();
        for pp in &pads {
            let (drill, plated) = match pp.pad.kind {
                PadKind::Tht { drill } => (drill, true),
                PadKind::Npth { drill } => (drill, false),
                PadKind::Smd => continue,
            };
            let label = if pp.number.is_empty() {
                format!("{} hole", pp.refdes)
            } else {
                format!("pad {}.{}", pp.refdes, pp.number)
            };
            let mut subjects = vec![ObjectRef::Pin { component: pp.refdes.clone(), pin: pp.number.clone() }];
            subjects.extend(pp.net.clone().map(ObjectRef::Net));
            let with = |mut d: Diagnostic| {
                d.subjects = subjects.clone();
                d.at(pp.center)
            };
            let min = if plated { q.min_drill } else { q.min_npth.or(q.min_drill) };
            if let Some(m) = min
                && drill < m
            {
                let code = if plated { "fab.drill" } else { "fab.npth_drill" };
                out.push(with(
                    Diagnostic::error(code, format!("{label} drill {drill} is below {who}'s minimum {m}"))
                        .with_hint(format!("use a footprint with a larger hole, {}", self.hint_other())),
                ));
            }
            if let Some(m) = q.max_drill
                && drill > m
            {
                out.push(with(Diagnostic::warning(
                    "fab.max_drill",
                    format!("{label} drill {drill} is above {who}'s largest drill {m} (may be routed)"),
                )));
            }
            if plated && let Some(m) = q.min_pth_ring {
                let (w, h) = pp.pad.shape.size();
                let ring = Nm((w.0.min(h.0) - drill.0) / 2);
                if ring < m {
                    out.push(with(
                        Diagnostic::error(
                            "fab.annular_ring",
                            format!("{label} annular ring {ring} is below {who}'s minimum {m}"),
                        )
                        .with_hint(format!("use a footprint with larger pads, {}", self.hint_other())),
                    ));
                }
            }
            pad_holes.push((pp.center, drill, label, subjects));
        }
        if let Some(m) = q.pad_hole_to_hole {
            for i in 0..pad_holes.len() {
                for j in i + 1..pad_holes.len() {
                    let (a, b) = (&pad_holes[i], &pad_holes[j]);
                    let (dx, dy) = ((a.0.x.0 - b.0.x.0) as f64, (a.0.y.0 - b.0.y.0) as f64);
                    let edge = (dx * dx + dy * dy).sqrt() - (a.1.0 + b.1.0) as f64 / 2.0;
                    if edge >= m.0 as f64 - 0.5 {
                        continue;
                    }
                    let mut d = Diagnostic::error(
                        "fab.pad_hole_to_hole",
                        format!(
                            "holes of {} and {} are {} apart (edge to edge), {who}'s minimum is {m}",
                            a.2,
                            b.2,
                            Nm(edge.max(0.0).round() as i64)
                        ),
                    )
                    .at(Point::new(Nm((a.0.x.0 + b.0.x.0) / 2), Nm((a.0.y.0 + b.0.y.0) / 2)))
                    .with_hint(format!("move the footprints apart, {}", self.hint_other()));
                    d.subjects.extend(a.3.iter().cloned());
                    d.subjects.extend(b.3.iter().cloned());
                    out.push(d);
                }
            }
        }
    }

    /// Silkscreen line widths and text heights (warnings: the fab may drop or blur them).
    fn silk(&self, out: &mut Vec<Diagnostic>) {
        let q = self.process;
        let who = self.who();
        let board = self.p.board();
        if let Some(m) = q.min_silk_width {
            // One finding per footprint, naming the components using it.
            let mut by_fp: std::collections::BTreeMap<String, (Nm, Vec<String>)> = Default::default();
            for r in board.footprints.keys() {
                let Some(fp) = geo::footprint_for(self.p, r) else { continue };
                // Stroked lines only: filled dots (pin 1 marks) have no stroke width.
                let thin = fp
                    .graphics
                    .iter()
                    .filter(|g| g.layer == GraphicLayer::Silk && g.width > Nm::ZERO)
                    .filter(|g| !matches!(g.geometry, GraphicGeometry::Circle { filled: true, .. }))
                    .map(|g| g.width)
                    .min();
                if let Some(w) = thin
                    && w < m
                {
                    by_fp.entry(fp.name.clone()).or_insert((w, Vec::new())).1.push(r.clone());
                }
            }
            for (name, (w, mut refs)) in by_fp {
                refs.sort_by(|a, b| natural_cmp(a, b));
                let mut d = Diagnostic::warning(
                    "fab.silk_width",
                    format!("footprint {name} has {w} silkscreen lines, {who}'s minimum is {m}"),
                )
                .with_hint("the fab may print thin lines faintly or drop them; widen the footprint's silk lines in the project library if they matter");
                d.subjects = refs.into_iter().map(ObjectRef::Name).collect();
                out.push(d);
            }
        }
        for g in board.graphics.iter().filter(|g| g.layer.ends_with(".SilkS")) {
            let subject = ObjectRef::Item { kind: "graphic".into(), index: g.id.0 };
            match &g.kind {
                GraphicKind::Line { points, width } => {
                    if let Some(m) = q.min_silk_width
                        && *width < m
                    {
                        let mut d = Diagnostic::warning(
                            "fab.silk_width",
                            format!("silkscreen line graphic#{} is {width} wide, {who}'s minimum is {m}", g.id.0),
                        )
                        .with_subject(subject)
                        .with_hint("redraw it wider");
                        if let Some(p0) = points.first() {
                            d = d.at(*p0);
                        }
                        out.push(d);
                    }
                }
                GraphicKind::Text { text, at, size, .. } => {
                    if let Some(m) = q.min_silk_height
                        && *size < m
                    {
                        out.push(
                            Diagnostic::warning(
                                "fab.silk_height",
                                format!("silkscreen text \"{text}\" is {size} high, {who}'s minimum is {m}"),
                            )
                            .with_subject(subject)
                            .at(*at)
                            .with_hint("use a larger text size"),
                        );
                    }
                }
            }
        }
    }

    /// Sides, package sizes and through-hole support of populated, placed components.
    fn assembly(&self, out: &mut Vec<Diagnostic>) {
        let Some(a) = &self.profile.assembly else { return };
        let name = &self.profile.name;
        let board = self.p.board();
        let dnp = &self.p.bom().dnp;
        let min = a.min_package.as_deref().and_then(|c| chip_size(c).map(|s| (c, s)));
        for (r, pf) in &board.footprints {
            if dnp.contains(r) || !self.p.circuit().components.contains_key(r) {
                continue;
            }
            let side = match pf.side {
                BoardSide::Top => Side::Top,
                BoardSide::Bottom => Side::Bottom,
            };
            if !a.sides.contains(&side) {
                out.push(
                    Diagnostic::error(
                        "fab.assembly_side",
                        format!("{r} is on the {side:?} side; {name} assembles only {:?}", a.sides),
                    )
                    .with_subject(ObjectRef::Name(r.clone()))
                    .at(pf.at)
                    .with_hint("move it to an assembled side (place.flip), or assemble it yourself (bom.dnp)"),
                );
            }
            let Some(fp) = geo::footprint_for(self.p, r) else { continue };
            if fp.mount == Mount::Tht && a.through_hole == Some(false) {
                out.push(
                    Diagnostic::error(
                        "fab.through_hole",
                        format!("{r} is through-hole; {name} does not assemble THT parts"),
                    )
                    .with_subject(ObjectRef::Name(r.clone()))
                    .at(pf.at)
                    .with_hint("use an SMD part (bom.replace) or mark it DNP and solder it yourself"),
                );
            }
            if let (Some((code, (ml, mw))), Some(body), Mount::Smd) = (min, fp.body, fp.mount) {
                let (l, w) = (body.width.max(body.length), body.width.min(body.length));
                // Smaller than the smallest chip size in both dimensions (5% tolerance).
                if l.0 * 100 < ml.0 * 95 && w.0 * 100 < mw.0 * 95 {
                    out.push(
                        Diagnostic::error(
                            "fab.package_size",
                            format!("{r} ({}, {l} x {w}) is smaller than {name}'s smallest package {code}", fp.name),
                        )
                        .with_subject(ObjectRef::Name(r.clone()))
                        .at(pf.at)
                        .with_hint(format!("use a {code} or larger package (bom.replace)")),
                    );
                }
            }
        }
    }
}

/// The part chosen for a BOM line at export.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinePick {
    /// Part ID.
    pub part: String,
    /// Manufacturer of the chosen MPN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// Chosen MPN (the part's own, else the first approved one, or the best offer's; a
    /// substitution from `fab-lock.json` when one applies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    /// Fab catalog SKU, from one of the profile's `catalog` providers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sku: Option<String>,
    /// Provider of the offer used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Availability, when suppliers were queried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Availability>,
    /// MPN replaced by a substitution (`"generic"` for a line without one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
}

/// Parts of a fab check: the pick per populated line, findings, and substitute candidates for
/// lines the fab cannot source as designed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartsReport {
    /// One pick per populated BOM line.
    pub picks: Vec<LinePick>,
    /// Findings (`fab.*`, `supplier.error`).
    pub diagnostics: Vec<Diagnostic>,
    /// Substitutions applied (from `fab-lock.json`), to keep in the next lock.
    pub applied: Vec<Substitution>,
    /// Substitute candidates per line that needs them, in BOM order.
    pub substitutes: Vec<LineSubstitutes>,
}

/// Providers whose SKUs fill the fab's BOM (configured ones among the profile's `catalog`).
pub fn catalog_providers(profile: &FabProfile, suppliers: &Suppliers) -> Vec<String> {
    let ids = suppliers.ids();
    profile.assembly.iter().flat_map(|a| a.catalog.iter()).filter(|c| ids.contains(c)).cloned().collect()
}

/// Picks parts for populated BOM lines and checks their availability for `boards` boards,
/// applying `applied` substitutions (from `fab-lock.json`), and finds up to `limit`
/// substitute candidates for every line that is not available (see [`crate::substitute`]).
/// With no suppliers configured, picks come from the BOM (and substitutions) alone and a
/// `fab.no_suppliers` warning is returned. Provider errors come back as `supplier.error`
/// warnings.
pub fn parts(
    p: &Project,
    rows: &[BomRow],
    profile: &FabProfile,
    suppliers: &Suppliers,
    boards: u64,
    applied: &[Substitution],
    limit: usize,
) -> PartsReport {
    let mut diags = Vec::new();
    let catalog: Vec<String> = profile.assembly.as_ref().map(|a| a.catalog.clone()).unwrap_or_default();
    let ids = suppliers.ids();
    let only = catalog_providers(profile, suppliers);
    if suppliers.is_empty() {
        diags.push(
            Diagnostic::warning(
                "fab.no_suppliers",
                "no part suppliers are configured; parts availability was not checked",
            )
            .with_hint("add catalog files to ~/.config/cadlab/catalogs/ or configure DigiKey (docs/PARTS.md)"),
        );
    } else if !catalog.is_empty() && only.is_empty() {
        diags.push(
            Diagnostic::info(
                "fab.no_catalog",
                format!(
                    "{} orders parts by {} SKU; no such provider is configured, availability checked at {}",
                    profile.name,
                    catalog.join("/"),
                    ids.join(", ")
                ),
            )
            .with_hint(format!("add a `{}` catalog to get the fab's SKUs", catalog[0])),
        );
    }
    let mut errors = Vec::new();
    let mut picks = Vec::new();
    let mut substitutes = Vec::new();
    for r in rows.iter().filter(|r| r.quantity > 0) {
        let what = format!("{} ({})", r.part, r.refdes.join(", "));
        let subject = ObjectRef::Part { scheme: "local".into(), id: r.part.clone() };
        let sub = applied.iter().find(|s| s.part == r.part);
        // A substituted line is sourced as its substitute.
        let row = match sub {
            Some(s) => {
                let replaces = r.order_mpn().map_or("generic".to_string(), |(_, m)| m.to_string());
                diags.push(
                    Diagnostic::info(
                        "fab.substituted",
                        format!("{what}: ordered as {} in place of {replaces} (fab-lock.json)", s.mpn),
                    )
                    .with_subject(subject.clone())
                    .with_hint(format!("undo with fab.substitute {} {} with remove", profile.id, r.part)),
                );
                BomRow {
                    manufacturer: s.manufacturer.clone(),
                    mpn: Some(s.mpn.clone()),
                    approved: Vec::new(),
                    ..r.clone()
                }
            }
            None => r.clone(),
        };
        let own = row.order_mpn();
        let mut pick = LinePick {
            part: r.part.clone(),
            manufacturer: own.and_then(|(m, _)| m.map(String::from)),
            mpn: own.map(|(_, m)| m.to_string()),
            sku: sub.filter(|s| s.provider.as_ref().is_some_and(|p| catalog.contains(p))).and_then(|s| s.sku.clone()),
            provider: sub.and_then(|s| s.provider.clone()),
            status: None,
            replaces: sub.map(|_| r.order_mpn().map_or("generic".to_string(), |(_, m)| m.to_string())),
        };
        if !suppliers.is_empty() {
            let s = sourcing::source_line(&row, boards.max(1), suppliers, &only, &mut errors);
            pick.status = Some(s.status);
            if let Some(o) = &s.offer {
                pick.manufacturer = o.manufacturer.clone().or(pick.manufacturer);
                pick.mpn = Some(o.mpn.clone());
                pick.provider = Some(o.provider.clone());
                pick.sku = catalog.contains(&o.provider).then(|| o.sku.clone());
            }
            let mut found = None;
            if s.status != Availability::Ok && sub.is_none() {
                let part = p.library().parts.get(&r.part);
                found = Some(crate::substitute::for_line(
                    r,
                    part,
                    s.status,
                    s.needed,
                    suppliers,
                    &only,
                    limit,
                    &mut errors,
                ));
            }
            let at = if only.is_empty() { String::new() } else { format!(" at {}", only.join("/")) };
            let d = match s.status {
                Availability::Ok => None,
                Availability::NoMpn => Some(
                    Diagnostic::warning("fab.no_mpn", format!("{what} has no MPN for {} to order", profile.name))
                        .with_hint("approve one with bom.approve or find candidates with bom.resolve"),
                ),
                Availability::NotFound => Some(
                    Diagnostic::warning("fab.part_not_found", format!("no supplier{at} has {what}"))
                        .with_hint("approve an alternate MPN with bom.approve, or see bom.substitutes"),
                ),
                Availability::LowStock => Some(
                    Diagnostic::warning(
                        "fab.low_stock",
                        format!("not enough stock{at} of {what} for {boards} board(s)"),
                    )
                    .with_hint("approve an alternate MPN with bom.approve"),
                ),
                Availability::EndOfLife => Some(
                    Diagnostic::warning("fab.end_of_life", format!("{what} is only offered as NRND/obsolete"))
                        .with_hint("approve an active alternate with bom.approve"),
                ),
            };
            if let Some(mut d) = d {
                if let Some(f) = &found {
                    d.hint = Some(match f.candidates.first() {
                        Some(best) => format!(
                            "{} substitute candidate(s) for {}, best {}; apply one for this fab with fab.substitute \
                             (recorded in fab-lock.json, not in the design), or approve it for every fab with bom.approve",
                            f.candidates.len(),
                            profile.name,
                            crate::substitute::describe(best)
                        ),
                        None => format!(
                            "no substitute found ({}); {}",
                            f.note.as_deref().unwrap_or("no candidate"),
                            d.hint.as_deref().unwrap_or_default()
                        ),
                    });
                }
                diags.push(d.with_subject(subject));
            }
            substitutes.extend(found);
        }
        picks.push(pick);
    }
    errors.sort();
    errors.dedup();
    for e in errors {
        diags.push(Diagnostic::warning("supplier.error", e).with_hint("results from other providers are still used"));
    }
    PartsReport { picks, diagnostics: diags, applied: applied.to_vec(), substitutes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_names() {
        assert!(same("HASL lead-free", "hasl leadfree"));
        assert!(!same("ENIG", "ENEPIG"));
    }
}
