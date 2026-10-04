# Electrical (M8)

Calculations and checks that need more than connectivity and geometry: transmission line impedance from the
stackup, track width for a current, a SPICE netlist for simulation, and a design lint beyond ERC. Code:
`src/electrical/` (impedance, current, DRC checks), `src/spice.rs`, `src/lint.rs`; commands in
`src/commands/electrical.rs`, `export.spice` and `circuit.lint`. Decisions: DECISIONS D29.

Differential pair routing and length matching are router work ([ROUTER.md](ROUTER.md), "Differential pairs"
and "Length tuning", D38); this document covers the numbers they route to. A pair (`diffpair.add`) routes at
its class's `diff_pair_width` / `diff_pair_gap`, the values `impedance.solve --gap --netclass` writes, and
routed lengths include vias measured through the stackup's dielectrics (the same effective stackup as the
impedance calculator).

## Stored data

All values are exact: lengths in `Nm`, electrical values as `Quantity` (decimal mantissa and exponent, see
`src/value.rs`). Floating point is used only inside the formulas; results are rounded when they are stored or
reported (impedances to 0.01 Ω, widths to 1 µm).

| Where | Field | Example | Set with |
|---|---|---|---|
| `board.json` stackup | `dielectrics`: one per gap between copper layers, top to bottom: `thickness`, `er`, optional `material` | `{"thickness": "0.2mm", "er": "4.4", "material": "7628 prepreg"}` | `board.dielectric` |
| `circuit.json` net | `voltage`, `current`, `temp_rise` | `"current": "2A"` | `net.set --voltage 3.3V --current 2A --temp-rise 20C` |
| `circuit.json` net class | `impedance`, `diff_impedance` (targets) | `"impedance": "50Ω"` | `netclass.set`, `impedance.solve --netclass` |
| part parameters | `spice_model`, `spice_lib`, `spice_pins` (text; a component property of the same name wins) | `spice_model=LM358` | `part.set --params`, `circuit.add --properties` |

These are optional fields: projects without them load and save unchanged, so no schema migration was needed
(an older cadlab rejects a file that uses them, as with any new field). Changing the copper layer count with
`board.setup` clears the dielectrics (`board.dielectrics_cleared` warning), since they no longer fit.

## Stackup

`board.stackup` shows copper layers (thickness, line model) and dielectrics; `board.dielectric --gap N` sets
one gap (1 = between `F.Cu` and the next layer), without `--gap` every gap. When the stackup has no dielectrics
(or not one per gap), the calculator **assumes** one: the board thickness minus copper, split equally over the
gaps, at εr 4.5 (a nominal FR-4 value near 1 GHz; laminate datasheets give 4.2–4.8). Results based on it come
with an `impedance.stackup_assumed` warning. That assumption is a placeholder, not a fab value: real stackups
(e.g. 0.2 mm prepreg outside a thick core on four layers) give very different outer-layer impedances, so set
the dielectrics from the fab's published stackup. `board.stackup_thickness` warns when copper plus dielectrics
differ from the board thickness by more than 10 %.

## Impedance (`impedance.calc`, `impedance.solve`)

Layer geometry from the stackup: an outer layer is a **surface microstrip** over the adjacent copper layer
(dielectric height = that gap, copper = outer copper); an inner layer is a **stripline** between its two
neighbours (heights = the two gaps, copper = inner copper, εr = thickness-weighted mean of the two gaps). Every
neighbouring layer is assumed to be a solid reference plane. Explicit `height`, `height2`, `copper`, `er` and
`model` override the stackup (with `height` and `er`, no layer is needed). `model embedded_microstrip` with
`height2` = thickness of the covering dielectric handles buried microstrips.

