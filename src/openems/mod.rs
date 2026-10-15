//! Export of a board region to openEMS (full-wave FDTD), `export.openems`. See
//! `docs/ELECTRICAL.md`, "Full-wave export (openEMS)".
//!
//! Written from openEMS's published documentation (the CSXCAD XML geometry format, and the
//! Python interface of its tutorials); no openEMS or CSXCAD code is used, linked or bundled.
//! The export has two files:
//!
//! - `<name>.xml`: the CSXCAD geometry inside an `<openEMS>` document: dielectrics from the
//!   stackup (εr, and a conductivity from the loss tangent), copper of every layer as extruded
//!   polygons (or zero-thickness sheets), plated holes and vias as metal cylinders, non-plated
//!   holes as air, and a graded rectilinear mesh (lines at metal edges, λ/N resolution);
//! - `<name>.py`: a driver that reads that geometry, adds the ports (lumped or microstrip), the
//!   Gaussian excitation and the boundary conditions, runs openEMS once per excited port and
//!   writes the S-parameters as a Touchstone file (read back with [`crate::touchstone`]).
//!
//! Coordinates are written in millimeters (`DeltaUnit` 1e-3), exactly from the integer
//! nanometers; z = 0 is the bottom of the bottom copper layer, z grows upward (towards `F.Cu`).

pub mod mesh;
mod script;
mod xml;

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::{self, CopperItem, ItemRef, PlacedPad};
use crate::diag::Diagnostic;
use crate::geom::Point;
use crate::geom::poly::{self, Boolean, FillRule, Location, Op, Polygon, Rect, Ring};
use crate::model::Project;
use crate::model::board::{Dielectric, Stackup};
use crate::refs::ObjectRef;
use crate::units::Nm;
use crate::value::{Quantity, Unit};

use mesh::Fixed;

/// Speed of light in vacuum (m/s).
pub const C0: f64 = 299_792_458.0;
/// Vacuum permittivity (F/m), CODATA 2018.
pub const EPS0: f64 = 8.854_187_812_8e-12;

/// Boundary condition on the six sides of the simulation domain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    /// Perfectly matched layer (`PML_<cells>` cells thick, inside the domain).
    #[default]
    Pml,
    /// Mur's first-order absorbing boundary.
    Mur,
}

/// How copper is modeled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CopperModel {
    /// Perfect conductor polygons extruded through the copper thickness (mesh lines at both
    /// copper surfaces).
    #[default]
    Solid,
    /// Zero-thickness perfect conductor sheets (copper thickness left out of the stack): fewer,
    /// larger cells in z.
    Sheet,
}

/// Port type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    /// A lumped port: a resistive sheet of the reference impedance from the signal copper to the
    /// reference layer, across the pad or track.
    #[default]
    Lumped,
    /// openEMS's microstrip port along a straight, axis-aligned track end (voltage and current
    /// probes on the line; S-parameters referenced to the middle of the port).
    Msl,
}

/// End of a track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackEnd {
    /// The track's start point.
    #[default]
    Start,
    /// The track's end point.
    End,
}

/// A port of the simulation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    /// Where: a pad (`J1.1`) or a track (`track#12`, at its `end`).
    pub at: String,
    /// Which end of a track (default `start`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<TrackEnd>,
    /// `lumped` (default) or `msl` (microstrip port; track ends only).
    #[serde(default)]
    pub kind: PortKind,
    /// Reference impedance (default 50 Ω).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z0: Option<Quantity>,
    /// Signal copper layer, for a pad on several layers (default: the outer layer of the
    /// footprint's side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Reference (return) copper layer (default: the nearest layer with copper of another net
    /// under the port).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Port width across the line (default: the track width, or the pad's narrower side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Nm>,
    /// Microstrip port length along the track (default: the track, at most 20 widths).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<Nm>,
}

/// Solder mask over the outer copper layers, as the field solver's `mask` options: a layer of
/// `thickness` over the laminate and `over_copper` over the copper (conformal, flat on top), open
/// at pads with a mask opening.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskOptions {
    /// Thickness over the laminate.
    pub thickness: Nm,
    /// Thickness over the copper (default: `thickness`).
    pub over_copper: Option<Nm>,
    /// Relative permittivity.
    pub er: f64,
    /// Loss tangent (frequency-independent: a Djordjevic–Sarkar model anchored at the
    /// excitation's center in the solver, a conductivity there in the export).
    pub loss_tangent: f64,
}

/// Export options (see `export.openems` for their meaning and defaults).
#[derive(Clone, Debug)]
pub struct Options {
    /// Base name of the files (`<name>.xml`, `<name>.py`, `<name>.sNp`).
    pub name: String,
    /// Title for comments (the project name).
    pub title: String,
    /// Nets whose copper (pads, tracks, vias; not pours) defines the region.
    pub nets: Vec<String>,
    /// Items whose extent defines the region: footprints, pads, `via#N`, `track#N`, nets.
    pub around: Vec<String>,
    /// Explicit region (two corners), used as given.
    pub window: Option<(Point, Point)>,
    /// Margin around `nets` / `around` (default: three stack heights, at least 1 mm).
    pub margin: Option<Nm>,
    /// Only copper of the selected nets, the ground nets and the ports' nets (default: all
    /// copper in the window).
    pub only_selected: bool,
    /// Ground nets kept with `only_selected` (default: nets named like ground).
    pub ground: Vec<String>,
    /// Ports, numbered from 1 in this order.
    pub ports: Vec<PortSpec>,
    /// Lowest frequency of interest (Hz; 0: from DC).
    pub f_min_hz: u64,
    /// Highest frequency of interest (Hz).
    pub f_max_hz: u64,
    /// Frequency points in the Touchstone file.
    pub points: u32,
    /// Boundary condition.
    pub boundary: Boundary,
    /// PML thickness in cells.
    pub pml_cells: u8,
    /// Air above and below the board (default: a quarter wavelength at `f_max`).
    pub air: Option<Nm>,
    /// Copper model.
    pub copper: CopperModel,
    /// Mesh cells per wavelength in the material (default 20).
    pub cells_per_wavelength: u32,
    /// Largest cell, instead of the wavelength rule.
    pub max_cell: Option<Nm>,
    /// Lines closer than this are merged (default: a twentieth of the largest cell).
    pub min_cell: Option<Nm>,
    /// Largest ratio between neighbouring cells, in thousandths (1500 = 1.5).
    pub grading_permille: u32,
    /// End criterion: stop when the field energy has decayed by this many dB.
    pub end_criteria_db: u32,
    /// Maximum number of timesteps.
    pub timesteps: u64,
    /// Thirds rule at copper edges (mesh lines a third of a cell inside and two thirds outside
    /// instead of one line on the edge).
    pub thirds: bool,
    /// Mesh lines at the microstrip ports' feed and reference planes (where the built-in solver
    /// feeds and measures).
    pub port_planes: bool,
    /// Solder mask on both outer layers (none by default).
    pub mask: Option<MaskOptions>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            name: "board".into(),
            title: String::new(),
            nets: Vec::new(),
            around: Vec::new(),
            window: None,
            margin: None,
            only_selected: false,
            ground: Vec::new(),
            ports: Vec::new(),
            f_min_hz: 0,
            f_max_hz: 0,
            points: 201,
            boundary: Boundary::Pml,
            pml_cells: 8,
            air: None,
            copper: CopperModel::Solid,
            cells_per_wavelength: 20,
            max_cell: None,
            min_cell: None,
            grading_permille: 1500,
            end_criteria_db: 40,
            timesteps: 200_000,
            thirds: false,
            port_planes: false,
            mask: None,
        }
    }
}

