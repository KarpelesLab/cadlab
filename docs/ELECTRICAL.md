# Electrical (M8)

Calculations and checks that need more than connectivity and geometry: transmission line impedance from the
stackup, track width for a current, a SPICE netlist for simulation, a full-wave (openEMS) model of a board
region with Touchstone results read back, a built-in full-wave (FDTD) solver for the S-parameters of a region,
and a design lint beyond ERC. Code: `src/electrical/` (impedance closed forms, 2D field solver, current, DRC
checks), `src/spice.rs`, `src/openems/`, `src/fdtd/`, `src/touchstone.rs`, `src/lint.rs`; commands in
`src/commands/electrical.rs`, `export.spice`, `export.openems`, `sim.sparams`, `touchstone.summary` and
`circuit.lint`. Decisions: DECISIONS D29, D49 (field solver), D52 (losses), D54 (openEMS export, Touchstone),
D59 (FDTD solver), D63 (full-wave losses), D68 (openEMS export parity, openEMS oracle, CPML), D73 (conformal copper),
D74 (openEMS stability, via barrels, board parts).

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
| dielectric `loss_tangent` | the stackup's (`board.dielectric --loss-tangent`), else 0.02 with an `impedance.loss_tangent_assumed` warning | `loss_tangent` overrides every laminate dielectric; constant over frequency unless a reference frequency is set |
| dielectric `ref_freq` | the stackup's (`board.dielectric --ref-freq 1GHz`), else none | the frequency at which εr and tan δ are given: the dielectric becomes dispersive (Djordjevic–Sarkar, below); `ref_freq` overrides every laminate dielectric, `"0"` makes them constant |
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

**Dispersion (Djordjevic–Sarkar).** With a reference frequency, a dielectric follows the wideband Debye
model of A. R. Djordjevic, R. M. Biljić, V. D. Likar-Smiljanić, T. K. Sarkar, "Wideband Frequency-Domain
Characterization of FR-4 and Time-Domain Causality", IEEE Trans. EMC 43 (2001): a continuum of Debye
relaxations uniform in log frequency between ω₁ and ω₂,
ε(ω) = ε∞ + Δε / ln(ω₂/ω₁) · ln((ω₂ + jω)/(ω₁ + jω)) (`src/electrical/dispersion.rs`). It is causal, its
tan δ is nearly flat between the corners and εr falls by about (2/π) εr tan δ ln 10 per decade (≈ 2.3 % per
decade for FR-4), the Kramers–Kronig partner of a flat loss tangent. ε∞ and Δε follow in closed form from εr
and tan δ at the reference frequency; the corners are fixed at 10 kHz and 1 THz, outside every PCB band
(εr(f)/εr and tan δ(f) then depend only on tan δ and the reference frequency). The line's C and G follow to
first order in the permittivity change, from the partial capacitance P of each group of regions sharing a
loss tangent and reference frequency: C(f) = C + Σ P (εr(f)/εr − 1), G(f) = ω Σ tan δ(f) P εr(f)/εr
(the field solver keeps P per group; the closed forms use P = C · (εr/εeff) ∂εeff/∂εr). A stripline between a
dispersive and a constant dielectric keeps each its own model in the field solver. Without a reference
frequency εr and tan δ stay constant (D52); the full-wave solver needs a causal model and anchors one at its
pulse's center (see "Full-wave solver").

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
| Djordjevic–Sarkar: εr and tan δ at the reference frequency; a midpoint sum of its Debye distribution | exact | 1e-12; 1e-6 |
| dispersive microstrip (εr 4.3, tan δ 0.02 at 1 GHz), 10 MHz to 50 GHz: first-order C(f), G(f) | field solver and closed forms solved again at εr(f), tan δ(f) | < 0.03 % (C), < 0.2 % (G) |

Expect the conductor loss of real boards to be dominated by roughness and the copper's actual conductivity;
the smooth-copper numbers are a lower bound. Not modeled: conductor loss of a plane's far side, radiation,
and the frequency dependence of the current distribution between DC and the skin-effect regime. Without
`ref_freq` the dielectric is constant over frequency (not causal: give the datasheet values at the frequency
of interest, or the reference frequency).

**`net.loss`** gives the insertion loss of a routed net: its tracks grouped by layer and width (lengths along
arcs, as in `diffpair`/length matching), each group's single-ended attenuation at its layer's stackup geometry
(closed forms, or `method: field_solver`), times its length, summed per frequency. Vias, pads, reflections and
coupling to other nets are not included (the via length is reported separately). `mask`, `mask_copper`,
`mask_er`, `mask_loss_tangent` and `etch` mean what they mean in `impedance.calc` (they select the field solver;
the mask applies to outer layers only), so the line whose width was solved with a mask is measured with it.

**Speed** (release): closed forms instant; a field-solver single line about 10 ms with losses, a masked pair
with coplanar grounds on 1.5 mm FR-4 about 0.7 s (pair and single line: the vacuum solve for the currents and
two extra solves per distinct loss tangent). The matrices do not depend on frequency: a sweep costs no more than one frequency.

### DRC: `drc.impedance`

Tracks of a net whose class has an `impedance` target are checked on their layer: a warning per net, layer and
width when the computed Z₀ is more than 10 % from the target (noted when the stackup is assumed). Differential
targets are not checked by the DRC (pair geometry belongs to the router).

## Current and temperature rise (`current.width`, `drc.current_width`)

Required cross-section A for a current I (RMS) and a temperature rise ΔT:

