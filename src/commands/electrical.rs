//! `board.stackup`, `board.dielectric`, `impedance.*`, `current.*`: stackup dielectrics,
//! transmission line impedance and IPC-2152 track widths (`docs/ELECTRICAL.md`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::electrical::fieldsolver::{self, Accuracy};
use crate::electrical::{
    self, Broadside, FieldOptions, GeometryError, LayerGeometry, MaskSpec, Model, Solver, current::Method,
};
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
        GeometryError::NoPairLayer => CommandError::invalid_args(
            "impedance.no_pair_layer",
            format!("`{layer}` is the bottom copper layer: a broadside pair uses the next layer down"),
        )
        .with_subject(ObjectRef::Layer(layer.to_string()))
        .with_hint("give the upper layer of the pair, or the geometry explicitly (`height`, `er`, `broadside_gap`)"),
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

/// Solder mask in a field-solver cross-section.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MaskInfo {
    /// Thickness over the laminate.
    pub thickness: Nm,
    /// Thickness over the copper.
    pub over_copper: Nm,
    /// Relative permittivity.
    pub er: Quantity,
}

/// The second trace of a broadside-coupled pair.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BroadsideInfo {
    /// Copper layer of the second trace (from the stackup).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Dielectric thickness between the two traces.
    pub gap: Nm,
    /// Its relative permittivity.
    pub er: Quantity,
}

/// Geometry used for a calculation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct GeometryInfo {
    /// Line model. With a broadside pair: the model of the pair (stripline between two planes,
    /// microstrip when the upper trace is on an outer layer).
    pub model: Model,
    /// Dielectric height to the (nearer) reference plane. Broadside: from the lower trace.
    pub height: Nm,
    /// Stripline: height to the other plane; embedded microstrip: cover thickness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height2: Option<Nm>,
    /// Copper thickness.
    pub copper: Nm,
    /// Relative permittivity (closed forms: the thickness-weighted mean of a stripline's two
    /// dielectrics; field solver: the dielectric below the trace).
    pub er: Quantity,
    /// Field solver: permittivity of the dielectric around and above the trace, when it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub er2: Option<Quantity>,
    /// Top-edge inset per side (trapezoidal etch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etch: Option<Nm>,
    /// Solder mask.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<MaskInfo>,
    /// Coplanar ground gap (grounded coplanar waveguide).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coplanar_gap: Option<Nm>,
    /// Broadside-coupled pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadside: Option<BroadsideInfo>,
    /// Reference planes (from the stackup).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// Whether the stackup's dielectrics were assumed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub assumed_stackup: bool,
    /// Formula or method used.
    pub formula: String,
}

/// Overrides of the stackup geometry and solver options, shared by `impedance.calc` and
/// `impedance.solve`.
struct GeoArgs<'a> {
    layer: Option<&'a str>,
    model: Option<Model>,
    height: Option<Nm>,
    height2: Option<Nm>,
    copper: Option<Nm>,
    er: Option<Quantity>,
    method: Option<Solver>,
    er2: Option<Quantity>,
    etch: Option<Nm>,
    mask: Option<Nm>,
    mask_copper: Option<Nm>,
    mask_er: Option<Quantity>,
    coplanar_gap: Option<Nm>,
    broadside: bool,
    broadside_gap: Option<Nm>,
}

impl GeoArgs<'_> {
    fn is_broadside(&self) -> bool {
        self.broadside || self.broadside_gap.is_some()
    }

    /// Options that only the field solver models.
    fn field_only(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.etch.is_some_and(|e| e > Nm::ZERO) {
            v.push("etch");
        }
        if self.mask.is_some() {
            v.push("mask");
        }
        if self.coplanar_gap.is_some() {
            v.push("coplanar_gap");
        }
        if self.is_broadside() {
            v.push("broadside");
        }
        if self.er2.is_some() {
            v.push("er2");
        }
        v
    }
}

/// A resolved cross-section: geometry, solver and field-solver options.
struct Resolved {
    layer: Option<String>,
    g: LayerGeometry,
    method: Solver,
    opts: FieldOptions,
    mask_er_assumed: bool,
}

