//! Track width for a current and temperature rise.
//!
//! - **IPC-2152** (default): curve fit of the standard's *universal* chart (Figure 5-2: the
//!   baseline, 3 oz conductors in a 0.070" polyimide board without copper planes, in still air,
//!   which IPC-2152 recommends for internal and external conductors). Fit published at
//!   <https://smps.us/pcb-calculator.html> with coefficients by Jack Olson:
//!   `A = (117.555 · ΔT^−0.913 + 1.15) · I^(0.84 · ΔT^−0.108 + 1.159)`, A in mil², I in A (RMS),
//!   ΔT in °C, interpolated between 2 and 100 °C (the tests behind the chart go up to 30 A).
//!   **Approximate.** Checked on 2026-11-13 against the only published readings of that chart
//!   that were found, both at 10 A and 20 °C: 500 mil² (smps.us) and about 520 mil² (Olson,
//!   <https://frontdoor.biz/PCBportal/HowTo2152.pdf>); the fit gives 513.1 mil², +2.6 % and
//!   −1.3 %. The error elsewhere on the chart is not known. The standard's modifiers are not
//!   applied: Olson's worked example gives × 0.84 for 1 oz copper, × 1.08 for a 0.062" board,
//!   × 1.02 for FR-4 and × 0.47 for a plane 20 mil away, and limits the chart to boards larger
//!   than 3 × 3 in. This is **not** IPC-2152's conservative chart, which is the IPC-2221
//!   internal chart below and asks for more copper (680 mil² at 10 A and 20 °C).
//! - **IPC-2221** (legacy): `I = k · ΔT^0.44 · A^0.725`, k = 0.048 for external and 0.024 for
//!   internal conductors (A in mil²), the long-standing published formula derived from the
//!   1950s charts of IPC-2221 / MIL-STD-275. Its internal chart is what IPC-2152 keeps as the
//!   conservative chart (a conductor in free air; M. Jouppi, "The Value of IPC-2152",
//!   <https://www.electronics.org/system/files/technical_resource/E7&S22_03.pdf>): the formula
//!   with k = 0.024 gives 666.7 and 286.9 mil² where Olson reads 680 (10 A, 20 °C) and 300
//!   (4 A, 10 °C) from that chart, −2.0 % and −4.4 %.
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
    /// IPC-2152 universal chart (curve fit, no modifiers): same for internal and external layers.
    #[default]
    Ipc2152,
    /// IPC-2221 legacy formula: external layers may carry about twice the current of internal
    /// ones; on internal layers it is the conservative chart of IPC-2152.
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
    fn ipc2152_fit_against_published_chart_readings() {
        // Universal chart (Figure 5-2) at 10 A, 20 °C: read as 500 mil² (smps.us) and as about
        // 520 mil² (Olson, HowTo2152.pdf). The fit gives 513.1 mil²: +2.6 % and −1.3 %.
        let a = required_area_um2(Method::Ipc2152, 10.0, 20.0, true) / UM2_PER_MIL2;
        assert!((a - 513.1).abs() < 0.1, "{a}");
        assert!((a / 500.0 - 1.0 - 0.026).abs() < 0.001);
        assert!((a / 520.0 - 1.0 + 0.013).abs() < 0.001);
        // It is not the conservative chart, which reads 680 mil² there (Olson).
        assert!(a < 0.8 * 680.0);
    }

    #[test]
    fn ipc2221_internal_is_the_conservative_chart() {
        // Olson's readings of IPC-2152's conservative chart (the IPC-2221 internal chart):
        // 680 mil² at 10 A and 20 °C, 300 mil² at 4 A and 10 °C. The formula is within 5 %.
        for (amps, rise, chart) in [(10.0, 20.0, 680.0), (4.0, 10.0, 300.0)] {
            let a = required_area_um2(Method::Ipc2221, amps, rise, false) / UM2_PER_MIL2;
            assert!((a / chart - 1.0).abs() < 0.05, "{amps} A, {rise} °C: {a}");
            // The universal chart fit asks for less.
            assert!(required_area_um2(Method::Ipc2152, amps, rise, false) / UM2_PER_MIL2 < a);
        }
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
