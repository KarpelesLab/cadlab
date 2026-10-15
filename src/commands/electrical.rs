//! `board.stackup`, `board.dielectric`, `impedance.*`, `current.*`: stackup dielectrics,
//! transmission line impedance and IPC-2152 track widths (`docs/ELECTRICAL.md`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::electrical::fieldsolver::LineMatrices;
use crate::electrical::fieldsolver::{self, Accuracy};
use crate::electrical::{
    self, Broadside, FieldOptions, GeometryError, LayerGeometry, MaskSpec, Model, Solver, current::Method, loss,
};
use crate::model::Project;
use crate::model::circuit::NetClass;
use crate::refs::ObjectRef;
use crate::units::{LengthUnit, Nm};
use crate::value::{Quantity, Unit};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Stackup>()
        .register::<SetDielectric>()
        .register::<FabStackup>()
        .register::<Calc>()
        .register::<Solve>()
        .register::<NetLoss>()
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

/// A reference frequency in Hz: `None` for 0 (no dispersion).
pub(crate) fn frequency_or_zero(q: Quantity, field: &str) -> Result<Option<f64>, CommandError> {
    let v = expect_unit(q, Unit::Hertz, field)?.to_f64();
    if !(0.0..1e15).contains(&v) {
        return Err(CommandError::invalid_args(
            "value.out_of_range",
            format!("`{field}` must be a frequency between 0 and 1 PHz"),
        ));
    }
    Ok((v > 0.0).then_some(v))
}

/// A loss tangent: dimensionless, 0 ≤ tan δ < 1.
fn loss_tangent_arg(q: Quantity, field: &str) -> Result<Quantity, CommandError> {
    let q = expect_unit(q, Unit::None, field)?;
    let v = q.to_f64();
    if !(0.0..1.0).contains(&v) {
        return Err(CommandError::invalid_args(
            "value.out_of_range",
            format!("`{field}` (loss tangent) must be at least 0 and below 1, e.g. 0.02"),
        ));
    }
    Ok(q)
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
    /// Loss tangent (tan δ), when set.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::value::plain")]
    #[schemars(with = "Option<Quantity>")]
    pub loss_tangent: Option<Quantity>,
    /// Frequency at which εr and tan δ are given, when set: the dielectric is dispersive
    /// (Djordjevic–Sarkar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_freq: Option<Quantity>,
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
            loss_tangent: d.loss_tangent,
            ref_freq: d.ref_freq,
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
            .with_hint(
                "apply your fab's published stackup (`board.fab_stackup jlcpcb`), or set each gap with \
                 `board.dielectric --gap N --thickness .. --er ..`",
            ),
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
                "  gap {}  {} εr {}{}{}{}",
                d.gap,
                d.thickness,
                d.er,
                d.loss_tangent.map(|t| format!(" tan δ {}", t.plain())).unwrap_or_default(),
                d.ref_freq.map(|f| format!(" at {f} (Djordjevic–Sarkar)")).unwrap_or_default(),
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

/// Apply a stackup a fab publishes (from its fab profile): copper layer count, board
/// thickness, copper weights and every dielectric (thickness, εr, material) in one step. The
/// values are copied into the board (the design keeps no reference to the fab). Without
/// `stackup`, lists the fab's stackups and changes nothing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FabStackup {
    /// Fab profile (`jlcpcb`).
    pub fab: String,
    /// The fab's stackup name (`JLC04161H-7628`); case-insensitive. Omitted: list them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stackup: Option<String>,
}

/// A stackup a fab publishes, as listed by `board.fab_stackup` without a name.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FabStackupEntry {
    /// The fab's name for it.
    pub id: String,
    /// Copper layers.
    pub layers: usize,
    /// Finished board thickness.
    pub thickness: Nm,
    /// Outer copper thickness.
    pub outer_copper: Nm,
    /// Inner copper thickness.
    pub inner_copper: Nm,
    /// Dielectrics between copper layers, top to bottom.
    pub dielectrics: Vec<crate::fab::FabDielectric>,
    /// The command that applies it.
    pub apply: String,
}

/// Result of `board.fab_stackup`: the board's new stackup, or the fab's stackups to choose
/// from.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FabStackupResult {
    /// The board's stackup after applying one.
    #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<StackupInfo>,
    /// Fab profile listed (without a stackup name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fab: Option<String>,
    /// The fab's stackups (without a stackup name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stackups: Vec<FabStackupEntry>,
}