fn formula(r: &Resolved, diff: bool) -> String {
    if r.method == Solver::FieldSolver {
        let mut s = String::from("2D quasi-static boundary element field solver");
        let mut parts = Vec::new();
        if r.g.broadside.is_some() {
            parts.push("broadside-coupled pair");
        } else if diff {
            parts.push("edge-coupled pair");
        }
        if r.opts.coplanar_gap.is_some() {
            parts.push("coplanar ground");
        }
        if r.opts.mask.is_some() {
            parts.push("solder mask");
        }
        if r.opts.etch > Nm::ZERO {
            parts.push("trapezoidal etch");
        }
        if !parts.is_empty() {
            s += &format!(" ({})", parts.join(", "));
        }
        if diff || r.g.broadside.is_some() {
            s += "; Zdiff and Zcomm from the mixed-mode characteristic impedance matrix";
        }
        return s;
    }
    let base = match r.g.model {
        Model::Microstrip => "Hammerstad-Jensen microstrip (with thickness correction)",
        Model::EmbeddedMicrostrip => "IPC-2141A embedded microstrip",
        Model::Stripline => "Wheeler stripline (asymmetric: parallel combination)",
    };
    if diff {
        let c = match r.g.model {
            Model::Stripline => "IPC-2141 edge-coupled stripline: Zdiff = 2 Z0 (1 - 0.347 exp(-2.9 S/b))",
            _ => "IPC-2141 edge-coupled microstrip: Zdiff = 2 Z0 (1 - 0.48 exp(-0.96 S/H))",
        };
        format!("{base}; {c}")
    } else {
        base.to_string()
    }
}

fn invalid_geometry(msg: impl Into<String>) -> CommandError {
    CommandError::invalid_args("impedance.invalid_geometry", msg)
}

fn positive_nm(v: Option<Nm>, field: &str) -> Result<(), CommandError> {
    match v {
        Some(x) if x <= Nm::ZERO => Err(invalid_geometry(format!("`{field}` must be positive"))),
        _ => Ok(()),
    }
}

fn permittivity(q: Quantity, field: &str) -> Result<f64, CommandError> {
    let e = expect_unit(q, Unit::None, field)?.to_f64();
    if e < 1.0 {
        return Err(invalid_geometry(format!("`{field}` (relative permittivity) must be at least 1")));
    }
    Ok(e)
}

