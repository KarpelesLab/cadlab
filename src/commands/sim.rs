//! Simulation commands: `sim.sparams` (the built-in FDTD solver, `crate::fdtd`, or the ems crate's
//! on the same region with `solver: ems`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::commands::export::{self, Excitation, WrittenFile};
use crate::commands::touchstone::{self as ts_cmd, ParamSummary};
use crate::fabout::OutFile;
use crate::fdtd::{self, PortRun};
use crate::geom::Point;
use crate::openems;
use crate::units::Nm;
use crate::value::{Quantity, Unit};

pub(crate) fn register(r: &mut Registry) {
    r.register::<SParams>();
}

/// Simulate a board region with the built-in 3D full-wave solver (FDTD) and write its
/// S-parameters as a Touchstone file. The region, ports and mesh are those of
/// `export.openems`: stackup dielectrics (εr, loss tangent, dispersive with a reference
/// frequency), copper with skin-effect loss and roughness (a surface impedance), optional
/// solder mask, vias, lumped or microstrip ports, CPML absorbing boundaries; one run per port.
/// Two-pin resistors, capacitors and inductors in the region are lumped elements from their part
/// values (`resistance`, `capacitance`, `inductance`, and `esr` / `esl` when given).
/// Long: reports progress and can be cancelled; `max_cells` guards the mesh size, and `plan` shows
/// a run's mesh, ports, parts, warnings and cost without running it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SParams {
    /// Output directory (default out/sim, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Base name of the Touchstone file (default: the project's name): `<name>.sNp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Highest frequency ("10GHz"): the excitation's bandwidth and the mesh resolution.
    pub f_max: Quantity,
    /// Lowest frequency (default 0: the pulse starts at DC, the results at 1 % of `f_max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f_min: Option<Quantity>,
    /// Region: the copper of these nets (pads, tracks, vias; not pours) plus `margin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nets: Vec<String>,
    /// Region: the extent of these items plus `margin`: footprints (`J1`), pads (`J1.1`), vias
    /// (`via#3`), tracks (`track#5`), nets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub around: Vec<String>,
    /// Region: two corners on the board, used as given (combined with `nets` / `around`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<[Point; 2]>,
    /// Margin around `nets` and `around` (default: three board stack heights, at least 1 mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<Nm>,
    /// Keep only the copper of `nets`, the ground nets and the ports' nets.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_selected: bool,
    /// Ground nets kept by `only_selected` (default: nets named like ground).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ground: Vec<String>,
    /// Ports, numbered from 1: `{"at": "J1.1"}` (lumped, at a pad) or
    /// `{"at": "track#4", "end": "end", "kind": "msl"}` (microstrip: S-parameters at the middle of
    /// its `length`); `z0` (default 50 Ω), `reference` layer, `width`, `length`, `layer`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<openems::PortSpec>,
    /// Frequency points (default 201).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<u32>,
    /// Absorbing (CPML) cells on every side of the region (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pml_cells: Option<u8>,
    /// Put the absorbing cells in added cells beyond the region's sides, with copper and
    /// dielectrics that reach a side continued through them (default false: the region's outer
    /// cells; `export.openems` defaults to true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pml_outside: Option<bool>,
    /// Air above and below the board (default: a quarter wavelength at `f_max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air: Option<Nm>,
    /// Copper model: `solid` (default, through its thickness) or `sheet` (zero thickness:
    /// fewer timesteps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copper: Option<openems::CopperModel>,
    /// Mesh cells per wavelength in the material (default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells_per_wavelength: Option<u32>,
    /// Largest mesh cell, instead of the wavelength rule ("0.2mm").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cell: Option<Nm>,
    /// Metal edges closer than this share a mesh line (default: a twentieth of the largest cell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cell: Option<Nm>,
    /// Largest size ratio between neighbouring cells (default 1.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grading: Option<Quantity>,
    /// Thirds rule at copper edges (default true: lines a third of a cell inside and two thirds
    /// outside each edge; false: one line on the edge, as `export.openems` by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thirds: Option<bool>,
    /// End criterion: stop a run when the energy (fields and dielectric polarization) has decayed
    /// by this much (default 60dB: a 40 dB stop leaves tails that bias |S| below f_max / 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_criteria: Option<Quantity>,
    /// Most timesteps per run (default 100000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timesteps: Option<u64>,
    /// Largest mesh accepted, in cells (default 4000000): a guard against runs of hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cells: Option<u64>,
    /// Solder mask thickness over the laminate ("20um"): a dielectric layer on both outer
    /// layers, open at pads with a mask opening (none by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Nm>,
    /// Solder mask thickness over the copper (default: `mask`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_copper: Option<Nm>,
    /// Solder mask relative permittivity (default 3.8, typical LPI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_er: Option<Quantity>,
    /// Solder mask loss tangent (default 0.025, typical LPI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_loss_tangent: Option<Quantity>,
    /// Copper conductivity for the skin-effect loss (default 58 MS/m, annealed copper: "58M").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conductivity: Option<Quantity>,
    /// Lossless copper: perfect conductors, no surface impedance (as `export.openems`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub perfect_copper: bool,
    /// Copper RMS surface roughness (Hammerstad–Jensen factor on the surface resistance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<Nm>,
    /// Huray snowball radius (with `huray_ratio`: Huray's roughness model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_radius: Option<Nm>,
    /// Huray surface ratio: snowball surface per flat area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huray_ratio: Option<Quantity>,
    /// Model the region's two-pin resistors, capacitors and inductors as series R–L–C lumped
    /// elements between their pads, from the part values (default true; false leaves their pads
    /// open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<bool>,
    /// Part values for this simulation only, without changing the design: designator →
    /// `capacitance`, `inductance`, `resistance`, `esr`, `esl`, e.g.
    /// `{"C3": {"capacitance": "1pF"}, "L1": {"esr": "0.3ohm"}}`. To try values of a matching
    /// network; the Touchstone header records them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub part_values: BTreeMap<String, BTreeMap<String, Quantity>>,
    /// Do not run: set the simulation up and report its mesh, ports, parts, warnings and cost.
    /// No file is written.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// Frequencies to report in the result ("2.44GHz"), interpolated: with the band extremes, the
    /// value at the frequency of interest. Reflections add the impedance looking into the port,
    /// VSWR, return loss and mismatch loss.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frequencies: Vec<Quantity>,
    /// Band of interest: the reported best and worst values are over these two frequencies
    /// ("2.4GHz", "2.4835GHz") instead of the whole simulated band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<[Quantity; 2]>,
    /// Conformal copper on diagonal and curved edges (default true: mesh cells cut by slanted
    /// copper use their open lengths and area; false: every edge is all copper or none, a
    /// staircase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conformal: Option<bool>,
    /// `builtin` (default) or `ems` (the ems crate's FDTD, a cross-check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver: Option<fdtd::Solver>,
    /// Worker threads (default: automatic); the result does not depend on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<u32>,
}