impl Command for FabStackup {
    const NAME: &'static str = "board.fab_stackup";
    const SUMMARY: &'static str =
        "Apply a fab's published layer stackup (layers, thickness, copper, dielectrics) from its fab profile";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["fab", "stackup"];
    type Output = FabStackupResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<FabStackupResult, CommandError> {
        let ps = super::fab::profiles(ctx);
        let (profile, _) = super::fab::profile(&ps, &self.fab)?;
        let all: Vec<&crate::fab::FabStackup> = profile.processes.iter().flat_map(|q| q.stackups.iter()).collect();
        let listing = || {
            all.iter()
                .map(|st| {
                    let d: Vec<String> =
                        st.dielectrics.iter().map(|d| format!("{} εr {}", d.thickness, d.er)).collect();
                    format!("{} ({} layers, {}: {})", st.id, st.layers(), st.thickness, d.join(" / "))
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        if all.is_empty() {
            return Err(CommandError::not_found(
                "fab.no_stackups",
                format!("fab profile `{}` publishes no stackups", profile.id),
            )
            .with_hint("set the dielectrics from the fab's documentation with `board.dielectric --gap N --thickness .. --er ..`"));
        }
        let Some(want) = &self.stackup else {
            let stackups = all
                .iter()
                .map(|st| FabStackupEntry {
                    id: st.id.clone(),
                    layers: st.layers(),
                    thickness: st.thickness,
                    outer_copper: st.outer_copper,
                    inner_copper: st.inner_copper,
                    dielectrics: st.dielectrics.clone(),
                    apply: format!("board.fab_stackup {} {}", profile.id, st.id),
                })
                .collect();
            return Ok(FabStackupResult { applied: None, fab: Some(profile.id.clone()), stackups });
        };
        let Some(st) = all.iter().find(|st| st.id.eq_ignore_ascii_case(want)) else {
            let near = crate::suggest::did_you_mean(want, all.iter().map(|st| st.id.as_str()), 3);
            return Err(CommandError::not_found(
                "board.unknown_stackup",
                format!("{} has no stackup `{want}`", profile.name),
            )
            .with_suggestions(&near)
            .with_hint_if_none(format!("stackups: {}", listing())));
        };
        let dielectrics: Vec<crate::model::board::Dielectric> = st
            .dielectrics
            .iter()
            .map(|d| crate::model::board::Dielectric {
                thickness: d.thickness,
                er: d.er,
                material: Some(d.material.clone()),
                loss_tangent: None,
                ref_freq: None,
            })
            .collect();
        let s = &mut ctx.project_mut()?.board_mut().stackup;
        s.copper_layers = st.layers() as u8;
        s.thickness = st.thickness;
        s.outer_copper = st.outer_copper;
        s.inner_copper = st.inner_copper;
        s.dielectrics = dielectrics;
        let o = stackup_info(ctx.project()?);
        for d in stackup_diagnostics(&o) {
            ctx.report(d);
        }
        Ok(FabStackupResult { applied: Some(o), fab: None, stackups: vec![] })
    }

    fn summarize(o: &FabStackupResult) -> String {
        if let Some(a) = &o.applied {
            return stackup_text(a);
        }
        let mut lines = vec![format!(
            "{} stackups (apply one with `board.fab_stackup {} NAME`):",
            o.stackups.len(),
            o.fab.as_deref().unwrap_or("FAB")
        )];
        for e in &o.stackups {
            let d: Vec<String> =
                e.dielectrics.iter().map(|d| format!("{} εr {} {}", d.thickness, d.er, d.material)).collect();
            lines.push(format!(
                "  {}: {} layers, {}, copper {} outer / {} inner; {}",
                e.id,
                e.layers,
                e.thickness,
                e.outer_copper,
                e.inner_copper,
                d.join(" / ")
            ));
        }
        lines.join("\n")
    }
}

/// Set the dielectric between two copper layers (thickness, relative permittivity, loss
/// tangent, material), used by the impedance and loss calculations. Unset dielectrics start
/// from the assumed ones.
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
    /// Loss tangent (dissipation factor, tan δ) from the laminate datasheet ("0.02").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_tangent: Option<Quantity>,
    /// Frequency at which `er` and `loss_tangent` are given ("1GHz", the datasheet's test
    /// frequency): makes the dielectric dispersive (Djordjevic–Sarkar wideband Debye model:
    /// εr falls slowly with frequency, tan δ stays nearly flat) in the loss calculations and the
    /// full-wave solver. "0" clears it (constant εr and tan δ).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_freq: Option<Quantity>,
    /// Material, for information ("FR-4 7628"); empty clears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Forget every dielectric (back to the assumed stackup).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear: bool,
}

impl Command for SetDielectric {
    const NAME: &'static str = "board.dielectric";
    const SUMMARY: &'static str = "Set a stackup dielectric (thickness, εr, loss tangent and its frequency, material) for impedance and loss calculations";
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
            let tan = self.loss_tangent.map(|t| loss_tangent_arg(t, "loss_tangent")).transpose()?;
            let ref_freq = match self.ref_freq {
                Some(f) => {
                    let f = expect_unit(f, Unit::Hertz, "ref_freq")?;
                    if f.to_f64() < 0.0 || f.to_f64() >= 1e15 {
                        return Err(CommandError::invalid_args(
                            "value.out_of_range",
                            "`ref_freq` must be a frequency between 0 (clears it) and 1 PHz",
                        ));
                    }
                    Some((f.to_f64() > 0.0).then_some(f))
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
                if let Some(t) = tan {
                    d.loss_tangent = Some(t);
                }
                if let Some(f) = ref_freq {
                    d.ref_freq = f;
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
    loss_tangent: Option<Quantity>,
    mask_loss_tangent: Option<Quantity>,
    ref_freq: Option<Quantity>,
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
    mask_tan_assumed: bool,
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
        tan_d: crate::model::board::DEFAULT_LOSS_TANGENT.to_f64(),
        tan_d_split: None,
        tan_d_assumed: true,
        ref_freq: None,
        ref_freq2: None,
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
        g.broadside = Some(Broadside { gap, er: g.er, layer: None, tan_d: g.tan_d, ref_freq: g.ref_freq });
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
    if let Some(t) = a.loss_tangent {
        let t = loss_tangent_arg(t, "loss_tangent")?.to_f64();
        g.tan_d = t;
        g.tan_d_split = None;
        g.tan_d_assumed = false;
        if let Some(b) = &mut g.broadside {
            b.tan_d = t;
        }
    }
    if let Some(f) = a.ref_freq {
        let f = frequency_or_zero(f, "ref_freq")?;
        g.ref_freq = f;
        g.ref_freq2 = f;
        if let Some(b) = &mut g.broadside {
            b.ref_freq = f;
        }
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
    if (a.mask_copper.is_some() || a.mask_er.is_some() || a.mask_loss_tangent.is_some()) && a.mask.is_none() {
        return Err(invalid_geometry(
            "`mask_copper`, `mask_er` and `mask_loss_tangent` need `mask` (the mask thickness)",
        ));
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
            let tan_d = match a.mask_loss_tangent {
                Some(q) => loss_tangent_arg(q, "mask_loss_tangent")?.to_f64(),
                None => electrical::DEFAULT_MASK_LOSS_TANGENT,
            };
            Some(MaskSpec { thickness: t, over_copper: a.mask_copper.unwrap_or(t), er, tan_d })
        }
        None => None,
    };
    let opts = FieldOptions { etch: a.etch.unwrap_or(Nm::ZERO), mask, coplanar_gap: a.coplanar_gap };
    Ok(Resolved {
        layer,
        g,
        method,
        opts,
        mask_er_assumed: a.mask.is_some() && a.mask_er.is_none(),
        mask_tan_assumed: a.mask.is_some() && a.mask_loss_tangent.is_none(),
    })
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
    /// Frequency-dependent losses (with `freq` or a sweep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<LossResult>,
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
        loss: None,
    })
}

// ---------------------------------------------------------------------------------------------
// Losses

/// Most frequencies in one calculation.
const MAX_FREQS: usize = 1001;

/// Frequency and conductor inputs of a loss calculation (shared by `impedance.calc` and
/// `net.loss`).
struct LossArgs<'a> {
    freq: &'a [Quantity],
    freq_from: Option<Quantity>,
    freq_to: Option<Quantity>,
    freq_points: Option<usize>,
    conductivity: Option<Quantity>,
    roughness: Option<Nm>,
    huray_radius: Option<Nm>,
    huray_ratio: Option<Quantity>,
}

/// Resolved loss inputs.
struct LossParams {
    freqs: Vec<Quantity>,
    sigma: Quantity,
    roughness: loss::Roughness,
    rough_in: (Option<Nm>, Option<Nm>, Option<Quantity>),
}

/// `v` rounded to `digits` significant digits.
fn sig(v: f64, digits: i32, unit: Unit) -> Quantity {
    if v == 0.0 || !v.is_finite() {
        return Quantity::int(0, unit);
    }
    let e = v.abs().log10().floor() as i32 - digits + 1;
    scaled(v, e.clamp(-60, 60) as i8, 0, unit)
}

fn frequency(q: Quantity, field: &str) -> Result<Quantity, CommandError> {
    let q = expect_unit(q, Unit::Hertz, field)?;
    if q.to_f64() <= 0.0 {
        return Err(CommandError::invalid_args("value.not_positive", format!("`{field}` must be positive")));
    }
    Ok(q)
}

/// The loss inputs; `None` when no frequency (and no loss option) is given.
fn loss_params(a: &LossArgs) -> Result<Option<LossParams>, CommandError> {
    let sweep = a.freq_from.is_some() || a.freq_to.is_some() || a.freq_points.is_some();
    let options =
        a.conductivity.is_some() || a.roughness.is_some() || a.huray_radius.is_some() || a.huray_ratio.is_some();
    if a.freq.is_empty() && !sweep {
        if options {
            return Err(CommandError::invalid_args(
                "impedance.no_frequency",
                "conductor options (`conductivity`, `roughness`, `huray_*`) need a frequency",
            )
            .with_hint("give `freq` (one or more), or a sweep with `freq_from`, `freq_to` and `freq_points`"));
        }
        return Ok(None);
    }
    let mut freqs: Vec<Quantity> = a.freq.iter().map(|f| frequency(*f, "freq")).collect::<Result<_, _>>()?;
    if sweep {
        let (Some(from), Some(to)) = (a.freq_from, a.freq_to) else {
            return Err(CommandError::invalid_args(
                "impedance.invalid_sweep",
                "a frequency sweep needs both `freq_from` and `freq_to`",
            ));
        };
        let (from, to) = (frequency(from, "freq_from")?.to_f64(), frequency(to, "freq_to")?.to_f64());
        let n = a.freq_points.unwrap_or(10);
        if to <= from || !(2..=MAX_FREQS).contains(&n) {
            return Err(CommandError::invalid_args(
                "impedance.invalid_sweep",
                format!("a sweep needs `freq_from` < `freq_to` and 2 to {MAX_FREQS} `freq_points`"),
            ));
        }
        // Logarithmic spacing, each frequency rounded to 4 significant digits.
        for i in 0..n {
            let f = from * (to / from).powf(i as f64 / (n - 1) as f64);
            freqs.push(sig(f, 4, Unit::Hertz));
        }
    }
    freqs.sort_by(|x, y| x.cmp_value(y));
    freqs.dedup_by(|x, y| x.cmp_value(y).is_eq());
    if freqs.len() > MAX_FREQS {
        return Err(CommandError::invalid_args(
            "impedance.invalid_sweep",
            format!("at most {MAX_FREQS} frequencies per calculation"),
        ));
    }
    let (sigma, roughness) = conductor_args(a.conductivity, a.roughness, a.huray_radius, a.huray_ratio)?;
    Ok(Some(LossParams { freqs, sigma, roughness, rough_in: (a.roughness, a.huray_radius, a.huray_ratio) }))
}

/// The copper's conductivity (default 58 MS/m) and roughness model from the shared arguments
/// (`conductivity`, `roughness`, `huray_radius`, `huray_ratio`).
pub(crate) fn conductor_args(
    conductivity: Option<Quantity>,
    roughness: Option<Nm>,
    huray_radius: Option<Nm>,
    huray_ratio: Option<Quantity>,
) -> Result<(Quantity, loss::Roughness), CommandError> {
    let sigma = match conductivity {
        Some(q) => {
            let q = match q.unit {
                Unit::None | Unit::Siemens => q.with_unit(Unit::Siemens),
                u => {
                    return Err(CommandError::invalid_args(
                        "value.wrong_unit",
                        format!("`conductivity` is in {}, expected S/m (\"58M\" or \"58MS\")", u.symbol()),
                    ));
                }
            };
            positive(&q, "conductivity")?;
            q
        }
        None => sig(loss::COPPER_CONDUCTIVITY, 3, Unit::Siemens),
    };
    let roughness = match (roughness, huray_radius, huray_ratio) {
        (None, None, None) => loss::Roughness::Smooth,
        (Some(r), None, None) => {
            if r < Nm::ZERO {
                return Err(CommandError::invalid_args("value.not_positive", "`roughness` must not be negative"));
            }
            loss::Roughness::HammerstadJensen { rms: r.to_f64(LengthUnit::Mm) * 1e-3 }
        }
        (None, Some(radius), Some(ratio)) => {
            let ratio = expect_unit(ratio, Unit::None, "huray_ratio")?.to_f64();
            if radius <= Nm::ZERO || ratio < 0.0 {
                return Err(CommandError::invalid_args(
                    "value.not_positive",
                    "`huray_radius` must be positive and `huray_ratio` not negative",
                ));
            }
            loss::Roughness::Huray { radius: radius.to_f64(LengthUnit::Mm) * 1e-3, ratio }
        }
        (None, _, _) => {
            return Err(CommandError::invalid_args(
                "impedance.invalid_roughness",
                "Huray's model needs both `huray_radius` (snowball radius) and `huray_ratio` (snowball surface per flat \
                 area)",
            ));
        }
        (Some(_), _, _) => {
            return Err(CommandError::invalid_args(
                "impedance.invalid_roughness",
                "give either `roughness` (Hammerstad-Jensen, RMS) or the Huray parameters, not both",
            ));
        }
    };
    Ok((sigma, roughness))
}

/// Losses at one frequency.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LossPointInfo {
    /// Frequency.
    pub freq: Quantity,
    /// Attenuation per meter (single line).
    pub alpha_db_per_m: Quantity,
    /// Attenuation per inch (single line).
    pub alpha_db_per_in: Quantity,
    /// Conductor part of the attenuation per meter, R / (2 Z0).
    pub alpha_conductor_db_per_m: Quantity,
    /// Dielectric part of the attenuation per meter, G Z0 / 2.
    pub alpha_dielectric_db_per_m: Quantity,
    /// Differential attenuation per meter of the pair (field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_diff_db_per_m: Option<Quantity>,
    /// Differential attenuation per inch of the pair (field solver).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_diff_db_per_in: Option<Quantity>,
    /// Real part of the complex characteristic impedance.
    pub z0_real: Quantity,
    /// Imaginary part of the complex characteristic impedance.
    pub z0_imag: Quantity,
    /// Resistance per meter (skin effect and roughness, at least the DC resistance).
    pub r_per_m: Quantity,
    /// Inductance per meter (external plus internal).
    pub l_per_m: Quantity,
    /// Conductance per meter (dielectric loss).
    pub g_per_m: Quantity,
    /// Capacitance per meter.
    pub c_per_m: Quantity,
    /// Skin depth in the copper.
    pub skin_depth: Nm,
    /// Roughness loss multiplier (with `roughness` or the Huray parameters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness_factor: Option<Quantity>,
}

/// Frequency-dependent losses of a line.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LossResult {
    /// Copper conductivity (S/m).
    pub conductivity: Quantity,
    /// Loss tangent of the dielectric (below the trace; closed forms: the mean of a
    /// stripline's two dielectrics, weighted by εr × thickness).
    pub loss_tangent: String,
    /// Field solver: loss tangent around and above the trace, when it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss_tangent2: Option<String>,
    /// Solder mask loss tangent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask_loss_tangent: Option<String>,
    /// RMS roughness (Hammerstad-Jensen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness: Option<Nm>,
    /// Huray snowball radius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub huray_radius: Option<Nm>,
    /// Huray surface ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub huray_ratio: Option<Quantity>,
    /// Frequency at which εr and tan δ are given (Djordjevic–Sarkar dispersive dielectric),
    /// when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_freq: Option<Quantity>,
    /// How the conductor loss was computed.
    pub conductor_method: String,
    /// How the dielectric loss was computed.
    pub dielectric_method: String,
    /// Losses per frequency, ascending.
    pub points: Vec<LossPointInfo>,
}

