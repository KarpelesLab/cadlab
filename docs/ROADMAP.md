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
- [x] Spatial index (R-tree) for shapes: `geom::RTree`, packed (STR), used by zone fill and the DRC outline rules (D42)
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
pass. **Done 2026-10-04** (spatial index added with the D42 performance pass).

## M1 — Parts, libraries, BOM (L)

Details in [PARTS.md](PARTS.md).

- [x] Part model: MPN, manufacturer, typed parameters (exact decimal values), symbol (pins + electrical types),
      footprint(s), pin↔pad map
- [x] Generic parts (`R 10k 1% 0402`) vs concrete parts (MPN); manual resolution (`bom.approve`, `bom.replace`)
- [x] Project-local library (one file per part/footprint)
- [x] Shared user libraries: user library + configured directories, `lib.list/show/publish/import/remove` (D19)
- [x] Own base library: footprint generator (IPC-7351B: chip, SOIC/SOP/TSSOP/MSOP/SOT-23, QFP, DFN, QFN, pin
      headers) and symbol generator from pin tables (no KiCad library content, see D7)
- [x] More footprint families: SOT-223/DPAK/D2PAK, SOD/MELF, SMA/SMB/SMC, BGA, DIP. Tables against IPC-7351B
      (D78, PARTS.md "Sources of the fillet tables"): molded body and the BGA land table checked with two
      independent sources (BGA lands now from the table); MELF, flat lead and flat lug equal to one source.
      Not checked: flat lead Least side goal and Least courtyard, BGA courtyard, BGA lands for 0.17/0.15 mm balls
- [x] BOM commands: list (grouped), replace, DNP, approved alternates, notes; components via `circuit.add/remove`
- [x] Supplier research provider trait, offline catalog provider, response cache with TTL and offline mode
- [x] DigiKey provider (API v4)
- [x] More providers: Mouser (Search API) and Nexar/Octopart (GraphQL) network providers; LCSC/JLCPCB through
      `catalog.import` of user-downloaded parts lists, since their APIs are partner-only; PCBWay has no parts
      API (D33, PARTS.md)
- [x] Search and filter by parameters, stock, price, lifecycle (`part.search`); `bom.resolve` for generic lines
- [x] BOM CSV export (generic, JLCPCB, PCBWay layouts)
- [x] BOM cost rollup at build quantity (`bom.cost`), availability check (`bom.check`)

**Exit:** an agent can go from "I need a 3.3 V LDO, 500 mA, SOT-23-5, in stock" to a concrete part in the BOM with
symbol and footprint attached. **Done 2026-11-13** (`tests/sourcing.rs`, `ldo_from_requirement_to_bom`: through
the registry with the offline catalog as the only provider, `part.search` → `part.create --fill-from-suppliers` →
`circuit.add`; the BOM line ends with manufacturer and MPN, in stock per `bom.check`, and the saved part has its
symbol, the generated SOT-23-5 footprint and a one-to-one pin↔pad map. The pin list is the agent's input, read
from the datasheet: no provider supplies pinouts.)

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
- [x] Layout improvements: block instances in titled frames, pull-ups/pull-downs as branches on their pin,
      crystals with load capacitors between their pins, decoupling capacitors on shared rails, collision-checked
      placement (no overlapping elements), skyline packing, multi-sheet rendering (A3 max; KiCad export one sheet,
      DECISIONS D24)
- [x] Optional manual hints (`schematic.place/unplace`) persisted in `schematic.json`
- [x] KiCad `.kicad_sch` export (`schematic.export`): lets `kicad-cli` run ERC as an oracle, and lets humans open it if they want

**Exit:** `cadlab render schematic -o sch.png` produces a readable schematic of the M2 board. **Done 2026-11-13**
(`tests/schematic_layout.rs`, `attiny_schematic_png_and_svg_are_readable`: `render.schematic` of the ATtiny85
board to PNG and SVG, with "readable" measured: every component drawn once, the nets read back from the sheet's
wires, labels and power symbols equal to the circuit's pin for pin, no overlapping elements, everything inside
the frame and off the title block, ink in the PNG at every symbol and label, both files byte-identical from a
second build, the SVG against `tests/golden/attiny85/schematic.svg`. The path is positional or `--path`; there is
no `-o`.)

## M4 — Board setup, placement, DRC, fab outputs (L)

- [x] Board model ([BOARD.md](BOARD.md)): stackup, outline (rectangle, rounded, circle, polygon), rules with
      conservative IPC class 2 defaults, tracks, vias, zones, keep-outs, graphics
- [x] Mounting holes, outline cutouts (`board.hole`, `board.cutout`; seen by DRC, zones, rendering, outputs)
- [x] Design rules: clearances, widths, via/drill limits, per net class, IPC class 2/3 presets
      (`board.rules` presets `ipc2`/`ipc3` and rules derived from a fab profile, `netclass.show`, DRC class via
      and class-vs-minimum checks; IPC numbers in `rule-presets/ipc.toml`, confirmed on secondary sources, D26, D79)
- [x] Fab profiles ([MANUFACTURING.md](MANUFACTURING.md)): JLCPCB and PCBWay, verified from their published
      capabilities (2026-10-04), plus a generic IPC class 2 profile; user overrides (`fab-profiles/`, `src/fab/`)
- [x] Provider-agnostic flow: compatibility targets, `fab check`, `fab compare`, `export fab --fab`, per-fab part
      resolution with substitution report, `fab-lock.json` (as `fab.*` and `drc.run` targets; ranked substitute
      candidates in `fab.check`/`fab.export` and `bom.substitutes`, applied per fab with `fab.substitute`, D26)
- [x] Footprint placement commands: set, move, rotate, flip (bottom side mirrored), lock, remove, list
- [x] Align, distribute (`place.align`, `place.distribute`)
- [x] Ratsnest (MST between copper islands per net), initial auto-placement (rows inside the outline)
- [x] "Place near", decoupling caps next to pins, placement by schematic groups (`place.near`,
      `place.auto` strategy `groups`, `src/board/place.rs`)
- [x] Placement quality on roomy boards: density-aware group spreading (windows may hold their share over the
      density), groups jumping to where their connections pull them, connectors filling empty stretches of
      their edge, every group move within the ratsnest budget; how a placement uses the board measured
      (`board::place::placement_stats`, `board_use` in `place.auto`; `tests/placement_quality.rs`; D67)
- [x] Regions pass from an IC to what goes with it, flow-through parts turned from their connections
      (`place.pair_crossing`), long connectors along edges where that costs no ratsnest (`along_edges` for all of
      them), board use says what its numbers mean on sparse boards (D71)
- [x] Manual routing commands: tracks through coordinates or pins (net inferred, shorts refused), vias; net
      class widths and via sizes
- [x] Copper zones with fill (thermal reliefs, clearances, islands removal, priorities, keep-outs)
- [x] Thermal relief held end to end (D109, BOARD.md "Thermal relief"): relief by default on every layer with
      spokes fitted to small pads, exposed pads and tabs of generated footprints solid on purpose, vias solid;
      pads in pours read from the filled copper (`board::thermal`); `drc.solid_pad`, `drc.thermal_nominal`,
      `drc.starved_thermal` (`min_thermal_spokes`), `drc.thermal_size`, `drc.current_spokes` in `drc.run` and as
      `fab.*` warnings in `fab.check` / `fab.export`; counts in `zone.fill` and at `board.import_kicad`;
      `zone.thermals`; `zone.relieve` to repair a board; KiCad's `starved_thermal` as the oracle of the spoke
      count (`tests/kicad_pcb_oracle.rs`)
