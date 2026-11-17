//! `sim.sparams`'s solver: the board region run by the [`ems`] crate's FDTD (DECISIONS D86, D87,
//! D89, D90, D94, D98, D100).
//!
//! The region, its mesh, its ports and its parts come from [`super::extract`] and
//! [`super::place_ports`]; the geometry is the property list of the openEMS
//! export ([`crate::openems::xml::properties`]): Djordjevic–Sarkar dielectrics as Debye materials
//! on the fitted terms, perfect copper as metal volumes or sheets, via barrels as cylinders plus
//! their node column, the solder mask, board parts as lumped elements. Lossy copper is where
//! this model leaves the export's (two conducting sheets per layer, roughness in the conductivity
//! at f₀): solid copper stays metal volumes with a surface impedance on every face, sheet copper
//! is a conducting sheet integrated as a rational two-port, both with the roughness model over
//! the band, and cells are cut where copper is oblique to the mesh ([`compile_options`]).
//! Ports are ems lumped ports (a resistor of z0 with a Thevenin source, the voltage the mean over
//! the port's columns); a microstrip port is that source on the line plus a voltage probe and two
//! current loops around its reference plane. The model is compiled once; one run per port,
//! S = B A⁻¹ on the waves.

use ems::compile::{ConformalOutlines, PoleWeighting, SheetModel};
use ems::csx;
use ems::csx::primitives::{BoxShape, Cylinder, LinPoly, Polygon};
use ems::csx::properties::{DebyeMaterial, DebyePole, LumpedElement, LumpedElementType, ProbeBox, ProbeType};
use ems::csx::{ContinuousStructure, Primitive, Property};
use ems::engine::surface::{Conductor, Roughness as EmsRoughness};
use ems::{
    Boundaries, Boundary, CompileOptions, CompiledModel, Cpml, EndCriteria, Fdtd, Settle, StepLimit, WarningCode,
    Waveform,
};

use super::{
    Error, Extracted, Gaussian, Hooks, LumpedPort, Measure, Options, Plan, PortSet, Probe, SParams, Waves, parts,
};
use crate::diag::Diagnostic;
use crate::electrical::complex::C64;
use crate::electrical::loss::Roughness;
use crate::model::Project;
use crate::openems::xml::{self, Kind, Prop, Shape};
use crate::openems::{EPS0, Region};
use crate::refs::ObjectRef;

/// Drawing unit of the model: millimetres, as the export's file.
const UNIT: f64 = 1e-3;
/// Most solver warnings passed on one by one.
const MAX_WARNINGS: usize = 12;
/// Name prefix of this module's probes.
const PROBE: &str = "cadlab_probe_";

/// A length in drawing units. Every coordinate goes through this one conversion, so a
/// primitive's corner on a mesh line is the mesh line's own value.
fn mm(nm: i64) -> f64 {
    nm as f64 * 1e-6
}

fn point(p: [i64; 3]) -> [f64; 3] {
    [mm(p[0]), mm(p[1]), mm(p[2])]
}

fn axis(a: usize) -> csx::Axis {
    [csx::Axis::X, csx::Axis::Y, csx::Axis::Z][a.min(2)]
}

fn ems_axis(a: usize) -> ems::Axis {
    [ems::Axis::X, ems::Axis::Y, ems::Axis::Z][a.min(2)]
}

/// A solver failure as a diagnostic.
fn solver_error(e: ems::Error) -> Error {
    Error::new(
        "sim.ems_failed",
        format!("the ems solver refused the model: {e}"),
        "check the region with `plan` and `export.openems`; if it is valid, report the case to ems",
    )
}

fn model_error(e: csx::Error) -> Error {
    solver_error(ems::Error::Csx(e))
}