/// The frequency-independent loss description of a resolved line: the single trace's
/// matrices, and the pair's (field solver) when there is one.
fn loss_matrices(r: &Resolved, w: Nm, gap: Option<Nm>) -> Result<(LineMatrices, Option<LineMatrices>), CommandError> {
    if r.g.t <= Nm::ZERO {
        return Err(CommandError::invalid_args(
            "impedance.loss_needs_copper",
            "conductor loss needs a copper thickness (a zero-thickness strip has no skin-effect model)",
        )
        .with_hint("give `copper` (e.g. 35um), or use the stackup's copper"));
    }
    match r.method {
        Solver::ClosedForm => {
            let m = loss::closed_form_dispersive(&r.g.geometry(), mm(w), r.g.tan_d, r.g.ref_freq.or(r.g.ref_freq2))
                .expect("thickness checked");
            Ok((m, None))
        }
        Solver::FieldSolver => {
            let st = r.g.structure(&r.opts);
            let tans = r.g.loss_tangents(&r.opts);
            let acc = Accuracy::default();
            let line = st.line_matrices(mm(w), gap.map(mm), false, &tans, acc).map_err(solver_error)?;
            let pair = if st.is_pair(gap.map(mm)) {
                Some(st.line_matrices(mm(w), gap.map(mm), true, &tans, acc).map_err(solver_error)?)
            } else {
                None
            };
            Ok((line, pair))
        }
    }
}

