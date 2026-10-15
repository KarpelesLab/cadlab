//! Simulation commands: `sim.sparams` (the built-in FDTD solver, `crate::fdtd`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, ErrorKind, Registry};
use crate::commands::export::{self, Excitation, WrittenFile};
use crate::commands::touchstone::ParamSummary;
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
/// `export.openems`: stackup dielectrics (εr, loss tangent), copper as perfect conductors,
/// vias, lumped or microstrip ports, CPML absorbing boundaries; one run per port. Long:
/// reports progress and can be cancelled; `max_cells` guards the mesh size.
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
    /// End criterion: stop a run when the field energy has decayed by this much (default 40dB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_criteria: Option<Quantity>,
    /// Most timesteps per run (default 100000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timesteps: Option<u64>,
    /// Largest mesh accepted, in cells (default 4000000): a guard against runs of hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cells: Option<u64>,
}

/// Result of `sim.sparams`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Simulated {
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
    /// Smallest and largest |S| over the band, per parameter (all of up to four ports, else the
    /// reflections and the transmissions from port 1).
    pub params: Vec<ParamSummary>,
    /// Nets with copper in the region.
    pub nets: Vec<String>,
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
            air: self.air,
            copper: self.copper.unwrap_or_default(),
            cells_per_wavelength: self.cells_per_wavelength.unwrap_or(20),
            max_cell: self.max_cell,
            min_cell: self.min_cell,
            grading_permille: grading,
            end_criteria_db: decibels(self.end_criteria, "end_criteria", 40)?,
            timesteps: self.timesteps.unwrap_or(100_000).max(1),
            thirds: self.thirds.unwrap_or(true),
            port_planes: true,
        };
        let opts = fdtd::Options { region, max_cells: self.max_cells.unwrap_or(4_000_000), ..fdtd::Options::default() };
        let result = {
            let progress = |done: u64, total: Option<u64>, msg: &str| ctx.progress(done, total, msg);
            let cancelled = || ctx.check_cancelled().is_err();
            fdtd::sparams(p, &opts, &fdtd::Hooks { progress: &progress, cancelled: &cancelled }).map_err(|e| {
                if e.code == "cancelled" {
                    return CommandError::new(ErrorKind::Cancelled, "cancelled", "operation cancelled");
                }
                let base = if e.code.starts_with("openems.unknown") {
                    CommandError::not_found(e.code, e.message)
                } else {
                    CommandError::invalid_args(e.code, e.message)
                };
                let mut c = base.with_hint(e.hint).with_suggestions(&e.suggestions);
                for s in e.subjects {
                    c = c.with_subject(s);
                }
                c
            })?
        };
        let dir = export::resolve(ctx, self.dir.as_deref(), PathBuf::from("out/sim"));
        let ts = &result.touchstone;
        let n = ts.ports;
        let file = format!("{name}.s{n}p");
        let files = export::write(vec![(
            dir.join(&file),
            OutFile { name: file.clone(), function: "S-parameters (Touchstone)".into(), content: ts.write() },
        )])?;
        for d in result.diagnostics {
            ctx.report(d);
        }
        let s = ts.s_params().unwrap_or_default();
        let wanted: Vec<(usize, usize)> = if n <= 4 {
            (0..n).flat_map(|i| (0..n).map(move |j| (i, j))).collect()
        } else {
            (0..n).map(|i| (i, i)).chain((1..n).map(|i| (i, 0))).collect()
        };
        let params =
            wanted.iter().map(|&(i, j)| crate::commands::touchstone::summarize(&ts.freqs, &s, i, j, n, &[])).collect();
        let hz = |v: u64| Quantity::int(v as i64, Unit::Hertz);
        Ok(Simulated {
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
        })
    }

    fn summarize(o: &Simulated) -> String {
        let mut s = format!(
            "simulated {} port(s) on {}×{}×{} lines ({} cells, timestep {} s)",
            o.ports.len(),
            o.mesh.lines[0],
            o.mesh.lines[1],
            o.mesh.lines[2],
            o.mesh.cells,
            o.timestep
        );
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
                "\n  {}: {} .. {} dB ({} Hz .. {} Hz)",
                p.name, p.min.db, p.max.db, p.min.freq, p.max.freq
            ));
        }
        s.push_str(&format!("\nwrote {}", o.touchstone));
        s
    }
}

fn round_sig(v: f64, digits: i32) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let e = v.abs().log10().floor() as i32;
    let f = 10f64.powi(digits - 1 - e);
    (v * f).round() / f
}