/// The export's property list as a csx model on the region's mesh.
fn structure(props: &[Prop], lines: &[Vec<i64>; 3]) -> Result<ContinuousStructure, Error> {
    let mut m = ContinuousStructure::new();
    m.grid.delta_unit = UNIT;
    for (a, l) in lines.iter().enumerate() {
        m.grid.set_lines(a, l.iter().map(|&v| mm(v))).map_err(model_error)?;
    }
    // (A property without primitives is the export's to write, the mask's openings on a board
    // that has none; a model has no use for it.)
    for p in props.iter().filter(|p| !p.prims.is_empty()) {
        let property = match &p.kind {
            Kind::Material { er, .. } => Property::material(p.name.as_str(), csx::Material::new(*er, 0.0)),
            Kind::Debye { eps_inf, sigma, terms } => Property::debye_material(
                p.name.as_str(),
                DebyeMaterial {
                    material: csx::Material::new(*eps_inf, *sigma),
                    poles: terms.iter().map(|&(delta, tau)| DebyePole::new(delta, tau)).collect(),
                },
            ),
            Kind::Metal => Property::metal(p.name.as_str()),
            Kind::Sheet { conductivity, thickness } => {
                Property::conducting_sheet(p.name.as_str(), *conductivity, *thickness)
            }
            Kind::Lumped { axis: a, r, l, c, series } => {
                let mut e = LumpedElement { direction: axis(*a), ..LumpedElement::default() };
                if *r > 0.0 {
                    e.r = (*r).into();
                }
                if *l > 0.0 {
                    e.l = (*l).into();
                }
                if let Some(c) = c {
                    e.c = (*c).into();
                }
                if *series {
                    e.le_type = Some(LumpedElementType::Series);
                }
                Property::lumped_element(p.name.as_str(), e)
            }
        };
        let id = m.add_property(property);
        for q in &p.prims {
            let prim = match &q.shape {
                Shape::Box { lo, hi } => Primitive::new(BoxShape::new(point(*lo), point(*hi))),
                Shape::Prism { ring, z0, z1 } => {
                    let pts: Vec<(f64, f64)> = ring.0.iter().map(|v| (mm(v.x), mm(v.y))).collect();
                    if z0 == z1 {
                        Primitive::new(Polygon::new(2, mm(*z0), pts))
                    } else {
                        Primitive::new(LinPoly::new(2, mm(*z0), mm(*z1) - mm(*z0), pts))
                    }
                }
                Shape::Cylinder { x, y, radius, z0, z1 } => {
                    Primitive::new(Cylinder::new(point([*x, *y, *z0]), point([*x, *y, *z1]), mm(*radius)))
                }
            };
            m.add_primitive(id, prim.with_priority(q.priority as i32)).map_err(model_error)?;
        }
    }
    Ok(m)
}

/// A region ready for ems: the model without ports, the ports on the mesh, the parts.
struct Prepared {
    region: Region,
    model: ContinuousStructure,
    /// How the model is compiled: conformal cells, the copper's conductivity and roughness.
    compile: CompileOptions,
    /// The design objects behind the model's properties, by property name.
    subjects: Vec<(String, Vec<ObjectRef>)>,
    ports: PortSet,
    parts: Vec<super::LumpedPart>,
    freqs: Vec<f64>,
    diags: Vec<Diagnostic>,
}