/// Resolves the geometry: the stackup's for the layer, with explicit values overriding it,
/// and the solver.
fn resolve(p: &Project, a: &GeoArgs) -> Result<Resolved, CommandError> {
    let field_only = a.field_only();
    let method = match a.method {
        Some(Solver::ClosedForm) if !field_only.is_empty() => {
            return Err(CommandError::invalid_args(
                "impedance.needs_field_solver",
                format!("the closed-form models do not cover {}", field_only.join(", ")),
            )
            .with_hint("use `method: field_solver` (or leave `method` unset: these options select it)"));
        }
        Some(m) => m,
        None if field_only.is_empty() => Solver::ClosedForm,
        None => Solver::FieldSolver,
    };
    let broadside = a.is_broadside();
    let explicit = a.height.is_some() && a.er.is_some();
    let layer = match a.layer {
        Some(l) => Some(l.to_string()),
        None if explicit => None,
        None => Some("F.Cu".to_string()),
    };
    let stack = &p.board().stackup;
    let base = match &layer {
        Some(l) => {
            let g =
                if broadside { electrical::broadside_geometry(stack, l) } else { electrical::layer_geometry(stack, l) };
            match g {
                Ok(g) => Some(g),
                Err(GeometryError::NoReference | GeometryError::NoPairLayer) if explicit => None,
                Err(e) => return Err(layer_error(p, l, e)),
            }
        }
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
        er_split: None,
        broadside: None,
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
        let e = permittivity(e, "er")?;
        g.er = e;
        g.er_split = None;
        if let Some(b) = &mut g.broadside {
            b.er = e;
        }
    }
    if broadside && g.broadside.is_none() {
        let Some(gap) = a.broadside_gap else {
            return Err(CommandError::invalid_args(
                "impedance.invalid_geometry",
                "a broadside pair needs `broadside_gap` (or a `layer` whose next copper layer holds the second trace)",
            ));
        };
        g.broadside = Some(Broadside { gap, er: g.er, layer: None });
    }
    if let Some(e) = a.er2 {
        let e = permittivity(e, "er2")?;
        let below = g.er_split.map_or(g.er, |s| s.0);
        g.er_split = Some((below, e));
        if let Some(b) = &mut g.broadside {
            b.er = e;
        }
    }
    if let (Some(b), Some(gap)) = (&mut g.broadside, a.broadside_gap) {
        b.gap = gap;
    }
    if g.h <= Nm::ZERO || g.t < Nm::ZERO || g.er < 1.0 {
        return Err(invalid_geometry(
            "the dielectric height must be positive, the copper thickness not negative, and εr at least 1",
        ));
    }
    match g.model {
        Model::Stripline if g.h2 <= Nm::ZERO => {
            return Err(invalid_geometry("a stripline needs `height2`, the dielectric height to the second plane"));
        }
        Model::EmbeddedMicrostrip if g.h2 <= Nm::ZERO => {
            return Err(invalid_geometry(
                "an embedded microstrip needs `height2`, the thickness of the dielectric covering the trace",
            ));
        }
        _ => {}
    }
    positive_nm(a.coplanar_gap, "coplanar_gap")?;
    positive_nm(a.mask, "mask")?;
    positive_nm(a.mask_copper, "mask_copper")?;
    if let Some(b) = &g.broadside
        && b.gap <= Nm::ZERO
    {
        return Err(invalid_geometry("`broadside_gap` must be positive"));
    }
    if a.etch.is_some_and(|e| e < Nm::ZERO) {
        return Err(invalid_geometry("`etch` must not be negative"));
    }
    if broadside && a.coplanar_gap.is_some() {
        return Err(invalid_geometry("coplanar grounds are not modeled with a broadside pair"));
    }
    if (a.mask_copper.is_some() || a.mask_er.is_some()) && a.mask.is_none() {
        return Err(invalid_geometry("`mask_copper` and `mask_er` need `mask` (the mask thickness)"));
    }
    let mask = match a.mask {
        Some(t) => {
            if g.model == Model::Stripline {
                return Err(invalid_geometry("a stripline has no solder mask (it is between two planes)")
                    .with_hint("solder mask applies to outer layers: microstrip or embedded microstrip"));
            }
            let er = match a.mask_er {
                Some(e) => permittivity(e, "mask_er")?,
                None => electrical::DEFAULT_MASK_ER,
            };
            Some(MaskSpec { thickness: t, over_copper: a.mask_copper.unwrap_or(t), er })
        }
        None => None,
    };
    let opts = FieldOptions { etch: a.etch.unwrap_or(Nm::ZERO), mask, coplanar_gap: a.coplanar_gap };
    Ok(Resolved { layer, g, method, opts, mask_er_assumed: a.mask.is_some() && a.mask_er.is_none() })
}

fn quantity(v: f64, decimals: i8, unit: Unit) -> Quantity {
    Quantity::from_f64(v, decimals, unit).unwrap_or(Quantity::int(0, unit))
}

fn geometry_info(r: &Resolved, diff: bool) -> GeometryInfo {
    let g = &r.g;
    let field = r.method == Solver::FieldSolver;
    let (er, er2) = match (field, g.er_split) {
        (true, Some((a, b))) => (a, (a != b).then_some(b)),
        _ => (g.er, None),
    };
    let dimensionless = |v: f64| Quantity::from_f64(v, 3, Unit::None).unwrap_or(Quantity::int(1, Unit::None));
    GeometryInfo {
        model: g.model,
        height: g.h,
        height2: (g.model != Model::Microstrip).then_some(g.h2),
        copper: g.t,
        er: dimensionless(er),
        er2: er2.map(dimensionless),
        etch: (r.opts.etch > Nm::ZERO).then_some(r.opts.etch),
        mask: r.opts.mask.map(|m| MaskInfo {
            thickness: m.thickness,
            over_copper: m.over_copper,
            er: dimensionless(m.er),
        }),
        coplanar_gap: r.opts.coplanar_gap,
        broadside: g.broadside.as_ref().map(|b| BroadsideInfo {
            layer: b.layer.clone(),
            gap: b.gap,
            er: dimensionless(b.er),
        }),
        references: g.references.clone(),
        assumed_stackup: g.assumed,
        formula: formula(r, diff),
    }
}

fn ohms(z: f64) -> Quantity {
    Quantity::from_f64(z, 2, Unit::Ohm).unwrap_or(Quantity::int(0, Unit::Ohm))
}

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

