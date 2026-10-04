//! `board.stackup`, `board.dielectric`, `impedance.*`, `current.*`: stackup dielectrics,
//! transmission line impedance and IPC-2152 track widths (`docs/ELECTRICAL.md`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::electrical::{self, GeometryError, LayerGeometry, Model, current::Method};
use crate::model::Project;
use crate::model::circuit::NetClass;
use crate::refs::ObjectRef;
use crate::units::{LengthUnit, Nm};
use crate::value::{Quantity, Unit};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Stackup>()
        .register::<SetDielectric>()
        .register::<Calc>()
        .register::<Solve>()
        .register::<CurrentWidth>();
}

/// Checks that `q` is in `unit` (a bare number is taken as `unit`).
pub(crate) fn expect_unit(q: Quantity, unit: Unit, field: &str) -> Result<Quantity, CommandError> {
    match q.unit {
        Unit::None => Ok(q.with_unit(unit)),
        u if u == unit => Ok(q),
        u => Err(CommandError::invalid_args(
            "value.wrong_unit",
            format!("`{field}` is in {}, expected {}", u.symbol(), unit.symbol()),
        )),
    }
}

fn positive(q: &Quantity, field: &str) -> Result<(), CommandError> {
    if q.to_f64() > 0.0 {
        Ok(())
    } else {
        Err(CommandError::invalid_args("value.not_positive", format!("`{field}` must be positive")))
    }
}

fn layer_error(p: &Project, layer: &str, e: GeometryError) -> CommandError {
    let names = p.board().stackup.copper_names().join(", ");
    match e {
        GeometryError::UnknownLayer => {
            CommandError::not_found("layer.not_found", format!("`{layer}` is not a copper layer of the board"))
                .with_subject(ObjectRef::Layer(layer.to_string()))
                .with_hint(format!("copper layers: {names}"))
        }
        GeometryError::NoReference => CommandError::invalid_args(
            "impedance.no_reference",
            "a single-layer board has no reference plane, so no stackup geometry",
        )
        .with_hint("give the geometry explicitly (`height`, `er`, `copper`), or use two or more layers (board.setup)"),
    }
}

// ---------------------------------------------------------------------------------------------
// Stackup

/// A copper layer in the stackup.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CopperInfo {
    /// Layer name.
    pub name: String,
    /// Copper thickness.
    pub thickness: Nm,
    /// Line model used for impedance on this layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
}

/// A dielectric in the stackup.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DielectricInfo {
    /// Gap number (1 = between the top copper layer and the next).
    pub gap: usize,
    /// Copper layers above and below.
    pub between: (String, String),
    /// Thickness.
    pub thickness: Nm,
    /// Relative permittivity.
    pub er: Quantity,
    /// Material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
}

/// The stackup, top to bottom.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StackupInfo {
    /// Copper layers.
    pub copper: Vec<CopperInfo>,
    /// Dielectrics between them.
    pub dielectrics: Vec<DielectricInfo>,
    /// Whether the dielectrics are assumed (not set with `board.dielectric`).
    pub assumed: bool,
    /// Board thickness (`board.setup`).
    pub thickness: Nm,
    /// Copper plus dielectric thickness.
    pub layers_thickness: Nm,
}

fn stackup_info(p: &Project) -> StackupInfo {
    let s = &p.board().stackup;
    let names = s.copper_names();
    let (diel, assumed) = s.effective_dielectrics();
    let copper: Vec<CopperInfo> = names
        .iter()
        .map(|n| CopperInfo {
            name: n.clone(),
            thickness: electrical::copper_thickness(s, n),
            model: electrical::layer_geometry(s, n).ok().map(|g| g.model),
        })
        .collect();
    let dielectrics: Vec<DielectricInfo> = diel
        .iter()
        .enumerate()
        .map(|(i, d)| DielectricInfo {
            gap: i + 1,
            between: (names[i].clone(), names[i + 1].clone()),
            thickness: d.thickness,
            er: d.er,
            material: d.material.clone(),
        })
        .collect();
    let layers_thickness =
        Nm(copper.iter().map(|c| c.thickness.0).sum::<i64>() + dielectrics.iter().map(|d| d.thickness.0).sum::<i64>());
    StackupInfo { copper, dielectrics, assumed, thickness: s.thickness, layers_thickness }
}

