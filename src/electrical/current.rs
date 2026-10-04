//! Track width for a current and temperature rise.
//!
//! - **IPC-2152** (default): curve fit of the standard's universal chart (Figure 5-2, internal
//!   conductors in a 0.070" polyimide board, no copper planes, which IPC-2152 presents as a
//!   conservative chart for both internal and external conductors). Fit published at
//!   <https://smps.us/pcb-calculator.html> from data points by Jack Olson:
//!   `A = (117.555 · ΔT^−0.913 + 1.15) · I^(0.84 · ΔT^−0.108 + 1.159)`, A in mil², I in A (RMS),
//!   ΔT in °C; stated within 3 % of the chart (e.g. 10 A, 20 °C: 513 mil² vs 500 mil²).
//!   **Approximate:** a fit of a chart, not the standard's text; board thickness, planes and
//!   copper weight modifiers of IPC-2152 are not applied (they would allow narrower tracks).
//! - **IPC-2221** (legacy): `I = k · ΔT^0.44 · A^0.725`, k = 0.048 for external and 0.024 for
//!   internal conductors (A in mil²), the long-standing published formula derived from the
//!   1950s charts of IPC-2221 / MIL-STD-275.
//!
//! See `docs/ELECTRICAL.md`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Square micrometers per square mil.
const UM2_PER_MIL2: f64 = 25.4 * 25.4;

/// Method used for the required cross-section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// IPC-2152 chart (curve fit): same for internal and external layers, conservative.
    #[default]
    Ipc2152,
    /// IPC-2221 legacy formula: external layers may carry about twice the current of internal ones.
    Ipc2221,
}

/// Cross-section area in µm² needed for `amps` (RMS) with a temperature rise of `rise` °C.
/// `external`: outer layer (only matters for IPC-2221).
pub fn required_area_um2(method: Method, amps: f64, rise: f64, external: bool) -> f64 {
    let amps = amps.abs();
    let mil2 = match method {
        Method::Ipc2152 => (117.555 * rise.powf(-0.913) + 1.15) * amps.powf(0.84 * rise.powf(-0.108) + 1.159),
        Method::Ipc2221 => {
            let k = if external { 0.048 } else { 0.024 };
            (amps / (k * rise.powf(0.44))).powf(1.0 / 0.725)
        }
    };
    mil2 * UM2_PER_MIL2
}

/// Track width in µm for `amps` with a rise of `rise` °C, in copper `copper_um` thick.
pub fn required_width_um(method: Method, amps: f64, rise: f64, copper_um: f64, external: bool) -> f64 {
    required_area_um2(method, amps, rise, external) / copper_um
}

/// The current (A) a track `width_um` wide and `copper_um` thick carries for a rise of `rise`
/// °C (the inverse of [`required_area_um2`], by bisection for IPC-2152).
pub fn max_current(method: Method, width_um: f64, copper_um: f64, rise: f64, external: bool) -> f64 {
    let area = width_um * copper_um;
    match method {
        Method::Ipc2221 => {
            let k = if external { 0.048 } else { 0.024 };
            k * rise.powf(0.44) * (area / UM2_PER_MIL2).powf(0.725)
        }
        Method::Ipc2152 => {
            let (mut lo, mut hi) = (0.0f64, 1000.0f64);
            for _ in 0..200 {
                let mid = 0.5 * (lo + hi);
                if required_area_um2(method, mid, rise, external) > area {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            0.5 * (lo + hi)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc2152_fit_published_example() {
        // 10 A, 20 °C rise: the fit gives 513.1 mil² (chart: 500 mil²).
        let a = required_area_um2(Method::Ipc2152, 10.0, 20.0, true) / UM2_PER_MIL2;
        assert!((a - 513.1).abs() < 1.0, "{a}");
        assert!((a - 500.0).abs() / 500.0 < 0.03);
    }

    #[test]
    fn ipc2221_known_values() {
        // 1 A, 10 °C, 1 oz (1.378 mil) external: the classic IPC-2221 result is ~0.3 mm (≈ 11.8 mil).
        let w = required_width_um(Method::Ipc2221, 1.0, 10.0, 35.0, true);
        assert!((w - 300.0).abs() < 10.0, "{w}");
        // Internal layers need ~2.6× the area for the same current (k halved, exponent 1/0.725).
        let wi = required_width_um(Method::Ipc2221, 1.0, 10.0, 35.0, false);
        assert!((wi / w - 2f64.powf(1.0 / 0.725)).abs() < 1e-9);
    }

    #[test]
    fn inverse() {
        for m in [Method::Ipc2152, Method::Ipc2221] {
            let w = required_width_um(m, 3.0, 10.0, 35.0, true);
            let i = max_current(m, w, 35.0, 10.0, true);
            assert!((i - 3.0).abs() < 1e-6, "{m:?} {i}");
        }
        // More rise allowed: narrower track.
        assert!(
            required_width_um(Method::Ipc2152, 2.0, 30.0, 35.0, true)
                < required_width_um(Method::Ipc2152, 2.0, 10.0, 35.0, true)
        );
    }
}
