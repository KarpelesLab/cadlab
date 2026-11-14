//! `sim.sparams --solver ems`: the board region run by the [`ems`] crate's FDTD solver instead of
//! [`super::engine`] (DECISIONS D86, D87; cargo feature `ems`).
//!
//! The region, its mesh, its ports and its parts are the ones the built-in solver gets
//! ([`super::extract`], [`super::place_ports`]); the geometry is the property list of the openEMS
//! export ([`crate::openems::xml::properties`]): Djordjevic–Sarkar dielectrics as Debye materials
//! on the fitted terms, perfect copper as metal volumes or sheets, via barrels as cylinders plus
//! their node column, the solder mask, board parts as lumped elements. Lossy copper is where
//! this model leaves the export's (two conducting sheets per layer, roughness in the conductivity
//! at f₀): solid copper stays metal volumes with a surface impedance on every face, sheet copper
//! is a conducting sheet integrated as a rational two-port, both with the roughness model over
//! the band ([`compile_options`]). Cells are cut where copper is oblique to the mesh
//! ([`compile`]). Ports are ems lumped ports (a resistor of z0 with a Thevenin source, the
//! voltage the mean over the port's columns as in the built-in solver); a microstrip port is
//! that source on the line plus a voltage probe and two current loops around its reference
//! plane, read as the built-in solver reads them. The model is compiled once; one run per port,
//! S = B A⁻¹ on the same waves.

use ems::compile::{ConformalOutlines, SheetModel};
use ems::csx;
use ems::csx::primitives::{BoxShape, Cylinder, LinPoly, Polygon};
use ems::csx::properties::{DebyeMaterial, DebyePole, LumpedElement, LumpedElementType, ProbeBox, ProbeType};
use ems::csx::{ContinuousStructure, Primitive, Property};
use ems::engine::surface::{Conductor, Roughness as EmsRoughness};
use ems::{Boundaries, Boundary, CompileOptions, CompiledModel, Cpml, EndCriteria, Fdtd, WarningCode, Waveform};

use super::engine::{EPS0, Gaussian, Hooks, LumpedPort, Probe};
use super::{Error, Extracted, Measure, Options, Plan, PortSet, SParams, Waves, parts};
use crate::diag::Diagnostic;
use crate::electrical::complex::C64;
use crate::electrical::loss::Roughness;
use crate::model::Project;
use crate::openems::Region;
use crate::openems::xml::{self, Kind, Prop, Shape};
use crate::refs::ObjectRef;