/// What a run will cost, from the plan.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SimEstimate {
    /// Runs (one per port).
    pub runs: usize,
    /// Timesteps of the excitation pulse alone: the least one run can take.
    pub pulse_steps: u64,
    /// Rough timesteps per run for a low-Q structure: one and a half pulses (session 6's runs took
    /// 1.03 pulses at 50 dB; docs/TESTING.md). A resonant structure rings longer, up to
    /// `timesteps`.
    pub expected_steps: u64,
    /// Most timesteps per run (`timesteps`).
    pub max_steps: u64,
    /// Cell updates of the whole simulation at `expected_steps`.
    pub cell_updates: u64,
    /// That in minutes on one thread at 50 million cells per second, the rate of a release build
    /// with lossy copper and dispersive dielectrics (session 6): a rough figure, several times
    /// longer on a loaded machine.
    /// The wall-clock time is about this over the threads the grid gets (one per 131072 nodes, up
    /// to the machine's).
    pub thread_minutes: f64,
}

/// Result of `sim.sparams`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Simulated {
    /// Only a plan: nothing ran and no file was written (`touchstone`, `runs` and `params` are
    /// empty).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub planned: bool,
    /// What the run costs (with `plan`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<SimEstimate>,
    /// Files written (the Touchstone file).
    pub files: Vec<WrittenFile>,
    /// The Touchstone file.
    pub touchstone: String,
    /// Board region simulated (two corners).
    pub window: [Point; 2],
    /// Domain bottom and top in z, with the air (z 0 = bottom of the bottom copper).
    pub z_range: [Nm; 2],
    /// Ports.
    pub ports: Vec<openems::PortInfo>,
    /// Mesh summary.
    pub mesh: openems::MeshInfo,
    /// Timestep used (s).
    pub timestep: f64,
    /// Excitation.
    pub excitation: Excitation,
    /// One run per port: timesteps and final energy.
    pub runs: Vec<PortRun>,
    /// Smallest and largest |S| over the band (or `band`), per parameter (all of up to four ports,
    /// else the reflections and the transmissions from port 1), and values at `frequencies`.
    pub params: Vec<ParamSummary>,
    /// Passivity and reciprocity of the result: how far to trust it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<crate::touchstone::Checks>,
    /// Nets with copper in the region.
    pub nets: Vec<String>,
    /// Board parts simulated as lumped elements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<fdtd::LumpedPart>,
    /// Mesh edges partly covered by diagonal or curved copper (conformal cells).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub conformal_edges: usize,
    /// The solver that ran, when not the built-in one.
    #[serde(default, skip_serializing_if = "is_builtin")]
    pub solver: fdtd::Solver,
}

