# Electrical (M8)

Calculations and checks that need more than connectivity and geometry: transmission line impedance from the
stackup, track width for a current, a SPICE netlist for simulation, a full-wave (openEMS) model of a board
region with Touchstone results read back, a built-in full-wave (FDTD) solver for the S-parameters of a region,
and a design lint beyond ERC. Code: `src/electrical/` (impedance closed forms, 2D field solver, current, DRC
checks), `src/spice.rs`, `src/openems/`, `src/fdtd/`, `src/touchstone.rs`, `src/lint.rs`; commands in
`src/commands/electrical.rs`, `export.spice`, `export.openems`, `sim.sparams`, `touchstone.summary` and
`circuit.lint`. Decisions: DECISIONS D29, D49 (field solver), D52 (losses), D54 (openEMS export, Touchstone),
D59 (FDTD solver).

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
| `board.json` stackup | `dielectrics`: one per gap between copper layers, top to bottom: `thickness`, `er`, optional `loss_tangent` (written as a plain decimal; used by the loss calculations and `export.openems`), optional `material` | `{"thickness": "0.2mm", "er": "4.4", "loss_tangent": "0.02", "material": "7628 prepreg"}` | `board.dielectric` |
| `circuit.json` net | `voltage`, `current`, `temp_rise` | `"current": "2A"` | `net.set --voltage 3.3V --current 2A --temp-rise 20C` |
| `circuit.json` net class | `impedance`, `diff_impedance` (targets) | `"impedance": "50Ω"` | `netclass.set`, `impedance.solve --netclass` |
| part parameters | `spice_model`, `spice_lib`, `spice_pins` (text; a component property of the same name wins) | `spice_model=LM358` | `part.set --params`, `circuit.add --properties` |

These are optional fields: projects without them load and save unchanged, so no schema migration was needed
(an older cadlab rejects a file that uses them, as with any new field). Changing the copper layer count with
`board.setup` clears the dielectrics (`board.dielectrics_cleared` warning), since they no longer fit.

## Stackup

`board.stackup` shows copper layers (thickness, line model) and dielectrics; `board.dielectric --gap N` sets
one gap (1 = between `F.Cu` and the next layer: thickness, εr, loss tangent, material), without `--gap` every
gap. When the stackup has no dielectrics
(or not one per gap), the calculator **assumes** one: the board thickness minus copper, split equally over the
gaps, at εr 4.5 (a nominal FR-4 value near 1 GHz; laminate datasheets give 4.2–4.8). Results based on it come
with an `impedance.stackup_assumed` warning. That assumption is a placeholder, not a fab value: real stackups
(e.g. 0.2 mm prepreg outside a thick core on four layers) give very different outer-layer impedances, so set
the dielectrics from the fab's published stackup: `board.fab_stackup jlcpcb JLC04161H-7628` copies one from
the fab profile in a step (layer count, thickness, copper, every dielectric with εr and material; D53; without
a name it lists them, D55), and
the fab export then names it in `fab-lock.json`. `board.stackup_thickness` warns when copper plus dielectrics
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

Not modeled by the closed forms: solder mask over outer layers (it typically lowers a microstrip by 1–3 Ω),
etching trapezoid, coplanar ground, broadside coupling, a stripline's two dielectrics separately; the field
solver below covers these. Not modeled at all: frequency dispersion of εr and tan δ, glass weave (losses: see
"Losses" below). Inputs outside a
formula's range give an `impedance.out_of_range` warning (its hint points to the field solver, which has no
range limits).

`impedance.solve` finds the width by bisection on a logarithmic scale between H/1000 and 100·H (impedance
falls monotonically with width), rounds it to 1 µm and reports the impedance at that width. A target outside
the reachable range is `impedance.unreachable`. With `gap` (or `differential` and the class's
`diff_pair_gap`) it solves the differential pair width for that gap. With `netclass` the result is written
into the class (created if needed): `track_width` + `impedance`, or `diff_pair_width` + `diff_pair_gap` +
`diff_impedance`. A net class has one width, so solve for the layer the class is routed on.

Outputs: `method`, `z0`, `zdiff`, `er_eff`, `delay_per_mm` (√εeff / c), `c_per_m` and `l_per_m` (capacitance
and inductance per meter of one track, √εeff / (Z₀c) and Z₀√εeff / c), and the geometry and formula used; the
field solver adds, for pairs, `zcomm`, `z_odd` (= Zdiff/2), `z_even` (= 2 Zcomm), `er_eff_odd` and
`er_eff_even`. With frequencies, `loss` (see "Losses").

Tests (`src/electrical/impedance.rs`, `tests/electrical.rs`): Pozar, *Microwave Engineering*, Example 3.7
(microstrip, εr 2.2, 1.59 mm, W = 4.9 mm → 50 Ω, εe 1.87) and Example 3.5 (stripline, b = 3.2 mm, εr 2.2,
W = 2.66 mm → 50 Ω); agreement with Pozar's closed forms within 1.5 % over 0.2 ≤ W/H ≤ 8; thick-strip results
within 8 % of the IPC-2141 estimates in their range; the IPC-2141A embedded formula evaluated directly; solve
round trips.

