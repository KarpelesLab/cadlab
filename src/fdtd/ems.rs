//! `sim.sparams --solver ems`: the board region run by the [`ems`] crate's FDTD solver instead of
//! [`super::engine`] (DECISIONS D86; cargo feature `ems`).
//!
//! The region, its mesh, its ports and its parts are the ones the built-in solver gets
//! ([`super::extract`], [`super::place_ports`]); the geometry is the property list of the openEMS
//! export ([`crate::openems::xml::properties`]), so ems runs the model `export.openems` writes:
//! Djordjevic–Sarkar dielectrics as Debye materials on the fitted terms, perfect copper as metal
//! volumes or sheets, lossy copper as conducting sheets on the copper surfaces (roughness folded
//! into the conductivity at f₀), via barrels as cylinders plus their node column, the solder
//! mask, board parts as lumped elements. Ports are ems lumped ports (a resistor of z0 with a
//! Thevenin source); a microstrip port is that source on the line plus a voltage probe and two
//! current loops around its reference plane, read as the built-in solver reads them. One run per
//! port, S = B A⁻¹ on the same waves.

use ems::csx;
use ems::csx::primitives::{BoxShape, Cylinder, LinPoly, Polygon};
use ems::csx::properties::{DebyeMaterial, DebyePole, LumpedElement, LumpedElementType, ProbeBox, ProbeType};
use ems::csx::{ContinuousStructure, Primitive, Property};
use ems::{Boundaries, Boundary, CompileOptions, Cpml, EndCriteria, Fdtd, Waveform};

use super::engine::{EPS0, Gaussian, Hooks, LumpedPort, Probe};
use super::{Error, Extracted, Measure, Options, Plan, PortSet, SParams, Waves, parts};
use crate::diag::Diagnostic;
use crate::electrical::complex::C64;
use crate::model::Project;
use crate::openems::xml::{self, Kind, Prop, Shape};
use crate::openems::{CopperModel, Region};

/// Drawing unit of the model: millimetres, as the export's file.
const UNIT: f64 = 1e-3;
/// Most solver warnings passed on one by one.
const MAX_WARNINGS: usize = 12;

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
        roughness: o.region.roughness,
        lines: [&r.lines[0], &r.lines[1], &r.lines[2]],
        window: r.window,
    });
    let model = structure(&props, &r.lines)?;
    if o.conformal {
        super::unresolved(&r, &mut diags);
    }
    let ports = super::place_ports(&r)?;
    let parts: Vec<super::LumpedPart> = boxes.into_iter().map(|b| b.info).collect();
    super::notes(p, o, &r, &parts, &mut diags);
    limits(o, &r, &mut diags);
    Ok(Prepared { region: r, model, ports, parts, freqs, diags })
}

/// What the built-in solver models and this path does not: said before the run.
fn limits(o: &Options, r: &Region, diags: &mut Vec<Diagnostic>) {
    // A slanted edge of a few cells: a diagonal track, not the arc of a round pad or track end.
    let long = 2.0 * r.mesh.max_cell.0 as f64;
    let slanted = r.copper.iter().any(|c| {
        c.shape.iter().flat_map(|q| std::iter::once(&q.outer).chain(&q.holes)).any(|ring| {
            let n = ring.0.len();
            (0..n).any(|i| {
                let (a, b) = (ring.0[i], ring.0[(i + 1) % n]);
                a.x != b.x && a.y != b.y && ((a.x - b.x) as f64).hypot((a.y - b.y) as f64) >= long
            })
        })
    });
    let lossy = o.region.conductivity.is_some();
    if o.conformal && slanted && (lossy || o.region.copper == CopperModel::Sheet) {
        diags.push(
            Diagnostic::warning(
                "sim.ems_staircase",
                "diagonal copper is a staircase in this run: ems's conformal cells cover solid perfect conductors \
                 only, not sheets on mesh planes or lossy copper (conducting sheets)",
            )
            .with_hint(
                "use `solver: builtin` (conformal cells with lossy and sheet copper), or `copper: solid` with \
                 `perfect_copper`, or a finer mesh (`max_cell`)",
            ),
        );
    }
    if lossy && o.region.roughness != crate::electrical::loss::Roughness::default() {
        diags.push(
            Diagnostic::info(
                "sim.ems_roughness_at_f0",
                "copper roughness is in the sheets' conductivity at the pulse's center only (as in `export.openems`): \
                 the conductor loss is low above it and high below",
            )
            .with_hint("use `solver: builtin` for the roughness factor over the whole band"),
        );
    }
    if lossy && o.region.copper == CopperModel::Solid {
        diags.push(
            Diagnostic::info(
                "sim.ems_copper_faces",
                "lossy solid copper is two conducting sheets on each layer's faces, each of the whole copper \
                 thickness, as in `export.openems`: the sidewalls carry no loss and nothing joins the two faces",
            )
            .with_hint("use `solver: builtin` for a surface impedance on every copper surface"),
        );
    }
}