fn stackup_diagnostics(o: &StackupInfo) -> Vec<Diagnostic> {
    let mut v = Vec::new();
    if o.assumed && !o.dielectrics.is_empty() {
        v.push(
            Diagnostic::info(
                "board.stackup_assumed",
                format!(
                    "dielectrics are not specified: assuming the material thickness split equally, εr {}",
                    crate::model::board::DEFAULT_ER
                ),
            )
            .with_hint("set them from your fab's stackup with `board.dielectric --gap N --thickness .. --er ..`"),
        );
    }
    let (a, b) = (o.layers_thickness.0 as f64, o.thickness.0 as f64);
    if !o.assumed && b > 0.0 && ((a - b) / b).abs() > 0.10 {
        v.push(
            Diagnostic::warning(
                "board.stackup_thickness",
                format!("copper and dielectrics add up to {}, the board is {} thick", o.layers_thickness, o.thickness),
            )
            .with_hint("fix the dielectric thicknesses (board.dielectric) or the board thickness (board.setup)"),
        );
    }
    v
}

fn stackup_text(o: &StackupInfo) -> String {
    let mut lines = Vec::new();
    for (i, c) in o.copper.iter().enumerate() {
        let model = c.model.map(|m| format!("  ({})", serde_json::to_value(m).unwrap().as_str().unwrap()));
        lines.push(format!("{:<8} copper {}{}", c.name, c.thickness, model.unwrap_or_default()));
        if let Some(d) = o.dielectrics.get(i) {
            lines.push(format!(
                "  gap {}  {} εr {}{}",
                d.gap,
                d.thickness,
                d.er,
                d.material.as_ref().map(|m| format!("  {m}")).unwrap_or_default()
            ));
        }
    }
    lines.push(format!(
        "total {} (board {}){}",
        o.layers_thickness,
        o.thickness,
        if o.assumed { ", dielectrics assumed" } else { "" }
    ));
    lines.join("\n")
}

/// Show the layer stackup: copper layers, dielectrics (thickness, εr) and the impedance model
/// of each copper layer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stackup {}

impl Command for Stackup {
    const NAME: &'static str = "board.stackup";
    const SUMMARY: &'static str = "Show the layer stackup: copper, dielectrics (thickness, εr), line models";
    const KIND: CommandKind = CommandKind::Query;
    type Output = StackupInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<StackupInfo, CommandError> {
        let o = stackup_info(ctx.project()?);
        for d in stackup_diagnostics(&o) {
            ctx.report(d);
        }
        Ok(o)
    }

    fn summarize(o: &StackupInfo) -> String {
        stackup_text(o)
    }
}

/// Set the dielectric between two copper layers (thickness, relative permittivity, material),
/// used by the impedance calculator. Unset dielectrics start from the assumed ones.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDielectric {
    /// Gap number: 1 is between the top copper layer and the next one. Omitted: every gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<usize>,
    /// Thickness, copper to copper ("0.2mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<Nm>,
    /// Relative permittivity ("4.4").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub er: Option<Quantity>,
    /// Material, for information ("FR-4 7628"); empty clears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Forget every dielectric (back to the assumed stackup).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear: bool,
}

impl Command for SetDielectric {
    const NAME: &'static str = "board.dielectric";
    const SUMMARY: &'static str = "Set a stackup dielectric (thickness, εr, material) for impedance calculations";
    const KIND: CommandKind = CommandKind::Mutation;
    type Output = StackupInfo;