fn is_builtin(s: &fdtd::Solver) -> bool {
    *s == fdtd::Solver::Builtin
}

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Cell updates per second of one thread: a release build with Debye dielectrics and the surface
/// impedance (docs/TESTING.md, session 6). A nominal figure, so the estimate stays deterministic.
const CELLS_PER_SECOND: f64 = 50e6;

/// The values `part_values` may set, with their units.
fn check_part_values(values: &BTreeMap<String, BTreeMap<String, Quantity>>) -> Result<(), CommandError> {
    for (refdes, m) in values {
        for (key, q) in m {
            let unit = match key.as_str() {
                "resistance" | "esr" => Unit::Ohm,
                "capacitance" => Unit::Farad,
                "inductance" | "esl" => Unit::Henry,
                _ => {
                    return Err(CommandError::invalid_args(
                        "sim.bad_part_value",
                        format!("`part_values` of {refdes}: `{key}` is not a simulated value"),
                    )
                    .with_hint("use `resistance`, `capacitance`, `inductance`, `esr` or `esl`"));
                }
            };
            crate::commands::electrical::expect_unit(*q, unit, key)?;
        }
    }
    Ok(())
}

fn decibels(q: Option<Quantity>, field: &str, default: u32) -> Result<u32, CommandError> {
    match q {
        Some(e) if matches!(e.unit, Unit::Decibel | Unit::None) && e.to_f64() != 0.0 => {
            Ok(e.to_f64().abs().round() as u32)
        }
        Some(_) => Err(CommandError::invalid_args(
            "value.wrong_unit",
            format!("`{field}` is an energy decay in dB, e.g. 40dB"),
        )),
        None => Ok(default),
    }
}

impl Command for SParams {
    const NAME: &'static str = "sim.sparams";
    const SUMMARY: &'static str =
        "Simulate a board region with the built-in 3D FDTD solver: S-parameters of its ports as Touchstone";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["dir"];
    type Output = Simulated;