fn report_geometry(ctx: &mut Context<'_>, r: &Resolved, w: Nm, gap: Option<Nm>) {
    if r.g.assumed {
        let mut d = Diagnostic::warning(
            "impedance.stackup_assumed",
            "the stackup's dielectrics are not specified: the result uses an assumed stackup",
        )
        .with_hint(
            "set the real dielectrics from your fab's stackup with `board.dielectric` (board.stackup shows them)",
        );
        if let Some(l) = &r.layer {
            d = d.with_subject(ObjectRef::Layer(l.to_string()));
        }
        ctx.report(d);
    }
    if r.mask_er_assumed {
        ctx.report(
            Diagnostic::info(
                "impedance.mask_er_assumed",
                format!("solder mask εr not given: assuming {}", electrical::DEFAULT_MASK_ER),
            )
            .with_hint("give `mask_er` from the mask's datasheet (or your fab's stackup)"),
        );
    }
    if r.method == Solver::ClosedForm {
        for note in r.g.geometry().range_notes(mm(w), gap.map(mm)) {
            ctx.report(
                Diagnostic::warning("impedance.out_of_range", format!("{note}: the result is less accurate"))
                    .with_hint(
                        "see docs/ELECTRICAL.md for each formula's range, or use `method: field_solver`, which has none",
                    ),
            );
        }
    }
}

/// Result of an impedance calculation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImpedanceResult {
    /// Copper layer, when the stackup gave the geometry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Track width (bottom width with an etch).
    pub width: Nm,
    /// Gap between the tracks of an edge-coupled differential pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<Nm>,
    /// How the impedance was computed.
    pub method: Solver,
    /// Single-ended characteristic impedance (of one track alone).
    pub z0: Quantity,
    /// Differential impedance (pair).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdiff: Option<Quantity>,
    /// Common-mode impedance (pair, field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zcomm: Option<Quantity>,
    /// Odd-mode impedance, Zdiff / 2 (pair, field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z_odd: Option<Quantity>,
    /// Even-mode impedance, 2 Zcomm (pair, field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub z_even: Option<Quantity>,
    /// Effective relative permittivity (of one track alone).
    pub er_eff: Quantity,
    /// Effective permittivity of the odd (differential) mode (pair, field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub er_eff_odd: Option<Quantity>,
    /// Effective permittivity of the even (common) mode (pair, field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub er_eff_even: Option<Quantity>,
    /// Propagation delay per millimeter.
    pub delay_per_mm: Quantity,
    /// Capacitance per meter of one track alone (√εeff / (Z0 c)).
    pub c_per_m: Quantity,
    /// Inductance per meter of one track alone (Z0 √εeff / c).
    pub l_per_m: Quantity,
    /// Geometry used.
    pub geometry: GeometryInfo,
}

/// `v · 10^-exp` rounded to `decimals`, as a quantity of `v` (keeps small values exact).
fn scaled(v: f64, exp: i8, decimals: i8, unit: Unit) -> Quantity {
    Quantity::from_f64(v * 10f64.powi(-i32::from(exp)), decimals, unit)
        .map(|q| Quantity::new(q.mantissa(), q.exp() + exp, unit))
        .unwrap_or(Quantity::int(0, unit))
}

fn solver_error(e: fieldsolver::SolveError) -> CommandError {
    invalid_geometry(format!("field solver: {e}"))
}