fn prepare(p: &Project, o: &Options) -> Result<Prepared, Error> {
    let Extracted { region: r, freqs, band, mut diags, candidates } = super::extract(p, o)?;
    let boxes = parts::boxes(&r, candidates, &mut diags);
    let props = xml::properties(&xml::Geometry {
        parts: &boxes,
        title: "",
        layers: &r.layers,
        diels: &r.diels,
        copper: &r.copper,
        holes: &r.holes,
        board: &r.board,
        masks: &r.masks,
        mask_material: r.mask_material,
        f0: r.f0,
        band,
        conductivity: o.region.conductivity,
        // The roughness goes to ems as a model over the band, not into the conductivity at f₀.
        roughness: Roughness::Smooth,
        volumes: true,
        lines: [&r.lines[0], &r.lines[1], &r.lines[2]],
        window: r.window,
    });
    let model = structure(&props, &r.lines)?;
    let compile = compile_options(o, &props, band);
    if o.conformal {
        super::unresolved(&r, &mut diags);
    }
    let ports = super::place_ports(&r)?;
    // The columns of mesh edges an element's box takes in.
    let within = |a: usize, lo: i64, hi: i64| r.lines[a].iter().filter(|&&v| lo <= v && v <= hi).count().max(1);
    let parts: Vec<super::LumpedPart> = boxes
        .into_iter()
        .map(|b| {
            let across = 1 - b.axis.min(1);
            let columns = within(across, b.lo[across], b.hi[across]) * within(2, b.lo[2], b.hi[2]);
            super::LumpedPart { columns: columns as u32, ..b.info }
        })
        .collect();
    super::notes(p, o, &r, &parts, &mut diags);
    let mut subjects: Vec<(String, Vec<ObjectRef>)> = props.into_iter().map(|p| (p.name, p.subjects)).collect();
    for (i, port) in r.ports.iter().enumerate() {
        let at = ObjectRef::parse(&port.info.at).unwrap_or(ObjectRef::Name(port.info.at.clone()));
        for kind in ["resist", "excite", "ut", "it"] {
            subjects.push((format!("port_{kind}{}{}", if kind.len() > 2 { "_" } else { "" }, i + 1), vec![at.clone()]));
        }
    }
    Ok(Prepared { region: r, model, compile, subjects, ports, parts, freqs, diags })
}

fn roughness(r: Roughness) -> EmsRoughness {
    match r {
        Roughness::Smooth => EmsRoughness::Smooth,
        Roughness::HammerstadJensen { rms } => EmsRoughness::HammerstadJensen { rms },
        Roughness::Huray { radius, ratio } => EmsRoughness::Huray { radius, ratio },
    }
}

/// How ems compiles the property list: conformal cells where copper is oblique to the mesh
/// (solid metal and the outlines of sheets on mesh planes; copper along the mesh keeps the
/// staircase the mesh was laid out for) and, for lossy copper, a
/// surface impedance fitted over the result's band on the faces of the copper volumes and as the
/// two-port of the copper sheets, each with the roughness model. A laminate's Debye poles go to
/// an edge in its surface by the share of the edge's dual face the laminate fills, as its static
/// permittivity does (`PoleWeighting::Averaged`; openEMS's rule,
/// ems's default, gives such an edge the whole poles: two points more loss on a 60 mm
/// microstrip at 9 GHz, D89, D90). What a small cut face does, and
/// whether a narrow slanted track or a barrel of a cell keeps whole edges, is ems's to decide
/// (its `ConformalOptions::keep_stable` and `CompileOptions::conformal_bridges`, both on by
/// default; cadlab chose a threshold and fell back to a staircase with ems 0.0.7, D87, D89).
fn compile_options(o: &Options, props: &[Prop], band: (f64, f64)) -> CompileOptions {
    let mut c = CompileOptions::default()
        .with_conformal(o.conformal)
        .with_conformal_outlines(ConformalOutlines::Slanted)
        .with_conformal_solids(ConformalOutlines::Slanted)
        .with_pole_weighting(PoleWeighting::Averaged);
    let Some(sigma) = o.region.conductivity else { return c };
    let rough = roughness(o.region.roughness);
    c = c.with_sheet_model(SheetModel::SurfaceImpedance);
    if band.0 > 0.0 && band.1 > band.0 {
        c = c.with_surface_band(band.0, band.1);
    }
    for p in props {
        match p.kind {
            // The copper of a net (not a 0 Ω part, which is a short).
            Kind::Metal if p.name.starts_with(xml::COPPER) => {
                c = c.with_lossy_metal(p.name.as_str(), Conductor::new(sigma).with_roughness(rough));
            }
            Kind::Sheet { .. } if rough != EmsRoughness::Smooth => c = c.with_sheet_roughness(p.name.as_str(), rough),
            _ => {}
        }
    }
    c
}

/// The mesh edges ems cut (those slanted or curved copper covers in part, and the ones it covers
/// whole next to them), for the result's `conformal_edges`.
fn cut_edges(sim: &ems::Simulation) -> usize {
    sim.boundaries().conformal.map_or(0, |r| r.edges + r.sheet_edges)
}