/// An export failure: a stable code, a message, the objects involved and a hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Stable code (`openems.no_region`, ...).
    pub code: &'static str,
    /// What is wrong.
    pub message: String,
    /// How to fix it.
    pub hint: String,
    /// Objects involved.
    pub subjects: Vec<ObjectRef>,
    /// Close names, for unknown identifiers.
    pub suggestions: Vec<String>,
}

impl Error {
    fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Error {
        Error { code, message: message.into(), hint: hint.into(), subjects: Vec::new(), suggestions: Vec::new() }
    }
    fn subject(mut self, r: ObjectRef) -> Error {
        self.subjects.push(r);
        self
    }
}

/// A resolved port, as reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PortInfo {
    /// Port number (1-based, the Touchstone index).
    pub number: usize,
    /// What it was placed at.
    pub at: String,
    /// Port type.
    pub kind: PortKind,
    /// Net of the signal copper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Signal copper layer.
    pub layer: String,
    /// Reference copper layer.
    pub reference: String,
    /// Reference impedance.
    pub z0: Quantity,
    /// Position on the board.
    pub position: Point,
    /// Width across the line.
    pub width: Nm,
    /// Microstrip ports: length along the track and the S-parameter reference plane (its
    /// middle).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<Nm>,
    /// Microstrip ports: where the S-parameters are referenced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_plane: Option<Point>,
}

/// The mesh, as reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MeshInfo {
    /// Number of lines in x, y and z.
    pub lines: [usize; 3],
    /// Number of cells.
    pub cells: u64,
    /// Largest cell allowed in the dielectric (λ/N at `f_max`).
    pub max_cell: Nm,
    /// Largest cell allowed in air.
    pub max_cell_air: Nm,
    /// Smallest cell in x, y and z (it sets the FDTD timestep).
    pub min_cell: [Nm; 3],
    /// Estimated timestep (Courant limit of the smallest cells), seconds.
    pub timestep: f64,
}

/// The export: file contents and a report.
#[derive(Clone, Debug)]
pub struct Export {
    /// CSXCAD geometry and mesh (`<name>.xml`).
    pub xml: String,
    /// Python driver (`<name>.py`).
    pub script: String,
    /// Touchstone file name the driver writes (`<name>.sNp`).
    pub touchstone: String,
    /// The board region simulated (x, y).
    pub window: (Point, Point),
    /// Domain bottom and top (z, with the air).
    pub z_range: (Nm, Nm),
    /// Ports.
    pub ports: Vec<PortInfo>,
    /// Mesh summary.
    pub mesh: MeshInfo,
    /// Nets with copper in the region.
    pub nets: Vec<String>,
    /// Gaussian pulse center and 20 dB half-bandwidth (Hz).
    pub excitation: (u64, u64),
    /// Warnings and notes.
    pub diagnostics: Vec<Diagnostic>,
}

/// A copper layer in z (nm).
#[derive(Clone, Debug)]
pub(crate) struct Layer {
    pub(crate) name: String,
    pub(crate) bottom: i64,
    pub(crate) top: i64,
}

/// A dielectric box in z (nm).
#[derive(Clone, Debug)]
pub(crate) struct Diel {
    pub(crate) gap: usize,
    pub(crate) bottom: i64,
    pub(crate) top: i64,
    pub(crate) d: Dielectric,
}

/// The stack: copper layers top to bottom, dielectrics top to bottom, whether assumed.
fn stack(s: &Stackup, copper: CopperModel) -> (Vec<Layer>, Vec<Diel>, bool) {
    let names = s.copper_names();
    let n = names.len();
    let (mut diel, mut assumed) = s.effective_dielectrics();
    if n == 1 {
        let t = Nm((s.thickness.0 - s.outer_copper.0).max(1));
        diel = vec![Dielectric {
            thickness: t,
            er: crate::model::board::DEFAULT_ER,
            loss_tangent: None,
            material: None,
            ref_freq: None,
        }];
        assumed = true;
    }
    let mut layers: Vec<Option<Layer>> = vec![None; n];
    let mut diels: Vec<Option<Diel>> = vec![None; diel.len()];
    let mut z = 0i64;
    for i in (0..n).rev() {
        if let Some(d) = diel.get(i) {
            diels[i] = Some(Diel { gap: i + 1, bottom: z, top: z + d.thickness.0, d: d.clone() });
            z += d.thickness.0;
        }
        let t = match copper {
            CopperModel::Solid => crate::electrical::copper_thickness(s, &names[i]).0,
            CopperModel::Sheet => 0,
        };
        layers[i] = Some(Layer { name: names[i].clone(), bottom: z, top: z + t });
        z += t;
    }
    // Inner copper is embedded in the dielectric above it (prepreg flows around the traces).
    let mut diels: Vec<Diel> = diels.into_iter().flatten().collect();
    let layers: Vec<Layer> = layers.into_iter().flatten().collect();
    for d in &mut diels {
        if let Some(below) = layers.get(d.gap)
            && d.gap < n - 1
        {
            d.bottom = below.bottom;
        }
    }
    (layers, diels, assumed)
}

fn pt(p: Point) -> poly::Point {
    poly::Point::new(p.x.0, p.y.0)
}

fn rect_ring(r: Rect) -> Ring {
    Ring::from(vec![
        poly::Point::new(r.min.x, r.min.y),
        poly::Point::new(r.max.x, r.min.y),
        poly::Point::new(r.max.x, r.max.y),
        poly::Point::new(r.min.x, r.max.y),
    ])
}

fn set_bbox(s: &[Polygon]) -> Option<Rect> {
    s.iter().filter_map(Polygon::bbox).reduce(|a, b| a.union(&b))
}