- [x] Thermal relief, second round (D112): spokes placed in the relief's other directions where the default ones
      do not survive; a zone connection and relief per placed pad, per layer if needed (`zone.pad`, saved, undone,
      KiCad export and import); `zone.relieve` takes each pad short of spokes alone (narrower relief, a track into
      the pour, solid on one layer with the reason) and leaves none cut off (corpus: 55 pads short of two spokes
      for the projects' own 143, 23 solid on purpose for 664, none cut off); the fitted spoke width in the KiCad
      export; `[thermal_relief]` in the fab profiles and the IPC references; the router commands, `drc.run` and
      `fab.check` name the count and `zone.relieve`; `drc run` on glasgow-revD1 38.6 s to 4.5 s; the KiCad
      install of the CI jobs retries
- [ ] Thermal relief, left open (D112): the native router does not keep room for the spokes of relieved pads
      (guide: the adapter must pass what BOARD.md "What a router must be told" lists); press-fit pins as a pad
      attribute (decided, not implemented); the 3D view draws no copper; `zone.relieve` takes 14 s on glasgow-revD1
- [x] DRC: clearance, width, annular ring, drill, hole-to-hole, copper-to-edge, courtyard overlap, unrouted nets,
      silk over pads, keep-outs (`drc.run`, `src/drc.rs`); zone fills are checked like any copper
- [x] Legible silkscreen (D82): `drc.silk_edge` and `drc.silk_overlap` (rules `silk_to_edge`, `silk_clearance`),
      reference designators moved to free positions by `silk.arrange` and `place.auto` (off the board edge,
      pads and other silkscreen; hidden with a warning as a last resort), set by hand with `silk.set`, kept in
      the board and printed at the same place by the Gerber legend, IPC-2581, the renders and the KiCad export
      (`src/board/silk.rs`, `tests/silk.rs`)
- [x] Silkscreen DRC over every pair that prints (D83): `drc.silk_overlap` also between drawings of different
      owners (two footprints, a footprint and a board drawing) and between a designator and its own footprint's
      drawings; `drc.silk_edge` only for what is printed or cut; board texts with a style (justification, lines,
      character width, stroke, italic, mirroring, knockout) drawn by one function for the DRC and every output;
      a designator's stroke (`silk.set --stroke`). The pairs and distances are KiCad's, probed with `kicad-cli`
      (`tests/kicad_silk_oracle.rs`)
- [x] Board rendering (per layer, composite, realistic top/bottom; highlight, ratsnest, markers, crop)
- [x] Outputs: Gerber X2 (+ X3 component data), Excellon/XNC drill (+ optional Gerber X2 drill), generic
      pick-and-place CSV, IPC-D-356A (`export.*`, `src/fabout/`)
- [x] Outputs per fab profile: file naming/layout, CPL columns and rotation offsets, archive (`fab.export`)
- [x] KiCad `.kicad_pcb` export for oracle tests (`board.export_kicad`, with `.kicad_pro`/`.kicad_dru`; KiCad DRC,
      IPC-D-356 and Gerber oracle in `tests/kicad_pcb_oracle.rs`)
- [x] Cross-checks: KiCad DRC vs cadlab DRC on the same boards, KiCad Gerbers vs ours (raster XOR)
- [x] gerbv oracle: our Gerbers parse and render as expected (`tests/gerber_oracle.rs`, pixel probes)

**Exit:** the target demo board passes cadlab DRC and the KiCad DRC oracle, and the *same unmodified project*
exports bundles that pass both JLCPCB's and PCBWay's online checks. **Not done yet: two of its four parts are
shown, two are waiting** (audit of 2026-11-13, D81). The board is `examples/demo-board/board.jsonl` (agent session
1's ATmega328P + USB-C + LDO + LEDs board as a batch, both fabs as targets, cadlab's own rules).
- Shown 2026-11-13: cadlab DRC clean (ERC and lint too), and one project, unchanged by the exports and naming no
  fab outside `targets`, exports a complete JLCPCB bundle and a complete PCBWay bundle with byte-identical
  fabrication data, the same archives from the documented command lines (`tests/demo_board.rs`,
  `demo_board_passes_drc_and_exports_for_both_fabs`, `documented_commands_write_the_same_bundles`).
- Shown 2026-11-13, in CI: the KiCad DRC oracle on this board (`tests/demo_board.rs`,
  `kicad_drc_clean_demo_board`; it needs `kicad-cli`, so it runs in CI's `oracles` job only). Its first CI run, with
  KiCad 10, failed on two findings: the reference designators of J1 and J3, the connectors at the top edge, were
  printed across the board edge (`silk_edge_clearance`); it also listed about a dozen designators over other
  silkscreen (`silk_overlap`), then allowed as a rule cadlab did not have. Both were fixed by D82: cadlab's DRC
  has the two rules, `place.auto` moves the designators to free positions, and the test no longer allows
  `silk_overlap` on KiCad's side alone. It has passed since (KiCad 10.0.8; master's run of D82, then the runs of
  D83, whose rules also check drawings against drawings and a designator against its own footprint: the board
  stays clean under them in both tools). Without KiCad, `reference_designators_are_legible` shows that no
  designator of the board comes within 0.15 mm of an edge or of other silkscreen.
- Pending, manual: the two fabs' online checks. They need a person with accounts; the bundles, what to upload
  where and what to look at are in [MANUFACTURING.md](MANUFACTURING.md), "Demo board", with a table for the result.

## M5 — Autorouter v1 (L)

Details in [ROUTER.md](ROUTER.md).

- [x] Specctra DSN/SES, implemented from the published Specctra spec (freerouting as benchmark oracle only):
      DSN export (`export.dsn`) and SES import (`route.import_ses`) for external routers, DSN reader in the
      library (`src/specctra/`; freerouting oracle in `tests/specctra.rs`)
- [x] Obstacle model, connection planning (per-net MST), ordering heuristics
- [x] Grid-based multi-layer A* maze router with vias, 45° moves
- [x] Negotiated-congestion rip-up and reroute
- [x] Post-processing: pull-tight, corner smoothing, via reduction
- [x] DRC-verified output, progress reporting, cancellation, time budget
- [x] `route` commands: whole board, net, net class, between two pads; keep or rip existing tracks

**Exit:** routes typical 2- and 4-layer hobby boards (≤ 200 nets) to 100% with zero DRC errors.
*Measured 2026-11-13 (D84, `CADLAB_BENCH_WORK=200000000 route_bench --corpus`, seeds 1–3), not closed:* the
generated 2- and 4-layer boards (ATtiny, SOIC grids, STM32) and four of the five small corpus boards
(nrfmicro, buspirate-flash-sop, buspirate-rs232, lumenpnp-ringlight) route to 100 % on all three seeds with zero
DRC errors, pours refilled; nrfmicro's 2–3 open connections (the USB-C D+/D− crossing) are closed (D84).
sweep-v2.2's 3 open connections are open in the original board too (its two halves are not joined on the
board), so no router closes them. Gap: the three denser corpus boards that are also 2- or 4-layer with at most
200 nets do not reach 100 %: corne-cherry (2 layers, 152 nets of two pads or more) 2/5/2 of 443 open, buspirate5-rev10 (4 layers,
183 nets) 2/4/1 of 538, tinytapeout-demo (4 layers, 137 nets) 25/10/13 of 403. They end on the 200 M budget or
close to it; what is open is pads cut off from pours (all 25 of tinytapeout-demo's in seed 1) and congestion
(ROUTER.md, "Corpus re-route, D84"). Whether a larger budget closes them was not measured.

## M6 — Autorouter v2: freerouting parity (XL)

- [x] Gridless, shape-based router (free-space decomposition, expansion rooms): gridless refinement of grid routes
      (a visibility graph over clearance hulls, D34) and a gridless search over expansion rooms (trapezoidal
      decomposition of the clearance-inflated obstacles per layer, vias between layers, funnel paths at 45°) that
      shares the grid's negotiation; `router: grid | gridless | auto`, `auto` (grid, then gridless for what it
      cannot route, gridless rip-up and retry) by default; neck-downs out of fine-pitch pads and small dog-bone vias
      from the corpus (DECISIONS D39)