    fn run(self, ctx: &mut Context<'_>) -> Result<Simulated, CommandError> {
        let (mask, mask_notes) =
            export::mask_options(self.mask, self.mask_copper, self.mask_er, self.mask_loss_tangent)?;
        let (conductivity, roughness) = export::copper_options(
            self.conductivity,
            self.perfect_copper,
            self.roughness,
            self.huray_radius,
            self.huray_ratio,
            "sim.perfect_copper",
        )?;
        check_part_values(&self.part_values)?;
        let p = ctx.project()?;
        let name = self.name.clone().unwrap_or_else(|| crate::model::part::slugify(&p.manifest().name));
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(CommandError::invalid_args("sim.bad_name", "`name` must be a plain file name"));
        }
        let grading = match self.grading {
            Some(g) => {
                let g = crate::commands::electrical::expect_unit(g, Unit::None, "grading")?.to_f64();
                if !(g > 1.0 && g <= 3.0) {
                    return Err(CommandError::invalid_args(
                        "value.out_of_range",
                        "`grading` must be above 1 and at most 3 (1.5 is usual)",
                    ));
                }
                (g * 1000.0).round() as u32
            }
            None => 1500,
        };
        let region = openems::Options {
            name: name.clone(),
            title: p.manifest().name.clone(),
            nets: self.nets.clone(),
            around: self.around.clone(),
            window: self.window.map(|[a, b]| (a, b)),
            margin: self.margin,
            only_selected: self.only_selected,
            ground: self.ground.clone(),
            ports: self.ports.clone(),
            f_min_hz: self.f_min.map(|f| export::hertz(f, "f_min")).transpose()?.unwrap_or(0),
            f_max_hz: export::hertz(self.f_max, "f_max")?,
            points: self.points.unwrap_or(201),
            boundary: openems::Boundary::Pml,
            pml_cells: self.pml_cells.unwrap_or(8).max(1),
            pml_outside: self.pml_outside.unwrap_or(false),
            parts: false,
            air: self.air,
            copper: self.copper.unwrap_or_default(),
            cells_per_wavelength: self.cells_per_wavelength.unwrap_or(20),
            max_cell: self.max_cell,
            min_cell: self.min_cell,
            grading_permille: grading,
            end_criteria_db: decibels(self.end_criteria, "end_criteria", 60)?,
            timesteps: self.timesteps.unwrap_or(100_000).max(1),
            thirds: self.thirds.unwrap_or(true),
            port_planes: true,
            mask,
            conductivity,
            roughness,
        };
        // Before a long run: what was asked to be read from the result must be in its band.
        let (lo, hi) = {
            let hi = region.f_max_hz as f64;
            (if region.f_min_hz > 0 { region.f_min_hz as f64 } else { hi / 100.0 }, hi)
        };
        let mut at = Vec::new();
        for q in &self.frequencies {
            let f = crate::commands::electrical::expect_unit(*q, Unit::Hertz, "frequencies")?.to_f64();
            if f < lo || f > hi {
                return Err(CommandError::invalid_args(
                    "touchstone.out_of_band",
                    format!("{q} is outside the simulated band ({} .. {})", ts_cmd::hz(lo), ts_cmd::hz(hi)),
                )
                .with_hint("choose frequencies inside `f_min` .. `f_max`"));
            }
            at.push(f);
        }
        let band = ts_cmd::band_hz(self.band, lo, hi)?;
        let max_steps = region.timesteps;
        let opts = fdtd::Options {
            region,
            max_cells: self.max_cells.unwrap_or(4_000_000),
            parts: self.parts.unwrap_or(true),
            overrides: self.part_values.clone(),
            conformal: self.conformal.unwrap_or(true),
            solver: self.solver.unwrap_or_default(),
            threads: self.threads.unwrap_or(0) as usize,
            ..fdtd::Options::default()
        };
        let to_error = |e: fdtd::Error| {
            if e.code == "cancelled" {
                return CommandError::new(ErrorKind::Cancelled, "cancelled", "operation cancelled");
            }
            let base = if e.code.starts_with("openems.unknown") || e.code == "sim.unknown_part" {
                CommandError::not_found(e.code, e.message)
            } else {
                CommandError::invalid_args(e.code, e.message)
            };
            let mut c = base.with_hint(e.hint).with_suggestions(&e.suggestions);
            for s in e.subjects {
                c = c.with_subject(s);
            }
            c
        };
        if self.plan {
            let plan = fdtd::plan(p, &opts).map_err(to_error)?;
            for d in mask_notes.into_iter().chain(plan.diagnostics) {
                ctx.report(d);
            }
            let hz = |v: u64| Quantity::int(v as i64, Unit::Hertz);
            let expected_steps = (plan.pulse_steps * 3 / 2).min(max_steps);
            let cell_updates = plan.mesh.cells * expected_steps * plan.ports.len() as u64;
            return Ok(Simulated {
                planned: true,
                estimate: Some(SimEstimate {
                    runs: plan.ports.len(),
                    pulse_steps: plan.pulse_steps,
                    expected_steps,
                    max_steps,
                    cell_updates,
                    thread_minutes: round_sig(cell_updates as f64 / CELLS_PER_SECOND / 60.0, 3),
                }),
                files: Vec::new(),
                touchstone: String::new(),
                window: [plan.window.0, plan.window.1],
                z_range: [plan.z_range.0, plan.z_range.1],
                ports: plan.ports,
                mesh: plan.mesh,
                timestep: round_sig(plan.timestep, 4),
                excitation: Excitation { f0: hz(plan.excitation.0), fc: hz(plan.excitation.1) },
                runs: Vec::new(),
                params: Vec::new(),
                checks: None,
                nets: plan.nets,
                parts: plan.parts,
                conformal_edges: plan.conformal_edges,
                solver: opts.solver,
            });
        }
        let mut result = {
            let progress = |done: u64, total: Option<u64>, msg: &str| ctx.progress(done, total, msg);
            let cancelled = || ctx.check_cancelled().is_err();
            fdtd::sparams(p, &opts, &fdtd::Hooks { progress: &progress, cancelled: &cancelled }).map_err(to_error)?
        };
        result.touchstone.comments =
            provenance(opts.solver, &result.ports, &result.parts, &result.window, &self.part_values);
        let dir = export::resolve(ctx, self.dir.as_deref(), PathBuf::from("out/sim"));
        let ts = &result.touchstone;
        let n = ts.ports;
        let file = format!("{name}.s{n}p");
        let files = export::write(vec![(
            dir.join(&file),
            OutFile { name: file.clone(), function: "S-parameters (Touchstone)".into(), content: ts.write() },
        )])?;
        for d in mask_notes.into_iter().chain(result.diagnostics) {
            ctx.report(d);
        }
        let s = ts.s_params().unwrap_or_default();
        let wanted: Vec<(usize, usize)> = if n <= 4 {
            (0..n).flat_map(|i| (0..n).map(move |j| (i, j))).collect()
        } else {
            (0..n).map(|i| (i, i)).chain((1..n).map(|i| (i, 0))).collect()
        };
        let view = ts_cmd::View { at: &at, band, reference: &ts.reference };
        let params = wanted.iter().map(|&(i, j)| ts_cmd::summarize(&ts.freqs, &s, i, j, n, &view)).collect();
        let checks = crate::touchstone::checks(&ts.freqs, &s, n);
        for d in ts_cmd::check_diagnostics(
            &checks,
            "sim",
            "lower `end_criteria` only with care: check that the runs converged, move the absorbing layer away from \
             the copper (`margin`, `air`) and refine the mesh (`max_cell`, `cells_per_wavelength`)",
        ) {
            ctx.report(d);
        }
        let hz = |v: u64| Quantity::int(v as i64, Unit::Hertz);
        Ok(Simulated {
            planned: false,
            estimate: None,
            checks: Some(checks),
            touchstone: dir.join(&file).display().to_string(),
            files: files.files,
            window: [result.window.0, result.window.1],
            z_range: [result.z_range.0, result.z_range.1],
            ports: result.ports,
            mesh: result.mesh,
            timestep: round_sig(result.timestep, 4),
            excitation: Excitation { f0: hz(result.excitation.0), fc: hz(result.excitation.1) },
            runs: result.runs,
            params,
            nets: result.nets,
            parts: result.parts,
            conformal_edges: result.conformal_edges,
            solver: opts.solver,
        })
    }

    fn summarize(o: &Simulated) -> String {
        let mut s = format!(
            "{}{} {} port(s) on {}×{}×{} lines ({} cells, smallest {} / {} / {}, timestep {:.3} ps)",
            if o.planned { "plan (not run):" } else { "simulated" },
            if o.solver == fdtd::Solver::Ems { " with ems:" } else { "" },
            o.ports.len(),
            o.mesh.lines[0],
            o.mesh.lines[1],
            o.mesh.lines[2],
            o.mesh.cells,
            o.mesh.min_cell[0],
            o.mesh.min_cell[1],
            o.mesh.min_cell[2],
            o.timestep * 1e12
        );
        if !o.parts.is_empty() {
            let names: Vec<&str> = o.parts.iter().map(|p| p.refdes.as_str()).collect();
            s.push_str(&format!("\n  lumped parts: {}", names.join(", ")));
        }
        for p in &o.ports {
            s.push_str(&format!("\n  port {}: {} ({}) on {} over {}", p.number, p.at, p.z0, p.layer, p.reference));
        }
        if let Some(e) = &o.estimate {
            s.push_str(&format!(
                "\n  cost: {} run(s), pulse {} timesteps, about {} per run (at most {}), {:.3e} cell updates, about \
                 {} min on one thread",
                e.runs, e.pulse_steps, e.expected_steps, e.max_steps, e.cell_updates as f64, e.thread_minutes
            ));
            return s;
        }
        for r in &o.runs {
            s.push_str(&format!(
                "\n  port {}: {} timesteps, energy {} dB{}",
                r.port,
                r.steps,
                r.energy_db,
                if r.converged { "" } else { " (not converged)" }
            ));
        }
        for p in &o.params {
            s.push_str(&format!(
                "\n  {}: {} dB at {} .. {} dB at {}",
                p.name,
                p.min.db,
                ts_cmd::hz(p.min.freq),
                p.max.db,
                ts_cmd::hz(p.max.freq)
            ));
            for a in &p.at {
                s.push_str(&format!("\n    {}", ts_cmd::point_text(a)));
            }
        }
        s.push_str(&format!("\nwrote {}", o.touchstone));
        s
    }
}