    fn run(self, ctx: &mut Context<'_>) -> Result<StackupInfo, CommandError> {
        let s = &ctx.project()?.board().stackup;
        let gaps = s.copper_names().len() - 1;
        if self.clear {
            ctx.project_mut()?.board_mut().stackup.dielectrics.clear();
        } else {
            if gaps == 0 {
                return Err(CommandError::invalid_args(
                    "board.no_dielectric",
                    "a single-layer board has no dielectric between copper layers",
                ));
            }
            if let Some(g) = self.gap
                && (g == 0 || g > gaps)
            {
                return Err(CommandError::invalid_args(
                    "board.invalid_gap",
                    format!("gap {g} does not exist: the board has gaps 1..{gaps}"),
                )
                .with_hint("gap 1 is between the top copper layer and the next (board.stackup lists them)"));
            }
            if let Some(t) = self.thickness
                && t <= Nm::ZERO
            {
                return Err(CommandError::invalid_args("board.invalid_dielectric", "`thickness` must be positive"));
            }
            let er = match self.er {
                Some(e) => {
                    let e = expect_unit(e, Unit::None, "er")?;
                    if e.to_f64() < 1.0 {
                        return Err(CommandError::invalid_args(
                            "board.invalid_dielectric",
                            "`er` (relative permittivity) must be at least 1",
                        ));
                    }
                    Some(e)
                }
                None => None,
            };
            let (mut diel, _) = s.effective_dielectrics();
            for (i, d) in diel.iter_mut().enumerate() {
                if self.gap.is_some_and(|g| g != i + 1) {
                    continue;
                }
                if let Some(t) = self.thickness {
                    d.thickness = t;
                }
                if let Some(e) = er {
                    d.er = e;
                }
                if let Some(m) = &self.material {
                    d.material = (!m.is_empty()).then(|| m.clone());
                }
            }
            ctx.project_mut()?.board_mut().stackup.dielectrics = diel;
        }
        let o = stackup_info(ctx.project()?);
        for d in stackup_diagnostics(&o) {
            ctx.report(d);
        }
        Ok(o)
    }

    fn summarize(o: &StackupInfo) -> String {
        stackup_text(o)
    }
}

// ---------------------------------------------------------------------------------------------
// Impedance

/// Geometry used for a calculation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GeometryInfo {
    /// Line model.
    pub model: Model,
    /// Dielectric height to the (nearer) reference plane.
    pub height: Nm,
    /// Stripline: height to the other plane; embedded microstrip: cover thickness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height2: Option<Nm>,
    /// Copper thickness.
    pub copper: Nm,
    /// Relative permittivity.
    pub er: Quantity,
    /// Reference planes (from the stackup).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// Whether the stackup's dielectrics were assumed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub assumed_stackup: bool,
    /// Formula used.
    pub formula: String,
}

/// Overrides of the stackup geometry, shared by `impedance.calc` and `impedance.solve`.
struct GeoArgs<'a> {
    layer: Option<&'a str>,
    model: Option<Model>,
    height: Option<Nm>,
    height2: Option<Nm>,
    copper: Option<Nm>,
    er: Option<Quantity>,
}

fn formula(model: Model, diff: bool) -> String {
    let base = match model {
        Model::Microstrip => "Hammerstad-Jensen microstrip (with thickness correction)",
        Model::EmbeddedMicrostrip => "IPC-2141A embedded microstrip",
        Model::Stripline => "Wheeler stripline (asymmetric: parallel combination)",
    };
    if diff {
        let c = match model {
            Model::Stripline => "IPC-2141 edge-coupled stripline: Zdiff = 2 Z0 (1 - 0.347 exp(-2.9 S/b))",
            _ => "IPC-2141 edge-coupled microstrip: Zdiff = 2 Z0 (1 - 0.48 exp(-0.96 S/H))",
        };
        format!("{base}; {c}")
    } else {
        base.to_string()
    }
}