- [x] Any-angle / 45° optimized output, arc support: 45° pull-tight over longer runs, mitered corners, a second
      optimization round, escape junctions smoothed, optional any-angle shortcuts, gridless refinement hugging
      minimum clearance, optional arc corners (`arcs`, exported as arcs)
- [x] Push-and-shove for incremental and interactive (API-driven) routing: segment-level walkaround, shove and
      spring-back of unlocked tracks and vias (`route.track` along waypoints, `route.connection`, leftovers of
      `route.all` / `route.nets`), locked items never move, DRC-checked (DECISIONS D34)
- [x] Close the benchmark gaps: STM32 boards at 100 % (escape retries, push-and-shove of leftovers), via
      minimization pass (4–33 % fewer vias)
- [x] BGA/fine-pitch fanout, escape routing: dog-bone quadrant fanout of area arrays, staggered escapes for
      off-grid fine-pitch pads, `route.fanout` (DECISIONS D28)
- [x] Parallel routing (independent regions/nets) with deterministic merge: level-scheduled batches of nets
      with disjoint regions, identical for any thread count
- [x] Benchmark suite: completion rate, vias, wirelength, runtime vs freerouting
- [x] Work budget (`work_budget`, `stats.work`): results that repeat exactly under any load, for corpus and benchmark
      comparisons and for users who want them; the budget also covers the verification DRC (DECISIONS D72)
      (`cargo run --release --example route_bench`, results in ROUTER.md)
- [x] Corpus completion pass: grid snapped to fine pad pitches, USB-C D+/D− crossover vias, failing searches
      skipped by exact reachability tests, capped present factor (corne-cherry 91.8 % → 99.1 %, DECISIONS D57)
- [x] Escape planning per package (an assignment over candidate exits with ejection chains, necks for crowded
      rows, boxed pads re-planned), exact stub conflicts, negotiation estimates the costs near its targets
      (DECISIONS D64)
- [x] Corpus residuals: USB-C crossovers leave the outer pads an exit, escapes may turn earlier, time for the
      boxed-pad re-plan, partial rip-up of large nets near the overuse (corne-cherry 97.7 % → 98.8 %, DECISIONS D69)
- [x] Pours: the corpus completion counted planned connections only, while pads that a pour joined on the empty
      board were cut off by the new tracks (seed 1: 210 failed, 480 open on the refilled boards). Fills are now
      places to connect to, the run fills the pours for its routing and negotiates again for the nets it cut
      off, and what stays cut off is reported as failed (open connections over seeds 1–3: 480/468/457 →
      254/251/297; buspirate5-rev10 66/63/64 → 4/0/7, lumenpnp-mobo 69/62/61 → 8/14/14; DECISIONS D80)
- [x] Corpus residuals of D80, diagnosed: finer grids after a run that leaves a few connections (nrfmicro's
      USB-C corner is fitted to 10 µm, off the 0.2 mm grid), grid lines along the finest rows, exits turning by
      less than onto a line (open connections over seeds 1–3: 254/251/297 → 227/236/222; nrfmicro 2/2/3 → 0/0/0,
      tinytapeout-demo 29/23/28 → 25/10/13; pads cut off from pours diagnosed and left; DECISIONS D84)
- [x] guide as a second backend, measured: `router: guide` on `route.all` and `route.nets` runs the
      [guide](https://crates.io/crates/guide) crate's PCB router through an adapter in the library
      (`router::guide`); its output is verified by cadlab's DRC, counted on the refilled board and kept to the
      native router's rule of no via in a pad. Both routers on the generated suite and the corpus, seeds 1–3
      (ROUTER.md, "Native router and guide, D85"): the native router completes more on every board where both
      finish at the 1200 s given to guide, or as much (generated boards 7 of 7 at 100 % against 6 of 7; on the
      seven corpus boards where guide ends, 13 to 22 connections open against 139), in a seventieth to a half of
      the time on all but the smallest board, and guide cannot be given a budget, stopped or asked for a
      partial result. The default stays the native router (DECISIONS D85)
- [x] guide 0.0.3 adopted in the adapter: budgets, cancellation and progress through guide's run control,
      guide's cause in every failed connection, cadlab's fills as the plan, keep-out exceptions, `layers`, `seed`,
      pad stacks, net ties and area clearances passed instead of worked around (`route.guide_no_budget`,
      `route.guide_keepout_exception` and `route.guide_net_tie` are gone). Measured again (ROUTER.md, "Native
      router and guide 0.0.3, D88"): the generated boards 7 of 7; on the corpus guide ends on eleven of twelve
      boards and leaves 351 connections open where the native router leaves 86 to 105, 281 of them pads it
      cannot leave without a via in the pad or a narrower track. The default stays the native router (D88)
- [x] guide 0.0.4 adopted in the adapter: no fill of guide's at the end, the smallest via the rules allow for
      its escapes (the native fanout's), half the time budget for the first run and a second run on the routed
      board after a cut run too. Measured at the default 60 s (ROUTER.md, "Native router and guide 0.0.4, D91"):
      589 connections open on the twelve corpus boards where guide 0.0.4 behind the adapter of D88 leaves 714
      and guide 0.0.3 left 1023 (lumenpnp-mobo 48 to 9, corne-cherry 90 to 54, glasgow-revD1 394 to 349); 240
      against the native router's 85 on the eleven boards both have a result for. The default stays the native
      router (D91)
- [x] guide 0.0.5 adopted in the adapter: its joining of zones' pieces left out (`join` off; cadlab's fill and
      the second run do it), the reason of a stopped connection by `Routed::tried`, no wording of its own for
      "found no way" after a deadline; the half of the budget and the second run measured again and kept.
      Measured at 60 s (ROUTER.md, "Native router and guide 0.0.5, D93"): 576 and 572 connections open on the
      twelve corpus boards in two runs where guide 0.0.4 leaves 647 and 604 (nrfmicro 11 to 8, cynthion 34 to
      23, lumenpnp-mobo 12 to 6); 227 against the native router's 93 on eleven boards, ahead on two, behind on
      seven. route_bench counts a via in a pad by the pad's copper. The default stays the native router (D93)
- [x] guide 0.0.6 adopted in the adapter: after a run its deadline stopped the second run takes every net the
      board has open, and `Routed::failed` leaves nets out only after a run that ended; the first run has 0.45
      of the budget where it had half; `join` off and the second run measured again and kept. Measured at 60 s
      (ROUTER.md, "Native router and guide 0.0.6, D95"): 353 and 359 connections open on the twelve corpus
      boards in two runs where guide 0.0.5 leaves 572 and 568 (glasgow-revD1 344 to 177, glasgow-revC3 69 to
      37, buspirate5-rev10 16 to 6, corne-cherry 54 to 46; lumenpnp-mobo 7 to 13), with a quarter to two fifths
      more vias and track on corne-cherry and glasgow-revC3; against the native router's 294, ahead on three
      boards (glasgow-revD1 177 against 201), level on three, behind on six, 176 against 93 on the eleven
      without glasgow-revD1. The default stays the native router (D95)
- [x] guide 0.0.7 adopted in the adapter: a courtyard's or an area's clearance rule goes over as a rule area
      whatever other kinds it names, as long as tracks and vias are among them, raised to the later rules that
      hold copper in it, and the last rule holding a pad is the pad's own clearance (a rule for the pads of a
      footprint); the first run's 0.45 and the second run measured again and kept. Measured at 60 s over three
      orders of the nets (ROUTER.md, "Native router and guide 0.0.7, D96"): 315, 275 and 300 connections open
      on the twelve corpus boards where guide 0.0.6 leaves 412, 417 and 374 in the same runs (glasgow-revD1
      228, 231, 201 to 168, 136, 150 by the release; tinytapeout-demo 40, 40, 37 to 14, 20, 20 by the rules);
      against the native router's 285, ahead on two boards in every order, level on three, within the noise on
      two, behind on five, 147, 139 and 150 against 93 on the eleven without glasgow-revD1. The default stays
      the native router (D96)
- [x] guide 0.0.8 and 0.0.9 adopted in the adapter: the first run has 0.7 of the budget, for 0.45, now that a
      net that found no way in its corridor keeps what it reaches with nothing moved before any net crosses
      another's routes (glasgow-revD1's first run has 182 connections open at 27 s for 325, and the board is
      the same with either share), and the nets with zones are among the first run's, open or not
      (lumenpnp-mobo's ground, which cadlab's fill joined whole and the routes then cut: 9, 10, 6 open to 0, 1,
      0); `pour_cuts` on, `row_fanout` off; 0.0.9 is 0.0.8 without its via on a via. Measured at 60 s over
      three orders of the nets (ROUTER.md, "Native router and guide 0.0.8, D97"): 273, 250 and 238 connections
      open on the twelve corpus boards where guide 0.0.7 leaves 354, 278 and 343 in the same runs; against the
      native router's 294, ahead in every order; ahead on three boards in every order, level on three, within
      the noise on three, behind on three (corne-cherry 36, 35, 38 against 3; nrfmicro 4, 7, 9 against 0;
      sweep-v2.2 6, 7, 6 against 3); 111, 114 and 114 against 93 on the eleven without glasgow-revD1, of which
      corne-cherry alone is more than the difference. The default stays the native router (D97)