fn calc(r: &Resolved, w: Nm, gap: Option<Nm>) -> Result<ImpedanceResult, CommandError> {
    const C: f64 = 299_792_458.0;
    let dimensionless = |v: f64| quantity(v, 3, Unit::None);
    let (line, zdiff, pair) = match r.method {
        Solver::ClosedForm => {
            let geo = r.g.geometry();
            (geo.line(mm(w)), gap.map(|s| geo.differential(mm(w), mm(s))), None)
        }
        Solver::FieldSolver => {
            let st = r.g.structure(&r.opts);
            let ev = st.evaluate(mm(w), gap.map(mm), Accuracy::default()).map_err(solver_error)?;
            (ev.line.line(), ev.pair.map(|p| p.zdiff), ev.pair)
        }
    };
    let root = line.er_eff.sqrt();
    Ok(ImpedanceResult {
        layer: r.layer.clone(),
        width: w,
        gap,
        method: r.method,
        z0: ohms(line.z0),
        zdiff: zdiff.map(ohms),
        zcomm: pair.map(|p| ohms(p.zcomm)),
        z_odd: pair.map(|p| ohms(p.z_odd)),
        z_even: pair.map(|p| ohms(p.z_even)),
        er_eff: dimensionless(line.er_eff),
        er_eff_odd: pair.map(|p| dimensionless(p.er_eff_odd)),
        er_eff_even: pair.map(|p| dimensionless(p.er_eff_even)),
        delay_per_mm: scaled(line.delay_ps_per_mm() * 1e-12, -15, 0, Unit::Second),
        c_per_m: scaled(root / (line.z0 * C), -12, 1, Unit::Farad),
        l_per_m: scaled(line.z0 * root / C, -9, 1, Unit::Henry),
        geometry: geometry_info(r, gap.is_some() || r.g.broadside.is_some()),
    })
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
    if let Some(z) = o.zdiff {
        match (o.gap, &o.geometry.broadside) {
            (Some(g), _) => s += &format!(", gap {g}: Zdiff {z}"),
            (None, Some(b)) => s += &format!(", broadside {}: Zdiff {z}", b.gap),
            (None, None) => s += &format!(": Zdiff {z}"),
        }
    }
    if let Some(z) = o.zcomm {
        s += &format!(", Zcomm {z}");
    }
    s += &format!(
        "  (εeff {}, {}/mm; H {}{}, T {}, εr {}{})",
        o.er_eff,
        o.delay_per_mm,
        o.geometry.height,
        o.geometry.height2.map(|h| format!("/{h}")).unwrap_or_default(),
        o.geometry.copper,
        o.geometry.er,
        o.geometry.er2.map(|e| format!("/{e}")).unwrap_or_default()
    );
    if o.method == Solver::FieldSolver {
        s += "  [field solver]";
    }
    s
}

/// Characteristic impedance of a track (or a differential pair: edge-coupled with `gap`,
/// broadside-coupled with `broadside`) on a copper layer, from the stackup: outer layers are
/// microstrips, inner layers striplines. Explicit `height`/`height2`/`copper`/`er`/`model`
/// override the stackup. `method: field_solver` (selected automatically by `etch`, `mask`,
/// `coplanar_gap`, `broadside` and `er2`) solves the cross-section numerically.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Calc {
    /// Track width ("0.2mm"); the bottom width with an `etch`.
    pub width: Nm,
    /// Gap between the two tracks of an edge-coupled differential pair (gives the
    /// differential impedance).
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
    /// `closed_form` (published formulas, default) or `field_solver` (2D boundary element
    /// solver; default when a field-solver-only option is given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Solver>,
    /// Field solver: permittivity of the dielectric around and above the trace (stripline
    /// upper dielectric, embedded cover, broadside separation) when it differs from `er`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub er2: Option<Quantity>,
    /// Field solver: trapezoidal etch, the top edge inset on each side ("10um").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etch: Option<Nm>,
    /// Field solver: solder mask thickness over the laminate ("25um"), outer layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Nm>,
    /// Field solver: solder mask thickness over the copper (default: `mask`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_copper: Option<Nm>,
    /// Field solver: solder mask permittivity (default 3.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_er: Option<Quantity>,
    /// Field solver: coplanar ground on both sides at this gap (grounded coplanar waveguide).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coplanar_gap: Option<Nm>,
    /// Field solver: broadside-coupled pair with a trace on the next copper layer down
    /// (stackup), or `broadside_gap` above the first (explicit geometry).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub broadside: bool,
    /// Field solver: dielectric thickness between the two traces of a broadside pair,
    /// overriding the stackup (implies `broadside`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadside_gap: Option<Nm>,
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
            method: self.method,
            er2: self.er2,
            etch: self.etch,
            mask: self.mask,
            mask_copper: self.mask_copper,
            mask_er: self.mask_er,
            coplanar_gap: self.coplanar_gap,
            broadside: self.broadside,
            broadside_gap: self.broadside_gap,
        };
        if self.gap.is_some() && args.is_broadside() {
            return Err(CommandError::invalid_args(
                "impedance.invalid_geometry",
                "a pair is either edge-coupled (`gap`) or broadside, not both",
            ));
        }
        let r = resolve(ctx.project()?, &args)?;
        let o = calc(&r, self.width, self.gap)?;
        report_geometry(ctx, &r, self.width, self.gap);
        Ok(o)
    }

    fn summarize(o: &ImpedanceResult) -> String {
        result_text(o)
    }
}