/// Mesh lines of the thirds rule around copper: at each axis-aligned copper edge, one line a
/// third of a fine cell `δ` inside the copper and one two thirds outside, so the edge falls
/// between lines (an FDTD conductor ending on a line behaves as if wider by about a third of a
/// cell). `δ` is at most `fine` and half the copper's width across the edge; where it would be
/// under twice `min`, the edge gets one line as without the rule. Rounded ends (no
/// axis-aligned edge at the extreme) keep a line at their extreme.
#[allow(clippy::needless_range_loop)]
fn thirds(shape: &[Polygon], fine: i64, min: i64, fx: &mut Vec<Fixed>, fy: &mut Vec<Fixed>) {
    // Axis-aligned edges per axis: (coordinate, span start, span end).
    let mut edges: [Vec<(i64, i64, i64)>; 2] = Default::default();
    for q in shape {
        for r in q.rings() {
            for (a, b) in r.edges() {
                if a.x == b.x && (a.y - b.y).abs() >= min {
                    edges[0].push((a.x, a.y.min(b.y), a.y.max(b.y)));
                }
                if a.y == b.y && (a.x - b.x).abs() >= min {
                    edges[1].push((a.y, a.x.min(b.x), a.x.max(b.x)));
                }
            }
        }
    }
    let probe = (min / 4).clamp(1, 1_000);
    for axis in 0..2 {
        let out = if axis == 0 { &mut *fx } else { &mut *fy };
        for &(at, s0, s1) in &edges[axis] {
            let mid = s0 + (s1 - s0) / 2;
            let inside = |d: i64| {
                let p = if axis == 0 { poly::Point::new(at + d, mid) } else { poly::Point::new(mid, at + d) };
                poly::locate(shape, p) == Location::Inside
            };
            let side = match (inside(probe), inside(-probe)) {
                (true, false) => 1,
                (false, true) => -1,
                _ => {
                    out.push(Fixed::soft(at));
                    continue;
                }
            };
            // The copper's width across the edge: the nearest parallel edge on the copper's side
            // whose span overlaps this one.
            let width = edges[axis]
                .iter()
                .filter(|&&(o, a, b)| (o - at) * side > 0 && a < s1 && b > s0)
                .map(|&(o, _, _)| (o - at).abs())
                .min()
                .unwrap_or(i64::MAX);
            let d = fine.min(width / 2);
            if d < 2 * min {
                out.push(Fixed::soft(at));
                continue;
            }
            out.extend([Fixed::soft(at + side * d / 3), Fixed::soft(at - side * 2 * d / 3)]);
        }
        // Extremes without an axis-aligned edge (rounded ends).
        if let Some(b) = shape.iter().filter_map(Polygon::bbox).reduce(|a, b| a.union(&b)) {
            let (lo, hi) = if axis == 0 { (b.min.x, b.max.x) } else { (b.min.y, b.max.y) };
            for v in [lo, hi] {
                if !edges[axis].iter().any(|e| e.0 == v) {
                    out.push(Fixed::soft(v));
                }
            }
        }
    }
}

/// Splits polygons with holes into hole-free rings: a polygon is cut in two by a vertical
/// line through the middle of its first hole (which opens that hole), recursively.
fn hole_free(polys: &[Polygon], out: &mut Vec<Ring>) {
    for p in polys {
        split(p, out, 0);
    }
}

fn split(p: &Polygon, out: &mut Vec<Ring>, depth: u32) {
    let Some(hole) = p.holes.first() else {
        out.push(p.outer.clone());
        return;
    };
    let (Some(hb), Some(pb)) = (hole.bbox(), p.bbox()) else { return };
    if depth > 64 {
        // Not expected (each cut opens a hole); keep the outline rather than loop.
        out.push(p.outer.clone());
        return;
    }
    let x = hb.min.x + (hb.max.x - hb.min.x) / 2;
    for half in [
        Rect::new(poly::Point::new(pb.min.x - 1, pb.min.y - 1), poly::Point::new(x, pb.max.y + 1)),
        Rect::new(poly::Point::new(x, pb.min.y - 1), poly::Point::new(pb.max.x + 1, pb.max.y + 1)),
    ] {
        let part = poly::boolean(Op::Intersection, p, &rect_ring(half), FillRule::NonZero).unwrap_or_default();
        for q in &part {
            split(q, out, depth + 1);
        }
    }
}

/// Solder mask on one outer side (nm).
pub(crate) struct MaskLayer {
    /// Copper layer index (0: top, the last: bottom).
    pub(crate) layer: usize,
    /// Laminate surface and the mask's surface over it.
    pub(crate) laminate: (i64, i64),
    /// Copper outer surface and the mask's surface over the copper.
    pub(crate) copper: (i64, i64),
    /// The mask's outermost extent (z) and its innermost (the laminate or copper surface).
    pub(crate) span: (i64, i64),
    /// Pad openings (pad copper with a mask opening), clipped to the window.
    pub(crate) openings: Vec<Polygon>,
}

/// A copper group: one net on one layer, clipped to the window.
pub(crate) struct Copper {
    pub(crate) layer: usize,
    pub(crate) net: Option<String>,
    pub(crate) shape: Vec<Polygon>,
}

/// A drilled hole in the region.
pub(crate) struct Hole {
    pub(crate) net: Option<String>,
    pub(crate) plated: bool,
    pub(crate) center: Point,
    pub(crate) radius: i64,
    /// Slots: the outline (extruded instead of a cylinder).
    pub(crate) slot: Option<Polygon>,
    /// Copper layer span (top index, bottom index).
    pub(crate) span: (usize, usize),
}

/// Where a port sits: net, signal layer index, position, span axis (pads) or propagation axis
/// (tracks; 0 x, 1 y), width, and for tracks the direction sign and length.
type Located = (Option<String>, usize, Point, usize, i64, Option<(i64, i64)>);

/// A resolved port with its geometry (nm).
pub(crate) struct Port {
    pub(crate) info: PortInfo,
    /// Sheet corners (nm): lumped ports from the reference surface to the signal surface,
    /// microstrip ports from the track end on the signal surface to the reference surface.
    pub(crate) start: [i64; 3],
    pub(crate) stop: [i64; 3],
    /// Lumped: excitation direction axis; microstrip: propagation axis (0 x, 1 y).
    pub(crate) prop: usize,
    /// Microstrip: feed and measurement plane distances from `start` along `prop`.
    pub(crate) feed_shift: i64,
    pub(crate) meas_shift: i64,
}

fn ground_like(name: &str) -> bool {
    crate::symbolgen::is_ground(name)
}

fn hz(v: u64) -> f64 {
    v as f64
}