### Field solver (`method: field_solver`)

A 2D quasi-static solver (`src/electrical/fieldsolver.rs`, DECISIONS D49) computes the capacitance matrix per
unit length of the cross-section with its dielectrics (C) and in vacuum (C₀), from which
L = μ₀ε₀ C₀⁻¹, Z₀ = 1 / (c √(C C₀)), εeff = C / C₀, and for pairs the characteristic impedance matrix
Z = C⁻¹ (C L)^½, whose mixed-mode diagonal gives Zdiff = Z₁₁ − Z₁₂ − Z₂₁ + Z₂₂ and
Zcomm = (Z₁₁ + Z₁₂ + Z₂₁ + Z₂₂) / 4 (2 Z_odd and Z_even / 2 for a symmetric pair; also valid for the
asymmetric broadside microstrip pair). It is selected with `method: field_solver`, or automatically when an
option only it models is given:

| Option | Models |
|---|---|
| `etch` | trapezoidal etch: the top edge inset by this much on each side (the width is the bottom width) |
| `mask`, `mask_copper`, `mask_er` | solder mask: thickness over the laminate, over the copper (default: `mask`), εr (default 3.8, `impedance.mask_er_assumed` note). Conformal over traces on a surface microstrip (surface rising to the copper level with the etch slope, at most 2:1); a flat layer over an embedded microstrip's cover |
| `coplanar_gap` | grounded coplanar waveguide: coplanar grounds at this gap on both sides (of the pair), as wide as the solved region; the plane below is the backing ground (vias are not modeled: a 2D section) |
| `broadside` (+ `broadside_gap`) | broadside-coupled pair: the trace on `layer` and the one on the next copper layer down, between the planes on either side (a stripline pair), or with only one plane a microstrip pair whose upper trace is on the outer layer. Explicit geometry: `height` below the lower trace, `broadside_gap` between the traces, `height2` above the upper one |
| `er2` | the permittivity around and above the trace (stripline upper dielectric, embedded cover, broadside separation) when it differs from `er` |

Without `er2`, a stripline from the stackup keeps its two dielectrics apart (the trace sits in the upper one,
as prepreg flows around it; the closed forms use their thickness-weighted mean). Microstrip and embedded
microstrip have one ground plane and air (or mask) above; stripline two planes.

**Method**: boundary elements (method of moments) with piecewise-constant total (free + polarization) charge
on conductor surfaces and dielectric interfaces, in a vacuum Green's function that already satisfies the
ground planes: one image for a single plane, the closed-form parallel-plate Green's function between two
planes. Conductor panels hold the conductor potential, interface panels the continuity of normal D; panel
integrals of the logarithmic kernel and its gradient are analytic, the smooth remainder of the parallel-plate
function is integrated by 4-point Gauss quadrature, and far panels use 2-point quadrature. Panels grade
geometrically towards conductor corners (size 0.25 × the distance to the nearest corner, down to 0.1 % of the
smallest conductor feature). Dielectric interfaces and coplanar grounds are truncated at 60 stack heights
(open top) or 8 plane spacings (stripline) beyond the conductors. Every structure is symmetric about the
trace (or pair) centre, so only half is discretized, with even and odd mirror kernels. Dense LU with
partial pivoting in a fixed order: deterministic. Zero-thickness conductors are single sheets whose charge is
split between the two faces from the normal field. Sources: R. F. Harrington, *Field Computation by Moment
Methods* (1968), ch. 2; C. Wei, R. F. Harrington, J. R. Mautz, T. K. Sarkar, "Multiconductor Transmission
Lines in Multilayered Dielectric Media", IEEE Trans. MTT-32 (1984); the parallel-plate Green's function is the
summed image series of a line charge between grounded plates (a standard textbook result).

**Accuracy** (tests in `src/electrical/fieldsolver.rs`, `tests/electrical.rs`):