/// Track width for a target impedance on a layer (differential with `gap` or `broadside`),
/// from the stackup. With `netclass`, writes the width (and gap and target) into that net
/// class, creating it if needed: `track_width` + `impedance`, or `diff_pair_width` +
/// `diff_pair_gap` + `diff_impedance`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Solve {
    /// Target impedance ("50ohm", "90"): single-ended, or differential with `gap` or
    /// `broadside`.
    pub target: Quantity,
    /// Differential pair gap. With `differential` and no gap, the net class's `diff_pair_gap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<Nm>,
    /// Solve for a differential pair (implied by `gap` and `broadside`).
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
    /// `closed_form` (default) or `field_solver` (default when a field-solver-only option is
    /// given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Solver>,
    /// Field solver: permittivity around and above the trace when it differs from `er`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub er2: Option<Quantity>,
    /// Field solver: trapezoidal etch, the top edge inset on each side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etch: Option<Nm>,
    /// Field solver: solder mask thickness over the laminate (outer layers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Nm>,
    /// Field solver: solder mask thickness over the copper (default: `mask`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_copper: Option<Nm>,
    /// Field solver: solder mask permittivity (default 3.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_er: Option<Quantity>,
    /// Field solver: coplanar ground on both sides at this gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coplanar_gap: Option<Nm>,
    /// Field solver: broadside-coupled pair with the next copper layer down.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub broadside: bool,
    /// Field solver: dielectric between the two traces of a broadside pair (implies
    /// `broadside`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadside_gap: Option<Nm>,
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
        let args = GeoArgs {
            layer: self.layer.as_deref(),
            model: self.model,
            height: self.height,
            height2: self.height2,
            copper: self.copper,
            er: self.er,
            method: self.method,
            er2: self.er2,
            etch: self.etch,
            mask: self.mask,
            mask_copper: self.mask_copper,
            mask_er: self.mask_er,
            coplanar_gap: self.coplanar_gap,
            broadside: self.broadside,
            broadside_gap: self.broadside_gap,
        };
        let broadside = args.is_broadside();
        let p = ctx.project()?;
        let class_gap = class_name.as_ref().and_then(|n| p.circuit().netclasses.get(n)).and_then(|c| c.diff_pair_gap);
        let gap = match (self.gap, self.differential && !broadside) {
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
        if gap.is_some() && broadside {
            return Err(CommandError::invalid_args(
                "impedance.invalid_geometry",
                "a pair is either edge-coupled (`gap`) or broadside, not both",
            ));
        }
        let r = resolve(p, &args)?;
        let geo = r.g.geometry();
        let found = match r.method {
            Solver::ClosedForm => geo.solve_width(target.to_f64(), gap.map(mm)),
            Solver::FieldSolver => {
                let st = r.g.structure(&r.opts);
                // Start from the closed-form width of the plain line (half the target for a
                // broadside pair, whose closed form does not exist).
                let start_target = if broadside { target.to_f64() / 2.0 } else { target.to_f64() };
                let start = geo
                    .solve_width(start_target, if broadside { None } else { gap.map(mm) })
                    .unwrap_or(geo.h)
                    .max(st.width_range().0);
                st.solve_width(target.to_f64(), gap.map(mm), start, Accuracy::default()).map_err(solver_error)?
            }
        };
        let Some(w) = found else {
            let (lo, hi) = match r.method {
                Solver::ClosedForm => {
                    (geo.impedance(geo.h * 100.0, gap.map(mm)), geo.impedance(geo.h / 1000.0, gap.map(mm)))
                }
                Solver::FieldSolver => {
                    let st = r.g.structure(&r.opts);
                    let (a, b) = st.width_range();
                    let z = |w| st.impedance(w, gap.map(mm), Accuracy::default()).map_err(solver_error);
                    (z(b)?, z(a)?)
                }
            };
            return Err(CommandError::invalid_args(
                "impedance.unreachable",
                format!("{target} cannot be reached with this geometry (about {} to {})", ohms(lo), ohms(hi)),
            )
            .with_hint("change the layer, dielectric height or gap, or the target"));
        };
        let width = Nm::from_um(((w * 1000.0).round() as i64).max(1));
        let result = calc(&r, width, gap)?;
        report_geometry(ctx, &r, width, gap);
        if let Some(name) = &class_name {
            let c: &mut NetClass = ctx.project_mut()?.circuit_mut().netclasses.entry(name.clone()).or_default();
            if gap.is_some() || broadside {
                c.diff_pair_width = Some(width);
                if let Some(s) = gap {
                    c.diff_pair_gap = Some(s);
                }
                c.diff_impedance = Some(target);
            } else {
                c.track_width = Some(width);
                c.impedance = Some(target);
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