- [x] guide 0.0.10 and 0.0.11 adopted in the adapter: its passes of rip-up end where they have stopped gaining, and what
      they leave open is settled by chains (a route for an open connection across other nets' routes, the nets
      it crosses cut and joined up again around it at once, kept only if the board then has less open, counting
      the pads that routes cut off their pours, and undone to the track if not); `settle` on, and `row_fanout`
      on boards of two copper layers, where it gains (nrfmicro, corne-cherry), and on no other, where it loses.
      Measured (ROUTER.md, "Native router and guide 0.0.10, D101") with a budget of 40 million nodes over three
      orders, the same run under any load: 853 connections open on the nine boards that a budget cuts for 892
      with 0.0.9 (corne-cherry 140 for 169), and with the rows nrfmicro 11 for 20 and corne-cherry 130. At 60 s
      on a machine with 68 to 75 runnable threads on its 64, where the native router has 370 open for D97's 294:
      412, 408 and 343 for 419, 468 and 362 with 0.0.9 in the same queue; behind the native router in two orders
      of three in that hour. guide 0.0.11 is 0.0.10 without its chains for the pads cut off their pours, which
      kept a board with every net routed from coming back in time on that machine. With it, on the machine as
      D97 had it (ROUTER.md, "Native router and guide 0.0.11, D101"): 263, 249 and 234 open on the twelve
      boards where 0.0.9 has 273, 249 and 243 in the same queue, against the native router's 290, ahead in
      every order; 101, 113 and 110 against 90 on the eleven without glasgow-revD1; ahead on three boards in
      every order, level on three, within the noise on three, behind in every order on three (nrfmicro 3, 5, 3
      against 0, where it had 4, 7, 9; sweep-v2.2 6, 7, 6 against 3; corne-cherry 31, 37, 36 against 3). The
      default stays the native router (D101)
- [x] guide 0.0.16 adopted in the adapter (D110): no via beside a via of its net (D108's failure: none of 48
      runs had copper taken out by the verification); the room of the spokes of pads in pours kept, the spokes
      given to guide as cadlab's fill makes them (`Zones::spokes`, `spoke_cost` 4; BOARD.md, "What a router
      must be told"): 82 to 100 pads short of two necks on the twelve boards over four orders at 60 s, for
      138 and 128 with 0.0.15 on the same fill, 154 to 170 after the native router and 116 as the projects
      route them, with 196 to 205 connections open for 238 and 218; the connections that the board itself
      keeps apart found once, told as such and left out of the later runs (`Cause::Apart`: sweep-v2.2's two
      nets across its halves); a net with zones has its run on the filled board whatever the first run said
      of it. The default stays the native router