| Case | Reference | Agreement |
|---|---|---|
| zero-thickness centred stripline, W/b 0.1 to 3 | exact: Z₀ = η₀ / (4√εr) · K(k)/K(k′), k = sech(πW/2b) (S. B. Cohn, IRE Trans. MTT 1954) | < 0.1 % (0.02 % at W/b = 0.4) |
| zero-thickness edge-coupled stripline, even and odd modes | exact: Cohn, IRE Trans. MTT 1955, k_e = tanh(πW/2b) tanh(π(W+S)/2b), k_o = tanh(πW/2b) coth(π(W+S)/2b) | < 0.1 % |
| zero-thickness microstrip, W/H 0.25 to 4, εr 2.2 to 10 | Hammerstad–Jensen (0.2 %) | < 0.3 % (Z₀ and εeff) |
| thick microstrip and stripline (35 / 17.5 µm) | Hammerstad–Jensen with thickness correction, Wheeler | < 1.5 % |
| asymmetric stripline | parallel-combination approximation | < 3 % (the approximation's own error) |
| grounded coplanar waveguide, zero thickness | conformal mapping (conductor-backed CPW, R. N. Simons, *Coplanar Waveguide Circuits, Components, and Systems*, Wiley 2001) | < 1 % (0.5 % typical) |
| solder mask, etch, split dielectrics, broadside, CPWG pair | default discretization against a 2.5× finer one | < 0.05 % |
| homogeneous dielectric | εeff = εr exactly | 1e-9 |

Physical checks: a 25 µm mask lowers a 50 Ω microstrip by 1–3 Ω; a mask of εr 1 equals no mask; inductance
does not depend on the dielectrics; an etch puts Z₀ between the bottom- and top-width rectangles; a thick
cover takes εeff to εr; widely spaced pairs give Zdiff = 2 Z₀ and Zcomm = Z₀ / 2.

**Speed** (release build): a single line in a few milliseconds; a masked CPWG pair on 1.5 mm FR-4 in about
0.13 s. `impedance.solve` starts from the closed-form width, brackets on a logarithmic scale and refines by
regula falsi, first at a coarser discretization and then at the default one (about 0.5 s for that pair).
The DRC's `drc.impedance` check keeps the closed forms.

### Losses (`impedance.calc --freq`, `net.loss`)

With `freq` (one or more frequencies) or a sweep (`freq_from`, `freq_to`, `freq_points`, logarithmic, default
10 points, each rounded to 4 significant digits), `impedance.calc` adds `loss`: per frequency the attenuation
`alpha_db_per_m` / `alpha_db_per_in` (Re γ, γ = √((R + jωL)(G + jωC))), its conductor and dielectric parts
(R / 2Z₀ and G Z₀ / 2 with the lossless Z₀), the complex characteristic impedance `z0_real` / `z0_imag`
(√((R + jωL)/(G + jωC))), `r_per_m`, `l_per_m` (external plus internal), `g_per_m`, `c_per_m`, the skin depth,
and for a pair with the field solver the differential attenuation `alpha_diff_db_per_m` / `_per_in`. Inputs:

| Input | Default | Notes |
|---|---|---|
| dielectric `loss_tangent` | the stackup's (`board.dielectric --loss-tangent`), else 0.02 with an `impedance.loss_tangent_assumed` warning | `loss_tangent` overrides every laminate dielectric; tan δ is taken constant over frequency |
| `mask_loss_tangent` | 0.025 (typical LPI; `impedance.mask_loss_tangent_assumed` note) | field solver with `mask` |
| `conductivity` | 58 MS/m (annealed copper, IEC 60028) | S/m (`"58M"` or `"58MS"`) |
| `roughness` | smooth | RMS roughness, Hammerstad–Jensen factor |
| `huray_radius` + `huray_ratio` | smooth | Huray snowball radius a and surface ratio SR = N·4πa²/A_flat |

**Conductor loss.** Rs = √(πfμ₀/σ), skin depth δ = 1/√(πfμ₀σ). R = K · Rs · (R/Rs), where R/Rs is a
property of the cross-section:

- closed forms: Wheeler's incremental inductance rule (H. A. Wheeler, "Formulas for the Skin Effect",
  Proc. IRE 1942), R = (Rs/μ₀) ∂L/∂n, every conductor wall receding by n (the strip loses n on each face,
  each plane moves n away), applied to the closed-form model's L = Z₀√εeff / c by a central difference;
- field solver: the perturbation (power-loss) method R = Rs ∮|Jₛ|² dl / I², with the surface current of the
  quasi-TEM mode: its magnetic field is the vacuum structure's, so Jₛ = c ρₛ, the surface charge of the vacuum
  boundary element solution on traces and coplanar grounds, plus the charge it induces on the ground planes
  (closed-form integrals of the plane Green's functions along each panel, integrated along the planes by
  graded Gauss quadrature). The corners are refined 1000× further than for the capacitance (the current
  crowds there, σ ~ r^(−1/3)). For a pair, R is the 2×2 matrix of the unit-current distributions.

Roughness multiplies R: Hammerstad–Jensen K = 1 + (2/π) atan(1.4 (Δ/δ)²) (Hammerstad & Jensen 1980, the paper
of the microstrip model), or Huray's snowball model K = 1 + (3/2) SR / (1 + δ/a + δ²/(2a²)) (P. G. Huray et al.,
"Impact of Copper Surface Texture on Loss: A Model that Works", DesignCon 2010; the matte-to-flat area ratio
is taken as 1); applied to every surface, planes included (conservative: plated tops are smoother). The
internal inductance of the (smooth) skin-effect surface, R/ω, is added to L. Below the skin-effect range
(δ above about half the copper thickness, `impedance.skin_depth_large` note) the result is approximate: R
never drops below the trace's DC resistance 1/(σ · t · (W − etch)), which takes over at low frequencies (for a
pair: each trace's DC resistance, return through the planes). Zero-thickness copper has no skin-effect model
(`impedance.loss_needs_copper`).