/// Sets the simulation up and checks it, without running it.
pub(crate) fn plan(p: &Project, o: &Options) -> Result<Plan, Error> {
    let Prepared { region, model, compile, subjects, ports, parts, mut diags, freqs, .. } = prepare(p, o)?;
    // Compiling the model is what ems checks it with: its own warnings and timestep.
    let mut model = model;
    add_ports(&mut model, &region, &ports)?;
    let sim = CompiledModel::new(&model, &settings(o, &region, 1, freqs.first().copied()), &compile)
        .and_then(|c| c.simulation_driving(&[]))
        .map_err(solver_error)?;
    warnings(&sim.diagnostics(), &subjects, &mut diags);
    let cut = cut_edges(&sim);
    let dt = sim.engine().dt();
    let mut plan = super::planned(region, o.courant, parts, cut, diags);
    plan.pulse_steps = (plan.pulse_steps as f64 * plan.timestep / dt).ceil() as u64;
    plan.timestep = dt;
    Ok(plan)
}

/// The ports of one run in the model: what to read them with.
struct Added {
    lumped: Vec<ems::port::LumpedPort>,
    /// Names of the probes of [`PortSet::probes`], in order.
    probes: Vec<String>,
}

/// Writes the ports, each with its source (a run drives one of them), and the probes into the
/// model.
fn add_ports(model: &mut ContinuousStructure, r: &Region, set: &PortSet) -> Result<Added, Error> {
    let at = |a: usize, i: usize| mm(r.lines[a][i]);
    let mut lumped = Vec::new();
    for (i, LumpedPort { axis: a, lo, hi, positive_high, r: z0 }) in set.ports.iter().enumerate() {
        // The positive terminal is ems's `start`.
        let (mut start, mut stop) =
            ([at(0, lo[0]), at(1, lo[1]), at(2, lo[2])], [at(0, hi[0]), at(1, hi[1]), at(2, hi[2])]);
        if *positive_high {
            (start[*a], stop[*a]) = (stop[*a], start[*a]);
        }
        let port = ems::port::LumpedPort::new(i as u32 + 1, *z0, start, stop, ems_axis(*a))
            .priority(30)
            .excite(1.0)
            .average_voltage(true);
        lumped.push(port.add_to(model).map_err(solver_error)?);
    }
    let mut probes = Vec::new();
    for (i, probe) in set.probes.iter().enumerate() {
        let name = format!("{PROBE}{i}");
        let (kind, start, stop) = match *probe {
            // φ(from) − φ(to): the integral of E from `from` to `to`.
            Probe::Voltage { axis: a, from, to } => {
                let start = [at(0, from[0]), at(1, from[1]), at(2, from[2])];
                let mut stop = start;
                stop[a] = at(a, to);
                (ProbeBox::new(ProbeType::Voltage), start, stop)
            }
            // The H loop around the nodes `lo ..= hi`, in the dual plane between `at` and `at + 1`:
            // a flat box in that plane (ems takes the dual plane nearest a flat box).
            Probe::Current { axis: a, at: k, lo, hi } => {
                let (u, v) = ((a + 1) % 3, (a + 2) % 3);
                let plane = 0.5 * (at(a, k) + at(a, k + 1));
                let (mut start, mut stop) = ([0.0; 3], [0.0; 3]);
                (start[a], stop[a]) = (plane, plane);
                (start[u], stop[u]) = (at(u, lo[0]), at(u, hi[0]));
                (start[v], stop[v]) = (at(v, lo[1]), at(v, hi[1]));
                (ProbeBox::new(ProbeType::Current).with_norm_dir(axis(a)), start, stop)
            }
        };
        let id = model.add_property(Property::probe(name.as_str(), kind));
        model.add_primitive(id, Primitive::new(BoxShape::new(start, stop)).with_priority(30)).map_err(model_error)?;
        probes.push(name);
    }
    Ok(Added { lumped, probes })
}