/// Resolves the geometry: the stackup's for the layer, with explicit values overriding it.
fn resolve(p: &Project, a: &GeoArgs) -> Result<(Option<String>, LayerGeometry), CommandError> {
    let explicit = a.height.is_some() && a.er.is_some();
    let layer = match a.layer {
        Some(l) => Some(l.to_string()),
        None if explicit => None,
        None => Some("F.Cu".to_string()),
    };
    let stack = &p.board().stackup;
    let base = match &layer {
        Some(l) => match electrical::layer_geometry(stack, l) {
            Ok(g) => Some(g),
            Err(GeometryError::NoReference) if explicit => None,
            Err(e) => return Err(layer_error(p, l, e)),
        },
        None => None,
    };
    let mut g = base.unwrap_or(LayerGeometry {
        model: Model::Microstrip,
        h: Nm::ZERO,
        h2: Nm::ZERO,
        t: stack.outer_copper,
        er: 0.0,
        references: Vec::new(),
        assumed: false,
    });
    if let Some(m) = a.model {
        if m != g.model {
            g.references.clear();
            if m == Model::EmbeddedMicrostrip || g.model == Model::EmbeddedMicrostrip {
                g.h2 = Nm::ZERO;
            }
        }
        g.model = m;
    }
    if let Some(h) = a.height {
        g.h = h;
        g.assumed = false;
    }
    if let Some(h) = a.height2 {
        g.h2 = h;
    }
    if let Some(t) = a.copper {
        g.t = t;
    }
    if let Some(e) = a.er {
        let e = expect_unit(e, Unit::None, "er")?;
        g.er = e.to_f64();
    }
    if g.h <= Nm::ZERO || g.t < Nm::ZERO || g.er < 1.0 {
        return Err(CommandError::invalid_args(
            "impedance.invalid_geometry",
            "the dielectric height must be positive, the copper thickness not negative, and εr at least 1",
        ));
    }
    match g.model {
        Model::Stripline if g.h2 <= Nm::ZERO => {
            return Err(CommandError::invalid_args(
                "impedance.invalid_geometry",
                "a stripline needs `height2`, the dielectric height to the second plane",
            ));
        }
        Model::EmbeddedMicrostrip if g.h2 <= Nm::ZERO => {
            return Err(CommandError::invalid_args(
                "impedance.invalid_geometry",
                "an embedded microstrip needs `height2`, the thickness of the dielectric covering the trace",
            ));
        }
        _ => {}
    }
    Ok((layer, g))
}

fn geometry_info(g: &LayerGeometry, diff: bool) -> GeometryInfo {
    GeometryInfo {
        model: g.model,
        height: g.h,
        height2: (g.model != Model::Microstrip).then_some(g.h2),
        copper: g.t,
        er: Quantity::from_f64(g.er, 3, Unit::None).unwrap_or(Quantity::int(1, Unit::None)),
        references: g.references.clone(),
        assumed_stackup: g.assumed,
        formula: formula(g.model, diff),
    }
}

fn ohms(z: f64) -> Quantity {
    Quantity::from_f64(z, 2, Unit::Ohm).unwrap_or(Quantity::int(0, Unit::Ohm))
}

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

fn report_geometry(ctx: &mut Context<'_>, layer: Option<&str>, g: &LayerGeometry, w: Nm, gap: Option<Nm>) {
    if g.assumed {
        let mut d = Diagnostic::warning(
            "impedance.stackup_assumed",
            "the stackup's dielectrics are not specified: the result uses an assumed stackup",
        )
        .with_hint(
            "set the real dielectrics from your fab's stackup with `board.dielectric` (board.stackup shows them)",
        );
        if let Some(l) = layer {
            d = d.with_subject(ObjectRef::Layer(l.to_string()));
        }
        ctx.report(d);
    }
    for note in g.geometry().range_notes(mm(w), gap.map(mm)) {
        ctx.report(
            Diagnostic::warning("impedance.out_of_range", format!("{note}: the result is less accurate"))
                .with_hint("see docs/ELECTRICAL.md for each formula's range"),
        );
    }
}

/// Result of an impedance calculation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImpedanceResult {
    /// Copper layer, when the stackup gave the geometry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Track width.
    pub width: Nm,
    /// Gap between the tracks of a differential pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<Nm>,
    /// Single-ended characteristic impedance (of one track).
    pub z0: Quantity,
    /// Differential impedance (with `gap`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdiff: Option<Quantity>,
    /// Effective relative permittivity.
    pub er_eff: Quantity,
    /// Propagation delay per millimeter.
    pub delay_per_mm: Quantity,
    /// Geometry used.
    pub geometry: GeometryInfo,
}

fn calc(layer: Option<String>, g: &LayerGeometry, w: Nm, gap: Option<Nm>) -> ImpedanceResult {
    let geo = g.geometry();
    let line = geo.line(mm(w));
    ImpedanceResult {
        layer,
        width: w,
        gap,
        z0: ohms(line.z0),
        zdiff: gap.map(|s| ohms(geo.differential(mm(w), mm(s)))),
        er_eff: Quantity::from_f64(line.er_eff, 3, Unit::None).unwrap_or(Quantity::int(1, Unit::None)),
        delay_per_mm: Quantity::from_f64(line.delay_ps_per_mm() * 1000.0, 0, Unit::Second)
            .map(|q| Quantity::new(q.mantissa(), q.exp() - 15, Unit::Second))
            .unwrap_or(Quantity::int(0, Unit::Second)),
        geometry: geometry_info(g, gap.is_some()),
    }
}