- **IPC-2152 (default)**: curve fit of the standard's *universal* chart (Figure 5-2: the baseline, 3 oz
  conductors in a 0.070" thick polyimide board without planes, in still air, which IPC-2152 recommends for
  internal and external conductors):
  `A [mil²] = (117.555 · ΔT^−0.913 + 1.15) · I^(0.84 · ΔT^−0.108 + 1.159)`.
  Source: <https://smps.us/pcb-calculator.html> (coefficients by Jack Olson; interpolated between 2 and 100 °C,
  the tests behind the chart go up to 30 A). **Approximate**: a fit of a chart; the standard itself was not read
  (it is paywalled). Checked on 2026-11-13 (D79):
  - *Error band.* Only one point of the universal chart was found with published readings, 10 A at 20 °C, read
    twice: 500 mil² (smps.us) and about 520 mil² (Olson, <https://frontdoor.biz/PCBportal/HowTo2152.pdf>). The
    fit gives 513.1 mil² (unit test): **+2.6 % and −1.3 %**, inside the 4 % the two readings differ by. No
    table of chart points is published, so the error elsewhere on the chart is **not known**. As a plausibility
    check only (another chart: 2 oz external), the fit Brooks and Adam digitized from IPC-2152,
    ΔT = 215.3 · I² · W^−1.15 · Th^−1 (mil;
    <https://pcdandf.com/pcdesign/index.php/editorial/menu-features/10122-thermal-management-1506>), gives areas
    that ours are 0.5 % below to 6.6 % above at six points between 2 A / 10 °C and 20 A / 40 °C.
  - *Not the conservative chart.* IPC-2152 keeps the IPC-2221 internal chart as its conservative chart (a
    conductor in free air; M. Jouppi, <https://www.electronics.org/system/files/technical_resource/E7&S22_03.pdf>),
    which reads 680 mil² at 10 A and 20 °C and 300 mil² at 4 A and 10 °C (Olson), where our fit gives 513 and
    192 mil²: a third to a half more copper. For that chart use `--method ipc2221`, whose internal-layer
    formula gives 666.7 and 286.9 mil² at those points (−2.0 %, −4.4 %).
  - *No modifiers.* The universal chart is for the baseline board, and Olson limits it to boards larger than
    3 × 3 in. His worked example multiplies the chart value by 0.84 for 1 oz copper, 1.08 for a 0.062" board,
    1.02 for FR-4 and 0.47 for a 1 oz plane 20 mil away. cadlab applies none: on a 1.6 mm FR-4 board with 1 oz
    copper and no plane the result is about 7 % wider than those factors give, with a plane it is far wider;
    with 3 oz copper on a thinner board without planes it is about 10 % too narrow.
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
used in openEMS's tutorials), never links, bundles or reads its code. Code: `src/openems/` (DECISIONS D54,
D68, D74). The export writes the model `sim.sparams` simulates: Djordjevic–Sarkar dielectrics
as the same Debye terms, lossy copper (as openEMS conducting sheets), solder mask, board parts as lumped elements.

```
cadlab export openems --f-max 10GHz --around via#12 \
  --ports '[{"at": "J1.1"}, {"at": "track#40", "end": "end", "kind": "msl"}]'
python3 out/openems/<name>.py              # needs openEMS with its Python modules, and numpy
cadlab touchstone summary out/openems/<name>.s2p --frequencies 1GHz,5GHz
```

**Files** (default `out/openems/<project>.*`):

- `<name>.xml`: a CSXCAD geometry file, a `<ContinuousStructure>` document (what `ContinuousStructure.
  ReadFromXML` reads and AppCSXCAD opens; before D68 it was wrapped in `<openEMS>`, which `ReadFromXML`
  rejects): properties with primitives, and the `<RectilinearGrid>` (lengths in mm, `DeltaUnit` 1e-3,
  written exactly from the integer nanometers);
- `<name>.py`: the driver. For each port in turn it builds an `openEMS` run (`SetGaussExcite`,
  `SetBoundaryCond`), reads the geometry (`ContinuousStructure.ReadFromXML`), adds every port
  (`AddLumpedPort`, `AddMSLPort` with `Feed_R` = z0: the line ends inside the region, so each port is
  its own matched termination; the excited one with amplitude 1),
  runs it in `<name>_sim/portN/`, then computes the port waves (`CalcPort`) and S<sub>ij</sub> =
  b<sub>i</sub> / a<sub>j</sub>, and writes `<name>.sNp` (Touchstone 1.1, RI, one reference impedance;
  2.0 with `[Reference]` when the ports' impedances differ). `--postprocess` recomputes the file from earlier
  runs. `CADLAB_OPENEMS_THREADS=n` in the environment runs openEMS's engine on n threads (`numThreads`); without it
  openEMS chooses.

**Region.** `nets` (the pads, tracks and vias of those nets; pours are left out, or a GND net would span the
board), `around` (footprints such as a connector, pads, `via#N` for a via transition, `track#N`, nets) and
`window` (two corners) combine into one rectangle; `nets` and `around` grow by `margin` (default three stack
heights, at least 1 mm). Copper of every net in the rectangle is clipped to it (`only_selected`: only the
selected nets, the ground nets and the ports' nets). The dielectrics follow the board outline inside the
rectangle (air beyond an edge), so edge launches are modeled.

**Geometry.**

| Item | Written as |
|---|---|
| Stackup | z = 0 at the bottom of the bottom copper, up to the top of `F.Cu`. A lossy dielectric is a `DebyeMaterial`: the Djordjevic–Sarkar model through its εr and tan δ (at `ref_freq`, or anchored at the pulse's center f₀ when the stackup gives none; an unset tan δ is taken as 0.02, as in the loss calculations, and marked "assumed"), fitted over the Touchstone band exactly as `sim.sparams` fits it (`fdtd::fit`: ε∞ as `Epsilon`, a conductivity as `Kappa`, and Δε on the shared relaxation times as `EpsilonDelta_n` / `EpsilonRelaxTime_n`, CSXCAD's documented parameter names; terms of zero weight left out, 7 significant digits); a comment gives εr and tan δ at f₀. A lossless one is a plain `Material` (`Epsilon` = εr). Inner copper sits inside the dielectric above it (prepreg flows around the traces); outer copper is in air, or under the solder mask |
| Solder mask (`mask`) | A `solder_mask` material (`mask_er`, default 3.8, `mask_loss_tangent`, default 0.025: Debye terms like an unreferenced laminate) on both outer layers: the board outline from the laminate surface to `mask` beyond it (priority 5) and each outer layer's copper from its outer surface to `mask_copper` (default `mask`) beyond it (priority 7); a `mask_openings` air material (εr 1) clears what lies above the copper outside its own mask (priority 6) and the pads with a mask opening on that side (their copper, priority 8). Metal (10) stays above all of it. Mesh lines at both mask surfaces |
| Copper | One `Metal` (perfect conductor) per net: the union of its pads, tracks, vias, drawings and zone fills on each layer, clipped to the region. `copper: solid` (default): extruded through the copper thickness (`LinPoly`, or `Box` for rectangles); `sheet`: zero-thickness `Polygon`s with the copper left out of the stack. Polygons with holes (planes around antipads) are cut into hole-free pieces by vertical lines through each hole. With a `conductivity` (default 58 MS/m, as `sim.sparams`; `perfect_copper` keeps perfect metal only) each net's copper on each layer is instead a `ConductingSheet` (`Conductivity`, `Thickness` = the layer's copper thickness: openEMS's documented `AddConductingSheet` model of a thin lossy metal) at priority 11: on both surfaces of solid copper, or as the sheet itself; the `Metal` keeps only the via barrels. (With a perfect conductor left between the two sheets of solid copper, openEMS read 60 % of the conductor loss of `sim.sparams`; without it the two agree: 0.017 against 0.018 dB at 1 GHz, 0.031 against 0.030 dB at 3 GHz on the 20 mm line below with lumped ports.) Roughness (`roughness`, `huray_radius` / `huray_ratio`) is folded into the conductivity at f₀ (σ / K², since Rs K = √(πfμ₀ / (σ/K²))): openEMS's sheet has no roughness model |
| Vias, plated holes | Metal `Cylinder`s of the drill diameter between the outer surfaces of their first and last layers (slots: the slot outline extruded), plus each round barrel's nearest node column as a line `Box` on mesh coordinates: a barrel between mesh lines holds no mesh edge and would conduct nothing (D74) |
| Board parts (`parts`, default true) | Two-pin resistors, capacitors and inductors, by the selection rules of `sim.sparams` (D65; `sim.part_not_modeled`, `sim.part_not_populated`): a `LumpedElement` (`Direction` along the axis joining the pads, `Caps`) in a box over the gap between the pads' copper, across where they face each other, through the copper's thickness, priority 9. A resistance alone is a plain element (`R`); with a reactance, a series one (`LEtype="1"`, `R`, `L`, `C`); 0 Ω is a `Metal` box. The output lists the parts written |
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
half-bandwidth f_c = (f_max − f_min) / 2); `boundary: pml` (default, `PML_8` on all sides, `pml_cells`) or `mur`.
A `PML_n` is the outermost n *cells*, however fine, so with `pml_outside` (default true) the export adds those
cells beyond the region's x and y sides (growing by `grading`, at most the mesh's largest cell), and copper and
dielectrics that reach a side run through them to the wall. The region keeps all its own cells, what ends inside
it stays out of the layer, and `window` still reports the region (the mesh is larger: `mesh.lines`). z is air
already. `boundary: mur` gets eight air cells the same way. `pml_outside: false` puts the layer in the region's
outer cells as `sim.sparams` does by default (where its stretched CPML is stable), and warns
`openems.edge_near_boundary` when the board's edge is in that band: both boundaries can diverge there, see "What
makes openEMS diverge" below. The driver stops with `openems.diverged` instead of writing non-finite or
|S| > 10 results, since a run that went to NaN otherwise ends as if converged. `boundary: mur` with `f_min` 0
warns `openems.mur_static_field`: the pulse's DC part stays as a static field the wall does not absorb. Air `air`
above and below (default a quarter wavelength at `f_max`); end criterion `end_criteria` (40 dB energy
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

openEMS's microstrip port measures a zero-thickness strip: with `copper: solid` its probes read nonsense
(εeff 3.6–6.8 for a 3.0 line, Z₀ with a large imaginary part, seen with the oracle), so an `msl` port on solid
copper is warned (`openems.msl_solid_copper`: export with `copper: sheet`, or use lumped ports).
`sim.sparams` measures both.

**Not modeled in the export:** silkscreen, the mask's own margin around pads (openings are the pad copper),
roughness beyond f₀ (folded into the sheets' conductivity there), conductor loss in via barrels (perfect
cylinders), components other than two-pin R, L, C (their pads are bare copper). The golden file
`tests/golden/openems/via.{xml,py}` (a 4-layer via transition) pins the format. The openEMS oracle
(`CADLAB_ORACLES=1`, `openEMS` on the PATH or `CADLAB_ORACLE_OPENEMS`, the driver run by `python3` or
`CADLAB_ORACLE_PYTHON`; CI job "openEMS oracle", openEMS built from source at a pinned commit) runs the
driver on a lossy microstrip
(`lossy_microstrip_oracle`: 24 mm of 1 mm strip on 0.5 mm of Djordjevic–Sarkar εr 4 / tan δ 0.02 at 1 GHz,
58 MS/m copper, 6 GHz, thirds rule) against `sim.sparams` on the same options, twice: sheet copper between
microstrip ports 4 mm long, and solid copper between lumped ports at the track's ends:

| Quantity (0.5–6 GHz) | openEMS | `sim.sparams` (ems) | Reference |
|---|---|---|---|
| sheet / msl: Z₀ from the ABCD matrix (to 3.6 GHz) | 50.3–52.8 Ω | 50.6–51.0 Ω | 2D field solver 50.8 Ω |
| sheet / msl: \|S21\| at 1.05 / 3.03 / 6 GHz | −0.097 / −0.220 / −0.422 dB | −0.083 / −0.206 / −0.378 dB | |
| sheet / msl: the line's loss without its mismatch at 0.56 / 1.05 / 3.03 / 6 GHz | 0.058 / 0.097 / 0.215 / 0.402 dB | 0.042 / 0.082 / 0.205 / 0.374 dB | D52 on 20 mm: 0.043 / 0.075 / 0.197 / 0.375 dB |
| solid / lumped: Z₀ (to 2.6 GHz) | 50.6–52.7 Ω | 49.6–52.8 Ω | |
| solid / lumped: \|S21\| at 1.05 / 3.03 / 6 GHz | −0.116 / −0.270 / −0.559 dB | −0.092 / −0.264 / −0.518 dB | |
| solid / lumped: the line's loss without its mismatch at 0.56 / 1.05 / 3.03 / 6 GHz | 0.068 / 0.116 / 0.262 / 0.536 dB | 0.049 / 0.092 / 0.252 / 0.490 dB | D52 on 24 mm: 0.051 / 0.090 / 0.236 / 0.450 dB |
| openEMS \|S11\| | ≤ 0.073 | | |

(The loss without the mismatch is −10 log₁₀(\|S21\|² / (1 − \|S11\|²)); D52 is the line's own model, the 2D
field solver's matrices of the 35 µm strip with the Djordjevic–Sarkar laminate and the skin effect, no FDTD in
it. The lumped ports at the ends of the solid line are not in that model, so its row is the weaker reference.)
The test asserts \|S21\| within 15 % + 0.025 dB, Z₀ within 5 % of `sim.sparams` (and of the 2D solver with the
microstrip ports), \|S11\| < 0.1 (`tests/openems.rs`; the tables go to `target/tmp/openems_oracle/summary.md`
and the CI job's summary). The two solvers are furthest apart at the lowest frequencies, 36.0 % (sheets) and
40.2 % (`solid`) at 0.56 GHz, inside the test's absolute term: there openEMS reads a third more loss than D52
(0.058 for 0.043 dB, 0.068 for 0.051 dB) and ems is within 5 % of it (D103). From 0.5 GHz ems is within 0.019 dB
of D52 on the sheet line (4.3 % RMS, +2.6 % in the mean) and openEMS within 0.027 dB (15.8 % RMS, +13.1 %); on
the solid line 0.040 dB (5.9 %, +4.6 %) against 0.086 dB (17.0 %, +15.1 %). At 0.06 GHz, 1 % of `f_max` and
below what the test holds, it is the other way round: ems is 0.008 and 0.009 dB from D52 (it reads a gain of
0.001 dB on the sheet line) and openEMS 0.005 and 0.004 dB. That is the end criterion: ems reaches −50 dB after
6145 steps where openEMS runs 8820, and to −70 dB ems reads 0.0063 dB for D52's 0.0072.

The golden via transition (`via_transition_export`, the region of the 4-layer board at 4 GHz, lumped ports) and
board parts (`lumped_parts_oracle`, a 0603 on a 1 mm microstrip between 3 mm microstrip ports, thirds rule, sheet
copper) run in the same job, at the pinned revision:

| Via transition | 0.54 GHz | 1.03 GHz | 2.02 GHz | 3.01 GHz | 4.00 GHz |
|---|---|---|---|---|---|
| \|S21\| openEMS / `sim.sparams` (dB) | −0.035 / −0.045 | −0.063 / −0.083 | −0.145 / −0.134 | −0.183 / −0.185 | −0.457 / −0.390 |
| \|S11\| openEMS / `sim.sparams` (dB) | −31.2 / −30.0 | −26.6 / −25.7 | −28.8 / −26.6 | −26.9 / −29.2 | −15.3 / −15.0 |

| \|S21\| openEMS / `sim.sparams` / ideal | 0.56 GHz | 1.05 GHz | 2.04 GHz | 3.03 GHz | 4.02 GHz |
|---|---|---|---|---|---|
| series 50 Ω | 0.666 / 0.667 / 0.667 | 0.666 / 0.667 / 0.667 | 0.665 / 0.668 / 0.667 | 0.664 / 0.670 / 0.667 | 0.665 / 0.672 / 0.667 |
| series 1 pF | 0.331 / 0.336 / 0.329 | 0.551 / 0.554 / 0.551 | 0.788 / 0.798 / 0.788 | 0.889 / 0.894 / 0.885 | 0.939 / 0.938 / 0.930 |

The test asserts \|S21\| within 0.25 dB from 0.5 GHz for the via (it measures 0.07), \|S21\| > −3 dB and
\|S11\| < −6 dB at 1 GHz, and for the parts within 0.03 of `sim.sparams` and of the ideal from 0.5 to 4.5 GHz
(it measures 0.009 and 0.009). `lossy_microstrip_oracle` keeps `pml_outside: false` in openEMS and in
`sim.sparams`, so its numbers above stay comparable. Every `sim.sparams` figure of this section is ems's (0.0.12
to 0.0.14, whose Touchstone data are the same on these cases, D103); until D100 the columns were read on
cadlab's own engine (D68, D74). Two things the tables do not say by themselves (D103):

- *The via transition's two solvers run on two meshes.* The case names no `thirds`, which `sim.sparams` has on by
  default and the export off: 130 626 cells for ems, and for openEMS the 64 × 53 × 55 lines that the export
  without the rule gives. With the rule stated for both (`via_same` in the comparison below) they are within
  0.047 dB of each other from 0.5 GHz. There is no reference for a via; against each solver on a finer mesh
  (`max_cell` 0.4 mm, 874 000 cells) ems on the default mesh is the nearer one both times (0.088 dB from its own
  finer mesh, where openEMS is 0.131 dB from it; 0.021 dB from openEMS's finer mesh, where openEMS itself is
  0.064 dB), and the two finer meshes are 0.067 dB apart. Not a convergence study: one refinement.
- *The ideal is not the part on its board.* S21 = 2 Z₀ / (2 Z₀ + Z) takes the lines for 50 Ω; the 1 mm strip is
  the field solver's 50.8 Ω, and a 50 Ω part between 5.15 mm of it on each side reads up to 0.004 above the ideal
  at 4.5 GHz, as much as the solvers differ by. Against the ideal openEMS is the nearer one (0.0029 for the
  resistor where ems is 0.0048, 0.0089 for the capacitor where ems is 0.0099); against the part between its
  lines (the cascade line, part, line in 50 Ω, with the field solver's Z₀ and εeff) ems is within 0.0015 for the
  resistor, where openEMS is 0.0054, and 0.0064 for the capacitor, where openEMS is 0.0058. On a finer mesh
  (0.25 mm) openEMS moves towards the ideal (0.0015) and ems away from it (0.0071), each staying within 0.0025 of
  its own coarse run; which of the two a real 0603 would agree with is not established here: the pads' gap and
  the element's own extent are in neither reference.

**ems beside openEMS** (D103). After the oracle tests the job runs both solvers on the same cases, one process
per run and one at a time, on one thread and on the runner's four (`solver_comparison`; `sim.sparams` with
`threads`, the driver with `CADLAB_OPENEMS_THREADS`), and keeps the table (`openems_comparison/comparison.md` in
the job's artifact, and its summary): the whole run's time and the time stepping's share, steps, time step,
cells, the process's peak memory and what it was before the run, and each solver's distance from the references
above. With ems 0.0.14 (run 44030782504, an Intel Xeon 6975P-C runner, four logical CPUs):

| Case | Cells | Steps per port, ems / openEMS | Whole run on one thread, ems / openEMS | Each at its fastest (threads) | Peak memory | Of it the run |
|---|---|---|---|---|---|---|
| lossy line, sheets, microstrip ports | 67 200 | 6145 / 8820 | 13.9 / 32.0 s | 6.8 (4) / 32.0 (1) s | 45 / 75 MB | 22 / 24 MB |
| lossy line, `solid`, lumped ports | 85 320 | 12 065 / 22 221 | 29.6 / 96.2 s | 13.7 (4) / 96.2 (1) s | 52 / 78 MB | 29 / 27 MB |
| via transition (two meshes, above) | 130 626 | 32 961 / 27 216 | 118.1 / 252.0 s | 67.0 (4) / 231.2 (4) s | 65 / 109 MB | 42 / 58 MB |
| via transition, `thirds` for both | 130 626 | 32 961 / 28 518 | 118.3 / 180.9 s | 67.1 (4) / 177.8 (4) s | 66 / 92 MB | 43 / 41 MB |
| series 50 Ω | 69 312 | 7105 / 8037 | 6.8 / 17.1 s | 3.4 (4) / 12.9 (4) s | 35 / 72 MB | 12 / 20 MB |
| series 1 pF | 69 312 | 7105 / 8460 | 6.9 / 17.9 s | 3.4 (4) / 13.5 (4) s | 35 / 71 MB | 12 / 20 MB |

ems is 1.5 to 3.2 times as fast on one thread and 2.7 to 7.0 times at each solver's fastest (on the five cases
measured with ems 0.0.12 on the same CPU, run 44006837399: 1.8 to 2.9 and 2.2 to 5.7 times then, 2.1 to 3.2 and
3.5 to 7.0 now; on other runners' CPUs the ratios are others, 1.4 to 2.4 times with 0.0.12 on an AMD EPYC
9V45). openEMS's whole run is its engine but for a second or two; the 51 MB it has before a run are
Python with numpy and its modules, ems's 23 MB the test binary with the board. Where openEMS is ahead or level:
the memory of the run itself on the `solid` line and on the via on one mesh (a Debye pole is 24 bytes per pole
and edge in ems, `f64`); the lowest frequency of a run to −50 dB; the parts against ideal lines; and, with
0.0.14, its time step, 2 to 16 % longer than ems's on the lines and the parts (ems took the Courant limit of
the smallest cells in vacuum times `courant`, and took fewer steps all the same but on the via). ems 0.0.15
takes the step from a bound over the cells (D105; "Guards" below): 0.2544 ps for openEMS's 0.1982 on the sheet
line, 0.1792 ps for its 0.1966 on the parts (above what a single cell proves there, 0.1886 at factor 1), the
via's unchanged. With 0.0.15 on an AMD EPYC 7763 runner (run 44058840773): 2.0 to 3.8 times openEMS's speed
on one thread and 3.2 to 6.6 times at each solver's fastest, the memory and the accuracy rows as above but for
the `solid` line's run (27 for 27 MB) and the lines' loss (0.0187 and 0.0403 dB from D52); the table is in D105.
With 0.0.17 on the same CPU model (run 44114553606; D107 has the table): the shares of the cells balanced and
the cut cells in the bound give 0.2742, 0.1338, 0.2356 and 0.0664 ps for openEMS's 0.1982, 0.1011, 0.1966 and
0.0482 or 0.0460, fewer steps than openEMS on every case, 2.4 to 4.8 times its speed on one thread and 3.8 to
8.4 times at each solver's fastest, and the run's own memory below its on every case (19 to 37 MB for 20 to
58). Where openEMS is still ahead: the 0.06 GHz point of a run to −50 dB, the parts against ideal lines, the
capacitor against the lines as they are by 0.0006, which are where ems's run ends (to −80 dB the capacitor
reads 0.0029 for openEMS's 0.0058 and the 0.06 GHz points 0.0007 dB for 0.005: D107). With 0.0.18 (run
44174491916, on an AMD EPYC 9V45, which is not D107's CPU; D111 has the table) a run goes on past its energy
criterion while the ports' signals still add to the lowest frequency, and the layers' convolution is second
order in the step: the 0.06 GHz points read 0.0008 and 0.0011 dB for openEMS's 0.0053 and 0.0035, the
capacitor 0.0029 for 0.0058, the lines' loss from 0.5 GHz 0.0167 and 0.0397 dB for 0.0268 and 0.0862; the
lines take twice the steps they took (8801 for openEMS's 8820, 17 953 for 22 221) and the via 6 % more
(28 577 for 27 216 and 28 518), and ems is 1.3 to 3.3 times as fast on one thread and 2.7 to 5.9 times at
each solver's fastest on that CPU. Where openEMS is still ahead: the parts against ideal 50 Ω lines (0.0029
for 0.0048, 0.0089 for 0.0093; against the lines as they are ems is the nearer), the `solid` line's Z₀ by
0.1 % at a frequency where the ports decide it, and the via's steps. One more difference is not in a
column: openEMS looks at its end criterion when it prints a progress line, every few seconds of wall time, so
its number of steps and with it its result follow the machine (the `solid` line ran 22 221 steps per port on
four runs and 22 221, 27 265, 22 960 and 19 475 on a slower runner, reading 41.6 % for the 40.2 % above; the
via on one mesh 0.047 or 0.112 dB from ems). ems looks every 32 steps, and its Touchstone data are the same
lines on one thread and on four, on the runners' three CPUs and on the development machine.


**What makes openEMS diverge** (D74, black-box runs of openEMS's CLI, energy against time). Run length matters: a
Mur case bounded at 40 000 steps went to NaN by 113 000, so the steps are given. On the 4-layer via board, whose
outline lay one 0.30 mm cell inside the region's y sides:

| Case | Steps | Result |
|---|---|---|
| PML, the board's edge (two planes with dielectric between them, ending together) one cell from the wall on y | 100 000 | +5 dB per 1000 steps, then NaN |
| the same edge 2, 3, 5, 8 cells from the wall on y; 1 or 3 cells on x (a 1.18 mm outer cell) | 250 000 | decays |
| the same reaching the wall | 300 000 | decays |
| the same one cell from the wall, with one plane, without the dielectric, with εr 1, or without the planes | 80–105 000 | decays |
| PML on x only or on z only, PEC elsewhere; PEC on all faces | about 40 000 | no growth seen |
| half the timestep | 79 000 | same growth per unit time, so not a Courant limit |
| a constant σ added; tracks, vias or copper thickness removed | 35–97 000 | still grows, so not D68's materials or those features |
| pulse with DC at −20, −55 or −163 dB | 60–80 000 | same rate, so not the excitation's DC |
| Mur, a dielectric's edge 1, 2 or 3 cells from the wall on y, with planes (1 cell also without) | 40–200 000 | NaN, or growth at 3 cells |
| Mur, a dielectric's edge on the wall or 4, 6, 8 cells from it on y; 1 or 2 cells on x | 200–250 000 | bounded |
| the export as it is now, PML, this board with its materials | 219 000 | decays (−68 dB, still falling); both ports reach the 40 dB criterion by 28 000 |
| the export as it is now, on a window cut from a 40 × 30 mm board (PML), and with Mur on this board | 44–60 000 | decays |
| `pml_outside: false` on this board, PML / Mur | 89 000 / 33 000 | +4.9 dB per 1000 steps / NaN |

The growing mode's frequency is above the band (the spectrum peaks at 7–16 GHz for a 4 GHz run, not resolved
further). Why this one position is unstable is **not established**: the runs show what triggers it, and openEMS's
internals are not read. The rule the export follows is empirical: keep what ends inside the region out of the
absorbing cells, and let what reaches a side reach the wall. A small box with PEC walls on x and z instead of a
PML diverged with a vacuum-only PML too (not explained, and not a configuration the export writes).

## Full-wave solver: FDTD (`sim.sparams`)

The S-parameters of a board region computed in cadlab's own process: a 3D finite-difference time-domain (FDTD)
run, `src/fdtd/`. It takes exactly the region, stack, copper, holes, ports and graded mesh of `export.openems`
(same arguments; the openEMS export stays available for larger or cross-checked runs), simulates once per port
and writes a Touchstone file. The solver is the [`ems`](https://crates.io/crates/ems) crate's: a clean-room,
pure-Rust FDTD solver (MIT, with its geometry crate `csx`; no further dependencies), used through its Rust API
from crates.io and a dependency of every build (DECISIONS D86, D94, D100). cadlab builds the model, places the
ports and reads the waves; ems does the time stepping. There is no solver to choose: until D100 cadlab also had
an FDTD engine of its own (D59) behind `solver: builtin`, and the parameter went with it. The result names what
ran, `solver: "ems 0.0.18"` in the output and `(solver: ems 0.0.18)` in the Touchstone file's first line.

```
cadlab sim sparams --f-max 10GHz --around track#12 --copper sheet \
  --ports '[{"at": "track#12", "kind": "msl", "length": "4mm"}, {"at": "track#12", "end": "end", "kind": "msl", "length": "4mm"}]'
cadlab touchstone summary out/sim/<name>.s2p --frequencies 1GHz,5GHz
```

**The model** (code: `fdtd::extract`, `fdtd::place_ports`, `fdtd::ems`, `fdtd::Waves`; no code of openEMS,
CSXCAD, Meep, gprMax or any other solver was read). The geometry ems gets is the property list of
`export.openems` (`openems::xml::properties`: the export's file is that list written out), with one difference:
lossy `solid` copper stays metal volumes, where the export writes two conducting sheets per layer.

- Mesh: the export's graded rectilinear mesh (below), handed over in millimetres, every coordinate through one
  conversion so a primitive's corner on a mesh line is that line. ems's fields are single precision.
- Dielectrics are the stackup's, inside the board outline (air outside); non-plated holes are air. Each is
  causal and dispersive: the Djordjevic–Sarkar model (see "Losses") through its εr and tan δ at the stackup's
  `ref_freq`, or, for a dielectric without one, at the pulse's center f₀ (tan δ 0.02 when unset), so a
  constant-tan-δ laminate is exact at f₀ and causal around it. cadlab fits it over the band of the result
  (`f_min`, or 1 % of `f_max`, to `f_max`) as ε∞ + σ/(jωε₀) + Σₚ Δεₚ/(1 + jωτₚ) on relaxation times shared by
  every material (three per two decades from a third of the band's bottom to three times its top, at least
  six), the weights by non-negative least squares on ε′ and ε″ in relative terms (C. L. Lawson, R. J. Hanson,
  *Solving Least Squares Problems*, 1974) with ε∞ ≥ 1 (`fdtd::fit`): non-negative Debye terms are passive, so
  the update stays stable (εr within 0.1 %, ε″ within 0.6 % of the model over the band). ems gets a
  `DebyeMaterial` with ε∞, the conductivity and the poles. An edge in the laminate's surface gets the poles by
  the share of its dual face the laminate fills, as it gets the static ε (`PoleWeighting::Averaged`; ems's
  default is openEMS's rule, the whole poles for such an edge, which read two points more loss on a 60 mm
  microstrip: D89, D90). Lossless dielectrics stay plain materials.
- Perfect copper (`perfect_copper`): `Metal`, as volumes between the mesh lines of both copper surfaces
  (`copper: solid`) or as sheets on the mesh plane (`sheet`). Plated holes and vias are metal cylinders plus the
  node column nearest their center, so a barrel thinner than the cells still connects its layers (the column
  is a wire: no loss of its own).
- Lossy copper (`conductivity`, default 58 MS/m): `solid` copper is the net's metal volumes named a conductor
  (`CompileOptions::with_lossy_metal`), a surface impedance on every face of the copper, sidewalls included (a
  sidewall 35 µm high and the corners hold about 40 % of a 1 mm microstrip's conductor loss in the field
  solver); `sheet` copper is a `ConductingSheet` per net and layer (the conductivity, the layer's copper
  thickness) integrated as a rational two-port of its two faces (`SheetModel::SurfaceImpedance`). Both are
  rational fits of the skin effect over the frequencies of the result. `roughness`, or `huray_radius` with
  `huray_ratio`, is Hammerstad–Jensen's or Huray's factor at every frequency of that fit
  (`Conductor::with_roughness`, `with_sheet_roughness`), not folded into the conductivity at f₀ as the export
  does.
- Diagonal and curved copper (`conformal`, default true; D73, D87, D89): cells are cut where copper is oblique
  to the mesh (diagonal tracks, round pads, barrels), for sheets on mesh planes and for solid copper
  (`ConformalOutlines::Slanted`); copper along the mesh keeps the staircase its mesh lines were placed for,
  with the thirds rule. What a small cut face does, and whether a slanted track or a barrel about a cell wide
  keeps whole edges, is ems's to decide (`ConformalOptions::keep_stable`, `CompileOptions::conformal_bridges`:
  it keeps a small face's own current where the cells around it are stable at the timestep, keeps whole the
  edges that carry a track about a cell wide, the edge that continues a lumped element's column into its pad
  and a sheet's edges under a barrel, and integrates the open area of the faces a barrel, a pad or a corner
  crosses). By construction, the covered part of an edge that a slanted outline of *solid* lossy copper cuts is
  a perfect conductor, and the whole edges beside it carry its share of the surface. `conformal_edges` in the
  output counts the cut edges; `conformal: false` makes every edge all copper or none.
- Solder mask (`mask`, `mask_copper`, `mask_er`, `mask_loss_tangent`, as in the export): a dielectric layer
  on both outer layers, over the laminate and over the copper, open at the pads; Djordjevic–Sarkar at f₀ like
  an unreferenced laminate; its surfaces are mesh lines (a 25 µm mask makes 25 µm cells, so a smaller
  timestep).
- Boundaries: a convolutional PML in the outer `pml_cells` cells (default 8) of every side, inside the region
  like openEMS's (or beyond it, `pml_outside`), with a complex frequency shift α of up to 2π ε₀ f₀
  (0.28 S/m at 5 GHz). The shift is what absorbs the evanescent near field of a line close to the layer: with
  a tenth of that value a microstrip 4 h from the side layer drew energy from it (a lossless line read up to
  −0.06 Np/m around 1.8 GHz; D68). Copper running into the layer is absorbed (a line through the region's edge
  is a matched continuation).
- Ports: a lumped port is ems's `LumpedPort`, a resistor of the reference impedance z0 with a Thevenin source,
  on columns of z edges from the reference copper's surface to the signal copper's across the port's width;
  its voltage is the mean over the columns (`average_voltage`) and its current the element's. A microstrip
  port feeds the same source on the line at a fifth of its length and measures at its middle (the reference
  plane, `reference_plane` in the output): a voltage probe from the line's middle to the reference surface
  and two current probes, the circulation of H around the strip on the dual planes on both sides of the plane,
  interpolated to it (not ems's three-probe `MslPort`, which needs a zero-thickness strip). The feed and
  reference planes get mesh lines.
- Board parts (D65; `parts`, default true): every two-pin resistor, capacitor and inductor with both pads in
  the region is a lumped series R–L–C element from its values: a resistor's `resistance`, a capacitor's
  `capacitance`, an inductor's `inductance`, plus `esr` and `esl` when given (a component property of the
  same name wins over the part's parameter; a 0 Ω resistor is a short, metal). The element is a box on the
  pads' copper layer along x or y (the axis joining the pad centers), between the two pads' facing edges, over
  the width where the pads face each other and the copper's thickness (`fdtd::parts`; the export writes the
  same box): a `LumpedElement`, a resistance alone or a series element with its reactance. `columns` in the
  output counts the columns of mesh edges the box takes in, the same with or without conformal cells. Parts
  left out, with a warning (`sim.part_not_modeled`): no value, more or fewer than two pads, a pad outside the
  region, pads on two layers, pads not facing along x or y, or no mesh cell between the pads' copper (their
  pads stay open); DNP parts are left open with a note. `parts` in the output lists the parts modeled with
  their values.
- Excitation: a Gaussian pulse covering f_min .. f_max at −20 dB (f₀ = (f_min + f_max)/2); one run per port,
  the other ports terminated by their resistances. The model is compiled once (`ems::CompiledModel`; 0.45 s
  for 67 000 cells) and every port's run is a new engine from it driving that port's source; a run gives its
  simulation up for its results, so the probes' series are held once (D98).
- S-parameters: the spectra of every port's voltage and current give the power waves a = (V + z0 I) / 2√z0,
  b = (V − z0 I) / 2√z0 of each port in each run; S = B A⁻¹ over the runs, so a termination that is not
  matched (a microstrip port's source on a line of another impedance) does not bias the result. Written as
  Touchstone 1.1 (RI, the ports' z0, 2.0 when they differ), 7 significant digits.
- Timestep: 0.95 of ems's limit for the mesh and its cut cells (`plan` reports it); each run stops when the
  energy of the fields has decayed by `end_criteria` (60 dB) below its peak, checked every 32 steps, and the
  ports' signals have no more to add at the lowest frequency of the result (`settle`, below), or at
  `timesteps` (100000, `sim.not_converged` warning). 40 dB (the default before D63) leaves a tail of about 1 %
  in amplitude, whose truncation biased |S| by a few hundredths of a dB below f_max / 10 (|S₂₁| above 0 dB on
  short lossy lines); 60 dB costs about a quarter more steps. Progress is reported with the timestep and the
  energy in dB, and the run can be cancelled.
- `settle` (default true; ems's `Settle`, D111): the energy of the whole field says little about the lowest
  frequency, whose spectrum lacks what the signals do after the run's end, and a slow tail of little energy
  is large there (a lossy 20 mm line run to −50 dB: 0.008 dB from its reference at 0.06 GHz, 0.002 dB at
  −60 dB, where the energy has a knee and 10 dB more cost 1.7 times the steps). Once the energy criterion is
  met after N₀ steps the run goes on while, for a voltage or current probe of the ports, twice the change of
  its running spectrum at the result's lowest frequency over the last eighth of the run, divided by the fall
  of the energy's logarithm over the same window, is more than 10⁻⁴ of the sum of the signal's absolute
  values: that is what the spectrum still lacks if the signal goes on as the window has it and decays as the
  fields do. A part's region, whose energy falls 30 dB in a third of a run, ends at once (a series resistor:
  1.01 times the steps; a capacitor 1.35 times, at −81 dB); a line past its knee goes on until the test is
  met, at 2.33 times the steps of a run to −50 dB (0.0001 and 0.0002 dB at 0.06 GHz), or until the cap of as
  many steps again (2 N₀): asked for 50 dB the lines end there, 0.0008 and 0.0012 dB from D52 at 0.06 GHz,
  and at the default 60 dB the test ends them, 1.2 to 1.35 times the steps of the energy criterion alone.
  `settle: false` is the energy criterion alone, as until ems 0.0.17. The tolerance is of the
  signal's size, not of its spectrum at that frequency, so a frequency the pulse hardly excites is held less
  tightly: a lossless bent line's Σ|S|² at its lowest frequency reads 1.0005 where the runs to −70 dB alone
  read 1.0001 to 1.0003.
- Absorbing layers: CPML of `pml_cells` cells on every face, its frequency shift α at f₀, and from ems 0.0.18
  its convolution second order in the time step (D111). The first-order recursion of the CPML's authors is,
  in a layer whose cells set the step, a layer of the conductivity σ/(1 − σ Δt/(2ε₀)), larger with a longer
  step; the near field of a line that runs beside a layer felt it, and the loss a lossless 60 mm line read
  at 9 GHz was 0.027 dB + 0.082 dB/ps·Δt (0.038 dB at ems 0.0.17's step). It reads 0.0278 dB at any step now,
  and the 64 mm lossy line's worst point is 0.082 dB from D52 where it was 0.090 to 0.092.
- Mesh: the export's, with the thirds rule on by default (`thirds`): at each axis-aligned copper edge, a mesh
  line a third of a fine cell inside the copper and one two thirds outside instead of one on the edge (a
  perfect conductor ending on a mesh line behaves about a third of a cell wider: without the rule a microstrip
  sixteen to four cells wide came out 3–10 % low in Z₀). The fine cell is the smallest of the largest cell, half
  the thinnest dielectric and half the copper's width across the edge; `export.openems --thirds` writes the
  same mesh. A diagonal track gets no lines of its own, and one under about a cell wide falls apart into separate
  pieces on any scheme: a 0.3 mm 45° track on the 0.72 mm cells of 10 GHz read |S₂₁| = −72 dB, an open circuit,
  with or without conformal cells. So a diagonal track under 1.5 cells wide (width 2 × area / perimeter of its
  polygon, slanted edge at least three widths long, so not round pads) refines the whole mesh to half its width,
  reported as `sim.mesh_refined`; with an explicit `max_cell`, or `conformal: false`, the mesh stays and
  `sim.slanted_copper_unresolved` warns with conformal on. This mesh is `sim.sparams`'s: to cross-check with
  `export.openems`, give both the same `max_cell`.
- Determinism and threads: the result is the same bytes for any thread count (`ems_runs_the_region`,
  `measure_threads`, `measure_threads_conformal`). Without `threads`, ems's own rule (a thread per five x
  planes and 4096 cells) within cadlab's pool, which `RAYON_NUM_THREADS` caps; one thread in a build without
  the `parallel` feature.

**Guards.** `max_cells` (default 4 million cells, `sim.too_many_cells` with what to shrink); `timesteps` per
run; `threads` sets the worker threads (default: from the grid and the machine; the result does not depend on
it). The cost is cells × timesteps × ports ("Measured" below has times; the timestep follows the smallest cell:
a 35 µm copper layer in `solid` copper forces small steps; `copper: sheet` avoids it). The timestep is `courant`
(0.95) of a bound over the mesh's cells, each with its own sizes, the permittivity of its edges and the walls
and copper it lies against, and with the part of the edges and faces it shares with its neighbours that
leaves the largest of the cells' eigenvalues smallest (ems's `StepLimit::Cells`, D105 and D107: proven for the
lossless scheme and, part by part, for the laminate's poles, lossy sheets and metal, lumped parts and cut
cells; tested from noise and by runs ten times the usual length). It is never less than `courant` of the
Courant limit of the smallest cell sizes in vacuum, which it was until ems 0.0.15: 1.41 times that on the
lossy sheet line, 1.35 on the `solid` line, 1.39 on the parts, 1.22 on the via transition (its cut cells are
in the bound as the scheme has them, and keep the faces they keep at the Courant step), and equal to it where
a cut face is too small to keep at the Courant step (session 6's path). Power iterations put the limit of the
scheme without its sheets and lossy metal at 1.29 to 1.45 times: on boards of perfect copper the bound is
that limit to four digits, so `courant` is the whole margin (D107). The smallest cells of these regions are
the ones the thirds rule puts at the region's walls, where it cuts the planes; leaving them out was
evaluated and not taken (D107: a fifth fewer cells and a step 1.26 to 1.56 times as long on the lines and
the parts, and two validations of eleven further from their references, one of them by 0.04 % of an impedance). The mesh keeps
`min_cell`
between metal lines: clusters of lines merge to one each, and a line that lands closer than `min_cell` to the
line before it goes (before D70 two neighbouring clusters could leave a 5 µm sliver in a 58 µm minimum, and
that one cell set the timestep).

**Plan first.** `plan: true` sets the run up and reports without running it (nothing is written; quick): the mesh
(line counts, smallest cells per axis), ports, the parts that become lumped elements, every warning below, ems's
own timestep and what it says of the compiled model, and `estimate`: runs, timesteps of the pulse alone, a rough
count for a low-Q structure (1.5 pulses; a resonant one rings longer, up to `timesteps`), cell updates, and
minutes on one thread at a nominal 50 million cells per second.
`sim.sparams` is a query and an MCP call blocks until it ends, so this is how to find out a region is too large
before spending the time.

**Reading the result.** `frequencies` reports |S| and phase there (interpolated), `band` limits the reported best
and worst values to the band of interest (the extremes of a 1–6 GHz run say nothing about 2.4 GHz), both checked
before the run (`touchstone.out_of_band`). Reflections add the impedance looking into the port, VSWR, return loss
and mismatch loss; transmissions at a requested frequency add `available_loss_db`, −10 log₁₀(|S₂₁|² / (1 − |S₁₁|²)):
what the network dissipates or radiates, with the reflection taken out (|S₂₁| between two 50 Ω terminations mixes
them). `checks` gives the largest Σᵢ|Sᵢⱼ|² and |Sᵢⱼ − Sⱼᵢ| over the band, with warnings above 1.02
(`sim.not_passive`) and 0.03 (`sim.not_reciprocal`). The Touchstone file starts with comment lines naming the
solver, the region, ports and part values, so it explains itself away from the project.

**Part values per simulation.** `part_values` (`{"C3": {"capacitance": "1pF"}, "L1": {"esr": "0.3ohm"}}`) replaces
values for one run without changing the design, to try a matching network; designators must exist
(`sim.unknown_part`), keys are `resistance`, `capacitance`, `inductance`, `esr`, `esl` (`sim.bad_part_value`), and an
override on a part that is not an element in this region warns (`sim.override_unused`).

**What the result is not.** Notes say it before the run: `sim.parts_ideal` lists the inductors and capacitors with
nominal values only (no ESL, Q or self-resonance from a datasheet); `sim.port_replaces_part` for each port on a
component pad: the part is not simulated, the port's `z0` loads the bare pad, so the S-parameters describe the board
network between these terminations. A chip's real output impedance, a connector's body and a mated cable are not in
the model; to judge a match, compare with the impedance the part's datasheet asks for. `sim.part_in_pml` warns of
lumped parts inside the absorbing layer; `openems.port_in_pml` says how much more `margin` the port needs. With
`only_selected`, parts on none of the kept signal nets are not part of the simulation: a single note
(`sim.parts_outside_selection`) instead of a warning each.

A run stopped inside the excitation pulse warns `sim.stopped_in_pulse` (its numbers mean nothing). Errors: the export's (`openems.*`), `sim.no_ports`, `sim.port_too_thin` (a lumped
port spanning no cell), `sim.port_too_short` (a microstrip port whose feed and reference plane share a mesh
line), `sim.port_at_edge`, `sim.unstable`, `sim.ems_failed`, `cancelled`. Notes: `sim.mesh_refined`,
`sim.slanted_copper_unresolved`. An old request that still names `solver` is refused as any unknown argument is
(`args.invalid`, with the list of the arguments there are).

| Code | |
|---|---|
| `sim.ems_<code>` (warning) | ems's own warnings, typed: its code with `_` for `.` (`sim.ems_mesh_resolution`, `sim.ems_lumped_edges`, `sim.ems_conformal_stability`, …, `sim.ems_other` for a wording ems has no code for), one diagnostic per code and property with the count, and as subjects the net and layer, the component or the port's pad behind the property ems names. At most twelve, then `sim.ems_warnings_omitted`. Left out, being what cadlab made on purpose: the mesh (`mesh.in_pml`, `mesh.pml_grading`, `mesh.grading`, `mesh.time_step`: copper and laminate run into the absorbing layer, the cells are graded and thin), the probes on dual planes (`box.snapped`), the barrels' node columns (`metal.lossy_thin`) and the covered parts of cut edges of lossy copper (`metal.lossy_cut`) |
| `sim.ems_failed`, `sim.unstable` (errors) | ems refused the model; the energy went to NaN, or below zero (the invariant of a stable scheme is not negative: ems ends such a run as diverged) |

**Accuracy.** Each row is a test with a reference that is not `sim.sparams` itself; the `#[ignore]` ones run
in release (`cargo test --release --test <file> -- --ignored --nocapture`). Figures are ems 0.0.12's unless a row
says otherwise; with 0.0.15's and 0.0.16's longer steps they move by at most three units of the fourth
decimal (the 64 mm line's worst point by 0.003 dB), each case with its reference before and after in D105's
and D107's tables, and D107 has which of those moves are the step and which are where the run ends; with
0.0.18's layer and end of run the lowest frequencies and the 64 mm line's upper band move by more, towards
their references but for six figures, each case before and after in D111's table (0.0.10's stand for 0.0.11 and 0.0.12, whose Touchstone files are 0.0.10's byte for byte on the
nine cases compared: D94, D98; and 0.0.12's for 0.0.13 and 0.0.14, whose Touchstone data are 0.0.12's on the ten
cases compared, the first comment line naming the version: D103).

| Case | Reference | Agreement |
|---|---|---|
| Lumped elements between 50 Ω ports on a 0.4 mm strip 0.2 mm over ground in a closed 3 mm box, 0.5–4 GHz (`lumped_fixture_matches_circuit_theory`, ems directly): series R 10 / 50 / 200 Ω | S₂₁ = 2Z₀/(2Z₀ + Z), S₁₁ = Z/(2Z₀ + Z) | \|S₂₁\| within 0.0009, \|S₁₁\| within 0.011 (the strip's own inductance) |
| the same: series 1 pF (high-pass), 5 nH (low-pass), 10 Ω + 4 nH + 1 pF (band-pass at 2.5 GHz) | same | \|S₂₁\| within 0.016 |
| the same: shunt 2 pF, a shunt 5 Ω + 2 nH + 2 pF (notch to 2R/(2R + Z₀) = 0.167 at 2.52 GHz), shunt 50 Ω | S₂₁ = 2Z/(2Z + Z₀) | \|S₂₁\| within 0.007 / 0.009 (0.1663 at the notch) / 0.002 |
| 20 × 12 mm PEC cavity, εr 4.4, 0.5 mm cells: TM₁₁₀ and TM₂₁₀ (`cavities_match_their_modes`, ems directly) | analytic c/(2√εr) √((m/a)² + (n/b)²) | −0.05 %, −0.08 % (numerical dispersion) |
| the same with σ for tan δ 0.01 at TM₁₁₀: energy decay | Q = 1 / tan δ = 100 | 100.04 |
| parallel-plate cavity, copper plates (58 MS/m, conducting sheets) 0.5 mm apart, εr 4.4, TM₁₁₀ at 1.4, 6.9 and 13.9 GHz | Q = h/δ (the plates' loss only) | +0.08 %, −0.14 %, −0.22 % |
| cavity filled with Djordjevic–Sarkar εr 4, tan δ 0.02 at 1 GHz (the Debye fit of `fdtd::fit` over 0.1–10 GHz or 0.5–20 GHz), TM₁₁₀ at 2.4, 9.0 and 14.8 GHz | resonance f √εr(f) = f_vacuum; Q = 1/tan δ(f) | resonance −0.01 %; Q −0.74, −0.65, −0.67 % |
| `sim.sparams`: lossless microstrip, W = 1 mm on 0.5 mm of εr 4, sheet copper, 20 mm between the reference planes of microstrip ports, 1.1–3.6 GHz (`measure_microstrip`; ems 0.0.10) | 2D field solver (D49): Z₀ 50.81 Ω, εeff 3.071 | Z₀ −0.2 to −1.2 %, εeff +0.31 to +0.59 % |
| `sim.sparams`: stripline, W = 0.25 mm between planes 0.3 mm away on either side, εr 4, sheet copper, 1.1–3.1 GHz (`measure_stripline`; ems 0.0.10) | field solver: 55.28 Ω, εeff 4 | Z₀ −0.7 to −1.8 %, εeff +0.44 to +0.45 % |
| `sim.sparams`, the full model (`sim_sparams_microstrip_validation`): a routed 64 mm line, W = 1 mm on 0.5 mm of Djordjevic–Sarkar εr 4 / tan δ 0.02 at 1 GHz, 35 µm solid copper at 58 MS/m, microstrip ports 4 mm; Z₀ and εeff from the S-parameters (ABCD of the 60 mm between the reference planes), 1–3.1 GHz | field solver with the copper thickness and the same laminate model | Z₀ +1.0 to +2.9 %, εeff +0.7 to +1.0 % |
| the same: matched line | | \|S₁₁\| at most 0.029 (−31 dB) from 0.6 to 9 GHz |
| the same: \|S₂₁\| (total loss: conductor with skin effect, dispersive dielectric) | D52, 0.136 dB at 0.6 GHz to 1.657 dB at 9 GHz | −1.3 % (1.1 GHz) to +5.3 % (9 GHz) |
| the same line with perfect solid copper, 24 mm: the dielectric loss alone, 0.56–6 GHz (`measure_dielectric_loss`; ems 0.0.10) | D52 | −2.5 to +7.1 % (and +15 % at 1.05 GHz, 0.009 dB): within the ±0.02 dB of one run at this mesh |
| the same line lossless (perfect copper, tan δ 0), the side PML 4 h from the strip (`lossless_line_near_the_pml_is_passive`) | passivity, \|S₁₁\|² + \|S₂₁\|² ≤ 1 | 0.9991 to 1.0007 from 0.6 to 3.8 GHz |
| the same line, 24 mm, sheet copper, 25 µm solder mask of εr 3.8 (`sim_sparams_mask_validation`) | field solver with the conformal mask: Z₀ 50.07 Ω, εeff 3.162 (bare line 3.071) | Z₀ +1.1 %, εeff −0.3 %; the mask's change of εeff −23 % (the bare line reads +0.4 % high) |
| `sim.sparams`: a 0603 part (pads 0.9 × 1 mm, 0.7 mm gap) in a 50 Ω microstrip (1 mm on 0.5 mm of εr 4), microstrip ports, 0.5–6 GHz (`sim_sparams_parts_validation`): 50 Ω resistor; 1 pF capacitor; 1 pF with 1 nH ESL and 1 Ω ESR | ideal series element between 50 Ω | \|S₂₁\| within +0.007 (R), +0.009 (C), +0.008 (RLC); the pads' own gap capacitance and steps account for the rest |
| `sim.sparams` (`routed_45_degree_line`): 8 mm, 45° run of 14.1 mm, 8 mm, perfect sheet copper, W = 1 mm, H = 0.5 mm, εr 4: arg S₂₁ at 10 GHz, \|S₁₁\| | the straight line of that length, 164.2° and −35.5 dB (the bends' own effect aside) | default mesh (0.75 mm): staircase 104.1°, −16.4 dB; conformal (refined to 0.49 mm) 154.7°, −23.4 dB; 0.25 mm cells: staircase 143.5°, −18.2 dB; conformal 157.7°, −27.7 dB |
| the same, W = 0.3 mm, H = 0.17 mm, εr 4.3 | straight 154.8° (as the removed engine read it, D73) | default mesh: staircase and conformal without refinement are open circuits (−72 dB, D73); with the refinement (0.15 mm, 2.46 M cells, 156 s on six threads): 141.1°, −22.7 dB |
| `sim.sparams`: a via of one cell (0.3 mm drill, 0.6 mm pads on 0.3 mm cells), four layers, perfect sheet copper (`ems_runs_a_via_of_one_cell`, `measure_via_convergence`; ems 0.0.8 on) | the staircase on the same mesh, and a finer mesh | \|S₂₁\| at 4 GHz −0.390 dB with conformal cells, −0.440 dB as a staircase; on 0.2 mm cells ems's own measurement (its DESIGN.md, 5.13.3) is −0.297 and −0.368 dB |
| `sim.sparams`: session 6's 2.4 GHz matching network (U1.30 to J1.1, shunt C3, series L1, lossy sheet copper, diagonal tracks 1.1 cells wide, 1–6 GHz, to −50 dB; `rf_path_full_wave`) | the ideal chain of the same parts | at 2.44 GHz \|S₁₁\| 0.5726, \|S₂₁\| 0.7988, within the test's 0.15 and 0.1 of the chain; largest Σ\|S\|² 0.982 |
| `sim.sparams` against openEMS on the export of the same region (CI job "openEMS oracle", `tests/openems.rs`; D103) | openEMS, and for each of the two D52, the 2D field solver or circuit theory (`solver_comparison`) | the lossy lines within 36.0 % (sheets) and 40.2 % (`solid`) of each other at 0.56 GHz, where the absolute term of the test's 15 % + 0.025 dB carries them and where openEMS is a third over D52; from 0.5 GHz ems within 0.019 and 0.040 dB of D52 (openEMS 0.027 and 0.086 dB); Z₀ of the sheet line 0.3 % from the field solver (openEMS 4.0 %); the via transition within 0.07 dB, 0.047 dB on the same mesh; the parts within 0.0048 and 0.0099 of the ideal (openEMS 0.0029 and 0.0089) |
| one thread against three or four, lossless and with lossy sheets, lossy `solid` copper, Debye terms and conformal cells (`ems_runs_the_region`, `measure_threads`, `measure_threads_conformal`) | | the same bytes |
| the coarse lossy line of `ems_runs_the_region`, between microstrip ports to 5 GHz and between lumped ports to 10 GHz | the recorded result of cadlab's removed engine on the same region and mesh (commit 3aa423f) | \|S₂₁\| within 0.0020 and \|S₁₁\| within 0.0034; lumped ports \|S₂₁\| within 0.0026 |

The cavities and the closed box (single mode, nothing radiated) are the precise checks of ems's loss models and
elements as cadlab uses them; the line runs check that they combine into D52's total, with the open region's own
measurement error.

**Measured** (`tests/fdtd_ems.rs`, the `measure_*` tests, and `rf_path_full_wave`; release builds, one run per
process, three threads, on a shared machine: D98, where the same table has the removed engine's column). The
peak is the whole process's, 22 MB of it before the run.

| Case | Cells | Time; peak memory |
|---|---|---|
| Bent line (8 mm, 14.1 mm at 45°, 8 mm), lossy sheets, 0.25 mm cells, to −70 dB | 578 680 | 28.1, 26.0 s; 91, 92 MB (on one thread 60.3, 64.2 s; 86 MB) |
| The same, lossy `solid` copper | 679 320 | 37.8, 37.0 s; 113, 111 MB |
| Bent line on the default mesh (0.49 mm), to −70 dB: sheets; `solid` | 135 660; 184 110 | 8.3, 7.9 s; 49 MB. 13.0, 12.0 s; 49, 50 MB |
| The openEMS oracle's lossy line (24 mm, 6 GHz), to −70 dB: sheets between microstrip ports; `solid` copper between lumped ports | 67 200; 85 320 | 15.0, 14.3 s; 42, 41 MB. 28.2, 28.3 s; 49, 48 MB |
| Via transition of the oracle (4 layers, lossy solid copper, absorbing cells beyond the region, 4 GHz): to −40 dB; to −60 dB, the default | 130 626 | 35.6, 37.5 s; 60, 59 MB. 54.0, 51.6 s; 58, 62 MB |
| Via on a region of 25 194 cells, to −30 dB: three threads; one | 25 194 | 16.1, 14.8 s; 47, 48 MB. 37.8, 38.0 s; 48 MB |
| Session 6's matching network (lossy sheets, graded mesh, to −50 dB), 14 529 steps per port | 152 280 | 42.9, 44.6, 42.7 s |

**Limits.** Parts are two-pin R, L, C only, ideal series elements at their values (no package model beyond
`esr` / `esl`, no self-resonance of inductors, no ferrite beads, ICs or connectors: those pads are bare
copper; terminate them with ports), placed across their pads' gap along x or y. Conformal cells correct the
outline, not the field's singularity at a sheet's edge, whose error stays first order, and a slanted strip needs
about two cells across (the mesh is refined for it). Dielectric boundaries (the board's outline, non-plated
holes) still staircase. The surface impedance assumes a skin depth well below the copper's thickness and its
features (as D52; no DC floor below the skin-effect range). `sheet` copper has no thickness: its edge current is
bounded only by the mesh, so its conductor loss depends on the cells at the strip edges; use `solid` copper for
loss. A thin mask makes thin cells, hence a smaller timestep. Accuracy depends on the mesh as for any FDTD: keep
`cells_per_wavelength` ≥ 20 and several cells across the narrowest copper and dielectric (`max_cell` refines
everything; the thirds rule the edges). Runs are long on large regions: cut the region to the structure
(`around` a via or connector, `margin`), use `sheet` copper, and keep `f_max` to what is needed.

**What is open:**

- *A second result.* Until D100 the same model could be run on cadlab's own engine in the same process, and a
  difference between the two beyond their known agreement was worth looking at. What stands in for it: the
  openEMS oracle (CI only, nothing to install locally but openEMS itself), the checks of a result
  (`sim.not_passive`, `sim.not_reciprocal`), a second mesh (`max_cell`), and `export.openems` of the same region.
- *Memory.* ems took 1.3 to 1.8 times the removed engine's (D98: 91 MB against 57 MB on 580 000 cells). By ems's
  notes: while it builds a port's engine, two `f64` vectors over the grid for the stability estimate of its
  conformal cells and the sheets' edges in their installed form beside the final one (the peak on the regions
  with slanted copper); while it runs, the coefficients where the mesh is graded in the plane, the compiled
  model next to the engine, a list of the Debye poles, and a value per step for each probe (30 probes and 49 000
  steps on the via transition: 12 MB).
- *Threads on a machine of one's own.* Every time above was taken in a pool of three threads on a shared
  machine. Without `threads` ems takes up to a thread per core of what the pool has: what that gives in time on
  more cores was not measured.
- *A build without the solver* does not exist: ems and csx are in every build (21 s of a release build).

**The engine that was removed** (D100). From D59 to D98 `sim.sparams` ran on an FDTD engine written in cadlab
(`fdtd::engine`, with its own conformal cells and surface impedance), and from D86 on ems as well, first as an
option and since D94 as the default. D86 to D98 hold the comparisons, and the last table with both columns is
D98's. In short, on the same mesh and timestep: on perfect conductors, lossless and Debye dielectrics and lumped
elements the two agreed to three or four digits; lossy copper along the mesh to 0.0002 dB in the conductor's part
of |S₂₁| for `solid` copper and to 4 % for sheets on the oracle's line, roughness to 0.0007 dB; copper of about
a cell, with conformal cells in both, to 0.018 dB in |S₂₁| on the via transition, to 0.1° (sheets) and 1.5°
(`solid` copper) on the bent line at two cells across the track, and to 0.001 in |S₁₁| and |S₂₁| on session 6's
matching network. ems took 0.3 to 0.9 times the engine's time with three threads each, 1.1 times on one thread
on a small region with a Debye laminate, and 1.3 to 1.8 times its memory. The engine's own validations (its
update on cut cells against cylinder and patch resonances, energy conservation, the Courant limit, its surface
impedance in resonators) went with it; they are in the history before D100. A Touchstone file whose first line
says `(solver: builtin)`, or names no solver (before D94), is that engine's.

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
return loss = largest |S11|, worst insertion loss = smallest |S21|). `band` narrows the extremes to two
frequencies. Reflections add `reflection` (impedance looking into the port, VSWR, return loss, mismatch loss) and
transmissions at a requested frequency `available_loss_db`; `checks` reports passivity and reciprocity, with
`touchstone.not_passive` and `touchstone.not_reciprocal` warnings. The port count of a version 1 file comes
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