- [ ] `router: guide` level with the native router on the corpus, by the ranges of four runs of each (D110,
      with guide 0.0.16: level on the three small boards and sweep-v2.2; ahead on lumenpnp-mobo, cynthion and
      glasgow-revD1; ranges that overlap on buspirate5-rev10, glasgow-revC3, tinytapeout-demo (2 to 10 against
      2 to 15) and nrfmicro (0 to 2 against 0); behind on corne-cherry, 15 to 21 against 0 to 4): needs, in
      guide, a negotiation over all the nets of a part at once that a minute pays for, for corne-cherry's two
      QFN-56 and nrfmicro's last data net (the native router plans no way out of the first QFN at all: it
      routes every net across every other at a price on one grid; fixed ways out for the pads left shut in
      were built and are no gain, D110), vias (lumenpnp-mobo 511 to 686 against 309 to 343: per-net cleanup
      with the others fixed removes few, dearer vias cost connections), a first pass on six layers whose
      third outside the searches is looked into (glasgow-revD1, timed by net, D110), a price on a spoke that
      rises as its pad runs short of them, and rule areas with a routed track width (D110, "Gaps"; D108,
      "Gaps"; D106, "Gaps"; D104,
      "Gaps"; D101,
      "Gaps"; D97, "Gaps"; D96, "Gaps"; D95, "Gaps"; D93, "Gaps"; D91, "What got in the way"; D88, "What still
      blocks it"). Done since D106 (D108): the D+ of a USB receptacle whose pads have those of D- between them
      and a part in front is crossed under the board by a plan (guide 0.0.15, `RouteOptions::crossings`: nrfmicro
      0 to 1 open for 2 to 5); more runs that move routes, in other orders, while there is time; the needless
      stubs to a plane taken out. Done since D104 (D106): a net's pads that nothing joins to its first are joined among
      themselves (guide 0.0.13: sweep-v2.2 has the 3 that the native router has); a last run that takes the
      routes of the runs before back as routes that move, for the pads that cadlab's fill finds cut off a pour
      and those left shut in (guide 0.0.14, `pcb::route_from`)

**Exit:** equal or better completion than freerouting on the benchmark corpus, with comparable runtime.
*Measured 2026-11-17 (D102; ROUTER.md, "freerouting on the corpus"), freerouting 2.5.0 run on both sets and counted by
cadlab's DRC:*
- *Generated benchmark suite: met.* The native router routes all seven comparable boards to 100 % with zero DRC
  errors in 0.2 to 12 s; freerouting 2.5.0 routes six of them to 100 % and bga144-4l to 95.7 %, in 7 to 470 s (2.4.1,
  as recorded since M6: three of seven). freerouting has fewer vias on five of the seven.
- *Open-source corpus, at 60 s: met on all twelve boards.* freerouting leaves 1647 connections open (1702 on a CI
  runner with four processors), the native router 283 (309) and guide 273 (318). freerouting has as few open as
  the native router on the three small boards it completes or misses by one, and more on the nine others.
- *Open-source corpus, freerouting given 30 minutes (2 hours on three boards): met on eleven boards, not on
  lumenpnp-mobo.* freerouting has 1187 open (945 with the 2-hour results), and on every board but one at least as
  many as the native router's 60 s run (as few on two small boards). The one is lumenpnp-mobo, where it closes
  every connection after 5.5 minutes and the native router leaves 10 at 60 s and 1 with the same 30 minutes
  (guide closes all of them there in 32 s). More time helps freerouting until its first routing passes are done
  (glasgow-revD1: 557 open at 30 minutes, still in its fanout, 309 after 2 hours) and not after: its count falls
  for a few passes at most and then moves around its best.
- *What the comparison does not show:* freerouting routes from a DSN, which has no place for pours as fills, a
  minimum track width, hole and edge clearances or the rules of single pads and areas; its results break those
  about 1200 times at 60 s (counted apart), and 14 times the rules the DSN did carry. One run per cell.
- [x] freerouting run on the open-source corpus under stated settings, counted by cadlab's DRC, regenerable
      (`route_bench --corpus` with `CADLAB_BENCH_ROUTER=freerouting`, CI job `freerouting`; DECISIONS D102)
- [ ] lumenpnp-mobo complete: the native router leaves 1 connection with 30 minutes and 10 at 60 s where
      freerouting, given 5.5 minutes, and guide, in 32 s, leave none (D102; with 30 minutes the one left is a
      ground pad of U5 with no way out between its neighbours, the kind of D84's "fine-pitch pad with no exit")

## M7 — KiCad import and more fabs (M)

KiCad writers and oracle checks already exist from M2–M4. This milestone adds import for migrating user projects.

- [x] `.kicad_pcb` import (`board.import_kicad`: setup, outline, footprints, copper, zones, keep-outs, holes,
  graphics; circuit matched or built from the board, DECISIONS D32)
- [x] `.kicad_pro` / `.kicad_dru` rules import (`board.import_kicad_rules`, also run by `board.import_kicad`)
- [x] User `.kicad_sym` / `.kicad_mod` import (`footprint.import_kicad`, `part.import_kicad_sym`, `lib.import_kicad`:
  footprints through the board importer's conversion, symbols as pins and fields with generated drawings,
  DECISIONS D41; fixtures, round trips through cadlab's exports, `kicad-cli fp/sym upgrade` oracle)
- [x] KiCad netlist import (circuits come in as netlists; no `.kicad_sch` parser, see DECISIONS D13):
  `circuit.import`, parts matched or created per DECISIONS D27, oracle round trip through `kicad-cli`
- [x] Round-trip and oracle tests: cadlab boards → `.kicad_pcb` → import (model and DRC equal), the same after
  `kicad-cli pcb upgrade`, KiCad DRC of the export vs cadlab DRC of the import (`tests/kicad_pcb_import.rs`,
  `tests/drc_crosscheck.rs`)
- [x] Round-trip and oracle tests on open-source projects fetched in CI: 12 pinned KiCad 6–10 projects
  (`tests/corpus/projects.toml`, `scripts/fetch-corpus.sh`, D35); netlist → `circuit.import`, board import,
  KiCad DRC of the original and of the re-export vs cadlab's, Gerber/drill raster cross-check, optional
  re-route; known differences recorded per board (`tests/corpus.rs`, TESTING.md "Open-source corpus")
- [x] Import gaps the corpus measures, modeled (D40): mask/paste margins and the minimum web, local clearances
  and zone connections, net ties, pads on the other side of their footprint and per-side mask openings,
  copper (and mask, paste) drawings in footprints and copper drawings and texts on the board, paste on
  through-hole pads, slots (routed in XNC), geometric custom rules (`board.custom_rule`); `footprint.set`
- [x] Trapezoid and chamfered pads, drill offsets (D44): modeled, imported, exported back and carried through
  DRC, zone fill, Gerber macros, IPC-2581, IPC-D-356, Specctra, router, renders and 3D/STEP drills
- [x] Per-layer pad stacks (D48): KiCad 9 `padstack` modes `front_inner_back` and `custom` modeled as the
  pad's `stack`, imported, exported back and carried through DRC, zone fill, Gerber, IPC-2581, Specctra
  padstacks, router obstacles, masks and renders; corpus counts re-measured (D51)
- [x] DRC and pour parity with KiCad (D46): silkscreen against solder mask openings (`silk_check`), thermal
  spoke angle/count and per-pad thermal settings (imported and exported), custom rule pairs (conditions on
  `B`), `disallow` and severities; corpus known differences re-measured with KiCad 10.0.7 (D51)
- [x] Corpus residuals analyzed (D56): exact clearance gaps, KiCad-measured clearance and courtyard
  tolerances, courtyards in pieces and on the other side, rule areas sharing a name, renamed designators,
  copper-to-hole clearance (`hole_clearance`, fills and DRC), pad stacks in IPC-D-356, the IPC-2581 land
  pattern and the footprint preview, SMD pad stacks
- [x] Pour residuals (D60): copper rings around non-plated holes (`npth` pads with `copper`: import, DRC,
  fills, router, Gerber `WasherPad`, drill, IPC-D-356A, DSN, KiCad export, `footprint.create`), thermal spokes
  joining a row of same-net pads, open connections paired by island in the corpus comparison; DRC
  clearance tolerance kept at 2 µm (measured)
- [x] Silkscreen on imported boards (D83): board texts as KiCad draws them (its placement conventions measured
  on its plots and converted at import and export; cadlab's own stroke font), footprint texts on the silkscreen
  as drawings of the footprint, a designator's stroke, the silkscreen of mouse-bite footprints; on the corpus
  the two DRCs agree on `silk_edge_clearance` / `silk_overlap` but for the stroke fonts, KiCad's circles without
  their stroke and its Margin layer, each counted per board in the manifest
- [x] More fab profiles: OSH Park, Aisler, Eurocircuits, Seeed Fusion, NextPCB, PCBgogo, ALLPCB, Elecrow (sourced,
  verified 2026-10-04; table in MANUFACTURING.md)
- [x] Fab profiles re-checked against the fabs' pages 2026-11-13: unverified values closed where published,
  JLCPCB minimum package and PCBWay/Eurocircuits/NextPCB assembly layouts corrected (D77)

**Exit:** a design made in KiCad moves into cadlab and goes on from there: its netlist, board, rules and libraries
import into a project that reads the same and checks the same, a change is made, the affected nets are rerouted,
DRC passes, and the result exports for any built-in fab and back to KiCad. **Done 2026-11-13 on KiCad files cadlab
wrote and on the format fixtures** (`tests/agent_session4.rs`, `migration_of_a_cadlab_board`: the whole migration
through the MCP server, to a JLCPCB and a PCBWay bundle and a KiCad re-export; `tests/kicad_pcb_import.rs`,
`round_trip_into_an_empty_project` and `round_trip_larger_boards`: model and DRC equal after the round trip;
`tests/netlist_import.rs`, `kicad_project_netlist`; `tests/kicad_lib_import.rs`, `symbols_become_parts` and
`footprints_from_a_pretty_directory`; `tests/fab_profile.rs`, `every_builtin_profile_checks_and_exports`). On
projects other people drew in KiCad, the same is shown by `tests/corpus.rs` (12 pinned projects) and
`tests/agent_session4.rs`, `migration_of_the_lumenpnp_ring_light`: they need the fetched corpus and `kicad-cli`, so
they run in CI's `corpus` and `oracles` jobs only and were not run in the audit of 2026-11-13 (D81).

## M8 — Advanced electrical (L)

- [x] Differential pairs (routing + rules), length/skew matching with meanders (`diffpair.*`, `lengthgroup.*`,
      `route.diffpair`, `route.tune`, pair-first `route.all`, `drc.diffpair_*` / `drc.length_mismatch`;
      [ROUTER.md](ROUTER.md), D38)
- [x] Impedance calculator from stackup (microstrip/stripline) → width per net class (stackup dielectrics,
      `board.dielectric`, `impedance.calc/solve`, `drc.impedance`; [ELECTRICAL.md](ELECTRICAL.md), D29)
- [x] SPICE netlist export (ngspice), simulation hooks (`export.spice`: supplies, analysis and `.control` lines;
      ngspice oracle in `tests/electrical.rs`)
- [x] Current/thermal checks (IPC-2152 trace width): `net.set --current`, `current.width`, `drc.current_width`
      (curve fit of the universal chart, approximate: +2.6 % / −1.3 % at the one published point, D79)
- [x] Design lint beyond ERC: missing decoupling, missing pull-ups on I²C, unterminated high-speed nets
      (`circuit.lint`, `circuit.erc --lint`: decoupling, I²C pull-ups, USB ESD, clock termination, floating inputs)

**Exit:** a board with a controlled-impedance interface is designed from its electrical requirements: the pair's
width and gap solved from the fab's stackup, the pair routed coupled and its skew tuned, impedance, pair and
current limits checked by DRC, the circuit linted, and a SPICE netlist written for simulation. **Done 2026-11-13**
(`tests/agent_session3.rs`, `stm32_usb_device_four_layers_from_a_prompt`: the USB device on JLCPCB's four-layer
stackup, 90 Ω pair solved, routed first and tuned inside its skew limit, ERC, lint and DRC clean;
`tests/electrical.rs`, `current_and_impedance_drc`, `spice_export_divider` and `lint_findings`; `tests/diffpair.rs`,
`length_group_tuned_with_trombones`). That ngspice runs the exported netlist is an oracle check, in CI only.

## M9 — 3D and advanced rendering (M)

- [x] Isometric 3D PNG render: board, layers, extruded package bodies generated from package dimensions
  (`render.board3d`, D30)
- [x] 3D model import for accurate bodies via oxideav-mesh3d (D36): `footprint.model_set / model_clear /
      model_list`, models on footprints or a part's footprint, stored in `library/models/`, carried by shared
      libraries; used by `render.board3d` (mesh instead of the generated body), `export.step` (faceted B-rep) and
      `export.idf` (model extent). STL, OBJ, glTF/GLB, USDZ, STEP (`oxideav-step`) and VRML (`oxideav-vrml`),
      with a `0.1in` unit for EDA-made VRML and the fitting unit named when a model's size is off
- [x] STEP export of the assembled board (`export.step`: AP214, exact B-rep board with cutouts and holes, box
      bodies from package dimensions as an assembly; FreeCAD oracle; D31)
- [x] STEP bodies generated like the 3D view's (`cadlab::body3d`, shared with `render.board3d`): chamfered
      molded bodies, leads and terminations, BGA, DIP and header pins as exact planar B-rep solids, MELF bodies and
      pin-1 dots as true cylinders; attached models stay faceted B-rep (D47)
- [x] IPC-2581 rev C output (`export.ipc2581`, also in `export.all`: BOM, stackup, profile, packages,
      components, logical nets, copper/mask/paste/legend/drill features; D31)
- [x] ODB++ if spec terms allow: they do not (Siemens' specification may not be used to develop interfaces
      without a non-sublicensable partnership license), so it is not implemented; IPC-2581 covers the need
      (MANUFACTURING.md, D37)
- [x] IDF 3.0 export for MCAD (`export.idf`: `.emn` board with outline, cutouts, holes and placement, `.emp`
      library of body outlines; D31)
- [x] IDF 3.0 `.ROUTE_OUTLINE` / `.PLACE_OUTLINE` (board inside the copper-to-edge clearance) and
      `.ROUTE_KEEPOUT` / `.VIA_KEEPOUT` / `.PLACE_KEEPOUT` from keep-outs (D47)
- [x] IDX (ProSTEP EDMD) export (`export.idx`: V4.5 baseline with board outline and cutouts, holes, keep-outs
      and component placements with body heights; validated against the published schema; D37)
- [x] IDX change and response messages for component placements: `export.idx_changes` (moved, re-packaged,
      added and removed components since the baseline and later messages, one transaction each) and
      `place.import_idx` (applies MCAD's accepted moves, writes the response with `Accept` and comments);
      validated against the published schema (D47). Changes of the outline, holes and keep-outs: a new baseline

**Exit:** the assembled board leaves cadlab in three dimensions and comes back from mechanical design: a 3D view
with generated or imported bodies, a STEP assembly, IDF and IDX for MCAD with placement changes exchanged in both
directions, and IPC-2581 as the single-file manufacturing hand-off. **Done 2026-11-13** (`tests/exchange.rs`,
`export_commands`, `golden_files`, `step_structure`, `ipc2581_structure`, `idf_structure`, `idx_structure` and
`idx_change_and_response`: every format written through the registry, parsed back and compared with the board, a
moved part exported as an IDX change and MCAD's move applied; `tests/render_board3d.rs`,
`deterministic_with_bodies_where_parts_are`; `tests/models3d.rs`, `render_uses_models_instead_of_bodies` and
`step_writes_models_as_faceted_breps`; `tests/agent_session3.rs` renders the 3D view of a designed board and
exports its STEP).
- [x] Found open by the audit of 2026-11-13 (D81): no other program read these files in any automatic run.
      Since 2026-11-16 CI's `exchange` job has FreeCAD 1.1.4 read the STEP files (the small board, the board with
      one part per package family, a board with an attached model: valid closed solids, the board's volume as
      computed) and `xmllint` validate the IDX baseline, change and response against prostep ivip's V4.5 schema;
      the job names both in `CADLAB_ORACLES_REQUIRED`, so a missing tool or schema fails the tests there
      (TESTING.md, "Exchange outputs"; D99).
- [ ] Open: nothing but cadlab's own reader has read its IPC-2581. `ipc2581_schema_oracle` needs IPC's
      `IPC-2581C.xsd`, and IPC's server refuses it (403 for `https://webstds.ipc.org/2581/`, from CI and from
      elsewhere, 2026-11-17), so the schema can be neither fetched in CI nor its terms read, and no run by hand is
      recorded. To close: a lawful copy of the schema (then `CADLAB_IPC2581_XSD` in the `exchange` job and in its
      `CADLAB_ORACLES_REQUIRED`), or another program that reads IPC-2581 in the job.

## Stretch goals (XL)

Research-sized items beyond the milestones above; none blocks a release.

- [x] 2D cross-section field solver (quasi-static, e.g. boundary or finite elements from textbook methods):
      Z₀, εeff and losses of single lines and coupled pairs from the stackup; covers what the closed forms of
      D29 do not (coplanar waveguide with ground, broadside and asymmetric coupling, solder-mask cover,
      trapezoidal etch profiles, tight coupling). Selectable in `impedance.calc/solve`, checked against the
      closed forms in their accurate range and against published reference values. Done as a boundary
      element solver (`method: field_solver`, D49; ELECTRICAL.md "Field solver"): Z₀, εeff, Zdiff, Zcomm,
      odd/even modes, C and L; within 0.1 % of the exact stripline and coupled-stripline results. Losses
      (D52, ELECTRICAL.md "Losses"): loss tangent per dielectric, skin effect (perturbation current method in
      the field solver, Wheeler's rule on the closed forms), Hammerstad–Jensen and Huray roughness, RLGC,
      complex Z₀ and dB/m (differential for pairs) at frequencies or a sweep in `impedance.calc`, and `net.loss`
      for a routed net
- [x] 3D full-wave EM solver (e.g. FDTD or FEM) for S-parameters of vias, transitions, connectors and short
      interconnects, and an export of the board geometry to an external solver (openEMS) as a first step.
      First step done (D54; ELECTRICAL.md "Full-wave export"): `export.openems` writes a board region (nets,
      items such as a via or connector, or a window) as CSXCAD geometry (stackup dielectrics with εr and loss
      tangent, copper per layer and net, vias and holes, solder mask), a graded λ/N mesh, and a Python driver
      with lumped or microstrip ports, Gaussian excitation and PML/MUR boundaries that writes a Touchstone file;
      the Touchstone 1.1/2.0 reader and `touchstone.summary` read results back. Built-in solver done as FDTD
      (D59, D63; ELECTRICAL.md "Full-wave solver"): `sim.sparams` simulates the same region (Yee grid on the
      graded mesh, copper with a surface impedance (skin effect, Hammerstad–Jensen or Huray roughness),
      Djordjevic–Sarkar dielectrics as fitted Debye terms (stackup `ref_freq`), solder mask, vias, CPML, lumped
      and microstrip ports, Gaussian excitation, S = B A⁻¹ from DFT power waves, Touchstone out), with a cell
      guard, progress and cancellation, thread-count-independent results; validated against the 2D field solver
      (Z₀, εeff within 1.5 %), D52's conductor, dielectric and total loss, cavity resonances and Q (copper:
      h/δ within 1 %; dispersive: 1/tan δ(f) within 1 %), energy conservation and the Courant limit.
      Board parts (D65): two-pin resistors, capacitors and inductors in the region are series R–L–C lumped
      elements from their part values (`esr`, `esl` too) across their pads' gap, validated against circuit
      theory (series and shunt R, L, C and resonant R–L–C between 50 Ω ports within 0.02 in |S₂₁|).
      Not covered (refinements, not part of this goal): subgridded edges, and a FEM alternative
- [x] openEMS export parity with the built-in solver (D68): Djordjevic–Sarkar dielectrics and the solder mask
      as `DebyeMaterial`s with the solver's own fitted terms, lossy copper as `ConductingSheet`s (roughness
      folded into the conductivity at f₀), matched microstrip port terminations, a geometry file openEMS reads;
      an openEMS oracle CI job (built from source, pinned) comparing a lossy microstrip with `sim.sparams`;
      the CPML's frequency shift raised to 2π ε₀ f₀, which removes the −9.5 % low loss reading at 1–2 GHz
- [x] Conformal copper in the built-in solver (D73; ELECTRICAL.md "Full-wave solver"): Dey–Mittra cells on
      diagonal and curved copper, at the uncut timestep, for perfect and lossy copper; 45° microstrip εeff within
      1 % at 8 cells across (staircase +7 %), curved wall resonances second order, wall and strip conductor Q
      within 2–5 % (staircase −21 %, +41 %); the mesh refined for diagonal tracks under 1.5 cells wide, which were
      open circuits at the default mesh
- [x] openEMS oracle of the via transition and of board parts (D74): the export's absorbing cells beyond the region
      (`pml_outside`), because a board edge next to the wall diverged in openEMS's PML and Mur; via barrels as node
      columns (they read open); two-pin R, L, C as `LumpedElement`s (`parts`), compared with `sim.sparams` and
      circuit theory; `openems.diverged`, `openems.edge_near_boundary`, `openems.mur_static_field`, `openems.part_in_pml`
- [x] The ems crate as a second full-wave solver (D86; ELECTRICAL.md "ems as a second solver"): `sim.sparams
      --solver ems` runs the region, mesh, ports and parts of the built-in solver on the `ems` FDTD (cargo feature
      `ems`), through the property list the openEMS export writes; measured against the built-in solver on its
      validation cases (lossless, Debye, lumped and cavity cases equal to 0.001, two to five times faster; lossy
      solid copper, conformal sheets and roughness not at parity). The built-in solver stays the default
- [x] ems 0.0.7 in `--solver ems` (D87): lossy `solid` copper as metal with a surface impedance on every face,
      sheets as rational two-ports, roughness over the band, conformal cells where copper is oblique to the mesh,
      typed warnings as `sim.ems_<code>` with subjects, one compile for all ports, the port voltage as the mean
      over its columns. Running ems's lossy copper with its conformal outlines for the first time found a panic,
      a growing mode and a loss that depended on the direction; fixed in ems, with tests there. Lossy and
      diagonal copper now agree with the built-in solver (0.006 dB on the oracle's line, 0.2° to 3.6° on D73's)
- [x] ems 0.0.9 in `--solver ems` (D89): the adapter's three workarounds are gone (the threshold for small cut
      faces with its three compiles, the staircase for a diagonal track under 1.5 cells, the staircase for a plated
      hole under two cells, `sim.ems_staircase`); ems does all three itself since 0.0.8 and keeps its conformal
      cells: the via transition within 0.012 dB of the built-in solver (0.048 dB as a staircase), session 6's
      network within 0.001. The built-in solver's board-level tests and the openEMS oracle run on both solvers.
      0.0.8 grew on lossy solid copper with slanted outlines on a fine mesh and ended the run as converged;
      0.0.9 has the fix
- [x] ems 0.0.10 in `--solver ems` (D90): the laminate's Debye poles averaged at its surface as the built-in
      solver's are (`PoleWeighting::Averaged`), ems's end criterion without the energy inside its conductor
      models, its corrected loss on slanted `solid` copper. D89's three findings measured gone: the 60 mm line
      within the built-in solver's 6 % (+5.3 % at most, +7.2 % before; the test's wider tolerance for ems is
      removed), lossy copper to −60 and −70 dB in the built-in solver's steps, slanted `solid` copper 0.2 to
      2.2 % below the built-in solver (10 to 14 % above). With `solid` copper ems is 1.2 to 4 times the faster
      solver (the via transition at the default end criterion: 75 s against 204 s)