/// A board region extracted for full-wave simulation: the stack, the copper, holes, board
/// shape, ports and the graded mesh. Shared by the openEMS export and the built-in FDTD solver
/// ([`crate::fdtd`]).
pub(crate) struct Region {
    pub(crate) layers: Vec<Layer>,
    pub(crate) diels: Vec<Diel>,
    pub(crate) copper: Vec<Copper>,
    pub(crate) holes: Vec<Hole>,
    /// Board outline within the window (dielectrics are only inside it).
    pub(crate) board: Vec<Polygon>,
    pub(crate) ports: Vec<Port>,
    /// Mesh lines in x, y, z (nm).
    pub(crate) lines: [Vec<i64>; 3],
    pub(crate) window: Rect,
    pub(crate) z_range: (i64, i64),
    pub(crate) mesh: MeshInfo,
    pub(crate) nets: Vec<String>,
    /// Gaussian pulse center and 20 dB half-bandwidth (Hz).
    pub(crate) f0: u64,
    pub(crate) fc: u64,
    /// Solder mask per outer side, and its εr and tan δ.
    pub(crate) masks: Vec<MaskLayer>,
    pub(crate) mask_material: (f64, f64),
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// Builds the export.
pub fn export(p: &Project, o: &Options) -> Result<Export, Error> {
    let r = extract(p, o)?;
    let geometry = xml::Geometry {
        title: &o.title,
        layers: &r.layers,
        diels: &r.diels,
        copper: &r.copper,
        holes: &r.holes,
        board: &r.board,
        masks: &r.masks,
        mask_material: r.mask_material,
        kappa_hz: r.f0,
        lines: [&r.lines[0], &r.lines[1], &r.lines[2]],
        window: r.window,
    };
    let xml = xml::write(&geometry);
    let n = r.ports.len();
    let touchstone = format!("{}.s{}p", o.name, n.max(1));
    let script = script::write(o, &r.ports, &touchstone, r.f0, r.fc);
    let mut diags = r.diagnostics;
    if r.ports.is_empty() {
        diags.push(
            Diagnostic::warning("openems.no_ports", "no ports: the driver has nothing to excite")
                .with_hint("add `ports` (pads like `J1.1`, or track ends like `track#3` with `end`)"),
        );
    }
    let window = r.window;
    Ok(Export {
        xml,
        script,
        touchstone,
        window: (Point::new(Nm(window.min.x), Nm(window.min.y)), Point::new(Nm(window.max.x), Nm(window.max.y))),
        z_range: (Nm(r.z_range.0), Nm(r.z_range.1)),
        ports: r.ports.into_iter().map(|p| p.info).collect(),
        mesh: r.mesh,
        nets: r.nets,
        excitation: (r.f0, r.fc),
        diagnostics: diags,
    })
}

/// Extracts the region, its geometry, ports and mesh (no files).
pub(crate) fn extract(p: &Project, o: &Options) -> Result<Region, Error> {
    if o.f_max_hz == 0 {
        return Err(Error::new(
            "openems.no_frequency",
            "`f_max` must be a positive frequency",
            "give the highest frequency of interest, e.g. `f_max: 10GHz`",
        ));
    }
    if o.f_min_hz >= o.f_max_hz {
        return Err(Error::new("openems.bad_frequency", "`f_min` must be below `f_max`", "lower `f_min`"));
    }
    if o.points < 2 {
        return Err(Error::new("openems.bad_points", "`points` must be at least 2", "use e.g. 201"));
    }
    let mut diags = Vec::new();
    let s = &p.board().stackup;
    let (layers, diels, assumed) = stack(s, o.copper);
    if assumed {
        diags.push(
            Diagnostic::warning(
                "openems.stackup_assumed",
                format!(
                    "the stackup has no dielectrics set: assuming equal gaps at εr {}",
                    crate::model::board::DEFAULT_ER
                ),
            )
            .with_hint("set them from the fab's stackup with `board.dielectric --gap N --thickness .. --er .. --loss-tangent ..`"),
        );
    }
    let names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
    let layer_index = |n: &str| names.iter().position(|x| x == n);
    let items = board::copper_items(p);
    let pads = board::placed_pads(p);

    // Region.
    let window = region(p, o, &items, &pads, &layers)?;
    let wring = rect_ring(window);

    // Copper, per layer and net.
    let mut keep: BTreeSet<String> = o.nets.iter().cloned().collect();
    let all_nets: BTreeSet<String> = items.iter().filter_map(|i| i.net.clone()).collect();
    if o.ground.is_empty() {
        keep.extend(all_nets.iter().filter(|n| ground_like(n)).cloned());
    } else {
        keep.extend(o.ground.iter().cloned());
    }
    let mut groups: BTreeMap<(usize, Option<String>), Vec<Polygon>> = BTreeMap::new();
    for it in &items {
        for l in &it.layers {
            let Some(li) = layer_index(l) else { continue };
            let polys: Vec<&Polygon> =
                it.shape.iter().filter(|q| q.bbox().is_some_and(|b| b.intersects(&window))).collect();
            if polys.is_empty() {
                continue;
            }
            groups.entry((li, it.net.clone())).or_default().extend(polys.into_iter().cloned());
        }
    }
    let mut copper: Vec<Copper> = Vec::new();
    for ((layer, net), polys) in groups {
        let clipped = Boolean::new()
            .subject(&polys, FillRule::NonZero)
            .clip(&wring, FillRule::NonZero)
            .op(Op::Intersection)
            .execute()
            .unwrap_or_default();
        if !clipped.is_empty() {
            copper.push(Copper { layer, net, shape: clipped });
        }
    }

    // Ports (before filtering nets: their nets are kept).
    let ports = resolve_ports(p, o, &pads, &layers, &copper, window)?;
    for pt in &ports {
        if let Some(n) = &pt.info.net {
            keep.insert(n.clone());
        }
    }
    if o.only_selected {
        copper.retain(|c| c.net.as_ref().is_some_and(|n| keep.contains(n)));
    }

    // Holes.
    let mut holes = Vec::new();
    let top_bottom = (0, layers.len() - 1);
    for v in &p.board().vias {
        let span: Vec<usize> = board::via_layers(p, v).iter().filter_map(|l| layer_index(l)).collect();
        holes.push(Hole {
            net: v.net.clone(),
            plated: true,
            center: v.at,
            radius: v.drill.0 / 2,
            slot: None,
            span: (span.iter().copied().min().unwrap_or(0), span.iter().copied().max().unwrap_or(top_bottom.1)),
        });
    }
    for pp in &pads {
        let Some((d, plated)) = pp.hole else { continue };
        let span: Vec<usize> = pp.layers.iter().filter_map(|l| layer_index(l)).collect();
        holes.push(Hole {
            net: pp.net.clone(),
            plated: plated && !span.is_empty(),
            center: pp.hole_at,
            radius: d.0 / 2,
            slot: if pp.slot.is_some() { pp.hole_shape() } else { None },
            span: if plated && !span.is_empty() {
                (*span.iter().min().unwrap(), *span.iter().max().unwrap())
            } else {
                top_bottom
            },
        });
    }
    holes.retain(|h| {
        let r = h.radius;
        let b = Rect::new(pt(h.center), pt(h.center)).expand(r);
        b.intersects(&window) && (!o.only_selected || !h.plated || h.net.as_ref().is_some_and(|n| keep.contains(n)))
    });

    // Board shape within the window, for the dielectrics.
    let board_shape: Vec<Polygon> = match board::zones::board_area(p, Nm::ZERO) {
        Ok(Some(area)) => poly::boolean(Op::Intersection, &area, &wring, FillRule::NonZero).unwrap_or_default(),
        _ => vec![Polygon::new(wring.clone(), vec![])],
    };
    if board_shape.is_empty() {
        return Err(Error::new(
            "openems.outside_board",
            "the region does not overlap the board outline",
            "choose nets, items or a window on the board",
        ));
    }

    // Frequencies and resolution.
    let er_max = diels.iter().map(|d| d.d.er.to_f64()).fold(1.0, f64::max);
    let lambda0 = C0 / hz(o.f_max_hz) * 1e9; // nm
    let cpw = o.cells_per_wavelength.max(4) as f64;
    let res_sub = o.max_cell.map(|c| c.0).unwrap_or((lambda0 / er_max.sqrt() / cpw).round() as i64).max(1_000);
    let res_air = o.max_cell.map(|c| c.0).unwrap_or((lambda0 / cpw).round() as i64).max(res_sub);
    let min_xy = o.min_cell.map(|c| c.0).unwrap_or(res_sub / 20).max(1);
    let air = o.air.map(|a| a.0).unwrap_or((lambda0 / 4.0).round() as i64).max(0);
    let ratio = (o.grading_permille.max(1001) as f64) / 1000.0;
    let masks = match &o.mask {
        Some(m) => mask_layers(m, &layers, &pads, &wring)?,
        None => Vec::new(),
    };
    let z_lo = masks.iter().map(|m| m.span.0.min(m.span.1)).fold(0i64, i64::min);
    let z_hi = masks.iter().map(|m| m.span.0.max(m.span.1)).fold(layers[0].top, i64::max);

    // Mesh.
    let mut fx: Vec<Fixed> = Vec::new();
    let mut fy: Vec<Fixed> = Vec::new();
    let add_ring = |r: &Ring, fx: &mut Vec<Fixed>, fy: &mut Vec<Fixed>| {
        if let Some(b) = r.bbox() {
            fx.extend([Fixed::soft(b.min.x), Fixed::soft(b.max.x)]);
            fy.extend([Fixed::soft(b.min.y), Fixed::soft(b.max.y)]);
        }
        for (a, b) in r.edges() {
            if a.x == b.x && (a.y - b.y).abs() >= min_xy {
                fx.push(Fixed::soft(a.x));
            }
            if a.y == b.y && (a.x - b.x).abs() >= min_xy {
                fy.push(Fixed::soft(a.y));
            }
        }
    };
    let h_min = diels.iter().map(|d| d.top - d.bottom).filter(|&t| t > 0).min().unwrap_or(res_sub);
    for c in &copper {
        if o.thirds {
            thirds(&c.shape, res_sub.min(h_min / 2), min_xy, &mut fx, &mut fy);
            continue;
        }
        for q in &c.shape {
            for r in q.rings() {
                add_ring(r, &mut fx, &mut fy);
            }
        }
    }
    for h in &holes {
        match &h.slot {
            Some(q) => add_ring(&q.outer, &mut fx, &mut fy),
            None => {
                fx.extend([Fixed::soft(h.center.x.0 - h.radius), Fixed::soft(h.center.x.0 + h.radius)]);
                fy.extend([Fixed::soft(h.center.y.0 - h.radius), Fixed::soft(h.center.y.0 + h.radius)]);
            }
        }
    }
    for q in &board_shape {
        for r in q.rings() {
            add_ring(r, &mut fx, &mut fy);
        }
    }
    let mut fz: Vec<Fixed> = Vec::new();
    for l in &layers {
        fz.extend([Fixed::hard(l.bottom), Fixed::hard(l.top)]);
    }
    for d in &diels {
        fz.extend([Fixed::hard(d.bottom), Fixed::hard(d.top)]);
    }
    for m in &masks {
        fz.extend([Fixed::hard(m.laminate.1), Fixed::hard(m.copper.1)]);
    }
    for port in &ports {
        for a in 0..2 {
            // With the thirds rule, the port's sides across the line are the copper's edges:
            // their lines are the thirds already (a line on the edge would undo them).
            let across = match port.info.kind {
                PortKind::Lumped => port.start[a] != port.stop[a],
                PortKind::Msl => a != port.prop,
            };
            if o.thirds && across {
                continue;
            }
            let f = if a == 0 { &mut fx } else { &mut fy };
            f.extend([Fixed::hard(port.start[a]), Fixed::hard(port.stop[a])]);
        }
        fz.extend([Fixed::hard(port.start[2]), Fixed::hard(port.stop[2])]);
        if o.port_planes && port.info.kind == PortKind::Msl {
            let a = port.prop;
            let s = (port.stop[a] - port.start[a]).signum();
            let f = if a == 0 { &mut fx } else { &mut fy };
            f.extend([
                Fixed::hard(port.start[a] + s * port.feed_shift),
                Fixed::hard(port.start[a] + s * port.meas_shift),
            ]);
        }
    }
    let xs = mesh::smooth(&mesh::merge(fx, window.min.x, window.max.x, min_xy), &|_, _| res_sub, ratio);
    let ys = mesh::smooth(&mesh::merge(fy, window.min.y, window.max.y, min_xy), &|_, _| res_sub, ratio);
    let zmax = |a: i64, b: i64| -> i64 {
        if a >= z_lo && b <= z_hi {
            // Inside the board (and its mask): λ/N in the substrate, and at least four cells
            // per dielectric.
            let mid = a + (b - a) / 2;
            let t = diels.iter().find(|d| d.bottom <= mid && mid <= d.top).map_or(i64::MAX, |d| (d.top - d.bottom) / 4);
            res_sub.min(t.max(1))
        } else {
            res_air
        }
    };
    let zs = mesh::smooth(&mesh::merge(fz, z_lo - air, z_hi + air, 1_000), &zmax, ratio);
    let (_, mx) = mesh::spacing(&xs);
    let (_, my) = mesh::spacing(&ys);
    let (_, mz) = mesh::spacing(&zs);
    let inv = |d: i64| if d > 0 { 1.0 / ((d as f64) * 1e-9).powi(2) } else { 0.0 };
    let timestep = 1.0 / (C0 * (inv(mx) + inv(my) + inv(mz)).sqrt());
    let mesh_info = MeshInfo {
        lines: [xs.len(), ys.len(), zs.len()],
        cells: (xs.len().saturating_sub(1) * ys.len().saturating_sub(1)) as u64 * zs.len().saturating_sub(1) as u64,
        max_cell: Nm(res_sub),
        max_cell_air: Nm(res_air),
        min_cell: [Nm(mx), Nm(my), Nm(mz)],
        timestep: round_sig(timestep, 4),
    };
    if mesh_info.cells > 50_000_000 {
        diags.push(
            Diagnostic::warning(
                "openems.large_mesh",
                format!("the mesh has {} cells: a long simulation", mesh_info.cells),
            )
            .with_hint("shrink the region (`margin`, `window`), lower `f_max`, or raise `min_cell`"),
        );
    }

    // Ports near the absorbing boundary.
    if o.boundary == Boundary::Pml {
        let n = o.pml_cells as usize;
        for port in &ports {
            let at = feed_point(port);
            let inside = |ls: &[i64], v: i64| ls.len() > 2 * n && (v < ls[n] || v > ls[ls.len() - 1 - n]);
            if inside(&xs, at[0]) || inside(&ys, at[1]) {
                diags.push(
                    Diagnostic::warning(
                        "openems.port_in_pml",
                        format!("port {} ({}) excites inside the {}-cell PML", port.info.number, port.info.at, n),
                    )
                    .with_subject(ObjectRef::parse(&port.info.at).unwrap_or(ObjectRef::Name(port.info.at.clone())))
                    .with_hint("grow the region (`margin`) or move the port away from the region's edge"),
                );
            }
        }
    }

    let nets: Vec<String> = copper.iter().filter_map(|c| c.net.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    let f0 = (o.f_min_hz + o.f_max_hz) / 2;
    let fc = (o.f_max_hz - o.f_min_hz) / 2;
    Ok(Region {
        layers,
        diels,
        copper,
        holes,
        board: board_shape,
        ports,
        lines: [xs, ys, zs],
        window,
        z_range: (z_lo - air, z_hi + air),
        mesh: mesh_info,
        nets,
        f0,
        fc,
        masks,
        mask_material: o.mask.as_ref().map_or((1.0, 0.0), |m| (m.er, m.loss_tangent)),
        diagnostics: diags,
    })
}

/// The solder mask on each outer side: over the laminate from its surface, over the copper from
/// the copper's outer surface, open at pads with a mask opening on that side.
fn mask_layers(m: &MaskOptions, layers: &[Layer], pads: &[PlacedPad], wring: &Ring) -> Result<Vec<MaskLayer>, Error> {
    let (t, tc) = (m.thickness.0, m.over_copper.unwrap_or(m.thickness).0);
    if t <= 0 || tc <= 0 {
        return Err(Error::new(
            "openems.bad_mask",
            "the solder mask thicknesses must be positive",
            "give `mask` (over the laminate) and `mask_copper` (over the copper) such as 20um",
        ));
    }
    if m.er < 1.0 || !(0.0..1.0).contains(&m.loss_tangent) {
        return Err(Error::new(
            "openems.bad_mask",
            "the solder mask needs εr ≥ 1 and 0 ≤ tan δ < 1",
            "give `mask_er` (e.g. 3.8) and `mask_loss_tangent` (e.g. 0.025)",
        ));
    }
    if layers.len() < 2 {
        return Ok(Vec::new());
    }
    let last = layers.len() - 1;
    let mut out = Vec::new();
    for (li, top) in [(0, true), (last, false)] {
        let l = &layers[li];
        let side = if top { crate::model::board::BoardSide::Top } else { crate::model::board::BoardSide::Bottom };
        let dir = if top { 1 } else { -1 };
        let (lam, cu) = if top { (l.bottom, l.top) } else { (l.top, l.bottom) };
        let laminate = (lam, lam + dir * t);
        let over = (cu, cu + dir * tc);
        let outer = if top { laminate.1.max(over.1) } else { laminate.1.min(over.1) };
        let polys: Vec<Polygon> = pads
            .iter()
            .filter(|p| p.layers.contains(&l.name) && p.mask_on(side))
            .map(|p| p.shape_on(&l.name).clone())
            .collect();
        let openings = if polys.is_empty() {
            Vec::new()
        } else {
            Boolean::new()
                .subject(&polys, FillRule::NonZero)
                .clip(wring, FillRule::NonZero)
                .op(Op::Intersection)
                .execute()
                .unwrap_or_default()
        };
        out.push(MaskLayer { layer: li, laminate, copper: over, span: (lam, outer), openings });
    }
    Ok(out)
}

fn round_sig(v: f64, digits: i32) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let e = v.abs().log10().floor() as i32;
    let f = 10f64.powi(digits - 1 - e);
    (v * f).round() / f
}

/// Where a port injects its excitation (x, y).
fn feed_point(p: &Port) -> [i64; 2] {
    match p.info.kind {
        PortKind::Lumped => [(p.start[0] + p.stop[0]) / 2, (p.start[1] + p.stop[1]) / 2],
        PortKind::Msl => {
            let mut at = [(p.start[0] + p.stop[0]) / 2, (p.start[1] + p.stop[1]) / 2];
            let a = p.prop;
            let s = (p.stop[a] - p.start[a]).signum();
            at[a] = p.start[a] + s * p.feed_shift;
            at
        }
    }
}

/// The region: bounding box of the selected nets and items grown by the margin, plus the
/// explicit window.
fn region(p: &Project, o: &Options, items: &[CopperItem], pads: &[PlacedPad], layers: &[Layer]) -> Result<Rect, Error> {
    let mut bbox: Option<Rect> = None;
    let mut add = |r: Option<Rect>| {
        if let Some(r) = r {
            bbox = Some(bbox.map_or(r, |b| b.union(&r)));
        }
    };
    let net_box = |net: &str| -> Option<Rect> {
        set_bbox(
            &items
                .iter()
                .filter(|i| i.net.as_deref() == Some(net) && !matches!(i.item, ItemRef::Zone(..)))
                .flat_map(|i| i.shape.iter().cloned())
                .collect::<Vec<_>>(),
        )
    };
    let known_nets: BTreeSet<&str> = p.circuit().nets.keys().map(String::as_str).collect();
    for n in &o.nets {
        let b = net_box(n);
        if b.is_none() {
            let mut e = if known_nets.contains(n.as_str()) {
                Error::new("openems.net_no_copper", format!("net `{n}` has no copper on the board"), "route it first")
            } else {
                Error::new("openems.unknown_net", format!("net `{n}` does not exist"), "check the name (`net.list`)")
            };
            e.suggestions = crate::suggest::did_you_mean(n, known_nets.iter().copied(), 3);
            return Err(e.subject(ObjectRef::Net(n.clone())));
        }
        add(b);
    }
    for a in &o.around {
        add(Some(item_box(p, a, items, pads, &net_box)?));
    }
    let stack_h = layers.first().map_or(0, |l| l.top);
    let margin = o.margin.map(|m| m.0).unwrap_or((3 * stack_h).max(1_000_000)).max(0);
    let mut r = bbox.map(|b| b.expand(margin));
    if let Some((a, b)) = o.window {
        let w = Rect::new(pt(a), pt(b));
        r = Some(r.map_or(w, |x| x.union(&w)));
    }
    let r = r.ok_or_else(|| {
        Error::new(
            "openems.no_region",
            "no region to export: give `nets`, `around` or `window`",
            "e.g. `around: [\"via#3\"]` for a via transition, `nets: [\"CLK\"]` for a net's path, or `around: [\"J1\"]` for a connector",
        )
    })?;
    if r.width() <= 0 || r.height() <= 0 {
        return Err(Error::new("openems.empty_region", "the region has no area", "give a larger `window` or `margin`"));
    }
    Ok(r)
}

fn item_box(
    p: &Project,
    a: &str,
    items: &[CopperItem],
    pads: &[PlacedPad],
    net_box: &dyn Fn(&str) -> Option<Rect>,
) -> Result<Rect, Error> {
    let unknown = || {
        Error::new(
            "openems.unknown_item",
            format!("`{a}` is not a footprint, pad, via, track or net on the board"),
            "use a designator (`J1`), a pad (`J1.1`), `via#N`, `track#N` or a net name",
        )
        .subject(ObjectRef::Name(a.to_string()))
    };
    let r = ObjectRef::parse(a).map_err(|_| unknown())?;
    let found = match &r {
        ObjectRef::Item { kind, index } if kind == "via" || kind == "track" => items
            .iter()
            .find(|i| match &i.item {
                ItemRef::Via(id) => kind == "via" && id.0 == *index,
                ItemRef::Track(id) => kind == "track" && id.0 == *index,
                _ => false,
            })
            .and_then(|i| set_bbox(&i.shape)),
        ObjectRef::Pin { component, pin } => {
            pads.iter().find(|q| q.refdes == *component && q.number == *pin).and_then(|q| q.shape.bbox())
        }
        ObjectRef::Net(n) => net_box(n),
        ObjectRef::Name(n) => {
            let fp: Vec<Polygon> = pads.iter().filter(|q| q.refdes == *n).map(|q| q.shape.clone()).collect();
            let court = board::placed_courtyard(p, n).and_then(|r| r.bbox());
            let pads_box = set_bbox(&fp);
            match (pads_box, court) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                (a, b) if a.is_some() || b.is_some() => a.or(b),
                _ => net_box(n),
            }
        }
        _ => None,
    };
    found.ok_or_else(unknown)
}

