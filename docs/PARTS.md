# Parts, libraries and BOM

## Part model

A **part** bundles everything needed to use a component in a design:

```
Part
├── id                 library-unique, e.g. "ldo/AP2112K-3.3"
├── kind               Generic | Concrete
├── manufacturer, mpn  (Concrete only)
├── category           resistor, capacitor, ldo, mcu, connector, ...
├── parameters         typed: resistance=10k, tolerance=1%, voltage_out=3.3V, package=SOT-23-5, ...
├── symbol             pins: number, name, electrical type (in/out/bidir/power_in/power_out/passive/nc/...),
│                      graphical units for multi-unit parts; drawing optional (auto-generated from pins)
├── footprints         one or more candidates; pin → pad mapping per footprint
├── model3d            optional: generated body dims, or STEP/VRML reference
├── datasheet          URL (+ cached local copy path)
├── sourcing           supplier offers: supplier, SKU, stock, price breaks, MOQ, lifecycle, fetched_at
└── provenance         where it came from (generated, supplier, user import, hand-written) + license
```

**Generic** parts describe a requirement (`resistor 10k 1% 0402`). They are enough for circuit design and layout,
since the footprint is known from the package. **Concrete** parts carry an MPN.

A BOM line holds the requirement and, optionally, an ordered list of **approved MPNs** (primary + alternates). It
does not hold a supplier or fab SKU as its identity. Resolution to an orderable SKU happens **per fab, at export**
(`fab check` / `export fab`), picking the first approved MPN the fab or its supplier has in stock, then falling
back to any part matching the generic requirement, and reporting every substitution. The same project can
therefore be built at JLCPCB one month and PCBWay the next. See
[MANUFACTURING.md](MANUFACTURING.md#provider-agnostic-projects).

## Libraries

- **Project library** (`library/` in the project): every part used by the project is copied here. A project never
  breaks because an external library changed.
- **User libraries**: directories or git repos registered in user config, searched by `part search`.
- **Generated**: footprints and symbols created from parameters (below). This is the main source: cadlab ships
  its own base library built from generators, never from KiCad's libraries (DECISIONS D7).
- **User imports**: a user's own KiCad `.kicad_sym` / `.kicad_mod` files can be imported (M7). The user is
  responsible for the license of what they import, which is recorded in provenance.

## Footprint generation (IPC-7351B)

Many standard packages can be computed instead of looked up, which helps agents a lot: given datasheet
dimensions, cadlab produces a correct land pattern.

- Families: chip (01005–2512), MELF, SOD/SOT, SOIC/SSOP/TSSOP/MSOP, QFP, QFN/DFN (with exposed pad, paste
  subdivision), BGA, SON, through-hole (axial, radial, DIP, pin headers, terminal blocks).
- Inputs: body and lead dimensions with tolerances, pitch, density level (Most/Nominal/Least).
- Outputs: pads, paste and mask apertures, courtyard, silkscreen, fab layer outline, pin-1 marker, 3D body dims.
- Names follow the IPC-7351B naming convention (`QFN50P500X500X80-33N`, `RESC1005X40N`). Common package names
  (`0402`, `SOT-23-5`, `QFN-32 5x5 0.5mm`) are accepted as aliases and resolved to generator parameters.

## Symbol generation

From a pin table (which an agent can extract from a datasheet), generate a clean rectangular symbol: pins grouped by
function (power top/bottom, inputs left, outputs right, by port), with configurable ordering. Multi-unit split for
large parts.

## BOM

The BOM is a view over circuit components plus a sourcing overlay.

| Command | Purpose |
|---|---|
| `bom list` | grouped lines (same part → one line, refdes list, qty) |
| `bom add <part>` | add part to library (and optionally instantiate) |
| `bom remove <part|refdes>` | remove, with impact report (nets left dangling) |
| `bom replace <old> <new>` | swap part, checking pin/footprint compatibility; reports remapping |
| `bom set-dnp <refdes>` | do not populate (stays on board, excluded from assembly) |
| `bom alternates <part> add/remove` | approved substitutes |
| `bom resolve` | propose approved MPNs for generic lines, given policy (stock across suppliers, price, lifecycle) |
| `bom availability --fab a,b` | per-line stock/coverage at each fab or supplier |
| `bom check` | stock at build qty, lifecycle (NRND/EOL), missing footprints/MPNs |
| `bom cost --qty 100` | price rollup per supplier, best mix |
| `bom export --fab jlcpcb|pcbway|...` | assembly BOM files in the fab profile's layout (see MANUFACTURING.md) |

## Research providers

Research is behind a trait so providers can be added independently and tested with recorded responses:

```rust
#[async_trait]
pub trait PartProvider {
    fn id(&self) -> &'static str;
    async fn search(&self, q: &SearchQuery) -> Result<Vec<PartCandidate>>;
    async fn details(&self, key: &ProviderKey) -> Result<PartDetails>;   // params, offers, datasheet
}
```

Candidates to implement (verify API terms and access requirements before each):

- Nexar / Octopart (aggregator, broad coverage)
- DigiKey API, Mouser API, Farnell/element14 API
- LCSC / JLCPCB parts catalog (JLCPCB assembly; basic vs extended part classes)
- PCBWay assembly parts sourcing
- Local/offline catalog: user CSV or a stocked-parts list (e.g. "what I have in my drawers")

Cross-cutting:

- API keys from env vars or `~/.config/cadlab/config.toml`. Never stored in projects.
- Responses cached in `.cadlab/cache/` with TTL. Offline mode uses cache only.
- Results normalized to the part model (units parsed, packages mapped to footprint generator names).
- The library does not judge or summarize datasheets. It fetches and exposes them (URL, cached PDF path). The agent
  reads them.