/// DC resistance of one trace, Ω/m.
fn dc_resistance(r: &Resolved, w: Nm, sigma: f64) -> f64 {
    let area = (mm(w) - mm(r.opts.etch)).max(1e-9) * mm(r.g.t) * 1e-6;
    1.0 / (sigma * area)
}

fn db(np: f64) -> f64 {
    np * loss::DB_PER_NEPER
}

/// Evaluates the matrices at every frequency.
fn loss_points(
    line: &LineMatrices,
    pair: Option<&LineMatrices>,
    lp: &LossParams,
    cond: &loss::Conductor,
) -> Vec<LossPointInfo> {
    const INCH: f64 = 0.0254;
    let rough = !matches!(lp.roughness, loss::Roughness::Smooth);
    lp.freqs
        .iter()
        .map(|fq| {
            let f = fq.to_f64();
            let p = loss::at(line, f, cond);
            let d = pair.map(|m| loss::pair_at(m, f, cond));
            LossPointInfo {
                freq: *fq,
                alpha_db_per_m: sig(db(p.alpha), 4, Unit::Decibel),
                alpha_db_per_in: sig(db(p.alpha) * INCH, 4, Unit::Decibel),
                alpha_conductor_db_per_m: sig(db(p.alpha_c), 4, Unit::Decibel),
                alpha_dielectric_db_per_m: sig(db(p.alpha_d), 4, Unit::Decibel),
                alpha_diff_db_per_m: d.map(|d| sig(db(d.alpha_diff), 4, Unit::Decibel)),
                alpha_diff_db_per_in: d.map(|d| sig(db(d.alpha_diff) * INCH, 4, Unit::Decibel)),
                z0_real: ohms(p.z0.re),
                z0_imag: quantity(p.z0.im, 3, Unit::Ohm),
                r_per_m: sig(p.r, 4, Unit::Ohm),
                l_per_m: sig(p.l, 4, Unit::Henry),
                g_per_m: sig(p.g, 4, Unit::Siemens),
                c_per_m: sig(p.c, 4, Unit::Farad),
                skin_depth: Nm((p.skin_depth * 1e9).round() as i64),
                roughness_factor: rough.then(|| quantity(p.roughness, 3, Unit::None)),
            }
        })
        .collect()
}