fn resolve_ports(
    p: &Project,
    o: &Options,
    pads: &[PlacedPad],
    layers: &[Layer],
    copper: &[Copper],
    window: Rect,
) -> Result<Vec<Port>, Error> {
    let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
    let mut out = Vec::new();
    for (k, spec) in o.ports.iter().enumerate() {
        let number = k + 1;
        let z0 = match spec.z0 {
            None => Quantity::int(50, Unit::Ohm),
            Some(q) if matches!(q.unit, Unit::Ohm | Unit::None) && q.to_f64() > 0.0 => q.with_unit(Unit::Ohm),
            Some(q) => {
                return Err(Error::new(
                    "openems.bad_impedance",
                    format!("port {number}: `z0` {q} is not a positive impedance"),
                    "give it in ohms, e.g. `50Ω` or `50`",
                ));
            }
        };
        let bad_layer = |l: &str| {
            Error::new(
                "openems.unknown_layer",
                format!("port {number}: `{l}` is not a copper layer"),
                format!("copper layers: {}", names.join(", ")),
            )
            .subject(ObjectRef::Layer(l.to_string()))
        };
        let r = ObjectRef::parse(&spec.at).ok();
        let (net, layer, pos, axis, width, track): Located = match &r {
            Some(ObjectRef::Pin { component, pin }) => {
                let Some(pad) = pads.iter().find(|q| q.refdes == *component && q.number == *pin) else {
                    let mut e = Error::new(
                        "openems.unknown_pad",
                        format!("port {number}: pad `{}` is not on the board", spec.at),
                        "give a placed footprint's pad as `REF.NUMBER` (pad numbers, not pin names)",
                    )
                    .subject(ObjectRef::Name(spec.at.clone()));
                    let all: Vec<String> = pads.iter().map(|q| format!("{}.{}", q.refdes, q.number)).collect();
                    e.suggestions = crate::suggest::did_you_mean(&spec.at, all.iter().map(String::as_str), 3);
                    return Err(e);
                };
                if spec.kind == PortKind::Msl {
                    return Err(Error::new(
                        "openems.msl_needs_track",
                        format!("port {number}: a microstrip port is placed on a track end, not a pad"),
                        "use `kind: lumped` at a pad, or `at: track#N` with `end`",
                    ));
                }
                let layer = match &spec.layer {
                    Some(l) => {
                        if !pad.layers.contains(l) {
                            return Err(bad_layer(l));
                        }
                        l.clone()
                    }
                    None if pad.layers.len() == 1 => pad.layers[0].clone(),
                    None => board::outer_layer(pad.side).to_string(),
                };
                let li = names.iter().position(|n| *n == layer).ok_or_else(|| bad_layer(&layer))?;
                let b = pad.shape_on(&layer).bbox().unwrap_or(Rect::new(pt(pad.center), pt(pad.center)));
                let (w, h) = (b.width(), b.height());
                let (axis, width) = if w <= h { (0, w) } else { (1, h) };
                (pad.net.clone(), li, pad.center, axis, spec.width.map_or(width, |x| x.0), None)
            }
            Some(ObjectRef::Item { kind, index }) if kind == "track" => {
                let Some(t) = p.board().tracks.iter().find(|t| t.id.0 == *index) else {
                    return Err(Error::new(
                        "openems.unknown_item",
                        format!("port {number}: `{}` does not exist", spec.at),
                        "list tracks with `track.list`",
                    )
                    .subject(ObjectRef::Name(spec.at.clone())));
                };
                let (at, other) = match spec.end.unwrap_or_default() {
                    TrackEnd::Start => (t.start, t.mid.unwrap_or(t.end)),
                    TrackEnd::End => (t.end, t.mid.unwrap_or(t.start)),
                };
                let (dx, dy) = (other.x.0 - at.x.0, other.y.0 - at.y.0);
                let prop = if dx.abs() >= dy.abs() { 0 } else { 1 };
                let sign = if prop == 0 { dx.signum() } else { dy.signum() };
                if spec.kind == PortKind::Msl && (t.mid.is_some() || (dx != 0 && dy != 0)) {
                    return Err(Error::new(
                            "openems.msl_not_straight",
                            format!("port {number}: `{}` is not a straight horizontal or vertical track", spec.at),
                            "openEMS microstrip ports run along x or y: use a lumped port, or start the line with an axis-aligned segment",
                        )
                        .subject(ObjectRef::Name(spec.at.clone())));
                }
                let li = names.iter().position(|n| *n == t.layer).ok_or_else(|| bad_layer(&t.layer))?;
                let len = board::track_length(t).0;
                (t.net.clone(), li, at, prop, spec.width.map_or(t.width.0, |x| x.0), Some((sign, len)))
            }
            _ => {
                return Err(Error::new(
                    "openems.bad_port",
                    format!("port {number}: `{}` is not a pad (`J1.1`) or a track (`track#12`)", spec.at),
                    "place ports at pads or track ends",
                )
                .subject(ObjectRef::Name(spec.at.clone())));
            }
        };
        if !window.contains_point(pt(pos)) {
            return Err(Error::new(
                "openems.port_outside",
                format!("port {number} ({}) is outside the region", spec.at),
                "include it with `around`, or grow `margin` / `window`",
            )
            .subject(ObjectRef::Name(spec.at.clone())));
        }
        // Reference layer.
        let reference = match &spec.reference {
            Some(l) => {
                let ri = names.iter().position(|n| n == l).ok_or_else(|| bad_layer(l))?;
                if ri == layer {
                    return Err(Error::new(
                        "openems.bad_reference",
                        format!("port {number}: the reference layer is the signal layer `{l}`"),
                        "choose another copper layer (the plane under the line)",
                    ));
                }
                ri
            }
            None => {
                let mut cands: Vec<usize> = (0..names.len()).filter(|&i| i != layer).collect();
                cands.sort_by_key(|&i| (i.abs_diff(layer), std::cmp::Reverse(i)));
                let under = |i: usize| {
                    copper
                        .iter()
                        .any(|c| c.layer == i && c.net != net && poly::locate(&c.shape, pt(pos)) != Location::Outside)
                };
                match cands.into_iter().find(|&i| under(i)) {
                    Some(i) => i,
                    None => {
                        return Err(Error::new(
                            "openems.no_reference",
                            format!(
                                "port {number} ({}): no copper of another net under it on any other layer",
                                spec.at
                            ),
                            "add a ground plane or pour under the port (`zone.add`), or name the `reference` layer",
                        )
                        .subject(ObjectRef::Name(spec.at.clone())));
                    }
                }
            }
        };
        let (sig, rf) = (&layers[layer], &layers[reference]);
        let (zs, zg) = if layer < reference { (sig.bottom, rf.top) } else { (sig.top, rf.bottom) };
        let (x, y) = (pos.x.0, pos.y.0);
        let half = width / 2;
        let mut info = PortInfo {
            number,
            at: match (&r, spec.end) {
                (Some(ObjectRef::Item { .. }), e) => {
                    format!("{}:{}", spec.at, if e == Some(TrackEnd::End) { "end" } else { "start" })
                }
                _ => spec.at.clone(),
            },
            kind: spec.kind,
            net,
            layer: names[layer].to_string(),
            reference: names[reference].to_string(),
            z0,
            position: pos,
            width: Nm(width),
            length: None,
            reference_plane: None,
        };
        let port = match spec.kind {
            PortKind::Lumped => {
                // Sheet across the line: spans `axis` (lumped at a pad) or the axis across the
                // track's direction.
                let span = if track.is_some() { 1 - axis } else { axis };
                let (mut start, mut stop) = ([x, y, zg], [x, y, zs]);
                start[span] -= half;
                stop[span] += half;
                Port { info, start, stop, prop: 2, feed_shift: 0, meas_shift: 0 }
            }
            PortKind::Msl => {
                let (sign, tlen) = track.unwrap_or((1, 0));
                let len = spec.length.map(|l| l.0).unwrap_or(tlen.min(20 * width)).max(1);
                let across = 1 - axis;
                let (mut start, mut stop) = ([x, y, zs], [x, y, zg]);
                stop[axis] += sign * len;
                start[across] -= half;
                stop[across] += half;
                let mut plane = [x, y];
                plane[axis] += sign * len / 2;
                info.length = Some(Nm(len));
                info.reference_plane = Some(Point::new(Nm(plane[0]), Nm(plane[1])));
                Port { info, start, stop, prop: axis, feed_shift: len / 5, meas_shift: len / 2 }
            }
        };
        out.push(port);
    }
    Ok(out)
}

