# Roadmap

Milestones are ordered by dependency. Each one ends with something usable end to end, even if limited.
Rough sizes: **S** ≈ days, **M** ≈ 1–3 weeks, **L** ≈ 1–2 months, **XL** = open-ended research.

The first useful target is **M4**: a simple two-layer board (MCU + USB-C + LDO + LEDs) designed entirely through
the CLI/MCP, hand- or simple-auto-routed, passing DRC and producing Gerbers that a fab (e.g. JLCPCB) accepts.

---

## M0 — Foundations (M)

Workspace skeleton and the pieces every later milestone depends on.

- [x] Crate and module layout per [ARCHITECTURE.md](ARCHITECTURE.md) (single crate, D16)
- [x] Core types: units (`Nm` = i64 nanometers, `Angle`), points/vectors, transforms, bounding boxes
- [x] Geometry: `cadlab::geom` adapter over polyclip ([POLYGON_LIB.md](POLYGON_LIB.md)), from crates.io
- [ ] Spatial index (R-tree) for shapes (moved to M4, where DRC first needs it)
- [x] ID scheme: stable internal IDs (`ObjectId`, persisted allocator), `ObjectRef` name syntax, "did you mean"
      suggestions (model-aware resolution comes with the circuit in M2)
- [x] Command system: `Command` trait, registry, transactions, undo/redo, dry-run, diagnostics type
- [x] Project load/save, deterministic serialization, schema versioning and migrations
- [x] `cadlab` binary: clap CLI generated from the registry, `--json` output, `call`, `batch`, `describe`
- [x] Settle async boundaries and MCP implementation: own minimal MCP layer, no async runtime (DECISIONS D14)
- [x] `cadlab mcp`: MCP server over stdio exposing the registry, `describe` tool
- [x] CI: fmt, clippy, tests, docs, MSRV; golden-file and snapshot tests; oracle discovery helpers in
      `tests/common` ([TESTING.md](TESTING.md)); the oracle CI job is added with the first oracle test (M2)
- [x] License check in CI (`cargo-deny`): MIT-compatible dependencies only

**Exit:** `cadlab project new demo && cadlab -p demo project info --json` works via CLI and MCP; round-trip tests
pass. **Done 2026-10-04** (spatial index deferred to M4).

## M1 — Parts, libraries, BOM (L)

Details in [PARTS.md](PARTS.md).

- [x] Part model: MPN, manufacturer, typed parameters (exact decimal values), symbol (pins + electrical types),
      footprint(s), pin↔pad map
- [x] Generic parts (`R 10k 1% 0402`) vs concrete parts (MPN); manual resolution (`bom.approve`, `bom.replace`)
- [x] Project-local library (one file per part/footprint)
- [x] Shared user libraries: user library + configured directories, `lib.list/show/publish/import/remove` (D19)
- [x] Own base library: footprint generator (IPC-7351B: chip, SOIC/SOP/TSSOP/MSOP/SOT-23, QFP, DFN, QFN, pin
      headers) and symbol generator from pin tables (no KiCad library content, see D7)
- [ ] More footprint families: SOT-223/DPAK, SOD/MELF, SMA/SMB, BGA, DIP
- [x] BOM commands: list (grouped), replace, DNP, approved alternates, notes; components via `circuit.add/remove`
- [x] Supplier research provider trait, offline catalog provider, response cache with TTL and offline mode
- [x] DigiKey provider (API v4)
- [ ] More network providers: LCSC/JLCPCB, PCBWay, Mouser, Nexar/Octopart (see PARTS.md)
- [x] Search and filter by parameters, stock, price, lifecycle (`part.search`); `bom.resolve` for generic lines
- [x] BOM CSV export (generic, JLCPCB, PCBWay layouts)
- [x] BOM cost rollup at build quantity (`bom.cost`), availability check (`bom.check`)

**Exit:** an agent can go from "I need a 3.3 V LDO, 500 mA, SOT-23-5, in stock" to a concrete part in the BOM with
symbol and footprint attached.

## M2 — Circuit and ERC (M)

- [x] Components (instances of parts, refdes auto-assignment, rename), nets, pins, externally driven power nets
- [x] Connect/disconnect/merge, no-connect marks; pin ranges and buses (`net.connect DATA[0..7] U1.PA0..PA7`)
- [x] Hierarchy: reusable blocks captured from existing components, instantiated with port mapping (D18)
- [x] Block library shared across projects: self-contained block files in shared libraries (D19)
- [x] Net classes (width, clearance, via size, diff pair) attached at circuit level
- [x] ERC: unconnected pins, conflicting drivers, undriven power inputs (ground exempt), single-pin nets, pin-type rules
- [x] Netlist export (KiCad netlist, JSON); oracle comparison comes with KiCad schematic export (M3)
- [x] Text summaries designed for LLM context (`cadlab circuit summary`)

**Exit:** a full MCU board circuit described via commands, ERC clean, netlist exported. **Done 2026-10-04**
(`tests/circuit.rs`, ATtiny85 board).

## M3 — Schematic view and rendering (M)

Details in [RENDERING.md](RENDERING.md).

- [x] Renderer core: scene → SVG and → PNG (tiny-skia), embedded Hershey stroke font
- [x] Symbol rendering from part definitions; footprint rendering
- [x] Schematic auto-layout: anchors (ICs/connectors), passives inline on their pin with chains, decoupling rows,
      net labels and power/ground symbols elsewhere; papers A4..A0