/// Losses of a resolved line at the requested frequencies, with their notes.
fn losses(
    ctx: &mut Context<'_>,
    r: &Resolved,
    w: Nm,
    gap: Option<Nm>,
    lp: &LossParams,
) -> Result<LossResult, CommandError> {
    let (line, pair) = loss_matrices(r, w, gap)?;
    let sigma = lp.sigma.to_f64();
    let cond = loss::Conductor { sigma, roughness: lp.roughness, r_dc: dc_resistance(r, w, sigma) };
    let points = loss_points(&line, pair.as_ref(), lp, &cond);
    report_loss_notes(ctx, r, lp, gap.is_some() && r.method == Solver::ClosedForm);
    let field = r.method == Solver::FieldSolver;
    let (tan, tan2) = match (field, r.g.tan_d_split) {
        (true, Some((a, b))) => (a, (a != b).then_some(b)),
        _ => (r.g.tan_d, None),
    };
    let tanq = |v: f64| sig(v, 4, Unit::None).plain();
    let rf = r.g.ref_freq.or(r.g.ref_freq2).or(r.g.broadside.as_ref().and_then(|b| b.ref_freq));
    let dispersive = if rf.is_some() {
        "; Djordjevic–Sarkar wideband Debye εr(f), tan δ(f) through the stackup's values at their reference frequency"
    } else {
        ""
    };
    Ok(LossResult {
        ref_freq: rf.map(|f| sig(f, 6, Unit::Hertz)),
        conductivity: lp.sigma,
        loss_tangent: tanq(tan),
        loss_tangent2: tan2.map(tanq),
        mask_loss_tangent: r.opts.mask.map(|m| tanq(m.tan_d)),
        roughness: lp.rough_in.0,
        huray_radius: lp.rough_in.1,
        huray_ratio: lp.rough_in.2,
        conductor_method: if field {
            "perturbation: Rs ∮|Js|² of the quasi-TEM surface current (field solver), planes included".into()
        } else {
            "Wheeler's incremental inductance rule on the closed-form model".into()
        },
        dielectric_method: if field {
            format!("partial capacitances: G = ω Σ tan δ · ε ∂C/∂ε (field solver){dispersive}")
        } else {
            format!("G = ω C tan δ · (εr/εeff) ∂εeff/∂εr (closed-form εeff){dispersive}")
        },
        points,
    })
}