/// The Touchstone header: what the data is, so the file explains itself away from this project.
fn provenance(
    solver: fdtd::Solver,
    ports: &[openems::PortInfo],
    parts: &[fdtd::LumpedPart],
    window: &(Point, Point),
    values: &BTreeMap<String, BTreeMap<String, Quantity>>,
) -> Vec<String> {
    let mut out = vec![format!(
        "cadlab sim.sparams{}: board region ({}, {}) .. ({}, {})",
        if solver == fdtd::Solver::Ems { " (solver: ems)" } else { "" },
        window.0.x,
        window.0.y,
        window.1.x,
        window.1.y
    )];
    for p in ports {
        out.push(format!("port {}: {} on {} over {}, {}", p.number, p.at, p.layer, p.reference, p.z0));
    }
    for q in parts {
        let v: Vec<String> =
            [q.resistance, q.inductance, q.capacitance].iter().flatten().map(|x| x.to_string()).collect();
        let tried = if values.contains_key(&q.refdes) { " (part_values)" } else { "" };
        out.push(format!("lumped {}: {}{tried}", q.refdes, v.join(" ")));
    }
    out
}

fn round_sig(v: f64, digits: i32) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let e = v.abs().log10().floor() as i32;
    let f = 10f64.powi(digits - 1 - e);
    (v * f).round() / f
}