/// Worker threads for a grid: `threads` when given, else ems's own rule within the threads
/// cadlab's pool may use.
fn threads(o: &Options, r: &Region) -> usize {
    if o.threads > 0 {
        return o.threads;
    }
    let nodes = [r.lines[0].len(), r.lines[1].len(), r.lines[2].len()];
    #[cfg(feature = "parallel")]
    let most = rayon::current_num_threads();
    #[cfg(not(feature = "parallel"))]
    let most = 1;
    ems::engine::default_threads_for(nodes).min(most).max(1)
}

/// The solver settings of a region: the pulse, CPML of `pml_cells` on every face with its
/// frequency shift α at f₀ (2π ε₀ f₀: at a tenth of that a line near the absorber read gain, D68)
/// and ems's second-order convolution (its default from 0.0.18: the first-order one was a layer
/// whose conductivity grew with the time step, D111),
/// the end criterion (the energy's decay and then, with `settle`, ems's test that the probes'
/// spectra at `lowest`, the lowest frequency of the result, have no more to come than a ten
/// thousandth of the signals' size, for half as many steps again at most: `ems::Settle`, D111),
/// the step limit and the timestep: `courant` of the bound of the mesh's cells
/// (ems's `StepLimit::Cells`, D105 and D107: each cell with its own sizes, the permittivity of its
/// edges, the walls and copper it lies against and the part of the edges it shares with its
/// neighbours that leaves the largest of the cells' eigenvalues smallest, the cells conformal copper
/// cuts among them; never less than `courant` of the Courant limit of the smallest cell sizes in
/// vacuum, which it was until ems 0.0.15). On perfect copper that bound is the scheme's limit, so
/// `courant` is the margin.
fn settings(o: &Options, r: &Region, threads: usize, lowest: Option<f64>) -> Fdtd {
    let f0 = r.f0 as f64;
    let pml = Cpml::cells(o.region.pml_cells as usize).alpha_max(2.0 * std::f64::consts::PI * EPS0 * f0);
    let end = EndCriteria::energy_decay_db(-(o.region.end_criteria_db as f64))
        .check_interval(32)
        .settle(lowest.filter(|_| o.settle).map(Settle::at));
    Fdtd::new()
        .waveform(Waveform::gaussian(f0, r.fc as f64))
        .boundaries(Boundaries::all(Boundary::Pml(pml)))
        .end_criteria(end)
        .max_timesteps(o.region.timesteps)
        .timestep_factor(o.courant)
        .step_limit(StepLimit::Cells)
        .threads(threads)
}

/// The solver's own warnings as diagnostics: `sim.ems_<code>` from ems's warning code
/// (`sim.ems_sheet_other`, `sim.ems_lumped_edges`, ...), the design objects behind the property
/// the warning names, one diagnostic per code and property (at most [`MAX_WARNINGS`]).
fn warnings(list: &[ems::Warning], subjects: &[(String, Vec<ObjectRef>)], diags: &mut Vec<Diagnostic>) {
    // The mesh, its grading into the absorbing layers and the copper and laminate running
    // through them are cadlab's, and the probes of this
    // module are on dual planes on purpose: not news about this run. Nor is what lossy copper
    // is by construction here: the node column of a barrel is a wire without loss, and the
    // covered parts of the edges a slanted outline cuts are perfect conductors beside lossy
    // neighbours that carry their surface (ELECTRICAL.md has what that costs).
    let ours = |w: &ems::Warning| match w.code() {
        WarningCode::MeshInPml
        | WarningCode::MeshPmlGrading
        | WarningCode::MeshGrading
        | WarningCode::MeshTimeStep
        | WarningCode::LossyMetalThin
        | WarningCode::LossyMetalCut => true,
        WarningCode::BoxSnapped => w.subject().is_some_and(|s| s.starts_with(PROBE)),
        _ => false,
    };
    let shown: Vec<&ems::Warning> = list.iter().filter(|w| !ours(w)).collect();
    let hint = "the result may still be valid: check its passivity and reciprocity (`checks`), and compare with a \
                finer mesh or with openEMS on `export.openems`";
    for w in shown.iter().take(MAX_WARNINGS) {
        let code = format!("sim.ems_{}", w.code().as_str().replace('.', "_"));
        let mut d = Diagnostic::warning(code, format!("ems: {w}")).with_hint(hint);
        let of = w.subject().and_then(|name| subjects.iter().find(|s| s.0 == name));
        for s in of.iter().flat_map(|s| &s.1) {
            d = d.with_subject(s.clone());
        }
        diags.push(d);
    }
    if shown.len() > MAX_WARNINGS {
        diags.push(
            Diagnostic::warning(
                "sim.ems_warnings_omitted",
                format!("ems: {} more warnings not shown", shown.len() - MAX_WARNINGS),
            )
            .with_hint(hint),
        );
    }
}