fn report_loss_notes(ctx: &mut Context<'_>, r: &Resolved, lp: &LossParams, closed_pair: bool) {
    if r.g.tan_d_assumed {
        let mut d = Diagnostic::warning(
            "impedance.loss_tangent_assumed",
            format!(
                "the dielectric loss tangent is not specified: assuming {} (nominal FR-4)",
                crate::model::board::DEFAULT_LOSS_TANGENT
            ),
        )
        .with_hint("set it from the laminate datasheet with `board.dielectric --loss-tangent`, or give `loss_tangent`");
        if let Some(l) = &r.layer {
            d = d.with_subject(ObjectRef::Layer(l.to_string()));
        }
        ctx.report(d);
    }
    if r.mask_tan_assumed {
        ctx.report(
            Diagnostic::info(
                "impedance.mask_loss_tangent_assumed",
                format!("solder mask loss tangent not given: assuming {}", electrical::DEFAULT_MASK_LOSS_TANGENT),
            )
            .with_hint("give `mask_loss_tangent` from the mask's datasheet"),
        );
    }
    // Skin depth against half the copper: below that frequency the skin-effect model is
    // approximate (the DC resistance is its floor).
    let half_t = mm(r.g.t) * 0.5e-3;
    let sigma = lp.sigma.to_f64();
    if let Some(f) = lp.freqs.iter().find(|f| loss::skin_depth(f.to_f64(), sigma) > half_t) {
        ctx.report(
            Diagnostic::info(
                "impedance.skin_depth_large",
                format!(
                    "at {f} the skin depth ({}) exceeds half the copper thickness ({}): the conductor loss there is \
                     approximate (between the DC and the skin-effect resistance)",
                    Nm((loss::skin_depth(f.to_f64(), sigma) * 1e9).round() as i64),
                    Nm(r.g.t.0 / 2)
                ),
            )
            .with_hint("the skin-effect model is accurate where the skin depth is well below the copper thickness"),
        );
    }
    if closed_pair {
        ctx.report(
            Diagnostic::info(
                "impedance.loss_single_ended",
                "the closed forms give the losses of one track: the differential attenuation needs the field solver",
            )
            .with_hint("add `method: field_solver` for `alpha_diff_db_per_m`"),
        );
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
    if let Some(l) = &o.loss {
        for p in &l.points {
            s += &format!(
                "\n  {}: {}/m ({}/in; conductor {}/m, dielectric {}/m){}, Z0 {} {} j{}",
                p.freq,
                p.alpha_db_per_m,
                p.alpha_db_per_in,
                p.alpha_conductor_db_per_m,
                p.alpha_dielectric_db_per_m,
                p.alpha_diff_db_per_m.map(|d| format!(", differential {d}/m")).unwrap_or_default(),
                p.z0_real,
                if p.z0_imag.to_f64() < 0.0 { "-" } else { "+" },
                p.z0_imag.with_unit(Unit::Ohm).to_string().trim_start_matches('-')
            );
        }
    }
    s
}

/// Characteristic impedance of a track (or a differential pair: edge-coupled with `gap`,
/// broadside-coupled with `broadside`) on a copper layer, from the stackup: outer layers are
/// microstrips, inner layers striplines. Explicit `height`/`height2`/`copper`/`er`/`model`
/// override the stackup. `method: field_solver` (selected automatically by `etch`, `mask`,
/// `coplanar_gap`, `broadside` and `er2`) solves the cross-section numerically. With `freq`
/// (or a sweep `freq_from`/`freq_to`/`freq_points`), also the losses: attenuation (dB/m,
/// dB/in; conductor with skin effect and optional roughness, dielectric from the loss
/// tangent), complex Z0 and R, L, G, C per meter.
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
    /// Frequencies to compute the losses at ("1GHz"; one or more). Losses: attenuation,
    /// complex Z0 and R, L, G, C per meter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub freq: Vec<Quantity>,
    /// Frequency sweep: first frequency ("10MHz").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_from: Option<Quantity>,
    /// Frequency sweep: last frequency ("10GHz").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_to: Option<Quantity>,
    /// Frequency sweep: number of frequencies, logarithmically spaced (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_points: Option<usize>,
    /// Loss tangent of the dielectric(s), overriding the stackup ("0.02").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_tangent: Option<Quantity>,
    /// Frequency at which εr and tan δ are given, overriding the stackup's ("1GHz": the
    /// Djordjevic–Sarkar dispersive model; "0": constant εr and tan δ).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_freq: Option<Quantity>,
    /// Field solver: solder mask loss tangent (default 0.025).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_loss_tangent: Option<Quantity>,
    /// Copper conductivity in S/m (default 58M, annealed copper).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductivity: Option<Quantity>,
    /// RMS surface roughness of the copper (Hammerstad-Jensen correction, "1um").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<Nm>,
    /// Huray snowball model: sphere radius ("0.5um"), with `huray_ratio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_radius: Option<Nm>,
    /// Huray snowball model: total snowball surface per flat area (N·4πa²/A, e.g. "2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_ratio: Option<Quantity>,
}