- [x] ems 0.0.11, and `sim.sparams` runs on ems by default (D94): the one case D90 was left with, lossy `sheet`
      copper with slanted outlines on a 0.25 mm mesh, takes ems 30 to 38 s where the built-in solver takes 33 to
      47 s (1.8 times with 0.0.10), in the same steps and with 0.0.10's results byte for byte; session 6's
      network 1.1 to 1.3 times the built-in solver's time when both get three threads, and 0.8 times as each is
      run without `threads`. `solver: builtin` runs cadlab's own solver, which is also what a build without the
      cargo feature `ems` runs; the output and the Touchstone file name the solver; every validation test runs
      on both
- [x] ems 0.0.12 (D98), written against what D94 left open on ems's side, with 0.0.11's results byte for byte
      on the nine cases compared: memory 1.3 to 1.8 times the built-in solver's (1.6 to 4.4; the bent line on
      0.25 mm cells 91 MB for 159 MB, the built-in solver 57 MB), a Debye laminate on one thread 1.1 times its
      time on a small region (1.3), session 6 at equal threads 43 to 45 s against 51 to 60 s (0.7 to 0.9 times;
      1.1 to 1.3). The openEMS oracle job passed with ems as the default (0.0.11 on master) and with 0.0.12
- [x] Remove the built-in engine (D100; the first of D98's three ways): ems and csx are dependencies of every
      build, the cargo feature `ems` is gone, and so are `fdtd::engine`, its conformal cells and its surface
      impedance fit (5500 lines with their tests). `sim.sparams` has no `solver` parameter, and the result names
      `ems 0.0.12`. Given up: the second result on the same model in the same process, a build without the
      solver dependency, and the engine's lower memory (ems takes 1.3 to 1.8 times as much, D98). Kept: the
      region, mesh, ports, parts and waves, the openEMS export and its oracle, every validation against an
      analytic reference (now on ems alone), and the engine's result on one coarse line as recorded figures
- [x] ems beside openEMS, measured (D103): the openEMS oracle job runs both solvers on its cases, one process
      per run on stated threads, and keeps the table: time, steps, cells, peak memory, and each solver's
      distance from D52, the field solver or circuit theory. With ems 0.0.12 ems took 0.35 to 0.7 of openEMS's
      time on one thread and 0.2 to 0.45 at each solver's fastest, less memory as a process on every case, and
      was the nearer one in the lines' loss (the 36 to 40 % of the oracle job at 0.56 GHz are openEMS's);
      openEMS was the nearer one at 1 % of `f_max` and for the parts against ideal lines. ems 0.0.13 and 0.0.14
      close what its profiles showed (the stability estimate of conformal PEC, the CPML's short rows, a Debye
      edge's loops) with the Touchstone data unchanged. Left: ems's time step, shorter than the scheme would
      carry, and the memory of a run with a Debye laminate, as much as openEMS's
- [x] ems 0.0.15 (D105): the time step from a bound over the mesh's cells (each with its sizes, permittivities,
      walls and copper), proven part by part and tested from noise and by runs ten times as long: 1.00 to 1.31
      times the Courant step on the oracle's regions, every validation within three units of the fourth decimal
      of what it read. Left: the part of the estimated limit (1.29 to 1.45 times) that single cells do not
      prove; the thirds lines at the region's walls, which set the smallest cells; the run's own memory
