//! Electrical calculations on the board: transmission line impedance from the stackup and
//! IPC-2152 track widths for a current, plus the DRC checks built on them. See
//! `docs/ELECTRICAL.md`.

pub mod current;
pub mod impedance;

use std::collections::BTreeMap;

use crate::diag::Diagnostic;
use crate::model::Project;
use crate::model::board::Stackup;
use crate::refs::ObjectRef;
use crate::units::{LengthUnit, Nm};
use crate::value::{Quantity, Unit};

pub use impedance::{Geometry, Model};

/// Temperature rise used when a net with a current sets none: 10 °C.
pub const DEFAULT_TEMP_RISE: Quantity = Quantity::from_parts(1, 1, Unit::Celsius);

/// How far a track's impedance may be from its class target before the DRC warns: 10 %.
pub const IMPEDANCE_TOLERANCE: f64 = 0.10;

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

/// The cross-section of a copper layer, from the stackup.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerGeometry {
    /// Model.
    pub model: Model,
    /// Dielectric height to the (first) reference plane.
    pub h: Nm,
    /// Stripline: dielectric height to the other plane; embedded microstrip: cover thickness.
    pub h2: Nm,
    /// Copper thickness.
    pub t: Nm,
    /// Relative permittivity.
    pub er: f64,
    /// Reference layers (plane above / below), by name.
    pub references: Vec<String>,
    /// Whether the dielectrics were assumed (the stackup does not specify them).
    pub assumed: bool,
}

impl LayerGeometry {
    /// The geometry for the formulas, in millimeters.
    pub fn geometry(&self) -> Geometry {
        Geometry { model: self.model, h: mm(self.h), h2: mm(self.h2), t: mm(self.t), er: self.er }
    }
}

/// Why a layer has no geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeometryError {
    /// The layer is not a copper layer of the board.
    UnknownLayer,
    /// Single-layer board: no reference plane.
    NoReference,
}

/// Index of copper layer `layer` in the stackup.
pub fn layer_index(stackup: &Stackup, layer: &str) -> Option<usize> {
    stackup.copper_names().iter().position(|n| n == layer)
}

/// Geometry of copper layer `layer`: outer layers are surface microstrips over the next copper
/// layer, inner layers striplines between their two neighbours (all neighbours are assumed to be
/// reference planes). Permittivity of a stripline is the thickness-weighted mean of its two
/// dielectrics.
pub fn layer_geometry(stackup: &Stackup, layer: &str) -> Result<LayerGeometry, GeometryError> {
    let names = stackup.copper_names();
    let k = layer_index(stackup, layer).ok_or(GeometryError::UnknownLayer)?;
    let n = names.len();
    if n < 2 {
        return Err(GeometryError::NoReference);
    }
    let (diel, assumed) = stackup.effective_dielectrics();
    let er = |i: usize| diel[i].er.to_f64();
    if k == 0 || k == n - 1 {
        let (gap, refl) = if k == 0 { (0, 1) } else { (n - 2, n - 2) };
        return Ok(LayerGeometry {
            model: Model::Microstrip,
            h: diel[gap].thickness,
            h2: Nm::ZERO,
            t: stackup.outer_copper,
            er: er(gap),
            references: vec![names[refl].clone()],
            assumed,
        });
    }
    let (h1, h2) = (diel[k - 1].thickness, diel[k].thickness);
    let (a, b) = (mm(h1), mm(h2));
    let er_mean = if a + b > 0.0 { (er(k - 1) * a + er(k) * b) / (a + b) } else { er(k) };
    Ok(LayerGeometry {
        model: Model::Stripline,
        h: h1,
        h2,
        t: stackup.inner_copper,
        er: er_mean,
        references: vec![names[k - 1].clone(), names[k + 1].clone()],
        assumed,
    })
}

/// Copper thickness of a layer.
pub fn copper_thickness(stackup: &Stackup, layer: &str) -> Nm {
    let n = stackup.copper_names();
    if n.first().is_some_and(|f| f == layer) || n.last().is_some_and(|l| l == layer) {
        stackup.outer_copper
    } else {
        stackup.inner_copper
    }
}

/// Required track width (rounded up to 1 µm) for `current` with `rise` on `layer`.
pub fn required_width(stackup: &Stackup, layer: &str, method: current::Method, amps: &Quantity, rise: &Quantity) -> Nm {
    let copper = copper_thickness(stackup, layer).to_f64(LengthUnit::Um);
    let outer = layer_index(stackup, layer).is_some_and(|k| k == 0 || k + 1 == stackup.copper_names().len());
    let um = current::required_width_um(method, amps.to_f64(), rise.to_f64(), copper, outer);
    Nm::from_um(um.ceil() as i64)
}