**Dielectric loss.** First-order perturbation with complex permittivities ε(1 − j tan δ):
G = ω Σᵣ tan δᵣ · εᵣ ∂C/∂εᵣ over the dielectric regions (εᵣ ∂C/∂εᵣ is the region's partial capacitance, its
share of the electric energy). Field solver: the derivative of the boundary element C when the permittivities
of all regions with the same tan δ are scaled together (one-sided second-order difference, two extra solves
per distinct loss tangent; masks, split stripline dielectrics and broadside separations keep their own tan δ).
Closed forms: G = ωC tan δ · (εr/εeff) ∂εeff/∂εr from the closed-form εeff (a stripline is fully filled; with
two dielectrics the closed forms use tan δ averaged with weights εr × thickness). D. M. Pozar, *Microwave
Engineering*, gives αd = k₀ εr (εeff − 1) tan δ / (2√εeff (εr − 1)), the same with εeff taken linear in εr;
it is about 1 % higher for FR-4 microstrip.

**Pairs.** The differential attenuation is the power-loss formula α = (iᵀRi + vᵀGv) / (2 vᵀi) for v = (1, −1)
and i = Z⁻¹v, Z the lossless characteristic impedance matrix (exactly the odd mode of a symmetric pair:
(R₁₁ − R₁₂)/(2 Z_odd) + (G₁₁ − G₁₂) Z_odd / 2). The closed forms give only the single line
(`impedance.loss_single_ended` note).

**Accuracy** (tests in `src/electrical/loss.rs`, `src/electrical/fieldsolver.rs`, `tests/electrical.rs`):