- [x] ems 0.0.16 and 0.0.17 (D107): the cells' shares balanced (the dual length at every node divided between
      its two cells so that the largest of the cells' eigenvalues is small: the limit itself on a graded mesh
      in one medium), the cut cells in the bound and the conformal treatment that of the Courant step: 1.22
      to 1.41 times the Courant step on the oracle's regions (0.0.15: 1.00 to 1.31), ems's step the longer
      beside openEMS on every case; the compiled model's poles as runs (memory); the moves of D105's table
      separated into the step (one) and where the run ends (six). Evaluated and not taken: the mesh without
      the thirds lines at the region's walls (two validations of eleven further from their references), a tail
      extrapolated from the port signals (it does not predict the signal run on). Left: the end criterion is
      the largest error of a run to −50 dB at the low end and on the parts; conformal models whose worst cut
      cell the shares of a slab do not reach

---

## Cross-cutting, always on

- **Docs:** every command documented from its schema; examples doubled as tests.
- **Performance:** benchmarks tracked in CI from M4 on (load, DRC, zone fill, route). A synthetic 500-component,
  four-layer board and timings of every heavy operation exist (`cargo run --release --example bigboard`,
  `tests/perf.rs`, the corpus boards with `--example bigboard corpus`; numbers in docs/BOARD.md "Performance").
  **Tracked in CI** (`bench` job, DECISIONS D43): router results on the generated boards against
  `tests/golden/bench/route.json` (fails on a completion drop, any DRC error, vias or length beyond tolerance) and
  the large-board timings against `tests/golden/bench/perf.json` (fails only on large regressions), with JSON and
  Markdown reports as artifacts.