impl Command for Calc {
    const NAME: &'static str = "impedance.calc";
    const SUMMARY: &'static str =
        "Impedance of a track or differential pair on a layer, from the stackup; losses at given frequencies";
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
            loss_tangent: self.loss_tangent,
            mask_loss_tangent: self.mask_loss_tangent,
            ref_freq: self.ref_freq,
        };
        if self.gap.is_some() && args.is_broadside() {
            return Err(CommandError::invalid_args(
                "impedance.invalid_geometry",
                "a pair is either edge-coupled (`gap`) or broadside, not both",
            ));
        }
        let lp = loss_params(&LossArgs {
            freq: &self.freq,
            freq_from: self.freq_from,
            freq_to: self.freq_to,
            freq_points: self.freq_points,
            conductivity: self.conductivity,
            roughness: self.roughness,
            huray_radius: self.huray_radius,
            huray_ratio: self.huray_ratio,
        })?;
        let r = resolve(ctx.project()?, &args)?;
        let mut o = calc(&r, self.width, self.gap)?;
        report_geometry(ctx, &r, self.width, self.gap);
        if let Some(lp) = lp {
            o.loss = Some(losses(ctx, &r, self.width, self.gap, &lp)?);
        }
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
            loss_tangent: None,
            mask_loss_tangent: None,
            ref_freq: None,
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
// Net insertion loss

/// Insertion loss of a routed net: the attenuation of its tracks on each layer and width
/// (single-ended, from the stackup like `impedance.calc`) times their routed length, at given
/// frequencies. Vias, pads and reflections are not included.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetLoss {
    /// Net name.
    pub net: String,
    /// Frequencies ("1GHz"; one or more).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub freq: Vec<Quantity>,
    /// Frequency sweep: first frequency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_from: Option<Quantity>,
    /// Frequency sweep: last frequency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_to: Option<Quantity>,
    /// Frequency sweep: number of frequencies, logarithmically spaced (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_points: Option<usize>,
    /// `closed_form` (default) or `field_solver`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Solver>,
    /// Loss tangent, overriding the stackup's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss_tangent: Option<Quantity>,
    /// Frequency at which εr and tan δ are given, overriding the stackup's ("1GHz":
    /// Djordjevic–Sarkar; "0": constant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_freq: Option<Quantity>,
    /// Copper conductivity in S/m (default 58M).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductivity: Option<Quantity>,
    /// RMS surface roughness (Hammerstad-Jensen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<Nm>,
    /// Huray snowball radius, with `huray_ratio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_radius: Option<Nm>,
    /// Huray surface ratio (N·4πa²/A).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_ratio: Option<Quantity>,
}

/// Tracks of a net on one layer at one width.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetLossSegment {
    /// Copper layer.
    pub layer: String,
    /// Track width.
    pub width: Nm,
    /// Routed length on this layer at this width.
    pub length: Nm,
    /// Line model of the layer.
    pub model: Model,
    /// Attenuation per meter at each frequency (in the order of `points`).
    pub alpha_db_per_m: Vec<Quantity>,
}