| Case | Reference | Agreement |
|---|---|---|
| stripline, b = 3.2 mm, εr 2.2, W = 2.66 mm, t = 10 µm, tan δ 0.001, 10 GHz | Pozar, Example 3.5: αd = 0.155 Np/m, αc = 0.122 Np/m (Pozar's closed form of Wheeler's rule) | αd < 1 %, αc < 3 % (closed forms), < 5 % (field solver) |
| homogeneous dielectric, single line and odd mode | exact: αd = π f √εr tan δ / c | 1e-4 |
| microstrip conductor loss, W/H 0.5, 1, 4 (H 0.5 mm, 35 µm) | R. A. Pucel, D. J. Massé, C. P. Hartwig, "Losses in Microstrip", IEEE MTT-16 (1968), as given by Gupta, Garg, Bahl, Bhartia, *Microstrip Lines and Slotlines* | Wheeler/Hammerstad–Jensen within 8 % (Pucel's formulas are themselves Wheeler's rule on older Z₀ approximations, discontinuous by ~10 % at W/H = 2); field solver within 1.5 % of Wheeler/Hammerstad–Jensen |
| dielectric partial capacitance, microstrip W/H 1, εr 4.4 | (εr/εeff) ∂εeff/∂εr of Hammerstad–Jensen (0.2 % εeff) | < 0.4 % (Pozar's filling factor: < 1.5 %) |
| wide microstrip, W/H = 50 | parallel plate: R = 2 Rs / W_eff, W_eff = h C₀/ε₀ | < 3 % |
| plane currents | Gauss's law: induced plane charge = −Q (with the truncated 1/x² tail added) | < 0.1 % |
| skin depth, copper at 1 GHz | 2.09 µm | 0.1 % |
| field-solver R and G against a 2.5× finer discretization (thick lines, mask, CPWG, etch, split dielectrics, pairs) | | < 1 % (R), < 0.2 % (G) |

Expect the conductor loss of real boards to be dominated by roughness and the copper's actual conductivity;
the smooth-copper numbers are a lower bound. Not modeled: dispersion of εr and tan δ (a constant tan δ is not
causal; use the datasheet values at the frequency of interest), conductor loss of a plane's far side,
radiation, and the frequency dependence of the current distribution between DC and the skin-effect regime.

**`net.loss`** gives the insertion loss of a routed net: its tracks grouped by layer and width (lengths along
arcs, as in `diffpair`/length matching), each group's single-ended attenuation at its layer's stackup geometry
(closed forms, or `method: field_solver`), times its length, summed per frequency. Vias, pads, reflections and
coupling to other nets are not included (the via length is reported separately).

**Speed** (release): closed forms instant; a field-solver single line about 10 ms with losses, a masked pair
with coplanar grounds on 1.5 mm FR-4 about 0.7 s (pair and single line: the vacuum solve for the currents and
two extra solves per distinct loss tangent). The matrices do not depend on frequency: a sweep costs no more than one frequency.

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

## Full-wave export: openEMS (`export.openems`)

The first step of full-wave analysis (S-parameters of vias, transitions, connectors and short interconnects):
a region of the board exported as an [openEMS](https://openems.de) FDTD simulation. openEMS is an external
tool (GPL): cadlab writes its documented input (the CSXCAD XML geometry format and its Python interface, as
used in openEMS's tutorials), never links, bundles or reads its code. Code: `src/openems/` (DECISIONS D54).

```
cadlab export openems --f-max 10GHz --around via#12 \
  --ports '[{"at": "J1.1"}, {"at": "track#40", "end": "end", "kind": "msl"}]'
python3 out/openems/<name>.py              # needs openEMS with its Python modules, and numpy
cadlab touchstone summary out/openems/<name>.s2p --frequencies 1GHz,5GHz
```

**Files** (default `out/openems/<project>.*`):

- `<name>.xml`: an `<openEMS>` document holding the `<ContinuousStructure>`: properties with primitives, and
  the `<RectilinearGrid>` (lengths in mm, `DeltaUnit` 1e-3, written exactly from the integer nanometers);
- `<name>.py`: the driver. For each port in turn it builds an `openEMS` run (`SetGaussExcite`,
  `SetBoundaryCond`), reads the geometry (`ContinuousStructure.ReadFromXML`), adds every port
  (`AddLumpedPort`, `AddMSLPort`; the excited one with amplitude 1, the others as matched terminations),
  runs it in `<name>_sim/portN/`, then computes the port waves (`CalcPort`) and S<sub>ij</sub> =
  b<sub>i</sub> / a<sub>j</sub>, and writes `<name>.sNp` (Touchstone 1.1, RI, one reference impedance;
  2.0 with `[Reference]` when the ports' impedances differ). `--postprocess` recomputes the file from earlier
  runs.

**Region.** `nets` (the pads, tracks and vias of those nets; pours are left out, or a GND net would span the
board), `around` (footprints such as a connector, pads, `via#N` for a via transition, `track#N`, nets) and
`window` (two corners) combine into one rectangle; `nets` and `around` grow by `margin` (default three stack
heights, at least 1 mm). Copper of every net in the rectangle is clipped to it (`only_selected`: only the
selected nets, the ground nets and the ports' nets). The dielectrics follow the board outline inside the
rectangle (air beyond an edge), so edge launches are modeled.

**Geometry.**

| Item | Written as |
|---|---|
| Stackup | z = 0 at the bottom of the bottom copper, up to the top of `F.Cu`. One `Material` per dielectric (`Epsilon` = εr; `Kappa` = 2π f₀ ε₀ εr tan δ, f₀ the excitation's center; an unset tan δ is taken as 0.02, as in the loss calculations, and marked "assumed" in the XML comment: losses are exact at f₀ and scale with frequency only approximately). Inner copper sits inside the dielectric above it (prepreg flows around the traces); outer copper is in air (solder mask is not modeled) |
| Copper | One `Metal` (perfect conductor) per net: the union of its pads, tracks, vias, drawings and zone fills on each layer, clipped to the region. `copper: solid` (default): extruded through the copper thickness (`LinPoly`, or `Box` for rectangles); `sheet`: zero-thickness `Polygon`s with the copper left out of the stack. Polygons with holes (planes around antipads) are cut into hole-free pieces by vertical lines through each hole |
| Vias, plated holes | Metal `Cylinder`s of the drill diameter between the outer surfaces of their first and last layers (slots: the slot outline extruded) |
| Non-plated holes | Air (εr 1) cylinders through the board, above everything else |

**Mesh.** A rectilinear grid per axis: fixed lines at metal edges (the x of vertical and y of horizontal
polygon edges, polygon extremes, hole extremes), the board outline, every copper and dielectric surface in z
and the ports; metal lines closer than `min_cell` (default a twentieth of the largest cell) share one line;
gaps are filled with cells of at most λ/N (`cells_per_wavelength`, default 20) at `f_max` in the highest εr of
the stack (in air above and below: λ₀/N), at least four cells per dielectric, and neighbouring cells differ by
at most the `grading` ratio (default 1.5), refined until the grading holds across fixed lines. `max_cell`
replaces the wavelength rule. The output gives the line counts, the cell count, the smallest cells and the
Courant timestep they imply (`openems.large_mesh` above 50 M cells). `thirds: true` applies the thirds rule at
copper edges (lines a third of a fine cell inside and two thirds outside, see "Full-wave solver" below; the
default of `sim.sparams`, off here so existing exports keep their mesh).

**Excitation and boundaries.** A Gaussian pulse covering `f_min` .. `f_max` (f₀ = (f_min + f_max) / 2, 20 dB
half-bandwidth f_c = (f_max − f_min) / 2); `boundary: pml` (default, `PML_8` on all sides, `pml_cells`) or `mur`;
air `air` above and below (default a quarter wavelength at `f_max`); end criterion `end_criteria` (40 dB energy
decay), at most `timesteps` steps. The Touchstone file has `points` frequencies (default 201) from `f_min`, or
1 % of `f_max` when `f_min` is 0 (port quantities are undefined at DC).

**Ports** (numbered from 1 in the order given; `z0` default 50 Ω):

| `kind` | At | Geometry |
|---|---|---|
| `lumped` (default) | a pad `J1.1` (its single copper layer, or `layer`, default the footprint side's outer layer), or a track end `track#N` + `end` | a resistive sheet of `z0` from the reference copper's surface to the signal copper's, across the line: as wide as the pad's narrower side (spanning that side) or the track (perpendicular to it), or `width` |
| `msl` | a straight, axis-aligned track end | openEMS's microstrip port: `length` along the track (default the track, at most 20 widths), feed at a fifth of it, S-parameters referenced to its middle (`reference_plane` in the output) |

The reference layer is `reference`, else the nearest copper layer (ties: the lower one) with copper of
another net under the port's position; none is `openems.no_reference`. Ports must be in the region
(`openems.port_outside`); one whose feed falls inside the PML cells is warned (`openems.port_in_pml`: grow the
margin).

**Not modeled:** solder mask and silkscreen, surface roughness and finite copper conductivity (copper is a
perfect conductor), frequency-dependent εr / tan δ, components (pads are bare copper: add lumped elements in
the driver if needed). openEMS is not installed on the development machines yet, so the files follow the
documentation and the golden file `tests/golden/openems/via.{xml,py}` (a 4-layer via transition) pins them;
the optional oracle (`CADLAB_ORACLES=1`, `openEMS` on the PATH or `CADLAB_ORACLE_OPENEMS`, the driver run by
`python3` or `CADLAB_ORACLE_PYTHON`) runs the driver on that board and checks |S21| > −3 dB and |S11| < −6 dB
at 1 GHz (`tests/openems.rs`).

## Full-wave solver: FDTD (`sim.sparams`)

The S-parameters of a board region computed in cadlab itself: a 3D finite-difference time-domain (FDTD)
solver, `src/fdtd/` (DECISIONS D59). It takes exactly the region, stack, copper, holes, ports and graded mesh
of `export.openems` (same arguments; the openEMS export stays available for larger or cross-checked runs),
simulates once per port and writes a Touchstone file.

```
cadlab sim sparams --f-max 10GHz --around track#12 --copper sheet \
  --ports '[{"at": "track#12", "kind": "msl", "length": "4mm"}, {"at": "track#12", "end": "end", "kind": "msl", "length": "4mm"}]'
cadlab touchstone summary out/sim/<name>.s2p --frequencies 1GHz,5GHz
```

**Method** (written from the textbooks, no code of openEMS, Meep, gprMax or any other solver used):

- Yee's staggered grid (K. S. Yee, IEEE Trans. AP-14, 1966) on the graded rectilinear mesh, in integral form
  (curls as circulations over primal and dual cell faces; A. Taflove and S. C. Hagness, *Computational
  Electrodynamics*, 3rd ed., ch. 3). Fields in single precision, coefficients from double precision.
- Materials: each E edge takes the area-weighted average of εr and σ of the four cells around it, with the
  semi-implicit conductivity update. Dielectrics are the stackup's, inside the board outline (air outside),
  with σ = 2π f₀ ε₀ εr tan δ at the pulse's center f₀ (tan δ 0.02 when unset), as in the openEMS export: the
  dielectric loss is exact at f₀ and the conductance G stays constant over frequency (a constant tan δ would
  have it grow with f). Non-plated holes are air.
- Copper is a perfect conductor: the E edges lying in the copper volume (`copper: solid`, between the mesh
  lines of both copper surfaces) or on the sheet (`sheet`), x and y edges by their midpoint, z edges by their
  node; plated holes and vias are metal cylinders (plus the node column nearest their center, so a barrel
  thinner than the cells still connects its layers). Copper conductivity and roughness are not modeled.
- Boundaries: a convolutional PML (J. A. Roden and S. D. Gedney, Microwave Opt. Technol. Lett. 27, 2000;
  Taflove ch. 7) in the outer `pml_cells` cells (default 8) of every side, inside the region like openEMS's:
  polynomial grading m = 3, σ_max = 0.8 (m + 1) / (η₀ Δ), κ = 1, a CFS α falling from 2π ε₀ f₀ / 10 to 0
  into the layer; the electric walls behind it close the domain. Copper running into the layer is absorbed
  (a line through the region's edge is a matched continuation).
- Ports: lumped ports are resistive voltage sources of the reference impedance z0 (M. Piket-May, A. Taflove,
  J. Baron, IEEE Trans. MTT-42, 1994; Taflove ch. 15): columns of z edges from the reference copper's surface
  to the signal copper's, across the port's width, each column `z0 · columns` in series with the source, the
  resistance folded into the edges' conductivity. Their voltage (mean of the columns) and current (exact
  element current, V + z0 I = V_s) are sampled at the half step. Microstrip ports feed the same source on the
  line at a fifth of their length and measure at its middle (the reference plane, `reference_plane` in the
  output): voltage from the line's middle to the reference surface, current from the circulation of H around
  the strip on both sides of the plane, interpolated to it. Their feed and reference planes get mesh lines.
- Excitation: a Gaussian pulse covering f_min .. f_max at −20 dB (f₀ = (f_min + f_max)/2, τ = √ln10 / (π f_c),
  starting 3.5 τ before its peak); one run per port, the other ports terminated by their resistances.
- S-parameters: running DFTs of every port's voltage and current at their sampling times give the power waves
  a = (V + z0 I) / 2√z0, b = (V − z0 I) / 2√z0 of each port in each run; S = B A⁻¹ over the runs, so a
  termination that is not matched (a microstrip port's source on a line of another impedance) does not
  bias the result. Written as Touchstone 1.1 (RI, the ports' z0, 2.0 when they differ), 7 significant digits.
- Timestep: 0.95 of the Courant limit of the smallest cells, 1 / (c √(Σ 1/Δ²)); each run stops when the field
  energy has decayed by `end_criteria` (40 dB) below its peak after the pulse, or at `timesteps` (100000,
  `sim.not_converged` warning). Progress is reported every 32 steps (timestep, energy in dB) and the run can
  be cancelled.
- Mesh: the export's, with the thirds rule on by default (`thirds`): at each axis-aligned copper edge, a mesh
  line a third of a fine cell inside the copper and one two thirds outside instead of one on the edge (a
  perfect conductor ending on a mesh line behaves about a third of a cell wider: without the rule a microstrip
  sixteen to four cells wide came out 3–10 % low in Z₀). The fine cell is the smallest of the largest cell, half
  the thinnest dielectric and half the copper's width across the edge; `export.openems --thirds` writes the
  same mesh.
- Determinism and threads: with the `parallel` feature the planes of the grid are split between threads (one
  per 131072 nodes, a pool of the solver's own); every update reads only the other field, reductions are
  summed per plane in order, so results are bit-identical for any thread count.

**Guards.** `max_cells` (default 4 million cells, `sim.too_many_cells` with what to shrink); `timesteps` per
run. The cost is cells × timesteps × ports; a release build updates about 80 million cells per second per
thread (the timestep follows the smallest cell: a 35 µm copper layer in `solid` copper forces small steps;
`copper: sheet` avoids it). Errors: the export's (`openems.*`), `sim.no_ports`, `sim.port_too_thin` (a lumped
port spanning no cell), `sim.port_too_short` (a microstrip port whose feed and reference plane share a mesh
line), `sim.port_at_edge`, `sim.unstable`, `cancelled`.

**Accuracy** (tests in `tests/fdtd.rs`; the `#[ignore]` ones run in release, `cargo test --release --test fdtd
-- --ignored`):

| Case | Reference | Agreement |
|---|---|---|
| zero-thickness microstrip W = 1 mm on 0.5 mm of εr 4, probes on the line, thirds rule, 4 cells across and in the substrate | 2D field solver (D49): Z₀ 50.81 Ω, εeff 3.071 | Z₀ +0.5–0.7 %, εeff +0.1–0.7 % (1–3 GHz) |
| the same, 8 cells across and in the substrate | field solver | Z₀ +0.1–0.4 %, εeff +0.2–0.3 % |
| the same without the thirds rule, 8 / 16 cells across | field solver | Z₀ −5.4 % / −3.4 %, εeff +1.0 % / +0.6 % |
| the same with tan δ 0.02: attenuation at f₀ = 5 GHz | perturbation model (D52), αd = 1.636 Np/m | +0.5 % |
| `sim.sparams` on a routed 24 mm line (the same section, microstrip ports 4 mm), Z₀ and εeff from the S-parameters (ABCD of the 20 mm between the reference planes), 1–3.5 GHz | field solver | Z₀ −0.4 to −0.8 %, εeff +0.3–1.4 % |
| the same: matched line | |S₁₁| < −28 dB from 0.5 to 9.5 GHz |
| the same: attenuation and |S₂₁|, mean over 1–9 GHz | D52: 1.636 Np/m, −0.284 dB | α +6 %, |S₂₁| −0.299 dB |
| 20 × 12 mm PEC cavity, εr 4.4, 0.5 mm cells: TM₁₁₀ and TM₂₁₀ | analytic c/(2√εr) √((m/a)² + (n/b)²) | −0.05 %, −0.08 % (numerical dispersion) |
| the same with σ for tan δ 0.01 at TM₁₁₀: energy decay | Q = 1 / tan δ = 100 | 99.99 |
| lossless graded mesh (ratio 1.4) with a dielectric and a strip, 6000 steps after the pulse | energy conservation | within −0.7 % / +0.05 % (the staggered E/H sampling) |
| uniform cubic grid at 0.99 and 1.03 of the Courant limit | stability | stable / diverges |
| one thread against three | | bit-identical spectra |

**Limits.** Perfect conductors (no skin-effect loss: add D52's conductor attenuation per length for a budget),
frequency-independent εr and the single-frequency σ above, no solder mask, no components (pads are bare
copper; terminate them with ports), staircased curves (arcs and round pads follow the mesh; vias are
cylinders of whole edges), quasi-uniform lines need the mesh lines that the export puts at copper edges
(diagonal tracks staircase). Accuracy depends on the mesh as for any FDTD: keep `cells_per_wavelength` ≥ 20
and several cells across the narrowest copper and dielectric (`max_cell` refines everything; the thirds rule
the edges). Runs are long on large regions: cut the region to the structure (`around` a via or connector,
`margin`), use `sheet` copper, and keep `f_max` to what is needed.

## Touchstone files (`touchstone.summary`)

`src/touchstone.rs` reads Touchstone files, implemented from the IBIS Open Forum's published specifications
(version 1.1 and 2.0): version 1.x option lines (`# GHz S MA R 50`, any order, defaults GHz S MA R 50), data
wrapped over any lines, the 2-port N11 N21 N12 N22 order, 2-port noise data, Z and Y normalized to R; version 2
keywords (`[Number of Ports]`, `[Two-Port Data Order]`, `[Number of Frequencies]`, `[Number of Noise
Frequencies]`, `[Reference]` per port, `[Matrix Format] Full/Lower/Upper`, `[Network Data]`, `[Noise Data]`,
`[Begin/End Information]`, `[End]`; Z and Y in ohms and siemens); S, Y, Z, H, G parameters; RI, MA, DB
formats. Mixed-mode data is refused. Y and Z convert to S with the reference impedances
(S = (z + I)⁻¹(z − I), z = R^−½ Z R^−½). A writer (RI, 1.1 or 2.0) gives round trips.

`touchstone.summary <file>` (no project needed) reports, per S-parameter (`params`, e.g. `S21`, `S1,10` past
nine ports; default all of up to four ports), the magnitude in dB, the magnitude and the phase at the
requested `frequencies` (linear interpolation of real and imaginary parts between points) and the band's
smallest and largest magnitudes with their frequencies: the loop an agent closes after a simulation (worst
return loss = largest |S11|, worst insertion loss = smallest |S21|). The port count of a version 1 file comes
from its extension (`.s2p`), else `ports`. Errors: `touchstone.syntax`, `touchstone.truncated`,
`touchstone.count_mismatch`, `touchstone.no_ports`, `touchstone.bad_param`, `touchstone.out_of_band`, ...

## Design lint (`circuit.lint`, `circuit.erc --lint`)

Heuristics for circuits that pass ERC but are probably wrong. Every finding has a stable code, the nets/pins
involved and a fix hint. Ground nets are nets named like ground (`GND`, `AGND`, `VSS`...) or holding a
ground power pin; supply nets hold a power pin, are `driven`, have a `voltage`, or are named like a rail (`3V3`).

| Code | Severity | Rule |
|---|---|---|
| `lint.missing_decoupling` | warning | an MCU/IC/regulator/oscillator power input (and a regulator's output) on a supply net with no two-pin capacitor from that net to ground, once per part and rail; a regulator output driving an inductor (a buck's switch node) is checked behind the inductor |
| `lint.i2c_pullup` | warning | a net named with an `SDA`/`SCL` token (`I2C1_SDA`, `SCL0`), or holding a non-connector pin so named (`PB7/SDA`), without a resistor to a supply net |
| `lint.usb_esd` | warning | a USB data net (`D+`, `D-`, `DP`, `DM`, `DN`, `USB_D+`... in the net name or a connector pin name) that reaches a connector and has no protection part on it (diode category, or `ESD`/`TVS` in the part ID, description or MPN) |
| `lint.clock_termination` | info | exactly one push-pull output drives a clock net (oscillator part, or `CLK`/`CLOCK` in pin or net name) to inputs of other ICs, with no resistor on the net (no series termination) |
| `lint.floating_input` | warning | an IC `input` pin not connected and not marked no-connect, or input pins on a net whose only other pins are capacitors (no DC level) |

Limits: names are heuristics (a pull-up on another sheet of a connector-only bus, or external ESD, give false
positives); "next to the pin" placement of decoupling capacitors is a board matter (`place.near`), not
checked here. `circuit.erc` includes the lint only with `lint: true`, so ERC results stay stable.