- **Determinism:** golden files for every exporter and renderer.
- **Agent ergonomics:** after each milestone, run a scripted agent session that designs a board from a prompt,
  and fix whatever the agent got stuck on. Session 1 done (ATmega328P + USB-C, JLCPCB bundle; 15 friction points
  fixed, D45); log in docs/TESTING.md "Agent sessions", replayed by `tests/agent_session.rs`. Session 1's open
  items fixed and the session replayed with them (D50: spread placement, unrouted summary, nested enums in
  `describe`, KiCad footprint names, `footprint.create`). Session 3 done (STM32 USB device on 4 layers with a
  90 Ω pair, buck, flash; JLCPCB bundle and STEP; D53), replayed by `tests/agent_session3.rs`; its open items
  fixed (D55: signal-path lengths with stubs reported, spread placement over the whole board, fab stackups
  listed). Session 4 done through the MCP server (KiCad migration of the lumenpnp ring light: netlist and
  board import, package swap, power LED, mounting hole, reroute, DRC with waivers, JLCPCB and PCBWay bundles,
  KiCad re-export; D58), replayed by `tests/agent_session4.rs`; its open items fixed (D62: KiCad DRC
  exclusions as waivers, keep-together hints, `place.near` over routing, `route.nets` shoving leftovers,
  zone fills in the KiCad export, supplier part numbers from netlist fields; the schematic netlist reader
  stays open). Session 5 done through the MCP server (mixed-signal 24-bit data logger: front-end block
  instantiated twice, AGND/GND star point under split pours, analog/digital placement regions; D66), replayed by
  `tests/agent_session5.rs`; fixed: first block instance at capture, net ties in ERC/SPICE/BOM/CPL/STEP,
  `place.hint --region`, edge hints over rooms, `drc.split_crossing`, ERC filters, warnings when routed parts
  move, SPICE subsets, named ground symbols. Its open items fixed (D71: `part.set --pins`, `describe` with shared
  definitions, folding and `path`, a smaller `tools/list`, regions inherited from ICs, flow-through orientation). Session 6 done through the MCP server, on the electrical side (2.4 GHz
  node: nRF52832, matching network, 50 Ω microstrip, U.FL; D70), replayed by `tests/agent_session6.rs`; fixed: mesh
  slivers, `sim.sparams --plan`, `part_values`, results at a frequency and over a band with impedance, VSWR and
  available loss, passivity and reciprocity checks, notes on ideal parts and ports on parts, `net.loss` with mask,
  impedance lines in `fab-lock.json`. Its open item fixed (D76): net exceptions in keep-outs (`keepout.add
  --allow-nets`, `keepout.set`), honored by the DRC, zone fill, the router (grid, gridless, push-and-shove, pairs)
  and `route.track`, and reported by the exports that cannot express them (DSN, IDF, IDX, KiCad).