/// Drawing unit of the model: millimetres, as the export's file.
const UNIT: f64 = 1e-3;
/// Most solver warnings passed on one by one.
const MAX_WARNINGS: usize = 12;
/// Name prefix of this module's probes.
const PROBE: &str = "cadlab_probe_";
/// Open-area fractions below which ems takes a cut face's current from its neighbours, tried in
/// this order before ems's own 0.5 (see [`compile`]).
const SMALL_FACES: [f64; 3] = [0.05, 0.1, 0.2];
/// The largest stability estimate (1: the limit) a run with one of [`SMALL_FACES`] is started on.
const STABILITY_MARGIN: f64 = 0.9;

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
        "run with `solver: builtin`; if the region is valid there, report the case to ems",
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
    for p in props {
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
    // Where ems's conformal cells do worse than a staircase, the run is a staircase, and says so.
    let staircase = unresolved(&r);
    let compile = compile_options(o, o.conformal && staircase.is_none(), &props, band);
    if o.conformal {
        super::unresolved(&r, &mut diags);
        if let Some((what, hint, subject)) = staircase {
            let mut d = Diagnostic::warning("sim.ems_staircase", format!("copper is a staircase in this run: {what}"))
                .with_hint(format!("{hint}; `solver: builtin` keeps its conformal cells"));
            if let Some(net) = subject {
                d = d.with_subject(ObjectRef::Net(net));
            }
            diags.push(d);
        }
    }
    let ports = super::place_ports(&r)?;
    let parts: Vec<super::LumpedPart> = boxes.into_iter().map(|b| b.info).collect();
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

/// Plated holes narrower than this many cells are below what ems's conformal cells resolve.
const HOLE_CELLS: f64 = 2.0;

/// What ems's conformal cells cannot take in this region, if anything: what, the hint, the net.
/// Measured (D87):
///
/// - A slanted track under 1.5 cells wide (an explicit `max_cell`: without one the mesh was
///   refined) has no row of whole edges along it, and ems's cut edge is its open part in series
///   with its covered part, so the track comes out in pieces. Session 6's matching network read
///   |S21| 0.57 that way, 0.79 as a staircase, 0.80 in the built-in solver.
/// - A plated hole under two cells across brings a barrel, pads and clearances of a cell or
///   less in radius. On the four-layer via transition (0.3 mm drill, 0.6 mm pads, 0.3 mm cells,
///   perfect sheet copper) |S21| at 4 GHz is −0.43 and −0.44 dB as staircases in the two
///   solvers, −0.38 dB with the built-in solver's conformal cells, −0.35 dB on cells of 0.2 mm,
///   and −0.58 dB with ems's conformal cells: further from the finer mesh than the staircase.
fn unresolved(r: &Region) -> Option<(String, String, Option<String>)> {
    for c in &r.copper {
        if !super::conformal::narrow(&c.shape, &r.lines[0], &r.lines[1], super::UNRESOLVED).is_empty() {
            return Some((
                "ems's conformal cells would cut the unresolved diagonal track into separate pieces".into(),
                "set `max_cell` as the warning about the track says, or leave it out to have the mesh refined".into(),
                c.net.clone(),
            ));
        }
    }
    // The cell a point is in, along one axis.
    let cell = |lines: &[i64], at: i64| {
        let i = lines.partition_point(|&l| l <= at).clamp(1, lines.len().max(2) - 1);
        lines.get(i).zip(lines.get(i - 1)).map_or(0, |(hi, lo)| hi - lo)
    };
    for h in r.holes.iter().filter(|h| h.plated && h.slot.is_none()) {
        let size = cell(&r.lines[0], h.center.x.0).max(cell(&r.lines[1], h.center.y.0));
        if ((2 * h.radius) as f64) < HOLE_CELLS * size as f64 {
            return Some((
                format!(
                    "a plated hole of {} mm on cells of {} mm brings a barrel, pads and clearances that ems's \
                     conformal cells do not resolve (a via transition read 0.23 dB from a finer mesh with them, \
                     0.09 dB as a staircase)",
                    crate::openems::mm(2 * h.radius),
                    crate::openems::mm(size)
                ),
                format!(
                    "set `max_cell` to {} mm or less for conformal cells",
                    crate::openems::mm(((2 * h.radius) as f64 / HOLE_CELLS) as i64)
                ),
                h.net.clone(),
            ));
        }
    }
    None
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
/// staircase the mesh was laid out for, as in the built-in solver) and, for lossy copper, a
/// surface impedance fitted over the result's band on the faces of the copper volumes and as the
/// two-port of the copper sheets, each with the roughness model.
fn compile_options(o: &Options, conformal: bool, props: &[Prop], band: (f64, f64)) -> CompileOptions {
    let mut c = CompileOptions::default()
        .with_conformal(conformal)
        .with_conformal_outlines(ConformalOutlines::Slanted)
        .with_conformal_solids(ConformalOutlines::Slanted);
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

/// Compiles the model. With conformal cells, ems takes the current of a cut face of less than
/// half its area from the faces around it, which keeps any mesh stable at its timestep and costs
/// accuracy where a track is a few cells wide (the bent line of D73 on its default mesh, two
/// cells across: 141° where the built-in solver reads 154.8° and the straight line 163.8°). A
/// board's mesh has cells much thinner than wide, so its timestep is far below what the cut
/// faces need: the threshold is lowered ([`SMALL_FACES`]) as far as ems's own stability estimate
/// stays under [`STABILITY_MARGIN`]; on a mesh without that margin ems's default is used.
fn compile(model: &ContinuousStructure, fdtd: &Fdtd, base: &CompileOptions) -> Result<CompiledModel, Error> {
    if base.conformal {
        for floor in SMALL_FACES {
            let mut opts = base.clone();
            opts.conformal_options.merge_below = floor;
            opts.conformal_options.deviation_area = floor;
            let compiled = CompiledModel::new(model, fdtd, &opts).map_err(solver_error)?;
            // The estimate is made when an engine is built.
            match compiled.simulation_driving(&[]).map_err(solver_error)?.boundaries().conformal {
                None => return Ok(compiled),
                // Nothing is cut: the threshold has nothing to act on.
                Some(r) if r.edges + r.faces + r.sheet_edges + r.sheet_faces == 0 => return Ok(compiled),
                Some(r) if r.verified && r.stability <= STABILITY_MARGIN => return Ok(compiled),
                Some(_) => {}
            }
        }
    }
    CompiledModel::new(model, fdtd, base).map_err(solver_error)
}

/// Sets the simulation up and checks it, without running it.
pub(crate) fn plan(p: &Project, o: &Options) -> Result<Plan, Error> {
    let Prepared { region, model, compile, subjects, ports, parts, mut diags, .. } = prepare(p, o)?;
    // Compiling the model is what ems checks it with: its own warnings and timestep.
    let mut model = model;
    add_ports(&mut model, &region, &ports)?;
    let sim =
        self::compile(&model, &settings(o, &region, 1), &compile)?.simulation_driving(&[]).map_err(solver_error)?;
    warnings(&sim.diagnostics(), &subjects, &mut diags);
    let dt = sim.engine().dt();
    let mut plan = super::planned(region, o.courant, parts, 0, diags);
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

/// The solver settings of a region: the pulse, CPML of `pml_cells` on every face with the
/// built-in solver's frequency shift at f₀, the end criterion, the step limit and the Courant
/// fraction.
fn settings(o: &Options, r: &Region, threads: usize) -> Fdtd {
    let f0 = r.f0 as f64;
    let pml = Cpml::cells(o.region.pml_cells as usize).alpha_max(2.0 * std::f64::consts::PI * EPS0 * f0);
    Fdtd::new()
        .waveform(Waveform::gaussian(f0, r.fc as f64))
        .boundaries(Boundaries::all(Boundary::Pml(pml)))
        .end_criteria(EndCriteria::energy_decay_db(-(o.region.end_criteria_db as f64)).check_interval(32))
        .max_timesteps(o.region.timesteps)
        .timestep_factor(o.courant)
        .threads(threads)
}

/// The solver's own warnings as diagnostics: `sim.ems_<code>` from ems's warning code
/// (`sim.ems_sheet_other`, `sim.ems_lumped_edges`, ...), the design objects behind the property
/// the warning names, one diagnostic per code and property (at most [`MAX_WARNINGS`]).
fn warnings(list: &[ems::Warning], subjects: &[(String, Vec<ObjectRef>)], diags: &mut Vec<Diagnostic>) {
    // The mesh, its grading into the absorbing layers and the copper and laminate running
    // through them are cadlab's (the built-in solver runs the same), and the probes of this
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
    let hint = "compare with `solver: builtin`; the result may still be valid";
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
    let fdtd = settings(o, &r, threads(o, &r));
    let np = set.ports.len();
    let excitation = Gaussian { f0: r.f0 as f64, fc: r.fc as f64 };
    let mut waves = Waves::new(np, freqs.len());
    let mut timestep = 0.0;
    // One compile; every run is a new engine from it, driving its own port.
    let added = add_ports(&mut model, &r, &set)?;
    let compiled = self::compile(&model, &fdtd, &compile)?;
    drop(model);
    for j in 0..np {
        let source = added.lumped[j].properties().and_then(|p| p.excitation.as_ref()).map(|e| e.0.as_str());
        let mut sim = compiled.simulation_driving(source.as_slice()).map_err(solver_error)?;
        if j == 0 {
            warnings(&sim.diagnostics(), &subjects, &mut diags);
        }
        timestep = sim.engine().dt();
        let mut cancelled = false;
        let res = sim
            .run_with(|pr| {
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
                "lower `courant`, or use `solver: builtin`; report the case to ems if it persists",
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
    Ok(waves.finish(r, freqs, set.z0, timestep, parts, 0, diags))
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