/// Insertion loss at one frequency.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetLossPoint {
    /// Frequency.
    pub freq: Quantity,
    /// Total loss of the routed tracks.
    pub loss: Quantity,
}

/// Result of `net.loss`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NetLossResult {
    /// Net.
    pub net: String,
    /// How the attenuation was computed.
    pub method: Solver,
    /// Routed track length (all layers).
    pub length: Nm,
    /// Via length (through the stackup), not included in the loss.
    pub via_length: Nm,
    /// Tracks per layer and width.
    pub segments: Vec<NetLossSegment>,
    /// Loss per frequency.
    pub points: Vec<NetLossPoint>,
}

impl Command for NetLoss {
    const NAME: &'static str = "net.loss";
    const SUMMARY: &'static str = "Insertion loss of a routed net's tracks at given frequencies (per layer and width)";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["net"];
    type Output = NetLossResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<NetLossResult, CommandError> {
        let p = ctx.project()?;
        let net = super::net::net_name(p, &self.net)?;
        let Some(lp) = loss_params(&LossArgs {
            freq: &self.freq,
            freq_from: self.freq_from,
            freq_to: self.freq_to,
            freq_points: self.freq_points,
            conductivity: self.conductivity,
            roughness: self.roughness,
            huray_radius: self.huray_radius,
            huray_ratio: self.huray_ratio,
        })?
        else {
            return Err(CommandError::invalid_args("impedance.no_frequency", "`net.loss` needs a frequency")
                .with_hint("give `freq` (one or more), or a sweep with `freq_from`, `freq_to` and `freq_points`"));
        };
        let mut by: std::collections::BTreeMap<(String, Nm), f64> = std::collections::BTreeMap::new();
        for t in p.board().tracks.iter().filter(|t| t.net.as_deref() == Some(net.as_str())) {
            *by.entry((t.layer.clone(), t.width)).or_default() += crate::board::track_length(t).0 as f64;
        }
        let via_length = crate::lengths::net_length(p, &net).vias;
        if by.is_empty() {
            ctx.report(
                Diagnostic::warning("net.not_routed", format!("net `{net}` has no tracks: no loss to compute"))
                    .with_subject(ObjectRef::Net(net.clone()))
                    .with_hint("route it first (route.nets), then measure its loss"),
            );
        }
        let sigma = lp.sigma.to_f64();
        let mut segments = Vec::new();
        let mut total = vec![0.0; lp.freqs.len()];
        let mut length = 0.0;
        let mut noted: Vec<String> = Vec::new();
        for ((layer, width), len_nm) in by {
            let args = GeoArgs {
                layer: Some(layer.as_str()),
                model: None,
                height: None,
                height2: None,
                copper: None,
                er: None,
                method: self.method,
                er2: None,
                etch: None,
                mask: None,
                mask_copper: None,
                mask_er: None,
                coplanar_gap: None,
                broadside: false,
                broadside_gap: None,
                loss_tangent: self.loss_tangent,
                mask_loss_tangent: None,
                ref_freq: self.ref_freq,
            };
            let r = resolve(ctx.project()?, &args)?;
            let (line, _) = loss_matrices(&r, width, None)?;
            let cond = loss::Conductor { sigma, roughness: lp.roughness, r_dc: dc_resistance(&r, width, sigma) };
            let meters = len_nm * 1e-9;
            let mut alphas = Vec::new();
            for (i, f) in lp.freqs.iter().enumerate() {
                let a = db(loss::at(&line, f.to_f64(), &cond).alpha);
                total[i] += a * meters;
                alphas.push(sig(a, 4, Unit::Decibel));
            }
            length += len_nm;
            if !noted.contains(&layer) {
                if r.g.assumed {
                    ctx.report(
                        Diagnostic::warning(
                            "impedance.stackup_assumed",
                            "the stackup's dielectrics are not specified: the result uses an assumed stackup",
                        )
                        .with_subject(ObjectRef::Layer(layer.clone()))
                        .with_hint("set the real dielectrics from your fab's stackup with `board.dielectric`"),
                    );
                }
                report_loss_notes(ctx, &r, &lp, false);
                noted.push(layer.clone());
            }
            segments.push(NetLossSegment {
                layer,
                width,
                length: Nm(len_nm.round() as i64),
                model: r.g.model,
                alpha_db_per_m: alphas,
            });
        }
        let points = lp
            .freqs
            .iter()
            .zip(&total)
            .map(|(f, l)| NetLossPoint { freq: *f, loss: quantity(*l, 3, Unit::Decibel) })
            .collect();
        Ok(NetLossResult {
            net,
            method: self.method.unwrap_or(Solver::ClosedForm),
            length: Nm(length.round() as i64),
            via_length,
            segments,
            points,
        })
    }

    fn summarize(o: &NetLossResult) -> String {
        let mut s = format!("net `{}`: {} of tracks (vias {} not included)", o.net, o.length, o.via_length);
        for p in &o.points {
            s += &format!("\n  {}: {}", p.freq, p.loss);
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