| Model | Formula | Source | Stated accuracy / range |
|---|---|---|---|
| Surface microstrip | Hammerstad–Jensen quasi-static Z₀ and εeff, with their strip-thickness correction (Δu₁, Δuᵣ) | E. Hammerstad, Ø. Jensen, "Accurate Models for Microstrip Computer-Aided Design", IEEE MTT-S 1980 | < 0.2 % (zero thickness) for 0.01 ≤ W/H ≤ 100, εr ≤ 128; thickness correction ~1 % |
| Embedded microstrip | εr' = εr (1 − e^(−1.55·H₁/H)), H₁ = H + T + cover; Z₀ = 60/√εr' · ln(5.98 H / (0.8 W + T)) | IPC-2141A | **approximate** (commonly quoted ±5 %), 0.1 < W/H < 2, 1 < εr < 15 |
| Symmetric stripline | Wheeler's formula with the thickness correction ΔW | H. A. Wheeler, "Transmission-Line Properties of a Strip Line Between Parallel Planes", IEEE MTT 1978; as given in B. C. Wadell, *Transmission Line Design Handbook*, §3.5.1 | 0.5 % for W/(b − T) < 10 |
| Asymmetric stripline | 2·Z(2h₁+T)·Z(2h₂+T) / (Z(2h₁+T) + Z(2h₂+T)) with the symmetric formula | Wadell §3.5 (Cohn's parallel-combination approximation) | approximate, a few % |
| Edge-coupled differential microstrip | Zdiff = 2 Z₀ (1 − 0.48 e^(−0.96 S/H)) | IPC-2141 (also in National Semiconductor AN-905) | **approximate**, ~±10 %, weak coupling |
| Edge-coupled differential stripline | Zdiff = 2 Z₀ (1 − 0.347 e^(−2.9 S/b)), b = plane spacing | IPC-2141 / AN-905 | **approximate**, ~±10 % |

Not modeled: solder mask over outer layers (it typically lowers a microstrip by 1–3 Ω), frequency dispersion,
etching trapezoid, glass weave, coplanar ground. Inputs outside a formula's range give an
`impedance.out_of_range` warning. For tight-tolerance lines, ask the fab for its field-solver numbers.

`impedance.solve` finds the width by bisection on a logarithmic scale between H/1000 and 100·H (impedance
falls monotonically with width), rounds it to 1 µm and reports the impedance at that width. A target outside
the reachable range is `impedance.unreachable`. With `gap` (or `differential` and the class's
`diff_pair_gap`) it solves the differential pair width for that gap. With `netclass` the result is written
into the class (created if needed): `track_width` + `impedance`, or `diff_pair_width` + `diff_pair_gap` +
`diff_impedance`. A net class has one width, so solve for the layer the class is routed on.

Outputs: `z0`, `zdiff`, `er_eff`, `delay_per_mm` (√εeff / c), and the geometry and formula used.

Tests (`src/electrical/impedance.rs`, `tests/electrical.rs`): Pozar, *Microwave Engineering*, Example 3.7
(microstrip, εr 2.2, 1.59 mm, W = 4.9 mm → 50 Ω, εe 1.87) and Example 3.5 (stripline, b = 3.2 mm, εr 2.2,
W = 2.66 mm → 50 Ω); agreement with Pozar's closed forms within 1.5 % over 0.2 ≤ W/H ≤ 8; thick-strip results
within 8 % of the IPC-2141 estimates in their range; the IPC-2141A embedded formula evaluated directly; solve
round trips.

### DRC: `drc.impedance`

Tracks of a net whose class has an `impedance` target are checked on their layer: a warning per net, layer and
width when the computed Z₀ is more than 10 % from the target (noted when the stackup is assumed). Differential
targets are not checked by the DRC (pair geometry belongs to the router).

## Current and temperature rise (`current.width`, `drc.current_width`)

Required cross-section A for a current I (RMS) and a temperature rise ΔT:

- **IPC-2152 (default)**: curve fit of the standard's universal chart (Figure 5-2: internal conductors in a
  0.070" thick polyimide board without planes, which IPC-2152 gives as a conservative chart for internal and
  external conductors):
  `A [mil²] = (117.555 · ΔT^−0.913 + 1.15) · I^(0.84 · ΔT^−0.108 + 1.159)`.
  Source: <https://smps.us/pcb-calculator.html> (coefficients fitted to chart points provided by Jack Olson),
  stated within 3 % of the chart, e.g. 10 A at 20 °C: 513 mil² against 500 mil² read from the chart (our unit
  test reproduces 513.1). **Approximate**: it is a fit of a chart, the standard itself was not read (it is
  paywalled), and IPC-2152's modifiers for board thickness, copper planes and copper weight are not applied,
  which keeps the result on the conservative (wider) side for most boards.
- **IPC-2221 (`--method ipc2221`)**: the long-published legacy formula `I = k · ΔT^0.44 · A^0.725`
  (A in mil², k = 0.048 external, 0.024 internal). 1 A, 10 °C, 1 oz external → 0.30 mm.