fn spectrum(res: &ems::Results, name: &str, freqs: &[f64]) -> Result<Vec<C64>, Error> {
    let probe = res
        .probe(name)
        .ok_or_else(|| solver_error(ems::Error::Invalid(format!("no probe {name:?} in the results"))))?;
    Ok(probe.dft(freqs).map_err(solver_error)?.into_iter().map(|c| C64::new(c.re, c.im)).collect())
}

/// Simulates the region with ems and returns its S-parameters.
pub(crate) fn sparams(p: &Project, o: &Options, hooks: &Hooks<'_>) -> Result<SParams, Error> {
    let Prepared { region: r, mut model, compile, subjects, ports: set, parts, freqs, mut diags } = prepare(p, o)?;
    let fdtd = settings(o, &r, threads(o, &r), freqs.first().copied());
    let np = set.ports.len();
    let excitation = Gaussian { f0: r.f0 as f64, fc: r.fc as f64 };
    let mut waves = Waves::new(np, freqs.len());
    let (mut timestep, mut conformal_edges) = (0.0, 0);
    // One compile; every run is a new engine from it, driving its own port.
    let added = add_ports(&mut model, &r, &set)?;
    let compiled = CompiledModel::new(&model, &fdtd, &compile).map_err(solver_error)?;
    drop(model);
    for j in 0..np {
        let source = added.lumped[j].properties().and_then(|p| p.excitation.as_ref()).map(|e| e.0.as_str());
        let sim = compiled.simulation_driving(source.as_slice()).map_err(solver_error)?;
        if j == 0 {
            warnings(&sim.diagnostics(), &subjects, &mut diags);
            conformal_edges = cut_edges(&sim);
        }
        timestep = sim.engine().dt();
        let mut cancelled = false;
        // The run takes the simulation: its results hold the probes' series, not a copy of them
        // beside the engine (ems 0.0.12, D98).
        let res = sim
            .run_into_results(|pr| {
                (hooks.progress)(
                    pr.steps,
                    Some(o.region.timesteps),
                    &format!("port {}/{np}: timestep {}, energy {:.1} dB (ems)", j + 1, pr.steps, pr.energy_db),
                );
                if (hooks.cancelled)() {
                    cancelled = true;
                    return core::ops::ControlFlow::Break(());
                }
                core::ops::ControlFlow::Continue(())
            })
            .map_err(solver_error)?;
        let unstable = || {
            Error::new(
                "sim.unstable",
                "the simulation diverged (ems)",
                "lower `courant`; report the case to ems if it persists",
            )
        };
        let Some(out) = res.outcome().copied() else { return Err(unstable()) };
        let converged = match out.reason {
            ems::engine::StopReason::EnergyDecay { .. } => true,
            ems::engine::StopReason::Aborted if cancelled => {
                return Err(Error::new("cancelled", "operation cancelled", "run it again"));
            }
            ems::engine::StopReason::Diverged => return Err(unstable()),
            _ => false,
        };
        let energy_db = if out.max_energy > 0.0 && out.energy > 0.0 {
            10.0 * (out.energy / out.max_energy).log10()
        } else {
            f64::NEG_INFINITY
        };
        let pulse_steps = (excitation.end() / timestep).ceil() as u64;
        waves.ran(j, out.steps, converged, energy_db, pulse_steps, &mut diags);
        for (i, m) in set.measures.iter().enumerate() {
            let (v, cur) = match *m {
                Measure::Element => {
                    let a = added.lumped[i].analyze(&res, &freqs).map_err(solver_error)?;
                    let c = |x: &[ems::Complex64]| x.iter().map(|c| C64::new(c.re, c.im)).collect::<Vec<_>>();
                    (c(a.voltage()), c(a.current()))
                }
                Measure::Line { v, i: [i1, i2], w: [w1, w2], sign } => {
                    let (c1, c2) =
                        (spectrum(&res, &added.probes[i1], &freqs)?, spectrum(&res, &added.probes[i2], &freqs)?);
                    let cur = c1
                        .iter()
                        .zip(&c2)
                        .map(|(x, y)| (*x * C64::new(w1, 0.0) + *y * C64::new(w2, 0.0)) * C64::new(sign, 0.0))
                        .collect();
                    (spectrum(&res, &added.probes[v], &freqs)?, cur)
                }
            };
            if v.iter().chain(&cur).any(|c| !c.re.is_finite() || !c.im.is_finite()) {
                return Err(unstable());
            }
            waves.set(i, j, set.z0[i], &v, &cur);
        }
    }
    Ok(waves.finish(r, freqs, set.z0, timestep, parts, conformal_edges, diags))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ems's warnings by code: what cadlab made on purpose is dropped, the rest becomes
    /// `sim.ems_<code>` with the design objects of the property the line names.
    #[test]
    fn warnings_keep_their_code_and_subject() {
        let lines = [
            r#"mesh warning: Metal "copper_GND" extends into the PML of face(s) xmin [property "copper_GND" primitive #0]"#,
            r#"probe "cadlab_probe_1": box corners snapped to the nearest mesh lines"#,
            "12 edge(s) of lossy metal are zero-thickness metal or wires and stay perfect conductors; use a \
             ConductingSheet for a lossy sheet",
            "lossy metal with conformal PEC: the surface impedance is on the edges wholly in the metal, which carry \
             the surface of the cut edges next to them too; the covered parts of the cut edges along its outline \
             stay perfectly conducting",
            r#"lumped element "part_R1": 2 edge(s) are PEC and were ignored"#,
            r#"lumped element "part_R1": 1 edge(s) in an absorbing layer were ignored"#,
            r#"probe "port_ut2": box corners snapped to the nearest mesh lines"#,
            "something ems says in words this build has no code for",
        ];
        let subjects = vec![
            ("copper_GND".to_string(), vec![ObjectRef::Net("GND".into())]),
            ("part_R1".to_string(), vec![ObjectRef::Name("R1".into())]),
            ("port_ut2".to_string(), vec![ObjectRef::Pin { component: "J2".into(), pin: "1".into() }]),
        ];
        let mut diags = Vec::new();
        warnings(&ems::warning::collect(&lines), &subjects, &mut diags);
        let got: Vec<(&str, &[ObjectRef])> = diags.iter().map(|d| (d.code.as_ref(), d.subjects.as_slice())).collect();
        assert_eq!(
            got,
            [
                ("sim.ems_lumped_edges", &subjects[1].1[..]),
                ("sim.ems_box_snapped", &subjects[2].1[..]),
                ("sim.ems_other", &[][..]),
            ]
        );
        // The two lines about R1 are one diagnostic that says so.
        assert!(diags[0].message.ends_with("(2 warnings of this kind)"), "{}", diags[0].message);
        assert!(diags.iter().all(|d| d.hint.is_some() && d.message.starts_with("ems: ")));

        // More than the limit: the rest is counted.
        let many: Vec<String> = (0..MAX_WARNINGS + 3).map(|i| format!("unknown wording number {i}")).collect();
        let mut diags = Vec::new();
        warnings(&ems::warning::collect(&many), &subjects, &mut diags);
        assert_eq!(diags.len(), MAX_WARNINGS + 1);
        assert_eq!(diags[MAX_WARNINGS].code, "sim.ems_warnings_omitted");
        assert!(diags[MAX_WARNINGS].message.contains("3 more"), "{}", diags[MAX_WARNINGS].message);
    }
}