fn result_text(o: &ImpedanceResult) -> String {
    let m = serde_json::to_value(o.geometry.model).unwrap();
    let mut s = format!(
        "{}{} {}: Z0 {}",
        o.layer.as_ref().map(|l| format!("{l} ")).unwrap_or_default(),
        m.as_str().unwrap(),
        o.width,
        o.z0
    );
    if let (Some(g), Some(z)) = (o.gap, o.zdiff) {
        s += &format!(", gap {g}: Zdiff {z}");
    }
    s += &format!(
        "  (εeff {}, {}/mm; H {}{}, T {}, εr {})",
        o.er_eff,
        o.delay_per_mm,
        o.geometry.height,
        o.geometry.height2.map(|h| format!("/{h}")).unwrap_or_default(),
        o.geometry.copper,
        o.geometry.er
    );
    s
}

/// Characteristic impedance of a track (or an edge-coupled differential pair with `gap`) on a
/// copper layer, from the stackup: outer layers are microstrips, inner layers striplines.
/// Explicit `height`/`height2`/`copper`/`er`/`model` override the stackup.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Calc {
    /// Track width ("0.2mm").
    pub width: Nm,
    /// Gap between the two tracks of a differential pair (gives the differential impedance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<Nm>,
    /// Copper layer (default F.Cu, or none when `height` and `er` are given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Line model, overriding the layer's (microstrip, embedded_microstrip, stripline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// Dielectric height to the (nearer) reference plane, overriding the stackup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Nm>,
    /// Stripline: dielectric height to the other plane. Embedded microstrip: thickness of
    /// the dielectric covering the trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height2: Option<Nm>,
    /// Copper thickness, overriding the stackup ("35um").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copper: Option<Nm>,
    /// Relative permittivity, overriding the stackup ("4.3").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub er: Option<Quantity>,
}

impl Command for Calc {
    const NAME: &'static str = "impedance.calc";
    const SUMMARY: &'static str = "Impedance of a track or differential pair on a layer, from the stackup";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["width"];
    type Output = ImpedanceResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<ImpedanceResult, CommandError> {
        if self.width <= Nm::ZERO || self.gap.is_some_and(|g| g <= Nm::ZERO) {
            return Err(CommandError::invalid_args("impedance.invalid_width", "width and gap must be positive"));
        }
        let args = GeoArgs {
            layer: self.layer.as_deref(),
            model: self.model,
            height: self.height,
            height2: self.height2,
            copper: self.copper,
            er: self.er,
        };
        let (layer, g) = resolve(ctx.project()?, &args)?;
        report_geometry(ctx, layer.as_deref(), &g, self.width, self.gap);
        Ok(calc(layer, &g, self.width, self.gap))
    }

    fn summarize(o: &ImpedanceResult) -> String {
        result_text(o)
    }
}

/// Track width for a target impedance on a layer (differential with `gap`), from the stackup.
/// With `netclass`, writes the width (and gap and target) into that net class, creating it if
/// needed: `track_width` + `impedance`, or `diff_pair_width` + `diff_pair_gap` +
/// `diff_impedance`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Solve {
    /// Target impedance ("50ohm", "90"): single-ended, or differential with `gap`.
    pub target: Quantity,
    /// Differential pair gap. With `differential` and no gap, the net class's `diff_pair_gap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<Nm>,
    /// Solve for a differential pair (implied by `gap`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub differential: bool,
    /// Copper layer (default F.Cu, or none when `height` and `er` are given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Line model, overriding the layer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// Dielectric height to the (nearer) reference plane, overriding the stackup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Nm>,
    /// Stripline: height to the other plane. Embedded microstrip: cover thickness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height2: Option<Nm>,
    /// Copper thickness, overriding the stackup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copper: Option<Nm>,
    /// Relative permittivity, overriding the stackup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub er: Option<Quantity>,
    /// Net class to write the width into (created if missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netclass: Option<String>,
}