Width = A / copper thickness (outer or inner copper of the layer), rounded up to 1 µm. `current.width` takes a
`current` (or a `net` with `net.set --current`), `temp_rise` (default the net's, else 10 °C), optionally one
`layer`, and lists the width per copper layer; with `netclass` it raises the class's `track_width` to the widest
requirement (never lowers it). The DRC warns (`drc.current_width`, one diagnostic per net and layer, track IDs as
subjects) when tracks of a net with a `current` are narrower than required on their layer. Vias, pads and zone
necks are not checked.

## SPICE export (`export.spice`)

ngspice-dialect netlist, written from the SPICE netlist conventions of the ngspice manual (no simulator code):

| Part category | Written as |
|---|---|
| resistor / capacitor / inductor | `R`/`C`/`L` with `resistance`/`capacitance`/`inductance` (missing value: placeholder) |
| ferrite bead, fuse | `R ... 1m` (DC approximation, `spice.approximated` note) |
| diode, LED | `D anode cathode model` (pins by name A/K, else pin 2/1); no `spice_model`: a default `.model D_<part> D` and `spice.default_model` warning |
| BJT, MOSFET | `Q c b e model`, `M d g s b model` (pins by name C/B/E, D/G/S, bulk = source unless a B pin); needs `spice_model` |
| other parts with pins (ICs, regulators, crystals, oscillators) | `X<ref> <nodes> <spice_model>`; node order = `spice_pins` if given, else pins in number order |
| connectors, test points, mechanical, switches, DNP | comment only (DNP with `include_dnp`) |

A component without a usable model is written **commented out** with a `spice.no_model` warning whose hint
gives the `part.set --params spice_model=... spice_lib=...` fix, so the rest of the circuit still simulates.
`spice_lib` files are `.include`d (paths as given). Node names: the ground net (`ground`, default `GND`, else
the first ground-like name) is `0`; others are sanitized (`D+` → `D_P`, leading digit prefixed with `N`) and made
unique case-insensitively (SPICE names are case-insensitive); unconnected pins get their own `NC_<ref>_<pin>`
node. Element names are the designator, prefixed with the element letter when needed (`XU1`, `RFB1`).

Simulation hooks: `supplies` adds `V<node> <node> 0 DC <v>` for every net with a `voltage`, and for driven nets
whose name gives one (`3V3`, `+5V`, `VCC_1V8`, `5V0`); a driven net without a voltage gets
`spice.supply_unknown`. `analysis` lines (`.op`, `.tran 1u 1m`) are written as-is, `control` lines inside
`.control`/`.endc`. Output is deterministic (components in natural order, golden file
`tests/golden/spice/divider.cir`); the ngspice oracle runs `ngspice -b` on it and checks v(out) = 2.5 V.

## Design lint (`circuit.lint`, `circuit.erc --lint`)

Heuristics for circuits that pass ERC but are probably wrong. Every finding has a stable code, the nets/pins
involved and a fix hint. Ground nets are nets named like ground (`GND`, `AGND`, `VSS`...) or holding a
ground power pin; supply nets hold a power pin, are `driven`, have a `voltage`, or are named like a rail (`3V3`).

| Code | Severity | Rule |
|---|---|---|
| `lint.missing_decoupling` | warning | an MCU/IC/regulator/oscillator power input (and a regulator's output) on a supply net with no two-pin capacitor from that net to ground, once per part and rail |
| `lint.i2c_pullup` | warning | a net named with an `SDA`/`SCL` token (`I2C1_SDA`, `SCL0`), or holding a non-connector pin so named (`PB7/SDA`), without a resistor to a supply net |
| `lint.usb_esd` | warning | a USB data net (`D+`, `D-`, `DP`, `DM`, `DN`, `USB_D+`... in the net name or a connector pin name) that reaches a connector and has no protection part on it (diode category, or `ESD`/`TVS` in the part ID, description or MPN) |
| `lint.clock_termination` | info | exactly one push-pull output drives a clock net (oscillator part, or `CLK`/`CLOCK` in pin or net name) to inputs of other ICs, with no resistor on the net (no series termination) |
| `lint.floating_input` | warning | an IC `input` pin not connected and not marked no-connect, or input pins on a net whose only other pins are capacitors (no DC level) |

Limits: names are heuristics (a pull-up on another sheet of a connector-only bus, or external ESD, give false
positives); "next to the pin" placement of decoupling capacitors is a board matter (`place.near`), not
checked here. `circuit.erc` includes the lint only with `lint: true`, so ERC results stay stable.