- [ ] Layout improvements: group by block instance, denser packing, multi-sheet
- [x] Optional manual hints (`schematic.place/unplace`) persisted in `schematic.json`
- [x] KiCad `.kicad_sch` export (`schematic.export`): lets `kicad-cli` run ERC as an oracle, and lets humans open it if they want

**Exit:** `cadlab render schematic -o sch.png` produces a readable schematic of the M2 board.

## M4 — Board setup, placement, DRC, fab outputs (L)

- [x] Board model ([BOARD.md](BOARD.md)): stackup, outline (rectangle, rounded, circle, polygon), rules with
      conservative IPC class 2 defaults, tracks, vias, zones, keep-outs, graphics
- [ ] Mounting holes, outline cutouts
- [ ] Design rules: clearances, widths, via/drill limits, per net class, IPC class 2/3 presets
- [ ] Fab profiles ([MANUFACTURING.md](MANUFACTURING.md)): JLCPCB and PCBWay, verified from their published
      capabilities
- [ ] Provider-agnostic flow: compatibility targets, `fab check`, `fab compare`, `export fab --fab`, per-fab part
      resolution with substitution report, `fab-lock.json`
- [x] Footprint placement commands: set, move, rotate, flip (bottom side mirrored), lock, remove, list
- [ ] Align, distribute
- [x] Ratsnest (MST between copper islands per net), initial auto-placement (rows inside the outline)
- [ ] "Place near", decoupling caps next to pins, placement by schematic groups
- [x] Manual routing commands: tracks through coordinates or pins (net inferred, shorts refused), vias; net
      class widths and via sizes
- [ ] Copper zones with fill (thermal reliefs, clearances, islands removal)
- [ ] DRC: clearance, width, annular ring, drill, hole-to-hole, copper-to-edge, courtyard overlap, unrouted nets,
      silk over pads, zone min width
- [ ] Board rendering (per layer, composite, realistic top/bottom)
- [ ] Outputs: Gerber X2 (+ X3 component data), Excellon/XNC drill, pick-and-place CSV, BOM CSV, IPC-D-356A,
      archive per fab profile
- [ ] KiCad `.kicad_pcb` export for oracle tests: KiCad DRC vs cadlab DRC, KiCad Gerbers vs ours (raster XOR)
- [ ] gerbv oracle: our Gerbers parse and render as expected

**Exit:** the target demo board passes cadlab DRC and the KiCad DRC oracle, and the *same unmodified project*
exports bundles that pass both JLCPCB's and PCBWay's online checks.

## M5 — Autorouter v1 (L)

Details in [ROUTER.md](ROUTER.md).

- [ ] Specctra DSN import / SES export, implemented from the published Specctra spec (freerouting as benchmark
      oracle only)
- [ ] Obstacle model, connection planning (per-net MST), ordering heuristics
- [ ] Grid-based multi-layer A* maze router with vias, 45° moves
- [ ] Negotiated-congestion rip-up and reroute
- [ ] Post-processing: pull-tight, corner smoothing, via reduction
- [ ] DRC-verified output, progress reporting, cancellation, time budget
- [ ] `route` commands: whole board, net, net class, between two pads; keep or rip existing tracks

**Exit:** routes typical 2- and 4-layer hobby boards (≤ 200 nets) to 100% with zero DRC errors.

## M6 — Autorouter v2: freerouting parity (XL)

- [ ] Gridless, shape-based router (free-space decomposition, expansion rooms)
- [ ] Any-angle / 45° optimized output, arc support
- [ ] Push-and-shove for incremental and interactive (API-driven) routing
- [ ] BGA/fine-pitch fanout, escape routing
- [ ] Parallel routing (independent regions/nets) with deterministic merge
- [ ] Benchmark suite: completion rate, vias, wirelength, runtime vs freerouting

**Exit:** equal or better completion than freerouting on the benchmark corpus, with comparable runtime.

## M7 — KiCad import and more fabs (M)

KiCad writers and oracle checks already exist from M2–M4. This milestone adds import for migrating user projects.

- [ ] `.kicad_pcb` import, `.kicad_pro` rules import, user `.kicad_sym` / `.kicad_mod` import
- [ ] KiCad netlist import (circuits come in as netlists; no `.kicad_sch` parser, see DECISIONS D13)
- [ ] Round-trip and oracle tests on open-source projects fetched in CI
- [ ] More fab profiles: OSH Park, Aisler, Eurocircuits, Seeed Fusion, NextPCB, PCBgogo, ALLPCB, Elecrow

## M8 — Advanced electrical (L)

- [ ] Differential pairs (routing + rules), length/skew matching with meanders
- [ ] Impedance calculator from stackup (microstrip/stripline) → width per net class
- [ ] SPICE netlist export (ngspice), simulation hooks
- [ ] Current/thermal checks (IPC-2152 trace width)
- [ ] Design lint beyond ERC: missing decoupling, missing pull-ups on I²C, unterminated high-speed nets

## M9 — 3D and advanced rendering (M)

- [ ] Isometric 3D PNG render: board, layers, extruded package bodies generated from package dimensions
- [ ] STEP/VRML model import for accurate bodies
- [ ] STEP export of the assembled board
- [ ] IPC-2581 rev C output; ODB++ if spec terms allow
- [ ] IDF 3.0 / IDX export for MCAD

---

## Cross-cutting, always on

- **Docs:** every command documented from its schema; examples doubled as tests.
- **Performance:** benchmarks tracked in CI from M4 on (load, DRC, zone fill, route).
- **Determinism:** golden files for every exporter and renderer.
- **Agent ergonomics:** after each milestone, run a scripted agent session that designs a board from a prompt,
  and fix whatever the agent got stuck on.