/// Sets the simulation up and checks it, without running it.
pub(crate) fn plan(p: &Project, o: &Options) -> Result<Plan, Error> {
    let Prepared { region, model, ports, parts, mut diags, .. } = prepare(p, o)?;
    // Compiling the model is what ems checks it with: its own warnings and timestep.
    let mut model = model;
    add_ports(&mut model, &region, &ports, Some(0))?;
    let sim =
        ems::Simulation::with_options(&model, &settings(o, &region, 1), &compile_options(o)).map_err(solver_error)?;
    warnings(sim.warnings(), &mut diags);
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

/// Writes the ports (the one at `excite` driven) and the probes into the model.
fn add_ports(
    model: &mut ContinuousStructure,
    r: &Region,
    set: &PortSet,
    excite: Option<usize>,
) -> Result<Added, Error> {
    let at = |a: usize, i: usize| mm(r.lines[a][i]);
    let mut lumped = Vec::new();
    for (i, LumpedPort { axis: a, lo, hi, positive_high, r: z0 }) in set.ports.iter().enumerate() {
        // The positive terminal is ems's `start`.
        let (mut start, mut stop) =
            ([at(0, lo[0]), at(1, lo[1]), at(2, lo[2])], [at(0, hi[0]), at(1, hi[1]), at(2, hi[2])]);
        if *positive_high {
            (start[*a], stop[*a]) = (stop[*a], start[*a]);
        }
        let mut port = ems::port::LumpedPort::new(i as u32 + 1, *z0, start, stop, ems_axis(*a)).priority(30);
        if excite == Some(i) {
            port = port.excite(1.0);
        }
        lumped.push(port.add_to(model).map_err(solver_error)?);
    }
    let mut probes = Vec::new();
    for (i, probe) in set.probes.iter().enumerate() {
        let name = format!("cadlab_probe_{i}");
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

fn compile_options(o: &Options) -> CompileOptions {
    CompileOptions::default().with_conformal(o.conformal)
}

/// The solver's own warnings, as diagnostics (the first [`MAX_WARNINGS`] distinct ones).
fn warnings(list: &[String], diags: &mut Vec<Diagnostic>) {
    let mut seen: Vec<&str> = Vec::new();
    for w in list {
        // The probes of this module are placed on dual planes on purpose; the mesh, its grading
        // into the absorbing layers and the copper and laminate running through them are cadlab's
        // (the built-in solver runs the same): not news about this run.
        let ours = (w.contains("cadlab_probe_") && w.contains("snapped"))
            || w.contains("neighbour cell size ratio")
            || w.contains("PML cells are strongly graded")
            || w.contains("tiny cells dominate the time step")
            || w.contains("extends into the PML");
        if ours || seen.contains(&w.as_str()) {
            continue;
        }
        seen.push(w);
    }
    for w in seen.iter().take(MAX_WARNINGS) {
        diags.push(
            Diagnostic::warning("sim.ems_warning", format!("ems: {w}"))
                .with_hint("compare with `solver: builtin`; the result may still be valid"),
        );
    }
    if seen.len() > MAX_WARNINGS {
        diags.push(
            Diagnostic::warning(
                "sim.ems_warning",
                format!("ems: {} more warnings not shown", seen.len() - MAX_WARNINGS),
            )
            .with_hint("compare with `solver: builtin`; the result may still be valid"),
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
    let Prepared { region: r, model, ports: set, parts, freqs, mut diags } = prepare(p, o)?;
    let fdtd = settings(o, &r, threads(o, &r));
    let np = set.ports.len();
    let excitation = Gaussian { f0: r.f0 as f64, fc: r.fc as f64 };
    let mut waves = Waves::new(np, freqs.len());
    let mut timestep = 0.0;
    for j in 0..np {
        let mut run_model = model.clone();
        let added = add_ports(&mut run_model, &r, &set, Some(j))?;
        let mut sim = ems::Simulation::with_options(&run_model, &fdtd, &compile_options(o)).map_err(solver_error)?;
        if j == 0 {
            warnings(sim.warnings(), &mut diags);
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