/// Result of `impedance.solve`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Solved {
    /// Target.
    pub target: Quantity,
    /// Width found, rounded to 1 µm, with the impedance it gives.
    #[serde(flatten)]
    pub result: ImpedanceResult,
    /// Net class updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub netclass: Option<String>,
}

impl Command for Solve {
    const NAME: &'static str = "impedance.solve";
    const SUMMARY: &'static str =
        "Track width for a target impedance (single or differential) on a layer; optionally into a net class";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["target"];
    type Output = Solved;

    fn run(self, ctx: &mut Context<'_>) -> Result<Solved, CommandError> {
        let target = expect_unit(self.target, Unit::Ohm, "target")?;
        positive(&target, "target")?;
        let class_name = match &self.netclass {
            Some(n) => {
                let n = n.trim().to_string();
                if n.is_empty() || n.contains(char::is_whitespace) {
                    return Err(CommandError::invalid_args("netclass.invalid_name", "class names have no spaces"));
                }
                Some(n)
            }
            None => None,
        };
        let p = ctx.project()?;
        let class_gap = class_name.as_ref().and_then(|n| p.circuit().netclasses.get(n)).and_then(|c| c.diff_pair_gap);
        let gap = match (self.gap, self.differential) {
            (Some(g), _) => Some(g),
            (None, true) => Some(class_gap.ok_or_else(|| {
                CommandError::invalid_args("impedance.no_gap", "a differential pair needs a gap")
                    .with_hint("give `gap`, or set the net class's diff_pair_gap (netclass.set)")
            })?),
            (None, false) => None,
        };
        if gap.is_some_and(|g| g <= Nm::ZERO) {
            return Err(CommandError::invalid_args("impedance.invalid_width", "the gap must be positive"));
        }
        let args = GeoArgs {
            layer: self.layer.as_deref(),
            model: self.model,
            height: self.height,
            height2: self.height2,
            copper: self.copper,
            er: self.er,
        };
        let (layer, g) = resolve(p, &args)?;
        let geo = g.geometry();
        let Some(w) = geo.solve_width(target.to_f64(), gap.map(mm)) else {
            let lo = geo.impedance(geo.h * 100.0, gap.map(mm));
            let hi = geo.impedance(geo.h / 1000.0, gap.map(mm));
            return Err(CommandError::invalid_args(
                "impedance.unreachable",
                format!("{target} cannot be reached with this geometry (about {} to {})", ohms(lo), ohms(hi)),
            )
            .with_hint("change the layer, dielectric height or gap, or the target"));
        };
        let width = Nm::from_um(((w * 1000.0).round() as i64).max(1));
        report_geometry(ctx, layer.as_deref(), &g, width, gap);
        let result = calc(layer, &g, width, gap);
        if let Some(name) = &class_name {
            let c: &mut NetClass = ctx.project_mut()?.circuit_mut().netclasses.entry(name.clone()).or_default();
            match gap {
                Some(s) => {
                    c.diff_pair_width = Some(width);
                    c.diff_pair_gap = Some(s);
                    c.diff_impedance = Some(target);
                }
                None => {
                    c.track_width = Some(width);
                    c.impedance = Some(target);
                }
            }
            for d in crate::drc::netclass_conflicts(ctx.project()?) {
                if d.subjects.contains(&ObjectRef::Name(name.clone())) {
                    ctx.report(d);
                }
            }
        }
        Ok(Solved { target, result, netclass: class_name })
    }

    fn summarize(o: &Solved) -> String {
        let mut s = format!("{} -> width {}\n{}", o.target, o.result.width, result_text(&o.result));
        if let Some(c) = &o.netclass {
            s += &format!("\nnet class `{c}` updated");
        }
        s
    }
}

// ---------------------------------------------------------------------------------------------
// Current

/// Required width on one layer.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayerWidth {
    /// Copper layer.
    pub layer: String,
    /// Copper thickness.
    pub copper: Nm,
    /// Minimum track width (rounded up to 1 µm).
    pub width: Nm,
}

