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
dimensions, cadlab produces a correct land pattern (`src/landpattern/`).

- Pads from the IPC-7351B equations (Z/G/X with toe/heel/side fillets, RMS tolerance stacking, F = 0.05 mm,
  P = 0.025 mm, 0.01 mm rounding; all configurable), at Most/Nominal/Least density.
- Families today: chip (0201–2512, resistor/capacitor/inductor/LED/diode/fuse), gull-wing two-row (SOIC, SOP,
  TSSOP, MSOP, SOT-23-3/5/6 with unpopulated slots), QFP, DFN/SON, QFN (exposed pad with paste windows), THT pin
  headers. Planned: SOT-223/DPAK (tab), SOD/MELF, molded bodies (SMA/SMB), BGA, DIP, terminal blocks.
- Outputs: pads, paste windows, courtyard, silkscreen clipped around pads (via polyclip), fab outline with pin-1
  chamfer, pin-1 dot, body dimensions for 3D.
- Names follow IPC-7351 (`RESC1005X40N`, `SOIC127P600X175-8N`, `QFN50P500X500X90-33N`). Common names (`0402`,
  `SOT-23-5`, `SOIC-8`, `TSSOP-20`, `LQFP-48`, `QFN-32 5x5mm P0.5mm EP3.1mm`, `PinHeader 2x05`) map to typical
  JEDEC/EIA dimensions (`landpattern::packages`); for anything else, pass the datasheet dimensions as a
  `PackageSpec` (`"0.15..0.35mm"`, `"1.0±0.05mm"`).
- **To verify:** the fillet goal table (`src/landpattern/ipc.rs`) follows IPC-7351B as best known; review it
  against the standard. A SOIC-8 computed with it matches widely used IPC-derived footprints to 0.01 mm.

## Symbol generation

From a pin table (which an agent can extract from a datasheet), `symbolgen` lays out a box symbol on a 2.54 mm
grid: ground pins at the bottom, supply inputs at the top, inputs and bidirectional pins left, outputs right,
connector pins left in number order. Ports (`PA*`, `PB*`, or explicit `group`s) stay together and are moved
between sides to balance them; explicit `side`s are kept. Two-terminal parts get fixed styles (resistor,
capacitor, inductor, diode/LED with pin 1 = cathode, ...). Multi-unit symbols are planned. Drawing happens in M3.

## Generic part specs

`R 10k 1% 0402`, `C 100nF 16V X7R 0402`, `L 4.7uH 1A 0805`, `FB 600R 0603`, `LED red 0603`: type, value and
package, plus optional tolerance, voltage/current/power rating, dielectric, color. The spec gets a canonical
form and ID (`R_10k_1pct_0402`), so the same requirement written differently reuses the same part.

## Commands

| Command | Purpose |
|---|---|
| `part.generic <spec>` | add a generic passive with generated symbol and footprint |
| `part.create` | concrete or custom part from a pin list + package name or dimensions |
| `part.list / show / set / remove` | inspect and edit the library (removal refused while in use) |
| `footprint.generate / list / show / remove` | land patterns |
| `circuit.add <part or spec>` | add components (`--count`), auto-numbered by category (R1, C3, U2) |
| `circuit.remove / list` | components |
| `bom.list` | one line per part: quantity, designators, DNP, order MPN, unsourced lines |
| `bom.approve <part>` | approved MPNs: alternates (concrete parts) or candidates (generic parts) |
| `bom.dnp <refdes>` | do not populate (stays on the board, excluded from assembly) |
| `bom.note <part> <text>` | purchasing/assembly note |
| `bom.replace <from> <to>` | switch components to another part, warning about missing pins |
| `bom.export <path> --format generic|jlcpcb|pcbway` | CSV; fab layouts omit DNP and warn about lines without MPN |

Planned with supplier research (rest of M1): `part.search`, `bom.resolve`, `bom.check`, `bom.cost`. Fab
column layouts move to fab profiles in M4 (they follow the fabs' current templates; verify before ordering).

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