/// Millimeters, exactly, from nanometers (`1.5`, `-0.000125`).
pub fn mm(nm: i64) -> String {
    let sign = if nm < 0 { "-" } else { "" };
    let a = nm.unsigned_abs();
    let (i, f) = (a / 1_000_000, a % 1_000_000);
    if f == 0 {
        return format!("{sign}{i}");
    }
    let frac = format!("{f:06}");
    format!("{sign}{i}.{}", frac.trim_end_matches('0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mm_is_exact() {
        assert_eq!(mm(0), "0");
        assert_eq!(mm(1_500_000), "1.5");
        assert_eq!(mm(-125), "-0.000125");
        assert_eq!(mm(35_000), "0.035");
        assert_eq!(mm(-2_000_000), "-2");
    }

    #[test]
    fn stack_heights() {
        let mut s = Stackup { copper_layers: 4, ..Stackup::default() };
        s.dielectrics = vec![
            Dielectric {
                thickness: Nm::from_um(200),
                er: Quantity::parse("4.1").unwrap(),
                loss_tangent: None,
                material: None,
                ref_freq: None,
            },
            Dielectric {
                thickness: Nm::from_um(1000),
                er: Quantity::parse("4.5").unwrap(),
                loss_tangent: None,
                material: None,
                ref_freq: None,
            },
            Dielectric {
                thickness: Nm::from_um(200),
                er: Quantity::parse("4.1").unwrap(),
                loss_tangent: None,
                material: None,
                ref_freq: None,
            },
        ];
        let (l, d, assumed) = stack(&s, CopperModel::Solid);
        assert!(!assumed);
        let z: Vec<(i64, i64)> = l.iter().map(|l| (l.bottom, l.top)).collect();
        assert_eq!(z, [(1_470_000, 1_505_000), (1_252_500, 1_270_000), (235_000, 252_500), (0, 35_000)]);
        // Inner copper embedded in the dielectric above it.
        let dz: Vec<(i64, i64)> = d.iter().map(|d| (d.bottom, d.top)).collect();
        assert_eq!(dz, [(1_252_500, 1_470_000), (235_000, 1_252_500), (35_000, 235_000)]);
        let (l, _, _) = stack(&s, CopperModel::Sheet);
        let z: Vec<i64> = l.iter().map(|l| l.top).collect();
        assert_eq!(z, [1_400_000, 1_200_000, 200_000, 0]);
    }

    #[test]
    fn holes_are_opened() {
        let outer = Ring::from(vec![
            poly::Point::new(0, 0),
            poly::Point::new(100, 0),
            poly::Point::new(100, 100),
            poly::Point::new(0, 100),
        ]);
        let h = |x: i64| {
            let mut r = Ring::from(vec![
                poly::Point::new(x, 40),
                poly::Point::new(x + 10, 40),
                poly::Point::new(x + 10, 60),
                poly::Point::new(x, 60),
            ]);
            r.reverse_orientation();
            r
        };
        let p = Polygon::new(outer, vec![h(20), h(70)]);
        let mut out = Vec::new();
        hole_free(&[p], &mut out);
        assert!(out.len() >= 2);
        let area: i128 = out.iter().map(|r| poly::ring_area2(r).abs()).sum();
        assert_eq!(area, 2 * (100 * 100 - 2 * 10 * 20));
    }
}