/// Result of `current.width`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CurrentWidths {
    /// Current.
    pub current: Quantity,
    /// Temperature rise.
    pub temp_rise: Quantity,
    /// Method.
    pub method: Method,
    /// Required width per copper layer.
    pub widths: Vec<LayerWidth>,
    /// Net class whose track width was raised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub netclass: Option<String>,
}

/// Minimum track width for a current (IPC-2152 chart fit by default; approximate, see
/// docs/ELECTRICAL.md), per copper layer. The current and temperature rise come from the
/// arguments or from a net (`net.set --current`). With `netclass`, raises the class's track
/// width to the widest requirement (never lowers it).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CurrentWidth {
    /// Current, RMS ("2A").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Quantity>,
    /// Net whose current and temperature rise to use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Allowed temperature rise ("10C", default the net's, else 10 °C).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_rise: Option<Quantity>,
    /// Only this copper layer (default: every layer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Method: ipc2152 (default) or ipc2221.
    #[serde(default)]
    pub method: Method,
    /// Net class whose track width to raise to the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netclass: Option<String>,
}

impl Command for CurrentWidth {
    const NAME: &'static str = "current.width";
    const SUMMARY: &'static str = "Minimum track width for a current and temperature rise (IPC-2152), per layer";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["current"];
    type Output = CurrentWidths;

    fn run(self, ctx: &mut Context<'_>) -> Result<CurrentWidths, CommandError> {
        let p = ctx.project()?;
        let net = match &self.net {
            Some(n) => Some(&p.circuit().nets[&super::net::net_name(p, n)?]),
            None => None,
        };
        let current = match (self.current, net.and_then(|n| n.current)) {
            (Some(c), _) => expect_unit(c, Unit::Ampere, "current")?,
            (None, Some(c)) => c,
            (None, None) => {
                return Err(CommandError::invalid_args("current.missing", "give a `current`, or a `net` that has one")
                    .with_hint("set a net's current with `net.set NET --current 2A`"));
            }
        };
        positive(&current, "current")?;
        let rise = match (self.temp_rise, net.and_then(|n| n.temp_rise)) {
            (Some(r), _) => expect_unit(r, Unit::Celsius, "temp_rise")?,
            (None, Some(r)) => r,
            (None, None) => electrical::DEFAULT_TEMP_RISE,
        };
        positive(&rise, "temp_rise")?;
        let s = &p.board().stackup;
        let layers = match &self.layer {
            Some(l) => {
                if electrical::layer_index(s, l).is_none() {
                    return Err(layer_error(p, l, GeometryError::UnknownLayer));
                }
                vec![l.clone()]
            }
            None => s.copper_names(),
        };
        let widths: Vec<LayerWidth> = layers
            .iter()
            .map(|l| LayerWidth {
                layer: l.clone(),
                copper: electrical::copper_thickness(s, l),
                width: electrical::required_width(s, l, self.method, &current, &rise),
            })
            .collect();
        let max = widths.iter().map(|w| w.width).max().unwrap_or(Nm::ZERO);
        if let Some(name) = &self.netclass {
            let name = name.trim();
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(CommandError::invalid_args("netclass.invalid_name", "class names have no spaces"));
            }
            let c = ctx.project_mut()?.circuit_mut().netclasses.entry(name.to_string()).or_default();
            if c.track_width.is_none_or(|w| w < max) {
                c.track_width = Some(max);
            }
        }
        Ok(CurrentWidths { current, temp_rise: rise, method: self.method, widths, netclass: self.netclass })
    }

    fn summarize(o: &CurrentWidths) -> String {
        let m = serde_json::to_value(o.method).unwrap();
        let mut s = format!("{} with {} rise ({}):", o.current, o.temp_rise, m.as_str().unwrap());
        for w in &o.widths {
            s += &format!("\n  {:<8} {} copper: {}", w.layer, w.copper, w.width);
        }
        if let Some(c) = &o.netclass {
            s += &format!("\nnet class `{c}` track width checked");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantity_units() {
        let q = Quantity::parse("50").unwrap();
        assert_eq!(expect_unit(q, Unit::Ohm, "t").unwrap().unit, Unit::Ohm);
        assert!(expect_unit(Quantity::parse("2A").unwrap(), Unit::Ohm, "t").is_err());
    }
}