/// DRC: tracks of nets with a `current` narrower than IPC-2152 asks on their layer, one
/// warning per net and layer (`drc.current_width`).
pub fn current_check(p: &Project) -> Vec<Diagnostic> {
    let stackup = &p.board().stackup;
    let mut by: BTreeMap<(&str, &str), Vec<(u64, Nm)>> = BTreeMap::new();
    for t in &p.board().tracks {
        let Some(net) = t.net.as_deref() else { continue };
        if p.circuit().nets.get(net).and_then(|n| n.current).is_some() {
            by.entry((net, t.layer.as_str())).or_default().push((t.id.0, t.width));
        }
    }
    let mut out = Vec::new();
    for ((net, layer), tracks) in by {
        let n = &p.circuit().nets[net];
        let amps = n.current.expect("filtered");
        let rise = n.temp_rise.unwrap_or(DEFAULT_TEMP_RISE);
        if amps.to_f64() <= 0.0 || rise.to_f64() <= 0.0 {
            continue;
        }
        let need = required_width(stackup, layer, current::Method::Ipc2152, &amps, &rise);
        let narrow: Vec<&(u64, Nm)> = tracks.iter().filter(|(_, w)| *w < need).collect();
        if narrow.is_empty() {
            continue;
        }
        let min = narrow.iter().map(|(_, w)| *w).min().expect("non-empty");
        let ids: Vec<String> = narrow.iter().map(|(i, _)| format!("track#{i}")).collect();
        let mut d = Diagnostic::warning(
            "drc.current_width",
            format!(
                "net `{net}` carries {amps}: IPC-2152 asks for {need} on {layer} ({} copper, {rise} rise), \
                 {} track(s) are narrower (down to {min}): {}",
                copper_thickness(stackup, layer),
                narrow.len(),
                ids.join(", ")
            ),
        )
        .with_subject(ObjectRef::Net(net.to_string()))
        .with_subject(ObjectRef::Layer(layer.to_string()))
        .with_hint(format!(
            "re-route these tracks at least {need} wide (`current.width --net {net} --netclass <class>` sets a \
             class width), use heavier copper (board.setup), or allow a larger rise (net.set --temp-rise)"
        ));
        for (i, _) in &narrow {
            d = d.with_subject(ObjectRef::Item { kind: "track".into(), index: *i });
        }
        out.push(d);
    }
    out
}

/// DRC: tracks of nets whose class sets a single-ended `impedance` target, more than
/// [`IMPEDANCE_TOLERANCE`] off on their layer (`drc.impedance`); one warning per net and layer.
pub fn impedance_check(p: &Project) -> Vec<Diagnostic> {
    let c = p.circuit();
    let stackup = &p.board().stackup;
    let mut by: BTreeMap<(&str, &str, Nm), Vec<u64>> = BTreeMap::new();
    for t in &p.board().tracks {
        let Some(net) = t.net.as_deref() else { continue };
        let target = c.nets.get(net).and_then(|n| n.class.as_ref()).and_then(|k| c.netclasses.get(k)?.impedance);
        if target.is_some() {
            by.entry((net, t.layer.as_str(), t.width)).or_default().push(t.id.0);
        }
    }
    let mut out = Vec::new();
    for ((net, layer, width), ids) in by {
        let class = c.nets[net].class.as_deref().expect("filtered");
        let target = c.netclasses[class].impedance.expect("filtered");
        let Ok(g) = layer_geometry(stackup, layer) else { continue };
        let z = g.geometry().line(mm(width)).z0;
        let t = target.to_f64();
        if t <= 0.0 || ((z - t) / t).abs() <= IMPEDANCE_TOLERANCE {
            continue;
        }
        let zq = Quantity::from_f64(z, 1, Unit::Ohm).unwrap_or(target);
        let mut d = Diagnostic::warning(
            "drc.impedance",
            format!(
                "net `{net}` (class `{class}`, target {target}): {width} tracks on {layer} are about {zq}{}",
                if g.assumed { " (assumed stackup)" } else { "" }
            ),
        )
        .with_subject(ObjectRef::Net(net.to_string()))
        .with_subject(ObjectRef::Layer(layer.to_string()))
        .with_hint(format!(
            "`impedance.solve {target} --layer {layer}` gives the width for this layer; re-route the tracks at that width"
        ));
        for i in ids {
            d = d.with_subject(ObjectRef::Item { kind: "track".into(), index: i });
        }
        out.push(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::board::Dielectric;

    #[test]
    fn default_stackup_geometry() {
        let s = Stackup::default();
        let g = layer_geometry(&s, "F.Cu").unwrap();
        assert!(g.assumed);
        assert_eq!(g.model, Model::Microstrip);
        assert_eq!(g.h, Nm::from_um(1530));
        assert_eq!(g.references, ["B.Cu"]);
        assert_eq!(layer_geometry(&s, "In1.Cu"), Err(GeometryError::UnknownLayer));
        let one = Stackup { copper_layers: 1, ..Stackup::default() };
        assert_eq!(layer_geometry(&one, "F.Cu"), Err(GeometryError::NoReference));
    }

    #[test]
    fn four_layer_geometry() {
        let d =
            |um, er: &str| Dielectric { thickness: Nm::from_um(um), er: Quantity::parse(er).unwrap(), material: None };
        let s = Stackup {
            copper_layers: 4,
            dielectrics: vec![d(200, "4.4"), d(1065, "4.6"), d(200, "4.4")],
            ..Stackup::default()
        };
        let g = layer_geometry(&s, "In1.Cu").unwrap();
        assert!(!g.assumed);
        assert_eq!(g.model, Model::Stripline);
        assert_eq!(g.references, ["F.Cu", "In2.Cu"]);
        let er = (4.4 * 0.2 + 4.6 * 1.065) / 1.265;
        assert!((g.er - er).abs() < 1e-9);
        let b = layer_geometry(&s, "B.Cu").unwrap();
        assert_eq!(b.references, ["In2.Cu"]);
        assert_eq!(b.h, Nm::from_um(200));
    }
}
